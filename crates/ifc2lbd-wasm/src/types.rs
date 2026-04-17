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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    Turtle,
    Nquads,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionMode {
    Fast,
    Lowmem,
}

#[derive(Clone, Copy, Debug)]
pub enum TurtleBatchKind {
    Lbd,
    Ifcowl,
}

#[derive(Debug, Clone)]
pub struct NquadsModuleOptions {
    pub lbd_graph_iri: Option<String>,
    pub ifcowl_graph_iri: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExecutionSettings {
    pub output_format: OutputFormat,
    pub emit_ifcowl: bool,
    pub nquads: NquadsModuleOptions,
    pub output_stem: String,
    pub turtle_grouping: TurtleGrouping,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurtleGrouping {
    Sorted,
    Streaming,
}
