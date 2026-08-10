//! Walking the STEP entity graph to find out what kind of body an element has.
//!
//! All IFC products carry their geometry at argument 6 as a reference to an
//! `IfcProductDefinitionShape`, and from there to shape representations and
//! their items.
//!
//! Only the *kind* is reported. The dimensions each solid carries are read by
//! `qto-geometry` from the entity itself, which understands the profile and the
//! sweep direction; a second, shallower reader here would be a second answer to
//! the same question and the two would drift apart.

use ifc_step::{EntityId, StepFile, StepValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolidKind {
    /// `IfcExtrudedAreaSolid` — a profile swept along an axis.
    ExtrudedAreaSolid,
    /// `IfcBoundingBox` — an explicit box, and nothing else about the shape.
    BoundingBox,
    /// A closed shell of planar faces: `IfcFacetedBrep` and friends.
    FacetedBrep,
    /// `IfcTriangulatedFaceSet` / `IfcPolygonalFaceSet` — pre-tessellated.
    TriangulatedFaceSet,
    /// `IfcFaceBasedSurfaceModel` — surface model, common in IFC2x3 exports.
    FaceBasedSurfaceModel,
    Unknown,
}

/// Every `IfcShapeRepresentation` id for the element at `element_id`.
pub fn shape_reps(step: &StepFile, element_id: EntityId) -> Vec<EntityId> {
    let Some(entity) = step.entities.get(&element_id) else {
        return vec![];
    };
    // Representation is at args[6] for all IfcProduct subtypes.
    let Some(prod_rep_id) = entity.args.get(6).and_then(StepValue::as_ref) else {
        return vec![];
    };
    let Some(prod_rep) = step.entities.get(&prod_rep_id) else {
        return vec![];
    };
    // IfcProductRepresentation: args[2] = list of shape representation refs.
    prod_rep
        .args
        .get(2)
        .and_then(StepValue::as_list)
        .map(|l| l.iter().filter_map(StepValue::as_ref).collect())
        .unwrap_or_default()
}

/// The best solid across all of an element's shape representations.
///
/// Priority: a sweep, then a polyhedron, then an explicit bounding box — the
/// order in which the body can be measured exactly.
pub fn best_solid(step: &StepFile, element_id: EntityId) -> SolidKind {
    let mut bbox_fallback = None;

    for rep_id in shape_reps(step, element_id) {
        let Some(rep) = step.entities.get(&rep_id) else {
            continue;
        };
        let Some(items) = rep.args.get(3).and_then(StepValue::as_list) else {
            continue;
        };
        for item in items.iter().filter_map(StepValue::as_ref) {
            match solid_from_entity(step, item, 0) {
                Some(SolidKind::BoundingBox) => bbox_fallback = Some(SolidKind::BoundingBox),
                Some(other) => return other,
                None => {}
            }
        }
    }
    bbox_fallback.unwrap_or(SolidKind::Unknown)
}

fn solid_from_entity(step: &StepFile, entity_id: EntityId, depth: usize) -> Option<SolidKind> {
    if depth > 6 {
        return None;
    }
    let e = step.entities.get(&entity_id)?;
    match e.entity_name.as_str() {
        "IFCEXTRUDEDAREASOLID" => Some(SolidKind::ExtrudedAreaSolid),
        "IFCBOUNDINGBOX" => Some(SolidKind::BoundingBox),
        "IFCFACETEDBREP" | "IFCFACETEDBREPWITHVOIDS" => Some(SolidKind::FacetedBrep),
        "IFCTRIANGULATEDFACESET" | "IFCPOLYGONALFACESET" => Some(SolidKind::TriangulatedFaceSet),
        "IFCFACEBASEDSURFACEMODEL" | "IFCSHELLBASEDSURFACEMODEL" => {
            Some(SolidKind::FaceBasedSurfaceModel)
        }
        // A boolean's first operand is the body before it was cut. Reporting
        // its kind is right — it says what the geometry is made of — but nothing
        // measures that operand directly, because the cut removed material from
        // it. Booleans reach the tessellated path instead.
        "IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT" => {
            solid_from_entity(step, e.args.get(1)?.as_ref()?, depth + 1)
        }
        // Mapped item: MappingSource → IfcRepresentationMap → MappedRepresentation.
        "IFCMAPPEDITEM" => {
            let map_source = step.entities.get(&e.args.first()?.as_ref()?)?;
            let mapped_rep = step.entities.get(&map_source.args.get(1)?.as_ref()?)?;
            mapped_rep
                .args
                .get(3)?
                .as_list()?
                .iter()
                .filter_map(StepValue::as_ref)
                .find_map(|item| solid_from_entity(step, item, depth + 1))
        }
        _ => None,
    }
}
