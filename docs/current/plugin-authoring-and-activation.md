# Module Authoring and Activation

This document defines the current module workflow for IFC2LBD-Neo.

## Activation Flags

- `--list-modules`: print registered module manifests.
- `--enable-module <module-id>`: activate one or more modules explicitly.
- `--module-config <module-id>:<key>=<value>`: attach module config values.
- `--show-module-plan`: print resolved activation result and exit.

Example:

```bash
target/release/ifc2lbd-neo model-A.ifc \
  --output out.nq \
  --output-format nquads \
  --enable-module neo-topology-full-producer \
  --module-config neo-topology-full-producer:max_pairs=12000 \
  --show-module-plan
```

Typed config keys currently supported:

- `neo-topology-full-producer:kernel_timeout_secs` (integer > 0)
- `neo-topology-full-producer:max_pairs_per_batch` (integer > 0)

## Current Producer Module Execution Rules

- Producer modules run in parallel task fan-out in `nquads` mode.
- Optional producer module failure is isolated and does not fail the full run.
- Required producer module failure fails the run.
- Multiple topology producers are only allowed in `nquads` mode.

## Scaffold a New Producer Module Crate

Use the template generator:

```bash
python3 scripts/scaffold_producer_plugin.py --id voxels --display-name "Voxel Producer"
```

This creates:

- `crates/plugin-voxels/Cargo.toml`
- `crates/plugin-voxels/src/lib.rs`
- `crates/plugin-voxels/README.md`

## Register a New Module

After scaffolding:

1. Add the crate to workspace members in root `Cargo.toml`.
2. Register module manifest in `crates/ifc2lbd-cli/src/pipeline_plugins.rs`.
3. Add one runtime executor entry in `crates/ifc2lbd-cli/src/topology_plugin.rs` (`TOPOLOGY_EXECUTORS`).
4. Validate:

```bash
cargo check -p ifc2lbd-cli
cargo test -p ifc2lbd-cli pipeline_plugins
```

## Benchmark Requirement

Before merging module changes, run model A regression:

```bash
/usr/bin/time -f 'wall=%e rss_kb=%M user=%U sys=%S exit=%x' \
  target/release/ifc2lbd-neo model-A.ifc \
  --output tmp/digitalhub_plugin_test.nq \
  --base-uri https://benchmark.test/digitalhub/ \
  --output-format nquads \
  --quad-chunking cores \
  --enable-module neo-topology-full-producer \
  --bbox
```
