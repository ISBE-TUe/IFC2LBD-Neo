/// Tier 1: axis-aligned bounding box from raw STEP vertex coordinates.
///
/// Collects all IFCCARTESIANPOINT entities reachable from a shape representation
/// and computes the AABB extents. Fast, WASM-safe, works for any geometry type.

use ifc_step::{EntityId, StepFile};

use crate::step_geom::{collect_points, shape_reps};

#[derive(Debug, Clone, Copy)]
pub struct BBoxResult {
    /// X extent (typically the longer horizontal dimension).
    pub x_dim: f64,
    /// Y extent.
    pub y_dim: f64,
    /// Z extent (typically the vertical / height dimension).
    pub z_dim: f64,
}


/// Compute the AABB from all vertices reachable from `element_id`.
///
/// Returns `None` if no cartesian points are found (element has no geometry).
pub fn compute(step: &StepFile, element_id: EntityId) -> Option<BBoxResult> {
    let reps = shape_reps(step, element_id);
    if reps.is_empty() {
        return None;
    }

    let mut x_min = f64::MAX;
    let mut x_max = f64::MIN;
    let mut y_min = f64::MAX;
    let mut y_max = f64::MIN;
    let mut z_min = f64::MAX;
    let mut z_max = f64::MIN;
    let mut found_any = false;

    for rep_id in reps {
        // Depth limit of 12 is enough for typical IFC geometry nesting.
        for [x, y, z] in collect_points(step, rep_id, 12) {
            found_any = true;
            if x < x_min { x_min = x; }
            if x > x_max { x_max = x; }
            if y < y_min { y_min = y; }
            if y > y_max { y_max = y; }
            if z < z_min { z_min = z; }
            if z > z_max { z_max = z; }
        }
    }

    if !found_any {
        return None;
    }

    Some(BBoxResult {
        x_dim: (x_max - x_min).abs(),
        y_dim: (y_max - y_min).abs(),
        z_dim: (z_max - z_min).abs(),
    })
}
