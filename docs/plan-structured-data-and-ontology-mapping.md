# Plan: Structured Data Import + RML Mapper

> **Status:** Updated — reviewer blockers resolved, ontology mapper deferred
> **Date:** 2026-06-26
> **Scope:** New pipeline modules for non-IFC structured data ingestion and RML mapping

---

## 1. Overview

The pipeline currently only accepts IFC files as input. We want to extend it to:

1. **Import structured data** (JSON, XML, CSV) as a new input source
2. **RML Mapper producer** — transform structured data into RDF triples using an RML mapping file (Turtle)

The **Custom Ontology Mapping** module is **defered** — it requires the Postprocess stage to be wired (which is currently unwired in both runners), and `needs_full_graph: true` causes OOM on WASM. This is a separate effort.

This requires changes across the full stack: frontend UI, WASM runner, CLI runner, and new Rust crates.

---

## 2. Architecture

### 2.1 New pipeline flow

```
Structured Data Import (new)
  ↓ inserts Arc<StructuredDataInput> into PipelineContext
RML Mapper producer (new)
  ↓ reads StructuredDataInput + RmlMappingConfig from context
  ↓ outputs triples into named graph: {base_uri}/rml
Serializer (existing)
  ↓ Turtle (joined/separate) or N-Quads
Export (existing)
  ↓ file output
```

The existing IFC import path remains untouched. The UI will offer both import paths — IFC or Structured Data — as mutually exclusive input sources.

### 2.2 Key design decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Import UI | New "Structured Data" section in left rail, hidden when IFC file is selected | Reuse existing file/directory picker logic; keep both paths but mutually exclusive |
| Structured data storage in context | `Arc<StructuredDataInput>` — typed context slot | Mirrors `Arc<IfcModel>` pattern; producers read via `ctx.get::<StructuredDataInput>()` |
| RML mapping file transport | Runner reads option → inserts `Arc<RmlMappingConfig>` into context → producer reads typed config | **Follows geometry-producer pattern** (reviewer BLOCKER-3 fix). Avoids base64-in-option-string and the `ExecutionSettings` layering bug |
| RML mapper implementation | Reuse `worker-rml-rust/` code — move `rml_mapper` library into `crates/rml-mapper/` | User-provided Rust RML engine, already supports JSON/CSV/XML |
| Named graph for RML | `{base_uri}/rml` | Follows existing producer pattern (`{base_uri}/bot`, etc.) |
| Multi-file input transport | Concatenate files or pass as single buffer with manifest | Reviewer BLOCKER-5 fix — worker currently passes single `inputBuffer` |

---

## 3. RML Engine Integration

### 3.1 Source: `worker-rml-rust/`

The repo already contains `worker-rml-rust/` — an HTTP worker wrapping the `rml_mapper` library. The library itself (`rust_rml_mapper`) is referenced via a sibling path (`../../../rust_rml_mapper`) and is **not yet in the repo**.

**The `rml_mapper` library API** (from `worker-rml-rust/src/main.rs`):

```rust
use rml_mapper::{
    conformer::MappingConformer,     // Converts old RML namespace → W3C RML
    executor::Executor,              // Executes mapping, writes to output store
    mapping::{MappingFactory, StrictMode},  // Parses mapping document
    store::{InMemoryQuadStore, QuadStore, RdfFormat},  // In-memory RDF store
};
```

**Execution flow (from worker code):**

1. Parse mapping Turtle into `InMemoryQuadStore` via `store.read()`
2. Conform mapping via `MappingConformer::new(store, None).conform()` (old RML → W3C RML)
3. Create mapping document via `MappingFactory::new(None, StrictMode::BestEffort).create_mapping(&store)`
4. Execute via `Executor::new(mapping, work_dir, StrictMode::BestEffort).execute()`
5. Read output from `executor.output_store()` — an `InMemoryQuadStore`
6. Serialize via `output_store.write(&mut buffer, RdfFormat::Turtle)`

**Supported input formats:** JSON (JSONPath), CSV, XML (XPath)
**Supported output formats:** Turtle, N-Triples, N-Quads, TriG, RDF/XML

### 3.2 Integration plan

The `worker-rml-rust/` folder will be **disassembled** — its content reused, moved, or rewritten into the standard crate structure, then the folder is deleted.

**Step 1: Bring `rml_mapper` library into the repo**

The `rust_rml_mapper` library is currently a sibling repo. Copy it into `crates/rml-mapper/` (or `crates/rml-mapper-lib/` if we want to separate the library from the pipeline plugin). This makes it a workspace member with a real path dependency.

**Step 2: Create `crates/rml-mapper-producer/` (the pipeline plugin crate)**

This crate wraps the `rml_mapper` library as a `ProducerPlugin`. It reuses the execution logic from `worker-rml-rust/src/main.rs` (the `execute_rml_sync` function) but:

- Strips the HTTP/axum layer (no server, no multipart)
- Reads input from `PipelineContext` instead of HTTP multipart
- Streams triples via `Sender<TaggedBatch>` instead of writing to a temp `InMemoryQuadStore`
- Uses `tempfile` for the work directory (same as the worker does)

**Step 3: Delete `worker-rml-rust/`**

After the code is migrated, the `worker-rml-rust/` folder is removed. Its `Cargo.toml`, `Dockerfile`, `build.sh`, `benchmark.sh`, `bootstrap.sh` are not needed — the pipeline plugin doesn't run as a standalone service.

**Crate structure:**

```
crates/rml-mapper-lib/          ← moved from worker-rml-rust's sibling rust_rml_mapper
├── Cargo.toml
├── src/
│   ├── lib.rs                   ← library root (MappingConformer, Executor, etc.)
│   ├── conformer.rs
│   ├── executor.rs
│   ├── mapping.rs
│   └── store.rs
└── tests/

crates/rml-mapper-producer/      ← new pipeline plugin crate
├── Cargo.toml                   — depends on lbd-pipeline, rml-mapper-lib, crossbeam, tempfile
├── src/
│   ├── lib.rs                   ← ProducerPlugin impl
│   └── engine.rs                ← adapted from worker-rml-rust execute_rml_sync()
└── tests/
    └── rml_mapping_test.rs      ← adapted from worker-rml-rust integration tests
```

**`rml-mapper-producer/src/engine.rs`** — adapted from worker code:

```rust
use rml_mapper::{
    conformer::MappingConformer,
    executor::Executor,
    mapping::{MappingFactory, StrictMode},
    store::{InMemoryQuadStore, QuadStore, RdfFormat},
};
use std::io::Cursor;
use tempfile::TempDir;

/// Execute RML mapping and return triples as N-Triples bytes.
/// Adapted from worker-rml-rust/src/main.rs execute_rml_sync().
pub fn execute_rml(
    mapping_turtle: &str,
    source_filename: &str,
    source_bytes: &[u8],
) -> Result<Vec<u8>, String> {
    let temp_dir = TempDir::new().map_err(|e| format!("temp dir: {e}"))?;
    let work_dir = temp_dir.path().to_path_buf();

    // Write source file to temp directory
    let source_path = work_dir.join(source_filename);
    std::fs::write(&source_path, source_bytes)
        .map_err(|e| format!("write source: {e}"))?;

    // Replace placeholder source filenames in mapping
    let mapping = prepare_mapping_for_source(mapping_turtle, source_filename);

    // Parse mapping into quad store
    let mut mapping_store = InMemoryQuadStore::new();
    let cursor = Cursor::new(mapping.as_bytes());
    mapping_store.read(cursor, None, RdfFormat::Turtle)
        .map_err(|e| format!("parse mapping: {e}"))?;

    // Conform mapping (old RML namespace → W3C RML)
    let mut conformer = MappingConformer::new(mapping_store, None);
    conformer.conform()
        .map_err(|e| format!("conform: {e}"))?;
    let mapping_store = conformer.into_store();

    // Create mapping document
    let factory = MappingFactory::new(None, StrictMode::BestEffort);
    let mapping = factory.create_mapping(&mapping_store)
        .map_err(|e| format!("create mapping: {e}"))?;

    // Execute mapping
    let mut executor = Executor::new(mapping, work_dir, StrictMode::BestEffort);
    executor.execute()
        .map_err(|e| format!("execute: {e}"))?;

    let output_store = executor.output_store();

    // Serialize as N-Triples (easy to parse into Triple structs)
    let mut buffer = Vec::new();
    output_store.write(&mut buffer, RdfFormat::NTriples)
        .map_err(|e| format!("serialize: {e}"))?;

    Ok(buffer)
}

/// Replace placeholder source filenames in mapping with actual filename.
/// From worker-rml-rust/src/main.rs.
fn prepare_mapping_for_source(mapping: &str, source_filename: &str) -> String {
    const PLACEHOLDERS: &[&str] = &[
        "source.xml", "source.json", "source.csv",
        "data.xml", "data.json", "data.csv",
        "input.xml", "input.json", "input.csv",
    ];
    let mut result = mapping.to_string();
    for placeholder in PLACEHOLDERS {
        if result.contains(*placeholder)
            && *placeholder != source_filename
            && !source_filename.contains(*placeholder)
        {
            result = result.replace(*placeholder, source_filename);
            break;
        }
    }
    result
}
```

---

## 4. New Modules

### 4.1 Structured Data Import

**Not a plugin** — Import has no plugin trait (reviewer WARN-2). This is runner code, same as IFC parsing.

**UI — Left rail (new section):**

```
┌─────────────────────────────────────┐
│ Import Directory                     │
│ [Choose IFC file…]                   │  ← existing IFC input
│                                       │
│ ── or ──                              │
│                                       │
│ Structured Data                       │  ← new section
│ [Choose file(s)…]                    │  ← file picker (.json,.xml,.csv)
│ [Choose directory…]                   │  ← directory picker (reuses showDirectoryPicker)
│ Filter: [*.json]                      │  ← optional file filter when directory is used
└─────────────────────────────────────┘
```

**Behavior:**

- IFC and Structured Data are **mutually exclusive**. Selecting one clears the other.
- File picker accepts: `.json`, `.xml`, `.csv`, `.tsv`
- Directory picker reuses `window.showDirectoryPicker()` (same as output dir). If browser doesn't support it, only file picker is available.
- Multiple files from a directory are collected and stored as a list.
- Selected files are read lazily at run time (same as IFC — `File.arrayBuffer()` deferred).

**State:**

```js
// state.js additions
structuredDataFiles: null,      // File[] or null
structuredDataBytes: null,      // Uint8Array[] or null (read at run time)
inputFormat: "ifc",             // "ifc" or "structured-data"
```

**Context type (Rust):**

```rust
// crates/structured-data/src/lib.rs

pub enum StructuredDataFormat {
    Json,
    Xml,
    Csv,
}

pub struct StructuredDataFile {
    pub filename: String,
    pub format: StructuredDataFormat,
    pub bytes: Vec<u8>,
}

pub struct StructuredDataInput {
    pub files: Vec<StructuredDataFile>,
}
```

Inserted into `PipelineContext` as `Arc<StructuredDataInput>`.

**RML mapping config (Rust) — follows geometry-producer pattern:**

```rust
// crates/structured-data/src/lib.rs

pub struct RmlMappingConfig {
    pub mapping_turtle: String,   // RML mapping file content (decoded from base64 option)
}
```

The runner reads the `rml_mapping` option, decodes base64, creates `RmlMappingConfig`, and inserts it into `PipelineContext` as `Arc<RmlMappingConfig>`. The producer reads it via `ctx.get::<RmlMappingConfig>()`. **This is the geometry-producer pattern** (reviewer BLOCKER-3 fix, SUGG-1).

**Multi-file transport (reviewer BLOCKER-5 fix):**

The worker currently passes a single `inputBuffer`. For structured data with multiple files, two options:

- **Option A (MVP):** Concatenate files with a manifest header. Worker receives one buffer, splits by manifest.
- **Option B (cleaner):** Add a `structuredDataBuffers: Vec<Vec<u8>>` field to the WASM request payload. Worker passes each buffer to the runner.

**Recommendation: Option B** — add a new field to the worker message. The worker already uses `postMessage` with a transferable; extending the payload to include multiple buffers is straightforward.

**WASM runner changes** (`runner.rs`):

- `run_to_sink()` currently always calls `parse_step_bytes(input)`. Needs a branch: if `input_format == StructuredData`, create `StructuredDataInput` from raw bytes instead of parsing IFC.
- New `ExecutionSettings` field: `input_format: InputFormat` (`Ifc` or `StructuredData`).
- Runner reads `rml_mapping` option → decodes base64 → inserts `Arc<RmlMappingConfig>` into context.

**CLI runner changes** (`main.rs`):

- Accept `--input-format structured-data` flag.
- When set, skip IFC parsing; read input file(s) as raw bytes into `StructuredDataInput`.

---

### 4.2 RML Mapper Producer

**Module ID:** `neo-rml-mapper`
**Stage:** Produce
**Named graph slug:** `rml`
**WASM compatible:** Yes
**Failure policy:** Required

**What it does:**

- Reads `StructuredDataInput` from `PipelineContext`
- Reads `RmlMappingConfig` from `PipelineContext` (inserted by runner, not from option string)
- Executes the RML mapping via the `rml_mapper` library (reused from `worker-rml-rust`)
- Parses the N-Triples output into `Triple` structs
- Streams triples as `TaggedBatch` with graph IRI `{base_uri}/rml`
- Uses `tempfile::TempDir` for the RML executor's work directory (same as worker code)

**Module options:**

| Key | Type | Values | Default |
|-----|------|--------|---------|
| `rml_mapping` | **file upload** | `.ttl` file | (required) |

**Producer implementation (follows existing pattern, reviewer BLOCKER-3 fix):**

```rust
fn produce(&self, ctx: &PipelineContext, sender: &Sender<TaggedBatch>) -> Result<(), ProducerError> {
    let data = ctx.get::<StructuredDataInput>()
        .ok_or_else(|| ProducerError::Conversion("No structured data input".into()))?;

    let mapping_config = ctx.get::<RmlMappingConfig>()
        .ok_or_else(|| ProducerError::Conversion("No RML mapping config".into()))?;

    let options = ctx.get::<ConvertOptions>()
        .ok_or_else(|| ProducerError::Conversion("No convert options".into()))?;

    let graph_iri = BatchKind::new(format!("{}rml", options.base_uri.trim_end_matches('/')));

    let (raw_sender, raw_receiver) = crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
    forward_as_tagged(raw_receiver, graph_iri, sender.clone());

    // Execute RML mapping (reuses worker-rml-rust logic)
    for file in &data.files {
        let ntriples_bytes = engine::execute_rml(
            &mapping_config.mapping_turtle,
            &file.filename,
            &file.bytes,
        ).map_err(|e| ProducerError::Conversion(e))?;

        // Parse N-Triples into Triple structs and send through channel
        let triples = parse_ntriples(&ntriples_bytes);
        raw_sender.send(triples)
            .map_err(|_| ProducerError::ChannelClosed)?;
    }

    Ok(())
}
```

**Note:** The RML executor currently writes to an `InMemoryQuadStore` then serializes. For large datasets, we may want to stream directly from the output store instead of serializing to a buffer and re-parsing. This is a future optimization — the worker code works fine for typical mapping sizes.

---

## 5. Cross-cutting changes

### 5.1 Validation fixes (reviewer BLOCKER-2)

**`crates/ifc2lbd-wasm/src/validation.rs` — `validate_activation_plan()`:**

Currently requires at least one of `BOT/BEO/BSDD/PROPS/OMG/IFCOWL`. Must add `RML_MAPPER_ID` to the producer check so RML-only presets are valid.

```rust
// Before:
let has_any_producer = active_ids.contains(BOT_PRODUCER_ID)
    || active_ids.contains(BEO_PRODUCER_ID)
    // ...

// After:
let has_any_producer = active_ids.contains(BOT_PRODUCER_ID)
    || active_ids.contains(BEO_PRODUCER_ID)
    // ...
    || active_ids.contains(RML_MAPPER_ID);
```

**Same fix in `crates/ifc2lbd-cli/src/main.rs`** (CLI parity).

### 5.2 Producer dispatch sites (reviewer WARN-1)

The WASM runner has ~8 hardcoded slug drain sites. `neo-rml-mapper` must be added to:

1. `active_producer_ids_from_settings()` (`runner.rs:112-122`) — must return `RML_MAPPER_ID` when active
2. `collect_and_emit!` / `drain_and_emit!` / `drain_sep_and_emit!` macros — must include `rml` slug
3. `drain_chunked!` / `drain_merged!` / `write_producer_nq!` — same
4. `turtle_file_summaries` / `nquads_file_summaries` — add `rml` entry
5. The "running" event loop (`runner.rs:394-399`) — add `neo-rml-mapper` status

The CLI routes by graph-IRI suffix (`main.rs:552-571`) — add `/rml` case.

### 5.3 State & UI (frontend)

**`state.js`:**

- Add `structuredDataFiles: null`, `structuredDataBytes: null`, `inputFormat: "ifc"`
- No new module IDs in default active set — RML mapper is activated by preset selection

**`index.html` — left rail additions:**

- New "Structured Data" section below IFC input
- File input with `accept=".json,.xml,.csv,.tsv"`
- Directory button (reuses `showDirectoryPicker` logic)
- Filter input (for directory mode: glob pattern)
- Mutual exclusion: selecting structured data clears IFC file and vice versa

**`app.js`:**

- Wire structured data file/directory inputs
- On run: if `inputFormat === "structured-data"`, send `inputFormat` in requestPayload, send structured data bytes
- Update expected output filenames to include `rml` slug
- New presets (see §5.5)

**`session.js`:**

- Add `neo-rml-mapper` to `PRODUCE_ORDER` array, after `neo-ifcowl-producer`

**`sidebar.js` — new file-upload option type:**

- `optionControl()` needs a new branch for file-upload options
- Detect by key name (`rml_mapping`)
- Render: `<input type="file">` + filename display
- On change: read file as text (RML mappings are Turtle/UTF-8) → store as string value in `moduleOptions`
- No base64 needed — RML mapping files are UTF-8 Turtle text, not binary. Store the text directly.

**`cli-command.js`:**

- Add `MODULE_DEFAULTS` entry for `neo-rml-mapper` (no defaults — `rml_mapping` is required)
- Update `outputExtension()` — no change needed (RML output goes through existing serializers)

**`wasm-lowmem-worker.js`:**

- Pass `inputFormat` in the request payload to the WASM call
- Support multiple file buffers in the worker message (reviewer BLOCKER-5)

### 5.4 Rust / WASM changes

**New crate: `crates/structured-data/`**

- `StructuredDataInput`, `StructuredDataFile`, `StructuredDataFormat`, `RmlMappingConfig` types
- Format detection from file extension

**New crate: `crates/rml-mapper-lib/`**

- Moved from `worker-rml-rust`'s sibling `rust_rml_mapper` repo
- The core RML mapping library (`MappingConformer`, `Executor`, `MappingFactory`, `InMemoryQuadStore`)

**New crate: `crates/rml-mapper-producer/`**

- `ProducerPlugin` implementation
- `engine.rs` — adapted from `worker-rml-rust/src/main.rs` `execute_rml_sync()`
- N-Triples parser to convert RML output into pipeline `Triple` structs
- ID: `neo-rml-mapper`

**`crates/lbd-pipeline/src/lib.rs`:**

- Add plugin ID constant: `RML_MAPPER_ID`
- `StructuredDataInput` and `RmlMappingConfig` as context types

**`crates/ifc2lbd-wasm/src/plugins.rs`:**

- Register `RmlMapperProducerPlugin`
- Add option keys to `module_option_keys()`:
  - `neo-rml-mapper` → `["rml_mapping"]`

**`crates/ifc2lbd-wasm/src/validation.rs`:**

- Add `rml_mapping` to `validate_typed_module_configs()` whitelist
- Add `RML_MAPPER_ID` to `validate_activation_plan()` producer check (BLOCKER-2)
- Add `InputFormat` to `ExecutionSettings` resolution
- Runner reads `rml_mapping` option → creates `RmlMappingConfig` → inserts into context

**`crates/ifc2lbd-wasm/src/runner.rs`:**

- Branch in `run_to_sink()`: if `input_format == StructuredData`, create `StructuredDataInput` from raw bytes instead of parsing IFC
- Read `rml_mapping` option → decode → insert `Arc<RmlMappingConfig>` into context (geometry-producer pattern)
- Add `neo-rml-mapper` to `active_producer_ids_from_settings()` and all slug drain sites (WARN-1)

**`crates/ifc2lbd-cli/src/pipeline_plugins.rs`:**

- Register `RmlMapperProducerPlugin` (CLI parity)

**`crates/ifc2lbd-cli/src/main.rs`:**

- Add `--input-format structured-data` CLI flag
- Add `RML_MAPPER_ID` to activation plan validation (BLOCKER-2)
- Add option validation for `rml_mapping`
- Read `rml_mapping` option → insert `RmlMappingConfig` into context

**`Cargo.toml` (workspace):**

- Add `crates/structured-data`, `crates/rml-mapper-lib`, `crates/rml-mapper-producer` to members
- Add `rml-mapper-lib` to workspace dependencies

**Delete `worker-rml-rust/`** after migration is complete.

### 5.5 Presets

New presets for structured data workflows:

| Preset | Input | Modules |
|--------|-------|---------|
| RML → Turtle | Structured Data | rml-mapper, turtle-serializer, file-export |
| RML → N-Quads | Structured Data | rml-mapper, nquads-serializer, file-export |

These are separate from the existing IFC presets. The preset dropdown groups them with a separator or optgroup.

### 5.6 Mobile — force landscape instead of separate view

The current `entry.js` splits at `window.innerWidth < 900` and loads a completely different `main.js` / mobile form. This is wrong — the pipeline GUI is already usable in widescreen mode. The portrait layout is the problem, not the screen size.

**New approach:**

- Remove the `#mobile-view` split entirely from `entry.js`
- Always load the pipeline dashboard (`app.js`), regardless of screen width
- Add a CSS/JS orientation overlay: when portrait mode is detected on a touch device, show a full-screen "Please rotate your device" overlay
- On desktop (no touch), always show the pipeline view
- Overlay disappears once the user rotates to landscape

**⚠️ Reviewer WARN-4:** Phone-landscape widths (667–900px) were never tested with the dashboard CSS. Must verify the layout works at these widths before deleting `main.js`/`#mobile-view`. Add a `@media (max-width: 900px)` pass to the pipeline CSS if layout breaks.

```js
// entry.js (new)
import "./pipeline/app.js";

// Orientation overlay for phones
function isPortrait() {
  return window.innerHeight > window.innerWidth;
}
function isTouchDevice() {
  return 'ontouchstart' in window || navigator.maxTouchPoints > 0;
}
function checkOrientation() {
  const overlay = document.getElementById('rotate-overlay');
  if (!overlay) return;
  overlay.style.display = (isTouchDevice() && isPortrait()) ? 'flex' : 'none';
}
window.addEventListener('resize', checkOrientation);
window.addEventListener('orientationchange', checkOrientation);
checkOrientation();
```

---

## 6. Implementation order

### Phase 1: RML Mapper MVP

1. **Copy `rust_rml_mapper` library** into `crates/rml-mapper-lib/` (bring the sibling repo in-house)
2. **Create `crates/structured-data/`** — types and format detection
3. **Create `crates/rml-mapper-producer/`** — producer crate with `ProducerPlugin` impl, `engine.rs` adapted from `worker-rml-rust/src/main.rs`
4. **Delete `worker-rml-rust/`** — code migrated, folder removed
5. **Rust: registration** — plugins.rs, pipeline_plugins.rs, validation.rs (BLOCKER-2 fix), module_option_keys, runner.rs context insertion (geometry-producer pattern)
6. **Rust: runner.rs branching** — input format detection, structured data path, slug drain sites (WARN-1)
7. **Frontend: structured data input UI** — file/directory picker, state wiring, mutual exclusion with IFC
8. **Frontend: file-upload option in sidebar** — new `optionControl()` branch for `rml_mapping`
9. **Frontend: presets** — add RML presets
10. **CLI parity** — `--input-format`, plugin registration, option validation
11. **Tests** — adapt `worker-rml-rust/tests/integration.rs` as crate-level tests; test format detection, activation plan, validation
12. **WASM build verification** — `cargo check --workspace` + `scripts/build_wasm_web.sh` must pass (reviewer SUGG-4)

### Phase 2: Mobile/landscape fix

1. **Test dashboard at 667–900px width** (reviewer WARN-4)
2. **Add `@media` fixes** if layout breaks
3. **Replace `entry.js`** — always load `app.js`, add rotate overlay
4. **Remove `#mobile-view`** from HTML, delete `main.js`
5. **Remove `src/styles.css`** (mobile-only styles)

### Phase 3: Polish

1. **Documentation** — update `docs/plugin-authoring-and-activation.md` (reviewer SUGG-3)
2. **RML streaming optimization** — if large datasets cause memory issues, stream from `InMemoryQuadStore` directly instead of serialize → re-parse
3. **CI gate** — add `cargo check --workspace` step to `deploy-web.yml` (reviewer SUGG-4)

---

## 7. Risks & mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| `rml_mapper` library not WASM-compatible | Build failure | Test `cargo check --target wasm32-unknown-unknown` early; the library uses `oxigraph` which is WASM-compatible |
| Slug drain sites missed | RML triples silently dropped | Audit all 8 sites in `runner.rs`; add tests that verify output file contains RML triples |
| CLI↔WASM parity drift | Silent feature gaps | Register in both runners from day one; add CLI tests |
| Phone-landscape layout broken | Bad mobile UX | Test at 667–900px before removing mobile view (WARN-4) |
| Structured data format edge cases | Parser failures | Start with JSON + CSV + XML (RML mapper handles these already) |

### Resolved questions

1. **RML engine:** `rml_mapper` library from `worker-rml-rust/` — moved into `crates/rml-mapper-lib/`. Worker's `execute_rml_sync()` adapted into `crates/rml-mapper-producer/src/engine.rs`.
2. **Ontology mapping:** Deferred. Requires wiring the Postprocess stage (currently unwired in both runners). `needs_full_graph: true` causes OOM on WASM. Separate effort.
3. **File upload size:** Non-issue. RML mapping files are UTF-8 Turtle text (KB range). Stored directly as string, no base64 needed.
4. **Structured data formats:** JSON, CSV, XML. RML mapper already handles these.
5. **Mobile:** No separate mobile view. Force landscape on phones. Replace `#mobile-view` with rotate overlay. Test at phone-landscape widths first.

---

## 8. File touch list

| File | Change |
|------|--------|
| `web/wasm-prototype/index.html` | Structured data input section, mutual exclusion logic, rotate overlay, remove mobile-view |
| `web/wasm-prototype/src/pipeline/state.js` | `structuredDataFiles`, `structuredDataBytes`, `inputFormat` |
| `web/wasm-prototype/src/pipeline/app.js` | Wire structured data inputs, run conversion branching, new presets |
| `web/wasm-prototype/src/pipeline/session.js` | Sort order for `neo-rml-mapper` |
| `web/wasm-prototype/src/pipeline/sidebar.js` | File-upload option type in `optionControl()` |
| `web/wasm-prototype/src/pipeline/cli-command.js` | MODULE_DEFAULTS for `neo-rml-mapper` |
| `web/wasm-prototype/src/entry.js` | Remove mobile-view split, always load app.js, add orientation overlay |
| `web/wasm-prototype/src/wasm-lowmem-worker.js` | Pass `inputFormat` and multiple file buffers to WASM call |
| `crates/structured-data/` | **New crate** — types, format detection |
| `crates/rml-mapper-lib/` | **New crate** — moved from `worker-rml-rust`'s sibling `rust_rml_mapper` |
| `crates/rml-mapper-producer/` | **New crate** — RML producer plugin, engine adapted from worker code |
| `crates/lbd-pipeline/src/lib.rs` | Plugin ID constant `RML_MAPPER_ID`, context types |
| `crates/ifc2lbd-wasm/src/plugins.rs` | Register plugin, `module_option_keys()` |
| `crates/ifc2lbd-wasm/src/validation.rs` | Validate `rml_mapping` option, fix `validate_activation_plan()` (BLOCKER-2), `InputFormat` resolution |
| `crates/ifc2lbd-wasm/src/runner.rs` | Input format branching, context insertion (geometry pattern), slug drain sites (WARN-1) |
| `crates/ifc2lbd-cli/src/pipeline_plugins.rs` | Register plugin (CLI parity) |
| `crates/ifc2lbd-cli/src/main.rs` | `--input-format` flag, option validation, activation plan fix |
| `crates/structured-data/src/lib.rs` | `StructuredDataInput`, `RmlMappingConfig` types |
| `worker-rml-rust/` | **Delete** — code migrated to `crates/rml-mapper-lib/` and `crates/rml-mapper-producer/` |
| `Cargo.toml` | Add new crates to workspace members |
| `docs/plugin-authoring-and-activation.md` | Update with RML mapper example |
