use lbd_pipeline::ActivationError;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum WasmApiError {
    #[error("{0}")]
    Message(String),
    #[error("module activation failed: {0}")]
    Activation(#[from] ActivationError),
    #[error("STEP parse failed: {0}")]
    Step(#[from] ifc_step::StepError),
    #[error("IFC model build failed: {0}")]
    Model(#[from] ifc_model::ModelError),
    #[error("serialization failed: {0}")]
    Serialization(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionRequest {
    pub module_ids: Vec<String>,
    #[serde(default)]
    pub module_options: Vec<String>,
    #[serde(default)]
    pub base_uri: Option<String>,
    #[serde(default)]
    pub output_stem: Option<String>,
    #[serde(default)]
    pub execution_mode: Option<String>,
    #[serde(default)]
    pub memory_feasibility_mb: Option<u64>,
    #[serde(default)]
    pub stream_batch_size: Option<usize>,
    #[serde(default)]
    pub ifcowl_max_workers: Option<usize>,
    #[serde(default)]
    pub sink_chunk_size_bytes: Option<usize>,
    #[serde(default)]
    pub sink_max_pending_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleManifestView {
    pub id: String,
    pub display_name: String,
    pub stage: String,
    pub description: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub requires: Vec<String>,
    pub conflicts_with: Vec<String>,
    pub failure_policy: String,
    pub parallelism: String,
    pub wasm_compatible: bool,
    pub option_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPlan {
    pub enabled_ids: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedFile {
    pub filename: String,
    pub mime_type: String,
    pub role: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportMetadata {
    pub exporter_id: String,
    pub serializer_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionBundle {
    pub resolved_plan: ResolvedPlan,
    pub export: ExportMetadata,
    pub exported_files: Vec<ExportedFile>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkBundle {
    pub resolved_plan: ResolvedPlan,
    pub export: ExportMetadata,
    pub output_file_count: usize,
    pub total_output_bytes: u64,
    pub output_files: Vec<OutputFileSummary>,
    pub warnings: Vec<String>,
    pub telemetry: ConversionTelemetry,
    pub stage_telemetry: Vec<StageTelemetry>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamConversionBundle {
    pub resolved_plan: ResolvedPlan,
    pub export: ExportMetadata,
    pub output_file_count: usize,
    pub total_output_bytes: u64,
    pub output_files: Vec<OutputFileSummary>,
    pub warnings: Vec<String>,
    pub telemetry: ConversionTelemetry,
    pub stage_telemetry: Vec<StageTelemetry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionTelemetry {
    pub execution_mode: String,
    pub stream_batch_size: usize,
    pub ifcowl_max_workers: usize,
    pub sink_chunk_size_bytes: usize,
    pub sink_max_pending_bytes: usize,
}

/// Per-stage telemetry for a single plugin execution.
///
/// Emitted live through the sink callback as `stageEvent` events,
/// and included in the final result bundle for post-hoc inspection.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageTelemetry {
    pub plugin_id: String,
    pub stage: String,
    pub status: String,
    pub duration_ms: u64,
    pub bytes_out: u64,
    pub triples_out: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionPlanView {
    pub selected_mode: String,
    pub estimated_peak_mb: u64,
    pub feasibility_check_mb: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputFileSummary {
    pub filename: String,
    pub mime_type: String,
    pub role: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct OutputFormats {
    pub turtle: bool,
    pub nquads: bool,
    pub nquads_chunked: bool,
}

impl OutputFormats {
    pub fn is_empty(&self) -> bool {
        !self.turtle && !self.nquads && !self.nquads_chunked
    }

    pub fn has_any_nquads(&self) -> bool {
        self.nquads || self.nquads_chunked
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionMode {
    Fast,
    Lowmem,
}

#[derive(Debug, Clone)]
pub struct NquadsModuleOptions {
    pub chunking: NquadsChunkingMode,
    pub chunk_size_lines: usize,
    pub chunk_size_bytes: usize,
    pub chunk_prefix: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NquadsChunkingMode {
    None,
    Lines,
    Bytes,
}

#[derive(Debug, Clone)]
pub struct ExecutionSettings {
    pub output_formats: OutputFormats,
    // LBD sub-module activation flags.
    // These drive `active_producer_ids_from_settings()` which builds the id list
    // passed to `spawn_producers()`. When the ActivationPlan is threaded through
    // to the dispatch sites directly, these per-field booleans can be removed.
    // TODO: replace with ActivationPlan.enabled_ids lookup once the plan is
    //       propagated into all dispatch sites.
    pub emit_bot: bool,
    pub emit_beo: bool,
    pub emit_props_opm: bool,
    pub emit_omg_fog: bool,
    // Other producer flags
    pub emit_ifcowl: bool,
    pub nquads: NquadsModuleOptions,
    pub output_stem: String,
    pub turtle_grouping: TurtleGrouping,
    pub turtle_layout: TurtleLayout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurtleGrouping {
    Sorted,
    Streaming,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurtleLayout {
    Joined,
    Separate,
}
