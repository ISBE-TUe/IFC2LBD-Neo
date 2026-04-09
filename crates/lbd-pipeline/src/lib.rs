use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

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
pub trait ProducerPlugin: PipelinePlugin {}
pub trait SerializerPlugin: PipelinePlugin {}
pub trait ExportPlugin: PipelinePlugin {}

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
        let mut manifests: Vec<_> = self.plugins.values().map(RegisteredPlugin::manifest).collect();
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

    pub fn resolve_activation(&self, requested: &[String]) -> Result<ActivationPlan, ActivationError> {
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

    impl ProducerPlugin for TestProducer {}

    #[test]
    fn registry_rejects_duplicate_ids() {
        let mut registry = PluginRegistry::new();
        registry.register_producer(TestProducer).unwrap();
        let err = registry.register_producer(TestProducer).unwrap_err();
        assert!(matches!(err, RegistryError::DuplicatePluginId("test-producer")));
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
    impl ProducerPlugin for DepProducer {}

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
    impl ProducerPlugin for NeedsDepProducer {}

    #[test]
    fn resolve_activation_adds_dependencies() {
        let mut registry = PluginRegistry::new();
        registry.register_producer(DepProducer).unwrap();
        registry.register_producer(NeedsDepProducer).unwrap();
        let plan = registry
            .resolve_activation(&["needs-dep".to_string()])
            .unwrap();
        assert_eq!(plan.enabled_ids, vec!["dep".to_string(), "needs-dep".to_string()]);
    }
}
