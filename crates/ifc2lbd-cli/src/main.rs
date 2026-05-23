//! Provides the command-line interface and end-to-end orchestration of the pipeline.
//! Parses flags and module options, wires together parsing, modeling, conversion, and serialization.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::path::PathBuf;
use std::thread;
use std::time::Instant;

use anyhow::Context;
use clap::{Parser, ValueEnum};
use ifc_model::build_model;
use ifc_step::parse_step_file;
use lbd_converter::{normalize_base_uri, stream_beo, stream_bot, stream_ifcowl, stream_omg_fog, stream_props_opm, ConvertOptions};
use lbd_serializer::{
    serialize_lbd_batches_incremental_to_writer, serialize_lbd_batches_to_writer,
    serialize_nquads_batches_to_writer, serialize_nquads_merged_batches_to_writer,
    serialize_turtle_batches_to_writer,
};

mod chunk_writer;
mod pipeline_plugins;

const SERIALIZER_CHANNEL_CAPACITY: usize = 32;
const SERIALIZER_BUFFER_BYTES: usize = 1024 * 1024;
const IFCOWL_TO_NQ_ESTIMATE_MULTIPLIER: u64 = 28;
const LBD_TO_NQ_ESTIMATE_MULTIPLIER: u64 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    Turtle,
    Nquads,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurtleGrouping {
    Sorted,
    Streaming,
}
#[derive(Debug, Parser)]
#[command(name = "ifc2lbd-neo")]
#[command(about = "Convert IFC STEP files to a first-slice LBD Turtle model")]
struct Args {
    input: Option<PathBuf>,

    #[arg(
        short = 'o',
        short_alias = 't',
        long = "output",
        visible_alias = "target-file"
    )]
    output_file: Option<PathBuf>,

    #[arg(
        short = 'u',
        long = "base-uri",
        visible_alias = "url",
        default_value = "https://lbd.example.com/"
    )]
    base_uri: String,

    /// Development tuning.
    #[arg(long = "geometry-tolerance", default_value_t = 1e-6, hide = true)]
    geometry_tolerance: f64, /* used by future CSG boolean intersection */

    // used for future CSG boolean intersection,
    /// Voxel cell size in meters (default 0.1 = 10cm).
    #[arg(long = "voxel-cell-size", default_value_t = 0.1, hide = true)]
    voxel_cell_size: f64,

    /// Skip elements whose voxel count exceeds this threshold (default 50000).
    /// Giant-footprint elements (e.g. BuildingElementProxy spanning a whole storey)
    /// produce enormous voxel sets and cause false-positive adjacency with nearly
    /// every other element. Setting to 0 disables the filter.
    #[arg(
        long = "voxel-max-element-voxels",
        default_value_t = 50_000,
        hide = true
    )]
    voxel_max_element_voxels: usize,

    /// List built-in pipeline modules and exit.
    #[arg(long = "list-modules", default_value_t = false)]
    list_modules: bool,

    /// Enable one or more modules by id. Can be provided multiple times.
    #[arg(long = "module")]
    module: Vec<String>,

    /// Set typed module options as `<module-id>.<key>=<value>`. Repeat as needed.
    #[arg(long = "module-opt")]
    module_opt: Vec<String>,

    /// Show resolved module activation plan and exit.
    #[arg(long = "show-module-plan", default_value_t = false)]
    show_module_plan: bool,
}

#[derive(Clone, Debug)]
struct NquadsModuleOptions {
    chunking: chunk_writer::QuadChunkingMode,
    chunk_size_lines: usize,
    chunk_size_bytes: usize,
    chunk_prefix: String,
    chunk_min_count: usize,
    chunk_core_count: Option<usize>,
}

#[derive(Clone, Debug)]
struct ExecutionSettings {
    output_format: OutputFormat,
    emit_ifcowl: bool,
    nquads: NquadsModuleOptions,
    turtle_grouping: TurtleGrouping,
}

fn main() -> anyhow::Result<()> {
    let run_start = Instant::now();
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(std::io::stderr)
        .init();
    let built_in_registry = pipeline_plugins::built_in_registry();
    tracing::debug!(
        "pipeline registry initialized with {} built-in modules",
        built_in_registry.len()
    );
    let args = Args::parse();
    if args.list_modules {
        print_pipeline_modules(&built_in_registry);
        return Ok(());
    }
    let requested_modules = build_requested_module_list(&args);
    let activation_plan = built_in_registry
        .resolve_activation(&requested_modules)
        .map_err(|error| anyhow::anyhow!("module activation failed: {}", error))?;
    let module_configs = parse_module_configs(&args.module_opt)
        .map_err(|error| anyhow::anyhow!("invalid --module-opt: {}", error))?;
    validate_module_configs(&activation_plan, &module_configs)?;
    validate_typed_module_configs(&module_configs)
        .map_err(|error| anyhow::anyhow!("invalid --module-opt: {}", error))?;
    let settings = resolve_execution_settings(&args, &activation_plan, &module_configs)?;
    validate_activation_plan_with_args(&activation_plan, &settings)?;
    if args.show_module_plan {
        print_module_plan(&built_in_registry, &activation_plan, &module_configs);
        return Ok(());
    }
    let input_path = args
        .input
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing required IFC input path"))?;
    validate_args(&args, &settings)?;
    let input_file_size_bytes = std::fs::metadata(input_path).map(|m| m.len()).unwrap_or(0);

    let active_plugins: HashSet<&str> = activation_plan
        .enabled_ids
        .iter()
        .map(|id| id.as_str())
        .collect();
    let mut active_producer_ids: Vec<String> = activation_plan
        .enabled_ids
        .iter()
        .filter_map(|id| built_in_registry.plugin(id).map(|p| p.manifest()))
        .filter(|m| m.stage == lbd_pipeline::PipelineStage::Produce)
        .map(|m| m.id.to_string())
        .collect();
    active_producer_ids.sort();
    tracing::info!(
        "active producer modules: {}",
        active_producer_ids.join(", ")
    );
    let output_format = settings.output_format;
    let emit_ifcowl = settings.emit_ifcowl;
    let turtle_grouping = settings.turtle_grouping;
    let normalized_base = normalize_base_for_graph_iri(&args.base_uri);
    let lbd_graph_iri = format!("{normalized_base}/lbd");
    let ifcowl_graph_iri = format!("{normalized_base}/ifcowl");

    let parse_start = Instant::now();
    let step = std::sync::Arc::new(
        parse_step_file(input_path)
            .with_context(|| format!("failed to parse STEP file {}", input_path.display()))?,
    );
    tracing::info!(
        "phase parse_step_file completed in {:.3}s",
        parse_start.elapsed().as_secs_f64()
    );
    let build_start = Instant::now();
    let model = std::sync::Arc::new(build_model(&step).context("failed to build IFC model")?);
    tracing::info!(
        "phase build_model completed in {:.3}s",
        build_start.elapsed().as_secs_f64()
    );

    let topology_graph_iri = format!("{normalized_base}/topology");

    let base_options = ConvertOptions {
        base_uri: args.base_uri,
        emit_ifcowl_links: emit_ifcowl,
        enable_topology: false,
        enable_topology_extension: false,
        topology_only: false,
        suppress_non_topology_fallback: false,
        geometry_relations: None,
        geometry_bounding_boxes: None,
        geometry_wkts: None,
        geometry_tolerance: args.geometry_tolerance,
        low_memory_mode: false,
        stream_batch_size: 8 * 1024,
        ifcowl_max_workers: 16,
    };

    // CLI producers use direct streaming (not spawn_producers), so we must
    // explicitly run the preprocess stage here before those calls execute.
    let preprocess_ids: Vec<String> = activation_plan
        .enabled_ids
        .iter()
        .filter_map(|id| built_in_registry.plugin(id).map(|p| p.manifest()))
        .filter(|m| m.stage == lbd_pipeline::PipelineStage::Preprocess)
        .map(|m| m.id.to_string())
        .collect();
    let limits = lbd_pipeline::ResourceLimits {
        memory_budget_bytes: 0,
        thread_count: rayon::current_num_threads().max(1),
        channel_capacity: SERIALIZER_CHANNEL_CAPACITY,
        batch_size: base_options.stream_batch_size,
    };
    let mut ctx = lbd_pipeline::PipelineContext::new(limits);
    ctx.insert(model.clone());
    ctx.insert(std::sync::Arc::new(base_options.clone()));
    ctx.insert(step.clone());
    lbd_pipeline::spawn_preprocessors(&preprocess_ids, &built_in_registry, &mut ctx)
        .map_err(|e| anyhow::anyhow!("preprocess stage failed: {:?}", e))?;
    // Re-read model and step — preprocess plugins may have replaced them via ctx.replace().
    let model = ctx
        .get::<ifc_model::IfcModel>()
        .ok_or_else(|| anyhow::anyhow!("IfcModel missing from context after preprocess"))?;
    let step = ctx
        .get::<ifc_step::StepFile>()
        .ok_or_else(|| anyhow::anyhow!("StepFile missing from context after preprocess"))?;

    let (converter_lbd_sender, converter_lbd_receiver) =
        crossbeam::channel::bounded(SERIALIZER_CHANNEL_CAPACITY);
    let lbd_receiver = converter_lbd_receiver;

    let (ifcowl_sender, mut ifcowl_receiver) = if emit_ifcowl {
        let (sender, receiver) = crossbeam::channel::bounded(SERIALIZER_CHANNEL_CAPACITY);
        (Some(sender), Some(receiver))
    } else {
        (None, None)
    };

    let lbd_target = args.output_file.clone();
    let lbd_base_uri = base_options.base_uri.clone();
    let lbd_graph_iri_thread = lbd_graph_iri.clone();
    let ifcowl_graph_iri_thread = ifcowl_graph_iri.clone();
    let topology_graph_iri_thread = topology_graph_iri.clone();
    let quad_chunking_mode = settings.nquads.chunking;
    let grafeo_direct_stream = active_plugins.contains(lbd_pipeline::GRAFEO_EXPORT_ID);
    let quad_chunk_size_lines = settings.nquads.chunk_size_lines;
    let quad_chunk_size_bytes = settings.nquads.chunk_size_bytes;
    let quad_chunk_prefix = settings.nquads.chunk_prefix.clone();
    let quad_chunk_min_count = settings.nquads.chunk_min_count;
    let ifcowl_chunk_core_count =
        chunk_writer::resolve_effective_core_chunk_count_for_estimated_bytes(
            settings.nquads.chunking,
            settings.nquads.chunk_core_count,
            settings.nquads.chunk_min_count,
            input_file_size_bytes.saturating_mul(IFCOWL_TO_NQ_ESTIMATE_MULTIPLIER),
        );
    let lbd_chunk_core_count = chunk_writer::resolve_effective_core_chunk_count_for_estimated_bytes(
        settings.nquads.chunking,
        settings.nquads.chunk_core_count,
        settings.nquads.chunk_min_count,
        input_file_size_bytes.saturating_mul(LBD_TO_NQ_ESTIMATE_MULTIPLIER),
    );
    if settings.nquads.chunking == chunk_writer::QuadChunkingMode::Cores {
        tracing::info!(
            "core chunk targets (auto): ifcowl={}, lbd={}",
            ifcowl_chunk_core_count.unwrap_or(1),
            lbd_chunk_core_count.unwrap_or(1),
        );
    }
    let merged_ifcowl_receiver = if output_format == OutputFormat::Nquads {
        ifcowl_receiver.take()
    } else {
        None
    };
    let merged_topology_receiver: Option<crossbeam::channel::Receiver<Vec<lbd_ontology::Triple>>> = None;
    let lbd_thread = thread::spawn(move || -> anyhow::Result<()> {
        match output_format {
            OutputFormat::Turtle => match lbd_target {
                Some(path) => {
                    let file = File::create(&path).with_context(|| {
                        format!("failed to create output file {}", path.display())
                    })?;
                    let writer = BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, file);
                    if turtle_grouping == TurtleGrouping::Sorted {
                        serialize_lbd_batches_to_writer(lbd_receiver, writer, &lbd_base_uri)
                            .with_context(|| {
                                format!("failed to write Turtle to {}", path.display())
                            })?;
                    } else {
                        serialize_lbd_batches_incremental_to_writer(
                            lbd_receiver,
                            writer,
                            &lbd_base_uri,
                        )
                        .with_context(|| format!("failed to write Turtle to {}", path.display()))?;
                    }
                }
                None => {
                    let stdout = std::io::stdout();
                    let handle = stdout.lock();
                    let writer = BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, handle);
                    if turtle_grouping == TurtleGrouping::Sorted {
                        serialize_lbd_batches_to_writer(lbd_receiver, writer, &lbd_base_uri)
                            .context("failed to write Turtle to stdout")?;
                    } else {
                        serialize_lbd_batches_incremental_to_writer(
                            lbd_receiver,
                            writer,
                            &lbd_base_uri,
                        )
                        .context("failed to write Turtle to stdout")?;
                    }
                }
            },
            OutputFormat::Nquads => {
                if grafeo_direct_stream {
                    let stdout = std::io::stdout();
                    let handle = stdout.lock();
                    let writer = BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, handle);
                    pipeline_plugins::stream_grafeo_batches_to_writer(
                        lbd_receiver,
                        merged_ifcowl_receiver,
                        merged_topology_receiver,
                        writer,
                        &lbd_graph_iri_thread,
                        &ifcowl_graph_iri_thread,
                        &topology_graph_iri_thread,
                    )
                    .context("failed to write Grafeo direct RDF stream to stdout")?;
                } else if quad_chunking_mode != chunk_writer::QuadChunkingMode::None {
                    let output_dir =
                        chunk_writer::resolve_quad_chunk_output_dir(lbd_target.as_deref());
                    let mut lbd_chunk_writer = chunk_writer::QuadChunkWriter::new(
                        output_dir,
                        format!("{}-lbd", quad_chunk_prefix),
                        quad_chunking_mode,
                        quad_chunk_size_lines,
                        quad_chunk_size_bytes,
                        quad_chunk_min_count,
                        lbd_chunk_core_count,
                    )?;
                    let mut ifcowl_chunk_writer = if merged_ifcowl_receiver.is_some() {
                        Some(chunk_writer::QuadChunkWriter::new(
                            chunk_writer::resolve_quad_chunk_output_dir(lbd_target.as_deref()),
                            format!("{}-ifcowl", quad_chunk_prefix),
                            quad_chunking_mode,
                            quad_chunk_size_lines,
                            quad_chunk_size_bytes,
                            quad_chunk_min_count,
                            ifcowl_chunk_core_count,
                        )?)
                    } else {
                        None
                    };
                    let mut topology_chunk_writer: Option<chunk_writer::QuadChunkWriter> = None;

                    let ifcowl_thread = if let Some(ifcowl_receiver) = merged_ifcowl_receiver {
                        let ifcowl_graph = ifcowl_graph_iri_thread.clone();
                        let mut writer = ifcowl_chunk_writer
                            .take()
                            .ok_or_else(|| anyhow::anyhow!("missing IfcOWL chunk writer"))?;
                        Some(thread::spawn(move || -> anyhow::Result<()> {
                            serialize_nquads_batches_to_writer(
                                ifcowl_receiver,
                                &mut writer,
                                &ifcowl_graph,
                            )
                            .context("failed to write IfcOWL chunked N-Quads output")?;
                            writer
                                .finish()
                                .context("failed to finalize IfcOWL quad chunk manifest")?;
                            Ok(())
                        }))
                    } else {
                        None
                    };
                    let topology_thread = if let Some(receiver) = merged_topology_receiver {
                        let topology_graph = topology_graph_iri_thread.clone();
                        let mut writer = topology_chunk_writer
                            .take()
                            .ok_or_else(|| anyhow::anyhow!("missing topology chunk writer"))?;
                        Some(thread::spawn(move || -> anyhow::Result<()> {
                            serialize_nquads_batches_to_writer(
                                receiver,
                                &mut writer,
                                &topology_graph,
                            )
                            .context("failed to write topology chunked N-Quads output")?;
                            writer
                                .finish()
                                .context("failed to finalize topology quad chunk manifest")?;
                            Ok(())
                        }))
                    } else {
                        None
                    };

                    serialize_nquads_batches_to_writer(
                        lbd_receiver,
                        &mut lbd_chunk_writer,
                        &lbd_graph_iri_thread,
                    )
                    .context("failed to write LBD chunked N-Quads output")?;
                    lbd_chunk_writer
                        .finish()
                        .context("failed to finalize LBD quad chunk manifest")?;

                    if let Some(handle) = ifcowl_thread {
                        handle.join().map_err(|_| {
                            anyhow::anyhow!("IfcOWL chunk writer thread panicked")
                        })??;
                    }
                    if let Some(handle) = topology_thread {
                        handle.join().map_err(|_| {
                            anyhow::anyhow!("topology chunk writer thread panicked")
                        })??;
                    }
                } else {
                    match lbd_target {
                        Some(path) => {
                            let file = File::create(&path).with_context(|| {
                                format!("failed to create output file {}", path.display())
                            })?;
                            let mut writer =
                                BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, file);
                            if let Some(ifcowl_receiver) = merged_ifcowl_receiver {
                                serialize_nquads_merged_batches_to_writer(
                                    lbd_receiver,
                                    ifcowl_receiver,
                                    &mut writer,
                                    &lbd_graph_iri_thread,
                                    &ifcowl_graph_iri_thread,
                                )
                                .with_context(|| {
                                    format!("failed to write N-Quads to {}", path.display())
                                })?;
                            } else {
                                serialize_nquads_batches_to_writer(
                                    lbd_receiver,
                                    &mut writer,
                                    &lbd_graph_iri_thread,
                                )
                                .with_context(|| {
                                    format!("failed to write N-Quads to {}", path.display())
                                })?;
                            }
                            if let Some(topology_receiver) = merged_topology_receiver {
                                serialize_nquads_batches_to_writer(
                                    topology_receiver,
                                    &mut writer,
                                    &topology_graph_iri_thread,
                                )
                                .with_context(|| {
                                    format!(
                                        "failed to append topology N-Quads to {}",
                                        path.display()
                                    )
                                })?;
                            }
                        }
                        None => {
                            let stdout = std::io::stdout();
                            let handle = stdout.lock();
                            let mut writer =
                                BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, handle);
                            if let Some(ifcowl_receiver) = merged_ifcowl_receiver {
                                serialize_nquads_merged_batches_to_writer(
                                    lbd_receiver,
                                    ifcowl_receiver,
                                    &mut writer,
                                    &lbd_graph_iri_thread,
                                    &ifcowl_graph_iri_thread,
                                )
                                .context("failed to write N-Quads to stdout")?;
                            } else {
                                serialize_nquads_batches_to_writer(
                                    lbd_receiver,
                                    &mut writer,
                                    &lbd_graph_iri_thread,
                                )
                                .context("failed to write N-Quads to stdout")?;
                            }
                            if let Some(topology_receiver) = merged_topology_receiver {
                                serialize_nquads_batches_to_writer(
                                    topology_receiver,
                                    &mut writer,
                                    &topology_graph_iri_thread,
                                )
                                .context("failed to append topology N-Quads to stdout")?;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    });

    let mut ifcowl_thread = None;
    if output_format == OutputFormat::Turtle && emit_ifcowl {
        let receiver = ifcowl_receiver
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing IfcOWL receiver for turtle sidecar mode"))?;
        let path = resolve_ifcowl_path(args.output_file.as_deref(), input_path);
        let ifcowl_base = base_options.base_uri.clone();
        ifcowl_thread = Some(thread::spawn(move || -> anyhow::Result<()> {
            let file = File::create(&path).with_context(|| {
                format!("failed to create IfcOWL output file {}", path.display())
            })?;
            let writer = BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, file);
            serialize_turtle_batches_to_writer(receiver, writer, Some(&ifcowl_base))
                .with_context(|| format!("failed to write IfcOWL Turtle to {}", path.display()))?;
            Ok(())
        }));
    }

    let producer_start = Instant::now();
    // Run IfcOWL in a background thread (if requested), then sequentially run the
    // active LBD producer modules.  This mirrors the WASM path so both produce
    // identical triples.
    let lbd_base = normalize_base_uri(&base_options.base_uri);
    let ifcowl_result: anyhow::Result<()> = if let Some(sender) = ifcowl_sender.as_ref() {
        let sender = sender.clone();
        let schema = step.header.schema;
        let base_clone = lbd_base.clone();
        let batch_size = base_options.stream_batch_size;
        let max_workers = base_options.ifcowl_max_workers;
        let step_ref = &step;
        std::thread::scope(|scope| -> anyhow::Result<()> {
            let ifcowl_handle = scope.spawn(move || {
                stream_ifcowl(step_ref, &base_clone, schema, &sender, batch_size, max_workers)
                    .map_err(|e| anyhow::anyhow!("ifcowl streaming failed: {:?}", e))
            });
            if active_plugins.contains(lbd_pipeline::BOT_PRODUCER_ID) {
                stream_bot(&model, &base_options, &converter_lbd_sender)
                    .map_err(|e| anyhow::anyhow!("bot streaming failed: {:?}", e))?;
            }
            if active_plugins.contains(lbd_pipeline::BEO_PRODUCER_ID) {
                stream_beo(&model, &base_options, &converter_lbd_sender)
                    .map_err(|e| anyhow::anyhow!("beo streaming failed: {:?}", e))?;
            }
            if active_plugins.contains(lbd_pipeline::PROPS_OPM_PRODUCER_ID) {
                stream_props_opm(&model, &base_options, &converter_lbd_sender)
                    .map_err(|e| anyhow::anyhow!("props-opm streaming failed: {:?}", e))?;
            }
            if active_plugins.contains(lbd_pipeline::OMG_FOG_PRODUCER_ID) {
                stream_omg_fog(&model, &base_options, &converter_lbd_sender)
                    .map_err(|e| anyhow::anyhow!("omg-fog streaming failed: {:?}", e))?;
            }
            ifcowl_handle
                .join()
                .map_err(|_| anyhow::anyhow!("ifcowl thread panicked"))?
        })
    } else {
        if active_plugins.contains(lbd_pipeline::BOT_PRODUCER_ID) {
            stream_bot(&model, &base_options, &converter_lbd_sender)
                .map_err(|e| anyhow::anyhow!("bot streaming failed: {:?}", e))?;
        }
        if active_plugins.contains(lbd_pipeline::BEO_PRODUCER_ID) {
            stream_beo(&model, &base_options, &converter_lbd_sender)
                .map_err(|e| anyhow::anyhow!("beo streaming failed: {:?}", e))?;
        }
        if active_plugins.contains(lbd_pipeline::PROPS_OPM_PRODUCER_ID) {
            stream_props_opm(&model, &base_options, &converter_lbd_sender)
                .map_err(|e| anyhow::anyhow!("props-opm streaming failed: {:?}", e))?;
        }
        if active_plugins.contains(lbd_pipeline::OMG_FOG_PRODUCER_ID) {
            stream_omg_fog(&model, &base_options, &converter_lbd_sender)
                .map_err(|e| anyhow::anyhow!("omg-fog streaming failed: {:?}", e))?;
        }
        Ok(())
    };
    ifcowl_result.context("failed to run core conversion producer modules")?;
    tracing::info!(
        "phase triple_production completed in {:.3}s",
        producer_start.elapsed().as_secs_f64()
    );
    drop(converter_lbd_sender);
    drop(ifcowl_sender);

    let serializer_join_start = Instant::now();
    lbd_thread
        .join()
        .map_err(|_| anyhow::anyhow!("LBD serializer thread panicked"))??;

    if let Some(thread) = ifcowl_thread {
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("IfcOWL serializer thread panicked"))??;
    }
    tracing::info!(
        "phase serializer_join completed in {:.3}s",
        serializer_join_start.elapsed().as_secs_f64()
    );
    tracing::info!("run completed in {:.3}s", run_start.elapsed().as_secs_f64());

    Ok(())
}

fn print_pipeline_modules(registry: &lbd_pipeline::PluginRegistry) {
    for manifest in registry.manifests() {
        println!(
            "{:?}\t{}\t{}\tparallel={:?}\tfailure={:?}\twasm={}",
            manifest.stage,
            manifest.id,
            manifest.display_name,
            manifest.parallelism,
            manifest.failure_policy,
            manifest.wasm_compatible
        );
    }
}

fn print_module_plan(
    registry: &lbd_pipeline::PluginRegistry,
    plan: &lbd_pipeline::ActivationPlan,
    configs: &HashMap<String, HashMap<String, String>>,
) {
    println!("Enabled modules:");
    for id in &plan.enabled_ids {
        if let Some(plugin) = registry.plugin(id) {
            let manifest = plugin.manifest();
            println!(
                "  {:?}\t{}\t{}\tparallel={:?}\tfailure={:?}\twasm={}",
                manifest.stage,
                manifest.id,
                manifest.display_name,
                manifest.parallelism,
                manifest.failure_policy,
                manifest.wasm_compatible
            );
        } else {
            println!("  unknown\t{}\t(unregistered)", id);
        }
        if let Some(entries) = configs.get(id) {
            for (key, value) in entries {
                println!("    config\t{}={}", key, value);
            }
        }
    }
}

fn build_requested_module_list(args: &Args) -> Vec<String> {
    let requested: Vec<String> = args.module.clone();

    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for id in requested {
        if seen.insert(id.clone()) {
            deduped.push(id);
        }
    }
    deduped
}

fn parse_module_configs(
    values: &[String],
) -> Result<HashMap<String, HashMap<String, String>>, String> {
    let mut by_plugin: HashMap<String, HashMap<String, String>> = HashMap::new();
    for raw in values {
        let (plugin, rest) = raw
            .split_once('.')
            .ok_or_else(|| format!("expected `<module-id>.<key>=<value>`, got `{}`", raw))?;
        let (key, value) = rest
            .split_once('=')
            .ok_or_else(|| format!("expected `<key>=<value>` in `{}`", raw))?;
        if plugin.is_empty() || key.is_empty() {
            return Err(format!(
                "module id and key must be non-empty in module config `{}`",
                raw
            ));
        }
        by_plugin
            .entry(plugin.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
    }
    Ok(by_plugin)
}

fn validate_module_configs(
    plan: &lbd_pipeline::ActivationPlan,
    configs: &HashMap<String, HashMap<String, String>>,
) -> anyhow::Result<()> {
    let active: HashSet<&str> = plan.enabled_ids.iter().map(|id| id.as_str()).collect();
    for plugin_id in configs.keys() {
        if !active.contains(plugin_id.as_str()) {
            anyhow::bail!(
                "module options provided for `{}` but module is not active; add `--module {}`",
                plugin_id,
                plugin_id
            );
        }
    }
    Ok(())
}

fn validate_typed_module_configs(
    configs: &HashMap<String, HashMap<String, String>>,
) -> Result<(), String> {
    for (module_id, entries) in configs {
        if module_id == lbd_pipeline::NQUADS_SERIALIZER_ID {
            validate_nquads_serializer_module_config(entries)?;
        }
        if module_id == lbd_pipeline::NQUADS_CHUNKED_SERIALIZER_ID {
            validate_nquads_chunked_serializer_module_config(entries)?;
        }
        if module_id == lbd_pipeline::TURTLE_SERIALIZER_ID {
            validate_turtle_serializer_module_config(entries)?;
        }
    }
    Ok(())
}

fn validate_activation_plan_with_args(
    plan: &lbd_pipeline::ActivationPlan,
    settings: &ExecutionSettings,
) -> anyhow::Result<()> {
    let active: HashSet<&str> = plan.enabled_ids.iter().map(|id| id.as_str()).collect();
    let output_format = settings.output_format;
    if active.contains(lbd_pipeline::GRAFEO_EXPORT_ID) {
        if output_format != OutputFormat::Nquads {
            anyhow::bail!("grafeo export module requires `neo-nquads-serializer`");
        }
        if settings.nquads.chunking != chunk_writer::QuadChunkingMode::None {
            anyhow::bail!("grafeo export module cannot be combined with N-Quads chunking");
        }
    }
    let has_any_producer = active.contains(lbd_pipeline::BOT_PRODUCER_ID)
        || active.contains(lbd_pipeline::BEO_PRODUCER_ID)
        || active.contains(lbd_pipeline::PROPS_OPM_PRODUCER_ID)
        || active.contains(lbd_pipeline::OMG_FOG_PRODUCER_ID);
    if !has_any_producer {
        anyhow::bail!(
            "module plan must include at least one LBD producer (`{}`, `{}`, or `{}`)",
            lbd_pipeline::BOT_PRODUCER_ID,
            lbd_pipeline::BEO_PRODUCER_ID,
            lbd_pipeline::PROPS_OPM_PRODUCER_ID,
        );
    }
    let has_file_export = active.contains(lbd_pipeline::FILE_EXPORT_ID);
    let has_log_export = active.contains(lbd_pipeline::LOG_EXPORT_ID);
    let has_stdout_export = active.contains(lbd_pipeline::STDOUT_EXPORT_ID);
    let has_grafeo_export = active.contains(lbd_pipeline::GRAFEO_EXPORT_ID);
    let export_count =
        has_file_export as usize + has_log_export as usize + has_stdout_export as usize + has_grafeo_export as usize;
    if export_count != 1 {
        anyhow::bail!(
            "module plan must include exactly one export module (`{}`, `{}`, `{}`, or `{}`)",
            lbd_pipeline::FILE_EXPORT_ID,
            lbd_pipeline::LOG_EXPORT_ID,
            lbd_pipeline::STDOUT_EXPORT_ID,
            lbd_pipeline::GRAFEO_EXPORT_ID
        );
    }
    Ok(())
}

fn resolve_execution_settings(
    _args: &Args,
    plan: &lbd_pipeline::ActivationPlan,
    configs: &HashMap<String, HashMap<String, String>>,
) -> anyhow::Result<ExecutionSettings> {
    let active: HashSet<&str> = plan.enabled_ids.iter().map(|id| id.as_str()).collect();
    let has_nquads = active.contains(lbd_pipeline::NQUADS_SERIALIZER_ID);
    let has_nquads_chunked = active.contains(lbd_pipeline::NQUADS_CHUNKED_SERIALIZER_ID);
    let has_turtle = active.contains(lbd_pipeline::TURTLE_SERIALIZER_ID);
    let output_format = match (has_turtle, has_nquads || has_nquads_chunked) {
        (true, false) => OutputFormat::Turtle,
        (false, true) => OutputFormat::Nquads,
        (true, true) => anyhow::bail!(
            "conflicting serializer modules enabled (`{}` and N-Quads serializer)",
            lbd_pipeline::TURTLE_SERIALIZER_ID,
        ),
        (false, false) => anyhow::bail!(
            "no serializer module enabled; add `--module {}`, `--module {}`, or `--module {}`",
            lbd_pipeline::TURTLE_SERIALIZER_ID,
            lbd_pipeline::NQUADS_SERIALIZER_ID,
            lbd_pipeline::NQUADS_CHUNKED_SERIALIZER_ID,
        ),
    };

    // For the chunked serializer the options live under its own namespace and
    // chunking defaults to "lines". For the plain serializer the default is "none".
    let nquads_entries = configs.get(lbd_pipeline::NQUADS_SERIALIZER_ID);
    let chunked_entries = configs.get(lbd_pipeline::NQUADS_CHUNKED_SERIALIZER_ID);
    let (effective_entries, default_chunking) = if has_nquads_chunked {
        (chunked_entries, "lines")
    } else {
        (nquads_entries, "none")
    };
    let chunking = parse_quad_chunking(
        effective_entries
            .and_then(|e| e.get("chunking"))
            .map(String::as_str)
            .or(Some(default_chunking)),
    )?;
    let chunk_size_lines =
        parse_usize_with_default(effective_entries, "chunk_size_lines", 2_000_000usize)?;
    let chunk_size_bytes =
        parse_usize_with_default(effective_entries, "chunk_size_bytes", 268_435_456usize)?;
    let chunk_prefix = string_with_default(effective_entries, "chunk_prefix", "out");
    let chunk_min_count = parse_usize_with_default(effective_entries, "chunk_min_count", 1usize)?;
    let chunk_core_count = parse_optional_usize(effective_entries, "chunk_core_count")?;

    let turtle_entries = configs.get(lbd_pipeline::TURTLE_SERIALIZER_ID);
    let turtle_grouping = match turtle_entries
        .and_then(|e| e.get("grouping"))
        .map(String::as_str)
        .unwrap_or("sorted")
    {
        "sorted" => TurtleGrouping::Sorted,
        "streaming" => TurtleGrouping::Streaming,
        other => anyhow::bail!(
            "invalid `neo-turtle-serializer.grouping={}` (expected sorted|streaming)",
            other
        ),
    };

    Ok(ExecutionSettings {
        output_format,
        emit_ifcowl: active.contains(lbd_pipeline::IFCOWL_PRODUCER_ID),
        nquads: NquadsModuleOptions {
            chunking,
            chunk_size_lines,
            chunk_size_bytes,
            chunk_prefix,
            chunk_min_count,
            chunk_core_count,
        },
        turtle_grouping,
    })
}

fn parse_quad_chunking(raw: Option<&str>) -> anyhow::Result<chunk_writer::QuadChunkingMode> {
    let value = raw.unwrap_or("none");
    match value {
        "none" => Ok(chunk_writer::QuadChunkingMode::None),
        "lines" => Ok(chunk_writer::QuadChunkingMode::Lines),
        "bytes" => Ok(chunk_writer::QuadChunkingMode::Bytes),
        "cores" => Ok(chunk_writer::QuadChunkingMode::Cores),
        _ => anyhow::bail!(
            "invalid `neo-nquads-serializer.chunking={}` (expected none|lines|bytes|cores)",
            value
        ),
    }
}

fn parse_usize_with_default(
    entries: Option<&HashMap<String, String>>,
    key: &str,
    default: usize,
) -> anyhow::Result<usize> {
    match entries.and_then(|m| m.get(key)) {
        Some(raw) => raw
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("invalid integer for `{}`: `{}`", key, raw)),
        None => Ok(default),
    }
}

fn parse_optional_usize(
    entries: Option<&HashMap<String, String>>,
    key: &str,
) -> anyhow::Result<Option<usize>> {
    match entries.and_then(|m| m.get(key)) {
        Some(raw) => raw
            .parse::<usize>()
            .map(Some)
            .map_err(|_| anyhow::anyhow!("invalid integer for `{}`: `{}`", key, raw)),
        None => Ok(None),
    }
}

fn string_with_default(
    entries: Option<&HashMap<String, String>>,
    key: &str,
    default: &str,
) -> String {
    entries
        .and_then(|m| m.get(key))
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn validate_nquads_serializer_module_config(
    entries: &HashMap<String, String>,
) -> Result<(), String> {
    for (key, value) in entries {
        match key.as_str() {
            "chunking" => {
                if !matches!(value.as_str(), "none" | "lines" | "bytes" | "cores") {
                    return Err(format!(
                        "`neo-nquads-serializer.chunking` must be one of none|lines|bytes|cores, got `{}`",
                        value
                    ));
                }
            }
            "chunk_size_lines" | "chunk_size_bytes" | "chunk_min_count" | "chunk_core_count" => {
                let parsed = value.parse::<usize>().map_err(|_| {
                    format!(
                        "`neo-nquads-serializer.{}` must be an integer, got `{}`",
                        key, value
                    )
                })?;
                if parsed == 0 {
                    return Err(format!("`neo-nquads-serializer.{}` must be > 0", key));
                }
            }
            "chunk_prefix" => {}
            other => {
                return Err(format!(
                    "unknown option `neo-nquads-serializer.{}` (supported: chunking, chunk_size_lines, chunk_size_bytes, chunk_prefix, chunk_min_count, chunk_core_count)",
                    other
                ));
            }
        }
    }
    Ok(())
}

fn validate_nquads_chunked_serializer_module_config(
    entries: &HashMap<String, String>,
) -> Result<(), String> {
    for (key, value) in entries {
        match key.as_str() {
            "chunking" => {
                if !matches!(value.as_str(), "none" | "lines" | "bytes" | "cores") {
                    return Err(format!(
                        "`neo-nquads-chunked-serializer.chunking` must be one of none|lines|bytes|cores, got `{}`",
                        value
                    ));
                }
            }
            "chunk_size_lines" | "chunk_size_bytes" | "chunk_min_count" | "chunk_core_count" => {
                let parsed = value.parse::<usize>().map_err(|_| {
                    format!(
                        "`neo-nquads-chunked-serializer.{}` must be an integer, got `{}`",
                        key, value
                    )
                })?;
                if parsed == 0 {
                    return Err(format!(
                        "`neo-nquads-chunked-serializer.{}` must be > 0",
                        key
                    ));
                }
            }
            "chunk_prefix" => {}
            other => {
                return Err(format!(
                    "unknown option `neo-nquads-chunked-serializer.{}` (supported: chunking, chunk_size_lines, chunk_size_bytes, chunk_prefix, chunk_min_count, chunk_core_count)",
                    other
                ));
            }
        }
    }
    Ok(())
}

fn validate_turtle_serializer_module_config(
    entries: &HashMap<String, String>,
) -> Result<(), String> {
    for (key, value) in entries {
        match key.as_str() {
            "grouping" => {
                if !matches!(value.as_str(), "sorted" | "streaming") {
                    return Err(format!(
                        "`neo-turtle-serializer.grouping` must be one of sorted|streaming, got `{}`",
                        value
                    ));
                }
            }
            other => {
                return Err(format!(
                    "unknown option `neo-turtle-serializer.{}` (supported: grouping)",
                    other
                ));
            }
        }
    }
    Ok(())
}

fn resolve_ifcowl_path(output_file: Option<&Path>, input_file: &Path) -> PathBuf {
    if let Some(path) = output_file {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("lbd_output");
        return parent.join(format!("{stem}_ifcowl.ttl"));
    }
    let stem = input_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("ifc_output");
    PathBuf::from(format!("{stem}_ifcowl.ttl"))
}

fn normalize_base_for_graph_iri(base_uri: &str) -> String {
    base_uri.trim_end_matches('/').to_string()
}

fn validate_args(args: &Args, settings: &ExecutionSettings) -> anyhow::Result<()> {
    if !args.list_modules && !args.show_module_plan && args.input.is_none() {
        anyhow::bail!("an IFC input path is required");
    }
    if args.module.is_empty() {
        anyhow::bail!(
            "no modules selected; add explicit modules via `--module <id>` (see `--list-modules`)"
        );
    }
    if settings.output_format != OutputFormat::Nquads
        && settings.nquads.chunking != chunk_writer::QuadChunkingMode::None
    {
        anyhow::bail!("N-Quads chunking options require `neo-nquads-serializer` to be active");
    }
    if settings.nquads.chunk_size_lines == 0 {
        anyhow::bail!("`neo-nquads-serializer.chunk_size_lines` must be > 0");
    }
    if settings.nquads.chunk_size_bytes == 0 {
        anyhow::bail!("`neo-nquads-serializer.chunk_size_bytes` must be > 0");
    }
    if settings.nquads.chunk_min_count == 0 {
        anyhow::bail!("`neo-nquads-serializer.chunk_min_count` must be > 0");
    }
    if settings
        .nquads
        .chunk_core_count
        .is_some_and(|count| count == 0)
    {
        anyhow::bail!("`neo-nquads-serializer.chunk_core_count` must be > 0 when provided");
    }
    if settings.nquads.chunking != chunk_writer::QuadChunkingMode::Cores
        && settings.nquads.chunk_core_count.is_some()
    {
        anyhow::bail!("`neo-nquads-serializer.chunk_core_count` is only valid when chunking=cores");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_requested_module_list,
        chunk_writer::{self, QuadChunkWriter, QuadChunkingMode},
        parse_module_configs, validate_args, Args, ExecutionSettings, NquadsModuleOptions,
        OutputFormat, TurtleGrouping,
    };
    use clap::Parser;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn cli_defaults_are_minimal() {
        let args = Args::try_parse_from(["ifc2lbd-neo", "input.ifc"]).expect("parse");
        assert!(args.module.is_empty());
        assert!(args.module_opt.is_empty());
    }

    #[test]
    fn cli_parses_new_flags() {
        let args = Args::try_parse_from([
            "ifc2lbd-neo",
            "input.ifc",
            "--module",
            "neo-bot-producer",
            "--module",
            "neo-nquads-serializer",
            "--output",
            "out.ttl",
            "--base-uri",
            "https://example.test/base/",
            "--module-opt",
            "neo-nquads-serializer.chunking=cores",
            "--module-opt",
            "neo-nquads-serializer.chunk_size_lines=999",
        ])
        .expect("parse");
        assert_eq!(args.output_file.as_deref(), Some(Path::new("out.ttl")));
        assert_eq!(args.base_uri, "https://example.test/base/");
        assert_eq!(
            args.module,
            vec![
                "neo-bot-producer".to_string(),
                "neo-nquads-serializer".to_string()
            ]
        );
        let cfg = parse_module_configs(&args.module_opt).expect("module cfg");
        assert_eq!(
            cfg.get("neo-nquads-serializer")
                .and_then(|m| m.get("chunking"))
                .map(String::as_str),
            Some("cores")
        );
    }

    #[test]
    fn module_selection_is_required() {
        let args = Args::try_parse_from(["ifc2lbd-neo", "input.ifc"]).expect("parse");
        let settings = test_settings(OutputFormat::Turtle);
        let err =
            validate_args(&args, &settings).expect_err("must reject missing module selection");
        assert!(err.to_string().contains("no modules selected"));
    }

    #[test]
    fn requested_modules_are_explicit() {
        let args = Args::try_parse_from([
            "ifc2lbd-neo",
            "input.ifc",
            "--module",
            "neo-bot-producer",
            "--module",
            "neo-topology-full-producer",
            "--module",
            "neo-nquads-serializer",
        ])
        .expect("parse");
        let requested = build_requested_module_list(&args);
        assert!(requested.contains(&"neo-bot-producer".to_string()));
        assert!(requested.contains(&"neo-topology-full-producer".to_string()));
        assert!(requested.contains(&"neo-nquads-serializer".to_string()));
    }


    #[test]
    fn quad_chunk_writer_rotates_and_writes_manifest() {
        let out_dir = unique_temp_dir("quad_chunk_writer_test");
        std::fs::create_dir_all(&out_dir).expect("mkdir");

        let mut writer = chunk_writer::QuadChunkWriter::new(
            out_dir.clone(),
            "test".to_string(),
            chunk_writer::QuadChunkingMode::Lines,
            2,
            1024,
            1,
            None,
        )
        .expect("new writer");
        writer
            .write_all(b"<s1> <p> <o> <g> .\n<s2> <p> <o> <g> .\n<s3> <p> <o> <g> .\n")
            .expect("write");
        writer.finish().expect("finish");

        let c0 = out_dir.join("test.part-000.nq");
        let c1 = out_dir.join("test.part-001.nq");
        let manifest = out_dir.join("test.manifest.json");
        assert!(c0.exists());
        assert!(c1.exists());
        assert!(manifest.exists());
        let c0_lines = std::fs::read_to_string(c0).expect("chunk0").lines().count();
        let c1_lines = std::fs::read_to_string(c1).expect("chunk1").lines().count();
        assert_eq!(c0_lines, 2);
        assert_eq!(c1_lines, 1);

        std::fs::remove_dir_all(&out_dir).ok();
    }

    #[test]
    fn quad_chunk_writer_cores_mode_writes_target_chunk_count() {
        let out_dir = unique_temp_dir("quad_chunk_writer_cores_test");
        std::fs::create_dir_all(&out_dir).expect("mkdir");

        let mut writer = chunk_writer::QuadChunkWriter::new(
            out_dir.clone(),
            "core".to_string(),
            chunk_writer::QuadChunkingMode::Cores,
            2_000_000,
            268_435_456,
            1,
            Some(3),
        )
        .expect("new writer");
        writer
            .write_all(b"<s1> <p> <o> <g> .\n<s2> <p> <o> <g> .\n<s3> <p> <o> <g> .\n")
            .expect("write");
        writer.finish().expect("finish");

        for idx in 0..3 {
            let path = out_dir.join(format!("core.part-{idx:03}.nq"));
            assert!(path.exists(), "missing {}", path.display());
        }
        let line_counts: Vec<usize> = (0..3)
            .map(|idx| out_dir.join(format!("core.part-{idx:03}.nq")))
            .map(|path| {
                std::fs::read_to_string(path)
                    .expect("chunk")
                    .lines()
                    .count()
            })
            .collect();
        let total_lines: usize = line_counts.iter().sum();
        assert_eq!(total_lines, 3);
        let non_empty = line_counts.iter().filter(|count| **count > 0).count();
        assert_eq!(non_empty, 1);

        std::fs::remove_dir_all(&out_dir).ok();
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{now}"))
    }

    fn test_settings(output_format: OutputFormat) -> ExecutionSettings {
        ExecutionSettings {
            output_format,
            emit_ifcowl: false,
            nquads: NquadsModuleOptions {
                chunking: chunk_writer::QuadChunkingMode::None,
                chunk_size_lines: 2_000_000,
                chunk_size_bytes: 268_435_456,
                chunk_prefix: "out".to_string(),
                chunk_min_count: 1,
                chunk_core_count: None,
            },
            turtle_grouping: TurtleGrouping::Sorted,
        }
    }
}
