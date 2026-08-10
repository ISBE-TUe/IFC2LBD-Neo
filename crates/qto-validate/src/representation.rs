//! Classify how an element's geometry is represented in the STEP file.
//!
//! Coverage decisions are made per representation type, not per element: a
//! backend either can or cannot compute a given representation exactly. So every
//! number this harness reports is broken down on this axis.
//!
//! Classification is deliberately independent of `plugin-qto-preprocess`'s own
//! `step_geom::best_solid` — that walker is one of the things under test, and a
//! classifier sharing its blind spots would hide them.

use std::fmt;

use ifc_step::{EntityId, StepFile, StepValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RepresentationType {
    /// Swept solid over a parametric profile (rectangle, circle, I/L/T/U/Z…).
    ExtrudedAnalyticProfile,
    /// Swept solid over an explicit polyline / indexed / composite curve.
    ExtrudedArbitraryProfile,
    /// Swept solid whose profile this classifier could not identify.
    ExtrudedUnknownProfile,
    /// Boolean result or clipping — the operand tree matters more than the leaf.
    Boolean,
    /// Explicit planar-faced solid.
    FacetedBrep,
    /// Pre-tessellated mesh (IFC4).
    Tessellated,
    /// Open/closed surface model (common in IFC2x3 Revit exports).
    SurfaceModel,
    /// Geometry reached through a representation map.
    MappedItem,
    /// Solid of revolution.
    Revolved,
    /// Swept disk / directrix sweeps.
    Swept,
    /// CSG primitives and half-spaces.
    CsgPrimitive,
    /// NURBS / advanced B-rep.
    AdvancedBrep,
    /// Only an explicit bounding box is present.
    BoundingBoxOnly,
    /// A body representation exists but is of a kind not recognised here.
    OtherBody,
    /// No body representation at all.
    None,
}

impl fmt::Display for RepresentationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::ExtrudedAnalyticProfile => "extruded/analytic-profile",
            Self::ExtrudedArbitraryProfile => "extruded/arbitrary-profile",
            Self::ExtrudedUnknownProfile => "extruded/unknown-profile",
            Self::Boolean => "boolean",
            Self::FacetedBrep => "faceted-brep",
            Self::Tessellated => "tessellated",
            Self::SurfaceModel => "surface-model",
            Self::MappedItem => "mapped-item",
            Self::Revolved => "revolved",
            Self::Swept => "swept",
            Self::CsgPrimitive => "csg-primitive",
            Self::AdvancedBrep => "advanced-brep",
            Self::BoundingBoxOnly => "bounding-box-only",
            Self::OtherBody => "other-body",
            Self::None => "none",
        };
        f.write_str(s)
    }
}

const ANALYTIC_PROFILES: &[&str] = &[
    "IFCRECTANGLEPROFILEDEF",
    "IFCROUNDEDRECTANGLEPROFILEDEF",
    "IFCRECTANGLEHOLLOWPROFILEDEF",
    "IFCCIRCLEPROFILEDEF",
    "IFCCIRCLEHOLLOWPROFILEDEF",
    "IFCELLIPSEPROFILEDEF",
    "IFCISHAPEPROFILEDEF",
    "IFCASYMMETRICISHAPEPROFILEDEF",
    "IFCTSHAPEPROFILEDEF",
    "IFCLSHAPEPROFILEDEF",
    "IFCUSHAPEPROFILEDEF",
    "IFCZSHAPEPROFILEDEF",
    "IFCCSHAPEPROFILEDEF",
    "IFCTRAPEZIUMPROFILEDEF",
];

const ARBITRARY_PROFILES: &[&str] = &[
    "IFCARBITRARYCLOSEDPROFILEDEF",
    "IFCARBITRARYPROFILEDEFWITHVOIDS",
];

/// Classify the element's *body* representation.
///
/// Prefers the representation whose `RepresentationIdentifier` is `Body`, since
/// products routinely also carry `Axis`, `Box` and `FootPrint` representations
/// that describe the same object far less completely.
pub fn classify(step: &StepFile, element_id: EntityId) -> RepresentationType {
    let reps = shape_representations(step, element_id);
    if reps.is_empty() {
        return RepresentationType::None;
    }

    let mut best: Option<RepresentationType> = None;
    let mut saw_box = false;

    for (identifier, rep_id) in reps {
        let is_body = identifier
            .as_deref()
            .map(|i| i.eq_ignore_ascii_case("body"))
            .unwrap_or(false);
        let kind = classify_items(step, rep_id, 0);
        if kind == RepresentationType::BoundingBoxOnly {
            saw_box = true;
            continue;
        }
        if kind == RepresentationType::None {
            continue;
        }
        if is_body {
            return kind;
        }
        best.get_or_insert(kind);
    }

    best.unwrap_or(if saw_box {
        RepresentationType::BoundingBoxOnly
    } else {
        RepresentationType::None
    })
}

/// `(RepresentationIdentifier, entity_id)` for every shape representation.
fn shape_representations(step: &StepFile, element_id: EntityId) -> Vec<(Option<String>, EntityId)> {
    let Some(entity) = step.entities.get(&element_id) else {
        return vec![];
    };
    // Representation is args[6] on every IfcProduct subtype.
    let Some(prod_rep_id) = entity.args.get(6).and_then(StepValue::as_ref) else {
        return vec![];
    };
    let Some(prod_rep) = step.entities.get(&prod_rep_id) else {
        return vec![];
    };
    let Some(list) = prod_rep.args.get(2).and_then(StepValue::as_list) else {
        return vec![];
    };
    list.iter()
        .filter_map(StepValue::as_ref)
        .map(|rep_id| {
            // IfcShapeRepresentation args[1] = RepresentationIdentifier.
            let identifier = step
                .entities
                .get(&rep_id)
                .and_then(|r| r.args.get(1))
                .and_then(StepValue::as_str)
                .map(|s| s.to_string());
            (identifier, rep_id)
        })
        .collect()
}

fn classify_items(step: &StepFile, rep_id: EntityId, depth: usize) -> RepresentationType {
    if depth > 6 {
        return RepresentationType::OtherBody;
    }
    let Some(rep) = step.entities.get(&rep_id) else {
        return RepresentationType::None;
    };
    let Some(items) = rep.args.get(3).and_then(StepValue::as_list) else {
        return RepresentationType::None;
    };
    for item in items.iter().filter_map(StepValue::as_ref) {
        let kind = classify_item(step, item, depth);
        if kind != RepresentationType::None {
            return kind;
        }
    }
    RepresentationType::None
}

fn classify_item(step: &StepFile, item_id: EntityId, depth: usize) -> RepresentationType {
    if depth > 6 {
        return RepresentationType::OtherBody;
    }
    let Some(e) = step.entities.get(&item_id) else {
        return RepresentationType::None;
    };
    match e.entity_name.as_str() {
        "IFCEXTRUDEDAREASOLID" | "IFCEXTRUDEDAREASOLIDTAPERED" => {
            let profile = e
                .args
                .first()
                .and_then(StepValue::as_ref)
                .and_then(|id| step.entities.get(&id))
                .map(|p| p.entity_name.to_string())
                .unwrap_or_default();
            if ANALYTIC_PROFILES.contains(&profile.as_str()) {
                RepresentationType::ExtrudedAnalyticProfile
            } else if ARBITRARY_PROFILES.contains(&profile.as_str()) {
                RepresentationType::ExtrudedArbitraryProfile
            } else if profile == "IFCDERIVEDPROFILEDEF" || profile == "IFCCOMPOSITEPROFILEDEF" {
                // Both wrap other profiles; report as unknown so they show up
                // separately rather than being credited to a category that a
                // backend claims to support.
                RepresentationType::ExtrudedUnknownProfile
            } else {
                RepresentationType::ExtrudedUnknownProfile
            }
        }
        "IFCBOOLEANRESULT" | "IFCBOOLEANCLIPPINGRESULT" => RepresentationType::Boolean,
        "IFCFACETEDBREP" | "IFCCLOSEDSHELL" => RepresentationType::FacetedBrep,
        "IFCTRIANGULATEDFACESET" | "IFCPOLYGONALFACESET" | "IFCTRIANGULATEDIRREGULARNETWORK" => {
            RepresentationType::Tessellated
        }
        "IFCFACEBASEDSURFACEMODEL" | "IFCSHELLBASEDSURFACEMODEL" => {
            RepresentationType::SurfaceModel
        }
        "IFCREVOLVEDAREASOLID" | "IFCREVOLVEDAREASOLIDTAPERED" => RepresentationType::Revolved,
        "IFCSWEPTDISKSOLID" | "IFCSWEPTDISKSOLIDPOLYGONAL" | "IFCSURFACECURVESWEPTAREASOLID"
        | "IFCFIXEDREFERENCESWEPTAREASOLID" | "IFCSECTIONEDSOLIDHORIZONTAL"
        | "IFCSECTIONEDSOLID" => RepresentationType::Swept,
        "IFCADVANCEDBREP" | "IFCADVANCEDBREPWITHVOIDS" | "IFCMANIFOLDSOLIDBREP" => {
            RepresentationType::AdvancedBrep
        }
        "IFCCSGSOLID" | "IFCBLOCK" | "IFCRECTANGULARPYRAMID" | "IFCRIGHTCIRCULARCONE"
        | "IFCRIGHTCIRCULARCYLINDER" | "IFCSPHERE" | "IFCHALFSPACESOLID"
        | "IFCPOLYGONALBOUNDEDHALFSPACE" | "IFCBOXEDHALFSPACE" => {
            RepresentationType::CsgPrimitive
        }
        "IFCBOUNDINGBOX" => RepresentationType::BoundingBoxOnly,
        "IFCMAPPEDITEM" => {
            // Report what the map actually contains — "mapped-item" alone would
            // hide whether the underlying geometry is tractable. Only fall back
            // to MappedItem when the target cannot be resolved.
            let resolved = e
                .args
                .first()
                .and_then(StepValue::as_ref)
                .and_then(|src| step.entities.get(&src))
                .and_then(|src| src.args.get(1))
                .and_then(StepValue::as_ref)
                .map(|mapped_rep| classify_items(step, mapped_rep, depth + 1));
            match resolved {
                Some(RepresentationType::None) | None => RepresentationType::MappedItem,
                Some(kind) => kind,
            }
        }
        // Curves and points are not bodies; keep looking.
        "IFCPOLYLINE" | "IFCCARTESIANPOINT" | "IFCINDEXEDPOLYCURVE" | "IFCTRIMMEDCURVE"
        | "IFCCOMPOSITECURVE" | "IFCCIRCLE" | "IFCLINE" | "IFCGEOMETRICCURVESET"
        | "IFCGEOMETRICSET" | "IFCANNOTATIONFILLAREA" | "IFCTEXTLITERAL"
        | "IFCTEXTLITERALWITHEXTENT" => RepresentationType::None,
        _ => RepresentationType::OtherBody,
    }
}
