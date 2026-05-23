//! Template: Postprocess plugin for IFC2LBD-Neo.
//!
//! # What postprocess plugins do
//!
//! A postprocess plugin runs **after all producers finish** but **before
//! serialization**. It receives the complete set of accumulated [`TaggedBatch`]
//! items and may add, remove, or rewrite triples in place.
//!
//! Typical use-cases:
//! - **SHACL validation** — scan the triple set for constraint violations,
//!   emit `sh:ValidationReport` triples into a new batch, or error-out early.
//! - **OWL inference** — infer subclass memberships or transitive properties
//!   from the accumulated triple set.
//! - **Cross-producer linking** — insert triples that relate nodes from
//!   different named graphs (e.g. linking a geometry artefact IRI to the BOT
//!   element IRI that produced it).
//!
//! # `needs_full_graph`
//!
//! When `needs_full_graph: true` in the manifest the pipeline orchestrator
//! buffers all `TaggedBatch` items from every producer before calling
//! `postprocess()`. Set this when your logic requires the complete triple set
//! (e.g. SHACL validation, OWL inference). When `false`, the orchestrator may
//! call `postprocess()` incrementally (streaming-friendly).
//!
//! # Registration
//!
//! ```rust,ignore
//! registry.register_postprocess(TemplatePostprocessPlugin).unwrap();
//! ```
//!
//! The orchestrator calls `lbd_pipeline::spawn_postprocessors(...)` after all
//! producers have finished and before serialization starts.
//!
//! # Adapting this template
//!
//! 1. Rename `TemplatePostprocessPlugin` and `TEMPLATE_POSTPROCESS_ID`.
//! 2. Set `needs_full_graph` based on whether you need all triples at once.
//! 3. Implement `postprocess()`: inspect `batches`, add new triples, or
//!    filter triples that fail validation.

use lbd_ontology::{Object, Triple};
use lbd_pipeline::{
    BatchKind, FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage,
    PluginManifest, PostprocessError, PostprocessPlugin, TaggedBatch,
};

/// Plugin ID — must be unique across all registered modules.
pub const TEMPLATE_POSTPROCESS_ID: &str = "template-postprocess-plugin";

// ---------------------------------------------------------------------------
// Plugin struct
// ---------------------------------------------------------------------------

/// A template postprocess plugin.
///
/// This example inspects all batches and appends a simple provenance triple
/// to a new batch in the `validation` named graph.
pub struct TemplatePostprocessPlugin;

impl PipelinePlugin for TemplatePostprocessPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: TEMPLATE_POSTPROCESS_ID,
            display_name: "Template postprocessor",
            stage: PipelineStage::Postprocess,
            description: "Example postprocess plugin — replace with your implementation.",
            inputs: vec!["triples"],
            outputs: vec!["triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
            // Set to `true` when you need the full graph before running
            // (e.g. SHACL validation, OWL inference). `false` allows streaming.
            needs_full_graph: true,
        }
    }
}

impl PostprocessPlugin for TemplatePostprocessPlugin {
    /// Inspect and optionally modify the accumulated triple batches.
    ///
    /// # Pattern: counting triples across graphs
    ///
    /// ```rust,ignore
    /// let total: usize = batches.iter().map(|b| b.triples.len()).sum();
    /// tracing::info!("total triples produced: {}", total);
    /// ```
    ///
    /// # Pattern: adding triples to an existing batch
    ///
    /// ```rust,ignore
    /// for batch in batches.iter_mut() {
    ///     if batch.kind.iri().ends_with("/bot") {
    ///         batch.triples.push(Triple {
    ///             subject: "https://example.org/meta".to_string(),
    ///             predicate: "http://purl.org/dc/terms/source".to_string(),
    ///             object: Object::Iri(batch.kind.iri().to_string()),
    ///         });
    ///     }
    /// }
    /// ```
    ///
    /// # Pattern: inserting a new named-graph batch
    ///
    /// ```rust,ignore
    /// let report_batch = TaggedBatch {
    ///     kind: BatchKind::new("https://example.org/validation"),
    ///     triples: vec![ /* validation result triples */ ],
    /// };
    /// batches.push(report_batch);
    /// ```
    ///
    /// # Pattern: returning a validation error
    ///
    /// ```rust,ignore
    /// return Err(PostprocessError::Postprocessing(
    ///     "SHACL constraint violated: ...".to_string(),
    /// ));
    /// ```
    fn postprocess(
        &self,
        _ctx: &PipelineContext,
        batches: &mut Vec<TaggedBatch>,
    ) -> Result<(), PostprocessError> {
        // Count total triples for a provenance annotation.
        let total_triples: usize = batches.iter().map(|b| b.triples.len()).sum();

        // Insert a provenance triple into a new "meta" named-graph batch.
        if total_triples > 0 {
            let provenance_triple = Triple {
                subject: "https://example.org/conversion".to_string(),
                predicate: "https://example.org/meta#tripleCount".to_string(),
                object: Object::TypedLiteral {
                    value: total_triples.to_string(),
                    datatype: "http://www.w3.org/2001/XMLSchema#integer".to_string(),
                },
            };
            batches.push(TaggedBatch {
                kind: BatchKind::new("https://example.org/meta"),
                triples: vec![provenance_triple],
            });
        }

        Ok(())
    }
}
