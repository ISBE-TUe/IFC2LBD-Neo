//! Approximate bounding box computation from IFC STEP data.
//!
//! Provides fast (but approximate) bounding box extraction by walking
//! the STEP data structure — no mesh extraction or OCC dependency needed.
//! Used by the WASM bbox enricher to compute `geometry_bounding_boxes`
//! for topology enrichment.

use std::collections::HashMap;

use ifc_model::IfcModel;
use ifc_step::{EntityId, StepFile, StepValue};
use lbd_geometry::BoundingBox;

/// Compute approximate bounding boxes for all elements in the model.
///
/// Returns a map from EntityId to BoundingBox. Elements without
/// placement or representation data are skipped.
pub fn compute_approximate_bboxes(
    step: &StepFile,
    model: &IfcModel,
) -> HashMap<EntityId, BoundingBox> {
    let mut result = HashMap::new();
    for &entity_id in model.elements.keys() {
        if let Some([x_min, y_min, z_min, x_max, y_max, z_max]) = approximate_bbox(step, entity_id)
        {
            result.insert(
                entity_id,
                BoundingBox {
                    x_min,
                    x_max,
                    y_min,
                    y_max,
                    z_min,
                    z_max,
                },
            );
        }
    }
    result
}

/// Compute an approximate bounding box for a single IFC element.
///
/// Walks the STEP data structure to find placement and coordinate points.
/// Ignores rotation — only accumulates translation offsets. Good enough
/// for spatial pre-filtering (adjacency detection from bounding boxes).
pub fn approximate_bbox(step: &StepFile, element_id: EntityId) -> Option<[f64; 6]> {
    let entity = step.entities.get(&element_id)?;
    // ObjectPlacement is args[5] for most IfcProduct subtypes.
    let placement_id = match entity.args.get(5) {
        Some(StepValue::Ref(id)) => *id,
        _ => return None,
    };
    let world_translate = placement_translation(step, placement_id);

    // Collect all 3D coordinate values from the representation items.
    let rep_id = match entity.args.get(6) {
        Some(StepValue::Ref(id)) => *id,
        _ => {
            // No representation — use placement origin as a point bbox.
            let [x, y, z] = world_translate;
            return Some([x, y, z, x, y, z]);
        }
    };
    let mut pts: Vec<[f64; 3]> = Vec::new();
    collect_points(step, rep_id, &mut pts, 0, 300);
    // Elements with > 300 coordinate points have complex/freeform geometry.
    // Skip them for topology analysis.
    if pts.len() >= 300 {
        return None;
    }
    if pts.is_empty() {
        let [x, y, z] = world_translate;
        return Some([x, y, z, x, y, z]);
    }
    // Apply the world translation to all collected points.
    let [tx, ty, tz] = world_translate;
    let mut min = [f64::MAX; 3];
    let mut max = [f64::MIN; 3];
    for [x, y, z] in &pts {
        let wx = x + tx;
        let wy = y + ty;
        let wz = z + tz;
        min[0] = min[0].min(wx);
        min[1] = min[1].min(wy);
        min[2] = min[2].min(wz);
        max[0] = max[0].max(wx);
        max[1] = max[1].max(wy);
        max[2] = max[2].max(wz);
    }
    Some([min[0], min[1], min[2], max[0], max[1], max[2]])
}

/// Walk a placement chain and return the accumulated translation.
/// Ignores rotation for speed — sufficient for spatial pre-filtering.
fn placement_translation(step: &StepFile, placement_id: EntityId) -> [f64; 3] {
    let mut tx = 0.0f64;
    let mut ty = 0.0f64;
    let mut tz = 0.0f64;
    let mut current_id = placement_id;
    let mut depth = 0;
    loop {
        if depth > 20 {
            break;
        }
        depth += 1;
        let Some(entity) = step.entities.get(&current_id) else {
            break;
        };
        match entity.entity_name.as_str() {
            "IFCLOCALPLACEMENT" => {
                let rel_id = match entity.args.get(1) {
                    Some(StepValue::Ref(id)) => *id,
                    _ => break,
                };
                let [lx, ly, lz] = axis2placement3d_origin(step, rel_id);
                tx += lx;
                ty += ly;
                tz += lz;
                match entity.args.first() {
                    Some(StepValue::Ref(parent_id)) => {
                        current_id = *parent_id;
                    }
                    _ => break,
                }
            }
            _ => break,
        }
    }
    [tx, ty, tz]
}

fn axis2placement3d_origin(step: &StepFile, id: EntityId) -> [f64; 3] {
    let Some(entity) = step.entities.get(&id) else {
        return [0.0, 0.0, 0.0];
    };
    if entity.entity_name != "IFCAXIS2PLACEMENT3D" {
        return [0.0, 0.0, 0.0];
    }
    let loc_id = match entity.args.first() {
        Some(StepValue::Ref(id)) => *id,
        _ => return [0.0, 0.0, 0.0],
    };
    cartesian_point_3d(step, loc_id)
}

fn cartesian_point_3d(step: &StepFile, id: EntityId) -> [f64; 3] {
    let Some(entity) = step.entities.get(&id) else {
        return [0.0, 0.0, 0.0];
    };
    if entity.entity_name != "IFCCARTESIANPOINT" {
        return [0.0, 0.0, 0.0];
    }
    let coords = match entity.args.first() {
        Some(StepValue::List(list)) => list,
        _ => return [0.0, 0.0, 0.0],
    };
    let x = coords.first().and_then(|v| v.as_real()).unwrap_or(0.0);
    let y = coords.get(1).and_then(|v| v.as_real()).unwrap_or(0.0);
    let z = coords.get(2).and_then(|v| v.as_real()).unwrap_or(0.0);
    [x, y, z]
}

/// Recursively collect 3D coordinate values from an IFC entity tree.
fn collect_points(
    step: &StepFile,
    id: EntityId,
    pts: &mut Vec<[f64; 3]>,
    depth: usize,
    max: usize,
) {
    if depth > 10 || pts.len() >= max {
        return;
    }
    let Some(entity) = step.entities.get(&id) else {
        return;
    };
    match entity.entity_name.as_str() {
        "IFCCARTESIANPOINT" => {
            if let Some(StepValue::List(coords)) = entity.args.first() {
                if coords.len() >= 3 {
                    let x = coords[0].as_real().unwrap_or(0.0);
                    let y = coords[1].as_real().unwrap_or(0.0);
                    let z = coords[2].as_real().unwrap_or(0.0);
                    pts.push([x, y, z]);
                }
            }
        }
        "IFCCARTESIANPOINTLIST3D" => {
            if let Some(StepValue::List(list)) = entity.args.first() {
                for item in list {
                    if let StepValue::List(coords) = item {
                        if coords.len() >= 3 {
                            let x = coords[0].as_real().unwrap_or(0.0);
                            let y = coords[1].as_real().unwrap_or(0.0);
                            let z = coords[2].as_real().unwrap_or(0.0);
                            pts.push([x, y, z]);
                        }
                    }
                }
            }
        }
        _ => {
            for arg in &entity.args {
                match arg {
                    StepValue::Ref(child_id) => {
                        collect_points(step, *child_id, pts, depth + 1, max);
                    }
                    StepValue::List(list) => {
                        for item in list {
                            if pts.len() >= max {
                                return;
                            }
                            if let StepValue::Ref(child_id) = item {
                                collect_points(step, *child_id, pts, depth + 1, max);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
