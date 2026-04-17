//! Provides the command-line interface and end-to-end orchestration of the pipeline.
//! Parses flags and modes (e.g., output format, chunking options, --topology, --topology-full, --bbox).
//! Wires together parsing, modeling, conversion, topology, geometry, and serialization components according to user-selected options, while keeping stages clearly separated and extensible.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use clap::{Parser, ValueEnum};
use ifc_model::build_model;
use ifc_model::IfcModel;
use ifc_step::{parse_step_file, EntityId, StepFile};
use lbd_converter::ConvertOptions;
use lbd_geometry::{BoundingBox, ExactCheckOptions, GeometryRelation, GeometryRelationKind};
use lbd_pipeline::FailurePolicy;
use lbd_serializer::{
    serialize_lbd_batches_incremental_to_writer, serialize_lbd_batches_to_writer,
    serialize_nquads_batches_to_writer, serialize_nquads_merged_batches_to_writer,
    serialize_turtle_batches_to_writer,
};

mod bbox;
mod chunk_writer;
mod kernel;
mod mesh;
mod pipeline_plugins;
mod producer_plugins;
mod topology_plugin;
mod transform;
mod voxel;

const SERIALIZER_CHANNEL_CAPACITY: usize = 32;
const SERIALIZER_BUFFER_BYTES: usize = 1024 * 1024;
const CORE_CHUNK_BLOCK_LINES: u64 = 4096;
const CORE_CHUNK_BATCH_BYTES: usize = 4 * 1024 * 1024;
const MIN_CORE_CHUNK_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CORE_CHUNK_BYTES: u64 = 512 * 1024 * 1024;
const IFC_TO_NQ_ESTIMATE_MULTIPLIER: u64 = 32;
const IFCOWL_TO_NQ_ESTIMATE_MULTIPLIER: u64 = 28;
const LBD_TO_NQ_ESTIMATE_MULTIPLIER: u64 = 2;
const TOPOLOGY_FULL_TO_NQ_ESTIMATE_MULTIPLIER: u64 = 1;

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
    lbd_graph_iri: Option<String>,
    ifcowl_graph_iri: Option<String>,
    chunking: chunk_writer::QuadChunkingMode,
    chunk_size_lines: usize,
    chunk_size_bytes: usize,
    chunk_prefix: String,
    chunk_min_count: usize,
    chunk_core_count: Option<usize>,
}

#[derive(Clone, Debug)]
struct BboxModuleOptions {
    inflation_threshold: f64,
    report_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct ExecutionSettings {
    output_format: OutputFormat,
    emit_ifcowl: bool,
    bbox: Option<BboxModuleOptions>,
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
    let lbd_graph_iri = settings
        .nquads
        .lbd_graph_iri
        .clone()
        .unwrap_or_else(|| format!("{normalized_base}/lbd"));
    let ifcowl_graph_iri = settings
        .nquads
        .ifcowl_graph_iri
        .clone()
        .unwrap_or_else(|| format!("{normalized_base}/ifcowl"));

    let parse_start = Instant::now();
    let step = parse_step_file(input_path)
        .with_context(|| format!("failed to parse STEP file {}", input_path.display()))?;
    tracing::info!(
        "phase parse_step_file completed in {:.3}s",
        parse_start.elapsed().as_secs_f64()
    );
    let build_start = Instant::now();
    let model = build_model(&step).context("failed to build IFC model")?;
    tracing::info!(
        "phase build_model completed in {:.3}s",
        build_start.elapsed().as_secs_f64()
    );

    let active_topology_plugin_ids: Vec<String> = activation_plan
        .enabled_ids
        .iter()
        .filter_map(|id| {
            built_in_registry
                .plugin(id)
                .map(|plugin| (id, plugin.manifest()))
        })
        .filter(|(_, manifest)| {
            manifest.stage == lbd_pipeline::PipelineStage::Produce
                && manifest
                    .outputs
                    .iter()
                    .any(|output| *output == "topology-triples")
        })
        .map(|(id, _)| id.clone())
        .collect();
    let topology_enabled = !active_topology_plugin_ids.is_empty();
    let derive_adjacency = active_topology_plugin_ids
        .iter()
        .any(|id| topology_plugin::plugin_requires_geometry_relations(id));
    let topology_graph_iri = format!("{normalized_base}/topology");

    let mut geometry_bounding_boxes: Option<Arc<HashMap<EntityId, BoundingBox>>> = None;
    let mut geometry_wkts: Option<Arc<HashMap<EntityId, String>>> = None;
    if settings.bbox.is_some() {
        let bbox_settings = settings
            .bbox
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing bbox module settings"))?;
        let bbox_start = Instant::now();
        let (mesh_bboxes, mesh_wkts, bbox_report) = bbox::collect_mesh_bounding_boxes_hybrid(
            &step,
            model.elements.keys().copied().collect(),
            bbox_settings.inflation_threshold,
        );
        tracing::info!(
            "bbox extraction produced {} bboxes in {:.3}s (exact escalations: {} / {}, avg inflation fast/final: {:.3}/{:.3}, max fast/final: {:.3}/{:.3})",
            mesh_bboxes.len(),
            bbox_start.elapsed().as_secs_f64(),
            bbox_report.escalated_exact_count,
            bbox_report.elements_with_mesh,
            bbox_report.avg_inflation_fast,
            bbox_report.avg_inflation_final,
            bbox_report.max_inflation_fast,
            bbox_report.max_inflation_final,
        );
        if let Some(path) = bbox_settings.report_path.as_ref() {
            let report_json = serde_json::to_string_pretty(&bbox_report)
                .context("failed to serialize bbox report JSON")?;
            std::fs::write(path, report_json)
                .with_context(|| format!("failed to write bbox report {}", path.display()))?;
        }
        geometry_bounding_boxes = Some(bbox::arc_bounding_boxes_from_raw(mesh_bboxes));
        geometry_wkts = Some(Arc::new(mesh_wkts));
    }

    let producer_plan =
        build_producer_execution_plan(output_format, active_topology_plugin_ids.len());
    if active_topology_plugin_ids.len() > 1 && !producer_plan.parallel_topology_plugin {
        anyhow::bail!(
            "multiple topology producer modules require `--output-format nquads` so they can run in parallel"
        );
    }
    if !active_topology_plugin_ids.is_empty() {
        tracing::info!(
            "topology producer modules selected: {} (parallel_nquads_mode={})",
            active_topology_plugin_ids.join(", "),
            producer_plan.parallel_topology_plugin
        );
    }

    let base_options = ConvertOptions {
        base_uri: args.base_uri,
        emit_ifcowl_links: emit_ifcowl,
        enable_topology: if producer_plan.parallel_topology_plugin {
            false
        } else {
            topology_enabled
        },
        enable_topology_extension: if producer_plan.parallel_topology_plugin {
            false
        } else {
            derive_adjacency
        },
        topology_only: false,
        suppress_non_topology_fallback: producer_plan.parallel_topology_plugin,
        geometry_relations: None,
        geometry_bounding_boxes: geometry_bounding_boxes.clone(),
        geometry_wkts: geometry_wkts.clone(),
        geometry_tolerance: args.geometry_tolerance,
        low_memory_mode: false,
        stream_batch_size: 8 * 1024,
        ifcowl_max_workers: 16,
    };

    let (converter_lbd_sender, converter_lbd_receiver) =
        crossbeam::channel::bounded(SERIALIZER_CHANNEL_CAPACITY);
    let lbd_receiver = converter_lbd_receiver;

    let (ifcowl_sender, mut ifcowl_receiver) = if emit_ifcowl {
        let (sender, receiver) = crossbeam::channel::bounded(SERIALIZER_CHANNEL_CAPACITY);
        (Some(sender), Some(receiver))
    } else {
        (None, None)
    };
    let (topology_sender, mut topology_receiver) = if producer_plan.parallel_topology_plugin {
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
    let topology_chunk_core_count =
        chunk_writer::resolve_effective_core_chunk_count_for_estimated_bytes(
            settings.nquads.chunking,
            settings.nquads.chunk_core_count,
            settings.nquads.chunk_min_count,
            if derive_adjacency {
                input_file_size_bytes.saturating_mul(TOPOLOGY_FULL_TO_NQ_ESTIMATE_MULTIPLIER)
            } else {
                (input_file_size_bytes / 8).max(1)
            },
        );
    if settings.nquads.chunking == chunk_writer::QuadChunkingMode::Cores {
        tracing::info!(
            "core chunk targets (auto): ifcowl={}, lbd={}, topology={}",
            ifcowl_chunk_core_count.unwrap_or(1),
            lbd_chunk_core_count.unwrap_or(1),
            topology_chunk_core_count.unwrap_or(1),
        );
    }
    let merged_ifcowl_receiver = if output_format == OutputFormat::Nquads {
        ifcowl_receiver.take()
    } else {
        None
    };
    let merged_topology_receiver = if output_format == OutputFormat::Nquads {
        topology_receiver.take()
    } else {
        None
    };
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
                    let mut topology_chunk_writer = if merged_topology_receiver.is_some() {
                        Some(chunk_writer::QuadChunkWriter::new(
                            chunk_writer::resolve_quad_chunk_output_dir(lbd_target.as_deref()),
                            format!("{}-topology", quad_chunk_prefix),
                            quad_chunking_mode,
                            quad_chunk_size_lines,
                            quad_chunk_size_bytes,
                            quad_chunk_min_count,
                            topology_chunk_core_count,
                        )?)
                    } else {
                        None
                    };

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
        let path = bbox::resolve_ifcowl_path(args.output_file.as_deref(), input_path);
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
    let write_topology_report = settings.bbox.is_none();
    let bbox_inflation_threshold = settings
        .bbox
        .as_ref()
        .map(|bbox| bbox.inflation_threshold)
        .unwrap_or(1.5);
    let bbox_report_path = settings
        .bbox
        .as_ref()
        .and_then(|bbox| bbox.report_path.clone());
    if producer_plan.parallel_topology_plugin {
        let model_ref = &model;
        let step_ref = &step;
        let converter_lbd_sender_clone = converter_lbd_sender.clone();
        let topology_sender_clone = topology_sender
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing topology sender for parallel topology mode"))?;
        let topology_base = base_options.base_uri.clone();
        let input_path_buf = input_path.to_path_buf();
        let geometry_tolerance = args.geometry_tolerance;
        let ifcowl_sender_clone = ifcowl_sender.clone();
        let mut producer_tasks = vec![ProducerTaskSpec {
            plugin_id: "neo-core-conversion".to_string(),
            failure_policy: FailurePolicy::Required,
            task: Box::new(move || {
                producer_plugins::run_core_conversion_plugin(
                    step_ref,
                    model_ref,
                    &base_options,
                    &converter_lbd_sender_clone,
                    ifcowl_sender_clone.as_ref(),
                )
                .context("failed to run core conversion producer module")?;
                Ok(())
            }),
        }];
        for plugin_id in &active_topology_plugin_ids {
            let plugin_id = plugin_id.clone();
            let failure_policy = resolve_failure_policy(&built_in_registry, &plugin_id);
            let topology_sender_clone = topology_sender_clone.clone();
            let topology_base = topology_base.clone();
            let input_path_buf = input_path_buf.clone();
            let bbox_report_path = bbox_report_path.clone();
            let module_config = module_configs.get(&plugin_id).cloned();
            producer_tasks.push(ProducerTaskSpec {
                plugin_id: plugin_id.clone(),
                failure_policy,
                task: Box::new(move || {
                    let topology_execution = topology_plugin::run_topology_plugin(
                        &plugin_id,
                        &topology_plugin::TopologyExecutionContext {
                            model: model_ref,
                            step: step_ref,
                            input_path: &input_path_buf,
                            geometry_tolerance,
                            bbox_inflation_threshold,
                            bbox_report_path: bbox_report_path.as_deref(),
                            write_report: write_topology_report,
                            module_config: module_config.as_ref(),
                        },
                    )?;
                    let topology_options = ConvertOptions {
                        base_uri: topology_base,
                        emit_ifcowl_links: false,
                        enable_topology: true,
                        enable_topology_extension: topology_execution.enable_topology_extension,
                        topology_only: true,
                        suppress_non_topology_fallback: false,
                        geometry_relations: topology_execution.geometry_relations,
                        geometry_bounding_boxes: None,
                        geometry_wkts: None,
                        geometry_tolerance,
                        low_memory_mode: false,
                        stream_batch_size: 8 * 1024,
                        ifcowl_max_workers: 16,
                    };
                    producer_plugins::run_topology_producer_plugin(
                        model_ref,
                        &topology_options,
                        &topology_sender_clone,
                    )
                    .with_context(|| {
                        format!("failed to run topology producer module `{}`", plugin_id)
                    })?;
                    Ok(())
                }),
            });
        }
        run_producer_plugin_tasks(producer_tasks)?;
    } else {
        let options = if derive_adjacency {
            let module_config = active_topology_plugin_ids
                .first()
                .and_then(|id| module_configs.get(id));
            let topology_execution = topology_plugin::run_topology_plugin(
                active_topology_plugin_ids
                    .first()
                    .map(String::as_str)
                    .ok_or_else(|| anyhow::anyhow!("missing topology plugin selection"))?,
                &topology_plugin::TopologyExecutionContext {
                    model: &model,
                    step: &step,
                    input_path,
                    geometry_tolerance: args.geometry_tolerance,
                    bbox_inflation_threshold,
                    bbox_report_path: bbox_report_path.as_deref(),
                    write_report: true,
                    module_config,
                },
            )?;
            ConvertOptions {
                base_uri: base_options.base_uri.clone(),
                emit_ifcowl_links: emit_ifcowl,
                enable_topology: topology_enabled,
                enable_topology_extension: topology_execution.enable_topology_extension,
                topology_only: false,
                suppress_non_topology_fallback: false,
                geometry_relations: topology_execution.geometry_relations,
                geometry_bounding_boxes: geometry_bounding_boxes.clone(),
                geometry_wkts: geometry_wkts.clone(),
                geometry_tolerance: args.geometry_tolerance,
                low_memory_mode: false,
                stream_batch_size: 8 * 1024,
                ifcowl_max_workers: 16,
            }
        } else {
            ConvertOptions {
                base_uri: base_options.base_uri.clone(),
                emit_ifcowl_links: emit_ifcowl,
                enable_topology: topology_enabled,
                enable_topology_extension: derive_adjacency,
                topology_only: false,
                suppress_non_topology_fallback: false,
                geometry_relations: None,
                geometry_bounding_boxes: geometry_bounding_boxes.clone(),
                geometry_wkts: geometry_wkts.clone(),
                geometry_tolerance: args.geometry_tolerance,
                low_memory_mode: false,
                stream_batch_size: 8 * 1024,
                ifcowl_max_workers: 16,
            }
        };

        producer_plugins::run_core_conversion_plugin(
            &step,
            &model,
            &options,
            &converter_lbd_sender,
            ifcowl_sender.as_ref(),
        )
        .context("failed to run core conversion producer module")?;
    }
    tracing::info!(
        "phase triple_production completed in {:.3}s",
        producer_start.elapsed().as_secs_f64()
    );
    drop(converter_lbd_sender);
    drop(ifcowl_sender);
    drop(topology_sender);

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
        topology_plugin::validate_typed_module_config(module_id, entries)?;
        if module_id == lbd_pipeline::NQUADS_SERIALIZER_ID {
            validate_nquads_serializer_module_config(entries)?;
        }
        if module_id == lbd_pipeline::BBOX_ENRICHER_ID {
            validate_bbox_module_config(entries)?;
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
    if !active.contains(lbd_pipeline::LBD_PRODUCER_ID) {
        anyhow::bail!(
            "module plan must include `{}`",
            lbd_pipeline::LBD_PRODUCER_ID
        );
    }
    let has_file_export = active.contains(lbd_pipeline::FILE_EXPORT_ID);
    let has_stdout_export = active.contains(lbd_pipeline::STDOUT_EXPORT_ID);
    let has_grafeo_export = active.contains(lbd_pipeline::GRAFEO_EXPORT_ID);
    let export_count =
        has_file_export as usize + has_stdout_export as usize + has_grafeo_export as usize;
    if export_count != 1 {
        anyhow::bail!(
            "module plan must include exactly one export module (`{}`, `{}`, or `{}`)",
            lbd_pipeline::FILE_EXPORT_ID,
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
    let has_turtle = active.contains(lbd_pipeline::TURTLE_SERIALIZER_ID);
    let output_format = match (has_turtle, has_nquads) {
        (true, false) => OutputFormat::Turtle,
        (false, true) => OutputFormat::Nquads,
        (true, true) => anyhow::bail!(
            "conflicting serializer modules enabled (`{}` and `{}`)",
            lbd_pipeline::TURTLE_SERIALIZER_ID,
            lbd_pipeline::NQUADS_SERIALIZER_ID
        ),
        (false, false) => anyhow::bail!(
            "no serializer module enabled; add `--module {}` or `--module {}`",
            lbd_pipeline::TURTLE_SERIALIZER_ID,
            lbd_pipeline::NQUADS_SERIALIZER_ID
        ),
    };

    let nquads_entries = configs.get(lbd_pipeline::NQUADS_SERIALIZER_ID);
    let chunking = parse_quad_chunking(
        nquads_entries
            .and_then(|e| e.get("chunking"))
            .map(String::as_str),
    )?;
    let chunk_size_lines =
        parse_usize_with_default(nquads_entries, "chunk_size_lines", 2_000_000usize)?;
    let chunk_size_bytes =
        parse_usize_with_default(nquads_entries, "chunk_size_bytes", 268_435_456usize)?;
    let chunk_prefix = string_with_default(nquads_entries, "chunk_prefix", "out");
    let chunk_min_count = parse_usize_with_default(nquads_entries, "chunk_min_count", 1usize)?;
    let chunk_core_count = parse_optional_usize(nquads_entries, "chunk_core_count")?;
    let lbd_graph_iri = nquads_entries.and_then(|e| e.get("lbd_graph_iri")).cloned();
    let ifcowl_graph_iri = nquads_entries
        .and_then(|e| e.get("ifcowl_graph_iri"))
        .cloned();

    let bbox = if active.contains(lbd_pipeline::BBOX_ENRICHER_ID) {
        let bbox_entries = configs.get(lbd_pipeline::BBOX_ENRICHER_ID);
        let inflation_threshold = parse_f64_with_default(bbox_entries, "inflation_threshold", 1.5)?;
        let report_path = bbox_entries
            .and_then(|entries| entries.get("report_path"))
            .map(PathBuf::from);
        Some(BboxModuleOptions {
            inflation_threshold,
            report_path,
        })
    } else {
        None
    };

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
        bbox,
        nquads: NquadsModuleOptions {
            lbd_graph_iri,
            ifcowl_graph_iri,
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

fn parse_f64_with_default(
    entries: Option<&HashMap<String, String>>,
    key: &str,
    default: f64,
) -> anyhow::Result<f64> {
    match entries.and_then(|m| m.get(key)) {
        Some(raw) => raw
            .parse::<f64>()
            .map_err(|_| anyhow::anyhow!("invalid float for `{}`: `{}`", key, raw)),
        None => Ok(default),
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
            "chunk_prefix" | "lbd_graph_iri" | "ifcowl_graph_iri" => {}
            other => {
                return Err(format!(
                    "unknown option `neo-nquads-serializer.{}` (supported: chunking, chunk_size_lines, chunk_size_bytes, chunk_prefix, chunk_min_count, chunk_core_count, lbd_graph_iri, ifcowl_graph_iri)",
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

fn validate_bbox_module_config(entries: &HashMap<String, String>) -> Result<(), String> {
    for (key, value) in entries {
        match key.as_str() {
            "inflation_threshold" => {
                let parsed = value.parse::<f64>().map_err(|_| {
                    format!(
                        "`neo-bbox-enricher.inflation_threshold` must be a float, got `{}`",
                        value
                    )
                })?;
                if parsed <= 0.0 {
                    return Err("`neo-bbox-enricher.inflation_threshold` must be > 0".to_string());
                }
            }
            "report_path" => {}
            other => {
                return Err(format!(
                    "unknown option `neo-bbox-enricher.{}` (supported: inflation_threshold, report_path)",
                    other
                ));
            }
        }
    }
    Ok(())
}

type ProducerTask<'a> = Box<dyn FnOnce() -> anyhow::Result<()> + Send + 'a>;

struct ProducerTaskSpec<'a> {
    plugin_id: String,
    failure_policy: FailurePolicy,
    task: ProducerTask<'a>,
}

#[derive(Clone, Copy, Debug)]
struct ProducerExecutionPlan {
    parallel_topology_plugin: bool,
}

fn build_producer_execution_plan(
    output_format: OutputFormat,
    topology_plugin_count: usize,
) -> ProducerExecutionPlan {
    ProducerExecutionPlan {
        parallel_topology_plugin: output_format == OutputFormat::Nquads
            && topology_plugin_count > 0,
    }
}

fn resolve_failure_policy(
    registry: &lbd_pipeline::PluginRegistry,
    plugin_id: &str,
) -> FailurePolicy {
    registry
        .plugin(plugin_id)
        .map(|plugin| plugin.manifest().failure_policy)
        .unwrap_or(FailurePolicy::Required)
}

fn run_producer_plugin_tasks<'a>(tasks: Vec<ProducerTaskSpec<'a>>) -> anyhow::Result<()> {
    std::thread::scope(|scope| -> anyhow::Result<()> {
        let mut handles = Vec::with_capacity(tasks.len());
        for spec in tasks {
            let ProducerTaskSpec {
                plugin_id,
                failure_policy,
                task,
            } = spec;
            let handle = scope.spawn(move || task());
            handles.push((plugin_id, failure_policy, handle));
        }
        let mut required_errors = Vec::new();
        for (plugin_id, failure_policy, handle) in handles {
            let result = handle
                .join()
                .map_err(|_| anyhow::anyhow!("producer module `{}` panicked", plugin_id));
            match (failure_policy, result) {
                (_, Ok(Ok(()))) => {}
                (FailurePolicy::Optional, Ok(Err(error))) => {
                    tracing::warn!(
                        "optional producer module `{}` failed and will be skipped: {}",
                        plugin_id,
                        error
                    );
                }
                (FailurePolicy::Optional, Err(error)) => {
                    tracing::warn!(
                        "optional producer module `{}` panicked and will be skipped: {}",
                        plugin_id,
                        error
                    );
                }
                (FailurePolicy::Required, Ok(Err(error))) => {
                    required_errors.push(anyhow::anyhow!(
                        "required producer module `{}` failed: {}",
                        plugin_id,
                        error
                    ));
                }
                (FailurePolicy::Required, Err(error)) => {
                    required_errors.push(anyhow::anyhow!(
                        "required producer module `{}` panicked: {}",
                        plugin_id,
                        error
                    ));
                }
            }
        }
        if let Some(error) = required_errors.into_iter().next() {
            return Err(error);
        }
        Ok(())
    })
}

pub(crate) fn topology_full_relations(
    model: &IfcModel,
    step: &StepFile,
    _input_path: &Path,
    geometry_tolerance: f64, /* used by future CSG boolean intersection */
    // used for future CSG boolean intersection,
    bbox_inflation_threshold: f64,
    _kernel_timeout: Duration,
    _max_pairs_per_batch: usize,
) -> anyhow::Result<(
    Vec<GeometryRelation>,
    HashMap<EntityId, [f64; 6]>,
    HashMap<EntityId, String>,
    bbox::BboxQualityReport,
)> {
    let (candidate_pairs, mut prefilter_bboxes) = bbox::rtree_candidate_pairs(model, step);
    let unique_elements = {
        let mut ids = HashSet::new();
        for (a, b) in &candidate_pairs {
            ids.insert(*a);
            ids.insert(*b);
        }
        ids
    };
    tracing::info!(
        "topology-full candidates: {} pairs across {} elements",
        candidate_pairs.len(),
        unique_elements.len(),
    );

    if candidate_pairs.is_empty() {
        let empty_report = bbox::BboxQualityReport {
            elements_requested: 0,
            elements_with_mesh: 0,
            escalated_exact_count: 0,
            rotated_bbox_count: 0,
            avg_inflation_fast: 0.0,
            max_inflation_fast: 0.0,
            avg_inflation_final: 0.0,
            max_inflation_final: 0.0,
            avg_escalated_reduction_ratio: 0.0,
            count_fast_over_1_2: 0,
            count_fast_over_1_5: 0,
            count_fast_over_1_8: 0,
            count_fast_over_2_0: 0,
            inflation_threshold: bbox_inflation_threshold,
            top_inflation_outliers: Vec::new(),
        };
        return Ok((Vec::new(), HashMap::new(), HashMap::new(), empty_report));
    }

    let mut sorted_element_ids: Vec<EntityId> = unique_elements.iter().copied().collect();
    sorted_element_ids.sort_unstable();
    let (mesh_bboxes, mesh_wkts, bbox_report) = bbox::collect_mesh_bounding_boxes_hybrid(
        step,
        sorted_element_ids,
        bbox_inflation_threshold,
    );

    for (eid, bbox) in mesh_bboxes.iter() {
        prefilter_bboxes.entry(*eid).or_insert(*bbox);
    }

    // 3-stage pipeline: R-tree broad-phase → voxel adjacency (fast surface detection)
    // → CSG boolean (exact volumetric check) → combine results.
    // Voxel + CSG replace the external OCC subprocess with pure Rust code that compiles to WASM.
    tracing::info!(
        "topology-full: running voxel adjacency ({} candidates)",
        candidate_pairs.len()
    );
    let (voxel_relations, _voxel_bboxes) =
        bbox::voxel_adjacency_relations_with_candidates(step, &candidate_pairs, 0.1, 50_000);
    tracing::info!(
        "topology-full: voxel adjacency produced {} relations",
        voxel_relations.len()
    );

    // Collect voxel relation set for dedup against CSG results
    let mut voxel_relation_set = HashSet::new();
    for r in &voxel_relations {
        if r.kind == GeometryRelationKind::IntersectingElement {
            let pair = if r.source < r.target {
                (r.source, r.target)
            } else {
                (r.target, r.source)
            };
            voxel_relation_set.insert(pair);
        }
    }

    // CSG pass: run boolean intersection on candidate pairs not already found by voxel.
    // Memory-bounded: processes pairs in batches, extracting only needed meshes per batch.
    let csg_tolerance = geometry_tolerance;
    tracing::info!(
        "topology-full: running CSG boolean on {} remaining candidate pairs",
        candidate_pairs.len()
    );
    let csg_options = ExactCheckOptions {
        tolerance: csg_tolerance,
    };
    let csg_relations = lbd_geometry::csg::derive_relations_with_csg(
        model,
        step,
        &candidate_pairs,
        &csg_options,
        &mesh_bboxes,
    );

    // Merge voxel + CSG results (deduplicated)
    let mut all_relations = Vec::with_capacity(voxel_relations.len() + csg_relations.len());
    let mut seen_relation_ids = HashSet::new();

    // Add voxel relations first
    for r in &voxel_relations {
        all_relations.push(r.clone());
        seen_relation_ids.insert((r.source, r.target, r.kind.clone()));
    }

    // Add CSG relations that aren't already in voxel set
    for r in &csg_relations {
        if seen_relation_ids.insert((r.source, r.target, r.kind.clone())) {
            all_relations.push(r.clone());
        }
    }

    tracing::info!(
        "topology-full: voxel {} + CSG {} = {} total relations",
        voxel_relations.len(),
        csg_relations.len(),
        all_relations.len()
    );

    return Ok((all_relations, mesh_bboxes, mesh_wkts, bbox_report));
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
            "neo-lbd-producer",
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
                "neo-lbd-producer".to_string(),
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
            "neo-lbd-producer",
            "--module",
            "neo-topology-full-producer",
            "--module",
            "neo-nquads-serializer",
        ])
        .expect("parse");
        let requested = build_requested_module_list(&args);
        assert!(requested.contains(&"neo-lbd-producer".to_string()));
        assert!(requested.contains(&"neo-topology-full-producer".to_string()));
        assert!(requested.contains(&"neo-nquads-serializer".to_string()));
    }

    #[test]
    fn core_chunk_count_is_capped_by_min_chunk_size_estimate() {
        // 10 MiB IFC => estimate 320 MiB => max 5 chunks at 64 MiB minimum.
        let effective = chunk_writer::resolve_effective_core_chunk_count(
            chunk_writer::QuadChunkingMode::Cores,
            Some(28),
            1,
            10 * 1024 * 1024,
        )
        .expect("effective");
        assert_eq!(effective, 5);
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
            bbox: None,
            nquads: NquadsModuleOptions {
                lbd_graph_iri: None,
                ifcowl_graph_iri: None,
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
