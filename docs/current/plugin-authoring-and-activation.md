# Plugin Authoring and Activation

This document defines the current plugin workflow for IFC2LBD-Neo.

## Activation Flags

- `--list-plugins`: print registered plugin manifests.
- `--enable-plugin <plugin-id>`: activate one or more plugins explicitly.
- `--plugin-config <plugin-id>:<key>=<value>`: attach plugin config values.
- `--show-pipeline-plan`: print resolved activation result and exit.

Example:

```bash
target/release/ifc2lbd-neo model-A.ifc \
  --output out.nq \
  --output-format nquads \
  --enable-plugin builtin-topology-full-producer \
  --plugin-config builtin-topology-full-producer:max_pairs=12000 \
  --show-pipeline-plan
```

## Current Producer Execution Rules

- Producer plugins run in parallel task fan-out in `nquads` mode.
- Optional producer plugin failure is isolated and does not fail the full run.
- Required producer plugin failure fails the run.
- Multiple topology producers are only allowed in `nquads` mode.

## Scaffold a New Producer Plugin Crate

Use the template generator:

```bash
python3 scripts/scaffold_producer_plugin.py --id voxels --display-name "Voxel Producer"
```

This creates:

- `crates/plugin-voxels/Cargo.toml`
- `crates/plugin-voxels/src/lib.rs`
- `crates/plugin-voxels/README.md`

## Register a New Plugin

After scaffolding:

1. Add the crate to workspace members in root `Cargo.toml`.
2. Register plugin manifest in `crates/ifc2lbd-cli/src/pipeline_plugins.rs`.
3. Add one runtime executor entry in `crates/ifc2lbd-cli/src/topology_plugin.rs` (`TOPOLOGY_EXECUTORS`).
4. Validate:

```bash
cargo check -p ifc2lbd-cli
cargo test -p ifc2lbd-cli pipeline_plugins
```

## Benchmark Requirement

Before merging plugin changes, run model A regression:

```bash
/usr/bin/time -f 'wall=%e rss_kb=%M user=%U sys=%S exit=%x' \
  target/release/ifc2lbd-neo model-A.ifc \
  --output tmp/digitalhub_plugin_test.nq \
  --base-uri https://benchmark.test/digitalhub/ \
  --output-format nquads \
  --quad-chunking cores \
  --topology-full --bbox
```
