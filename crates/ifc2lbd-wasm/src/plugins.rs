use std::io::Write;
use std::sync::{Arc, Mutex};

use crossbeam::channel::Sender;
use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_converter::{stream_beo, stream_bot, stream_bsdd_with_cache, stream_omg_fog, stream_props_opm, BsddMatchCache, ConvertOptions};
use serde_json::json;
use lbd_ontology::Triple;
use lbd_pipeline::{
    BatchKind, DerivedFile, ExportError, ExportFileSummary, ExportPlugin, ExportSession,
    FailurePolicy, FILE_EXPORT_ID, IFCOWL_PRODUCER_ID, LOG_EXPORT_ID, NQUADS_CHUNKED_SERIALIZER_ID,
    NQUADS_SERIALIZER_ID, OMG_FOG_PRODUCER_ID, ParallelismMode, PipelineContext, PipelinePlugin,
    PipelineStage, PluginManifest, PluginRegistry, ProducerError,
    ProducerPlugin, SerializerPlugin, TaggedBatch, BEO_PRODUCER_ID, BOT_PRODUCER_ID,
    BSDD_PRODUCER_ID, PROPS_OPM_PRODUCER_ID, TURTLE_SERIALIZER_ID,
};
use serde_json;
use plugin_property_preprocess::{BsddMatchPreprocessPlugin, CleanupPreprocessPlugin};
use plugin_fragments_producer::FragmentsProducerPlugin;
use plugin_qto_preprocess::QtoPreprocessPlugin;
use plugin_geometry_preprocess::GeometryPreprocessPlugin;
use plugin_geometry_producer::GeometryProducerPlugin;
use wasm_bindgen::prelude::*;

use crate::types::ModuleManifestView;

pub(crate) fn to_view(manifest: PluginManifest) -> ModuleManifestView {
    ModuleManifestView {
        id: manifest.id.to_string(),
        display_name: manifest.display_name.to_string(),
        stage: format!("{:?}", manifest.stage),
        description: manifest.description.to_string(),
        inputs: manifest.inputs.into_iter().map(str::to_string).collect(),
        outputs: manifest.outputs.into_iter().map(str::to_string).collect(),
        requires: manifest.requires.into_iter().map(str::to_string).collect(),
        conflicts_with: manifest
            .conflicts_with
            .into_iter()
            .map(str::to_string)
            .collect(),
        failure_policy: format!("{:?}", manifest.failure_policy),
        parallelism: format!("{:?}", manifest.parallelism),
        wasm_compatible: manifest.wasm_compatible,
        option_keys: module_option_keys(manifest.id),
    }
}

pub(crate) fn module_option_keys(module_id: &str) -> Vec<String> {
    match module_id {
        NQUADS_SERIALIZER_ID => vec![],
        NQUADS_CHUNKED_SERIALIZER_ID => vec![
            "chunking".to_string(),
            "chunk_size_lines".to_string(),
            "chunk_size_bytes".to_string(),
            "chunk_prefix".to_string(),
        ],
        TURTLE_SERIALIZER_ID => vec!["grouping".to_string(), "layout".to_string()],
        IFCOWL_PRODUCER_ID => vec!["mode".to_string()],
        BSDD_PRODUCER_ID => vec!["profile".to_string(), "compact".to_string(), "include_standard_attrs".to_string(), "dedup_properties".to_string()],
        FILE_EXPORT_ID => vec!["output_stem".to_string()],
        LOG_EXPORT_ID => vec![],
        "neo-geometry-preprocess" => vec!["metadata".to_string()],
        "neo-geometry-producer" => vec!["format".to_string()],
        _ => Vec::new(),
    }
}

pub(crate) fn browser_registry() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry.register_preprocess(CleanupPreprocessPlugin).unwrap();
    registry.register_preprocess(BsddMatchPreprocessPlugin).unwrap();
    registry.register_preprocess(QtoPreprocessPlugin).unwrap();
    // Modular LBD producers
    registry.register_producer(BotProducerPlugin).unwrap();
    registry.register_producer(BeoProducerPlugin).unwrap();
    registry.register_producer(BsddProducerPlugin).unwrap();
    registry.register_producer(PropsOpmProducerPlugin).unwrap();
    registry.register_producer(OmgFogProducerPlugin).unwrap();
    registry.register_producer(FragmentsProducerPlugin).unwrap();
    // Geometry pipeline
    registry.register_preprocess(GeometryPreprocessPlugin).unwrap();
    registry.register_producer(GeometryProducerPlugin::default()).unwrap();
    // Other producers
    registry.register_producer(IfcowlProducerPlugin).unwrap();
    // Serializers (registration only; serialization happens in runner.rs)
    registry.register_serializer(TurtleSerializerPlugin).unwrap();
    registry.register_serializer(NquadsSerializerPlugin).unwrap();
    registry.register_serializer(NquadsChunkedSerializerPlugin).unwrap();
    // Export
    registry.register_export(FileExportPlugin).unwrap();
    registry.register_export(LogExportPlugin).unwrap();
    registry
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Spawn a task that forwards raw-triple batches as tagged batches.
fn forward_as_tagged(
    raw_receiver: crossbeam::channel::Receiver<Vec<Triple>>,
    kind: BatchKind,
    tagged_sender: Sender<TaggedBatch>,
) {
    rayon::spawn(move || {
        for batch in raw_receiver {
            if tagged_sender
                .send(TaggedBatch { kind: kind.clone(), triples: batch })
                .is_err()
            {
                break;
            }
        }
    });
}

pub(crate) fn js_err<E: ToString>(error: E) -> JsValue {
    JsValue::from_str(&error.to_string())
}

// ---------------------------------------------------------------------------
// BOT Producer
// ---------------------------------------------------------------------------

struct BotProducerPlugin;

impl PipelinePlugin for BotProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: BOT_PRODUCER_ID,
            display_name: "BOT",
            stage: PipelineStage::Produce,
            description: "Generates BOT spatial hierarchy and element-type triples.",
            inputs: vec!["ifc-model"],
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
        let graph_iri =
            BatchKind::new(format!("{}bot", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());

        stream_bot(&model, &options, &raw_sender)
            .map(|_| ())
            .map_err(|_| ProducerError::ChannelClosed)
    }
}

// ---------------------------------------------------------------------------
// BEO Producer
// ---------------------------------------------------------------------------

struct BeoProducerPlugin;
struct BsddProducerPlugin;

impl PipelinePlugin for BeoProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: BEO_PRODUCER_ID,
            display_name: "BEO",
            stage: PipelineStage::Produce,
            description: "Generates BEO / FURN product-class type triples for IFC elements.",
            inputs: vec!["ifc-model"],
            outputs: vec!["beo-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
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
        let graph_iri =
            BatchKind::new(format!("{}beo", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());

        stream_beo(&model, &options, &raw_sender)
            .map(|_| ())
            .map_err(|_| ProducerError::ChannelClosed)
    }
}

// ---------------------------------------------------------------------------
// bSDD Producer
// ---------------------------------------------------------------------------

impl PipelinePlugin for BsddProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: BSDD_PRODUCER_ID,
            display_name: "bSDD",
            stage: PipelineStage::Produce,
            description: "Generates standalone bSDD semantic class/property triples with OPM states.",
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
        let graph_iri =
            BatchKind::new(format!("{}bsdd", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());

        let cache = ctx.get::<BsddMatchCache>();
        let (_, dedup_stats) = stream_bsdd_with_cache(&model, &options, &raw_sender, cache.as_deref())
            .map_err(|e| ProducerError::Conversion(format!("bSDD streaming failed: {e}")))?;
        if options.bsdd_dedup_properties {
            ctx.write_log(BSDD_PRODUCER_ID, json!({
                "dedup_properties": true,
                "prop_instances_deduped": dedup_stats.prop_instances_deduped,
                "set_defs_deduped": dedup_stats.set_defs_deduped,
                "set_contains_deduped": dedup_stats.set_contains_deduped,
                "total_triples_saved": dedup_stats.prop_instances_deduped
                    + dedup_stats.set_defs_deduped
                    + dedup_stats.set_contains_deduped,
            }));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Props-OPM Producer
// ---------------------------------------------------------------------------

struct PropsOpmProducerPlugin;

impl PipelinePlugin for PropsOpmProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PROPS_OPM_PRODUCER_ID,
            display_name: "Props-OPM",
            stage: PipelineStage::Produce,
            description: "Generates OPM property-set, quantity-set and standard-attribute triples.",
            inputs: vec!["ifc-model"],
            outputs: vec!["props-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
            named_graph_slug: Some("props"),
            needs_full_graph: false,
        }
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
        let graph_iri =
            BatchKind::new(format!("{}props", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());

        stream_props_opm(&model, &options, &raw_sender)
            .map(|_| ())
            .map_err(|_| ProducerError::ChannelClosed)
    }
}

// ---------------------------------------------------------------------------
// OMG-FOG Producer
// ---------------------------------------------------------------------------

struct OmgFogProducerPlugin;

impl PipelinePlugin for OmgFogProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: OMG_FOG_PRODUCER_ID,
            display_name: "OMG-FOG",
            stage: PipelineStage::Produce,
            description: "Generates OMG geometry-link triples (omg:hasGeometry / omg:Geometry) for all elements and spatial nodes.",
            inputs: vec!["ifc-model"],
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

        let (raw_sender, raw_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
        let graph_iri =
            BatchKind::new(format!("{}omg", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());

        stream_omg_fog(&model, &options, &raw_sender)
            .map(|_| ())
            .map_err(|_| ProducerError::ChannelClosed)
    }
}

// ---------------------------------------------------------------------------
// IfcOWL Producer
// ---------------------------------------------------------------------------

struct IfcowlProducerPlugin;

impl PipelinePlugin for IfcowlProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: IFCOWL_PRODUCER_ID,
            display_name: "IfcOWL",
            stage: PipelineStage::Produce,
            description: "Generates IfcOWL triples from parsed IFC STEP entities.",
            inputs: vec!["step-file", "ifc-model"],
            outputs: vec!["ifcowl-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
            named_graph_slug: Some("ifcowl"),
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
        let _model = ctx.get::<IfcModel>().ok_or_else(|| {
            ProducerError::Conversion(
                "IfcowlProducerPlugin: missing IfcModel in context".to_string(),
            )
        })?;
        let options = ctx.get::<ConvertOptions>().ok_or_else(|| {
            ProducerError::Conversion(
                "IfcowlProducerPlugin: missing ConvertOptions in context".to_string(),
            )
        })?;

        let (ifcowl_sender, ifcowl_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
        let graph_iri = BatchKind::new(format!(
            "{}ifcowl",
            options.base_uri.trim_end_matches('/')
        ));
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
// Serializer plugins — registration only; serialization happens in runner.rs
// ---------------------------------------------------------------------------

struct TurtleSerializerPlugin;

impl PipelinePlugin for TurtleSerializerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: TURTLE_SERIALIZER_ID,
            display_name: "Turtle serializer",
            stage: PipelineStage::Serialize,
            description: "Serializes triples into Turtle output.",
            inputs: vec!["triples"],
            outputs: vec!["turtle-bytes"],
            requires: vec![],
            conflicts_with: vec![NQUADS_SERIALIZER_ID, NQUADS_CHUNKED_SERIALIZER_ID],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl SerializerPlugin for TurtleSerializerPlugin {}

struct NquadsSerializerPlugin;

impl PipelinePlugin for NquadsSerializerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: NQUADS_SERIALIZER_ID,
            display_name: "N-Quads serializer",
            stage: PipelineStage::Serialize,
            description: "Serializes graph streams into N-Quads output.",
            inputs: vec!["quads"],
            outputs: vec!["nquads-bytes"],
            requires: vec![],
            conflicts_with: vec![TURTLE_SERIALIZER_ID, NQUADS_CHUNKED_SERIALIZER_ID],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl SerializerPlugin for NquadsSerializerPlugin {}

struct NquadsChunkedSerializerPlugin;

impl PipelinePlugin for NquadsChunkedSerializerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: NQUADS_CHUNKED_SERIALIZER_ID,
            display_name: "N-Quads chunked serializer",
            stage: PipelineStage::Serialize,
            description: "Serializes graph streams into chunked N-Quads output.",
            inputs: vec!["quads"],
            outputs: vec!["nquads-bytes"],
            requires: vec![],
            conflicts_with: vec![TURTLE_SERIALIZER_ID, NQUADS_SERIALIZER_ID],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl SerializerPlugin for NquadsChunkedSerializerPlugin {}

// ---------------------------------------------------------------------------
// File Export — in-memory collector for browser download
// ---------------------------------------------------------------------------

struct FileExportPlugin;
struct LogExportPlugin;

impl PipelinePlugin for FileExportPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: FILE_EXPORT_ID,
            display_name: "File exporter",
            stage: PipelineStage::Export,
            description: "Collects serialized output and sidecar artefacts in memory for browser download.",
            inputs: vec!["turtle-bytes", "nquads-bytes"],
            outputs: vec!["browser-files"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ExportPlugin for FileExportPlugin {
    fn start_session(
        &self,
        _ctx: &PipelineContext,
    ) -> Result<Box<dyn ExportSession>, ExportError> {
        Ok(Box::new(WasmFileExportSession {
            buffers: Arc::new(Mutex::new(Vec::new())),
            derived: Vec::new(),
        }))
    }
}

impl PipelinePlugin for LogExportPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: LOG_EXPORT_ID,
            display_name: "Log exporter",
            stage: PipelineStage::Export,
            description: "Collects serialized output and writes conversion-log.json sidecar in memory.",
            inputs: vec!["turtle-bytes", "nquads-bytes"],
            outputs: vec!["browser-files", "log-sidecar"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ExportPlugin for LogExportPlugin {
    fn start_session(
        &self,
        ctx: &PipelineContext,
    ) -> Result<Box<dyn ExportSession>, ExportError> {
        let logs = ctx.read_log_bundle();
        Ok(Box::new(WasmLogExportSession {
            buffers: Arc::new(Mutex::new(Vec::new())),
            derived: Vec::new(),
            logs,
        }))
    }
}

/// In-memory export session for the browser environment.
///
/// `open_sink()` returns a writer that appends to an in-memory `Vec<u8>`.
/// `finalize()` packages every collected buffer as an `ExportFileSummary`.
struct WasmFileExportSession {
    /// Shared storage: (filename, mime_type, role, bytes).
    buffers: Arc<Mutex<Vec<(String, String, String, Vec<u8>)>>>,
    derived: Vec<ExportFileSummary>,
}

struct WasmLogExportSession {
    buffers: Arc<Mutex<Vec<(String, String, String, Vec<u8>)>>>,
    derived: Vec<ExportFileSummary>,
    logs: lbd_pipeline::PipelineLogBundle,
}

impl ExportSession for WasmFileExportSession {
    fn open_sink(
        &mut self,
        filename: &str,
        mime_type: &str,
        role: &str,
    ) -> Result<Box<dyn Write + Send>, ExportError> {
        let buffers = Arc::clone(&self.buffers);
        let filename = filename.to_string();
        let mime_type = mime_type.to_string();
        let role = role.to_string();
        Ok(Box::new(WasmSinkWriter {
            buffers,
            filename,
            mime_type,
            role,
            buf: Vec::new(),
        }))
    }

    fn accept_derived_file(&mut self, file: DerivedFile) -> Result<(), ExportError> {
        self.derived.push(ExportFileSummary {
            filename: file.filename,
            mime_type: file.mime_type.to_string(),
            role: "derived".to_string(),
            bytes: file.bytes.len() as u64,
        });
        Ok(())
    }

    fn finalize(self: Box<Self>) -> Result<Vec<ExportFileSummary>, ExportError> {
        let mut summaries = self.derived;
        let guard = self.buffers.lock().unwrap();
        for (filename, mime_type, role, bytes) in guard.iter() {
            summaries.push(ExportFileSummary {
                filename: filename.clone(),
                mime_type: mime_type.clone(),
                role: role.clone(),
                bytes: bytes.len() as u64,
            });
        }
        Ok(summaries)
    }
}

impl ExportSession for WasmLogExportSession {
    fn open_sink(
        &mut self,
        filename: &str,
        mime_type: &str,
        role: &str,
    ) -> Result<Box<dyn Write + Send>, ExportError> {
        let buffers = Arc::clone(&self.buffers);
        let filename = filename.to_string();
        let mime_type = mime_type.to_string();
        let role = role.to_string();
        Ok(Box::new(WasmSinkWriter {
            buffers,
            filename,
            mime_type,
            role,
            buf: Vec::new(),
        }))
    }

    fn accept_derived_file(&mut self, file: DerivedFile) -> Result<(), ExportError> {
        self.derived.push(ExportFileSummary {
            filename: file.filename,
            mime_type: file.mime_type.to_string(),
            role: "derived".to_string(),
            bytes: file.bytes.len() as u64,
        });
        Ok(())
    }

    fn finalize(self: Box<Self>) -> Result<Vec<ExportFileSummary>, ExportError> {
        let mut summaries = self.derived;
        let mut guard = self.buffers.lock().unwrap();
        let mut module_ids: Vec<&str> = self.logs.modules.keys().map(String::as_str).collect();
        module_ids.sort_unstable();
        for module_id in module_ids {
            let stats = &self.logs.modules[module_id];
            let filename = format!("{module_id}.log.json");
            let json = serde_json::to_vec_pretty(stats)
                .map_err(|e| ExportError::Export(format!("cannot serialize {filename}: {e}")))?;
            guard.push((filename, "application/json".to_string(), "log".to_string(), json));
        }
        for (filename, mime_type, role, bytes) in guard.iter() {
            summaries.push(ExportFileSummary {
                filename: filename.clone(),
                mime_type: mime_type.clone(),
                role: role.clone(),
                bytes: bytes.len() as u64,
            });
        }
        Ok(summaries)
    }
}

/// A writer that accumulates bytes and on flush/drop registers the buffer in
/// the shared `WasmFileExportSession`.
struct WasmSinkWriter {
    buffers: Arc<Mutex<Vec<(String, String, String, Vec<u8>)>>>,
    filename: String,
    mime_type: String,
    role: String,
    buf: Vec<u8>,
}

impl Write for WasmSinkWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for WasmSinkWriter {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.buffers.lock() {
            guard.push((
                self.filename.clone(),
                self.mime_type.clone(),
                self.role.clone(),
                std::mem::take(&mut self.buf),
            ));
        }
    }
}
