# Open Work Items

## Performance

### 2. Geometry dedup gap (1502 vs 662 distinct shells)

**File:** `crates/plugin-geometry-producer/src/lib.rs` — `build_meshes`, `docs/geometry-dedup-todo.md`

For the TUX model, fragments output is 29 MB instead of the oracle's ~14 MB because
repeated geometry instances are not deduplicated. Option A in the dedup doc — content-hash
on the ifc-lite mesh before tessellation — is marked low-complexity and recommended first.
Implementing it in `build_meshes` is a self-contained change in the producer crate.

### 3. WASM `StepFile` re-parse on every geometry run

**File:** `crates/ifc2lbd-wasm/src/runner.rs:405`

`run_to_sink` re-parses the raw IFC bytes a second time via `parse_step_bytes` to get a
`StepFile` for the geometry pipeline, because `StepFile` is not `Clone`. Costs ~50 ms on
TUX. Fix: wrap `StepFile` in `Arc` at first parse and thread it through, or derive `Clone`
on `StepFile` if feasible upstream.

---

## Architecture / Modularisation

### 4. Three identical pipeline context setup blocks in WASM runner

**File:** `crates/ifc2lbd-wasm/src/runner.rs:1168, 1327, 1520`

`turtle_to_sink_joined`, `turtle_to_sink_separate`, and `nquads_to_sink` each contain an
identical 15-line block: `make_pipeline_context` → QTO insert → `CompressOutput` insert →
preprocess-ID filter → `run_preprocess_with_events`. Extract into a shared helper
`prepare_lbd_context(settings, model, options, step, sink, chan_cap)`.

### 5. `sidecar_tx` not wired in LBD context paths

**File:** `crates/ifc2lbd-wasm/src/runner.rs:1168, 1327, 1520`

`make_pipeline_context` does not set `ctx.sidecar_tx`. The three LBD call sites never set
it either. If a geometry producer were activated alongside LBD in these paths, its sidecar
output would be silently dropped. Address alongside item 4 above.

### 6. Dynamic plugin system not started

**File:** `docs/plan-dynamic-plugins.md`

All four phases of the WASM dynamic plugin system (custom plugin ABI, SDK, loader,
hot-reload) remain unchecked. No crates exist yet. This is a larger initiative — add to
roadmap rather than near-term backlog.

---

## Docs cleaned up this session

- Deleted `docs/fragments-geometry-port-plan.md` (ABANDONED stub)
- Deleted `docs/fragments-producer-handoff.md` (SUPERSEDED)
