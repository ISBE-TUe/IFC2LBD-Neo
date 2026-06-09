# IFC2LBD-Neo

High-performance Rust converter from IFC STEP to LBD, IfcOWL, and 3D geometry (Fragments/glTF/Parquet), built around a module-first pipeline architecture.

---

## Overview

IFC2LBD-Neo reads IFC STEP files and produces RDF output in Turtle or N-Quads format, plus optional 3D geometry sidecars. The conversion pipeline is driven entirely by explicit module selection — there are no implicit profiles or hidden defaults. Every active preprocessor, producer, serializer, and exporter must be named on the command line (or selected in the web UI).

The project ships three artefacts:

- `ifc2lbd-neo` — command-line binary (native, full feature set)
- `ifc2lbd-wasm` — WebAssembly library for in-browser conversion
- Web prototype — browser UI at [ifc2lbd-neo.pages.dev](https://ifc2lbd-neo.pages.dev)

---

## Quick Start

Install from source:

```bash
cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo
```

The binary is at `target/release/ifc2lbd-neo`.

Minimal LBD Turtle conversion (BOT + BEO + bSDD + OMG-FOG):

```bash
ifc2lbd-neo model.ifc \
  --output out.ttl \
  --base-uri https://example.org/building/ \
  --module neo-cleanup-preprocess \
  --module neo-bot-producer \
  --module neo-beo-producer \
  --module neo-bsdd-producer \
  --module neo-omg-fog \
  --module neo-turtle-serializer \
  --module neo-file-export
```

Full pipeline with geometry (LBD + Fragments + gzip output):

```bash
ifc2lbd-neo model.ifc \
  --output out.ttl \
  --base-uri https://example.org/building/ \
  --module neo-cleanup-preprocess \
  --module neo-qto-preprocess \
  --module neo-geometry-preprocess \
  --module neo-bot-producer \
  --module neo-beo-producer \
  --module neo-bsdd-producer \
  --module neo-omg-fog \
  --module neo-geometry-producer \
  --module neo-turtle-serializer \
  --module neo-file-export \
  --module-opt neo-turtle-serializer.grouping=sorted \
  --module-opt neo-geometry-producer.format=fragments \
  --module-opt neo-file-export.compress=gzip
```

The web UI generates an equivalent CLI command from the current configuration via the "CLI command" button in the left rail.

---

## Modules

### Preprocessors

| Module ID                    | Purpose                                      |
| ---------------------------- | -------------------------------------------- |
| `neo-cleanup-preprocess`     | ASCII repair and property deduplication      |
| `neo-qto-preprocess`         | Quantity take-off (QTO) set reconstruction   |
| `neo-bsdd-match-preprocess`  | bSDD class/property lookup and caching       |
| `neo-geometry-preprocess`    | IFC geometry tessellation (required for 3D)  |

### Producers

| Module ID               | Output                       | Named graph slug |
| ----------------------- | ---------------------------- | ---------------- |
| `neo-bot-producer`      | BOT building topology        | `/bot`           |
| `neo-beo-producer`      | BEO building elements        | `/beo`           |
| `neo-props-opm`         | OPM property sets            | `/props`         |
| `neo-bsdd-producer`     | bSDD typed properties        | `/bsdd`          |
| `neo-omg-fog`           | OMG/FOG geometry links       | `/omg`           |
| `neo-ifcowl-producer`   | Full IfcOWL ontology         | `/ifcowl`        |
| `neo-geometry-producer` | 3D geometry sidecar file     | —                |

Named graph IRIs are derived from `--base-uri`: `{base-uri}/{slug}`.

### Serializers

| Module ID                       | Format  | Notes                        |
| ------------------------------- | ------- | ---------------------------- |
| `neo-turtle-serializer`         | Turtle  | Joined or per-producer files |
| `neo-nquads-serializer`         | N-Quads | Per-producer named graphs    |
| `neo-nquads-chunked-serializer` | N-Quads | Chunked output with manifest |

### Exporters

| Module ID           | Purpose                    |
| ------------------- | -------------------------- |
| `neo-file-export`   | Write output files to disk |
| `neo-stdout-export` | Write to stdout            |
| `neo-log-export`    | Emit conversion log JSON   |

---

## Module Options

### `neo-turtle-serializer`

| Option     | Values                | Default     |
| ---------- | --------------------- | ----------- |
| `grouping` | `sorted`, `streaming` | `streaming` |
| `layout`   | `joined`, `separate`  | `joined`    |

`sorted` groups triples by subject (compact, ~2.5× smaller files, slightly slower).  
`streaming` writes in arrival order (lower memory, faster).  
`separate` writes one file per active producer instead of a merged file.

### `neo-nquads-chunked-serializer`

| Option             | Values                       | Default     |
| ------------------ | ---------------------------- | ----------- |
| `chunking`         | `none`, `lines`, `bytes`     | `lines`     |
| `chunk_size_lines` | integer                      | `2000000`   |
| `chunk_size_bytes` | integer                      | `268435456` |
| `chunk_prefix`     | string                       | `out`       |

### `neo-bsdd-producer`

| Option                   | Values                                              | Default |
| ------------------------ | --------------------------------------------------- | ------- |
| `profile`                | `base`, `revit-dach`, `allplan-de`, `tekla-en`      | `base`  |
| `compact`                | `true`, `false`                                     | `false` |
| `include_standard_attrs` | `true`, `false`                                     | `true`  |
| `dedup_properties`       | `true`, `false`                                     | `false` |

### `neo-geometry-preprocess`

| Option     | Values              | Default |
| ---------- | ------------------- | ------- |
| `metadata` | `full`, `stripped`  | `full`  |

`stripped` omits element names, descriptions, and CRS metadata from the geometry sidecar (smaller output).

### `neo-geometry-producer`

| Option   | Values                                      | Default     |
| -------- | ------------------------------------------- | ----------- |
| `format` | `fragments`, `gltf`, `parquet`, `ifc5`      | `fragments` |

### `neo-file-export`

| Option     | Values         | Default |
| ---------- | -------------- | ------- |
| `compress` | `none`, `gzip` | `none`  |

When `gzip` is set, the output file gets a `.gz` extension and is compressed with fast gzip. Suitable for direct Blazegraph/Oxigraph loading.

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

CLI binary (native, for the host platform):

```bash
cargo build --release -p ifc2lbd-cli --bin ifc2lbd-neo
```

CLI binary for Linux x86_64 (e.g. cross-building from macOS) — uses Docker:

```bash
bash scripts/build_linux_cli.sh   # -> ./ifc2lbd-neo-linux-x86_64
```

`cross` does not work in this repo because of the nightly rustup override used
for WASM; the script builds inside a Linux `rust` container instead. The cargo
registry and target dir are cached, so only the first run is slow. See the
header of [scripts/build_linux_cli.sh](scripts/build_linux_cli.sh) for details.

WebAssembly library (requires Rust nightly and wasm-bindgen-cli):

```bash
bash scripts/build_wasm_web.sh
```

Web prototype (after building WASM):

```bash
cd web/wasm-prototype
npm ci
npm run dev        # local dev server on port 5173
```

For local testing with correct COOP/COEP headers (required for SharedArrayBuffer):

```bash
cd web/wasm-prototype
docker compose up --build
# served at http://localhost:3000
```

---

## Web Prototype

The browser UI at [ifc2lbd-neo.pages.dev](https://ifc2lbd-neo.pages.dev) lets you load any IFC file, configure the module pipeline, and download the converted RDF output and geometry — all processing happens client-side in WebAssembly with no server upload.

Automatic deployments are triggered on every push to `main` via the GitHub Actions workflow at `.github/workflows/deploy-web.yml`.

The web prototype requires a cross-origin isolated context (COOP + COEP headers). Local dev via `docker compose` and the Cloudflare Pages deployment both set these headers correctly.

---

## Testing

Run the full native test suite:

```bash
cargo test
```

Run performance benchmarks:

```bash
cargo bench -p ifc2lbd-cli
```

---

## Documentation

| Document                        | Location                                    |
| ------------------------------- | ------------------------------------------- |
| Module authoring and activation | `docs/plugin-authoring-and-activation.md`   |
| Converter pipeline architecture | `docs/converter-pipeline.md`                |
| Testing and benchmarking        | `docs/testing-and-benchmarking.md`          |
| Geometry module plan            | `docs/geometry-module-plan.md`              |
| Geometry dedup open work        | `docs/geometry-dedup-todo.md`               |
| Oxigraph loading                | `docs/oxigraph-loading.md`                  |
| Open work items                 | `docs/todo.md`                              |

---

## Repository Structure

```text
crates/
  ifc-step/                    IFC STEP file parser
  ifc-schema/                  IFC type hierarchy (IFC2X3, IFC4, IFC4x3)
  ifc-model/                   IFC object model builder
  ifc-geometry/                Geometry wrapper (tessellation, mesh streaming)
  lbd-converter/               LBD triple producers (BOT, BEO, Props, OMG, IfcOWL)
  lbd-pipeline/                Module trait definitions and plugin registry
  lbd-serializer/              Turtle and N-Quads serializers
  lbd-ontology/                RDF triple types and ontology constants
  lbd-topology/                IFC spatial relationship topology
  fragments-core/              Fragments format serialization
  fragments-schema/            Flatbuffers schema for Fragments
  tessellated-model/           Shared tessellation result type
  plugin-geometry-preprocess/  Geometry tessellation pipeline plugin
  plugin-geometry-producer/    Geometry sidecar producer (Fragments/glTF/Parquet)
  plugin-fragments-producer/   Standalone Fragments producer
  plugin-property-preprocess/  Cleanup and bSDD match preprocessors
  plugin-qto-preprocess/       QTO reconstruction preprocessor
  plugin-topology-full/        Full geometry-based topology plugin
  ifc2lbd-wasm/                WebAssembly facade and browser runner
  ifc2lbd-cli/                 CLI binary and pipeline orchestration
vendor/
  geometry/                    Vendored geometry engine (MPL-2.0)
web/
  wasm-prototype/              Browser UI (Vite + WASM)
docs/                          Architecture and usage documentation
scripts/                       Build and tooling scripts
```
