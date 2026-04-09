# IFC2LBD-Neo

High-performance Rust converter from IFC STEP to LBD/IfcOWL in Turtle or N-Quads.

## Why This Exists

IFC2LBD-Neo focuses on fast, production-grade conversion with explicit pipeline stages and module-based extensibility.

## Quick Start

Full run (LBD + IfcOWL + full topology + bbox) in N-Quads:

```bash
ifc2lbd-neo model.ifc \
  --output out.nq \
  --base-uri https://example.test/base/ \
  --output-format nquads \
  --quad-chunking cores \
  --enable-module neo-topology-full-producer \
  --bbox
```

Minimal Turtle:

```bash
ifc2lbd-neo model.ifc --output out.ttl
```

## Topology Behavior

- `neo-topology-lite-producer`: IFC relationship topology only.
- `neo-topology-full-producer`: includes lite topology and adds OCC-backed geometry refinement.

So yes: full topology already includes the simple topology layer.

## Module Activation

List installed modules:

```bash
ifc2lbd-neo --list-modules
```

Activate a module:

```bash
ifc2lbd-neo model.ifc --output out.nq --output-format nquads \
  --enable-module neo-topology-full-producer
```

Module-specific typed config (example for full topology):

```bash
ifc2lbd-neo model.ifc --output out.nq --output-format nquads \
  --enable-module neo-topology-full-producer \
  --module-config neo-topology-full-producer:kernel_timeout_secs=900 \
  --module-config neo-topology-full-producer:max_pairs_per_batch=75000
```

Show the resolved plan:

```bash
ifc2lbd-neo model.ifc --output out.nq --output-format nquads \
  --enable-module neo-topology-full-producer \
  --show-module-plan
```

Grafeo streaming is module-only (no dedicated CLI flag):

```bash
ifc2lbd-neo model.ifc --output-format nquads \
  --enable-module neo-grafeo-export
```

## Built-In Modules

| Stage | Module ID | Purpose |
|---|---|---|
| Produce | `neo-lbd-producer` | Emit LBD triples |
| Produce | `neo-ifcowl-producer` | Emit IfcOWL triples |
| Produce | `neo-topology-lite-producer` | BOT topology from IFC relationships |
| Produce | `neo-topology-full-producer` | BOT topology with OCC geometry refinement |
| Serialize | `neo-turtle-serializer` | Serialize Turtle |
| Serialize | `neo-nquads-serializer` | Serialize N-Quads (merged/chunked) |
| Export | `neo-file-export` | Write files/manifests |
| Export | `neo-stdout-export` | Write to stdout |
| Export | `neo-grafeo-export` | Direct Grafeo framed stream |

## Main Flags

- `--output`
- `--base-uri`
- `--output-format <turtle|nquads>`
- `--profile <basic-ttl|full-nquads>`
- `--quad-chunking <none|lines|bytes|cores>`
- `--bbox`
- `--list-modules`
- `--enable-module <module-id>` (repeatable)
- `--module-config <module-id>:<key>=<value>` (repeatable)
- `--show-module-plan`

Current typed module-config keys:
- `neo-topology-full-producer:kernel_timeout_secs` (integer > 0)
- `neo-topology-full-producer:max_pairs_per_batch` (integer > 0)

## Build

```bash
cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo
cargo build --release -p lbd-geometry-kernel --bin lbd-geometry-kernel
```

## Documentation

- Module authoring and activation: `docs/current/plugin-authoring-and-activation.md`
- Agent module instructions: `docs/current/agent-plugin-instructions.md`
- Oxigraph streaming/chunk loading: `docs/current/oxigraph-loading.md`
- Testing and benchmarking: `docs/current/testing-and-benchmarking.md`
