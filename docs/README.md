# Documentation

## Architecture

- [Converter Pipeline](./converter-pipeline.md) — stage order, threading model, channel backpressure
- [Vocabulary Fixes — Handoff](./vocabulary-fixes-handoff.md) — the cn3-pt1 audit that found terms resolving to nothing
- [Vocabulary Fixes — Plan](./vocabulary-fixes-plan.md) — what was changed, where, and what remains open

## Plugin Development

- [Plugin Authoring and Activation](./plugin-authoring-and-activation.md) — canonical guide; start here
- [Agent Plugin Instructions](./agent-plugin-instructions.md) — AI-agent-specific workflow for building plugins

Template crates (copy, don't modify): `crates/plugin-template-{preprocess,producer,postprocess,export}/`

## Operations

- [Contributing](./contributing.md)
- [Testing and Benchmarking](./testing-and-benchmarking.md)
