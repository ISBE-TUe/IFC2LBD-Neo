# Contributing Guide

This guide defines contribution standards for maintainable, predictable converter evolution.

## Principles

- Preserve semantic stability unless a change is explicitly scoped as behavior-changing.
- Keep output deterministic for reproducible diffs and tests.
- Prefer small, focused pull requests over mixed refactor + feature bundles.
- Document design intent with every non-trivial change.

## Branch and PR Scope

- One concern per PR.
- Refactor PRs should not change output semantics.
- Feature PRs must include tests and docs updates.
- If a change impacts CLI behavior, update README examples and flag docs.

## Code Standards

- Use Rust idioms and keep functions focused.
- Avoid broad mutable shared state; pass explicit context.
- Keep module APIs narrow (`pub(crate)` where possible).
- Add comments only where intent is not obvious from code.

## Converter-Specific Rules

- Do not change URI generation format in refactor-only work.
- Keep OPM-compatible modeling for property/state semantics.
- Keep topology behavior tied to explicit flags.
- Keep IfcOWL output in separate file behavior unchanged.
- Keep stable ordering before emission where ordering affects output.

## Tests Required by Change Type

- Refactor-only: existing test suite must pass; add regression test if moving logic had bug risk.
- New converter behavior: unit tests + at least one fixture-based integration check.
- CLI behavior: argument parsing and validation tests.
- Serializer behavior: output normalization/dedup and prefix behavior tests.

## Documentation Required by Change Type

- Any converter pipeline change: update `docs/current/converter-pipeline.md`.
- Any new flag or changed flag semantics: update `README.md` and CLI docs.
- Any new benchmark workflow: update `docs/current/testing-and-benchmarking.md`.

## Review Checklist

- Does this change alter TTL semantics intentionally?
- Are ordering and determinism preserved?
- Are tests sufficient for the changed area?
- Are performance implications measured if hot path changed?
- Is documentation updated in the same PR?

## Commit Message Guidance

Use clear imperative subjects, for example:

- `converter: extract core entity emitter module`
- `cli: simplify topology flags`
- `docs: add module extension checklist`
