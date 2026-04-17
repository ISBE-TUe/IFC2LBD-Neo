# IFC2LBD-Neo Status (April 2026)

## Completed This Session

### P0a: CLI Turtle Streaming ✅
- Swapped `serialize_lbd_batches_to_writer` → `serialize_lbd_batches_incremental_to_writer` in CLI
- **Turtle LBD speed: 71% faster** (1.857s → 0.546s for model A)
- **Turtle LBD memory: 75% less** (367MB → 93MB for model A)
- Trade-off: output ~1.5× larger (no dedup/subject grouping), but valid RDF

### P0b: `low_memory_mode` Awareness ✅
- Confirmed `low_memory_mode` is correctly set by WASM runner and propagated to `ConvertOptions`

### P0c: Shared Plugin IDs + BatchKind::Topology ✅
- Added 10 `pub const` plugin ID strings to `lbd_pipeline/src/lib.rs`
- Added `BatchKind::Topology` variant
- All references updated across 7+ files

### P1a: Real `produce()` implementations ✅
- Extended `PipelineContext` with `insert::<T>()`/`get::<T>()` type-erased data methods
- WASM `LbdProducerPlugin::produce()` and `IfcowlProducerPlugin::produce()` now call `stream_step_and_model()`

### Turtle grouping flag ✅
- Added `TurtleGrouping` enum (`Sorted` | `Streaming`) to CLI and WASM
- CLI default: `sorted` (compact, grouped Turtle — the RDF standard)
- CLI opt-in: `--module-opt neo-turtle-serializer.grouping=streaming`
- WASM `run_memory`: `sorted`, `run_to_sink`: `streaming`

### P1b: CLI `main.rs` module extraction ✅
- Extracted `bbox.rs` (925 lines), `kernel.rs` (185 lines), `chunk_writer.rs` (452 lines)
- **main.rs: 3174 → 1669 lines** (47% reduction)
- All 31 tests pass

### P2a: SinkChunkWriter pending-byte limit + browser safety ✅
- Added `max_pending_bytes` parameter to `SinkChunkWriter::new()`
- When `pending.len() >= max_pending_bytes`, flushes immediately (regardless of `chunk_size`)
- `should_flush()` method combines chunk-size and pending-byte-limit checks
- Default: `max_pending_bytes = 4 × chunk_size` (4MB for 1MB chunks)
- Configurable via `ConversionRequest.sink_max_pending_bytes` and `sink_chunk_size_bytes`
- **Preserved JS error messages** — all `SinkChunkWriter` JS errors now include the original JS error string instead of generic `ErrorKind::Other`
- Added `SinkConfig` struct to centralize chunk/pending configuration

### P2b: WASM feature parity (topology-lite, bbox) ✅
- Added `TopologyLiteProducerPlugin` to WASM registry (manifest with `wasm_compatible: true`)
- Added `BboxEnricherPlugin` to WASM registry (manifest with `wasm_compatible: true`)
- `TopologyFullProducerPlugin` excluded (`wasm_compatible: false` — needs OCC kernel subprocess)
- `produce()` implementations return descriptive "not yet wired" errors (same pattern as CLI)
- New test: `resolve_plan_accepts_topology_lite`
- **31 tests pass** (was 30)

## In Progress

### Further decompose `fn main()` (680 lines remaining)
### Wire topology-lite and bbox `produce()` through PipelineRunner

## Recently Completed

### WASM v6: Web Worker Architecture ✅
- Moved WASM conversion from main thread to dedicated Web Worker (`wasm-lowmem-worker.js`)
- Fixes Chrome/mobile freezing and WASM trap isolation
- Removed `showSaveFilePicker` (Chrome-only) → universal Blob downloads
- Removed fast→lowmem JS retry — auto mode handles it; worker isolation means failures are clean errors
- `role` field on expected files for cleaner metadata
- Runtime build: `worker-v6-2026-04-17Z`

### WASM v7: Pipeline Dashboard UI ✅
- Full-width DAG pipeline visualization (SVG, CSS-animated)
- Left slide-out settings panel (⚙ button) — plugin toggles, file input, base URI, output stem
- Right slide-out detail panel (click DAG node) — status, timing, options, telemetry, metadata
- Bottom collapsible log panel
- Per-stage telemetry from Rust: `StageTelemetry` struct + `stageEvent` sink events
- Config save/load (JSON export/import)
- Mobile fallback (width < 900px shows simple v6 form)
- Ableton-inspired grey palette with cool pastel status colors
- TU/e logo in white, no red
- Files: `pipeline/app.js`, `dag.js`, `stage-panel.js`, `sidebar.js`, `log-panel.js`, `config.js`, `state.js`, `pipeline.css`

## CLI File Structure

| File | Lines | Purpose |
|---|---|---|
| `main.rs` | 1669 | CLI args, orchestration, validation |
| `bbox.rs` | 925 | Bbox extraction, adjacency, WKT |
| `chunk_writer.rs` | 452 | N-Quads file chunking |
| `pipeline_plugins.rs` | 500 | Plugin manifests, grafeo streaming |
| `mesh.rs` | 1058 | Triangle mesh extraction |
| `topology_plugin.rs` | 202 | Topology plugin config |
| `kernel.rs` | 185 | Geometry kernel binary resolution |
| `transform.rs` | 270 | 4×4 affine transform extraction |
| `voxel.rs` | 316 | Voxel-based adjacency detection |
| `producer_plugins.rs` | 25 | Producer plugin re-exports |

## WASM Plugin Registry

| Plugin | Stage | wasm_compatible | produce() wired |
|---|---|---|---|
| LbdProducerPlugin | Produce | ✅ | ✅ |
| IfcowlProducerPlugin | Produce | ✅ | ✅ |
| TopologyLiteProducerPlugin | Produce | ✅ | ❌ (manifest only) |
| BboxEnricherPlugin | Produce | ✅ | ❌ (manifest only) |
| TurtleSerializerPlugin | Serialize | ✅ | ✅ |
| NquadsSerializerPlugin | Serialize | ✅ | ✅ |
| FileExportPlugin | Export | ✅ | ✅ |

## Benchmark Summary (after P2)

| Test | Time | Peak RSS |
|---|---|---|
| DH LBD Turtle | 1.33s | 378MB |
| DH LBD+IfcOWL NQ | 1.56s | 535MB |
| DH LBD+IfcOWL Turtle | 2.13s | 454MB |
| WH LBD Turtle | 0.50s | 191MB |
| WH LBD+IfcOWL NQ | 0.88s | 386MB |
| WH LBD+IfcOWL Turtle | 0.90s | 339MB |
| WH LBD+IfcOWL+TopoFull NQ | 4.18s | 374MB |
