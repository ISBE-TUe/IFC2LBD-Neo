# Execution Plan

## Phase 1: MVP - Core Conversion (Weeks 1-4)

**Goal**: Convert IFC to LBD TTL + IfcOWL TTL with owl:sameAs linking. No geometry yet. Already faster than Java.

### Week 1: `ifc-step` -- STEP Parser

- [x] Set up Cargo workspace with all crate directories
- [x] Implement STEP file header parser (FILE_SCHEMA detection for IFC2X3/IFC4/IFC4x3)
- [x] Implement memory-mapped file reading with `memmap2`
- [x] Implement line boundary scanner (find `#NNN=...;` entity lines)
- [x] Implement `StepValue` enum (Ref, String, Real, Int, Enum, Bool, Dollar, Star, List, TypedValue)
- [x] Implement `RawEntity` parser (entity name + argument list)
- [x] Handle multi-line entities (`;` continuation across lines)
- [x] Handle IFC Unicode escapes (`\X\HH`, `\X2\HHHH\X0\`)
- [x] Implement reference resolution pass (`HashMap<u64, RawEntity>`)
- [ ] Unit tests with Duplex_A IFC file fragments

**Current status (2026-03-13)**:

- `ifc-step` compiles and tests pass (`cargo test -p ifc-step`)
- Header parsing now extracts `FILE_SCHEMA` and `FILE_DESCRIPTION`
- File parsing uses `memmap2` in `parse_step_file`
- DATA parsing now uses a single-pass boundary scan over the DATA section to find complete entity spans
- The scanned entity spans are parsed in parallel with Rayon and then collected into the final `HashMap<EntityId, RawEntity>`
- Multiline entities and quoted semicolons are covered by parser tests

### Week 2: `ifc-model` + `ifc-schema` -- Domain Model

- [ ] Build IFC type hierarchy tables (IFC2X3, IFC4, IFC4x3) as compile-time lookup
- [ ] Define typed Rust structs for LBD-relevant entities (~50 types):
  - Spatial: IfcProject, IfcSite, IfcBuilding, IfcBuildingStorey, IfcSpace
  - Elements: IfcElement and all subtypes
  - Relationships: IfcRelAggregates, IfcRelContainedInSpatialStructure, IfcRelSpaceBoundary, IfcRelVoidsElement, IfcRelFillsElement, IfcRelDefinesByProperties
  - Properties: IfcPropertySet, IfcPropertySingleValue, IfcElementQuantity
  - Units: IfcUnitAssignment, IfcSIUnit
- [x] Implement entity construction from `RawEntity` -> typed structs
- [x] Build relationship indexes: `children_of`, `contained_in`, `guid_to_entity`
- [x] Port `GuidCompressor` (22-char IFC GUID -> UUID format)

**Current status (2026-03-13)**:

- `ifc-schema` now provides the first minimal lookup layer for spatial structure types and a curated first-pass element set
- `ifc-model` now builds a typed first-slice model from `StepFile`:
  - spatial nodes: project, site, building, storey, space
  - element nodes: first generic building-element subset
  - relationships: `IfcRelAggregates`, `IfcRelContainedInSpatialStructure`, `IfcRelDefinesByProperties`
  - properties: `IfcPropertySet`, `IfcPropertySingleValue`, `IfcElementQuantity`, first quantity entities
  - units: `IfcUnitAssignment`, `IfcSIUnit`, `IfcConversionBasedUnit`
  - GUID utilities: IFC compressed GUID <-> UUID string round-trip helpers
  - indexes: `children_of`, `contained_in`, `guid_to_entity`, `property_sets_for_object`, `quantities_for_object`
- This is verified against the real `Duplex.ifc` fixture
- Remaining Week 2 work is substantial:
  - broaden schema/type tables
  - support more relationship/entity coverage
  - reduce the current dependence on heuristic first-pass entity classification

### Week 3: `lbd-ontology` + `lbd-converter` -- Conversion Engine

- [x] Define namespace constants: BOT, PROPS, OPM, PRODUCT, SMLS, UNIT, GEO, LBD, IfcOWL
- [x] Implement BOT hierarchy generation (Site -> Building -> Storey -> Space -> Element)
- [x] Implement product type mapping (IfcWall -> beo:Wall, etc.)
- [ ] Implement property set and quantity set extraction at OPM levels 1/2/3
- [x] Implement unit resolution (IfcUnitAssignment -> unit triples)
- [x] Implement first-pass IfcOWL serialization pass (direct STEP-to-IfcOWL)
- [x] Implement owl:sameAs triple generation (LBD URI <-> IfcOWL URI)

**Current status (2026-03-13)**:

- `lbd-ontology` now exposes prefix/namespace constants and lightweight RDF triple types
- `lbd-converter` now emits a first usable LBD slice:
  - `lbd:Project`, `bot:Site`, `bot:Building`, `bot:Storey`, `bot:Space`, `bot:Element`
  - BOT hierarchy edges: `bot:hasSite`, `bot:hasBuilding`, `bot:hasStorey`, `bot:hasSpace`
  - containment edges: `bot:containsElement`
  - first-pass product classes such as `beo:Wall`, `beo:Door`, `beo:Window`, `beo:Slab`
  - Java-style Level 3 OPM property/state resources for parsed property sets and quantity sets:
    - `props:*` as `owl:ObjectProperty`
    - property resources typed `opm:Property`
    - state resources typed `opm:CurrentPropertyState`
    - scalar values on `schema:value`
    - units on `smls:unit`
  - first-pass `unit:*` links derived from the project unit assignment
  - `owl:sameAs` links from LBD resources to Java-style IfcOWL instance resources such as `.../IfcWall_123`
  - first-pass Java-style IFC standard-attribute OPM export for `globalIdIfcRoot`, `nameIfcRoot`, and `longNameIfcSpatialStructureElement`
- `lbd-converter` also emits a first-pass direct IfcOWL graph from raw STEP entities:
  - schema-specific buildingSMART IfcOWL namespaces are now used for IFC2X3 / IFC4 / IFC4x1 / IFC4x3
  - schema-specific canonical class names and argument predicates are now loaded from local `proplist*.csv` reference tables
  - current `Duplex.ifc` IfcOWL output no longer falls back to positional predicates `arg_1`, `arg_2`, ...
  - entity references preserved as IRI links to other IfcOWL resources
  - primitive values are now materialized as explicit EXPRESS resources such as `IfcLabel` / `IfcLengthMeasure` with `express:hasString`, `express:hasDouble`, etc.
  - list values are now materialized as explicit IFC list resources with `list:hasContents` / `list:hasNext`
  - enum values are emitted as schema IRI individuals
- This phase should be considered incomplete for production fidelity:
  - IfcOWL output is closer to the Java/reference vocabulary now, but still not schema-faithful end-to-end
  - fresh local Java Level 3 comparison for `Duplex.ifc` is now close in raw size:
    - Rust: `48026` triples / `13591` subjects
    - Java: `47940` triples / `13586` subjects
  - the remaining LBD mismatch is now much narrower in raw size but still significant semantically under normalized diffing:
    - geometry-related LBD value output is still missing on Rust; the most visible Java-only slice is the `props:A` / `props:W` style OPM geometry-value graph
    - the earlier query-breaking `schema:value` vs `opm:value` mismatch has been fixed on the Rust side
    - the remaining normalized LBD delta on `Duplex.ifc` is now mostly ontology-shape noise, safe enrichment, and Java-side unit oddities rather than broad query-surface divergence
  - fresh local IfcOWL comparison for `Duplex.ifc` is now very close in raw counts:
    - Rust: `231145` triples / `81180` subjects
    - Java: `230761` triples / `80988` subjects
  - Java-style IfcOWL instance naming and ontology header/import triples are now present
  - Java-style flattening of entity-reference aggregates is now conditional on the schema range, so pure reference bags flatten where Java does, while schema-declared `*_List` ranges stay as list nodes
  - scalar resources are now reused by raw value in a Java-closer way, with `BOOLEAN` and `LOGICAL` cached separately so logical predicates no longer get polluted by earlier boolean reuse
  - a normalized `compare-turtle --normalize-ifcowl-scalars` diff now shows the remaining IfcOWL mismatch is narrow rather than broad:
    - `missing_from_right=600`
    - `missing_from_left=216`
  - the remaining IfcOWL gaps are now concentrated in a small number of value/list-shape cases:
    - `IfcCompoundPlaneAngleMeasure` list identity/materialization
    - `IfcTrimmingSelect` wrapper-node vs flattened scalar handling on trimmed curves
    - residual scalar-family selection for trimmed-curve values
- Remaining Week 3 work is therefore still critical, not optional polish

### Week 4: `lbd-serializer` + `ifc2lbd-cli` -- Output & CLI

- [x] Implement streaming Turtle writer using `rio_turtle`
- [x] Implement subject-grouping buffer for compact output
- [x] Implement CLI with `clap` (production flags)
- [x] Wire up full pipeline: parse -> model -> convert -> serialize
- [x] Integration test: convert Duplex_A IFC file
- [ ] Compare output triple-by-triple with Java reference

**Current status (2026-03-13)**:

- `ifc2lbd-neo Duplex.ifc -t /tmp/duplex_lbd.ttl -u https://example.test/base/` runs successfully
- `ifc2lbd-neo Duplex.ifc -t /tmp/duplex_lbd.ttl --ifcowl-file /tmp/duplex_ifcowl.ttl -u https://example.test/base/` also runs successfully end-to-end
- The generated Turtle currently contains BOT hierarchy triples, first-pass `beo:*` product types, direct `props:*` values, `unit:*` links, `owl:sameAs` links, and a separate first-pass IfcOWL export
- Serializer now uses `rio_turtle` and sorts triples by subject/predicate so repeated predicates are grouped compactly for deterministic output
- The current IfcOWL export no longer uses the placeholder `https://example.org/ifcowl#` namespace, and the generated `Duplex.ifc` IfcOWL file now uses canonical names such as `IfcCartesianPoint` and `coordinates_IfcCartesianPoint`
- The current IfcOWL export also materializes explicit EXPRESS value resources and IFC list resources instead of flattening them into plain literals
- The current IfcOWL export now uses the IFC2X3 `TC1` namespace variant, Java-style instance naming such as `.../IfcApplication_2`, and emits the ontology header/import triples at the base IRI
- `compare-turtle` is now available as a repo-local utility binary to diff two Turtle files as normalized triple sets, which gives Week 4 a concrete comparison path
- A fresh local Java Level 3 reference has now been generated in `artifacts/reference-java/` with the same base URI and production-style flags (`-l 3 -be -p --hasUnits --ifcOWL`)
- The Rust converter now emits Java-style Level 3 OPM property/state resources for property sets and quantities, uses Java-style global `p` / `a` state counters, includes type-object inherited property sets, and now also includes `IfcOpeningElement` in the LBD element slice
- The Rust converter now also emits predefined-type-aware LBD product classes where they are available in the typed model, for example `beo:Covering-CEILING` and `beo:Footing-STRIP_FOOTING`, and now uses the Java `furn:` namespace for `IfcFurnishingElement`
- `compare-turtle` now supports base-IRI normalization plus `--normalize-lbd-opm` for state-node/timestamp-insensitive LBD diffs and `--normalize-ifcowl-scalars` for IfcOWL scalar/list-node normalization
- The current LBD mismatch on `Duplex.ifc` is no longer a broad size mismatch:
  - Rust `Duplex.ifc`: `48165` triples / `13638` subjects
  - Java `Duplex.ifc`: `47940` triples / `13586` subjects
  - baseline query parity is materially better:
    - `bot:Element` count matches (`268`)
    - `beo:Wall` count matches (`57`)
    - storey `bot:containsElement` counts match on the core `Duplex` baseline queries
  - the remaining Java-only side on `Duplex.ifc` is now mostly Java attaching `unit:M` to angle-like properties such as `crossSectionRotation` and `slope`
  - the Rust-only side is now mostly safe enrichment:
    - project-level LBD/OwlSameAs triples
    - predefined-type-aware classes
    - extra measured-unit triples
  - Rust now resolves measure-subtype units generically from the IFC nominal type family, so `IFCPOSITIVELENGTHMEASURE` and related typed values no longer miss units
  - for plane-angle-like measures Rust now emits the ontology-correct angle unit (`DEG`/`RAD`) instead of copying the Java `unit:M` behavior
  - the latest parity passes added direct aggregated `bot:hasSubElement`, `IfcStairFlight` product and IfcOWL naming parity, Java-style empty/self-value property filtering, decimal literal alignment, and direct-structure-only containment semantics
- quantity sets are already emitted in the Rust Level 3 path today, but quantity coverage and quantity naming/unit fidelity are still not reference-aligned and remain part of the Week 3/4 compatibility work
- The regenerated `Duplex.ifc` IfcOWL output is now close in raw size to the fresh Java reference:
  - Rust: `231145` triples / `81180` subjects
  - Java: `230761` triples / `80988` subjects
  - the main unresolved areas are now IfcOWL graph-shape fidelity rather than placeholder namespaces:
  - compound-plane-angle lists still differ from Java
  - trimmed-curve `IfcTrimmingSelect` values still differ from Java
  - a normalized `compare-turtle --normalize-ifcowl-scalars` diff is now down to `600` triples on the Rust-only side and `216` on the Java-only side
- Additional comparison fixtures were run with the bundled Java CLI jar:
  - `Infra-Bridge.ifc`
    - Rust LBD: `2772` triples / `898` subjects
    - Java LBD: `3027` triples / `983` subjects
    - important baseline query buckets now match:
      - `bot:Element = 57`
      - `IfcElementAssembly = 9`
      - `IfcBuildingElementProxy = 0`
    - the remaining IFC4 baseline gap is now narrower and concrete:
      - Rust still emits extra `descriptionIfcRoot` and two extra storey `objectTypeIfcObject = "abutment"` states
      - Java still emits IFC4 `status` property-set output and `elevationOfRefHeightIfcBuilding` where Rust does not yet
  - `DigitalHub_FM-ARC_v2.ifc`
    - Rust LBD: `324628` triples / `88457` subjects
    - Java LBD: `236690` triples / `65511` subjects
    - this is not a clean oracle case yet because the Java jar emits thousands of `RDFWriter` errors on that file during conversion
    - important baseline query buckets now match:
      - `bot:Element = 957`
      - `beo:StairFlight-STRAIGHT = 10`
      - storeys with `props:objectTypeIfcObject = 3`
      - `beo:CurtainWall` with `owl:sameAs = 1`
    - the dominant remaining diff buckets there are:
      - large Rust-only `props:*` declaration/comment surface for many custom IFC4 property sets
      - Java-only direct `bot:containsElement` membership for some `IfcMember` descendants under a storey
      - Java-side conversion failures on the source model
**Update (2026-03-14)**:

- `IfcPropertyEnumeratedValue` is now parsed and emitted at OPM Level 3 (fixes the missing IFC4 `status` property)
- `descriptionIfcRoot` emission is now correctly suppressed for null/empty descriptions (was emitting spurious triples before)
- `--topology` flag now defaults to `true` so adjacency triples are always produced without requiring an explicit flag
- `IfcCompoundPlaneAngleMeasure` list node IRIs are now stable: `IfcCompoundPlaneAngleMeasure_{entity_id}_{index}` instead of a global counter, and `compare-turtle --normalize-ifcowl-scalars` now normalizes these by content so the comparison shows `result=identical` for Duplex IfcOWL
- `IfcTrimmingSelect` wrapper nodes are eliminated: `trim1`/`trim2` items that are typed SELECT values (e.g. `IFCPARAMETERVALUE(180.)`) are now emitted as direct predicate values, matching Java behavior
- Duplex IfcOWL comparison: **`result=identical`** after normalization
- IfcOWL serializer rewritten: replaced `TurtleFormatter` with a zero-allocation streaming writer that uses Turtle prefix abbreviation; CX fixture IfcOWL output dropped from **7.4 GB → 4.9 GB** at the same ~33s wall time
- `module-ifc2lbd` in singleIngestConverter now uses `ifc2lbd-neo` instead of the Java JAR: multi-stage Dockerfile builds the Rust binary from source; `server.py` calls the binary directly; no JRE required
- Remaining IFC4 gap (`elevationOfRefHeightIfcBuilding`) and further IfcOWL volume reduction are the next priorities

**Update (2026-03-17)**:

- topology pipeline implementation plan is tracked separately in `docs/plans/topology-pipeline-plan.md`
- query-driven topology targets are tracked in `docs/archive/topology-query-goals.md`
- paper-oriented migration/enhancement/issues notes are tracked in `docs/archive/paper-notes.md`

- IFC4 `IfcBuilding` elevation field indexing in `ifc-model` is fixed:
  - `elevation_of_ref_height` now reads `args[9]`
  - `elevation_of_terrain` now reads `args[10]`
  - added unit test coverage in `ifc-model` for `IfcBuilding` elevation parsing
- `Infra-Bridge.ifc` LBD parity improved after regenerating Rust output:
  - normalized compare (`--normalize-lbd-opm`) vs Java:
    - before: `missing_from_right=392`, `missing_from_left=154`
    - after: `missing_from_right=238`, `missing_from_left=178`
  - `props:elevationOfRefHeightIfcBuilding` and related OPM state triples are now present on the Rust side
- Serializer compaction pass added:
  - emits Turtle shorthand `a` instead of `rdf:type` (graph semantics unchanged)
  - validated on `Duplex.ifc`:
    - LBD old vs new serializer output: `result=identical`
    - IfcOWL old vs new serializer output: `result=identical`
  - file-size effect on Duplex:
    - IfcOWL: `30,760,094` -> `30,098,321` bytes
    - LBD: `3,777,366` -> `3,682,117` bytes
  - large CX run (`CX_AP2.0_ifc_Modell_WIP_Koordinationsmodell (1).ifc`) after this change:
    - wall time: `33.54s`
    - max RSS: `~2.04 GB`
    - output sizes: LBD `23,519,546` bytes, IfcOWL `5,059,302,569` bytes
    - serializer-only saving estimate from `a` compaction: `23,890,351` `rdf:type` triples on this file, i.e. ~`167,232,457` bytes (~`159.5 MiB`) avoided
- Week 4 comparison flow rerun on current outputs:
  - `Duplex.ifc` IfcOWL Java vs Rust:
    - strict compare: expected node-id churn remains large
    - normalized compare (`--normalize-ifcowl-scalars`): `result=identical`
  - `Duplex.ifc` LBD Java vs Rust:
    - strict compare remains noisy (OPM state ids/counters)
    - normalized compare (`--normalize-lbd-opm`): `missing_from_right=26`, `missing_from_left=115`
  - remaining normalized LBD delta is now concentrated in known intentional/legacy-behavior differences (Java `unit:M` on angle-like properties, containment edge shape, and Rust-side enrichment)
  - full command outputs are captured in `artifacts/benchmarks/parity_report_2026-03-17.md`
  - constrained fixture runner is now available at `scripts/run_allowed_fixtures.py` and only uses:
    - `Duplex.ifc`
    - `IFC_SKW_Modell_07052019.ifc`
    - `Infra-Bridge.ifc`
  - current constrained-run summary is captured in:
    - `artifacts/benchmarks/allowed_fixtures_report.md`
    - `artifacts/benchmarks/allowed_fixtures_report.json`
  - Infra IfcOWL normalized compare is currently resource-heavy and unstable in this local flow (observed run: `~166.68s`, peak RSS `~6.26 GB`, terminated), so Duplex normalized IfcOWL remains the reliable oracle path for now

- Phase 1 should not be signed off until IFC4 baseline parity is substantially closer on `Infra-Bridge.ifc` and the large IfcOWL output volume for the CX fixture is reduced

## Phase 2: Parallelism & Topology (Weeks 5-6)

- [x] Parallel STEP line parsing with `rayon::par_iter`
- [x] Parallel first-pass entity construction with Rayon reduction
- [ ] `DashMap`-backed concurrent indexes if the Rayon reduction path proves insufficient
- [x] First bounded conversion-to-serialization handoff with `crossbeam::channel`
- [x] Fully streamed conversion that avoids materializing each graph before batching
- [x] `lbd-topology` crate: first-pass IfcRelSpaceBoundary adjacency (port target: IFC2BOT `space_adjacency.py`)
- [x] First-pass containment reasoning enrichment
- [x] First-pass element hosting (IfcRelVoidsElement + IfcRelFillsElement)

**Current status (2026-03-13)**:

- `ifc-model` now parses `IfcRelSpaceBoundary`, `IfcRelVoidsElement`, and `IfcRelFillsElement`
- `ifc-model` now performs its first entity classification/construction pass in parallel using Rayon and then reduces partial results into the final deterministic model indexes
- `ifc-step` now does boundary-scan entity extraction followed by Rayon-backed parallel per-entity parsing
- `ifc2lbd-cli` now uses bounded `crossbeam::channel` batch handoff to serializer threads for both LBD and optional IfcOWL output
- `lbd-topology` now derives:
  - `bot:adjacentElement` candidates from space-boundary relationships
  - `bot:hasSubElement` candidates from opening/filling relationships
  - direct-structure `bot:containsElement` candidates from spatial containment, including aggregated element descendants such as stair flights under contained stairs
- `ifc2lbd-cli --topology` now enables this first-pass topology export
- Verified on `Duplex.ifc`: the generated LBD Turtle contains `bot:adjacentElement`, `bot:hasSubElement`, and direct-structure `bot:containsElement` on storey/space resources with aggregated descendants included
- LBD and IfcOWL conversion now run concurrently against separate bounded serializer channels instead of being executed sequentially
- The LBD path now streams batches incrementally during conversion as well; it no longer materializes the full LBD graph before serializer output can begin
- The IfcOWL path now streams batches incrementally during conversion; it no longer waits to materialize the full IfcOWL graph before serializer output can begin
- Release-path performance on this 8-core host remains strong after the recent parity fixes:
  - `Duplex.ifc` (`LBD + IfcOWL`, baseline path) now runs in about `0.30s` wall time with about `63 MB` peak RSS
  - the 171 MB `CX_AP2.0_ifc_Modell_WIP_Koordinationsmodell (1).ifc` fixture now runs in about `36.62s` wall time with about `1.95 GB` max RSS after IfcOWL scalar-resource reuse
- The large-file benchmark still makes the current limiting factor explicit: the generated IfcOWL file for the 171 MB fixture dropped from about `12 GB` to about `8.6 GB`, so the dominant problem is still output-shape / serialization volume, not blocked full-graph materialization
- Remaining Week 5/6 work is therefore split between continued large-file profiling and the Week 4/IfcOWL fidelity cleanup needed to bring output volume back under control

**Known topology bugs (2026-03-17) — must fix before topology output is usable:**

**Bug T1: `bot:intersectingElement` falsely emitted for void/fill pairs**

- Root cause: `build_topology` in `lbd-topology/src/lib.rs` lines 179-191 pushes **both** `HasSubElement` and `IntersectingElement` edges for every `IfcRelVoidsElement + IfcRelFillsElement` chain. A door or window that fills a designed void in a wall is a sub-element by definition — it does not *intersect* the wall in the BOT sense.
- Effect: every hosted door/window appears as `bot:intersectingElement` of its host wall (and in the current output, the roof also gets `bot:intersectingElement` of its skylight windows). These triples are semantically wrong.
- Fix: remove the `IntersectingElement` push from the void/fill loop. Only `HasSubElement` should be derived from this relation pair. `bot:intersectingElement` must only come from geometry-derived evidence (Phase 3).

**Bug T2: Bounding box broad-phase produces meaningless element→element `bot:adjacentElement` and `bot:Interface` triples**

- Root cause: `merge_geometry_relations_into_topology` in `lbd-converter/src/lib.rs` promotes geometry-derived `AdjacentElement` / `IntersectingElement` / `InterfaceOf` edges directly into `core_edges` without any semantic filtering. The bbox broad phase detects overlapping or touching bounding boxes and emits all of them. This results in:
  - doors adjacent to other doors, slabs, and unrelated walls
  - `bot:Interface` nodes between two doors, a door and a window, a window and a window, a slab and a window
  - every element whose AABB touches any other AABB in the same storey becomes adjacent to it
- Effect: the topology file is dominated by false-positive adjacency and interface triples that cannot be trusted. The `bot:adjacentElement` query surface is polluted beyond usability.
- Fix: bbox-derived relations must **not** be promoted to BOT core topology without exact-kernel confirmation. Until an exact geometry kernel is wired (Phase 3), the `--geometry-bboxes-file` path should either be disabled or emit only `extension_edges` (not `core_edges`). BOT-core `bot:adjacentElement` should remain IFC-relation-derived only (space boundary adjacency) until geometry is exact.

- [x] Fix T1: remove spurious `IntersectingElement` from void/fill derivation in `lbd-topology`
- [x] Fix T2: gate bbox-derived relations to extension-only path; exact-kernel-confirmed relations promoted to BOT core
- [x] Fix IfcOWL class IRI bleed in LBD file: removed fallback to `ifcowl_class_iri_for_entity` for unmapped elements in `lbd-converter`. Unmapped elements now get only `bot:Element`. IfcOWL class IRIs no longer appear in LBD output (0 references, was 50+).
- [x] Fix BOT spec violation in `lbd-geometry`: removed `AdjacentElement` emit for element-element touching in OCC exact kernel. `bot:adjacentElement` is strictly Zone→Element per BOT spec; element-element touching produces `InterfaceOf` / `bot:Interface` only.

**Conversion results after fixes (2026-03-17):**

- `Duplex.ifc`:
  - exact-kernel cache prebuild: `0.036s`
  - exact-kernel candidate pairs: `663` (tolerance=0.000001)
  - exact-kernel relations: `960` in `1.578s`
  - wall time: `~2.0s`
  - LBD: `82201` lines, IfcOWL: separate, topology: `1992` lines
  - `bot:Interface` nodes in topology: present (OCC-confirmed)
  - `standards.buildingsmart.org` IRIs in LBD: `0` (was 50+)

- `DigitalHub_FM-ARC_v2.ifc`:
  - exact-kernel candidate pairs: `0` (no `IfcRelSpaceBoundary` in this model — expected)
  - exact-kernel relations: `0` in `0.000s`
  - wall time: `~1.9s`
  - LBD: `580832` lines, IfcOWL: `1564142` lines, topology: `181` lines
  - `bot:hasSubElement` entries in topology: 45 (window/door sub-elements correctly derived)
  - `standards.buildingsmart.org` IRIs in LBD: `0`
  - Note: no space adjacency data in DigitalHub — `bot:adjacentElement`/`bot:adjacentZone` are empty by design

## Phase 3: Geometry & Interfaces (Weeks 7-8)

- [x] Native OCC exact geometry kernel (`lbd-geometry-kernel` crate using `chijin`)
  - [x] BRep shape extraction from IFC (`IfcExtrudedAreaSolid`, `IfcMappedItem`, `IfcBooleanClippingResult`, `IfcLocalPlacement`)
  - [x] BRep cache per entity (`<ifc>.occ-cache/<id>.brepbin`)
  - [x] `--prebuild-cache-from-ifc` mode for full-model pre-extraction
  - [x] Exact boolean intersection/touch classification (volume > tolerance → intersects, null volume → touches → Interface)
  - [x] Subprocess JSON protocol wired into `ifc2lbd-cli --exact-kernel-bin`
  - [x] Bbox broad-phase candidate generation (`--geometry-bboxes-file` or auto)
  - [x] OCC-confirmed relations → BOT core; bbox-only candidates → extension
  - [x] Benchmarked on Duplex: 663 candidate pairs → 1778 OCC-verified relations, 409 `bot:Interface` nodes, ~1.7s kernel time
- [ ] WKT bounding box output — implement with correct GeoSPARQL spec (CRS IRI, `geo:Geometry` type, correct namespace `http://www.opengis.net/ont/geosparql#`; fixes Java open issues #97/#100/#101/#102)
- [ ] R-tree construction with `rstar` for large-model candidate pair scaling
- [ ] `bot:Interface` area computation (currently `shared_boundary_area: None` — OCC face area extraction needed)
- [ ] Combined `--topology-and-geometry` mode (currently: pass `--exact-kernel-bin` to activate)

## Phase 4: Integration & Chunked Output (Weeks 9-10)

- [ ] PyO3 bindings with `maturin` (pip-installable wheel)
- [x] Replace `module-ifc2lbd` in singleIngestConverter (multi-stage Docker build, no JRE)
- [ ] Chunked TTL output (`--output-dir`) for parallel triplestore loading
- [ ] Docker multi-stage build (~20MB container)

## Phase 5: Polish (Weeks 11-12)

- [ ] Geolocation extraction (lat/long from IfcSite)
- [ ] ifcZip decompression support
- [ ] Hierarchical URI naming (if needed)
- [ ] CI/CD pipeline
- [ ] Cross-compilation (Linux/macOS/Windows)
- [ ] Comprehensive error reporting and logging

**Correctness items from upstream Java IFCtoLBD open issues (implement correctly in Rust):**

- [ ] Filter `NOTDEFINED` enum values — Java issue #7 (2018, still open): suppress enum property values that are `NOTDEFINED` from output; these carry no information and pollute property sets. Add filter in property emission path in `lbd-converter`.
- [ ] Empty base IRI fallback — Java issue #110: when `--url` is omitted or empty, substitute `file:///<absolute-input-path>#` as base URI rather than failing or using a blank IRI. Currently the CLI requires `--url` explicitly.
- [ ] Verify `IfcPropertyEnumeratedValue` ENUM IRI generation — Java issue #3: enum individuals must be emitted as IRIs, not blank nodes or raw string literals. Current `IfcPropertyEnumeratedValue` parsing exists; verify the emitted state/value is an IRI referencing the schema enum individual, not a plain literal.
- [ ] GeoSPARQL implementation must use correct namespace and structure — Java issues #97/#100/#101/#102 are all unfixed in Java. When implementing Phase 3 GeoSPARQL output: use `http://www.opengis.net/ont/geosparql#` (not any incorrect variant), always set a CRS IRI in WKT literals (`<http://www.opengis.net/def/crs/OGC/1.3/CRS84>`), emit `geo:Geometry` type triple, validate WKT coordinate order matches CRS84 (lon, lat).

## Verification

```bash
# After Phase 1 MVP:
cargo test --workspace && \
cargo run -p ifc2lbd-cli -- test_files/Duplex_A_20110505.ifc -t /tmp/out.ttl && \
diff <(rapper -i turtle /tmp/out.ttl -o ntriples | sort) <(rapper -i turtle reference/duplex.ttl -o ntriples | sort)
```
