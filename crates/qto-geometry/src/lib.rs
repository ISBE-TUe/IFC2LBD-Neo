//! Exact quantity computation from IFC geometry.
//!
//! Two paths, chosen by what the representation is rather than by convenience:
//!
//! * **Polyhedral** — `IfcFacetedBrep`, `IfcTriangulatedFaceSet`,
//!   `IfcPolygonalFaceSet`, `IfcFaceBasedSurfaceModel`. Volume and area are
//!   *exactly* computable by the divergence theorem; no kernel is needed or
//!   would help. See [`polyhedron`].
//! * **Curved, swept and boolean** — extrusions, revolutions, sweeps, CSG.
//!   These need a real B-rep kernel to be exact, because tessellation can never
//!   reproduce `πr²h`. Handled via OCCT (not yet wired up here).
//!
//! Everything in this crate refuses rather than approximates: a computation
//! that cannot be exact returns `Err`, and the caller emits nothing.

#[cfg(feature = "occt")]
pub mod occt;
#[cfg(feature = "occt")]
pub mod occt_build;

pub mod curve;
pub mod extrusion;
pub mod ifc_faces;
pub mod polyhedron;
pub mod profile;

pub use extrusion::{metrics_for_extrusion, ExtrusionMetrics};
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
