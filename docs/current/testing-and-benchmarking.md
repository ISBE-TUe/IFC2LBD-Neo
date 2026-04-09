# Testing and Benchmarking

This document defines how to validate correctness and performance of `ifc2lbd-neo`.

## Test Layers

1. Unit tests (fast)
- Located in each crate.
- Validate local logic: IRI generation, property state modeling, topology merge behavior, decimal canonicalization.

2. Integration checks (medium)
- Run converter on representative IFC fixtures.
- Validate key triple patterns and expected output shape.

3. Benchmark runs (slow)
- Measure runtime and memory trends on selected fixtures.
- Compare before/after for performance-sensitive changes.

## Standard Commands

```bash
cargo test
cargo test -p lbd-converter
cargo check -p ifc2lbd-cli
python3 scripts/run_allowed_fixtures.py
python3 scripts/run_release_benchmarks.py
```

## Fixture Policy

- Keep heavy IFC fixtures out of git unless strictly required.
- Scripts should skip missing fixtures instead of hard-failing.
- Prefer a small stable set of representative fixtures for regression confidence.

## What to Verify for Converter Changes

- LBD-only mode: no topology triples unless enabled.
- IfcOWL producer active: sidecar/named-graph IfcOWL output and `owl:sameAs` links in LBD.
- `neo-topology-lite-producer`: IFC-relation topology in LBD output.
- `neo-topology-full-producer`: advanced topology mode behavior matches expectations.
- `neo-bbox-enricher` active: geometry nodes + `geo:asWKT` are emitted.
- Property/state modeling remains queryable and OPM-compatible.

## Determinism Checks

- Repeat conversion on same input and compare normalized output.
- Ensure stable ordering of emitted triples where expected.
- Keep serializer dedup behavior intact.

## Performance Checklist

When modifying hot paths (`lbd-converter`, `lbd-serializer`, topology merge):

- Run benchmark scripts before and after.
- Record wall-time deltas and memory observations.
- Note any known trade-off (for example, extra triples for better queryability).

## Minimum PR Evidence for Performance-Sensitive Changes

- Commands executed.
- Fixture(s) used.
- Before/after summary.
- Any caveats (missing fixtures, environment limits).
