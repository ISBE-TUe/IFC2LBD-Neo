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

pub use engine::{build_mapping_tables, parse_rdf_mappings, MappingTables};

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

        // Build mapping tables from alignment + ontology files.
        let tables = engine::build_mapping_tables(&config.alignment_turtle, &config.ontology_turtle)
            .map_err(PostprocessError::Postprocessing)?;

        if tables.property_map.is_empty() && tables.class_map.is_empty() {
            // No mappings found — nothing to do.
            return Ok(());
        }

        let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

        // Apply mappings to all triples from all producers.
        let graph_iri = BatchKind::new(format!(
            "{}/{}",
            options.base_uri.trim_end_matches('/'),
            GRAPH_SLUG,
        ));

        let mut mapped_triples: Vec<Triple> = Vec::new();
        for batch in batches.iter() {
            for triple in &batch.triples {
                let mapped_predicate = tables
                    .property_map
                    .get(&triple.predicate)
                    .cloned()
                    .unwrap_or_else(|| triple.predicate.clone());

                // For rdf:type triples, also map the object (class).
                let mapped_object = if triple.predicate == rdf_type {
                    match &triple.object {
                        lbd_ontology::Object::Iri(iri) => {
                            tables
                                .class_map
                                .get(iri)
                                .map(|c| lbd_ontology::Object::Iri(c.clone()))
                                .unwrap_or_else(|| triple.object.clone())
                        }
                        _ => triple.object.clone(),
                    }
                } else {
                    triple.object.clone()
                };

                if mapped_predicate != triple.predicate || mapped_object != triple.object {
                    mapped_triples.push(Triple {
                        subject: triple.subject.clone(),
                        predicate: mapped_predicate,
                        object: mapped_object,
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
