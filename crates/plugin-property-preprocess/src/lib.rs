use std::sync::Arc;

use ifc_model::IfcModel;
use lbd_converter::{build_bsdd_match_cache, dedup_model_property_sets, BsddMatchCache};
use lbd_pipeline::{
    PipelineContext, PipelineLogBundle, PipelineLogEntry, PipelinePlugin, PipelineStage,
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
            display_name: "Cleanup preprocess",
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
        let deduped = dedup_model_property_sets(&model);
        let after_props: usize = deduped.property_sets.values().map(|ps| ps.properties.len()).sum();
        ctx.replace(Arc::new(deduped));

        let deduped_count = before_props.saturating_sub(after_props);
        let mut logs = ctx.get::<PipelineLogBundle>().map(|x| (*x).clone()).unwrap_or_default();
        logs.entries.push(PipelineLogEntry {
            module_id: CLEANUP_PREPROCESS_ID.to_string(),
            metric: "properties_before_dedup".to_string(),
            value: json!(before_props),
        });
        logs.entries.push(PipelineLogEntry {
            module_id: CLEANUP_PREPROCESS_ID.to_string(),
            metric: "properties_after_dedup".to_string(),
            value: json!(after_props),
        });
        logs.entries.push(PipelineLogEntry {
            module_id: CLEANUP_PREPROCESS_ID.to_string(),
            metric: "properties_deduped".to_string(),
            value: json!(deduped_count),
        });
        ctx.replace(Arc::new(logs));
        info!("cleanup preprocess complete: dedup applied");
        Ok(())
    }
}

impl PipelinePlugin for BsddMatchPreprocessPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: BSDD_MATCH_PREPROCESS_ID,
            display_name: "bSDD match preprocess",
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

        let cache: BsddMatchCache = build_bsdd_match_cache(&model)
            .map_err(|e| PreprocessError::Preprocessing(format!("BsddMatchPreprocessPlugin: failed building bSDD match cache: {e}")))?;

        if ctx.get::<BsddMatchCache>().is_some() {
            ctx.replace(Arc::new(cache));
        } else {
            ctx.insert(Arc::new(cache));
        }

        let mut logs = ctx.get::<PipelineLogBundle>().map(|x| (*x).clone()).unwrap_or_default();
        logs.entries.push(PipelineLogEntry {
            module_id: BSDD_MATCH_PREPROCESS_ID.to_string(),
            metric: "bsdd_match_cache_built".to_string(),
            value: json!(true),
        });
        ctx.replace(Arc::new(logs));
        info!("bSDD match preprocess complete: cache ready");
        Ok(())
    }
}
