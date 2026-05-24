/// Helpers for walking the STEP entity graph to find geometry representations.
///
/// All IFC products store their geometry at args[6] (0-indexed) as a reference
/// to an IFCPRODUCTREPRESENTATION entity. From there we navigate to shape
/// representations and their geometry items.

use ifc_step::{EntityId, RawEntity, StepFile, StepValue};

#[derive(Debug, Clone)]
pub enum SolidKind {
    /// IFCEXTRUDEDAREASOLID — profile swept along an axis.
    ExtrudedAreaSolid { profile_id: EntityId, depth: f64 },
    /// IFCBOUNDINGBOX — explicit axis-aligned box in local coords.
    BoundingBox { x_dim: f64, y_dim: f64, z_dim: f64 },
    /// IFCFACETEDBREP — closed shell of planar faces.
    FacetedBrep { shell_id: EntityId },
    /// IFCTRIANGULATEDFACESET — pre-tessellated mesh (IFC4).
    /// `faceset_id` is the entity containing both Coordinates and CoordIndex.
    TriangulatedFaceSet { faceset_id: EntityId },
    Unknown,
}

/// Return all IFCSHAPEREPRESENTATION IDs for the element at `element_id`.
pub fn shape_reps(step: &StepFile, element_id: EntityId) -> Vec<EntityId> {
    let entity = match step.entities.get(&element_id) {
        Some(e) => e,
        None => return vec![],
    };
    // Representation is at args[6] for all IfcProduct subtypes.
    let prod_rep_id = match entity.args.get(6).and_then(StepValue::as_ref) {
        Some(id) => id,
        None => return vec![],
    };
    let prod_rep = match step.entities.get(&prod_rep_id) {
        Some(e) => e,
        None => return vec![],
    };
    // IFCPRODUCTREPRESENTATION: args[2] = list of shape rep refs.
    let list = match prod_rep.args.get(2).and_then(StepValue::as_list) {
        Some(l) => l,
        None => return vec![],
    };
    list.iter().filter_map(StepValue::as_ref).collect()
}

/// Return the first recognisable solid from a shape representation's item list.
pub fn first_solid_in_rep(step: &StepFile, shape_rep_id: EntityId) -> SolidKind {
    let rep = match step.entities.get(&shape_rep_id) {
        Some(e) => e,
        None => return SolidKind::Unknown,
    };
    // IFCSHAPEREPRESENTATION: args[3] = item list.
    let items = match rep.args.get(3).and_then(StepValue::as_list) {
        Some(l) => l,
        None => return SolidKind::Unknown,
    };
    for item_val in items {
        let item_id = match item_val.as_ref() {
            Some(id) => id,
            None => continue,
        };
        if let Some(solid) = solid_from_entity(step, item_id) {
            return solid;
        }
    }
    SolidKind::Unknown
}

/// Find the best solid across all shape representations for an element.
///
/// Priority: ExtrudedAreaSolid > FacetedBrep / TriangulatedFaceSet > BoundingBox.
pub fn best_solid(step: &StepFile, element_id: EntityId) -> SolidKind {
    let reps = shape_reps(step, element_id);
    let mut bbox_fallback: Option<SolidKind> = None;

    for rep_id in reps {
        let solid = first_solid_in_rep(step, rep_id);
        match &solid {
            SolidKind::ExtrudedAreaSolid { .. }
            | SolidKind::FacetedBrep { .. }
            | SolidKind::TriangulatedFaceSet { .. } => return solid,
            SolidKind::BoundingBox { .. } => {
                if bbox_fallback.is_none() {
                    bbox_fallback = Some(solid);
                }
            }
            SolidKind::Unknown => {}
        }
    }
    bbox_fallback.unwrap_or(SolidKind::Unknown)
}

fn solid_from_entity(step: &StepFile, entity_id: EntityId) -> Option<SolidKind> {
    let e = step.entities.get(&entity_id)?;
    match e.entity_name.as_str() {
        "IFCEXTRUDEDAREASOLID" => {
            let profile_id = e.args.get(0)?.as_ref()?;
            let depth = real_from_step(e.args.get(3)?)?;
            Some(SolidKind::ExtrudedAreaSolid { profile_id, depth })
        }
        "IFCBOUNDINGBOX" => {
            let x_dim = real_from_step(e.args.get(1)?)?;
            let y_dim = real_from_step(e.args.get(2)?)?;
            let z_dim = real_from_step(e.args.get(3)?)?;
            Some(SolidKind::BoundingBox { x_dim, y_dim, z_dim })
        }
        "IFCFACETEDBREP" => {
            let shell_id = e.args.get(0)?.as_ref()?;
            Some(SolidKind::FacetedBrep { shell_id })
        }
        "IFCTRIANGULATEDFACESET" => {
            Some(SolidKind::TriangulatedFaceSet { faceset_id: entity_id })
        }
        _ => None,
    }
}

/// Collect all IFCCARTESIANPOINT coordinates reachable under `root_id`.
///
/// Walks refs recursively up to `max_depth` levels to avoid runaway traversal.
pub fn collect_points(step: &StepFile, root_id: EntityId, max_depth: usize) -> Vec<[f64; 3]> {
    let mut out = Vec::new();
    collect_points_inner(step, root_id, max_depth, &mut out);
    out
}

fn collect_points_inner(
    step: &StepFile,
    entity_id: EntityId,
    depth: usize,
    out: &mut Vec<[f64; 3]>,
) {
    if depth == 0 {
        return;
    }
    let entity = match step.entities.get(&entity_id) {
        Some(e) => e,
        None => return,
    };
    if entity.entity_name == "IFCCARTESIANPOINT" {
        if let Some(coords) = parse_cartesian_point(entity) {
            out.push(coords);
            return; // don't recurse into the point's own args
        }
    }
    // IFC4 indexed point lists: points are embedded as nested coordinate tuples,
    // not as entity references, so the generic ref traversal misses them.
    if matches!(
        entity.entity_name.as_str(),
        "IFCCARTESIANPOINTLIST2D" | "IFCCARTESIANPOINTLIST3D"
    ) {
        if let Some(coord_lists) = entity.args.first().and_then(StepValue::as_list) {
            for coord_val in coord_lists {
                if let Some(coords) = coord_val.as_list() {
                    let x = coords.first().and_then(real_from_step).unwrap_or(0.0);
                    let y = coords.get(1).and_then(real_from_step).unwrap_or(0.0);
                    let z = coords.get(2).and_then(real_from_step).unwrap_or(0.0);
                    out.push([x, y, z]);
                }
            }
        }
        return;
    }
    for arg in &entity.args {
        visit_value(step, arg, depth - 1, out);
    }
}

fn visit_value(step: &StepFile, val: &StepValue, depth: usize, out: &mut Vec<[f64; 3]>) {
    match val {
        StepValue::Ref(id) => collect_points_inner(step, *id, depth, out),
        StepValue::List(items) => {
            for item in items {
                visit_value(step, item, depth, out);
            }
        }
        _ => {}
    }
}

/// Parse coordinates from an IFCCARTESIANPOINT entity.
pub fn parse_cartesian_point(e: &RawEntity) -> Option<[f64; 3]> {
    let coords = e.args.first()?.as_list()?;
    let x = real_from_step(coords.first()?).unwrap_or(0.0);
    let y = coords.get(1).and_then(real_from_step).unwrap_or(0.0);
    let z = coords.get(2).and_then(|v| real_from_step(v)).unwrap_or(0.0);
    Some([x, y, z])
}

/// Extract a real from a StepValue, unwrapping Typed wrappers like IFCLENGTHMEASURE.
pub fn real_from_step(val: &StepValue) -> Option<f64> {
    match val {
        StepValue::Real(v) => Some(*v),
        StepValue::Int(v) => Some(*v as f64),
        StepValue::Typed { value, .. } => real_from_step(value),
        _ => None,
    }
}
