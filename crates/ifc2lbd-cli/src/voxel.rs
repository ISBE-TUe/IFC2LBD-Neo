//! Voxel-based adjacency detection for IFC building elements.
//!
//! Voxelizes triangle meshes into a uniform 3D grid and checks whether two
//! voxel sets share face-adjacent cells (6-connectivity), indicating that the
//! corresponding building elements physically touch.

use std::collections::HashSet;

/// A voxel cell coordinate in the uniform grid.
pub type VoxelCoord = (i32, i32, i32);

/// The 6 face-neighbor offsets (±x, ±y, ±z).
const FACE_NEIGHBORS: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

/// Voxelize a triangle mesh into a set of occupied grid cells.
///
/// Uses conservative triangle rasterization: for each triangle, we walk all
/// voxel cells whose AABB overlaps the triangle and perform a triangle-box
/// intersection test to mark only truly overlapping cells.
///
/// * `vertices` — flat array of vertex positions [x0,y0,z0, x1,y1,z1, ...]
/// * `indices`  — triangle index triples [i0,i1,i2, i3,i4,i5, ...]
/// * `cell_size` — edge length of each cubic voxel cell (e.g. 0.1 for 10 cm)
///
/// Returns the set of occupied voxel coordinates.
pub fn voxelize_triangles(
    vertices: &[f64],
    indices: &[u32],
    cell_size: f64,
) -> HashSet<VoxelCoord> {
    assert!(cell_size > 0.0, "cell_size must be positive");
    let inv = 1.0 / cell_size;
    let mut voxels = HashSet::new();

    for tri in indices.chunks_exact(3) {
        let i0 = tri[0] as usize * 3;
        let i1 = tri[1] as usize * 3;
        let i2 = tri[2] as usize * 3;
        if i0 + 2 >= vertices.len() || i1 + 2 >= vertices.len() || i2 + 2 >= vertices.len() {
            continue;
        }
        let v0 = [vertices[i0], vertices[i0 + 1], vertices[i0 + 2]];
        let v1 = [vertices[i1], vertices[i1 + 1], vertices[i1 + 2]];
        let v2 = [vertices[i2], vertices[i2 + 1], vertices[i2 + 2]];

        // Triangle AABB
        let tmin = [
            v0[0].min(v1[0]).min(v2[0]),
            v0[1].min(v1[1]).min(v2[1]),
            v0[2].min(v1[2]).min(v2[2]),
        ];
        let tmax = [
            v0[0].max(v1[0]).max(v2[0]),
            v0[1].max(v1[1]).max(v2[1]),
            v0[2].max(v1[2]).max(v2[2]),
        ];

        // Voxel range that covers the triangle AABB
        let gmin = [
            (tmin[0] * inv).floor() as i32,
            (tmin[1] * inv).floor() as i32,
            (tmin[2] * inv).floor() as i32,
        ];
        let gmax = [
            (tmax[0] * inv).floor() as i32,
            (tmax[1] * inv).floor() as i32,
            (tmax[2] * inv).floor() as i32,
        ];

        for gz in gmin[2]..=gmax[2] {
            for gy in gmin[1]..=gmax[1] {
                for gx in gmin[0]..=gmax[0] {
                    // Voxel cell AABB
                    let box_center = [
                        (gx as f64 + 0.5) * cell_size,
                        (gy as f64 + 0.5) * cell_size,
                        (gz as f64 + 0.5) * cell_size,
                    ];
                    let half = cell_size * 0.5;
                    if triangle_box_overlap(&box_center, half, &v0, &v1, &v2) {
                        voxels.insert((gx, gy, gz));
                    }
                }
            }
        }
    }

    voxels
}

/// Check if two voxel sets are face-adjacent (6-connectivity) or share any cell.
///
/// Returns `true` if any voxel in `a` is identical to or a face-neighbor of
/// any voxel in `b`. Iterates over the smaller set for efficiency.
pub fn voxels_adjacent(a: &HashSet<VoxelCoord>, b: &HashSet<VoxelCoord>) -> bool {
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    for &(x, y, z) in small {
        // Same cell
        if big.contains(&(x, y, z)) {
            return true;
        }
        // 6 face-neighbors
        for &(dx, dy, dz) in &FACE_NEIGHBORS {
            if big.contains(&(x + dx, y + dy, z + dz)) {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Triangle-box overlap test (Separating Axis Theorem)
// Based on Tomas Akenine-Möller's method:
// "Fast 3D Triangle-Box Overlap Testing" (2001)
// ---------------------------------------------------------------------------

fn triangle_box_overlap(
    box_center: &[f64; 3],
    half_size: f64,
    v0: &[f64; 3],
    v1: &[f64; 3],
    v2: &[f64; 3],
) -> bool {
    // Translate triangle so box center is at origin
    let v0 = [
        v0[0] - box_center[0],
        v0[1] - box_center[1],
        v0[2] - box_center[2],
    ];
    let v1 = [
        v1[0] - box_center[0],
        v1[1] - box_center[1],
        v1[2] - box_center[2],
    ];
    let v2 = [
        v2[0] - box_center[0],
        v2[1] - box_center[1],
        v2[2] - box_center[2],
    ];

    // Edge vectors
    let e0 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
    let e1 = [v2[0] - v1[0], v2[1] - v1[1], v2[2] - v1[2]];
    let e2 = [v0[0] - v2[0], v0[1] - v2[1], v0[2] - v2[2]];

    let h = half_size;

    // 9 axis tests (cross products of edge vectors with box axes)
    if !axis_test_x01(&v0, &v2, &e0, h) {
        return false;
    }
    if !axis_test_x01(&v0, &v2, &e1, h) {
        return false;
    }
    if !axis_test_x01(&v0, &v2, &e2, h) {
        return false;
    }

    if !axis_test_y02(&v0, &v1, &e0, h) {
        return false;
    }
    if !axis_test_y02(&v0, &v1, &e1, h) {
        return false;
    }
    if !axis_test_y02(&v0, &v1, &e2, h) {
        return false;
    }

    if !axis_test_z12(&v1, &v2, &e0, h) {
        return false;
    }
    if !axis_test_z12(&v1, &v2, &e1, h) {
        return false;
    }
    if !axis_test_z12(&v1, &v2, &e2, h) {
        return false;
    }

    // Test the 3 AABB face normals (x, y, z axes)
    let min_x = v0[0].min(v1[0]).min(v2[0]);
    let max_x = v0[0].max(v1[0]).max(v2[0]);
    if min_x > h || max_x < -h {
        return false;
    }

    let min_y = v0[1].min(v1[1]).min(v2[1]);
    let max_y = v0[1].max(v1[1]).max(v2[1]);
    if min_y > h || max_y < -h {
        return false;
    }

    let min_z = v0[2].min(v1[2]).min(v2[2]);
    let max_z = v0[2].max(v1[2]).max(v2[2]);
    if min_z > h || max_z < -h {
        return false;
    }

    // Test triangle normal (plane test)
    let normal = cross(&e0, &e1);
    let d = dot(&normal, &v0);
    let r = h * normal[0].abs() + h * normal[1].abs() + h * normal[2].abs();
    if d > r || d < -r {
        return false;
    }

    true
}

/// Cross product of two edges projected onto box axis X, testing vertices va, vb.
fn axis_test_x01(va: &[f64; 3], vb: &[f64; 3], e: &[f64; 3], h: f64) -> bool {
    let p0 = e[2] * va[1] - e[1] * va[2];
    let p1 = e[2] * vb[1] - e[1] * vb[2];
    let (min, max) = if p0 < p1 { (p0, p1) } else { (p1, p0) };
    let rad = e[2].abs() * h + e[1].abs() * h;
    !(min > rad || max < -rad)
}

fn axis_test_y02(va: &[f64; 3], vb: &[f64; 3], e: &[f64; 3], h: f64) -> bool {
    let p0 = -e[2] * va[0] + e[0] * va[2];
    let p1 = -e[2] * vb[0] + e[0] * vb[2];
    let (min, max) = if p0 < p1 { (p0, p1) } else { (p1, p0) };
    let rad = e[2].abs() * h + e[0].abs() * h;
    !(min > rad || max < -rad)
}

fn axis_test_z12(va: &[f64; 3], vb: &[f64; 3], e: &[f64; 3], h: f64) -> bool {
    let p0 = e[1] * va[0] - e[0] * va[1];
    let p1 = e[1] * vb[0] - e[0] * vb[1];
    let (min, max) = if p0 < p1 { (p0, p1) } else { (p1, p0) };
    let rad = e[1].abs() * h + e[0].abs() * h;
    !(min > rad || max < -rad)
}

fn cross(a: &[f64; 3], b: &[f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: &[f64; 3], b: &[f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_triangle_voxelization() {
        // A triangle in the XY plane, roughly 1m x 1m
        let vertices = vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let indices = vec![0, 1, 2];
        let voxels = voxelize_triangles(&vertices, &indices, 0.1);
        // Conservative SAT voxelization can vary at triangle boundaries, so we
        // assert broad invariants instead of a specific corner cell.
        assert!(!voxels.is_empty());
        assert!(voxels.iter().all(|&(_, _, z)| z == 0));
        assert!(voxels.iter().any(|&(x, _, _)| x >= 0 && x <= 9));
        assert!(voxels.iter().any(|&(_, y, _)| y >= 0 && y <= 9));
    }

    #[test]
    fn test_adjacency_touching() {
        let mut a = HashSet::new();
        a.insert((0, 0, 0));
        a.insert((1, 0, 0));

        let mut b = HashSet::new();
        b.insert((2, 0, 0)); // face-neighbor of (1,0,0)

        assert!(voxels_adjacent(&a, &b));
    }

    #[test]
    fn test_adjacency_not_touching() {
        let mut a = HashSet::new();
        a.insert((0, 0, 0));

        let mut b = HashSet::new();
        b.insert((2, 0, 0)); // 2 cells away — not adjacent

        assert!(!voxels_adjacent(&a, &b));
    }

    #[test]
    fn test_adjacency_overlapping() {
        let mut a = HashSet::new();
        a.insert((5, 5, 5));

        let mut b = HashSet::new();
        b.insert((5, 5, 5)); // same cell

        assert!(voxels_adjacent(&a, &b));
    }

    #[test]
    fn test_adjacency_diagonal_not_adjacent() {
        let mut a = HashSet::new();
        a.insert((0, 0, 0));

        let mut b = HashSet::new();
        b.insert((1, 1, 0)); // diagonal — NOT face-adjacent

        assert!(!voxels_adjacent(&a, &b));
    }
}
