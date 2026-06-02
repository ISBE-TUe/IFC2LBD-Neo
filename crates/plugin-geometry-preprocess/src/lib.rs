//! Geometry preprocess plugin.
//!
//! Uses ifc-lite's geometry engine (via `ifc-geometry`) to tessellate all
//! IFC elements and store the result as `Arc<TessellatedModel>` in the
//! PipelineContext. Runs once; all downstream geometry consumers read from it.

use std::sync::Arc;

use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_pipeline::{
    FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage,
    PluginManifest, PreprocessError, PreprocessPlugin,
};
use tessellated_model::{MetadataMode, TessellatedModel};

pub const GEOMETRY_PREPROCESS_ID: &str = "neo-geometry-preprocess";

pub struct GeometryPreprocessPlugin;

impl PipelinePlugin for GeometryPreprocessPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: GEOMETRY_PREPROCESS_ID,
            display_name: "Geometry preprocessor",
            stage: PipelineStage::Preprocess,
            description: "Tessellates IFC geometry using ifc-lite and stores TessellatedModel in context.",
            inputs: vec!["ifc-step", "ifc-model"],
            outputs: vec!["tessellated-model"],
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

impl PreprocessPlugin for GeometryPreprocessPlugin {
    fn preprocess(&self, ctx: &mut PipelineContext) -> Result<(), PreprocessError> {
        let model = ctx.get::<IfcModel>().ok_or_else(|| {
            PreprocessError::Preprocessing("GeometryPreprocessPlugin: missing IfcModel".to_string())
        })?;
        let step = ctx.get::<StepFile>().ok_or_else(|| {
            PreprocessError::Preprocessing("GeometryPreprocessPlugin: missing StepFile".to_string())
        })?;

        // Raw IFC content is required for ifc-lite's EntityDecoder.
        // Stored in context as Arc<IFCContent> by the CLI/WASM runner.
        let content = ctx.get::<IFCContent>().ok_or_else(|| {
            PreprocessError::Preprocessing(
                "GeometryPreprocessPlugin: missing IFCContent in context. \
                 The runner must insert Arc::new(IFCContent(raw_ifc_string)) before running."
                    .to_string(),
            )
        })?;

        let metadata_mode = ctx
            .get::<MetadataModeOption>()
            .map(|o| o.0)
            .unwrap_or_default();

        // Collect element IDs: physical elements + IFCSPACE from spatial nodes.
        // Matches oracle's classes.elements (IFCOPENINGELEMENT excluded).
        let mut element_ids: Vec<u64> = model
            .elements
            .keys()
            .copied()
            .filter(|id| {
                !step
                    .entities
                    .get(id)
                    .map(|e| e.entity_name == "IFCOPENINGELEMENT")
                    .unwrap_or(false)
            })
            .collect();

        // Include IFCSPACE from spatial nodes (oracle does this too)
        for id in model.spatial_nodes.keys().copied() {
            if let Some(e) = step.entities.get(&id) {
                if e.entity_name == "IFCSPACE" {
                    element_ids.push(id);
                }
            }
        }

        element_ids.sort_unstable();
        element_ids.dedup();

        // Tessellate using ifc-lite
        let meshes = ifc_geometry::stream_meshes(&content.0, &element_ids);

        let tessellated = Arc::new(TessellatedModel::new(meshes, metadata_mode));
        ctx.insert(tessellated);

        Ok(())
    }
}

/// Raw IFC text content, stored in PipelineContext by runners.
/// ifc-lite's EntityDecoder needs it for geometry resolution.
#[derive(Clone)]
pub struct IFCContent(pub String);

/// Optional metadata mode override stored in context by runners.
#[derive(Clone, Copy)]
pub struct MetadataModeOption(pub MetadataMode);
