# Topology Query Goals

## Target Questions

1. How do we get from an entry to a target space?
2. How much wall area is inside a certain room?

## Current Capability (2026-03-17)

- strong IFC-semantic topology baseline:
  - `bot:containsZone`
  - `bot:containsElement`
  - `bot:adjacentElement`
  - `bot:adjacentZone`
  - `bot:hasSubElement`
  - `bot:intersectingElement` (from opening/filling relations)
- optional extension seam for non-BOT semantics
- geometry hook interfaces available in `lbd-geometry` but no exact-kernel backend yet

## What Is Missing For Production-Grade Answers

For routing:

- passability semantics (which adjacency is traversable)
- interface semantics (`bot:Interface`) with connection evidence
- optional weights (distance/cost/access constraints)

For room-inside wall area:

- per-room boundary surfaces and robust room-to-surface attribution
- precise geometric intersection/area computation
- confidence/evidence metadata on geometry-derived values

## Implementation Path

### Phase A: Semantic-first improvements (no geometry kernel)

- keep BOT core stable and non-duplicated
- provide deterministic query-ready graph for route search

### Phase B: Geometry broad phase (fast candidates)

- use bounding boxes + spatial index to generate candidate touching/intersection pairs
- emit as extension evidence only (`topo:*`)
- avoid promoting candidates to BOT without narrow-phase confirmation

### Phase C: Exact geometry narrow phase

- integrate exact BRep/solid checks (OpenCascade-backed backend)
- compute:
  - exact intersection/touch
  - exact shared boundary area
  - room-internal wall-side area
- emit confidence + derivation metadata

### Phase D: Interface and query hardening

- model stable interfaces (`bot:Interface`, `bot:interfaceOf`) when evidence is robust
- freeze route-oriented and area-oriented query patterns
- benchmark on Duplex/SKW/Infra fixtures and track regressions

## Non-goals

- graph utility algorithms are not embedded in converter output logic
- downstream users can run shortest path/community/etc. externally on emitted graph data
