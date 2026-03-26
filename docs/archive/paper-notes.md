# IFC2LBD Rust Rewrite - Paper Notes

## Scope

This document tracks:

- what has been ported from Java IFCtoLBD to Rust
- where the Rust pipeline has been enhanced
- where we see near-term and mid-term potential
- current issues and limitations

## Porting Status (Java -> Rust)

- full end-to-end converter stack implemented in Rust:
  - `ifc-step` parser
  - `ifc-model` typed model + indexes
  - `ifc-schema` lookups/classification
  - `lbd-ontology` vocab and RDF helpers
  - `lbd-converter` LBD + IfcOWL generation
  - `lbd-serializer` streamed Turtle output
  - `ifc2lbd-cli` production CLI workflow
- broad Java IFCtoLBD behavior has been ported, including:
  - spatial/element hierarchy
  - property and quantity extraction
  - OPM state/value graph modeling
  - IfcOWL export with schema-aware predicates and EXPRESS/list materialization
  - `owl:sameAs` links
- topology semantics from IFC relations ported and expanded in Rust.

### IfcOWL Parity Note

- IfcOWL output is treated as a standardized target surface.
- Current parity status for Duplex in this project:
  - normalized comparison (`--normalize-ifcowl-scalars`) reports `result=identical`
  - strict byte/triple identity can still differ because of deterministic node-id/materialization choices
  - paper framing should state normalized semantic identity, not strict lexical identity

## Enhancements Implemented In Rust

- streamed conversion and serialization with bounded channels
- deterministic triple ordering for stable output
- topology sidecar split support (separate topology TTL)
- BOT topology depth increased in core output:
  - `bot:containsZone`
  - `bot:containsElement`
  - `bot:adjacentElement`
  - `bot:adjacentZone`
  - `bot:hasSubElement`
  - `bot:intersectingElement` (semantic relation-derived)
- geometry hook architecture added:
  - `BoundingBoxProvider`
  - `GeometryProvider`
  - exact-kernel adapter contracts in `lbd-geometry`

## Design Decision: Property Sets + Quantity Sets In LBD

Current behavior:

- properties and quantities are emitted in OPM-style state/value form (`props:*`, `opm:*`, `schema:value`), attached to object subjects.

Smarter target model (recommended):

1. Keep current OPM state graph for value-level interoperability and Java parity.
2. Add explicit LBD set resources and links:
   - `lbd:hasPropertySets` -> `lbd:PropertySet`
   - `lbd:hasQuantitySet` -> `lbd:ElementQuantitySet`
3. Link set resources to their contained properties/quantities and keep state/value nodes for actual measurements.

Why this is better:

- preserves existing query surface and parity
- adds clearer set-level semantics for tooling and documentation
- enables easier downstream analytics grouped by originating set

### OPM vs LBD Position

- Cleaner long-term model: dual-surface.
  - LBD set layer provides canonical grouping semantics.
  - OPM state layer provides value-state/time-oriented semantics and current parity behavior.
- Using only one loses value:
  - only OPM -> weak set-level discoverability
  - only LBD set links -> weaker value-state semantics

## Potential

Near-term:

- integrate exact geometry backend (OpenCascade-backed adapter) for robust interface and area semantics
- emit `bot:Interface` / `bot:interfaceOf` when evidence is strong
- complete dual-surface LBD set modeling for properties/quantities

Mid-term:

- geometry-backed quantity completion fallback (only when IFC quantities are missing)
- confidence/derivation metadata for all geometry-derived facts
- precise room-level wall/interface area analytics

## Current Issues

### Namespace availability

- As of 2026-03-17, `https://linkedbuildingdata.org/LBD#` was not reliably dereferenceable (observed `502` in local checks).
- Impact:
  - namespace use in RDF remains syntactically valid
  - but online ontology dereferencing and documentation access may fail for users/tools
- Mitigation direction:
  - keep namespace for compatibility
  - maintain local cached ontology references in repo
  - prefer persistent redirect patterns for new extension vocabularies

### Topology bugs T1 and T2 — FIXED (2026-03-17)

Both bugs are resolved. Topology output is now semantically correct for `Duplex.ifc` and `DigitalHub_FM-ARC_v2.ifc`.

- **T1 fixed**: `IntersectingElement` is no longer emitted from void/fill chains. Only `HasSubElement` is derived from `IfcRelVoidsElement + IfcRelFillsElement`.
- **T2 fixed**: bbox-derived geometry relations now go to `extension_edges` only. OCC exact-kernel confirmed relations are promoted to `core_edges`.
- **Additional**: `AdjacentElement` was incorrectly emitted by the OCC kernel for element-element touching — removed. Per BOT spec, `bot:adjacentElement` is Zone→Element only. Element-element touching → `bot:Interface` via `bot:interfaceOf`.
- **Additional**: IfcOWL class IRI bleed in LBD file fixed. Unmapped elements now get only `bot:Element`, never IfcOWL class IRIs.

### IFC4 space adjacency — structural gap, not a bug

`bot:adjacentElement` (Zone→Element) requires knowing which elements bound each space. In IFC2x3 models (Duplex), this is provided by `IfcRelSpaceBoundary` records. In many IFC4 models (e.g. DigitalHub), space boundaries are optional and often absent.

**What we can derive without space boundaries:**

- `bot:containsElement` and `bot:containsZone` — from containment/aggregation relations (always available)
- `bot:hasSubElement` — from void/fill chains
- `bot:Interface` / `bot:interfaceOf` / `bot:intersectingElement` — from OCC exact kernel on element-element pairs

**What requires space geometry to derive:**

- `bot:adjacentElement` — need to test which elements bound each space's volume
- `bot:adjacentZone` — need shared boundary elements between pairs of spaces

**How to rebuild it from geometry (future work):**

1. Build BRep shapes for `IfcSpace` entities — currently blocked because DigitalHub spaces use `IfcTriangulatedFaceSet` which requires extending chijin's C++ layer to build OCC shells from triangle lists.
2. Add space-element candidate pair generation: for each storey, pair each space against all elements in the same storey.
3. Test space-element OCC pairs: if an element's shape touches the space's shape, emit `bot:adjacentElement` (space → element).
4. From shared adjacent elements between two spaces, emit `bot:adjacentZone`.

**Room traversal without space boundaries:**
Element-level traversal (door-to-door, wall-sharing) is still possible via `bot:Interface` when the exact kernel is active. Without `IfcRelSpaceBoundary`, space-to-space graph traversal is not available from the current pipeline.

### IFC4 geometry kernel coverage — known gaps

The OCC kernel (`lbd-geometry-kernel`) handles the following IFC geometry types:

- `IfcExtrudedAreaSolid` ✓
- `IfcMappedItem` ✓
- `IfcBooleanClippingResult` ✓
- `IfcRectangleProfileDef` ✓
- `IfcArbitraryClosedProfileDef` with `IfcPolyline` ✓
- `IfcArbitraryClosedProfileDef` with `IfcIndexedPolyCurve` ✓ (fixed 2026-03-17)
- `IfcArbitraryProfileDefWithVoids` ✓ outer profile (voids ignored — acceptable for intersection detection)

Known gaps:

- `IfcTriangulatedFaceSet` / `IfcPolygonalFaceSet` — IFC4 tessellated BRep. Used by DigitalHub spaces and complex elements. Requires extending chijin to build OCC shells from triangle lists.
- `IfcFacetedBrep` — IFC2x3 faceted BRep (used in SKW). Same gap.
- `IfcCircleProfileDef` — circular cross-sections (columns, pipes).
- `IfcPolygonalBoundedHalfSpace` — clipping half-spaces for openings.

**Impact on DigitalHub (2026-03-17):** 662 / 957 elements have successfully built BRep shapes (69%). The remaining 31% use tessellated or unsupported geometry and silently fail — they are skipped from topology analysis.

### Quantity/Property set representation not yet dual-surface

- set-level `lbd:*Set` linkage is not yet fully emitted alongside OPM state graph

## Reproducibility Notes

- fixture focus for topology benchmarking:
  - Duplex
  - SKW
  - Infra
- benchmark and parity artifacts are tracked in `artifacts/benchmarks/` and `artifacts/topology/`
