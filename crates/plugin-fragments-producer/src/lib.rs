use crossbeam::channel::Sender;
use fragments_core::{convert_step_to_fragments, FragmentsConfig};
use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_pipeline::{
    DerivedFile, FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage,
    PluginManifest, ProducerError, ProducerPlugin, TaggedBatch,
};

pub const FRAGMENTS_PRODUCER_ID: &str = "fragments-producer";

pub struct FragmentsProducerPlugin;

impl PipelinePlugin for FragmentsProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: FRAGMENTS_PRODUCER_ID,
            display_name: "Fragments producer",
            stage: PipelineStage::Produce,
            description: "Converts IFC input into a ThatOpen-compatible fragments sidecar.",
            inputs: vec!["ifc-model", "ifc-step"],
            outputs: vec!["fragments-sidecar"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ProducerPlugin for FragmentsProducerPlugin {
    fn produce(
        &self,
        ctx: &PipelineContext,
        _sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        let model = ctx.get::<IfcModel>().ok_or_else(|| {
            ProducerError::Conversion(
                "FragmentsProducerPlugin: missing IfcModel in context".to_string(),
            )
        })?;
        let step = ctx.get::<StepFile>().ok_or_else(|| {
            ProducerError::Conversion(
                "FragmentsProducerPlugin: missing StepFile in context".to_string(),
            )
        })?;

        let config = FragmentsConfig::default();
        let fragments = convert_step_to_fragments(&model, &step, &config)
            .map_err(|e| ProducerError::Conversion(format!("fragments conversion failed: {e}")))?;
        let raw_len = fragments.raw.len();
        let compressed_len = fragments.compressed.len();

        if let Some(tx) = &ctx.sidecar_tx {
            let _ = tx.send(DerivedFile {
                filename: "model.frag".to_string(),
                mime_type: "application/octet-stream",
                bytes: fragments.compressed,
            });
        }

        ctx.write_log(
            FRAGMENTS_PRODUCER_ID,
            serde_json::json!({
                "raw_bytes": raw_len,
                "compressed_bytes": compressed_len,
                "compression": "zlib",
            }),
        );

        Ok(())
    }
}
