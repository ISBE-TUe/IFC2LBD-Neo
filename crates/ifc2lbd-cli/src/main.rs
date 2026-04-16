//! Provides the command-line interface and end-to-end orchestration of the pipeline.
//! Parses flags and modes (e.g., output format, chunking options, --topology, --topology-full, --bbox).
//! Wires together parsing, modeling, conversion, topology, geometry, and serialization components according to user-selected options, while keeping stages clearly separated and extensible.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::{Parser, ValueEnum};
use ifc_model::build_model;
use ifc_model::IfcModel;
use ifc_step::{parse_step_file, EntityId, StepFile, StepValue};
use lbd_converter::ConvertOptions;
use lbd_geometry::{
    derive_relations_with_exact_kernel_subprocess_batch, BoundingBox, ExactCheckOptions,
    GeometryRelation, GeometryRelationKind, SubprocessKernelExecutionOptions,
};
use lbd_pipeline::FailurePolicy;
use lbd_serializer::{
    serialize_lbd_batches_to_writer, serialize_nquads_batches_to_writer,
    serialize_nquads_merged_batches_to_writer, serialize_turtle_batches_to_writer,
};
use rayon::prelude::*;
use serde::Serialize;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum QuadChunkingMode {
    None,
    Lines,
    Bytes,
    Cores,
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
    geometry_tolerance: f64,

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
    chunking: QuadChunkingMode,
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
        let (mesh_bboxes, mesh_wkts, bbox_report) = collect_mesh_bounding_boxes_hybrid(
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
        geometry_bounding_boxes = Some(arc_bounding_boxes_from_raw(mesh_bboxes));
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
    let grafeo_direct_stream = active_plugins.contains(pipeline_plugins::GRAFEO_EXPORT_ID);
    let quad_chunk_size_lines = settings.nquads.chunk_size_lines;
    let quad_chunk_size_bytes = settings.nquads.chunk_size_bytes;
    let quad_chunk_prefix = settings.nquads.chunk_prefix.clone();
    let quad_chunk_min_count = settings.nquads.chunk_min_count;
    let ifcowl_chunk_core_count = resolve_effective_core_chunk_count_for_estimated_bytes(
        settings.nquads.chunking,
        settings.nquads.chunk_core_count,
        settings.nquads.chunk_min_count,
        input_file_size_bytes.saturating_mul(IFCOWL_TO_NQ_ESTIMATE_MULTIPLIER),
    );
    let lbd_chunk_core_count = resolve_effective_core_chunk_count_for_estimated_bytes(
        settings.nquads.chunking,
        settings.nquads.chunk_core_count,
        settings.nquads.chunk_min_count,
        input_file_size_bytes.saturating_mul(LBD_TO_NQ_ESTIMATE_MULTIPLIER),
    );
    let topology_chunk_core_count = resolve_effective_core_chunk_count_for_estimated_bytes(
        settings.nquads.chunking,
        settings.nquads.chunk_core_count,
        settings.nquads.chunk_min_count,
        if derive_adjacency {
            input_file_size_bytes.saturating_mul(TOPOLOGY_FULL_TO_NQ_ESTIMATE_MULTIPLIER)
        } else {
            (input_file_size_bytes / 8).max(1)
        },
    );
    if settings.nquads.chunking == QuadChunkingMode::Cores {
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
                    serialize_lbd_batches_to_writer(lbd_receiver, writer, &lbd_base_uri)
                        .with_context(|| format!("failed to write Turtle to {}", path.display()))?;
                }
                None => {
                    let stdout = std::io::stdout();
                    let handle = stdout.lock();
                    let writer = BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, handle);
                    serialize_lbd_batches_to_writer(lbd_receiver, writer, &lbd_base_uri)
                        .context("failed to write Turtle to stdout")?;
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
                } else if quad_chunking_mode != QuadChunkingMode::None {
                    let output_dir = resolve_quad_chunk_output_dir(lbd_target.as_deref());
                    let mut lbd_chunk_writer = QuadChunkWriter::new(
                        output_dir,
                        format!("{}-lbd", quad_chunk_prefix),
                        quad_chunking_mode,
                        quad_chunk_size_lines,
                        quad_chunk_size_bytes,
                        quad_chunk_min_count,
                        lbd_chunk_core_count,
                    )?;
                    let mut ifcowl_chunk_writer = if merged_ifcowl_receiver.is_some() {
                        Some(QuadChunkWriter::new(
                            resolve_quad_chunk_output_dir(lbd_target.as_deref()),
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
                        Some(QuadChunkWriter::new(
                            resolve_quad_chunk_output_dir(lbd_target.as_deref()),
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
        if module_id == pipeline_plugins::NQUADS_SERIALIZER_ID {
            validate_nquads_serializer_module_config(entries)?;
        }
        if module_id == pipeline_plugins::BBOX_ENRICHER_ID {
            validate_bbox_module_config(entries)?;
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
    if active.contains(pipeline_plugins::GRAFEO_EXPORT_ID) {
        if output_format != OutputFormat::Nquads {
            anyhow::bail!("grafeo export module requires `neo-nquads-serializer`");
        }
        if settings.nquads.chunking != QuadChunkingMode::None {
            anyhow::bail!("grafeo export module cannot be combined with N-Quads chunking");
        }
    }
    if !active.contains(pipeline_plugins::LBD_PRODUCER_ID) {
        anyhow::bail!(
            "module plan must include `{}`",
            pipeline_plugins::LBD_PRODUCER_ID
        );
    }
    let has_file_export = active.contains(pipeline_plugins::FILE_EXPORT_ID);
    let has_stdout_export = active.contains(pipeline_plugins::STDOUT_EXPORT_ID);
    let has_grafeo_export = active.contains(pipeline_plugins::GRAFEO_EXPORT_ID);
    let export_count =
        has_file_export as usize + has_stdout_export as usize + has_grafeo_export as usize;
    if export_count != 1 {
        anyhow::bail!(
            "module plan must include exactly one export module (`{}`, `{}`, or `{}`)",
            pipeline_plugins::FILE_EXPORT_ID,
            pipeline_plugins::STDOUT_EXPORT_ID,
            pipeline_plugins::GRAFEO_EXPORT_ID
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
    let has_nquads = active.contains(pipeline_plugins::NQUADS_SERIALIZER_ID);
    let has_turtle = active.contains(pipeline_plugins::TURTLE_SERIALIZER_ID);
    let output_format = match (has_turtle, has_nquads) {
        (true, false) => OutputFormat::Turtle,
        (false, true) => OutputFormat::Nquads,
        (true, true) => anyhow::bail!(
            "conflicting serializer modules enabled (`{}` and `{}`)",
            pipeline_plugins::TURTLE_SERIALIZER_ID,
            pipeline_plugins::NQUADS_SERIALIZER_ID
        ),
        (false, false) => anyhow::bail!(
            "no serializer module enabled; add `--module {}` or `--module {}`",
            pipeline_plugins::TURTLE_SERIALIZER_ID,
            pipeline_plugins::NQUADS_SERIALIZER_ID
        ),
    };

    let nquads_entries = configs.get(pipeline_plugins::NQUADS_SERIALIZER_ID);
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

    let bbox = if active.contains(pipeline_plugins::BBOX_ENRICHER_ID) {
        let bbox_entries = configs.get(pipeline_plugins::BBOX_ENRICHER_ID);
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

    Ok(ExecutionSettings {
        output_format,
        emit_ifcowl: active.contains(pipeline_plugins::IFCOWL_PRODUCER_ID),
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
    })
}

fn parse_quad_chunking(raw: Option<&str>) -> anyhow::Result<QuadChunkingMode> {
    let value = raw.unwrap_or("none");
    match value {
        "none" => Ok(QuadChunkingMode::None),
        "lines" => Ok(QuadChunkingMode::Lines),
        "bytes" => Ok(QuadChunkingMode::Bytes),
        "cores" => Ok(QuadChunkingMode::Cores),
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

pub(crate) fn topology_full_occ_relations(
    model: &IfcModel,
    step: &StepFile,
    input_path: &Path,
    geometry_tolerance: f64,
    bbox_inflation_threshold: f64,
    kernel_timeout: Duration,
    max_pairs_per_batch: usize,
) -> anyhow::Result<(
    Vec<GeometryRelation>,
    HashMap<EntityId, [f64; 6]>,
    HashMap<EntityId, String>,
    BboxQualityReport,
)> {
    let (candidate_pairs, mut prefilter_bboxes) = semantic_candidate_pairs(model, step);
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
        let empty_report = BboxQualityReport {
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
    let (mesh_bboxes, mesh_wkts, bbox_report) =
        collect_mesh_bounding_boxes_hybrid(step, sorted_element_ids, bbox_inflation_threshold);

    for (eid, bbox) in mesh_bboxes.iter() {
        prefilter_bboxes.entry(*eid).or_insert(*bbox);
    }

    let kernel_bin = resolve_geometry_kernel_bin()?;
    let (kernel_args, _cache_guard) = prepare_kernel_cache_args(input_path)?;
    tracing::info!("topology-full OCC kernel: {}", kernel_bin.display());

    let options = ExactCheckOptions {
        tolerance: geometry_tolerance,
    };
    let execution = SubprocessKernelExecutionOptions {
        timeout: kernel_timeout,
        // Keep one kernel invocation for typical model sizes to avoid rebuilding
        // in-memory shape maps across multiple subprocess calls.
        max_pairs_per_batch,
    };

    let relations = derive_relations_with_exact_kernel_subprocess_batch(
        model,
        kernel_bin,
        kernel_args,
        input_path.to_path_buf(),
        &candidate_pairs,
        &options,
        &execution,
        &prefilter_bboxes,
    )
    .context("exact OCC topology kernel failed")?;

    let intersecting_triples = relations
        .iter()
        .filter(|r| r.kind == GeometryRelationKind::IntersectingElement)
        .count();
    let interface_of_triples = relations
        .iter()
        .filter(|r| r.kind == GeometryRelationKind::InterfaceOf)
        .count();
    let interface_nodes = relations
        .iter()
        .filter(|r| r.kind == GeometryRelationKind::InterfaceOf)
        .map(|r| r.source)
        .collect::<HashSet<_>>()
        .len();
    tracing::info!(
        "topology-full OCC relations: intersecting triples={}, interfaceOf triples={}, intersecting pairs={}, interface nodes={}",
        intersecting_triples,
        interface_of_triples,
        intersecting_triples / 2,
        interface_nodes,
    );

    Ok((relations, mesh_bboxes, mesh_wkts, bbox_report))
}

#[derive(Debug)]
struct CacheDirGuard {
    path: PathBuf,
    cleanup_on_drop: bool,
}

impl Drop for CacheDirGuard {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            if let Err(error) = std::fs::remove_dir_all(&self.path) {
                tracing::warn!(
                    "failed to remove temporary OCC cache dir {}: {}",
                    self.path.display(),
                    error
                );
            }
        }
    }
}

fn prepare_kernel_cache_args(input_path: &Path) -> anyhow::Result<(Vec<String>, CacheDirGuard)> {
    if let Ok(override_dir) = std::env::var("IFC2LBD_OCC_CACHE_DIR") {
        let path = PathBuf::from(override_dir);
        std::fs::create_dir_all(&path).with_context(|| {
            format!(
                "failed to create IFC2LBD_OCC_CACHE_DIR at {}",
                path.display()
            )
        })?;
        return Ok((
            vec![
                "--brep-cache-dir".to_string(),
                path.to_string_lossy().into_owned(),
            ],
            CacheDirGuard {
                path,
                cleanup_on_drop: false,
            },
        ));
    }

    let keep_temp_cache = std::env::var("IFC2LBD_OCC_CACHE_PERSIST")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let stem = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("ifc");
    let safe_stem: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let path = std::env::temp_dir()
        .join("ifc2lbd-neo-occ-cache")
        .join(format!("{safe_stem}_{pid}_{now}"));
    std::fs::create_dir_all(&path).with_context(|| {
        format!(
            "failed to create temporary OCC cache dir {}",
            path.display()
        )
    })?;
    tracing::info!(
        "topology-full OCC cache dir: {}{}",
        path.display(),
        if keep_temp_cache {
            " (persist=true)"
        } else {
            " (ephemeral)"
        }
    );
    Ok((
        vec![
            "--brep-cache-dir".to_string(),
            path.to_string_lossy().into_owned(),
        ],
        CacheDirGuard {
            path,
            cleanup_on_drop: !keep_temp_cache,
        },
    ))
}

fn resolve_geometry_kernel_bin() -> anyhow::Result<PathBuf> {
    if let Ok(path) = std::env::var("IFC2LBD_GEOMETRY_KERNEL_BIN") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Ok(p);
        }
    }

    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("lbd-geometry-kernel"));
        }
    }
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    candidates.push(workspace_root.join("target/release/lbd-geometry-kernel"));
    candidates.push(workspace_root.join("target/debug/lbd-geometry-kernel"));

    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Ok(found);
    }

    tracing::info!("building lbd-geometry-kernel once (auto-discovery path)");
    let mut cargo_build = Command::new("cargo");
    cargo_build
        .arg("build")
        .arg("-p")
        .arg("lbd-geometry-kernel")
        .arg("--bin")
        .arg("lbd-geometry-kernel")
        .current_dir(&workspace_root);
    configure_pyo3_python_env(&mut cargo_build);
    let status = cargo_build
        .status()
        .context("failed to start cargo build for lbd-geometry-kernel")?;
    if !status.success() {
        anyhow::bail!(
            "failed to build lbd-geometry-kernel automatically (status: {})",
            status
        );
    }

    let built = workspace_root.join("target/debug/lbd-geometry-kernel");
    if built.is_file() {
        Ok(built)
    } else {
        anyhow::bail!(
            "lbd-geometry-kernel build finished but binary was not found at {}",
            built.display()
        )
    }
}

fn configure_pyo3_python_env(cmd: &mut Command) {
    if std::env::var_os("PYO3_PYTHON").is_some() {
        return;
    }
    if let Some(python) = detect_python3_executable() {
        tracing::info!("using detected python for pyo3: {}", python.display());
        cmd.env("PYO3_PYTHON", python);
    }
}

fn detect_python3_executable() -> Option<PathBuf> {
    let output = Command::new("python3")
        .arg("-c")
        .arg("import sys; print(sys.executable)")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return None;
    }
    let resolved = PathBuf::from(path);
    if resolved.is_file() {
        Some(resolved)
    } else {
        None
    }
}

/// Extract an approximate axis-aligned bounding box for an IFC product from raw STEP entities.
/// Walks the representation tree and collects 3D coordinate values without building OCC geometry.
/// Returns [minX, minY, minZ, maxX, maxY, maxZ] or None if no coordinates are found.
fn approximate_bbox(step: &StepFile, element_id: EntityId) -> Option<[f64; 6]> {
    let entity = step.entities.get(&element_id)?;
    // ObjectPlacement is args[5] for most IfcProduct subtypes.
    let placement_id = match entity.args.get(5) {
        Some(StepValue::Ref(id)) => *id,
        _ => return None,
    };
    // Walk the placement chain to get the world translation (approximate: take the
    // last LocalPlacement translation, ignoring rotation and parent transforms).
    // Good enough for spatial pre-filtering within a single storey.
    let world_translate = placement_translation(step, placement_id);

    // Collect all 3D coordinate values from the representation items.
    let rep_id = match entity.args.get(6) {
        Some(StepValue::Ref(id)) => *id,
        _ => {
            // No representation — use placement origin as a point bbox.
            let [x, y, z] = world_translate;
            return Some([x, y, z, x, y, z]);
        }
    };
    let mut pts: Vec<[f64; 3]> = Vec::new();
    collect_points(step, rep_id, &mut pts, 0, 300);
    // Elements with > 300 coordinate points have complex/freeform geometry (furniture,
    // appliances, MEP). These are never structural and we skip them for topology analysis.
    if pts.len() >= 300 {
        return None;
    }
    if pts.is_empty() {
        let [x, y, z] = world_translate;
        return Some([x, y, z, x, y, z]);
    }
    // Apply the world translation to all collected points.
    let [tx, ty, tz] = world_translate;
    let mut min = [f64::MAX; 3];
    let mut max = [f64::MIN; 3];
    for [x, y, z] in &pts {
        let wx = x + tx;
        let wy = y + ty;
        let wz = z + tz;
        min[0] = min[0].min(wx);
        min[1] = min[1].min(wy);
        min[2] = min[2].min(wz);
        max[0] = max[0].max(wx);
        max[1] = max[1].max(wy);
        max[2] = max[2].max(wz);
    }
    Some([min[0], min[1], min[2], max[0], max[1], max[2]])
}

/// Walk a placement chain and return the accumulated translation (sum of all local origins).
/// Ignores rotation for speed — sufficient for spatial pre-filtering.
fn placement_translation(step: &StepFile, placement_id: EntityId) -> [f64; 3] {
    let mut tx = 0.0f64;
    let mut ty = 0.0f64;
    let mut tz = 0.0f64;
    let mut current_id = placement_id;
    let mut depth = 0;
    loop {
        if depth > 20 {
            break;
        } // guard against cycles
        depth += 1;
        let Some(entity) = step.entities.get(&current_id) else {
            break;
        };
        match entity.entity_name.as_str() {
            "IFCLOCALPLACEMENT" => {
                // args[0] = PlacementRelTo (parent, optional), args[1] = RelativePlacement
                let rel_id = match entity.args.get(1) {
                    Some(StepValue::Ref(id)) => *id,
                    _ => break,
                };
                let [lx, ly, lz] = axis2placement3d_origin(step, rel_id);
                tx += lx;
                ty += ly;
                tz += lz;
                match entity.args.first() {
                    Some(StepValue::Ref(parent_id)) => {
                        current_id = *parent_id;
                    }
                    _ => break,
                }
            }
            _ => break,
        }
    }
    [tx, ty, tz]
}

fn axis2placement3d_origin(step: &StepFile, id: EntityId) -> [f64; 3] {
    let Some(entity) = step.entities.get(&id) else {
        return [0.0, 0.0, 0.0];
    };
    if entity.entity_name != "IFCAXIS2PLACEMENT3D" {
        return [0.0, 0.0, 0.0];
    }
    let loc_id = match entity.args.first() {
        Some(StepValue::Ref(id)) => *id,
        _ => return [0.0, 0.0, 0.0],
    };
    cartesian_point_3d(step, loc_id)
}

fn cartesian_point_3d(step: &StepFile, id: EntityId) -> [f64; 3] {
    let Some(entity) = step.entities.get(&id) else {
        return [0.0, 0.0, 0.0];
    };
    if entity.entity_name != "IFCCARTESIANPOINT" {
        return [0.0, 0.0, 0.0];
    }
    let coords = match entity.args.first() {
        Some(StepValue::List(list)) => list,
        _ => return [0.0, 0.0, 0.0],
    };
    let x = coords.get(0).and_then(|v| v.as_real()).unwrap_or(0.0);
    let y = coords.get(1).and_then(|v| v.as_real()).unwrap_or(0.0);
    let z = coords.get(2).and_then(|v| v.as_real()).unwrap_or(0.0);
    [x, y, z]
}

/// Recursively collect 3D coordinate values from an IFC entity tree.
/// Stops at depth 10 to avoid runaway traversal.
fn collect_points(
    step: &StepFile,
    id: EntityId,
    pts: &mut Vec<[f64; 3]>,
    depth: usize,
    max: usize,
) {
    if depth > 10 || pts.len() >= max {
        return;
    }
    let Some(entity) = step.entities.get(&id) else {
        return;
    };
    match entity.entity_name.as_str() {
        "IFCCARTESIANPOINT" => {
            if let Some(StepValue::List(coords)) = entity.args.first() {
                if coords.len() >= 3 {
                    let x = coords[0].as_real().unwrap_or(0.0);
                    let y = coords[1].as_real().unwrap_or(0.0);
                    let z = coords[2].as_real().unwrap_or(0.0);
                    pts.push([x, y, z]);
                }
            }
        }
        "IFCCARTESIANPOINTLIST3D" => {
            if let Some(StepValue::List(list)) = entity.args.first() {
                for item in list {
                    if let StepValue::List(coords) = item {
                        if coords.len() >= 3 {
                            let x = coords[0].as_real().unwrap_or(0.0);
                            let y = coords[1].as_real().unwrap_or(0.0);
                            let z = coords[2].as_real().unwrap_or(0.0);
                            pts.push([x, y, z]);
                        }
                    }
                }
            }
        }
        _ => {
            // Walk references in args.
            for arg in &entity.args {
                match arg {
                    StepValue::Ref(child_id) => {
                        collect_points(step, *child_id, pts, depth + 1, max);
                    }
                    StepValue::List(list) => {
                        for item in list {
                            if pts.len() >= max {
                                return;
                            }
                            if let StepValue::Ref(child_id) = item {
                                collect_points(step, *child_id, pts, depth + 1, max);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn bboxes_overlap_3d(a: &[f64; 6], b: &[f64; 6], tolerance: f64) -> bool {
    // X
    a[0] - tolerance <= b[3] + tolerance
        && a[3] + tolerance >= b[0] - tolerance
    // Y
        && a[1] - tolerance <= b[4] + tolerance
        && a[4] + tolerance >= b[1] - tolerance
    // Z
        && a[2] - tolerance <= b[5] + tolerance
        && a[5] + tolerance >= b[2] - tolerance
}

/// IFC element types that can generate bot:Interface (shared surfaces between structural elements).
/// Furniture, MEP, sanitary, distribution, and annotation elements are excluded — they
/// never touch structural elements in a surface-sharing sense.
fn is_structural_ifc_type(entity_name: &str) -> bool {
    matches!(
        entity_name,
        "IFCWALL"
            | "IFCWALLSTANDARDCASE"
            | "IFCSLAB"
            | "IFCCOLUMN"
            | "IFCBEAM"
            | "IFCROOF"
            | "IFCCOVERING"
            | "IFCCURTAINWALL"
            | "IFCPLATE"
            | "IFCMEMBER"
            | "IFCDOOR"
            | "IFCWINDOW"
            | "IFCSTAIR"
            | "IFCSTAIRFLIGHT"
            | "IFCRAMP"
            | "IFCRAMPFLIGHT"
            | "IFCFOOTING"
            | "IFCPILE"
            | "IFCBUILDINGELEMENTPROXY"
    )
}

fn semantic_candidate_pairs(
    model: &IfcModel,
    step: &StepFile,
) -> (Vec<(EntityId, EntityId)>, HashMap<EntityId, [f64; 6]>) {
    use std::collections::HashSet;
    let mut by_space: HashMap<EntityId, Vec<EntityId>> = HashMap::new();
    for boundary in &model.rel_space_boundaries {
        let Some(element) = boundary.element else {
            continue;
        };
        if model.elements.contains_key(&element) {
            by_space.entry(boundary.space).or_default().push(element);
        }
    }

    let mut pairs = HashSet::new();
    for elements in by_space.values_mut() {
        elements.sort_unstable();
        elements.dedup();
        for i in 0..elements.len() {
            for j in (i + 1)..elements.len() {
                let a = elements[i];
                let b = elements[j];
                let canonical = if a < b { (a, b) } else { (b, a) };
                pairs.insert(canonical);
            }
        }
    }

    // If we found pairs from space boundaries, return them (no bboxes needed — space boundary path).
    if !pairs.is_empty() {
        let mut out: Vec<_> = pairs.into_iter().collect();
        out.sort_unstable();
        return (out, HashMap::new());
    }

    // Fallback: no IfcRelSpaceBoundary records — group elements by storey/structure containment.
    // Only consider structural/architectural elements that can generate bot:Interface.
    // Furniture, MEP, and distribution elements never share structural surfaces.
    tracing::info!(
        "No IfcRelSpaceBoundary records found; falling back to storey-scoped candidate pairs (structural elements only)"
    );
    let mut by_structure: HashMap<EntityId, Vec<EntityId>> = HashMap::new();
    for (&element_id, &structure_id) in &model.contained_in {
        if let Some(node) = model.elements.get(&element_id) {
            if is_structural_ifc_type(node.entity_name.as_str()) {
                by_structure
                    .entry(structure_id)
                    .or_default()
                    .push(element_id);
            }
        }
    }
    // Compute approximate bboxes for all candidate elements from STEP data (pure Rust, fast).
    let mut element_bboxes: HashMap<EntityId, [f64; 6]> = HashMap::new();
    for elements in by_structure.values() {
        for &id in elements {
            if let Some(bbox) = approximate_bbox(step, id) {
                element_bboxes.insert(id, bbox);
            }
        }
    }

    // Generate pairs only where bboxes overlap (XY plane, 5cm tolerance).
    // Adjacent elements share a face so their bboxes truly overlap; 5cm covers
    // placement approximation errors without pairing distant elements.
    const BBOX_TOLERANCE: f64 = 0.05; // 5cm — touching elements have overlapping bboxes
    for elements in by_structure.values_mut() {
        elements.sort_unstable();
        elements.dedup();
        for i in 0..elements.len() {
            for j in (i + 1)..elements.len() {
                let a = elements[i];
                let b = elements[j];
                // Require both elements to have a bbox. Elements without a bbox have
                // complex/freeform geometry (furniture, MEP) or no geometry — skip them.
                // Among elements with bboxes, only pair those whose bboxes overlap.
                match (element_bboxes.get(&a), element_bboxes.get(&b)) {
                    (Some(ba), Some(bb)) => {
                        if !bboxes_overlap_3d(ba, bb, BBOX_TOLERANCE) {
                            continue;
                        }
                    }
                    _ => continue,
                }
                let canonical = if a < b { (a, b) } else { (b, a) };
                pairs.insert(canonical);
            }
        }
    }
    if pairs.len() > 100_000 {
        tracing::warn!(
            "storey-scoped candidate pairs ({}) exceeds 100k — this is unexpected after bbox filtering",
            pairs.len()
        );
    }

    let mut out: Vec<_> = pairs.into_iter().collect();
    out.sort_unstable();
    (out, element_bboxes)
}

/// Voxel-based adjacency detection: extract meshes, voxelize, and check adjacency.
///
/// 1. Generate candidate pairs (same as before: storey-scoped, bbox-filtered)
/// 2. For each unique element in the pairs, extract its triangle mesh and voxelize it
/// 3. For each candidate pair, check voxel adjacency
/// 4. Return GeometryRelation::AdjacentElement for each adjacent pair
/// Returns (relations, mesh_bboxes) where mesh_bboxes maps EntityId → [xmin,ymin,zmin,xmax,ymax,zmax]
/// computed from the actual triangle mesh in world coordinates.
fn voxel_adjacency_relations(
    model: &IfcModel,
    step: &StepFile,
    cell_size: f64,
    max_element_voxels: usize,
) -> (Vec<GeometryRelation>, HashMap<EntityId, [f64; 6]>) {
    let (candidates, _element_bboxes) = semantic_candidate_pairs(model, step);
    tracing::info!(
        "voxel adjacency: {} candidate pairs from {} structural elements",
        candidates.len(),
        {
            let mut ids = HashSet::new();
            for (a, b) in &candidates {
                ids.insert(*a);
                ids.insert(*b);
            }
            ids.len()
        }
    );

    if candidates.is_empty() {
        return (Vec::new(), HashMap::new());
    }

    // Collect unique element IDs
    let mut element_ids: Vec<EntityId> = {
        let mut ids = HashSet::new();
        for (a, b) in &candidates {
            ids.insert(*a);
            ids.insert(*b);
        }
        ids.into_iter().collect()
    };
    element_ids.sort_unstable();

    // Step 1: Extract and voxelize all elements in parallel; capture mesh bboxes.
    let voxel_start = Instant::now();
    let voxel_maps: Vec<(EntityId, HashSet<voxel::VoxelCoord>, [f64; 6])> = element_ids
        .par_iter()
        .filter_map(|&eid| {
            let world_t = transform::element_world_transform(step, eid);
            let mesh = mesh::extract_element_mesh(step, eid, &world_t);
            if mesh.is_empty() {
                tracing::debug!("element #{} has no mesh", eid);
                return None;
            }
            // Compute mesh bbox in world coordinates
            let mut mn = [f64::MAX; 3];
            let mut mx = [f64::MIN; 3];
            for chunk in mesh.vertices.chunks_exact(3) {
                for i in 0..3 {
                    mn[i] = mn[i].min(chunk[i]);
                    mx[i] = mx[i].max(chunk[i]);
                }
            }
            let bbox = [mn[0], mn[1], mn[2], mx[0], mx[1], mx[2]];
            let voxels = voxel::voxelize_triangles(&mesh.vertices, &mesh.indices, cell_size);
            if voxels.is_empty() {
                return None;
            }
            if max_element_voxels > 0 && voxels.len() > max_element_voxels {
                tracing::warn!(
                    "element #{} skipped: {} voxels exceeds limit {} (bbox {:.1}×{:.1}×{:.1}m)",
                    eid,
                    voxels.len(),
                    max_element_voxels,
                    mx[0] - mn[0],
                    mx[1] - mn[1],
                    mx[2] - mn[2],
                );
                return None;
            }
            Some((eid, voxels, bbox))
        })
        .collect();

    let mut mesh_bboxes: HashMap<EntityId, [f64; 6]> = HashMap::with_capacity(voxel_maps.len());
    let voxel_map: HashMap<EntityId, HashSet<voxel::VoxelCoord>> = voxel_maps
        .into_iter()
        .map(|(eid, voxels, bbox)| {
            mesh_bboxes.insert(eid, bbox);
            (eid, voxels)
        })
        .collect();

    let meshed = voxel_map.len();
    let total_voxels: usize = voxel_map.values().map(|v| v.len()).sum();
    tracing::info!(
        "voxelized {}/{} elements ({} total voxels) in {:.3}s",
        meshed,
        element_ids.len(),
        total_voxels,
        voxel_start.elapsed().as_secs_f64(),
    );

    // Step 2: Check adjacency for all candidate pairs in parallel
    let adj_start = Instant::now();
    let adjacent_pairs: Vec<(EntityId, EntityId)> = candidates
        .par_iter()
        .filter_map(|&(a, b)| {
            let va = voxel_map.get(&a)?;
            let vb = voxel_map.get(&b)?;
            if voxel::voxels_adjacent(va, vb) {
                Some((a, b))
            } else {
                None
            }
        })
        .collect();

    // Step 3: Build proper BOT relations per spec:
    //   - bot:intersectingElement in both directions (element-element)
    //   - bot:Interface instance with bot:interfaceOf to both elements
    // bot:adjacentElement is Zone→Element only per BOT spec.
    // Synthetic interface IDs: use a range above any real entity ID.
    let max_entity_id = step.entities.keys().copied().max().unwrap_or(0);
    let mut relations = Vec::with_capacity(adjacent_pairs.len() * 4);
    for (i, &(a, b)) in adjacent_pairs.iter().enumerate() {
        let interface_id = max_entity_id + 1 + i as u64;
        // IntersectingElement both directions
        relations.push(GeometryRelation {
            source: a,
            target: b,
            kind: GeometryRelationKind::IntersectingElement,
        });
        relations.push(GeometryRelation {
            source: b,
            target: a,
            kind: GeometryRelationKind::IntersectingElement,
        });
        // InterfaceOf: synthetic interface → both elements
        relations.push(GeometryRelation {
            source: interface_id,
            target: a,
            kind: GeometryRelationKind::InterfaceOf,
        });
        relations.push(GeometryRelation {
            source: interface_id,
            target: b,
            kind: GeometryRelationKind::InterfaceOf,
        });
    }

    tracing::info!(
        "adjacency check: {} adjacent pairs found from {} candidates in {:.3}s",
        adjacent_pairs.len(),
        candidates.len(),
        adj_start.elapsed().as_secs_f64(),
    );

    (relations, mesh_bboxes)
}

#[derive(Debug, Clone, Serialize)]
struct BboxOutlier {
    entity_id: EntityId,
    inflation_fast: f64,
    inflation_final: f64,
    used_exact: bool,
    used_rotated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BboxQualityReport {
    elements_requested: usize,
    elements_with_mesh: usize,
    escalated_exact_count: usize,
    rotated_bbox_count: usize,
    avg_inflation_fast: f64,
    max_inflation_fast: f64,
    avg_inflation_final: f64,
    max_inflation_final: f64,
    avg_escalated_reduction_ratio: f64,
    count_fast_over_1_2: usize,
    count_fast_over_1_5: usize,
    count_fast_over_1_8: usize,
    count_fast_over_2_0: usize,
    inflation_threshold: f64,
    top_inflation_outliers: Vec<BboxOutlier>,
}

fn collect_mesh_bounding_boxes_hybrid(
    step: &StepFile,
    element_ids: Vec<EntityId>,
    inflation_threshold: f64,
) -> (
    HashMap<EntityId, [f64; 6]>,
    HashMap<EntityId, String>,
    BboxQualityReport,
) {
    let records: Vec<(EntityId, [f64; 6], String, f64, f64, bool, bool)> = element_ids
        .par_iter()
        .filter_map(|&eid| {
            let world_t = transform::element_world_transform(step, eid);
            let local_mesh =
                mesh::extract_element_mesh(step, eid, &transform::Transform4::identity());
            if local_mesh.is_empty() {
                return None;
            }
            let local_bbox = bbox_from_vertices(&local_mesh.vertices)?;
            let local_volume = bbox_volume(&local_bbox);
            let fast_world_bbox = transform_aabb(&world_t, &local_bbox);
            let fast_world_volume = bbox_volume(&fast_world_bbox);
            let inflation = if local_volume > 1e-12 {
                fast_world_volume / local_volume
            } else {
                1.0
            };

            if inflation > inflation_threshold {
                let mut exact_mesh = local_mesh;
                exact_mesh.transform(&world_t);
                let exact_world_bbox = bbox_from_vertices(&exact_mesh.vertices)?;
                let exact_world_volume = bbox_volume(&exact_world_bbox);
                if let Some((wkt, obb_volume)) = oriented_bbox_wkt_xy(&exact_mesh.vertices) {
                    let final_inflation = if local_volume > 1e-12 {
                        obb_volume / local_volume
                    } else {
                        1.0
                    };
                    Some((
                        eid,
                        exact_world_bbox,
                        wkt,
                        inflation,
                        final_inflation,
                        true,
                        true,
                    ))
                } else {
                    let final_inflation = if local_volume > 1e-12 {
                        exact_world_volume / local_volume
                    } else {
                        1.0
                    };
                    Some((
                        eid,
                        exact_world_bbox,
                        bbox_wkt_polyhedral_surface_from_raw(&exact_world_bbox),
                        inflation,
                        final_inflation,
                        true,
                        false,
                    ))
                }
            } else {
                Some((
                    eid,
                    fast_world_bbox,
                    bbox_wkt_polyhedral_surface_from_raw(&fast_world_bbox),
                    inflation,
                    inflation,
                    false,
                    false,
                ))
            }
        })
        .collect();

    let mut out = HashMap::with_capacity(records.len());
    let mut wkts = HashMap::with_capacity(records.len());
    let mut sum_inflation_fast = 0.0_f64;
    let mut max_inflation_fast = 0.0_f64;
    let mut sum_inflation_final = 0.0_f64;
    let mut max_inflation_final = 0.0_f64;
    let mut escalated_exact_count = 0_usize;
    let mut escalated_reduction_sum = 0.0_f64;
    let mut count_fast_over_1_2 = 0_usize;
    let mut count_fast_over_1_5 = 0_usize;
    let mut count_fast_over_1_8 = 0_usize;
    let mut count_fast_over_2_0 = 0_usize;
    let mut outliers: Vec<BboxOutlier> = Vec::with_capacity(records.len());

    let mut rotated_bbox_count = 0_usize;
    for (eid, bbox, wkt, inflation_fast, inflation_final, escalated, used_rotated) in records {
        out.insert(eid, bbox);
        wkts.insert(eid, wkt);
        sum_inflation_fast += inflation_fast;
        max_inflation_fast = max_inflation_fast.max(inflation_fast);
        sum_inflation_final += inflation_final;
        max_inflation_final = max_inflation_final.max(inflation_final);
        if inflation_fast > 1.2 {
            count_fast_over_1_2 += 1;
        }
        if inflation_fast > 1.5 {
            count_fast_over_1_5 += 1;
        }
        if inflation_fast > 1.8 {
            count_fast_over_1_8 += 1;
        }
        if inflation_fast > 2.0 {
            count_fast_over_2_0 += 1;
        }
        if escalated {
            escalated_exact_count += 1;
            if inflation_fast > 1e-12 {
                escalated_reduction_sum += (inflation_fast - inflation_final) / inflation_fast;
            }
        }
        if used_rotated {
            rotated_bbox_count += 1;
        }
        outliers.push(BboxOutlier {
            entity_id: eid,
            inflation_fast,
            inflation_final,
            used_exact: escalated,
            used_rotated,
        });
    }

    outliers.sort_by(|a, b| b.inflation_fast.total_cmp(&a.inflation_fast));
    outliers.truncate(20);

    let elements_with_mesh = out.len();
    let avg_inflation_fast = if elements_with_mesh > 0 {
        sum_inflation_fast / elements_with_mesh as f64
    } else {
        0.0
    };
    let avg_inflation_final = if elements_with_mesh > 0 {
        sum_inflation_final / elements_with_mesh as f64
    } else {
        0.0
    };
    let avg_escalated_reduction_ratio = if escalated_exact_count > 0 {
        escalated_reduction_sum / escalated_exact_count as f64
    } else {
        0.0
    };
    (
        out,
        wkts,
        BboxQualityReport {
            elements_requested: element_ids.len(),
            elements_with_mesh,
            escalated_exact_count,
            rotated_bbox_count,
            avg_inflation_fast,
            max_inflation_fast,
            avg_inflation_final,
            max_inflation_final,
            avg_escalated_reduction_ratio,
            count_fast_over_1_2,
            count_fast_over_1_5,
            count_fast_over_1_8,
            count_fast_over_2_0,
            inflation_threshold,
            top_inflation_outliers: outliers,
        },
    )
}

fn bbox_from_vertices(vertices: &[f64]) -> Option<[f64; 6]> {
    if vertices.len() < 3 {
        return None;
    }
    let mut mn = [f64::MAX; 3];
    let mut mx = [f64::MIN; 3];
    let mut any = false;
    for chunk in vertices.chunks_exact(3) {
        any = true;
        for i in 0..3 {
            mn[i] = mn[i].min(chunk[i]);
            mx[i] = mx[i].max(chunk[i]);
        }
    }
    if !any {
        return None;
    }
    Some([mn[0], mn[1], mn[2], mx[0], mx[1], mx[2]])
}

fn bbox_volume(bbox: &[f64; 6]) -> f64 {
    let dx = (bbox[3] - bbox[0]).max(0.0);
    let dy = (bbox[4] - bbox[1]).max(0.0);
    let dz = (bbox[5] - bbox[2]).max(0.0);
    dx * dy * dz
}

fn transform_aabb(t: &transform::Transform4, bbox: &[f64; 6]) -> [f64; 6] {
    let [x0, y0, z0, x1, y1, z1] = *bbox;
    let corners = [
        [x0, y0, z0],
        [x1, y0, z0],
        [x0, y1, z0],
        [x1, y1, z0],
        [x0, y0, z1],
        [x1, y0, z1],
        [x0, y1, z1],
        [x1, y1, z1],
    ];
    let mut mn = [f64::MAX; 3];
    let mut mx = [f64::MIN; 3];
    for p in corners {
        let tp = t.transform_point(&p);
        for i in 0..3 {
            mn[i] = mn[i].min(tp[i]);
            mx[i] = mx[i].max(tp[i]);
        }
    }
    [mn[0], mn[1], mn[2], mx[0], mx[1], mx[2]]
}

fn bbox_wkt_polyhedral_surface_from_raw(bbox: &[f64; 6]) -> String {
    let [x0, y0, z0, x1, y1, z1] = *bbox;
    let x0 = fmt_num(x0);
    let y0 = fmt_num(y0);
    let z0 = fmt_num(z0);
    let x1 = fmt_num(x1);
    let y1 = fmt_num(y1);
    let z1 = fmt_num(z1);
    format!(
        "POLYHEDRALSURFACE Z ((({x0} {y0} {z0}, {x1} {y0} {z0}, {x1} {y1} {z0}, {x0} {y1} {z0}, {x0} {y0} {z0})), (({x0} {y0} {z1}, {x0} {y1} {z1}, {x1} {y1} {z1}, {x1} {y0} {z1}, {x0} {y0} {z1})), (({x0} {y0} {z0}, {x0} {y0} {z1}, {x1} {y0} {z1}, {x1} {y0} {z0}, {x0} {y0} {z0})), (({x1} {y0} {z0}, {x1} {y0} {z1}, {x1} {y1} {z1}, {x1} {y1} {z0}, {x1} {y0} {z0})), (({x1} {y1} {z0}, {x1} {y1} {z1}, {x0} {y1} {z1}, {x0} {y1} {z0}, {x1} {y1} {z0})), (({x0} {y1} {z0}, {x0} {y1} {z1}, {x0} {y0} {z1}, {x0} {y0} {z0}, {x0} {y1} {z0})))"
    )
}

fn oriented_bbox_wkt_xy(vertices: &[f64]) -> Option<(String, f64)> {
    if vertices.len() < 9 {
        return None;
    }
    let mut z_min = f64::MAX;
    let mut z_max = f64::MIN;
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(vertices.len() / 3);
    for p in vertices.chunks_exact(3) {
        pts.push((p[0], p[1]));
        z_min = z_min.min(p[2]);
        z_max = z_max.max(p[2]);
    }
    if pts.is_empty() {
        return None;
    }

    let n = pts.len() as f64;
    let (sum_x, sum_y) = pts
        .iter()
        .fold((0.0, 0.0), |(sx, sy), (x, y)| (sx + x, sy + y));
    let cx = sum_x / n;
    let cy = sum_y / n;

    let mut sxx = 0.0;
    let mut syy = 0.0;
    let mut sxy = 0.0;
    for (x, y) in &pts {
        let dx = *x - cx;
        let dy = *y - cy;
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    // Principal direction in XY plane (PCA for 2D cloud)
    let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let (ct, st) = (theta.cos(), theta.sin());
    let u = (ct, st);
    let v = (-st, ct);

    let mut u_min = f64::MAX;
    let mut u_max = f64::MIN;
    let mut v_min = f64::MAX;
    let mut v_max = f64::MIN;
    for (x, y) in &pts {
        let dx = *x - cx;
        let dy = *y - cy;
        let pu = dx * u.0 + dy * u.1;
        let pv = dx * v.0 + dy * v.1;
        u_min = u_min.min(pu);
        u_max = u_max.max(pu);
        v_min = v_min.min(pv);
        v_max = v_max.max(pv);
    }
    let du = (u_max - u_min).max(0.0);
    let dv = (v_max - v_min).max(0.0);
    let dz = (z_max - z_min).max(0.0);
    if du <= f64::EPSILON || dv <= f64::EPSILON || dz <= f64::EPSILON {
        return None;
    }

    let corner_uv = [
        (u_min, v_min),
        (u_max, v_min),
        (u_max, v_max),
        (u_min, v_max),
    ];
    let mut cxy = [(0.0, 0.0); 4];
    for (i, (cu, cv)) in corner_uv.iter().enumerate() {
        cxy[i] = (cx + cu * u.0 + cv * v.0, cy + cu * u.1 + cv * v.1);
    }
    let (x0, y0) = cxy[0];
    let (x1, y1) = cxy[1];
    let (x2, y2) = cxy[2];
    let (x3, y3) = cxy[3];
    let z0 = fmt_num(z_min);
    let z1 = fmt_num(z_max);
    let wkt = format!(
        "POLYHEDRALSURFACE Z ((({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})))",
        fmt_num(x0), fmt_num(y0), z0, fmt_num(x1), fmt_num(y1), z0, fmt_num(x2), fmt_num(y2), z0, fmt_num(x3), fmt_num(y3), z0, fmt_num(x0), fmt_num(y0), z0,
        fmt_num(x0), fmt_num(y0), z1, fmt_num(x3), fmt_num(y3), z1, fmt_num(x2), fmt_num(y2), z1, fmt_num(x1), fmt_num(y1), z1, fmt_num(x0), fmt_num(y0), z1,
        fmt_num(x0), fmt_num(y0), z0, fmt_num(x0), fmt_num(y0), z1, fmt_num(x1), fmt_num(y1), z1, fmt_num(x1), fmt_num(y1), z0, fmt_num(x0), fmt_num(y0), z0,
        fmt_num(x1), fmt_num(y1), z0, fmt_num(x1), fmt_num(y1), z1, fmt_num(x2), fmt_num(y2), z1, fmt_num(x2), fmt_num(y2), z0, fmt_num(x1), fmt_num(y1), z0,
        fmt_num(x2), fmt_num(y2), z0, fmt_num(x2), fmt_num(y2), z1, fmt_num(x3), fmt_num(y3), z1, fmt_num(x3), fmt_num(y3), z0, fmt_num(x2), fmt_num(y2), z0,
        fmt_num(x3), fmt_num(y3), z0, fmt_num(x3), fmt_num(y3), z1, fmt_num(x0), fmt_num(y0), z1, fmt_num(x0), fmt_num(y0), z0, fmt_num(x3), fmt_num(y3), z0
    );
    Some((wkt, du * dv * dz))
}

fn fmt_num(v: f64) -> String {
    let mut s = format!("{v:.9}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

/// Write element bounding boxes as GeoSPARQL WKT triples to a Turtle file.
///
/// Each element gets:
///   <element_iri> geo:hasGeometry <element_iri_geom> .
///   <element_iri_geom> a geo:Geometry ;
///       geo:asWKT "POLYGON Z ((...bottom face...))"^^geo:wktLiteral ;
///       geo:dimension 3 .
///
/// The WKT is a 3D polyhedron (6 faces of the bounding box) encoded as
/// POLYHEDRALSURFACE Z for maximum compatibility. The footprint POLYGON Z
/// (bottom face) plus a separate LINESTRING Z marking the height extent
/// is also included for 2D-capable tools.
fn arc_bounding_boxes_from_raw(
    raw: HashMap<EntityId, [f64; 6]>,
) -> Arc<HashMap<EntityId, BoundingBox>> {
    let mapped = raw
        .into_iter()
        .map(|(entity_id, [x_min, y_min, z_min, x_max, y_max, z_max])| {
            (
                entity_id,
                BoundingBox {
                    x_min,
                    x_max,
                    y_min,
                    y_max,
                    z_min,
                    z_max,
                },
            )
        })
        .collect();
    Arc::new(mapped)
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
        && settings.nquads.chunking != QuadChunkingMode::None
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
    if settings.nquads.chunking != QuadChunkingMode::Cores
        && settings.nquads.chunk_core_count.is_some()
    {
        anyhow::bail!("`neo-nquads-serializer.chunk_core_count` is only valid when chunking=cores");
    }
    Ok(())
}

fn resolve_quad_chunk_output_dir(output_file: Option<&Path>) -> PathBuf {
    output_file
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn resolve_effective_core_chunk_count(
    mode: QuadChunkingMode,
    requested_core_count: Option<usize>,
    min_chunk_count: usize,
    input_file_size_bytes: u64,
) -> Option<usize> {
    let estimated_nq_bytes =
        (input_file_size_bytes.saturating_mul(IFC_TO_NQ_ESTIMATE_MULTIPLIER)).max(1);
    resolve_effective_core_chunk_count_for_estimated_bytes(
        mode,
        requested_core_count,
        min_chunk_count,
        estimated_nq_bytes,
    )
}

fn resolve_effective_core_chunk_count_for_estimated_bytes(
    mode: QuadChunkingMode,
    requested_core_count: Option<usize>,
    min_chunk_count: usize,
    estimated_nq_bytes: u64,
) -> Option<usize> {
    if mode != QuadChunkingMode::Cores {
        return requested_core_count;
    }
    let available_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let requested = requested_core_count
        .unwrap_or(available_cores)
        .max(min_chunk_count);
    let min_chunks_by_max_size =
        ((estimated_nq_bytes + (MAX_CORE_CHUNK_BYTES - 1)) / MAX_CORE_CHUNK_BYTES).max(1) as usize;
    let max_chunks_by_min_size = (estimated_nq_bytes / MIN_CORE_CHUNK_BYTES).max(1) as usize;
    let floor = min_chunk_count.max(min_chunks_by_max_size);
    let ceiling = max_chunks_by_min_size.max(floor);
    let effective = requested.clamp(floor, ceiling);
    Some(effective)
}

#[derive(Debug, Serialize)]
struct QuadChunkManifest {
    chunking: String,
    chunk_size_lines: u64,
    chunk_size_bytes: u64,
    chunk_prefix: String,
    min_chunk_count: u64,
    core_chunk_count: u64,
    files: Vec<QuadChunkEntry>,
    total_lines: u64,
    total_triples_estimate: u64,
}

#[derive(Clone, Debug, Serialize)]
struct QuadChunkEntry {
    file: String,
    bytes: u64,
    lines: u64,
}

#[derive(Debug)]
struct QuadChunkWriter {
    output_dir: PathBuf,
    chunk_prefix: String,
    mode: QuadChunkingMode,
    lines_per_chunk: u64,
    bytes_per_chunk: u64,
    min_chunk_count: u64,
    core_chunk_count: u64,
    current_index: usize,
    current_file: Option<BufWriter<File>>,
    current_bytes: u64,
    current_lines: u64,
    pending_buffer: Vec<u8>,
    manifest_entries: Vec<QuadChunkEntry>,
    total_lines: u64,
    core_current_writer: usize,
    core_lines_in_block: u64,
    core_sender: Option<crossbeam::channel::Sender<CoreChunkWriteMsg>>,
    core_writer_thread: Option<thread::JoinHandle<anyhow::Result<()>>>,
    core_pending_buffers: Vec<Vec<u8>>,
    core_bytes: Vec<u64>,
    core_lines: Vec<u64>,
}

#[derive(Debug)]
enum CoreChunkWriteMsg {
    Data { index: usize, bytes: Vec<u8> },
}

impl QuadChunkWriter {
    fn new(
        output_dir: PathBuf,
        chunk_prefix: String,
        mode: QuadChunkingMode,
        lines_per_chunk: usize,
        bytes_per_chunk: usize,
        min_chunk_count: usize,
        core_count_override: Option<usize>,
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&output_dir).with_context(|| {
            format!(
                "failed to create quad chunk output dir {}",
                output_dir.display()
            )
        })?;
        let available_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let selected_cores = core_count_override.unwrap_or(available_cores);
        let core_chunk_count = if mode == QuadChunkingMode::Cores {
            selected_cores.max(min_chunk_count) as u64
        } else {
            0
        };

        let mut writer = Self {
            output_dir,
            chunk_prefix,
            mode,
            lines_per_chunk: lines_per_chunk as u64,
            bytes_per_chunk: bytes_per_chunk as u64,
            min_chunk_count: min_chunk_count as u64,
            core_chunk_count,
            current_index: 0,
            current_file: None,
            current_bytes: 0,
            current_lines: 0,
            pending_buffer: Vec::new(),
            manifest_entries: Vec::new(),
            total_lines: 0,
            core_current_writer: 0,
            core_lines_in_block: 0,
            core_sender: None,
            core_writer_thread: None,
            core_pending_buffers: Vec::new(),
            core_bytes: Vec::new(),
            core_lines: Vec::new(),
        };
        if writer.mode == QuadChunkingMode::Cores {
            writer.start_core_chunk_writer_thread(core_chunk_count as usize)?;
        }
        Ok(writer)
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        if !self.pending_buffer.is_empty() {
            if !self.pending_buffer.ends_with(b"\n") {
                self.pending_buffer.push(b'\n');
            }
            self.consume_complete_lines()?;
        }
        if self.mode == QuadChunkingMode::Cores {
            self.flush_core_pending_buffers()?;
            self.close_core_chunk_files()?;
        } else {
            self.close_current_file()?;
        }
        let manifest_path = self
            .output_dir
            .join(format!("{}.manifest.json", self.chunk_prefix));
        let manifest = QuadChunkManifest {
            chunking: match self.mode {
                QuadChunkingMode::None => "none".to_string(),
                QuadChunkingMode::Lines => "lines".to_string(),
                QuadChunkingMode::Bytes => "bytes".to_string(),
                QuadChunkingMode::Cores => "cores".to_string(),
            },
            chunk_size_lines: self.lines_per_chunk,
            chunk_size_bytes: self.bytes_per_chunk,
            chunk_prefix: self.chunk_prefix.clone(),
            min_chunk_count: self.min_chunk_count,
            core_chunk_count: self.core_chunk_count,
            files: self.manifest_entries.clone(),
            total_lines: self.total_lines,
            total_triples_estimate: self.total_lines,
        };
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .context("failed to serialize quad chunk manifest JSON")?;
        std::fs::write(&manifest_path, manifest_json)
            .with_context(|| format!("failed to write manifest {}", manifest_path.display()))?;
        Ok(())
    }

    fn write_complete_line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        if self.mode == QuadChunkingMode::Cores {
            return self.write_round_robin_line(line);
        }
        if self.current_file.is_none() {
            self.open_next_chunk_file()?;
        }
        let line_len = line.len() as u64;
        if self.should_rotate(line_len) {
            self.close_current_file()?;
            self.open_next_chunk_file()?;
        }
        if let Some(file) = self.current_file.as_mut() {
            file.write_all(line)?;
        }
        self.current_bytes += line_len;
        self.current_lines += 1;
        self.total_lines += 1;
        Ok(())
    }

    fn consume_complete_lines(&mut self) -> anyhow::Result<()> {
        loop {
            let Some(pos) = self.pending_buffer.iter().position(|&b| b == b'\n') else {
                break;
            };
            let line = self.pending_buffer[..=pos].to_vec();
            self.pending_buffer.drain(..=pos);
            self.write_complete_line(&line)?;
        }
        Ok(())
    }

    fn should_rotate(&self, next_line_len: u64) -> bool {
        if self.current_file.is_none() || self.current_lines == 0 {
            return false;
        }
        match self.mode {
            QuadChunkingMode::None => false,
            QuadChunkingMode::Lines => self.current_lines >= self.lines_per_chunk,
            QuadChunkingMode::Bytes => self.current_bytes + next_line_len > self.bytes_per_chunk,
            QuadChunkingMode::Cores => false,
        }
    }

    fn open_next_chunk_file(&mut self) -> anyhow::Result<()> {
        let file_name = format!("{}.part-{:03}.nq", self.chunk_prefix, self.current_index);
        let path = self.output_dir.join(file_name);
        let file = File::create(&path)
            .with_context(|| format!("failed to create quad chunk {}", path.display()))?;
        self.current_file = Some(BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, file));
        self.current_bytes = 0;
        self.current_lines = 0;
        self.current_index += 1;
        Ok(())
    }

    fn close_current_file(&mut self) -> anyhow::Result<()> {
        if let Some(mut file) = self.current_file.take() {
            file.flush()?;
            let file_name = format!(
                "{}.part-{:03}.nq",
                self.chunk_prefix,
                self.current_index - 1
            );
            self.manifest_entries.push(QuadChunkEntry {
                file: file_name,
                bytes: self.current_bytes,
                lines: self.current_lines,
            });
            self.current_bytes = 0;
            self.current_lines = 0;
        }
        Ok(())
    }

    fn start_core_chunk_writer_thread(&mut self, count: usize) -> anyhow::Result<()> {
        let mut paths = Vec::with_capacity(count);
        self.core_bytes = vec![0; count];
        self.core_lines = vec![0; count];
        self.core_pending_buffers = (0..count)
            .map(|_| Vec::with_capacity(CORE_CHUNK_BATCH_BYTES))
            .collect();
        for i in 0..count {
            let file_name = format!("{}.part-{:03}.nq", self.chunk_prefix, i);
            let path = self.output_dir.join(&file_name);
            paths.push(path);
        }
        let (sender, receiver) = crossbeam::channel::bounded::<CoreChunkWriteMsg>(64);
        let writer_thread = thread::spawn(move || -> anyhow::Result<()> {
            let mut writers = Vec::with_capacity(paths.len());
            for path in &paths {
                let file = File::create(path)
                    .with_context(|| format!("failed to create quad chunk {}", path.display()))?;
                writers.push(BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, file));
            }
            for msg in receiver {
                match msg {
                    CoreChunkWriteMsg::Data { index, bytes } => {
                        let writer = writers.get_mut(index).ok_or_else(|| {
                            anyhow::anyhow!("invalid chunk index {} in writer thread", index)
                        })?;
                        writer.write_all(&bytes)?;
                    }
                }
            }
            for writer in &mut writers {
                writer.flush()?;
            }
            Ok(())
        });
        self.core_sender = Some(sender);
        self.core_writer_thread = Some(writer_thread);
        Ok(())
    }

    fn write_round_robin_line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        if self.core_pending_buffers.is_empty() {
            return Ok(());
        }
        let idx = self.core_current_writer % self.core_pending_buffers.len();
        self.core_pending_buffers[idx].extend_from_slice(line);
        let line_len = line.len() as u64;
        self.core_bytes[idx] += line_len;
        self.core_lines[idx] += 1;
        self.total_lines += 1;
        if self.core_pending_buffers[idx].len() >= CORE_CHUNK_BATCH_BYTES {
            self.flush_core_buffer(idx)?;
        }
        self.core_lines_in_block += 1;
        if self.core_lines_in_block >= CORE_CHUNK_BLOCK_LINES
            && !self.core_pending_buffers.is_empty()
        {
            self.core_current_writer =
                (self.core_current_writer + 1) % self.core_pending_buffers.len();
            self.core_lines_in_block = 0;
        }
        Ok(())
    }

    fn close_core_chunk_files(&mut self) -> anyhow::Result<()> {
        self.core_sender.take();
        if let Some(handle) = self.core_writer_thread.take() {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("core chunk writer thread panicked"))??;
        }
        for idx in 0..self.core_bytes.len() {
            let file_name = format!("{}.part-{:03}.nq", self.chunk_prefix, idx);
            self.manifest_entries.push(QuadChunkEntry {
                file: file_name,
                bytes: self.core_bytes[idx],
                lines: self.core_lines[idx],
            });
        }
        Ok(())
    }

    fn flush_core_buffer(&mut self, index: usize) -> anyhow::Result<()> {
        let bytes = std::mem::take(
            self.core_pending_buffers
                .get_mut(index)
                .ok_or_else(|| anyhow::anyhow!("invalid pending buffer index {}", index))?,
        );
        if bytes.is_empty() {
            return Ok(());
        }
        let sender = self
            .core_sender
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing core chunk sender"))?;
        sender
            .send(CoreChunkWriteMsg::Data { index, bytes })
            .map_err(|_| anyhow::anyhow!("core chunk writer channel closed"))?;
        Ok(())
    }

    fn flush_core_pending_buffers(&mut self) -> anyhow::Result<()> {
        for idx in 0..self.core_pending_buffers.len() {
            self.flush_core_buffer(idx)?;
        }
        Ok(())
    }
}

impl Write for QuadChunkWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut cursor = 0usize;
        if !self.pending_buffer.is_empty() {
            if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                self.pending_buffer.extend_from_slice(&buf[..=pos]);
                let line = std::mem::take(&mut self.pending_buffer);
                self.write_complete_line(&line)
                    .map_err(std::io::Error::other)?;
                cursor = pos + 1;
            } else {
                self.pending_buffer.extend_from_slice(buf);
                return Ok(buf.len());
            }
        }

        while cursor < buf.len() {
            let remainder = &buf[cursor..];
            let Some(pos) = remainder.iter().position(|&b| b == b'\n') else {
                break;
            };
            let end = cursor + pos + 1;
            self.write_complete_line(&buf[cursor..end])
                .map_err(std::io::Error::other)?;
            cursor = end;
        }

        if cursor < buf.len() {
            self.pending_buffer.extend_from_slice(&buf[cursor..]);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(file) = self.current_file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_requested_module_list, parse_module_configs, resolve_effective_core_chunk_count,
        validate_args, Args, ExecutionSettings, NquadsModuleOptions, OutputFormat, QuadChunkWriter,
        QuadChunkingMode,
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
        let effective = resolve_effective_core_chunk_count(
            QuadChunkingMode::Cores,
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

        let mut writer = QuadChunkWriter::new(
            out_dir.clone(),
            "test".to_string(),
            QuadChunkingMode::Lines,
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

        let mut writer = QuadChunkWriter::new(
            out_dir.clone(),
            "core".to_string(),
            QuadChunkingMode::Cores,
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
                chunking: QuadChunkingMode::None,
                chunk_size_lines: 2_000_000,
                chunk_size_bytes: 268_435_456,
                chunk_prefix: "out".to_string(),
                chunk_min_count: 1,
                chunk_core_count: None,
            },
        }
    }
}
