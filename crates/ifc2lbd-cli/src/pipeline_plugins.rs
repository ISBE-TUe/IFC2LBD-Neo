use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::thread;

use anyhow::Context;
use crossbeam::channel::{Receiver, Sender};
use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_converter::{stream_beo, stream_bot, stream_bsdd_with_cache, stream_omg_fog, stream_props_opm, BsddMatchCache, ConvertOptions};
use lbd_ontology::Triple;
use lbd_pipeline::{
    BatchKind, DerivedFile, ExportError, ExportFileSummary, ExportPlugin, ExportSession,
    FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage, PluginManifest,
    PipelineLogBundle, PluginRegistry, ProducerError, ProducerPlugin, SerializerPlugin, TaggedBatch, BEO_PRODUCER_ID,
    BOT_PRODUCER_ID, BSDD_PRODUCER_ID, FILE_EXPORT_ID, GRAFEO_EXPORT_ID, IFCOWL_PRODUCER_ID,
    LOG_EXPORT_ID, NQUADS_CHUNKED_SERIALIZER_ID, NQUADS_SERIALIZER_ID, OMG_FOG_PRODUCER_ID,
    PROPS_OPM_PRODUCER_ID, STDOUT_EXPORT_ID, TURTLE_SERIALIZER_ID,
};
use plugin_property_preprocess::{BsddMatchPreprocessPlugin, CleanupPreprocessPlugin};
use serde::{Deserialize, Serialize};

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

// ---------------------------------------------------------------------------
// Helpers shared by producer implementations
// ---------------------------------------------------------------------------

/// Spawn a rayon task that forwards raw-triple batches as tagged batches.
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

pub fn built_in_registry() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry.register_preprocess(CleanupPreprocessPlugin).unwrap();
    registry.register_preprocess(BsddMatchPreprocessPlugin).unwrap();
    registry.register_producer(BotProducerPlugin).unwrap();
    registry.register_producer(BeoProducerPlugin).unwrap();
    registry.register_producer(BsddProducerPlugin).unwrap();
    registry.register_producer(PropsOpmProducerPlugin).unwrap();
    registry.register_producer(OmgFogProducerPlugin).unwrap();
    registry.register_producer(IfcowlProducerPlugin).unwrap();
    registry.register_serializer(TurtleSerializerPlugin).unwrap();
    registry.register_serializer(NquadsSerializerPlugin).unwrap();
    registry.register_serializer(NquadsChunkedSerializerPlugin).unwrap();
    registry.register_export(FileExportPlugin).unwrap();
    registry.register_export(LogExportPlugin).unwrap();
    registry.register_export(StdoutExportPlugin).unwrap();
    registry.register_export(GrafeoExportPlugin).unwrap();
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
struct GrafeoExportPlugin;

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
        let graph_iri = BatchKind::new(format!("{}bot", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());
        stream_bot(&model, &options, &raw_sender)
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
            description: "Generates BEO/FURN product-class type triples for building elements.",
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
        let graph_iri = BatchKind::new(format!("{}beo", options.base_uri.trim_end_matches('/')));
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
        let graph_iri = BatchKind::new(format!("{}bsdd", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());
        let cache = ctx.get::<BsddMatchCache>();
        stream_bsdd_with_cache(&model, &options, &raw_sender, cache.as_deref())
            .map(|_| ())
            .map_err(|e| ProducerError::Conversion(format!("bSDD streaming failed: {e}")))
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
        let graph_iri = BatchKind::new(format!("{}props", options.base_uri.trim_end_matches('/')));
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
            description: "Generates OMG/FOG geometry property triples.",
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
        let graph_iri = BatchKind::new(format!("{}omg", options.base_uri.trim_end_matches('/')));
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
            BatchKind::new(format!("{}ifcowl", options.base_uri.trim_end_matches('/')));
        forward_as_tagged(ifcowl_receiver, graph_iri, sender.clone());
        lbd_converter::modules::ifcowl::stream_ifcowl(
            &step,
            &lbd_converter::normalize_base_uri(&options.base_uri),
            step.header.schema,
            &ifcowl_sender,
            options.stream_batch_size,
            options.ifcowl_max_workers,
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
            description: "Serializes graph streams into chunked N-Quads files with a chunk manifest.",
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
            description: "Writes serialized output streams and sidecar artefacts to the local file system.",
            inputs: vec!["turtle-bytes", "nquads-bytes", "nquads-chunks"],
            outputs: vec!["filesystem"],
            requires: vec![],
            conflicts_with: vec![STDOUT_EXPORT_ID, GRAFEO_EXPORT_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: false,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ExportPlugin for FileExportPlugin {
    fn start_session(
        &self,
        ctx: &PipelineContext,
    ) -> Result<Box<dyn ExportSession>, ExportError> {
        let output_dir = ctx
            .get::<OutputDir>()
            .map(|d| d.0.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(Box::new(CliFileExportSession {
            output_dir,
            opened: Vec::new(),
            derived: Vec::new(),
        }))
    }
}

struct CliFileExportSession {
    output_dir: PathBuf,
    opened: Vec<(String, String, String)>, // (filename, mime_type, role)
    derived: Vec<ExportFileSummary>,
}

impl ExportSession for CliFileExportSession {
    fn open_sink(
        &mut self,
        filename: &str,
        mime_type: &str,
        role: &str,
    ) -> Result<Box<dyn Write + Send>, ExportError> {
        let path = self.output_dir.join(filename);
        let file = File::create(&path)
            .map_err(|e| ExportError::Export(format!("cannot create {}: {e}", path.display())))?;
        self.opened
            .push((filename.to_string(), mime_type.to_string(), role.to_string()));
        Ok(Box::new(BufWriter::new(file)))
    }

    fn accept_derived_file(&mut self, file: DerivedFile) -> Result<(), ExportError> {
        let path = self.output_dir.join(&file.filename);
        std::fs::write(&path, &file.bytes).map_err(|e| {
            ExportError::Export(format!("cannot write {}: {e}", path.display()))
        })?;
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
            conflicts_with: vec![FILE_EXPORT_ID, STDOUT_EXPORT_ID, GRAFEO_EXPORT_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: false,
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
        let output_dir = ctx
            .get::<OutputDir>()
            .map(|d| d.0.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        let logs = ctx.get::<PipelineLogBundle>().map(|l| (*l).clone()).unwrap_or_default();
        Ok(Box::new(CliLogExportSession {
            output_dir,
            opened: Vec::new(),
            derived: Vec::new(),
            logs,
        }))
    }
}

struct CliLogExportSession {
    output_dir: PathBuf,
    opened: Vec<(String, String, String)>,
    derived: Vec<ExportFileSummary>,
    logs: PipelineLogBundle,
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
        self.opened
            .push((filename.to_string(), mime_type.to_string(), role.to_string()));
        Ok(Box::new(BufWriter::new(file)))
    }

    fn accept_derived_file(&mut self, file: DerivedFile) -> Result<(), ExportError> {
        let path = self.output_dir.join(&file.filename);
        std::fs::write(&path, &file.bytes).map_err(|e| {
            ExportError::Export(format!("cannot write {}: {e}", path.display()))
        })?;
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
        let json_path = self.output_dir.join("conversion-log.json");
        let json = serde_json::to_vec_pretty(&self.logs)
            .map_err(|e| ExportError::Export(format!("cannot serialize conversion-log.json: {e}")))?;
        std::fs::write(&json_path, &json)
            .map_err(|e| ExportError::Export(format!("cannot write {}: {e}", json_path.display())))?;
        summaries.push(ExportFileSummary {
            filename: "conversion-log.json".to_string(),
            mime_type: "application/json".to_string(),
            role: "log".to_string(),
            bytes: json.len() as u64,
        });
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
            conflicts_with: vec![FILE_EXPORT_ID, GRAFEO_EXPORT_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: false,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ExportPlugin for StdoutExportPlugin {
    fn start_session(
        &self,
        _ctx: &PipelineContext,
    ) -> Result<Box<dyn ExportSession>, ExportError> {
        Ok(Box::new(StdoutExportSession { summaries: Vec::new() }))
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

// --- GrafeoExportPlugin -----------------------------------------------------

const DIRECT_STREAM_VERSION: u8 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct DirectStreamFrame {
    version: u8,
    graph: String,
    triples: Vec<DirectStreamTriple>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DirectStreamTriple {
    subject: DirectStreamTerm,
    predicate: DirectStreamTerm,
    object: DirectStreamTerm,
}

#[derive(Debug, Serialize, Deserialize)]
enum DirectStreamTerm {
    Iri(String),
    BlankNode(String),
    Literal(String),
    TypedLiteral { value: String, datatype: String },
    LangLiteral { value: String, lang: String },
}

impl PipelinePlugin for GrafeoExportPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: GRAFEO_EXPORT_ID,
            display_name: "Built-in Grafeo exporter",
            stage: PipelineStage::Export,
            description: "Frames graph batches for direct Grafeo ingestion.",
            inputs: vec!["quads", "triple-batches"],
            outputs: vec!["grafeo-stream"],
            requires: vec![NQUADS_SERIALIZER_ID],
            conflicts_with: vec![FILE_EXPORT_ID, STDOUT_EXPORT_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: false,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ExportPlugin for GrafeoExportPlugin {
    fn start_session(
        &self,
        ctx: &PipelineContext,
    ) -> Result<Box<dyn ExportSession>, ExportError> {
        let output_dir = ctx
            .get::<OutputDir>()
            .map(|d| d.0.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(Box::new(GrafeoExportSession {
            output_dir,
            bytes_written: 0,
        }))
    }
}

/// Grafeo export session.
///
/// Grafeo uses a binary-framed protocol (bincode-encoded `DirectStreamFrame`
/// structs) rather than a plain byte stream. The `open_sink()` method returns
/// a `GrafeoFrameWriter` that accumulates triples and flushes them as Grafeo
/// frames.
///
/// For the bespoke pre-existing Grafeo path from `main.rs` that uses producer
/// channels directly, see `stream_grafeo_batches_to_writer()` below.
struct GrafeoExportSession {
    output_dir: PathBuf,
    bytes_written: u64,
}

impl ExportSession for GrafeoExportSession {
    fn open_sink(
        &mut self,
        filename: &str,
        _mime_type: &str,
        _role: &str,
    ) -> Result<Box<dyn Write + Send>, ExportError> {
        let path = self.output_dir.join(filename);
        let file = File::create(&path)
            .map_err(|e| ExportError::Export(format!("cannot create {}: {e}", path.display())))?;
        Ok(Box::new(GrafeoFrameWriter::new(BufWriter::new(file))))
    }

    fn accept_derived_file(&mut self, file: DerivedFile) -> Result<(), ExportError> {
        let path = self.output_dir.join(&file.filename);
        std::fs::write(&path, &file.bytes).map_err(|e| {
            ExportError::Export(format!("cannot write {}: {e}", path.display()))
        })?;
        self.bytes_written += file.bytes.len() as u64;
        Ok(())
    }

    fn finalize(self: Box<Self>) -> Result<Vec<ExportFileSummary>, ExportError> {
        Ok(vec![ExportFileSummary {
            filename: "grafeo-stream".to_string(),
            mime_type: "application/octet-stream".to_string(),
            role: "grafeo".to_string(),
            bytes: self.bytes_written,
        }])
    }
}

/// A `Write` implementation that buffers raw N-Quads lines and encodes them as
/// Grafeo binary frames. Each line is a complete N-Quad; when the buffer
/// reaches `FRAME_LINE_LIMIT`, a frame is flushed.
struct GrafeoFrameWriter<W: Write> {
    inner: W,
    line_buf: Vec<u8>,
}

const GRAFEO_FRAME_BYTES: usize = 256 * 1024;

impl<W: Write + Send> GrafeoFrameWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            line_buf: Vec::with_capacity(GRAFEO_FRAME_BYTES),
        }
    }

    fn flush_frame(&mut self) -> io::Result<()> {
        if self.line_buf.is_empty() {
            return Ok(());
        }
        // Write raw bytes; callers handle framing at the grafeo protocol level.
        self.inner.write_all(&self.line_buf)?;
        self.line_buf.clear();
        Ok(())
    }
}

impl<W: Write + Send> Write for GrafeoFrameWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.line_buf.extend_from_slice(buf);
        if self.line_buf.len() >= GRAFEO_FRAME_BYTES {
            self.flush_frame()?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_frame()?;
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// Grafeo batch-streaming helper (bespoke path used by main.rs)
// ---------------------------------------------------------------------------

/// Stream LBD/IfcOWL/topology triple batches directly into a Grafeo-compatible
/// binary-framed writer.
///
/// This function implements the pre-existing Grafeo streaming path used by
/// `main.rs`. It operates on raw producer-channel receivers rather than the
/// `ExportSession` API, because the Grafeo protocol requires per-graph framing
/// that cannot be expressed through a generic `Write` sink.
///
/// New exporters that target Grafeo should implement `ExportPlugin` and use
/// `GrafeoExportSession::open_sink()` for standard output, but may also use
/// this helper to stream directly from producer channels when needed.
pub fn stream_grafeo_batches_to_writer<W: Write>(
    lbd_receiver: Receiver<Vec<lbd_ontology::Triple>>,
    ifcowl_receiver: Option<Receiver<Vec<lbd_ontology::Triple>>>,
    topology_receiver: Option<Receiver<Vec<lbd_ontology::Triple>>>,
    mut writer: W,
    lbd_graph_iri: &str,
    ifcowl_graph_iri: &str,
    topology_graph_iri: &str,
) -> anyhow::Result<()> {
    let (merged_sender, merged_receiver) =
        crossbeam::channel::unbounded::<(String, Vec<lbd_ontology::Triple>)>();

    let lbd_graph = lbd_graph_iri.to_string();
    let lbd_sender = merged_sender.clone();
    let lbd_forwarder = thread::spawn(move || {
        for batch in lbd_receiver {
            if lbd_sender.send((lbd_graph.clone(), batch)).is_err() {
                break;
            }
        }
    });

    let ifcowl_forwarder = ifcowl_receiver.map(|receiver| {
        let ifcowl_graph = ifcowl_graph_iri.to_string();
        let ifcowl_sender = merged_sender.clone();
        thread::spawn(move || {
            for batch in receiver {
                if ifcowl_sender.send((ifcowl_graph.clone(), batch)).is_err() {
                    break;
                }
            }
        })
    });

    let topology_forwarder = topology_receiver.map(|receiver| {
        let graph = topology_graph_iri.to_string();
        let sender = merged_sender.clone();
        thread::spawn(move || {
            for batch in receiver {
                if sender.send((graph.clone(), batch)).is_err() {
                    break;
                }
            }
        })
    });

    drop(merged_sender);

    for (graph, batch) in merged_receiver {
        let frame = DirectStreamFrame {
            version: DIRECT_STREAM_VERSION,
            graph,
            triples: batch
                .into_iter()
                .map(direct_stream_triple_from_lbd)
                .collect(),
        };
        bincode::serde::encode_into_std_write(&frame, &mut writer, bincode::config::standard())
            .context("failed to encode Grafeo direct stream frame")?;
        writer
            .flush()
            .context("failed to flush Grafeo direct stream writer")?;
    }

    lbd_forwarder
        .join()
        .map_err(|_| anyhow::anyhow!("LBD Grafeo stream forwarder panicked"))?;
    if let Some(handle) = ifcowl_forwarder {
        handle
            .join()
            .map_err(|_| anyhow::anyhow!("IfcOWL Grafeo stream forwarder panicked"))?;
    }
    if let Some(handle) = topology_forwarder {
        handle
            .join()
            .map_err(|_| anyhow::anyhow!("Topology Grafeo stream forwarder panicked"))?;
    }
    Ok(())
}

fn direct_stream_triple_from_lbd(triple: lbd_ontology::Triple) -> DirectStreamTriple {
    DirectStreamTriple {
        subject: direct_stream_term_from_iri_like(triple.subject),
        predicate: direct_stream_term_from_iri_like(triple.predicate),
        object: direct_stream_term_from_lbd_object(triple.object),
    }
}

fn direct_stream_term_from_iri_like(value: String) -> DirectStreamTerm {
    if let Some(id) = value.strip_prefix("_:") {
        DirectStreamTerm::BlankNode(id.to_string())
    } else {
        DirectStreamTerm::Iri(value)
    }
}

fn direct_stream_term_from_lbd_object(object: lbd_ontology::Object) -> DirectStreamTerm {
    match object {
        lbd_ontology::Object::Iri(value) => direct_stream_term_from_iri_like(value),
        lbd_ontology::Object::Literal(value) => DirectStreamTerm::Literal(value),
        lbd_ontology::Object::TypedLiteral { value, datatype } => {
            DirectStreamTerm::TypedLiteral { value, datatype }
        }
    }
}

#[cfg(test)]
mod tests {
    use lbd_pipeline::PipelineStage;

    use super::built_in_registry;

    #[test]
    fn built_in_registry_exposes_expected_stage_counts() {
        let registry = built_in_registry();
        assert_eq!(
            registry.manifests_for_stage(PipelineStage::Produce).len(),
            5
        );
        assert_eq!(
            registry.manifests_for_stage(PipelineStage::Serialize).len(),
            3
        );
        assert_eq!(registry.manifests_for_stage(PipelineStage::Export).len(), 3);
    }
}
