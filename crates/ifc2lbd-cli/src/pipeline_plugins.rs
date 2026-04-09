use std::io::Write;
use std::thread;

use anyhow::Context;
use crossbeam::channel::Receiver;
use lbd_pipeline::{
    ExportPlugin, FailurePolicy, ParallelismMode, PipelinePlugin, PipelineStage, PluginManifest,
    PluginRegistry, ProducerPlugin, SerializerPlugin,
};
use serde::{Deserialize, Serialize};

pub fn built_in_registry() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry.register_producer(LbdProducerPlugin).unwrap();
    registry.register_producer(IfcowlProducerPlugin).unwrap();
    registry
        .register_producer(TopologyLiteProducerPlugin)
        .unwrap();
    registry
        .register_producer(TopologyFullProducerPlugin)
        .unwrap();
    registry.register_producer(BboxEnricherPlugin).unwrap();
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

pub const TOPOLOGY_LITE_PRODUCER_ID: &str = "neo-topology-lite-producer";
pub const TOPOLOGY_FULL_PRODUCER_ID: &str = "neo-topology-full-producer";
pub const LBD_PRODUCER_ID: &str = "neo-lbd-producer";
pub const IFCOWL_PRODUCER_ID: &str = "neo-ifcowl-producer";
pub const BBOX_ENRICHER_ID: &str = "neo-bbox-enricher";
pub const TURTLE_SERIALIZER_ID: &str = "neo-turtle-serializer";
pub const NQUADS_SERIALIZER_ID: &str = "neo-nquads-serializer";
pub const FILE_EXPORT_ID: &str = "neo-file-export";
pub const STDOUT_EXPORT_ID: &str = "neo-stdout-export";
pub const GRAFEO_EXPORT_ID: &str = "neo-grafeo-export";

struct LbdProducerPlugin;
struct IfcowlProducerPlugin;
struct TopologyLiteProducerPlugin;
struct TopologyFullProducerPlugin;
struct BboxEnricherPlugin;
struct TurtleSerializerPlugin;
struct NquadsSerializerPlugin;
struct FileExportPlugin;
struct StdoutExportPlugin;
struct GrafeoExportPlugin;

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

impl ProducerPlugin for LbdProducerPlugin {}

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

impl ProducerPlugin for IfcowlProducerPlugin {}

impl PipelinePlugin for TopologyLiteProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: TOPOLOGY_LITE_PRODUCER_ID,
            display_name: "Built-in topology producer (light)",
            stage: PipelineStage::Produce,
            description: "Generates BOT topology triples from IFC relationship evidence.",
            inputs: vec!["ifc-model"],
            outputs: vec!["topology-triples"],
            requires: vec![],
            conflicts_with: vec![TOPOLOGY_FULL_PRODUCER_ID],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
        }
    }
}

impl ProducerPlugin for TopologyLiteProducerPlugin {}

impl PipelinePlugin for TopologyFullProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: TOPOLOGY_FULL_PRODUCER_ID,
            display_name: "Built-in topology producer (full OCC)",
            stage: PipelineStage::Produce,
            description: "Generates BOT topology triples using staged geometric refinement with OCC exact checks.",
            inputs: vec!["ifc-model", "step-file", "geometry-relations"],
            outputs: vec!["topology-triples"],
            requires: vec![],
            conflicts_with: vec![TOPOLOGY_LITE_PRODUCER_ID],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: false,
        }
    }
}

impl ProducerPlugin for TopologyFullProducerPlugin {}

impl PipelinePlugin for BboxEnricherPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: BBOX_ENRICHER_ID,
            display_name: "Neo bbox enricher",
            stage: PipelineStage::Produce,
            description: "Adds bbox geometry enrichment data for LBD output.",
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

impl ProducerPlugin for BboxEnricherPlugin {}

impl PipelinePlugin for TurtleSerializerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: TURTLE_SERIALIZER_ID,
            display_name: "Built-in Turtle serializer",
            stage: PipelineStage::Serialize,
            description: "Serializes triple streams into Turtle output.",
            inputs: vec!["triples"],
            outputs: vec!["turtle-bytes"],
            requires: vec![LBD_PRODUCER_ID],
            conflicts_with: vec![NQUADS_SERIALIZER_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
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
            description: "Serializes graph streams into merged or chunked N-Quads output.",
            inputs: vec!["quads"],
            outputs: vec!["nquads-bytes", "nquads-chunks"],
            requires: vec![LBD_PRODUCER_ID],
            conflicts_with: vec![TURTLE_SERIALIZER_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
        }
    }
}

impl SerializerPlugin for NquadsSerializerPlugin {}

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
        }
    }
}

impl ExportPlugin for FileExportPlugin {}

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
        }
    }
}

impl ExportPlugin for StdoutExportPlugin {}

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
        }
    }
}

impl ExportPlugin for GrafeoExportPlugin {}

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
