# IFC2LBD-Neo

High-performance Rust converter from IFC STEP to LBD, IfcOWL, and 3D geometry (Fragments/glTF/Parquet), built around a module-first pipeline architecture. Also supports structured data (JSON/CSV/XML) input via RML mappings and ontology alignment.

---

## Try It Now

| Platform | Link |
|----------|------|
| 🌐 **Web App** (no install, runs in browser) | **[ifc2lbd-neo.pages.dev](https://ifc2lbd-neo.pages.dev)** |
| 🔍 **Viewer** (SPARQL + 3D model viewer) | **[viewer-ifc2lbd-neo.pages.dev](https://viewer-ifc2lbd-neo.pages.dev)** |
| 🖥️ **Desktop App** (macOS `.dmg`, Windows `.exe`) | **[GitHub Releases](https://github.com/ISBE-TUe/IFC2LBD-Neo/releases/latest)** |
| 📦 **CLI Binary** (Linux, macOS, Windows) | **[GitHub Releases](https://github.com/ISBE-TUe/IFC2LBD-Neo/releases/latest)** |

> **⚠️ macOS**: The desktop app and CLI binary are not Apple-code-signed. After installing, run this in Terminal before opening:
> ```bash
> xattr -cr /Applications/IFC2LBD-Neo.app
> ```

---

## Overview

IFC2LBD-Neo reads IFC STEP files (or structured data) and produces RDF output in Turtle or N-Quads format, plus optional 3D geometry sidecars. The conversion pipeline is driven entirely by explicit module selection — there are no implicit profiles or hidden defaults. Every active preprocessor, producer, serializer, and exporter must be named on the command line (or selected in the web UI).

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

Structured data conversion via RML mapping:

```bash
ifc2lbd-neo data.json \
  --input-format structured-data \
  --output out.ttl \
  --base-uri https://example.org/ \
  --module neo-rml-mapper \
  --module neo-turtle-serializer \
  --module neo-file-export \
  --module-opt neo-rml-mapper.rml_mapping=@mapping.ttl
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

| Module ID                 | Output                       | Named graph slug |
| ------------------------- | ---------------------------- | ---------------- |
| `neo-bot-producer`        | BOT building topology        | `/bot`           |
| `neo-beo-producer`        | BEO building elements        | `/beo`           |
| `neo-props-opm`           | OPM property sets            | `/props`         |
| `neo-bsdd-producer`       | bSDD typed properties        | `/bsdd`          |
| `neo-omg-fog`             | OMG/FOG geometry links       | `/omg`           |
| `neo-ifcowl-producer`     | Full IfcOWL ontology         | `/ifcowl`        |
| `neo-geometry-producer`   | 3D geometry sidecar file     | —                |
| `neo-rml-mapper`          | RML-mapped triples from structured data | `/rml` |
| `neo-ontology-mapper`     | Ontology-aligned triples from structured data | `/ontology` |

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

### `neo-rml-mapper`

| Option        | Values                  | Default   |
| ------------- | ----------------------- | --------- |
| `rml_mapping` | RML mapping file (Turtle) | (required) |

Upload an RML mapping file (Turtle) that defines how structured data (JSON/CSV/XML) is transformed into RDF triples.

### `neo-ontology-mapper`

| Option            | Values                        | Default   |
| ----------------- | ----------------------------- | --------- |
| `alignment_file`  | Alignment file (Turtle/RDF)    | (optional) |
| `ontology_file`   | Ontology file (Turtle/OWL)     | (optional) |

The alignment file maps source predicates to target predicates using `owl:equivalentProperty`, `rdfs:subPropertyOf`, or EDOAL alignment entries. The ontology file provides additional `owl:equivalentProperty` mappings.

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
header of [scripts/build_linux_cli.sh](scripts/build_linux_linux_cli.sh) for details.

WebAssembly library (requires Rust nightly and wasm-bindgen-cli):

```bash
bash scripts/build_wasm_web.sh
```

Web prototype (after building WASM):

```bash
cd web/wasm-prototype
npm ci
npm run dev        # local dev server on port 3031
```

For local testing with correct COOP/COEP headers (required for SharedArrayBuffer):

```bash
cd web/wasm-prototype
docker compose up --build
# served at http://localhost:3000
```

---

## Web Prototype

The browser UI at [ifc2lbd-neo.pages.dev](https://ifc2lbd-neo.pages.dev) lets you load any IFC file (or structured data), configure the module pipeline, and download the converted RDF output and geometry — all processing happens client-side in WebAssembly with no server upload.

Automatic deployments are triggered on every push to `main` via the GitHub Actions workflow at `.github/workflows/deploy-web.yml`.

The web prototype requires a cross-origin isolated context (COOP + COEP headers). Local dev via `docker compose` and the Cloudflare Pages deployment both set these headers correctly.

Features:

- IFC file import (file picker + directory picker)
- Structured data import (JSON/CSV/XML) — click the "Parse Structured Data" module in the Import column to select files
- Module pipeline grid with clickable activation circles
- Preset configurations (Default, Geometry, IfcOWL, RML, Ontology — Turtle/N-Quads)
- Compressed mode toggle (bSDD compact+dedup + gzip export)
- CLI command generator with download bin buttons (Linux/macOS/Windows)
- Citation widget (paper link + BibTeX copy)
- Mobile: rotate-device overlay (landscape required)

---

## Releases

The CLI binaries are built **natively** on their respective platform runners (no cross-compilation) and published to GitHub Releases. The web UI download buttons point to the latest release assets.

### How to ship a new release

```bash
# 1. Ensure main is green and you're on the latest commit
git checkout main && git pull

# 2. Tag the release
git tag v0.1.0
git push origin v0.1.0
```

Pushing a tag (`v*`) triggers **three workflows**:

| Workflow                         | What it does                                                            |
| -------------------------------- | ----------------------------------------------------------------------- |
| `build-cli.yml`                  | Builds Linux + macOS + Windows binaries → creates GitHub Release        |
| `deploy-web.yml`                 | Rebuilds WASM + Vite app → deploys to Cloudflare Pages                   |
| `build-desktop.yml`              | Downloads CLI binaries from release → builds Electron .dmg + .exe → uploads to same release |

`build-cli.yml` and `deploy-web.yml` fire in parallel on tag push. `build-desktop.yml` chains after `build-cli.yml` (via `workflow_run`) because it needs the CLI binaries to bundle into the installers.

### Manual trigger (no git tag)

If you want to build binaries without creating a git tag:

1. Go to **GitHub → Actions → "Build CLI Binaries" → Run workflow**
2. Enter a tag name (e.g. `v0.1.0-rc1`) — it does **not** need to exist as a git ref
3. The workflow builds and creates a release under that name

### Download URLs

The web UI download buttons use these stable URLs (always resolve to the latest release):

```
https://github.com/ISBE-TUe/IFC2LBD-Neo/releases/latest/download/ifc2lbd-neo-cli-linux
https://github.com/ISBE-TUe/IFC2LBD-Neo/releases/latest/download/ifc2lbd-neo-cli-macos
https://github.com/ISBE-TUe/IFC2LBD-Neo/releases/latest/download/ifc2lbd-neo-cli-windows.exe
```

### Local builds (for testing)

For local testing without GitHub Actions, use the Docker-based script:

```bash
./scripts/build_all_cli.sh
```

This builds Linux (Docker `linux/amd64`) and macOS (native) locally. Windows requires the GitHub Actions workflow or a Windows machine — cross-compiling from Linux hits `windows-sys` import-library issues.

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
  structured-data/             Structured data input types (JSON/CSV/XML)
  rml-mapper-lib/              RML mapping engine (rio_api + rio_turtle)
  rml-mapper-producer/         RML mapper producer plugin
  ontology-mapper-producer/    Ontology alignment producer plugin
  plugin-geometry-preprocess/  Geometry tessellation pipeline plugin
  plugin-geometry-producer/    Geometry sidecar producer (Fragments/glTF/Parquet)
  plugin-property-preprocess/  Cleanup and bSDD match preprocessors
  plugin-qto-preprocess/       QTO reconstruction preprocessor
  plugin-topology-full/        Full geometry-based topology plugin
  ifc2lbd-wasm/                WebAssembly facade and browser runner
  ifc2lbd-cli/                 CLI binary and pipeline orchestration
vendor/
  geometry/                    Vendored geometry engine (MPL-2.0)
web/
  wasm-prototype/              Browser UI (Vite + WASM)
electron/                      Electron desktop app (native CLI sidecar)
docs/                          Architecture and usage documentation
scripts/                       Build and tooling scripts
```

---

## Desktop App (Electron)

The desktop app wraps the web UI in Electron and replaces the WASM conversion engine with the native CLI binary running as a sidecar process. This provides full native threading (rayon) and no memory limits.

- **macOS**: `.dmg` (Apple Silicon)
- **Windows**: `.exe` (NSIS installer, x64)

Desktop installers are built by `.github/workflows/build-desktop.yml` and published to the same GitHub Release as the CLI binaries. See [`electron/README.md`](electron/README.md) for architecture and development details.

---

## License

IFC2LBD-Neo is dual-licensed:

- **AGPL-3.0-only** for open-source use. See [LICENSE](LICENSE) for the full text.
- **Commercial license** for proprietary or commercial use incompatible with AGPL-3.0 terms. Contact: **<l.t.kirner@tue.nl>**

The vendored geometry engine in `vendor/geometry/` is derived from [ifc-lite](https://github.com/LTplus-AG/ifc-lite) and remains licensed under [MPL-2.0](LICENSE-MPL). MPL-2.0 is compatible with AGPL-3.0 per Mozilla's Section 3.3. See [NOTICE](NOTICE) for details.
