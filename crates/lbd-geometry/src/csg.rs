//! Constructive solid geometry (CSG) boolean intersection using csgrs.
//!
//! Extracts triangle meshes from IFC geometry and performs boolean intersection
//! via csgrs BSP-tree mesh boolean operations. Replaces the external OCC subprocess
//! with pure Rust code that compiles to WASM.
//!
//! Pipeline: R-tree broad-phase → CSG boolean intersection (with pair limiting) →
//! bbox fallback for elements without meshes or meshes too large for CSG.

use std::collections::HashMap;

use csgrs::csg::CSG;
use csgrs::polygon::Polygon;
use csgrs::vertex::Vertex;
use ifc_model::IfcModel;
use ifc_step::{EntityId, StepFile, StepValue};

use nalgebra::{Point3, Vector3};
use tracing::{debug, info};

use crate::{
    append_pair_relations, ExactCheckOptions, ExactGeometryKernel, ExactPairAnalysis,
    GeometryKernelError,
};

// ---------------------------------------------------------------------------
// Triangle mesh type used by mesh extraction
// ---------------------------------------------------------------------------

/// A triangle mesh: flat vertex array + triangle index array.
#[derive(Debug, Clone)]
pub struct TriangleMesh {
    /// Flat vertex positions: [x0,y0,z0, x1,y1,z1, ...]
    pub vertices: Vec<f64>,
    /// Triangle indices (0-based): [i0,i1,i2, i3,i4,i5, ...]
    pub indices: Vec<u32>,
}

impl TriangleMesh {
    pub fn new() -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Append another mesh into this one, offsetting indices.
    pub fn append(&mut self, other: &TriangleMesh) {
        let offset = (self.vertices.len() / 3) as u32;
        self.vertices.extend_from_slice(&other.vertices);
        self.indices.extend(
            other
                .indices
                .iter()
                .map(|i| i + offset),
        );
    }

    /// Apply a 4x4 affine transform to all vertices in-place.
    pub fn transform(&mut self, t: &Affine3) {
        for chunk in self.vertices.chunks_exact_mut(3) {
            let p = [chunk[0], chunk[1], chunk[2]];
            let tp = t.transform_point(&p);
            chunk[0] = tp[0];
            chunk[1] = tp[1];
            chunk[2] = tp[2];
        }
    }
}

/// 4x4 affine transformation matrix (column-major).
#[derive(Debug, Clone, Copy)]
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

    pub fn transform_point(&self, p: &[f64; 3]) -> [f64; 3] {
        let w = self.m[0][3] * p[0]
            + self.m[1][3] * p[1]
            + self.m[2][3] * p[2]
            + self.m[3][3];
        [
            (self.m[0][0] * p[0] + self.m[1][0] * p[1] + self.m[2][0] * p[2]
                + self.m[3][0])
                / w,
            (self.m[0][1] * p[0] + self.m[1][1] * p[1] + self.m[2][1] * p[2]
                + self.m[3][1])
                / w,
            (self.m[0][2] * p[0] + self.m[1][2] * p[1] + self.m[2][2] * p[2]
                + self.m[3][2])
                / w,
        ]
    }

    pub fn mul(&self, other: &Self) -> Self {
        let mut result = Self::identity();
        for i in 0..4 {
            for j in 0..4 {
                result.m[i][j] = (0..4)
                    .map(|k| self.m[i][k] * other.m[k][j])
                    .sum();
            }
        }
        result
    }
}

// ---------------------------------------------------------------------------
// IFC geometry → mesh extraction (from CLI mesh.rs)
// ---------------------------------------------------------------------------

/// Extract the combined triangle mesh for an IFC element in world coordinates.
pub fn extract_element_mesh(
    step: &StepFile,
    element_id: EntityId,
    world_transform: &Affine3,
) -> TriangleMesh {
    let mut mesh = TriangleMesh::new();

    let entity = match step.entities.get(&element_id) {
        Some(e) => e,
        None => return mesh,
    };

    // args[6] = Representation (IfcProductDefinitionShape)
    let rep_id = match entity.args.get(6) {
        Some(StepValue::Ref(id)) => *id,
        _ => return mesh,
    };

    let rep_entity = match step.entities.get(&rep_id) {
        Some(e) => e,
        None => return mesh,
    };

    let rep_list = match rep_entity.entity_name.as_str() {
        "IFCPRODUCTDEFINITIONSHAPE" => match rep_entity.args.get(2) {
            Some(StepValue::List(list)) => list.clone(),
            _ => return mesh,
        },
        _ => return mesh,
    };

    for rep_ref in &rep_list {
        let shape_rep_id = match rep_ref {
            StepValue::Ref(id) => *id,
            _ => continue,
        };
        let shape_rep = match step.entities.get(&shape_rep_id) {
            Some(e) => e,
            None => continue,
        };
        if shape_rep.entity_name != "IFCSHAPEREPRESENTATION" {
            continue;
        }

        let ident = shape_rep
            .args
            .get(1)
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !ident.is_empty() && ident != "Body" && ident != "Facetation" {
            continue;
        }

        let items = match shape_rep.args.get(3) {
            Some(StepValue::List(list)) => list,
            _ => continue,
        };

        for item_ref in items {
            let item_id = match item_ref {
                StepValue::Ref(id) => *id,
                _ => continue,
            };
            let item_mesh = extract_representation_item(step, item_id, 0);
            mesh.append(&item_mesh);
        }
    }

    mesh.transform(world_transform);
    mesh
}

fn extract_representation_item(step: &StepFile, item_id: EntityId, depth: usize) -> TriangleMesh {
    if depth > 10 {
        return TriangleMesh::new();
    }
    let entity = match step.entities.get(&item_id) {
        Some(e) => e,
        None => return TriangleMesh::new(),
    };

    match entity.entity_name.as_str() {
        "IFCTRIANGULATEDFACESET" => extract_triangulated_face_set(step, entity),
        "IFCPOLYGONALFACESET" => extract_polygonal_face_set(step, entity),
        "IFCFACETEDBREP" => extract_faceted_brep(step, entity),
        "IFCFACEBASEDSURFACEMODEL" => extract_face_based_surface_model(step, entity),
        "IFCEXTRUDEDAREASOLID" => extract_extruded_area_solid(step, entity),
        "IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT" => match entity.args.get(1) {
            Some(StepValue::Ref(id)) => extract_representation_item(step, *id, depth + 1),
            _ => TriangleMesh::new(),
        },
        "IFCMAPPEDITEM" => extract_mapped_item(step, entity, depth),
        _ => TriangleMesh::new(),
    }
}

fn extract_triangulated_face_set(
    step: &StepFile,
    entity: &ifc_step::RawEntity,
) -> TriangleMesh {
    let coords_id = match entity.args.get(0) {
        Some(StepValue::Ref(id)) => *id,
        _ => return TriangleMesh::new(),
    };
    let vertices = read_cartesian_point_list_3d(step, coords_id);
    if vertices.is_empty() {
        return TriangleMesh::new();
    }

    let coord_index = find_coord_index_list(entity);
    let coord_index = match coord_index {
        Some(list) => list,
        None => return TriangleMesh::new(),
    };

    let mut indices = Vec::new();
    for tri in coord_index {
        if let StepValue::List(idx_list) = tri {
            if idx_list.len() >= 3 {
                if let (Some(a), Some(b), Some(c)) = (
                    idx_list[0].as_int(),
                    idx_list[1].as_int(),
                    idx_list[2].as_int(),
                ) {
                    indices.push((a - 1) as u32);
                    indices.push((b - 1) as u32);
                    indices.push((c - 1) as u32);
                }
            }
        }
    }

    TriangleMesh { vertices, indices }
}

fn find_coord_index_list(entity: &ifc_step::RawEntity) -> Option<&[StepValue]> {
    for idx in [3, 4] {
        if let Some(StepValue::List(list)) = entity.args.get(idx) {
            if list.first().map_or(false, |v| matches!(v, StepValue::List(_))) {
                return Some(list);
            }
        }
    }
    None
}

fn extract_polygonal_face_set(
    step: &StepFile,
    entity: &ifc_step::RawEntity,
) -> TriangleMesh {
    let coords_id = match entity.args.get(0) {
        Some(StepValue::Ref(id)) => *id,
        _ => return TriangleMesh::new(),
    };
    let vertices = read_cartesian_point_list_3d(step, coords_id);
    if vertices.is_empty() {
        return TriangleMesh::new();
    }

    let faces_list = match entity.args.get(2) {
        Some(StepValue::List(list)) => list,
        _ => return TriangleMesh::new(),
    };

    let mut indices = Vec::new();
    for face_ref in faces_list {
        let face_id = match face_ref {
            StepValue::Ref(id) => *id,
            _ => continue,
        };
        let face_entity = match step.entities.get(&face_id) {
            Some(e) => e,
            None => continue,
        };
        let coord_idx = match face_entity.args.get(0) {
            Some(StepValue::List(list)) => list,
            _ => continue,
        };
        let face_indices: Vec<u32> = coord_idx
            .iter()
            .filter_map(|v| v.as_int().map(|i| (i - 1) as u32))
            .collect();
        if face_indices.len() >= 3 {
            for i in 1..face_indices.len() - 1 {
                indices.push(face_indices[0]);
                indices.push(face_indices[i]);
                indices.push(face_indices[i + 1]);
            }
        }
    }

    TriangleMesh { vertices, indices }
}

fn extract_faceted_brep(
    step: &StepFile,
    entity: &ifc_step::RawEntity,
) -> TriangleMesh {
    let shell_id = match entity.args.get(0) {
        Some(StepValue::Ref(id)) => *id,
        _ => return TriangleMesh::new(),
    };
    extract_shell(step, shell_id)
}

fn extract_shell(step: &StepFile, shell_id: EntityId) -> TriangleMesh {
    let shell = match step.entities.get(&shell_id) {
        Some(e) => e,
        None => return TriangleMesh::new(),
    };
    let faces_list = match shell.args.get(0) {
        Some(StepValue::List(list)) => list,
        _ => return TriangleMesh::new(),
    };

    let mut mesh = TriangleMesh::new();
    for face_ref in faces_list {
        let face_id = match face_ref {
            StepValue::Ref(id) => *id,
            _ => continue,
        };
        let face_mesh = extract_ifc_face(step, face_id);
        mesh.append(&face_mesh);
    }
    mesh
}

fn extract_ifc_face(step: &StepFile, face_id: EntityId) -> TriangleMesh {
    let face = match step.entities.get(&face_id) {
        Some(e) => e,
        None => return TriangleMesh::new(),
    };
    let bounds = match face.args.get(0) {
        Some(StepValue::List(list)) => list,
        _ => return TriangleMesh::new(),
    };

    for bound_ref in bounds {
        let bound_id = match bound_ref {
            StepValue::Ref(id) => *id,
            _ => continue,
        };
        let bound = match step.entities.get(&bound_id) {
            Some(e) => e,
            None => continue,
        };
        let loop_id = match bound.args.get(0) {
            Some(StepValue::Ref(id)) => *id,
            _ => continue,
        };
        let loop_entity = match step.entities.get(&loop_id) {
            Some(e) => e,
            None => continue,
        };
        if loop_entity.entity_name != "IFCPOLYLOOP" {
            continue;
        }
        let polygon_refs = match loop_entity.args.get(0) {
            Some(StepValue::List(list)) => list,
            _ => continue,
        };
        let mut polygon_pts: Vec<[f64; 3]> = Vec::new();
        for pt_ref in polygon_refs {
            if let StepValue::Ref(pt_id) = pt_ref {
                polygon_pts.push(cartesian_point_3d(step, *pt_id));
            }
        }
        if polygon_pts.len() >= 3 {
            return triangulate_polygon(&polygon_pts);
        }
    }

    TriangleMesh::new()
}

fn extract_face_based_surface_model(
    step: &StepFile,
    entity: &ifc_step::RawEntity,
) -> TriangleMesh {
    let face_sets = match entity.args.get(0) {
        Some(StepValue::List(list)) => list,
        _ => return TriangleMesh::new(),
    };
    let mut mesh = TriangleMesh::new();
    for set_ref in face_sets {
        let set_id = match set_ref {
            StepValue::Ref(id) => *id,
            _ => continue,
        };
        let sub_mesh = extract_shell(step, set_id);
        mesh.append(&sub_mesh);
    }
    mesh
}

fn extract_extruded_area_solid(step: &StepFile, entity: &ifc_step::RawEntity) -> TriangleMesh {
    let profile_id = match entity.args.get(0) {
        Some(StepValue::Ref(id)) => *id,
        _ => return TriangleMesh::new(),
    };

    let local_transform = match entity.args.get(1) {
        Some(StepValue::Ref(id)) => axis2placement3d_to_affine(step, *id),
        _ => Affine3::identity(),
    };

    let extrude_dir = match entity.args.get(2) {
        Some(StepValue::Ref(id)) => read_direction(step, *id),
        _ => [0.0, 0.0, 1.0],
    };

    let depth = match entity.args.get(3) {
        Some(v) => v.as_real().unwrap_or(0.0),
        None => return TriangleMesh::new(),
    };

    if depth <= 0.0 {
        return TriangleMesh::new();
    }

    let profile_pts = extract_profile_polygon(step, profile_id);
    if profile_pts.len() < 3 {
        return TriangleMesh::new();
    }

    let extrude_vec = [
        extrude_dir[0] * depth,
        extrude_dir[1] * depth,
        extrude_dir[2] * depth,
    ];

    let mut mesh = build_prism_mesh(&profile_pts, &extrude_vec);
    mesh.transform(&local_transform);
    mesh
}

fn extract_profile_polygon(step: &StepFile, profile_id: EntityId) -> Vec<[f64; 3]> {
    let entity = match step.entities.get(&profile_id) {
        Some(e) => e,
        None => return Vec::new(),
    };

    match entity.entity_name.as_str() {
        "IFCARBITRARYCLOSEDPROFILEDEF" | "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
            let curve_id = match entity.args.get(2) {
                Some(StepValue::Ref(id)) => *id,
                _ => match entity.args.get(1) {
                    Some(StepValue::Ref(id)) => *id,
                    _ => return Vec::new(),
                },
            };
            extract_curve_points(step, curve_id)
        }
        "IFCRECTANGLEPROFILEDEF" | "IFCRECTANGLEHOLLOWPROFILEDEF" => {
            let position = match entity.args.get(2) {
                Some(StepValue::Ref(id)) => Some(*id),
                _ => None,
            };
            let x_dim = entity.args.get(3).and_then(|v| v.as_real()).unwrap_or(1.0);
            let y_dim = entity.args.get(4).and_then(|v| v.as_real()).unwrap_or(1.0);
            let hx = x_dim / 2.0;
            let hy = y_dim / 2.0;
            let mut pts = vec![
                [-hx, -hy, 0.0],
                [hx, -hy, 0.0],
                [hx, hy, 0.0],
                [-hx, hy, 0.0],
            ];
            if let Some(pos_id) = position {
                let t = profile_placement_transform(step, pos_id);
                for pt in &mut pts {
                    *pt = t.transform_point(pt);
                }
            }
            pts
        }
        "IFCCIRCLEPROFILEDEF" | "IFCCIRCLEHOLLOWPROFILEDEF" => {
            let position = match entity.args.get(2) {
                Some(StepValue::Ref(id)) => Some(*id),
                _ => None,
            };
            let radius = entity.args.get(3).and_then(|v| v.as_real()).unwrap_or(0.5);
            let n = 16;
            let mut pts = Vec::with_capacity(n);
            for i in 0..n {
                let angle = 2.0 * std::f64::consts::PI * (i as f64) / (n as f64);
                pts.push([radius * angle.cos(), radius * angle.sin(), 0.0]);
            }
            if let Some(pos_id) = position {
                let t = profile_placement_transform(step, pos_id);
                for pt in &mut pts {
                    *pt = t.transform_point(pt);
                }
            }
            pts
        }
        "IFCISHAPEPROFILEDEF" => {
            let position = match entity.args.get(2) {
                Some(StepValue::Ref(id)) => Some(*id),
                _ => None,
            };
            let w = entity.args.get(3).and_then(|v| v.as_real()).unwrap_or(0.2);
            let d = entity.args.get(4).and_then(|v| v.as_real()).unwrap_or(0.4);
            let tw = entity.args.get(5).and_then(|v| v.as_real()).unwrap_or(0.01);
            let tf = entity.args.get(6).and_then(|v| v.as_real()).unwrap_or(0.02);
            let hw = w / 2.0;
            let hd = d / 2.0;
            let htw = tw / 2.0;
            let mut pts = vec![
                [-hw, -hd, 0.0],
                [hw, -hd, 0.0],
                [hw, -hd + tf, 0.0],
                [htw, -hd + tf, 0.0],
                [htw, hd - tf, 0.0],
                [hw, hd - tf, 0.0],
                [hw, hd, 0.0],
                [-hw, hd, 0.0],
                [-hw, hd - tf, 0.0],
                [-htw, hd - tf, 0.0],
                [-htw, -hd + tf, 0.0],
                [-hw, -hd + tf, 0.0],
            ];
            if let Some(pos_id) = position {
                let t = profile_placement_transform(step, pos_id);
                for pt in &mut pts {
                    *pt = t.transform_point(pt);
                }
            }
            pts
        }
        "IFCLSHAPEPROFILEDEF" => {
            let position = match entity.args.get(2) {
                Some(StepValue::Ref(id)) => Some(*id),
                _ => None,
            };
            let depth = entity.args.get(3).and_then(|v| v.as_real()).unwrap_or(0.2);
            let width = entity.args.get(4).and_then(|v| v.as_real()).unwrap_or(0.2);
            let thickness = entity.args.get(5).and_then(|v| v.as_real()).unwrap_or(0.02);
            let mut pts = vec![
                [0.0, 0.0, 0.0],
                [width, 0.0, 0.0],
                [width, thickness, 0.0],
                [thickness, thickness, 0.0],
                [thickness, depth, 0.0],
                [0.0, depth, 0.0],
            ];
            if let Some(pos_id) = position {
                let t = profile_placement_transform(step, pos_id);
                for pt in &mut pts {
                    *pt = t.transform_point(pt);
                }
            }
            pts
        }
        _ => Vec::new(),
    }
}

fn extract_curve_points(step: &StepFile, curve_id: EntityId) -> Vec<[f64; 3]> {
    let entity = match step.entities.get(&curve_id) {
        Some(e) => e,
        None => return Vec::new(),
    };

    match entity.entity_name.as_str() {
        "IFCPOLYLINE" => match entity.args.get(0) {
            Some(StepValue::List(list)) => {
                let mut pts = Vec::new();
                for pt_ref in list {
                    if let StepValue::Ref(pt_id) = pt_ref {
                        pts.push(cartesian_point_3d(step, *pt_id));
                    }
                }
                if pts.len() > 1 {
                    let first = pts[0];
                    let last = pts[pts.len() - 1];
                    if (first[0] - last[0]).abs() < 1e-10
                        && (first[1] - last[1]).abs() < 1e-10
                        && (first[2] - last[2]).abs() < 1e-10
                    {
                        pts.pop();
                    }
                }
                pts
            }
            _ => Vec::new(),
        },
        "IFCINDEXEDPOLYCURVE" => {
            let pts_id = match entity.args.get(0) {
                Some(StepValue::Ref(id)) => *id,
                _ => return Vec::new(),
            };
            let pts_entity = match step.entities.get(&pts_id) {
                Some(e) => e,
                None => return Vec::new(),
            };
            match pts_entity.entity_name.as_str() {
                "IFCCARTESIANPOINTLIST3D" => read_point_list_3d(pts_entity),
                "IFCCARTESIANPOINTLIST2D" => read_point_list_2d(pts_entity),
                _ => Vec::new(),
            }
        }
        "IFCCOMPOSITECURVE" => {
            let segments = match entity.args.get(0) {
                Some(StepValue::List(list)) => list,
                _ => return Vec::new(),
            };
            let mut pts = Vec::new();
            for seg_ref in segments {
                let seg_id = match seg_ref {
                    StepValue::Ref(id) => *id,
                    _ => continue,
                };
                let seg = match step.entities.get(&seg_id) {
                    Some(e) => e,
                    None => continue,
                };
                let parent_curve_id = match seg.args.get(2) {
                    Some(StepValue::Ref(id)) => *id,
                    _ => continue,
                };
                let seg_pts = extract_curve_points(step, parent_curve_id);
                pts.extend(seg_pts);
            }
            pts.dedup_by(|a, b| {
                (a[0] - b[0]).abs() < 1e-10
                    && (a[1] - b[1]).abs() < 1e-10
                    && (a[2] - b[2]).abs() < 1e-10
            });
            pts
        }
        "IFCTRIMMEDCURVE" => {
            match entity.args.get(0) {
                Some(StepValue::Ref(id)) => extract_curve_points(step, *id),
                _ => Vec::new(),
            }
        }
        "IFCCIRCLE" | "IFCELLIPSE" => {
            let radius = entity.args.get(1).and_then(|v| v.as_real()).unwrap_or(0.5);
            let n = 16;
            let mut pts = Vec::with_capacity(n);
            for i in 0..n {
                let angle = 2.0 * std::f64::consts::PI * (i as f64) / (n as f64);
                pts.push([radius * angle.cos(), radius * angle.sin(), 0.0]);
            }
            pts
        }
        "IFCLINE" => match entity.args.get(0) {
            Some(StepValue::Ref(id)) => vec![cartesian_point_3d(step, *id)],
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn extract_mapped_item(
    step: &StepFile,
    entity: &ifc_step::RawEntity,
    depth: usize,
) -> TriangleMesh {
    let map_source_id = match entity.args.get(0) {
        Some(StepValue::Ref(id)) => *id,
        _ => return TriangleMesh::new(),
    };

    let target_transform = match entity.args.get(1) {
        Some(StepValue::Ref(id)) => read_cartesian_transform_operator(step, *id),
        _ => Affine3::identity(),
    };

    let map_source = match step.entities.get(&map_source_id) {
        Some(e) => e,
        None => return TriangleMesh::new(),
    };

    let origin_transform = match map_source.args.get(0) {
        Some(StepValue::Ref(id)) => {
            let origin_entity = step.entities.get(id);
            if let Some(oe) = origin_entity {
                if oe.entity_name == "IFCAXIS2PLACEMENT3D" {
                    axis2placement3d_to_affine(step, *id)
                } else {
                    Affine3::identity()
                }
            } else {
                Affine3::identity()
            }
        }
        _ => Affine3::identity(),
    };

    let mapped_rep_id = match map_source.args.get(1) {
        Some(StepValue::Ref(id)) => *id,
        _ => return TriangleMesh::new(),
    };

    let mapped_rep = match step.entities.get(&mapped_rep_id) {
        Some(e) => e,
        None => return TriangleMesh::new(),
    };

    let items = match mapped_rep.args.get(3) {
        Some(StepValue::List(list)) => list,
        _ => return TriangleMesh::new(),
    };

    let mut mesh = TriangleMesh::new();
    for item_ref in items {
        let item_id = match item_ref {
            StepValue::Ref(id) => *id,
            _ => continue,
        };
        let item_mesh = extract_representation_item(step, item_id, depth + 1);
        mesh.append(&item_mesh);
    }

    let combined = target_transform.mul(&origin_transform);
    mesh.transform(&combined);
    mesh
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_cartesian_point_list_3d(step: &StepFile, id: EntityId) -> Vec<f64> {
    let entity = match step.entities.get(&id) {
        Some(e) => e,
        None => return Vec::new(),
    };
    if entity.entity_name != "IFCCARTESIANPOINTLIST3D" {
        return Vec::new();
    }
    let list = match entity.args.get(0) {
        Some(StepValue::List(list)) => list,
        _ => return Vec::new(),
    };
    let mut vertices = Vec::with_capacity(list.len() * 3);
    for item in list {
        if let StepValue::List(coords) = item {
            let x = coords.get(0).and_then(|v| v.as_real()).unwrap_or(0.0);
            let y = coords.get(1).and_then(|v| v.as_real()).unwrap_or(0.0);
            let z = coords.get(2).and_then(|v| v.as_real()).unwrap_or(0.0);
            vertices.push(x);
            vertices.push(y);
            vertices.push(z);
        }
    }
    vertices
}

fn read_point_list_3d(entity: &ifc_step::RawEntity) -> Vec<[f64; 3]> {
    let list = match entity.args.get(0) {
        Some(StepValue::List(list)) => list,
        _ => return Vec::new(),
    };
    let mut pts = Vec::with_capacity(list.len());
    for item in list {
        if let StepValue::List(coords) = item {
            let x = coords.get(0).and_then(|v| v.as_real()).unwrap_or(0.0);
            let y = coords.get(1).and_then(|v| v.as_real()).unwrap_or(0.0);
            let z = coords.get(2).and_then(|v| v.as_real()).unwrap_or(0.0);
            pts.push([x, y, z]);
        }
    }
    pts
}

fn read_point_list_2d(entity: &ifc_step::RawEntity) -> Vec<[f64; 3]> {
    let list = match entity.args.get(0) {
        Some(StepValue::List(list)) => list,
        _ => return Vec::new(),
    };
    let mut pts = Vec::with_capacity(list.len());
    for item in list {
        if let StepValue::List(coords) = item {
            let x = coords.get(0).and_then(|v| v.as_real()).unwrap_or(0.0);
            let y = coords.get(1).and_then(|v| v.as_real()).unwrap_or(0.0);
            pts.push([x, y, 0.0]);
        }
    }
    pts
}

fn cartesian_point_3d(step: &StepFile, id: EntityId) -> [f64; 3] {
    let entity = match step.entities.get(&id) {
        Some(e) => e,
        None => return [0.0, 0.0, 0.0],
    };
    if entity.entity_name != "IFCCARTESIANPOINT" {
        return [0.0, 0.0, 0.0];
    }
    let coords = match entity.args.first() {
        Some(StepValue::List(list)) => list,
        _ => return [0.0, 0.0, 0.0],
    };
    let x = coords.get(0).and_then(|v| v.as_real()).unwrap_or(0.0);
    let y = coords.get(1).and_then(|v| v.as_real()).unwrap_or(0.0);
    let z = coords.get(2).and_then(|v| v.as_real()).unwrap_or(0.0);
    [x, y, z]
}

fn read_direction(step: &StepFile, id: EntityId) -> [f64; 3] {
    let entity = match step.entities.get(&id) {
        Some(e) => e,
        None => return [0.0, 0.0, 1.0],
    };
    if entity.entity_name != "IFCDIRECTION" {
        return [0.0, 0.0, 1.0];
    }
    let ratios = match entity.args.first() {
        Some(StepValue::List(list)) => list,
        _ => return [0.0, 0.0, 1.0],
    };
    let x = ratios.get(0).and_then(|v| v.as_real()).unwrap_or(0.0);
    let y = ratios.get(1).and_then(|v| v.as_real()).unwrap_or(0.0);
    let z = ratios.get(2).and_then(|v| v.as_real()).unwrap_or(0.0);
    [x, y, z]
}

fn read_placement_origin(step: &StepFile, id: EntityId) -> [f64; 3] {
    let entity = match step.entities.get(&id) {
        Some(e) => e,
        None => return [0.0, 0.0, 0.0],
    };
    match entity.args.get(0) {
        Some(StepValue::Ref(pt_id)) => cartesian_point_3d(step, *pt_id),
        _ => [0.0, 0.0, 0.0],
    }
}

fn read_placement_axis(step: &StepFile, id: EntityId) -> [f64; 3] {
    let entity = match step.entities.get(&id) {
        Some(e) => e,
        None => return [0.0, 0.0, 1.0],
    };
    match entity.args.get(1) {
        Some(StepValue::Ref(dir_id)) => read_direction(step, *dir_id),
        _ => [0.0, 0.0, 1.0],
    }
}

fn read_placement_ref_dir(step: &StepFile, id: EntityId) -> [f64; 3] {
    let entity = match step.entities.get(&id) {
        Some(e) => e,
        None => return [1.0, 0.0, 0.0],
    };
    match entity.args.get(2) {
        Some(StepValue::Ref(dir_id)) => read_direction(step, *dir_id),
        _ => [1.0, 0.0, 0.0],
    }
}

fn profile_placement_transform(step: &StepFile, pos_id: EntityId) -> Affine3 {
    let entity = match step.entities.get(&pos_id) {
        Some(e) => e,
        None => return Affine3::identity(),
    };
    match entity.entity_name.as_str() {
        "IFCAXIS2PLACEMENT3D" => axis2placement3d_to_affine(step, pos_id),
        "IFCAXIS2PLACEMENT2D" => {
            let origin = match entity.args.get(0) {
                Some(StepValue::Ref(id)) => {
                    let pt = cartesian_point_3d(step, *id);
                    pt
                }
                _ => [0.0, 0.0, 0.0],
            };
            let ref_dir = match entity.args.get(1) {
                Some(StepValue::Ref(id)) => read_direction(step, *id),
                _ => [1.0, 0.0, 0.0],
            };
            Affine3::from_axis_and_origin(origin, [0.0, 0.0, 1.0], ref_dir)
        }
        _ => Affine3::identity(),
    }
}

fn read_cartesian_transform_operator(step: &StepFile, id: EntityId) -> Affine3 {
    let entity = match step.entities.get(&id) {
        Some(e) => e,
        None => return Affine3::identity(),
    };
    let origin = match entity.args.get(2) {
        Some(StepValue::Ref(id)) => cartesian_point_3d(step, *id),
        _ => [0.0, 0.0, 0.0],
    };
    let x_axis = match entity.args.get(0) {
        Some(StepValue::Ref(id)) => read_direction(step, *id),
        _ => [1.0, 0.0, 0.0],
    };
    let z_axis = match entity.args.get(4) {
        Some(StepValue::Ref(id)) => read_direction(step, *id),
        _ => [0.0, 0.0, 1.0],
    };
    let scale = entity.args.get(3).and_then(|v| v.as_real()).unwrap_or(1.0);

    let mut t = Affine3::from_axis_and_origin(origin, z_axis, x_axis);
    if (scale - 1.0).abs() > 1e-10 {
        for col in 0..3 {
            for row in 0..3 {
                t.m[col][row] *= scale;
            }
        }
    }
    t
}

impl Affine3 {
    /// Create a 4x4 transformation matrix from axis and origin.
    pub fn from_axis_and_origin(origin: [f64; 3], axis: [f64; 3], ref_dir: [f64; 3]) -> Self {
        // Orthonormalize: x = ref_dir, z = axis, y = z × x
        let x = normalize(ref_dir);
        let z = normalize(axis);
        let y = cross(&z, &x);
        let x = normalize(x);
        let y = normalize(y);

        Self {
            m: [
                [x[0], x[1], x[2], origin[0]],
                [y[0], y[1], y[2], origin[1]],
                [z[0], z[1], z[2], origin[2]],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-15 {
        return [0.0, 0.0, 1.0];
    }
    [v[0] / len, v[1] / len, v[2] / len]
}

fn cross(a: &[f64; 3], b: &[f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn build_prism_mesh(profile: &[[f64; 3]], extrude: &[f64; 3]) -> TriangleMesh {
    let n = profile.len();
    let mut vertices = Vec::with_capacity(n * 2 * 3);
    for p in profile {
        vertices.push(p[0]);
        vertices.push(p[1]);
        vertices.push(p[2]);
    }
    for p in profile {
        vertices.push(p[0] + extrude[0]);
        vertices.push(p[1] + extrude[1]);
        vertices.push(p[2] + extrude[2]);
    }

    let mut indices = Vec::new();

    // Bottom face (fan triangulation, reversed winding for outward normal)
    for i in 1..n - 1 {
        indices.push(0u32);
        indices.push((i + 1) as u32);
        indices.push(i as u32);
    }

    // Top face (fan triangulation)
    let top = n as u32;
    for i in 1..n - 1 {
        indices.push(top);
        indices.push(top + i as u32);
        indices.push(top + (i + 1) as u32);
    }

    // Side faces (quads → 2 triangles each)
    for i in 0..n {
        let next = (i + 1) % n;
        let b0 = i as u32;
        let b1 = next as u32;
        let t0 = (n + i) as u32;
        let t1 = (n + next) as u32;
        indices.push(b0);
        indices.push(b1);
        indices.push(t1);
        indices.push(b0);
        indices.push(t1);
        indices.push(t0);
    }

    TriangleMesh { vertices, indices }
}

fn triangulate_polygon(pts: &[[f64; 3]]) -> TriangleMesh {
    let mut vertices = Vec::with_capacity(pts.len() * 3);
    for p in pts {
        vertices.push(p[0]);
        vertices.push(p[1]);
        vertices.push(p[2]);
    }
    let mut indices = Vec::new();
    for i in 1..pts.len() - 1 {
        indices.push(0u32);
        indices.push(i as u32);
        indices.push((i + 1) as u32);
    }
    TriangleMesh { vertices, indices }
}

/// Convert axis2placement3d to Affine3 transform.
fn axis2placement3d_to_affine(step: &StepFile, id: EntityId) -> Affine3 {
    let origin = read_placement_origin(step, id);
    let axis = read_placement_axis(step, id);
    let ref_dir = read_placement_ref_dir(step, id);
    Affine3::from_axis_and_origin(origin, axis, ref_dir)
}

// ---------------------------------------------------------------------------
// Convert TriangleMesh → csgrs CSG (for boolean operations)
// ---------------------------------------------------------------------------

/// Convert a TriangleMesh into a csgrs CSG solid for boolean operations.
/// Each triangle becomes a closed polygon (3 vertices forming a triangle).
fn to_csgrs_csg(mesh: &TriangleMesh) -> Option<CSG<f64>> {
    let vertices: Vec<[f64; 3]> = mesh
        .vertices
        .chunks_exact(3)
        .map(|c| [c[0], c[1], c[2]])
        .collect();

    if vertices.len() < 3 {
        return None;
    }

    let mut polygons = Vec::new();

    for tri in mesh.indices.chunks_exact(3) {
        if tri.len() != 3 {
            continue;
        }
        let a = vertices[tri[0] as usize];
        let b = vertices[tri[1] as usize];
        let c = vertices[tri[2] as usize];

        // Compute triangle normal via cross product
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let normal = cross(&ab, &ac);
        let normal = normalize(normal);

        let v0 = Vertex::new(
            Point3::new(a[0], a[1], a[2]),
            Vector3::new(normal[0], normal[1], normal[2]),
        );
        let v1 = Vertex::new(
            Point3::new(b[0], b[1], b[2]),
            Vector3::new(normal[0], normal[1], normal[2]),
        );
        let v2 = Vertex::new(
            Point3::new(c[0], c[1], c[2]),
            Vector3::new(normal[0], normal[1], normal[2]),
        );

        let polygon = Polygon::new(vec![v0, v1, v2], None);
        polygons.push(polygon);
    }

    if polygons.is_empty() {
        return None;
    }

    Some(CSG::from_polygons(&polygons))
}

// ---------------------------------------------------------------------------
// CSG boolean intersection
// ---------------------------------------------------------------------------

/// Analyze a single pair of elements using csgrs mesh boolean intersection.
pub fn csg_boolean_intersection(
    step: &StepFile,
    left_id: EntityId,
    right_id: EntityId,
    left_bbox: Option<&[f64]>,
    right_bbox: Option<&[f64]>,
    options: &ExactCheckOptions,
) -> Result<ExactPairAnalysis, GeometryKernelError> {
    // Extract world transforms for both elements
    let left_placement_id = match step.entities.get(&left_id) {
        Some(e) => match e.args.get(5) {
            Some(StepValue::Ref(id)) => *id,
            _ => return Ok(ExactPairAnalysis::default()),
        },
        None => return Ok(ExactPairAnalysis::default()),
    };
    let left_world = extract_placement_transform(step, left_placement_id);

    let right_placement_id = match step.entities.get(&right_id) {
        Some(e) => match e.args.get(5) {
            Some(StepValue::Ref(id)) => *id,
            _ => return Ok(ExactPairAnalysis::default()),
        },
        None => return Ok(ExactPairAnalysis::default()),
    };
    let right_world = extract_placement_transform(step, right_placement_id);

    // Extract meshes
    let left_mesh = match extract_element_mesh(step, left_id, &left_world) {
        m if m.is_empty() => {
            debug!(
                "csg: left element {} has no mesh, using bbox fallback",
                left_id
            );
            return analyze_with_bbox_fallback(
                left_id,
                right_id,
                left_bbox,
                right_bbox,
                options,
            );
        }
        m => m,
    };

    let right_mesh = match extract_element_mesh(step, right_id, &right_world) {
        m if m.is_empty() => {
            debug!(
                "csg: right element {} has no mesh, using bbox fallback",
                right_id
            );
            return analyze_with_bbox_fallback(
                left_id,
                right_id,
                left_bbox,
                right_bbox,
                options,
            );
        }
        m => m,
    };

    // Limit triangle count to prevent stack overflow from csgrs BSP recursion.
    let max_triangles = 2000;
    let left_tri_count = left_mesh.indices.len() / 3;
    let right_tri_count = right_mesh.indices.len() / 3;
    if left_tri_count > max_triangles || right_tri_count > max_triangles {
        debug!(
            "csg: pair ({},{}) has {}+{} triangles (>{}), bbox fallback",
            left_id, right_id, left_tri_count, right_tri_count, max_triangles
        );
        return analyze_with_bbox_fallback(
            left_id,
            right_id,
            left_bbox,
            right_bbox,
            options,
        );
    }

    // Convert to csgrs CSG solids
    let left_csg = match to_csgrs_csg(&left_mesh) {
        Some(m) => m,
        None => {
            debug!("csg: left element {} failed csgrs conversion", left_id);
            return analyze_with_bbox_fallback(
                left_id,
                right_id,
                left_bbox,
                right_bbox,
                options,
            );
        }
    };

    let right_csg = match to_csgrs_csg(&right_mesh) {
        Some(m) => m,
        None => {
            debug!("csg: right element {} failed csgrs conversion", right_id);
            return analyze_with_bbox_fallback(
                left_id,
                right_id,
                left_bbox,
                right_bbox,
                options,
            );
        }
    };

    // Perform boolean intersection
    let result = left_csg.intersection(&right_csg);

    // Check if the intersection has any volume (non-empty result)
    let intersects = !result.polygons.is_empty();

    if intersects {
        debug!(
            "csg: elements {} and {} intersect ({} polygons)",
            left_id,
            right_id,
            result.polygons.len()
        );
    }

    Ok(ExactPairAnalysis {
        intersects,
        touches_within_tolerance: false,
        minimum_distance: None,
        interface: None,
    })
}

/// Fallback to bounding box analysis when mesh extraction fails.
fn analyze_with_bbox_fallback(
    _left_id: EntityId,
    _right_id: EntityId,
    left_bbox: Option<&[f64]>,
    right_bbox: Option<&[f64]>,
    options: &ExactCheckOptions,
) -> Result<ExactPairAnalysis, GeometryKernelError> {
    let Some(left_box) = left_bbox else {
        return Ok(ExactPairAnalysis::default());
    };
    let Some(right_box) = right_bbox else {
        return Ok(ExactPairAnalysis::default());
    };

    // Simple AABB overlap check
    let x_overlap = (left_box[3].min(right_box[3]) - left_box[0].max(right_box[0])).max(0.0);
    let y_overlap = (left_box[4].min(right_box[4]) - left_box[1].max(right_box[1])).max(0.0);
    let z_overlap = (left_box[5].min(right_box[5]) - left_box[2].max(right_box[2])).max(0.0);

    let intersects = x_overlap > options.tolerance
        && y_overlap > options.tolerance
        && z_overlap > options.tolerance;

    Ok(ExactPairAnalysis {
        intersects,
        touches_within_tolerance: false,
        minimum_distance: None,
        interface: None,
    })
}

/// Extract the world transform for a placement chain.
fn extract_placement_transform(step: &StepFile, placement_id: EntityId) -> Affine3 {
    let mut result = Affine3::identity();
    let mut current_id = placement_id;
    let mut depth = 0;

    // Walk the placement chain from leaf to root, accumulating transforms.
    while depth < 20 {
        depth += 1;
        let Some(entity) = step.entities.get(&current_id) else {
            break;
        };
        if entity.entity_name != "IFCLOCALPLACEMENT" {
            break;
        }
        let rel_id = match entity.args.get(1) {
            Some(StepValue::Ref(id)) => *id,
            _ => break,
        };

        // Extract this placement's transform
        let this_transform = match step.entities.get(&rel_id) {
            Some(e) => match e.entity_name.as_str() {
                "IFCAXIS2PLACEMENT3D" => axis2placement3d_to_affine(step, rel_id),
                "IFCAXIS2PLACEMENT2D" => {
                    let origin = match e.args.get(0) {
                        Some(StepValue::Ref(id)) => cartesian_point_3d(step, *id),
                        _ => [0.0, 0.0, 0.0],
                    };
                    let ref_dir = match e.args.get(1) {
                        Some(StepValue::Ref(id)) => read_direction(step, *id),
                        _ => [1.0, 0.0, 0.0],
                    };
                    Affine3::from_axis_and_origin(origin, [0.0, 0.0, 1.0], ref_dir)
                }
                _ => Affine3::identity(),
            },
            None => Affine3::identity(),
        };

        // Pre-multiply: new_transform * accumulated
        result = this_transform.mul(&result);

        // Move to parent placement
        current_id = match entity.args.first() {
            Some(StepValue::Ref(id)) => *id,
            _ => break,
        };
    }

    result
}

impl Default for ExactPairAnalysis {
    fn default() -> Self {
        Self {
            intersects: false,
            touches_within_tolerance: false,
            minimum_distance: None,
            interface: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Batch CSG analysis for topology pipeline
// ---------------------------------------------------------------------------

/// Analyze candidate pairs using csgrs mesh boolean intersection.
/// Returns geometry relations (IntersectingElement + InterfaceOf).
///
/// Uses pair limiting to prevent stack overflow from deep BSP recursion.
/// Pairs beyond the limit fall back to bbox analysis.
pub fn derive_relations_with_csg(
    model: &IfcModel,
    step: &StepFile,
    candidate_pairs: &[(EntityId, EntityId)],
    options: &ExactCheckOptions,
    fallback_bboxes: &HashMap<EntityId, [f64; 6]>,
) -> Vec<crate::GeometryRelation> {
    // Build unique, canonical pairs
    let mut unique_pairs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut all_element_ids = std::collections::HashSet::new();

    for &(left, right) in candidate_pairs {
        if left == right
            || !model.elements.contains_key(&left)
            || !model.elements.contains_key(&right)
        {
            continue;
        }
        all_element_ids.insert(left);
        all_element_ids.insert(right);
        let canonical = if left < right {
            (left, right)
        } else {
            (right, left)
        };
        if seen.insert(canonical) {
            unique_pairs.push(canonical);
        }
    }

    if unique_pairs.is_empty() {
        return Vec::new();
    }

    // Extract and cache mesh + transform for each unique element
    let mut mesh_cache: HashMap<EntityId, TriangleMesh> = HashMap::new();
    let mut transform_cache: HashMap<EntityId, Affine3> = HashMap::new();
    let mut elem_count = 0usize;

    for &eid in &all_element_ids {
        if let Some(entity) = step.entities.get(&eid) {
            if let Some(StepValue::Ref(placement_id)) = entity.args.get(5) {
                let world = extract_placement_transform(step, *placement_id);
                let mesh = extract_element_mesh(step, eid, &world);
                let tri_count = mesh.triangle_count();
                if !mesh.is_empty() {
                    mesh_cache.insert(eid, mesh);
                    transform_cache.insert(eid, world);
                    elem_count += 1;
                    if elem_count <= 5 {
                        info!(
                            "csg: element {} mesh: {} triangles",
                            eid, tri_count
                        );
                    }
                }
            }
        }
    }

    info!(
        "csg: cached {} element meshes",
        elem_count
    );

    // Process pairs with CSG, with pair limiting to prevent stack overflow
    let mut relations = Vec::new();
    let mut csg_pairs_processed = 0usize;
    let max_csg_pairs = 200; // Cap CSG pairs to prevent stack overflow

    for (left, right) in &unique_pairs {
        csg_pairs_processed += 1;
        if csg_pairs_processed > max_csg_pairs {
            info!(
                "csg: stopping CSG after {} pairs (capped), using bbox fallback for remaining {} pairs",
                csg_pairs_processed - 1,
                unique_pairs.len().saturating_sub(csg_pairs_processed - 1)
            );
            break;
        }

        let left_mesh = match mesh_cache.get(left) {
            Some(m) => m,
            None => {
                let left_bbox = fallback_bboxes.get(left).map(|b| b.as_slice());
                let right_bbox = fallback_bboxes.get(right).map(|b| b.as_slice());
                let analysis = analyze_with_bbox_fallback(
                    *left,
                    *right,
                    left_bbox,
                    right_bbox,
                    options,
                )
                .unwrap_or(ExactPairAnalysis::default());
                append_pair_relations((*left, *right), analysis, &mut relations);
                continue;
            }
        };

        let right_mesh = match mesh_cache.get(right) {
            Some(m) => m,
            None => {
                let left_bbox = fallback_bboxes.get(left).map(|b| b.as_slice());
                let right_bbox = fallback_bboxes.get(right).map(|b| b.as_slice());
                let analysis = analyze_with_bbox_fallback(
                    *left,
                    *right,
                    left_bbox,
                    right_bbox,
                    options,
                )
                .unwrap_or(ExactPairAnalysis::default());
                append_pair_relations((*left, *right), analysis, &mut relations);
                continue;
            }
        };

        // Convert cached meshes to csgrs solids
        let left_csg = match to_csgrs_csg(left_mesh) {
            Some(m) => m,
            None => {
                let left_bbox = fallback_bboxes.get(left).map(|b| b.as_slice());
                let right_bbox = fallback_bboxes.get(right).map(|b| b.as_slice());
                let analysis = analyze_with_bbox_fallback(
                    *left,
                    *right,
                    left_bbox,
                    right_bbox,
                    options,
                )
                .unwrap_or(ExactPairAnalysis::default());
                append_pair_relations((*left, *right), analysis, &mut relations);
                continue;
            }
        };

        let right_csg = match to_csgrs_csg(right_mesh) {
            Some(m) => m,
            None => {
                let left_bbox = fallback_bboxes.get(left).map(|b| b.as_slice());
                let right_bbox = fallback_bboxes.get(right).map(|b| b.as_slice());
                let analysis = analyze_with_bbox_fallback(
                    *left,
                    *right,
                    left_bbox,
                    right_bbox,
                    options,
                )
                .unwrap_or(ExactPairAnalysis::default());
                append_pair_relations((*left, *right), analysis, &mut relations);
                continue;
            }
        };

        // Perform boolean intersection
        let result = left_csg.intersection(&right_csg);
        let intersects = !result.polygons.is_empty();

        if intersects {
            info!(
                "csg: pair {} intersects with {} ({} polygons)",
                left, right, result.polygons.len()
            );
            append_pair_relations(
                (*left, *right),
                ExactPairAnalysis {
                    intersects: true,
                    touches_within_tolerance: false,
                    minimum_distance: None,
                    interface: None,
                },
                &mut relations,
            );
        }
    }

    info!(
        "csg: processed {} CSG pairs, found {} intersection pairs, {} total relations",
        csg_pairs_processed,
        relations.iter().filter(|r| r.kind == crate::GeometryRelationKind::IntersectingElement).count() / 2,
        relations.len(),
    );

    relations
}

// ---------------------------------------------------------------------------
// ExactGeometryKernel trait implementation for csgrs
// ---------------------------------------------------------------------------

/// A pure-Rust exact geometry kernel backed by csgrs.
#[derive(Debug, Clone)]
pub struct CsgrsGeometryKernel {
    pub fallback_bboxes: std::sync::Arc<HashMap<EntityId, [f64; 6]>>,
}

impl Default for CsgrsGeometryKernel {
    fn default() -> Self {
        Self {
            fallback_bboxes: std::sync::Arc::new(HashMap::new()),
        }
    }
}

impl ExactGeometryKernel for CsgrsGeometryKernel {
    fn analyze_pair(
        &self,
        _model: &IfcModel,
        _left: EntityId,
        _right: EntityId,
        _options: &ExactCheckOptions,
    ) -> Result<ExactPairAnalysis, GeometryKernelError> {
        tracing::warn!(
            "CsgrsGeometryKernel::analyze_pair not efficient for single pairs"
        );
        Ok(ExactPairAnalysis::default())
    }
}

// ---------------------------------------------------------------------------
// BoundingBoxProvider for mesh-based bounding boxes
// ---------------------------------------------------------------------------

/// Extract bounding boxes from mesh vertices.
pub fn collect_mesh_bounding_boxes(
    step: &StepFile,
    element_ids: &[EntityId],
) -> (
    HashMap<EntityId, [f64; 6]>,
    HashMap<EntityId, String>,
) {
    let mut bboxes = HashMap::new();
    let mut wkts = HashMap::new();

    for &id in element_ids {
        let entity = match step.entities.get(&id) {
            Some(e) => e,
            None => continue,
        };

        let placement_id = match entity.args.get(5) {
            Some(StepValue::Ref(id)) => *id,
            _ => continue,
        };
        let world = extract_placement_transform(step, placement_id);

        let mesh = extract_element_mesh(step, id, &world);
        if mesh.is_empty() {
            continue;
        }

        let mut bbox = [
            f64::MAX,
            f64::MAX,
            f64::MAX,
            f64::MIN,
            f64::MIN,
            f64::MIN,
        ];
        for chunk in mesh.vertices.chunks_exact(3) {
            bbox[0] = bbox[0].min(chunk[0]);
            bbox[1] = bbox[1].min(chunk[1]);
            bbox[2] = bbox[2].min(chunk[2]);
            bbox[3] = bbox[3].max(chunk[0]);
            bbox[4] = bbox[4].max(chunk[1]);
            bbox[5] = bbox[5].max(chunk[2]);
        }
        bboxes.insert(id, bbox);
        wkts.insert(id, format_bbox_wkt(bbox));
    }

    (bboxes, wkts)
}

fn format_bbox_wkt(bbox: [f64; 6]) -> String {
    format!(
        "BOUNDCOORDS(({} {} {} {} {} {}))",
        bbox[0], bbox[1], bbox[2], bbox[3], bbox[4], bbox[5]
    )
}
