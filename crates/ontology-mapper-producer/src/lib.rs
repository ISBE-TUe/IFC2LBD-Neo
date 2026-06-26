//! Ontology Mapping Producer Plugin for IFC2LBD-Neo.
//!
//! Takes structured data input + an ontology file + an alignment file,
//! produces triples mapped to the target ontology.
//!
//! # Pipeline role
//!
//! - Stage: Produce
//! - Named graph slug: `ontology` (→ graph IRI `{base_uri}/ontology`)
//! - Reads: `StructuredDataInput`, `OntologyMappingConfig` from `PipelineContext`
//! - Emits: RDF triples via `TaggedBatch` channel
//! - Failure policy: Optional (adds optional data)

use crossbeam::channel::Sender;
use lbd_converter::ConvertOptions;
use lbd_pipeline::{
    BatchKind, FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage,
    PluginManifest, ProducerError, ProducerPlugin, TaggedBatch,
};
use structured_data::{OntologyMappingConfig, StructuredDataInput};

mod engine;
mod forward;

pub use engine::execute_ontology_mapping;

/// Plugin ID — must be unique across all registered modules.
pub const ONTOLOGY_MAPPER_ID: &str = "neo-ontology-mapper";

/// Graph URL slug — appended to `{base_uri}/` to form this module's named-graph IRI.
const GRAPH_SLUG: &str = "ontology";

/// The ontology mapper producer plugin.
pub struct OntologyMapperProducerPlugin;

impl PipelinePlugin for OntologyMapperProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: ONTOLOGY_MAPPER_ID,
            display_name: "Ontology Mapper",
            stage: PipelineStage::Produce,
            description: "Maps structured data to a target ontology using an alignment file.",
            inputs: vec!["structured-data", "ontology-file", "alignment-file"],
            outputs: vec!["ontology-triples"],
            requires: vec!["structured-data"],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
            named_graph_slug: Some(GRAPH_SLUG),
            needs_full_graph: false,
        }
    }
}

impl ProducerPlugin for OntologyMapperProducerPlugin {
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        let data = ctx
            .get::<StructuredDataInput>()
            .ok_or_else(|| ProducerError::Conversion("No structured data input".into()))?;

        let config = ctx
            .get::<OntologyMappingConfig>()
            .ok_or_else(|| ProducerError::Conversion("No ontology mapping config".into()))?;

        let options = ctx
            .get::<ConvertOptions>()
            .ok_or_else(|| ProducerError::Conversion("No ConvertOptions in context".into()))?;

        let graph_iri = BatchKind::new(format!(
            "{}{}",
            options.base_uri.trim_end_matches('/'),
            GRAPH_SLUG,
        ));

        let (raw_sender, raw_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);

        forward::forward_as_tagged(raw_receiver, graph_iri, sender.clone());

        // Execute ontology mapping for each input file
        for file in &data.files {
            let triples = engine::execute_ontology_mapping(
                &config.alignment_turtle,
                &config.ontology_turtle,
                &file.filename,
                &file.bytes,
            )
            .map_err(ProducerError::Conversion)?;

            raw_sender
                .send(triples)
                .map_err(|_| ProducerError::ChannelClosed)?;
        }

        Ok(())
    }
}
