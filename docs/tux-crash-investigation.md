# TUX Crash Investigation — Observed Facts Only

## Current status (2026-06-03)

- Native CLI no longer uses the legacy BSP path for the failing solid-solid boolean chain on TUX.
- Native CLI now succeeds on TUX with Manifold CSG enabled.
- The browser/WASM build remains on the legacy non-Manifold path.
- A direct Manifold enable for `wasm32-unknown-unknown` was built and tested, but the browser trapped during `neo-geometry-preprocess` with `ERROR: unreachable executed`.
- That WASM target is explicitly provisional upstream and runs without a C++ exception runtime, so an internal Manifold throw becomes a hard wasm trap rather than a recoverable Rust error.
- Decision: keep Manifold enabled for native CLI only; keep WASM on the previous working path until either `wasm32-unknown-emscripten` is adopted or the `wasm32-unknown-unknown` Manifold failure is reproduced and fixed upstream.

## What is known with certainty

### Test matrix (release build, current committed code)

| Command | Result |
|---------|--------|
| TUX + `neo-geometry-preprocess` + `neo-geometry-producer` + `neo-file-export` | **Works**, completes in ~1.5s |
| TUX + `neo-bot-producer` + `neo-turtle-serializer` + `neo-file-export` | **Works**, completes in ~0.5s |
| TUX + `neo-bsdd-producer` + `neo-turtle-serializer` + `neo-file-export` | **Works**, completes in ~14s |
| TUX + `neo-geometry-preprocess` + `neo-geometry-producer` + `neo-bot-producer` + `neo-bsdd-producer` + `neo-turtle-serializer` + `neo-file-export` | **CRASHES** — `thread 'ifc2lbd-rayon-6' has overflowed its stack` |
| TUX + all of the above **in WASM** | **Works**, completes in ~65s |

### Crash details

- Crash type: `fatal runtime error: stack overflow, aborting`
- Crashing thread: `ifc2lbd-rayon-6` (a rayon worker thread)
- Crash occurs immediately after `phase build_model completed` — before any preprocess or producer log lines appear
- Exit code 0 (stack overflow does not set non-zero exit on this platform)
- Reproducible every run

### What was tried and what it showed

1. **`rayon::ThreadPoolBuilder::new().stack_size(256MB).build_global()`** — rayon worker `ifc2lbd-rayon-6` still crashes
2. **`rayon::ThreadPoolBuilder::new().stack_size(512MB).build_global()`** — rayon worker still crashes
3. **`rayon::ThreadPoolBuilder::new().stack_size(1GB).build_global()`** — rayon worker still crashes
4. **`stacker::maybe_grow` on BSP recursion points** — does not crash but stacker allocates at least 40MB of additional stack segments before the process was killed (test timed out). RAM grows without bound.
5. **Geometry-only** on TUX (no LBD): works fine with the dedicated std::thread approach
6. **LBD-only** on TUX (no geometry): works fine

### What is NOT known

- Whether the crash is caused by the geometry processing, the bSDD producer, the BOT producer, or an interaction between them
- Why adding LBD producers causes a crash when each works independently
- Why WASM does not crash — **this is unexplained**. The shadow-stack hypothesis was rejected by observation: if that were the cause, WASM would also eventually run out of memory on the same model, which it does not
- Whether this crash existed before the geometry parallelism changes (never tested TUX with geometry+LBD before today)

## Next diagnostic steps needed (in order — do not skip)

### Step 1: find the minimal crashing combination
Run these two tests before touching any code:

```bash
# A: geo + BOT only (no bSDD)
target/release/ifc2lbd-neo TUX.ifc \
  --module neo-geometry-preprocess --module neo-geometry-producer \
  --module-opt neo-geometry-producer.format=fragments \
  --module neo-bot-producer \
  --module neo-turtle-serializer --module neo-file-export \
  -o /tmp/tux_a.ttl

# B: geo + bSDD only (no BOT)
target/release/ifc2lbd-neo TUX.ifc \
  --module neo-geometry-preprocess --module neo-geometry-producer \
  --module-opt neo-geometry-producer.format=fragments \
  --module neo-bsdd-producer \
  --module neo-turtle-serializer --module neo-file-export \
  -o /tmp/tux_b.ttl
```

Expected outcomes and what they tell us:
- A crashes, B works → crash is in BOT producer or its interaction with geo
- A works, B crashes → crash is in bSDD producer or its interaction with geo
- Both work → crash only happens with ALL three together (three-way interaction)
- Both crash → either geo interaction crashes any LBD producer

### Step 2: check git history
The user reports bSDD+geo worked ~3 commits ago. Run `git log --oneline -5` and `git diff HEAD~3 -- crates/ifc2lbd-cli/src/main.rs crates/ifc-geometry/src/lib.rs` to find what changed in those commits that could affect rayon thread pool state.

### Step 3: check rayon pool initialization order
The crash occurs in a rayon worker **immediately after build_model** — before any preprocess plugin logs. This timing means the crash may be in a rayon task that was enqueued *during* build_model and is still executing when the next operation starts. Verify whether `build_model` or `parse_step_bytes` leave rayon tasks running asynchronously.

## Why WASM does not crash

**Unknown.** The shadow-stack hypothesis was rejected: if the WASM process used >4GB of stack-equivalent memory for BSP recursion, it would also fail in WASM, which it does not. The correct explanation has not been found. Do not implement fixes based on stack size assumptions until the true cause is identified via Step 1 above.

## What was tried and failed (do not retry these)

- `build_global` with 256MB, 512MB, 1GB rayon worker stacks — all still crash
- `stacker::maybe_grow` on BSP recursion points — RAM grows without bound, no crash but process never completes normally
- Dedicated `std::thread` with large stack for geometry — geometry-only works, combined still crashes
- Iterative BSP tree (non-recursive) — correct but ~10× slower than recursive version, unacceptable
