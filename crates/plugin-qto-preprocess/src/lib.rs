//! QTO preprocess plugin.
//!
//! Detects missing IFC quantity sets on building elements, computes them from
//! STEP geometry (three tiers: BBox, ExtrudedAreaSolid, parry3d TriMesh), and
//! injects synthetic PhysicalQuantity + ElementQuantity records into the model
//! before any producer runs.

mod audit;
mod bbox;
mod inject;
mod mesh_volume;
mod qto_names;
mod rep_parser;
mod step_geom;

use std::sync::Arc;

use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_pipeline::{
    FailurePolicy, ParallelismMode, PipelineContext, PipelineLogBundle, PipelinePlugin,
    PipelineStage, PluginManifest, PreprocessError, PreprocessPlugin, QTO_PREPROCESS_ID,
};
use serde_json::json;
use tracing::info;

use audit::MissingQuantityReport;
use inject::{inject, ComputedValues};
use qto_names::QuantityKind;
use step_geom::SolidKind;

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Configuration for the QTO preprocess plugin.
///
/// Insert an `Arc<QtoOptions>` into the pipeline context before running to
/// enable this plugin. If absent from context the plugin skips immediately.
/// The env var `IFC2LBD_QTO_ENABLED=1` can also activate it with defaults.
#[derive(Debug, Clone)]
pub struct QtoOptions {
    /// Run Tier 3 mesh volume (parry3d TriMesh). Slower on complex geometry.
    pub compute_mesh_volume: bool,
}

impl Default for QtoOptions {
    fn default() -> Self {
        Self { compute_mesh_volume: true }
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct QtoPreprocessPlugin;

impl PipelinePlugin for QtoPreprocessPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: QTO_PREPROCESS_ID,
            display_name: "QTO Rebuild",
            stage: PipelineStage::Preprocess,
            description: "Detects missing IFC quantity sets and computes them from STEP geometry.",
            inputs: vec!["ifc-model", "ifc-step"],
            outputs: vec!["ifc-model"],
            requires: vec!["neo-cleanup-preprocess"],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Optional,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl PreprocessPlugin for QtoPreprocessPlugin {
    fn preprocess(&self, ctx: &mut PipelineContext) -> Result<(), PreprocessError> {
        // Activation check: explicit options in context or env-var fallback.
        let options = match ctx.get::<QtoOptions>() {
            Some(o) => (*o).clone(),
            None => {
                if std::env::var("IFC2LBD_QTO_ENABLED").as_deref() != Ok("1") {
                    return Ok(());
                }
                QtoOptions::default()
            }
        };

        let model = ctx
            .get::<IfcModel>()
            .ok_or_else(|| missing("IfcModel"))?;
        let step = ctx
            .get::<StepFile>()
            .ok_or_else(|| missing("StepFile"))?;

        // --- Audit ----------------------------------------------------------
        let reports = audit::audit(&model);
        let elements_scanned = model.elements.len() + model.spatial_nodes.len();
        let elements_missing_qto = reports.len();
        let elements_with_all_qto = elements_scanned.saturating_sub(elements_missing_qto);

        // --- Compute --------------------------------------------------------
        // Compute geometry for each element independently — parallel-safe since
        // StepFile and IfcModel are read-only through shared references.
        #[cfg(not(target_arch = "wasm32"))]
        let raw_results: Vec<(usize, ComputeOutput)> = {
            use rayon::prelude::*;
            reports
                .par_iter()
                .enumerate()
                .map(|(idx, report)| (idx, compute_for_element(&step, &model, report, &options)))
                .collect()
        };
        #[cfg(target_arch = "wasm32")]
        let raw_results: Vec<(usize, ComputeOutput)> = reports
            .iter()
            .enumerate()
            .map(|(idx, report)| (idx, compute_for_element(&step, &model, report, &options)))
            .collect();

        let mut computed_pairs: Vec<(usize, ComputedValues)> = Vec::new();
        let mut tier_bbox: u32 = 0;
        let mut tier_rep: u32 = 0;
        let mut tier_mesh: u32 = 0;
        let mut skipped_no_geometry: u32 = 0;

        for (idx, cv) in raw_results {
            if cv.is_none_all() {
                skipped_no_geometry += 1;
                continue;
            }
            match cv.tier {
                ComputeTier::Bbox => tier_bbox += 1,
                ComputeTier::Rep => tier_rep += 1,
                ComputeTier::Mesh => tier_mesh += 1,
            }
            computed_pairs.push((idx, cv.values));
        }

        // --- Inject ---------------------------------------------------------
        let (augmented, quantities_computed_total) =
            inject(&model, &step, &reports, &computed_pairs);

        let sets_created = augmented.element_quantities.len()
            - model.element_quantities.len();
        let sets_extended = computed_pairs.len().saturating_sub(sets_created);

        ctx.replace(Arc::new(augmented));

        // --- Log ------------------------------------------------------------
        let mut logs = ctx
            .get::<PipelineLogBundle>()
            .map(|x| (*x).clone())
            .unwrap_or_default();
        logs.write_module(
            QTO_PREPROCESS_ID,
            json!({
                "elements_scanned": elements_scanned,
                "elements_with_all_qto": elements_with_all_qto,
                "elements_missing_qto": elements_missing_qto,
                "qto_sets_found_existing": sets_extended,
                "qto_sets_created_new": sets_created,
                "quantities_computed_total": quantities_computed_total,
                "tier_used": {
                    "bbox_only": tier_bbox,
                    "rep_parser": tier_rep,
                    "mesh_volume": tier_mesh,
                },
                "elements_skipped_no_geometry": skipped_no_geometry,
            }),
        );
        ctx.replace(Arc::new(logs));

        info!(
            "qto preprocess: {quantities_computed_total} quantities computed, \
             {sets_created} sets created, {sets_extended} sets extended"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Per-element computation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComputeTier {
    Bbox,
    Rep,
    Mesh,
}

struct ComputeOutput {
    values: ComputedValues,
    tier: ComputeTier,
}

impl ComputeOutput {
    fn is_none_all(&self) -> bool {
        let v = &self.values;
        v.length.is_none()
            && v.height.is_none()
            && v.width.is_none()
            && v.depth.is_none()
            && v.gross_volume.is_none()
            && v.net_volume.is_none()
            && v.gross_area.is_none()
            && v.net_area.is_none()
            && v.area.is_none()
            && v.gross_footprint_area.is_none()
            && v.net_footprint_area.is_none()
            && v.gross_side_area.is_none()
            && v.net_side_area.is_none()
            && v.gross_floor_area.is_none()
            && v.net_floor_area.is_none()
            && v.cross_section_area.is_none()
            && v.outer_surface_area.is_none()
            && v.gross_perimeter.is_none()
            && v.perimeter.is_none()
    }
}

fn compute_for_element(
    step: &StepFile,
    model: &IfcModel,
    report: &MissingQuantityReport,
    options: &QtoOptions,
) -> ComputeOutput {
    let element_id = report.element_id;
    let mut cv = ComputedValues::default();
    let mut tier = ComputeTier::Bbox;

    // Borrow inline fields from ElementNode if available.
    let (inline_height, inline_width) = model
        .elements
        .get(&element_id)
        .map(|e| (e.overall_height, e.overall_width))
        .unwrap_or((None, None));

    let solid = step_geom::best_solid(step, element_id);

    match &solid {
        // ------------------------------------------------------------------
        // Tier 2: ExtrudedAreaSolid
        // ------------------------------------------------------------------
        SolidKind::ExtrudedAreaSolid { profile_id, depth } => {
            if let Some(rep) = rep_parser::from_extruded_solid(step, *profile_id, *depth) {
                tier = ComputeTier::Rep;

                // Map extrusion depth → correct dimension based on element type.
                let entity_type = report.entity_type.to_uppercase();
                let needs = |k: QuantityKind| report.missing.contains(&k);

                match entity_type.as_str() {
                    "IFCBEAM" | "IFCCOLUMN" | "IFCMEMBER" | "IFCPILE" | "IFCFOOTING" => {
                        if needs(QuantityKind::Length) {
                            cv.length = Some(rep.extrusion_depth);
                        }
                        if needs(QuantityKind::CrossSectionArea) {
                            cv.cross_section_area = Some(rep.profile_area);
                        }
                        if needs(QuantityKind::OuterSurfaceArea) {
                            cv.outer_surface_area = Some(rep.outer_surface_area());
                        }
                    }
                    "IFCWALL" | "IFCWALLSTANDARDCASE" => {
                        // Extrusion depth is typically wall height for walls
                        // extruded vertically; use overall_height if available.
                        let h = inline_height.unwrap_or(rep.extrusion_depth);
                        if needs(QuantityKind::Height) {
                            cv.height = Some(h);
                        }
                        if needs(QuantityKind::GrossFootprintArea) {
                            // Profile area IS the footprint for a vertically-extruded wall.
                            cv.gross_footprint_area = Some(rep.profile_area);
                        }
                        if needs(QuantityKind::NetFootprintArea) {
                            cv.net_footprint_area = Some(rep.profile_area);
                        }
                        if needs(QuantityKind::GrossSideArea) {
                            cv.gross_side_area = Some(rep.outer_surface_area());
                        }
                    }
                    "IFCSLAB" | "IFCROOF" | "IFCPLATE" | "IFCCOVERING" => {
                        if needs(QuantityKind::Depth) {
                            cv.depth = Some(rep.extrusion_depth);
                        }
                        if needs(QuantityKind::GrossArea) {
                            cv.gross_area = Some(rep.profile_area);
                        }
                        if needs(QuantityKind::Perimeter) {
                            cv.perimeter = Some(rep.profile_perimeter);
                        }
                    }
                    _ => {}
                }

                // Volume is universally derivable from any ExtrudedAreaSolid.
                // GrossVolume uses the same formula (we can't subtract openings here).
                if needs(QuantityKind::NetVolume) {
                    cv.net_volume = Some(rep.net_volume());
                }
                if needs(QuantityKind::GrossVolume) {
                    cv.gross_volume = Some(rep.net_volume());
                }
                if needs(QuantityKind::CrossSectionArea) && cv.cross_section_area.is_none() {
                    cv.cross_section_area = Some(rep.profile_area);
                }
            }
        }

        // ------------------------------------------------------------------
        // Tier 3: mesh-based
        // ------------------------------------------------------------------
        SolidKind::FacetedBrep { shell_id } if options.compute_mesh_volume => {
            if let Some(mesh) = mesh_volume::from_faceted_brep(step, *shell_id) {
                tier = ComputeTier::Mesh;
                let needs = |k: QuantityKind| report.missing.contains(&k);
                if needs(QuantityKind::NetVolume) {
                    cv.net_volume = Some(mesh.net_volume);
                }
                if needs(QuantityKind::OuterSurfaceArea) {
                    cv.outer_surface_area = Some(mesh.net_surface_area);
                }
            }
        }
        SolidKind::TriangulatedFaceSet { faceset_id } if options.compute_mesh_volume => {
            if let Some(mesh) = mesh_volume::from_triangulated_faceset(step, *faceset_id) {
                tier = ComputeTier::Mesh;
                let needs = |k: QuantityKind| report.missing.contains(&k);
                if needs(QuantityKind::NetVolume) {
                    cv.net_volume = Some(mesh.net_volume);
                }
                if needs(QuantityKind::OuterSurfaceArea) {
                    cv.outer_surface_area = Some(mesh.net_surface_area);
                }
            }
        }

        // ------------------------------------------------------------------
        // Tier 1 BBox fallback (also runs after Tier 2/3 to fill any gaps)
        // ------------------------------------------------------------------
        SolidKind::BoundingBox { x_dim, y_dim, z_dim } => {
            apply_bbox_dims(&mut cv, report, *x_dim, *y_dim, *z_dim, inline_height, inline_width);
        }
        _ => {}
    }

    // For ExtrudedAreaSolid / mesh results, always fill remaining gaps with BBox.
    if !matches!(solid, SolidKind::BoundingBox { .. }) {
        if let Some(bb) = bbox::compute(step, element_id) {
            if tier == ComputeTier::Bbox || tier == ComputeTier::Rep || tier == ComputeTier::Mesh {
                apply_bbox_dims(
                    &mut cv,
                    report,
                    bb.x_dim,
                    bb.y_dim,
                    bb.z_dim,
                    inline_height,
                    inline_width,
                );
            }
            // Tag as bbox only if nothing better was computed.
            if tier == ComputeTier::Bbox {
                tier = ComputeTier::Bbox;
            }
        }
    }

    // Inline height/width from ElementNode override bbox for doors/windows.
    let entity_upper = report.entity_type.to_uppercase();
    if matches!(entity_upper.as_str(), "IFCDOOR" | "IFCWINDOW") {
        let needs = |k: QuantityKind| report.missing.contains(&k);
        if let Some(h) = inline_height {
            if needs(QuantityKind::Height) { cv.height = Some(h); }
        }
        if let Some(w) = inline_width {
            if needs(QuantityKind::Width) { cv.width = Some(w); }
        }
        if let (Some(h), Some(w)) = (cv.height, cv.width) {
            if needs(QuantityKind::Area) { cv.area = Some(h * w); }
            if needs(QuantityKind::Perimeter) { cv.perimeter = Some(2.0 * (h + w)); }
        }
    }

    ComputeOutput { values: cv, tier }
}

fn apply_bbox_dims(
    cv: &mut ComputedValues,
    report: &MissingQuantityReport,
    x_dim: f64,
    y_dim: f64,
    z_dim: f64,
    inline_height: Option<f64>,
    inline_width: Option<f64>,
) {
    let needs = |k: QuantityKind| report.missing.contains(&k);
    let h = inline_height.unwrap_or(z_dim);
    let w = inline_width.unwrap_or(x_dim.min(y_dim));
    let l = x_dim.max(y_dim);

    if needs(QuantityKind::GrossVolume) && cv.gross_volume.is_none() {
        cv.gross_volume = Some(x_dim * y_dim * z_dim);
    }
    if needs(QuantityKind::GrossFootprintArea) && cv.gross_footprint_area.is_none() {
        cv.gross_footprint_area = Some(x_dim * y_dim);
    }
    if needs(QuantityKind::Height) && cv.height.is_none() {
        cv.height = Some(h);
    }
    if needs(QuantityKind::Width) && cv.width.is_none() {
        cv.width = Some(w);
    }
    if needs(QuantityKind::Length) && cv.length.is_none() {
        cv.length = Some(l);
    }
    if needs(QuantityKind::Depth) && cv.depth.is_none() {
        cv.depth = Some(z_dim.min(x_dim).min(y_dim));
    }
    if needs(QuantityKind::GrossArea) && cv.gross_area.is_none() {
        cv.gross_area = Some(x_dim * y_dim);
    }
    if needs(QuantityKind::GrossFloorArea) && cv.gross_floor_area.is_none() {
        cv.gross_floor_area = Some(x_dim * y_dim);
    }
    if needs(QuantityKind::NetFloorArea) && cv.net_floor_area.is_none() {
        cv.net_floor_area = Some(x_dim * y_dim);
    }
    if needs(QuantityKind::GrossSideArea) && cv.gross_side_area.is_none() {
        cv.gross_side_area = Some(l * h);
    }
    if needs(QuantityKind::Perimeter) && cv.perimeter.is_none() {
        cv.perimeter = Some(2.0 * (x_dim + y_dim));
    }
    if needs(QuantityKind::GrossPerimeter) && cv.gross_perimeter.is_none() {
        cv.gross_perimeter = Some(2.0 * (x_dim + y_dim));
    }
}

fn missing(what: &str) -> PreprocessError {
    PreprocessError::Preprocessing(format!("QtoPreprocessPlugin: missing {what} in context"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ifc_model::IfcModel;
    use ifc_step::parse_step_bytes;
    use lbd_pipeline::{PipelineContext, ResourceLimits};

    use super::*;

    /// Minimal IFC4 STEP file: one wall, ExtrudedAreaSolid (200 × 300 profile, 3000 deep).
    /// No quantity sets present — the plugin must create Qto_WallBaseQuantities from scratch.
    const MINIMAL_WALL_STEP: &[u8] = b"\
ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION((''),'2;1');\n\
FILE_NAME('','',(''),(''),'',' ','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n\
#1=IFCWALL('2yVYPvADD2uRS2rmRe$fCB',$,'TestWall',$,$,$,#10,$,$);\n\
#10=IFCPRODUCTREPRESENTATION($,$,(#11));\n\
#11=IFCSHAPEREPRESENTATION(#20,'Body','SweptSolid',(#30));\n\
#20=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-5,#21,$);\n\
#21=IFCAXIS2PLACEMENT3D(#22,$,$);\n\
#22=IFCCARTESIANPOINT((0.,0.,0.));\n\
#30=IFCEXTRUDEDAREASOLID(#31,#21,#23,3000.);\n\
#31=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,200.,300.);\n\
#23=IFCDIRECTION((0.,0.,1.));\n\
ENDSEC;\n\
END-ISO-10303-21;\n\
";

    fn context_from_step(step_bytes: &[u8], opts: QtoOptions) -> PipelineContext {
        let step = parse_step_bytes(step_bytes).expect("parse step");
        let model = IfcModel::from_step_file(&step).expect("build model");
        let mut ctx = PipelineContext::new(ResourceLimits::default());
        ctx.insert(Arc::new(step));
        ctx.insert(Arc::new(model));
        ctx.insert(Arc::new(opts));
        ctx
    }

    #[test]
    fn creates_qto_wall_base_quantities_from_scratch() {
        let mut ctx = context_from_step(
            MINIMAL_WALL_STEP,
            QtoOptions { compute_mesh_volume: false },
        );
        QtoPreprocessPlugin
            .preprocess(&mut ctx)
            .expect("preprocess ok");

        let model = ctx.get::<IfcModel>().expect("model in ctx");

        // The wall must now have at least one quantity set.
        assert!(
            !model.quantities_for_object.is_empty(),
            "quantities_for_object should be populated"
        );

        // Find the Qto_WallBaseQuantities set.
        let has_wall_qto = model
            .element_quantities
            .values()
            .any(|qs| qs.name.as_deref() == Some("Qto_WallBaseQuantities"));
        assert!(has_wall_qto, "Qto_WallBaseQuantities should have been created");

        // At minimum, NetVolume should be computable from ExtrudedAreaSolid:
        // profile = 200 × 300 = 60_000, depth = 3000 → NetVolume = 180_000_000
        let net_vol_qty = model
            .element_quantities
            .values()
            .flat_map(|qs| &qs.quantities)
            .filter_map(|&id| model.physical_quantities.get(&id))
            .find(|q| q.name == "NetVolume");
        assert!(net_vol_qty.is_some(), "NetVolume quantity should exist");

        if let Some(qty) = net_vol_qty {
            if let Some(ifc_step::StepValue::Real(v)) = &qty.value {
                let expected = 200.0 * 300.0 * 3000.0;
                assert!(
                    (v - expected).abs() < 1.0,
                    "NetVolume {v} should be ~{expected}"
                );
            }
        }
    }

    #[test]
    fn skips_when_options_absent_and_env_not_set() {
        let step = parse_step_bytes(MINIMAL_WALL_STEP).expect("parse step");
        let model = IfcModel::from_step_file(&step).expect("build model");
        let before_qty_count = model.element_quantities.len();

        // No QtoOptions in context, env var not set → plugin must no-op.
        let mut ctx = PipelineContext::new(ResourceLimits::default());
        ctx.insert(Arc::new(step));
        ctx.insert(Arc::new(model));
        QtoPreprocessPlugin.preprocess(&mut ctx).expect("preprocess ok");

        let after = ctx.get::<IfcModel>().expect("model in ctx");
        assert_eq!(
            after.element_quantities.len(),
            before_qty_count,
            "model should be unchanged when plugin is not activated"
        );
    }
}
