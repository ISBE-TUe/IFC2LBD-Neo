#![allow(unused_imports)]

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use flate2::write::GzEncoder;
use flate2::Compression;

use crossbeam::channel::Sender;
use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_converter::{
    stream_beo, stream_bot, stream_bsdd_with_cache, stream_omg_fog, stream_props_opm,
    BsddMatchCache, ConvertOptions,
};
use lbd_ontology::Triple;
use lbd_pipeline::{
    BatchKind, DerivedFile, ExportError, ExportFileSummary, ExportPlugin, ExportSession,
    FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage, PluginManifest,
    PluginRegistry, ProducerError, ProducerPlugin, SerializerPlugin, TaggedBatch, BEO_PRODUCER_ID,
    BOT_PRODUCER_ID, BSDD_PRODUCER_ID, FILE_EXPORT_ID, IFCOWL_PRODUCER_ID, LOG_EXPORT_ID,
    NQUADS_CHUNKED_SERIALIZER_ID, NQUADS_SERIALIZER_ID, OMG_FOG_PRODUCER_ID, ONTOLOGY_MAPPER_ID,
    PROPS_OPM_PRODUCER_ID, RML_MAPPER_ID, STDOUT_EXPORT_ID, TURTLE_SERIALIZER_ID,
};
use ontology_mapper_producer::OntologyMapperProducerPlugin;
use plugin_geometry_preprocess::GeometryPreprocessPlugin;
use plugin_geometry_producer::GeometryProducerPlugin;
use plugin_property_preprocess::{BsddMatchPreprocessPlugin, CleanupPreprocessPlugin};
use plugin_qto_preprocess::QtoPreprocessPlugin;
use rml_mapper_producer::RmlMapperProducerPlugin;
use serde_json::json;

// ---------------------------------------------------------------------------
// OutputDir — context key for the export destination directory
//
// CLI code inserts `Arc<OutputDir>` into the pipeline context before running
// producers. The `FileExportPlugin` session reads it from context.
// ---------------------------------------------------------------------------

/// The directory where file-export plugins write their output.
///
/// Inserted into `PipelineContext` by `main.rs` as `Arc<OutputDir>` before the
/// pipeline runs. `FileExportPlugin::start_session()` reads it to determine
/// where to write output files.
pub struct OutputDir(pub PathBuf);

/// When `true`, the file exporter wraps its output stream in gzip compression
/// and appends `.gz` to the filename.
///
/// Inserted into `PipelineContext` by `main.rs` as `Arc<CompressOutput>` when
/// `--module-opt neo-file-export.compress=gzip` is set.
pub struct CompressOutput(pub bool);

// ---------------------------------------------------------------------------
// Helpers shared by producer implementations
// ---------------------------------------------------------------------------

/// Spawn a rayon task that forwards raw-triple batches as tagged batches.
fn forward_as_tagged(
    raw_receiver: crossbeam::channel::Receiver<Vec<Triple>>,
    kind: BatchKind,
    tagged_sender: Sender<TaggedBatch>,
) {
    // Use std::thread::spawn (not rayon::spawn) so the relay does NOT occupy a
    // rayon worker thread while blocked on channel send.
    //
    // Each producer spawns one forwarder. With 6 producers on a 14-thread pool,
    // rayon::spawn forwarders would consume 6 of 14 threads just to block on
    // channel I/O. When IfcOWL (rayon::scope, up to 12 sub-tasks) and bSDD
    // (par_iter) also need rayon threads, the pool exhausts and deadlocks.
    // OS threads are correct for I/O-bound relays — they sleep in the kernel
    // when blocked, not in a rayon semaphore.
    std::thread::spawn(move || {
        for batch in raw_receiver {
            if tagged_sender
                .send(TaggedBatch {
                    kind: kind.clone(),
                    triples: batch,
                })
                .is_err()
            {
                break;
            }
        }
    });
}

/// Module option keys for each plugin (mirrors the WASM side).
pub fn module_option_keys(module_id: &str) -> Vec<String> {
    match module_id {
        lbd_pipeline::NQUADS_SERIALIZER_ID => vec!["graph_naming".to_string()],
        lbd_pipeline::NQUADS_CHUNKED_SERIALIZER_ID => vec![
            "chunking".to_string(),
            "chunk_size_lines".to_string(),
            "chunk_size_bytes".to_string(),
            "chunk_prefix".to_string(),
            "graph_naming".to_string(),
            "partitioning".to_string(),
        ],
        lbd_pipeline::TURTLE_SERIALIZER_ID => vec!["grouping".to_string(), "layout".to_string()],
        lbd_pipeline::BOT_PRODUCER_ID => vec!["mode".to_string()],
        lbd_pipeline::IFCOWL_PRODUCER_ID => vec!["mode".to_string()],
        lbd_pipeline::BSDD_PRODUCER_ID => vec![
            "profile".to_string(),
            "compact".to_string(),
            "include_standard_attrs".to_string(),
            "dedup_properties".to_string(),
            "opm_level".to_string(),
        ],
        lbd_pipeline::PROPS_OPM_PRODUCER_ID => vec!["opm_level".to_string()],
        lbd_pipeline::OMG_FOG_PRODUCER_ID => {
            vec!["opm_level".to_string(), "emit_bounding_boxes".to_string()]
        }
        lbd_pipeline::FILE_EXPORT_ID => vec!["output_stem".to_string(), "compress".to_string()],
        lbd_pipeline::LOG_EXPORT_ID => vec![],
        "neo-geometry-preprocess" => vec!["metadata".to_string()],
        "neo-geometry-producer" => vec!["format".to_string()],
        RML_MAPPER_ID => vec!["rml_mapping".to_string()],
        ONTOLOGY_MAPPER_ID => vec!["alignment_file".to_string(), "ontology_file".to_string()],
        _ => Vec::new(),
    }
}

pub fn built_in_registry() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry
        .register_preprocess(CleanupPreprocessPlugin)
        .unwrap();
    registry
        .register_preprocess(BsddMatchPreprocessPlugin)
        .unwrap();
    registry.register_preprocess(QtoPreprocessPlugin).unwrap();
    registry.register_producer(BotProducerPlugin).unwrap();
    registry.register_producer(BeoProducerPlugin).unwrap();
    registry.register_producer(BsddProducerPlugin).unwrap();
    registry.register_producer(PropsOpmProducerPlugin).unwrap();
    registry.register_producer(OmgFogProducerPlugin).unwrap();
    registry.register_producer(IfcowlProducerPlugin).unwrap();
    registry.register_producer(RmlMapperProducerPlugin).unwrap();
    registry
        .register_postprocess(OntologyMapperProducerPlugin)
        .unwrap();
    registry
        .register_preprocess(GeometryPreprocessPlugin)
        .unwrap();
    registry
        .register_producer(GeometryProducerPlugin::default())
        .unwrap();
    registry
        .register_serializer(TurtleSerializerPlugin)
        .unwrap();
    registry
        .register_serializer(NquadsSerializerPlugin)
        .unwrap();
    registry
        .register_serializer(NquadsChunkedSerializerPlugin)
        .unwrap();
    registry.register_export(FileExportPlugin).unwrap();
    registry.register_export(LogExportPlugin).unwrap();
    registry.register_export(StdoutExportPlugin).unwrap();
    registry
}

struct BotProducerPlugin;
struct BeoProducerPlugin;
struct BsddProducerPlugin;
struct PropsOpmProducerPlugin;
struct OmgFogProducerPlugin;
struct IfcowlProducerPlugin;
struct TurtleSerializerPlugin;
struct NquadsSerializerPlugin;
struct NquadsChunkedSerializerPlugin;
struct FileExportPlugin;
struct LogExportPlugin;
struct StdoutExportPlugin;

// ---------------------------------------------------------------------------
// Producer plugins
// ---------------------------------------------------------------------------

impl PipelinePlugin for BotProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: BOT_PRODUCER_ID,
            display_name: "BOT producer",
            stage: PipelineStage::Produce,
            description: "Generates BOT spatial-structure and element triples.",
            inputs: vec!["ifc-model", "tessellated-model (extended mode)"],
            outputs: vec!["bot-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
            named_graph_slug: Some("bot"),
            needs_full_graph: false,
        }
    }
}

impl ProducerPlugin for BotProducerPlugin {
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        let model = ctx.get::<IfcModel>().ok_or_else(|| {
            ProducerError::Conversion("BotProducerPlugin: missing IfcModel in context".to_string())
        })?;
        let options = ctx.get::<ConvertOptions>().ok_or_else(|| {
            ProducerError::Conversion(
                "BotProducerPlugin: missing ConvertOptions in context".to_string(),
            )
        })?;
        let (raw_sender, raw_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
        let graph_iri = BatchKind::new(format!("{}/bot", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());
        let bot_config = ctx
            .get::<lbd_topology::BotConfig>()
            .unwrap_or_else(|| std::sync::Arc::new(lbd_topology::BotConfig::default()));
        let topology = match bot_config.mode {
            lbd_topology::BotMode::Ifc => std::sync::Arc::new(lbd_topology::build_topology(&model)),
            lbd_topology::BotMode::Extended => {
                let tessellated = ctx
                    .get::<tessellated_model::TessellatedModel>()
                    .ok_or_else(|| {
                        ProducerError::Conversion(
                            "extended BOT mode requires neo-geometry-preprocess".to_string(),
                        )
                    })?;
                let (graph, stats) = plugin_topology_full::build_extended_topology_parallel(
                    &model,
                    &tessellated,
                    bot_config.tolerance,
                );
                ctx.write_log(
                    lbd_pipeline::BOT_PRODUCER_ID,
                    serde_json::json!({
                        "mode": "extended",
                        "semantic_edges": stats.semantic_edges,
                        "geometry_edges": stats.geometry_edges,
                        "indexed_meshes": stats.indexed_meshes,
                        "rstar_candidate_pairs": stats.candidate_pairs,
                        "rayon_threads": stats.rayon_threads,
                        "cache_mode": stats.cache_mode,
                        "prepared_mesh_builds": stats.prepared_mesh_builds,
                        "parry_checks": stats.candidate_pairs,
                        "parry_separated": stats.separated,
                        "parry_touching": stats.touching,
                        "parry_intersecting": stats.intersecting,
                        "parry_contained": stats.contained,
                        "subelement_relations": stats.propagated_from_subelements,
                        "candidate_seconds": stats.candidate_seconds,
                        "geometry_enrichment_seconds": stats.elapsed_seconds,
                        "tolerance": bot_config.tolerance,
                    }),
                );
                std::sync::Arc::new(graph)
            }
        };
        lbd_converter::stream_bot_with_topology(&model, &options, &topology, &raw_sender)
            .map(|_| ())
            .map_err(|_| ProducerError::ChannelClosed)
    }
}

impl PipelinePlugin for BeoProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: BEO_PRODUCER_ID,
            display_name: "BEO producer",
            stage: PipelineStage::Produce,
            description: "Generates BEO product-class type triples for building elements.",
            inputs: vec!["ifc-model"],
            outputs: vec!["beo-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
            named_graph_slug: Some("beo"),
            needs_full_graph: false,
        }
    }
}

impl ProducerPlugin for BeoProducerPlugin {
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        let model = ctx.get::<IfcModel>().ok_or_else(|| {
            ProducerError::Conversion("BeoProducerPlugin: missing IfcModel in context".to_string())
        })?;
        let options = ctx.get::<ConvertOptions>().ok_or_else(|| {
            ProducerError::Conversion(
                "BeoProducerPlugin: missing ConvertOptions in context".to_string(),
            )
        })?;
        let (raw_sender, raw_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
        let graph_iri = BatchKind::new(format!("{}/beo", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());
        stream_beo(&model, &options, &raw_sender)
            .map(|_| ())
            .map_err(|_| ProducerError::ChannelClosed)
    }
}

impl PipelinePlugin for PropsOpmProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PROPS_OPM_PRODUCER_ID,
            display_name: "PROPS-OPM producer",
            stage: PipelineStage::Produce,
            description: "Generates OPM-style property and quantity triples.",
            inputs: vec!["ifc-model"],
            outputs: vec!["props-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
            named_graph_slug: Some("props"),
            needs_full_graph: false,
        }
    }
}

impl PipelinePlugin for BsddProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: BSDD_PRODUCER_ID,
            display_name: "bSDD producer",
            stage: PipelineStage::Produce,
            description:
                "Generates standalone bSDD semantic class/property triples with OPM states.",
            inputs: vec!["ifc-model"],
            outputs: vec!["bsdd-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
            named_graph_slug: Some("bsdd"),
            needs_full_graph: false,
        }
    }
}

impl ProducerPlugin for BsddProducerPlugin {
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        let model = ctx.get::<IfcModel>().ok_or_else(|| {
            ProducerError::Conversion("BsddProducerPlugin: missing IfcModel in context".to_string())
        })?;
        let options = ctx.get::<ConvertOptions>().ok_or_else(|| {
            ProducerError::Conversion(
                "BsddProducerPlugin: missing ConvertOptions in context".to_string(),
            )
        })?;
        let (raw_sender, raw_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
        let graph_iri = BatchKind::new(format!("{}/bsdd", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());
        let cache = ctx.get::<BsddMatchCache>();
        let (_, dedup_stats) =
            stream_bsdd_with_cache(&model, &options, &raw_sender, cache.as_deref())
                .map_err(|e| ProducerError::Conversion(format!("bSDD streaming failed: {e}")))?;
        if options.bsdd_dedup_properties {
            ctx.write_log(
                BSDD_PRODUCER_ID,
                json!({
                    "dedup_properties": true,
                    "prop_instances_deduped": dedup_stats.prop_instances_deduped,
                    "set_defs_deduped": dedup_stats.set_defs_deduped,
                    "set_contains_deduped": dedup_stats.set_contains_deduped,
                    "total_triples_saved": dedup_stats.prop_instances_deduped
                        + dedup_stats.set_defs_deduped
                        + dedup_stats.set_contains_deduped,
                }),
            );
        }
        Ok(())
    }
}

impl ProducerPlugin for PropsOpmProducerPlugin {
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        let model = ctx.get::<IfcModel>().ok_or_else(|| {
            ProducerError::Conversion(
                "PropsOpmProducerPlugin: missing IfcModel in context".to_string(),
            )
        })?;
        let options = ctx.get::<ConvertOptions>().ok_or_else(|| {
            ProducerError::Conversion(
                "PropsOpmProducerPlugin: missing ConvertOptions in context".to_string(),
            )
        })?;
        let (raw_sender, raw_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
        let graph_iri = BatchKind::new(format!("{}/props", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());
        stream_props_opm(&model, &options, &raw_sender)
            .map(|_| ())
            .map_err(|_| ProducerError::ChannelClosed)
    }
}

impl PipelinePlugin for OmgFogProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: OMG_FOG_PRODUCER_ID,
            display_name: "OMG-FOG producer",
            stage: PipelineStage::Produce,
            description: "Generates OMG links and GeoSPARQL bounding boxes when tessellated geometry is available.",
            inputs: vec!["ifc-model", "tessellated-model (optional)"],
            outputs: vec!["omg-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
            named_graph_slug: Some("omg"),
            needs_full_graph: false,
        }
    }
}

impl ProducerPlugin for OmgFogProducerPlugin {
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        let model = ctx.get::<IfcModel>().ok_or_else(|| {
            ProducerError::Conversion(
                "OmgFogProducerPlugin: missing IfcModel in context".to_string(),
            )
        })?;
        let options = ctx.get::<ConvertOptions>().ok_or_else(|| {
            ProducerError::Conversion(
                "OmgFogProducerPlugin: missing ConvertOptions in context".to_string(),
            )
        })?;
        // Per-object geometry hashes and cached world-space bounding boxes.
        //
        // Computed here from the TessellatedModel rather than read from the geometry
        // producer: both are Produce-stage modules and run in parallel,
        // so omg cannot assume neo-geometry-producer has already run. The
        // TessellatedModel is a Preprocess output, so it is guaranteed present by the
        // time any producer starts.
        //
        // The model calculates its AABBs once during construction, so RDF and topology
        // reuse the same values without scanning the meshes again.
        let options = match ctx.get::<tessellated_model::TessellatedModel>() {
            Some(tessellated) => {
                let hashes = plugin_geometry_producer::stable_element_geometry_hashes(&tessellated);
                let mut with_geometry = (*options).clone();
                with_geometry.geometry_hashes = Some(std::sync::Arc::new(hashes));
                if with_geometry.omg_emit_bounding_boxes {
                    let boxes: std::collections::HashMap<_, _> = tessellated
                        .world_bounding_boxes()
                        .iter()
                        .map(|(&id, &[x_min, x_max, y_min, y_max, z_min, z_max])| {
                            (
                                id,
                                lbd_converter::BoundingBox {
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
                    with_geometry.geometry_bounding_boxes = Some(std::sync::Arc::new(boxes));
                }
                std::sync::Arc::new(with_geometry)
            }
            // No tessellation in this run (geometry preprocessing not enabled): emit the
            // structural links only, as before. Degrades rather than fails.
            None => options,
        };

        let (raw_sender, raw_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
        let graph_iri = BatchKind::new(format!("{}/omg", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());
        stream_omg_fog(&model, &options, &raw_sender)
            .map(|_| ())
            .map_err(|_| ProducerError::ChannelClosed)
    }
}

impl PipelinePlugin for IfcowlProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: IFCOWL_PRODUCER_ID,
            display_name: "Built-in IfcOWL producer",
            stage: PipelineStage::Produce,
            description: "Generates IfcOWL triples from parsed IFC STEP entities.",
            inputs: vec!["step-file", "ifc-model"],
            outputs: vec!["ifcowl-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ProducerPlugin for IfcowlProducerPlugin {
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        let step = ctx.get::<StepFile>().ok_or_else(|| {
            ProducerError::Conversion(
                "IfcowlProducerPlugin: missing StepFile in context".to_string(),
            )
        })?;
        let options = ctx.get::<ConvertOptions>().ok_or_else(|| {
            ProducerError::Conversion(
                "IfcowlProducerPlugin: missing ConvertOptions in context".to_string(),
            )
        })?;
        let (ifcowl_sender, ifcowl_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
        let graph_iri =
            BatchKind::new(format!("{}/ifcowl", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(ifcowl_receiver, graph_iri, sender.clone());
        lbd_converter::modules::ifcowl::stream_ifcowl(
            &step,
            &lbd_converter::normalize_base_uri(&options.base_uri),
            step.header.schema,
            &ifcowl_sender,
            options.stream_batch_size,
            options.ifcowl_max_workers,
            options.ifcowl_mode,
        )
        .map_err(|_| ProducerError::ChannelClosed)
    }
}

// ---------------------------------------------------------------------------
// Serializer plugins — registration only; serialization happens in main.rs
// ---------------------------------------------------------------------------

impl PipelinePlugin for TurtleSerializerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: TURTLE_SERIALIZER_ID,
            display_name: "Built-in Turtle serializer",
            stage: PipelineStage::Serialize,
            description: "Serializes triple streams into Turtle output.",
            inputs: vec!["triples"],
            outputs: vec!["turtle-bytes"],
            requires: vec![],
            conflicts_with: vec![NQUADS_SERIALIZER_ID, NQUADS_CHUNKED_SERIALIZER_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl SerializerPlugin for TurtleSerializerPlugin {}

impl PipelinePlugin for NquadsSerializerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: NQUADS_SERIALIZER_ID,
            display_name: "Built-in N-Quads serializer",
            stage: PipelineStage::Serialize,
            description: "Serializes graph streams into merged N-Quads output.",
            inputs: vec!["quads"],
            outputs: vec!["nquads-bytes"],
            requires: vec![],
            conflicts_with: vec![TURTLE_SERIALIZER_ID, NQUADS_CHUNKED_SERIALIZER_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl SerializerPlugin for NquadsSerializerPlugin {}

impl PipelinePlugin for NquadsChunkedSerializerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: NQUADS_CHUNKED_SERIALIZER_ID,
            display_name: "Built-in N-Quads chunked serializer",
            stage: PipelineStage::Serialize,
            description:
                "Serializes graph streams into chunked N-Quads files with a chunk manifest.",
            inputs: vec!["quads"],
            outputs: vec!["nquads-chunks"],
            requires: vec![],
            conflicts_with: vec![TURTLE_SERIALIZER_ID, NQUADS_SERIALIZER_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl SerializerPlugin for NquadsChunkedSerializerPlugin {}

// ---------------------------------------------------------------------------
// Export plugins
// ---------------------------------------------------------------------------

// --- FileExportPlugin -------------------------------------------------------

impl PipelinePlugin for FileExportPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: FILE_EXPORT_ID,
            display_name: "Built-in file exporter",
            stage: PipelineStage::Export,
            description:
                "Writes serialized output streams and sidecar artefacts to the local file system.",
            inputs: vec!["turtle-bytes", "nquads-bytes", "nquads-chunks"],
            outputs: vec!["filesystem"],
            requires: vec![],
            conflicts_with: vec![STDOUT_EXPORT_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: false,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ExportPlugin for FileExportPlugin {
    fn start_session(&self, ctx: &PipelineContext) -> Result<Box<dyn ExportSession>, ExportError> {
        let output_dir = ctx
            .get::<OutputDir>()
            .map(|d| d.0.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        let compress = ctx.get::<CompressOutput>().map(|c| c.0).unwrap_or(false);
        Ok(Box::new(CliFileExportSession {
            output_dir,
            compress,
            opened: Vec::new(),
            staged: Vec::new(),
            derived: Vec::new(),
        }))
    }
}

struct CliFileExportSession {
    output_dir: PathBuf,
    compress: bool,
    opened: Vec<(String, String, String)>, // (filename, mime_type, role)
    staged: Vec<(String, String, String)>, // (logical, temporary, final)
    derived: Vec<ExportFileSummary>,
}

impl ExportSession for CliFileExportSession {
    fn open_sink(
        &mut self,
        filename: &str,
        mime_type: &str,
        role: &str,
    ) -> Result<Box<dyn Write + Send>, ExportError> {
        let actual_filename = if self.compress {
            format!("{filename}.gz")
        } else {
            filename.to_string()
        };
        let path = self.output_dir.join(&actual_filename);
        let file = File::create(&path)
            .map_err(|e| ExportError::Export(format!("cannot create {}: {e}", path.display())))?;
        self.opened
            .push((actual_filename, mime_type.to_string(), role.to_string()));
        if self.compress {
            Ok(Box::new(GzEncoder::new(
                BufWriter::new(file),
                Compression::fast(),
            )))
        } else {
            Ok(Box::new(BufWriter::new(file)))
        }
    }

    fn open_staged_sink(
        &mut self,
        filename: &str,
        mime_type: &str,
        role: &str,
    ) -> Result<Box<dyn Write + Send>, ExportError> {
        let actual_filename = if self.compress {
            format!("{filename}.gz")
        } else {
            filename.to_string()
        };
        let temporary_filename = format!(".{actual_filename}.partial");
        let path = self.output_dir.join(&temporary_filename);
        let file = File::create(&path)
            .map_err(|e| ExportError::Export(format!("cannot create {}: {e}", path.display())))?;
        self.opened.push((
            actual_filename.clone(),
            mime_type.to_string(),
            role.to_string(),
        ));
        self.staged
            .push((filename.to_string(), temporary_filename, actual_filename));
        if self.compress {
            Ok(Box::new(GzEncoder::new(
                BufWriter::new(file),
                Compression::fast(),
            )))
        } else {
            Ok(Box::new(BufWriter::new(file)))
        }
    }

    fn commit_staged_sink(&mut self, filename: &str) -> Result<(), ExportError> {
        let index = self
            .staged
            .iter()
            .position(|(logical, _, _)| logical == filename)
            .ok_or_else(|| ExportError::Export(format!("unknown staged sink {filename}")))?;
        let (_, temporary_filename, actual_filename) = self.staged.remove(index);
        let temporary_path = self.output_dir.join(&temporary_filename);
        let final_path = self.output_dir.join(&actual_filename);
        std::fs::rename(&temporary_path, &final_path).map_err(|e| {
            ExportError::Export(format!(
                "cannot publish {} as {}: {e}",
                temporary_path.display(),
                final_path.display()
            ))
        })
    }

    fn published_filename(&self, filename: &str) -> String {
        if self.compress {
            format!("{filename}.gz")
        } else {
            filename.to_string()
        }
    }

    fn accept_derived_file(&mut self, file: DerivedFile) -> Result<(), ExportError> {
        let path = self.output_dir.join(&file.filename);
        std::fs::write(&path, &file.bytes)
            .map_err(|e| ExportError::Export(format!("cannot write {}: {e}", path.display())))?;
        self.derived.push(ExportFileSummary {
            filename: file.filename,
            mime_type: file.mime_type.to_string(),
            role: "derived".to_string(),
            bytes: file.bytes.len() as u64,
        });
        Ok(())
    }

    fn finalize(self: Box<Self>) -> Result<Vec<ExportFileSummary>, ExportError> {
        if !self.staged.is_empty() {
            return Err(ExportError::Export(format!(
                "{} staged output sink(s) were not committed",
                self.staged.len()
            )));
        }
        let mut summaries = self.derived;
        for (filename, mime_type, role) in &self.opened {
            let path = self.output_dir.join(filename);
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            summaries.push(ExportFileSummary {
                filename: filename.clone(),
                mime_type: mime_type.clone(),
                role: role.clone(),
                bytes,
            });
        }
        Ok(summaries)
    }
}

impl PipelinePlugin for LogExportPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: LOG_EXPORT_ID,
            display_name: "Log exporter",
            stage: PipelineStage::Export,
            description: "Writes normal output files plus a structured conversion log sidecar (JSON/JSON-LD).",
            inputs: vec!["turtle-bytes", "nquads-bytes", "nquads-chunks"],
            outputs: vec!["filesystem", "log-sidecar"],
            requires: vec![],
            conflicts_with: vec![STDOUT_EXPORT_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: false,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ExportPlugin for LogExportPlugin {
    fn start_session(&self, ctx: &PipelineContext) -> Result<Box<dyn ExportSession>, ExportError> {
        let output_dir = ctx
            .get::<OutputDir>()
            .map(|d| d.0.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        let logs = ctx.read_log_bundle();
        Ok(Box::new(CliLogExportSession {
            output_dir,
            opened: Vec::new(),
            staged: Vec::new(),
            derived: Vec::new(),
            logs,
        }))
    }
}

struct CliLogExportSession {
    output_dir: PathBuf,
    opened: Vec<(String, String, String)>,
    staged: Vec<(String, String)>, // (temporary, final)
    derived: Vec<ExportFileSummary>,
    logs: lbd_pipeline::PipelineLogBundle,
}

impl ExportSession for CliLogExportSession {
    fn open_sink(
        &mut self,
        filename: &str,
        mime_type: &str,
        role: &str,
    ) -> Result<Box<dyn Write + Send>, ExportError> {
        let path = self.output_dir.join(filename);
        let file = File::create(&path)
            .map_err(|e| ExportError::Export(format!("cannot create {}: {e}", path.display())))?;
        self.opened.push((
            filename.to_string(),
            mime_type.to_string(),
            role.to_string(),
        ));
        Ok(Box::new(BufWriter::new(file)))
    }

    fn open_staged_sink(
        &mut self,
        filename: &str,
        mime_type: &str,
        role: &str,
    ) -> Result<Box<dyn Write + Send>, ExportError> {
        let temporary_filename = format!(".{filename}.partial");
        let path = self.output_dir.join(&temporary_filename);
        let file = File::create(&path)
            .map_err(|e| ExportError::Export(format!("cannot create {}: {e}", path.display())))?;
        self.opened.push((
            filename.to_string(),
            mime_type.to_string(),
            role.to_string(),
        ));
        self.staged.push((temporary_filename, filename.to_string()));
        Ok(Box::new(BufWriter::new(file)))
    }

    fn commit_staged_sink(&mut self, filename: &str) -> Result<(), ExportError> {
        let index = self
            .staged
            .iter()
            .position(|(_, final_name)| final_name == filename)
            .ok_or_else(|| ExportError::Export(format!("unknown staged sink {filename}")))?;
        let (temporary_filename, final_filename) = self.staged.remove(index);
        let temporary_path = self.output_dir.join(&temporary_filename);
        let final_path = self.output_dir.join(&final_filename);
        std::fs::rename(&temporary_path, &final_path).map_err(|e| {
            ExportError::Export(format!(
                "cannot publish {} as {}: {e}",
                temporary_path.display(),
                final_path.display()
            ))
        })
    }

    fn accept_derived_file(&mut self, file: DerivedFile) -> Result<(), ExportError> {
        let path = self.output_dir.join(&file.filename);
        std::fs::write(&path, &file.bytes)
            .map_err(|e| ExportError::Export(format!("cannot write {}: {e}", path.display())))?;
        self.derived.push(ExportFileSummary {
            filename: file.filename,
            mime_type: file.mime_type.to_string(),
            role: "derived".to_string(),
            bytes: file.bytes.len() as u64,
        });
        Ok(())
    }

    fn finalize(self: Box<Self>) -> Result<Vec<ExportFileSummary>, ExportError> {
        if !self.staged.is_empty() {
            return Err(ExportError::Export(format!(
                "{} staged output sink(s) were not committed",
                self.staged.len()
            )));
        }
        let mut summaries = self.derived;
        for (filename, mime_type, role) in &self.opened {
            let path = self.output_dir.join(filename);
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            summaries.push(ExportFileSummary {
                filename: filename.clone(),
                mime_type: mime_type.clone(),
                role: role.clone(),
                bytes,
            });
        }
        let mut module_ids: Vec<&str> = self.logs.modules.keys().map(String::as_str).collect();
        module_ids.sort_unstable();
        for module_id in module_ids {
            let stats = &self.logs.modules[module_id];
            let filename = format!("{module_id}.log.json");
            let json = serde_json::to_vec_pretty(stats)
                .map_err(|e| ExportError::Export(format!("cannot serialize {filename}: {e}")))?;
            let path = self.output_dir.join(&filename);
            std::fs::write(&path, &json).map_err(|e| {
                ExportError::Export(format!("cannot write {}: {e}", path.display()))
            })?;
            summaries.push(ExportFileSummary {
                filename,
                mime_type: "application/json".to_string(),
                role: "log".to_string(),
                bytes: json.len() as u64,
            });
        }
        Ok(summaries)
    }
}

// --- StdoutExportPlugin -----------------------------------------------------

impl PipelinePlugin for StdoutExportPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: STDOUT_EXPORT_ID,
            display_name: "Built-in stdout exporter",
            stage: PipelineStage::Export,
            description: "Writes serialized output streams to stdout.",
            inputs: vec!["turtle-bytes", "nquads-bytes"],
            outputs: vec!["stdout"],
            requires: vec![],
            conflicts_with: vec![FILE_EXPORT_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: false,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ExportPlugin for StdoutExportPlugin {
    fn start_session(&self, _ctx: &PipelineContext) -> Result<Box<dyn ExportSession>, ExportError> {
        Ok(Box::new(StdoutExportSession {
            summaries: Vec::new(),
        }))
    }
}

struct StdoutExportSession {
    summaries: Vec<ExportFileSummary>,
}

impl ExportSession for StdoutExportSession {
    fn open_sink(
        &mut self,
        filename: &str,
        mime_type: &str,
        role: &str,
    ) -> Result<Box<dyn Write + Send>, ExportError> {
        let f = filename.to_string();
        let m = mime_type.to_string();
        let r = role.to_string();
        // Placeholder summary; bytes are unknown until finalize.
        self.summaries.push(ExportFileSummary {
            filename: f,
            mime_type: m,
            role: r,
            bytes: 0,
        });
        Ok(Box::new(CountingStdoutWriter::new()))
    }

    fn accept_derived_file(&mut self, _file: DerivedFile) -> Result<(), ExportError> {
        // stdout cannot handle sidecar binary artefacts
        Err(ExportError::Export(
            "StdoutExportPlugin does not support sidecar files; use neo-file-export instead"
                .to_string(),
        ))
    }

    fn finalize(self: Box<Self>) -> Result<Vec<ExportFileSummary>, ExportError> {
        Ok(self.summaries)
    }
}

/// A writer that counts bytes while forwarding to stdout.
struct CountingStdoutWriter {
    bytes: u64,
}

impl CountingStdoutWriter {
    fn new() -> Self {
        Self { bytes: 0 }
    }
}

impl Write for CountingStdoutWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = io::stdout().write(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::time::{SystemTime, UNIX_EPOCH};

    use flate2::read::GzDecoder;
    use lbd_pipeline::{ExportSession, PipelineStage};

    use crate::chunk_writer::{QuadChunkWriter, QuadChunkingMode};
    use crate::session;

    use super::{built_in_registry, CliFileExportSession};

    #[test]
    fn built_in_registry_exposes_expected_stage_counts() {
        let registry = built_in_registry();
        // Bot, Beo, Bsdd, PropsOpm, OmgFog, Ifcowl, RmlMapper, GeometryProducer
        assert_eq!(
            registry.manifests_for_stage(PipelineStage::Produce).len(),
            8
        );
        // OntologyMapper
        assert_eq!(
            registry
                .manifests_for_stage(PipelineStage::Postprocess)
                .len(),
            1
        );
        // Turtle, NQuads, NQuadsChunked
        assert_eq!(
            registry.manifests_for_stage(PipelineStage::Serialize).len(),
            3
        );
        // File, Log, Stdout
        assert_eq!(registry.manifests_for_stage(PipelineStage::Export).len(), 3);
    }

    #[test]
    fn staged_file_sink_is_hidden_until_commit() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let output_dir = std::env::temp_dir().join(format!("ifc2lbd-staged-sink-{unique}"));
        std::fs::create_dir_all(&output_dir).expect("mkdir");
        let mut session = CliFileExportSession {
            output_dir: output_dir.clone(),
            compress: true,
            opened: Vec::new(),
            staged: Vec::new(),
            derived: Vec::new(),
        };

        let mut sink = session
            .open_staged_sink("part-000.nq", "application/n-quads", "chunk-data")
            .expect("open staged sink");
        sink.write_all(b"<s> <p> <o> <g> .\n").expect("write");
        drop(sink);

        let temporary = output_dir.join(".part-000.nq.gz.partial");
        let published = output_dir.join("part-000.nq.gz");
        assert!(temporary.exists());
        assert!(!published.exists());

        session
            .commit_staged_sink("part-000.nq")
            .expect("commit staged sink");
        assert!(!temporary.exists());
        assert!(published.exists());

        let mut decoded = String::new();
        GzDecoder::new(std::fs::File::open(&published).expect("open gzip"))
            .read_to_string(&mut decoded)
            .expect("decode gzip");
        assert_eq!(decoded, "<s> <p> <o> <g> .\n");

        std::fs::remove_dir_all(output_dir).ok();
    }

    #[test]
    fn gzip_chunk_manifest_references_published_gzip_files() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let output_dir = std::env::temp_dir().join(format!("ifc2lbd-gzip-chunk-manifest-{unique}"));
        std::fs::create_dir_all(&output_dir).expect("mkdir");
        let export_session = CliFileExportSession {
            output_dir: output_dir.clone(),
            compress: true,
            opened: Vec::new(),
            staged: Vec::new(),
            derived: Vec::new(),
        };
        let shared_session = session::new_shared(Box::new(export_session));
        let mut writer = QuadChunkWriter::new(
            shared_session,
            "test".to_string(),
            QuadChunkingMode::Lines,
            1,
            1024,
            1,
            None,
        )
        .expect("new writer");
        writer
            .write_all(b"<s1> <p> <o> <g> .\n<s2> <p> <o> <g> .\n")
            .expect("write chunks");
        writer.finish().expect("finish chunks");

        let manifest_path = output_dir.join("test.manifest.json.gz");
        let mut decoded = String::new();
        GzDecoder::new(std::fs::File::open(&manifest_path).expect("open manifest"))
            .read_to_string(&mut decoded)
            .expect("decode manifest");
        let manifest: serde_json::Value = serde_json::from_str(&decoded).expect("parse manifest");
        let files = manifest["files"].as_array().expect("manifest files");
        assert_eq!(files.len(), 2);
        for (index, entry) in files.iter().enumerate() {
            let filename = entry["file"].as_str().expect("manifest filename");
            assert_eq!(filename, format!("test.part-{index:03}.nq.gz"));
            assert!(output_dir.join(filename).exists());
            assert!(!output_dir.join(format!("test.part-{index:03}.nq")).exists());
        }

        std::fs::remove_dir_all(output_dir).ok();
    }
}
