# Plan: Dynamic WASM Plugin Loading

Status: **Draft — not yet implemented**

## Problem

Built-in plugins are compiled into the CLI binary and the WASM bundle. Adding a new plugin today requires cloning the main repo, writing Rust, and opening a PR. Users who know IFC but not Rust have no path to extending the converter on their own.

## Goal

A user clones a plugin template repository, writes their plugin logic in Rust, pushes to GitHub, and GitHub Actions compiles it to a `.wasm` file. They drop that file into a local plugin directory. The CLI and browser UI pick it up automatically at startup — no manual compilation, no changes to the main repo.

The UX model is Grasshopper plugins: a folder on disk, files dropped in, available immediately on next launch.

## Non-Goals

- Rhai scripting tier (possible future extension, see end of document)
- Plugin marketplace or registry
- Hot-reload at runtime (restart required)
- Sandboxing / trust system beyond what WASM provides by default

---

## Architecture Overview

```
plugin template repo (user's GitHub)
  └─ GitHub Actions → compiles to → myplugin.wasm

user's plugin directory
  ├─ myplugin.wasm
  └─ otherplugin.wasm

ifc2lbd-neo CLI
  └─ --plugin-dir ~/ifc-plugins
       └─ wasmtime loads *.wasm, reads manifest, registers in registry

browser UI
  └─ "Add plugin folder" button (File System Access API)
       └─ WebAssembly.instantiate() loads *.wasm, reads manifest, registers
```

Both hosts use the same `.wasm` file. The plugin targets `wasm32-unknown-unknown` (no WASI), so it runs natively in the browser and under wasmtime in the CLI.

---

## WASM ABI Contract

This is the stable contract between any `.wasm` plugin file and the host runtime. It must never break without a major version bump.

### Plugin exports (what every `.wasm` plugin must export)

```
ifc2lbd_manifest()               -> i32   // pointer to null-terminated JSON
ifc2lbd_alloc(size: i32)         -> i32   // allocate `size` bytes, return pointer
ifc2lbd_free(ptr: i32, len: i32)          // free previously allocated bytes
ifc2lbd_run(ptr: i32, len: i32)  -> i32   // run the plugin; input and output are JSON
```

The host never links anything into the plugin. All communication is through these four functions and WASM linear memory.

### `ifc2lbd_manifest` response (JSON)

```json
{
  "abi_version": 1,
  "id": "my-fire-rating-validator",
  "display_name": "Fire Rating Validator",
  "stage": "postprocess",
  "failure_policy": "optional",
  "named_graph_slug": null
}
```

`stage` is one of `"preprocess"`, `"produce"`, `"postprocess"`, `"export"`.
`named_graph_slug` is a string for produce plugins, `null` otherwise.

### `ifc2lbd_run` input (JSON written by host into plugin memory)

The host calls `ifc2lbd_alloc(len)` to get a pointer, writes the JSON bytes there, then calls `ifc2lbd_run(ptr, len)`.

**Preprocess input:**
```json
{
  "stage": "preprocess",
  "base_uri": "https://example.com/",
  "batch_size": 4096,
  "entities": [ ... ]
}
```

**Produce input:**
```json
{
  "stage": "produce",
  "base_uri": "https://example.com/",
  "batch_size": 4096,
  "entities": [ ... ]
}
```

**Postprocess input:**
```json
{
  "stage": "postprocess",
  "base_uri": "https://example.com/",
  "triples": [ { "s": "...", "p": "...", "o": "..." }, ... ]
}
```

`entities` is a JSON-serialized slice of IFC entities. This requires `ifc_model::IfcModel` to gain a `to_json_entities()` method (see Phase 1 work).

### `ifc2lbd_run` output (JSON returned from plugin)

`ifc2lbd_run` returns a pointer to a null-terminated JSON string in plugin memory. The host reads it, then calls `ifc2lbd_free` on it.

**Success — produce:**
```json
{
  "ok": true,
  "triples": [ { "s": "...", "p": "...", "o": "..." }, ... ],
  "error": null
}
```

**Success — preprocess (mutated entities):**
```json
{
  "ok": true,
  "entities": [ ... ],
  "error": null
}
```

**Failure:**
```json
{
  "ok": false,
  "error": "FireRating property missing on entity #12345"
}
```

### Memory contract

- The host never accesses plugin memory except through `ifc2lbd_alloc` / `ifc2lbd_free`.
- The plugin owns all memory it allocates. The host calls `ifc2lbd_free` on every pointer it receives from the plugin after reading it.
- The host calls `ifc2lbd_free` on the input buffer it wrote after `ifc2lbd_run` returns.

---

## New Crates

### `crates/plugin-wasm-abi`

Pure-data crate. No dependencies except `serde` and `serde_json`.

Contains:
- `PluginManifest` struct (deserializable from manifest JSON)
- `PluginInput` enum (Preprocess / Produce / Postprocess / Export variants)
- `PluginOutput` enum (success with triples or entities, or error)
- `ABI_VERSION` constant (currently `1`)

Used by both the host-side loader and the plugin SDK. This is the one crate that must never gain host-side dependencies (ifc-model, wasmtime, etc.).

### `crates/plugin-wasm-sdk`

The crate a plugin author adds as a dependency. Depends on `plugin-wasm-abi`.

Provides:
- The four exported ABI functions as a `#[no_mangle]` boilerplate that a macro generates
- A `Plugin` trait the user implements (one method per stage)
- `register_plugin!(MyPlugin)` macro that wires the trait impl to the ABI exports
- Re-exports `plugin_wasm_abi::{PluginInput, PluginOutput, PluginManifest}`

The user never writes `extern "C"` or touches raw pointers.

### `crates/plugin-wasm-loader`

Host-side loader. Used by CLI and (a subset compiled to native) by the server-side wasm runner if needed.

Provides:
- `WasmPluginLoader::load_dir(path) -> Vec<LoadedPlugin>` — scans a directory for `*.wasm`, instantiates each with wasmtime, reads its manifest, returns a list
- `LoadedPlugin` — holds the wasmtime `Instance`, implements the same `PreprocessPlugin` / `ProducerPlugin` / etc. traits as built-in plugins so it can be registered in the existing `PluginRegistry` without special casing

Depends on: `wasmtime`, `plugin-wasm-abi`

No WASM target compilation — this crate is native only.

### `crates/plugin-template-wasm` (new template)

Lives in this repo alongside the other templates (`plugin-template-preprocess`, `plugin-template-producer`, etc.). Users copy it the same way they copy any other template:

```bash
cp -r crates/plugin-template-wasm/ /path/to/my-plugin/
```

Structure:

```
plugin-template-wasm/
  Cargo.toml          (depends on plugin-wasm-sdk; standalone, not in workspace)
  src/lib.rs          (implement Plugin trait, call register_plugin! macro)
  .github/
    workflows/
      build.yml       (GitHub Actions: compiles to wasm32-unknown-unknown, uploads artifact)
  README.md           (instructions: fill in manifest, implement logic, push to get .wasm)
```

`Cargo.toml` references `plugin-wasm-sdk` and `plugin-wasm-abi` by version from crates.io (once published) rather than by workspace path, so the copied crate compiles independently without needing the rest of this repo. Users push their copy to any GitHub repo and the included workflow produces the `.wasm`.

---

## CLI Integration

### Flag

```
ifc2lbd-neo --plugin-dir ~/ifc-plugins <file.ifc> --module my-fire-rating-validator ...
```

`--plugin-dir` can be specified multiple times. All `*.wasm` files in each directory are loaded at startup before the registry is built.

### Loading sequence in `main.rs`

```
1. load built-in registry (current behaviour)
2. for each --plugin-dir:
     WasmPluginLoader::load_dir(path)?
       for each .wasm:
         instantiate, read manifest, register into registry
3. build activation plan (existing, unchanged)
4. run pipeline (existing, unchanged)
```

Loaded WASM plugins are registered in the same `PluginRegistry` as built-ins. The rest of the pipeline sees no difference.

### Error handling

- A `.wasm` file that fails to instantiate: log warning, skip (never abort the whole run unless `failure_policy: required` in manifest)
- A `.wasm` file with `abi_version > ABI_VERSION` supported by this host: skip with warning
- `--module` names a plugin that doesn't exist after loading: error as today

---

## Browser Integration

### User flow

1. User clicks "Add plugin folder" in the UI settings panel
2. Browser shows native folder picker (File System Access API `showDirectoryPicker()`)
3. UI saves the `FileSystemDirectoryHandle` to IndexedDB (persists across sessions)
4. On each convert run, UI iterates `*.wasm` files in the handle, reads them as `ArrayBuffer`, calls `WebAssembly.instantiate(buffer)`, reads manifest, registers in the in-browser plugin registry

### Plugin registry in browser

The browser runner (`ifc2lbd-wasm`) currently has a `browser_registry()` function that returns a `PluginRegistry` of compiled-in plugins. This gains a second parameter or a separate call:

```typescript
const builtins = await getBuiltinRegistry();
const dynamic  = await loadDynamicPlugins(directoryHandle); // new
const registry = merge(builtins, dynamic);
```

`loadDynamicPlugins` is TypeScript/JS that:
1. Calls `WebAssembly.instantiate` on each `.wasm` buffer
2. Calls the exported `ifc2lbd_manifest()` to get the JSON manifest
3. Builds a JS wrapper object that calls `ifc2lbd_run` when the pipeline reaches that stage
4. Returns a list of these wrapper objects

The wrapper objects implement the same JS plugin interface the WASM runner already uses internally.

### Security note

`showDirectoryPicker()` requires a user gesture and persists only via IndexedDB handle grant. The browser's WASM sandbox means a malicious plugin cannot escape the tab. Still, the UI should display plugin name, source directory, and manifest on first load and ask the user to confirm.

---

## GitHub Actions Workflow (in plugin template)

The template's `.github/workflows/build.yml`:

```yaml
name: Build WASM plugin

on:
  push:
    branches: [main]
  release:
    types: [created]

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: wasm32-unknown-unknown

      - name: Build
        run: cargo build --target wasm32-unknown-unknown --release

      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: plugin-wasm
          path: target/wasm32-unknown-unknown/release/*.wasm

      - name: Attach to release
        if: github.event_name == 'release'
        uses: softprops/action-gh-release@v2
        with:
          files: target/wasm32-unknown-unknown/release/*.wasm
```

Users download the `.wasm` from the GitHub Actions artifact or release assets and drop it into their plugin folder.

---

## Phased Delivery

### Phase 1 — ABI and SDK (prerequisite for everything)

- [ ] Create `crates/plugin-wasm-abi`: `PluginManifest`, `PluginInput`, `PluginOutput`, `ABI_VERSION`
- [ ] Add `to_json_entities() -> Vec<JsonEntity>` to `ifc_model::IfcModel` (needed to serialize input)
- [ ] Create `crates/plugin-wasm-sdk`: `Plugin` trait, `register_plugin!` macro, ABI boilerplate
- [ ] Create `crates/plugin-template-wasm` with the GitHub Actions workflow
- [ ] Unit test: compile the template to `wasm32-unknown-unknown`, instantiate with wasmtime, read manifest

This phase is self-contained and can be done without touching the CLI or browser runner.

### Phase 2 — CLI loader

- [ ] Create `crates/plugin-wasm-loader` with wasmtime dependency
- [ ] Add `--plugin-dir` flag to CLI
- [ ] Wire loader into `main.rs` before registry build
- [ ] Integration test: a minimal test plugin `.wasm` loaded from a temp dir, activated via `--module`

### Phase 3 — Browser loader

- [ ] Add `showDirectoryPicker()` folder selection to settings panel in the web UI
- [ ] Persist `FileSystemDirectoryHandle` to IndexedDB
- [ ] Write `loadDynamicPlugins(handle)` TypeScript function
- [ ] Wire into the convert flow alongside `browser_registry()`
- [ ] Manual test: load the test plugin `.wasm` from a local folder, run a convert, verify triples appear

### Phase 4 — Polish

- [ ] UI panel showing loaded dynamic plugins (name, stage, source path, ABI version)
- [ ] Confirmation prompt on first load of a new plugin
- [ ] `--list-modules` output marks dynamic plugins with `[dynamic]`
- [ ] Error messages when ABI version mismatch

---

## Performance Characteristics

Dynamic WASM plugins are not as fast as compiled-in plugins:

- JSON serialization of IFC entities adds overhead proportional to entity count
- wasmtime JIT compilation at startup (one-time, ~10–50 ms per plugin)
- No rayon parallelism within a dynamic plugin (WASM is single-threaded)

This is acceptable for the target use cases: custom validators, domain-specific property extractors, simple postprocessors. For high-throughput producers (IfcOWL, topology), compiled-in plugins remain the right choice.

---

## Future Extension: Rhai Scripting Tier

Rhai is a pure-Rust scripting language that compiles to WASM. It could provide a zero-compilation option for very simple plugins: write a `.rhai` file, drop it in the plugin directory, done.

Limitations that cap its usefulness:
- All IFC model access must be explicitly registered as Rhai functions — significant API surface to maintain
- Interpreted, so 10–50× slower than compiled WASM for entity-heavy work
- Single-threaded, no rayon
- No access to external crates

If added, Rhai would be a third tier below the WASM tier, suitable only for postprocess validators that check existing triples. It would share the same plugin directory and discovery mechanism — only the file extension (`.rhai` vs `.wasm`) distinguishes the tier.

This is deferred until Phase 4 is stable and there is demonstrated user demand.
