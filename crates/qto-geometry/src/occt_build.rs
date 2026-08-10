//! Build OCCT solids from IFC representation items.
//!
//! Geometry is placed **at construction**: profile points are carried into world
//! coordinates before the sweep, and sweep directions are rotated with them.
//! cadrum exposes only axis-aligned rotations, so composing an arbitrary
//! `IfcAxis2Placement3D` afterwards is not available — and it is not needed,
//! since a placed profile produces a placed solid.
//!
//! Placement matters most for booleans. Two operands measured in their own local
//! frames and then subtracted would give an answer for geometry that does not
//! exist; both must be in the same frame first.

use cadrum::{DVec3, Edge, Solid};
use ifc_step::{EntityId, StepFile, StepValue};

use crate::curve;
use crate::occt::{extrude_circle, OcctError};
use crate::profile::real;

/// A right-handed frame: origin plus three orthonormal axes.
#[derive(Debug, Clone, Copy)]
struct Frame {
    origin: [f64; 3],
    x: [f64; 3],
    y: [f64; 3],
    z: [f64; 3],
}

impl Frame {
    fn identity() -> Self {
        Self {
            origin: [0.0; 3],
            x: [1.0, 0.0, 0.0],
            y: [0.0, 1.0, 0.0],
            z: [0.0, 0.0, 1.0],
        }
    }

    /// Carry a point given in this frame's coordinates into world space.
    fn point(&self, p: [f64; 3]) -> DVec3 {
        DVec3::new(
            self.origin[0] + self.x[0] * p[0] + self.y[0] * p[1] + self.z[0] * p[2],
            self.origin[1] + self.x[1] * p[0] + self.y[1] * p[1] + self.z[1] * p[2],
            self.origin[2] + self.x[2] * p[0] + self.y[2] * p[1] + self.z[2] * p[2],
        )
    }

    /// Rotate a direction into world space. Directions ignore the origin.
    fn dir(&self, d: [f64; 3]) -> DVec3 {
        DVec3::new(
            self.x[0] * d[0] + self.y[0] * d[1] + self.z[0] * d[2],
            self.x[1] * d[0] + self.y[1] * d[1] + self.z[1] * d[2],
            self.x[2] * d[0] + self.y[2] * d[1] + self.z[2] * d[2],
        )
    }
}

/// Build an OCCT solid for a representation item.
///
/// `Err` on anything that cannot be built exactly — the caller emits nothing.
pub fn build(step: &StepFile, id: EntityId) -> Result<Solid, OcctError> {
    build_inner(step, id, 0)
}

fn build_inner(step: &StepFile, id: EntityId, depth: usize) -> Result<Solid, OcctError> {
    if depth > 8 {
        return Err(OcctError::KernelFailed("representation nested too deeply".into()));
    }
    let e = step
        .entities
        .get(&id)
        .ok_or_else(|| OcctError::BadProfile(format!("missing entity #{id}")))?;

    match e.entity_name.as_str() {
        "IFCEXTRUDEDAREASOLID" => extruded_area_solid(step, id),

        "IFCBOOLEANRESULT" | "IFCBOOLEANCLIPPINGRESULT" => {
            let op = e
                .args
                .first()
                .and_then(StepValue::as_enum)
                .unwrap_or("DIFFERENCE")
                .to_uppercase();
            let first = e
                .args
                .get(1)
                .and_then(StepValue::as_ref)
                .ok_or_else(|| OcctError::BadProfile("boolean without first operand".into()))?;
            let second = e
                .args
                .get(2)
                .and_then(StepValue::as_ref)
                .ok_or_else(|| OcctError::BadProfile("boolean without second operand".into()))?;

            // Both operands are built before combining. Measuring only the first
            // — what the previous implementation effectively did — answers for a
            // solid that was never there.
            let a = build_inner(step, first, depth + 1)?;
            let b = build_inner(step, second, depth + 1)?;
            let combined = match op.as_str() {
                "DIFFERENCE" => (&a - &b).build(),
                "UNION" => (&a + &b).build(),
                "INTERSECTION" => (&a * &b).build(),
                other => {
                    return Err(OcctError::KernelFailed(format!(
                        "unknown boolean operator {other}"
                    )))
                }
            };
            combined.map_err(|e| OcctError::KernelFailed(e.to_string()))
        }

        // A half-space is unbounded. For a *clipping* operation only the part
        // near the host matters, so a box large enough to swallow the host gives
        // an identical result — exactly, not approximately.
        "IFCHALFSPACESOLID" | "IFCPOLYGONALBOUNDEDHALFSPACE" | "IFCBOXEDHALFSPACE" => {
            half_space(step, id)
        }

        "IFCMAPPEDITEM" => {
            let src = e
                .args
                .first()
                .and_then(StepValue::as_ref)
                .ok_or_else(|| OcctError::BadProfile("mapped item without source".into()))?;
            let mapped = step
                .entities
                .get(&src)
                .and_then(|m| m.args.get(1))
                .and_then(StepValue::as_ref)
                .ok_or_else(|| OcctError::BadProfile("representation map without target".into()))?;
            let items = step
                .entities
                .get(&mapped)
                .and_then(|r| r.args.get(3))
                .and_then(StepValue::as_list)
                .ok_or_else(|| OcctError::BadProfile("mapped representation has no items".into()))?;
            items
                .iter()
                .filter_map(StepValue::as_ref)
                .find_map(|i| build_inner(step, i, depth + 1).ok())
                .ok_or_else(|| OcctError::BadProfile("no buildable item in map".into()))
        }

        other => Err(OcctError::BadProfile(format!("unsupported item {other}"))),
    }
}

fn extruded_area_solid(step: &StepFile, id: EntityId) -> Result<Solid, OcctError> {
    let e = &step.entities[&id];
    let profile_id = e
        .args
        .first()
        .and_then(StepValue::as_ref)
        .ok_or_else(|| OcctError::BadProfile("extrusion without profile".into()))?;
    let frame = e
        .args
        .get(1)
        .and_then(StepValue::as_ref)
        .map(|p| placement3d(step, p))
        .unwrap_or_else(Frame::identity);
    let depth = e
        .args
        .get(3)
        .and_then(real)
        .filter(|d| d.is_finite() && *d > 0.0)
        .ok_or_else(|| OcctError::BadProfile("extrusion without positive depth".into()))?;
    let local_dir = e
        .args
        .get(2)
        .and_then(StepValue::as_ref)
        .and_then(|d| direction(step, d))
        .unwrap_or([0.0, 0.0, 1.0]);

    let world_dir = frame.dir(local_dir);
    let len = (world_dir.x * world_dir.x + world_dir.y * world_dir.y + world_dir.z * world_dir.z)
        .sqrt();
    if len < 1e-12 {
        return Err(OcctError::BadProfile("degenerate extrusion direction".into()));
    }
    let sweep = DVec3::new(
        world_dir.x / len * depth,
        world_dir.y / len * depth,
        world_dir.z / len * depth,
    );

    let prof = step
        .entities
        .get(&profile_id)
        .ok_or_else(|| OcctError::BadProfile("missing profile".into()))?;

    // A circle is swept as a real circle rather than as a chorded polygon; that
    // is the whole reason this path exists alongside the analytic one.
    if prof.entity_name == "IFCCIRCLEPROFILEDEF" {
        let r = prof
            .args
            .get(3)
            .and_then(real)
            .ok_or_else(|| OcctError::BadProfile("circle without radius".into()))?;
        // Only the axis-aligned case is exact through this helper; a rotated
        // circle needs the frame, so fall through to a refusal rather than
        // silently building it in the wrong plane.
        if frame.z == [0.0, 0.0, 1.0] && frame.origin == [0.0, 0.0, 0.0] {
            return extrude_circle(r, [sweep.x, sweep.y, sweep.z]);
        }
        return Err(OcctError::BadProfile(
            "placed circular profile not yet supported".into(),
        ));
    }

    let outline = profile_outline(step, profile_id)
        .ok_or_else(|| OcctError::BadProfile("profile outline not expressible".into()))?;
    if outline.len() < 3 {
        return Err(OcctError::BadProfile("degenerate outline".into()));
    }

    let pts: Vec<DVec3> = outline
        .iter()
        .map(|&[x, y]| frame.point([x, y, 0.0]))
        .collect();
    let wire = Edge::polygon(&pts).map_err(|e| OcctError::BadProfile(e.to_string()))?;
    Solid::extrude(&wire, sweep).map_err(|e| OcctError::KernelFailed(e.to_string()))
}

/// A 2D outline for the profile types that have one.
fn profile_outline(step: &StepFile, profile_id: EntityId) -> Option<Vec<[f64; 2]>> {
    let e = step.entities.get(&profile_id)?;
    match e.entity_name.as_str() {
        "IFCRECTANGLEPROFILEDEF" => {
            let (x, y) = (e.args.get(3).and_then(real)?, e.args.get(4).and_then(real)?);
            let (hx, hy) = (x / 2.0, y / 2.0);
            Some(vec![[-hx, -hy], [hx, -hy], [hx, hy], [-hx, hy]])
        }
        "IFCARBITRARYCLOSEDPROFILEDEF" | "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
            curve::polygon(step, e.args.get(2)?.as_ref()?)
        }
        _ => None,
    }
}

/// A half-space, realised as a box big enough that clipping against it is exact.
fn half_space(step: &StepFile, id: EntityId) -> Result<Solid, OcctError> {
    const REACH: f64 = 1.0e6;

    let e = &step.entities[&id];
    let surface = e
        .args
        .first()
        .and_then(StepValue::as_ref)
        .ok_or_else(|| OcctError::BadProfile("half-space without surface".into()))?;
    // IfcPlane -> IfcAxis2Placement3D
    let frame = step
        .entities
        .get(&surface)
        .and_then(|p| p.args.first())
        .and_then(StepValue::as_ref)
        .map(|p| placement3d(step, p))
        .unwrap_or_else(Frame::identity);

    // AgreementFlag selects which side of the plane is solid.
    let agreement = matches!(e.args.get(1), Some(StepValue::Bool(true)));
    let depth = if agreement { -REACH } else { REACH };

    let square = [
        [-REACH, -REACH],
        [REACH, -REACH],
        [REACH, REACH],
        [-REACH, REACH],
    ];
    let pts: Vec<DVec3> = square.iter().map(|&[x, y]| frame.point([x, y, 0.0])).collect();
    let wire = Edge::polygon(&pts).map_err(|e| OcctError::BadProfile(e.to_string()))?;
    let n = frame.z;
    Solid::extrude(
        &wire,
        DVec3::new(n[0] * depth, n[1] * depth, n[2] * depth),
    )
    .map_err(|e| OcctError::KernelFailed(e.to_string()))
}

/// `IfcAxis2Placement3D` -> an orthonormal frame.
fn placement3d(step: &StepFile, id: EntityId) -> Frame {
    let Some(e) = step.entities.get(&id) else {
        return Frame::identity();
    };
    let origin = e
        .args
        .first()
        .and_then(StepValue::as_ref)
        .and_then(|p| point3(step, p))
        .unwrap_or([0.0; 3]);
    let z = e
        .args
        .get(1)
        .and_then(StepValue::as_ref)
        .and_then(|d| direction(step, d))
        .map(normalise)
        .unwrap_or([0.0, 0.0, 1.0]);
    let ref_x = e
        .args
        .get(2)
        .and_then(StepValue::as_ref)
        .and_then(|d| direction(step, d))
        .unwrap_or_else(|| {
            // IFC's own default: any axis not parallel to Z.
            if z[0].abs() < 0.9 {
                [1.0, 0.0, 0.0]
            } else {
                [0.0, 1.0, 0.0]
            }
        });
    // Gram-Schmidt: RefDirection need not be perpendicular to Axis.
    let dot = ref_x[0] * z[0] + ref_x[1] * z[1] + ref_x[2] * z[2];
    let x = normalise([
        ref_x[0] - dot * z[0],
        ref_x[1] - dot * z[1],
        ref_x[2] - dot * z[2],
    ]);
    let y = [
        z[1] * x[2] - z[2] * x[1],
        z[2] * x[0] - z[0] * x[2],
        z[0] * x[1] - z[1] * x[0],
    ];
    Frame { origin, x, y, z }
}

fn normalise(v: [f64; 3]) -> [f64; 3] {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if n < 1e-12 {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / n, v[1] / n, v[2] / n]
    }
}

fn point3(step: &StepFile, id: EntityId) -> Option<[f64; 3]> {
    let c = step.entities.get(&id)?.args.first()?.as_list()?;
    Some([
        real(c.first()?)?,
        c.get(1).and_then(real).unwrap_or(0.0),
        c.get(2).and_then(real).unwrap_or(0.0),
    ])
}

fn direction(step: &StepFile, id: EntityId) -> Option<[f64; 3]> {
    point3(step, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::occt::{init, measure};
    use ifc_step::parse_step_bytes;

    fn parse(body: &str) -> StepFile {
        let src = format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
             FILE_NAME('','',(''),(''),'',' ','');\nFILE_SCHEMA(('IFC4'));\nENDSEC;\n\
             DATA;\n{body}ENDSEC;\nEND-ISO-10303-21;\n"
        );
        parse_step_bytes(src.as_bytes()).expect("parse")
    }

    const PLACEMENT: &str = "#90=IFCAXIS2PLACEMENT3D(#91,#92,#93);\n\
                             #91=IFCCARTESIANPOINT((0.,0.,0.));\n\
                             #92=IFCDIRECTION((0.,0.,1.));\n\
                             #93=IFCDIRECTION((1.,0.,0.));\n";

    #[test]
    fn placed_extrusion_has_the_right_volume() {
        init();
        let s = parse(&format!(
            "{PLACEMENT}\
             #1=IFCEXTRUDEDAREASOLID(#2,#90,#3,3.);\n\
             #2=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,5.,0.2);\n\
             #3=IFCDIRECTION((0.,0.,1.));\n"
        ));
        let m = measure(&build(&s, 1).expect("solid")).expect("measured");
        assert!((m.volume - 3.0).abs() < 1e-9, "volume {}", m.volume);
    }

    /// The case the analytic path cannot do: the volume of a clipped solid is
    /// not the volume of its first operand.
    #[test]
    fn boolean_difference_subtracts_the_second_operand() {
        init();
        let s = parse(&format!(
            "{PLACEMENT}\
             #1=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#10,#20);\n\
             #10=IFCEXTRUDEDAREASOLID(#11,#90,#12,4.);\n\
             #11=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
             #12=IFCDIRECTION((0.,0.,1.));\n\
             #20=IFCEXTRUDEDAREASOLID(#21,#94,#22,1.);\n\
             #21=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);\n\
             #22=IFCDIRECTION((0.,0.,1.));\n\
             #94=IFCAXIS2PLACEMENT3D(#95,#92,#93);\n\
             #95=IFCCARTESIANPOINT((0.,0.,0.));\n"
        ));
        let m = measure(&build(&s, 1).expect("solid")).expect("measured");
        // 2x2x4 = 16, minus a 1x1x1 notch sunk into its base.
        assert!((m.volume - 15.0).abs() < 1e-9, "volume {}", m.volume);
    }

    /// A boolean whose operands are placed in different frames must be built in
    /// a common frame; measuring the first operand alone would give 16.
    #[test]
    fn operand_placement_is_applied_before_the_boolean() {
        init();
        let s = parse(&format!(
            "{PLACEMENT}\
             #1=IFCBOOLEANRESULT(.DIFFERENCE.,#10,#20);\n\
             #10=IFCEXTRUDEDAREASOLID(#11,#90,#12,4.);\n\
             #11=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
             #12=IFCDIRECTION((0.,0.,1.));\n\
             #20=IFCEXTRUDEDAREASOLID(#21,#96,#22,4.);\n\
             #21=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
             #22=IFCDIRECTION((0.,0.,1.));\n\
             #96=IFCAXIS2PLACEMENT3D(#97,#92,#93);\n\
             #97=IFCCARTESIANPOINT((100.,0.,0.));\n"
        ));
        // The cutter sits 100 units away, so nothing is removed.
        let m = measure(&build(&s, 1).expect("solid")).expect("measured");
        assert!((m.volume - 16.0).abs() < 1e-9, "volume {}", m.volume);
    }

    #[test]
    fn unsupported_items_are_refused() {
        init();
        let s = parse("#1=IFCREVOLVEDAREASOLID(#2,$,$,1.57);\n#2=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);\n");
        assert!(build(&s, 1).is_err());
    }
}
