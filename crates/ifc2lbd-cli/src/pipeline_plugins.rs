use std::io::Write;
use std::thread;

use anyhow::Context;
use crossbeam::channel::{Receiver, Sender};
use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_converter::{stream_beo, stream_bot, stream_omg_fog, stream_props_opm, ConvertOptions};
use lbd_ontology::Triple;
use lbd_pipeline::{
    BatchKind, ExportPlugin, FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin,
    PipelineStage, PluginManifest, PluginRegistry, ProducerError, ProducerPlugin, SerializerPlugin,
    TaggedBatch, BEO_PRODUCER_ID, BOT_PRODUCER_ID, FILE_EXPORT_ID,
    GRAFEO_EXPORT_ID, IFCOWL_PRODUCER_ID, NQUADS_SERIALIZER_ID,
    OMG_FOG_PRODUCER_ID, PROPS_OPM_PRODUCER_ID, STDOUT_EXPORT_ID,
    TURTLE_SERIALIZER_ID,
};

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
use serde::{Deserialize, Serialize};

pub fn built_in_registry() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry.register_producer(BotProducerPlugin).unwrap();
    registry.register_producer(BeoProducerPlugin).unwrap();
    registry.register_producer(PropsOpmProducerPlugin).unwrap();
    registry.register_producer(OmgFogProducerPlugin).unwrap();
    registry.register_producer(IfcowlProducerPlugin).unwrap();
    registry
        .register_serializer(TurtleSerializerPlugin)
        .unwrap();
    registry
        .register_serializer(NquadsSerializerPlugin)
        .unwrap();
    registry.register_export(FileExportPlugin).unwrap();
    registry.register_export(StdoutExportPlugin).unwrap();
    registry.register_export(GrafeoExportPlugin).unwrap();
    registry
}

struct BotProducerPlugin;
struct BeoProducerPlugin;
struct PropsOpmProducerPlugin;
struct OmgFogProducerPlugin;
struct IfcowlProducerPlugin;
struct TurtleSerializerPlugin;
struct NquadsSerializerPlugin;
struct FileExportPlugin;
struct StdoutExportPlugin;
struct GrafeoExportPlugin;

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
            conflicts_with: vec![NQUADS_SERIALIZER_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
        }
    }
}

impl SerializerPlugin for TurtleSerializerPlugin {
    fn serialize(
        &self,
        _ctx: &lbd_pipeline::PipelineContext,
        _receiver: crossbeam::channel::Receiver<lbd_pipeline::TaggedBatch>,
        _writer: &mut dyn std::io::Write,
    ) -> Result<lbd_pipeline::SerializeStats, lbd_pipeline::SerializerError> {
        Err(lbd_pipeline::SerializerError::Serialization(
            "serialize not yet via PipelineRunner".to_string(),
        ))
    }
}

impl PipelinePlugin for NquadsSerializerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: NQUADS_SERIALIZER_ID,
            display_name: "Built-in N-Quads serializer",
            stage: PipelineStage::Serialize,
            description: "Serializes graph streams into merged or chunked N-Quads output.",
            inputs: vec!["quads"],
            outputs: vec!["nquads-bytes", "nquads-chunks"],
            requires: vec![],
            conflicts_with: vec![TURTLE_SERIALIZER_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
            named_graph_slug: None,
        }
    }
}

impl SerializerPlugin for NquadsSerializerPlugin {
    fn serialize(
        &self,
        _ctx: &lbd_pipeline::PipelineContext,
        _receiver: crossbeam::channel::Receiver<lbd_pipeline::TaggedBatch>,
        _writer: &mut dyn std::io::Write,
    ) -> Result<lbd_pipeline::SerializeStats, lbd_pipeline::SerializerError> {
        Err(lbd_pipeline::SerializerError::Serialization(
            "serialize not yet via PipelineRunner".to_string(),
        ))
    }
}

impl PipelinePlugin for FileExportPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: FILE_EXPORT_ID,
            display_name: "Built-in file exporter",
            stage: PipelineStage::Export,
            description: "Writes serialized output streams to files and chunk manifests.",
            inputs: vec!["turtle-bytes", "nquads-bytes", "nquads-chunks"],
            outputs: vec!["filesystem"],
            requires: vec![],
            conflicts_with: vec![STDOUT_EXPORT_ID, GRAFEO_EXPORT_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: false,
            named_graph_slug: None,
        }
    }
}

impl ExportPlugin for FileExportPlugin {
    fn export_in_memory(
        &self,
        _ctx: &lbd_pipeline::PipelineContext,
        _files: Vec<lbd_pipeline::ExportedFile>,
    ) -> Result<Vec<lbd_pipeline::ExportFileSummary>, lbd_pipeline::ExportError> {
        Err(lbd_pipeline::ExportError::Export(
            "export_in_memory not yet via PipelineRunner".to_string(),
        ))
    }
}

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
            wasm_compatible: true,
            named_graph_slug: None,
        }
    }
}

impl ExportPlugin for StdoutExportPlugin {
    fn export_in_memory(
        &self,
        _ctx: &lbd_pipeline::PipelineContext,
        _files: Vec<lbd_pipeline::ExportedFile>,
    ) -> Result<Vec<lbd_pipeline::ExportFileSummary>, lbd_pipeline::ExportError> {
        Err(lbd_pipeline::ExportError::Export(
            "export_in_memory not yet via PipelineRunner".to_string(),
        ))
    }
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
        }
    }
}

impl ExportPlugin for GrafeoExportPlugin {
    fn export_in_memory(
        &self,
        _ctx: &lbd_pipeline::PipelineContext,
        _files: Vec<lbd_pipeline::ExportedFile>,
    ) -> Result<Vec<lbd_pipeline::ExportFileSummary>, lbd_pipeline::ExportError> {
        Err(lbd_pipeline::ExportError::Export(
            "export_in_memory not yet via PipelineRunner".to_string(),
        ))
    }
}

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
            2
        );
        assert_eq!(registry.manifests_for_stage(PipelineStage::Export).len(), 3);
    }
}
