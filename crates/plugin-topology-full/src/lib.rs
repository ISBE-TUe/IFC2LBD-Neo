use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_geometry::csg::{
    prepare_containment_mesh, prepare_triangle_mesh, prepared_mesh_contains_mesh,
    prepared_meshes_analyze, ContactType, PreparedContainmentMesh, PreparedTriangleMesh,
    TriangleMesh,
};
use lbd_geometry::ExactCheckOptions;
use lbd_geometry::GeometryRelation;
use lbd_topology::{
    build_topology, TopologyEdge, TopologyEdgeKind, TopologyGraph, TopologyNodeKind,
};
use rayon::prelude::*;
use rstar::{RTree, RTreeObject, AABB};
use tessellated_model::TessellatedModel;

#[derive(Debug, Default)]
struct TopologyCandidateSet {
    indexed_meshes: usize,
    pairs: Vec<(u64, u64)>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GeometryEnrichmentStats {
    pub semantic_edges: usize,
    pub indexed_meshes: usize,
    pub candidate_pairs: usize,
    pub rayon_threads: usize,
    pub prepared_mesh_builds: usize,
    pub separated: usize,
    pub touching: usize,
    pub intersecting: usize,
    pub contained: usize,
    pub propagated_from_subelements: usize,
    pub geometry_edges: usize,
    pub candidate_seconds: f64,
    pub elapsed_seconds: f64,
    pub cache_mode: &'static str,
}

struct IndexedEnvelope {
    id: u64,
    envelope: AABB<[f64; 3]>,
}

struct PreparedFlatMesh {
    collision: PreparedTriangleMesh,
    containment: Option<PreparedContainmentMesh>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PairOutcome {
    Separated,
    Touching,
    Intersecting,
    Contains { zone: u64, element: u64 },
}

impl RTreeObject for IndexedEnvelope {
    type Envelope = AABB<[f64; 3]>;

    fn envelope(&self) -> Self::Envelope {
        self.envelope
    }
}

fn build_candidate_set(
    graph: &TopologyGraph,
    model: &TessellatedModel,
    tolerance: f64,
) -> TopologyCandidateSet {
    let meshes: Vec<_> = model
        .meshes
        .iter()
        .filter_map(|flat| {
            let envelope = world_envelope(flat)?;
            Some(IndexedEnvelope {
                id: flat.express_id,
                envelope,
            })
        })
        .filter(|entry| graph.node_kinds.contains_key(&entry.id))
        .collect();
    let tree = RTree::bulk_load(meshes);
    let mut seen = HashSet::new();
    let mut pairs = Vec::new();
    for left in tree.iter() {
        let lower = left.envelope.lower();
        let upper = left.envelope.upper();
        let query = AABB::from_corners(
            [
                lower[0] - tolerance,
                lower[1] - tolerance,
                lower[2] - tolerance,
            ],
            [
                upper[0] + tolerance,
                upper[1] + tolerance,
                upper[2] + tolerance,
            ],
        );
        for right in tree.locate_in_envelope_intersecting(&query) {
            let pair = if left.id < right.id {
                (left.id, right.id)
            } else {
                (right.id, left.id)
            };
            if pair.0 == pair.1 || !seen.insert(pair) {
                continue;
            }
            pairs.push(pair);
        }
    }
    pairs.sort_unstable();
    TopologyCandidateSet {
        indexed_meshes: tree.size(),
        pairs,
    }
}

const FULL_CACHE_MAX_MESHES: usize = 4_096;
const BOUNDED_CACHE_PAIR_BATCH: usize = 128;

/// Build semantic and geometry-derived BOT topology inside the BOT producer.
pub fn build_extended_topology_parallel(
    model: &IfcModel,
    tessellated: &TessellatedModel,
    tolerance: f64,
) -> (TopologyGraph, GeometryEnrichmentStats) {
    let mut graph = build_topology(model);
    let semantic_edges = graph.core_edges.len();
    let candidate_started = Instant::now();
    let candidates = build_candidate_set(&graph, tessellated, tolerance);
    let candidate_seconds = candidate_started.elapsed().as_secs_f64();
    let mut stats = enrich_topology_parallel(&mut graph, tessellated, &candidates, tolerance);
    stats.semantic_edges = semantic_edges;
    stats.candidate_seconds = candidate_seconds;
    stats.rayon_threads = rayon::current_num_threads().max(1);
    tracing::info!(
        "BOT extended topology: {} meshes, {} RStar candidates, {} Parry checks on {} threads, cache={}, builds={}, touching={}, intersecting={}, contained={}, propagated={}, separated={}, geometry_edges={}, candidates={:.3}s exact={:.3}s",
        stats.indexed_meshes,
        stats.candidate_pairs,
        stats.candidate_pairs,
        stats.rayon_threads,
        stats.cache_mode,
        stats.prepared_mesh_builds,
        stats.touching,
        stats.intersecting,
        stats.contained,
        stats.propagated_from_subelements,
        stats.separated,
        stats.geometry_edges,
        stats.candidate_seconds,
        stats.elapsed_seconds,
    );
    (graph, stats)
}

/// Run exact geometry checks on the producer-side Rayon pool.
///
/// Small models prepare every Parry mesh once. Large models prepare only the
/// meshes referenced by a bounded pair batch, trading some repeated builds for
/// a predictable memory ceiling per active Rayon worker.
fn enrich_topology_parallel(
    graph: &mut TopologyGraph,
    model: &TessellatedModel,
    candidates: &TopologyCandidateSet,
    tolerance: f64,
) -> GeometryEnrichmentStats {
    let started = Instant::now();
    let flat_by_id: HashMap<u64, &tessellated_model::FlatMesh> = model
        .meshes
        .iter()
        .map(|flat| (flat.express_id, flat))
        .collect();
    let options = ExactCheckOptions { tolerance };

    let (verdicts, prepared_mesh_builds, cache_mode) = if candidates.indexed_meshes
        <= FULL_CACHE_MAX_MESHES
    {
        let prepared: HashMap<u64, PreparedFlatMesh> = flat_by_id
            .par_iter()
            .filter_map(|(&id, flat)| {
                prepare_flat_mesh(flat, graph.node_kinds.get(&id).copied()).map(|mesh| (id, mesh))
            })
            .collect();
        let verdicts: Vec<((u64, u64), PairOutcome)> = candidates
            .pairs
            .par_iter()
            .map(|&(left, right)| {
                let outcome = match (prepared.get(&left), prepared.get(&right)) {
                    (Some(left_mesh), Some(right_mesh)) => {
                        analyze_pair(left, right, graph, left_mesh, right_mesh, &options)
                    }
                    _ => PairOutcome::Separated,
                };
                ((left, right), outcome)
            })
            .collect();
        (verdicts, prepared.len(), "full")
    } else {
        let chunks: Vec<(Vec<((u64, u64), PairOutcome)>, usize)> = candidates
            .pairs
            .par_chunks(BOUNDED_CACHE_PAIR_BATCH)
            .map(|pair_chunk| {
                let ids: HashSet<u64> = pair_chunk
                    .iter()
                    .flat_map(|&(left, right)| [left, right])
                    .collect();
                let prepared: HashMap<u64, PreparedFlatMesh> = ids
                    .into_iter()
                    .filter_map(|id| {
                        let flat = flat_by_id.get(&id)?;
                        prepare_flat_mesh(flat, graph.node_kinds.get(&id).copied())
                            .map(|mesh| (id, mesh))
                    })
                    .collect();
                let verdicts = pair_chunk
                    .iter()
                    .map(|&(left, right)| {
                        let outcome = match (prepared.get(&left), prepared.get(&right)) {
                            (Some(left_mesh), Some(right_mesh)) => {
                                analyze_pair(left, right, graph, left_mesh, right_mesh, &options)
                            }
                            _ => PairOutcome::Separated,
                        };
                        ((left, right), outcome)
                    })
                    .collect();
                (verdicts, prepared.len())
            })
            .collect();
        let prepared_mesh_builds = chunks.iter().map(|(_, builds)| *builds).sum();
        let verdicts = chunks
            .into_iter()
            .flat_map(|(verdicts, _)| verdicts)
            .collect();
        (verdicts, prepared_mesh_builds, "bounded")
    };

    let semantic_edge_count = graph.core_edges.len();
    let mut seen_edges: HashSet<_> = graph
        .core_edges
        .iter()
        .map(|edge| (edge.source, edge.target, edge.kind))
        .collect();
    let mut stats = GeometryEnrichmentStats {
        indexed_meshes: candidates.indexed_meshes,
        candidate_pairs: candidates.pairs.len(),
        prepared_mesh_builds,
        cache_mode,
        ..GeometryEnrichmentStats::default()
    };
    for (pair, outcome) in verdicts {
        match outcome {
            PairOutcome::Separated => stats.separated += 1,
            PairOutcome::Touching => stats.touching += 1,
            PairOutcome::Intersecting => stats.intersecting += 1,
            PairOutcome::Contains { .. } => stats.contained += 1,
        }
        add_exact_relation_with_seen(graph, &mut seen_edges, pair, outcome);
    }
    stats.propagated_from_subelements = propagate_subelement_incidence(graph, &mut seen_edges);
    graph
        .core_edges
        .sort_by_key(|edge| (edge.source, edge.target, edge_rank(edge.kind)));
    stats.geometry_edges = graph.core_edges.len().saturating_sub(semantic_edge_count);
    stats.elapsed_seconds = started.elapsed().as_secs_f64();
    stats
}

fn prepare_flat_mesh(
    flat: &tessellated_model::FlatMesh,
    kind: Option<TopologyNodeKind>,
) -> Option<PreparedFlatMesh> {
    let world = world_mesh(flat)?;
    let collision = prepare_triangle_mesh(&world)?;
    let containment = matches!(kind, Some(TopologyNodeKind::Space | TopologyNodeKind::Zone))
        .then(|| prepare_containment_mesh(&world))
        .flatten();
    Some(PreparedFlatMesh {
        collision,
        containment,
    })
}

fn analyze_pair(
    left_id: u64,
    right_id: u64,
    graph: &TopologyGraph,
    left: &PreparedFlatMesh,
    right: &PreparedFlatMesh,
    options: &ExactCheckOptions,
) -> PairOutcome {
    match prepared_meshes_analyze(&left.collision, &right.collision, options) {
        ContactType::Touching => PairOutcome::Touching,
        ContactType::Intersecting => PairOutcome::Intersecting,
        ContactType::Separated => {
            match (
                graph.node_kinds.get(&left_id),
                graph.node_kinds.get(&right_id),
            ) {
                (
                    Some(TopologyNodeKind::Space | TopologyNodeKind::Zone),
                    Some(TopologyNodeKind::Element),
                ) if left.containment.as_ref().is_some_and(|container| {
                    prepared_mesh_contains_mesh(container, &right.collision, options.tolerance)
                }) =>
                {
                    PairOutcome::Contains {
                        zone: left_id,
                        element: right_id,
                    }
                }
                (
                    Some(TopologyNodeKind::Element),
                    Some(TopologyNodeKind::Space | TopologyNodeKind::Zone),
                ) if right.containment.as_ref().is_some_and(|container| {
                    prepared_mesh_contains_mesh(container, &left.collision, options.tolerance)
                }) =>
                {
                    PairOutcome::Contains {
                        zone: right_id,
                        element: left_id,
                    }
                }
                _ => PairOutcome::Separated,
            }
        }
    }
}

#[cfg(test)]
fn add_exact_relation(graph: &mut TopologyGraph, pair: (u64, u64), outcome: PairOutcome) {
    let mut seen_edges: HashSet<_> = graph
        .core_edges
        .iter()
        .map(|edge| (edge.source, edge.target, edge.kind))
        .collect();
    add_exact_relation_with_seen(graph, &mut seen_edges, pair, outcome);
}

fn add_exact_relation_with_seen(
    graph: &mut TopologyGraph,
    seen_edges: &mut HashSet<(u64, u64, TopologyEdgeKind)>,
    pair: (u64, u64),
    outcome: PairOutcome,
) {
    if outcome == PairOutcome::Separated {
        return;
    }
    if let PairOutcome::Contains { zone, element } = outcome {
        push_edge(
            graph,
            seen_edges,
            zone,
            element,
            TopologyEdgeKind::ContainsElement,
        );
        return;
    }
    let left_kind = graph.node_kinds.get(&pair.0).copied();
    let right_kind = graph.node_kinds.get(&pair.1).copied();
    match (left_kind, right_kind) {
        (
            Some(TopologyNodeKind::Space | TopologyNodeKind::Zone),
            Some(TopologyNodeKind::Element),
        ) => {
            let kind = match outcome {
                PairOutcome::Touching => TopologyEdgeKind::AdjacentElement,
                PairOutcome::Intersecting => TopologyEdgeKind::IntersectingElement,
                _ => return,
            };
            push_zone_element_edge(graph, seen_edges, pair.0, pair.1, kind);
        }
        (
            Some(TopologyNodeKind::Element),
            Some(TopologyNodeKind::Space | TopologyNodeKind::Zone),
        ) => {
            let kind = match outcome {
                PairOutcome::Touching => TopologyEdgeKind::AdjacentElement,
                PairOutcome::Intersecting => TopologyEdgeKind::IntersectingElement,
                _ => return,
            };
            push_zone_element_edge(graph, seen_edges, pair.1, pair.0, kind);
        }
        (
            Some(TopologyNodeKind::Space | TopologyNodeKind::Zone),
            Some(TopologyNodeKind::Space | TopologyNodeKind::Zone),
        ) => {
            push_edge(
                graph,
                seen_edges,
                pair.0,
                pair.1,
                TopologyEdgeKind::AdjacentZone,
            );
            push_edge(
                graph,
                seen_edges,
                pair.1,
                pair.0,
                TopologyEdgeKind::AdjacentZone,
            );
        }
        (Some(TopologyNodeKind::Element), Some(TopologyNodeKind::Element)) => match outcome {
            PairOutcome::Intersecting | PairOutcome::Touching => {
                let interface = interface_id(pair);
                graph
                    .node_kinds
                    .insert(interface, TopologyNodeKind::Interface);
                push_edge(
                    graph,
                    seen_edges,
                    interface,
                    pair.0,
                    TopologyEdgeKind::InterfaceOf,
                );
                push_edge(
                    graph,
                    seen_edges,
                    interface,
                    pair.1,
                    TopologyEdgeKind::InterfaceOf,
                );
            }
            PairOutcome::Separated | PairOutcome::Contains { .. } => {}
        },
        _ => {}
    }
}

fn push_edge(
    graph: &mut TopologyGraph,
    seen_edges: &mut HashSet<(u64, u64, TopologyEdgeKind)>,
    source: u64,
    target: u64,
    kind: TopologyEdgeKind,
) {
    push_edge_derived(
        graph,
        seen_edges,
        source,
        target,
        kind,
        "GeometryProvider::parry",
    );
}

fn push_zone_element_edge(
    graph: &mut TopologyGraph,
    seen_edges: &mut HashSet<(u64, u64, TopologyEdgeKind)>,
    zone: u64,
    element: u64,
    kind: TopologyEdgeKind,
) {
    let conflicting = match kind {
        TopologyEdgeKind::AdjacentElement => TopologyEdgeKind::IntersectingElement,
        TopologyEdgeKind::IntersectingElement => TopologyEdgeKind::AdjacentElement,
        _ => return,
    };
    // Preserve the first evidence for a pair. Semantic IFC relationships are
    // present before geometry enrichment, so an authored space boundary wins
    // over a conflicting mesh classification. BOT declares adjacency and
    // intersection disjoint; publishing both would make the graph inconsistent.
    if seen_edges.contains(&(zone, element, conflicting)) {
        return;
    }
    push_edge(graph, seen_edges, zone, element, kind);
}

fn push_edge_derived(
    graph: &mut TopologyGraph,
    seen_edges: &mut HashSet<(u64, u64, TopologyEdgeKind)>,
    source: u64,
    target: u64,
    kind: TopologyEdgeKind,
    derived_from: &'static str,
) {
    if !seen_edges.insert((source, target, kind)) {
        return;
    }
    graph.core_edges.push(TopologyEdge {
        source,
        target,
        kind,
        derived_from: Some(derived_from),
    });
}

/// Materialize zone incidence on an aggregate element when one of its
/// subelements supplies the exact geometric evidence.
///
/// This is essential for IFC stairs whose parent `IfcStair` has no Body shape:
/// the flights and landings carry the geometry. Intersection dominates
/// adjacency for the same zone/parent pair because BOT declares those
/// properties disjoint. Containment is intentionally not propagated from one
/// child; the complete aggregate may extend into another zone or storey.
fn propagate_subelement_incidence(
    graph: &mut TopologyGraph,
    seen_edges: &mut HashSet<(u64, u64, TopologyEdgeKind)>,
) -> usize {
    let mut parents_by_child: HashMap<u64, Vec<u64>> = HashMap::new();
    for edge in &graph.core_edges {
        if edge.kind == TopologyEdgeKind::HasSubElement {
            parents_by_child
                .entry(edge.target)
                .or_default()
                .push(edge.source);
        }
    }

    let incidence: Vec<_> = graph
        .core_edges
        .iter()
        .filter(|edge| {
            matches!(
                edge.kind,
                TopologyEdgeKind::AdjacentElement | TopologyEdgeKind::IntersectingElement
            ) && matches!(
                graph.node_kinds.get(&edge.source),
                Some(TopologyNodeKind::Space | TopologyNodeKind::Zone)
            )
        })
        .map(|edge| (edge.source, edge.target, edge.kind))
        .collect();

    let mut strongest: HashMap<(u64, u64), TopologyEdgeKind> = HashMap::new();
    for (zone, child, kind) in incidence {
        let mut stack = parents_by_child.get(&child).cloned().unwrap_or_default();
        let mut visited = HashSet::new();
        while let Some(parent) = stack.pop() {
            if !visited.insert(parent) {
                continue;
            }
            strongest
                .entry((zone, parent))
                .and_modify(|existing| {
                    if kind == TopologyEdgeKind::IntersectingElement {
                        *existing = kind;
                    }
                })
                .or_insert(kind);
            if let Some(grandparents) = parents_by_child.get(&parent) {
                stack.extend(grandparents.iter().copied());
            }
        }
    }

    let before = graph.core_edges.len();
    for ((zone, parent), kind) in strongest {
        let conflicting = match kind {
            TopologyEdgeKind::AdjacentElement => TopologyEdgeKind::IntersectingElement,
            TopologyEdgeKind::IntersectingElement => TopologyEdgeKind::AdjacentElement,
            _ => continue,
        };
        if seen_edges.contains(&(zone, parent, conflicting)) {
            continue;
        }
        push_edge_derived(
            graph,
            seen_edges,
            zone,
            parent,
            kind,
            "GeometryProvider::subelement",
        );
    }
    graph.core_edges.len().saturating_sub(before)
}

fn interface_id((left, right): (u64, u64)) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in left.to_le_bytes().into_iter().chain(right.to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash | (1_u64 << 63)
}

fn world_envelope(flat: &tessellated_model::FlatMesh) -> Option<AABB<[f64; 3]>> {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for geometry in &flat.geometries {
        let transform = multiply(&geometry.world_transform, &geometry.local_transform);
        for point in geometry.mesh.positions.chunks_exact(3) {
            let world = transform_point(&transform, point);
            for axis in 0..3 {
                min[axis] = min[axis].min(world[axis]);
                max[axis] = max[axis].max(world[axis]);
            }
        }
    }
    if !min[0].is_finite() {
        None
    } else {
        Some(AABB::from_corners(min, max))
    }
}

fn world_mesh(flat: &tessellated_model::FlatMesh) -> Option<TriangleMesh> {
    let mut result = TriangleMesh::new();
    for geometry in &flat.geometries {
        let transform = multiply(&geometry.world_transform, &geometry.local_transform);
        let offset = (result.vertices.len() / 3) as u32;
        for point in geometry.mesh.positions.chunks_exact(3) {
            result
                .vertices
                .extend_from_slice(&transform_point(&transform, point));
        }
        result
            .indices
            .extend(geometry.mesh.indices.iter().map(|index| index + offset));
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn multiply(left: &[f64; 16], right: &[f64; 16]) -> [f64; 16] {
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            result[column * 4 + row] = (0..4)
                .map(|k| left[k * 4 + row] * right[column * 4 + k])
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

fn edge_rank(kind: TopologyEdgeKind) -> u8 {
    match kind {
        TopologyEdgeKind::ContainsZone => 0,
        TopologyEdgeKind::ContainsElement => 1,
        TopologyEdgeKind::AdjacentElement => 2,
        TopologyEdgeKind::AdjacentZone => 3,
        TopologyEdgeKind::HasSubElement => 4,
        TopologyEdgeKind::IntersectingElement => 5,
        TopologyEdgeKind::InterfaceOf => 6,
    }
}

#[derive(Debug, Clone)]
pub struct FullTopologyPluginResult<R> {
    pub relations: Arc<Vec<GeometryRelation>>,
    pub report: R,
}

pub fn run_full_topology_plugin<R>(
    model: &IfcModel,
    step: &StepFile,
    input_path: &Path,
    geometry_tolerance: f64,
    bbox_inflation_threshold: f64,
    bbox_report_path: Option<&Path>,
    write_report: bool,
    derive_relations_and_report: impl FnOnce(
        &IfcModel,
        &StepFile,
        &Path,
        f64,
        f64,
    ) -> anyhow::Result<(Vec<GeometryRelation>, R)>,
) -> anyhow::Result<FullTopologyPluginResult<R>>
where
    R: serde::Serialize,
{
    let full_start = Instant::now();
    let (relations, report) = derive_relations_and_report(
        model,
        step,
        input_path,
        geometry_tolerance,
        bbox_inflation_threshold,
    )?;
    tracing::info!(
        "topology-full OCC produced {} relations in {:.3}s",
        relations.len(),
        full_start.elapsed().as_secs_f64(),
    );
    if write_report {
        if let Some(path) = bbox_report_path {
            let report_json = serde_json::to_string_pretty(&report)
                .context("failed to serialize bbox report JSON")?;
            std::fs::write(path, report_json)
                .with_context(|| format!("failed to write bbox report {}", path.display()))?;
        }
    }
    Ok(FullTopologyPluginResult {
        relations: Arc::new(relations),
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_space_element_contact_becomes_adjacent_element() {
        let mut graph = TopologyGraph::default();
        graph.node_kinds.insert(10, TopologyNodeKind::Space);
        graph.node_kinds.insert(20, TopologyNodeKind::Element);

        add_exact_relation(&mut graph, (10, 20), PairOutcome::Touching);

        assert!(graph.core_edges.iter().any(|edge| {
            edge.source == 10 && edge.target == 20 && edge.kind == TopologyEdgeKind::AdjacentElement
        }));
    }

    #[test]
    fn exact_element_contact_creates_one_shared_interface() {
        let mut graph = TopologyGraph::default();
        graph.node_kinds.insert(10, TopologyNodeKind::Element);
        graph.node_kinds.insert(20, TopologyNodeKind::Element);

        add_exact_relation(&mut graph, (10, 20), PairOutcome::Touching);

        let interface = interface_id((10, 20));
        assert_eq!(
            graph.node_kinds.get(&interface),
            Some(&TopologyNodeKind::Interface)
        );
        assert_eq!(
            graph
                .core_edges
                .iter()
                .filter(|edge| {
                    edge.source == interface && edge.kind == TopologyEdgeKind::InterfaceOf
                })
                .count(),
            2
        );
    }

    #[test]
    fn exact_element_intersection_also_creates_an_interface() {
        let mut graph = TopologyGraph::default();
        graph.node_kinds.insert(10, TopologyNodeKind::Element);
        graph.node_kinds.insert(20, TopologyNodeKind::Element);

        add_exact_relation(&mut graph, (10, 20), PairOutcome::Intersecting);

        assert!(!graph
            .core_edges
            .iter()
            .any(|edge| edge.kind == TopologyEdgeKind::IntersectingElement));
        let interface = interface_id((10, 20));
        assert_eq!(
            graph
                .core_edges
                .iter()
                .filter(|edge| {
                    edge.source == interface && edge.kind == TopologyEdgeKind::InterfaceOf
                })
                .count(),
            2
        );
    }

    #[test]
    fn exact_space_element_intersection_is_not_adjacency() {
        let mut graph = TopologyGraph::default();
        graph.node_kinds.insert(10, TopologyNodeKind::Space);
        graph.node_kinds.insert(20, TopologyNodeKind::Element);

        add_exact_relation(&mut graph, (10, 20), PairOutcome::Intersecting);

        assert!(graph.core_edges.iter().any(|edge| {
            edge.source == 10
                && edge.target == 20
                && edge.kind == TopologyEdgeKind::IntersectingElement
        }));
        assert!(!graph.core_edges.iter().any(|edge| {
            edge.source == 10 && edge.target == 20 && edge.kind == TopologyEdgeKind::AdjacentElement
        }));
    }

    #[test]
    fn authored_adjacency_wins_over_conflicting_geometry_intersection() {
        let mut graph = TopologyGraph::default();
        graph.node_kinds.insert(10, TopologyNodeKind::Space);
        graph.node_kinds.insert(20, TopologyNodeKind::Element);
        graph.core_edges.push(TopologyEdge {
            source: 10,
            target: 20,
            kind: TopologyEdgeKind::AdjacentElement,
            derived_from: Some("IfcRelSpaceBoundary"),
        });

        add_exact_relation(&mut graph, (10, 20), PairOutcome::Intersecting);

        assert!(graph.core_edges.iter().any(|edge| {
            edge.source == 10 && edge.target == 20 && edge.kind == TopologyEdgeKind::AdjacentElement
        }));
        assert!(!graph.core_edges.iter().any(|edge| {
            edge.source == 10
                && edge.target == 20
                && edge.kind == TopologyEdgeKind::IntersectingElement
        }));
    }

    #[test]
    fn enclosed_element_becomes_contained_element() {
        let mut graph = TopologyGraph::default();
        graph.node_kinds.insert(10, TopologyNodeKind::Space);
        graph.node_kinds.insert(20, TopologyNodeKind::Element);

        add_exact_relation(
            &mut graph,
            (10, 20),
            PairOutcome::Contains {
                zone: 10,
                element: 20,
            },
        );

        assert!(graph.core_edges.iter().any(|edge| {
            edge.source == 10 && edge.target == 20 && edge.kind == TopologyEdgeKind::ContainsElement
        }));
    }

    #[test]
    fn subelement_intersection_is_materialized_on_parent_element() {
        let mut graph = TopologyGraph::default();
        graph.node_kinds.insert(10, TopologyNodeKind::Space);
        graph.node_kinds.insert(20, TopologyNodeKind::Element);
        graph.node_kinds.insert(30, TopologyNodeKind::Element);
        graph.core_edges.push(TopologyEdge {
            source: 30,
            target: 20,
            kind: TopologyEdgeKind::HasSubElement,
            derived_from: Some("IfcRelAggregates"),
        });
        graph.core_edges.push(TopologyEdge {
            source: 10,
            target: 20,
            kind: TopologyEdgeKind::IntersectingElement,
            derived_from: Some("GeometryProvider::parry"),
        });
        let mut seen: HashSet<_> = graph
            .core_edges
            .iter()
            .map(|edge| (edge.source, edge.target, edge.kind))
            .collect();

        assert_eq!(propagate_subelement_incidence(&mut graph, &mut seen), 1);
        assert!(graph.core_edges.iter().any(|edge| {
            edge.source == 10
                && edge.target == 30
                && edge.kind == TopologyEdgeKind::IntersectingElement
                && edge.derived_from == Some("GeometryProvider::subelement")
        }));
    }
}
