# Plan: Complete Modularization Cleanup

## Context
The produce() trait was recently wired for the 3 main dispatch paths in runner.rs. An audit found several remaining issues: a broken test import, 2 hardcoded dispatch paths still bypassing the trait, duplicated helper code between WASM and CLI, and `ExecutionSettings.emit_*` booleans that should be replaced by the existing `ActivationPlan.enabled_ids`. This plan closes all remaining gaps.

---

## Step 1 — Fix broken test imports (CRITICAL — won't compile)
**File:** `crates/ifc2lbd-wasm/src/tests.rs`

`LBD_PRODUCER_ID` was deleted when the monolithic producer was replaced. The test file still imports and uses it at lines 9, 24, 45, 64, 95.

- Remove `LBD_PRODUCER_ID` from imports; add `BOT_PRODUCER_ID`, `BEO_PRODUCER_ID`, `PROPS_OPM_PRODUCER_ID`, `OMG_FOG_PRODUCER_ID`
- Line 24: replace the single `ids.contains(LBD_PRODUCER_ID)` assertion with individual assertions for all 5 producers (BOT, BEO, PROPS_OPM, OMG_FOG, IFCOWL)
- Lines 45, 64, 95: replace `LBD_PRODUCER_ID.to_string()` with `BOT_PRODUCER_ID.to_string()` (a single active producer is enough for the test to be meaningful)

---

## Step 2 — Extract `forward_as_tagged` to `lbd-pipeline`
**Files:** `crates/lbd-pipeline/src/lib.rs`, `crates/ifc2lbd-wasm/src/plugins.rs`, `crates/ifc2lbd-cli/src/pipeline_plugins.rs`

The helper is copy-pasted identically in both crates. Extract it to `lbd-pipeline` and delete both private copies.

Add to `lbd-pipeline/src/lib.rs`:
```rust
pub fn forward_as_tagged(
    raw_receiver: Receiver<Vec<Triple>>,
    kind: BatchKind,
    tagged_sender: Sender<TaggedBatch>,
) {
    rayon::spawn(move || {
        for batch in raw_receiver {
            if tagged_sender.send(TaggedBatch { kind: kind.clone(), triples: batch }).is_err() { break; }
        }
    });
}
```

Both `plugins.rs` and `pipeline_plugins.rs` already import `lbd_pipeline::*` — just add `forward_as_tagged` to those imports and remove the private copies.

---

## Step 3 — Replace turtle default/fallback path with `spawn_producers`
**File:** `crates/ifc2lbd-wasm/src/runner.rs` (~lines 960–1098)

This path still has hardcoded `if emit_bot_turtle { stream_bot(...) }` blocks. Replace with the same pattern used in the 3 already-wired paths:
```rust
let producer_ids = active_producer_ids_from_settings(&settings);
let receivers = spawn_producers(&producer_ids, &registry, &ctx, chan_cap);
for (id, rx) in receivers { /* drain TaggedBatch into serializer sink */ }
```

---

## Step 4 — Replace `export_browser_files` in-memory path with `spawn_producers`
**File:** `crates/ifc2lbd-wasm/src/runner.rs` (~lines 2028–2130)

The `collect_producer!` macro calls `stream_bot`, `stream_beo` etc. directly. Replace by:
1. Calling `spawn_producers()` with unbounded channels (single-threaded path, no backpressure needed)
2. Collecting all `TaggedBatch` items grouped by `batch.kind` (the graph IRI slug)
3. Writing each group as a named graph using the existing `write_nquads_batch()` call

The graph IRI is already embedded in `TaggedBatch.kind` by each plugin's `produce()` — no routing table needed.

---

## Step 5 — Thread `ActivationPlan` through; remove `emit_*` booleans
**Files:** `crates/ifc2lbd-wasm/src/types.rs`, `crates/ifc2lbd-wasm/src/runner.rs`, `crates/ifc2lbd-wasm/src/validation.rs`

`ExecutionSettings` has 8 `emit_*` booleans that duplicate `ActivationPlan.enabled_ids`. The `active_producer_ids_from_settings()` helper rebuilds a list from them unnecessarily.

- Store `ActivationPlan` in `ExecutionSettings` (or pass it alongside it into dispatch functions)
- Replace `active_producer_ids_from_settings(&settings)` with:
  ```rust
  plan.enabled_ids.iter().filter(|id| registry.producer(id).is_some()).cloned().collect()
  ```
- Remove the 8 `emit_*` fields from `ExecutionSettings`
- Remove the corresponding assignments in `validation.rs` (lines ~167–172)
- The `active_producer_ids_from_settings()` function itself becomes dead code — delete it

---

## Files to Modify
| File | Change |
|------|--------|
| `crates/ifc2lbd-wasm/src/tests.rs` | Replace `LBD_PRODUCER_ID` with 5 individual producer IDs |
| `crates/lbd-pipeline/src/lib.rs` | Add public `forward_as_tagged()` |
| `crates/ifc2lbd-wasm/src/plugins.rs` | Remove private `forward_as_tagged()` |
| `crates/ifc2lbd-cli/src/pipeline_plugins.rs` | Remove private `forward_as_tagged()` |
| `crates/ifc2lbd-wasm/src/runner.rs` | Replace turtle-default + export_browser_files dispatch; remove emit_* usage |
| `crates/ifc2lbd-wasm/src/types.rs` | Remove `emit_*` fields, add `ActivationPlan` |
| `crates/ifc2lbd-wasm/src/validation.rs` | Remove emit_* assignments |

## Execution order
Steps 1 → 2 → 3 → 4 → 5. Each step is independently verifiable with `cargo check -p ifc2lbd-cli`.

## Verification
```bash
cargo check -p ifc2lbd-cli          # 0 errors after each step
docker compose run --rm check       # WASM crate with wasm32 target
cargo test -p lbd-pipeline          # lib unit tests
```
