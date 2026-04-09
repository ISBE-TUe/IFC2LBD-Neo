# IFC2LBD-Neo

Rust converter from IFC STEP to LBD/IfcOWL in Turtle or N-Quads.

## Quick Start

Recommended (N-Quads + auto chunking):

```bash
ifc2lbd-neo model.ifc \
  --output out.nq \
  --base-uri https://example.test/base/ \
  --output-format nquads \
  --quad-chunking cores \
  --topology-full \
  --bbox
```

Basic Turtle:

```bash
ifc2lbd-neo model.ifc --output out.ttl
```

Turtle with IfcOWL sidecar:

```bash
ifc2lbd-neo model.ifc --output out.ttl --ifcowl
```

## Main Flags

- `--output`
- `--base-uri`
- `--output-format <turtle|nquads>`
- `--quad-chunking <none|cores>`
- `--topology`
- `--topology-full`
- `--bbox`
- `--list-plugins`
- `--enable-plugin <plugin-id>`
- `--plugin-config <plugin-id>:<key>=<value>`
- `--show-pipeline-plan`

Notes:
- In `nquads` mode, IfcOWL is emitted automatically.
- With chunking enabled, output is split per stream (`lbd`, `ifcowl`, and `topology` when enabled).
- Auto chunking targets practical chunk sizes (about `64–512 MiB`).

## Built-In Plugins (Current)

Preprocess:
- none currently.

Produce:
- `builtin-lbd-producer`
- `builtin-ifcowl-producer`
- `builtin-topology-lite-producer`
- `builtin-topology-full-producer`

Serialize:
- `builtin-turtle-serializer`
- `builtin-nquads-serializer`

Export:
- `builtin-file-export`
- `builtin-stdout-export`
- `builtin-grafeo-export`

## Oxigraph Streaming Load (Chunked N-Quads)

Load chunks directly from manifests (no merge step needed):

```bash
jq -r '.files[].file' out-ifcowl.manifest.json | while read -r f; do
  oxigraph_server load --file "$f" --format nquads
done

jq -r '.files[].file' out-lbd.manifest.json | while read -r f; do
  oxigraph_server load --file "$f" --format nquads
done

jq -r '.files[].file' out-topology.manifest.json | while read -r f; do
  oxigraph_server load --file "$f" --format nquads
done
```

## Build

```bash
cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo
cargo build --release -p lbd-geometry-kernel --bin lbd-geometry-kernel
```

Prebuilt Linux amd64 binaries are stored in `artifacts/bin/`.

## Plugin Workflow

Inspect available plugins:

```bash
target/release/ifc2lbd-neo --list-plugins
```

Show the resolved activation plan before a run:

```bash
target/release/ifc2lbd-neo model-A.ifc \
  --output out.nq \
  --output-format nquads \
  --enable-plugin builtin-topology-full-producer \
  --show-pipeline-plan
```

Scaffold a new producer plugin crate template:

```bash
python3 scripts/scaffold_producer_plugin.py --id voxels --display-name "Voxel Producer"
```

For topology-producer execution wiring, add one executor entry in `crates/ifc2lbd-cli/src/topology_plugin.rs` (`TOPOLOGY_EXECUTORS`).

Detailed plugin instructions:
- `docs/current/plugin-authoring-and-activation.md`
- `docs/current/agent-plugin-instructions.md`
