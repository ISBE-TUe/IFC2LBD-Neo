//! Ontology Mapping Postprocess Plugin for IFC2LBD-Neo.
//!
//! Takes the triples produced by all producers (BOT, BEO, props, IfcOWL, etc.),
//! applies predicate mappings from an alignment file + ontology file, and
//! emits the mapped triples into a new named graph (`{base_uri}/ontology`).
//!
//! In addition to simple 1:1 IRI remapping, this module now supports **OWL
//! reasoning**: `owl:equivalentClass` axioms with blank-node (complex) right
//! sides are parsed into class expression trees and evaluated against the
//! triple set to infer new `rdf:type` triples.
//!
//! # Pipeline role
//!
//! - Stage: Postprocess
//! - Named graph slug: `ontology` (→ graph IRI `{base_uri}/ontology`)
//! - Reads: `OntologyMappingConfig` from `PipelineContext`
//! - Input: all accumulated `TaggedBatch` items from every producer
//! - Output: new `TaggedBatch` with predicate-mapped + inferred triples
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
use lbd_ontology::{Object, Triple};
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

/// The `rdf:type` predicate IRI.
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// The ontology mapper postprocess plugin.
///
/// Reads `OntologyMappingConfig` (alignment + ontology file contents) from the
/// pipeline context, builds a predicate mapping table, applies it to all
/// accumulated triples from every producer, and runs OWL reasoning to infer
/// additional `rdf:type` triples. The mapped + inferred triples are pushed as
/// a new `TaggedBatch` with the `ontology` named graph IRI.
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

        // Collect all triples from all batches for mapping + reasoning.
        let all_triples: Vec<Triple> = batches
            .iter()
            .flat_map(|b| b.triples.iter().cloned())
            .collect();

        // Apply simple mapping + OWL reasoning.
        let mapped_triples = apply_ontology_mapping(
            &config.alignment_turtle,
            &config.ontology_turtle,
            &all_triples,
        );

        if mapped_triples.is_empty() {
            return Ok(());
        }

        let graph_iri = BatchKind::new(format!(
            "{}/{}",
            options.base_uri.trim_end_matches('/'),
            GRAPH_SLUG,
        ));

        batches.push(TaggedBatch {
            kind: graph_iri,
            triples: mapped_triples,
        });

        Ok(())
    }
}

/// Apply ontology mapping + OWL reasoning to a flat set of triples.
///
/// This is the shared entry point used by both:
/// - The postprocess plugin (CLI + streaming WASM via `spawn_postprocessors`)
/// - The WASM in-memory path (`export_browser_files`)
///
/// # Algorithm
///
/// 1. **Build simple mapping tables** from alignment + ontology files
///    (existing behavior — `owl:equivalentProperty`, `owl:equivalentClass`
///    named↔named, `rdfs:subPropertyOf`, `rdfs:subClassOf`, `align:entity`).
/// 2. **Build reasoning rules** from `owl:equivalentClass` with blank-node
///    (complex) right sides. Parse failure → empty rules + warning (simple
///    mapping still runs).
/// 3. **Apply simple IRI remapping** (changed-only filter — only triples
///    that actually changed are included in the output).
/// 4. **Run OWL reasoning** on the union of original + mapped triples.
///    The reasoner needs to see remapped types (e.g. `ifc:IfcBuilding →
///    saref4bldg:Building`) to evaluate conditions like
///    `intersectionOf(saref4bldg:Building, …)`.
/// 5. **Add inferred `rdf:type` triples** (deduped against existing types).
///
/// Returns the mapped + inferred triples. Only changed/new triples are
/// included — unchanged triples are NOT in the output.
pub fn apply_ontology_mapping(
    alignment_turtle: &str,
    ontology_turtle: &str,
    all_triples: &[Triple],
) -> Vec<Triple> {
    // 1. Build simple mapping tables (existing behaviour, unchanged).
    let tables = match engine::build_mapping_tables(alignment_turtle, ontology_turtle) {
        Ok(t) => t,
        Err(e) => {
            // Mapping table parse failure → no mapping at all.
            // Don't even attempt reasoning.
            return Vec::new();
        }
    };

    // Early exit: no mappings AND no reasoning possible.
    // We still need to check for reasoning rules even if simple maps are
    // empty, because reasoning rules don't require simple maps.
    let has_simple_maps = !tables.property_map.is_empty() || !tables.class_map.is_empty();

    // 2. Build reasoning rules (new — may fail without affecting step 1).
    let (rules, _rule_warnings) = owl_reasoner::parse_rules(alignment_turtle, ontology_turtle)
        .unwrap_or_else(|e| {
            // Parse failure must not regress simple mapping.
            // Log warning, continue with no rules.
            (Vec::new(), vec![format!("OWL reasoning skipped: {e}")])
        });

    if !has_simple_maps && rules.is_empty() {
        return Vec::new();
    }

    // 3. Apply simple IRI remapping (changed-only filter).
    let mut mapped_triples: Vec<Triple> = Vec::new();
    if has_simple_maps {
        for triple in all_triples {
            let mapped_predicate = tables
                .property_map
                .get(&triple.predicate)
                .cloned()
                .unwrap_or_else(|| triple.predicate.clone());

            let mapped_object = if triple.predicate == RDF_TYPE {
                match &triple.object {
                    Object::Iri(iri) => tables
                        .class_map
                        .get(iri)
                        .map(|c| Object::Iri(c.clone()))
                        .unwrap_or_else(|| triple.object.clone()),
                    _ => triple.object.clone(),
                }
            } else {
                triple.object.clone()
            };

            // Changed-only filter: only include triples that actually changed.
            if mapped_predicate != triple.predicate || mapped_object != triple.object {
                mapped_triples.push(Triple {
                    subject: triple.subject.clone(),
                    predicate: mapped_predicate,
                    object: mapped_object,
                });
            }
        }
    }

    // 4. Run OWL reasoning on union of original + mapped triples.
    //    The reasoner must see remapped types to evaluate conditions.
    if !rules.is_empty() {
        let mut all_for_reasoning: Vec<Triple> =
            Vec::with_capacity(all_triples.len() + mapped_triples.len());
        all_for_reasoning.extend_from_slice(all_triples);
        all_for_reasoning.extend(mapped_triples.iter().cloned());

        let inferred = owl_reasoner::infer_types(&rules, &all_for_reasoning);

        // 5. Add inferred rdf:type triples (dedup against existing types).
        let existing_types: std::collections::HashSet<(String, String)> = all_for_reasoning
            .iter()
            .filter(|t| t.predicate == RDF_TYPE)
            .filter_map(|t| match &t.object {
                Object::Iri(iri) => Some((t.subject.clone(), iri.clone())),
                _ => None,
            })
            .collect();

        for inf in inferred {
            if let Object::Iri(class_iri) = &inf.object {
                if !existing_types.contains(&(inf.subject.clone(), class_iri.clone())) {
                    mapped_triples.push(Triple {
                        subject: inf.subject,
                        predicate: RDF_TYPE.to_string(),
                        object: inf.object,
                    });
                }
            }
        }
    }

    mapped_triples
}
