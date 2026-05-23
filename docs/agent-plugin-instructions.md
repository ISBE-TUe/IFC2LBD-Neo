# Agent Guide: Building a Plugin for IFC2LBD-Neo

You are an AI agent about to build a new plugin for the IFC2LBD-Neo converter.
This document tells you exactly what to do, in order.

**Before writing any code**, read these two files in full:

- `AGENTS.md` (repo root) — hard architectural rules you must not break
- `docs/plugin-authoring-and-activation.md` — full API reference with code examples

---

## Step 1: Ask the user these questions

Do not guess. Ask all of these before writing a single line of code.

```text
1. What should the plugin DO in plain English?
   (e.g. "validate that every IfcWall has a FireRating property set")

2. What type of plugin is it?
   - Preprocess  → runs before producers, can read/modify the IFC model in context
   - Producer    → emits RDF triples into a named graph
   - Postprocess → inspects or mutates the full set of produced triples
   - Export      → writes output to disk, memory, or a remote endpoint

3. What plugin ID should it have? (kebab-case, e.g. "fire-rating-validator")
   This is permanent — it cannot be renamed after first use.

4. What display name should it have? (human-readable, e.g. "Fire Rating Validator")

5. Should the pipeline abort if this plugin fails, or just log a warning and continue?
   - Required → abort on failure
   - Optional → warn and continue (use this unless the data is critical)

6. Should it work in the browser (WASM), or CLI only?
   Answer "WASM too" unless the plugin uses native-only code (OpenCascade, file I/O, etc.)

7. [Producer plugins only] What named graph should the triples go into?
   This becomes the URL slug, e.g. "fire-rating" → https://base-uri/fire-rating
   If unsure, use the plugin ID.

8. [Preprocess plugins only] What does it read from the IFC model, and what does it write back?
   (e.g. "reads IfcPropertySet, adds computed values, replaces the model in context")

9. Any specific IFC entity types, ontologies, or property sets it should work with?
```

---

## Step 2: Confirm your plan with the user

Before touching any file, summarise what you are about to build:

```text
"I will create a [type] plugin with:
  ID:           <id>
  Display name: <name>
  Failure:      Required / Optional
  WASM:         Yes / No
  [Producer] Named graph: <slug>
  Crate path:   crates/<id>/

  It will [plain-English description of what it does].

  Registration:
  - CLI:  crates/ifc2lbd-cli/src/pipeline_plugins.rs
  - WASM: crates/ifc2lbd-wasm/src/plugins.rs  [if WASM compatible]

  Shall I proceed?"
```

Only continue after the user confirms.

---

## Step 3: Create the crate

Copy the matching template — never start from scratch or copy from `pipeline_plugins.rs`.

```bash
cp -r crates/plugin-template-<type>/ crates/<your-plugin-id>/
```

Template locations:

- Preprocess  → `crates/plugin-template-preprocess/`
- Producer    → `crates/plugin-template-producer/`
- Postprocess → `crates/plugin-template-postprocess/`
- Export      → `crates/plugin-template-export/`

Then add it to the workspace. Open `Cargo.toml` at the repo root and add the crate to the `[workspace] members` list:

```toml
members = [
    # ... existing entries ...
    "crates/<your-plugin-id>",
]
```

If other crates will depend on this plugin, also add it under `[workspace.dependencies]`.

---

## Step 4: Implement the plugin

Open `crates/<your-plugin-id>/src/lib.rs`.

### 4a. Set the plugin ID

```rust
pub const YOUR_PLUGIN_ID: &str = "<your-plugin-id>";
```

### 4b. Fill in the manifest

Every field is required. Common mistakes:

- `id` must match the constant above exactly
- `named_graph_slug` is `Some("your-slug")` for producers, `None` for everything else
- `wasm_compatible: false` if the plugin uses native-only code, otherwise `true`
- `needs_full_graph: false` unless the plugin genuinely needs all triples (postprocess only)
- `failure_policy: FailurePolicy::Required` or `FailurePolicy::Optional` per user's answer

### 4c. Implement the stage method

**Preprocess** — reads from context, optionally replaces model:

```rust
fn preprocess(&self, ctx: &mut PipelineContext) -> Result<(), PreprocessError> {
    let model = ctx.get::<ifc_model::IfcModel>()
        .ok_or_else(|| PreprocessError::Preprocessing("missing IfcModel".into()))?;

    // ... read model, validate, compute something ...

    // If you modified the model, write it back:
    // ctx.replace(Arc::new(updated_model));

    Ok(())
}
```

**Producer** — emits RDF triples:

```rust
fn produce(&self, ctx: &PipelineContext, sender: &Sender<TaggedBatch>) -> Result<(), ProducerError> {
    let model = ctx.get::<ifc_model::IfcModel>()
        .ok_or_else(|| ProducerError::Conversion("missing IfcModel".into()))?;
    let options = ctx.get::<ConvertOptions>()
        .ok_or_else(|| ProducerError::Conversion("missing ConvertOptions".into()))?;

    let graph_iri = BatchKind::new(format!(
        "{}{}", options.base_uri.trim_end_matches('/'), GRAPH_SLUG,
    ));

    for chunk in model.entities().chunks(options.stream_batch_size) {
        let triples = chunk.iter().map(|e| /* build Triple */ ).collect();
        sender.send(TaggedBatch { kind: graph_iri.clone(), triples })
            .map_err(|_| ProducerError::ChannelClosed)?;
    }
    Ok(())
}
```

### Rules you must follow

- Never use `unwrap()` or `expect()` — always return the typed `Err` variant
- Never call `ctx.insert()` for a type already in context — use `ctx.replace()`
- Never hold a Mutex lock across a `sender.send()` call
- If unsure what types are available in context, check `make_pipeline_context()` in
  `crates/ifc2lbd-wasm/src/runner.rs` (lines ~47–64) — those are always present

---

## Step 5: Register the plugin — CLI

Open `crates/ifc2lbd-cli/src/pipeline_plugins.rs`.

Add the import at the top:

```rust
use <your_crate_name>::YourPlugin;
```

Inside `built_in_registry()`, add one line:

```rust
registry.register_preprocess(YourPlugin).unwrap();  // or register_producer / register_postprocess / register_export
```

Also add the crate as a dependency in `crates/ifc2lbd-cli/Cargo.toml`:

```toml
[dependencies]
<your-crate-name> = { path = "../<your-plugin-id>" }
```

---

## Step 6: Register the plugin — WASM

**Skip this step if the user said CLI only / `wasm_compatible: false`.**

Open `crates/ifc2lbd-wasm/src/plugins.rs`.

Add the import and register line — identical pattern to the CLI step above, but inside `browser_registry()`.

Also add the dependency in `crates/ifc2lbd-wasm/Cargo.toml`.

---

## Step 7: Verify

Run these in order and fix any errors before reporting done:

```bash
# 1. Compiles?
cargo check -p ifc2lbd-cli

# 2. Plugin appears in the registry?
cargo build --release -p ifc2lbd-cli
./target/release/ifc2lbd-neo --list-modules | grep <your-plugin-id>

# 3. Activation plan resolves correctly?
./target/release/ifc2lbd-neo <any-ifc-file> --module <your-plugin-id> --show-module-plan

# 4. Full run on a small file (test.ifc is in web/wasm-prototype/public/):
./target/release/ifc2lbd-neo web/wasm-prototype/public/test.ifc \
    --module <your-plugin-id> \
    --module neo-turtle-serializer \
    --module neo-stdout-export

# 5. Registry tests still pass?
cargo test -p ifc2lbd-cli -- pipeline_plugins
```

If step 4 produces output without panics or errors, the plugin is working.

---

## Step 8: Report back

Tell the user:

- The plugin ID and crate path
- What it does
- The exact `--module <id>` flag to activate it
- Any `--module-opt <id>.<key>=<value>` options if you added typed config
- Results of the test run (triple count, any warnings)

---

## Reference: What is available in PipelineContext

These types are always inserted before any plugin runs:

- `ifc_model::IfcModel` — `ctx.get::<IfcModel>()` — parsed IFC entities, relationships, property sets
- `ifc_step::StepFile` — `ctx.get::<StepFile>()` — raw STEP file, schema version, header
- `lbd_converter::ConvertOptions` — `ctx.get::<ConvertOptions>()` — base_uri, batch_size, feature flags

A preprocess plugin can also insert new types for producers to use:

```rust
ctx.insert(Arc::new(MyPrecomputedLookup { ... }));
// then in a producer:
let lookup = ctx.get::<MyPrecomputedLookup>().unwrap();
```

## Reference: Working example

`crates/plugin-topology-full/` is a complete, registered producer plugin.
Read it alongside the template when implementing.
