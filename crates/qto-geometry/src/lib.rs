//! Exact quantity computation from IFC geometry.
//!
//! Two paths, chosen by what the representation is rather than by convenience:
//!
//! * **Polyhedral** — `IfcFacetedBrep`, `IfcTriangulatedFaceSet`,
//!   `IfcPolygonalFaceSet`, `IfcFaceBasedSurfaceModel`. Volume and area are
//!   *exactly* computable by the divergence theorem; no kernel is needed or
//!   would help. See [`polyhedron`].
//! * **Analytic** — an `IfcExtrudedAreaSolid`'s volume is `profile area x
//!   perpendicular sweep`, in closed form, from a profile whose own area is
//!   closed form. Nothing is tessellated and nothing is chorded: a profile
//!   containing an arc is refused rather than approximated by its chords.
//!
//! Curved, boolean and swept bodies that neither path can measure exactly are
//! handled by the caller, which tessellates them and applies an enclosure bound
//! to the result.
//!
//! Everything in this crate refuses rather than approximates: a computation
//! that cannot be exact returns `Err`, and the caller emits nothing.

pub mod curve;
pub mod extrusion;
pub mod ifc_faces;
pub mod plan_obb;
pub mod polyhedron;
pub mod profile;

pub use extrusion::{metrics_for_extrusion, ExtrusionMetrics};
pub use plan_obb::{min_area_rect, PlanObb};
pub use ifc_faces::faces_for_solid;
pub use profile::{metrics as profile_metrics, ProfileMetrics};
pub use polyhedron::{metrics as polyhedron_metrics, Face, PolyhedronError, PolyhedronMetrics};

/// Exact metrics for a polyhedral IFC solid, or `Err` if it is not one or
/// cannot be measured exactly.
pub fn polyhedral_metrics_for(
    step: &ifc_step::StepFile,
    solid_id: ifc_step::EntityId,
) -> Result<PolyhedronMetrics, PolyhedronError> {
    let faces = faces_for_solid(step, solid_id).ok_or(PolyhedronError::NotEnoughFaces)?;
    polyhedron_metrics(&faces)
}
