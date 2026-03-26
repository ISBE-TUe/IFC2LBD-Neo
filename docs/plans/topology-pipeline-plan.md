# Topology Pipeline Plan

## Goal

Build a valid, fast, and modular topology pipeline for `ifc2lbd-neo` that:

- emits BOT-core topology by default
- supports richer topology semantics through an explicit extension layer
- preserves converter stability by keeping topology derivation separate from RDF emission
- creates a path toward the semantic depth of TopologicPy without forcing graph-analysis concepts into BOT
- tracks query-driven targets in `docs/archive/topology-query-goals.md`
- tracks exact-geometry implementation in `docs/plans/geometry-kernel-plan.md`

## Design Principles

- BOT is the interoperable default semantic surface.
- Derived topology must be traceable back to IFC relations or geometry rules.
- Core conversion logic must not be tightly coupled to topology experimentation.
- Geometry enrichment must be optional and off the hot path unless explicitly requested.
- Extension terms should only be used where BOT is too weak or too abstract.

## Semantic Split

### BOT core

The default topology path should emit only ontology-valid BOT relations:

- `bot:hasSite`
- `bot:hasBuilding`
- `bot:hasStorey`
- `bot:hasSpace`
- `bot:containsElement`
- `bot:adjacentElement`
- `bot:hasSubElement`
- later: `bot:intersectingElement`
- later: `bot:Interface`
- later: `bot:interfaceOf`

### Extension layer

The extension layer should carry quantified, derived, or evidence-based topology that BOT should not model directly.

Proposed extension terms:

- `topo:Adjacency`
- `topo:adjacencyType`
- `topo:derivedFrom`
- `topo:confidence`
- `topo:distance`
- `topo:overlapArea`
- `topo:sharedBoundaryArea`
- `topo:touches`
- `topo:withinTolerance`

Deliberately excluded:

- `topo:graphNode`
- `topo:graphEdge`

These are implementation artifacts rather than stable domain semantics.

## Pipeline Architecture

### Stage 1: IFC-native topology extraction

Build an internal topology graph from IFC relationships only:

- `IfcRelAggregates`
- `IfcRelContainedInSpatialStructure`
- `IfcRelSpaceBoundary`
- `IfcRelVoidsElement`
- `IfcRelFillsElement`

Output:

- deterministic internal `TopologyGraph`

This stage must stay fast and geometry-free.

### Stage 2: BOT projection

Convert internal topology edges to BOT triples.

This is the stable default output path.

### Stage 3: Geometry enrichment

Add optional geometry-derived evidence:

- bounding boxes
- candidate zone adjacency
- candidate element intersection and touching
- candidate interfaces

The geometry stage should enrich the internal graph first, then project to BOT core plus extension triples.

### Stage 4: Topology analytics

Optional graph-style analysis can be built on top of the internal graph:

- connectivity
- shortest paths
- clustering/community
- accessibility or routing

These should remain downstream capabilities rather than BOT-core semantics.

## Implementation Plan

### Step 1: Refactor `lbd-topology` into explicit core and extension layers

Add an internal graph model with:

- node kinds: `Project`, `Site`, `Building`, `Storey`, `Space`, `Element`, later `Interface`
- edge kinds: `ContainsZone`, `ContainsElement`, `AdjacentElement`, `HasSubElement`, later `IntersectingElement`, `InterfaceOf`
- optional evidence metadata on edges

Keep compatibility accessors so current converter output does not change.

### Step 2: Route converter BOT topology through `lbd-topology`

The converter should consume the graph builder instead of mixing derivation and emission logic.

This isolates topology policy from RDF serialization policy.

### Step 3: Introduce extension vocabulary support

Add extension namespace constants and RDF helpers only after the internal graph model is stable.

Do not emit extension triples by default yet.

Status (2026-03-17):

- done: `topo:` namespace + prefix support in ontology/serializer path
- done: `--topology-extension` flag in CLI and converter option wiring
- done: first relation-evidence enricher seam in `lbd-topology` (currently no-op)
  - extension remains reserved for non-BOT semantics
  - IFC `IfcRelAggregates` zone containment is emitted as BOT core `bot:containsZone`
- done: BOT zone adjacency enrichment from IFC space boundaries
  - `bot:adjacentZone` inferred between spaces sharing boundary elements
- done: semantic element intersection enrichment from opening/filling relations
  - `bot:intersectingElement` inferred from `IfcRelVoidsElement + IfcRelFillsElement`
- default remains unchanged: extension triples are still off unless explicitly enabled

### Step 4: Add geometry hooks

Prepare interfaces for geometry enrichment:

- `GeometryProvider`
- `BoundingBoxProvider`
- topology enrichment entrypoint that decorates an existing `TopologyGraph`

No runtime dependency on IfcOpenShell should be introduced into the core conversion path at this step.

Status (2026-03-17):

- done: `lbd-geometry` now exposes:
  - `BoundingBoxProvider`
  - `GeometryProvider`
  - `GeometryRelation` / `GeometryRelationKind`
- done: topology enrichment entrypoint added:
  - `enrich_topology_with_geometry(model, &mut TopologyGraph, provider)`
  - only decorates `extension_edges` (core BOT edges unchanged)
- done: helper `collect_bounding_boxes(...)` for spatial+element bbox harvesting
- done: crate tests with mock providers verifying:
  - geometry enrichment inserts extension edges without mutating `core_edges`
  - bbox collection over model entities
- done: bbox-driven geometry relation derivation utility
  - `derive_relations_from_bounding_boxes(...)` emits candidate `AdjacentElement` / `IntersectingElement`
  - `BboxGeometryProvider` wraps a `BoundingBoxProvider` into the generic `GeometryProvider` interface
- still pending: wiring a concrete geometry backend (IfcOpenShell or equivalent) behind these hooks
- done: converter/CLI wiring for bbox-driven geometry enrichment path
  - `ifc2lbd-neo --geometry-bboxes-file <json> [--geometry-tolerance <f64>]`
  - geometry-derived `AdjacentElement` / `IntersectingElement` are merged into BOT core emission (no `topo:*` duplication)
  - topology split mode still works; geometry-enriched BOT triples land in the topology output file
- done: exact-kernel command integration path
  - `ifc2lbd-neo --geometry-bboxes-file <json> --exact-kernel-bin <cmd> [--exact-kernel-arg ...]`
  - broad phase stays bbox-based; narrow phase calls the external exact kernel per candidate pair
  - exact-kernel relations are merged into BOT core topology output

### Step 5: Add `bot:Interface`

Only implement `bot:Interface` once geometry-derived interfaces are stable and testable.

## Source Mapping

### What to port from local IFC2BOT

Valid and useful semantics to retain:

- zone classification
- zone hierarchy
- `bot:containsElement`
- `bot:adjacentElement`
- `bot:hasSubElement`

Do not port `storey_element.py` as-is because its emitted relation is not a valid BOT interpretation.

## Validation Fixtures

Keep topology development constrained to these fixtures:

- `Duplex.ifc`
- `IFC_SKW_Modell_07052019.ifc`
- `Infra-Bridge.ifc`

## Acceptance Criteria

- default topology remains on by default
- default topology path remains fast on the allowed fixtures
- converter behavior remains stable while topology internals evolve
- topology derivation is isolated in `lbd-topology`
- extension terms are optional and evidence-backed
- geometry-derived topology can be added without rewriting BOT-core emission
