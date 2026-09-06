//! Shared data model for tessellated IFC geometry.
//!
//! Stored as `Arc<TessellatedModel>` in `PipelineContext`. Read by
//! `plugin-geometry-producer` and any future geometry-consuming module
//! (topology, QTO, clash, etc.).

use std::collections::HashMap;
use std::sync::OnceLock;

pub use ifc_geometry::{FlatMesh, GeometryInstance, Mesh};

/// The complete tessellated geometry of one IFC model, ready for export.
#[derive(Debug, Clone)]
pub struct TessellatedModel {
    pub meshes: Vec<FlatMesh>,
    /// Column-major 4×4 coordination matrix (from IFCCOORDINATEOPERATION or identity)
    pub coordination_matrix: [f64; 16],
    /// Per-object world-space AABB as
    /// `[x_min, x_max, y_min, y_max, z_min, z_max]`, in metres.
    /// Computed once with the tessellation and shared by downstream consumers.
    world_bounding_boxes: OnceLock<HashMap<u64, [f64; 6]>>,
    pub metadata_mode: MetadataMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MetadataMode {
    /// Include GUIDs, categories, attributes and relations (default).
    #[default]
    Full,
    /// Geometry + GUIDs only — for visualization-only workflows.
    Stripped,
}

impl TessellatedModel {
    pub fn new(meshes: Vec<FlatMesh>, metadata_mode: MetadataMode) -> Self {
        Self {
            meshes,
            coordination_matrix: IDENTITY_4X4,
            world_bounding_boxes: OnceLock::new(),
            metadata_mode,
        }
    }

    /// Return cached world-space AABBs, computing them on first use.
    ///
    /// Geometry-only exports and OMG runs with bounding boxes disabled never pay
    /// for this additional full vertex scan.
    pub fn world_bounding_boxes(&self) -> &HashMap<u64, [f64; 6]> {
        self.world_bounding_boxes
            .get_or_init(|| compute_world_bounding_boxes(&self.meshes))
    }
}

fn compute_world_bounding_boxes(meshes: &[FlatMesh]) -> HashMap<u64, [f64; 6]> {
    let mut boxes = HashMap::new();
    for flat_mesh in meshes {
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        let mut has_point = false;
        for geometry in &flat_mesh.geometries {
            let transform = multiply(&geometry.world_transform, &geometry.local_transform);
            for position in geometry.mesh.positions.chunks_exact(3) {
                let point = transform_point(&transform, position);
                if !point.iter().all(|component| component.is_finite()) {
                    continue;
                }
                has_point = true;
                for axis in 0..3 {
                    min[axis] = min[axis].min(point[axis]);
                    max[axis] = max[axis].max(point[axis]);
                }
            }
        }
        if has_point {
            boxes.insert(
                flat_mesh.express_id,
                [min[0], max[0], min[1], max[1], min[2], max[2]],
            );
        }
    }
    boxes
}

fn multiply(left: &[f64; 16], right: &[f64; 16]) -> [f64; 16] {
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            result[column * 4 + row] = (0..4)
                .map(|index| left[index * 4 + row] * right[column * 4 + index])
                .sum();
        }
    }
    result
}

fn transform_point(transform: &[f64; 16], point: &[f32]) -> [f64; 3] {
    let x = f64::from(point[0]);
    let y = f64::from(point[1]);
    let z = f64::from(point[2]);
    [
        transform[0] * x + transform[4] * y + transform[8] * z + transform[12],
        transform[1] * x + transform[5] * y + transform[9] * z + transform[13],
        transform[2] * x + transform[6] * y + transform[10] * z + transform[14],
    ]
}

const IDENTITY_4X4: [f64; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_caches_world_bounding_boxes_with_all_transforms_applied() {
        let world = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 10.0, 20.0, 30.0, 1.0,
        ];
        let local = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 2.0, 3.0, 1.0,
        ];
        let model = TessellatedModel::new(
            vec![FlatMesh {
                express_id: 42,
                guid: "test-guid".to_string(),
                category: "IFCWALL".to_string(),
                geometries: vec![GeometryInstance {
                    mesh: Mesh {
                        positions: vec![-1.0, 2.0, 3.0, 4.0, -5.0, 6.0],
                        normals: Vec::new(),
                        indices: Vec::new(),
                        rtc_applied: false,
                    },
                    color: [1.0; 4],
                    world_transform: world,
                    local_transform: local,
                    geometry_id: 7,
                    dedup_key: None,
                }],
            }],
            MetadataMode::Full,
        );

        assert_eq!(
            model.world_bounding_boxes().get(&42),
            Some(&[10.0, 15.0, 17.0, 24.0, 36.0, 39.0])
        );
    }
}
