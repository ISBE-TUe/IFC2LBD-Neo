# AGENTS.md — Architectural rules for AI agents

This file governs how AI agents (Claude Code, Codex, Copilot Workspace, etc.) must behave when
modifying IFC2LBD-Neo. All rules below are **hard constraints**, not suggestions.

---

## 1. Understand the pipeline before touching it

The conversion pipeline has a fixed stage order:

```
Import → Preprocess → Produce → Postprocess → Serialize → Export
```

- **Import**: parse the IFC file into `IfcModel` / `StepFile`, insert into `PipelineContext`.
- **Preprocess**: enrich / validate the model; run sequentially via `spawn_preprocessors()`.
- **Produce**: emit RDF `TaggedBatch`es into a bounded channel; run in parallel via `spawn_producers()`.
- **Postprocess**: inspect / mutate the full batch set; run sequentially via `spawn_postprocessors()`.
- **Serialize**: convert batches to bytes (Turtle / N-Quads); bespoke in each runner.
- **Export**: write bytes to disk / blob / memory; session-based via `ExportSession`.

Never add a new stage without updating `PipelineStage` in `lbd-pipeline/src/lib.rs` AND updating
both runners (`ifc2lbd-cli/src/main.rs` and `ifc2lbd-wasm/src/runner.rs`).

---

## 2. Plugin traits

| Stage | Trait | Dispatch function |
|-------|-------|------------------|
| Preprocess | `PreprocessPlugin` | `spawn_preprocessors()` |
| Produce | `ProducerPlugin` | `spawn_producers()` |
| Postprocess | `PostprocessPlugin` | `spawn_postprocessors()` |
| Serialize | `SerializerPlugin` | **none** — marker trait only |
| Export | `ExportPlugin` | `ExportPlugin::start_session()` |

### SerializerPlugin is a marker trait

`SerializerPlugin` has **no dispatch method**. Never add `fn serialize()` or any similar method
to the trait. Serialisation logic lives bespoke in `main.rs` / `runner.rs`. The trait exists only
for registration and `conflicts_with` resolution.

### ExportPlugin uses the session pattern

`ExportPlugin::start_session(ctx)` returns `Box<dyn ExportSession>`. All output goes through the
session's `open_sink()` / `accept_derived_file()` / `finalize()`. Never bypass the session.

**CLI**: `main.rs` resolves the active export plugin via
`registry.resolve_active_export(&plan.enabled_ids)`, opens a session, wraps it in
`Arc<Mutex<Option<Box<dyn ExportSession>>>>`, and every file-writing thread (LBD serializer,
IfcOWL sidecar, `QuadChunkWriter` for chunked N-Quads, manifest writer) requests its writer
through `session::open_sink()`. After all writer threads join, `session::finalize()` returns the
per-file summaries which are logged.

**WASM**: the streaming runner (`run_to_sink`) emits bytes via a `js_sys::Function` JS callback
which is `!Send`. The `ExportSession` trait requires `Send` for CLI-side mutex sharing, so the
streaming path cannot use the trait directly — `SinkChunkWriter` writes straight to the JS
callback. The runner still honors the activation plan: it checks `settings.has(LOG_EXPORT_ID)`
before emitting per-module log sidecars, so plugin-driven dispatch decisions are respected even
though the delivery mechanism (JS callback) is platform-fixed. The `run_memory` in-memory path
uses `WasmFileExportSession` / `WasmLogExportSession` directly because buffered byte vectors
are `Send`.

---

## 3. `PipelineContext` rules

```rust
ctx.insert(Arc::new(value));     // add a NEW type — panics if T already present
ctx.replace(Arc::new(value));    // update an EXISTING type — safe to call any time
ctx.get::<T>()                   // read a typed value — returns Option<Arc<T>>
```

- **Never call `insert` for a type that is already in context.** Use `replace`.
- **Never downcast `Arc<dyn Any>` manually.** Always use `ctx.get::<T>()`.
- **Sidecar channel** — `ctx.sidecar_tx: Option<Sender<DerivedFile>>`:
  - Always guard: `if let Some(tx) = &ctx.sidecar_tx { ... }`.
  - Ignore send errors: the receiver may have been dropped during shutdown.
  - `DerivedFile.mime_type` is `&'static str` — use a string literal.

---

## 4. Sidecar file lifecycle

Sidecar artefacts (geometry files, thumbnails, etc.) flow like this:

```
ProducerPlugin::produce()
  └─ ctx.sidecar_tx.send(DerivedFile { ... })
        │
        ▼ (orchestrator drains channel after all producers finish)
ExportSession::accept_derived_file(file)
  └─ write to disk / upload / buffer in memory
```

The orchestrator creates the channel before producers run and tears it down after. Producers must
not hold the `sidecar_tx` beyond the `produce()` call.

---

## 5. Registration rules

- Every plugin must be registered in **both** `ifc2lbd-cli/src/pipeline_plugins.rs` AND
  `ifc2lbd-wasm/src/plugins.rs`, UNLESS `wasm_compatible: false` in the manifest (in which case
  omit WASM registration).
- Use `registry.register_preprocess()`, `register_producer()`, `register_postprocess()`,
  `register_serializer()`, or `register_export()` — never push to internal vecs directly.
- Plugin IDs are kebab-case, globally unique, and **immutable once published**. A rename is a
  breaking change (stored in persisted state).

---

## 6. Template crates are the canonical starting point

When creating a new plugin, **always** copy the matching template:

| Plugin type | Template |
|-------------|----------|
| Preprocess | `crates/plugin-template-preprocess/` |
| Producer | `crates/plugin-template-producer/` |
| Postprocess | `crates/plugin-template-postprocess/` |
| Export | `crates/plugin-template-export/` |

Never copy from `pipeline_plugins.rs` — it contains CLI-specific boilerplate that does not belong
in a standalone plugin crate.

---

## 7. Concurrency rules

- Producers run on rayon worker threads. Every `ProducerPlugin` impl must be `Send + Sync`.
- Never hold a `Mutex` lock or a non-`Send` reference across a `sender.send()` call.
- `ExportSession` implements `Send`. Sessions must not hold thread-local state.
- `spawn_preprocessors` and `spawn_postprocessors` run sequentially — no rayon inside them.

---

## 8. Error handling

- Return errors through the typed error enums: `PreprocessError`, `ProducerError`,
  `PostprocessError`, `ExportError`. Never `panic!` or `unwrap` in plugin code.
- `FailurePolicy::Required` → returning `Err` aborts the entire pipeline run.
- `FailurePolicy::Optional` → returning `Err` logs a warning and the run continues.
- Choose `Required` unless the plugin genuinely adds optional data.

---

## 9. `needs_full_graph`

Setting `needs_full_graph: true` in a postprocess manifest causes the orchestrator to buffer the
**entire** triple set in memory before calling `postprocess()`. This can exhaust memory for large
IFC files. Default to `false`. Only set `true` when the plugin genuinely requires cross-graph
visibility (e.g. SHACL shapes that reference multiple named graphs).

---

## 10. CLI ↔ WASM parity

The CLI (`ifc2lbd-cli`) and WASM (`ifc2lbd-wasm`) runners must expose equivalent functionality.

- Any plugin registered for CLI should also be registered for WASM (unless `wasm_compatible: false`).
- Any change to `PipelineContext` slots must be reflected in both `main.rs` and `runner.rs`.
- Do not add CLI-only or WASM-only fields to `PluginManifest` or `PipelineContext`.

---

## 11. Adding a new workspace crate

1. Create the crate under `crates/`.
2. Add it to `[workspace] members` in root `Cargo.toml`.
3. If the crate is a cross-crate dependency, add it under `[workspace.dependencies]`.
4. Run `cargo check --workspace` (excluding the Python crate) before committing.

---

## 12. What NOT to do

| Do NOT | Reason |
|--------|--------|
| Add `fn serialize()` to `SerializerPlugin` | Serialisation is bespoke in each runner |
| Call `ctx.insert<T>()` for an existing type | Panics at runtime |
| Call `preprocess()` / `postprocess()` directly | Always go through the dispatcher functions |
| Add a stage without updating both runners | Runners will silently skip the new stage |
| Duplicate plugin registration in one runner | `register_*` returns `Err` on duplicate ID |
| Use `unwrap()` / `expect()` in plugin code | Prefer returning the typed `Error` variant |
| Set `wasm_compatible: true` for native-only plugins | Will cause WASM link errors |
| Change a published plugin ID | Breaking change for stored pipelines |

---

## 13. Further reading

- Plugin authoring guide: `docs/plugin-authoring-and-activation.md`
- Pipeline architecture: `docs/converter-pipeline.md`
- Topology plugin example: `crates/plugin-topology-full/`
