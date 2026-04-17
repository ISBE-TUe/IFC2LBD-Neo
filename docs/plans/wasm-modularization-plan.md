# WASM Crate Modularization Plan

## Problem Statement

The `ifc2lbd-wasm` crate is a 1700-line `lib.rs` with:
- **Hollow plugins** — the 5 plugin structs only provide manifests; all real logic is hardcoded in monolithic functions
- **3 copy-pasted conversion paths** (`convert_ifc_impl`, `convert_ifc_to_sink_impl`, `export_browser_file_summaries_streaming`) sharing ~80% boilerplate
- **Memory cliff** — `convertIfc` materializes all triples then OOMs on large files; `convertIfcToSink` only works on wasm32; no unified path that adapts
- **No pipeline orchestration** — the `lbd_pipeline` registry resolves modules but never drives execution; everything is hand-wired

## Goals

1. **Real plugins** — each plugin implements execution traits, not just manifests
2. **Streaming-first, everywhere** — bounded channels + backpressure from end to end; no materialization of full triple sets
3. **Memory-adaptive** — the same pipeline auto-tunes buffer sizes / concurrency based on available memory; works on 512MB browser tabs AND 64GB servers
4. **Single conversion path** — one `PipelineRunner` that works for all export modes (in-memory, streaming sink, benchmark, file)
5. **Clean module structure** — split the god file into focused submodules

---

## Phase 1: Make the Pipeline Traits Executable

### 1.1 Add execution methods to plugin traits

The current traits are declaration-only. Add streaming execution methods:

```rust
// lbd_pipeline/src/lib.rs

/// Shared context passed to every plugin at execution time.
pub struct PipelineContext {
    pub step: Arc<StepFile>,
    pub model: Arc<IfcModel>,
    pub options: Arc<ConvertOptions>,
    pub resource_limits: ResourceLimits,
}

/// Memory/concurrency budget for this run.
pub struct ResourceLimits {
    /// Approximate peak memory budget in bytes the pipeline should respect.
    pub memory_budget_bytes: u64,
    /// Number of rayon threads available.
    pub thread_count: usize,
    /// Bounded channel capacity (derived from memory budget).
    pub channel_capacity: usize,
    /// Batch size for triple production (derived from memory budget).
    pub batch_size: usize,
}

impl ResourceLimits {
    /// Derive resource limits from input size and available memory.
    pub fn auto(input_bytes: u64, available_memory_mb: Option<u64>) -> Self {
        let threads = rayon::current_num_threads().max(1);
        let available = available_memory_mb
            .unwrap_or_else(|| estimate_available_memory_mb())
            * 1024 * 1024;

        // Tight budget → small batches, few channels, fewer workers
        // Generous budget → large batches, more buffering
        let input_mb = (input_bytes / (1024 * 1024)).max(1);
        let ratio = available / (input_mb * 1024 * 1024).max(1);

        let (channel_capacity, batch_size) = if ratio < 4 {
            // Memory-constrained: minimize buffering
            (4, 256)
        } else if ratio < 16 {
            (8, 1024)
        } else {
            (16, 4096)
        };

        Self {
            memory_budget_bytes: available,
            thread_count: threads,
            channel_capacity,
            batch_size,
        }
    }
}
```

### 1.2 Extend ProducerPlugin with a streaming produce method

```rust
pub trait ProducerPlugin: PipelinePlugin {
    /// Produce triples in bounded batches, sending them through `sender`.
    /// Backpressure is natural: if the sender's channel is full, this blocks.
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<Vec<Triple>>,
    ) -> Result<(), ProducerError>;
}
```

Two implementations:

```rust
// LbdProducerPlugin
impl ProducerPlugin for LbdProducerPlugin {
    fn produce(&self, ctx: &PipelineContext, sender: &Sender<Vec<Triple>>) -> Result<(), ProducerError> {
        stream_lbd(&ctx.model, &ctx.options, &ctx.options.base_uri, |batch| {
            sender.send(batch).map_err(|_| ProducerError::SinkClosed)
        })
    }
}

// IfcowlProducerPlugin
impl ProducerPlugin for IfcowlProducerPlugin {
    fn produce(&self, ctx: &PipelineContext, sender: &Sender<Vec<Triple>>) -> Result<(), ProducerError> {
        stream_ifcowl(&ctx.step, &ctx.model, &ctx.options, sender)
    }
}
```

### 1.3 Extend SerializerPlugin with a streaming serialize method

```rust
pub trait SerializerPlugin: PipelinePlugin {
    /// Serialize triple batches from `receiver` into `writer`.
    /// Called on the consumer side of the channel pipeline.
    fn serialize(
        &self,
        ctx: &PipelineContext,
        receiver: Receiver<Vec<Triple>>,
        writer: &mut dyn Write,
    ) -> Result<SerializeStats, SerializerError>;
}

pub struct SerializeStats {
    pub bytes_written: u64,
    pub triples_written: u64,
}
```

### 1.4 Extend ExportPlugin with an export method

```rust
pub trait ExportPlugin: PipelinePlugin {
    /// Export serialized bytes. Two variants:
    /// - In-memory: collects into Vec<ExportedFile>
    /// - Streaming: calls a sink callback with chunks
    fn export_in_memory(
        &self,
        ctx: &PipelineContext,
        files: Vec<(String, String, String, Vec<u8>)>,  // (filename, mime, role, bytes)
    ) -> Result<Vec<ExportedFile>, ExportError>;

    #[cfg(target_arch = "wasm32")]
    fn export_to_sink(
        &self,
        ctx: &PipelineContext,
        files: Vec<(String, String, String, Vec<u8>)>,
        sink: &js_sys::Function,
    ) -> Result<Vec<OutputFileSummary>, ExportError>;
}
```

---

## Phase 2: Build a PipelineRunner

This is the core orchestration that replaces all three monolithic conversion paths.

### 2.1 Runner struct

```rust
// ifc2lbd-wasm/src/runner.rs

pub struct PipelineRunner {
    registry: PluginRegistry,
    limits: ResourceLimits,
}

pub struct RunConfig {
    pub module_ids: Vec<String>,
    pub module_options: Vec<String>,
    pub base_uri: String,
    pub output_stem: String,
}

pub enum OutputSink {
    /// Collect all output into memory (for convertIfc / benchmark)
    Memory,
    /// Stream chunks to a JS callback (for convertIfcToSink)
    #[cfg(target_arch = "wasm32")]
    JsSink { sink: js_sys::Function, chunk_size: usize },
    /// Write to files on disk (for CLI reuse)
    File { output_dir: PathBuf },
}

pub struct RunResult {
    pub plan: ResolvedPlan,
    pub export_metadata: ExportMetadata,
    pub output_files: Vec<OutputFileSummary>,
    pub warnings: Vec<String>,
    pub telemetry: ConversionTelemetry,
}
```

### 2.2 Runner::run — the single conversion path

```rust
impl PipelineRunner {
    pub fn run(
        &self,
        input: &[u8],
        config: &RunConfig,
        sink: OutputSink,
    ) -> Result<RunResult, WasmApiError> {
        // 1. Resolve & validate (shared, ~30 lines)
        let (plan, settings) = self.resolve_and_validate(config)?;

        // 2. Parse & model (always streaming-capable)
        let step = parse_step_bytes(input)?;
        let model = build_model(&step)?;

        // 3. Derive limits from input size + available memory
        let limits = ResourceLimits::auto(input.len() as u64, config.memory_budget_mb);
        let ctx = PipelineContext::new(step, model, &settings, &limits);

        // 4. Wire producers → channels → serializer → writer
        let writer = self.create_writer(&settings, &sink)?;
        let result = self.execute_pipeline(&ctx, &plan, &settings, writer)?;

        // 5. Export
        let output = self.export(result, &settings, &sink)?;

        Ok(output)
    }
}
```

### 2.3 execute_pipeline — unified wiring

The key insight: **every path is streaming**. The difference between "fast" and "lowmem" is just the channel capacity and batch size — not the architecture.

```rust
fn execute_pipeline(
    &self,
    ctx: &PipelineContext,
    plan: &ActivationPlan,
    settings: &ExecutionSettings,
    mut writer: Box<dyn Write>,
) -> Result<PipelineOutput, WasmApiError> {
    let active: HashSet<&str> = plan.enabled_ids.iter().map(|s| s.as_str()).collect();
    let cap = ctx.limits.channel_capacity;

    match settings.output_format {
        OutputFormat::Turtle => {
            let (lbd_sender, lbd_receiver) = crossbeam::channel::bounded(cap);

            if settings.emit_ifcowl {
                // Two producer channels → two writers → two output files
                let (ifcowl_sender, ifcowl_receiver) = crossbeam::channel::bounded(cap);

                // Spawn producers in parallel
                let ctx_lbd = ctx.clone();
                let ctx_ifc = ctx.clone();
                let lbd_producer = self.registry.plugin(LBD_PRODUCER_ID).unwrap();
                let ifcowl_producer = self.registry.plugin(IFCOWL_PRODUCER_ID).unwrap();

                rayon::scope(|s| {
                    s.spawn(|_| { lbd_producer.produce(&ctx_lbd, &lbd_sender); });
                    s.spawn(|_| { ifcowl_producer.produce(&ctx_ifc, &ifcowl_sender); });
                });

                // Serialize each stream to its own writer
                // (writer splitting happens in the export layer)
                let serializer = self.registry.plugin(TURTLE_SERIALIZER_ID).unwrap();
                let lbd_stats = serializer.serialize(&ctx, lbd_receiver, &mut lbd_writer)?;
                let ifcowl_stats = serializer.serialize(&ctx, ifcowl_receiver, &mut ifcowl_writer)?;
            } else {
                let producer = self.registry.plugin(LBD_PRODUCER_ID).unwrap();
                let ctx_c = ctx.clone();
                rayon::scope(|s| {
                    s.spawn(|_| { producer.produce(&ctx_c, &lbd_sender); });
                });
                let serializer = self.registry.plugin(TURTLE_SERIALIZER_ID).unwrap();
                let stats = serializer.serialize(&ctx, lbd_receiver, &mut writer)?;
            }
        }
        OutputFormat::Nquads => {
            // Similar but feeds into nquads serializer which merges streams
        }
    }
}
```

**Why this eliminates the fast/lowmem split:** The current code has two architectures — `convert_step_and_model` (materialize everything) vs `stream_step_and_model` (streaming). With bounded channels and `ResourceLimits`, the streaming path *is* the fast path when you have memory (large channels = more buffering = higher throughput). And when memory is tight, small channels + small batches naturally limit peak usage. One architecture, one code path, auto-tuned parameters.

---

## Phase 3: Module File Structure

Split `lib.rs` (1700 lines) into:

```
crates/ifc2lbd-wasm/src/
├── lib.rs              (~80 lines)  — public re-exports, wasm_bindgen entry points
├── api.rs              (~120 lines) — wasm_bindgen functions: listModules, resolvePlan, convertIfc, etc.
├── runner.rs           (~200 lines) — PipelineRunner: resolve → parse → produce → serialize → export
├── plugins.rs          (~150 lines) — LbdProducerPlugin, IfcowlProducerPlugin, TurtleSerializerPlugin, etc.
├── sink.rs             (~120 lines) — SinkChunkWriter + CountingWriter (trait-based)
├── validation.rs       (~150 lines) — parse_module_configs, validate_* (all the config validation)
├── types.rs            (~200 lines) — ConversionRequest, ExecutionSettings, OutputFormat, etc.
├── memory.rs           (~100 lines) — ResourceLimits, select_execution_mode, estimate_available_memory
└── tests.rs            (~100 lines) — existing tests, largely unchanged
```

**Total ~1200 lines** (vs 1700 now) — the reduction comes from eliminating duplication.

### 3.1 lib.rs — thin facade

```rust
mod api;
mod memory;
mod plugins;
mod runner;
mod sink;
mod types;
mod validation;

pub use api::*;
pub use types::*;
```

### 3.2 api.rs — wasm_bindgen surface

Only the `#[wasm_bindgen]` functions. Each one is now ~10 lines: parse args → delegate to `PipelineRunner`:

```rust
#[wasm_bindgen(js_name = convertIfc)]
pub fn convert_ifc(input: &[u8], request: JsValue) -> Result<JsValue, JsValue> {
    let request: ConversionRequest = serde_wasm_bindgen::from_value(request).map_err(js_err)?;
    let runner = PipelineRunner::new();
    let config = request.to_run_config();
    let result = runner.run(input, &config, OutputSink::Memory).map_err(js_err)?;
    serde_wasm_bindgen::to_value(&result).map_err(js_err)
}
```

### 3.3 sink.rs — trait-based writers

Abstract the output strategy behind a trait so the runner doesn't care whether it's writing to memory, a JS sink, or a file:

```rust
/// Abstraction over "where serialized bytes go".
pub trait OutputWriter: Write {
    /// Finalize and return stats for this output file.
    fn finish(self: Box<Self>) -> Result<OutputFileSummary, SerializerError>;
}

// In-memory: collects into Vec<u8>
pub struct VecWriter { /* ... */ }

// Wasm JS sink: chunks via SinkChunkWriter
#[cfg(target_arch = "wasm32")]
pub struct JsSinkWriter<'a> { /* ... */ }

// File: writes to disk (reusable from CLI)
pub struct FileWriter { /* ... */ }
```

This eliminates the 3 separate `export_browser_files*` functions. The runner just does:

```rust
let writer: Box<dyn OutputWriter> = match &sink {
    OutputSink::Memory => Box::new(VecWriter::new(filename, mime, role)),
    OutputSink::JsSink { sink, chunk_size } => Box::new(JsSinkWriter::new(sink, ...)),
    OutputSink::File { output_dir } => Box::new(FileWriter::new(output_dir.join(filename))),
};
```

### 3.4 memory.rs — adaptive resource limits

```rust
/// Estimate available memory in MB.
/// On wasm32: uses `performance.memory` via JS interop if available, else 512MB default.
/// On native: uses system info.
pub fn estimate_available_memory_mb() -> u64 {
    #[cfg(target_arch = "wasm32")]
    { wasm_available_memory_mb().unwrap_or(512) }
    #[cfg(not(target_arch = "wasm32"))]
    { sys_info::mem_info().map(|m| m.total / 1024).unwrap_or(4096) }
}

impl ResourceLimits {
    pub fn auto(input_bytes: u64, available_mb: Option<u64>) -> Self {
        let available = available_mb.unwrap_or_else(estimate_available_memory_mb);
        let input_mb = (input_bytes / (1024 * 1024)).max(1);

        // Peak estimate: STEP parse + model + triple buffers + serialized output
        // Multipliers calibrated from benchmarks (same as current select_execution_mode)
        let estimated_peak_mb = 96 + input_mb * 14;  // conservative, LBD-only Turtle

        // Memory pressure determines buffering aggressiveness
        let pressure = estimated_peak_mb as f64 / available as f64;

        let (channel_capacity, batch_size, ifcowl_workers) = if pressure < 0.25 {
            // Very comfortable — go wide
            (16, 4096, rayon::current_num_threads().max(1))
        } else if pressure < 0.5 {
            // Comfortable — moderate
            (8, 2048, (rayon::current_num_threads() / 2).max(1))
        } else if pressure < 0.8 {
            // Tight — minimize buffering
            (4, 512, 1)
        } else {
            // Very tight — survival mode
            (2, 128, 1)
        };

        Self {
            memory_budget_bytes: available * 1024 * 1024,
            thread_count: rayon::current_num_threads().max(1),
            channel_capacity,
            batch_size,
            ifcowl_workers,
        }
    }
}
```

**Key insight:** This replaces the binary `Fast`/`Lowmem` mode with a continuous spectrum. The pipeline architecture is always streaming; the only thing that changes is how much we buffer.

---

## Phase 4: Eliminate the OOM Path

The current `convertIfc` API calls `convert_step_and_model` which materializes *all* triples before serializing. This is the path that OOMs on large files.

### 4.1 Make convertIfc streaming too

Even the "in-memory" path should use streaming internally. The only difference from `convertIfcToSink` is the final writer:

| API | Producer | Channel | Serializer | Writer |
|---|---|---|---|---|
| `convertIfc` | `stream_step_and_model` | bounded | streaming | `VecWriter` (collects bytes) |
| `convertIfcToSink` | `stream_step_and_model` | bounded | streaming | `JsSinkWriter` (chunks to JS) |
| `benchmarkConvertIfc` | `stream_step_and_model` | bounded | streaming | `CountingWriter` (counts bytes) |

**Same pipeline, different writer.** Peak memory is bounded by channel capacity × batch size × number of channels — regardless of input file size.

### 4.2 Guard: fail fast if input exceeds budget

Before parsing, check:

```rust
let limits = ResourceLimits::auto(input.len() as u64, config.memory_budget_mb);
if input.len() as u64 > limits.memory_budget_bytes / 2 {
    return Err(WasmApiError::Message(format!(
        "Input file ({:.0} MB) likely exceeds available memory ({:.0} MB). \
         Use a smaller file or increase memory budget.",
        input.len() as f64 / (1024.0 * 1024.0),
        limits.memory_budget_bytes as f64 / (1024.0 * 1024.0),
    )));
}
```

This replaces the current "lowmem selected; use convertIfcToSink" error with a clear message *before* spending any time parsing.

---

## Phase 5: Clean Up the Plugin Wiring

### 5.1 Move browser_registry into plugins.rs with real execution

Currently `browser_registry()` creates hollow plugins. After Phase 1, they have real `produce`/`serialize` methods:

```rust
// plugins.rs

pub fn browser_registry() -> PluginRegistry {
    let mut r = PluginRegistry::new();
    r.register_producer(LbdProducerPlugin).unwrap();
    r.register_producer(IfcowlProducerPlugin).unwrap();
    r.register_serializer(TurtleSerializerPlugin).unwrap();
    r.register_serializer(NquadsSerializerPlugin).unwrap();
    r.register_export(FileExportPlugin).unwrap();
    r
}

// Each plugin now has real execution logic, not just a manifest.
// The manifest is still there (for discovery), but produce()/serialize() do the work.
```

### 5.2 Share the registry between CLI and WASM

Both the CLI `main.rs` and `ifc2lbd-wasm` currently build their own registries independently. After this refactor:

- `lbd-pipeline` defines the traits + `PipelineRunner`
- Each crate (`lbd-converter`, `lbd-serializer`, etc.) provides plugin implementations
- Both CLI and WASM register the same plugins and use the same `PipelineRunner`

This eliminates the ~500 lines of duplicated channel-wiring in `main.rs` too.

---

## Phase 6: Improved Error Propagation

### 6.1 Preserve JS error messages in SinkChunkWriter

Current code: every JS error → `io::ErrorKind::Other`. Loses the original message.

```rust
// Before:
self.sink.call1(&JsValue::NULL, &event)
    .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::Other.into()))?;

// After:
self.sink.call1(&JsValue::NULL, &event)
    .map_err(|js_err| {
        let msg = js_err.as_string().unwrap_or_else(|| "unknown JS error".to_string());
        lbd_serializer::SerializerError::Io(io::Error::new(io::ErrorKind::Other, msg))
    })?;
```

### 6.2 Structured error type for the wasm API

```rust
#[derive(Debug, thiserror::Error)]
enum WasmApiError {
    #[error("module activation failed: {0}")]
    Activation(#[from] ActivationError),
    #[error("STEP parse failed: {0}")]
    Step(#[from] ifc_step::StepError),
    #[error("model build failed: {0}")]
    Model(#[from] ifc_model::ModelError),
    #[error("producer {plugin_id} failed: {source}")]
    Producer { plugin_id: String, source: ProducerError },
    #[error("serializer {plugin_id} failed: {source}")]
    Serializer { plugin_id: String, source: SerializerError },
    #[error("export failed: {0}")]
    Export(String),
    #[error("insufficient memory: {0}")]
    MemoryBudget(String),
    #[error("{0}")]
    Message(String),
}
```

This gives JS callers structured, actionable error messages.

---

## Migration Strategy

The refactoring should be done incrementally, with each phase producing a working crate:

| Step | What changes | Tests pass? |
|---|---|---|
| **0** | Split `lib.rs` into modules (pure file reorganization, no logic changes) | ✅ All existing tests |
| **1** | Add `ResourceLimits` + `PipelineContext` to `lbd_pipeline` | ✅ All existing tests |
| **2** | Add `produce()` / `serialize()` methods to plugin traits (default impl = unimplemented) | ✅ All existing tests |
| **3** | Implement `produce()` on LBD + IfcOWL producer plugins (move logic from converter) | ✅ Existing + new unit tests |
| **4** | Implement `serialize()` on Turtle + Nquads serializer plugins | ✅ Existing + new unit tests |
| **5** | Build `PipelineRunner` that wires traits together, wire `convertIfc` through it | ✅ All existing tests |
| **6** | Wire `convertIfcToSink` + `benchmarkConvertIfc` through `PipelineRunner` | ✅ All existing tests |
| **7** | Delete old monolithic functions | ✅ All existing tests |
| **8** | Share `PipelineRunner` with CLI (`ifc2lbd-cli` main.rs) | ✅ CLI integration tests |

Each step is independently mergeable and testable. No big-bang rewrite.

---

## Summary of Key Design Decisions

| Decision | Rationale |
|---|---|
| **Streaming always** | Bounded channels + backpressure = guaranteed memory ceiling. No separate "fast" path. |
| **ResourceLimits as a knob, not a mode** | Continuous spectrum (channel cap 2–16, batch 128–4096) adapts to any machine. Better than binary fast/lowmem. |
| **OutputWriter trait** | One pipeline, N output strategies. Eliminates 3 copy-pasted export functions. |
| **Real plugin execution traits** | PipelineRunner orchestrates via trait methods, not hardcoded function calls. Makes it actually pluggable. |
| **Fail-fast memory guard** | Check budget before parsing. Clear error message, not a mysterious OOM halfway through. |
| **Incremental migration** | Each phase is a working crate. No "rewrite everything and hope" step. |
