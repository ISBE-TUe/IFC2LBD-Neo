use std::io::Write;

use crossbeam::channel::{Receiver, Sender};
use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_converter::{stream_step_and_model, ConvertOptions};
use lbd_ontology::Triple;
use lbd_pipeline::{
    BatchKind, ExportError, ExportFileSummary, ExportPlugin, ExportedFile, FailurePolicy,
    ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage, PluginManifest,
    PluginRegistry, ProducerError, ProducerPlugin, SerializeStats, SerializerError,
    SerializerPlugin, TaggedBatch, BBOX_ENRICHER_ID, FILE_EXPORT_ID, IFCOWL_PRODUCER_ID,
    IFC_TOPOLOGY_PRODUCER_ID, LBD_PRODUCER_ID, NQUADS_CHUNKED_SERIALIZER_ID, NQUADS_SERIALIZER_ID,
    TOPOLOGY_FULL_PRODUCER_ID, TURTLE_SERIALIZER_ID,
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
        NQUADS_SERIALIZER_ID => vec!["lbd_graph_iri".to_string(), "ifcowl_graph_iri".to_string()],
        NQUADS_CHUNKED_SERIALIZER_ID => vec![
            "chunking".to_string(),
            "chunk_size_lines".to_string(),
            "chunk_size_bytes".to_string(),
            "chunk_prefix".to_string(),
            "lbd_graph_iri".to_string(),
            "ifcowl_graph_iri".to_string(),
        ],
        TURTLE_SERIALIZER_ID => vec!["grouping".to_string()],
        FILE_EXPORT_ID => vec!["output_stem".to_string()],
        BBOX_ENRICHER_ID => vec!["inflation_threshold".to_string()],
        TOPOLOGY_FULL_PRODUCER_ID => vec![
            "geometry_tolerance".to_string(),
            "inflation_threshold".to_string(),
        ],
        _ => Vec::new(),
    }
}

pub(crate) fn browser_registry() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry.register_producer(LbdProducerPlugin).unwrap();
    registry.register_producer(IfcowlProducerPlugin).unwrap();
    registry
        .register_producer(IfcTopologyProducerPlugin)
        .unwrap();
    registry.register_producer(BboxEnricherPlugin).unwrap();
    registry
        .register_producer(TopologyFullProducerPlugin)
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
    registry
}

// ---------------------------------------------------------------------------
// Conversion helper
// ---------------------------------------------------------------------------

fn map_ser_err(e: lbd_serializer::SerializerError) -> SerializerError {
    match e {
        lbd_serializer::SerializerError::Io(io) => SerializerError::Io(io),
        lbd_serializer::SerializerError::Utf8(e) => SerializerError::Serialization(e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// LBD Producer
// ---------------------------------------------------------------------------

struct LbdProducerPlugin;

impl PipelinePlugin for LbdProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: LBD_PRODUCER_ID,
            display_name: "Built-in LBD producer",
            stage: PipelineStage::Produce,
            description: "Generates LBD triples from the typed IFC model.",
            inputs: vec!["ifc-model"],
            outputs: vec!["lbd-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
        }
    }
}

impl ProducerPlugin for LbdProducerPlugin {
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        let step = ctx.get::<StepFile>().ok_or_else(|| {
            ProducerError::Conversion("LbdProducerPlugin: missing StepFile in context".to_string())
        })?;
        let model = ctx.get::<IfcModel>().ok_or_else(|| {
            ProducerError::Conversion("LbdProducerPlugin: missing IfcModel in context".to_string())
        })?;
        let options = ctx.get::<ConvertOptions>().ok_or_else(|| {
            ProducerError::Conversion(
                "LbdProducerPlugin: missing ConvertOptions in context".to_string(),
            )
        })?;

        // Create a raw-triple sender that wraps batches as TaggedBatch(Lbd, ..)
        let (raw_sender, raw_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
        let tagged_sender = sender.clone();

        // Forward raw batches as tagged batches
        rayon::spawn(move || {
            for batch in raw_receiver {
                if tagged_sender
                    .send(TaggedBatch {
                        kind: BatchKind::Lbd,
                        triples: batch,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        stream_step_and_model(&step, &model, &options, &raw_sender, None)
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
        let model = ctx.get::<IfcModel>().ok_or_else(|| {
            ProducerError::Conversion(
                "IfcowlProducerPlugin: missing IfcModel in context".to_string(),
            )
        })?;
        let options = ctx.get::<ConvertOptions>().ok_or_else(|| {
            ProducerError::Conversion(
                "IfcowlProducerPlugin: missing ConvertOptions in context".to_string(),
            )
        })?;

        // Create raw-triple senders for LBD and IfcOWL
        let (lbd_sender, lbd_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
        let (ifcowl_sender, ifcowl_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
        let tagged_sender = sender.clone();

        // Forward LBD batches
        let lbd_tagged = tagged_sender.clone();
        rayon::spawn(move || {
            for batch in lbd_receiver {
                if lbd_tagged
                    .send(TaggedBatch {
                        kind: BatchKind::Lbd,
                        triples: batch,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        // Forward IfcOWL batches
        let ifcowl_tagged = sender.clone();
        rayon::spawn(move || {
            for batch in ifcowl_receiver {
                if ifcowl_tagged
                    .send(TaggedBatch {
                        kind: BatchKind::Ifcowl,
                        triples: batch,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        stream_step_and_model(&step, &model, &options, &lbd_sender, Some(&ifcowl_sender))
            .map_err(|_| ProducerError::ChannelClosed)
    }
}

// ---------------------------------------------------------------------------
// IfcTopology Producer
// ---------------------------------------------------------------------------

struct IfcTopologyProducerPlugin;

impl PipelinePlugin for IfcTopologyProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: IFC_TOPOLOGY_PRODUCER_ID,
            display_name: "IfcTopology",
            stage: PipelineStage::Produce,
            description:
                "Generates BOT topology triples from IFC spatial and element relationships.",
            inputs: vec!["ifc-model"],
            outputs: vec!["topology-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
        }
    }
}

impl ProducerPlugin for IfcTopologyProducerPlugin {
    fn produce(
        &self,
        _ctx: &PipelineContext,
        _sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        Err(ProducerError::Conversion(
            "IfcTopologyProducerPlugin::produce() not yet wired through PipelineRunner in WASM"
                .to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Bbox Enricher — adds bounding-box geometry triples (geo:asWKT, hasBoundingBox)
// to the LBD output. Standalone: does NOT implicitly enable topology.
// ---------------------------------------------------------------------------

struct BboxEnricherPlugin;

impl PipelinePlugin for BboxEnricherPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: BBOX_ENRICHER_ID,
            display_name: "Bbox",
            stage: PipelineStage::Produce,
            description: "Adds bounding-box geometry triples (GeoSPARQL WKT) to LBD output.",
            inputs: vec!["ifc-model", "step-file"],
            outputs: vec!["bbox-geometry"],
            requires: vec![LBD_PRODUCER_ID],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
        }
    }
}

impl ProducerPlugin for BboxEnricherPlugin {
    fn produce(
        &self,
        _ctx: &PipelineContext,
        _sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        Err(ProducerError::Conversion(
            "BboxEnricherPlugin::produce() not yet wired through PipelineRunner in WASM"
                .to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Topology Full — IfcTopology + bbox-based adjacency/intersection detection
// Produces enriched topology with bot:adjacentElement, bot:intersectingElement edges.
// ---------------------------------------------------------------------------

struct TopologyFullProducerPlugin;

impl PipelinePlugin for TopologyFullProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: TOPOLOGY_FULL_PRODUCER_ID,
            display_name: "TopologyFull",
            stage: PipelineStage::Produce,
            description: "Full topology with bbox-derived adjacency and intersection detection.",
            inputs: vec!["ifc-model", "step-file"],
            outputs: vec!["topology-triples", "adjacency-relations"],
            requires: vec![],
            conflicts_with: vec![IFC_TOPOLOGY_PRODUCER_ID],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
        }
    }
}

impl ProducerPlugin for TopologyFullProducerPlugin {
    fn produce(
        &self,
        _ctx: &PipelineContext,
        _sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        Err(ProducerError::Conversion(
            "TopologyFullProducerPlugin::produce() not yet wired through PipelineRunner in WASM"
                .to_string(),
        ))
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
            display_name: "Built-in Turtle serializer",
            stage: PipelineStage::Serialize,
            description: "Serializes triples into Turtle output.",
            inputs: vec!["triples"],
            outputs: vec!["turtle-bytes"],
            requires: vec![LBD_PRODUCER_ID],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
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
            display_name: "Built-in N-Quads serializer",
            stage: PipelineStage::Serialize,
            description: "Serializes graph streams into N-Quads output.",
            inputs: vec!["quads"],
            outputs: vec!["nquads-bytes"],
            requires: vec![LBD_PRODUCER_ID],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
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
            let graph_iri = match batch.kind {
                BatchKind::Lbd => "https://lbd.example.com/lbd",
                BatchKind::Ifcowl => "https://lbd.example.com/ifcowl",
                BatchKind::Topology => "https://lbd.example.com/topology",
            };
            write_nquads_batch(&mut counting, &batch.triples, graph_iri)?;
        }
        stats.bytes_written = counting.bytes;
        Ok(stats)
    }
}

/// Write N-Quads batch by sending triples through a temporary channel
/// so we can reuse the lbd_serializer function.
fn write_nquads_batch<W: Write>(
    writer: &mut W,
    triples: &[Triple],
    graph_iri: &str,
) -> Result<(), SerializerError> {
    let (tx, rx) = crossbeam::channel::bounded(1);
    tx.send(triples.to_vec())
        .map_err(|_| SerializerError::ChannelClosed)?;
    drop(tx);
    serialize_nquads_batches_to_writer(rx, writer, graph_iri).map_err(map_ser_err)
}

// ---------------------------------------------------------------------------
// N-Quads Chunked Serializer (produces chunked N-Quads output)
// ---------------------------------------------------------------------------

struct NquadsChunkedSerializerPlugin;

impl PipelinePlugin for NquadsChunkedSerializerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: NQUADS_CHUNKED_SERIALIZER_ID,
            display_name: "Built-in N-Quads chunked serializer",
            stage: PipelineStage::Serialize,
            description: "Serializes graph streams into chunked N-Quads output with configurable chunk boundaries.",
            inputs: vec!["quads"],
            outputs: vec!["nquads-bytes"],
            requires: vec![LBD_PRODUCER_ID],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
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
        // For now, same as regular N-Quads — chunking logic to be added
        let mut stats = SerializeStats::default();
        let mut counting = CountingWriterWrap::new(writer);
        for batch in receiver {
            stats.triples_written += batch.triples.len() as u64;
            let graph_iri = match batch.kind {
                BatchKind::Lbd => "https://lbd.example.com/lbd",
                BatchKind::Ifcowl => "https://lbd.example.com/ifcowl",
                BatchKind::Topology => "https://lbd.example.com/topology",
            };
            write_nquads_batch(&mut counting, &batch.triples, graph_iri)?;
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
            display_name: "Built-in file exporter",
            stage: PipelineStage::Export,
            description: "Exports browser-downloadable artifacts from serializer output.",
            inputs: vec!["turtle-bytes", "nquads-bytes"],
            outputs: vec!["browser-files"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
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

/// A writer wrapper that counts bytes written (used for stats).
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
