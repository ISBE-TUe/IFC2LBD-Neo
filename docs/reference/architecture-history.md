# ifc2lbd-neo: High-Performance IFC-to-LBD Converter in Rust

> Status note: this document contains valuable architecture context, but parts are historical snapshots.
> For current behavioral contracts, use `docs/current/status.md` and the docs linked under "Authoritative (Current)".

## Context

Replace the existing Java-based IFC2LBD converter (slow, RAM-hungry, single-threaded) with a modern Rust implementation that maximizes throughput across 32+ threads, uses minimal RAM, and serves as a drop-in replacement for the singleIngestConverter's `module-ifc2lbd` service.

## Language Recommendation: Rust

**Why Rust over Go or C++:**

| Criterion | Rust | Go | C++ |
| --- | --- | --- | --- |
| Memory efficiency | No GC, zero-cost abstractions | GC pauses, higher baseline RAM | Manual, error-prone |
| Thread safety | Compile-time guarantees (ownership) | Goroutines are easy but share-nothing | Data races are your problem |
| Performance ceiling | Matches C++ | 10-30% slower than C/Rust for compute | Matches Rust |
| RDF ecosystem | `oxigraph`, `rio_turtle`, `sophia` | Almost nothing | `raptor2` (C library) |
| IFC parsing | Custom (STEP is simple, parallelizable) | Custom (same effort) | IfcOpenShell is C++ (direct use) |
| Spatial indexing | `rstar` (excellent R-tree) | `rtreego` (basic) | CGAL, Boost.Geometry |
| Python bindings | PyO3 + maturin (first-class) | cgo (awkward) | pybind11 (mature but painful) |
| Developer velocity | Moderate (steep curve, but strong tooling) | Fast (simple language) | Slow (build systems, UB) |
| Binary size | ~5-10MB static binary | ~10-15MB | Varies |
| Safety | Memory-safe by default | Memory-safe (GC) | Unsafe by default |

**Go** would be a reasonable choice for simpler tooling, but its GC and lack of zero-cost abstractions make it suboptimal for a CPU+memory-intensive parser/converter. **C++** gives maximum performance but the development/maintenance cost is too high and memory bugs are inevitable. **Rust** hits the sweet spot: C++-level performance with memory safety, excellent parallelism via Rayon, and first-class Python integration via PyO3.

## Key Architectural Insight

The Java system takes a **massive detour**: it converts IFC STEP -> IfcOWL RDF (millions of triples including all geometry coordinates, owner history, etc.) -> stores in TDB2 on disk -> queries it back via SPARQL path queries to extract the ~50 entity types LBD actually needs. This intermediate IfcOWL model is the root cause of the RAM and speed problems.

**ifc2lbd-neo skips the IfcOWL *intermediate model*.** It parses IFC STEP directly into a lightweight typed domain model and emits LBD triples directly. However, IfcOWL TTL *output* is still generated (it's required for the triplestore) — but it's produced as a direct STEP-to-IfcOWL serialization pass, not by building an in-memory Jena model. Both LBD and IfcOWL output are connected via `owl:sameAs` triples (same as Java — see `LBD_RDF_Utils.java:40-129`).

## IfcOWL Output & owl:sameAs Linking (Critical Requirement)

Both LBD TTL and IfcOWL TTL are **always produced** and loaded into separate named graphs in the triplestore (`{base}/lbd` and `{base}/ifcowl`). They must be linked:

- The Java converter creates `owl:sameAs` triples in the LBD output: `<base/wall_GUID> owl:sameAs <ifcowl_resource>` for every mapped element
- ifc2lbd-neo replicates this: during LBD conversion, when `ifcowl` is enabled, emit `owl:sameAs` triples linking LBD URIs to IfcOWL URIs
- The IfcOWL output is a separate serialization pass: walk the raw `HashMap<u64, RawEntity>` and emit typed IfcOWL triples directly from STEP data (no Jena, no intermediate OWL model)

**Chunked Output for Parallel Triplestore Loading:**

For large files (500MB+ IFC -> huge TTL output), ifc2lbd-neo can optionally write output in chunks to enable the triplestore to start loading while conversion continues:

```text
ifc2lbd-neo input.ifc --output-dir /imports/ --chunk-size 50MB
  -> /imports/lbd_chunk_001.ttl    (triplestore starts loading immediately)
  -> /imports/lbd_chunk_002.ttl    (written while chunk_001 is loading)
  -> /imports/ifcowl_chunk_001.ttl
  -> ...
```

Each chunk is a self-contained Turtle file (prefixes repeated). The singleIngestConverter uses Blazegraph's file-based load API (`POST ?uri=file:///imports/...`) which is much faster than SPARQL INSERT. Chunks enable pipelining: convert -> write chunk -> triplestore loads chunk -> convert next chunk.

**Note:** This chunked approach is an optimization for Phase 4/5. The MVP writes single complete files.

## Crate Structure (Cargo Workspace)

```text
ifc2lbd-neo/
  Cargo.toml                   # workspace root
  crates/
    ifc-step/                   # IFC STEP file parser (zero-copy, parallel)
    ifc-model/                  # Typed IFC domain model / intermediate representation
    ifc-schema/                 # IFC type hierarchy tables (IFC2X3, IFC4, IFC4x3)
    lbd-ontology/               # LBD namespace constants (BOT, PROPS, OPM, PRODUCT, etc.)
    lbd-converter/              # Core: IFC model -> LBD triples + IfcOWL output + owl:sameAs
    lbd-topology/               # IFC2BOT topology analysis (IfcRelSpaceBoundary adjacency)
    lbd-geometry/               # Bounding boxes, R-tree, interface detection
    lbd-serializer/             # Streaming Turtle/TriG/JSON-LD writer
    ifc2lbd-cli/                # CLI binary (clap, matching Java CLI flags)
    ifc2lbd-python/             # PyO3 bindings (pip-installable via maturin)
```

## Current Implementation Status

As of 2026-03-13, `crates/ifc-step`, `crates/ifc-schema`, and `crates/ifc-model` contain real code. Current implemented capability:

- `ifc-step`
  - memory-mapped file reads via `memmap2`
  - STEP header parsing for `FILE_SCHEMA` plus `FILE_DESCRIPTION`
  - single-pass DATA boundary scan to find complete `#...;` entity spans
  - Rayon-backed parallel per-entity parsing into `HashMap<u64, RawEntity>`
  - IFC Unicode decoding for `\X\`, `\X2\...\X0\`, and `\S\`
- `ifc-schema`
  - minimal spatial type lookup
  - first-pass element classification for the Duplex fixture and initial BOT hierarchy work
- `ifc-model`
  - typed model construction for project/site/building/storey/space
  - first-pass element nodes
  - `IfcRelAggregates`, `IfcRelContainedInSpatialStructure`, and `IfcRelDefinesByProperties`
  - property/unit slices for `IfcPropertySet`, `IfcPropertySingleValue`, `IfcElementQuantity`, quantity entities, `IfcUnitAssignment`, `IfcSIUnit`, and `IfcConversionBasedUnit`
  - compressed IFC GUID <-> UUID conversion helpers
  - relationship indexes: `children_of`, `contained_in`, `guid_to_entity`, `property_sets_for_object`, `quantities_for_object`
  - Rayon-backed parallel first-pass entity classification and reduction into the final model
- `lbd-ontology`
  - namespace constants for the first conversion path
  - lightweight RDF triple/object types used between conversion and serialization
- `lbd-converter`
  - first-pass BOT hierarchy emission
  - first-pass `beo:*` product typing from IFC entity names
  - Java-style Level 3 OPM property/state emission for `IfcPropertySet` / `IfcElementQuantity`
  - quantity sets are already included in the current LBD export path; they are not yet separately configurable and still need Java-reference alignment for naming/unit fidelity
  - first-pass Java-style IFC standard-attribute OPM emission for `globalIdIfcRoot`, `nameIfcRoot`, and `longNameIfcSpatialStructureElement`
  - first-pass QUDT-backed unit links resolved from `IfcUnitAssignment`
  - `owl:sameAs` links from LBD resources to Java-style IfcOWL instance resources such as `.../IfcWall_123`
  - first-pass direct IfcOWL serialization from raw STEP entities using schema-specific buildingSMART namespaces plus local `proplist*.csv` lookup tables for canonical class names / argument predicates and ontology-derived predicate ranges
  - explicit EXPRESS value resources and IFC list resources in the IfcOWL pass (`IfcLabel`, `IfcLengthMeasure`, `..._List`, `express:hasString`, `list:hasContents`, `list:hasNext`)
  - Java-style IfcOWL instance naming and ontology header/import triples (`.../IfcApplication_2`, base `owl:Ontology`, base `owl:imports ifc:`)
  - concurrent LBD/IfcOWL streaming in the runtime path, with incremental LBD and IfcOWL batch emission instead of full-graph buffering before serializer output
  - batch-oriented channel handoff helpers for LBD and IfcOWL serializer threads
- `lbd-topology`
  - first-pass topology derivation from `IfcRelSpaceBoundary`, `IfcRelVoidsElement`, and `IfcRelFillsElement`
  - emits adjacency, host/sub-element, and direct-structure containment relationships for the current BOT export path, including aggregated element descendants under the directly contained host element
- `lbd-serializer`
  - Turtle output via `rio_turtle`
  - deterministic subject/predicate ordering so repeated predicates are grouped compactly in output
  - receiver-based batch serialization API for bounded handoff from the converter
- `ifc2lbd-cli`
  - end-to-end CLI path from IFC input to LBD Turtle output
  - optional separate IfcOWL Turtle output via `--ifcowl-file`
  - optional topology enrichment via `--topology`
  - bounded channel handoff to serializer threads for actual file writing
  - `compare-turtle` utility binary for normalized Turtle-vs-Turtle diffing during reference validation

This implementation is verified against `Duplex.ifc` and has also been compared against `Infra-Bridge.ifc` and `model-A.ifc` using the bundled Java CLI jar. The generated IfcOWL output for `Duplex.ifc` now uses canonical names such as `IfcCartesianPoint` and `coordinates_IfcCartesianPoint`, no longer falls back to `arg_n` predicates there, materializes explicit EXPRESS/list value resources, canonicalizes local names from the ontology, reuses scalar resources by raw value in a Java-closer way, keeps `BOOLEAN` and `LOGICAL` cache identity separate, emits the Java-style base ontology header/import, uses Java-style instance naming such as `.../IfcApplication_2`, flattens entity-reference aggregates only where the schema range is not a list class, uses EXPRESS namespace list classes such as `express:REAL_List` where Java does, skips derived `*` placeholders instead of serializing them as literals, and uses a boundary-scan plus Rayon-backed parallel STEP parse in `ifc-step`. The LBD side now uses Java-style Level 3 OPM property/state resources with `schema:value`, Java-style global `p` / `a` state counters, type-object inherited property sets, includes `IfcOpeningElement` in the current LBD element slice, emits fallback IFC typing for non-LBD elements such as openings, emits predefined-type-aware product classes such as `beo:Covering-CEILING`, `beo:Footing-STRIP_FOOTING`, `beo:Slab-FLOOR`, `beo:Railing-NOTDEFINED`, uses the Java `furn:` namespace for `IfcFurnishingElement`, emits the Java bounding-box predicate declarations (`lbd:hasBoundingBox`, `lbd:x-min`, `lbd:x-max`, `lbd:y-min`, `lbd:y-max`, `lbd:z-min`, `lbd:z-max`), aligns `IfcBuildingElementProxy` to Java-style `buildingelement_<guid>` URIs, carries spatial `objectTypeIfcObject` in the typed model, and resolves measure-subtype units generically from IFC nominal value types so typed values such as `IFCPOSITIVELENGTHMEASURE` no longer miss units. Current release-path measurements on this 8-core machine remain strong on memory: a recent release run on `Duplex.ifc` peaked at about `64 MB` RSS, and the previously validated baseline path still runs in about `0.30s` wall time; the 171 MB `CX_AP2.0_ifc_Modell_WIP_Koordinationsmodell (1).ifc` fixture still measures about `36.62s` / `1.95 GB` max RSS. That large-file run still produces about `8.6 GB` of IfcOWL Turtle, down from the earlier `12 GB`, which makes the remaining problem explicit: the current converter is no longer bottlenecked by parser/materialization plumbing, but by output shape and serializer volume. Against the fresh local Java Level 3 `Duplex.ifc` references in `artifacts/reference-java/`, the current Rust LBD output is now `48165` triples / `13638` subjects versus Java `47940` / `13586`, while the current Rust IfcOWL output is `231145` triples / `81180` subjects versus Java `230761` / `80988`. A normalized development compare that abstracts generated scalar/list node IDs narrows the remaining IfcOWL mismatch to `600` Rust-only triples and `216` Java-only triples; the remaining delta is now concentrated in `IfcCompoundPlaneAngleMeasure` list identity/materialization and trimmed-curve wrapper/value-shape cases rather than broad namespace or graph-structure mismatches. On the LBD side, the earlier query-breaking `schema:value` vs `opm:value` mismatch is fixed, baseline query parity on `Duplex.ifc` is materially better (`bot:Element` counts match and `beo:Wall` counts match), and the remaining Java-only side is mostly the Java exporter attaching `unit:M` to angle-like properties where Rust now emits the ontology-correct angle unit. The `Infra-Bridge.ifc` comparison is now much narrower at the query surface: `bot:Element`, `IfcElementAssembly`, and `IfcBuildingElementProxy` counts match, and the remaining baseline gap is mostly extra Rust `descriptionIfcRoot` / storey `objectTypeIfcObject = "abutment"` states versus Java-side `status` and `elevationOfRefHeightIfcBuilding` output. The `model-A.ifc` comparison is still not a clean oracle case because the Java jar itself emits thousands of `RDFWriter` errors on that model, but several important query buckets now match there too (`bot:Element`, `beo:StairFlight-STRAIGHT`, storey `objectTypeIfcObject`, and `beo:CurtainWall` with `owl:sameAs`). The dominant remaining `model A` gap is now split between a large Rust-only `props:*` declaration/comment surface for custom IFC4 property sets and a real baseline containment mismatch where Java still emits some `bot:containsElement` links for `IfcMember` descendants under a storey that Rust does not yet reproduce. At this point the main remaining semantic work is IFC4 baseline parity and the small IfcOWL tail, not parser speed or threading. The architecture below remains the target end state; reference-faithful LBD semantics across IFC4, deeper topology fidelity, geometry instances, chunked output, and alternate output formats are still pending.

## Processing Pipeline

```text
                    +-----------------------------------------------------------+
                    |                    32+ threads via Rayon                   |
                    +-----------------------------------------------------------+

  IFC File --> [1. Parse STEP] --> [2. Build Model] --> [3. Convert to LBD] --> [4. Serialize]
   (mmap)       parallel lines      parallel entities     parallel subgraphs     streaming output
                                                                |
                                                    [3b. Geometry + R-tree]
                                                      (parallel per element)
```

### Stage 1: Parse IFC STEP (parallel)

- Memory-map file with `memmap2` (OS manages paging, no heap copy)
- Single-pass scan for line boundaries (`#NNN=...;` lines)
- `rayon::par_iter` to parse each line into `RawEntity { id, type_name, args: Vec<StepValue> }`
- Entity names interned via `SmolStr` (~800 unique IFC types)
- Handle multi-line entities (`;` continuation)
- Handle IFC Unicode escapes (`\X\HH`, `\X2\HHHH\X0\`)

### Stage 2: Build Typed Domain Model (parallel)

- From `HashMap<u64, RawEntity>`, construct only the ~50 entity types LBD needs:
  - Spatial: IfcProject, IfcSite, IfcBuilding, IfcBuildingStorey, IfcSpace
  - Elements: All IfcElement subtypes (IfcWall, IfcDoor, IfcWindow, etc.)
  - Relationships: IfcRelAggregates, IfcRelContainedInSpatialStructure, IfcRelSpaceBoundary, IfcRelVoidsElement, IfcRelFillsElement, IfcRelDefinesByProperties
  - Properties: IfcPropertySet, IfcPropertySingleValue, IfcElementQuantity, quantities
  - Units: IfcUnitAssignment, IfcSIUnit
- Skip everything else (geometry coordinates, owner history, representations) unless geometry is requested
- Use `rayon::par_iter` for entity construction (read-only shared HashMap)
- Build relationship indexes: `children_of`, `contained_in`, `guid_to_entity`

### Stage 3: Convert to LBD + IfcOWL Triples (parallel per subgraph)

- Walk spatial hierarchy: Site -> Building -> Storey -> Space -> Element
- Emit BOT triples: `bot:hasBuilding`, `bot:hasStorey`, `bot:hasSpace`, `bot:containsElement`, `bot:adjacentElement`, `bot:hasSubElement`
- Emit PRODUCT type triples: `beo:Wall`, `beo:Door`, etc. (from IFC type hierarchy lookup)
- Extract property sets at configured OPM level (1/2/3)
- Port `GuidCompressor` (22-char IFC GUID -> UUID format)
- **IfcOWL pass**: Parallel serialization of raw entities to IfcOWL triples (direct STEP-to-IfcOWL, no intermediate model)
- **owl:sameAs**: For each LBD resource, emit `owl:sameAs` linking to corresponding IfcOWL URI
- Send triples via `crossbeam::channel` to serializer (bounded, backpressure)

### Stage 3b: Topology Analysis (--topology flag, from IFC2BOT)

- **IfcRelSpaceBoundary adjacency**: Walk all IfcRelSpaceBoundary entities, emit `bot:adjacentElement` triples between spaces and their bounding elements (port of `space_adjacency.py`)
- **Containment reasoning**: Enrich space/storey containment from IfcRelContainedInSpatialStructure
- **Element hosting**: IfcRelVoidsElement + IfcRelFillsElement -> `bot:hasSubElement` (doors in walls, windows in walls)
- No geometry required -- purely topological from IFC relationship entities

### Stage 3c: Geometry & Spatial Indexing (--geometry flag, parallel, optional)

- Invoke IfcOpenShell subprocess for bounding boxes per GUID (same binary protocol as Java)
- Build `rstar::RTree<3>` from bounding boxes
- Generate 6 face rectangles per element for interface detection
- Proximity queries with 0.05 tolerance -> `bot:Interface` resources
- Emit bounding box triples (WKT format)
- Can combine with `--topology` for richest output

### Stage 4: Streaming Serialization

- `rio_turtle::TurtleFormatter` for Turtle output
- Buffer triples per subject for compact grouping, flush periodically
- Single writer thread consuming from channel
- TriG support (named graph headers)
- JSON-LD via `serde_json` (lower priority)

## Memory Management Strategy

| Strategy | Crate | Purpose |
| --- | --- | --- |
| `memmap2` | ifc-step | IFC file stays on disk, OS pages in as needed |
| `SmolStr` + interning | ifc-step, ifc-model | Only ~800 unique type names across all entities |
| Lazy entity construction | ifc-model | Only materialize typed structs for the ~50 entity types LBD needs; rest stays as lightweight `RawEntity` |
| Streaming output | lbd-serializer | Never hold all triples in memory |
| `crossbeam::channel` (bounded) | lbd-converter | Backpressure prevents unbounded buffering |
| Rayon reduce over partial maps | ifc-model | Parallel entity construction without shared mutable state |
| `DashMap` | ifc-model | Still an option if shared concurrent indexing becomes necessary |

**Expected RAM**: The architectural target remains a lightweight parse/model footprint for a 200MB IFC, because there is still no in-memory IfcOWL graph expansion. However, current end-to-end release measurements are not at that target yet: the 171 MB fixture currently peaks at about `1.95 GB` RSS, largely due to the present IfcOWL output shape and serialization volume. Compare to Java: 200MB+ heap + TDB2 disk store. No `performance_boost` needed -- we parse everything but only *materialize* what LBD needs; the remaining work is to make the emitted graph shape match that intention more closely.

## CLI Interface (simplified, production-focused)

The Java CLI has many flags, most unused in production. We keep the core set used by singleIngestConverter and add topology analysis.

**Actual production flags**: `--hasBuildingElements --hasBuildingElementProperties --hasUnits --hasIfc_based_elements --ifcOWL` (no performanceBoost -- it loses data; ifcOWL always on)

```text
ifc2lbd-neo <input.ifc> [OPTIONS]

  # Core (matching production usage)
  -t, --target-file <FILE>          Output file [default: <input>.ttl]
  -u, --url <URI>                   Base URI [default: https://lbd.example.com/]
  -l, --level <LEVEL>               OPM property level 1|2|3 [default: 3]
  --building-elements                Include building elements [default: true]
  --building-properties              Include element properties [default: true]
  --units                            Include unit information [default: true]
  --ifcowl                           Also export IfcOWL model + owl:sameAs links [default: true]

  # Topology & Geometry (NEW -- from IFC2BOT)
  --topology                         Enable topological analysis:
                                       - Space adjacency via IfcRelSpaceBoundary
                                       - Storey/space containment reasoning
  --geometry                         Enable geometric analysis (requires IfcOpenShell):
                                       - Bounding boxes + WKT
                                       - R-tree spatial containment
                                       - bot:Interface detection (face proximity 0.05 tol.)
  --topology-and-geometry            Both topology + geometry combined

  # Output
  --output-dir <DIR>                 Write chunks to dir (for parallel triplestore loading)
  --chunk-size <SIZE>                Chunk size for --output-dir [default: 50MB]
  --trig                             Output TriG format
  --json                             Output JSON-LD format

  # Performance
  -j, --threads <N>                  Thread count [default: all cores]
```

**Flags intentionally dropped** (low-value or harmful):

- `--has-performance-boost` -- **REMOVED**: loses data. Not needed in Rust anyway (no IfcOWL intermediate, so parsing all entities is cheap)
- `--has-hierarchical-naming` -- can add later if needed
- `--has-geolocation` -- rarely used, can add later
- `--has-separate-*-model` -- always produce combined output
- `--has-ifc-based-elements` -- always include (matches production config)

## Python Integration (PyO3)

```python
import ifc2lbd_neo

result = ifc2lbd_neo.convert(
    "input.ifc",
    base_uri="https://ub-edge.de/base/",
    level=3,
    building_elements=True,
    building_properties=True,
    units=True,
)
result.lbd_ttl      # -> str (Turtle)
result.ifcowl_ttl   # -> Optional[str]
```

The singleIngestConverter's `module-ifc2lbd/server.py` replaces the 176MB JAR subprocess call with a Python import. Alternatively, ifc2lbd-neo can expose its own HTTP endpoint via `axum` (~20MB container vs 176MB JAR + JRE).

## Key Rust Dependencies

- **Parsing**: `memmap2`, `smol_str`, `bumpalo` (arena allocator)
- **Parallelism**: `rayon`, `crossbeam`, `dashmap`
- **RDF**: `rio_turtle` (fast Turtle serializer), `rio_api` (RDF types)
- **Spatial**: `rstar` (R-tree)
- **CLI**: `clap`
- **Python**: `pyo3`, `maturin`
- **Logging**: `tracing`
- **Testing**: `criterion` (benchmarks), `proptest` (property-based)
- **Compression**: `flate2` (ifcZip support)

## Files to Reference During Implementation

- `IFCtoLBD/IFCtoLBD/src/main/java/org/linkedbuildingdata/ifc2lbd/core/IFCtoLBDConverterCore.java` -- Complete conversion algorithm
- `IFCtoLBD/IFCtoRDF/src/main/java/be/ugent/IfcSpfParser.java` -- STEP parser
- `IFCtoLBD/IFCtoLBD/src/main/java/org/linkedbuildingdata/ifc2lbd/core/valuesets/PropertySet.java` -- Property extraction at OPM levels
- `IFCtoLBD/IFCtoLBD_Geometry/src/main/java/de/rwth_aachen/dc/lbd/IFCGeometry.java` -- IfcOpenShell integration
- `IFC2BOT/IFC2BOT/space_adjacency.py` -- IfcRelSpaceBoundary adjacency logic
- `singleIngestConverter/services/module-ifc2lbd/server.py` -- Integration point to replace
