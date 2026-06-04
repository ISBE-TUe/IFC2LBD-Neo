//! Port of engine_fragment's getShellData pipeline (shells/index.ts + sub-files).
//!
//! Takes a triangulated mesh (from our STEP geometry extraction) and produces
//! the coplanar-face-based Shell structure that the fragments format expects.

use std::collections::HashMap;

// ─── Settings (matches oracle's IfcImporter.geometryProcessSettings) ──────────

const PRECISION: f64 = 1_000_000.0;
const NORMAL_PRECISION: f64 = 10_000_000.0;
const PLANE_PRECISION: f64 = 1_000.0;
const THRESHOLD: usize = 3_000; // vertex count; above this → raw mode
const FACE_THRESHOLD: f64 = 0.6; // dot product for hard/soft edges

// ─── Output type ──────────────────────────────────────────────────────────────

/// Result of get_shell_data — mirrors oracle's ShellData.
pub struct ShellOutput {
    /// Deduplicated, rounded 3D points (profile indices reference these).
    pub points: Vec<[f32; 3]>,
    /// face_id → outer profile (ordered point indices, closed polygon).
    pub profiles: HashMap<usize, Vec<u32>>,
    /// face_id → holes (each hole is an ordered list of point indices).
    pub holes: HashMap<usize, Vec<Vec<u32>>>,
    /// one entry per profile (in profiles key order), the smoothing face ID.
    pub profiles_face_ids: Vec<u16>,
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

#[inline]
fn round(v: f64, p: f64) -> f64 {
    (v * p).round() / p
}

// ─── Point ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct ShellPoint {
    x: f64,
    y: f64,
    z: f64,
    hash: String,
    id: u32,
}

impl ShellPoint {
    fn new(x: f32, y: f32, z: f32, id: u32) -> Self {
        let rx = round(x as f64, PRECISION);
        let ry = round(y as f64, PRECISION);
        let rz = round(z as f64, PRECISION);
        let hash = format!("{}/{}/{}", rx, ry, rz);
        Self { x: rx, y: ry, z: rz, hash, id }
    }
}

// ─── Plane ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct ShellPlane {
    nx: f64,
    ny: f64,
    nz: f64,
    constant: f64,
    id: String,
}

impl ShellPlane {
    fn from_normal_and_point(nx: f32, ny: f32, nz: f32, px: f32, py: f32, pz: f32) -> Self {
        let rnx = round(nx as f64, NORMAL_PRECISION);
        let rny = round(ny as f64, NORMAL_PRECISION);
        let rnz = round(nz as f64, NORMAL_PRECISION);
        // Plane constant = -n·p  (THREE.js Plane.setFromNormalAndCoplanarPoint)
        let c = round(-(rnx * px as f64 + rny * py as f64 + rnz * pz as f64), PLANE_PRECISION);
        let id = format!("{}||{}||{}||{}", rnx, rny, rnz, c);
        Self { nx: rnx, ny: rny, nz: rnz, constant: c, id }
    }
}

// ─── Edge ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct ShellEdge {
    p1: ShellPoint,
    p2: ShellPoint,
    /// Canonical hash: smaller-coord point first, so AB == BA.
    hash: String,
}

impl ShellEdge {
    fn new(p1: ShellPoint, p2: ShellPoint) -> Self {
        let (a, b) = if (p1.x, p1.y, p1.z) <= (p2.x, p2.y, p2.z) {
            (p1.clone(), p2.clone())
        } else {
            (p2.clone(), p1.clone())
        };
        let hash = format!("{}_{}", a.hash, b.hash);
        Self { p1, p2, hash }
    }
}

// ─── Face ─────────────────────────────────────────────────────────────────────

struct ShellFace {
    /// All edges (hash → edge).
    edges: HashMap<String, ShellEdge>,
    /// Currently open (boundary) edge hashes.
    open_edges: std::collections::HashSet<String>,
}

impl ShellFace {
    fn new() -> Self {
        Self { edges: HashMap::new(), open_edges: std::collections::HashSet::new() }
    }

    fn add(&mut self, triangle: [ShellEdge; 3]) {
        for edge in triangle {
            if self.open_edges.contains(&edge.hash) {
                self.open_edges.remove(&edge.hash);
            } else {
                self.open_edges.insert(edge.hash.clone());
            }
            self.edges.insert(edge.hash.clone(), edge);
        }
    }

    /// An edge of `triangle` shares a boundary with this face (same plane required separately).
    fn matches(&self, triangle: &[ShellEdge; 3]) -> bool {
        triangle.iter().any(|e| self.open_edges.contains(&e.hash))
    }

    fn merge(&mut self, other: ShellFace) {
        for (hash, edge) in other.edges {
            self.edges.insert(hash, edge);
        }
        for hash in other.open_edges {
            if self.open_edges.contains(&hash) {
                self.open_edges.remove(&hash);
            } else {
                self.open_edges.insert(hash);
            }
        }
    }

    fn open_edges(&self) -> Vec<&ShellEdge> {
        self.open_edges.iter()
            .map(|h| self.edges.get(h).unwrap())
            .collect()
    }
}

// ─── Profile ──────────────────────────────────────────────────────────────────

struct ShellProfile {
    closed: bool,
    open_start: Option<String>,
    open_end: Option<String>,
    ordered_points: Vec<ShellPoint>,
    plane_normal: [f64; 3],
}

impl ShellProfile {
    fn new(plane_normal: [f64; 3]) -> Self {
        Self {
            closed: false,
            open_start: None,
            open_end: None,
            ordered_points: Vec::new(),
            plane_normal,
        }
    }

    fn match_count(&self, edge: &ShellEdge) -> usize {
        if self.closed { return 0; }
        let mut n = 0;
        if self.open_start.as_deref() == Some(&edge.p1.hash) { n += 1; }
        if self.open_start.as_deref() == Some(&edge.p2.hash) { n += 1; }
        if self.open_end.as_deref() == Some(&edge.p1.hash) { n += 1; }
        if self.open_end.as_deref() == Some(&edge.p2.hash) { n += 1; }
        n
    }

    fn add(&mut self, edge: &ShellEdge) {
        if self.ordered_points.is_empty() {
            self.open_start = Some(edge.p1.hash.clone());
            self.open_end = Some(edge.p2.hash.clone());
            self.ordered_points.push(edge.p1.clone());
            self.ordered_points.push(edge.p2.clone());
            return;
        }

        let m = self.match_count(edge);
        if m == 2 {
            self.closed = true;
            self.open_start = None;
            self.open_end = None;
            return;
        }
        if m == 0 { return; }

        if self.open_start.as_deref() == Some(&edge.p1.hash) {
            self.ordered_points.insert(0, edge.p2.clone());
            self.open_start = Some(edge.p2.hash.clone());
        } else if self.open_end.as_deref() == Some(&edge.p1.hash) {
            self.ordered_points.push(edge.p2.clone());
            self.open_end = Some(edge.p2.hash.clone());
        } else if self.open_start.as_deref() == Some(&edge.p2.hash) {
            self.ordered_points.insert(0, edge.p1.clone());
            self.open_start = Some(edge.p1.hash.clone());
        } else if self.open_end.as_deref() == Some(&edge.p2.hash) {
            self.ordered_points.push(edge.p1.clone());
            self.open_end = Some(edge.p1.hash.clone());
        }
    }

    fn get_indices(&self) -> Vec<u32> {
        self.ordered_points.iter().map(|p| p.id).collect()
    }

    /// 2D projected area (Shoelace), dropping the dominant normal axis.
    fn get_area(&self) -> f64 {
        let (nx, ny, nz) = (self.plane_normal[0].abs(), self.plane_normal[1].abs(), self.plane_normal[2].abs());
        let (d1, d2) = if nx >= ny && nx >= nz { (1, 2) }
                       else if ny >= nx && ny >= nz { (0, 2) }
                       else { (0, 1) };
        let pts: Vec<[f64; 3]> = self.ordered_points.iter().map(|p| [p.x, p.y, p.z]).collect();
        let n = pts.len();
        let mut total = 0.0f64;
        for i in 0..n {
            let j = (i + 1) % n;
            total += pts[i][d1] * pts[j][d2] * 0.5;
            total -= pts[j][d1] * pts[i][d2] * 0.5;
        }
        total.abs()
    }

    fn get_edges(&self, reverse: bool) -> Vec<ShellEdge> {
        let pts = &self.ordered_points;
        if reverse {
            (1..pts.len()).rev().map(|i| ShellEdge::new(pts[i].clone(), pts[i-1].clone())).collect()
        } else {
            (0..pts.len()-1).map(|i| ShellEdge::new(pts[i].clone(), pts[i+1].clone())).collect()
        }
    }

    fn merge(&mut self, other: &ShellProfile) {
        // Determine orientation
        let reverse = other.open_end == self.open_end || other.open_end == self.open_start;
        let edges = other.get_edges(reverse);
        for edge in &edges {
            self.add(edge);
        }
    }
}

// ─── ProfileSet ───────────────────────────────────────────────────────────────

struct ProfileSet {
    list: HashMap<usize, ShellProfile>,
    next_id: usize,
    plane_normal: [f64; 3],
}

impl ProfileSet {
    fn new(plane_normal: [f64; 3]) -> Self {
        Self { list: HashMap::new(), next_id: 0, plane_normal }
    }

    fn add(&mut self, edge: &ShellEdge) {
        let matches: Vec<usize> = self.list.iter()
            .filter(|(_, p)| p.match_count(edge) > 0)
            .map(|(id, _)| *id)
            .collect();

        match matches.len() {
            0 => {
                let id = self.next_id;
                self.next_id += 1;
                let mut p = ShellProfile::new(self.plane_normal);
                p.add(edge);
                self.list.insert(id, p);
            }
            1 => {
                self.list.get_mut(&matches[0]).unwrap().add(edge);
            }
            _ => {
                // Merge all into first
                self.list.get_mut(&matches[0]).unwrap().add(edge);
                // Collect other profiles to merge
                let others: Vec<usize> = matches[1..].to_vec();
                for other_id in others {
                    if let Some(other) = self.list.remove(&other_id) {
                        let base = self.list.get_mut(&matches[0]).unwrap();
                        base.merge(&other);
                    }
                }
            }
        }
    }

    /// Returns (outer_profile_indices, holes).
    fn get_profiles(&self) -> Option<(Vec<u32>, Vec<Vec<u32>>)> {
        let mut biggest_id = None;
        let mut biggest_area = 0.0f64;
        for (id, p) in &self.list {
            let area = p.get_area();
            if area > biggest_area {
                biggest_area = area;
                biggest_id = Some(*id);
            }
        }
        let biggest_id = biggest_id?;
        let profile = self.list.get(&biggest_id)?.get_indices();
        let holes: Vec<Vec<u32>> = self.list.iter()
            .filter(|(id, _)| **id != biggest_id)
            .map(|(_, p)| p.get_indices())
            .collect();
        Some((profile, holes))
    }
}

// (FaceCollection removed — plane grouping done inline in get_shell_data)

// ─── Raw mode (when vertex count > threshold) ─────────────────────────────────

/// Raw shell: one profile per triangle (welded points), no coplanar merging.
/// Guaranteed faithful to the input triangulation — used for meshes whose
/// triangulation the coplanar boundary tracer can't robustly reconstruct
/// (e.g. ifc-lite's arbitrary fan/strip triangulations).
pub fn get_raw_shell_data(
    position: &[[f32; 3]],
    triangles: &[[u32; 3]],
) -> ShellOutput {
    let mut point_map: HashMap<String, (u32, [f32; 3])> = HashMap::new();
    let mut profiles: HashMap<usize, Vec<u32>> = HashMap::new();

    let get_or_insert = |x: f32, y: f32, z: f32,
                          map: &mut HashMap<String, (u32, [f32; 3])>| -> u32 {
        let rx = round(x as f64, PRECISION) as f32;
        let ry = round(y as f64, PRECISION) as f32;
        let rz = round(z as f64, PRECISION) as f32;
        let key = format!("{},{},{}", rx, ry, rz);
        if let Some((id, _)) = map.get(&key) {
            *id
        } else {
            let id = map.len() as u32;
            map.insert(key, (id, [rx, ry, rz]));
            id
        }
    };

    for (i, tri) in triangles.iter().enumerate() {
        let [i1, i2, i3] = *tri;
        let p1 = position[i1 as usize];
        let p2 = position[i2 as usize];
        let p3 = position[i3 as usize];
        let id1 = get_or_insert(p1[0], p1[1], p1[2], &mut point_map);
        let id2 = get_or_insert(p2[0], p2[1], p2[2], &mut point_map);
        let id3 = get_or_insert(p3[0], p3[1], p3[2], &mut point_map);
        profiles.insert(i, vec![id1, id2, id3]);
    }

    let mut points_ordered: Vec<(u32, [f32; 3])> = point_map.into_values().collect();
    points_ordered.sort_by_key(|(id, _)| *id);
    let points: Vec<[f32; 3]> = points_ordered.into_iter().map(|(_, p)| p).collect();

    let profiles_face_ids: Vec<u16> = (0..profiles.len()).map(|i| i as u16).collect();

    ShellOutput {
        points,
        profiles,
        holes: HashMap::new(),
        profiles_face_ids,
    }
}

// ─── Main function ─────────────────────────────────────────────────────────────

/// Port of GeomsFbUtils.getShellData.
///
/// `position`: per-vertex positions.
/// `normals`:  per-vertex normals (flat: 3 floats per vertex).
/// `triangles`: triangle index triples.
pub fn get_shell_data(
    position: &[[f32; 3]],
    _normals: &[[f32; 3]], // retained for API compatibility; plane grouping uses geometric normals
    triangles: &[[u32; 3]],
) -> ShellOutput {
    let vertex_count = position.len();

    if vertex_count == 0 || triangles.is_empty() {
        return ShellOutput {
            points: Vec::new(),
            profiles: HashMap::new(),
            holes: HashMap::new(),
            profiles_face_ids: Vec::new(),
        };
    }

    // raw mode for large meshes
    if vertex_count > THRESHOLD {
        return get_raw_shell_data(position, triangles);
    }

    // ── Step 1: group triangle indices by plane ────────────────────────────────
    // plane_id → (plane, list of triangle indices)
    let mut plane_map: HashMap<String, (ShellPlane, Vec<usize>)> = HashMap::new();

    for (tri_idx, &[i1, i2, i3]) in triangles.iter().enumerate() {
        // Plane membership is a GEOMETRIC property — derive the plane from the triangle's
        // own face normal (cross product of its edges), NOT from a stored vertex normal.
        // Vertex normals are frequently smooth/averaged (ifc-lite and many exporters share
        // a corner vertex between perpendicular faces), so keying the plane on a vertex
        // normal mis-buckets coplanar triangles and makes the boundary tracer stitch
        // non-coplanar regions into giant spanning polygons.
        let p = position[i1 as usize];
        let p2 = position[i2 as usize];
        let p3 = position[i3 as usize];
        let ab = [(p2[0] - p[0]) as f64, (p2[1] - p[1]) as f64, (p2[2] - p[2]) as f64];
        let ac = [(p3[0] - p[0]) as f64, (p3[1] - p[1]) as f64, (p3[2] - p[2]) as f64];
        let mut nx = ab[1] * ac[2] - ab[2] * ac[1];
        let mut ny = ab[2] * ac[0] - ab[0] * ac[2];
        let mut nz = ab[0] * ac[1] - ab[1] * ac[0];
        let len = (nx * nx + ny * ny + nz * nz).sqrt();
        if len < 1e-12 {
            continue; // degenerate triangle — contributes no face
        }
        nx /= len; ny /= len; nz /= len;
        let plane = ShellPlane::from_normal_and_point(nx as f32, ny as f32, nz as f32, p[0], p[1], p[2]);
        plane_map.entry(plane.id.clone())
            .or_insert_with(|| (plane, Vec::new()))
            .1.push(tri_idx);
    }

    // ── Step 2: build Points dedup map ────────────────────────────────────────
    let mut point_by_hash: HashMap<String, ShellPoint> = HashMap::new();

    let get_or_create_point = |x: f32, y: f32, z: f32,
                                map: &mut HashMap<String, ShellPoint>| -> ShellPoint {
        let p = ShellPoint::new(x, y, z, map.len() as u32);
        if !map.contains_key(&p.hash) {
            map.insert(p.hash.clone(), p.clone());
        }
        map.get(&p.hash).unwrap().clone()
    };

    // ── Step 3: for each plane, build Face (open edge tracking) ───────────────
    let mut plane_faces: Vec<(ShellPlane, ShellFace)> = Vec::new();

    for (_, (plane, tri_indices)) in plane_map {
        let mut face = ShellFace::new();
        let mut valid = true;

        for tri_idx in &tri_indices {
            let [i1, i2, i3] = triangles[*tri_idx];
            let p1pos = position[i1 as usize];
            let p2pos = position[i2 as usize];
            let p3pos = position[i3 as usize];

            // Degenerate triangle check (oracle: isValidTriangle)
            let point_precision = (1.0 / PRECISION) * 10.0;
            let d12 = {
                let dx = (p1pos[0] - p2pos[0]) as f64;
                let dy = (p1pos[1] - p2pos[1]) as f64;
                let dz = (p1pos[2] - p2pos[2]) as f64;
                (dx*dx + dy*dy + dz*dz).sqrt()
            };
            let d13 = {
                let dx = (p1pos[0] - p3pos[0]) as f64;
                let dy = (p1pos[1] - p3pos[1]) as f64;
                let dz = (p1pos[2] - p3pos[2]) as f64;
                (dx*dx + dy*dy + dz*dz).sqrt()
            };
            let d23 = {
                let dx = (p2pos[0] - p3pos[0]) as f64;
                let dy = (p2pos[1] - p3pos[1]) as f64;
                let dz = (p2pos[2] - p3pos[2]) as f64;
                (dx*dx + dy*dy + dz*dz).sqrt()
            };
            if d12 <= point_precision || d13 <= point_precision || d23 <= point_precision {
                continue; // degenerate
            }

            let pt1 = get_or_create_point(p1pos[0], p1pos[1], p1pos[2], &mut point_by_hash);
            let pt2 = get_or_create_point(p2pos[0], p2pos[1], p2pos[2], &mut point_by_hash);
            let pt3 = get_or_create_point(p3pos[0], p3pos[1], p3pos[2], &mut point_by_hash);

            let e1 = ShellEdge::new(pt1.clone(), pt2.clone());
            let e2 = ShellEdge::new(pt2.clone(), pt3.clone());
            let e3 = ShellEdge::new(pt3, pt1);

            // Merge triangles that share open edges with existing face groups within this plane
            // (oracle uses Faces.add which finds matching faces and merges them)
            // Here we use a simpler: all triangles in the same plane → same face
            face.add([e1, e2, e3]);
        }

        if !face.open_edges.is_empty() || !face.edges.is_empty() {
            plane_faces.push((plane, face));
        }
    }

    // ── Step 4: for each face, build profile from open edges ──────────────────
    let mut profiles: HashMap<usize, Vec<u32>> = HashMap::new();
    let mut holes: HashMap<usize, Vec<Vec<u32>>> = HashMap::new();
    let mut face_counter = 0usize;

    let mut raw_fallback = false;

    for (plane, face) in &plane_faces {
        let open = face.open_edges();
        if open.is_empty() {
            // oracle: if no open edges → use raw fallback
            raw_fallback = true;
            break;
        }

        let plane_normal = [plane.nx, plane.ny, plane.nz];
        let mut profile_set = ProfileSet::new(plane_normal);
        for edge in open {
            profile_set.add(edge);
        }

        if let Some((profile, face_holes)) = profile_set.get_profiles() {
            if !profile.is_empty() {
                profiles.insert(face_counter, profile);
                if !face_holes.is_empty() {
                    holes.insert(face_counter, face_holes);
                }
            }
        }
        face_counter += 1;
    }

    if raw_fallback {
        return get_raw_shell_data(position, triangles);
    }

    // Filter out empty profiles (oracle filters in getShellData)
    let filtered_profiles: HashMap<usize, Vec<u32>> = profiles.into_iter()
        .filter(|(_, p)| !p.is_empty())
        .enumerate()
        .map(|(new_id, (_, v))| (new_id, v))
        .collect();

    let filtered_holes: HashMap<usize, Vec<Vec<u32>>> = holes.into_iter()
        .filter(|(_, h)| !h.is_empty())
        .collect();

    // ── Step 5: compute profiles_face_ids ─────────────────────────────────────
    // (oracle: computeShellFaceIds — group profiles into smooth face IDs)
    let profiles_face_ids = compute_face_ids(&filtered_profiles, &filtered_holes,
                                              &point_by_hash);

    // Build final points array in ID order
    let mut points_ordered: Vec<ShellPoint> = point_by_hash.into_values().collect();
    points_ordered.sort_by_key(|p| p.id);
    let points: Vec<[f32; 3]> = points_ordered.iter()
        .map(|p| [p.x as f32, p.y as f32, p.z as f32])
        .collect();

    ShellOutput { points, profiles: filtered_profiles, holes: filtered_holes, profiles_face_ids }
}

// ─── computeShellFaceIds port ─────────────────────────────────────────────────

fn compute_face_ids(
    profiles: &HashMap<usize, Vec<u32>>,
    _holes: &HashMap<usize, Vec<Vec<u32>>>,
    point_map: &HashMap<String, ShellPoint>,
) -> Vec<u16> {
    let n = profiles.len();
    if n == 0 { return Vec::new(); }

    // Build a point lookup by ID for normal computation
    let mut pts_by_id: Vec<[f64; 3]> = Vec::new();
    for p in point_map.values() {
        let id = p.id as usize;
        if id >= pts_by_id.len() { pts_by_id.resize(id + 1, [0.0; 3]); }
        pts_by_id[id] = [p.x, p.y, p.z];
    }

    // Compute a normal for each profile (from first valid triangle in profile)
    let mut profile_normals: HashMap<usize, [f64; 3]> = HashMap::new();
    for (&pid, profile) in profiles {
        let norm = compute_profile_normal(profile, &pts_by_id);
        profile_normals.insert(pid, norm);
    }

    // Build edge → [profile_ids] adjacency
    let mut edge_to_profiles: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut profile_edges: HashMap<usize, Vec<u64>> = HashMap::new();

    for (&pid, profile) in profiles {
        let len = profile.len();
        for j in 0..len {
            let a = profile[j];
            let b = profile[(j + 1) % len];
            let (lo, hi) = if a < b { (a, b) } else { (b, a) };
            // Pack two u32s into a u64
            let eid = ((lo as u64) << 32) | (hi as u64);
            edge_to_profiles.entry(eid).or_default().push(pid);
            profile_edges.entry(pid).or_default().push(eid);
        }
    }

    // Assign face IDs using same algorithm as oracle
    let mut face_id_per_profile: HashMap<usize, u16> = HashMap::new();
    let mut next_face_id: u16 = 0;

    let mut sorted_pids: Vec<usize> = profiles.keys().copied().collect();
    sorted_pids.sort_unstable();

    for &pid in &sorted_pids {
        let fid = *face_id_per_profile.entry(pid).or_insert_with(|| {
            let id = next_face_id;
            next_face_id += 1;
            id
        });
        let n1 = profile_normals.get(&pid).copied().unwrap_or([0.0, 0.0, 1.0]);

        if let Some(edges) = profile_edges.get(&pid) {
            for &eid in edges {
                if let Some(neighbors) = edge_to_profiles.get(&eid) {
                    for &other_pid in neighbors {
                        if other_pid == pid { continue; }
                        let n2 = profile_normals.get(&other_pid).copied().unwrap_or([0.0, 0.0, 1.0]);
                        let dot = (n1[0]*n2[0] + n1[1]*n2[1] + n1[2]*n2[2]).abs();
                        let is_hard = dot < FACE_THRESHOLD;

                        if face_id_per_profile.contains_key(&other_pid) {
                            if !is_hard {
                                // Merge: replace all occurrences of other's face id with current
                                let other_fid = face_id_per_profile[&other_pid];
                                for v in face_id_per_profile.values_mut() {
                                    if *v == other_fid { *v = fid; }
                                }
                            }
                        } else {
                            let new_fid = if is_hard { let f = next_face_id; next_face_id += 1; f }
                                         else { fid };
                            face_id_per_profile.insert(other_pid, new_fid);
                        }
                    }
                }
            }
        }
    }

    // Output in sorted profile key order
    sorted_pids.iter()
        .map(|pid| face_id_per_profile.get(pid).copied().unwrap_or(0))
        .collect()
}

fn compute_profile_normal(profile: &[u32], pts: &[[f64; 3]]) -> [f64; 3] {
    let n = profile.len();
    if n < 3 { return [0.0, 0.0, 1.0]; }
    // Try consecutive triples until we get a non-zero normal
    for i in 0..n.saturating_sub(2) {
        let p1 = pts.get(profile[i] as usize).copied().unwrap_or([0.0; 3]);
        let p2 = pts.get(profile[i+1] as usize).copied().unwrap_or([0.0; 3]);
        let p3 = pts.get(profile[i+2] as usize).copied().unwrap_or([0.0; 3]);
        let ab = [p2[0]-p1[0], p2[1]-p1[1], p2[2]-p1[2]];
        let ac = [p3[0]-p1[0], p3[1]-p1[1], p3[2]-p1[2]];
        let cx = ab[1]*ac[2] - ab[2]*ac[1];
        let cy = ab[2]*ac[0] - ab[0]*ac[2];
        let cz = ab[0]*ac[1] - ab[1]*ac[0];
        let len = (cx*cx + cy*cy + cz*cz).sqrt();
        if len > 1e-12 {
            return [cx/len, cy/len, cz/len];
        }
    }
    [0.0, 0.0, 1.0]
}
