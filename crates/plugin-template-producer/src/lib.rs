//! Template: Producer plugin for IFC2LBD-Neo (with optional sidecar file support).
//!
//! # What producer plugins do
//!
//! A producer plugin streams RDF triples into a bounded channel. Each batch of
//! triples is wrapped in a [`TaggedBatch`] that carries the named-graph IRI the
//! triples belong to. The serializer stage reads from this channel and writes
//! the triples to the output format (Turtle, N-Quads, etc.).
//!
//! Producers can additionally emit **sidecar artefacts** — non-triple binary
//! files (e.g. geometry `.frag` files, GeoJSON, images) — via
//! `ctx.sidecar_tx`. The active export plugin's session receives these files
//! via `ExportSession::accept_derived_file()` after all producers finish.
//!
//! # Named graph
//!
//! Each producer owns one named graph IRI: `{base_uri}/{slug}`. Set
//! `named_graph_slug` in the manifest to your slug (e.g. `"geometry"`), and
//! derive the full IRI from `ConvertOptions::base_uri` at produce-time.
//!
//! # Parallelism
//!
//! `spawn_producers()` runs multiple producers concurrently (bounded by
//! `(thread_count - 1) / 2`). Your `produce()` implementation must be
//! `Send + Sync`. Avoid holding non-`Send` state across `.send()` calls.
//!
//! # Registration
//!
//! Register in the built-in registry:
//!
//! ```rust,ignore
//! registry.register_producer(TemplateProducerPlugin).unwrap();
//! ```
//!
//! # Adapting this template
//!
//! 1. Rename `TemplateProducerPlugin` and `TEMPLATE_PRODUCER_ID`.
//! 2. Set `named_graph_slug` to your graph's URL slug.
//! 3. Replace the hardcoded triple emission with real IFC-model-driven logic.
//! 4. If your plugin produces sidecar files (e.g. geometry), emit them via
//!    `ctx.sidecar_tx` (see the sidecar section in `produce()` below).
//! 5. Update `wasm_compatible: false` if the plugin requires native-only code
//!    (e.g. OpenCascade geometry kernel).

use crossbeam::channel::Sender;
use lbd_ontology::{Object, Triple};
use lbd_pipeline::{
    BatchKind, DerivedFile, FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin,
    PipelineStage, PluginManifest, ProducerError, ProducerPlugin, TaggedBatch,
};

/// Plugin ID — must be unique across all registered modules.
pub const TEMPLATE_PRODUCER_ID: &str = "template-producer-plugin";

/// Graph URL slug — appended to `{base_uri}/` to form this module's named-graph IRI.
///
/// Choose a meaningful slug that identifies the ontology or data set your
/// producer emits (e.g. `"geometry"`, `"props"`, `"bot"`).
const GRAPH_SLUG: &str = "template";

// ---------------------------------------------------------------------------
// Plugin struct
// ---------------------------------------------------------------------------

/// A template producer plugin.
///
/// Replace this with your real implementation. The struct may hold
/// configuration (read from `PipelineContext` at produce-time) or be
/// stateless (configuration is read from `ctx` only inside `produce()`).
pub struct TemplateProducerPlugin;

impl PipelinePlugin for TemplateProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: TEMPLATE_PRODUCER_ID,
            display_name: "Template producer",
            stage: PipelineStage::Produce,
            description: "Example producer plugin — replace with your implementation.",
            inputs: vec!["ifc-model"],
            outputs: vec!["template-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
            named_graph_slug: Some(GRAPH_SLUG),
            needs_full_graph: false,
        }
    }
}

impl ProducerPlugin for TemplateProducerPlugin {
    /// Produce RDF triples and optionally sidecar artefacts.
    ///
    /// # Emitting triples
    ///
    /// Build `TaggedBatch` values and send them through `sender`. The channel
    /// is bounded — if it is full, `send()` blocks until the serializer drains
    /// a batch. This is the backpressure mechanism; never use an unbounded channel.
    ///
    /// ```rust,ignore
    /// // Real IFC-driven example (after adding ifc-model dependency):
    /// let model = ctx.get::<IfcModel>()
    ///     .ok_or_else(|| ProducerError::Conversion("missing IfcModel".into()))?;
    /// let options = ctx.get::<ConvertOptions>()
    ///     .ok_or_else(|| ProducerError::Conversion("missing ConvertOptions".into()))?;
    ///
    /// let graph_iri = BatchKind::new(format!(
    ///     "{}{}",
    ///     options.base_uri.trim_end_matches('/'), GRAPH_SLUG,
    /// ));
    ///
    /// for chunk in model.entities().chunks(options.stream_batch_size) {
    ///     let triples = chunk.iter().map(|e| triple_for(e, &options)).collect();
    ///     sender.send(TaggedBatch { kind: graph_iri.clone(), triples })
    ///         .map_err(|_| ProducerError::ChannelClosed)?;
    /// }
    /// ```
    ///
    /// # Emitting sidecar files
    ///
    /// Sidecar artefacts (geometry files, lookup tables, etc.) are sent through
    /// `ctx.sidecar_tx`. The orchestrator collects them after all producers
    /// finish and passes them to the active export plugin.
    ///
    /// ```rust,ignore
    /// if let Some(tx) = &ctx.sidecar_tx {
    ///     let frag_bytes = generate_frag_geometry(&model);
    ///     // Ignore send errors: the orchestrator may have dropped the receiver
    ///     // if no export plugin is active or the pipeline is shutting down.
    ///     let _ = tx.send(DerivedFile {
    ///         filename: "model.frag".to_string(),
    ///         mime_type: "application/octet-stream",
    ///         bytes: frag_bytes,
    ///     });
    /// }
    /// ```
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        // Derive the named-graph IRI from context.
        //
        // In a real plugin you'd read ConvertOptions from context:
        //   let options = ctx.get::<ConvertOptions>()...;
        //   let base = options.base_uri.trim_end_matches('/');
        //
        // Here we use a placeholder base.
        let graph_iri = BatchKind::new(format!("https://example.org/{}", GRAPH_SLUG));

        // --- Emit a minimal example triple batch ---
        let triples = vec![Triple {
            subject: "https://example.org/building".to_string(),
            predicate: "http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string(),
            object: Object::Iri("https://w3id.org/bot#Building".to_string()),
        }];
        sender
            .send(TaggedBatch { kind: graph_iri, triples })
            .map_err(|_| ProducerError::ChannelClosed)?;

        // --- Emit an optional sidecar artefact ---
        //
        // This shows the sidecar pattern. Replace with real binary generation.
        if let Some(tx) = &ctx.sidecar_tx {
            let content = b"template sidecar file contents".to_vec();
            // Ignore errors: receiver may have been dropped if no export
            // plugin handles sidecars in the current run.
            let _ = tx.send(DerivedFile {
                filename: "template.txt".to_string(),
                mime_type: "text/plain",
                bytes: content,
            });
        }

        Ok(())
    }
}
