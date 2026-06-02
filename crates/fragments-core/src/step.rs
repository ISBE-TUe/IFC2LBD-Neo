use std::collections::BTreeMap;

use ifc_step::{EntityId, RawEntity, StepFile, StepValue};

#[derive(Clone, Copy, Debug)]
pub struct Affine3 {
    pub m: [[f64; 4]; 4],
}

impl Affine3 {
    pub fn identity() -> Self {
        Self {
            m: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    pub fn mul(&self, rhs: &Self) -> Self {
        let mut out = [[0.0; 4]; 4];
        for r in 0..4 {
            for c in 0..4 {
                out[r][c] = (0..4).map(|k| self.m[r][k] * rhs.m[k][c]).sum();
            }
        }
        Self { m: out }
    }

    pub fn transform_point(&self, point: [f64; 3]) -> [f64; 3] {
        let x = self.m[0][0] * point[0] + self.m[0][1] * point[1] + self.m[0][2] * point[2] + self.m[0][3];
        let y = self.m[1][0] * point[0] + self.m[1][1] * point[1] + self.m[1][2] * point[2] + self.m[1][3];
        let z = self.m[2][0] * point[0] + self.m[2][1] * point[1] + self.m[2][2] * point[2] + self.m[2][3];
        [x, y, z]
    }

    pub fn translation(&self) -> [f64; 3] {
        [self.m[0][3], self.m[1][3], self.m[2][3]]
    }

    pub fn axes(&self) -> ([f32; 3], [f32; 3]) {
        (
            [self.m[0][0] as f32, self.m[1][0] as f32, self.m[2][0] as f32],
            [self.m[0][1] as f32, self.m[1][1] as f32, self.m[2][1] as f32],
        )
    }

    /// Inverse of this rigid-body transform (rotation transpose + adjusted translation).
    pub fn inverse(&self) -> Self {
        let m = &self.m;
        // Transpose the 3×3 rotation block
        let rt = [
            [m[0][0], m[1][0], m[2][0]],
            [m[0][1], m[1][1], m[2][1]],
            [m[0][2], m[1][2], m[2][2]],
        ];
        let tx = -(rt[0][0] * m[0][3] + rt[0][1] * m[1][3] + rt[0][2] * m[2][3]);
        let ty = -(rt[1][0] * m[0][3] + rt[1][1] * m[1][3] + rt[1][2] * m[2][3]);
        let tz = -(rt[2][0] * m[0][3] + rt[2][1] * m[1][3] + rt[2][2] * m[2][3]);
        Self {
            m: [
                [rt[0][0], rt[0][1], rt[0][2], tx],
                [rt[1][0], rt[1][1], rt[1][2], ty],
                [rt[2][0], rt[2][1], rt[2][2], tz],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }
}

#[derive(Clone, Debug)]
pub struct ShellGeometry {
    pub points: Vec<[f32; 3]>,
    pub faces: Vec<Vec<u32>>,
}

impl ShellGeometry {
    pub fn bbox(&self) -> ([f32; 3], [f32; 3]) {
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for point in &self.points {
            for axis in 0..3 {
                min[axis] = min[axis].min(point[axis]);
                max[axis] = max[axis].max(point[axis]);
            }
        }
        (min, max)
    }

    /// Triangulate polygon faces (fan triangulation) and compute per-vertex normals.
    /// Returns (positions, normals, triangles) matching what web-ifc gives the oracle.
    pub fn to_triangulated(&self) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<[u32; 3]>) {
        let mut triangles: Vec<[u32; 3]> = Vec::new();

        for face in &self.faces {
            if face.len() < 3 { continue; }
            // Fan triangulation from vertex 0
            let v0 = face[0];
            for i in 1..face.len()-1 {
                triangles.push([v0, face[i], face[i + 1]]);
            }
        }

        // Compute per-vertex normals: accumulate face normals at each vertex
        let n = self.points.len();
        let mut normals = vec![[0.0f32; 3]; n];

        for &[i1, i2, i3] in &triangles {
            let p1 = self.points[i1 as usize];
            let p2 = self.points[i2 as usize];
            let p3 = self.points[i3 as usize];
            let ab = [p2[0]-p1[0], p2[1]-p1[1], p2[2]-p1[2]];
            let ac = [p3[0]-p1[0], p3[1]-p1[1], p3[2]-p1[2]];
            let cx = ab[1]*ac[2] - ab[2]*ac[1];
            let cy = ab[2]*ac[0] - ab[0]*ac[2];
            let cz = ab[0]*ac[1] - ab[1]*ac[0];
            for &vi in &[i1, i2, i3] {
                normals[vi as usize][0] += cx;
                normals[vi as usize][1] += cy;
                normals[vi as usize][2] += cz;
            }
        }

        // Normalize
        for n in &mut normals {
            let len = (n[0]*n[0] + n[1]*n[1] + n[2]*n[2]).sqrt();
            if len > 1e-12 {
                n[0] /= len; n[1] /= len; n[2] /= len;
            } else {
                *n = [0.0, 0.0, 1.0];
            }
        }

        (self.points.clone(), normals, triangles)
    }
}

/// A geometry instance for one element: the shell (in definition space) plus the
/// local transform that maps from definition space to the element's coordinate frame.
/// `local_transform` is identity for non-mapped items.
/// `item_id` is the STEP express ID of the geometry item — used as the dedup key.
#[derive(Clone, Debug)]
pub struct GeometryInstance {
    pub shell: ShellGeometry,
    pub local_transform: Affine3,
    pub item_id: EntityId,
}

impl Affine3 {
    /// Returns true when this transform is the identity (within floating-point tolerance).
    pub fn is_identity(&self) -> bool {
        let m = &self.m;
        (m[0][0] - 1.0).abs() < 1e-6
            && m[0][1].abs() < 1e-6
            && m[0][2].abs() < 1e-6
            && m[0][3].abs() < 1e-10
            && m[1][0].abs() < 1e-6
            && (m[1][1] - 1.0).abs() < 1e-6
            && m[1][2].abs() < 1e-6
            && m[1][3].abs() < 1e-10
            && m[2][0].abs() < 1e-6
            && m[2][1].abs() < 1e-6
            && (m[2][2] - 1.0).abs() < 1e-6
            && m[2][3].abs() < 1e-10
    }
}

pub fn entity_name(step: &StepFile, id: EntityId) -> Option<&str> {
    step.entities.get(&id).map(|entity| entity.entity_name.as_str())
}

pub fn product_world_transform(step: &StepFile, element_id: EntityId) -> Affine3 {
    let Some(entity) = step.entities.get(&element_id) else {
        return Affine3::identity();
    };
    let Some(placement_id) = entity.args.get(5).and_then(StepValue::as_ref) else {
        return Affine3::identity();
    };
    extract_placement_transform(step, placement_id)
}

/// Returns all geometry instances for an element.
/// Each instance has its shell in definition space + the local transform.
/// For non-IFCMAPPEDITEM items the local transform is identity.
/// For IFCMAPPEDITEM the local transform is the mapping operator — NOT baked into vertices.
pub fn geometry_instances_for_product(step: &StepFile, element_id: EntityId) -> Vec<GeometryInstance> {
    let mut out = Vec::new();
    for rep_id in shape_reps(step, element_id) {
        let Some(rep) = step.entities.get(&rep_id) else { continue; };
        let Some(items) = rep.args.get(3).and_then(StepValue::as_list) else { continue; };
        for item_ref in items {
            let Some(item_id) = item_ref.as_ref() else { continue; };
            collect_geometry_instances(step, item_id, Affine3::identity(), 0, &mut out);
        }
    }
    out
}

/// Recursively collect ALL geometry instances from an item tree, accumulating into `out`.
/// Each IFCMAPPEDITEM contributes one instance per leaf geometry in its mapped representation.
fn collect_geometry_instances(
    step: &StepFile,
    item_id: EntityId,
    parent_transform: Affine3,
    depth: usize,
    out: &mut Vec<GeometryInstance>,
) {
    if depth > 8 { return; }
    let Some(entity) = step.entities.get(&item_id) else { return; };
    match entity.entity_name.as_str() {
        "IFCMAPPEDITEM" => {
            let Some(map_source_id) = entity.args.first().and_then(StepValue::as_ref) else { return; };
            let Some(map_target_id) = entity.args.get(1).and_then(StepValue::as_ref) else { return; };
            let Some(map_source) = step.entities.get(&map_source_id) else { return; };
            let Some(mapped_rep_id) = map_source.args.get(1).and_then(StepValue::as_ref) else { return; };
            let mapping_origin_id = map_source.args.first().and_then(StepValue::as_ref);
            let Some(mapped_rep) = step.entities.get(&mapped_rep_id) else { return; };
            let Some(child_items) = mapped_rep.args.get(3).and_then(StepValue::as_list) else { return; };

            let target = read_cartesian_transform_operator(step, map_target_id);
            let origin = mapping_origin_id
                .map(|id| axis2placement3d_to_affine(step, id))
                .unwrap_or_else(Affine3::identity);
            let mapping = target.mul(&origin);
            let combined = parent_transform.mul(&mapping);

            // Recurse into ALL items in the mapped representation
            for child_ref in child_items {
                let Some(child_id) = child_ref.as_ref() else { continue; };
                collect_geometry_instances(step, child_id, combined, depth + 1, out);
            }
        }
        "IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT" => {
            if let Some(first) = entity.args.get(1).and_then(StepValue::as_ref) {
                collect_geometry_instances(step, first, parent_transform, depth + 1, out);
            }
        }
        _ => {
            if let Some((shell, item_placement)) = shell_from_item_direct(step, entity, item_id) {
                let combined = parent_transform.mul(&item_placement);
                out.push(GeometryInstance { shell, local_transform: combined, item_id });
            }
        }
    }
}

/// Extract shell from a direct (non-mapped) geometry item.
/// Returns `(shell, item_placement)` where:
/// - `shell` is in the geometry's own definition space (item placement NOT applied to vertices)
/// - `item_placement` is the item's own placement within the representation (axis2placement3d)
///
/// Callers accumulate item_placement into the running transform chain; the shell stays raw.
/// This matches oracle behavior where geometry is kept in local space and transforms are separate.
fn shell_from_item_direct(
    step: &StepFile,
    entity: &RawEntity,
    item_id: EntityId,
) -> Option<(ShellGeometry, Affine3)> {
    match entity.entity_name.as_str() {
        "IFCEXTRUDEDAREASOLID" => {
            // arg[1] = Position (IFCAXIS2PLACEMENT3D) — extract but do NOT apply to points
            let placement = entity.args.get(1).and_then(StepValue::as_ref)
                .map(|id| axis2placement3d_to_affine(step, id))
                .unwrap_or_else(Affine3::identity);
            let shell = extruded_area_solid_raw(step, entity)?;
            Some((shell, placement))
        }
        "IFCBOUNDINGBOX" => {
            Some((bounding_box_to_shell(entity)?, Affine3::identity()))
        }
        "IFCTRIANGULATEDFACESET" | "IFCPOLYGONALFACESET" => {
            Some((triangulated_faceset_to_shell(step, item_id)?, Affine3::identity()))
        }
        "IFCFACETEDBREP" => {
            Some((faceted_brep_to_shell(step, entity)?, Affine3::identity()))
        }
        "IFCFACEBASEDSURFACEMODEL" => {
            Some((face_based_surface_model_to_shell(step, entity)?, Affine3::identity()))
        }
        _ => None,
    }
}

fn shape_reps(step: &StepFile, element_id: EntityId) -> Vec<EntityId> {
    let Some(entity) = step.entities.get(&element_id) else {
        return vec![];
    };
    let Some(prod_rep_id) = entity.args.get(6).and_then(StepValue::as_ref) else {
        return vec![];
    };
    let Some(prod_rep) = step.entities.get(&prod_rep_id) else {
        return vec![];
    };
    let Some(list) = prod_rep.args.get(2).and_then(StepValue::as_list) else {
        return vec![];
    };
    list.iter().filter_map(StepValue::as_ref).collect()
}

fn shell_from_item(step: &StepFile, item_id: EntityId, depth: usize) -> Option<ShellGeometry> {
    if depth > 8 { return None; }
    let entity = step.entities.get(&item_id)?;
    match entity.entity_name.as_str() {
        "IFCMAPPEDITEM" => mapped_item_to_shell(step, entity, depth + 1),
        "IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT" => {
            let first = entity.args.get(1).and_then(StepValue::as_ref)?;
            shell_from_item(step, first, depth + 1)
        }
        _ => shell_from_item_direct(step, entity, item_id).map(|(shell, _)| shell),
    }
}

fn mapped_item_to_shell(step: &StepFile, entity: &RawEntity, depth: usize) -> Option<ShellGeometry> {
    let map_source_id = entity.args.get(0)?.as_ref()?;
    let map_target_id = entity.args.get(1)?.as_ref()?;
    let map_source = step.entities.get(&map_source_id)?;
    let mapped_rep_id = map_source.args.get(1)?.as_ref()?;
    let mapping_origin_id = map_source.args.first().and_then(StepValue::as_ref);
    let mapped_rep = step.entities.get(&mapped_rep_id)?;
    let items = mapped_rep.args.get(3)?.as_list()?;

    for item in items {
        let Some(child_id) = item.as_ref() else {
            continue;
        };
        let mut shell = shell_from_item(step, child_id, depth)?;
        let target = read_cartesian_transform_operator(step, map_target_id);
        let origin = mapping_origin_id
            .map(|id| axis2placement3d_to_affine(step, id))
            .unwrap_or_else(Affine3::identity);
        let combined = target.mul(&origin);
        for point in &mut shell.points {
            let [x, y, z] = combined.transform_point([point[0] as f64, point[1] as f64, point[2] as f64]);
            *point = [x as f32, y as f32, z as f32];
        }
        return Some(shell);
    }
    None
}

fn extruded_area_solid_to_shell(step: &StepFile, entity: &RawEntity) -> Option<ShellGeometry> {
    let profile_id = entity.args.first()?.as_ref()?;
    let transform = entity
        .args
        .get(1)
        .and_then(StepValue::as_ref)
        .map(|id| axis2placement3d_to_affine(step, id))
        .unwrap_or_else(Affine3::identity);
    let shell = extruded_area_solid_raw(step, entity)?;
    // Apply placement transform to produce element-local geometry (legacy path, used by shell_from_item)
    let points = shell.points.into_iter().map(|p| {
        let [x, y, z] = transform.transform_point([p[0] as f64, p[1] as f64, p[2] as f64]);
        [x as f32, y as f32, z as f32]
    }).collect();
    Some(ShellGeometry { points, faces: shell.faces })
}

/// Same as `extruded_area_solid_to_shell` but WITHOUT applying the axis2placement3d.
/// Returns geometry in the solid's own profile-origin space (no placement).
fn extruded_area_solid_raw(step: &StepFile, entity: &RawEntity) -> Option<ShellGeometry> {
    let profile_id = entity.args.first()?.as_ref()?;
    let direction = entity
        .args
        .get(2)
        .and_then(StepValue::as_ref)
        .map(|id| read_direction(step, id))
        .unwrap_or([0.0, 0.0, 1.0]);
    let depth = real_from_step(entity.args.get(3)?)?;

    let profile = profile_polygon(step, profile_id)?;
    if profile.len() < 3 {
        return None;
    }

    let top: Vec<[f64; 3]> = profile
        .iter()
        .map(|p| [p[0] + direction[0] * depth, p[1] + direction[1] * depth, p[2] + direction[2] * depth])
        .collect();

    let mut points = Vec::with_capacity(profile.len() * 2);
    for p in profile.iter().chain(top.iter()) {
        points.push([p[0] as f32, p[1] as f32, p[2] as f32]);
    }

    let n = profile.len() as u32;
    let mut faces = Vec::with_capacity(profile.len() + 2);
    faces.push((0..n).collect());
    faces.push((0..n).rev().map(|idx| idx + n).collect());
    for idx in 0..n {
        let next = (idx + 1) % n;
        faces.push(vec![idx, next, next + n, idx + n]);
    }

    Some(ShellGeometry { points, faces })
}

fn triangulated_faceset_to_shell(step: &StepFile, faceset_id: EntityId) -> Option<ShellGeometry> {
    let entity = step.entities.get(&faceset_id)?;
    match entity.entity_name.as_str() {
        "IFCPOLYGONALFACESET" => polygonal_faceset_to_shell(step, entity),
        _ => triangulated_faceset_to_shell_inner(step, entity),
    }
}

/// IFCPOLYGONALFACESET(Coordinates, Closed, Faces, PnIndex)
/// - arg[0]: Coordinates ref (IFCCARTESIANPOINTLIST3D)
/// - arg[1]: Closed (OPTIONAL BOOLEAN — skip)
/// - arg[2]: Faces (LIST OF IFCINDEXEDPOLYGONALFACE refs)
/// - arg[3]: PnIndex (OPTIONAL — skip)
fn polygonal_faceset_to_shell(step: &StepFile, entity: &RawEntity) -> Option<ShellGeometry> {
    let coords_id = entity.args.first()?.as_ref()?;
    let coords_entity = step.entities.get(&coords_id)?;
    let coord_lists = coords_entity.args.first()?.as_list()?;
    let points: Vec<[f32; 3]> = coord_lists
        .iter()
        .filter_map(StepValue::as_list)
        .map(|coords| [
            real_from_step(coords.first().unwrap_or(&StepValue::Int(0))).unwrap_or(0.0) as f32,
            real_from_step(coords.get(1).unwrap_or(&StepValue::Int(0))).unwrap_or(0.0) as f32,
            real_from_step(coords.get(2).unwrap_or(&StepValue::Int(0))).unwrap_or(0.0) as f32,
        ])
        .collect();

    // Faces are at arg[2], not arg[1] (arg[1] = Closed boolean)
    let faces_refs = entity.args.get(2)?.as_list()?;
    let mut faces = Vec::new();
    for face_ref in faces_refs {
        let face_id = face_ref.as_ref()?;
        let face_entity = step.entities.get(&face_id)?;
        // IFCINDEXEDPOLYGONALFACE: arg[0] = CoordIndex (list of integers, 1-indexed)
        let indices = face_entity.args.first()?.as_list()?;
        let face_indices: Vec<u32> = indices
            .iter()
            .filter_map(integer_from_step)
            .map(|idx| (idx - 1) as u32)
            .collect();
        if face_indices.len() >= 3 {
            faces.push(face_indices);
        }
    }
    if points.is_empty() || faces.is_empty() {
        return None;
    }
    Some(ShellGeometry { points, faces })
}

/// IFCTRIANGULATEDFACESET(Coordinates, Normals, Closed, CoordIndex, PnIndex)
/// - arg[0]: Coordinates ref
/// - arg[1]: Normals (OPTIONAL — skip)
/// - arg[2]: Closed (OPTIONAL BOOLEAN — skip)
/// - arg[3]: CoordIndex (LIST OF [3:3] triples, 1-indexed)
/// - arg[4]: PnIndex (OPTIONAL — skip)
fn triangulated_faceset_to_shell_inner(step: &StepFile, entity: &RawEntity) -> Option<ShellGeometry> {
    let coords_id = entity.args.first()?.as_ref()?;
    let coords_entity = step.entities.get(&coords_id)?;
    let coord_lists = coords_entity.args.first()?.as_list()?;
    let points: Vec<[f32; 3]> = coord_lists
        .iter()
        .filter_map(StepValue::as_list)
        .map(|coords| [
            real_from_step(coords.first().unwrap_or(&StepValue::Int(0))).unwrap_or(0.0) as f32,
            real_from_step(coords.get(1).unwrap_or(&StepValue::Int(0))).unwrap_or(0.0) as f32,
            real_from_step(coords.get(2).unwrap_or(&StepValue::Int(0))).unwrap_or(0.0) as f32,
        ])
        .collect();

    // CoordIndex is at arg[3], not arg[1]
    let faces_arg = entity.args.get(3)?.as_list()?;
    let mut faces = Vec::new();
    for face in faces_arg {
        let Some(indices) = face.as_list() else { continue; };
        let face_indices: Vec<u32> = indices
            .iter()
            .filter_map(integer_from_step)
            .map(|idx| (idx - 1) as u32)
            .collect();
        if face_indices.len() >= 3 {
            faces.push(face_indices);
        }
    }
    if points.is_empty() || faces.is_empty() {
        return None;
    }
    Some(ShellGeometry { points, faces })
}

fn faceted_brep_to_shell(step: &StepFile, entity: &RawEntity) -> Option<ShellGeometry> {
    let shell_id = entity.args.first()?.as_ref()?;
    let shell = step.entities.get(&shell_id)?;
    let faces_refs = shell.args.first()?.as_list()?;
    shell_from_face_set(step, faces_refs)
}

fn face_based_surface_model_to_shell(step: &StepFile, entity: &RawEntity) -> Option<ShellGeometry> {
    let sets = entity.args.first()?.as_list()?;
    let set_id = sets.first()?.as_ref()?;
    let connected = step.entities.get(&set_id)?;
    let faces_refs = connected.args.first()?.as_list()?;
    shell_from_face_set(step, faces_refs)
}

fn shell_from_face_set(step: &StepFile, faces_refs: &[StepValue]) -> Option<ShellGeometry> {
    let mut point_map: BTreeMap<String, u32> = BTreeMap::new();
    let mut points = Vec::new();
    let mut faces = Vec::new();

    for face_ref in faces_refs {
        let Some(face_id) = face_ref.as_ref() else {
            continue;
        };
        let Some(face) = step.entities.get(&face_id) else {
            continue;
        };
        let Some(bounds) = face.args.first().and_then(StepValue::as_list) else {
            continue;
        };
        let Some(bound_id) = bounds.first().and_then(StepValue::as_ref) else {
            continue;
        };
        let Some(bound) = step.entities.get(&bound_id) else {
            continue;
        };
        let Some(loop_id) = bound.args.first().and_then(StepValue::as_ref) else {
            continue;
        };
        let Some(poly_loop) = step.entities.get(&loop_id) else {
            continue;
        };
        let Some(loop_points) = poly_loop.args.first().and_then(StepValue::as_list) else {
            continue;
        };
        let mut face_indices = Vec::new();
        for point_ref in loop_points {
            let Some(point_id) = point_ref.as_ref() else {
                continue;
            };
            let Some(point) = cartesian_point(step, point_id) else {
                continue;
            };
            let key = format!("{:.9}|{:.9}|{:.9}", point[0], point[1], point[2]);
            let index = if let Some(existing) = point_map.get(&key) {
                *existing
            } else {
                let next = points.len() as u32;
                points.push([point[0] as f32, point[1] as f32, point[2] as f32]);
                point_map.insert(key, next);
                next
            };
            face_indices.push(index);
        }
        if face_indices.len() >= 3 {
            faces.push(face_indices);
        }
    }

    if points.is_empty() || faces.is_empty() {
        return None;
    }
    Some(ShellGeometry { points, faces })
}

fn bounding_box_to_shell(entity: &RawEntity) -> Option<ShellGeometry> {
    let x = real_from_step(entity.args.get(1)?)? as f32;
    let y = real_from_step(entity.args.get(2)?)? as f32;
    let z = real_from_step(entity.args.get(3)?)? as f32;
    let points = vec![
        [0.0, 0.0, 0.0],
        [x, 0.0, 0.0],
        [x, y, 0.0],
        [0.0, y, 0.0],
        [0.0, 0.0, z],
        [x, 0.0, z],
        [x, y, z],
        [0.0, y, z],
    ];
    let faces = vec![
        vec![0, 1, 2, 3],
        vec![4, 5, 6, 7],
        vec![0, 1, 5, 4],
        vec![1, 2, 6, 5],
        vec![2, 3, 7, 6],
        vec![3, 0, 4, 7],
    ];
    Some(ShellGeometry { points, faces })
}

fn profile_polygon(step: &StepFile, profile_id: EntityId) -> Option<Vec<[f64; 3]>> {
    let entity = step.entities.get(&profile_id)?;
    match entity.entity_name.as_str() {
        "IFCRECTANGLEPROFILEDEF" => {
            let x = real_from_step(entity.args.get(3)?)?;
            let y = real_from_step(entity.args.get(4)?)?;
            let mut points = vec![
                [-x / 2.0, -y / 2.0, 0.0],
                [x / 2.0, -y / 2.0, 0.0],
                [x / 2.0, y / 2.0, 0.0],
                [-x / 2.0, y / 2.0, 0.0],
            ];
            if let Some(pos_id) = entity.args.get(2).and_then(StepValue::as_ref) {
                let t = profile_placement_transform(step, pos_id);
                for point in &mut points {
                    *point = t.transform_point(*point);
                }
            }
            Some(points)
        }
        "IFCARBITRARYCLOSEDPROFILEDEF" => {
            let curve_id = entity.args.get(2)?.as_ref()?;
            arbitrary_closed_curve(step, curve_id).map(|pts| apply_profile_transform(step, entity, pts))
        }
        "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
            let curve_id = entity.args.get(2)?.as_ref()?;
            arbitrary_closed_curve(step, curve_id).map(|pts| apply_profile_transform(step, entity, pts))
        }
        _ => None,
    }
}

fn apply_profile_transform(step: &StepFile, entity: &RawEntity, mut points: Vec<[f64; 3]>) -> Vec<[f64; 3]> {
    if let Some(pos_id) = entity.args.get(2).and_then(StepValue::as_ref) {
        let t = profile_placement_transform(step, pos_id);
        for point in &mut points {
            *point = t.transform_point(*point);
        }
    }
    points
}

fn arbitrary_closed_curve(step: &StepFile, curve_id: EntityId) -> Option<Vec<[f64; 3]>> {
    let curve = step.entities.get(&curve_id)?;
    match curve.entity_name.as_str() {
        "IFCPOLYLINE" => {
            let refs = curve.args.first()?.as_list()?;
            let mut points = Vec::new();
            for point_ref in refs {
                let point_id = point_ref.as_ref()?;
                points.push(cartesian_point(step, point_id)?);
            }
            Some(points)
        }
        "IFCINDEXEDPOLYCURVE" => {
            let point_list_id = curve.args.first()?.as_ref()?;
            let point_list = step.entities.get(&point_list_id)?;
            let coords = point_list.args.first()?.as_list()?;
            let points = coords
                .iter()
                .filter_map(StepValue::as_list)
                .map(|items| {
                    [
                        real_from_step(items.first().unwrap_or(&StepValue::Int(0))).unwrap_or(0.0),
                        real_from_step(items.get(1).unwrap_or(&StepValue::Int(0))).unwrap_or(0.0),
                        real_from_step(items.get(2).unwrap_or(&StepValue::Int(0))).unwrap_or(0.0),
                    ]
                })
                .collect();
            Some(points)
        }
        _ => None,
    }
}

fn extract_placement_transform(step: &StepFile, placement_id: EntityId) -> Affine3 {
    let mut current = placement_id;
    let mut result = Affine3::identity();

    loop {
        let Some(entity) = step.entities.get(&current) else {
            break;
        };
        if entity.entity_name != "IFCLOCALPLACEMENT" {
            break;
        }
        let Some(rel_id) = entity.args.get(1).and_then(StepValue::as_ref) else {
            break;
        };
        let this = match entity_name(step, rel_id) {
            Some("IFCAXIS2PLACEMENT3D") => axis2placement3d_to_affine(step, rel_id),
            Some("IFCAXIS2PLACEMENT2D") => axis2placement2d_to_affine(step, rel_id),
            _ => Affine3::identity(),
        };
        result = this.mul(&result);
        let Some(parent) = entity.args.first().and_then(StepValue::as_ref) else {
            break;
        };
        current = parent;
    }

    result
}

fn axis2placement3d_to_affine(step: &StepFile, id: EntityId) -> Affine3 {
    let origin = read_placement_origin(step, id);
    let z = normalize(read_placement_axis(step, id));
    let x_seed = normalize(read_placement_ref_dir(step, id));
    let y = normalize(cross(z, x_seed));
    let x = normalize(cross(y, z));
    Affine3 {
        m: [
            [x[0], y[0], z[0], origin[0]],
            [x[1], y[1], z[1], origin[1]],
            [x[2], y[2], z[2], origin[2]],
            [0.0, 0.0, 0.0, 1.0],
        ],
    }
}

fn axis2placement2d_to_affine(step: &StepFile, id: EntityId) -> Affine3 {
    let Some(entity) = step.entities.get(&id) else {
        return Affine3::identity();
    };
    let origin = entity
        .args
        .first()
        .and_then(StepValue::as_ref)
        .and_then(|point_id| cartesian_point(step, point_id))
        .unwrap_or([0.0, 0.0, 0.0]);
    let ref_dir = entity
        .args
        .get(1)
        .and_then(StepValue::as_ref)
        .map(|dir_id| read_direction(step, dir_id))
        .unwrap_or([1.0, 0.0, 0.0]);
    let x = normalize([ref_dir[0], ref_dir[1], 0.0]);
    let y = normalize([-x[1], x[0], 0.0]);
    Affine3 {
        m: [
            [x[0], y[0], 0.0, origin[0]],
            [x[1], y[1], 0.0, origin[1]],
            [0.0, 0.0, 1.0, origin[2]],
            [0.0, 0.0, 0.0, 1.0],
        ],
    }
}

fn profile_placement_transform(step: &StepFile, pos_id: EntityId) -> Affine3 {
    match entity_name(step, pos_id) {
        Some("IFCAXIS2PLACEMENT3D") => axis2placement3d_to_affine(step, pos_id),
        Some("IFCAXIS2PLACEMENT2D") => axis2placement2d_to_affine(step, pos_id),
        _ => Affine3::identity(),
    }
}

fn read_cartesian_transform_operator(step: &StepFile, id: EntityId) -> Affine3 {
    let Some(entity) = step.entities.get(&id) else {
        return Affine3::identity();
    };
    let origin = entity
        .args
        .get(0)
        .and_then(StepValue::as_ref)
        .and_then(|point_id| cartesian_point(step, point_id))
        .unwrap_or([0.0, 0.0, 0.0]);
    let axis1 = entity
        .args
        .get(1)
        .and_then(StepValue::as_ref)
        .map(|dir_id| read_direction(step, dir_id))
        .unwrap_or([1.0, 0.0, 0.0]);
    let axis2 = entity
        .args
        .get(2)
        .and_then(StepValue::as_ref)
        .map(|dir_id| read_direction(step, dir_id))
        .unwrap_or([0.0, 1.0, 0.0]);
    let scale = entity.args.get(3).and_then(real_from_step).unwrap_or(1.0);
    let x = normalize(axis1);
    let y = normalize(axis2);
    let z = normalize(cross(x, y));
    Affine3 {
        m: [
            [x[0] * scale, y[0] * scale, z[0] * scale, origin[0]],
            [x[1] * scale, y[1] * scale, z[1] * scale, origin[1]],
            [x[2] * scale, y[2] * scale, z[2] * scale, origin[2]],
            [0.0, 0.0, 0.0, 1.0],
        ],
    }
}

fn read_placement_origin(step: &StepFile, id: EntityId) -> [f64; 3] {
    step.entities
        .get(&id)
        .and_then(|entity| entity.args.first())
        .and_then(StepValue::as_ref)
        .and_then(|point_id| cartesian_point(step, point_id))
        .unwrap_or([0.0, 0.0, 0.0])
}

fn read_placement_axis(step: &StepFile, id: EntityId) -> [f64; 3] {
    step.entities
        .get(&id)
        .and_then(|entity| entity.args.get(1))
        .and_then(StepValue::as_ref)
        .map(|dir_id| read_direction(step, dir_id))
        .unwrap_or([0.0, 0.0, 1.0])
}

fn read_placement_ref_dir(step: &StepFile, id: EntityId) -> [f64; 3] {
    step.entities
        .get(&id)
        .and_then(|entity| entity.args.get(2))
        .and_then(StepValue::as_ref)
        .map(|dir_id| read_direction(step, dir_id))
        .unwrap_or([1.0, 0.0, 0.0])
}

fn read_direction(step: &StepFile, id: EntityId) -> [f64; 3] {
    let Some(entity) = step.entities.get(&id) else {
        return [1.0, 0.0, 0.0];
    };
    let Some(values) = entity.args.first().and_then(StepValue::as_list) else {
        return [1.0, 0.0, 0.0];
    };
    [
        values.first().and_then(real_from_step).unwrap_or(1.0),
        values.get(1).and_then(real_from_step).unwrap_or(0.0),
        values.get(2).and_then(real_from_step).unwrap_or(0.0),
    ]
}

fn cartesian_point(step: &StepFile, id: EntityId) -> Option<[f64; 3]> {
    let entity = step.entities.get(&id)?;
    let coords = entity.args.first()?.as_list()?;
    Some([
        coords.first().and_then(real_from_step).unwrap_or(0.0),
        coords.get(1).and_then(real_from_step).unwrap_or(0.0),
        coords.get(2).and_then(real_from_step).unwrap_or(0.0),
    ])
}

fn integer_from_step(value: &StepValue) -> Option<i64> {
    match value {
        StepValue::Int(v) => Some(*v),
        StepValue::Typed { value, .. } => integer_from_step(value),
        _ => None,
    }
}

fn real_from_step(value: &StepValue) -> Option<f64> {
    match value {
        StepValue::Real(v) => Some(*v),
        StepValue::Int(v) => Some(*v as f64),
        StepValue::Typed { value, .. } => real_from_step(value),
        _ => None,
    }
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len == 0.0 {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}
