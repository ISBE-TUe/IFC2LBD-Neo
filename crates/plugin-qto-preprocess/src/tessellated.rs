//! Quantities measured from tessellated geometry.
//!
//! This is what IfcOpenShell does, and it is why IfcOpenShell reproduces the
//! authored quantities on models where per-representation arithmetic does not:
//! it evaluates the element's geometry into triangles — whatever the
//! representation was — and measures the result.
//!
//! `ifc-lite` already does that evaluation here. Its 15 processors cover
//! extrusions, tapered extrusions, revolutions, sweeps, CSG primitives,
//! booleans, half-space clipping, breps, tessellated sets and mapped items, and
//! `ifc-geometry::stream_meshes` returns one mesh per element **with openings
//! already subtracted**. Measuring that mesh answers every representation type
//! with one code path instead of a special case per solid kind.

use std::collections::HashMap;
use std::sync::Arc;

use qto_geometry::polyhedron::{self, Face};

/// Volume, areas and extents for one element, from its tessellated geometry.
///
/// Every figure is in world axes: `stream_meshes` returns each instance with the
/// placement that puts it where it belongs in the model, and the measurements
/// below are taken after that transform.
#[derive(Debug, Clone, Copy, Default)]
pub struct MeshQuantities {
    /// Net volume: `stream_meshes` subtracts openings before tessellating.
    pub volume: f64,
    /// Total boundary area of the solid.
    pub surface_area: f64,
    /// Area of the solid's shadow on the plane normal to each world axis:
    /// `[YZ, XZ, XY]`. The last is the footprint.
    ///
    /// For a closed surface the projected area is half the summed magnitude of
    /// the triangles' area vectors along the axis, because every point of the
    /// shadow is covered exactly twice: once entering the solid, once leaving.
    pub shadow: [f64; 3],
    /// World-axis extents, in `x, y, z` order — *not* sorted, because which axis
    /// a dimension runs along is exactly what distinguishes Height from Length.
    pub extent: [f64; 3],
    /// How many separate mesh instances make up this element.
    ///
    /// Areas and volumes add across instances; a dimension does not, and a
    /// projected area double-counts wherever two instances overlap in that view.
    /// Callers that need either use this to withhold instead.
    pub parts: u32,
    /// Minimum-area enclosing rectangle of the plan projection.
    ///
    /// This is what makes a dimension readable from a mesh at all: a wall at 30
    /// degrees has world extents far larger than itself in both plan
    /// directions, while its oriented rectangle is its own thickness and run.
    pub plan: Option<qto_geometry::PlanObb>,
}

impl MeshQuantities {
    /// Shadow on the ground plane.
    pub fn footprint_area(&self) -> f64 {
        self.shadow[2]
    }

    /// Extents smallest-first.
    pub fn sorted_extent(&self) -> [f64; 3] {
        let mut e = self.extent;
        e.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        e
    }

    /// Does the solid fill its own bounding box? Only then are its extents its
    /// dimensions rather than an over-estimate.
    pub fn fills_extent(&self) -> bool {
        let b = self.extent[0] * self.extent[1] * self.extent[2];
        b > 0.0 && ((self.volume - b).abs() / b) < 1e-6
    }

    /// Can this volume be believed?
    ///
    /// The divergence theorem is exact for a closed, consistently oriented,
    /// non-self-intersecting shell. Tessellated output from long boolean chains
    /// is often none of those, and the resulting integral over-reports without
    /// any local sign of trouble — the ArchiCAD model in the corpus returned six
    /// times the true volume for its median element while its surface area and
    /// its shadows stayed exact.
    ///
    /// This is the bound that catches it: a solid lies inside the prism formed
    /// by its own shadow and its own extent along the same axis, so its volume
    /// cannot exceed that product on *any* axis. The test is a strict geometric
    /// consequence, not a tolerance — no correct volume can fail it — and it
    /// needs nothing the measurement does not already produce.
    pub fn is_volume_sound(&self) -> bool {
        // A hair of slack for the f32 vertex quantisation the mesh carries:
        // shadow and extent are computed from the same rounded coordinates, so
        // an exactly-prismatic solid can land a few ulps over its own bound.
        const SLACK: f64 = 1.0 + 1e-6;
        self.volume > 0.0
            && (0..3).all(|i| {
                let bound = self.shadow[i] * self.extent[i];
                bound > 0.0 && self.volume <= bound * SLACK
            })
    }

}

/// Tessellate the given elements and measure each one.
///
/// Keyed by STEP entity id. Elements whose geometry does not close are absent
/// rather than approximated — an open shell has no enclosed volume.
///
/// `metres_per_unit` is the model's `LENGTHUNIT` in metres. It is needed because
/// `stream_meshes` normalises its output to metres whatever the file declares —
/// it feeds a renderer — while every other measurement here is in the file's own
/// units. Without this the mesh figures for a millimetre model come out 10³,
/// 10⁶ and 10⁹ too small once the quantity-unit conversion is applied on top.
pub fn measure_all(
    content: Arc<String>,
    element_ids: &[u64],
    metres_per_unit: f64,
) -> HashMap<u64, MeshQuantities> {
    let mut out = HashMap::new();
    if element_ids.is_empty() || !(metres_per_unit.is_finite() && metres_per_unit > 0.0) {
        return out;
    }
    // Metres back into the file's own length unit.
    let k = 1.0 / metres_per_unit;
    let (k2, k3) = (k * k, k * k * k);

    for flat in ifc_geometry::stream_meshes(content, element_ids) {
        // An element's geometry may arrive as several instances; they are parts
        // of one object, so their volumes and areas add. The bounding box is the
        // union, which stays meaningful however many parts there are.
        let mut acc = MeshQuantities::default();
        let mut lo = [f64::MAX; 3];
        let mut hi = [f64::MIN; 3];
        // Plan points from every instance together: the oriented rectangle of
        // an element assembled from several solids is the rectangle around all
        // of them, not one per part.
        let mut plan_pts: Vec<[f64; 2]> = Vec::new();
        for inst in &flat.geometries {
            let Some((m, l, h)) = measure_mesh(&inst.mesh, &inst.world_transform, &mut plan_pts)
            else {
                continue;
            };
            acc.volume += m.volume;
            acc.surface_area += m.surface_area;
            for i in 0..3 {
                acc.shadow[i] += m.shadow[i];
                lo[i] = lo[i].min(l[i]);
                hi[i] = hi[i].max(h[i]);
            }
            acc.parts += 1;
        }
        if acc.parts > 0 && acc.volume > 0.0 {
            acc.extent = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
            acc.plan = qto_geometry::min_area_rect(&plan_pts);

            acc.volume *= k3;
            acc.surface_area *= k2;
            for i in 0..3 {
                acc.shadow[i] *= k2;
                acc.extent[i] *= k;
            }
            acc.plan = acc.plan.map(|p| qto_geometry::PlanObb {
                min_side: p.min_side * k,
                max_side: p.max_side * k,
                hull_area: p.hull_area * k2,
            });

            out.insert(flat.express_id, acc);
        }
    }
    out
}

/// Measure one tessellated mesh, in world space. Returns the metrics plus the
/// instance's own world-space bounding box.
///
/// The mesh stores positions as `f32` for rendering. Volume is accumulated in
/// `f64` after transforming, so the only loss is the vertex quantisation itself
/// — the same trade IfcOpenShell makes, and well inside the 0.1% the validation
/// harness checks against.
fn measure_mesh(
    mesh: &ifc_geometry::Mesh,
    world: &[f64; 16],
    plan_pts: &mut Vec<[f64; 2]>,
) -> Option<(MeshQuantities, [f64; 3], [f64; 3])> {
    if mesh.indices.len() < 12 || mesh.positions.len() < 12 {
        return None;
    }
    let xf = |i: usize| -> [f64; 3] {
        let (x, y, z) = (
            mesh.positions[i * 3] as f64,
            mesh.positions[i * 3 + 1] as f64,
            mesh.positions[i * 3 + 2] as f64,
        );
        // Column-major 4x4.
        [
            world[0] * x + world[4] * y + world[8] * z + world[12],
            world[1] * x + world[5] * y + world[9] * z + world[13],
            world[2] * x + world[6] * y + world[10] * z + world[14],
        ]
    };

    let faces: Vec<Face> = mesh
        .indices
        .chunks_exact(3)
        .filter_map(|t| {
            let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
            let n = mesh.positions.len() / 3;
            (a < n && b < n && c < n).then(|| Face {
                outer: vec![xf(a), xf(b), xf(c)],
                inner: vec![],
            })
        })
        .collect();

    // Reuses the exact polyhedral measurement, including its closure check: a
    // mesh whose edges do not pair up does not bound a volume, and no number is
    // produced for it.
    let base = polyhedron::metrics(&faces).ok()?;

    // Shadow areas and extents, from the same transformed triangles.
    let mut twice_shadow = [0.0f64; 3];
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for f in &faces {
        let (a, b, c) = (f.outer[0], f.outer[1], f.outer[2]);
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        // Triangle area vector: its components are twice the signed projected
        // areas on each plane.
        twice_shadow[0] += (u[1] * v[2] - u[2] * v[1]).abs();
        twice_shadow[1] += (u[2] * v[0] - u[0] * v[2]).abs();
        twice_shadow[2] += (u[0] * v[1] - u[1] * v[0]).abs();
        for p in [a, b, c] {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
            plan_pts.push([p[0], p[1]]);
        }
    }

    let m = MeshQuantities {
        volume: base.volume,
        surface_area: base.surface_area,
        // Halved once for the doubled area vector, again because a closed
        // surface covers its own shadow twice.
        shadow: [
            twice_shadow[0] / 4.0,
            twice_shadow[1] / 4.0,
            twice_shadow[2] / 4.0,
        ],
        extent: [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]],
        parts: 1,
        plan: None,
    };
    Some((m, lo, hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prism(volume: f64) -> MeshQuantities {
        // A 2 x 3 x 4 box: shadows are its three faces, extents its three sides.
        MeshQuantities {
            volume,
            surface_area: 2.0 * (2.0 * 3.0 + 3.0 * 4.0 + 2.0 * 4.0),
            shadow: [3.0 * 4.0, 2.0 * 4.0, 2.0 * 3.0],
            extent: [2.0, 3.0, 4.0],
            parts: 1,
            plan: None,
        }
    }

    #[test]
    fn a_solid_that_fills_its_own_prism_is_sound() {
        assert!(prism(24.0).is_volume_sound());
    }

    /// The failure this exists for: a shell from a boolean chain that integrates
    /// to more than the box it lives in. The corpus had elements at 6x.
    #[test]
    fn a_volume_larger_than_its_own_enclosure_is_refused() {
        assert!(!prism(24.0 * 1.001).is_volume_sound());
        assert!(!prism(24.0 * 6.0).is_volume_sound());
    }

    /// The bound only ever rules things out — a genuinely smaller solid inside
    /// the same envelope must still pass.
    #[test]
    fn a_smaller_solid_in_the_same_envelope_still_passes() {
        assert!(prism(1.0).is_volume_sound());
    }

    #[test]
    fn a_degenerate_solid_is_not_sound() {
        assert!(!prism(0.0).is_volume_sound());
        assert!(!MeshQuantities::default().is_volume_sound());
    }

    /// Height is the `z` extent and Length is not, so the extents must stay in
    /// world-axis order and only `sorted_extent` may reorder them.
    #[test]
    fn extents_keep_their_axes() {
        let m = MeshQuantities {
            extent: [5.0, 0.2, 3.0],
            ..prism(1.0)
        };
        assert_eq!(m.extent[2], 3.0, "z must stay z");
        assert_eq!(m.sorted_extent(), [0.2, 3.0, 5.0]);
    }
}
