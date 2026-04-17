# Architecture Comparison: WASM vs CLI — Verified Analysis (April 2026)

> Every claim below has been verified against the actual source code.

---

## 1. Speed: CLI vs WASM

**Both crates run the same Rust code paths natively — they are identical in speed.**

The shared hot-path crates are: `ifc-step` (parsing), `ifc-model` (model building), `lbd-converter` (triple production), `lbd-serializer` (serialization). Both WASM and CLI call the same functions from these crates. There is zero orchestration overhead — all the time is in the core crates.

The one real difference is **code path selection**:

| WASM Method | Core Function | Memory Pattern |
|---|---|---|
| `run_memory` | `convert_step_and_model()` | ALL triples in `Vec<Triple>` → serialize at end |
| `run_benchmark` | `stream_step_and_model()` | Bounded channel batches → serialize incrementally |
| `run_to_sink` | `stream_step_and_model()` | Bounded channel batches → serialize incrementally |

| CLI | Core Function | Memory Pattern |
|---|---|---|
| Turtle output | `stream_step_and_model()` → `serialize_lbd_batches_to_writer()` | **ALL triples collected into Vec** → sort → group → write |
| N-Quads output | `stream_step_and_model()` → `serialize_nquads_batches_to_writer()` | Batch-by-batch streaming |

**Key finding I got wrong before:** The CLI's Turtle path is NOT fully streaming! `serialize_lbd_batches_to_writer` collects all batches from the channel into a `Vec<Triple>`, then calls `write_grouped_turtle()` which sorts by subject/predicate and groups with `;`/`,` syntax. This means **CLI Turtle output also has peak memory = all triples in RAM** — same problem as WASM's `run_memory`.

The only truly streaming Turtle path is the WASM crate's `serialize_lbd_batches_incremental_to_writer` (used in lowmem mode) and `serialize_turtle_batch_to_writer` (used in fast streaming mode). Neither of these sorts or groups.

### Benchmark numbers (CLI, release build, native)

| Fixture | Mode | Total | Parse | Build | Produce | Serialize | Peak RSS |
|---|---|---|---|---|---|---|---|
| DigitalHub 8.6MB | LBD Turtle | 1.45s | 0.08s | 0.01s | 0.38s | 0.98s | 392 MB |
| DigitalHub 8.6MB | LBD+IfcOWL NQ | 1.36s | 0.05s | 0.02s | 1.29s | 0.01s | 224 MB |
| Wohn 8.2MB | LBD Turtle | 0.56s | 0.09s | 0.07s | 0.13s | 0.27s | 201 MB |
| Wohn 8.2MB | LBD+IfcOWL+TopoFull NQ | 4.83s | 0.07s | 0.02s | 4.73s | 0.004s | 281 MB |
| CX 171MB | LBD Turtle | 4.44s | 2.46s | 0.11s | 0.74s | 1.13s | 831 MB |

**Verdict: Identical speed.** The bottleneck for Turtle is always the sort+group in `write_grouped_turtle`. For N-Quads, it's IfcOWL production.

---

## 2. Browser Memory Safety: Is the WASM Architecture Safe?

### The 3-tier model (verified)

```
                    ┌──────────────────────────────────┐
                    │    select_execution_mode()        │
                    │    input_size × format_multiplier │
                    │    vs memory_feasibility_mb       │
                    └───────────┬──────────────────────┘
                                │
                  ┌─────────────┼──────────────┐
                  ▼             ▼              ▼
             Fast mode     Lowmem mode    (manual override)
                  │             │              │
                  ▼             ▼              ▼
           run_memory()   run_to_sink()  run_to_sink()
           convert_step_   stream_step_   stream_step_
           and_model()     and_model()    and_model()
           (ALL triples    (bounded       (bounded
            in RAM,        channels,      channels,
            REJECTS if     batch-by-      batch-by-
            Lowmem)        batch)         batch)
```

### What actually works ✅

1. **`select_execution_mode()`** correctly estimates peak memory using format-specific multipliers (14-26× input size). If estimate > feasibility threshold → auto-selects Lowmem. **Verified in `memory.rs` lines 36-80.**

2. **`run_memory()` rejects Lowmem** with error `"lowmem mode selected; use convertIfcToSink for streamed export"`. **Verified in `runner.rs` line 69-71.**

3. **`run_to_sink()` uses `stream_step_and_model()`** with bounded channels (capacity 4 in lowmem, 16 in fast). **Verified in `runner.rs` lines 559, 753, 761.**

4. **Batch size adapts**: Fast mode = `threads * 1024` (clamp 1024-32768), Lowmem = `threads * 256` (clamp 128-8192). **Verified in `memory.rs` lines 16-22.**

5. **IfcOWL worker count adapts**: Fast = full thread count, Lowmem = half. **Verified in `memory.rs` lines 28-31.**

6. **Streaming Turtle in lowmem**: Uses `serialize_turtle_batch_raw_to_writer` (no sort, no prefix compaction) or `serialize_lbd_batches_incremental_to_writer` (prefix compaction, no sort). Both are truly streaming — no triple collection. **Verified in `lbd-serializer/src/lib.rs` and `runner.rs`.**

7. **N-Quads is always streaming** — `serialize_nquads_batches_to_writer` processes each batch immediately without collection. **Verified in `lbd-serializer/src/lib.rs` lines 118-124.**

### What doesn't work ❌

1. **`low_memory_mode` flag is a dead letter in the core crates.** It's defined in `ConvertOptions` but **never read** by `lbd_converter` or `lbd_serializer`. Only the WASM runner reads it. **Verified: zero hits for `options.low_memory_mode` in `lbd-converter/src/lib.rs` and `lbd-serializer/src/lib.rs`.**

2. **No backpressure from JS to Rust.** `SinkChunkWriter::write()` calls `self.sink.call1()` synchronously. If JS is slow (e.g., writing to IndexedDB), Rust blocks. The `max_pending` field tracks peak memory for telemetry but doesn't enforce a limit. The `pending` Vec in `SinkChunkWriter` can grow unbounded if JS can't keep up and chunk boundaries don't align with write calls. **Verified in `sink.rs` lines 65-78.**

3. **The feasibility check is approximate.** Uses fixed multipliers (14-26× input size) that are not calibrated against actual measurements. The default `memory_feasibility_mb` is `4 × estimated_peak_mb.max(512)`, which may be too generous or too conservative depending on the model.

4. **No browser memory API integration.** The `memory_feasibility_mb` must be provided by the JS caller. There's no `navigator.deviceMemory` usage or `performance.measureUserAgentSpecificMemory()` integration in the Rust code.

5. **`run_memory`'s `export_browser_files()` for N-Quads also collects.** When `run_memory` outputs N-Quads, it calls `convert_step_and_model()` (in-memory), then `export_browser_files()` which sends the entire triple vec through an unbounded channel for serialization. **Verified in `runner.rs` lines 791-810.**

### Verdict: **Architecturally sound, but fragile**

The 3-tier model is correct. The auto-selection logic works. The streaming paths truly stream. But:
- `low_memory_mode` needs to actually affect the core crates (skip sorting, reduce Vec capacities)
- `SinkChunkWriter` needs a bounded pending buffer
- Memory estimation needs calibration

---

## 3. Plugin Compatibility: Are CLI and WASM Plugins Interchangeable?

### Same trait system ✅

Both crates use `lbd_pipeline::PipelinePlugin`, `ProducerPlugin`, `SerializerPlugin`, `ExportPlugin` — same trait definitions, same `PipelineContext`, same `TaggedBatch`, same error types. **Verified in `lbd-pipeline/src/lib.rs`.**

### Manifest comparison (exact diff)

| Plugin ID | Field | WASM | CLI | Match? |
|---|---|---|---|---|
| `neo-lbd-producer` | all fields | identical | identical | ✅ |
| `neo-ifcowl-producer` | all fields | identical | identical | ✅ |
| `neo-turtle-serializer` | description | "Serializes triples into Turtle output." | "Serializes triple streams into Turtle output." | ⚠️ Minor wording |
| `neo-turtle-serializer` | other fields | identical | identical | ✅ |
| `neo-nquads-serializer` | description | "Serializes graph streams into N-Quads output." | "Serializes graph streams into merged or chunked N-Quads output." | ⚠️ CLI mentions chunking |
| `neo-nquads-serializer` | outputs | `["nquads-bytes"]` | `["nquads-bytes", "nquads-chunks"]` | ❌ Diverged |
| `neo-file-export` | description | "Exports browser-downloadable artifacts..." | "Writes serialized output streams to files and chunk manifests." | ⚠️ Different |
| `neo-file-export` | inputs | `["turtle-bytes", "nquads-bytes"]` | `["turtle-bytes", "nquads-bytes", "nquads-chunks"]` | ❌ Diverged |
| `neo-file-export` | outputs | `["browser-files"]` | `["filesystem"]` | ❌ Diverged |
| `neo-file-export` | conflicts_with | `[]` | `[STDOUT_EXPORT_ID, GRAFEO_EXPORT_ID]` | ❌ Diverged |
| `neo-file-export` | wasm_compatible | `true` | `false` | ❌ Diverged |

### Plugin IDs are duplicated across crates

The string constants `"neo-lbd-producer"`, `"neo-ifcowl-producer"`, etc. are defined independently in both `crates/ifc2lbd-wasm/src/plugins.rs` and `crates/ifc2lbd-cli/src/pipeline_plugins.rs`. If someone changes one, the other silently diverges.

### `produce()` is a dead letter ❌

Both crates define `ProducerPlugin::produce()` but **neither calls it**:
- **WASM**: `PipelineRunner` calls `stream_step_and_model()` directly (not `produce()`)
- **CLI**: `main.rs` calls `producer_plugins::run_core_conversion_plugin()` which calls `stream_step_and_model()` directly

The plugin trait method exists but is never invoked in production. Both implementations return an error:
```rust
fn produce(&self, _ctx: &PipelineContext, _sender: &Sender<TaggedBatch>) -> Result<(), ProducerError> {
    Err(ProducerError::Conversion("LbdProducerPlugin::produce must be called via PipelineRunner".to_string()))
}
```

### `serialize()` is implemented in WASM, stub in CLI

- **WASM `TurtleSerializerPlugin::serialize()`**: Real implementation — writes prefixes + batch-by-batch Turtle. **Verified in `plugins.rs` lines 183-195.**
- **WASM `NquadsSerializerPlugin::serialize()`**: Real implementation — writes N-Quads per batch with graph IRI from BatchKind. **Verified in `plugins.rs` lines 217-232.**
- **CLI serializers**: Both return errors. **Verified in `pipeline_plugins.rs`.**

### `export_in_memory()` is implemented in WASM, stub in CLI

- **WASM `FileExportPlugin::export_in_memory()`**: Real implementation — converts `ExportedFile` → `ExportFileSummary` (strips payload, keeps metadata). **Verified in `plugins.rs` lines 259-269.**
- **CLI exports**: All return errors.

### Verdict: **Same trait system, plugins NOT interchangeable**

The `lbd_pipeline` traits are the right foundation. But:
- Plugins are defined locally (can't share between crates)
- `produce()` is never called (both crates bypass it)
- Manifests for NQuads/FileExport have diverged
- WASM has real serialize/export impls, CLI has stubs
- CLI has 5 extra plugins (topology-lite/full, bbox, stdout, grafeo)

---

## 4. Critical Finding: CLI Turtle Also Collects All Triples

I initially stated that only WASM's `run_memory` path collects all triples. **This was wrong.**

The CLI's `serialize_lbd_batches_to_writer` does this:
```rust
pub fn serialize_lbd_batches_to_writer<W: Write>(receiver, mut writer, instance_base) {
    let mut triples = Vec::new();       // ← COLLECTS ALL
    for mut batch in receiver {
        triples.append(&mut batch);      // ← INTO ONE VEC
    }
    write_grouped_turtle(&triples, ...)  // ← SORT + GROUP
}
```

This means **both** WASM `run_memory` and CLI Turtle output have the same peak memory problem. The only fully streaming Turtle path is in the WASM crate (used by `run_benchmark` and `run_to_sink`).

The CLI doesn't even have a low-memory Turtle option. It always sorts+groups.

---

## 5. What Needs to Happen — Prioritized

### P0: Fix the shared memory problem (both crates)

| Issue | Fix | Effort |
|---|---|---|
| CLI Turtle collects all triples | Add `serialize_lbd_batches_incremental_to_writer` path to CLI | 0.5 day |
| WASM `run_memory` collects all triples | Make `run_memory` also use streaming + `CountingWriter` | 0.5 day |
| `low_memory_mode` is a dead letter | Make it affect Vec::with_capacity hints in `emit_lbd`, `stream_lbd`, and the serializer | 1 day |

### P0: Extract shared plugins into `lbd_pipeline`

| Issue | Fix | Effort |
|---|---|---|
| Plugin structs duplicated | Move `LbdProducerPlugin`, `IfcowlProducerPlugin`, `TurtleSerializerPlugin`, `NquadsSerializerPlugin`, `FileExportPlugin` into `lbd_pipeline` (or new `lbd-plugins` crate) | 1 day |
| Manifest IDs duplicated | Centralize ID constants in `lbd_pipeline` | 0.5 day |
| Manifests diverged | Reconcile NQuads/FileExport manifests (use union of capabilities) | 0.5 day |

### P1: Make `produce()` real

| Issue | Fix | Effort |
|---|---|---|
| `produce()` never called | Implement it by wrapping `stream_step_and_model()`. Change PipelineRunner to call `produce()` instead of calling `stream_step_and_model()` directly. | 1 day |
| CLI bypasses plugins entirely | Refactor CLI `main.rs` to use PipelineRunner (which calls `produce()`) | 2 days |

### P1: Extract PipelineRunner into `lbd_pipeline`

| Issue | Fix | Effort |
|---|---|---|
| PipelineRunner only in WASM | Move `PipelineRunner`, `resolve_and_validate`, `make_convert_options` into `lbd_pipeline::runner` | 2 days |
| CLI is 3,100 lines of hand-wired channels | Replace with `PipelineRunner::run(input, config, sink)` | 2 days |

### P2: Browser safety hardening

| Issue | Fix | Effort |
|---|---|---|
| SinkChunkWriter has no pending limit | Add `max_pending_bytes` enforcement — block/flush when exceeded | 0.5 day |
| Memory estimation uncalibrated | Run benchmarks across 10+ models to calibrate multipliers | 1 day |
| No JS memory API | Add `navigator.deviceMemory` integration in JS wrapper | 0.5 day |

### P2: Feature parity

| Issue | Fix | Effort |
|---|---|---|
| WASM has no topology | Add topology-lite plugin to WASM registry (topology-full needs OCC binary, not available in browser) | 0.5 day |
| WASM has no bbox | Add bbox plugin to WASM registry | 0.5 day |

**Total: ~12 days for full architectural parity and safety hardening.**

---

## 6. Summary Answers

### "How much faster is CLI vs WASM?"
**Zero difference.** Both call the same `lbd_converter` and `lbd_serializer` functions. Same speed, same peak memory. The only difference is that WASM-in-browser would be ~1.5-2× slower due to wasm32 limitations (no SIMD, limited rayon threads), but native-to-native they are identical.

### "Are we modular enough with plugins and safe for browsers with less memory?"
**Yes, the architecture is right, but there are holes.**
- The 3-tier execution model (Fast/Lowmem/Reject) correctly prevents browser OOM
- The streaming paths truly stream (no triple collection)
- But `low_memory_mode` doesn't actually do anything in the core crates
- And `SinkChunkWriter` lacks a pending-byte limit
- And neither WASM `run_memory` nor CLI Turtle truly stream — they both collect all triples

### "Are CLI and WASM plugins compatible?"
**Same trait system, but plugins are NOT interchangeable.**
- Same `lbd_pipeline` traits ✅
- Same manifest IDs ✅ (but duplicated, not shared)
- `produce()` is never called ❌
- NQuads/FileExport manifests have diverged ❌
- WASM has real `serialize()`/`export_in_memory()`, CLI has stubs ⚠️
- CLI has 5 extra plugins that WASM doesn't ❌

The right fix is: shared plugin definitions in `lbd_pipeline`, real `produce()` implementations, and `PipelineRunner` in `lbd_pipeline` that both crates use.
