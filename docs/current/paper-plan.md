# Paper Plan (Current)

This is the authoritative paper-planning document for the `ifc2lbd-neo` EG-ICE manuscript work.

Use this file instead of the old archive note when describing:

- architecture
- evaluation scope
- benchmark expectations
- writing rules
- future work positioning

## Scope

The paper is about the current Rust rewrite and extension of the Java IFCtoLBD line.

The framing is fixed:

- respectful lineage, not "replacement" rhetoric
- direct comparison with the Java-based predecessor
- code-backed architecture claims only
- benchmark-backed performance claims only
- a substantial roadmap section that separates future work from current implementation

## Source Of Truth Order

When drafting the paper, use this order:

1. code
2. `README.md`
3. `docs/current/*`
4. reference/archive notes

If a current doc conflicts with code-backed behavior, treat the doc as stale and do not repeat the stale claim in the paper.

## Architecture Reality Check

These points must be treated as current.

### Current Behavior

- `neo-topology-full-producer` is currently wired through the OCC exact-kernel subprocess path in [`crates/ifc2lbd-cli/src/main.rs`](../../crates/ifc2lbd-cli/src/main.rs).
- Property and quantity set resources are already emitted in [`crates/lbd-converter/src/lib.rs`](../../crates/lbd-converter/src/lib.rs) with `lbd:hasPropertySets`, `lbd:PropertySet`, `lbd:hasQuantitySet`, and `lbd:ElementQuantitySet`.
- Chunked N-Quads output can separate LBD, IfcOWL, and topology streams into named-graph chunk manifests.
- The current architecture supports adding new built-in flags and conversion stages.

### Known Documentation Drift

- [`docs/archive/paper-notes.md`](../archive/paper-notes.md) is historical only.
- Some current docs still describe full topology as voxel-based. That description is stale for the active main path.
- Older notes that present property/quantity set resources as future-only are stale.
- Loose wording such as "separate topology TTL" should be replaced with precise named-graph/chunked-output language.

## Paper Positioning

The paper should present:

1. a Rust rewrite in the IFCtoLBD lineage
2. a changed execution architecture
3. a compatibility/parity story against the Java reference implementation
4. a measured performance and deployment story
5. a realistic roadmap for extensibility and future topology/geometry work

The paper should not present:

- the Java converter as an inferior strawman
- every semantic difference as an improvement
- future architectural goals as if they are already delivered

## Locked Outline

1. Introduction
2. Background And Prior Work
3. Lineage, Design Goals, And Rewrite Framing
4. Current Rust Converter Architecture
5. Compatibility And Semantic Comparison With The Java Converter
6. Evaluation And Benchmarks
7. Discussion And Threats To Validity
8. Outlook And Next Steps
9. Conclusion

The old 2018 paper must be cited in:

- Introduction
- Background
- comparison framing
- discussion of continuity and change

## Current Architecture Points To Describe

Describe these features exactly as they exist today:

- direct IFC STEP parsing
- typed IFC model construction
- streaming LBD conversion
- IfcOWL sidecar or named-graph output
- chunked N-Quads for ingest workflows
- relation-based topology via `neo-topology-lite-producer`
- exact-kernel-backed full topology path via `neo-topology-full-producer`
- bounding-box geometry emission via `neo-bbox-enricher`

Do not overstate:

- full IFC4 parity
- dynamic module loading support
- universal exact-geometry coverage

## Extensibility Status

### Supported Today

The current architecture already supports controlled extension for new built-in conversion capabilities:

- new CLI flags can be added centrally in [`crates/ifc2lbd-cli/src/main.rs`](../../crates/ifc2lbd-cli/src/main.rs)
- new converter options can be threaded through `ConvertOptions` in [`crates/lbd-converter/src/lib.rs`](../../crates/lbd-converter/src/lib.rs)
- new conversion concerns can be extracted into focused emitter modules following [`converter-pipeline.md`](./converter-pipeline.md)

### Not Supported Today

The current architecture is not yet a true dynamic plugin system:

- no runtime dynamic loading or discovery of conversion modules
- no third-party dynamic module API for adding flags or emitters independently of the codebase

Paper wording:

- say "extension-friendly module architecture for built-in conversion features"
- do not say "runtime dynamic plugin architecture" for the current implementation

## Benchmark Plan

Use a reproducible core set plus one large-model scalability case.

### Fixtures

- `Duplex.ifc`: main parity oracle
- `DigitalHub_FM-ARC_v2.ifc`: main IFC4 comparison case
- `Wohn-Geschaeftshaus.ifc`: important topology case, especially for diagonal-wall behavior and bbox-based logic
- one locked large IFC model in the ~170 MB class: scalability case only, not a clean Java comparison oracle

### Per-Run Requirements

Record:

- exact command
- Rust commit context
- Java jar path and version
- host environment
- fixture size
- wall time
- max RSS
- output sizes
- triple counts
- normalized compare mode and result
- selected query buckets
- whether Java is treated as a clean oracle

### Comparison Categories

Keep these separate in the manuscript:

- semantic parity
- performance comparison
- scalability/deployment capability

Large-model rule:

- if the Java baseline runs out of memory on the large fixture, state that directly and keep that case separate from semantic-parity evaluation

## Figure And Plot Plan

The paper should contain both quantitative plots and explanatory pipeline figures.

### Quantitative Plots

- reuse the Python plotting scripts already present in the repo where they fit the final evaluation
- show wall time, max RSS, output size, and other metrics only when they help the argument
- include at least one large-model plot that makes the scale of the output visible

### Explanatory Figures

- include one architecture overview figure for the main pipeline
- include one decision-flow or escalation figure for topology/full-topology behavior
- explain:
  - what is derived only from IFC relations
  - what enters candidate generation
  - where bounding boxes are used
  - where exact checks are used
  - where escalation/fallback logic applies

## Writing Rules

- Treat the Java converter as foundational prior work.
- Use "Java-based predecessor", "original converter", or "reference implementation".
- Never claim full parity without naming fixture, normalization mode, and graph scope.
- Separate current implementation from future work.
- Every novelty claim must be backed by code or measured evidence.
- Do not copy stale archive wording into the paper.
- Use exact flags, fixtures, versions, and dates for experiments.
- When Rust differs from Java, label the difference as intentional, improved, unresolved, or not directly comparable.
- Use scientific language, but keep it plain and readable.
- Prefer short, concise sentences.
- Avoid inflated jargon and unnecessarily complex sentence structure.
- Avoid em-dash-heavy sentence constructions and other AI-sounding stylistic habits.

## Citation Rules

- add every cited work to `refs.bib`
- verify that each cited paper really exists
- double-check DOI, authors, title, venue, year, and page information
- use correct BibTeX entry types
- prefer authoritative metadata over copied third-party entries
- do not leave placeholder or unchecked BibTeX records in the final paper plan or manuscript

## Outlook And Next Steps

This must be a large standalone section in the paper.

### Short-Term

- align current docs with code
- rerun the locked benchmark suite
- generate final figures and tables
- stabilize architecture wording for the manuscript
- package reproducibility details cleanly

### Medium-Term

- improve IFC4 parity where gaps remain
- strengthen difficult-model handling
- expand exact-kernel coverage
- clarify evaluation boundaries on non-oracle fixtures
- continue moving monolithic conversion logic into dedicated emitter modules

### Longer-Term

- richer topology and geometry support
- broader hard-geometry coverage
- larger benchmark campaigns
- improved ingestion/deployment workflows
- first-class extension registration for future conversion additions

Future extensibility statement:

- the current architecture is already suitable for new built-in flags and new conversion modules
- a true dynamic plugin mechanism is future work, not a current claim
