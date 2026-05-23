# Plugin Authoring and Activation

This document is the authoritative reference for writing, registering, and activating plugins
in IFC2LBD-Neo. Read it before touching any plugin code.

---

## Overview: the four pluggable stages

| Stage | Trait | Runs | Purpose |
|-------|-------|------|---------|
| **Preprocess** | `PreprocessPlugin` | sequentially, before producers | Enrich/validate the IFC model or insert auxiliary context data |
| **Produce** | `ProducerPlugin` | in parallel (rayon), after preprocessors | Stream RDF triples into named-graph batches; emit sidecar files |
| **Postprocess** | `PostprocessPlugin` | sequentially, after all producers | Inspect/modify the full triple set; insert derived triples; run SHACL |
| **Export** | `ExportPlugin` | once, after serialisation | Decide *where* output files go (disk, blob storage, browser download) |

Serializer plugins (`SerializerPlugin`) are a fifth kind but act as registration markers only —
their dispatch is bespoke inside each runner (CLI / WASM). You never call `serialize()` through
trait dispatch.

---

## `PluginManifest` field reference

Every plugin implements `PipelinePlugin::manifest()` returning a `PluginManifest`:

| Field | Type | Description |
|-------|------|-------------|
| `id` | `&'static str` | Unique kebab-case ID, used as CLI `--module` flag value |
| `display_name` | `&'static str` | Human-readable label for `--list-modules` output |
| `stage` | `PipelineStage` | `Preprocess`, `Produce`, `Postprocess`, `Serialize`, or `Export` |
| `description` | `&'static str` | One-line description shown in `--list-modules` |
| `inputs` | `Vec<&'static str>` | Logical slot names this plugin reads from context |
| `outputs` | `Vec<&'static str>` | Logical slot names this plugin writes to context |
| `requires` | `Vec<&'static str>` | Other plugin IDs that must be active when this one is |
| `conflicts_with` | `Vec<&'static str>` | Other plugin IDs that must NOT be active simultaneously |
| `failure_policy` | `FailurePolicy` | `Required` (failure aborts the run) or `Optional` (failure is logged) |
| `parallelism` | `ParallelismMode` | `Serial` or `ParallelByBatch` |
| `wasm_compatible` | `bool` | Set `false` for native-only code (e.g. OpenCascade) |
| `named_graph_slug` | `Option<&'static str>` | URL slug for the producer's named graph; `None` for non-producers |
| `needs_full_graph` | `bool` | `true` → orchestrator buffers ALL triples before calling postprocess |

---

## `PipelineContext` API

`PipelineContext` is a typed key-value store (keyed by Rust type) shared across all stages.

```rust
// Insert a new typed value (panics if T is already present):
ctx.insert(Arc::new(my_value));

// Replace an existing typed value (removes old T, inserts new):
ctx.replace(Arc::new(updated_value));

// Read a typed value (returns None if not present):
let model = ctx.get::<IfcModel>().ok_or_else(|| ...)?;

// Emit sidecar artefacts (available inside ProducerPlugin::produce):
if let Some(tx) = &ctx.sidecar_tx {
    let _ = tx.send(DerivedFile { filename, mime_type, bytes });
}
```

**Registered context slots** (set by CLI/WASM runners before the pipeline starts):

| Type | Slot description |
|------|-----------------|
| `ifc_model::IfcModel` | Parsed IFC model |
| `ifc_step::StepFile` | Raw STEP AST |
| `lbd_pipeline::ConvertOptions` | CLI/WASM conversion options (base URI, batch size, …) |
| `ifc2lbd_cli::pipeline_plugins::OutputDir` | *(CLI only)* output directory for file export |

Preprocess plugins may insert additional types that producers or postprocessors read.

---

## Writing a Preprocess plugin

Preprocess plugins run **after** parsing and **before** any producers.
Use them to: compute missing quantity sets, validate the model, or insert
precomputed lookup tables into context.

### Minimal example

```rust
use lbd_pipeline::{
    FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin,
    PipelineStage, PluginManifest, PreprocessError, PreprocessPlugin,
};

pub const MY_PREPROCESSOR_ID: &str = "acme-quantity-enricher";

pub struct AcmeQuantityEnricher;

impl PipelinePlugin for AcmeQuantityEnricher {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: MY_PREPROCESSOR_ID,
            display_name: "ACME quantity enricher",
            stage: PipelineStage::Preprocess,
            description: "Fills in missing IfcQuantityVolume values.",
            inputs: vec!["ifc-model"],
            outputs: vec!["ifc-model"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,   // preprocessors never own a named graph
            needs_full_graph: false,
        }
    }
}

impl PreprocessPlugin for AcmeQuantityEnricher {
    fn preprocess(&self, ctx: &mut PipelineContext) -> Result<(), PreprocessError> {
        let model = ctx
            .get::<ifc_model::IfcModel>()
            .ok_or_else(|| PreprocessError::Preprocessing("missing IfcModel".into()))?;

        let mut new_model = (*model).clone();
        // … enrich new_model …

        ctx.replace(Arc::new(new_model));
        Ok(())
    }
}
```

### Registration

```rust
// CLI: crates/ifc2lbd-cli/src/pipeline_plugins.rs
registry.register_preprocess(AcmeQuantityEnricher).unwrap();

// WASM: crates/ifc2lbd-wasm/src/plugins.rs
registry.register_preprocess(AcmeQuantityEnricher).unwrap();
```

### Starting point

Copy `crates/plugin-template-preprocess/` and rename everything.

---

## Writing a Producer plugin

Producers stream RDF triples into a bounded crossbeam channel. Backpressure is built-in: if
the channel is full `send()` blocks until the serialiser drains a batch.

Producers can also emit **sidecar artefacts** (non-triple binary files such as geometry `.frag`
files) via `ctx.sidecar_tx`.

### Minimal example

```rust
use crossbeam::channel::Sender;
use lbd_ontology::{Object, Triple};
use lbd_pipeline::{
    BatchKind, DerivedFile, FailurePolicy, ParallelismMode, PipelineContext,
    PipelinePlugin, PipelineStage, PluginManifest, ProducerError, ProducerPlugin, TaggedBatch,
};

pub const MY_PRODUCER_ID: &str = "acme-geometry-producer";
const GRAPH_SLUG: &str = "geometry";

pub struct AcmeGeometryProducer;

impl PipelinePlugin for AcmeGeometryProducer {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: MY_PRODUCER_ID,
            display_name: "ACME geometry producer",
            stage: PipelineStage::Produce,
            description: "Emits geometry RDF and a .frag sidecar file.",
            inputs: vec!["ifc-model"],
            outputs: vec!["geometry-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: false,        // uses native geometry kernel
            named_graph_slug: Some(GRAPH_SLUG),
            needs_full_graph: false,
        }
    }
}

impl ProducerPlugin for AcmeGeometryProducer {
    fn produce(&self, ctx: &PipelineContext, sender: &Sender<TaggedBatch>) -> Result<(), ProducerError> {
        let model = ctx.get::<IfcModel>()
            .ok_or_else(|| ProducerError::Conversion("missing IfcModel".into()))?;
        let options = ctx.get::<ConvertOptions>()
            .ok_or_else(|| ProducerError::Conversion("missing ConvertOptions".into()))?;

        let graph_iri = BatchKind::new(format!(
            "{}/{}", options.base_uri.trim_end_matches('/'), GRAPH_SLUG,
        ));

        for chunk in model.elements().chunks(options.stream_batch_size) {
            let triples = chunk.iter().map(|e| geometry_triple(e, &options)).collect();
            sender.send(TaggedBatch { kind: graph_iri.clone(), triples })
                .map_err(|_| ProducerError::ChannelClosed)?;
        }

        // --- Sidecar: emit the .frag binary for the 3-D viewer ---
        if let Some(tx) = &ctx.sidecar_tx {
            let frag_bytes = generate_frag_geometry(&model);
            let _ = tx.send(DerivedFile {
                filename: "model.frag".to_string(),
                mime_type: "application/octet-stream",
                bytes: frag_bytes,
            });
        }

        Ok(())
    }
}
```

### Sidecar files in detail

`DerivedFile` is:

```rust
pub struct DerivedFile {
    pub filename: String,
    pub mime_type: &'static str,
    pub bytes: Vec<u8>,
}
```

**Lifecycle:**

1. The orchestrator creates a bounded `crossbeam::channel` before spawning producers.
2. It sets `ctx.sidecar_tx = Some(tx)` so each producer can send into it.
3. After all producers finish, the orchestrator drains the channel.
4. Each drained `DerivedFile` is forwarded to `ExportSession::accept_derived_file()`.
5. The export plugin writes the file to disk, uploads to blob storage, etc.

`sidecar_tx` is `Option<Sender<DerivedFile>>` — always check for `Some` before sending.
Errors on `send()` can be safely ignored: the receiver may have been dropped during shutdown.

### Registration

```rust
registry.register_producer(AcmeGeometryProducer).unwrap();
```

### Starting point

Copy `crates/plugin-template-producer/`.

---

## Writing a Postprocess plugin

Postprocess plugins run **after** all producers have finished, before serialisation.
They receive the complete `Vec<TaggedBatch>` and may add, remove, or rewrite triples.

Use cases: SHACL validation, inserting provenance triples, deriving summary statistics.

### `needs_full_graph`

If `needs_full_graph: true` in the manifest, the orchestrator buffers every triple from every
producer before calling your plugin. This costs peak memory but gives you the full graph.
Set it only when your postprocessor genuinely needs cross-graph visibility.

If `needs_full_graph: false`, the orchestrator may pass batches incrementally. For postprocessors
that only append new triples or that operate on one graph at a time this is fine and much cheaper.

### Minimal example

```rust
use lbd_pipeline::{
    FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin,
    PipelineStage, PluginManifest, PostprocessError, PostprocessPlugin, TaggedBatch,
};

pub const MY_POSTPROCESSOR_ID: &str = "acme-shacl-validator";

pub struct AcmeShaclValidator;

impl PipelinePlugin for AcmeShaclValidator {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: MY_POSTPROCESSOR_ID,
            display_name: "ACME SHACL validator",
            stage: PipelineStage::Postprocess,
            description: "Validates emitted triples against SHACL shapes.",
            inputs: vec!["all-triples"],
            outputs: vec![],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: true,   // needs all triples before validating
        }
    }
}

impl PostprocessPlugin for AcmeShaclValidator {
    fn postprocess(
        &self,
        _ctx: &PipelineContext,
        batches: &mut Vec<TaggedBatch>,
    ) -> Result<(), PostprocessError> {
        // Inspect batches, add violation triples, or return Err to abort.
        validate(batches).map_err(|e| PostprocessError::Postprocessing(e.to_string()))
    }
}
```

### Registration

```rust
registry.register_postprocess(AcmeShaclValidator).unwrap();
```

### Starting point

Copy `crates/plugin-template-postprocess/`.

---

## Writing an Export plugin

Export plugins decide **where** output bytes go. One export plugin may be active per run.

The orchestrator calls `start_session()` once. The returned `ExportSession` then handles:
- `open_sink()` — called per output file (one per serialiser chunk)
- `accept_derived_file()` — called once per sidecar file emitted by producers
- `finalize()` — called after all writes are done; returns an audit summary

### `ExportSession` trait

```rust
pub trait ExportSession: Send {
    fn open_sink(
        &mut self,
        filename: &str,
        mime_type: &str,
        role: &str,
    ) -> Result<Box<dyn std::io::Write + Send>, ExportError>;

    fn accept_derived_file(&mut self, file: DerivedFile) -> Result<(), ExportError>;

    fn finalize(self: Box<Self>) -> Result<Vec<ExportFileSummary>, ExportError>;
}
```

### Minimal example (blob storage)

```rust
use lbd_pipeline::{
    DerivedFile, ExportError, ExportFileSummary, ExportPlugin, ExportSession,
    FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin,
    PipelineStage, PluginManifest,
};

pub const MY_EXPORTER_ID: &str = "acme-blob-exporter";

pub struct AcmeBlobExporter;

impl PipelinePlugin for AcmeBlobExporter {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: MY_EXPORTER_ID,
            display_name: "ACME blob exporter",
            stage: PipelineStage::Export,
            description: "Uploads output to Azure Blob Storage.",
            inputs: vec!["turtle-bytes", "nquads-bytes"],
            outputs: vec!["azure-blob"],
            requires: vec![],
            conflicts_with: vec![
                lbd_pipeline::FILE_EXPORT_ID,
                lbd_pipeline::STDOUT_EXPORT_ID,
                lbd_pipeline::GRAFEO_EXPORT_ID,
            ],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: false,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ExportPlugin for AcmeBlobExporter {
    fn start_session(&self, ctx: &PipelineContext) -> Result<Box<dyn ExportSession>, ExportError> {
        let target = ctx.get::<BlobTarget>()
            .ok_or_else(|| ExportError::Export("missing BlobTarget in context".into()))?;
        Ok(Box::new(AcmeBlobSession::new(target.container_url.clone())))
    }
}

struct AcmeBlobSession { /* … streaming upload state … */ }

impl ExportSession for AcmeBlobSession {
    fn open_sink(&mut self, filename: &str, mime_type: &str, _role: &str)
        -> Result<Box<dyn std::io::Write + Send>, ExportError>
    {
        // Open a streaming upload; return a writer that feeds bytes to it.
        let writer = self.begin_upload(filename, mime_type)?;
        Ok(Box::new(writer))
    }

    fn accept_derived_file(&mut self, file: DerivedFile) -> Result<(), ExportError> {
        self.upload_bytes(&file.filename, file.mime_type, &file.bytes)
            .map_err(|e| ExportError::Export(e.to_string()))
    }

    fn finalize(self: Box<Self>) -> Result<Vec<ExportFileSummary>, ExportError> {
        self.finish_all_uploads()
    }
}
```

### `conflicts_with` for export plugins

Always list all built-in exporters in `conflicts_with` so the orchestrator detects misconfiguration
instead of silently running two exporters:

```rust
conflicts_with: vec![
    lbd_pipeline::FILE_EXPORT_ID,
    lbd_pipeline::STDOUT_EXPORT_ID,
    lbd_pipeline::GRAFEO_EXPORT_ID,
],
```

### Registration

```rust
registry.register_export(AcmeBlobExporter).unwrap();
```

### Starting point

Copy `crates/plugin-template-export/`.

---

## Activation via CLI

```bash
ifc2lbd-neo model.ifc \
  --output ./out \
  --module neo-lbd-producer \
  --module neo-topology-full-producer \
  --module neo-nquads-serializer \
  --module neo-file-export
```

- `--module <id>` activates a registered plugin by its manifest ID.
- `--module-opt <id>.<key>=<value>` sets typed plugin options (read from context in the plugin).
- `--list-modules` prints all registered manifests.
- `--show-module-plan` prints the resolved activation plan and exits.

### Conflict and requirement resolution

Before the pipeline runs, the orchestrator validates:
1. For each active plugin's `requires`, the required ID is also active. If not → error.
2. For each active plugin's `conflicts_with`, the conflicting ID is NOT active. If it is → error.

Plugins are never auto-activated. The user/caller must list every plugin explicitly.

---

## Step-by-step: adding a new plugin

1. **Create the crate.** Copy the appropriate template crate:
   - Preprocess → `crates/plugin-template-preprocess/`
   - Producer → `crates/plugin-template-producer/`
   - Postprocess → `crates/plugin-template-postprocess/`
   - Export → `crates/plugin-template-export/`

2. **Rename everything.** Plugin ID constant, struct name, crate name, `Cargo.toml` `name`.

3. **Add to workspace.** In root `Cargo.toml` `[workspace] members`:
   ```toml
   "crates/plugin-my-plugin",
   ```

4. **Implement the trait.** Fill in the manifest and the dispatch method.

5. **Register in CLI.** In `crates/ifc2lbd-cli/src/pipeline_plugins.rs`:
   ```rust
   use plugin_my_plugin::MyPlugin;
   // inside built_in_registry():
   registry.register_*(MyPlugin).unwrap();
   ```
   Add the crate to `crates/ifc2lbd-cli/Cargo.toml` dependencies.

6. **Register in WASM.** In `crates/ifc2lbd-wasm/src/plugins.rs` — same pattern.
   Add the crate to `crates/ifc2lbd-wasm/Cargo.toml` dependencies.
   If `wasm_compatible: false`, omit the WASM registration.

7. **Verify.**
   ```bash
   cargo check --workspace
   cargo test -p ifc2lbd-cli pipeline_plugins
   cargo test -p ifc2lbd-wasm
   ```

---

## Architecture rules for AI agents

These invariants **must not be violated** when modifying plugin infrastructure:

1. **Never add dispatch logic to `SerializerPlugin`.** It is a marker trait only.
   Serialisation is bespoke inside each runner (`main.rs`, `runner.rs`). The trait exists
   only for manifest registration and conflict resolution.

2. **Never call `ctx.insert<T>()` when `T` is already in context.** Use `ctx.replace<T>()`
   for updates. `insert` panics on duplicate types; `replace` is idempotent.

3. **`sidecar_tx` is optional — always guard with `if let Some(tx) = &ctx.sidecar_tx`.** It is
   `None` when no export plugin is active or the pipeline is shutting down.

4. **Producers must be `Send + Sync`.** They run on rayon worker threads. Holding a `Mutex`
   lock or a non-`Send` reference across a `.send()` call is a data race.

5. **`ExportSession::finalize` takes `self: Box<Self>`.** It consumes the session. Never add
   methods that borrow `&self` after `finalize`.

6. **`needs_full_graph: true` is expensive.** It buffers the entire triple set before calling
   `postprocess()`. Set it only when the plugin genuinely requires cross-graph visibility.
   Default to `false`.

7. **One export plugin per run.** Declare `conflicts_with` for all built-in exporters
   (`FILE_EXPORT_ID`, `STDOUT_EXPORT_ID`, `GRAFEO_EXPORT_ID`) in custom export plugins.

8. **Preprocess plugins must not hold `Arc` references across `ctx.replace<T>()`.** After
   calling `ctx.replace(Arc::new(new_value))`, the old `Arc` is dropped. If you cloned
   the old `Arc` and still hold it, that's fine — but both objects now exist in memory.
   Release the old clone promptly.

9. **Plugin IDs are kebab-case, globally unique, and never change once published.**
   A running deployment may store plugin IDs in persisted state. Rename = breaking change.

10. **Sidecar `mime_type` is `&'static str`, not `String`.** Use a string literal. If you need
    a dynamic MIME type, make it a `const &'static str`.

11. **All four template crates are the canonical starting points.** Do not copy from
    `pipeline_plugins.rs` (which contains CLI-specific boilerplate). Always start from a
    template crate.

12. **`spawn_preprocessors` and `spawn_postprocessors` are the only correct dispatch paths.**
    Do not call `preprocess()` or `postprocess()` directly outside the orchestrator.

---

## Built-in plugin IDs

### Serialisers

| ID | Crate constant | Description |
|----|---------------|-------------|
| `neo-turtle-serializer` | `TURTLE_SERIALIZER_ID` | Streams Turtle to a single sink |
| `neo-nquads-serializer` | `NQUADS_SERIALIZER_ID` | Streams N-Quads to a single sink |
| `neo-nquads-chunked-serializer` | `NQUADS_CHUNKED_SERIALIZER_ID` | Streams N-Quads in parallel chunks |

### Exporters

| ID | Crate constant | Description |
|----|---------------|-------------|
| `neo-file-export` | `FILE_EXPORT_ID` | Writes files to disk (CLI only) |
| `neo-stdout-export` | `STDOUT_EXPORT_ID` | Writes to stdout (CLI only) |
| `neo-grafeo-export` | `GRAFEO_EXPORT_ID` | Uploads to Grafeo RDF store (CLI only) |

---

## Benchmark requirement

Before merging any plugin changes, run the DigitalHub regression:

```bash
/usr/bin/time -f 'wall=%e rss_kb=%M user=%U sys=%S exit=%x' \
  target/release/ifc2lbd-neo DigitalHub_FM-ARC_v2.ifc \
  --output tmp/digitalhub_plugin_test.nq \
  --base-uri https://benchmark.test/digitalhub/ \
  --module neo-lbd-producer \
  --module neo-ifcowl-producer \
  --module neo-topology-full-producer \
  --module neo-nquads-serializer \
  --module neo-file-export \
  --module-opt neo-nquads-serializer.chunking=cores
```

Compare wall time and peak RSS against the previous run on the same hardware.
