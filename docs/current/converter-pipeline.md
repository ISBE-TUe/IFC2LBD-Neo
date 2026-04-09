# Converter Pipeline and Module Extension Guide

This document defines the engineering contract for `crates/lbd-converter`.

It is the source of truth for:

- pipeline order,
- module responsibilities,
- extension workflow,
- output-stability rules.

## Design Goals

- Make converter behavior easy to extend without destabilizing output.
- Keep LBD/IfcOWL semantics deterministic and testable.
- Keep streaming and non-streaming conversion paths aligned.
- Isolate concerns into composable emitter modules.

## Current Runtime Flow

Entry points in `crates/lbd-converter/src/lib.rs`:

- `convert_step_and_model`
- `convert_model`
- `stream_step_and_model`

High-level flow:

1. Normalize base URI.
2. Convert IfcOWL triples (sidecar graph path).
3. Emit LBD triples through `emit_lbd`.
4. Serialize in downstream crate (`lbd-serializer`).

## Emitter Module Pattern

Each LBD concern should be implemented as one module with one focused emitter function:

```rust
pub(crate) fn emit_xxx<E, F>(
    model: &IfcModel,
    options: &ConvertOptions,
    base: &str,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>
```

Why this is required:

- Same code works for buffered conversion and streaming conversion.
- Avoids materializing full intermediate graphs.
- Keeps error handling generic and composable.

## Module Layout

Module index:

- `crates/lbd-converter/src/modules/mod.rs`

Extracted module today:

- `crates/lbd-converter/src/modules/core_entities.rs`
  - spatial typing,
  - element typing/product typing,
  - spatial hierarchy edges,
  - optional `owl:sameAs` emission.
- `crates/lbd-converter/src/modules/ifcowl.rs`
  - IfcOWL batch conversion path.
  - IfcOWL streaming conversion path.
  - IfcOWL triple deduplication in module-local helper.

Planned extraction order (behavior-preserving):

1. `core_entities` (done)
2. `ifcowl` (done)
3. `topology`
4. `containment_fallback`
5. `property_sets`
6. `quantity_sets`
7. `bounding_box_geometries`
8. `standard_attributes`

## Output Stability Contract

Unless explicitly scoped as a semantic change, refactors must preserve:

- URI shape and namespace strategy,
- predicate choices,
- topology module behavior,
- OPM property/state modeling semantics,
- triple determinism assumptions used by tests.

## Invariants

These invariants are expected across releases:

- IfcOWL producer activation emits sidecar/named-graph IfcOWL output and keeps links in LBD via `owl:sameAs`.
- Topology triples are emitted only when topology producer modules are active.
- `neo-topology-lite-producer` is IFC-relation topology mode.
- `neo-topology-full-producer` is advanced topology mode.
- Bounding boxes are emitted only when `neo-bbox-enricher` is active.
- Bounding boxes are represented via geometry resources (`lbd:hasBoundingBox`, `geo:hasGeometry`, `geo:asWKT`).
- Property states remain queryable via OPM-compatible predicates.

## Determinism Rules

- Sort IDs before emission when source maps are unordered.
- Keep module invocation order explicit in `emit_lbd`.
- Avoid hidden nondeterminism in hash-map iteration.
- Keep serializer dedup/order behavior compatible with tests.

## Extension Workflow (Best Practice)

1. Create a module file under `crates/lbd-converter/src/modules/`.
2. Register it in `modules/mod.rs`.
3. Move logic by extraction first, not rewrite.
4. Keep helper visibility minimal (`pub(crate)` only when needed).
5. Wire module call into `emit_lbd` in deterministic order.
6. Add or update tests for moved behavior.
7. Run validation commands and capture results in PR description.

## Validation Commands

```bash
cargo fmt
cargo test -p lbd-converter
cargo check -p ifc2lbd-cli
```

For behavior-sensitive changes, also run fixture and benchmark scripts:

```bash
python3 scripts/run_allowed_fixtures.py
python3 scripts/run_release_benchmarks.py
```

## PR Checklist for Converter Refactors

- Refactor scope is clearly separated from feature scope.
- Output semantics preserved (or change explicitly documented).
- Tests pass and cover moved code paths.
- Documentation updated in same PR.
- Any residual risk or TODO is explicitly noted.

## Known Current Gap

- `neo-topology-full-producer` is now wired through the OCC exact-kernel path from the CLI, but parts of the converter implementation are still more monolithic than the target module layout described above.
