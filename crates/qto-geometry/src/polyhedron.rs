//! Exact volume and surface area for polyhedral solids.
//!
//! A polyhedron's volume is *exactly* computable by elementary means — the
//! divergence theorem turns the volume integral into a sum of signed tetrahedra,
//! one per triangle, with no approximation anywhere. No B-rep kernel is needed
//! or would help: `IfcFacetedBrep`, `IfcTriangulatedFaceSet`, `IfcPolygonalFaceSet`
//! and `IfcFaceBasedSurfaceModel` are all exactly measurable here.
//!
//! The previous implementation got this wrong in two ways, both measured as a
//! systematic over-estimate (+7.24% median on faceted breps across the
//! validation corpus):
//!
//! 1. **It discarded inner bounds.** A face's holes were skipped entirely, so
//!    material that isn't there was counted as present.
//! 2. **It fan-triangulated from vertex 0.** That is only valid for *convex*
//!    polygons. Real IFC faces are routinely concave — the element that exposed
//!    this had 12- and 20-vertex loops — and a fan across a concave polygon
//!    emits overlapping and inverted triangles.
//!
//! Both are fixed here by projecting each planar face into its own plane and
//! running a proper ear-clipping triangulation that understands holes.

use ifc_lite_geometry::triangulation::triangulate_polygon_with_holes;
use nalgebra::Point2;

/// One boundary loop of a face, in 3D. IFC gives these as `IfcPolyLoop`s.
pub type Loop = Vec<[f64; 3]>;

/// A planar face: one outer boundary and zero or more holes.
#[derive(Debug, Clone, Default)]
pub struct Face {
    pub outer: Loop,
    pub inner: Vec<Loop>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PolyhedronMetrics {
    /// Enclosed volume. Exact for a closed, consistently-oriented polyhedron.
    pub volume: f64,
    /// Outer surface area: holes excluded, and interior partitions between
    /// abutting sub-solids excluded.
    pub surface_area: f64,
    /// Number of triangles the faces resolved to — a proxy for whether
    /// triangulation actually succeeded.
    pub triangle_count: usize,
    /// Extents of the axis-aligned bounding box in the solid's own coordinate
    /// frame, ascending: `[smallest, middle, largest]`.
    ///
    /// Unlike a bbox assembled from a whole representation graph, every vertex
    /// here comes from one solid in one frame, so this is a real measurement of
    /// that solid rather than a union of unrelated point sets. It is still only
    /// an *envelope*: see [`PolyhedronMetrics::fills_extent`] before using it as
    /// a dimension.
    pub extent: [f64; 3],
}

impl PolyhedronMetrics {
    /// Does the solid fill its own bounding box?
    ///
    /// True exactly when the polyhedron *is* that box, and then — and only then
    /// — its extents are its dimensions rather than an over-estimate. A diagonal
    /// brace or an L-shaped part fills far less than its box, which is why a
    /// bounding box is not a source of quantities in general.
    pub fn fills_extent(&self) -> bool {
        let bbox = self.extent[0] * self.extent[1] * self.extent[2];
        bbox > 0.0 && ((self.volume - bbox).abs() / bbox) < 1e-9
    }
}

/// Why a solid could not be measured.
///
/// Under the project rule that a missing quantity beats a wrong one, every one
/// of these results in *no value*, never a fallback estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolyhedronError {
    /// Fewer than four faces cannot bound a volume.
    NotEnoughFaces,
    /// No face survived triangulation.
    TriangulationFailed,
    /// The shell is not closed, so "enclosed volume" is undefined.
    ///
    /// Detected by Euler-style edge parity: in a closed manifold every edge is
    /// shared by exactly two triangles, traversed once in each direction.
    NotClosed,
}

/// Compute exact metrics for a polyhedron given its faces.
///
/// Returns `Err` rather than an approximation whenever the input cannot support
/// an exact answer.
pub fn metrics(faces: &[Face]) -> Result<PolyhedronMetrics, PolyhedronError> {
    if faces.len() < 4 {
        return Err(PolyhedronError::NotEnoughFaces);
    }

    // Which faces are interior partitions must be decided from the *faces*, not
    // from their triangles: two coincident faces are triangulated independently
    // and may be split along different diagonals, so their triangles need not
    // match even though the faces do.
    let internal = internal_faces(faces);

    let mut triangles: Vec<[[f64; 3]; 3]> = Vec::new();
    let mut tri_face: Vec<usize> = Vec::new();
    for (i, face) in faces.iter().enumerate() {
        let before = triangles.len();
        triangulate_face(face, &mut triangles);
        tri_face.resize(triangles.len(), i);
        debug_assert!(triangles.len() >= before);
    }
    if triangles.is_empty() {
        return Err(PolyhedronError::TriangulationFailed);
    }
    if !is_closed(&triangles) {
        return Err(PolyhedronError::NotClosed);
    }

    // Solids assembled from abutting blocks are routinely exported as one shell
    // with the shared partitions left in. Those internal faces cancel pairwise in
    // the volume sum, so the volume stays exact — but each one is counted in a
    // naive area sum, inflating the surface area. Measured on real reinforcement
    // parts this produced an area ratio of 1.9667 against an exactly correct
    // volume for the same elements.
    let mut volume6 = 0.0_f64;
    let mut area2 = 0.0_f64;
    for (i, &[a, b, c]) in triangles.iter().enumerate() {
        // Signed tetrahedron against the origin: a · (b × c) / 6. Summed over a
        // closed surface this is exactly the enclosed volume, origin-independent.
        // Internal faces are kept here: they cancel, and dropping only one of a
        // pair would corrupt the sum.
        volume6 += dot(a, cross(b, c));
        if !internal[tri_face[i]] {
            area2 += norm(cross(sub(b, a), sub(c, a)));
        }
    }

    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for &[a, b, c] in &triangles {
        for p in [a, b, c] {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
    }
    let mut extent = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    extent.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    Ok(PolyhedronMetrics {
        extent,
        // abs() because a consistently-oriented shell may be wound either way;
        // only mixed winding would be wrong, and `is_closed` does not detect
        // that on its own.
        volume: (volume6 / 6.0).abs(),
        surface_area: area2 / 2.0,
        triangle_count: triangles.len(),
    })
}

/// Project a planar face into its own plane, triangulate with holes, map back.
fn triangulate_face(face: &Face, out: &mut Vec<[[f64; 3]; 3]>) {
    if face.outer.len() < 3 {
        return;
    }

    // Newell's method: robust for non-planar-ish and concave polygons alike,
    // unlike taking the cross product of the first two edges.
    let normal = newell_normal(&face.outer);
    let n = norm(normal);
    if n < 1e-15 {
        return; // degenerate face, no area, contributes nothing
    }
    let w = scale(normal, 1.0 / n);
    let (u, v) = basis_from_normal(w);
    let origin = face.outer[0];

    let to_2d = |p: [f64; 3]| {
        let d = sub(p, origin);
        Point2::new(dot(d, u), dot(d, v))
    };

    let outer2: Vec<Point2<f64>> = face.outer.iter().map(|&p| to_2d(p)).collect();
    let holes2: Vec<Vec<Point2<f64>>> = face
        .inner
        .iter()
        .filter(|h| h.len() >= 3)
        .map(|h| h.iter().map(|&p| to_2d(p)).collect())
        .collect();

    let Ok(indices) = triangulate_polygon_with_holes(&outer2, &holes2) else {
        return;
    };

    // The triangulator indexes into outer followed by each hole, in order.
    let mut verts: Vec<[f64; 3]> = face.outer.clone();
    for h in face.inner.iter().filter(|h| h.len() >= 3) {
        verts.extend_from_slice(h);
    }

    for tri in indices.chunks_exact(3) {
        let (Some(&a), Some(&b), Some(&c)) =
            (verts.get(tri[0]), verts.get(tri[1]), verts.get(tri[2]))
        else {
            continue;
        };
        out.push([a, b, c]);
    }
}

/// Mark triangles that are interior partitions rather than boundary surface.
///
/// A face shared by two abutting sub-solids appears twice with opposite winding.
/// Neither copy bounds the outside, so neither belongs in a surface area.
fn internal_faces(faces: &[Face]) -> Vec<bool> {
    use std::collections::HashMap;

    fn key(p: [f64; 3]) -> (i64, i64, i64) {
        const Q: f64 = 1.0e6;
        (
            (p[0] * Q).round() as i64,
            (p[1] * Q).round() as i64,
            (p[2] * Q).round() as i64,
        )
    }

    // Orientation-independent identity: the same vertex set, in any order and
    // either winding. Holes are included so a face with a bore does not match a
    // solid one of the same outline.
    let ids: Vec<Vec<(i64, i64, i64)>> = faces
        .iter()
        .map(|f| {
            let mut k: Vec<_> = f
                .outer
                .iter()
                .chain(f.inner.iter().flatten())
                .map(|&p| key(p))
                .collect();
            k.sort_unstable();
            k.dedup();
            k
        })
        .collect();

    let mut seen: HashMap<&Vec<(i64, i64, i64)>, usize> = HashMap::new();
    for id in &ids {
        *seen.entry(id).or_insert(0) += 1;
    }
    // Exactly two coincident copies is a shared partition. Three or more is
    // malformed input and is left alone rather than guessed at.
    ids.iter().map(|id| seen[id] == 2).collect()
}

/// Is the triangle soup a closed surface?
///
/// Every directed edge must have exactly one opposite-directed partner. This
/// catches the open shells and missing faces that would otherwise yield a
/// confident but meaningless volume.
fn is_closed(triangles: &[[[f64; 3]; 3]]) -> bool {
    use std::collections::HashMap;

    // Quantise to 1 µm before hashing: vertices that are numerically distinct
    // but geometrically identical must match, or every shell looks open.
    fn key(p: [f64; 3]) -> (i64, i64, i64) {
        const Q: f64 = 1.0e6;
        (
            (p[0] * Q).round() as i64,
            (p[1] * Q).round() as i64,
            (p[2] * Q).round() as i64,
        )
    }

    let mut edges: HashMap<((i64, i64, i64), (i64, i64, i64)), i32> = HashMap::new();
    for &[a, b, c] in triangles {
        for (p, q) in [(a, b), (b, c), (c, a)] {
            let (kp, kq) = (key(p), key(q));
            if kp == kq {
                continue; // degenerate edge
            }
            // Count each undirected edge, signed by traversal direction.
            let (lo, hi, dir) = if kp < kq { (kp, kq, 1) } else { (kq, kp, -1) };
            *edges.entry((lo, hi)).or_insert(0) += dir;
        }
    }
    // Closed and consistently oriented => every edge nets to zero.
    !edges.is_empty() && edges.values().all(|&v| v == 0)
}

fn newell_normal(points: &[[f64; 3]]) -> [f64; 3] {
    let mut n = [0.0; 3];
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    n
}

/// An arbitrary orthonormal basis in the plane with normal `w`.
fn basis_from_normal(w: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    // Pick a seed that is never parallel to w.
    let seed = if w[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u = cross(w, seed);
    let u = scale(u, 1.0 / norm(u));
    let v = cross(w, u);
    (u, v)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(p: [[f64; 3]; 4]) -> Face {
        Face {
            outer: p.to_vec(),
            inner: vec![],
        }
    }

    /// Axis-aligned box, outward-oriented.
    fn unit_box(sx: f64, sy: f64, sz: f64) -> Vec<Face> {
        let v = |x: f64, y: f64, z: f64| [x * sx, y * sy, z * sz];
        vec![
            quad([v(0., 0., 0.), v(0., 1., 0.), v(1., 1., 0.), v(1., 0., 0.)]), // bottom
            quad([v(0., 0., 1.), v(1., 0., 1.), v(1., 1., 1.), v(0., 1., 1.)]), // top
            quad([v(0., 0., 0.), v(1., 0., 0.), v(1., 0., 1.), v(0., 0., 1.)]),
            quad([v(1., 0., 0.), v(1., 1., 0.), v(1., 1., 1.), v(1., 0., 1.)]),
            quad([v(1., 1., 0.), v(0., 1., 0.), v(0., 1., 1.), v(1., 1., 1.)]),
            quad([v(0., 1., 0.), v(0., 0., 0.), v(0., 0., 1.), v(0., 1., 1.)]),
        ]
    }

    #[test]
    fn box_volume_and_area_are_exact() {
        let m = metrics(&unit_box(2.0, 3.0, 4.0)).expect("closed box");
        assert!((m.volume - 24.0).abs() < 1e-12, "volume {}", m.volume);
        let expect_area = 2.0 * (2.0 * 3.0 + 3.0 * 4.0 + 2.0 * 4.0);
        assert!(
            (m.surface_area - expect_area).abs() < 1e-12,
            "area {}",
            m.surface_area
        );
    }

    /// The defect that produced +7.24% on real beams: a concave face
    /// fan-triangulated from vertex 0 emits overlapping triangles.
    #[test]
    fn concave_faces_are_measured_correctly() {
        // L-shaped prism: footprint area 6, height 2 => volume 12.
        let l = [
            [0.0, 0.0],
            [4.0, 0.0],
            [4.0, 1.0],
            [1.0, 1.0],
            [1.0, 3.0],
            [0.0, 3.0],
        ];
        let h = 2.0;
        let bottom: Loop = l.iter().rev().map(|&[x, y]| [x, y, 0.0]).collect();
        let top: Loop = l.iter().map(|&[x, y]| [x, y, h]).collect();
        let mut faces = vec![
            Face { outer: bottom, inner: vec![] },
            Face { outer: top, inner: vec![] },
        ];
        for i in 0..l.len() {
            let a = l[i];
            let b = l[(i + 1) % l.len()];
            faces.push(quad([
                [a[0], a[1], 0.0],
                [b[0], b[1], 0.0],
                [b[0], b[1], h],
                [a[0], a[1], h],
            ]));
        }
        let m = metrics(&faces).expect("closed L-prism");
        assert!((m.volume - 12.0).abs() < 1e-9, "volume {}", m.volume);
    }

    /// The other defect: inner bounds were skipped entirely, so a hole through
    /// a solid was counted as solid material.
    #[test]
    fn holes_in_faces_are_subtracted() {
        // 10x10x2 slab with a 2x2 square hole punched all the way through.
        // Volume = 200 - 2*2*2 = 192.
        let outer_bottom: Loop = vec![
            [0., 0., 0.], [0., 10., 0.], [10., 10., 0.], [10., 0., 0.],
        ];
        let hole_bottom: Loop = vec![
            [4., 4., 0.], [6., 4., 0.], [6., 6., 0.], [4., 6., 0.],
        ];
        let outer_top: Loop = vec![
            [0., 0., 2.], [10., 0., 2.], [10., 10., 2.], [0., 10., 2.],
        ];
        let hole_top: Loop = vec![
            [4., 4., 2.], [4., 6., 2.], [6., 6., 2.], [6., 4., 2.],
        ];
        let mut faces = vec![
            Face { outer: outer_bottom, inner: vec![hole_bottom.clone()] },
            Face { outer: outer_top, inner: vec![hole_top.clone()] },
        ];
        // outer walls
        let o = [[0., 0.], [10., 0.], [10., 10.], [0., 10.]];
        for i in 0..4 {
            let a = o[i];
            let b = o[(i + 1) % 4];
            faces.push(quad([
                [a[0], a[1], 0.], [b[0], b[1], 0.], [b[0], b[1], 2.], [a[0], a[1], 2.],
            ]));
        }
        // hole walls (inward)
        let hh = [[4., 4.], [6., 4.], [6., 6.], [4., 6.]];
        for i in 0..4 {
            let a = hh[i];
            let b = hh[(i + 1) % 4];
            faces.push(quad([
                [b[0], b[1], 0.], [a[0], a[1], 0.], [a[0], a[1], 2.], [b[0], b[1], 2.],
            ]));
        }
        let m = metrics(&faces).expect("closed slab with bore");
        assert!((m.volume - 192.0).abs() < 1e-9, "volume {} != 192", m.volume);
    }

    #[test]
    fn open_shells_are_refused_rather_than_guessed() {
        let mut faces = unit_box(1.0, 1.0, 1.0);
        faces.pop(); // remove a wall
        assert_eq!(metrics(&faces), Err(PolyhedronError::NotClosed));
    }

    #[test]
    fn too_few_faces_is_refused() {
        assert_eq!(
            metrics(&unit_box(1.0, 1.0, 1.0)[..2]),
            Err(PolyhedronError::NotEnoughFaces)
        );
    }

    #[test]
    fn volume_is_independent_of_where_the_origin_sits() {
        let near = metrics(&unit_box(2.0, 3.0, 4.0)).unwrap();
        let shifted: Vec<Face> = unit_box(2.0, 3.0, 4.0)
            .into_iter()
            .map(|f| Face {
                outer: f.outer.iter().map(|p| [p[0] + 1e5, p[1] - 3e4, p[2] + 7e4]).collect(),
                inner: vec![],
            })
            .collect();
        let far = metrics(&shifted).unwrap();
        assert!(
            (near.volume - far.volume).abs() < 1e-6,
            "{} vs {}",
            near.volume,
            far.volume
        );
    }
}

#[cfg(test)]
mod internal_face_tests {
    use super::*;

    fn quad(p: [[f64; 3]; 4]) -> Face {
        Face { outer: p.to_vec(), inner: vec![] }
    }

    /// Two unit cubes sharing a face, exported as one shell with the shared
    /// partition left in — the real pattern behind an area ratio of ~1.97
    /// against an exactly correct volume.
    #[test]
    fn interior_partitions_are_excluded_from_surface_area() {
        let v = |x: f64, y: f64, z: f64| [x, y, z];
        let mut faces = Vec::new();
        // Two boxes: x in [0,1] and x in [1,2], both y,z in [0,1].
        for x0 in [0.0, 1.0] {
            let x1 = x0 + 1.0;
            faces.extend([
                quad([v(x0,0.,0.), v(x0,1.,0.), v(x1,1.,0.), v(x1,0.,0.)]),
                quad([v(x0,0.,1.), v(x1,0.,1.), v(x1,1.,1.), v(x0,1.,1.)]),
                quad([v(x0,0.,0.), v(x1,0.,0.), v(x1,0.,1.), v(x0,0.,1.)]),
                quad([v(x1,0.,0.), v(x1,1.,0.), v(x1,1.,1.), v(x1,0.,1.)]),
                quad([v(x1,1.,0.), v(x0,1.,0.), v(x0,1.,1.), v(x1,1.,1.)]),
                quad([v(x0,1.,0.), v(x0,0.,0.), v(x0,0.,1.), v(x0,1.,1.)]),
            ]);
        }
        let m = metrics(&faces).expect("closed");
        // Volume is unaffected: the shared partition cancels.
        assert!((m.volume - 2.0).abs() < 1e-9, "volume {}", m.volume);
        // Outer surface of a 2x1x1 box = 2(2+2+1) = 10. Counting the shared
        // partition twice would give 12.
        assert!(
            (m.surface_area - 10.0).abs() < 1e-9,
            "surface {} should be 10, not 12",
            m.surface_area
        );
    }
}

#[cfg(test)]
mod extent_tests {
    use super::*;

    fn quad(p: [[f64; 3]; 4]) -> Face {
        Face { outer: p.to_vec(), inner: vec![] }
    }

    fn boxed(sx: f64, sy: f64, sz: f64) -> Vec<Face> {
        let v = |x: f64, y: f64, z: f64| [x * sx, y * sy, z * sz];
        vec![
            quad([v(0.,0.,0.), v(0.,1.,0.), v(1.,1.,0.), v(1.,0.,0.)]),
            quad([v(0.,0.,1.), v(1.,0.,1.), v(1.,1.,1.), v(0.,1.,1.)]),
            quad([v(0.,0.,0.), v(1.,0.,0.), v(1.,0.,1.), v(0.,0.,1.)]),
            quad([v(1.,0.,0.), v(1.,1.,0.), v(1.,1.,1.), v(1.,0.,1.)]),
            quad([v(1.,1.,0.), v(0.,1.,0.), v(0.,1.,1.), v(1.,1.,1.)]),
            quad([v(0.,1.,0.), v(0.,0.,0.), v(0.,0.,1.), v(0.,1.,1.)]),
        ]
    }

    #[test]
    fn a_box_reports_its_extents_and_fills_them() {
        let m = metrics(&boxed(2.0, 5.0, 3.0)).unwrap();
        assert_eq!(m.extent, [2.0, 3.0, 5.0]);
        assert!(m.fills_extent(), "a box must fill its own bounding box");
    }

    /// The whole reason a bounding box is not a source of dimensions: an
    /// L-shaped part occupies far less than its envelope, so its extents are
    /// not its length and thickness.
    #[test]
    fn a_non_box_does_not_fill_its_extent() {
        let l = [[0.,0.],[4.,0.],[4.,1.],[1.,1.],[1.,3.],[0.,3.]];
        let h = 2.0;
        let bottom: Loop = l.iter().rev().map(|&[x,y]| [x,y,0.0]).collect();
        let top: Loop = l.iter().map(|&[x,y]| [x,y,h]).collect();
        let mut faces = vec![
            Face { outer: bottom, inner: vec![] },
            Face { outer: top, inner: vec![] },
        ];
        for i in 0..l.len() {
            let (a, b) = (l[i], l[(i + 1) % l.len()]);
            faces.push(quad([
                [a[0],a[1],0.0], [b[0],b[1],0.0], [b[0],b[1],h], [a[0],a[1],h],
            ]));
        }
        let m = metrics(&faces).unwrap();
        assert_eq!(m.extent, [2.0, 3.0, 4.0]);
        // Volume 12 against a 24 envelope.
        assert!(!m.fills_extent(), "an L-prism must not be treated as a box");
    }
}
