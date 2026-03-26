# Geometry Kernel Plan

## Goal

Provide precise, high-confidence geometry-derived topology enrichment while keeping conversion fast and modular.

Implementation details for the external adapter contract live in `docs/reference/geometry-kernel-adapter.md`.

## Principles

- BOT core stays clean and interoperable.
- Geometry facts are evidence-backed and deterministic.
- Broad-phase filtering is fast; narrow-phase checks are exact.
- Geometry backend is optional and isolated behind traits/interfaces.

## Architecture

### Stage A: Broad Phase (Rust-native)

- Build AABB index over candidate geometry (`rstar`).
- Generate candidate pairs for touch/intersection/interface.
- Keep this as a filter only, not final truth.

### Stage B: Narrow Phase (Exact Kernel)

- Run exact BRep/solid checks on candidate pairs via OpenCascade-backed backend.
- Derive robust facts:
  - intersecting
  - touching / within tolerance
  - shared boundary area
  - room-side wall/interface area

### Stage C: Topology Projection

- Promote robust facts to BOT where semantically valid:
  - e.g. `bot:intersectingElement`, later `bot:Interface`, `bot:interfaceOf`
- Emit quantified/evidence metadata via `topo:*` extension terms:
  - confidence, tolerance, derivedFrom, area, distance

## BOT Interface Strategy

- Doors remain elements (`bot:Element` + domain classing).
- Interfaces are explicit boundary entities when robustly identified.
- Do not conflate every door with an interface by default.
- Create `bot:Interface` only when geometry or high-quality IFC boundaries support it.

## Backend Strategy

1. Keep current trait seams (`GeometryProvider`, `BoundingBoxProvider`) as stable API.
2. Add OpenCascade-backed adapter as optional component.
3. Keep converter core independent from direct OCC runtime dependency.

## Performance Strategy

- Parallel broad-phase candidate generation.
- Batched narrow-phase checks.
- Geometry cache keyed by element id + representation hash.
- Deterministic sorting of emitted topology edges.

## Validation Gates

- Fixture scope: Duplex, SKW, Infra.
- For each gate:
  - precision/recall proxy on known relations
  - runtime and memory budget
  - deterministic output diff stability

## Delivery Sequence

1. Baseline broad-phase + wiring (done/ongoing).
2. Exact-kernel adapter skeleton + API contract. (done)
  - implemented in `lbd-geometry`:
    - `ExactGeometryKernel` trait
    - `ExactCheckOptions`
    - `ExactPairAnalysis` + `InterfaceEvidence` data contract
    - `derive_relations_with_exact_kernel(...)` deterministic adapter
3. First exact predicate: `intersectingElement`. (in progress)
  - CLI now supports an external exact kernel command:
    - `--exact-kernel-bin <path>`
    - requires `--geometry-bboxes-file` for broad-phase candidate generation
    - timeout/chunking are internal production defaults (not user-facing tuning knobs)
  - subprocess JSON stdin contract (per candidate pair):
    - request: `{ "ifc_path": "...", "left": 123, "right": 456, "tolerance": 1e-6 }`
    - response: `{ "intersects": bool, "touches_within_tolerance": bool, "minimum_distance": number|null, "interface": { "interface_id": id, "shared_boundary_area": number|null }|null }`
  - current converter behavior:
    - if exact-kernel output is available, those relations are used
    - otherwise bbox-only relations are used
  - strictness:
    - any kernel per-pair error fails conversion
    - incomplete batch response fails conversion
4. Interface extraction and `bot:Interface` projection.
5. Area-focused enrichment for room-inside wall/interface analytics.
