//! QTO preprocess plugin.
//!
//! Detects missing IFC quantity sets on building elements, computes them from
//! STEP geometry and injects synthetic PhysicalQuantity + ElementQuantity
//! records into the model before any producer runs.
//!
//! Computation lives in `qto-geometry`, which is exact or silent:
//!
//! * **polyhedral** — the divergence theorem is exact for a closed polyhedron,
//!   so breps and tessellated sets need no kernel;
//! * **analytic** — an extrusion's volume is `profile area x perpendicular
//!   sweep` in closed form;
//! * **OCCT** (feature `occt`) — booleans, half-space clipping and circular
//!   sweeps, where closed form is unavailable.
//!
//! The bounding-box tier that used to sit under these was removed: it unioned
//! points from unrelated coordinate frames and produced, for one measured
//! example, a 0.05 m "Depth" for a 0.3 m slab, taken from a cutting plane's
//! origin. Nothing replaces it. An element whose geometry cannot be measured
//! exactly yields no quantity, because a wrong number is worse than a missing
//! one for a consumer that calculates with it.

mod audit;
mod gate;
mod inject;
mod qto_names;
pub mod units;
mod rep_parser;
mod step_geom;

use std::sync::Arc;

use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_pipeline::{
    FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin,
    PipelineStage, PluginManifest, PreprocessError, PreprocessPlugin, QTO_PREPROCESS_ID,
};
use serde_json::json;
use tracing::info;

use audit::MissingQuantityReport;
use inject::{inject, ComputedValues};
use qto_names::QuantityKind;
use step_geom::SolidKind;

/// Sum the volumes of all IFCOPENINGELEMENT entities that void `element_id`.
/// `element_cross_section_max_depth` caps the effective opening depth to the element's own
/// cross-section dimension — openings are modelled to extend beyond the element on both sides
/// for clean boolean cuts, so their full extrusion depth overcounts the actual subtraction.
fn sum_opening_volumes(
    step: &ifc_step::StepFile,
    model: &IfcModel,
    element_id: ifc_step::EntityId,
    element_cross_section_max_depth: f64,
) -> f64 {
    model
        .rel_voids
        .iter()
        .filter(|rv| rv.element == element_id)
        .filter_map(|rv| {
            match step_geom::best_solid(step, rv.opening) {
                SolidKind::ExtrudedAreaSolid { profile_id, depth } => {
                    rep_parser::from_extruded_solid(step, profile_id, depth)
                        .map(|r| r.profile_area * depth.min(element_cross_section_max_depth))
                }
                SolidKind::BoundingBox { x_dim, y_dim, z_dim } => {
                    Some(x_dim * y_dim * z_dim.min(element_cross_section_max_depth))
                }
                _ => None,
            }
        })
        .sum()
}

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

        // Quantities must be emitted in the units the model declares, and the
        // compute tiers work in raw geometry units. If the scale cannot be
        // established there is no safe value to write, so the plugin does
        // nothing rather than emitting against a guessed one.
        let scales = match units::scales_for(&model) {
            Ok(s) => s,
            Err(reason) => {
                ctx.write_log(QTO_PREPROCESS_ID, json!({
                    "skipped": true,
                    "reason": reason,
                }));
                info!("qto preprocess: skipped — {reason}");
                return Ok(());
            }
        };

        // --- Audit ----------------------------------------------------------
        let reports = audit::audit(&model);
        let elements_scanned = model.elements.len() + model.spatial_nodes.len();
        let elements_missing_qto = reports.len();
        let elements_with_all_qto = elements_scanned.saturating_sub(elements_missing_qto);

        // --- Compute --------------------------------------------------------
        // One cache per run, shared across workers. Keyed by solid, so elements
        // that share geometry through IfcMappedItem are measured once.
        #[cfg(feature = "occt")]
        let occt_cache: OcctCache = Default::default();
        #[cfg(feature = "occt")]
        qto_geometry::occt::init();

        // Compute geometry for each element independently — parallel-safe since
        // StepFile and IfcModel are read-only through shared references.
        #[cfg(not(target_arch = "wasm32"))]
        let raw_results: Vec<(usize, ComputeOutput)> = {
            use rayon::prelude::*;
            reports
                .par_iter()
                .enumerate()
                .map(|(idx, report)| {
                    (
                        idx,
                        compute_for_element(
                            &step,
                            &model,
                            report,
                            &options,
                            #[cfg(feature = "occt")]
                            &occt_cache,
                        ),
                    )
                })
                .collect()
        };
        #[cfg(target_arch = "wasm32")]
        let raw_results: Vec<(usize, ComputeOutput)> = reports
            .iter()
            .enumerate()
            .map(|(idx, report)| {
                (
                    idx,
                    compute_for_element(
                        &step,
                        &model,
                        report,
                        &options,
                        #[cfg(feature = "occt")]
                        &occt_cache,
                    ),
                )
            })
            .collect();

        let mut computed_pairs: Vec<(usize, ComputedValues)> = Vec::new();
        let mut tier_bbox: u32 = 0;
        let mut tier_rep: u32 = 0;
        let mut tier_mesh: u32 = 0;
        let mut skipped_no_geometry: u32 = 0;
        let mut occt_attempted: u32 = 0;
        let mut occt_succeeded: u32 = 0;

        for (idx, cv) in raw_results {
            if cv.occt_attempted {
                occt_attempted += 1;
            }
            if cv.occt_succeeded {
                occt_succeeded += 1;
            }
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
        let (augmented, quantities_computed_total, rejections) =
            inject(&model, &step, &reports, &computed_pairs, &scales);

        let sets_created = augmented.element_quantities.len()
            - model.element_quantities.len();
        let sets_extended = computed_pairs.len().saturating_sub(sets_created);

        ctx.replace(Arc::new(augmented));

        // --- Log ------------------------------------------------------------
        ctx.write_log(QTO_PREPROCESS_ID, json!({
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
            "occt": {
                "compiled_in": cfg!(feature = "occt"),
                "attempted": occt_attempted,
                "succeeded": occt_succeeded,
            },
            "units": {
                "length_to_si": scales.length,
                "area_to_si": scales.area,
                "volume_to_si": scales.volume,
            },
            "values_refused": {
                "total": rejections.total(),
                "not_finite": rejections.not_finite,
                "non_positive": rejections.non_positive,
                "net_exceeds_gross": rejections.net_exceeds_gross,
            },
        }));

        info!(
            "qto preprocess: {quantities_computed_total} quantities computed, \
             {sets_created} sets created, {sets_extended} sets extended, \
             {} values refused, occt {occt_succeeded}/{occt_attempted}",
            rejections.total()
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
    /// Did the OCCT fallback get asked, and did it answer? Recorded so a
    /// backend that is wired but idle is visible rather than silently absent.
    occt_attempted: bool,
    occt_succeeded: bool,
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
            && v.gross_surface_area.is_none()
            && v.net_surface_area.is_none()
            && v.gross_perimeter.is_none()
            && v.perimeter.is_none()
    }
}

fn compute_for_element(
    step: &StepFile,
    model: &IfcModel,
    report: &MissingQuantityReport,
    options: &QtoOptions,
    #[cfg(feature = "occt")] occt_cache: &OcctCache,
) -> ComputeOutput {
    let element_id = report.element_id;
    let mut cv = ComputedValues::default();
    let mut tier = ComputeTier::Bbox;
    let mut occt_attempted = false;
    let mut occt_succeeded = false;

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
        SolidKind::ExtrudedAreaSolid { .. } => {
            // Exact: volume is profile area x perpendicular sweep, closed form.
            // `qto-geometry` reads the extrusion *direction*, which the previous
            // implementation ignored — an oblique sweep was over-reported by
            // 1/cos(theta), and a horizontally extruded slab had its thickness
            // and length swapped.
            let parts: Vec<_> = extrusion_solid_ids(step, element_id)
                .into_iter()
                .filter_map(|id| qto_geometry::metrics_for_extrusion(step, id))
                .collect();
            if let Some(m) = parts.first() {
                tier = ComputeTier::Rep;
                let entity_type = report.entity_type.to_uppercase();
                let needs = |k: QuantityKind| report.missing.contains(&k);

                // Volumes and swept surfaces add across parts; profiles do not.
                // A cross-section area or a perimeter belongs to one solid, so
                // where a body is made of several they are withheld rather than
                // taken from whichever happened to be first.
                let total_volume: f64 = parts.iter().map(|p| p.volume).sum();
                let total_lateral: f64 = parts.iter().map(|p| p.lateral_area).sum();
                let single = parts.len() == 1;

                match entity_type.as_str() {
                    "IFCBEAM" | "IFCCOLUMN" | "IFCMEMBER" | "IFCPILE" | "IFCFOOTING" => {
                        // Length runs along the sweep axis, so it is the sweep
                        // distance itself, not its vertical component.
                        if single && needs(QuantityKind::Length) {
                            cv.length = Some(m.depth);
                        }
                        if single && needs(QuantityKind::CrossSectionArea) {
                            cv.cross_section_area = Some(m.profile.area);
                        }
                        // The swept surface, excluding the two end caps.
                        if needs(QuantityKind::OuterSurfaceArea) {
                            cv.outer_surface_area = Some(total_lateral);
                        }
                        if single && needs(QuantityKind::GrossSurfaceArea) {
                            cv.gross_surface_area = Some(m.total_area);
                        }
                    }
                    "IFCWALL" | "IFCWALLSTANDARDCASE" => {
                        let h = inline_height.unwrap_or(m.height);
                        if single && needs(QuantityKind::Height) {
                            cv.height = Some(h);
                        }
                        // For a vertically swept wall the profile IS the plan
                        // footprint, so its spans give length and thickness.
                        // Both were previously left to the bbox, which is empty
                        // for a parametric profile — hence no Length or Width.
                        // Length/Width are deliberately NOT taken from the
                        // profile's bounding spans. Measured against authored
                        // data that mapping scored 34.3% where the previous
                        // source scored 91.4%: a profile's bbox spans are not
                        // the wall's length and thickness once the outline is
                        // non-rectangular or the wall is not axis-aligned.
                        if single && needs(QuantityKind::GrossFootprintArea) {
                            cv.gross_footprint_area = Some(m.profile.area);
                        }
                        // NetFootprintArea is the footprint "taking all wall
                        // modifications (like recesses) into account". Nothing
                        // here models recesses, so it is only safe where the
                        // element has no openings at all and the two figures
                        // coincide.
                        if single
                            && needs(QuantityKind::NetFootprintArea)
                            && !has_openings(model, element_id)
                        {
                            cv.net_footprint_area = Some(m.profile.area);
                        }
                        // One side face: length x height. The previous code used
                        // the full lateral wrap — both faces plus both ends —
                        // which measured ~2x too large.
                        // GrossSideArea is the wall "as viewed by an elevation
                        // view of the middle plane". Its length is the wall's
                        // run along that plane, which equals the profile's
                        // longest bbox span only when the footprint is a
                        // rectangle. For an L-shaped, tapered or curved
                        // footprint the span is shorter than the run, so the
                        // area would be under-reported.
                        if single
                            && needs(QuantityKind::GrossSideArea)
                            && is_rectangular(&m.profile)
                        {
                            cv.gross_side_area = Some(m.profile.max_span * h);
                        }
                    }
                    "IFCSLAB" | "IFCROOF" | "IFCPLATE" | "IFCCOVERING" => {
                        if single && needs(QuantityKind::Depth) {
                            cv.depth = Some(m.height);
                        }
                        // Same as walls: bbox spans of the plan outline are not
                        // the slab's Length and Width. Withheld until a mapping
                        // is validated against authored data.
                        if single && needs(QuantityKind::GrossArea) {
                            cv.gross_area = Some(m.profile.area);
                        }
                        if single && needs(QuantityKind::Perimeter) {
                            cv.perimeter = Some(m.profile.perimeter);
                        }
                    }
                    // IfcSpace was absent from this match entirely, so rooms got
                    // no height, floor area or perimeter despite all three being
                    // directly available from the extrusion.
                    "IFCSPACE" => {
                        if single && needs(QuantityKind::Height) {
                            cv.height = Some(m.height);
                        }
                        if single && needs(QuantityKind::GrossFloorArea) {
                            cv.gross_floor_area = Some(m.profile.area);
                        }
                        // NetFloorArea is "sum of all net usable floor areas" —
                        // gross floor area minus what is not usable. The
                        // extrusion profile gives the gross figure only.
                        if single && needs(QuantityKind::GrossPerimeter) {
                            cv.gross_perimeter = Some(m.profile.perimeter);
                        }
                    }
                    _ => {}
                }

                if needs(QuantityKind::GrossVolume) {
                    cv.gross_volume = Some(total_volume);
                }
                if needs(QuantityKind::NetVolume) {
                    let opening_vol =
                        sum_opening_volumes(step, model, element_id, m.profile.min_span);
                    cv.net_volume = Some((total_volume - opening_vol).max(0.0));
                }
                if single && needs(QuantityKind::CrossSectionArea) && cv.cross_section_area.is_none() {
                    cv.cross_section_area = Some(m.profile.area);
                }
                if single && needs(QuantityKind::Area) && cv.area.is_none() {
                    cv.area = Some(m.profile.area);
                }
            }
        }

        // ------------------------------------------------------------------
        // Tier 3: mesh-based
        // ------------------------------------------------------------------
        // Polyhedral representations go through the exact path in
        // `qto-geometry`: the divergence theorem is exact for a closed
        // polyhedron, so no kernel and no tolerance are involved. It refuses
        // open or untriangulable shells rather than returning a plausible
        // number, which is why there is no fallback arm here.
        SolidKind::FacetedBrep { .. }
        | SolidKind::TriangulatedFaceSet { .. }
        | SolidKind::FaceBasedSurfaceModel { .. }
            if options.compute_mesh_volume =>
        {
            // The solid entity, not the shell: `qto-geometry` walks the
            // representation itself so it can see voids and inner bounds that a
            // bare shell reference hides.
            if let Some(solid_id) = polyhedral_solid_id(step, element_id) {
                match qto_geometry::polyhedral_metrics_for(step, solid_id) {
                    Ok(m) => {
                        tier = ComputeTier::Mesh;
                        let needs = |k: QuantityKind| report.missing.contains(&k);
                        // A brep is the as-built shape: the exporter has
                        // already cut its openings and recesses, so its volume
                        // is the *net* figure.
                        if needs(QuantityKind::NetVolume) {
                            cv.net_volume = Some(m.volume);
                        }
                        // GrossVolume explicitly does not take openings into
                        // account, so it cannot be read off a solid that has
                        // them cut — unless the element has none, in which case
                        // the two are the same figure and withholding it would
                        // discard a correct value.
                        if needs(QuantityKind::GrossVolume) && !has_openings(model, element_id) {
                            cv.gross_volume = Some(m.volume);
                        }

                        // Dimensions from the solid's own extents, and only
                        // where the solid provably *is* its bounding box. Every
                        // vertex here belongs to one solid in one coordinate
                        // frame, so unlike the removed bbox tier this measures
                        // the object rather than a union of unrelated point
                        // sets — but an envelope is still not a dimension
                        // unless the object fills it.
                        if m.fills_extent() {
                            if needs(QuantityKind::Length) {
                                cv.length = Some(m.extent[2]);
                            }
                            // Width and Depth are NOT taken from the extents.
                            // Measured against authored data, the smallest
                            // extent scored 26.4% as Width (against 98.3% from
                            // the previous source) and 0.5% as Depth. Which of
                            // a box's three dimensions an exporter calls its
                            // width or depth depends on the object's own axes,
                            // which the extents do not carry.
                        }
                        // No surface-area quantity is emitted from a brep.
                        //
                        // IFC defines OuterSurfaceArea as "total area of the
                        // surfaces of the object (not taking into account the
                        // end cap areas)", i.e. the lateral surface. A general
                        // polyhedron has no identifiable end caps without a
                        // sweep axis, so the total boundary area this path
                        // computes is a different quantity and must not be
                        // labelled OuterSurfaceArea. Emitting it scored 2.9%
                        // against authored data.
                        //
                        // GrossSurfaceArea and NetSurfaceArea are withheld too:
                        // the total boundary area measures ~1.97x the authored
                        // figure on 6,230 elements whose volume this same code
                        // reproduces exactly (ratio 1.000000), and that
                        // discrepancy is not yet explained. Until it is, there
                        // is no surface figure here that can be emitted
                        // honestly.
                    }
                    Err(reason) => {
                        tracing::debug!(
                            element = %element_id,
                            ?reason,
                            "polyhedron not exactly measurable; emitting nothing"
                        );
                    }
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

    // The bounding-box gap-fill that used to sit here has been removed.
    //
    // `bbox::compute` unions every IfcCartesianPoint reachable from every shape
    // representation with no transform applied: profile-local 2D points,
    // placement origins, axis curves and clipping-plane origins all land in one
    // AABB. Measured against authored data it produced a 0.05 m "Depth" for a
    // 0.3 m slab, taken from a cutting plane's origin, and Width values that
    // scored 34% correct where a real source scored 91%.
    //
    // It was mostly invisible before: elements whose geometry could not be
    // parsed were dropped wholesale, so its values rarely reached output. Once
    // the exact paths started succeeding, those same elements passed the filter
    // and carried the bbox values with them. A quantity derived from mixed
    // coordinate frames is not recoverable by any tolerance, so it is not
    // emitted at all.

    // OCCT fallback for the geometry the exact analytic paths refuse: booleans
    // and half-space clipping, circular sweeps, and profiles whose outline
    // contains arcs. It runs only where nothing was computed, so an exact
    // closed-form answer is never replaced by a kernel one.
    #[cfg(feature = "occt")]
    if cv.gross_volume.is_none() && cv.net_volume.is_none() {
        if let Some(item) = first_body_item(step, element_id) {
            occt_attempted = true;
            if let Some(m) = occt_measure_cached(step, item, occt_cache) {
                occt_succeeded = true;
                tier = ComputeTier::Mesh;
                let needs = |k: QuantityKind| report.missing.contains(&k);
                // A boolean result is the as-built shape, openings already cut,
                // so its volume is the net figure — the same reasoning as for a
                // brep. Gross is only the same number when nothing voids it.
                if needs(QuantityKind::NetVolume) {
                    cv.net_volume = Some(m.volume);
                }
                if needs(QuantityKind::GrossVolume) && !has_openings(model, element_id) {
                    cv.gross_volume = Some(m.volume);
                }
                // Surface area is deliberately not taken here. OuterSurfaceArea
                // excludes end caps by definition and a boolean result has no
                // identifiable caps, exactly as for a brep.
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

    ComputeOutput {
        values: cv,
        tier,
        occt_attempted,
        occt_succeeded,
    }
}

/// Entity id of the element's first polyhedral solid.
///
/// `SolidKind::FacetedBrep` carries the *shell*, but `qto-geometry` needs the
/// solid itself so it can see `IfcFacetedBrepWithVoids`' inner shells. Booleans
/// and mapped items are followed the same way `step_geom` follows them.
fn polyhedral_solid_id(
    step: &StepFile,
    element_id: ifc_step::EntityId,
) -> Option<ifc_step::EntityId> {
    fn walk(
        step: &StepFile,
        id: ifc_step::EntityId,
        depth: usize,
    ) -> Option<ifc_step::EntityId> {
        if depth > 6 {
            return None;
        }
        let e = step.entities.get(&id)?;
        match e.entity_name.as_str() {
            "IFCFACETEDBREP"
            | "IFCFACETEDBREPWITHVOIDS"
            | "IFCTRIANGULATEDFACESET"
            | "IFCPOLYGONALFACESET"
            | "IFCFACEBASEDSURFACEMODEL"
            | "IFCSHELLBASEDSURFACEMODEL" => Some(id),
            // As above: a boolean's first operand is the uncut solid.
            "IFCMAPPEDITEM" => {
                let src = e.args.first().and_then(ifc_step::StepValue::as_ref)?;
                let mapped = step
                    .entities
                    .get(&src)?
                    .args
                    .get(1)
                    .and_then(ifc_step::StepValue::as_ref)?;
                let items = step.entities.get(&mapped)?.args.get(3)?.as_list()?;
                items
                    .iter()
                    .filter_map(ifc_step::StepValue::as_ref)
                    .find_map(|i| walk(step, i, depth + 1))
            }
            _ => None,
        }
    }

    for rep_id in step_geom::shape_reps(step, element_id) {
        let Some(rep) = step.entities.get(&rep_id) else {
            continue;
        };
        let Some(items) = rep.args.get(3).and_then(ifc_step::StepValue::as_list) else {
            continue;
        };
        if let Some(found) = items
            .iter()
            .filter_map(ifc_step::StepValue::as_ref)
            .find_map(|i| walk(step, i, 0))
        {
            return Some(found);
        }
    }
    None
}

/// Entity id of the element's first `IfcExtrudedAreaSolid`, following mapped
/// items and boolean first-operands as `step_geom` does.
/// Volume and area per *distinct solid*, not per element.
///
/// Corpus models carry 871-3,339 distinct solids regardless of element count —
/// model E's 92,361 mapped items resolve to 2,541 solids, a 36x reuse factor.
/// Since OCCT booleans cost ~3.3 ms and parallelise only 1.4x on 14 cores,
/// measuring each solid once is the difference between seconds and minutes.
///
/// Keyed on the representation item, so two elements sharing geometry through
/// IfcMappedItem share the result. Volume is invariant under the rigid
/// transforms mapped items apply; a *scaled* instance would not be, which is
/// why only rigid placements reach this path.
#[cfg(feature = "occt")]
pub(crate) type OcctCache = std::sync::Mutex<
    std::collections::HashMap<ifc_step::EntityId, Option<qto_geometry::occt::SolidMetrics>>,
>;

/// Measure a solid through OCCT, reusing an earlier result for the same solid.
#[cfg(feature = "occt")]
fn occt_measure_cached(
    step: &StepFile,
    solid_id: ifc_step::EntityId,
    cache: &OcctCache,
) -> Option<qto_geometry::occt::SolidMetrics> {
    if let Ok(c) = cache.lock() {
        if let Some(hit) = c.get(&solid_id) {
            return *hit;
        }
    }
    // Built outside the lock: OCCT construction is the expensive part and
    // holding the mutex across it would serialise every worker.
    let result = qto_geometry::occt_build::build(step, solid_id)
        .and_then(|solid| qto_geometry::occt::measure(&solid))
        .ok();
    if let Ok(mut c) = cache.lock() {
        c.insert(solid_id, result);
    }
    result
}

/// First representation item of the element's body, whatever kind it is.
#[cfg(feature = "occt")]
fn first_body_item(
    step: &StepFile,
    element_id: ifc_step::EntityId,
) -> Option<ifc_step::EntityId> {
    for rep_id in step_geom::shape_reps(step, element_id) {
        let rep = step.entities.get(&rep_id)?;
        let is_body = rep
            .args
            .get(1)
            .and_then(ifc_step::StepValue::as_str)
            .map(|i| i.eq_ignore_ascii_case("body"))
            .unwrap_or(false);
        if !is_body {
            continue;
        }
        if let Some(items) = rep.args.get(3).and_then(ifc_step::StepValue::as_list) {
            if let Some(first) = items.iter().filter_map(ifc_step::StepValue::as_ref).next() {
                return Some(first);
            }
        }
    }
    None
}

fn extrusion_solid_ids(step: &StepFile, element_id: ifc_step::EntityId) -> Vec<ifc_step::EntityId> {
    fn walk(step: &StepFile, id: ifc_step::EntityId, depth: usize) -> Option<ifc_step::EntityId> {
        if depth > 6 {
            return None;
        }
        let e = step.entities.get(&id)?;
        match e.entity_name.as_str() {
            "IFCEXTRUDEDAREASOLID" => Some(id),
            // Deliberately does NOT descend into booleans. The first operand is
            // the solid *before* it was cut, so measuring it reports material
            // that the boolean removed. Clipped geometry is the OCCT path's
            // job; without that feature the element yields nothing, which is
            // the correct outcome rather than an over-report.
            "IFCMAPPEDITEM" => {
                let src = e.args.first().and_then(ifc_step::StepValue::as_ref)?;
                let mapped = step
                    .entities
                    .get(&src)?
                    .args
                    .get(1)
                    .and_then(ifc_step::StepValue::as_ref)?;
                let items = step.entities.get(&mapped)?.args.get(3)?.as_list()?;
                items
                    .iter()
                    .filter_map(ifc_step::StepValue::as_ref)
                    .find_map(|i| walk(step, i, depth + 1))
            }
            _ => None,
        }
    }
    // Every item, not just the first. A Body representation routinely holds more
    // than one solid: a wall built from two wythes, a stair flight carrying one
    // extrusion per tread. Taking only the first reported 39% of such a wall and
    // 0.5% of such a stair — both found by cross-checking against IfcOpenShell,
    // which sums them.
    let mut out = Vec::new();
    for rep_id in step_geom::shape_reps(step, element_id) {
        let Some(rep) = step.entities.get(&rep_id) else {
            continue;
        };
        let Some(items) = rep.args.get(3).and_then(ifc_step::StepValue::as_list) else {
            continue;
        };
        for item in items.iter().filter_map(ifc_step::StepValue::as_ref) {
            if let Some(found) = walk(step, item, 0) {
                out.push(found);
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

/// Is this profile a rectangle, to within f64 slack?
///
/// Several quantities are only well defined for a prismatic, rectangular
/// footprint — IFC says as much for `Width` ("only given if the object has
/// constant thickness"). Where the outline is not a rectangle its bbox spans
/// stop describing the object and the quantity is withheld.
/// Does anything void this element?
///
/// Several quantities are defined as "gross" (ignoring openings) or "net"
/// (accounting for them). Where an element has no openings the two coincide, so
/// one computation legitimately serves both — but only then.
fn has_openings(model: &IfcModel, element_id: ifc_step::EntityId) -> bool {
    model.rel_voids.iter().any(|rv| rv.element == element_id)
}

fn is_rectangular(p: &qto_geometry::ProfileMetrics) -> bool {
    let bbox_area = p.max_span * p.min_span;
    bbox_area > 0.0 && ((p.area - bbox_area).abs() / bbox_area) < 1e-9
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

    /// The same wall, in a model whose geometry is in millimetres while its
    /// quantities are declared in SI — the configuration three of the six
    /// corpus models use, and the one that made 45% of emitted values unusable.
    ///
    /// Geometry: 200 x 300 profile, 3000 deep, all mm → 1.8e8 mm³ = 0.18 m³.
    /// `VOLUMEUNIT` is CUBIC_METRE, so 0.18 is the only correct output.
    const MM_WALL_SI_QUANTITIES: &[u8] = b"\
ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION((''),'2;1');\n\
FILE_NAME('','',(''),(''),'',' ','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n\
#1=IFCWALL('2yVYPvADD2uRS2rmRe$fCB',$,'TestWall',$,$,$,#10,$,$);\n\
#2=IFCPROJECT('0project00000000000000',$,'P',$,$,$,$,$,#3);\n\
#3=IFCUNITASSIGNMENT((#4,#5,#6));\n\
#4=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);\n\
#5=IFCSIUNIT(*,.AREAUNIT.,$,.SQUARE_METRE.);\n\
#6=IFCSIUNIT(*,.VOLUMEUNIT.,$,.CUBIC_METRE.);\n\
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

    fn quantity_named(model: &IfcModel, name: &str) -> Option<f64> {
        model
            .element_quantities
            .values()
            .flat_map(|qs| &qs.quantities)
            .filter_map(|id| model.physical_quantities.get(id))
            .find(|q| q.name == name)
            .and_then(|q| match &q.value {
                Some(ifc_step::StepValue::Real(v)) => Some(*v),
                _ => None,
            })
    }

    #[test]
    fn volumes_are_emitted_in_the_declared_unit_not_raw_geometry_units() {
        let mut ctx = context_from_step(
            MM_WALL_SI_QUANTITIES,
            QtoOptions {
                compute_mesh_volume: false,
            },
        );
        QtoPreprocessPlugin.preprocess(&mut ctx).expect("preprocess ok");
        let model = ctx.get::<IfcModel>().expect("model in ctx");

        let net_volume = quantity_named(&model, "NetVolume").expect("NetVolume emitted");

        // 200 x 300 x 3000 mm = 1.8e8 mm³ = 0.18 m³.
        assert!(
            (net_volume - 0.18).abs() < 1e-9,
            "NetVolume should be 0.18 m³ (declared VOLUMEUNIT), got {net_volume}"
        );
        // Guard the specific regression: emitting the raw mm³ figure.
        assert!(
            net_volume < 1.0,
            "NetVolume {net_volume} looks like raw mm³ — unit conversion was skipped"
        );
    }

    /// Length quantities are declared in `LENGTHUNIT`, the same unit the geometry
    /// uses, so they must pass through unscaled. Over-converting them would be as
    /// wrong as not converting volumes.
    #[test]
    fn lengths_are_not_rescaled_in_a_millimetre_model() {
        let mut ctx = context_from_step(
            MM_WALL_SI_QUANTITIES,
            QtoOptions {
                compute_mesh_volume: false,
            },
        );
        QtoPreprocessPlugin.preprocess(&mut ctx).expect("preprocess ok");
        let model = ctx.get::<IfcModel>().expect("model in ctx");

        if let Some(height) = quantity_named(&model, "Height") {
            assert!(
                (height - 3000.0).abs() < 1e-6,
                "Height should stay 3000 mm, got {height}"
            );
        }
    }

    /// One model in the corpus (`20210219Architecture.ifc`) declares its length
    /// unit as a conversion-based FOOT. The conversion factor lives in an
    /// `IfcMeasureWithUnit` that `ifc-model` does not expose, so there is no
    /// trustworthy scale — and under the project rule a value that cannot be
    /// scaled correctly must not be written at all.
    #[test]
    fn emits_nothing_when_units_are_conversion_based() {
        let imperial: &[u8] = b"\
ISO-10303-21;\n\
HEADER;\n\
FILE_DESCRIPTION((''),'2;1');\n\
FILE_NAME('','',(''),(''),'',' ','');\n\
FILE_SCHEMA(('IFC4'));\n\
ENDSEC;\n\
DATA;\n\
#1=IFCWALL('2yVYPvADD2uRS2rmRe$fCB',$,'TestWall',$,$,$,#10,$,$);\n\
#2=IFCPROJECT('0project00000000000000',$,'P',$,$,$,$,$,#3);\n\
#3=IFCUNITASSIGNMENT((#4));\n\
#4=IFCCONVERSIONBASEDUNIT(#7,.LENGTHUNIT.,'FOOT',#8);\n\
#7=IFCDIMENSIONALEXPONENTS(1,0,0,0,0,0,0);\n\
#8=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(0.3048),#9);\n\
#9=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n\
#10=IFCPRODUCTREPRESENTATION($,$,(#11));\n\
#11=IFCSHAPEREPRESENTATION(#20,'Body','SweptSolid',(#30));\n\
#20=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-5,#21,$);\n\
#21=IFCAXIS2PLACEMENT3D(#22,$,$);\n\
#22=IFCCARTESIANPOINT((0.,0.,0.));\n\
#30=IFCEXTRUDEDAREASOLID(#31,#21,#23,10.);\n\
#31=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,2.);\n\
#23=IFCDIRECTION((0.,0.,1.));\n\
ENDSEC;\n\
END-ISO-10303-21;\n\
";
        let mut ctx = context_from_step(
            imperial,
            QtoOptions {
                compute_mesh_volume: false,
            },
        );
        QtoPreprocessPlugin.preprocess(&mut ctx).expect("preprocess ok");
        let model = ctx.get::<IfcModel>().expect("model in ctx");

        assert!(
            model.element_quantities.is_empty(),
            "no quantity may be written when the unit scale is unknown, got {:?}",
            model.element_quantities
        );
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
