//! RML Mapper Producer Plugin for IFC2LBD-Neo.
//!
//! Executes RML mappings to transform structured data (JSON, CSV, XML) into
//! RDF triples. The RML mapping engine is reused from the `rml-mapper-lib`
//! crate (adapted from `worker-rml-rust`).
//!
//! # Pipeline role
//!
//! - Stage: Produce
//! - Named graph slug: `rml` (→ graph IRI `{base_uri}/rml`)
//! - Reads: `StructuredDataInput` and `RmlMappingConfig` from `PipelineContext`
//! - Emits: RDF triples via `TaggedBatch` channel
//! - Failure policy: Required
//!
//! # Registration
//!
//! Register in both runners:
//!
//! ```rust,ignore
//! registry.register_producer(RmlMapperProducerPlugin).unwrap();
//! ```

use crossbeam::channel::Sender;
use lbd_converter::ConvertOptions;
use lbd_pipeline::{
    BatchKind, FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage,
    PluginManifest, ProducerError, ProducerPlugin, TaggedBatch,
};
use structured_data::{RmlMappingConfig, StructuredDataInput};

mod engine;
mod forward;

pub use engine::{execute_rml, execute_rml_streaming};

/// Plugin ID — must be unique across all registered modules.
pub const RML_MAPPER_ID: &str = "neo-rml-mapper";

/// Graph URL slug — appended to `{base_uri}/` to form this module's named-graph IRI.
const GRAPH_SLUG: &str = "rml";

/// The RML mapper producer plugin.
pub struct RmlMapperProducerPlugin;

impl PipelinePlugin for RmlMapperProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: RML_MAPPER_ID,
            display_name: "RML Mapper",
            stage: PipelineStage::Produce,
            description:
                "Transforms structured data (JSON/CSV/XML) into RDF triples using RML mappings.",
            inputs: vec!["structured-data", "rml-mapping"],
            outputs: vec!["rml-triples"],
            requires: vec!["structured-data"],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
            named_graph_slug: Some(GRAPH_SLUG),
            needs_full_graph: false,
        }
    }
}

impl ProducerPlugin for RmlMapperProducerPlugin {
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        let data = ctx
            .get::<StructuredDataInput>()
            .ok_or_else(|| ProducerError::Conversion("No structured data input".into()))?;

        let mapping_config = ctx
            .get::<RmlMappingConfig>()
            .ok_or_else(|| ProducerError::Conversion("No RML mapping config".into()))?;

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

        // Forward raw triples as TaggedBatch with our graph IRI.
        forward::forward_as_tagged(raw_receiver, graph_iri, sender.clone());

        let batch_size = ctx.resource_limits.batch_size;

        // Execute RML mapping for each input file, streaming triples directly
        // through the channel (no N-Triples serialization/re-parsing).
        for file in &data.files {
            engine::execute_rml_streaming(
                &mapping_config.mapping_turtle,
                &file.filename,
                &file.bytes,
                &raw_sender,
                batch_size,
            )
            .map_err(ProducerError::Conversion)?;
        }

        // raw_sender is dropped here, signaling the forwarder to finish
        Ok(())
    }
}
