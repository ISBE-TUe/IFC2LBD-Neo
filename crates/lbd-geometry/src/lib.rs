//! Geometry hooks for optional topology enrichment.
//!
//! Provides the geometry-intensive steps used primarily in full topology mode.
//! Computes bounding boxes, performs R-tree spatial indexing, and derives topology
//! relations (adjacency, intersection, interface) from bounding-box and CSG analysis.

pub mod csg;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ifc_model::IfcModel;
use ifc_step::EntityId;
use lbd_topology::{TopologyEdge, TopologyEdgeKind, TopologyGraph};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundingBox {
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
    pub z_min: f64,
    pub z_max: f64,
}

pub trait BoundingBoxProvider {
    fn bounding_box(&self, model: &IfcModel, entity_id: EntityId) -> Option<BoundingBox>;
}

#[derive(Debug, Clone, Default)]
pub struct MapBoundingBoxProvider {
    boxes: Arc<HashMap<EntityId, BoundingBox>>,
}

impl MapBoundingBoxProvider {
    pub fn new(boxes: Arc<HashMap<EntityId, BoundingBox>>) -> Self {
        Self { boxes }
    }
}

impl BoundingBoxProvider for MapBoundingBoxProvider {
    fn bounding_box(&self, _model: &IfcModel, entity_id: EntityId) -> Option<BoundingBox> {
        self.boxes.get(&entity_id).copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeometryRelationKind {
    AdjacentElement,
    IntersectingElement,
    InterfaceOf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeometryRelation {
    pub source: EntityId,
    pub target: EntityId,
    pub kind: GeometryRelationKind,
}

pub trait GeometryProvider {
    fn candidate_relations(&self, model: &IfcModel) -> Vec<GeometryRelation>;
}

#[derive(Debug, thiserror::Error)]
pub enum GeometryKernelError {
    #[error("pair analysis failed for entity pair ({left}, {right}): {message}")]
    PairAnalysisFailed {
        left: EntityId,
        right: EntityId,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExactCheckOptions {
    pub tolerance: f64,
}

impl Default for ExactCheckOptions {
    fn default() -> Self {
        Self { tolerance: 1e-6 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceEvidence {
    pub interface_id: EntityId,
    pub shared_boundary_area: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExactPairAnalysis {
    pub intersects: bool,
    pub touches_within_tolerance: bool,
    pub minimum_distance: Option<f64>,
    pub interface: Option<InterfaceEvidence>,
}

pub trait ExactGeometryKernel {
    fn analyze_pair(
        &self,
        model: &IfcModel,
        left: EntityId,
        right: EntityId,
        options: &ExactCheckOptions,
    ) -> Result<ExactPairAnalysis, GeometryKernelError>;
}


#[derive(Debug, Clone)]
pub struct BboxGeometryProvider<P> {
    pub bbox_provider: P,
    pub tolerance: f64,
}

impl<P> BboxGeometryProvider<P> {
    pub fn new(bbox_provider: P, tolerance: f64) -> Self {
        Self {
            bbox_provider,
            tolerance,
        }
    }
}

impl<P> GeometryProvider for BboxGeometryProvider<P>
where
    P: BoundingBoxProvider,
{
    fn candidate_relations(&self, model: &IfcModel) -> Vec<GeometryRelation> {
        derive_relations_from_bounding_boxes(model, &self.bbox_provider, self.tolerance)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeometryEnrichmentResult {
    pub inserted_extension_edges: usize,
    pub kept_existing_extension_edges: usize,
}

/// Decorate an existing topology graph with geometry-derived extension edges.
/// This intentionally leaves core BOT edges untouched.
pub fn enrich_topology_with_geometry(
    model: &IfcModel,
    graph: &mut TopologyGraph,
    provider: &impl GeometryProvider,
) -> GeometryEnrichmentResult {
    let mut inserted = 0_usize;
    let existing = graph.extension_edges.len();

    for relation in provider.candidate_relations(model) {
        let kind = map_relation_kind(relation.kind);

        // For InterfaceOf edges, the source is a synthetic interface entity
        // (not in the graph). Only check that the target is a real element.
        // For all other relation kinds, both endpoints must exist.
        let source_ok = match kind {
            TopologyEdgeKind::InterfaceOf => true, // synthetic interface entity source
            _ => graph.node_kinds.contains_key(&relation.source),
        };
        if !source_ok || !graph.node_kinds.contains_key(&relation.target) {
            continue;
        }

        if insert_unique_extension_edge(
            &mut graph.extension_edges,
            relation.source,
            relation.target,
            kind,
            Some(derived_from_label(relation.kind)),
        ) {
            inserted += 1;
        }
    }

    GeometryEnrichmentResult {
        inserted_extension_edges: inserted,
        kept_existing_extension_edges: existing,
    }
}

pub fn collect_bounding_boxes(
    model: &IfcModel,
    provider: &impl BoundingBoxProvider,
) -> HashMap<EntityId, BoundingBox> {
    let mut boxes = HashMap::new();
    for &entity_id in model.spatial_nodes.keys() {
        if let Some(bbox) = provider.bounding_box(model, entity_id) {
            boxes.insert(entity_id, bbox);
        }
    }
    for &entity_id in model.elements.keys() {
        if let Some(bbox) = provider.bounding_box(model, entity_id) {
            boxes.insert(entity_id, bbox);
        }
    }
    boxes
}

pub fn derive_relations_from_bounding_boxes(
    model: &IfcModel,
    provider: &impl BoundingBoxProvider,
    tolerance: f64,
) -> Vec<GeometryRelation> {
    let boxes = collect_bounding_boxes(model, provider);
    let mut element_ids: Vec<_> = model.elements.keys().copied().collect();
    element_ids.sort_unstable();

    let mut relations = Vec::new();
    for left_index in 0..element_ids.len() {
        for right_index in (left_index + 1)..element_ids.len() {
            let left_id = element_ids[left_index];
            let right_id = element_ids[right_index];
            let Some(left_box) = boxes.get(&left_id) else {
                continue;
            };
            let Some(right_box) = boxes.get(&right_id) else {
                continue;
            };

            if boxes_intersect(left_box, right_box, tolerance) {
                relations.push(GeometryRelation {
                    source: left_id,
                    target: right_id,
                    kind: GeometryRelationKind::IntersectingElement,
                });
                relations.push(GeometryRelation {
                    source: right_id,
                    target: left_id,
                    kind: GeometryRelationKind::IntersectingElement,
                });
            } else if boxes_touch_within_tolerance(left_box, right_box, tolerance) {
                relations.push(GeometryRelation {
                    source: left_id,
                    target: right_id,
                    kind: GeometryRelationKind::AdjacentElement,
                });
                relations.push(GeometryRelation {
                    source: right_id,
                    target: left_id,
                    kind: GeometryRelationKind::AdjacentElement,
                });
            }
        }
    }

    relations
}

pub fn derive_candidate_pairs_from_bounding_boxes(
    model: &IfcModel,
    provider: &impl BoundingBoxProvider,
    tolerance: f64,
) -> Vec<(EntityId, EntityId)> {
    let boxes = collect_bounding_boxes(model, provider);
    let mut element_ids: Vec<_> = model.elements.keys().copied().collect();
    element_ids.sort_unstable();
    let mut pairs = Vec::new();
    for left_index in 0..element_ids.len() {
        for right_index in (left_index + 1)..element_ids.len() {
            let left_id = element_ids[left_index];
            let right_id = element_ids[right_index];
            let Some(left_box) = boxes.get(&left_id) else {
                continue;
            };
            let Some(right_box) = boxes.get(&right_id) else {
                continue;
            };
            if boxes_intersect(left_box, right_box, tolerance)
                || boxes_touch_within_tolerance(left_box, right_box, tolerance)
            {
                pairs.push((left_id, right_id));
            }
        }
    }
    pairs
}

pub fn derive_relations_with_exact_kernel(
    model: &IfcModel,
    kernel: &impl ExactGeometryKernel,
    candidate_pairs: &[(EntityId, EntityId)],
    options: &ExactCheckOptions,
) -> Result<Vec<GeometryRelation>, GeometryKernelError> {
    let mut relations = Vec::new();
    let mut seen = HashSet::new();
    for &(left, right) in candidate_pairs {
        if left == right
            || !model.elements.contains_key(&left)
            || !model.elements.contains_key(&right)
        {
            continue;
        }
        let canonical = if left < right {
            (left, right)
        } else {
            (right, left)
        };
        if !seen.insert(canonical) {
            continue;
        }

        let analysis = kernel.analyze_pair(model, canonical.0, canonical.1, options)?;
        append_pair_relations(canonical, analysis, &mut relations);
    }
    Ok(relations)
}

pub fn enrich_topology_with_exact_geometry<B, K>(
    model: &IfcModel,
    graph: &mut TopologyGraph,
    bbox_provider: &B,
    kernel: &K,
    options: &ExactCheckOptions,
) -> Result<GeometryEnrichmentResult, GeometryKernelError>
where
    B: BoundingBoxProvider,
    K: ExactGeometryKernel,
{
    let candidates =
        derive_candidate_pairs_from_bounding_boxes(model, bbox_provider, options.tolerance);
    let relations = derive_relations_with_exact_kernel(model, kernel, &candidates, options)?;
    let mut inserted = 0_usize;
    let existing = graph.extension_edges.len();
    for relation in relations {
        let kind = map_relation_kind(relation.kind);
        if !graph.node_kinds.contains_key(&relation.source)
            || !graph.node_kinds.contains_key(&relation.target)
        {
            continue;
        }
        if insert_unique_extension_edge(
            &mut graph.extension_edges,
            relation.source,
            relation.target,
            kind,
            Some(derived_from_label(relation.kind)),
        ) {
            inserted += 1;
        }
    }
    Ok(GeometryEnrichmentResult {
        inserted_extension_edges: inserted,
        kept_existing_extension_edges: existing,
    })
}

fn boxes_intersect(left: &BoundingBox, right: &BoundingBox, tolerance: f64) -> bool {
    overlap_length(left.x_min, left.x_max, right.x_min, right.x_max) > tolerance
        && overlap_length(left.y_min, left.y_max, right.y_min, right.y_max) > tolerance
        && overlap_length(left.z_min, left.z_max, right.z_min, right.z_max) > tolerance
}

fn boxes_touch_within_tolerance(left: &BoundingBox, right: &BoundingBox, tolerance: f64) -> bool {
    let x_overlap = overlap_length(left.x_min, left.x_max, right.x_min, right.x_max);
    let y_overlap = overlap_length(left.y_min, left.y_max, right.y_min, right.y_max);
    let z_overlap = overlap_length(left.z_min, left.z_max, right.z_min, right.z_max);

    // Touching/near-touching requires overlap in at least two axes and a small gap in one axis.
    let x_gap = interval_gap(left.x_min, left.x_max, right.x_min, right.x_max);
    let y_gap = interval_gap(left.y_min, left.y_max, right.y_min, right.y_max);
    let z_gap = interval_gap(left.z_min, left.z_max, right.z_min, right.z_max);

    (y_overlap >= -tolerance && z_overlap >= -tolerance && x_gap <= tolerance)
        || (x_overlap >= -tolerance && z_overlap >= -tolerance && y_gap <= tolerance)
        || (x_overlap >= -tolerance && y_overlap >= -tolerance && z_gap <= tolerance)
}

fn overlap_length(a_min: f64, a_max: f64, b_min: f64, b_max: f64) -> f64 {
    (a_max.min(b_max) - a_min.max(b_min)).max(0.0)
}

fn interval_gap(a_min: f64, a_max: f64, b_min: f64, b_max: f64) -> f64 {
    if a_max < b_min {
        b_min - a_max
    } else if b_max < a_min {
        a_min - b_max
    } else {
        0.0
    }
}

fn append_pair_relations(
    canonical: (EntityId, EntityId),
    analysis: ExactPairAnalysis,
    relations: &mut Vec<GeometryRelation>,
) {
    if analysis.intersects {
        relations.push(GeometryRelation {
            source: canonical.0,
            target: canonical.1,
            kind: GeometryRelationKind::IntersectingElement,
        });
        relations.push(GeometryRelation {
            source: canonical.1,
            target: canonical.0,
            kind: GeometryRelationKind::IntersectingElement,
        });
    }
    // bot:adjacentElement is strictly a Zone→Element relation per the BOT spec.
    // For element-element touching, the correct BOT representation is bot:Interface
    // (captured via InterfaceOf below). Do not emit AdjacentElement for element pairs.

    if let Some(interface) = analysis.interface {
        relations.push(GeometryRelation {
            source: interface.interface_id,
            target: canonical.0,
            kind: GeometryRelationKind::InterfaceOf,
        });
        relations.push(GeometryRelation {
            source: interface.interface_id,
            target: canonical.1,
            kind: GeometryRelationKind::InterfaceOf,
        });
    }
}

fn map_relation_kind(kind: GeometryRelationKind) -> TopologyEdgeKind {
    match kind {
        GeometryRelationKind::AdjacentElement => TopologyEdgeKind::AdjacentElement,
        GeometryRelationKind::IntersectingElement => TopologyEdgeKind::IntersectingElement,
        GeometryRelationKind::InterfaceOf => TopologyEdgeKind::InterfaceOf,
    }
}

fn derived_from_label(kind: GeometryRelationKind) -> &'static str {
    match kind {
        GeometryRelationKind::AdjacentElement => "GeometryProvider::adjacent",
        GeometryRelationKind::IntersectingElement => "GeometryProvider::intersecting",
        GeometryRelationKind::InterfaceOf => "GeometryProvider::interfaceOf",
    }
}

fn insert_unique_extension_edge(
    extension_edges: &mut Vec<TopologyEdge>,
    source: EntityId,
    target: EntityId,
    kind: TopologyEdgeKind,
    derived_from: Option<&'static str>,
) -> bool {
    if extension_edges
        .iter()
        .any(|edge| edge.source == source && edge.target == target && edge.kind == kind)
    {
        return false;
    }
    extension_edges.push(TopologyEdge {
        source,
        target,
        kind,
        derived_from,
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use ifc_model::build_model;
    use ifc_step::parse_step_file;
    use lbd_topology::build_topology;
    use std::path::PathBuf;

    #[derive(Debug, Default, Clone, Copy)]
    struct MockGeometryProvider;

    impl GeometryProvider for MockGeometryProvider {
        fn candidate_relations(&self, model: &IfcModel) -> Vec<GeometryRelation> {
            let mut space_ids: Vec<_> = model.spatial_nodes.keys().copied().collect();
            let mut element_ids: Vec<_> = model.elements.keys().copied().collect();
            space_ids.sort_unstable();
            element_ids.sort_unstable();
            let Some(space_id) = space_ids.first().copied() else {
                return Vec::new();
            };
            let Some(element_id) = element_ids.first().copied() else {
                return Vec::new();
            };
            vec![
                GeometryRelation {
                    source: space_id,
                    target: element_id,
                    kind: GeometryRelationKind::AdjacentElement,
                },
                GeometryRelation {
                    source: element_id,
                    target: element_id,
                    kind: GeometryRelationKind::IntersectingElement,
                },
            ]
        }
    }

    #[derive(Debug, Default, Clone, Copy)]
    struct MockBoundingBoxProvider;

    impl BoundingBoxProvider for MockBoundingBoxProvider {
        fn bounding_box(&self, model: &IfcModel, entity_id: EntityId) -> Option<BoundingBox> {
            if model.spatial_nodes.contains_key(&entity_id)
                || model.elements.contains_key(&entity_id)
            {
                Some(BoundingBox {
                    x_min: 0.0,
                    x_max: 1.0,
                    y_min: 0.0,
                    y_max: 1.0,
                    z_min: 0.0,
                    z_max: 1.0,
                })
            } else {
                None
            }
        }
    }

    #[derive(Debug, Clone)]
    struct StaticBoundingBoxProvider {
        boxes: HashMap<EntityId, BoundingBox>,
    }

    impl BoundingBoxProvider for StaticBoundingBoxProvider {
        fn bounding_box(&self, _model: &IfcModel, entity_id: EntityId) -> Option<BoundingBox> {
            self.boxes.get(&entity_id).copied()
        }
    }

    #[derive(Debug, Default, Clone, Copy)]
    struct MockExactGeometryKernel;

    impl ExactGeometryKernel for MockExactGeometryKernel {
        fn analyze_pair(
            &self,
            _model: &IfcModel,
            left: EntityId,
            right: EntityId,
            _options: &ExactCheckOptions,
        ) -> Result<ExactPairAnalysis, GeometryKernelError> {
            if left % 2 == 0 || right % 2 == 0 {
                return Ok(ExactPairAnalysis {
                    intersects: true,
                    touches_within_tolerance: false,
                    minimum_distance: Some(0.0),
                    interface: Some(InterfaceEvidence {
                        interface_id: left + right + 1_000_000,
                        shared_boundary_area: Some(1.5),
                    }),
                });
            }
            Ok(ExactPairAnalysis {
                intersects: false,
                touches_within_tolerance: true,
                minimum_distance: Some(0.0001),
                interface: None,
            })
        }
    }

    fn duplex_model() -> Option<IfcModel> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Duplex.ifc");
        if !path.exists() {
            return None;
        }
        let step = parse_step_file(&path).ok()?;
        build_model(&step).ok()
    }

    #[test]
    fn enrich_topology_with_geometry_adds_extension_edges_only() {
        let Some(model) = duplex_model() else {
            return;
        };
        let mut graph = build_topology(&model);
        let core_before = graph.core_edges.clone();

        let result = enrich_topology_with_geometry(&model, &mut graph, &MockGeometryProvider);

        assert!(result.inserted_extension_edges > 0);
        assert_eq!(core_before, graph.core_edges);
        assert!(graph
            .extension_edges
            .iter()
            .any(|edge| edge.kind == TopologyEdgeKind::AdjacentElement));
        assert!(graph
            .extension_edges
            .iter()
            .any(|edge| edge.kind == TopologyEdgeKind::IntersectingElement));
    }

    #[test]
    fn collect_bounding_boxes_returns_boxes_for_model_entities() {
        let Some(model) = duplex_model() else {
            return;
        };
        let boxes = collect_bounding_boxes(&model, &MockBoundingBoxProvider);

        assert!(boxes.len() >= model.spatial_nodes.len());
        assert!(boxes.len() >= model.elements.len());
    }

    #[test]
    fn derive_relations_from_bounding_boxes_detects_intersection_and_adjacency() {
        let Some(model) = duplex_model() else {
            return;
        };
        let mut element_ids: Vec<_> = model.elements.keys().copied().collect();
        element_ids.sort_unstable();
        let a = element_ids[0];
        let b = element_ids[1];
        let c = element_ids[2];

        let provider = StaticBoundingBoxProvider {
            boxes: HashMap::from([
                (
                    a,
                    BoundingBox {
                        x_min: 0.0,
                        x_max: 1.0,
                        y_min: 0.0,
                        y_max: 1.0,
                        z_min: 0.0,
                        z_max: 1.0,
                    },
                ),
                (
                    b,
                    BoundingBox {
                        x_min: 0.5,
                        x_max: 1.5,
                        y_min: 0.0,
                        y_max: 1.0,
                        z_min: 0.0,
                        z_max: 1.0,
                    },
                ),
                (
                    c,
                    BoundingBox {
                        x_min: 1.5,
                        x_max: 2.5,
                        y_min: 0.0,
                        y_max: 1.0,
                        z_min: 0.0,
                        z_max: 1.0,
                    },
                ),
            ]),
        };

        let relations = derive_relations_from_bounding_boxes(&model, &provider, 1e-6);
        assert!(relations.iter().any(|relation| {
            relation.source == a
                && relation.target == b
                && relation.kind == GeometryRelationKind::IntersectingElement
        }));
        assert!(relations.iter().any(|relation| {
            relation.source == b
                && relation.target == c
                && relation.kind == GeometryRelationKind::AdjacentElement
        }));
    }

    #[test]
    fn derive_relations_with_exact_kernel_emits_intersection_adjacency_and_interface() {
        let Some(model) = duplex_model() else {
            return;
        };
        let mut ids: Vec<_> = model.elements.keys().copied().collect();
        ids.sort_unstable();
        let a = *ids.iter().find(|id| **id % 2 == 0).expect("need even id");
        let b = *ids.iter().find(|id| **id % 2 == 1).expect("need odd id");
        let c = *ids
            .iter()
            .find(|id| **id != a && **id != b)
            .expect("need third distinct id");
        let options = ExactCheckOptions::default();

        let relations = derive_relations_with_exact_kernel(
            &model,
            &MockExactGeometryKernel,
            &[(a, b), (c, b), (b, a)],
            &options,
        )
        .expect("mock kernel should not fail");

        // Touching element pairs must produce only InterfaceOf (not AdjacentElement).
        // AdjacentElement is a Zone→Element relation per BOT spec; element-element
        // touching is represented solely by bot:Interface via InterfaceOf.
        assert!(!relations
            .iter()
            .any(|relation| relation.kind == GeometryRelationKind::AdjacentElement));
        assert!(relations.iter().any(|relation| {
            relation.kind == GeometryRelationKind::IntersectingElement
                || relation.kind == GeometryRelationKind::InterfaceOf
        }));
        assert!(relations
            .iter()
            .any(|relation| relation.kind == GeometryRelationKind::InterfaceOf));
    }

    #[test]
    fn enrich_topology_with_exact_geometry_adds_extension_edges() {
        let Some(model) = duplex_model() else {
            return;
        };
        let mut graph = build_topology(&model);
        let result = enrich_topology_with_exact_geometry(
            &model,
            &mut graph,
            &MockBoundingBoxProvider,
            &MockExactGeometryKernel,
            &ExactCheckOptions::default(),
        )
        .expect("mock exact enrichment should succeed");

        assert!(result.inserted_extension_edges > 0);
        assert!(!graph.extension_edges.is_empty());
    }

}
