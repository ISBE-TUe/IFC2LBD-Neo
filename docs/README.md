# Documentation

## Architecture

- [Converter Pipeline](./converter-pipeline.md) — stage order, threading model, channel backpressure
- [WASM vs CLI Comparison](./wasm-vs-cli-comparison.md) — where the two runners diverge
- [Full Topology Workflow](./topology-full-workflow.md) — geometry kernel, adjacency, topology producer

## Plugin Development

- [Plugin Authoring and Activation](./plugin-authoring-and-activation.md) — canonical guide; start here
- [Agent Plugin Instructions](./agent-plugin-instructions.md) — AI-agent-specific workflow for building plugins

Template crates (copy, don't modify): `crates/plugin-template-{preprocess,producer,postprocess,export}/`

## Operations

- [Contributing](./contributing.md)
- [Testing and Benchmarking](./testing-and-benchmarking.md)
- [Oxigraph Loading](./oxigraph-loading.md) — how to load chunked N-Quads output into Oxigraph

## Plans

- [WebAssembly Plan](./future-wasm-plan.md) — browser WASM delivery phases
- [Dynamic WASM Plugins](./plan-dynamic-plugins.md) — Grasshopper-style local plugin directory, WASM ABI, GitHub Actions CI/CD
