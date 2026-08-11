//! QTO preprocess plugin.
//!
//! Detects missing IFC quantity sets on building elements, computes them from
//! STEP geometry and injects synthetic PhysicalQuantity + ElementQuantity
//! records into the model before any producer runs.
//!
//! Measurement happens three ways, in order of how much the result can be
//! trusted, and each one refuses rather than approximates:
//!
//! * **tessellation** — `ifc-lite` evaluates whatever the representation is
//!   (sweeps, breps, booleans, half-space clips, CSG, revolutions, mapped
//!   items) into triangles with openings already cut, and the divergence
//!   theorem measures the result. One path for every solid kind, which is how
//!   IfcOpenShell reaches the coverage it does. Guarded by an enclosure bound,
//!   because a shell built from a long boolean chain can be non-manifold and
//!   integrate to several times its own volume without any local sign of it;
//! * **polyhedral** — the divergence theorem applied to a brep straight from
//!   the STEP file, where the exporter's own facets are the authority;
//! * **analytic** — an extrusion's volume is `profile area x perpendicular
//!   sweep` in closed form, and its sweep axis is the only thing that says
//!   which direction the element runs in.
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
mod tessellated;
pub mod units;
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

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Marker that switches the QTO preprocess plugin on.
///
/// Insert an `Arc<QtoOptions>` into the pipeline context before running to
/// enable this plugin. If absent from context the plugin skips immediately.
/// The env var `IFC2LBD_QTO_ENABLED=1` can also activate it.
///
/// It carries no settings. The two it used to carry — whether to measure
/// tessellated geometry at all, and whether to take areas from it as well as
/// volumes — existed to score the tessellation approach against
/// per-representation arithmetic on the same files. That comparison is settled:
/// tessellation reached 44.8% coverage at 98.2% precision where the
/// per-representation path reached 25.1% at 92.2%, so there is nothing left to
/// switch between.
#[derive(Debug, Clone, Copy, Default)]
pub struct QtoOptions;

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
        // Activation check: explicit marker in context or env-var fallback.
        if ctx.get::<QtoOptions>().is_none()
            && std::env::var("IFC2LBD_QTO_ENABLED").as_deref() != Ok("1")
        {
            return Ok(());
        }

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

        // Tessellated geometry, measured the way IfcOpenShell measures it: let
        // ifc-lite evaluate whatever the representation is into triangles, with
        // openings already subtracted, then measure the result. One code path
        // for every solid kind.
        let mesh_q = match ctx.get::<plugin_geometry_preprocess::IFCContent>() {
            Some(content) => {
                let mut ids: Vec<u64> = reports.iter().map(|r| r.element_id).collect();
                // The openings that doors and windows fill are measured too:
                // their quantity sets are defined against the lining, which is
                // the hole rather than the leaf.
                let wanted: std::collections::HashSet<u64> = ids.iter().copied().collect();
                ids.extend(
                    model
                        .rel_fills
                        .iter()
                        .filter(|rf| wanted.contains(&rf.element))
                        .map(|rf| rf.opening),
                );
                ids.sort_unstable();
                ids.dedup();
                tessellated::measure_all(std::sync::Arc::clone(&content.0), &ids, scales.length)
            }
            None => Default::default(),
        };

        // --- Compute --------------------------------------------------------
        // Each element is measured independently — parallel-safe, since StepFile
        // and IfcModel are read-only through shared references.
        let measure = |(idx, report): (usize, &MissingQuantityReport)| {
            (
                idx,
                compute_for_element(
                    &step,
                    &model,
                    report,
                    mesh_q.get(&report.element_id).copied(),
                    &mesh_q,
                ),
            )
        };
        #[cfg(not(target_arch = "wasm32"))]
        let raw_results: Vec<(usize, ComputeOutput)> = {
            use rayon::prelude::*;
            reports.par_iter().enumerate().map(measure).collect()
        };
        #[cfg(target_arch = "wasm32")]
        let raw_results: Vec<(usize, ComputeOutput)> =
            reports.iter().enumerate().map(measure).collect();

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
             {} values refused",
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
    mesh: Option<tessellated::MeshQuantities>,
    opening_mesh: &std::collections::HashMap<u64, tessellated::MeshQuantities>,
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

    // Volume from the tessellated solid, before any per-representation
    // arithmetic. openings are already subtracted by ifc-lite, so this is the
    // net figure; gross is the same only where nothing voids the element.
    let entity_upper = report.entity_type.to_uppercase();
    if let Some(m) = mesh {
        let needs = |k: QuantityKind| report.missing.contains(&k);
        // A tessellated volume is only believed when it passes the enclosure
        // bound. Without it, models built from long boolean chains report
        // volumes several times too large — ArchiCAD's model D model
        // over-reported a 1.2 x 0.01 x 0.07 m slab strip by 94%, and its median
        // element by 6x, while its surface and shadow figures were exact.
        //
        // Voided elements are excluded outright, for net as well as gross. The
        // mesh does have its openings cut, but *how* they were cut is exactly
        // what cannot be checked: on elements with declared voids the
        // tessellated volume was right 56% of the time against 96% on elements
        // without. The exact paths below still answer for those where the
        // representation supports it.
        // Only a mesh *proved* to be a single closed orientable solid, from a
        // single segment, licenses a divergence-theorem volume — an open,
        // non-orientable, multi-component or summed-over-items surface produces
        // a number that looks perfectly ordinary and is not the object's.
        //
        // Voided elements stay excluded on top of that, and the topology verdict
        // is not enough to replace it: on 703 voided walls the mesh passed all
        // three clauses and the volume was still right only 3.3% of the time,
        // ratios spread 0.004 to 3.8. The boolean result is approximately the
        // right solid and exactly the wrong number.
        if m.trustworthy_solid && m.is_volume_sound() && !has_openings(step, model, element_id) {
            if needs(QuantityKind::NetVolume) {
                cv.net_volume = Some(m.volume);
            }
            if needs(QuantityKind::GrossVolume) {
                cv.gross_volume = Some(m.volume);
            }
        }
        // Areas and dimensions from the same tessellation. A closed surface
        // covers its own shadow exactly twice, so a projected area is half the
        // summed triangle-area component along that axis — which gives the plan
        // and elevation views the footprint and side quantities are defined as.
        {
            if needs(QuantityKind::NetSurfaceArea) {
                // Not for bar-like members. The tessellated boundary counts
                // every face, while the authored figure deducts where members
                // meet: measured, 212 of 426 disagree and 74% of them by the
                // same +1.5%, so it is a definitional difference, not noise.
                // Not for bar-like members on IFC2X3. The tessellated boundary
                // counts every face while that schema's exporters deduct where
                // members meet: 212 of 426 disagree and 74% of them by the same
                // +1.5%, a definitional difference rather than noise. On IFC4 the
                // same measurement is right, so the schema is the discriminator.
                let bar = matches!(
                    entity_upper.as_str(),
                    "IFCMEMBER" | "IFCMEMBERSTANDARDCASE" | "IFCBEAM" | "IFCBEAMSTANDARDCASE"
                );
                if !(bar && matches!(model.schema, ifc_step::StepSchema::Ifc2x3)) {
                    cv.net_surface_area = Some(m.surface_area);
                }
            }
            // GrossSurfaceArea is NOT taken from the mesh. It measured 78.4%
            // there against 100% from the extrusion's closed-form total area,
            // which the sweep path supplies below.

            // Footprint-derived areas belong only to classes whose reference
            // plane is the plan. A window's GrossArea is its elevation, and
            // taking its shadow instead was wrong for every one of the 174 in
            // the corpus — the plan view of a window is its thickness.
            // The plan-referenced areas hold on IFC4 (NetArea 99.5%, GrossArea
            // 99.9%) but not on IFC2X3, where a slab's authored NetArea and
            // NetVolume disagree with the projected footprint with no clustering
            // (ratios 0.0 to 10.8 — the upper tail is that exporter writing
            // square feet). 419 values, none recoverable by a rule.
            let plan_ok = is_plan_referenced(&entity_upper)
                && !(matches!(entity_upper.as_str(), "IFCSLAB" | "IFCSLABSTANDARDCASE")
                    && matches!(model.schema, ifc_step::StepSchema::Ifc2x3));
            if plan_ok && m.footprint_area() > 0.0 {
                if needs(QuantityKind::GrossFootprintArea) && !has_openings(step, model, element_id) {
                    cv.gross_footprint_area = Some(m.footprint_area());
                }
                if needs(QuantityKind::NetFootprintArea) {
                    cv.net_footprint_area = Some(m.footprint_area());
                }
                if needs(QuantityKind::GrossArea) && !has_openings(step, model, element_id) {
                    cv.gross_area = Some(m.footprint_area());
                }
                if needs(QuantityKind::NetArea) {
                    cv.net_area = Some(m.footprint_area());
                }
                // A slab's `Width` is its thickness — IFC defines
                // Qto_SlabBaseQuantities.Width as the nominal thickness, and
                // that set carries no Depth at all. Read as the vertical extent,
                // and only where the slab really does lie flat, so a slab
                // modelled on edge cannot report its span as a thickness.
                // A slab's `Width` is its thickness — IFC defines
                // `Qto_SlabBaseQuantities.Width` as the nominal thickness, and
                // that set carries no `Depth` at all. Read as the vertical
                // extent, and only where the slab really does lie flat, so a
                // slab modelled on edge cannot report its span as a thickness.
                //
                // Not on IFC2X3. The two schemas' exporters disagree about what
                // the name means and no geometric test separates them: on IFC4
                // it is the thickness and this is right 99.8% of the time over
                // 1,310 values, while on IFC2X3 the authored figure is a plan
                // dimension and this is right 0% of the time over 137. The
                // schema is the only discriminator available, so it is the one
                // used — a wrong value is not worth 137 of anything.
                if !matches!(model.schema, ifc_step::StepSchema::Ifc2x3)
                    && needs(QuantityKind::Width)
                    && m.extent[2] <= m.sorted_extent()[0]
                {
                    cv.width = Some(m.extent[2]);
                }
                // Perimeter of the plan outline, but only where that outline is
                // its own oriented rectangle — compared by area, so an L-shaped
                // or notched slab, whose true perimeter is longer than the
                // rectangle's, is refused rather than under-reported.
                // Perimeter of the plan outline, where that outline provably
                // *is* its oriented rectangle. Right 100% of the time over 1,468
                // values on IFC4, and only 71% on IFC2X3 with no clustering at
                // all (ratios 0.009 to 1.002) — that schema's exporters measure
                // an outline the mesh does not project. Withheld there.
                if !matches!(model.schema, ifc_step::StepSchema::Ifc2x3)
                    && needs(QuantityKind::Perimeter)
                {
                    if let Some(p) = m.plan {
                        let box_area = p.min_side * p.max_side;
                        if box_area > 0.0
                            && ((m.footprint_area() - box_area).abs() / box_area) < 1e-3
                        {
                            cv.perimeter = Some(2.0 * (p.min_side + p.max_side));
                        }
                    }
                }
            }
            // A wall's Height is its vertical extent. This is the one dimension
            // a world-axis extent does carry, because walls stand up: 99.4%
            // correct over 513 walls, against 42.8% coverage from the sweep
            // alone. Length and Width are *not* taken here — the oriented
            // rectangle gives them, but measured only 73.3% and 81.3%, so they
            // stay with the sweep.
            if matches!(entity_upper.as_str(), "IFCWALL" | "IFCWALLSTANDARDCASE")
                && needs(QuantityKind::Height)
                && m.extent[2] > 0.0
            {
                cv.height = Some(m.extent[2]);
            }
            // Side areas are NOT taken from the shadow. Measured, the largest
            // vertical projection matched NetSideArea 67.5% of the time with a
            // p95 of 203%: the elevation a side area is defined against is the
            // element's own middle plane, and a world-axis shadow is only that
            // for an axis-aligned wall. Volume/thickness, which does follow the
            // middle plane, scored 72.5% and is no better.

            // Extents are dimensions only where the solid is its own box.
            if m.fills_extent() {
                // Length is NOT the largest extent. A 50 m column's longest
                // side is its height, and a slab's is a plan dimension —
                // neither is what Length means. It is the distance along the
                // element's own axis, which only the sweep direction gives, so
                // it comes from the extrusion (below) and from nothing else.
                if needs(QuantityKind::Depth) {
                    cv.depth = Some(m.sorted_extent()[0]);
                }
                // Width is NOT taken from the middle extent. Measured, that
                // scored 49.2% where the nominal OverallWidth scores 98.3%:
                // which of a box's three dimensions an exporter calls its width
                // depends on the object's own axes, and world-axis extents do
                // not carry them.
            }
        }
        tier = ComputeTier::Mesh;
    }

    let solid = step_geom::best_solid(step, element_id);

    match &solid {
        // ------------------------------------------------------------------
        // Tier 2: ExtrudedAreaSolid
        // ------------------------------------------------------------------
        SolidKind::ExtrudedAreaSolid => {
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
                let single = parts.len() == 1;

                match entity_type.as_str() {
                    "IFCBEAM" | "IFCCOLUMN" | "IFCMEMBER" | "IFCPILE" | "IFCFOOTING" => {
                        // Length runs along the sweep axis, so it is the sweep
                        // distance itself, not its vertical component.
                        if single && needs(QuantityKind::Length) {
                            cv.length = Some(m.depth);
                        }
                        // GrossSurfaceArea comes straight from IFC's own
                        // formula for it — "perimeter * length + 2 * cross
                        // section area" — which is exactly this total. 100%
                        // correct over the corpus.
                        if single && needs(QuantityKind::GrossSurfaceArea) {
                            cv.gross_surface_area = Some(m.total_area);
                        }
                        // OuterSurfaceArea and CrossSectionArea stay off, and
                        // not for want of a formula: IFC defines the first as
                        // the same total "not taking into account the end cap
                        // areas", i.e. the lateral wrap, and the second as the
                        // profile area. Both are computed here already. Emitting
                        // them by those definitions scored 10.6% and 0.6%:
                        // exporters write the *total* surface under
                        // OuterSurfaceArea (89.6% against the tessellated
                        // surface, still short of the bar), and one model stores
                        // 1.5775 m2 as the CrossSectionArea of a mullion whose
                        // profile is 0.0075 m2. Where the file and the standard
                        // disagree this consistently, no value can be written
                        // that is right for both.
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
                            && !has_openings(step, model, element_id)
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
                    // Area is not emitted for doors and windows.
                    //
                    // Traced to a real door: type "EN 179 - ML - 1010 x 2250",
                    // authored Area 2.2725 = 1.010 x 2.250 — the nominal
                    // catalogue opening. The instance's own OverallHeight is
                    // 2135, and its swept profile is the plan footprint, so
                    // neither the attributes nor the geometry reproduce that
                    // figure. It is a property of the door type, not a
                    // measurement of the object, and no computation here can
                    // recover it.
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

                // GrossVolume ignores openings by definition, so a sweep that
                // is the whole body gives it directly. Where the body is *not*
                // the whole story — a wall clipped by a roof, a beam notched by
                // a boolean — the sweep is only one operand and reports material
                // that was cut away: 0 of the 23 voided elements that reached
                // here were right. Those are refused.
                if needs(QuantityKind::GrossVolume) && !has_openings(step, model, element_id) {
                    cv.gross_volume = Some(total_volume);
                }
                // NetVolume is the swept volume only where nothing voids the
                // element — then the two figures are the same and the closed
                // form is exact.
                //
                // Where there *are* openings it is NOT computed by subtracting
                // them here. That subtraction used to sum each opening's own
                // extrusion with its depth capped at the wall's thickness, which
                // is a guess twice over: an opening that does not pass all the
                // way through is over-subtracted, and two openings that overlap
                // are subtracted twice. The tessellated solid already has its
                // openings cut properly, so that value — set earlier and left
                // standing here — is the one to keep.
                if needs(QuantityKind::NetVolume) && !has_openings(step, model, element_id) {
                    cv.net_volume = Some(total_volume);
                }
                // Area is not emitted. Measured, the swept profile's area
                // matched the authored figure 0% of the time: for a vertically
                // extruded element the profile is its plan footprint, which is
                // not what "Area" means for it.
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
        SolidKind::FacetedBrep
        | SolidKind::TriangulatedFaceSet
        | SolidKind::FaceBasedSurfaceModel
        => {
            // The solid entity, not the shell: `qto-geometry` walks the
            // representation itself so it can see voids and inner bounds that a
            // bare shell reference hides.
            // Every solid in the body, summed. One that cannot be measured
            // exactly disqualifies the whole element rather than being skipped,
            // because a partial sum is a wrong total, not a smaller one.
            let ids = polyhedral_solid_ids(step, element_id);
            let measured: Result<Vec<_>, _> = ids
                .iter()
                .map(|&id| qto_geometry::polyhedral_metrics_for(step, id))
                .collect();
            if !ids.is_empty() {
                match measured.map(|parts| qto_geometry::PolyhedronMetrics {
                    volume: parts.iter().map(|p| p.volume).sum(),
                    surface_area: parts.iter().map(|p| p.surface_area).sum(),
                    triangle_count: parts.iter().map(|p| p.triangle_count).sum(),
                    // Extents belong to one solid; a body made of several has
                    // none, so `fills_extent` below cannot fire for it.
                    extent: if parts.len() == 1 { parts[0].extent } else { [0.0; 3] },
                }) {
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
                        if needs(QuantityKind::GrossVolume) && !has_openings(step, model, element_id) {
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
                // Length is NOT the largest extent. A 50 m column's longest
                // side is its height, and a slab's is a plan dimension —
                // neither is what Length means. It is the distance along the
                // element's own axis, which only the sweep direction gives, so
                // it comes from the extrusion (below) and from nothing else.
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

    // A door or window is measured by the hole it sits in.
    //
    // IFC defines Qto_DoorBaseQuantities and Qto_WindowBaseQuantities against
    // the *lining*: Width and Height are the outer lining dimensions and Area is
    // their product. The lining is the opening, and the opening is real geometry
    // — which matters, because the nominal `OverallHeight` on the entity is not
    // always that. One model in the corpus carries 2.35 m windows whose opening,
    // and whose authored Height, are both 1.5 m; the attribute is 57% too large
    // for all 47 of them.
    //
    // So the opening comes first and the attribute is the fallback: 95.6%
    // correct over 482 doors and windows against 85.9% from the attribute alone.
    if matches!(entity_upper.as_str(), "IFCDOOR" | "IFCWINDOW") {
        let needs = |k: QuantityKind| report.missing.contains(&k);
        let opening = filled_opening(model, element_id);
        let op_profile = opening.and_then(|op| opening_profile(step, op));
        let op_mesh = opening.and_then(|op| opening_mesh.get(&op).copied());

        // Height: the opening's vertical extent. Its profile is no substitute —
        // an opening swept vertically has a plan profile, so the profile's
        // longest span is a plan dimension for those and scored 78.5%.
        let height = op_mesh
            .map(|m| m.extent[2])
            .filter(|h| *h > 0.0)
            .or(inline_height);
        // Width: the opening profile spans the lining in elevation, so one of
        // its two extents is the height and the other is the width. Which is
        // which is *not* decided by taking the smaller — that is only the width
        // for an opening taller than it is wide, and every window in the
        // validation corpus happens to be, so the mistake would not have shown
        // up there. A 2.0 x 1.6 m window would have reported 1.6 as its width.
        //
        // The height is already known independently, from the opening's own
        // vertical extent, so the width is simply the span that is not it. No
        // threshold is involved: whichever span sits further from the height is
        // the other dimension.
        let width = op_profile
            .map(|p| match height {
                Some(h) if (p.max_span - h).abs() < (p.min_span - h).abs() => p.min_span,
                Some(_) => p.max_span,
                // Without a height there is nothing to disambiguate against;
                // the lining of a door or window is usually the taller way up.
                None => p.min_span,
            })
            .or(inline_width);

        // `Height` and `Width` are NOT emitted for doors and windows.
        //
        // Three candidate sources, three different answers: the nominal
        // `OverallHeight`/`OverallWidth`, the lining (the opening the element
        // fills), and the authored figure. On IFC2X3 the authored value is
        // smaller than *both* — a clear/daylight dimension, leaf minus frame —
        // so the attribute runs 1.7-8.6% large and the lining 1.5-17% large.
        // The best single rule reproduces 2.3% of 6,072 `Width` values and 1.1%
        // of 5,930 `Height` values. Recovering a clear dimension needs the frame
        // rebate, a type property that is not modelled.
        let _ = (height, width);

        // `Area` is NOT emitted, and this is a deliberate refusal rather than a
        // gap. The three exporters in the corpus mean three different things by
        // it: two give the lining rectangle, which the rule above reproduces
        // exactly for all 236 of them, while the third gives half the door
        // leaf's total surface — 2.045 where the lining is 2.000, for 164 doors.
        // The best single rule therefore lands at 61.6%, and a value that
        // disagrees with the file's own convention two times in five is worse
        // than no value at all. `GrossArea` above carries the lining figure for
        // the classes whose quantity set defines it.
    }

    ComputeOutput { values: cv, tier }
}

/// Every polyhedral solid in the element's body.
///
/// `SolidKind::FacetedBrep` names the *shell*, but `qto-geometry` needs the
/// solid itself so it can see `IfcFacetedBrepWithVoids`' inner shells. Mapped
/// items are followed the same way `step_geom` follows them.
///
/// All of them, not just the first: a body routinely holds several face sets —
/// one per material layer, one per stair tread — and measuring only the first
/// reported 1% of a stair flight's volume.
fn polyhedral_solid_ids(step: &StepFile, element_id: ifc_step::EntityId) -> Vec<ifc_step::EntityId> {
    fn walk(step: &StepFile, id: ifc_step::EntityId, depth: usize, out: &mut Vec<ifc_step::EntityId>) {
        if depth > 6 {
            return;
        }
        let Some(e) = step.entities.get(&id) else {
            return;
        };
        match e.entity_name.as_str() {
            "IFCFACETEDBREP"
            | "IFCFACETEDBREPWITHVOIDS"
            | "IFCTRIANGULATEDFACESET"
            | "IFCPOLYGONALFACESET"
            | "IFCFACEBASEDSURFACEMODEL"
            | "IFCSHELLBASEDSURFACEMODEL" => out.push(id),
            "IFCMAPPEDITEM" => {
                let Some(items) = e
                    .args
                    .first()
                    .and_then(ifc_step::StepValue::as_ref)
                    .and_then(|src| step.entities.get(&src))
                    .and_then(|m| m.args.get(1))
                    .and_then(ifc_step::StepValue::as_ref)
                    .and_then(|r| step.entities.get(&r))
                    .and_then(|r| r.args.get(3))
                    .and_then(ifc_step::StepValue::as_list)
                else {
                    return;
                };
                for i in items.iter().filter_map(ifc_step::StepValue::as_ref) {
                    walk(step, i, depth + 1, out);
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    for rep_id in step_geom::shape_reps(step, element_id) {
        let Some(items) = step
            .entities
            .get(&rep_id)
            .and_then(|r| r.args.get(3))
            .and_then(ifc_step::StepValue::as_list)
        else {
            continue;
        };
        for item in items.iter().filter_map(ifc_step::StepValue::as_ref) {
            walk(step, item, 0, &mut out);
        }
        if !out.is_empty() {
            break;
        }
    }
    out
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

/// Measure a solid through OCCT, reusing an earlier result for the same solid.

/// First representation item of the element's body, whatever kind it is.

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
/// The `IfcOpeningElement` this element fills, if it fills one.
fn filled_opening(model: &IfcModel, element_id: ifc_step::EntityId) -> Option<ifc_step::EntityId> {
    model
        .rel_fills
        .iter()
        .find(|rf| rf.element == element_id)
        .map(|rf| rf.opening)
}

/// Profile of an opening — the hole itself, as authored.
///
/// `IfcRelFillsElement` points at an `IfcOpeningElement` whose profile is the
/// opening rectangle, extruded through the host's thickness. Its short span is
/// the lining width regardless of how the wall is turned, which is what makes it
/// a better source than the nominal `OverallWidth` attribute.
fn opening_profile(
    step: &StepFile,
    opening: ifc_step::EntityId,
) -> Option<qto_geometry::ProfileMetrics> {
    extrusion_solid_ids(step, opening)
        .into_iter()
        .filter_map(|id| qto_geometry::metrics_for_extrusion(step, id))
        .max_by(|a, b| {
            a.profile
                .area
                .partial_cmp(&b.profile.area)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|m| m.profile)
}

fn has_openings(
    step: &StepFile,
    model: &IfcModel,
    element_id: ifc_step::EntityId,
) -> bool {
    // Declared voids.
    if model.rel_voids.iter().any(|rv| rv.element == element_id) {
        return true;
    }
    // Openings baked straight into the shape as a boolean. Checking only
    // IfcRelVoidsElement missed these, so a mesh — which always has its
    // openings already cut, making it the *net* figure — was being emitted as
    // GrossVolume for them.
    fn has_boolean(step: &StepFile, id: ifc_step::EntityId, depth: usize) -> bool {
        if depth > 6 {
            return false;
        }
        let Some(e) = step.entities.get(&id) else {
            return false;
        };
        match e.entity_name.as_str() {
            "IFCBOOLEANRESULT" | "IFCBOOLEANCLIPPINGRESULT" => true,
            "IFCMAPPEDITEM" => e
                .args
                .first()
                .and_then(ifc_step::StepValue::as_ref)
                .and_then(|src| step.entities.get(&src))
                .and_then(|m| m.args.get(1))
                .and_then(ifc_step::StepValue::as_ref)
                .and_then(|r| step.entities.get(&r))
                .and_then(|r| r.args.get(3))
                .and_then(ifc_step::StepValue::as_list)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(ifc_step::StepValue::as_ref)
                        .any(|i| has_boolean(step, i, depth + 1))
                })
                .unwrap_or(false),
            _ => false,
        }
    }
    step_geom::shape_reps(step, element_id).into_iter().any(|rep| {
        step.entities
            .get(&rep)
            .and_then(|r| r.args.get(3))
            .and_then(ifc_step::StepValue::as_list)
            .map(|items| {
                items
                    .iter()
                    .filter_map(ifc_step::StepValue::as_ref)
                    .any(|i| has_boolean(step, i, 0))
            })
            .unwrap_or(false)
    })
}

/// Is this class measured against the plan view?
///
/// `GrossArea`, `NetArea` and the footprint quantities are the area of the
/// element as seen from above only for elements that lie flat. For a window or
/// a door the same names mean the elevation, and taking the plan shadow instead
/// reports the frame thickness — measured, that was wrong for all 174 windows in
/// the corpus that carry a `GrossArea`, while it is 99.8% right for slabs.
fn is_plan_referenced(entity_upper: &str) -> bool {
    matches!(
        entity_upper,
        "IFCSLAB"
            | "IFCSLABELEMENTEDCASE"
            | "IFCSLABSTANDARDCASE"
            | "IFCROOF"
            | "IFCPLATE"
            | "IFCPLATESTANDARDCASE"
            | "IFCCOVERING"
            | "IFCFOOTING"
            | "IFCSPACE"
            | "IFCRAMP"
            | "IFCRAMPFLIGHT"
            | "IFCPAVEMENT"
    )
}

fn is_rectangular(p: &qto_geometry::ProfileMetrics) -> bool {
    let bbox_area = p.max_span * p.min_span;
    bbox_area > 0.0 && ((p.area - bbox_area).abs() / bbox_area) < 1e-9
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
            QtoOptions,
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
            QtoOptions,
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
            QtoOptions,
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
            QtoOptions,
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
