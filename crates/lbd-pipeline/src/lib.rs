use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use crossbeam::channel::{Receiver, Sender};
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

/// Shared context passed to every plugin at execution time.
///
/// Carries resource limits and optional typed data that producers need
/// (e.g. StepFile, IfcModel, ConvertOptions). The typed data is stored
/// as `Arc<dyn Any + Send + Sync>` and accessed via `get::<T>()`.
#[derive(Clone)]
pub struct PipelineContext {
    pub resource_limits: ResourceLimits,
    /// Optional typed data: `Arc<StepFile>`, `Arc<IfcModel>`, `ConvertOptions`, etc.
    data: Vec<Arc<dyn std::any::Any + Send + Sync>>,
}

impl PipelineContext {
    pub fn new(limits: ResourceLimits) -> Self {
        Self {
            resource_limits: limits,
            data: Vec::new(),
        }
    }

    /// Insert a typed value into the context.
    pub fn insert<T: 'static + Send + Sync>(&mut self, value: Arc<T>) {
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
}

// ---------------------------------------------------------------------------
// Streaming batch type
// ---------------------------------------------------------------------------

/// Tag for a triple batch when multiple producers feed into the same channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum BatchKind {
    Lbd,
    Ifcowl,
    Topology,
}

// ---------------------------------------------------------------------------
// Shared plugin ID constants
// ---------------------------------------------------------------------------

pub const LBD_PRODUCER_ID: &str = "neo-lbd-producer";
pub const IFCOWL_PRODUCER_ID: &str = "neo-ifcowl-producer";
pub const IFC_TOPOLOGY_PRODUCER_ID: &str = "neo-ifc-topology-producer";
pub const TOPOLOGY_FULL_PRODUCER_ID: &str = "neo-topology-full-producer";
pub const BBOX_ENRICHER_ID: &str = "neo-bbox-enricher";
pub const TURTLE_SERIALIZER_ID: &str = "neo-turtle-serializer";
pub const NQUADS_SERIALIZER_ID: &str = "neo-nquads-serializer";
pub const NQUADS_CHUNKED_SERIALIZER_ID: &str = "neo-nquads-chunked-serializer";
pub const FILE_EXPORT_ID: &str = "neo-file-export";
pub const STDOUT_EXPORT_ID: &str = "neo-stdout-export";
pub const GRAFEO_EXPORT_ID: &str = "neo-grafeo-export";

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PipelineStage {
    Preprocess,
    Produce,
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
}

pub trait PipelinePlugin: Send + Sync {
    fn manifest(&self) -> PluginManifest;
}

pub trait PreprocessPlugin: PipelinePlugin {}

/// A producer plugin that emits triples in bounded streaming batches.
///
/// Implementations send batches through `sender`; backpressure is natural —
/// if the channel is full, `send` blocks.
pub trait ProducerPlugin: PipelinePlugin {
    /// Produce triples in bounded batches, sending them through `sender`.
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError>;
}

/// A serializer plugin that consumes triple batches from a channel and writes
/// them to a `Write` sink.
pub trait SerializerPlugin: PipelinePlugin {
    /// Serialize tagged batches from `receiver` into `writer`.
    fn serialize(
        &self,
        ctx: &PipelineContext,
        receiver: Receiver<TaggedBatch>,
        writer: &mut dyn std::io::Write,
    ) -> Result<SerializeStats, SerializerError>;
}

/// An export plugin that handles the final output of serialized bytes.
pub trait ExportPlugin: PipelinePlugin {
    /// Export in-memory byte buffers. Returns summaries of exported files.
    fn export_in_memory(
        &self,
        ctx: &PipelineContext,
        files: Vec<ExportedFile>,
    ) -> Result<Vec<ExportFileSummary>, ExportError>;
}

/// A file produced by the pipeline, ready for export.
#[derive(Clone, Debug)]
pub struct ExportedFile {
    pub filename: String,
    pub mime_type: String,
    pub role: String,
    pub bytes: Vec<u8>,
}

/// Summary of an exported file (bytes only, no payload).
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
    Serializer(Arc<dyn SerializerPlugin>),
    Export(Arc<dyn ExportPlugin>),
}

impl RegisteredPlugin {
    pub fn manifest(&self) -> PluginManifest {
        match self {
            Self::Preprocess(plugin) => plugin.manifest(),
            Self::Producer(plugin) => plugin.manifest(),
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

    fn register(&mut self, plugin: RegisteredPlugin) -> Result<(), RegistryError> {
        let manifest = plugin.manifest();
        if self.plugins.contains_key(manifest.id) {
            return Err(RegistryError::DuplicatePluginId(manifest.id));
        }
        self.plugins.insert(manifest.id, plugin);
        Ok(())
    }
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
}
