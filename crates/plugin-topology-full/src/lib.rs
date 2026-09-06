use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
            let [x_min, x_max, y_min, y_max, z_min, z_max] =
                *model.world_bounding_boxes().get(&flat.express_id)?;
            let envelope = AABB::from_corners([x_min, y_min, z_min], [x_max, y_max, z_max]);
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

/// Byte ceiling on simultaneously-resident prepared Parry meshes in the extended
/// BOT topology phase.
///
/// Derived from what the machine actually has rather than fixed, because a budget
/// larger than free memory bounds nothing -- it just moves the cost from RAM to
/// swap, where the verdict phase degrades by orders of magnitude instead of
/// failing cleanly. A fixed 8 GiB was fine on the 64-thread server it was tuned
/// on and pathological on a 16 GB workstation running the same model.
///
/// Sources, most specific first:
///   1. `IFC2LBD_TOPOLOGY_MESH_BUDGET_MB` -- operator override, in MiB.
///   2. The cgroup memory limit (v2 `memory.max`, else v1 `memory.limit_in_bytes`),
///      which is what actually kills the process in a container.
///   3. `MemAvailable` from `/proc/meminfo`.
///
/// Whatever is found is taken at 60% and clamped to [256 MiB, 8 GiB]. The other
/// 40% is not slack: the IFC model, the tessellation and the RDF batches in
/// flight are all live at the same time as this cache.
///
/// The budget stays independent of the Rayon thread count -- that was the point
/// of the original change and it still holds. Peak memory does not grow with
/// core count.
fn prepared_cache_budget_bytes() -> usize {
    const MIN: usize = 256 << 20;
    const MAX: usize = 8 << 30;

    let detected = budget_override_bytes()
        .or_else(|| {
            let cgroup = cgroup_limit_bytes();
            let available = mem_available_bytes();
            match (cgroup, available) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (some, None) | (None, some) => some,
            }
            .map(|bytes| bytes / 5 * 3)
        })
        .unwrap_or(MIN);

    detected.clamp(MIN, MAX)
}

/// Operator escape hatch, in MiB, for when the probes below read the wrong thing
/// (an unusual container runtime, a shared host, a deliberately small cap).
/// Applied verbatim rather than scaled -- an explicit number means what it says.
fn budget_override_bytes() -> Option<usize> {
    let raw = std::env::var("IFC2LBD_TOPOLOGY_MESH_BUDGET_MB").ok()?;
    let mib: usize = raw.trim().parse().ok()?;
    Some(mib << 20)
}

#[cfg(target_arch = "wasm32")]
fn cgroup_limit_bytes() -> Option<usize> {
    None
}

#[cfg(target_arch = "wasm32")]
fn mem_available_bytes() -> Option<usize> {
    // No /proc under wasm, and the address space is 4 GiB at most. Let the
    // clamp floor apply rather than pretending to a server-sized budget.
    None
}

/// The container's memory limit, if this process is in a limited cgroup.
/// cgroup v2 reports the literal string "max" when unlimited; v1 reports a
/// sentinel near `u64::MAX`. Both mean "no limit" and yield `None`.
#[cfg(not(target_arch = "wasm32"))]
fn cgroup_limit_bytes() -> Option<usize> {
    let v2 = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok());
    let v1 = || {
        std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
    };
    // Anything at or above half the address space is the "unlimited" sentinel.
    v2.or_else(v1).filter(|&bytes| bytes < (1_usize << 62))
}

/// Memory the kernel believes is available without swapping, which is the number
/// that matters here -- `MemFree` understates it badly on a machine with page cache.
#[cfg(not(target_arch = "wasm32"))]
fn mem_available_bytes() -> Option<usize> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in meminfo.lines() {
        let Some(rest) = line.strip_prefix("MemAvailable:") else {
            continue;
        };
        let kib: usize = rest.split_whitespace().next()?.parse().ok()?;
        return Some(kib << 10);
    }
    None
}

/// Estimate the resident byte footprint of a prepared Parry mesh for one flat
/// geometry, used to enforce [`prepared_cache_budget_bytes`] without actually
/// allocating the mesh just to measure it.
///
/// A Parry `TriMesh` stores 3 × f64 vertices + 3 × u32 indices per triangle
/// (36 B), plus a QBVH acceleration structure (roughly doubles it), plus
/// representative points for containment classification. The containment shell
/// (spaces/zones) is a second full `TriMesh` with half-edge topology. The
/// constant deliberately *over*-estimates so the byte budget actually binds
/// rather than silently letting more memory in than intended.
fn mesh_byte_estimate(flat: &tessellated_model::FlatMesh) -> usize {
    let mut triangles: u64 = 0;
    for geometry in &flat.geometries {
        triangles += (geometry.mesh.indices.len() / 3) as u64;
    }
    // 220 B/triangle covers the collision TriMesh (36 B) + QBVH + representative
    // points; the containment shell (spaces/zones) roughly doubles it, so
    // account for the worst case here.
    (triangles * 220) as usize
}

/// Prepared Parry meshes for the verdict phase, in one of two regimes.
///
/// The split exists because the verdict phase is embarrassingly parallel and any
/// shared mutable state in its inner loop shows up directly as lost throughput.
/// Whenever the referenced meshes fit [`prepared_cache_budget_bytes`] -- which is
/// the overwhelmingly common case -- they are prepared once up front and then read
/// through an immutable map with no synchronisation at all.
enum PreparedMeshes {
    /// Every referenced mesh fits the budget: prepared once, in parallel, and read
    /// lock-free for the rest of the phase.
    Resident(HashMap<u64, Arc<PreparedFlatMesh>>),
    /// Too large to hold at once: a locked cache with FIFO eviction. Meshes are
    /// still built *outside* the lock, so preparation stays parallel here too.
    Bounded(BoundedMeshCache),
}

impl PreparedMeshes {
    fn get(
        &self,
        id: u64,
        flat: &tessellated_model::FlatMesh,
        kind: Option<TopologyNodeKind>,
    ) -> Option<Arc<PreparedFlatMesh>> {
        match self {
            PreparedMeshes::Resident(meshes) => meshes.get(&id).cloned(),
            PreparedMeshes::Bounded(cache) => cache.get_or_prepare(id, flat, kind),
        }
    }

    fn builds(&self) -> usize {
        match self {
            PreparedMeshes::Resident(meshes) => meshes.len(),
            PreparedMeshes::Bounded(cache) => cache.builds.load(Ordering::Relaxed),
        }
    }

    fn cache_mode(&self) -> &'static str {
        match self {
            PreparedMeshes::Resident(_) => "shared-full",
            PreparedMeshes::Bounded(_) => "shared-bounded",
        }
    }
}

/// Byte-budgeted mesh cache for models whose referenced meshes do not all fit in
/// memory at once. Demand-loads, and evicts oldest-first to stay under budget.
struct BoundedMeshCache {
    budget_bytes: usize,
    builds: AtomicUsize,
    inner: Mutex<CacheState>,
}

struct CacheEntry {
    mesh: Arc<PreparedFlatMesh>,
    bytes: usize,
}

struct CacheState {
    meshes: HashMap<u64, CacheEntry>,
    /// Insertion order, so eviction can drop the oldest entry rather than the
    /// whole cache.
    order: VecDeque<u64>,
    total_bytes: usize,
}

impl BoundedMeshCache {
    fn new(budget_bytes: usize) -> Self {
        BoundedMeshCache {
            budget_bytes,
            builds: AtomicUsize::new(0),
            inner: Mutex::new(CacheState {
                meshes: HashMap::new(),
                order: VecDeque::new(),
                total_bytes: 0,
            }),
        }
    }

    /// Fetch a prepared mesh by express id, building it on demand if absent.
    ///
    /// The lock is held only for the map lookup and the insert -- never across
    /// `prepare_flat_mesh`, which builds a world `TriangleMesh` and its QBVH and is
    /// by far the expensive part. Holding it there would put every Rayon worker
    /// in a queue behind one mesh build at a time, turning the whole preparation
    /// pass single-threaded no matter how many cores the pool has.
    fn get_or_prepare(
        &self,
        id: u64,
        flat: &tessellated_model::FlatMesh,
        kind: Option<TopologyNodeKind>,
    ) -> Option<Arc<PreparedFlatMesh>> {
        if let Some(hit) = self
            .inner
            .lock()
            .expect("mesh cache poisoned")
            .meshes
            .get(&id)
        {
            return Some(hit.mesh.clone());
        }

        // Built with no lock held. Two threads racing on the same id both build
        // it and one discards its copy -- a bounded, rare waste, and far cheaper
        // than serialising every build in the pool behind a single mutex.
        let prepared = Arc::new(prepare_flat_mesh(flat, kind)?);
        self.builds.fetch_add(1, Ordering::Relaxed);
        let bytes = mesh_byte_estimate(flat);

        let mut state = self.inner.lock().expect("mesh cache poisoned");
        if let Some(winner) = state.meshes.get(&id) {
            // Lost the race: keep the copy already cached so both callers share one.
            return Some(winner.mesh.clone());
        }
        if bytes > self.budget_bytes {
            // A single mesh larger than the entire budget: hand it back uncached
            // rather than evicting everything else to make room for it.
            return Some(prepared);
        }
        // Evict oldest-first until this one fits. An evicted mesh stays alive for
        // whoever is still analysing a pair with it -- they hold an `Arc` -- so the
        // real peak is the budget plus the handful of meshes in flight, which is
        // bounded by the thread count rather than by model size.
        while state.total_bytes + bytes > self.budget_bytes {
            let Some(oldest) = state.order.pop_front() else {
                break;
            };
            if let Some(evicted) = state.meshes.remove(&oldest) {
                state.total_bytes = state.total_bytes.saturating_sub(evicted.bytes);
            }
        }
        state.meshes.insert(
            id,
            CacheEntry {
                mesh: prepared.clone(),
                bytes,
            },
        );
        state.order.push_back(id);
        state.total_bytes += bytes;
        Some(prepared)
    }
}

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
/// Every mesh referenced by a candidate pair is prepared exactly once and reused
/// across all verdicts. When they all fit [`prepared_cache_budget_bytes`] they are
/// prepared in parallel into an immutable map that the verdict phase reads without
/// synchronisation; larger models fall back to a demand-loaded cache with
/// oldest-first eviction. Either way the ceiling is independent of the Rayon
/// thread count, so peak memory does not grow with core count.
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

    // Every mesh referenced by a candidate pair, exactly once.
    let mut referenced_ids: Vec<u64> = candidates
        .pairs
        .iter()
        .flat_map(|&(left, right)| [left, right])
        .collect();
    referenced_ids.sort_unstable();
    referenced_ids.dedup();

    let total_estimate: usize = referenced_ids
        .iter()
        .filter_map(|&id| flat_by_id.get(&id))
        .map(|flat| mesh_byte_estimate(flat))
        .sum();

    let budget_bytes = prepared_cache_budget_bytes();
    tracing::info!(
        "BOT extended topology mesh cache: {} referenced meshes, {:.1} GiB estimated, {:.1} GiB budget",
        referenced_ids.len(),
        total_estimate as f64 / (1u64 << 30) as f64,
        budget_bytes as f64 / (1u64 << 30) as f64,
    );

    let meshes = if total_estimate <= budget_bytes {
        // Fits: prepare them all in parallel, then read lock-free. Peak memory is
        // the total of the referenced meshes -- fixed, and independent of how many
        // threads are analysing pairs.
        PreparedMeshes::Resident(
            referenced_ids
                .par_iter()
                .filter_map(|&id| {
                    let flat = flat_by_id.get(&id)?;
                    let kind = graph.node_kinds.get(&id).copied();
                    prepare_flat_mesh(flat, kind).map(|mesh| (id, Arc::new(mesh)))
                })
                .collect(),
        )
    } else {
        // Does not fit: demand-load during the verdict phase and evict as we go.
        // No pre-warm pass -- filling a cache that must immediately evict most of
        // what it just built is pure wasted work.
        PreparedMeshes::Bounded(BoundedMeshCache::new(budget_bytes))
    };

    let verdicts: Vec<((u64, u64), PairOutcome)> = candidates
        .pairs
        .par_iter()
        .map(|&(left, right)| {
            let left_mesh = flat_by_id
                .get(&left)
                .and_then(|f| meshes.get(left, f, graph.node_kinds.get(&left).copied()));
            let right_mesh = flat_by_id
                .get(&right)
                .and_then(|f| meshes.get(right, f, graph.node_kinds.get(&right).copied()));
            let outcome = match (left_mesh, right_mesh) {
                (Some(left_mesh), Some(right_mesh)) => {
                    analyze_pair(left, right, graph, &left_mesh, &right_mesh, &options)
                }
                _ => PairOutcome::Separated,
            };
            ((left, right), outcome)
        })
        .collect();

    let prepared_mesh_builds = meshes.builds();
    let cache_mode = meshes.cache_mode();
    drop(meshes);

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
