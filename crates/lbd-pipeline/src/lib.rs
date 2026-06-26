use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

use crossbeam::channel::Sender;
use lbd_ontology::Triple;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Resource limits & pipeline context
// ---------------------------------------------------------------------------

/// Memory/concurrency budget for a pipeline run.
///
/// Derived from input size and available memory; controls channel capacity,
/// batch sizes, and parallelism throughout the pipeline.
#[derive(Clone, Debug)]
pub struct ResourceLimits {
    /// Approximate peak memory budget in bytes.
    pub memory_budget_bytes: u64,
    /// Number of worker threads available.
    pub thread_count: usize,
    /// Bounded channel capacity (derived from memory budget).
    pub channel_capacity: usize,
    /// Batch size for triple production (derived from memory budget).
    pub batch_size: usize,
}

impl ResourceLimits {
    /// Derive resource limits from input size and optionally available memory.
    ///
    /// The returned limits tune the pipeline from "survival mode" (tiny channels,
    /// small batches) on memory-constrained devices to "full throughput" (wide
    /// channels, large batches) on servers.
    pub fn auto(input_bytes: u64, available_memory_mb: Option<u64>) -> Self {
        let threads = rayon::current_num_threads().max(1);
        let available = available_memory_mb.unwrap_or(4096) * 1024 * 1024;
        let input_mb = (input_bytes / (1024 * 1024)).max(1);
        let ratio = available / (input_mb * 1024 * 1024).max(1);

        let (channel_capacity, batch_size) = if ratio < 4 {
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

impl Default for ResourceLimits {
    fn default() -> Self {
        Self::auto(0, None)
    }
}

/// A sidecar file emitted by a producer plugin alongside its triple stream.
///
/// Producers that generate non-triple binary artefacts (e.g. geometry `.frag`
/// files) send them through `ctx.sidecar_tx`. The pipeline orchestrator drains
/// this channel after all producers finish and routes the files to the active
/// export plugin's session via `ExportSession::accept_derived_file()`.
#[derive(Clone, Debug)]
pub struct DerivedFile {
    /// Suggested filename for the artefact (e.g. `"model.frag"`).
    pub filename: String,
    /// MIME type string (e.g. `"application/octet-stream"`).
    pub mime_type: &'static str,
    /// Raw bytes of the artefact.
    pub bytes: Vec<u8>,
}

/// Shared context passed to every plugin at execution time.
///
/// Carries resource limits, optional typed data that producers need
/// (e.g. `StepFile`, `IfcModel`, `ConvertOptions`), and an optional sidecar
/// channel for producers that emit non-triple binary artefacts.
///
/// Typed data is stored as `Arc<dyn Any + Send + Sync>` and accessed via
/// `get::<T>()` or replaced in-place via `replace::<T>()` (for preprocessors).
///
/// Log bundle uses interior mutability (`Mutex`) so that both preprocessors
/// (`&mut PipelineContext`) and producers (`&PipelineContext`) can write stats.
#[derive(Clone)]
pub struct PipelineContext {
    pub resource_limits: ResourceLimits,
    /// Optional typed data: `Arc<StepFile>`, `Arc<IfcModel>`, `ConvertOptions`, etc.
    data: Vec<Arc<dyn std::any::Any + Send + Sync>>,
    /// If set, producers may send sidecar artefacts (geometry files, etc.) here.
    /// The orchestrator drains this after all producers finish.
    pub sidecar_tx: Option<Sender<DerivedFile>>,
    /// Per-module stats written by both preprocessors and producers.
    /// Uses `Arc<Mutex<>>` so producers (which only get `&PipelineContext`) can write.
    log_bundle: Arc<Mutex<PipelineLogBundle>>,
}

impl PipelineContext {
    pub fn new(limits: ResourceLimits) -> Self {
        Self {
            resource_limits: limits,
            data: Vec::new(),
            sidecar_tx: None,
            log_bundle: Arc::new(Mutex::new(PipelineLogBundle::default())),
        }
    }

    /// Insert a typed value into the context.
    pub fn insert<T: 'static + Send + Sync>(&mut self, value: Arc<T>) {
        self.data
            .push(value as Arc<dyn std::any::Any + Send + Sync>);
    }

    /// Replace any existing value of type `T` with `value`.
    ///
    /// Used by preprocessors to update context data (e.g. a modified `IfcModel`)
    /// before the produce stage runs. If no existing `T` is found, this behaves
    /// identically to `insert`.
    pub fn replace<T: 'static + Send + Sync>(&mut self, value: Arc<T>) {
        self.data.retain(|item| item.downcast_ref::<T>().is_none());
        self.data
            .push(value as Arc<dyn std::any::Any + Send + Sync>);
    }

    /// Retrieve a typed value from the context.
    ///
    /// Returns the first `Arc<T>` found, or `None` if not present.
    pub fn get<T: 'static + Send + Sync>(&self) -> Option<Arc<T>> {
        for item in &self.data {
            if let Ok(downcast) = item.clone().downcast::<T>() {
                return Some(downcast);
            }
        }
        None
    }

    /// Write per-module stats to the log bundle. Works from both `&mut self`
    /// (preprocessors) and `&self` (producers) because the bundle uses a `Mutex`.
    pub fn write_log(&self, module_id: &str, stats: serde_json::Value) {
        if let Ok(mut guard) = self.log_bundle.lock() {
            guard.write_module(module_id, stats);
        }
    }

    /// Snapshot of the accumulated log bundle. Called by log exporters after
    /// all stages complete.
    pub fn read_log_bundle(&self) -> PipelineLogBundle {
        self.log_bundle
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Streaming batch type
// ---------------------------------------------------------------------------

/// Tag for a triple batch carrying the named-graph IRI the triples belong to.
///
/// Each producer sets this to `"{base_uri}/{slug}"` so serializers can route
/// batches to the correct named graph without a central enum.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct BatchKind(pub String);

impl BatchKind {
    pub fn new(iri: impl Into<String>) -> Self {
        Self(iri.into())
    }

    pub fn iri(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Shared plugin ID constants
// ---------------------------------------------------------------------------

// LBD sub-module producers (replace the old monolithic neo-lbd-producer)
pub const BOT_PRODUCER_ID: &str = "neo-bot-producer";
pub const BEO_PRODUCER_ID: &str = "neo-beo-producer";
pub const PROPS_OPM_PRODUCER_ID: &str = "neo-props-opm";
pub const BSDD_PRODUCER_ID: &str = "neo-bsdd-producer";
pub const OMG_FOG_PRODUCER_ID: &str = "neo-omg-fog";
pub const IFCOWL_PRODUCER_ID: &str = "neo-ifcowl-producer";
pub const TURTLE_SERIALIZER_ID: &str = "neo-turtle-serializer";
pub const NQUADS_SERIALIZER_ID: &str = "neo-nquads-serializer";
pub const NQUADS_CHUNKED_SERIALIZER_ID: &str = "neo-nquads-chunked-serializer";
pub const FILE_EXPORT_ID: &str = "neo-file-export";
pub const LOG_EXPORT_ID: &str = "neo-log-export";
pub const STDOUT_EXPORT_ID: &str = "neo-stdout-export";
pub const CLEANUP_PREPROCESS_ID: &str = "neo-cleanup-preprocess";
pub const BSDD_MATCH_PREPROCESS_ID: &str = "neo-bsdd-match-preprocess";
pub const QTO_PREPROCESS_ID: &str = "neo-qto-preprocess";
pub const RML_MAPPER_ID: &str = "neo-rml-mapper";

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct PipelineLogBundle {
    pub modules: HashMap<String, serde_json::Value>,
}

impl PipelineLogBundle {
    pub fn write_module(&mut self, module_id: &str, stats: serde_json::Value) {
        self.modules.insert(module_id.to_string(), stats);
    }
}

/// A tagged batch of triples produced by a producer plugin.
#[derive(Clone, Debug)]
pub struct TaggedBatch {
    pub kind: BatchKind,
    pub triples: Vec<Triple>,
}

// ---------------------------------------------------------------------------
// Plugin errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ProducerError {
    #[error("channel closed")]
    ChannelClosed,
    #[error("conversion failed: {0}")]
    Conversion(String),
}

/// Error from a preprocess plugin.
#[derive(Debug, thiserror::Error)]
pub enum PreprocessError {
    #[error("preprocessing failed: {0}")]
    Preprocessing(String),
}

/// Error from a postprocess plugin.
#[derive(Debug, thiserror::Error)]
pub enum PostprocessError {
    #[error("postprocessing failed: {0}")]
    Postprocessing(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SerializerError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("channel closed")]
    ChannelClosed,
    #[error("serialization failed: {0}")]
    Serialization(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("export failed: {0}")]
    Export(String),
}

/// Statistics returned by a serializer after consuming all batches.
#[derive(Clone, Debug, Default)]
pub struct SerializeStats {
    pub bytes_written: u64,
    pub triples_written: u64,
}

/// Pipeline stages in execution order.
///
/// The numeric order reflects the actual execution sequence:
/// Import → Preprocess → Produce → Postprocess → Serialize → Export
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PipelineStage {
    Import,
    Preprocess,
    Produce,
    /// Runs after all producers finish, before serialization.
    /// Receives the full collected triple set when `needs_full_graph` is true.
    Postprocess,
    Serialize,
    Export,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ParallelismMode {
    Serial,
    ParallelByEntity,
    ParallelByBatch,
    ParallelByPartition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum FailurePolicy {
    Required,
    Optional,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: &'static str,
    pub display_name: &'static str,
    pub stage: PipelineStage,
    pub description: &'static str,
    pub inputs: Vec<&'static str>,
    pub outputs: Vec<&'static str>,
    pub requires: Vec<&'static str>,
    pub conflicts_with: Vec<&'static str>,
    pub failure_policy: FailurePolicy,
    pub parallelism: ParallelismMode,
    pub wasm_compatible: bool,
    /// URL slug appended to `base_uri` to form this module's named-graph IRI.
    /// `None` for non-producer modules (serializers, exporters, etc.).
    #[serde(default)]
    pub named_graph_slug: Option<&'static str>,
    /// Whether a postprocess plugin requires the full accumulated triple graph
    /// before it can run. When `true`, the orchestrator buffers all `TaggedBatch`
    /// items from every producer before calling `postprocess()`. When `false`,
    /// only the batches produced so far are passed (streaming-friendly).
    ///
    /// Ignored for non-postprocess plugins.
    #[serde(default)]
    pub needs_full_graph: bool,
}

pub trait PipelinePlugin: Send + Sync {
    fn manifest(&self) -> PluginManifest;
}

/// A preprocess plugin that transforms the pipeline context before production.
///
/// Typical uses: compute missing quantity sets, validate the IFC model,
/// augment geometry data. The plugin reads typed data from `ctx` (e.g.
/// `Arc<IfcModel>`), produces a modified version, and writes it back via
/// `ctx.replace::<IfcModel>(Arc::new(modified_model))`.
pub trait PreprocessPlugin: PipelinePlugin {
    /// Apply in-place transformations to the pipeline context.
    ///
    /// Called sequentially for each active preprocess plugin before producers run.
    fn preprocess(&self, ctx: &mut PipelineContext) -> Result<(), PreprocessError>;
}

/// A producer plugin that emits triples in bounded streaming batches.
///
/// Implementations send batches through `sender`; backpressure is natural —
/// if the channel is full, `send` blocks.
///
/// Producers that also generate non-triple sidecar artefacts (e.g. geometry
/// `.frag` files) may send them via `ctx.sidecar_tx` at any point during
/// `produce()`.
pub trait ProducerPlugin: PipelinePlugin {
    /// Produce triples in bounded batches, sending them through `sender`.
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError>;
}

/// A postprocess plugin that inspects or modifies the accumulated triple set
/// before serialization.
///
/// Typical uses: SHACL validation, OWL inference, cross-producer triple
/// insertion (e.g. linking geometry artefacts to graph nodes). When
/// `needs_full_graph` is `true` in the manifest, the orchestrator buffers all
/// produced batches before calling this plugin.
pub trait PostprocessPlugin: PipelinePlugin {
    /// Transform or validate the accumulated triple batches.
    ///
    /// `batches` contains every `TaggedBatch` emitted by all producers.
    /// Implementations may add, remove, or rewrite triples in place.
    fn postprocess(
        &self,
        ctx: &PipelineContext,
        batches: &mut Vec<TaggedBatch>,
    ) -> Result<(), PostprocessError>;
}

/// A serializer plugin registered for manifest/conflict resolution.
///
/// Serializer implementations live in the pipeline runners (CLI `main.rs` and
/// WASM `runner.rs`), not in the trait. This trait exists so serializers can be
/// discovered, conflict-checked, and plan-resolved through the same plugin
/// registry as all other stages.
pub trait SerializerPlugin: PipelinePlugin {}

/// A live export session opened for one conversion run.
///
/// The orchestrator:
/// 1. Calls `start_session()` on the active `ExportPlugin` before serialization.
/// 2. Calls `open_sink()` for each named output stream (the serializer writes to it).
/// 3. Calls `accept_derived_file()` for every sidecar file emitted by producers.
/// 4. Calls `finalize()` after all sinks are flushed and all sidecars delivered.
pub trait ExportSession: Send {
    /// Open a named output sink (file path, stdout, TCP stream, memory buffer, etc.).
    ///
    /// `filename` is the suggested file name (e.g. `"out.ttl"` or `"model.nq"`).
    /// The returned `Write` is used by the serializer; the caller flushes and
    /// drops it before calling `finalize()`.
    fn open_sink(
        &mut self,
        filename: &str,
        mime_type: &str,
        role: &str,
    ) -> Result<Box<dyn std::io::Write + Send>, ExportError>;

    /// Accept a sidecar artefact emitted by a producer plugin.
    ///
    /// May be called zero or more times before `finalize()`.
    fn accept_derived_file(&mut self, file: DerivedFile) -> Result<(), ExportError>;

    /// Finalise the export. Called once after all sinks are flushed and all
    /// derived files accepted. Returns summaries of every exported artefact.
    fn finalize(self: Box<Self>) -> Result<Vec<ExportFileSummary>, ExportError>;
}

/// An export plugin that delivers serialized output to its final destination.
///
/// Implementations differ by target:
/// - `neo-file-export` — write files to the local file system (CLI) or
///   buffer in memory for browser download (WASM).
/// - `neo-stdout-export` — write to stdout.
/// - Custom — upload to blob storage, POST to a REST API, etc.
pub trait ExportPlugin: PipelinePlugin {
    /// Create a fresh export session for one conversion run.
    ///
    /// Called by the orchestrator before serialization begins. The session
    /// owns all mutable state for the current conversion.
    fn start_session(&self, ctx: &PipelineContext) -> Result<Box<dyn ExportSession>, ExportError>;
}

/// A file produced by the pipeline, ready for export (WASM in-memory path).
#[derive(Clone, Debug)]
pub struct ExportedFile {
    pub filename: String,
    pub mime_type: String,
    pub role: String,
    pub bytes: Vec<u8>,
}

/// Summary of an exported file (metadata only, no payload).
#[derive(Clone, Debug)]
pub struct ExportFileSummary {
    pub filename: String,
    pub mime_type: String,
    pub role: String,
    pub bytes: u64,
}

#[derive(Clone)]
pub enum RegisteredPlugin {
    Preprocess(Arc<dyn PreprocessPlugin>),
    Producer(Arc<dyn ProducerPlugin>),
    Postprocess(Arc<dyn PostprocessPlugin>),
    Serializer(Arc<dyn SerializerPlugin>),
    Export(Arc<dyn ExportPlugin>),
}

impl RegisteredPlugin {
    pub fn manifest(&self) -> PluginManifest {
        match self {
            Self::Preprocess(plugin) => plugin.manifest(),
            Self::Producer(plugin) => plugin.manifest(),
            Self::Postprocess(plugin) => plugin.manifest(),
            Self::Serializer(plugin) => plugin.manifest(),
            Self::Export(plugin) => plugin.manifest(),
        }
    }

    pub fn stage(&self) -> PipelineStage {
        self.manifest().stage
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("plugin id `{0}` is already registered")]
    DuplicatePluginId(&'static str),
}

#[derive(Debug, thiserror::Error, Clone, Eq, PartialEq)]
pub enum ActivationError {
    #[error("unknown plugin id `{0}`")]
    UnknownPlugin(String),
    #[error("plugin `{plugin}` requires missing plugin `{missing}`")]
    MissingDependency { plugin: String, missing: String },
    #[error("plugin conflict between `{left}` and `{right}`")]
    Conflict { left: String, right: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationPlan {
    pub enabled_ids: Vec<String>,
}

#[derive(Default, Clone)]
pub struct PluginRegistry {
    plugins: HashMap<&'static str, RegisteredPlugin>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_preprocess<P>(&mut self, plugin: P) -> Result<(), RegistryError>
    where
        P: PreprocessPlugin + 'static,
    {
        self.register(RegisteredPlugin::Preprocess(Arc::new(plugin)))
    }

    pub fn register_producer<P>(&mut self, plugin: P) -> Result<(), RegistryError>
    where
        P: ProducerPlugin + 'static,
    {
        self.register(RegisteredPlugin::Producer(Arc::new(plugin)))
    }

    pub fn register_postprocess<P>(&mut self, plugin: P) -> Result<(), RegistryError>
    where
        P: PostprocessPlugin + 'static,
    {
        self.register(RegisteredPlugin::Postprocess(Arc::new(plugin)))
    }

    pub fn register_serializer<P>(&mut self, plugin: P) -> Result<(), RegistryError>
    where
        P: SerializerPlugin + 'static,
    {
        self.register(RegisteredPlugin::Serializer(Arc::new(plugin)))
    }

    pub fn register_export<P>(&mut self, plugin: P) -> Result<(), RegistryError>
    where
        P: ExportPlugin + 'static,
    {
        self.register(RegisteredPlugin::Export(Arc::new(plugin)))
    }

    pub fn manifests(&self) -> Vec<PluginManifest> {
        let mut manifests: Vec<_> = self
            .plugins
            .values()
            .map(RegisteredPlugin::manifest)
            .collect();
        manifests.sort_by_key(|manifest| (manifest.stage as u8, manifest.id));
        manifests
    }

    pub fn manifests_for_stage(&self, stage: PipelineStage) -> Vec<PluginManifest> {
        self.manifests()
            .into_iter()
            .filter(|manifest| manifest.stage == stage)
            .collect()
    }

    pub fn plugin(&self, id: &str) -> Option<&RegisteredPlugin> {
        self.plugins.get(id)
    }

    /// Look up a registered producer plugin by module ID.
    pub fn producer(&self, id: &str) -> Option<Arc<dyn ProducerPlugin>> {
        match self.plugins.get(id)? {
            RegisteredPlugin::Producer(p) => Some(p.clone()),
            _ => None,
        }
    }

    /// Look up a registered postprocess plugin by module ID.
    pub fn postprocessor(&self, id: &str) -> Option<Arc<dyn PostprocessPlugin>> {
        match self.plugins.get(id)? {
            RegisteredPlugin::Postprocess(p) => Some(p.clone()),
            _ => None,
        }
    }

    /// Look up a registered export plugin by module ID.
    pub fn exporter(&self, id: &str) -> Option<Arc<dyn ExportPlugin>> {
        match self.plugins.get(id)? {
            RegisteredPlugin::Export(p) => Some(p.clone()),
            _ => None,
        }
    }

    pub fn resolve_activation(
        &self,
        requested: &[String],
    ) -> Result<ActivationPlan, ActivationError> {
        let mut ordered_enabled: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: Vec<String> = requested.to_vec();

        while let Some(id) = queue.pop() {
            if seen.contains(&id) {
                continue;
            }
            let plugin = self
                .plugin(&id)
                .ok_or_else(|| ActivationError::UnknownPlugin(id.clone()))?;
            let manifest = plugin.manifest();
            seen.insert(id.clone());
            ordered_enabled.push(id.clone());
            for dependency in manifest.requires.iter().rev() {
                if !seen.contains(*dependency) {
                    queue.push((*dependency).to_string());
                }
            }
        }

        for id in &ordered_enabled {
            let plugin = self
                .plugin(id)
                .ok_or_else(|| ActivationError::UnknownPlugin(id.clone()))?;
            let manifest = plugin.manifest();
            for required in manifest.requires {
                if !seen.contains(required) {
                    return Err(ActivationError::MissingDependency {
                        plugin: id.clone(),
                        missing: required.to_string(),
                    });
                }
            }
        }

        for left in &ordered_enabled {
            let left_manifest = self
                .plugin(left)
                .ok_or_else(|| ActivationError::UnknownPlugin(left.clone()))?
                .manifest();
            for right in &ordered_enabled {
                if left == right {
                    continue;
                }
                let right_manifest = self
                    .plugin(right)
                    .ok_or_else(|| ActivationError::UnknownPlugin(right.clone()))?
                    .manifest();
                if left_manifest.conflicts_with.contains(&right_manifest.id)
                    || right_manifest.conflicts_with.contains(&left_manifest.id)
                {
                    return Err(ActivationError::Conflict {
                        left: left.clone(),
                        right: right.clone(),
                    });
                }
            }
        }

        ordered_enabled.sort();
        ordered_enabled.dedup();
        Ok(ActivationPlan {
            enabled_ids: ordered_enabled,
        })
    }

    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Find the single active export plugin in the activation plan.
    /// Returns an error if zero or multiple export plugins are active —
    /// the activation resolver enforces mutual exclusion via `conflicts_with`,
    /// so this should always yield exactly one.
    pub fn resolve_active_export(
        &self,
        active_ids: &[String],
    ) -> Result<Arc<dyn ExportPlugin>, String> {
        let mut candidates: Vec<Arc<dyn ExportPlugin>> = active_ids
            .iter()
            .filter_map(|id| self.exporter(id))
            .collect();
        match candidates.len() {
            0 => Err("no active export plugin in activation plan".to_string()),
            1 => Ok(candidates.remove(0)),
            n => Err(format!(
                "expected exactly one active export plugin, found {n}"
            )),
        }
    }

    fn register(&mut self, plugin: RegisteredPlugin) -> Result<(), RegistryError> {
        let manifest = plugin.manifest();
        if self.plugins.contains_key(manifest.id) {
            return Err(RegistryError::DuplicatePluginId(manifest.id));
        }
        self.plugins.insert(manifest.id, plugin);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// spawn_preprocessors — run preprocess plugins sequentially
// ---------------------------------------------------------------------------

/// Run all active preprocess plugins sequentially before production begins.
///
/// Returns the first error encountered, leaving the context in the state it
/// was in at the point of failure.
pub fn spawn_preprocessors(
    active_ids: &[String],
    registry: &PluginRegistry,
    ctx: &mut PipelineContext,
) -> Result<(), PreprocessError> {
    spawn_preprocessors_with(active_ids, registry, ctx, |_| {}, |_| {})
}

/// Same as `spawn_preprocessors`, but invokes `before(id)` immediately before
/// each plugin runs and `after(id)` immediately after it succeeds. The caller
/// is responsible for measuring time with whatever clock fits the target
/// (e.g. `std::time::Instant` on native, `js_sys::Date::now()` on WASM).
pub fn spawn_preprocessors_with<B, A>(
    active_ids: &[String],
    registry: &PluginRegistry,
    ctx: &mut PipelineContext,
    mut before: B,
    mut after: A,
) -> Result<(), PreprocessError>
where
    B: FnMut(&str),
    A: FnMut(&str),
{
    for id in active_ids {
        let plugin = match registry.plugin(id) {
            Some(RegisteredPlugin::Preprocess(p)) => p.clone(),
            _ => continue,
        };
        before(id);
        plugin.preprocess(ctx)?;
        after(id);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// spawn_producers — generic helper for running producer plugins in parallel
// ---------------------------------------------------------------------------

type ProducerQueue = Arc<
    Mutex<
        VecDeque<(
            Arc<dyn ProducerPlugin>,
            crossbeam::channel::Sender<TaggedBatch>,
        )>,
    >,
>;

/// Pops the next producer from the shared queue and spawns it as a rayon task.
/// When that producer finishes it calls itself recursively, forming a chain that
/// drains the queue at most one producer at a time per "slot".
fn start_next_producer(queue: ProducerQueue, ctx: Arc<PipelineContext>) {
    let item = queue.lock().unwrap().pop_front();
    if let Some((plugin, tx)) = item {
        rayon::spawn(move || {
            let _ = plugin.produce(&ctx, &tx);
            drop(tx); // signal receiver that this producer is done
            start_next_producer(queue, ctx);
        });
    }
}

/// Spawn producer plugins and return `(id, receiver)` pairs for the caller to drain.
///
/// Concurrency is bounded to `max(1, (thread_count - 1) / 2)` simultaneous
/// producers, where `thread_count = rayon::current_num_threads()`. Each
/// producer occupies ~2 rayon threads (one for its `rayon::spawn` task and
/// one for the `forward_as_tagged` hop inside `produce()`). Keeping at least
/// one thread free prevents the pool from being fully occupied, which would
/// starve the drain-side consumer and cause a deadlock.
///
/// Scaling examples:
///   4 threads  →  1 concurrent producer  (typical WASM)
///   8 threads  →  3 concurrent producers
///  16 threads  →  7 concurrent producers
///
/// With many modules the remaining producers queue up and start as slots free,
/// so throughput scales automatically without ever exhausting the pool.
///
/// # Parameters
/// * `active_ids` — module IDs to execute (only producer IDs are acted on).
/// * `registry`   — used to resolve each ID to a `ProducerPlugin`.
/// * `ctx`        — shared pipeline context passed to every `produce()` call.
/// * `chan_cap`   — bounded channel capacity for backpressure.
pub fn spawn_producers(
    active_ids: &[String],
    registry: &PluginRegistry,
    ctx: &Arc<PipelineContext>,
    chan_cap: usize,
) -> Vec<(String, crossbeam::channel::Receiver<TaggedBatch>)> {
    let thread_count = rayon::current_num_threads().max(2);
    let max_concurrent = ((thread_count - 1) / 2).max(1);

    let mut queue: VecDeque<(
        Arc<dyn ProducerPlugin>,
        crossbeam::channel::Sender<TaggedBatch>,
    )> = VecDeque::new();
    let mut receivers = Vec::new();

    for id in active_ids {
        let plugin = match registry.producer(id) {
            Some(p) => p,
            None => continue,
        };
        let (tx, rx) = crossbeam::channel::bounded::<TaggedBatch>(chan_cap);
        receivers.push((id.clone(), rx));
        queue.push_back((plugin, tx));
    }

    let shared_queue = Arc::new(Mutex::new(queue));
    for _ in 0..max_concurrent {
        start_next_producer(Arc::clone(&shared_queue), Arc::clone(ctx));
    }

    receivers
}

// ---------------------------------------------------------------------------
// spawn_postprocessors — run postprocess plugins sequentially
// ---------------------------------------------------------------------------

/// Run all active postprocess plugins sequentially after production finishes.
///
/// `batches` must contain the full collected set of `TaggedBatch` items from
/// all producers. Each plugin may mutate the batch list in place.
pub fn spawn_postprocessors(
    active_ids: &[String],
    registry: &PluginRegistry,
    ctx: &PipelineContext,
    batches: &mut Vec<TaggedBatch>,
) -> Result<(), PostprocessError> {
    for id in active_ids {
        let plugin = match registry.plugin(id) {
            Some(RegisteredPlugin::Postprocess(p)) => p.clone(),
            _ => continue,
        };
        plugin.postprocess(ctx, batches)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestProducer;

    impl PipelinePlugin for TestProducer {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "test-producer",
                display_name: "Test Producer",
                stage: PipelineStage::Produce,
                description: "emits test records",
                inputs: vec!["ifc-model"],
                outputs: vec!["triples"],
                requires: vec![],
                conflicts_with: vec![],
                failure_policy: FailurePolicy::Required,
                parallelism: ParallelismMode::ParallelByBatch,
                wasm_compatible: true,
                named_graph_slug: Some("test"),
                needs_full_graph: false,
            }
        }
    }

    impl ProducerPlugin for TestProducer {
        fn produce(
            &self,
            _ctx: &PipelineContext,
            _sender: &Sender<TaggedBatch>,
        ) -> Result<(), ProducerError> {
            Ok(())
        }
    }

    #[test]
    fn registry_rejects_duplicate_ids() {
        let mut registry = PluginRegistry::new();
        registry.register_producer(TestProducer).unwrap();
        let err = registry.register_producer(TestProducer).unwrap_err();
        assert!(matches!(
            err,
            RegistryError::DuplicatePluginId("test-producer")
        ));
    }

    #[test]
    fn registry_filters_stage_manifests() {
        let mut registry = PluginRegistry::new();
        registry.register_producer(TestProducer).unwrap();
        let manifests = registry.manifests_for_stage(PipelineStage::Produce);
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].id, "test-producer");
    }

    struct DepProducer;
    impl PipelinePlugin for DepProducer {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "dep",
                display_name: "Dependency",
                stage: PipelineStage::Produce,
                description: "dependency producer",
                inputs: vec![],
                outputs: vec![],
                requires: vec![],
                conflicts_with: vec![],
                failure_policy: FailurePolicy::Required,
                parallelism: ParallelismMode::Serial,
                wasm_compatible: true,
                named_graph_slug: None,
                needs_full_graph: false,
            }
        }
    }
    impl ProducerPlugin for DepProducer {
        fn produce(
            &self,
            _ctx: &PipelineContext,
            _sender: &Sender<TaggedBatch>,
        ) -> Result<(), ProducerError> {
            Ok(())
        }
    }

    struct NeedsDepProducer;
    impl PipelinePlugin for NeedsDepProducer {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "needs-dep",
                display_name: "Needs dep",
                stage: PipelineStage::Produce,
                description: "requires dep",
                inputs: vec![],
                outputs: vec![],
                requires: vec!["dep"],
                conflicts_with: vec![],
                failure_policy: FailurePolicy::Required,
                parallelism: ParallelismMode::Serial,
                wasm_compatible: true,
                named_graph_slug: None,
                needs_full_graph: false,
            }
        }
    }
    impl ProducerPlugin for NeedsDepProducer {
        fn produce(
            &self,
            _ctx: &PipelineContext,
            _sender: &Sender<TaggedBatch>,
        ) -> Result<(), ProducerError> {
            Ok(())
        }
    }

    #[test]
    fn resolve_activation_adds_dependencies() {
        let mut registry = PluginRegistry::new();
        registry.register_producer(DepProducer).unwrap();
        registry.register_producer(NeedsDepProducer).unwrap();
        let plan = registry
            .resolve_activation(&["needs-dep".to_string()])
            .unwrap();
        assert_eq!(
            plan.enabled_ids,
            vec!["dep".to_string(), "needs-dep".to_string()]
        );
    }

    #[test]
    fn pipeline_context_replace_updates_existing() {
        let mut ctx = PipelineContext::new(ResourceLimits::default());
        ctx.insert(Arc::new(42u32));
        ctx.replace(Arc::new(99u32));
        assert_eq!(*ctx.get::<u32>().unwrap(), 99);
    }

    #[test]
    fn pipeline_stage_ordering_correct() {
        assert!((PipelineStage::Produce as u8) < (PipelineStage::Postprocess as u8));
        assert!((PipelineStage::Postprocess as u8) < (PipelineStage::Serialize as u8));
        assert!((PipelineStage::Serialize as u8) < (PipelineStage::Export as u8));
    }
}
