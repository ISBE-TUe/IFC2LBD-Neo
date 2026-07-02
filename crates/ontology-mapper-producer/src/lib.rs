//! Ontology Mapping Postprocess Plugin for IFC2LBD-Neo.
//!
//! Takes the triples produced by all producers (BOT, BEO, props, IfcOWL, etc.),
//! applies predicate mappings from an alignment file + ontology file, and
//! emits the mapped triples into a new named graph (`{base_uri}/ontology`).
//!
//! # Pipeline role
//!
//! - Stage: Postprocess
//! - Named graph slug: `ontology` (→ graph IRI `{base_uri}/ontology`)
//! - Reads: `OntologyMappingConfig` from `PipelineContext`
//! - Input: all accumulated `TaggedBatch` items from every producer
//! - Output: new `TaggedBatch` with predicate-mapped triples in the ontology graph
//! - Failure policy: Optional (adds optional data)
//! - `needs_full_graph: true` — must see all producer triples before mapping
//!
//! # Registration
//!
//! Register in both runners:
//!
//! ```rust,ignore
//! registry.register_postprocess(OntologyMapperProducerPlugin).unwrap();
//! ```

use lbd_converter::ConvertOptions;
use lbd_ontology::Triple;
use lbd_pipeline::{
    BatchKind, FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage,
    PluginManifest, PostprocessError, PostprocessPlugin, TaggedBatch,
};
use structured_data::OntologyMappingConfig;

mod engine;

pub use engine::parse_rdf_mappings;

/// Plugin ID — must be unique across all registered modules.
pub const ONTOLOGY_MAPPER_ID: &str = "neo-ontology-mapper";

/// Graph URL slug — appended to `{base_uri}/` to form this module's named-graph IRI.
const GRAPH_SLUG: &str = "ontology";

/// The ontology mapper postprocess plugin.
///
/// Reads `OntologyMappingConfig` (alignment + ontology file contents) from the
/// pipeline context, builds a predicate mapping table, and applies it to all
/// accumulated triples from every producer. The mapped triples are pushed as a
/// new `TaggedBatch` with the `ontology` named graph IRI.
pub struct OntologyMapperProducerPlugin;

impl PipelinePlugin for OntologyMapperProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: ONTOLOGY_MAPPER_ID,
            display_name: "Ontology Mapper",
            stage: PipelineStage::Postprocess,
            description: "Maps converter triples to a target ontology using an alignment file.",
            inputs: vec!["triples", "ontology-file", "alignment-file"],
            outputs: vec!["ontology-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: Some(GRAPH_SLUG),
            needs_full_graph: true,
        }
    }
}

impl PostprocessPlugin for OntologyMapperProducerPlugin {
    fn postprocess(
        &self,
        ctx: &PipelineContext,
        batches: &mut Vec<TaggedBatch>,
    ) -> Result<(), PostprocessError> {
        let config = ctx.get::<OntologyMappingConfig>().ok_or_else(|| {
            PostprocessError::Postprocessing("No ontology mapping config in context".to_string())
        })?;

        let options = ctx.get::<ConvertOptions>().ok_or_else(|| {
            PostprocessError::Postprocessing("No ConvertOptions in context".to_string())
        })?;

        // Build the predicate mapping table from alignment + ontology files.
        let alignment_maps = engine::parse_rdf_mappings(&config.alignment_turtle)
            .map_err(PostprocessError::Postprocessing)?;
        let ontology_maps = engine::parse_rdf_mappings(&config.ontology_turtle)
            .map_err(PostprocessError::Postprocessing)?;

        let mut predicate_map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (src, tgt) in alignment_maps {
            predicate_map.insert(src, tgt);
        }
        for (src, tgt) in ontology_maps {
            predicate_map.entry(src).or_insert(tgt);
        }

        if predicate_map.is_empty() {
            // No mappings found — nothing to do.
            return Ok(());
        }

        // Apply predicate mappings to all triples from all producers.
        let graph_iri = BatchKind::new(format!(
            "{}/{}",
            options.base_uri.trim_end_matches('/'),
            GRAPH_SLUG,
        ));

        let mut mapped_triples: Vec<Triple> = Vec::new();
        for batch in batches.iter() {
            for triple in &batch.triples {
                if let Some(mapped_predicate) = predicate_map.get(&triple.predicate) {
                    mapped_triples.push(Triple {
                        subject: triple.subject.clone(),
                        predicate: mapped_predicate.clone(),
                        object: triple.object.clone(),
                    });
                }
            }
        }

        if !mapped_triples.is_empty() {
            batches.push(TaggedBatch {
                kind: graph_iri,
                triples: mapped_triples,
            });
        }

        Ok(())
    }
}
