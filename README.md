# IFC2LBD-Neo

High-performance Rust converter from IFC STEP to LBD/IfcOWL with a module-first pipeline.

## CLI Model

The CLI is module-driven:

- `--module <id>` activates a module (repeatable).
- `--module-opt <module-id>.<key>=<value>` configures module options (repeatable).
- `--show-module-plan` prints the resolved module set and options.

No profiles are required for normal usage.

## Quick Start

LBD Turtle to file:

```bash
ifc2lbd-neo model.ifc \
  --output out.ttl \
  --base-uri https://example.test/base/ \
  --module neo-lbd-producer \
  --module neo-turtle-serializer \
  --module neo-file-export
```

LBD + full topology N-Quads (no IfcOWL):

```bash
ifc2lbd-neo model.ifc \
  --output out.nq \
  --base-uri https://example.test/base/ \
  --module neo-lbd-producer \
  --module neo-topology-full-producer \
  --module neo-nquads-serializer \
  --module neo-file-export
```

Full pipeline (LBD + IfcOWL + full topology + bbox) in N-Quads with core chunking:

```bash
ifc2lbd-neo model.ifc \
  --output out.nq \
  --base-uri https://example.test/base/ \
  --module neo-lbd-producer \
  --module neo-ifcowl-producer \
  --module neo-topology-full-producer \
  --module neo-bbox-enricher \
  --module neo-nquads-serializer \
  --module neo-file-export \
  --module-opt neo-nquads-serializer.chunking=cores \
  --module-opt neo-bbox-enricher.inflation_threshold=1.5
```

## Built-In Neo Modules

| Stage | Module ID | Purpose |
|---|---|---|
| Produce | `neo-lbd-producer` | Emit LBD triples |
| Produce | `neo-ifcowl-producer` | Emit IfcOWL triples |
| Produce | `neo-topology-lite-producer` | BOT topology from IFC relationships |
| Produce | `neo-topology-full-producer` | BOT topology with OCC geometry refinement |
| Produce | `neo-bbox-enricher` | Compute bbox geometry enrichment |
| Serialize | `neo-turtle-serializer` | Serialize Turtle |
| Serialize | `neo-nquads-serializer` | Serialize N-Quads (merged/chunked) |
| Export | `neo-file-export` | Write files/manifests |
| Export | `neo-stdout-export` | Write to stdout |
| Export | `neo-grafeo-export` | Direct Grafeo framed stream |

## Module Options

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
- `lbd_graph_iri` (string IRI)
- `ifcowl_graph_iri` (string IRI)

`neo-bbox-enricher`:

- `inflation_threshold` (float > 0)
- `report_path` (path)

## Discovery and Plan

```bash
ifc2lbd-neo --list-modules
ifc2lbd-neo model.ifc ... --show-module-plan
```

## Build

```bash
cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo
cargo build --release -p lbd-geometry-kernel --bin lbd-geometry-kernel
```

## Documentation

- Module authoring and activation: `docs/current/plugin-authoring-and-activation.md`
- Agent module instructions: `docs/current/agent-plugin-instructions.md`
- Oxigraph loading/chunking: `docs/current/oxigraph-loading.md`
- Testing and benchmarking: `docs/current/testing-and-benchmarking.md`
