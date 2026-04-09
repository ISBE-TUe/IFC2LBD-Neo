# Module Authoring and Activation

This document defines the current module-only workflow for IFC2LBD-Neo.

## Activation Flags

- `--module <module-id>`: activate one or more modules explicitly.
- `--module-opt <module-id>.<key>=<value>`: attach typed module options.
- `--list-modules`: print registered module manifests.
- `--show-module-plan`: print resolved activation result and exit.

Example:

```bash
target/release/ifc2lbd-neo DigitalHub_FM-ARC_v2.ifc \
  --output out.nq \
  --module neo-lbd-producer \
  --module neo-topology-full-producer \
  --module neo-nquads-serializer \
  --module neo-file-export \
  --module-opt neo-topology-full-producer.max_pairs_per_batch=12000 \
  --show-module-plan
```

## Typed Module Options

`neo-topology-full-producer`:

- `kernel_timeout_secs` (integer > 0)
- `max_pairs_per_batch` (integer > 0)

`neo-nquads-serializer`:

- `chunking` = `none|lines|bytes|cores`
- `chunk_size_lines` (integer > 0)
- `chunk_size_bytes` (integer > 0)
- `chunk_prefix` (string)
- `chunk_min_count` (integer > 0)
- `chunk_core_count` (integer > 0, only with `chunking=cores`)
- `lbd_graph_iri` (string)
- `ifcowl_graph_iri` (string)

`neo-bbox-enricher`:

- `inflation_threshold` (float > 0)
- `report_path` (path)

## Current Producer Module Execution Rules

- Producer modules run in parallel task fan-out when N-Quads serializer is active.
- Optional producer module failure is isolated and does not fail the full run.
- Required producer module failure fails the run.
- Multiple topology producers are only allowed with N-Quads serializer.

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

Before merging module changes, run DigitalHub regression:

```bash
/usr/bin/time -f 'wall=%e rss_kb=%M user=%U sys=%S exit=%x' \
  target/release/ifc2lbd-neo DigitalHub_FM-ARC_v2.ifc \
  --output tmp/digitalhub_module_test.nq \
  --base-uri https://benchmark.test/digitalhub/ \
  --module neo-lbd-producer \
  --module neo-ifcowl-producer \
  --module neo-topology-full-producer \
  --module neo-bbox-enricher \
  --module neo-nquads-serializer \
  --module neo-file-export \
  --module-opt neo-nquads-serializer.chunking=cores \
  --module-opt neo-bbox-enricher.inflation_threshold=1.5
```
