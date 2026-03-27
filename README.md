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

Notes:
- In `nquads` mode, IfcOWL is emitted automatically.
- With chunking enabled, output is split per stream (`lbd`, `ifcowl`, and `topology` when enabled).
- Auto chunking targets practical chunk sizes (about `64–512 MiB`).

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
