//! Triangle mesh extraction from IFC STEP representation items.
//!
//! Walks the representation tree of an IFC product element and extracts
//! triangle meshes from all supported geometry types:
//!
//! - **IfcTriangulatedFaceSet** (IFC4) — direct vertex+index extraction
//! - **IfcPolygonalFaceSet** (IFC4) — polygon fan triangulation
//! - **IfcFacetedBrep** (IFC2x3/4) — BRep with planar face polygons
//! - **IfcFaceBasedSurfaceModel** (IFC2x3) — set of connected face sets
//! - **IfcExtrudedAreaSolid** (IFC2x3/4) — profile extrusion → prism mesh
//! - **IfcBooleanClippingResult / IfcBooleanResult** — extracts the first operand's mesh
//! - **IfcMappedItem** — follows the mapping source to the underlying representation

use ifc_step::{EntityId, StepFile, StepValue};

use crate::transform::Transform4;

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

    /// Append another mesh into this one, offsetting indices.
    pub fn append(&mut self, other: &TriangleMesh) {
        let offset = (self.vertices.len() / 3) as u32;
        self.vertices.extend_from_slice(&other.vertices);
        self.indices
            .extend(other.indices.iter().map(|i| i + offset));
    }

    /// Apply a transform to all vertices in-place.
    pub fn transform(&mut self, t: &Transform4) {
        for chunk in self.vertices.chunks_exact_mut(3) {
            let p = [chunk[0], chunk[1], chunk[2]];
            let tp = t.transform_point(&p);
            chunk[0] = tp[0];
            chunk[1] = tp[1];
            chunk[2] = tp[2];
        }
    }
}

/// Extract the combined triangle mesh for an IFC element in world coordinates.
///
/// Walks the element's ProductRepresentation → ShapeRepresentation → Items,
/// applies the element's world placement transform, and merges all items
/// into a single mesh.
pub fn extract_element_mesh(
    step: &StepFile,
    element_id: EntityId,
    world_transform: &Transform4,
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

    // IfcProductDefinitionShape → Representations (list of IfcShapeRepresentation)
    let rep_entity = match step.entities.get(&rep_id) {
        Some(e) => e,
        None => return mesh,
    };

    // args[2] = Representations (list of refs to IfcShapeRepresentation)
    // For IfcProductDefinitionShape it's args[2], but some schemas use different indices.
    // Walk all list args looking for shape representations.
    let rep_list = match rep_entity.entity_name.as_str() {
        "IFCPRODUCTDEFINITIONSHAPE" => {
            // args[2] = Representations
            match rep_entity.args.get(2) {
                Some(StepValue::List(list)) => list.clone(),
                _ => return mesh,
            }
        }
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

        // Prefer "Body" representation context for geometry.
        // args[1] = RepresentationIdentifier (optional string)
        let ident = shape_rep.args.get(1).and_then(|v| v.as_str()).unwrap_or("");
        // We want "Body" or "Facetation" representations, skip "Axis", "FootPrint", "Box" etc.
        if !ident.is_empty() && ident != "Body" && ident != "Facetation" {
            continue;
        }

        // args[3] = Items (list of representation items)
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

    // Apply world transform to bring all vertices into global coordinates
    mesh.transform(world_transform);
    mesh
}

/// Extract a triangle mesh from a single representation item.
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
        "IFCBOOLEANCLIPPINGRESULT" | "IFCBOOLEANRESULT" => {
            // args[1] = FirstOperand — extract mesh from that
            match entity.args.get(1) {
                Some(StepValue::Ref(id)) => extract_representation_item(step, *id, depth + 1),
                _ => TriangleMesh::new(),
            }
        }
        "IFCMAPPEDITEM" => extract_mapped_item(step, entity, depth),
        _ => TriangleMesh::new(),
    }
}

// ---------------------------------------------------------------------------
// IfcTriangulatedFaceSet (IFC4)
// ---------------------------------------------------------------------------

fn extract_triangulated_face_set(step: &StepFile, entity: &ifc_step::RawEntity) -> TriangleMesh {
    // args[0] = Coordinates (ref to IfcCartesianPointList3D)
    // args[3] = CoordIndex (list of list of 3 ints) — 1-based indices
    let coords_id = match entity.args.get(0) {
        Some(StepValue::Ref(id)) => *id,
        _ => return TriangleMesh::new(),
    };
    let vertices = read_cartesian_point_list_3d(step, coords_id);
    if vertices.is_empty() {
        return TriangleMesh::new();
    }

    // CoordIndex — can be args[3] or args[4] depending on schema variant
    let coord_index = find_coord_index_list(entity);
    let coord_index = match coord_index {
        Some(list) => list,
        None => return TriangleMesh::new(),
    };

    let mut indices = Vec::new();
    for tri in coord_index {
        if let StepValue::List(idx_list) = tri {
            if idx_list.len() >= 3 {
                // 1-based → 0-based
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

/// Find the CoordIndex list in a IfcTriangulatedFaceSet.
/// It can be at args[3] or args[4] depending on whether Normals is present.
fn find_coord_index_list(entity: &ifc_step::RawEntity) -> Option<&[StepValue]> {
    // Try args[3] and args[4] — the CoordIndex is a list of lists
    for idx in [3, 4] {
        if let Some(StepValue::List(list)) = entity.args.get(idx) {
            // Verify it's a list of lists (index triples)
            if list
                .first()
                .map_or(false, |v| matches!(v, StepValue::List(_)))
            {
                return Some(list);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// IfcPolygonalFaceSet (IFC4)
// ---------------------------------------------------------------------------

fn extract_polygonal_face_set(step: &StepFile, entity: &ifc_step::RawEntity) -> TriangleMesh {
    // args[0] = Coordinates (ref to IfcCartesianPointList3D)
    // args[2] = Faces (list of refs to IfcIndexedPolygonalFace)
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
        // IfcIndexedPolygonalFace: args[0] = CoordIndex (list of ints, 1-based)
        // IfcIndexedPolygonalFaceWithVoids: args[0] = CoordIndex, args[1] = InnerCoordIndices
        let coord_idx = match face_entity.args.get(0) {
            Some(StepValue::List(list)) => list,
            _ => continue,
        };
        // Fan triangulation of the polygon
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

// ---------------------------------------------------------------------------
// IfcFacetedBrep (IFC2x3 / IFC4)
// ---------------------------------------------------------------------------

fn extract_faceted_brep(step: &StepFile, entity: &ifc_step::RawEntity) -> TriangleMesh {
    // args[0] = Outer (ref to IfcClosedShell)
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
    // IfcClosedShell / IfcOpenShell: args[0] = CfsFaces (list of refs to IfcFace)
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
    // IfcFace: args[0] = Bounds (list of refs to IfcFaceBound / IfcFaceOuterBound)
    let bounds = match face.args.get(0) {
        Some(StepValue::List(list)) => list,
        _ => return TriangleMesh::new(),
    };

    // Collect the outer bound polygon (take the first IfcFaceOuterBound, or just the first bound)
    for bound_ref in bounds {
        let bound_id = match bound_ref {
            StepValue::Ref(id) => *id,
            _ => continue,
        };
        let bound = match step.entities.get(&bound_id) {
            Some(e) => e,
            None => continue,
        };
        // IfcFaceBound / IfcFaceOuterBound: args[0] = Bound (ref to IfcPolyLoop), args[1] = Orientation
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
        // IfcPolyLoop: args[0] = Polygon (list of refs to IfcCartesianPoint)
        let polygon_refs = match loop_entity.args.get(0) {
            Some(StepValue::List(list)) => list,
            _ => continue,
        };
        let mut polygon_pts: Vec<[f64; 3]> = Vec::new();
        for pt_ref in polygon_refs {
            if let StepValue::Ref(pt_id) = pt_ref {
                let pt = cartesian_point_3d(step, *pt_id);
                polygon_pts.push(pt);
            }
        }
        if polygon_pts.len() >= 3 {
            return triangulate_polygon(&polygon_pts);
        }
    }

    TriangleMesh::new()
}

// ---------------------------------------------------------------------------
// IfcFaceBasedSurfaceModel (IFC2x3)
// ---------------------------------------------------------------------------

fn extract_face_based_surface_model(step: &StepFile, entity: &ifc_step::RawEntity) -> TriangleMesh {
    // args[0] = FbsmFaces (set of refs to IfcConnectedFaceSet)
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
        // IfcConnectedFaceSet is essentially a shell
        let sub_mesh = extract_shell(step, set_id);
        mesh.append(&sub_mesh);
    }
    mesh
}

// ---------------------------------------------------------------------------
// IfcExtrudedAreaSolid (IFC2x3 / IFC4)
// ---------------------------------------------------------------------------

fn extract_extruded_area_solid(step: &StepFile, entity: &ifc_step::RawEntity) -> TriangleMesh {
    // args[0] = SweptArea (ref to IfcProfileDef)
    // args[1] = Position (ref to IfcAxis2Placement3D) — local placement of the extrusion
    // args[2] = ExtrudedDirection (ref to IfcDirection)
    // args[3] = Depth (real)

    let profile_id = match entity.args.get(0) {
        Some(StepValue::Ref(id)) => *id,
        _ => return TriangleMesh::new(),
    };

    let local_transform = match entity.args.get(1) {
        Some(StepValue::Ref(id)) => crate::transform::Transform4::from_axis_and_origin(
            read_placement_origin(step, *id),
            read_placement_axis(step, *id),
            read_placement_ref_dir(step, *id),
        ),
        _ => Transform4::identity(),
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

    // Extract profile polygon
    let profile_pts = extract_profile_polygon(step, profile_id);
    if profile_pts.len() < 3 {
        return TriangleMesh::new();
    }

    // Build prism mesh: bottom face + top face + side quads
    let extrude_vec = [
        extrude_dir[0] * depth,
        extrude_dir[1] * depth,
        extrude_dir[2] * depth,
    ];

    let mut mesh = build_prism_mesh(&profile_pts, &extrude_vec);

    // Apply local transform
    mesh.transform(&local_transform);

    mesh
}

/// Extract a polygon from an IFC profile definition.
fn extract_profile_polygon(step: &StepFile, profile_id: EntityId) -> Vec<[f64; 3]> {
    let entity = match step.entities.get(&profile_id) {
        Some(e) => e,
        None => return Vec::new(),
    };

    match entity.entity_name.as_str() {
        "IFCARBITRARYCLOSEDPROFILEDEF" | "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
            // args[1] (or args[2] for WithVoids) = OuterCurve (ref to IfcPolyline or IfcCompositeCurve)
            // ArbitraryClosedProfileDef: args[0]=ProfileType, args[1]=ProfileName, args[2]=OuterCurve
            let curve_id = match entity.args.get(2) {
                Some(StepValue::Ref(id)) => *id,
                _ => {
                    // Try args[1] as fallback
                    match entity.args.get(1) {
                        Some(StepValue::Ref(id)) => *id,
                        _ => return Vec::new(),
                    }
                }
            };
            extract_curve_points(step, curve_id)
        }
        "IFCRECTANGLEPROFILEDEF" | "IFCRECTANGLEHOLLOWPROFILEDEF" => {
            // args[0] = ProfileType, args[1] = ProfileName, args[2] = Position, args[3] = XDim, args[4] = YDim
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

            // Apply profile position if present
            if let Some(pos_id) = position {
                let t = profile_placement_transform(step, pos_id);
                for pt in &mut pts {
                    *pt = t.transform_point(pt);
                }
            }
            pts
        }
        "IFCCIRCLEPROFILEDEF" | "IFCCIRCLEHOLLOWPROFILEDEF" => {
            // Approximate circle with a 16-sided polygon
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
            // I-beam profile: args[3]=OverallWidth, args[4]=OverallDepth, args[5]=WebThickness,
            // args[6]=FlangeThickness, args[7]=FilletRadius (optional)
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

            // I-beam cross section (12 points, CCW)
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

/// Extract points from a curve entity (IfcPolyline, IfcCompositeCurve, IfcIndexedPolyCurve).
fn extract_curve_points(step: &StepFile, curve_id: EntityId) -> Vec<[f64; 3]> {
    let entity = match step.entities.get(&curve_id) {
        Some(e) => e,
        None => return Vec::new(),
    };

    match entity.entity_name.as_str() {
        "IFCPOLYLINE" => {
            // args[0] = Points (list of refs to IfcCartesianPoint)
            match entity.args.get(0) {
                Some(StepValue::List(list)) => {
                    let mut pts = Vec::new();
                    for pt_ref in list {
                        if let StepValue::Ref(pt_id) = pt_ref {
                            pts.push(cartesian_point_3d(step, *pt_id));
                        }
                    }
                    // Remove last point if it duplicates the first (closed polyline)
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
            }
        }
        "IFCINDEXEDPOLYCURVE" => {
            // IFC4: args[0] = Points (ref to IfcCartesianPointList2D or 3D)
            // args[1] = Segments (optional list), args[2] = SelfIntersect
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
            // args[0] = Segments (list of refs to IfcCompositeCurveSegment)
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
                // IfcCompositeCurveSegment: args[2] = ParentCurve
                let parent_curve_id = match seg.args.get(2) {
                    Some(StepValue::Ref(id)) => *id,
                    _ => continue,
                };
                let seg_pts = extract_curve_points(step, parent_curve_id);
                pts.extend(seg_pts);
            }
            // Deduplicate consecutive duplicate points
            pts.dedup_by(|a, b| {
                (a[0] - b[0]).abs() < 1e-10
                    && (a[1] - b[1]).abs() < 1e-10
                    && (a[2] - b[2]).abs() < 1e-10
            });
            pts
        }
        "IFCTRIMMEDCURVE" => {
            // Approximate: just use start and end trim points
            // args[0] = BasisCurve, args[1] = Trim1, args[2] = Trim2
            // For simplicity, extract basis curve points
            match entity.args.get(0) {
                Some(StepValue::Ref(id)) => extract_curve_points(step, *id),
                _ => Vec::new(),
            }
        }
        "IFCCIRCLE" | "IFCELLIPSE" => {
            // Approximate with polygon
            let radius = entity.args.get(1).and_then(|v| v.as_real()).unwrap_or(0.5);
            let n = 16;
            let mut pts = Vec::with_capacity(n);
            for i in 0..n {
                let angle = 2.0 * std::f64::consts::PI * (i as f64) / (n as f64);
                pts.push([radius * angle.cos(), radius * angle.sin(), 0.0]);
            }
            pts
        }
        "IFCLINE" => {
            // args[0] = Pnt (CartesianPoint), args[1] = Dir (IfcVector)
            // Lines are infinite — return just the start point
            match entity.args.get(0) {
                Some(StepValue::Ref(id)) => vec![cartesian_point_3d(step, *id)],
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// IfcMappedItem
// ---------------------------------------------------------------------------

fn extract_mapped_item(
    step: &StepFile,
    entity: &ifc_step::RawEntity,
    depth: usize,
) -> TriangleMesh {
    // args[0] = MappingSource (ref to IfcRepresentationMap)
    // args[1] = MappingTarget (ref to IfcCartesianTransformationOperator3D)
    let map_source_id = match entity.args.get(0) {
        Some(StepValue::Ref(id)) => *id,
        _ => return TriangleMesh::new(),
    };

    let target_transform = match entity.args.get(1) {
        Some(StepValue::Ref(id)) => read_cartesian_transform_operator(step, *id),
        _ => Transform4::identity(),
    };

    let map_source = match step.entities.get(&map_source_id) {
        Some(e) => e,
        None => return TriangleMesh::new(),
    };

    // IfcRepresentationMap: args[0] = MappingOrigin (IfcAxis2Placement), args[1] = MappedRepresentation
    let origin_transform = match map_source.args.get(0) {
        Some(StepValue::Ref(id)) => {
            let origin_entity = step.entities.get(id);
            if let Some(oe) = origin_entity {
                if oe.entity_name == "IFCAXIS2PLACEMENT3D" {
                    Transform4::from_axis_and_origin(
                        read_placement_origin(step, *id),
                        read_placement_axis(step, *id),
                        read_placement_ref_dir(step, *id),
                    )
                } else {
                    Transform4::identity()
                }
            } else {
                Transform4::identity()
            }
        }
        _ => Transform4::identity(),
    };

    let mapped_rep_id = match map_source.args.get(1) {
        Some(StepValue::Ref(id)) => *id,
        _ => return TriangleMesh::new(),
    };

    // Get items from the mapped representation
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

    // Apply: target_transform * origin_transform
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

fn profile_placement_transform(step: &StepFile, pos_id: EntityId) -> Transform4 {
    let entity = match step.entities.get(&pos_id) {
        Some(e) => e,
        None => return Transform4::identity(),
    };
    match entity.entity_name.as_str() {
        "IFCAXIS2PLACEMENT3D" => Transform4::from_axis_and_origin(
            read_placement_origin(step, pos_id),
            read_placement_axis(step, pos_id),
            read_placement_ref_dir(step, pos_id),
        ),
        "IFCAXIS2PLACEMENT2D" => {
            // args[0] = Location (2D point), args[1] = RefDirection (2D direction)
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
            Transform4::from_axis_and_origin(origin, [0.0, 0.0, 1.0], ref_dir)
        }
        _ => Transform4::identity(),
    }
}

fn read_cartesian_transform_operator(step: &StepFile, id: EntityId) -> Transform4 {
    let entity = match step.entities.get(&id) {
        Some(e) => e,
        None => return Transform4::identity(),
    };
    // IfcCartesianTransformationOperator3D:
    // args[0] = Axis1 (X), args[1] = Axis2 (Y), args[2] = LocalOrigin, args[3] = Scale, args[4] = Axis3 (Z)
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

    let mut t = Transform4::from_axis_and_origin(origin, z_axis, x_axis);
    if (scale - 1.0).abs() > 1e-10 {
        // Apply scale to rotation columns
        for col in 0..3 {
            for row in 0..3 {
                t.m[col][row] *= scale;
            }
        }
    }
    t
}

/// Build a prism mesh from a polygon profile and an extrusion vector.
fn build_prism_mesh(profile: &[[f64; 3]], extrude: &[f64; 3]) -> TriangleMesh {
    let n = profile.len();
    // Vertices: bottom ring (0..n) + top ring (n..2n)
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

/// Fan-triangulate a planar polygon.
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
