//! Extract face boundaries from IFC polyhedral representations.
//!
//! Produces [`Face`]s carrying **both** the outer boundary and any inner
//! boundaries, which is the part the previous implementation dropped. It also
//! honours each bound's orientation flag: `IfcFaceBound.Orientation = .F.`
//! means the loop is traversed in reverse, and ignoring it flips a face's
//! normal, corrupting the signed volume sum.

use ifc_step::{EntityId, RawEntity, StepFile, StepValue};

use crate::polyhedron::{Face, Loop};

/// Collect the faces of a polyhedral solid.
///
/// `None` means the entity is not a polyhedral representation this module
/// understands — the caller must then emit nothing rather than fall back.
pub fn faces_for_solid(step: &StepFile, solid_id: EntityId) -> Option<Vec<Face>> {
    let e = step.entities.get(&solid_id)?;
    match e.entity_name.as_str() {
        "IFCFACETEDBREP" => {
            let shell = e.args.first()?.as_ref()?;
            faces_of_shell(step, shell)
        }
        // Voids are additional *closed shells* inside the outer one. Their faces
        // join the same soup: the signed-tetrahedron sum subtracts an
        // inward-oriented inner shell automatically.
        "IFCFACETEDBREPWITHVOIDS" => {
            let mut faces = faces_of_shell(step, e.args.first()?.as_ref()?)?;
            if let Some(voids) = e.args.get(1).and_then(StepValue::as_list) {
                for v in voids.iter().filter_map(StepValue::as_ref) {
                    if let Some(inner) = faces_of_shell(step, v) {
                        faces.extend(inner);
                    }
                }
            }
            Some(faces)
        }
        "IFCFACEBASEDSURFACEMODEL" | "IFCSHELLBASEDSURFACEMODEL" => {
            let sets = e.args.first()?.as_list()?;
            let mut faces = Vec::new();
            for s in sets.iter().filter_map(StepValue::as_ref) {
                if let Some(f) = faces_of_shell(step, s) {
                    faces.extend(f);
                }
            }
            (!faces.is_empty()).then_some(faces)
        }
        "IFCTRIANGULATEDFACESET" => triangulated_faceset(step, e),
        "IFCPOLYGONALFACESET" => polygonal_faceset(step, e),
        _ => None,
    }
}

/// `IfcClosedShell` / `IfcOpenShell` / `IfcConnectedFaceSet` all hold a face list
/// at argument 0.
fn faces_of_shell(step: &StepFile, shell_id: EntityId) -> Option<Vec<Face>> {
    let shell = step.entities.get(&shell_id)?;
    let list = shell.args.first()?.as_list()?;
    let mut faces = Vec::new();
    for face_id in list.iter().filter_map(StepValue::as_ref) {
        if let Some(f) = parse_face(step, face_id) {
            faces.push(f);
        }
    }
    (!faces.is_empty()).then_some(faces)
}

/// `IfcFace.Bounds` — one `IfcFaceOuterBound` plus any number of
/// `IfcFaceBound`s, which are holes.
fn parse_face(step: &StepFile, face_id: EntityId) -> Option<Face> {
    let face = step.entities.get(&face_id)?;
    let bounds = face.args.first()?.as_list()?;

    let mut outer: Option<Loop> = None;
    let mut inner: Vec<Loop> = Vec::new();
    // Some exporters emit only `IfcFaceBound`s with no outer bound marked; the
    // largest loop is then the outer one.
    let mut unmarked: Vec<Loop> = Vec::new();

    for bound_id in bounds.iter().filter_map(StepValue::as_ref) {
        let Some(bound) = step.entities.get(&bound_id) else {
            continue;
        };
        let is_outer = match bound.entity_name.as_str() {
            "IFCFACEOUTERBOUND" => true,
            "IFCFACEBOUND" => false,
            _ => continue,
        };
        let Some(loop_id) = bound.args.first().and_then(StepValue::as_ref) else {
            continue;
        };
        let Some(mut pts) = poly_loop(step, loop_id) else {
            continue;
        };
        // Orientation = .F. means traverse the loop in reverse.
        if matches!(bound.args.get(1), Some(StepValue::Bool(false))) {
            pts.reverse();
        }
        if pts.len() < 3 {
            continue;
        }
        if is_outer {
            outer = Some(pts);
        } else if outer.is_none() {
            unmarked.push(pts);
        } else {
            inner.push(pts);
        }
    }

    match outer {
        Some(o) => {
            inner.extend(unmarked);
            Some(Face { outer: o, inner })
        }
        None => {
            // Fall back to the loop enclosing the largest area.
            let idx = unmarked
                .iter()
                .enumerate()
                .max_by(|a, b| {
                    loop_extent(a.1)
                        .partial_cmp(&loop_extent(b.1))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i)?;
            let o = unmarked.remove(idx);
            Some(Face {
                outer: o,
                inner: unmarked,
            })
        }
    }
}

/// Cheap size proxy for picking an outer loop — bounding-box diagonal squared.
fn loop_extent(l: &Loop) -> f64 {
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for p in l {
        for i in 0..3 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    (0..3).map(|i| (hi[i] - lo[i]).powi(2)).sum()
}

fn poly_loop(step: &StepFile, loop_id: EntityId) -> Option<Loop> {
    let l = step.entities.get(&loop_id)?;
    if l.entity_name != "IFCPOLYLOOP" {
        return None;
    }
    let pts = l.args.first()?.as_list()?;
    let out: Loop = pts
        .iter()
        .filter_map(StepValue::as_ref)
        .filter_map(|id| step.entities.get(&id))
        .filter_map(cartesian_point)
        .collect();
    (out.len() >= 3).then_some(out)
}

fn cartesian_point(e: &RawEntity) -> Option<[f64; 3]> {
    let c = e.args.first()?.as_list()?;
    Some([
        real(c.first()?)?,
        c.get(1).and_then(real).unwrap_or(0.0),
        c.get(2).and_then(real).unwrap_or(0.0),
    ])
}

fn real(v: &StepValue) -> Option<f64> {
    match v {
        StepValue::Real(r) => Some(*r),
        StepValue::Int(i) => Some(*i as f64),
        StepValue::Typed { value, .. } => real(value),
        _ => None,
    }
}

/// `IfcTriangulatedFaceSet`: a shared coordinate list plus 1-based index triples.
fn triangulated_faceset(step: &StepFile, e: &RawEntity) -> Option<Vec<Face>> {
    let verts = coord_list(step, e.args.first()?.as_ref()?)?;
    let indices = e.args.get(3)?.as_list()?;
    let faces: Vec<Face> = indices
        .iter()
        .filter_map(|t| {
            let t = t.as_list()?;
            let idx = |n: usize| -> Option<[f64; 3]> {
                let i = (t.get(n)?.as_int()? - 1) as usize;
                verts.get(i).copied()
            };
            Some(Face {
                outer: vec![idx(0)?, idx(1)?, idx(2)?],
                inner: vec![],
            })
        })
        .collect();
    (!faces.is_empty()).then_some(faces)
}

/// `IfcPolygonalFaceSet`: n-gons over a shared coordinate list, and unlike the
/// triangulated form its faces may carry holes.
fn polygonal_faceset(step: &StepFile, e: &RawEntity) -> Option<Vec<Face>> {
    let verts = coord_list(step, e.args.first()?.as_ref()?)?;
    let face_ids = e.args.get(2)?.as_list()?;
    let pick = |list: &[StepValue]| -> Loop {
        list.iter()
            .filter_map(|v| v.as_int())
            .filter_map(|i| verts.get((i - 1) as usize).copied())
            .collect()
    };

    let mut faces = Vec::new();
    for f in face_ids.iter().filter_map(StepValue::as_ref) {
        let Some(fe) = step.entities.get(&f) else {
            continue;
        };
        let Some(outer) = fe.args.first().and_then(StepValue::as_list).map(|l| pick(l)) else {
            continue;
        };
        if outer.len() < 3 {
            continue;
        }
        let mut inner = Vec::new();
        if fe.entity_name == "IFCINDEXEDPOLYGONALFACEWITHVOIDS" {
            if let Some(voids) = fe.args.get(1).and_then(StepValue::as_list) {
                for v in voids {
                    // Each void is itself a list of indices.
                    let l = match v {
                        StepValue::List(_) => v.as_list(),
                        StepValue::Typed { value, .. } => value.as_list(),
                        _ => None,
                    };
                    if let Some(l) = l {
                        let h = pick(l);
                        if h.len() >= 3 {
                            inner.push(h);
                        }
                    }
                }
            }
        }
        faces.push(Face { outer, inner });
    }
    (!faces.is_empty()).then_some(faces)
}

/// `IfcCartesianPointList2D` / `3D` — coordinates inline, not entity references.
fn coord_list(step: &StepFile, id: EntityId) -> Option<Vec<[f64; 3]>> {
    let e = step.entities.get(&id)?;
    let rows = e.args.first()?.as_list()?;
    Some(
        rows.iter()
            .filter_map(|r| {
                let c = r.as_list()?;
                Some([
                    real(c.first()?)?,
                    c.get(1).and_then(real).unwrap_or(0.0),
                    c.get(2).and_then(real).unwrap_or(0.0),
                ])
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ifc_step::parse_step_bytes;

    fn parse(body: &str) -> StepFile {
        let src = format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
             FILE_NAME('','',(''),(''),'',' ','');\nFILE_SCHEMA(('IFC4'));\nENDSEC;\n\
             DATA;\n{body}ENDSEC;\nEND-ISO-10303-21;\n"
        );
        parse_step_bytes(src.as_bytes()).expect("parse")
    }

    /// A face carrying an inner bound must report it, not silently drop it.
    #[test]
    fn inner_bounds_are_captured() {
        let step = parse(
            "#1=IFCFACE((#2,#3));\n\
             #2=IFCFACEOUTERBOUND(#4,.T.);\n\
             #3=IFCFACEBOUND(#5,.T.);\n\
             #4=IFCPOLYLOOP((#10,#11,#12,#13));\n\
             #5=IFCPOLYLOOP((#14,#15,#16,#17));\n\
             #10=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #11=IFCCARTESIANPOINT((10.,0.,0.));\n\
             #12=IFCCARTESIANPOINT((10.,10.,0.));\n\
             #13=IFCCARTESIANPOINT((0.,10.,0.));\n\
             #14=IFCCARTESIANPOINT((4.,4.,0.));\n\
             #15=IFCCARTESIANPOINT((6.,4.,0.));\n\
             #16=IFCCARTESIANPOINT((6.,6.,0.));\n\
             #17=IFCCARTESIANPOINT((4.,6.,0.));\n",
        );
        let f = parse_face(&step, 1).expect("face");
        assert_eq!(f.outer.len(), 4);
        assert_eq!(f.inner.len(), 1, "inner bound must not be dropped");
        assert_eq!(f.inner[0].len(), 4);
    }

    /// `Orientation = .F.` reverses the loop; ignoring it flips the face normal
    /// and corrupts the signed volume sum.
    #[test]
    fn reversed_orientation_reverses_the_loop() {
        let mk = |flag: &str| {
            let step = parse(&format!(
                "#1=IFCFACE((#2));\n\
                 #2=IFCFACEOUTERBOUND(#4,{flag});\n\
                 #4=IFCPOLYLOOP((#10,#11,#12));\n\
                 #10=IFCCARTESIANPOINT((0.,0.,0.));\n\
                 #11=IFCCARTESIANPOINT((1.,0.,0.));\n\
                 #12=IFCCARTESIANPOINT((0.,1.,0.));\n"
            ));
            parse_face(&step, 1).expect("face").outer
        };
        let fwd = mk(".T.");
        let rev = mk(".F.");
        assert_eq!(fwd.len(), 3);
        assert_eq!(rev.len(), 3);
        assert_eq!(rev[0], fwd[2], "loop should be reversed");
        assert_eq!(rev[2], fwd[0]);
    }

    #[test]
    fn triangulated_faceset_is_read() {
        let step = parse(
            "#1=IFCTRIANGULATEDFACESET(#2,$,.T.,((1,2,3),(1,3,4)),$);\n\
             #2=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,0.,0.),(1.,1.,0.),(0.,1.,0.)));\n",
        );
        let faces = faces_for_solid(&step, 1).expect("faces");
        assert_eq!(faces.len(), 2);
        assert_eq!(faces[0].outer.len(), 3);
    }

    #[test]
    fn unknown_representations_yield_nothing() {
        let step = parse("#1=IFCEXTRUDEDAREASOLID(#2,$,$,3.0);\n#2=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,2.);\n");
        assert!(faces_for_solid(&step, 1).is_none());
    }
}
