/// Tier 3: exact NetVolume and NetSurfaceArea via signed-tetrahedral decomposition.
///
/// Tessellates IFCFACETEDBREP (fan-triangulate each face polygon) and
/// IFCTRIANGULATEDFACESET into a triangle list, then computes:
///   - NetVolume via signed tetrahedral sum (divergence theorem)
///   - NetSurfaceArea via summed triangle areas
///
/// Pure Rust — no external geometry crate needed.

use ifc_step::{EntityId, StepFile, StepValue};

use crate::step_geom::{parse_cartesian_point, real_from_step};

#[derive(Debug, Clone, Copy)]
pub struct MeshResult {
    pub net_volume: f64,
    pub net_surface_area: f64,
}

/// Attempt to compute mesh metrics from an IFCFACETEDBREP.
pub fn from_faceted_brep(step: &StepFile, shell_id: EntityId) -> Option<MeshResult> {
    let triangles = tessellate_closed_shell(step, shell_id)?;
    Some(metrics_from_triangles(&triangles))
}

/// Attempt to compute mesh metrics from an IFCFACEBASEDSURFACEMODEL.
/// The model has a list of IfcConnectedFaceSet (FbsmFaces), each of which
/// contains IfcFace entities — same structure as IfcClosedShell.
pub fn from_face_based_surface_model(step: &StepFile, model_id: EntityId) -> Option<MeshResult> {
    let model = step.entities.get(&model_id)?;
    // FbsmFaces is args[0]: list of IfcConnectedFaceSet refs.
    let face_sets = model.args.first()?.as_list()?;
    let mut triangles = Vec::new();
    for set_val in face_sets {
        if let Some(set_id) = set_val.as_ref() {
            let set = step.entities.get(&set_id)?;
            // CfsFaces is args[0]: list of IfcFace refs.
            if let Some(faces) = set.args.first().and_then(StepValue::as_list) {
                for face_val in faces {
                    if let Some(face_id) = face_val.as_ref() {
                        tessellate_face(step, face_id, &mut triangles);
                    }
                }
            }
        }
    }
    if triangles.is_empty() { None } else { Some(metrics_from_triangles(&triangles)) }
}

/// Attempt to compute mesh metrics from an IFCTRIANGULATEDFACESET.
pub fn from_triangulated_faceset(step: &StepFile, faceset_id: EntityId) -> Option<MeshResult> {
    let triangles = parse_triangulated_faceset(step, faceset_id)?;
    Some(metrics_from_triangles(&triangles))
}

type Tri = [[f64; 3]; 3];

fn metrics_from_triangles(triangles: &[Tri]) -> MeshResult {
    let mut vol = 0.0_f64;
    let mut area = 0.0_f64;
    for &[a, b, c] in triangles {
        vol += signed_tet_vol(a, b, c);
        area += tri_area(a, b, c);
    }
    MeshResult {
        net_volume: vol.abs(),
        net_surface_area: area,
    }
}

/// Signed tetrahedral volume contribution (divergence theorem, one triangle).
fn signed_tet_vol(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    // (a · (b × c)) / 6
    let bxc = cross(b, c);
    dot(a, bxc) / 6.0
}

fn tri_area(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    let ab = sub(b, a);
    let ac = sub(c, a);
    let n = cross(ab, ac);
    magnitude(n) * 0.5
}

fn cross(u: [f64; 3], v: [f64; 3]) -> [f64; 3] {
    [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ]
}

fn dot(u: [f64; 3], v: [f64; 3]) -> f64 {
    u[0] * v[0] + u[1] * v[1] + u[2] * v[2]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn magnitude(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

// ---------------------------------------------------------------------------
// IFCFACETEDBREP tessellation
// ---------------------------------------------------------------------------

fn tessellate_closed_shell(step: &StepFile, shell_id: EntityId) -> Option<Vec<Tri>> {
    let shell = step.entities.get(&shell_id)?;
    let face_list = shell.args.first()?.as_list()?;
    let mut triangles = Vec::new();
    for face_val in face_list {
        if let Some(face_id) = face_val.as_ref() {
            tessellate_face(step, face_id, &mut triangles);
        }
    }
    if triangles.is_empty() { None } else { Some(triangles) }
}

fn tessellate_face(step: &StepFile, face_id: EntityId, triangles: &mut Vec<Tri>) {
    let face = match step.entities.get(&face_id) {
        Some(e) => e,
        None => return,
    };
    let bounds = match face.args.first().and_then(StepValue::as_list) {
        Some(l) => l,
        None => return,
    };
    for bound_val in bounds {
        let bound_id = match bound_val.as_ref() {
            Some(id) => id,
            None => continue,
        };
        let bound = match step.entities.get(&bound_id) {
            Some(e) => e,
            None => continue,
        };
        if !matches!(bound.entity_name.as_str(), "IFCFACEOUTERBOUND" | "IFCFACEBOUND") {
            continue;
        }
        let loop_id = match bound.args.first().and_then(StepValue::as_ref) {
            Some(id) => id,
            None => continue,
        };
        let poly_loop = match step.entities.get(&loop_id) {
            Some(e) => e,
            None => continue,
        };
        if poly_loop.entity_name != "IFCPOLYLOOP" {
            continue;
        }
        let pt_list = match poly_loop.args.first().and_then(StepValue::as_list) {
            Some(l) => l,
            None => continue,
        };
        let pts: Vec<[f64; 3]> = pt_list
            .iter()
            .filter_map(|v| v.as_ref())
            .filter_map(|id| step.entities.get(&id))
            .filter_map(parse_cartesian_point)
            .collect();

        // Fan triangulation from vertex 0.
        for i in 1..pts.len().saturating_sub(1) {
            triangles.push([pts[0], pts[i], pts[i + 1]]);
        }
        break; // outer bound only
    }
}

// ---------------------------------------------------------------------------
// IFCTRIANGULATEDFACESET parsing
// ---------------------------------------------------------------------------

fn parse_triangulated_faceset(step: &StepFile, faceset_id: EntityId) -> Option<Vec<Tri>> {
    let e = step.entities.get(&faceset_id)?;
    let coords_id = e.args.first()?.as_ref()?;
    let coord_list_e = step.entities.get(&coords_id)?;
    let raw_coords = coord_list_e.args.first()?.as_list()?;

    let verts: Vec<[f64; 3]> = raw_coords
        .iter()
        .filter_map(|v| {
            let pts = v.as_list()?;
            let x = real_from_step(pts.first()?)?;
            let y = real_from_step(pts.get(1)?)?;
            let z = real_from_step(pts.get(2)?)?;
            Some([x, y, z])
        })
        .collect();

    let index_list = e.args.get(3)?.as_list()?;
    let triangles: Vec<Tri> = index_list
        .iter()
        .filter_map(|triple| {
            let t = triple.as_list()?;
            // IFC indices are 1-based.
            let a = (t.first()?.as_int()? - 1) as usize;
            let b = (t.get(1)?.as_int()? - 1) as usize;
            let c = (t.get(2)?.as_int()? - 1) as usize;
            if a >= verts.len() || b >= verts.len() || c >= verts.len() {
                return None;
            }
            Some([verts[a], verts[b], verts[c]])
        })
        .collect();

    if triangles.is_empty() { None } else { Some(triangles) }
}
