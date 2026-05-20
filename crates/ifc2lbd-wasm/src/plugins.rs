use std::io::Write;

use crossbeam::channel::{Receiver, Sender};
use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_converter::{stream_beo, stream_bot, stream_omg_fog, stream_props_opm, ConvertOptions};
use lbd_ontology::Triple;
use lbd_pipeline::{
    BatchKind, BEO_PRODUCER_ID, BOT_PRODUCER_ID, ExportError, ExportFileSummary, ExportPlugin,
    ExportedFile, FailurePolicy, FILE_EXPORT_ID, IFCOWL_PRODUCER_ID,
    NQUADS_CHUNKED_SERIALIZER_ID, NQUADS_SERIALIZER_ID, OMG_FOG_PRODUCER_ID, ParallelismMode,
    PipelineContext, PipelinePlugin, PipelineStage, PluginManifest, PluginRegistry, ProducerError,
    ProducerPlugin, PROPS_OPM_PRODUCER_ID, SerializeStats, SerializerError, SerializerPlugin,
    TaggedBatch, TURTLE_SERIALIZER_ID,
};
use lbd_serializer::{
    serialize_nquads_batches_to_writer, serialize_turtle_batch_raw_to_writer,
    write_turtle_prefixes_for_stream,
};
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
        FILE_EXPORT_ID => vec!["output_stem".to_string()],
        _ => Vec::new(),
    }
}

pub(crate) fn browser_registry() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    // Modular LBD producers
    registry.register_producer(BotProducerPlugin).unwrap();
    registry.register_producer(BeoProducerPlugin).unwrap();
    registry.register_producer(PropsOpmProducerPlugin).unwrap();
    registry.register_producer(OmgFogProducerPlugin).unwrap();
    // Other producers
    registry.register_producer(IfcowlProducerPlugin).unwrap();
    // Serializers
    registry
        .register_serializer(TurtleSerializerPlugin)
        .unwrap();
    registry
        .register_serializer(NquadsSerializerPlugin)
        .unwrap();
    registry
        .register_serializer(NquadsChunkedSerializerPlugin)
        .unwrap();
    // Export
    registry.register_export(FileExportPlugin).unwrap();
    registry
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn map_ser_err(e: lbd_serializer::SerializerError) -> SerializerError {
    match e {
        lbd_serializer::SerializerError::Io(io) => SerializerError::Io(io),
        lbd_serializer::SerializerError::Utf8(e) => SerializerError::Serialization(e.to_string()),
    }
}

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
        let graph_iri = BatchKind::new(format!(
            "{}bot",
            options.base_uri.trim_end_matches('/')
        ));
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
        let graph_iri = BatchKind::new(format!(
            "{}beo",
            options.base_uri.trim_end_matches('/')
        ));
        forward_as_tagged(raw_receiver, graph_iri, sender.clone());

        stream_beo(&model, &options, &raw_sender)
            .map(|_| ())
            .map_err(|_| ProducerError::ChannelClosed)
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
        let graph_iri = BatchKind::new(format!(
            "{}props",
            options.base_uri.trim_end_matches('/')
        ));
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
        let graph_iri = BatchKind::new(format!(
            "{}omg",
            options.base_uri.trim_end_matches('/')
        ));
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

        // The IfcOWL producer also emits owl:sameAs links (to the alignment graph).
        // Those are emitted as part of the LBD core-entities pass when emit_ifcowl_links=true.
        // Here we only produce the IfcOWL entity triples.
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
// Turtle Serializer
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
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
        }
    }
}

impl SerializerPlugin for TurtleSerializerPlugin {
    fn serialize(
        &self,
        _ctx: &PipelineContext,
        receiver: Receiver<TaggedBatch>,
        writer: &mut dyn Write,
    ) -> Result<SerializeStats, SerializerError> {
        let mut stats = SerializeStats::default();
        let mut counting = CountingWriterWrap::new(writer);
        write_turtle_prefixes_for_stream(&mut counting, None).map_err(map_ser_err)?;
        for batch in receiver {
            stats.triples_written += batch.triples.len() as u64;
            serialize_turtle_batch_raw_to_writer(&batch.triples, &mut counting)
                .map_err(map_ser_err)?;
        }
        stats.bytes_written = counting.bytes;
        Ok(stats)
    }
}

// ---------------------------------------------------------------------------
// N-Quads Serializer
// ---------------------------------------------------------------------------

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
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
            named_graph_slug: None,
        }
    }
}

impl SerializerPlugin for NquadsSerializerPlugin {
    fn serialize(
        &self,
        _ctx: &PipelineContext,
        receiver: Receiver<TaggedBatch>,
        writer: &mut dyn Write,
    ) -> Result<SerializeStats, SerializerError> {
        let mut stats = SerializeStats::default();
        let mut counting = CountingWriterWrap::new(writer);
        for batch in receiver {
            stats.triples_written += batch.triples.len() as u64;
            // The named-graph IRI comes directly from the batch's kind.
            write_nquads_batch(&mut counting, &batch.triples, batch.kind.iri())?;
        }
        stats.bytes_written = counting.bytes;
        Ok(stats)
    }
}

fn write_nquads_batch<W: Write>(
    writer: &mut W,
    triples: &[lbd_ontology::Triple],
    graph_iri: &str,
) -> Result<(), SerializerError> {
    let (tx, rx) = crossbeam::channel::bounded(1);
    tx.send(triples.to_vec())
        .map_err(|_| SerializerError::ChannelClosed)?;
    drop(tx);
    serialize_nquads_batches_to_writer(rx, writer, graph_iri).map_err(map_ser_err)
}

// ---------------------------------------------------------------------------
// N-Quads Chunked Serializer
// ---------------------------------------------------------------------------

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
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
            named_graph_slug: None,
        }
    }
}

impl SerializerPlugin for NquadsChunkedSerializerPlugin {
    fn serialize(
        &self,
        _ctx: &PipelineContext,
        receiver: Receiver<TaggedBatch>,
        writer: &mut dyn Write,
    ) -> Result<SerializeStats, SerializerError> {
        let mut stats = SerializeStats::default();
        let mut counting = CountingWriterWrap::new(writer);
        for batch in receiver {
            stats.triples_written += batch.triples.len() as u64;
            write_nquads_batch(&mut counting, &batch.triples, batch.kind.iri())?;
        }
        stats.bytes_written = counting.bytes;
        Ok(stats)
    }
}

// ---------------------------------------------------------------------------
// File Export
// ---------------------------------------------------------------------------

struct FileExportPlugin;

impl PipelinePlugin for FileExportPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: FILE_EXPORT_ID,
            display_name: "File exporter",
            stage: PipelineStage::Export,
            description: "Exports browser-downloadable artifacts from serializer output.",
            inputs: vec!["turtle-bytes", "nquads-bytes"],
            outputs: vec!["browser-files"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
        }
    }
}

impl ExportPlugin for FileExportPlugin {
    fn export_in_memory(
        &self,
        _ctx: &PipelineContext,
        files: Vec<ExportedFile>,
    ) -> Result<Vec<ExportFileSummary>, ExportError> {
        Ok(files
            .into_iter()
            .map(|f| ExportFileSummary {
                filename: f.filename,
                mime_type: f.mime_type,
                role: f.role,
                bytes: f.bytes.len() as u64,
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) struct CountingWriterWrap<'a> {
    inner: &'a mut dyn Write,
    pub bytes: u64,
}

impl<'a> CountingWriterWrap<'a> {
    pub fn new(inner: &'a mut dyn Write) -> Self {
        Self { inner, bytes: 0 }
    }
}

impl Write for CountingWriterWrap<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub(crate) fn js_err<E: ToString>(error: E) -> JsValue {
    JsValue::from_str(&error.to_string())
}
