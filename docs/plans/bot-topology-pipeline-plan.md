# BOT topology pipeline plan

Status: planning baseline on `plan/bot-topology-pipeline`  
Date: 2026-08-14  
Scope: local experiments and core-library work only. Do not register the new path in the CLI or
WASM runners until the acceptance gates below pass.

## 1. Goal

Build a trustworthy BOT topology model from IFC without first materialising full ifcOWL. The
implementation must:

1. prefer authored IFC relationship evidence;
2. use geometry to confirm or fill gaps, not silently replace IFC semantics;
3. distinguish candidate generation from topological conclusions;
4. emit only relations allowed by BOT's domains and ranges;
5. preserve evidence, confidence and diagnostics internally;
6. remain deterministic, memory-bounded, pure Rust and eventually WASM-compatible.

The first deliverable on this branch is a standalone local probe and a tested core API. Integration
into `neo-bot-producer`, plugin registration, CLI orchestration and WASM orchestration is a later
phase and must update both runners together.

## 2. What BOT contains

The canonical BOT vocabulary is maintained by the W3C Linked Building Data Community Group. Its
current source is [bot.ttl](https://github.com/w3c-lbd-cg/bot/blob/master/bot.ttl), with the
canonical representation at <https://w3id.org/bot/bot.ttl>.

### Classes

- `bot:Zone`, with `bot:Site`, `bot:Building`, `bot:Storey` and `bot:Space` as subclasses.
- `bot:Element` for construction constituents.
- `bot:Interface` for a qualified relation between two or more things where at least one thing is
  a zone or building element.

BOT declares `Zone`, `Element` and `Interface` disjoint. An IFC relationship must therefore not be
typed as an element or zone merely because it participates in topology.

### Zone hierarchy

- `bot:containsZone` (`Zone -> Zone`, transitive)
- `bot:hasBuilding` (subproperty of `containsZone`)
- `bot:hasStorey` (subproperty of `containsZone`)
- `bot:hasSpace` (subproperty of `containsZone`)

BOT has no project class and no project-to-site predicate. `IfcProject` currently maps to
`dicp:ConstructionProject`; emitting `project bot:containsZone site` would infer that the project is
also a `bot:Zone` through the property's domain. Keep project linkage outside BOT unless an explicit,
documented alignment intentionally gives the project both types.

### Zone relationships

- `bot:adjacentZone` (`Zone -> Zone`, symmetric)
- `bot:intersectsZone` (`Zone -> Zone`, symmetric)

These properties are disjoint. Touching at a meaningful boundary is adjacency; overlap of spatial
interiors is intersection.

### Zone-to-element relationships

- `bot:hasElement` (`Zone -> Element`)
- `bot:containsElement` (subproperty of `hasElement`)
- `bot:adjacentElement` (subproperty of `hasElement`)
- `bot:intersectingElement` (subproperty of `hasElement`)

These are not element-to-element predicates. In particular, two touching walls must not be linked
with `bot:adjacentElement`, and two clashing elements must not be linked with
`bot:intersectingElement`. BOT uses an `Interface` to qualify an element-to-element connection.

### Element and interface relationships

- `bot:hasSubElement` (`Element -> Element`)
- `bot:interfaceOf` (`Interface -> owl:Thing`; normally zones and/or elements here)

### Geometry hooks

- `bot:has3DModel` links a zone or element to a separately described geometry resource.
- `bot:hasSimple3DModel` stores a literal geometry representation.
- `bot:hasZeroPoint` is unstable and out of scope for topology derivation.

The geometry itself remains the concern of OMG/FOG/GeoSPARQL or the existing geometry sidecar. BOT
does not define meshes, boundary surfaces, contact areas or coordinate systems.

## 3. What BOT does not contain

BOT deliberately does not model:

- IFC relationship identity, owner history or lifecycle;
- path connection location (`ATSTART`, `ATEND`, `ATPATH`) or layer priorities;
- physical/virtual and internal/external boundary flags;
- first-level versus second-level space-boundary semantics;
- boundary curves/surfaces, contact patches or shared-boundary area;
- boolean subtraction by openings;
- material layers, profiles, properties, quantities or systems;
- evidence quality, tolerance, algorithm version or confidence;
- passability, entrances/exits, route direction, traversal cost or accessibility constraints.

Do not invent BOT or project-specific RDF predicates for these concepts. Keep them in typed
internal evidence records until a maintained external vocabulary has been selected and its meaning
verified. Being useful to the implementation is not sufficient reason to publish a new term.

## 4. IFC-to-BOT mapping contract

| IFC evidence | BOT conclusion | Conditions |
|---|---|---|
| `IfcRelAggregates` between spatial nodes | `containsZone`, or the specific `hasBuilding` / `hasStorey` / `hasSpace` subproperty | Direct relationship is valid and endpoints are zones |
| `IfcRelContainedInSpatialStructure` | `containsElement` | Structure is a zone and object is an element |
| `IfcRelSpaceBoundary` | `space adjacentElement element` | The relationship actually names the space and boundary element |
| `IfcRelSpaceBoundary2ndLevel.CorrespondingBoundary` | `adjacentZone` and one shared `Interface` | Pair is internal and connects distinct spaces |
| Matched first-level boundaries | `adjacentZone` and an `Interface` | Only after geometric boundary matching; sharing the same wall ID alone is insufficient |
| `IfcRelVoidsElement` + `IfcRelFillsElement` | `host hasSubElement filling` | Both relationships use the same opening |
| `IfcRelConnectsPathElements` | `Interface interfaceOf elementA, elementB` | Represents connectivity; keep path position/priorities as evidence, not BOT |
| Confirmed zone/element containment | `containsElement` | Exact or sufficiently validated solid classification |
| Confirmed zone/element boundary contact | `adjacentElement` plus optional `Interface` | Contact must bound the zone; proximity alone is insufficient |
| Confirmed zone/element interior overlap | `intersectingElement` | Interior overlap, not surface touch |
| Confirmed zone/zone boundary contact | `adjacentZone` plus `Interface` | Meaningful boundary contact |
| Confirmed zone/zone interior overlap | `intersectsZone` | Interior overlap |
| Confirmed element/element boundary contact or authored path connection | shared `Interface` | Never `adjacentElement` |

`IfcRelVoidsElement` alone describes subtraction and does not imply a BOT subelement: the opening is
a feature, while the filled door/window becomes a host subelement only when `IfcRelFillsElement`
closes the chain.

### Stable interface identity

- Authored relationship: derive the interface IRI from the IFC relationship GUID where available.
- Paired second-level boundaries: derive it from the sorted pair of boundary GUIDs.
- Geometry-derived interface: derive it from sorted endpoint IRIs, relation kind, tolerance profile
  and an optional contact-patch key.
- Never use arithmetic such as `left_id + right_id + constant`; it is collision-prone and unstable
  across files.

## 5. Existing repository inventory

### Reusable

1. `crates/ifc-model`
   - Already parses aggregation, spatial containment, void/fill relationships and basic space
     boundaries.
   - Already creates stable domain entities used by BOT/BEO.
2. `crates/lbd-topology`
   - Has `TopologyGraph`, node/edge kinds, deterministic sorting and relation-backed topology.
   - Correctly maps space boundary to `adjacentElement` and void+fill to `hasSubElement`.
3. `crates/lbd-geometry`
   - Has bounding-box and exact-kernel abstractions.
   - Already depends on `rstar` and Parry 0.26.
   - Has Parry `TriMesh` conversion, batched pair processing and AABB/contact experiments.
4. `crates/plugin-geometry-preprocess` and `crates/tessellated-model`
   - Provide the existing tessellation stage and shared mesh representation.
   - A topology implementation should consume this output instead of implementing another IFC
     representation parser.
5. `crates/lbd-ontology`
   - Already contains BOT IRI helpers.
6. `lbd-converter::stream_topology_model` and the monolithic topology emitter
   - Useful as migration reference and for output comparison, but not the final dispatch path.

### Incomplete or unsafe as currently written

1. The registered `neo-bot-producer` emits hierarchy and containment but does not call
   `lbd-topology`.
2. `plugin-topology-full` is a helper function, not a `PipelinePlugin` implementation, despite
   stale documentation claiming it is registered.
3. `IfcRelationEvidenceEnricher` is a no-op.
4. `lbd-topology` infers every pair of spaces using the same element as adjacent. A long wall can
   bound multiple unrelated spaces, so this creates false positives unless corresponding boundary
   evidence or boundary-geometry matching exists.
5. `lbd-geometry::derive_relations_from_bounding_boxes` is an O(n²) scan. The crate depends on
   `rstar`, but the current source does not use an R-tree.
6. Bounding-box overlap is currently capable of becoming `IntersectingElement` evidence. AABB
   overlap is only a candidate signal and must never become a published topology assertion.
7. Some dormant geometry relations treat element-element contact/intersection as BOT
   `adjacentElement`/`intersectingElement`; these predicates are `Zone -> Element`.
8. `csg.rs` duplicates IFC-to-mesh extraction and ignores or simplifies several boolean operations.
   It should consume `TessellatedModel` instead.
9. The `ParryGeometryKernel::analyze_pair` trait implementation intentionally returns an error and
   tells callers to use a separate batched function. The abstraction is therefore not complete.
10. Interface IDs are synthetic integer sums and can collide.
11. Hidden CLI voxel options remain, but no current voxel stage consumes them.
12. The old history contains an R-tree/voxel implementation. Its surface-voxel 6-neighbour test is
    useful as an experiment, but its fixed resolution and giant-element false positives make it
    unsuitable as authoritative topology.
13. The current BOT producer emits `IfcProject bot:containsZone IfcSite`, although the project is
    typed as `dicp:ConstructionProject` and BOT does not define a project zone. This mapping needs a
    separate alignment or must be removed from the standards-only BOT graph.

### Missing IFC model fields

Extend the typed model before topology inference:

- space-boundary entity subtype/level;
- connection-geometry reference;
- `ParentBoundary` and `CorrespondingBoundary` for 1st/2nd-level boundaries;
- relationship GUID for stable interface IRIs;
- `IfcRelConnectsPathElements`, including both elements, connection geometry, priorities and
  connection types.

Parsing must remain schema-aware: inherited argument positions differ across IFC versions and must
be tested for IFC2X3, IFC4 and IFC4X3 rather than assumed from one schema CSV.

## 6. External approaches and what to borrow

### TopologicPy / Topologic

[TopologicPy](https://topologicpy.readthedocs.io/en/latest/) uses an Open CASCADE-backed
non-manifold topology core. Relevant concepts include:

- cells for spaces/solids;
- faces as explicit shared boundaries;
- cell complexes that retain internal and external faces;
- `AdjacentTopologies`, `SharedFaces`, `SharedTopologies` and spatial relationship classification;
- graph generation from topology, IFC relationships or semantic triples;
- defeaturing before topology construction.

Borrow the conceptual model: make shared boundaries first-class evidence and derive graphs from
them. Do not add TopologicPy as a runtime dependency: it is Python/C++/Open CASCADE, AGPL-licensed,
large, and unsuitable for the intended pure-Rust/WASM path. It can be an offline oracle on a small
test corpus.

#### Topologic concepts to retain

[TopologicPy's topology model](https://topologicpy.readthedocs.io/en/latest/topologicpy.Topology.html)
contains more than spatial adjacency. The following concepts are valuable, but adopting a
computational concept does not automatically mean emitting its `top:` RDF term.

| Topologic concept | Meaning for this project | Representation decision |
|---|---|---|
| `Vertex`, `Edge`, `Wire`, `Face`, `Shell`, `Cell` | Dimensional primitives and their incidence | Internal geometry only initially. Existing triangle vertices/faces are sufficient for the first tier; introduce explicit loops, shells and cells only where algorithms require them |
| `CellComplex` | Cells whose shared faces are retained once rather than duplicated | Target conceptual model for reliable space-boundary reconstruction; not required for relationship-only BOT |
| shared subtopologies (`SharedFaces`, `SharedEdges`, `SharedVertices`) | Classify face-, edge- and point-contact separately | Implement shared/contact-patch classification; only meaningful shared faces become building interfaces by default |
| subtopology/supertopology incidence | Which face bounds which shell/cell and which edge bounds which face | Maintain explicit incidence tables with stable IDs; do not infer this repeatedly from coordinates |
| external and internal boundaries | Envelope faces versus faces separating cells | Map to typed internal boundary patches and IFC boundary evidence; BOT alone cannot serialize the distinction |
| free and non-manifold subtopologies | Boundaries belonging to no host, or to an invalid number of cells | Mesh/cell-complex quality diagnostics and `Unknown` verdicts, not published BOT assertions |
| `Aperture` | An opening embedded in a face/edge/cell boundary | Model the geometric aperture and preserve its IFC opening/door/window chain. An aperture is not automatically a door or a navigable portal |
| `Content` / `Context` | Semantic objects attached to topology and the inverse placement relation | Use explicit links between IFC/BOT entities and internal primitives; do not expose generic context predicates unless a reviewed ontology needs them |
| topology `Dictionary` | Metadata attached to primitives | Replace with typed Rust records for identity, evidence, quality and provenance; avoid untyped string-key dictionaries in the core |
| dual `Graph` | Cells become nodes and shared/passable boundaries become edges | Build two derived graphs: physical cell adjacency and the stricter room/portal navigation graph |
| boolean/imprint/slice/self-merge operations | Make shared boundaries explicit and repair fragmented input | Use narrowly for boundary reconstruction after benchmarking; this is beyond Parry's collision API |
| defeaturing, welding and coplanar/collinear merging | Normalise tiny fragments and nearly coincident geometry | Add as tolerance-profiled preprocessing with source traceability; never silently alter authored geometry |
| spatial relationship classification | DE-9IM/RCC-style relations between topology objects | Prefer GeoSPARQL semantics for RDF and conservative internal classifiers for meshes |
| internal/selecting vertex | Stable representative point for associating a cell with a graph node or IFC space | Compute a point proven inside the cell, not merely a centroid, because concave spaces can have an external centroid |
| weighted/directed graph paths | Shortest, constrained and direction-sensitive routes | Reuse the concept in the navigation layer; graph algorithms do not establish passability themselves |
| `Cluster` | Heterogeneous collection without stronger topology | Useful only as an import/diagnostic container; not a semantic building relation |
| `Grid`, `Matrix`, `Vector`, `Project` | Utility or container concepts | Reuse existing math/transforms and `dicp:ConstructionProject`; do not duplicate them from TopologicPy |

The current [TopologicPy ontology module](https://topologicpy.readthedocs.io/en/latest/topologicpy.Ontology.html)
defines classes such as `Cell`, `CellComplex`, `Face`, `Aperture` and `Graph`, and properties such as
`adjacentTo`, `containsElement`, `interfaceOf`, `hasFaces` and `hasInternalBoundaries`. Several
overlap BOT or geometry vocabularies with different domains and intent. Therefore its ontology is
an evaluation candidate, not an automatic dependency. In particular:

- use BOT's `Interface`, adjacency and containment terms for the BOT graph;
- use GeoSPARQL for reviewed DE-9IM relations;
- do not equate `top:Cell` with every `bot:Space`: derived free-space cells may subdivide one room,
  and some IFC spaces may not have valid closed cells;
- do not equate a `Face` with a BOT `Interface`: a face is geometry, while an interface is a
  qualified building relation that can reference boundary geometry;
- do not expose the analysis graph as though its vertices and edges were physical building
  objects.

#### Incremental implementation instead of a complete Topologic clone

**Tier 1 — shared-boundary model, required:**

1. Add stable internal IDs for connected shells and boundary/contact patches.
2. Build triangle incidence maps (vertex-to-face and undirected-edge-to-face) from
   `TessellatedModel` once per unique mesh.
3. Classify closed, open and non-manifold shells; an undirected mesh edge with one incident face is
   a boundary edge, two is manifold, and more than two is non-manifold.
4. Group coplanar, consistently oriented triangles into face patches without losing their source
   triangle/entity IDs.
5. Represent `BoundaryPatch { host_cells, host_elements, kind, geometry_ref, evidence }` and make
   BOT interfaces refer to confirmed patches internally.
6. Represent an aperture as one or more inner boundary loops plus its IFC
   void/fill/product evidence.
7. Generate a dual physical-adjacency graph from confirmed shared patches and a separate
   navigation graph only from confirmed passable apertures/connectors.

This tier needs the existing Parry and `rstar` dependencies plus deterministic incidence maps. It
does not require a general B-rep or CSG crate.

**Tier 2 — boundary reconstruction, conditional:**

1. Weld near-coincident vertices under an explicit metric tolerance while retaining a reversible
   source map.
2. Project candidate coplanar patches to a stable 2D plane and calculate polygon overlap,
   difference and inner loops.
3. Imprint matched overlap polygons on both sides so a shared patch has one canonical identity.
4. Reconstruct and validate closed cell shells; calculate a proven internal point and oriented
   boundary.
5. Compare results with TopologicPy `SharedFaces`, `InternalFaces`, `ExternalFaces` and
   `NonManifoldFaces` on the oracle corpus.

This tier requires selecting and benchmarking a robust 2D polygon/predicate implementation. Do not
choose a crate merely from its API name; test holes, near-coincident edges, large coordinates and
deterministic output first.

**Tier 3 — complete non-manifold cell complex, optional research:**

Constructing a Topologic-style cell complex from arbitrary overlapping solids requires robust
boolean intersection, imprinting, splitting, healing, orientation and tolerance management. Parry
does not provide these operations. Before this tier, establish that Tier 1/2 cannot answer the
competency questions, then separately evaluate a native B-rep kernel or an offline native helper.
This tier is likely to conflict with the pure-Rust/WASM goal and must not be smuggled into the first
topology plugin.

Acceptance tests for Topologic-inspired support must include:

- every shell face/edge incidence is internally consistent;
- external patches have one host cell, ordinary internal patches have two, and higher incidence is
  explicitly non-manifold;
- face-, edge- and point-contact produce different classifications;
- aperture loops lie within their host patch and retain the opening/filling chain;
- physical graph edges correspond to confirmed shared interfaces;
- navigation graph edges correspond to confirmed passable portals, never merely shared walls;
- normalization preserves a reversible map to IFC entities and source triangles.

### IfcOpenShell geometry trees

[IfcOpenShell's geometry tree](https://docs.ifcopenshell.org/ifcopenshell-python/geometry_tree.html)
separates:

1. spatial-tree candidate selection;
2. surface collision/touching;
3. interior intersection with a tolerance;
4. clearance/proximity queries.

Borrow this separation and its explicit tolerance semantics. Use IfcOpenShell only as a local
reference oracle, not a runtime dependency.

### buildingSMART space boundaries

The official definitions distinguish logical/physical boundaries, first-level closed-shell
boundaries and second-level paired heat-transfer boundaries. In particular:

- [IfcRelSpaceBoundary](https://ifc43-docs.standards.buildingsmart.org/IFC/RELEASE/IFC4x3/HTML/lexical/IfcRelSpaceBoundary.htm)
  is the authoritative space-to-element delimiter;
- [first-level boundaries](https://ifc43-docs.standards.buildingsmart.org/IFC/RELEASE/IFC4x3/HTML/lexical/IfcRelSpaceBoundary1stLevel.htm)
  can form the shell but do not encode what lies on the other side;
- [second-level boundaries](https://ifc43-docs.standards.buildingsmart.org/IFC/RELEASE/IFC4x3/HTML/lexical/IfcRelSpaceBoundary2ndLevel.htm)
  can explicitly pair opposite sides with `CorrespondingBoundary`;
- [IfcRelVoidsElement](https://ifc43-docs.standards.buildingsmart.org/IFC/RELEASE/IFC4x3/HTML/lexical/IfcRelVoidsElement.htm)
  implies geometric subtraction;
- [IfcRelConnectsPathElements](https://ifc43-docs.standards.buildingsmart.org/IFC/RELEASE/IFC4x3/HTML/lexical/IfcRelConnectsPathElements.htm)
  carries element connectivity plus start/end/path position and layer priorities.

### Parry and R-tree

- [Parry](https://parry.rs/docs/) is appropriate for BVH-accelerated intersection, distance,
  closest-point, ray and contact queries. It is collision/proximity software, not a B-rep boolean
  or mesh-repair kernel.
- `intersection_test` deliberately treats touching and penetration alike. It cannot by itself
  classify BOT adjacency versus intersection.
- Evaluate `parry3d-f64` 0.26 before committing to `parry3d`/f32. BIM geometry and georeferenced
  coordinates require a documented precision strategy; RTC/local coordinates may make f32 viable,
  but this must be measured.
- [`rstar::RTree`](https://docs.rs/rstar/latest/rstar/struct.RTree.html) supports bulk loading and
  intersecting-envelope queries. Use it for broad-phase candidate generation.

### Voxels

Voxels are useful for occupancy, flood-fill, missing-space reconstruction and approximate
connectivity in dirty non-manifold models. They are resolution-dependent, can merge narrow gaps,
lose thin elements, and consume large amounts of memory. Therefore:

- no voxel crate is required for phases 1-3;
- first evaluate the removed in-repository SAT surface voxelizer in an isolated benchmark;
- if retained, use a sparse/chunked representation with an explicit resolution profile;
- voxel conclusions must be marked approximate and must not override authored IFC evidence;
- do not adopt a game/world voxel engine merely for one fallback operation.

## 7. Proposed internal model

Replace the current `core_edges` versus `extension_edges` distinction with evidence-bearing facts:

```rust
TopologyFact {
    subject: TopologyNode,
    relation: TopologyRelation,
    object: TopologyNode,
    evidence: Vec<Evidence>,
    verdict: Verdict,
}

Evidence {
    source: EvidenceSource,       // IFC relationship, exact mesh, voxel, etc.
    source_entity_ids: Vec<u64>,
    method: MethodId,
    tolerance_m: Option<f64>,
    measurements: Measurements,  // distance/contact area/etc. when available
}

Verdict = Confirmed | Approximate | Rejected | Unknown
```

Only `Confirmed` facts are emitted to the standards-only BOT graph by default. Approximate facts
are diagnostics unless a caller explicitly requests an experimental evidence graph.

Represent interfaces as real nodes in the topology graph, not as edge-source exceptions. This
removes the current special case where a synthetic interface ID is absent from `node_kinds`.

## 8. Staged topology pipeline

### Stage A — IFC semantic facts

Inputs: `IfcModel`, `StepFile`. No geometry.

1. Build zone hierarchy and direct containment.
2. Emit space-to-element adjacency from valid space boundaries.
3. Pair second-level corresponding boundaries exactly.
4. Do not infer space pairs merely because they reference the same element.
5. Resolve void/fill chains to host/filling `hasSubElement`.
6. Convert path connections to explicit interface nodes.
7. Record invalid, dangling and contradictory relationships in a report.

This stage should be fast, deterministic and useful without geometry.

### Stage B — Shared world-space geometry index

Inputs: `TessellatedModel`, model identity map.

1. Reuse tessellated meshes and their world transforms.
2. Build one entity-to-mesh map and one world-space AABB per zone/element.
3. Track mesh quality: empty, open shell, non-manifold edge, self-intersection suspicion, degenerate
   triangles and coordinate range.
4. Bulk-load AABBs into `rstar`.
5. Generate only meaningful pair classes: zone-zone, zone-element and selected element-element
   pairs. Apply storey/containment and IFC-class filters before narrow phase.
6. Inflate query envelopes by the selected tolerance, but never turn an AABB result into a fact.

### Stage C — Exact/near-contact classification

Inputs: candidate pairs and validated meshes.

Use Parry for BVH-accelerated primitive queries, then classify with pair-specific rules:

- separated beyond tolerance -> rejected;
- within clearance only -> proximity diagnostic, not BOT adjacency;
- surface contact -> possible adjacency/interface;
- interior penetration -> intersection;
- full containment without surface collision -> containment, requiring point-in-solid/ray checks on
  a closed mesh;
- ambiguous because the mesh is open/non-manifold -> unknown or voxel fallback.

`contact()` returning one contact point is insufficient proof of a shared face. Face-only, edge-only
and point-only contact must be distinguishable when generating a building interface. Start with
interface existence; shared contact geometry/area is a later capability.

### Stage D — Boundary/interface reconstruction

1. Prefer authored `ConnectionGeometry` and corresponding-boundary links.
2. Match unpaired boundary surfaces using spatial index, opposite normals, coplanarity, overlap area
   and tolerance.
3. For geometry-only element interfaces, group contact samples by connected patch so one physical
   junction does not become many interface nodes.
4. If contact area is required, add a focused planar-polygon overlap implementation or evaluate an
   appropriate library separately. Parry is not a boolean intersection-surface generator.

### Stage E — Optional voxel fallback

Run only for candidates left `Unknown`, or for explicit space reconstruction mode:

1. choose cell size from model units and a documented tolerance profile;
2. rasterize surface occupancy conservatively;
3. for closed solids, flood-fill exterior and classify interior occupancy;
4. use 6-connectivity for face adjacency and keep edge/vertex contact separate;
5. reject/skip elements exceeding memory limits rather than silently asserting adjacency;
6. report resolution, occupied-cell count and every approximate conclusion.

Voxels are not needed when valid IFC boundaries or reliable exact meshes already answer the
question.

### Stage F — BOT emission

1. Validate every fact against BOT subject/object kinds.
2. Emit stable interface IRIs.
3. Deduplicate facts while retaining all evidence internally.
4. Materialise both directions of symmetric relations for simple SPARQL clients, while keeping a
   canonical internal pair for determinism.
5. Keep geometry/evidence triples in separate named graphs.

## 9. Rust dependencies

### Keep and actually use

- `rstar = 0.12`: broad-phase spatial index.
- Parry 0.26 API: BVH-backed `TriMesh`, distance, closest points, contact, ray and intersection
  queries.
- existing `rayon`, `crossbeam`, `rustc-hash`, `thiserror`, `tracing` and shared mesh crates.

### Evaluate before choosing

- `parry3d-f64 = 0.26`: preferred correctness experiment versus current f32 build.
- `petgraph`: only if graph algorithms beyond the current deterministic maps/vectors are needed.
  It is not required merely to store BOT facts.
- a 2D polygon boolean library for coplanar contact patches, only after interface-area requirements
  are proven.

### Do not add initially

- TopologicPy/Open CASCADE runtime dependencies;
- a general voxel/game-world engine;
- another IFC parser or tessellator;
- a full CSG library solely for boolean existence checks.

## 10. Local-first implementation phases

### Phase 0 — Correctness harness

- Add a local `topology-probe` binary or example that accepts an IFC path and writes JSON evidence
  plus an optional BOT Turtle preview.
- It calls core library APIs directly and is not registered as a pipeline module.
- Record timings, peak/counted allocations where practical, candidate reduction, mesh-quality
  counts, verdict counts and evidence source.

### Phase 1 — Relationship-complete BOT

- Extend `IfcModel` for boundary levels/correspondence and path connections.
- Refactor `lbd-topology` to evidence-bearing facts and real interface nodes.
- Remove the shared-element space-adjacency shortcut.
- Add a standards-valid BOT emitter independent of `lbd-converter::emit_lbd`.
- Validate on synthetic IFC2X3/IFC4/IFC4X3 relationship fixtures.

### Phase 2 — Geometry adapter and R-tree

- Adapt `TessellatedModel` to world-space topology meshes.
- Replace O(n²) AABB scans with `rstar` candidate generation.
- Remove duplicated STEP mesh extraction from the topology path.
- Produce candidate and mesh-quality reports without publishing geometry-derived BOT facts yet.

### Phase 3 — Parry classifier

- Benchmark f32 with RTC/local coordinates against `parry3d-f64`.
- Implement pair-specific classification and conservative `Unknown` handling.
- Add batched, memory-bounded processing over unique entities rather than repeatedly extracting the
  same mesh per pair batch.
- Enable geometry-derived facts only after the synthetic truth-table tests pass.

### Phase 4 — Boundary matching and interfaces

- Decode authored connection surfaces where feasible.
- Pair first-level/unpaired boundaries by geometry.
- Group element contact into deterministic interface nodes.
- Compare a corpus against IfcOpenShell and TopologicPy as offline oracles.

### Phase 5 — Voxel experiment, only if justified

- Port the removed voxel code into an isolated benchmark, not production flow.
- Test multiple resolutions and thin/gapped/large elements.
- Keep it only if it improves recall on `Unknown` cases without unacceptable false positives or
  memory use.

### Phase 6 — Pipeline integration after approval

The preferred final integration is:

1. `neo-bot-producer` remains the single owner of BOT RDF and always includes relationship-backed
   topology.
2. A new geometry topology **preprocess** plugin, copied from
   `crates/plugin-template-preprocess`, optionally enriches a `TopologyGraph`/evidence slot using
   `TessellatedModel` before producers run.
3. The BOT producer consumes the enriched context when present and otherwise uses semantic facts.
4. Never make one producer depend on another producer.
5. Register the preprocess plugin in both CLI and WASM and update both orchestration paths together.
6. Use `ctx.insert` only for a new topology type and `ctx.replace` for subsequent enrichment.
7. Keep serialization in the runners; do not add a serializer method to `SerializerPlugin`.

Do not publish or persist a new plugin ID until the experiment establishes whether one optional
geometry preprocessor is sufficient or relation/exact preprocessors must remain separate.

## 11. Test matrix and acceptance gates

### Synthetic geometry truth table

Cover at least:

- separated boxes;
- distance just below/above tolerance;
- coplanar face contact;
- partial face contact;
- edge-only and point-only contact;
- shallow and deep penetration;
- full containment without surface crossing;
- nested zones;
- open, non-manifold and degenerate meshes;
- thin elements smaller than a candidate voxel size;
- rotated elements and large/georeferenced coordinates;
- duplicated/instanced meshes.

### IFC semantic fixtures

- physical, virtual, internal and external boundaries;
- first- and second-level boundaries;
- corresponding and parent/inner boundaries;
- wall/opening/door and wall/opening/window chains;
- L/T path connections with start/end/path types;
- one stair/stair-flight assembly incident with spaces on two or more storeys, including IFC
  containment under only one storey;
- ordered storeys, equal-elevation split levels, overlapping vertical extents and missing elevation;
- dangling references and conflicting evidence;
- the same long wall bounding multiple non-adjacent spaces.

### RDF invariants

- no `adjacentElement` or `intersectingElement` with an element subject;
- no AABB-only published relation;
- `Interface` nodes have at least two `interfaceOf` links for pair connections;
- no self-loops;
- stable output and IRIs across repeated runs;
- all emitted BOT predicates exist in canonical BOT;
- zone/element/interface disjointness is respected;
- symmetric relationships are emitted consistently;
- storey vertical-order edges are building-scoped and acyclic;
- a multi-storey stair is reachable through its lower and upper interfaces without asserting a
  fabricated direct zone adjacency.

### Performance gates

- candidate generation is sub-quadratic on multi-thousand-element models;
- candidate count and reduction ratio are reported;
- exact checks are batched and bounded by configured memory limits;
- graph reachability runs over compact incidence data without revisiting geometry;
- no unbounded mesh cache in WASM;
- no silent fallback from exact analysis to asserted AABB topology;
- optional failures become `Unknown` diagnostics, not false facts.

### Baseline status

The current topology tests could not be executed in this environment during planning:

1. the repository `target/` directory is root-owned;
2. an isolated `CARGO_TARGET_DIR` passed that obstacle but the environment has no `cc` linker.

This is an environment blocker, not a recorded test failure. Install/configure a C linker and run:

```bash
CARGO_TARGET_DIR=/tmp/ifc2lbd-topology-target \
  cargo test -p lbd-topology -p lbd-geometry
```

before implementation begins.

## 12. Ontology requirement register

This register describes the concepts the result needs without preselecting a vocabulary merely to
fill a gap. Terms move into emitted RDF only after their ontology, version, license, maintenance and
semantic fit have been reviewed.

| Required concept | BOT coverage | Candidate standard | Decision |
|---|---|---|---|
| Building zones, elements, containment, adjacency, intersection and qualified interfaces | Native | BOT | Adopt |
| Product kind such as wall, door, stair and stair flight | None beyond generic `Element` | Existing BEO output; compare SAREF4BLDG only if broader device/building semantics are needed | BEO already sufficient for the first target query |
| Geometric `touches`, `within`, `contains`, `overlaps`, `crosses`, `disjoint` and `intersects` | BOT has only its building-oriented subset | [OGC GeoSPARQL](https://docs.ogc.org/is/22-047r1/22-047r1.html), whose Simple Features relation family is based on DE-9IM | Adopt only for relations whose geometries and exact semantics are available; do not equate every `sfTouches` result with a useful BOT interface |
| Geometry representation and coordinate reference system | Geometry hooks only | Existing OMG/FOG output plus GeoSPARQL where appropriate | Reuse existing modules |
| IFC boundary level, physical/virtual flag, internal/external flag, corresponding boundary, connection type and layer priorities | None | IFC schema / ifcOWL is the faithful source | Keep internal when full ifcOWL is disabled; search for a maintained narrower alignment before exposing it |
| Navigable cell, passable/non-passable boundary, navigation state/node, directed transition/edge, transfer space and route | None | [OGC IndoorGML 2.0 conceptual model](https://docs.ogc.org/is/22-045r5/22-045r5.html) | Strong conceptual match; do not emit guessed RDF IRIs. Verify an official or maintained RDF encoding first |
| Entrance/exit role | None | IndoorGML `NavigableBoundary` covers passage but the exact entrance/exit classification still needs vocabulary review | Open ontology-selection item; an exterior door is not automatically the intended public entrance |
| Agent-dependent passability and accessibility: width, headroom, slope, steps, wheelchair suitability, permissions and temporal closure | None | Search accessibility, indoor-navigation and access-control ontologies; quantities/units may reuse QUDT | Open; keep computed constraints internal until selected |
| Distance, duration, effort, risk and other route costs | None | IndoorGML supplies route/network concepts, but cost semantics require a verified vocabulary | Open |
| Derivation activity, input, software agent, source and time | None | [W3C PROV-O](https://www.w3.org/TR/prov-o/) | Adopt for provenance when an evidence graph is approved |
| Dataset/linkset precision, accuracy and quality measurements | None | [W3C DQV](https://www.w3.org/TR/vocab-dqv/) with a recognised unit vocabulary | Evaluate for aggregate output quality; do not misuse it as per-triple confidence without a reviewed model |
| Algorithm tolerance, contact area and per-fact confidence | None | No choice yet; QUDT can supply units but not the missing relationship semantics | Open; internal only, no custom RDF terms |

The [W3C LBD ontology catalogue](https://github.com/w3c-lbd-cg/ontologies) is the first search
index for building-domain candidates. TopologicPy's vocabulary can also be evaluated, especially
its interface terms, but implementation availability does not by itself establish ontology
maturity or interoperability.

### Ontology-selection gate

For every non-BOT term, record:

1. the exact canonical IRI and ontology version;
2. its stated domain, range and intended meaning;
3. whether it represents an authored fact, physical relation, inferred fact or route state;
4. maintenance/governance and license;
5. overlap or conflict with BOT, BEO, GeoSPARQL and existing repository output;
6. one competency question and one counterexample that the term must handle.

If no suitable term passes this gate, the concept remains in the Rust evidence/analysis model
and is not serialized. This is preferable to minting a plausible-looking namespace.

## 13. Query capability and design refinement

### Physical topology and graph reachability

The proposed A-F stages produce a general physical incidence graph. Topological reachability is a
core competency and does not imply human navigation or passability. A graph search may traverse:

- zone-to-zone `bot:adjacentZone` edges;
- zone-to-element `bot:adjacentElement` and `bot:intersectingElement` edges;
- `bot:Interface` nodes and their `bot:interfaceOf` incidence;
- element-to-element interfaces, including authored path connections;
- the same stair or stair assembly incident with spaces on different storeys.

The RDF predicates retain their BOT directions, while the internal incidence graph can expose the
corresponding reverse lookup for graph algorithms. Consumers choose the allowed node/edge kinds. A
pure topology query can cross a wall or any other shared element if requested; a navigation,
accessibility, services-routing or structural consumer may impose additional rules later. None of
those application rules is a prerequisite for the topology model.

For a multi-storey stair, represent both ends explicitly when evidence exists:

```turtle
:lower_stair_interface a bot:Interface ;
    bot:interfaceOf :lower_hall, :stair_1 .

:upper_stair_interface a bot:Interface ;
    bot:interfaceOf :upper_hall, :stair_1 .
```

If IFC assigns the stair to only one storey, use stair/stair-flight aggregation plus world-space
intersection or boundary evidence to find every incident space. Do not require a false direct
`adjacentZone` assertion between the lower and upper spaces; graph reachability through the shared
stair and interfaces is sufficient.

### Performance and precision refinements

The staged design is appropriate if implemented as an evidence-first cascade rather than a full
all-pairs geometry analysis:

1. Accept valid authored space boundaries, corresponding-boundary pairs and void/fill chains
   first. Geometry validation of these facts should be an optional audit mode, not mandatory work.
2. Partition candidates by storey, containment and meaningful class pair before querying an
   R-tree. Maintain zone-zone, zone-element and narrowly selected element-element query paths
   instead of colliding every mesh with every other mesh.
3. Tessellate each entity once. Reuse immutable Parry triangle-mesh acceleration structures and
   instance transforms; use a bounded cache for large/WASM models.
4. Use early exits: semantic fact, then AABB candidate, then distance/contact, then expensive
   containment or boundary-patch analysis. Stop as soon as the requested competency question has
   enough evidence.
5. Treat boundary surfaces as first-class inputs. A valid `IfcRelSpaceBoundary` surface is usually
   more precise and cheaper than recovering the same interface from two complete solids.
6. Use exact classification only for missing, conflicting or validation cases. Keep open or
   non-manifold meshes as `Unknown` rather than forcing a result.
7. Keep voxels as a sparse, resolution-declared fallback for space reconstruction or unresolved
   free space. They are not the default route planner or adjacency detector.
8. Run reachability over the compact topology/incidence graph, not meshes or voxels. Geometry
   establishes missing edges; it is not the graph-search data structure.

A non-manifold cell-complex/B-rep approach like Topologic is attractive because shared faces are
explicit, but robustly constructing that complex from dirty IFC meshes requires substantially more
repair and boolean machinery than Rust/Parry supplies. The hybrid relationship + boundary-surface
+ R-tree + conservative Parry approach is the better first implementation. TopologicPy and
IfcOpenShell should remain comparison oracles on the test corpus.

### Target query 1: walls adjacent to rooms containing stairs

This becomes answerable with BOT plus the already emitted BEO types, provided the converter can
establish that the stair is contained in or intersects the room. A representative query is:

```sparql
SELECT DISTINCT ?wall ?room ?stair WHERE {
  ?room a bot:Space ;
        bot:adjacentElement ?wall .
  ?room (bot:containsElement|bot:intersectingElement) ?stair .
  ?wall a beo:Wall .
  ?stair a beo:Stair .
}
```

The caveat matters: many IFC files contain a stair only at storey level, and a stair may span or
intersect several spaces. The geometry stage must then establish room/stair containment or
intersection. The query should say whether "in" means fully contained, intersecting or assigned by
IFC containment.

### Target query 2: topological path across spaces and storeys

Yes. Search the heterogeneous incidence graph rather than only `adjacentZone`. A result may be:

```text
bathroom -> hallway -> lower stair interface -> stair
         -> upper stair interface -> upper hall -> target room
```

For a simple shared-stair query:

```sparql
SELECT ?lower ?stair ?upper WHERE {
  ?lower bot:adjacentElement ?stair .
  ?upper bot:adjacentElement ?stair .
  ?stair a beo:Stair .
  FILTER (?lower != ?upper)
}
```

The core graph API should support breadth-first search and weighted/unweighted shortest paths over
a caller-selected edge mask. `petgraph` becomes justified if it simplifies these algorithms without
duplicating the stable RDF identity model.

### Storey vertical order

BOT relates a building to its storeys but does not order them. Establish a building-scoped vertical
partial order from, in priority order:

1. `IfcBuildingStorey.Elevation` and related reference-elevation fields;
2. world-space storey bounds;
3. contained-space/element bounds as a reported fallback when the storey has no geometry.

Use the SPOT asset Z axis as the reference orientation when available. RELOC can then serialize an
existing-vocabulary relation:

```turtle
:second_storey reloc:disjointTop :first_storey .
:first_storey  reloc:disjointTop :ground_storey .
```

Use `reloc:meetTop` only when the represented regions actually meet. RELOC does not define a
dedicated `immediatelyAboveStorey` term, so store only a semantically true immediate chain (or all
true pairs when explicitly configured) and use SPARQL property paths for closure:

```sparql
# Every storey below the second storey
SELECT ?below WHERE {
  :second_storey reloc:disjointTop+ ?below .
}
```

Within one building, the topmost storey has no incoming `reloc:disjointTop` edge and the bottommost
has no outgoing one. Equal elevations, split levels, overlapping vertical intervals and missing
elevations must remain a partial order with multiple possible extrema or `Unknown`; never force an
arbitrary total sequence.

## 14. SPOT and SPOT-AM review

### Scope and fit

The [Space Types Ontology (SPOT)](https://design-computation.pages.git-ce.rwth-aachen.de/spot/)
models reference spaces for heterogeneous resources. Its `AssetSpace`, `DocumentSpace`,
`EntitySpace`, dimensional space classes, axes and reified `Appearance` relation answer a different
question from BOT:

- BOT: what building zones/elements contain, touch or intersect one another?
- SPOT: in which physical or virtual reference space does a resource representation appear?
- SPOT-AM: how are the axes and boundary points of that representation qualitatively mapped to the
  reference space?

This is a valuable complement for aligning an IFC model with drawings, scans, photographs, sensor
positions or other models. It does not replace BOT topology, OMG/FOG geometry, a CRS vocabulary or
the converter's numeric transformation matrices.

Do not automatically assert that a `bot:Space` is a `spot:Space`. BOT's class represents a building
zone, while SPOT's class represents a physical/virtual reference space. Likewise, do not mirror the
IFC site/building/storey/room hierarchy into `spot:hasSubspace` without a reviewed alignment: that
property concerns nested reference spaces, not merely spatial decomposition of the building.

### Recommended adoption status

| Part | Recommendation | Reason |
|---|---|---|
| SPOT core | Prototype as an optional independent producer | It fills the cross-resource spatial-reference gap and has stable `w3id` IRIs, version 1.0.0 and CC BY 4.0 licensing, although its documentation still calls it a draft |
| SPOT-AM | Prototype behind an experimental flag; do not promise production semantics yet | Useful qualitative axis/extent mapping, but the published 1.0.0 TTL contains issues that require clarification |
| RELOC relations used by SPOT-AM | Treat as a reviewed dependency, not an incidental namespace | SPOT-AM refers to `reloc:directionalRelation`; emitted specialized relations need the exact RELOC version and semantics |
| Numeric placements/transforms | Keep in the existing geometry/reference model | SPOT-AM cannot faithfully encode arbitrary numeric affine transforms by itself |

SPOT is most useful when the converter or downstream platform handles more than one resource. For
a single IFC converted in isolation, emitting axes and appearances for every object can add many
triples with little query value. Activation should therefore be explicit and scope-configurable:
whole model only, selected spatial resources, or per-entity representations.

### Questions and defects to resolve upstream

The downloaded version 1.0.0 serializations reveal several points that must be answered before a
production module is enabled by default:

1. SPOT's TTL defines `https://w3id.org/spot#appearsIn`, while SPOT-AM declares and uses the
   case-distinct `https://w3id.org/spot#AppearsIn`. RDF IRIs are case-sensitive.
2. The SPOT-AM property chain `hasAxisMapping o hasTargetAxis` terminates in an `Axis`, while the
   core `appearsIn` property has range `Space`. If the upper-case property was intended to be the
   core property, this chain appears type-incompatible.
3. SPOT-AM uses `reloc:directionalRelation` but its TTL does not declare an `owl:imports` for RELOC.
4. The core TTL includes `LineSpace` and `isInverseDirectionOf`, but they are absent from the
   generated HTML overview. Establish which serialization is normative.
5. There is no evident numeric coordinate, rotation-angle or complete matrix representation.
   `Codirectional`/`Antiparallel` plus `Exact`/`Approximate`/`Broad` are qualitative. Confirm the
   intended representation of arbitrary rotations, translations, scale and georeferencing.
6. Clarify what resource should carry `EntitySpace`: an entire IFC model, an IFC representation,
   an individual product, or a separate spatial-representation resource. The ontology currently
   provides no obvious general property linking an external resource to its `EntitySpace`; do not
   invent one or dual-type every wall as a space without guidance.
7. Clarify boundary-point mapping targets and whether accuracy also applies to
   `BoundaryPointMapping`, since the current `hasAccuracy` domain is only `AxisMapping`.

Open these as ontology-author questions and pin the reviewed ontology versions in tests. Until
resolved, the prototype must emit only the unambiguous subset.

### Converter architecture

SPOT should not be added to `neo-bot-producer`. Use independent modules with a shared typed model:

```text
IFC placements + coordination operation + optional tessellated extents
                              │
                              ▼
                 SpatialReferenceModel preprocess
                         │              │
                         ▼              ▼
                SPOT core producer   SPOT-AM producer
                  graph: spot         graph: spot-am
```

Provisional design, with plugin IDs left unpublished until the prototype is accepted:

1. Add a small `spatial-reference-model` core crate holding reference spaces, representation
   extents, local/world transforms, axes, dimensionality, source identity and derivation quality.
2. Add a preprocess plugin copied from `plugin-template-preprocess`. It decodes IFC local-placement
   chains and coordinate operations once. When `TessellatedModel` is available it also calculates
   local/world extents; it must use `ctx.insert` only for the new model and `ctx.replace` for later
   enrichment.
3. Add a SPOT producer copied from `plugin-template-producer`. It emits only SPOT core resources
   into its own named graph and is optional.
4. Add SPOT-AM as a separate producer because it has additional ontology dependencies, geometry
   needs and maturity risk. Its activation requires the shared preprocess and SPOT core producer,
   but it reads the shared `SpatialReferenceModel`; it never consumes another producer's output.
5. Register accepted plugins in both CLI and WASM. Keep both pure Rust and bounded; no ontology
   download occurs during conversion.
6. Vendor a small reviewed term/IRI manifest or generated constants with ontology version and
   checksums. Do not silently follow a changing remote ontology at runtime.
7. Use stable instance IRIs derived from source-resource identity and IFC GUID/representation ID,
   not anonymous nodes where cross-file linking is expected.

The SPOT core producer may work from parsed placements alone. SPOT-AM boundary-point output needs
an extent, normally the local bounding box from `TessellatedModel`; missing geometry must cause that
mapping to be skipped with a diagnostic, not approximated from IFC names or containment.

### Mapping experiment

The first experiment should be model-level, not one SPOT graph structure per triangle or topology
patch:

1. Represent the physical asset reference frame as `spot:AssetSpace` only after deciding which IFC
   resource denotes that asset and how the two are linked.
2. Represent the IFC file/model reference frame according to the authors' clarified
   `DocumentSpace`/`EntitySpace` pattern.
3. Create X/Y/Z axes and their declared directions for the reference space.
4. Reify placement in an `Appearance` and link it with `spot:hasAppearance` / `spot:appearsIn`.
5. Derive source-axis vectors from the existing f64 world transform. Emit SPOT-AM
   `Codirectional`/`Antiparallel` only when the dot-product test meets a documented tolerance; do
   not force an arbitrary rotated axis into the closest named direction.
6. Derive min/max boundary points from local-space bounds, transform them to the reference space,
   and emit only relations explicitly supported by the reviewed SPOT-AM/RELOC contract.
7. Preserve the complete f64 matrix and geometry through the existing geometry path because the
   qualitative triples are intentionally lossy.

Test identity, 90-degree rotation, arbitrary rotation, reflection, large/georeferenced translation,
scaled coordinate operation, missing geometry and a resource with multiple appearances. Add an OWL
reasoning test that detects unintended inferred `spot:Space`/`spot:Axis` types and a triple-budget
test for per-entity mode.

### Instance identity and cross-ontology joins

Connect modules by reusing stable instance IRIs for things that are genuinely identical, and use
established properties between distinct things. Named-graph separation does not prevent SPARQL
joins over the same IRI.

The repository already provides the required identity spine:

- BOT and BEO use the same IFC-GUID-derived IRI for the physical entity;
- OMG links that entity with `omg:hasGeometry` to the deterministic
  `geometry_<safe-guid>` resource;
- the geometry sidecar uses the same physical entity IRI for picking/linking.

The preferred candidate alignment is to type the existing geometry/representation node as the
appropriate SPOT entity space, subject to confirmation with the SPOT authors that an OMG geometry
resource is the intended carrier of `spot:EntitySpace`:

```turtle
:wall_123
    a bot:Element, beo:Wall ;
    omg:hasGeometry :geometry_123 .

:geometry_123
    a omg:Geometry, spot:VolumeSpace ;
    spot:hasAppearance :appearance_123_asset .

:appearance_123_asset
    a spot:AssetAppearance ;
    spot:appearsIn :asset_reference_space .

:asset_reference_space
    a spot:AssetSpace ;
    spot:hasXAxis :asset_x ;
    spot:hasYAxis :asset_y ;
    spot:hasZAxis :asset_z .
```

The same pattern applies to a room: the room resource remains `bot:Space`, while its geometry or
spatial representation can be a `spot:VolumeSpace`. Do not type the wall or room itself as a SPOT
space merely to make a join.

SPOT-AM extends the appearance and axis resources rather than creating another copy of the wall:

```turtle
:appearance_123_asset
    spot-am:hasAxisMapping :axis_mapping_123_x .

:axis_mapping_123_x
    a spot-am:AxisMapping ;
    spot-am:hasSourceAxis :geometry_123_x ;
    spot-am:hasTargetAxis :asset_x ;
    spot-am:hasAxisRelationType spot-am:Codirectional ;
    spot-am:hasAccuracy spot-am:Exact .
```

RELOC facts about stable object-relative relationships should normally use the physical entity
IRIs, so type filters and topology joins remain direct:

```turtle
:column_456
    a beo:Column ;
    reloc:disjointLeft :column_123 .
```

Only emit that persistent triple when the reference frame is stable and explicit. A viewer-relative
version belongs in an ephemeral query result or view-specific named graph and must be recomputed
from the view transform.

Rules:

1. Never connect the physical object and its representation with `owl:sameAs`; they are distinct.
2. Do not mint a custom `hasSpatialRepresentation` predicate. Reuse `omg:hasGeometry` if the SPOT
   carrier alignment is confirmed.
3. Do not duplicate physical resources per named graph. Every producer calls the same canonical IRI
   helpers.
4. The SPOT producer should require/activate the OMG structural-link producer for per-entity mode,
   but it must not consume that producer's output; both independently calculate the same stable
   geometry IRI.
5. Keep the `spot:AssetSpace` distinct from `dicp:ConstructionProject` and `bot:Building` until a
   verified alignment property is selected. It represents the asset reference space, not simply the
   administrative project or building zone.
6. Stable appearance, axis and mapping IRIs derive from source resource ID, target reference-space
   ID and axis role. They must not depend on emission order or blank-node allocation.

If the ontology authors say an OMG geometry must not also be a SPOT `EntitySpace`, create a separate
SPOT entity-space node. Do not invent the bridge to it: select a maintained resource/representation
ontology property with the authors, or postpone that finer-grained output.

### Competency questions enabled

SPOT/SPOT-AM can support questions such as:

- which models, drawings or scans appear in the same asset reference space?
- which representation is located in the front/right/top region of another reference space?
- which resources use aligned or reversed axes?
- which entity-space appearance should be transformed when overlaying a model and a document?

They do not directly answer wall adjacency, material composition or navigability. Those remain
separate topology, material and downstream-analysis concerns.

### RELOC and view-relative interaction

The [Relative Location Ontology (RELOC)](https://annegoebels.github.io/reloc/) is a useful
vocabulary for natural-language directional/topological statements. Version 1.0.1 combines three
oriented axes—front/centre/rear, left/centre/right and bottom/centre/top—with 2D relations such as
disjoint, meet, intersect, contained-in and equal. For example, `reloc:disjointLeft` means left and
separate, while `reloc:meetLeft` means directly touching at the left side.

This can support an instruction such as "highlight the column left of the highlighted column", but
the reference frame must be explicit. The RELOC paper defines directions relative to the oriented
target/reference object. A viewer-relative command instead depends on the current camera axes; if
the viewer rotates 180 degrees, left and right reverse. Therefore view-relative relations must not
be stored as timeless model facts.

Recommended split:

| Relation kind | When calculated | Persistence |
|---|---|---|
| Asset-relative: north/east, façade front/rear, local element axes | Conversion or indexed query | May be emitted as stable RELOC facts when the reference frame is explicit and reviewed |
| Object-relative: left face of a beam, damage in the lower-left region of a column | Conversion or authoring/query time | Suitable RELOC use; direction is defined by the target object's oriented axes |
| View-relative: object visually left/right/above another in the current viewport | Interaction time from camera/view/projection matrix | Ephemeral result; do not place in the persistent building graph by default |

The interactive algorithm should be geometry-backed rather than an RDF all-pairs scan:

1. The client sends the selected entity ID, camera/view matrix, projection matrix and viewport.
2. Obtain candidate entities from the existing scene/R-tree index and filter semantic type, e.g.
   `beo:Column`.
3. Transform candidate bounding boxes—or projected visible geometry for higher precision—into
   screen coordinates.
4. Define strict screen-left using extents, for example candidate `max_x` less than selected
   `min_x` minus a pixel tolerance. A looser mode can compare projected centres.
5. Rank candidates by horizontal distance, with vertical separation and depth as penalties, so
   "the column left of" returns the visually closest plausible column rather than every column in
   the left half-plane.
6. Apply frustum and optional occlusion checks; otherwise a hidden column behind the selection can
   win.
7. Return entity IDs plus the interpreted relation, which can be labelled
   `reloc:disjointLeft`/`reloc:meetLeft` where its semantics actually match.

Use projected extents rather than centroids for large or irregular objects. Also make ambiguity
visible: if two columns score almost equally, return candidates or ask the user rather than silently
choosing one.

SPOT can describe a saved view/reference space and its axes, so a future persisted scene annotation
could combine SPOT appearances with RELOC. The current ontologies do not yet make the qualification
of a RELOC triple by a particular camera pose completely clear. Until this pattern is confirmed
with the authors, keep camera-relative relations in a typed `ViewContext`/query result or a
short-lived named graph rather than inventing a predicate.

No converter producer is required for the interactive use case. Implement a reusable
`relative-spatial-query` library/service over the converter's stable entity IDs, BEO types and
world-space geometry index. An optional RELOC producer is justified only for a configured,
bounded set of stable asset/object-relative relations; it must not materialise left/right for every
entity pair.

RELOC also needs a small maturity review before broad emission. Its offset node currently carries a
numeric value and string unit but the published model does not clearly connect that node to both a
target and a directional relation; use QUDT-compatible numeric distance data elsewhere until the
intended pattern is confirmed.

## 15. Immediate next actions

1. Agree on the IFC-to-BOT mapping contract, especially edge/point contact and whether every
   authored path connection becomes a BOT interface.
2. Send the SPOT/SPOT-AM questions above to the ontology maintainers and build a small hand-authored
   expected RDF fixture from their clarified pattern.
3. Implement Phase 0 and the missing typed IFC relationship fields.
4. Fix relation-only topology and its RDF invariants before touching exact geometry.
5. Implement the Topologic-inspired Tier 1 incidence, shell-quality and boundary-patch model while
   building the `TessellatedModel` adapter and R-tree candidate report.
6. Run the Parry f32/f64 truth-table experiment.
7. Decide whether Tier 2 polygon imprinting and cell-shell reconstruction is needed.
8. Decide from evidence whether voxel fallback is needed.
9. Treat navigation or any other application graph as a separate follow-up consumer, not a gate for
   physical topology.
10. Prototype the shared `SpatialReferenceModel` and SPOT core output independently of BOT.
11. Add a small viewer-relative relation proof using two/three columns, projected extents and camera
    rotation; verify that a 180-degree rotation reverses left/right without changing persistent RDF.
12. Only then design the final runner/plugin activation change.
