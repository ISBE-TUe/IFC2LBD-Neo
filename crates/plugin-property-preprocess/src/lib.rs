use std::sync::Arc;

use ifc_model::IfcModel;
use lbd_converter::{build_bsdd_match_cache, dedup_model_property_sets, BsddMatchCache, ConvertOptions};
use lbd_pipeline::{
    PipelineContext, PipelineLogBundle, PipelinePlugin, PipelineStage,
    PluginManifest, PreprocessError, PreprocessPlugin, FailurePolicy, ParallelismMode,
    CLEANUP_PREPROCESS_ID, BSDD_MATCH_PREPROCESS_ID,
};
use serde_json::json;
use tracing::info;

pub struct CleanupPreprocessPlugin;
pub struct BsddMatchPreprocessPlugin;

impl PipelinePlugin for CleanupPreprocessPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: CLEANUP_PREPROCESS_ID,
            display_name: "ASCII Repair",
            stage: PipelineStage::Preprocess,
            description: "Deduplicates IFC property occurrences and normalizes property payload quality.",
            inputs: vec!["ifc-model"],
            outputs: vec!["ifc-model"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl PreprocessPlugin for CleanupPreprocessPlugin {
    fn preprocess(&self, ctx: &mut PipelineContext) -> Result<(), PreprocessError> {
        let model = ctx
            .get::<IfcModel>()
            .ok_or_else(|| PreprocessError::Preprocessing("CleanupPreprocessPlugin: missing IfcModel in context".to_string()))?;

        let before_props: usize = model.property_sets.values().map(|ps| ps.properties.len()).sum();
        let property_names_with_non_ascii = model
            .property_single_values
            .values()
            .filter(|p| !p.name.is_ascii())
            .count()
            + model
                .property_enumerated_values
                .values()
                .filter(|p| !p.name.is_ascii())
                .count();
        let segmented_property_names = model
            .property_single_values
            .values()
            .filter(|p| has_segment_marker(p.name.as_str()))
            .count()
            + model
                .property_enumerated_values
                .values()
                .filter(|p| has_segment_marker(p.name.as_str()))
                .count();
        let property_sets_with_non_ascii = model
            .property_sets
            .values()
            .filter(|ps| ps.name.as_deref().is_some_and(|name| !name.is_ascii()))
            .count();
        let deduped = dedup_model_property_sets(&model);
        let after_props: usize = deduped.property_sets.values().map(|ps| ps.properties.len()).sum();
        let (normalized_model, normalization_stats) = normalize_model_text(&deduped);
        ctx.replace(Arc::new(normalized_model));

        let deduped_count = before_props.saturating_sub(after_props);
        let mut logs = ctx.get::<PipelineLogBundle>().map(|x| (*x).clone()).unwrap_or_default();
        logs.write_module(CLEANUP_PREPROCESS_ID, json!({
            "properties_before_dedup": before_props,
            "properties_after_dedup": after_props,
            "properties_deduped": deduped_count,
            "property_names_with_non_ascii": property_names_with_non_ascii,
            "property_sets_with_non_ascii": property_sets_with_non_ascii,
            "segmented_property_names": segmented_property_names,
            "property_names_normalized": normalization_stats.property_names_normalized,
            "property_sets_normalized": normalization_stats.property_sets_normalized,
            "non_ascii_names_remaining": normalization_stats.non_ascii_names_remaining,
        }));
        ctx.replace(Arc::new(logs));
        info!("cleanup preprocess complete: dedup applied");
        Ok(())
    }
}

impl PipelinePlugin for BsddMatchPreprocessPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: BSDD_MATCH_PREPROCESS_ID,
            display_name: "bSDD Matcher",
            stage: PipelineStage::Preprocess,
            description: "Precomputes bSDD fuzzy/exact match cache shared by producers.",
            inputs: vec!["ifc-model"],
            outputs: vec!["bsdd-match-cache"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl PreprocessPlugin for BsddMatchPreprocessPlugin {
    fn preprocess(&self, ctx: &mut PipelineContext) -> Result<(), PreprocessError> {
        let model = ctx
            .get::<IfcModel>()
            .ok_or_else(|| PreprocessError::Preprocessing("BsddMatchPreprocessPlugin: missing IfcModel in context".to_string()))?;

        // Env var takes precedence (override), then ConvertOptions.bsdd_profile, then "base".
        let options_profile = ctx.get::<ConvertOptions>()
            .and_then(|o| o.bsdd_profile.clone());
        let profile_name = std::env::var("IFC2LBD_BSDD_PROFILE").ok().or(options_profile);
        let cache: BsddMatchCache = build_bsdd_match_cache(&model, profile_name.as_deref())
            .map_err(|e| PreprocessError::Preprocessing(format!("BsddMatchPreprocessPlugin: failed building bSDD match cache: {e}")))?;

        let match_stats = cache.stats();

        if ctx.get::<BsddMatchCache>().is_some() {
            ctx.replace(Arc::new(cache));
        } else {
            ctx.insert(Arc::new(cache));
        }

        let mut logs = ctx.get::<PipelineLogBundle>().map(|x| (*x).clone()).unwrap_or_default();
        logs.write_module(BSDD_MATCH_PREPROCESS_ID, match_stats);
        ctx.replace(Arc::new(logs));
        info!("bSDD match preprocess complete: cache ready");
        Ok(())
    }
}

fn has_segment_marker(value: &str) -> bool {
    value.contains(':') || value.contains('/') || value.contains('\\') || value.contains('|')
}

#[derive(Default)]
struct TextNormalizationStats {
    property_names_normalized: usize,
    property_sets_normalized: usize,
    non_ascii_names_remaining: usize,
}

// Transliteration no longer mutates the model — it belongs only in the matcher's
// internal normalization key (bsdd.rs::normalize). The model keeps original labels
// like "Höhe" so consumers get faithful data; only the lookup key sees "hoehe".
fn normalize_model_text(model: &IfcModel) -> (IfcModel, TextNormalizationStats) {
    let updated = model.clone();
    let mut stats = TextNormalizationStats::default();

    for property in updated.property_single_values.values() {
        if !property.name.is_ascii() {
            stats.non_ascii_names_remaining += 1;
        }
    }
    for property in updated.property_enumerated_values.values() {
        if !property.name.is_ascii() {
            stats.non_ascii_names_remaining += 1;
        }
    }
    for property_set in updated.property_sets.values() {
        if property_set.name.as_deref().is_some_and(|n| !n.is_ascii()) {
            stats.non_ascii_names_remaining += 1;
        }
    }

    (updated, stats)
}
