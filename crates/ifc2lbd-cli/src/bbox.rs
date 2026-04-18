//! Bounding box extraction and adjacency detection for IFC building elements.
//!
//! Provides approximate and exact bounding box computation from STEP data,
//! voxel-based adjacency detection, and WKT serialization for GeoSPARQL output.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use ifc_model::IfcModel;
use ifc_step::{EntityId, StepFile, StepValue};
use lbd_geometry::{BoundingBox, GeometryRelation, GeometryRelationKind};
use rayon::prelude::*;
use rstar::{RTree, RTreeObject, AABB};
use serde::Serialize;

use crate::mesh;
use crate::transform;
use crate::voxel;

pub(crate) fn approximate_bbox(step: &StepFile, element_id: EntityId) -> Option<[f64; 6]> {
    let entity = step.entities.get(&element_id)?;
    // ObjectPlacement is args[5] for most IfcProduct subtypes.
    let placement_id = match entity.args.get(5) {
        Some(StepValue::Ref(id)) => *id,
        _ => return None,
    };
    // Walk the placement chain to get the world translation (approximate: take the
    // last LocalPlacement translation, ignoring rotation and parent transforms).
    // Good enough for spatial pre-filtering within a single storey.
    let world_translate = placement_translation(step, placement_id);

    // Collect all 3D coordinate values from the representation items.
    let rep_id = match entity.args.get(6) {
        Some(StepValue::Ref(id)) => *id,
        _ => {
            // No representation — use placement origin as a point bbox.
            let [x, y, z] = world_translate;
            return Some([x, y, z, x, y, z]);
        }
    };
    let mut pts: Vec<[f64; 3]> = Vec::new();
    collect_points(step, rep_id, &mut pts, 0, 300);
    // Elements with > 300 coordinate points have complex/freeform geometry (furniture,
    // appliances, MEP). These are never structural and we skip them for topology analysis.
    if pts.len() >= 300 {
        return None;
    }
    if pts.is_empty() {
        let [x, y, z] = world_translate;
        return Some([x, y, z, x, y, z]);
    }
    // Apply the world translation to all collected points.
    let [tx, ty, tz] = world_translate;
    let mut min = [f64::MAX; 3];
    let mut max = [f64::MIN; 3];
    for [x, y, z] in &pts {
        let wx = x + tx;
        let wy = y + ty;
        let wz = z + tz;
        min[0] = min[0].min(wx);
        min[1] = min[1].min(wy);
        min[2] = min[2].min(wz);
        max[0] = max[0].max(wx);
        max[1] = max[1].max(wy);
        max[2] = max[2].max(wz);
    }
    Some([min[0], min[1], min[2], max[0], max[1], max[2]])
}

/// Walk a placement chain and return the accumulated translation (sum of all local origins).
/// Ignores rotation for speed — sufficient for spatial pre-filtering.
pub(crate) fn placement_translation(step: &StepFile, placement_id: EntityId) -> [f64; 3] {
    let mut tx = 0.0f64;
    let mut ty = 0.0f64;
    let mut tz = 0.0f64;
    let mut current_id = placement_id;
    let mut depth = 0;
    loop {
        if depth > 20 {
            break;
        } // guard against cycles
        depth += 1;
        let Some(entity) = step.entities.get(&current_id) else {
            break;
        };
        match entity.entity_name.as_str() {
            "IFCLOCALPLACEMENT" => {
                // args[0] = PlacementRelTo (parent, optional), args[1] = RelativePlacement
                let rel_id = match entity.args.get(1) {
                    Some(StepValue::Ref(id)) => *id,
                    _ => break,
                };
                let [lx, ly, lz] = axis2placement3d_origin(step, rel_id);
                tx += lx;
                ty += ly;
                tz += lz;
                match entity.args.first() {
                    Some(StepValue::Ref(parent_id)) => {
                        current_id = *parent_id;
                    }
                    _ => break,
                }
            }
            _ => break,
        }
    }
    [tx, ty, tz]
}

pub(crate) fn axis2placement3d_origin(step: &StepFile, id: EntityId) -> [f64; 3] {
    let Some(entity) = step.entities.get(&id) else {
        return [0.0, 0.0, 0.0];
    };
    if entity.entity_name != "IFCAXIS2PLACEMENT3D" {
        return [0.0, 0.0, 0.0];
    }
    let loc_id = match entity.args.first() {
        Some(StepValue::Ref(id)) => *id,
        _ => return [0.0, 0.0, 0.0],
    };
    cartesian_point_3d(step, loc_id)
}

pub(crate) fn cartesian_point_3d(step: &StepFile, id: EntityId) -> [f64; 3] {
    let Some(entity) = step.entities.get(&id) else {
        return [0.0, 0.0, 0.0];
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

/// Recursively collect 3D coordinate values from an IFC entity tree.
/// Stops at depth 10 to avoid runaway traversal.
pub(crate) fn collect_points(
    step: &StepFile,
    id: EntityId,
    pts: &mut Vec<[f64; 3]>,
    depth: usize,
    max: usize,
) {
    if depth > 10 || pts.len() >= max {
        return;
    }
    let Some(entity) = step.entities.get(&id) else {
        return;
    };
    match entity.entity_name.as_str() {
        "IFCCARTESIANPOINT" => {
            if let Some(StepValue::List(coords)) = entity.args.first() {
                if coords.len() >= 3 {
                    let x = coords[0].as_real().unwrap_or(0.0);
                    let y = coords[1].as_real().unwrap_or(0.0);
                    let z = coords[2].as_real().unwrap_or(0.0);
                    pts.push([x, y, z]);
                }
            }
        }
        "IFCCARTESIANPOINTLIST3D" => {
            if let Some(StepValue::List(list)) = entity.args.first() {
                for item in list {
                    if let StepValue::List(coords) = item {
                        if coords.len() >= 3 {
                            let x = coords[0].as_real().unwrap_or(0.0);
                            let y = coords[1].as_real().unwrap_or(0.0);
                            let z = coords[2].as_real().unwrap_or(0.0);
                            pts.push([x, y, z]);
                        }
                    }
                }
            }
        }
        _ => {
            // Walk references in args.
            for arg in &entity.args {
                match arg {
                    StepValue::Ref(child_id) => {
                        collect_points(step, *child_id, pts, depth + 1, max);
                    }
                    StepValue::List(list) => {
                        for item in list {
                            if pts.len() >= max {
                                return;
                            }
                            if let StepValue::Ref(child_id) = item {
                                collect_points(step, *child_id, pts, depth + 1, max);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

pub(crate) fn bboxes_overlap_3d(a: &[f64; 6], b: &[f64; 6], tolerance: f64) -> bool {
    // X
    a[0] - tolerance <= b[3] + tolerance
        && a[3] + tolerance >= b[0] - tolerance
    // Y
        && a[1] - tolerance <= b[4] + tolerance
        && a[4] + tolerance >= b[1] - tolerance
    // Z
        && a[2] - tolerance <= b[5] + tolerance
        && a[5] + tolerance >= b[2] - tolerance
}

/// IFC element types that can generate bot:Interface (shared surfaces between structural elements).
/// Furniture, MEP, sanitary, distribution, and annotation elements are excluded — they
/// never touch structural elements in a surface-sharing sense.
pub(crate) fn is_structural_ifc_type(entity_name: &str) -> bool {
    matches!(
        entity_name,
        "IFCWALL"
            | "IFCWALLSTANDARDCASE"
            | "IFCSLAB"
            | "IFCCOLUMN"
            | "IFCBEAM"
            | "IFCROOF"
            | "IFCCOVERING"
            | "IFCCURTAINWALL"
            | "IFCPLATE"
            | "IFCMEMBER"
            | "IFCDOOR"
            | "IFCWINDOW"
            | "IFCSTAIR"
            | "IFCSTAIRFLIGHT"
            | "IFCRAMP"
            | "IFCRAMPFLIGHT"
            | "IFCFOOTING"
            | "IFCPILE"
            | "IFCBUILDINGELEMENTPROXY"
    )
}

pub(crate) fn semantic_candidate_pairs(
    model: &IfcModel,
    step: &StepFile,
) -> (Vec<(EntityId, EntityId)>, HashMap<EntityId, [f64; 6]>) {
    // Legacy function — delegates to the new R-tree based approach.
    rtree_candidate_pairs(model, step)
}

// ---------------------------------------------------------------------------
// R-tree based candidate pair generation
// ---------------------------------------------------------------------------

/// A spatially-indexed bounding box entry for the R-tree.
/// Stores the element's EntityId and its approximate axis-aligned bbox.
#[derive(Clone, Debug)]
struct SpatialBbox {
    entity_id: EntityId,
    bbox: [f64; 6], // [xmin, ymin, zmin, xmax, ymax, zmax]
}

impl RTreeObject for SpatialBbox {
    type Envelope = AABB<[f64; 3]>;

    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners(
            [self.bbox[0], self.bbox[1], self.bbox[2]],
            [self.bbox[3], self.bbox[4], self.bbox[5]],
        )
    }
}

/// Generate candidate element pairs using R-tree spatial indexing.
///
/// Unlike the old storey-scoped approach, this finds ALL pairs of structural
/// elements whose bounding boxes overlap — regardless of which storey they
/// belong to. This correctly catches multi-storey columns, slab-to-wall
/// connections across storeys, foundation-to-column connections, etc.
///
/// Returns (sorted candidate pairs, approximate bboxes per element).
pub(crate) fn rtree_candidate_pairs(
    model: &IfcModel,
    step: &StepFile,
) -> (Vec<(EntityId, EntityId)>, HashMap<EntityId, [f64; 6]>) {
    let rtree_start = Instant::now();

    // Step 1: Compute approximate bboxes for all structural elements.
    let mut element_bboxes: HashMap<EntityId, [f64; 6]> = HashMap::new();
    for (&entity_id, node) in &model.elements {
        if !is_structural_ifc_type(node.entity_name.as_str()) {
            continue;
        }
        if let Some(bbox) = approximate_bbox(step, entity_id) {
            element_bboxes.insert(entity_id, bbox);
        }
    }

    let total_structural = element_bboxes.len();

    // Step 2: Build R-tree from all structural element bboxes.
    let entries: Vec<SpatialBbox> = element_bboxes
        .iter()
        .map(|(&entity_id, bbox)| SpatialBbox {
            entity_id,
            bbox: *bbox,
        })
        .collect();
    let rtree: RTree<SpatialBbox> = RTree::bulk_load(entries);

    // Step 3: Query overlapping pairs with 5cm tolerance.
    // For each element, expand its bbox by the tolerance and find all overlapping
    // entries. Collect unique canonical pairs (smaller ID first).
    const BBOX_TOLERANCE: f64 = 0.05; // 5cm — covers placement approximation errors
    let mut pairs = HashSet::new();
    for (&entity_id, bbox) in &element_bboxes {
        // Expand query envelope by tolerance
        let query_envelope = AABB::from_corners(
            [
                bbox[0] - BBOX_TOLERANCE,
                bbox[1] - BBOX_TOLERANCE,
                bbox[2] - BBOX_TOLERANCE,
            ],
            [
                bbox[3] + BBOX_TOLERANCE,
                bbox[4] + BBOX_TOLERANCE,
                bbox[5] + BBOX_TOLERANCE,
            ],
        );
        for neighbor in rtree.locate_in_envelope_intersecting(&query_envelope) {
            if neighbor.entity_id == entity_id {
                continue;
            }
            let canonical = if entity_id < neighbor.entity_id {
                (entity_id, neighbor.entity_id)
            } else {
                (neighbor.entity_id, entity_id)
            };
            pairs.insert(canonical);
        }
    }

    if pairs.len() > 100_000 {
        tracing::warn!(
            "R-tree candidate pairs ({}) exceeds 100k — consider stricter filtering",
            pairs.len()
        );
    }

    let mut out: Vec<_> = pairs.into_iter().collect();
    out.sort_unstable();

    tracing::info!(
        "R-tree broad-phase: {} structural elements, {} candidate pairs in {:.3}s",
        total_structural,
        out.len(),
        rtree_start.elapsed().as_secs_f64(),
    );

    (out, element_bboxes)
}

/// Voxel-based adjacency detection with externally provided candidate pairs.
///
/// For each unique element in the pairs, extract its triangle mesh, voxelize it,
/// and check face-adjacency (6-connectivity) between each candidate pair.
///
/// Returns (relations, mesh_bboxes) where mesh_bboxes maps EntityId → world-space bbox
/// computed from the actual triangle mesh.
pub(crate) fn voxel_adjacency_relations_with_candidates(
    step: &StepFile,
    candidate_pairs: &[(EntityId, EntityId)],
    cell_size: f64,
    max_element_voxels: usize,
) -> (Vec<GeometryRelation>, HashMap<EntityId, [f64; 6]>) {
    tracing::info!(
        "voxel narrow-phase: {} candidate pairs from {} unique elements",
        candidate_pairs.len(),
        {
            let mut ids = HashSet::new();
            for (a, b) in candidate_pairs {
                ids.insert(*a);
                ids.insert(*b);
            }
            ids.len()
        }
    );

    if candidate_pairs.is_empty() {
        return (Vec::new(), HashMap::new());
    }

    // Collect unique element IDs
    let mut element_ids: Vec<EntityId> = {
        let mut ids = HashSet::new();
        for (a, b) in candidate_pairs {
            ids.insert(*a);
            ids.insert(*b);
        }
        ids.into_iter().collect()
    };
    element_ids.sort_unstable();

    // Step 1: Extract and voxelize all elements in parallel; capture mesh bboxes.
    let voxel_start = Instant::now();
    let voxel_maps: Vec<(EntityId, HashSet<voxel::VoxelCoord>, [f64; 6])> = element_ids
        .par_iter()
        .filter_map(|&eid| {
            let world_t = transform::element_world_transform(step, eid);
            let mesh = mesh::extract_element_mesh(step, eid, &world_t);
            if mesh.is_empty() {
                tracing::debug!("element #{} has no mesh", eid);
                return None;
            }
            // Compute mesh bbox in world coordinates
            let mut mn = [f64::MAX; 3];
            let mut mx = [f64::MIN; 3];
            for chunk in mesh.vertices.chunks_exact(3) {
                for i in 0..3 {
                    mn[i] = mn[i].min(chunk[i]);
                    mx[i] = mx[i].max(chunk[i]);
                }
            }
            let bbox = [mn[0], mn[1], mn[2], mx[0], mx[1], mx[2]];
            let voxels = voxel::voxelize_triangles(&mesh.vertices, &mesh.indices, cell_size);
            if voxels.is_empty() {
                return None;
            }
            if max_element_voxels > 0 && voxels.len() > max_element_voxels {
                tracing::warn!(
                    "element #{} skipped: {} voxels exceeds limit {} (bbox {:.1}×{:.1}×{:.1}m)",
                    eid,
                    voxels.len(),
                    max_element_voxels,
                    mx[0] - mn[0],
                    mx[1] - mn[1],
                    mx[2] - mn[2],
                );
                return None;
            }
            Some((eid, voxels, bbox))
        })
        .collect();

    let mut mesh_bboxes: HashMap<EntityId, [f64; 6]> = HashMap::with_capacity(voxel_maps.len());
    let voxel_map: HashMap<EntityId, HashSet<voxel::VoxelCoord>> = voxel_maps
        .into_iter()
        .map(|(eid, voxels, bbox)| {
            mesh_bboxes.insert(eid, bbox);
            (eid, voxels)
        })
        .collect();

    let meshed = voxel_map.len();
    let total_voxels: usize = voxel_map.values().map(|v| v.len()).sum();
    tracing::info!(
        "voxelized {}/{} elements ({} total voxels) in {:.3}s",
        meshed,
        element_ids.len(),
        total_voxels,
        voxel_start.elapsed().as_secs_f64(),
    );

    // Step 2: Check voxel overlap (volumetric intersection) AND adjacency (surface contact)
    // for all candidate pairs in parallel.
    // Uses the voxel sets already computed — no CSG, no stack overflow risk.
    let adj_start = Instant::now();
    let overlapping_pairs: Vec<(EntityId, EntityId)> = candidate_pairs
        .par_iter()
        .filter_map(|&(a, b)| {
            let va = voxel_map.get(&a)?;
            let vb = voxel_map.get(&b)?;
            // Volumetric intersection: do the two element voxel sets share any voxel?
            let intersects = va.iter().any(|v| vb.contains(v));
            if intersects {
                Some((a, b))
            } else {
                None
            }
        })
        .collect();

    // Step 3: Build proper BOT relations per spec:
    //   - bot:intersectingElement for volumetric overlap
    //   - bot:Interface instance with bot:interfaceOf for surface contact (adjacency)
    // bot:adjacentElement is Zone→Element only per BOT spec.
    // Synthetic interface IDs: use a range above any real entity ID.
    let max_entity_id = step.entities.keys().copied().max().unwrap_or(0);
    let mut relations = Vec::with_capacity(overlapping_pairs.len() * 4);
    for (i, &(a, b)) in overlapping_pairs.iter().enumerate() {
        let interface_id = max_entity_id + 1 + i as u64;
        // IntersectingElement both directions (volumetric overlap)
        relations.push(GeometryRelation {
            source: a,
            target: b,
            kind: GeometryRelationKind::IntersectingElement,
        });
        relations.push(GeometryRelation {
            source: b,
            target: a,
            kind: GeometryRelationKind::IntersectingElement,
        });
        // InterfaceOf: synthetic interface → both elements (surface contact via shared voxels)
        relations.push(GeometryRelation {
            source: interface_id,
            target: a,
            kind: GeometryRelationKind::InterfaceOf,
        });
        relations.push(GeometryRelation {
            source: interface_id,
            target: b,
            kind: GeometryRelationKind::InterfaceOf,
        });
    }

    tracing::info!(
        "voxel narrow-phase: {} intersecting pairs found from {} candidates in {:.3}s",
        overlapping_pairs.len(),
        candidate_pairs.len(),
        adj_start.elapsed().as_secs_f64(),
    );

    (relations, mesh_bboxes)
}

/// Legacy voxel adjacency function — uses R-tree candidate generation internally.
pub(crate) fn voxel_adjacency_relations(
    model: &IfcModel,
    step: &StepFile,
    cell_size: f64,
    max_element_voxels: usize,
) -> (Vec<GeometryRelation>, HashMap<EntityId, [f64; 6]>) {
    let (candidates, _) = rtree_candidate_pairs(model, step);
    voxel_adjacency_relations_with_candidates(step, &candidates, cell_size, max_element_voxels)
}

// ---------------------------------------------------------------------------
// Combined R-tree + Voxel topology pipeline
// ---------------------------------------------------------------------------

/// Two-stage topology detection pipeline:
///   Stage 1 (broad-phase): R-tree spatial indexing finds all element pairs
///            whose approximate bboxes overlap (no storey constraint).
///   Stage 2 (narrow-phase): Voxel adjacency confirms actual surface contact
///            between candidate pairs.
///
/// Returns (geometry relations, mesh bboxes in world coordinates).
pub(crate) fn rtree_voxel_topology_relations(
    model: &IfcModel,
    step: &StepFile,
    cell_size: f64,
    max_element_voxels: usize,
) -> (Vec<GeometryRelation>, HashMap<EntityId, [f64; 6]>) {
    let total_start = Instant::now();

    // Stage 1: R-tree broad-phase
    let (candidates, _approx_bboxes) = rtree_candidate_pairs(model, step);

    // Stage 2: Voxel narrow-phase
    let (relations, mesh_bboxes) =
        voxel_adjacency_relations_with_candidates(step, &candidates, cell_size, max_element_voxels);

    tracing::info!(
        "R-tree + Voxel pipeline: {} broad-phase → {} confirmed relations in {:.3}s",
        candidates.len(),
        relations.len(),
        total_start.elapsed().as_secs_f64(),
    );

    (relations, mesh_bboxes)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BboxOutlier {
    entity_id: EntityId,
    inflation_fast: f64,
    inflation_final: f64,
    used_exact: bool,
    used_rotated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BboxQualityReport {
    pub(crate) elements_requested: usize,
    pub(crate) elements_with_mesh: usize,
    pub(crate) escalated_exact_count: usize,
    pub(crate) rotated_bbox_count: usize,
    pub(crate) avg_inflation_fast: f64,
    pub(crate) max_inflation_fast: f64,
    pub(crate) avg_inflation_final: f64,
    pub(crate) max_inflation_final: f64,
    pub(crate) avg_escalated_reduction_ratio: f64,
    pub(crate) count_fast_over_1_2: usize,
    pub(crate) count_fast_over_1_5: usize,
    pub(crate) count_fast_over_1_8: usize,
    pub(crate) count_fast_over_2_0: usize,
    pub(crate) inflation_threshold: f64,
    pub(crate) top_inflation_outliers: Vec<BboxOutlier>,
}

pub(crate) fn collect_mesh_bounding_boxes_hybrid(
    step: &StepFile,
    element_ids: Vec<EntityId>,
    inflation_threshold: f64,
) -> (
    HashMap<EntityId, [f64; 6]>,
    HashMap<EntityId, String>,
    BboxQualityReport,
) {
    let records: Vec<(EntityId, [f64; 6], String, f64, f64, bool, bool)> = element_ids
        .par_iter()
        .filter_map(|&eid| {
            let world_t = transform::element_world_transform(step, eid);
            let local_mesh =
                mesh::extract_element_mesh(step, eid, &transform::Transform4::identity());
            if local_mesh.is_empty() {
                return None;
            }
            let local_bbox = bbox_from_vertices(&local_mesh.vertices)?;
            let local_volume = bbox_volume(&local_bbox);
            let fast_world_bbox = transform_aabb(&world_t, &local_bbox);
            let fast_world_volume = bbox_volume(&fast_world_bbox);
            let inflation = if local_volume > 1e-12 {
                fast_world_volume / local_volume
            } else {
                1.0
            };

            if inflation > inflation_threshold {
                let mut exact_mesh = local_mesh;
                exact_mesh.transform(&world_t);
                let exact_world_bbox = bbox_from_vertices(&exact_mesh.vertices)?;
                let exact_world_volume = bbox_volume(&exact_world_bbox);
                if let Some((wkt, obb_volume)) = oriented_bbox_wkt_xy(&exact_mesh.vertices) {
                    let final_inflation = if local_volume > 1e-12 {
                        obb_volume / local_volume
                    } else {
                        1.0
                    };
                    Some((
                        eid,
                        exact_world_bbox,
                        wkt,
                        inflation,
                        final_inflation,
                        true,
                        true,
                    ))
                } else {
                    let final_inflation = if local_volume > 1e-12 {
                        exact_world_volume / local_volume
                    } else {
                        1.0
                    };
                    Some((
                        eid,
                        exact_world_bbox,
                        bbox_wkt_polyhedral_surface_from_raw(&exact_world_bbox),
                        inflation,
                        final_inflation,
                        true,
                        false,
                    ))
                }
            } else {
                Some((
                    eid,
                    fast_world_bbox,
                    bbox_wkt_polyhedral_surface_from_raw(&fast_world_bbox),
                    inflation,
                    inflation,
                    false,
                    false,
                ))
            }
        })
        .collect();

    let mut out = HashMap::with_capacity(records.len());
    let mut wkts = HashMap::with_capacity(records.len());
    let mut sum_inflation_fast = 0.0_f64;
    let mut max_inflation_fast = 0.0_f64;
    let mut sum_inflation_final = 0.0_f64;
    let mut max_inflation_final = 0.0_f64;
    let mut escalated_exact_count = 0_usize;
    let mut escalated_reduction_sum = 0.0_f64;
    let mut count_fast_over_1_2 = 0_usize;
    let mut count_fast_over_1_5 = 0_usize;
    let mut count_fast_over_1_8 = 0_usize;
    let mut count_fast_over_2_0 = 0_usize;
    let mut outliers: Vec<BboxOutlier> = Vec::with_capacity(records.len());

    let mut rotated_bbox_count = 0_usize;
    for (eid, bbox, wkt, inflation_fast, inflation_final, escalated, used_rotated) in records {
        out.insert(eid, bbox);
        wkts.insert(eid, wkt);
        sum_inflation_fast += inflation_fast;
        max_inflation_fast = max_inflation_fast.max(inflation_fast);
        sum_inflation_final += inflation_final;
        max_inflation_final = max_inflation_final.max(inflation_final);
        if inflation_fast > 1.2 {
            count_fast_over_1_2 += 1;
        }
        if inflation_fast > 1.5 {
            count_fast_over_1_5 += 1;
        }
        if inflation_fast > 1.8 {
            count_fast_over_1_8 += 1;
        }
        if inflation_fast > 2.0 {
            count_fast_over_2_0 += 1;
        }
        if escalated {
            escalated_exact_count += 1;
            if inflation_fast > 1e-12 {
                escalated_reduction_sum += (inflation_fast - inflation_final) / inflation_fast;
            }
        }
        if used_rotated {
            rotated_bbox_count += 1;
        }
        outliers.push(BboxOutlier {
            entity_id: eid,
            inflation_fast,
            inflation_final,
            used_exact: escalated,
            used_rotated,
        });
    }

    outliers.sort_by(|a, b| b.inflation_fast.total_cmp(&a.inflation_fast));
    outliers.truncate(20);

    let elements_with_mesh = out.len();
    let avg_inflation_fast = if elements_with_mesh > 0 {
        sum_inflation_fast / elements_with_mesh as f64
    } else {
        0.0
    };
    let avg_inflation_final = if elements_with_mesh > 0 {
        sum_inflation_final / elements_with_mesh as f64
    } else {
        0.0
    };
    let avg_escalated_reduction_ratio = if escalated_exact_count > 0 {
        escalated_reduction_sum / escalated_exact_count as f64
    } else {
        0.0
    };
    (
        out,
        wkts,
        BboxQualityReport {
            elements_requested: element_ids.len(),
            elements_with_mesh,
            escalated_exact_count,
            rotated_bbox_count,
            avg_inflation_fast,
            max_inflation_fast,
            avg_inflation_final,
            max_inflation_final,
            avg_escalated_reduction_ratio,
            count_fast_over_1_2,
            count_fast_over_1_5,
            count_fast_over_1_8,
            count_fast_over_2_0,
            inflation_threshold,
            top_inflation_outliers: outliers,
        },
    )
}

pub(crate) fn bbox_from_vertices(vertices: &[f64]) -> Option<[f64; 6]> {
    if vertices.len() < 3 {
        return None;
    }
    let mut mn = [f64::MAX; 3];
    let mut mx = [f64::MIN; 3];
    let mut any = false;
    for chunk in vertices.chunks_exact(3) {
        any = true;
        for i in 0..3 {
            mn[i] = mn[i].min(chunk[i]);
            mx[i] = mx[i].max(chunk[i]);
        }
    }
    if !any {
        return None;
    }
    Some([mn[0], mn[1], mn[2], mx[0], mx[1], mx[2]])
}

pub(crate) fn bbox_volume(bbox: &[f64; 6]) -> f64 {
    let dx = (bbox[3] - bbox[0]).max(0.0);
    let dy = (bbox[4] - bbox[1]).max(0.0);
    let dz = (bbox[5] - bbox[2]).max(0.0);
    dx * dy * dz
}

pub(crate) fn transform_aabb(t: &transform::Transform4, bbox: &[f64; 6]) -> [f64; 6] {
    let [x0, y0, z0, x1, y1, z1] = *bbox;
    let corners = [
        [x0, y0, z0],
        [x1, y0, z0],
        [x0, y1, z0],
        [x1, y1, z0],
        [x0, y0, z1],
        [x1, y0, z1],
        [x0, y1, z1],
        [x1, y1, z1],
    ];
    let mut mn = [f64::MAX; 3];
    let mut mx = [f64::MIN; 3];
    for p in corners {
        let tp = t.transform_point(&p);
        for i in 0..3 {
            mn[i] = mn[i].min(tp[i]);
            mx[i] = mx[i].max(tp[i]);
        }
    }
    [mn[0], mn[1], mn[2], mx[0], mx[1], mx[2]]
}

pub(crate) fn bbox_wkt_polyhedral_surface_from_raw(bbox: &[f64; 6]) -> String {
    let [x0, y0, z0, x1, y1, z1] = *bbox;
    let x0 = fmt_num(x0);
    let y0 = fmt_num(y0);
    let z0 = fmt_num(z0);
    let x1 = fmt_num(x1);
    let y1 = fmt_num(y1);
    let z1 = fmt_num(z1);
    format!(
        "POLYHEDRALSURFACE Z ((({x0} {y0} {z0}, {x1} {y0} {z0}, {x1} {y1} {z0}, {x0} {y1} {z0}, {x0} {y0} {z0})), (({x0} {y0} {z1}, {x0} {y1} {z1}, {x1} {y1} {z1}, {x1} {y0} {z1}, {x0} {y0} {z1})), (({x0} {y0} {z0}, {x0} {y0} {z1}, {x1} {y0} {z1}, {x1} {y0} {z0}, {x0} {y0} {z0})), (({x1} {y0} {z0}, {x1} {y0} {z1}, {x1} {y1} {z1}, {x1} {y1} {z0}, {x1} {y0} {z0})), (({x1} {y1} {z0}, {x1} {y1} {z1}, {x0} {y1} {z1}, {x0} {y1} {z0}, {x1} {y1} {z0})), (({x0} {y1} {z0}, {x0} {y1} {z1}, {x0} {y0} {z1}, {x0} {y0} {z0}, {x0} {y1} {z0})))"
    )
}

pub(crate) fn oriented_bbox_wkt_xy(vertices: &[f64]) -> Option<(String, f64)> {
    if vertices.len() < 9 {
        return None;
    }
    let mut z_min = f64::MAX;
    let mut z_max = f64::MIN;
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(vertices.len() / 3);
    for p in vertices.chunks_exact(3) {
        pts.push((p[0], p[1]));
        z_min = z_min.min(p[2]);
        z_max = z_max.max(p[2]);
    }
    if pts.is_empty() {
        return None;
    }

    let n = pts.len() as f64;
    let (sum_x, sum_y) = pts
        .iter()
        .fold((0.0, 0.0), |(sx, sy), (x, y)| (sx + x, sy + y));
    let cx = sum_x / n;
    let cy = sum_y / n;

    let mut sxx = 0.0;
    let mut syy = 0.0;
    let mut sxy = 0.0;
    for (x, y) in &pts {
        let dx = *x - cx;
        let dy = *y - cy;
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    // Principal direction in XY plane (PCA for 2D cloud)
    let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let (ct, st) = (theta.cos(), theta.sin());
    let u = (ct, st);
    let v = (-st, ct);

    let mut u_min = f64::MAX;
    let mut u_max = f64::MIN;
    let mut v_min = f64::MAX;
    let mut v_max = f64::MIN;
    for (x, y) in &pts {
        let dx = *x - cx;
        let dy = *y - cy;
        let pu = dx * u.0 + dy * u.1;
        let pv = dx * v.0 + dy * v.1;
        u_min = u_min.min(pu);
        u_max = u_max.max(pu);
        v_min = v_min.min(pv);
        v_max = v_max.max(pv);
    }
    let du = (u_max - u_min).max(0.0);
    let dv = (v_max - v_min).max(0.0);
    let dz = (z_max - z_min).max(0.0);
    if du <= f64::EPSILON || dv <= f64::EPSILON || dz <= f64::EPSILON {
        return None;
    }

    let corner_uv = [
        (u_min, v_min),
        (u_max, v_min),
        (u_max, v_max),
        (u_min, v_max),
    ];
    let mut cxy = [(0.0, 0.0); 4];
    for (i, (cu, cv)) in corner_uv.iter().enumerate() {
        cxy[i] = (cx + cu * u.0 + cv * v.0, cy + cu * u.1 + cv * v.1);
    }
    let (x0, y0) = cxy[0];
    let (x1, y1) = cxy[1];
    let (x2, y2) = cxy[2];
    let (x3, y3) = cxy[3];
    let z0 = fmt_num(z_min);
    let z1 = fmt_num(z_max);
    let wkt = format!(
        "POLYHEDRALSURFACE Z ((({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})), (({} {} {}, {} {} {}, {} {} {}, {} {} {}, {} {} {})))",
        fmt_num(x0), fmt_num(y0), z0, fmt_num(x1), fmt_num(y1), z0, fmt_num(x2), fmt_num(y2), z0, fmt_num(x3), fmt_num(y3), z0, fmt_num(x0), fmt_num(y0), z0,
        fmt_num(x0), fmt_num(y0), z1, fmt_num(x3), fmt_num(y3), z1, fmt_num(x2), fmt_num(y2), z1, fmt_num(x1), fmt_num(y1), z1, fmt_num(x0), fmt_num(y0), z1,
        fmt_num(x0), fmt_num(y0), z0, fmt_num(x0), fmt_num(y0), z1, fmt_num(x1), fmt_num(y1), z1, fmt_num(x1), fmt_num(y1), z0, fmt_num(x0), fmt_num(y0), z0,
        fmt_num(x1), fmt_num(y1), z0, fmt_num(x1), fmt_num(y1), z1, fmt_num(x2), fmt_num(y2), z1, fmt_num(x2), fmt_num(y2), z0, fmt_num(x1), fmt_num(y1), z0,
        fmt_num(x2), fmt_num(y2), z0, fmt_num(x2), fmt_num(y2), z1, fmt_num(x3), fmt_num(y3), z1, fmt_num(x3), fmt_num(y3), z0, fmt_num(x2), fmt_num(y2), z0,
        fmt_num(x3), fmt_num(y3), z0, fmt_num(x3), fmt_num(y3), z1, fmt_num(x0), fmt_num(y0), z1, fmt_num(x0), fmt_num(y0), z0, fmt_num(x3), fmt_num(y3), z0
    );
    Some((wkt, du * dv * dz))
}

pub(crate) fn fmt_num(v: f64) -> String {
    let mut s = format!("{v:.9}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

/// Write element bounding boxes as GeoSPARQL WKT triples to a Turtle file.
///
/// Each element gets:
///   <element_iri> geo:hasGeometry <element_iri_geom> .
///   <element_iri_geom> a geo:Geometry ;
///       geo:asWKT "POLYGON Z ((...bottom face...))"^^geo:wktLiteral ;
///       geo:dimension 3 .
///
/// The WKT is a 3D polyhedron (6 faces of the bounding box) encoded as
/// POLYHEDRALSURFACE Z for maximum compatibility. The footprint POLYGON Z
/// (bottom face) plus a separate LINESTRING Z marking the height extent
/// is also included for 2D-capable tools.
pub(crate) fn arc_bounding_boxes_from_raw(
    raw: HashMap<EntityId, [f64; 6]>,
) -> Arc<HashMap<EntityId, BoundingBox>> {
    let mapped = raw
        .into_iter()
        .map(|(entity_id, [x_min, y_min, z_min, x_max, y_max, z_max])| {
            (
                entity_id,
                BoundingBox {
                    x_min,
                    x_max,
                    y_min,
                    y_max,
                    z_min,
                    z_max,
                },
            )
        })
        .collect();
    Arc::new(mapped)
}

pub(crate) fn resolve_ifcowl_path(output_file: Option<&Path>, input_file: &Path) -> PathBuf {
    if let Some(path) = output_file {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("lbd_output");
        return parent.join(format!("{stem}_ifcowl.ttl"));
    }

    let stem = input_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("ifc_output");
    PathBuf::from(format!("{stem}_ifcowl.ttl"))
}
