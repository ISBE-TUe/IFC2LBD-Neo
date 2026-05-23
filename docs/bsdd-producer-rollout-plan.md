# bSDD Producer Rollout Plan

Status: draft architecture note for `neo-bsdd-producer`

## Goal
Add a bSDD-aware producer that enriches IFC2LBD-Neo output with canonical buildingSMART Data Dictionary URIs, while preserving current BOT/BEO/OPM behavior and CLI↔WASM parity.

## Constraints
- Keep existing pipeline stages unchanged.
- Implement as `ProducerPlugin` and register in both runners.
- Follow module-first activation (`--module neo-bsdd-producer`).
- Start optional (`FailurePolicy::Optional`) to avoid hard pipeline failures on mapping gaps.

## Decision Summary
- Do **not** convert IFC2x3/IFC4 models to IFC4.3 as a preprocessing rewrite.
- Keep source IFC semantics and map to bSDD IFC 4.3 URIs where safe.
- Keep OPM for value/state modeling; use bSDD URIs for semantic identity.
- Start additive (new `/bsdd` graph), do not replace `/beo` or `/props` immediately.

## Phases

### Phase 1: Additive semantic links
- New producer emits only bSDD links:
  - element -> bSDD class URI
  - property state/property resource -> bSDD property URI
- No removal/change to existing BEO or PROPS/OPM triples.
- Local cache for class/property lookups keyed by URI.
- Output coverage counters (mapped/unmapped entities/properties).

### Phase 2: Cross-version crosswalks
- Add deterministic crosswalk tables for IFC2x3/IFC4 names to IFC4.3 bSDD URIs.
- Keep unmapped terms explicit (marker triples + diagnostics).
- Add strict mode for CI/use-cases that require full semantic binding.

### Phase 3: Consolidation
- Evaluate whether portions of `neo-beo-producer` and/or `neo-props-opm` can be deprecated.
- Only deprecate where bSDD coverage and stability are proven.
- Preserve backward compatibility by transition period and clear module docs.

## Runtime/Data Strategy
- Prefer API endpoint calls against `api.bsdd.buildingsmart.org` for selected URIs.
- Avoid full bSDD ingestion at startup.
- Add optional offline snapshot mode later for reproducibility.

## Open Design Questions
- Canonical predicates for semantic binding (e.g., `owl:sameAs` vs dedicated predicate).
- Whether bSDD links attach to `opm:Property`, `opm:CurrentPropertyState`, or both.
- Final named graph slug (`/bsdd`) and interaction with merged Turtle layout.
