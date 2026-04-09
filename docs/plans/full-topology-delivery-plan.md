# Full Topology Delivery Plan

This plan defines a practical path to production-grade `--topology-full`.

Goal:
- deliver high-confidence BOT topology with complete interface relations,
- keep runtime predictable on large IFC models,
- preserve deterministic output and explainability.

## Target End State

`--topology-full` should produce:
- BOT core hierarchy from IFC relations,
- geometry-confirmed topology enrichment (`bot:intersectingElement`, `bot:interfaceOf`),
- optional `neo-bbox-enricher` output with GeoSPARQL WKT bbox geometry,
- stable runtime and memory envelopes on benchmark fixtures.

## Non-Goals (Current Iteration)

- full 3D minimum-volume OBB for all elements,
- arbitrary freeform constructive-solid semantics in RDF,
- browser/WebAssembly exact-kernel path.

## Phase 0: Baseline and Instrumentation

Scope:
- freeze current behavior and benchmark baselines,
- make evidence visible for performance and quality regressions.

Deliverables:
- benchmark table for topology and bbox modules (`neo-topology-lite-producer`, `neo-topology-full-producer`, `neo-bbox-enricher`),
- bbox report metrics persisted in CI artifacts for selected fixtures,
- topology relation counts snapshot per fixture.

Acceptance:
- no unknown performance regressions (>15%) without explanation,
- deterministic relation counts on repeated runs.

## Phase 1: Topology Candidate Quality

Scope:
- improve candidate pair generation before expensive checks.

Work:
- enforce strict structural filtering rules,
- maintain storey/zone scoping constraints,
- add diagnostics (`candidates_total`, `filtered_by_scope`, `filtered_by_type`).

Acceptance:
- candidate count reduction on large fixtures without relation-loss regressions,
- identical or better precision against manual inspection samples.

## Phase 2: Narrow-Phase Geometry Validation

Scope:
- replace/augment voxel-only confirmation for high-risk pairs.

Work:
- integrate exact-kernel adapter contract from `geometry-kernel-plan.md`,
- run bbox broad-phase + exact narrow-phase,
- keep provenance on derived edges (`derived_from` evidence tags).

Acceptance:
- known false positives from voxel path reduced,
- topology output remains BOT-compliant and deterministic.

## Phase 3: Interface Semantics Hardening

Scope:
- make `bot:Interface` creation rules explicit and testable.

Work:
- define interface creation matrix by source evidence,
- enforce no orphan interface nodes,
- add fixture tests asserting bidirectional consistency.

Acceptance:
- every emitted interface links to >=2 valid elements,
- no interface duplication for same element pair/evidence class.

## Phase 4: Performance Budgeting

Scope:
- ensure operational feasibility for production model sizes.

Budgets (initial targets):
- `DigitalHub_FM-ARC_v2.ifc`: `< 30s`, `< 1.2GB` peak RSS for full topology + bbox modules,
- `Wohn-Geschaeftshaus.ifc`: `< 25s`, `< 1.0GB` peak RSS for full topology + bbox modules.

Work:
- parallelize narrow-phase safely,
- add timeout and fallback policy for exact-kernel stage,
- profile hotspots and enforce max-candidate guardrails.

Acceptance:
- budgets met on release builds on reference machine class,
- graceful degradation path documented if exact-kernel unavailable.

## Phase 5: Release and Validation Pack

Scope:
- ship full-topology mode as documented default for advanced users.

Deliverables:
- updated README and current docs,
- query cookbook for topology QA (counts, interface checks, adjacency checks),
- fixture-based validation report committed under `artifacts/benchmarks/`.

Acceptance:
- reproducible validation run with published commands and outputs,
- no open critical correctness issues.

## Test Strategy

Required checks per phase:
- `cargo test`
- fixture run on at least:
  - `DigitalHub_FM-ARC_v2.ifc`
  - `Wohn-Geschaeftshaus.ifc`
- topology count diff against previous baseline
- manual inspection sample for 3-5 known rotated/non-orthogonal elements.

## Risks and Mitigations

- Risk: exact-kernel integration increases latency.
  - Mitigation: strict broad-phase pruning + candidate caps + timeouts.
- Risk: semantic drift in interface generation.
  - Mitigation: invariant tests + evidence tags + deterministic sorting.
- Risk: overfitting thresholds to one fixture.
  - Mitigation: multi-fixture threshold sweeps and report-driven defaults.

## Immediate Next Sprint

1. finalize and freeze bbox fallback default (`1.5`) and reporting schema.
2. implement exact-kernel adapter wiring behind current topology-full path.
3. add topology QA queries and expected ranges for two core fixtures.
4. run full benchmark + publish regression table.
