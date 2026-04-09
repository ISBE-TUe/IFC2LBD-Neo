use std::collections::HashMap;
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: &'static str,
    pub display_name: &'static str,
    pub stage: PipelineStage,
    pub description: &'static str,
    pub inputs: Vec<&'static str>,
    pub outputs: Vec<&'static str>,
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
}
