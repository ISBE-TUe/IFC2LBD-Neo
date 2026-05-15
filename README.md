# IFC2LBD-Neo

High-performance Rust converter from IFC STEP to LBD and IfcOWL, built around a
module-first pipeline architecture.

---

## Overview

IFC2LBD-Neo reads IFC STEP files and produces RDF output in Turtle or N-Quads format.
The conversion pipeline is driven entirely by explicit module selection — there are no
implicit profiles or hidden defaults. Every active producer, serializer, and exporter
must be named on the command line (or selected in the web UI).

The project ships three artefacts:

- `ifc2lbd-neo` — command-line binary (native, full feature set)
- `ifc2lbd-wasm` — WebAssembly library for in-browser conversion
- Web prototype — browser UI deployed at [ifc2lbd-neo.pages.dev](https://ifc2lbd-neo.pages.dev)

---

## Quick Start

Install from source:

```bash
cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo
```

The binary is at `target/release/ifc2lbd-neo`.

Minimal LBD Turtle conversion (BOT + BEO + Props + OMG-FOG):

```bash
ifc2lbd-neo model.ifc \
  --output out.ttl \
  --base-uri https://example.org/building/ \
  --module neo-bot-producer \
  --module neo-beo-producer \
  --module neo-props-opm \
  --module neo-omg-fog \
  --module neo-turtle-serializer \
  --module neo-file-export
```

LBD + IfcOWL as N-Quads with per-producer named graphs:

```bash
ifc2lbd-neo model.ifc \
  --output out.nq \
  --base-uri https://example.org/building/ \
  --module neo-bot-producer \
  --module neo-beo-producer \
  --module neo-props-opm \
  --module neo-omg-fog \
  --module neo-ifcowl-producer \
  --module neo-nquads-serializer \
  --module neo-file-export
```

The web UI generates an equivalent CLI command from the current configuration
via the "CLI command" button in the left rail.

---

## Modules

### Producers

| Module ID              | Output                    | Named graph slug |
| ---------------------- | ------------------------- | ---------------- |
| `neo-bot-producer`     | BOT building topology     | `/bot`           |
| `neo-beo-producer`     | BEO building elements     | `/beo`           |
| `neo-props-opm`        | OPM property sets         | `/props`         |
| `neo-omg-fog`          | OMG/FOG geometry links    | `/omg`           |
| `neo-ifcowl-producer`  | Full IfcOWL ontology      | `/ifcowl`        |

Named graph IRIs are derived automatically from the `--base-uri` value:
`{base-uri}/{slug}`. For example, with `--base-uri https://example.org/b/`,
the BOT graph IRI is `https://example.org/b/bot`.

### Serializers

| Module ID                       | Format  | Notes                         |
| ------------------------------- | ------- | ----------------------------- |
| `neo-turtle-serializer`         | Turtle  | Joined or per-producer files  |
| `neo-nquads-serializer`         | N-Quads | Per-producer named graphs     |
| `neo-nquads-chunked-serializer` | N-Quads | Chunked output with manifest  |

### Exporters

| Module ID            | Purpose                        |
| -------------------- | ------------------------------ |
| `neo-file-export`    | Write output files to disk     |
| `neo-stdout-export`  | Write to stdout                |
| `neo-grafeo-export`  | Stream directly into Grafeo    |

---

## Module Options

### `neo-turtle-serializer`

| Option     | Values                | Default  |
| ---------- | --------------------- | -------- |
| `grouping` | `sorted`, `streaming` | `sorted` |
| `layout`   | `joined`, `separate`  | `joined` |

`sorted` groups triples by subject (compact, standard Turtle).
`streaming` writes triples in arrival order (lower memory).
`separate` writes one file per active producer instead of a single merged file.

### `neo-nquads-serializer`

No options. Named graph IRIs are derived from `--base-uri` automatically.

### `neo-nquads-chunked-serializer`

| Option              | Values                            | Default                         |
| ------------------- | --------------------------------- | ------------------------------- |
| `chunking`          | `none`, `lines`, `bytes`, `cores` | `lines`                         |
| `chunk_size_lines`  | integer                           | `2000000`                       |
| `chunk_size_bytes`  | integer                           | `268435456`                     |
| `chunk_prefix`      | string                            | `out`                           |
| `chunk_min_count`   | integer                           | `1`                             |
| `chunk_core_count`  | integer                           | — (only with `chunking=cores`)  |

### `neo-bbox-enricher`

| Option                 | Values     | Default |
| ---------------------- | ---------- | ------- |
| `inflation_threshold`  | float > 0  | `1.5`   |

---

## Discovery and Diagnostics

List all registered modules:

```bash
ifc2lbd-neo --list-modules
```

Print the resolved module activation plan and exit without converting:

```bash
ifc2lbd-neo model.ifc --module neo-bot-producer --module neo-turtle-serializer \
  --module neo-file-export --show-module-plan
```

---

## Build

CLI binary:

```bash
cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo
```

WebAssembly library (requires Rust nightly and wasm-bindgen-cli):

```bash
bash scripts/build_wasm_web.sh
```

Web prototype (after building WASM):

```bash
cd web/wasm-prototype
npm ci
npm run dev        # local dev server on port 3031
npm run build      # production build into dist/
```

For local testing with correct COOP/COEP headers (required for SharedArrayBuffer):

```bash
cd web/wasm-prototype
docker compose up --build
# served at http://localhost:3000
```

---

## Web Prototype

The browser UI at [ifc2lbd-neo.pages.dev](https://ifc2lbd-neo.pages.dev) lets you load any IFC file, configure
the module pipeline, and download the converted RDF output — all processing happens
client-side in WebAssembly with no server upload.

Automatic deployments are triggered on every push to `main` via the GitHub Actions
workflow at `.github/workflows/deploy-web.yml`.

The web prototype requires a cross-origin isolated context (COOP + COEP headers).
Local dev via `docker compose` and the Cloudflare Pages deployment both set these
headers correctly.

---

## Testing

Run the full native test suite:

```bash
cargo test
```

Run end-to-end browser tests against the local Docker container (port 3000 must be running):

```bash
cd web/wasm-prototype
npm run test:e2e
```

Run performance benchmarks:

```bash
cargo bench -p ifc2lbd-cli
```

---

## Documentation

| Document                           | Location                                           |
| ---------------------------------- | -------------------------------------------------- |
| Module authoring and activation    | `docs/current/plugin-authoring-and-activation.md`  |
| Converter pipeline architecture    | `docs/current/converter-pipeline.md`               |
| Full topology workflow             | `docs/current/topology-full-workflow.md`           |
| Testing and benchmarking           | `docs/current/testing-and-benchmarking.md`         |
| ProduceContext / trait wiring plan | `docs/plan-produce-trait-wiring.md`                |
| Oxigraph loading                   | `docs/current/oxigraph-loading.md`                 |

If documents conflict, prefer files under `docs/current/`.

---

## Repository Structure

```text
crates/
  ifc-step/           IFC STEP file parser
  ifc-model/          IFC object model builder
  lbd-converter/      LBD triple producers (BOT, BEO, Props, OMG, IfcOWL)
  lbd-pipeline/       Module trait definitions and plugin registry
  lbd-serializer/     Turtle and N-Quads serializers
  lbd-ontology/       RDF triple types and ontology constants
  lbd-topology/       IFC spatial relationship topology
  lbd-geometry/       Bounding box and geometry computation
  ifc2lbd-wasm/       WebAssembly facade and browser runner
  ifc2lbd-cli/        CLI binary and pipeline orchestration
  plugin-topology-full/  Full geometry-based topology plugin
web/
  wasm-prototype/     Browser UI (Vite + WASM)
docs/                 Architecture and usage documentation
scripts/              Build and tooling scripts
```
