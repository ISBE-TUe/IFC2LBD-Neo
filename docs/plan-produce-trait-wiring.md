# Plan: Wire `produce()` Trait — Remove Hardcoded Producer Dispatch

## Problem

The `ProducerPlugin` trait has a `produce()` method, and most plugins already implement
it correctly in `ifc2lbd-wasm/src/plugins.rs`. But neither the WASM runner nor the CLI
ever calls it. Both runtimes bypass the trait entirely with hardcoded direct calls:

```
runner.rs → stream_bot(), stream_beo(), stream_props_opm(), …  (direct)
main.rs   → stream_step_and_model(), stream_topology_model()   (direct)
```

Three plugin `produce()` methods are stubs that return an error:
- `IfcTopologyProducerPlugin`
- `BboxEnricherPlugin`
- `TopologyFullProducerPlugin`

The consequence: adding a new producer module requires editing the runner's dispatch
block in addition to writing the plugin. That defeats the whole point of the registry.

---

## What already exists (no changes needed)

| Thing | Where | Status |
|-------|-------|--------|
| `ProducerPlugin::produce(&ctx, &sender)` trait | `lbd-pipeline/src/lib.rs:239` | ✅ done |
| `PipelineContext` with `insert<T>()` / `get<T>()` | `lbd-pipeline/src/lib.rs:70` | ✅ done |
| `TaggedBatch { graph_iri, triples }` type | `lbd-pipeline/src/lib.rs:146` | ✅ done |
| `produce()` implementations for Bot/Beo/Props/Omg/Ifcowl | `ifc2lbd-wasm/src/plugins.rs` | ✅ done |
| Plugin registry with `plugin(id)` lookup | `lbd-pipeline/src/lib.rs` | ✅ done |

---

## Step 1 — Populate `PipelineContext` before producer dispatch

**Files:** `ifc2lbd-wasm/src/runner.rs`, `ifc2lbd-cli/src/main.rs`

After the model is built and options are resolved, construct a `PipelineContext` and
insert the three data types every producer needs:

```rust
use std::sync::Arc;
use lbd_pipeline::{PipelineContext, ResourceLimits};

let mut ctx = PipelineContext::new(ResourceLimits::auto(threads, Some(mem_bytes)));
ctx.insert(Arc::new(step.clone()));      // Arc<StepFile>
ctx.insert(Arc::new(model.clone()));     // Arc<IfcModel>
ctx.insert(Arc::new(options.clone()));   // Arc<ConvertOptions>
```

`ConvertOptions` already carries `base_uri`, so producers derive their graph IRI from
it without a separate field in the context.

For **bbox** and **topology-full**, additional data goes in after their compute phase:

```rust
// after bbox collection:
ctx.insert(Arc::new(mesh_bboxes));       // Arc<HashMap<EntityId, BoundingBox>>

// after CSG / adjacency:
ctx.insert(Arc::new(adjacency_result));  // Arc<AdjacencyRelations> (new type, see Step 3b)
```

---

## Step 2 — Replace hardcoded dispatch in runner.rs with a trait loop

**File:** `ifc2lbd-wasm/src/runner.rs`

Currently `turtle_to_sink` and `nquads_to_sink` each have a long `rayon::spawn` block
that manually calls `stream_bot()`, `stream_beo()` etc. for every producer.

Replace this with a generic helper that works for both paths:

```rust
fn spawn_producers(
    active_ids: &[String],
    registry: &PluginRegistry,
    ctx: &PipelineContext,
) -> Vec<(String, crossbeam::channel::Receiver<TaggedBatch>)> {
    active_ids
        .iter()
        .filter_map(|id| {
            let plugin = registry.producer(id)?;
            let (tx, rx) = crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
            let plugin = Arc::clone(&plugin);
            let ctx = ctx.clone();
            rayon::spawn(move || {
                if let Err(e) = plugin.produce(&ctx, &tx) {
                    tracing::warn!("producer {id} failed: {e:?}");
                }
            });
            Some((id.clone(), rx))
        })
        .collect()
}
```

The caller receives a `Vec<(id, Receiver<TaggedBatch>)>` and drains each receiver into
the appropriate sink. The graph IRI is already embedded in each `TaggedBatch` by the
plugin — no routing table needed.

Add a `producer(id) -> Option<Arc<dyn ProducerPlugin>>` method to `PluginRegistry`
(mirrors the existing `plugin(id)` method).

---

## Step 3 — Implement the three stubs

### 3a. `IfcTopologyProducerPlugin`

Calls `lbd_converter::stream_topology_model()`:

```rust
fn produce(&self, ctx: &PipelineContext, sender: &Sender<TaggedBatch>) -> Result<(), ProducerError> {
    let model = ctx.get::<IfcModel>().ok_or(…)?;
    let options = ctx.get::<ConvertOptions>().ok_or(…)?;
    let graph_iri = BatchKind::new(format!("{}/topology", options.base_uri.trim_end_matches('/')));
    let (tx, rx) = crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);
    forward_as_tagged(rx, graph_iri, sender.clone());
    lbd_converter::stream_topology_model(&model, &TopologyOptions { base_uri: options.base_uri.clone() }, &tx)
        .map(|_| ()).map_err(|_| ProducerError::ChannelClosed)
}
```

### 3b. `BboxEnricherPlugin`

Bbox collection requires the `StepFile` and is CPU-heavy (CSG/parry3d). The plugin
cannot run that inline — the runner must compute bboxes in the pre-produce phase and
insert them into the context (see Step 1).

```rust
fn produce(&self, ctx: &PipelineContext, sender: &Sender<TaggedBatch>) -> Result<(), ProducerError> {
    let model = ctx.get::<IfcModel>().ok_or(…)?;
    let options = ctx.get::<ConvertOptions>().ok_or(…)?;
    let bboxes = ctx.get::<HashMap<EntityId, BoundingBox>>().ok_or(…)?;
    // emit WKT triples for each element that has a bbox
    let graph_iri = BatchKind::new(format!("{}/bbox", options.base_uri.trim_end_matches('/')));
    // … stream triples into sender …
}
```

This requires extracting the WKT-triple emission logic from the CLI into
`lbd-converter` as `stream_bbox_geometry(model, bboxes, options, sender)`.

### 3c. `TopologyFullProducerPlugin`

Same pattern as `IfcTopologyProducerPlugin` but also reads
`Arc<AdjacencyRelations>` from the context (pre-computed by the runner before
calling `spawn_producers`). Calls `plugin_topology_full::stream_topology_full()`.

---

## Step 4 — Apply the same pattern to the CLI

**File:** `ifc2lbd-cli/src/main.rs`

The CLI currently calls `stream_step_and_model()` (monolithic) and
`stream_topology_model()` directly. Replace with the same `spawn_producers` helper.

The CLI already builds an `ActivationPlan` and a `built_in_registry`, so the
iteration is trivial once `spawn_producers` exists in a shared location.

Extract `spawn_producers` into `lbd-pipeline` or a new `lbd-runner` crate so both
WASM and CLI import it, rather than duplicating it.

---

## Step 5 — Delete the old direct-call blocks

Once all producers go through `spawn_producers`:
- Remove the per-producer `if settings.emit_bot { … } if settings.emit_beo { … }` blocks from `runner.rs`
- Remove `stream_step_and_model()` from `producer_plugins.rs` (CLI)
- The `ExecutionSettings.emit_*` booleans in the WASM types can be replaced by the `ActivationPlan.enabled_ids` set (they already duplicate each other)

---

## Migration order

1. **Step 1** — populate context (no behaviour change, safe)
2. **Step 2** — add `spawn_producers` and `registry.producer()` — use it for the five working producers first, keep the three stubs behind a feature flag / `FailurePolicy::Disabled`
3. **Step 3a** — implement `IfcTopologyProducerPlugin` (straightforward, no new context data needed)
4. **Step 3b/3c** — implement Bbox and TopologyFull (require context plumbing)
5. **Step 4** — apply to CLI
6. **Step 5** — delete dead code

Each step is independently testable: the E2E Playwright tests and criterion benchmarks
written in the previous session catch regressions after every step.

---

## Files touched

| File | Change |
|------|--------|
| `lbd-pipeline/src/lib.rs` | Add `PluginRegistry::producer()` method |
| `lbd-pipeline/src/lib.rs` or new `lbd-runner` crate | Add `spawn_producers()` helper |
| `ifc2lbd-wasm/src/runner.rs` | Replace emit_* dispatch blocks with `spawn_producers` |
| `ifc2lbd-wasm/src/plugins.rs` | Implement 3 stubs (Steps 3a–3c) |
| `ifc2lbd-cli/src/main.rs` | Replace direct calls with `spawn_producers` |
| `lbd-converter/src/lib.rs` | Add `stream_bbox_geometry()` (for Step 3b) |
