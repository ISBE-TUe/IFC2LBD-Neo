//! Template: Preprocess plugin for IFC2LBD-Neo.
//!
//! # What preprocess plugins do
//!
//! A preprocess plugin runs **after** the IFC file is parsed and the model is
//! built, but **before** any triple producers execute. It receives the
//! [`PipelineContext`] (which holds `Arc<IfcModel>`, `Arc<StepFile>`,
//! `Arc<ConvertOptions>`, etc.) and may modify it in-place.
//!
//! Typical use-cases:
//! - Compute missing quantity sets (e.g. calculate volume when `IfcQuantityArea`
//!   is absent).
//! - Validate the model and bail out early if invariants are violated.
//! - Augment context with precomputed lookup tables that producers will use.
//!
//! # Registration
//!
//! Register your plugin with `PluginRegistry::register_preprocess()` in the
//! built-in registry (CLI: `crates/ifc2lbd-cli/src/pipeline_plugins.rs`,
//! WASM: `crates/ifc2lbd-wasm/src/plugins.rs`):
//!
//! ```rust,ignore
//! registry.register_preprocess(MyPreprocessPlugin).unwrap();
//! ```
//!
//! # How the orchestrator dispatches preprocess plugins
//!
//! The CLI `main.rs` / WASM `runner.rs` calls
//! `lbd_pipeline::spawn_preprocessors(&active_ids, &registry, &mut ctx)` before
//! running any producers. Each active preprocess plugin's `preprocess()` method
//! is called sequentially in the order modules appear in `active_ids`.
//!
//! # Adapting this template
//!
//! 1. Rename `TemplatePreprocessPlugin` (and the module ID constant).
//! 2. Add `ifc-model` to `Cargo.toml` if you need to inspect the IFC model.
//! 3. Implement `preprocess()`: read data from `ctx`, build a new value, then
//!    call `ctx.replace(Arc::new(new_value))` to update it.
//! 4. Register the plugin in the CLI and/or WASM registry.
//! 5. Optionally add `--module my-preprocess-plugin` to the CLI help text.

use lbd_pipeline::{
    FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage, PluginManifest,
    PreprocessError, PreprocessPlugin,
};

/// Plugin ID string registered in the module registry.
///
/// Must be unique across all registered modules. Use kebab-case prefixed
/// with your organisation or project (e.g. `"acme-quantity-enricher"`).
pub const TEMPLATE_PREPROCESS_ID: &str = "template-preprocess-plugin";

// ---------------------------------------------------------------------------
// Plugin struct
// ---------------------------------------------------------------------------

/// A template preprocess plugin.
///
/// Replace this struct name and add any configuration fields you need
/// (e.g. loaded from a JSON config file or from CLI flags stored in context).
pub struct TemplatePreprocessPlugin;

impl PipelinePlugin for TemplatePreprocessPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: TEMPLATE_PREPROCESS_ID,
            display_name: "Template preprocessor",
            stage: PipelineStage::Preprocess,
            description: "Example preprocess plugin — replace with your implementation.",
            inputs: vec!["ifc-model"],
            outputs: vec!["ifc-model"],  // same slot: updated in place via ctx.replace()
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,  // preprocess plugins never own a named graph
            needs_full_graph: false,
        }
    }
}

impl PreprocessPlugin for TemplatePreprocessPlugin {
    /// Perform preprocessing on the pipeline context.
    ///
    /// # Pattern: reading and replacing typed context data
    ///
    /// ```rust,ignore
    /// // Read the model (add `ifc-model` dependency first):
    /// let model = ctx
    ///     .get::<ifc_model::IfcModel>()
    ///     .ok_or_else(|| PreprocessError::Preprocessing("missing IfcModel".into()))?;
    ///
    /// // Build a modified version:
    /// let mut new_model = (*model).clone();
    /// new_model.add_quantity_sets(...);
    ///
    /// // Write it back so downstream producers see the updated model:
    /// ctx.replace(Arc::new(new_model));
    /// ```
    ///
    /// # Inserting auxiliary data
    ///
    /// Your plugin can also insert brand-new typed values that producers read later:
    ///
    /// ```rust,ignore
    /// let lookup = Arc::new(my_precomputed_lookup_table);
    /// ctx.insert(lookup);
    /// ```
    fn preprocess(&self, _ctx: &mut PipelineContext) -> Result<(), PreprocessError> {
        // TODO: implement your preprocessing logic here.
        //
        // This template is a no-op. Real implementations would:
        // 1. Retrieve typed data from `_ctx` via `_ctx.get::<T>()`.
        // 2. Transform or validate it.
        // 3. Put updated data back via `_ctx.replace(Arc::new(updated))`.
        Ok(())
    }
}
