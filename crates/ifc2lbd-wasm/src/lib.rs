use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

use ifc_model::build_model;
use ifc_step::parse_step_bytes;
use lbd_converter::{convert_step_and_model, stream_step_and_model, ConvertOptions};
use lbd_pipeline::{
    ActivationError, ActivationPlan, ExportPlugin, FailurePolicy, ParallelismMode, PipelinePlugin,
    PipelineStage, PluginManifest, PluginRegistry, ProducerPlugin, SerializerPlugin,
};
use lbd_serializer::{
    serialize_lbd_batches_incremental_to_writer, serialize_nquads_batches_to_writer,
    serialize_nquads_merged_batches_to_writer, serialize_turtle_batch_to_writer,
    serialize_turtle_batch_raw_to_writer, write_turtle_prefixes_for_stream,
    serialize_turtle_to_writer,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
#[cfg(target_arch = "wasm32")]
use js_sys::{Function, Object, Reflect, Uint8Array};

const LBD_PRODUCER_ID: &str = "neo-lbd-producer";
const IFCOWL_PRODUCER_ID: &str = "neo-ifcowl-producer";
const TURTLE_SERIALIZER_ID: &str = "neo-turtle-serializer";
const NQUADS_SERIALIZER_ID: &str = "neo-nquads-serializer";
const FILE_EXPORT_ID: &str = "neo-file-export";

const DEFAULT_BASE_URI: &str = "https://lbd.example.com/";

#[derive(Debug, thiserror::Error)]
enum WasmApiError {
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
struct ConversionRequest {
    module_ids: Vec<String>,
    #[serde(default)]
    module_options: Vec<String>,
    #[serde(default)]
    base_uri: Option<String>,
    #[serde(default)]
    output_stem: Option<String>,
    #[serde(default)]
    execution_mode: Option<String>,
    #[serde(default)]
    memory_feasibility_mb: Option<u64>,
    #[serde(default)]
    stream_batch_size: Option<usize>,
    #[serde(default)]
    ifcowl_max_workers: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModuleManifestView {
    id: String,
    display_name: String,
    stage: String,
    description: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    requires: Vec<String>,
    conflicts_with: Vec<String>,
    failure_policy: String,
    parallelism: String,
    wasm_compatible: bool,
    option_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResolvedPlan {
    enabled_ids: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportedFile {
    filename: String,
    mime_type: String,
    role: String,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportMetadata {
    exporter_id: String,
    serializer_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConversionBundle {
    resolved_plan: ResolvedPlan,
    export: ExportMetadata,
    exported_files: Vec<ExportedFile>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkBundle {
    resolved_plan: ResolvedPlan,
    export: ExportMetadata,
    output_file_count: usize,
    total_output_bytes: u64,
    output_files: Vec<OutputFileSummary>,
    warnings: Vec<String>,
    telemetry: ConversionTelemetry,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamConversionBundle {
    resolved_plan: ResolvedPlan,
    export: ExportMetadata,
    output_file_count: usize,
    total_output_bytes: u64,
    output_files: Vec<OutputFileSummary>,
    warnings: Vec<String>,
    telemetry: ConversionTelemetry,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConversionTelemetry {
    execution_mode: String,
    stream_batch_size: usize,
    ifcowl_max_workers: usize,
    sink_chunk_size_bytes: usize,
    sink_max_pending_bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionPlanView {
    selected_mode: String,
    estimated_peak_mb: u64,
    feasibility_check_mb: u64,
    reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputFileSummary {
    filename: String,
    mime_type: String,
    role: String,
    bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputFormat {
    Turtle,
    Nquads,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutionMode {
    Fast,
    Lowmem,
}

#[derive(Clone, Copy, Debug)]
enum TurtleBatchKind {
    Lbd,
    Ifcowl,
}

#[derive(Debug, Clone)]
struct NquadsModuleOptions {
    lbd_graph_iri: Option<String>,
    ifcowl_graph_iri: Option<String>,
}

#[derive(Debug, Clone)]
struct ExecutionSettings {
    output_format: OutputFormat,
    emit_ifcowl: bool,
    nquads: NquadsModuleOptions,
    output_stem: String,
}

#[wasm_bindgen(js_name = listModules)]
pub fn list_modules() -> Result<JsValue, JsValue> {
    let registry = browser_registry();
    let modules: Vec<ModuleManifestView> = registry.manifests().into_iter().map(to_view).collect();
    serde_wasm_bindgen::to_value(&modules).map_err(js_err)
}

#[wasm_bindgen(js_name = resolvePlan)]
pub fn resolve_plan(
    requested_modules: JsValue,
    module_options: JsValue,
) -> Result<JsValue, JsValue> {
    let requested: Vec<String> =
        serde_wasm_bindgen::from_value(requested_modules).map_err(js_err)?;
    let options: Vec<String> = if module_options.is_null() || module_options.is_undefined() {
        Vec::new()
    } else {
        serde_wasm_bindgen::from_value(module_options).map_err(js_err)?
    };
    let resolved = resolve_plan_impl(requested, options).map_err(js_err)?;
    serde_wasm_bindgen::to_value(&resolved).map_err(js_err)
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = initNeoThreadPool)]
pub fn init_neo_thread_pool(threads: usize) -> js_sys::Promise {
    wasm_bindgen_rayon::init_thread_pool(threads)
}

#[wasm_bindgen(js_name = convertIfc)]
pub fn convert_ifc(input: &[u8], request: JsValue) -> Result<JsValue, JsValue> {
    let request: ConversionRequest = serde_wasm_bindgen::from_value(request).map_err(js_err)?;
    let result = convert_ifc_impl(input, request).map_err(js_err)?;
    serde_wasm_bindgen::to_value(&result).map_err(js_err)
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = convertIfcToSink)]
pub fn convert_ifc_to_sink(
    input: &[u8],
    request: JsValue,
    sink: Function,
) -> Result<JsValue, JsValue> {
    let request: ConversionRequest = serde_wasm_bindgen::from_value(request).map_err(js_err)?;
    let result = convert_ifc_to_sink_impl(input, request, &sink).map_err(js_err)?;
    serde_wasm_bindgen::to_value(&result).map_err(js_err)
}

#[wasm_bindgen(js_name = benchmarkConvertIfc)]
pub fn benchmark_convert_ifc(input: &[u8], request: JsValue) -> Result<JsValue, JsValue> {
    let request: ConversionRequest = serde_wasm_bindgen::from_value(request).map_err(js_err)?;
    let result = benchmark_convert_ifc_impl(input, request).map_err(js_err)?;
    serde_wasm_bindgen::to_value(&result).map_err(js_err)
}

#[wasm_bindgen(js_name = planExecution)]
pub fn plan_execution(input_size_bytes: f64, request: JsValue) -> Result<JsValue, JsValue> {
    let request: ConversionRequest = serde_wasm_bindgen::from_value(request).map_err(js_err)?;
    let settings = requested_settings_for_planning(&request).map_err(js_err)?;
    let (mode, estimated_peak_mb, feasibility_check_mb, reason) =
        select_execution_mode(input_size_bytes.max(0.0) as u64, &request, &settings);
    let plan = ExecutionPlanView {
        selected_mode: execution_mode_str(mode).to_string(),
        estimated_peak_mb,
        feasibility_check_mb,
        reason,
    };
    serde_wasm_bindgen::to_value(&plan).map_err(js_err)
}

fn resolve_plan_impl(
    requested_modules: Vec<String>,
    module_options: Vec<String>,
) -> Result<ResolvedPlan, WasmApiError> {
    let registry = browser_registry();
    let requested = dedupe_modules(requested_modules);
    let plan = registry.resolve_activation(&requested)?;
    let configs = parse_module_configs(&module_options)?;
    validate_module_configs(&plan, &configs)?;
    validate_typed_module_configs(&configs)?;
    validate_activation_plan(&plan)?;
    Ok(ResolvedPlan {
        enabled_ids: plan.enabled_ids,
        warnings: Vec::new(),
    })
}

fn convert_ifc_impl(
    input: &[u8],
    request: ConversionRequest,
) -> Result<ConversionBundle, WasmApiError> {
    let registry = browser_registry();
    let requested = dedupe_modules(request.module_ids.clone());
    let plan = registry.resolve_activation(&requested)?;
    let configs = parse_module_configs(&request.module_options)?;
    validate_module_configs(&plan, &configs)?;
    validate_typed_module_configs(&configs)?;
    validate_activation_plan(&plan)?;

    let mut warnings = Vec::new();
    let settings = resolve_execution_settings(&plan, &configs, &request, &mut warnings)?;
    let base_uri = request
        .base_uri
        .clone()
        .unwrap_or_else(|| DEFAULT_BASE_URI.to_string());

    let (mode, estimated_peak_mb, feasibility_check_mb, reason) =
        select_execution_mode(input.len() as u64, &request, &settings);
    warnings.push(format!(
        "execution mode={} (estimated_peak_mb={} feasibility_check_mb={}): {}",
        execution_mode_str(mode),
        estimated_peak_mb,
        feasibility_check_mb,
        reason
    ));
    if mode == ExecutionMode::Lowmem {
        return Err(WasmApiError::Message(
            "lowmem mode selected; use convertIfcToSink for streamed export".to_string(),
        ));
    }

    let step = parse_step_bytes(input)?;
    let model = build_model(&step)?;
    let stream_batch_size = effective_stream_batch_size(mode, &request);
    let ifcowl_max_workers = effective_ifcowl_workers(mode, &request);
    let options = ConvertOptions {
        base_uri: base_uri.clone(),
        emit_ifcowl_links: settings.emit_ifcowl,
        enable_topology: false,
        enable_topology_extension: false,
        topology_only: false,
        suppress_non_topology_fallback: false,
        geometry_relations: None,
        geometry_bounding_boxes: None,
        geometry_wkts: None,
        geometry_tolerance: 1e-6,
        low_memory_mode: false,
        stream_batch_size,
        ifcowl_max_workers,
    };
    let conversion = convert_step_and_model(&step, &model, &options);
    let exported_files = export_browser_files(&conversion, &base_uri, &settings)
        .map_err(|err| WasmApiError::Serialization(err.to_string()))?;

    let serializer_id = match settings.output_format {
        OutputFormat::Turtle => TURTLE_SERIALIZER_ID,
        OutputFormat::Nquads => NQUADS_SERIALIZER_ID,
    };

    Ok(ConversionBundle {
        resolved_plan: ResolvedPlan {
            enabled_ids: plan.enabled_ids,
            warnings: Vec::new(),
        },
        export: ExportMetadata {
            exporter_id: FILE_EXPORT_ID.to_string(),
            serializer_id: serializer_id.to_string(),
        },
        exported_files,
        warnings,
    })
}

fn benchmark_convert_ifc_impl(
    input: &[u8],
    request: ConversionRequest,
) -> Result<BenchmarkBundle, WasmApiError> {
    let registry = browser_registry();
    let requested = dedupe_modules(request.module_ids.clone());
    let plan = registry.resolve_activation(&requested)?;
    let configs = parse_module_configs(&request.module_options)?;
    validate_module_configs(&plan, &configs)?;
    validate_typed_module_configs(&configs)?;
    validate_activation_plan(&plan)?;

    let mut warnings = Vec::new();
    let settings = resolve_execution_settings(&plan, &configs, &request, &mut warnings)?;
    let base_uri = request
        .base_uri
        .clone()
        .unwrap_or_else(|| DEFAULT_BASE_URI.to_string());
    let (mode, _estimated_peak_mb, _feasibility_check_mb, _reason) =
        select_execution_mode(input.len() as u64, &request, &settings);

    let step = parse_step_bytes(input)?;
    let model = build_model(&step)?;
    let lowmem = mode == ExecutionMode::Lowmem;
    let stream_batch_size = effective_stream_batch_size(mode, &request);
    let ifcowl_max_workers = effective_ifcowl_workers(mode, &request);
    let options = ConvertOptions {
        base_uri: base_uri.clone(),
        emit_ifcowl_links: settings.emit_ifcowl,
        enable_topology: false,
        enable_topology_extension: false,
        topology_only: false,
        suppress_non_topology_fallback: false,
        geometry_relations: None,
        geometry_bounding_boxes: None,
        geometry_wkts: None,
        geometry_tolerance: 1e-6,
        low_memory_mode: lowmem,
        stream_batch_size,
        ifcowl_max_workers,
    };
    let output_files =
        export_browser_file_summaries_streaming(&step, &model, &options, &base_uri, &settings)
            .map_err(|err| WasmApiError::Serialization(err.to_string()))?;
    let total_output_bytes = output_files.iter().map(|f| f.bytes).sum();
    let serializer_id = match settings.output_format {
        OutputFormat::Turtle => TURTLE_SERIALIZER_ID,
        OutputFormat::Nquads => NQUADS_SERIALIZER_ID,
    };
    Ok(BenchmarkBundle {
        resolved_plan: ResolvedPlan {
            enabled_ids: plan.enabled_ids,
            warnings: Vec::new(),
        },
        export: ExportMetadata {
            exporter_id: FILE_EXPORT_ID.to_string(),
            serializer_id: serializer_id.to_string(),
        },
        output_file_count: output_files.len(),
        total_output_bytes,
        output_files,
        warnings,
        telemetry: ConversionTelemetry {
            execution_mode: execution_mode_str(mode).to_string(),
            stream_batch_size: options.stream_batch_size,
            ifcowl_max_workers: options.ifcowl_max_workers,
            sink_chunk_size_bytes: 0,
            sink_max_pending_bytes: 0,
        },
    })
}

#[cfg(target_arch = "wasm32")]
fn convert_ifc_to_sink_impl(
    input: &[u8],
    request: ConversionRequest,
    sink: &Function,
) -> Result<StreamConversionBundle, WasmApiError> {
    let registry = browser_registry();
    let requested = dedupe_modules(request.module_ids.clone());
    let plan = registry.resolve_activation(&requested)?;
    let configs = parse_module_configs(&request.module_options)?;
    validate_module_configs(&plan, &configs)?;
    validate_typed_module_configs(&configs)?;
    validate_activation_plan(&plan)?;

    let mut warnings = Vec::new();
    let settings = resolve_execution_settings(&plan, &configs, &request, &mut warnings)?;
    let base_uri = request
        .base_uri
        .clone()
        .unwrap_or_else(|| DEFAULT_BASE_URI.to_string());
    let (mode, estimated_peak_mb, feasibility_check_mb, reason) =
        select_execution_mode(input.len() as u64, &request, &settings);
    warnings.push(format!(
        "execution mode={} (estimated_peak_mb={} feasibility_check_mb={}): {}",
        execution_mode_str(mode),
        estimated_peak_mb,
        feasibility_check_mb,
        reason
    ));

    let serializer_id = match settings.output_format {
        OutputFormat::Turtle => TURTLE_SERIALIZER_ID,
        OutputFormat::Nquads => NQUADS_SERIALIZER_ID,
    };
    let step = parse_step_bytes(input)?;
    let model = build_model(&step)?;
    let lowmem = mode == ExecutionMode::Lowmem;
    let stream_batch_size = effective_stream_batch_size(mode, &request);
    let ifcowl_max_workers = effective_ifcowl_workers(mode, &request);
    let options = ConvertOptions {
        base_uri: base_uri.clone(),
        emit_ifcowl_links: settings.emit_ifcowl,
        enable_topology: false,
        enable_topology_extension: false,
        topology_only: false,
        suppress_non_topology_fallback: false,
        geometry_relations: None,
        geometry_bounding_boxes: None,
        geometry_wkts: None,
        geometry_tolerance: 1e-6,
        low_memory_mode: lowmem,
        stream_batch_size,
        ifcowl_max_workers,
    };
    let (output_files, sink_max_pending_bytes, sink_chunk_size_bytes) = export_browser_files_to_sink_streaming(
        step,
        model,
        options,
        &base_uri,
        &settings,
        sink,
    )
    .map_err(|err| WasmApiError::Serialization(err.to_string()))?;
    let total_output_bytes = output_files.iter().map(|f| f.bytes).sum();
    Ok(StreamConversionBundle {
        resolved_plan: ResolvedPlan {
            enabled_ids: plan.enabled_ids,
            warnings: Vec::new(),
        },
        export: ExportMetadata {
            exporter_id: FILE_EXPORT_ID.to_string(),
            serializer_id: serializer_id.to_string(),
        },
        output_file_count: output_files.len(),
        total_output_bytes,
        output_files,
        warnings,
        telemetry: ConversionTelemetry {
            execution_mode: execution_mode_str(mode).to_string(),
            stream_batch_size,
            ifcowl_max_workers,
            sink_chunk_size_bytes,
            sink_max_pending_bytes,
        },
    })
}

fn requested_settings_for_planning(request: &ConversionRequest) -> Result<ExecutionSettings, WasmApiError> {
    let registry = browser_registry();
    let requested = dedupe_modules(request.module_ids.clone());
    let plan = registry.resolve_activation(&requested)?;
    let configs = parse_module_configs(&request.module_options)?;
    validate_module_configs(&plan, &configs)?;
    validate_typed_module_configs(&configs)?;
    validate_activation_plan(&plan)?;
    let mut warnings = Vec::new();
    resolve_execution_settings(&plan, &configs, request, &mut warnings)
}

fn execution_mode_str(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Fast => "fast",
        ExecutionMode::Lowmem => "lowmem",
    }
}

fn effective_stream_batch_size(mode: ExecutionMode, request: &ConversionRequest) -> usize {
    if let Some(explicit) = request.stream_batch_size {
        return explicit.clamp(64, 32 * 1024);
    }
    let threads = rayon::current_num_threads().max(1);
    match mode {
        ExecutionMode::Fast => (threads * 1024).clamp(1024, 32 * 1024),
        ExecutionMode::Lowmem => (threads * 256).clamp(128, 8 * 1024),
    }
}

fn effective_ifcowl_workers(mode: ExecutionMode, request: &ConversionRequest) -> usize {
    if let Some(explicit) = request.ifcowl_max_workers {
        return explicit.clamp(1, 64);
    }
    let threads = rayon::current_num_threads().max(1);
    match mode {
        ExecutionMode::Fast => threads.max(1),
        ExecutionMode::Lowmem => threads.div_ceil(2).max(1),
    }
}

fn select_execution_mode(
    input_size_bytes: u64,
    request: &ConversionRequest,
    settings: &ExecutionSettings,
) -> (ExecutionMode, u64, u64, String) {
    let input_mb = (input_size_bytes / (1024 * 1024)).max(1);
    let multiplier = match (settings.output_format, settings.emit_ifcowl) {
        (OutputFormat::Nquads, true) => 26,
        (OutputFormat::Nquads, false) => 16,
        (OutputFormat::Turtle, true) => 22,
        (OutputFormat::Turtle, false) => 14,
    };
    let estimated_peak_mb = 96 + input_mb.saturating_mul(multiplier);
    let feasibility_check_mb = request
        .memory_feasibility_mb
        .unwrap_or_else(|| estimated_peak_mb.saturating_mul(4).max(512));
    let requested_mode = request
        .execution_mode
        .as_deref()
        .unwrap_or("auto")
        .to_ascii_lowercase();
    match requested_mode.as_str() {
        "fast" => (
            ExecutionMode::Fast,
            estimated_peak_mb,
            feasibility_check_mb,
            "explicit fast mode requested".to_string(),
        ),
        "lowmem" => (
            ExecutionMode::Lowmem,
            estimated_peak_mb,
            feasibility_check_mb,
            "explicit lowmem mode requested".to_string(),
        ),
        _ => {
            if estimated_peak_mb > feasibility_check_mb {
                (
                    ExecutionMode::Lowmem,
                    estimated_peak_mb,
                    feasibility_check_mb,
                    "auto selected lowmem because estimate exceeds feasibility check".to_string(),
                )
            } else {
                (
                    ExecutionMode::Fast,
                    estimated_peak_mb,
                    feasibility_check_mb,
                    "auto selected fast because estimate is within feasibility check".to_string(),
                )
            }
        }
    }
}

fn export_browser_file_summaries_streaming(
    step: &ifc_step::StepFile,
    model: &ifc_model::IfcModel,
    options: &ConvertOptions,
    base_uri: &str,
    settings: &ExecutionSettings,
) -> Result<Vec<OutputFileSummary>, lbd_serializer::SerializerError> {
    match settings.output_format {
        OutputFormat::Turtle => {
            let chan_cap = if options.low_memory_mode { 4 } else { 16 };
            let (lbd_sender, lbd_receiver) = crossbeam::channel::bounded(chan_cap);
            if settings.emit_ifcowl {
                let (ifcowl_sender, ifcowl_receiver) = crossbeam::channel::bounded(chan_cap);
                let (merged_sender, merged_receiver) = crossbeam::channel::bounded(chan_cap * 2);
                let (producer_result_sender, producer_result_receiver) =
                    crossbeam::channel::bounded::<Result<(), lbd_serializer::SerializerError>>(1);
                let (forward_result_sender, forward_result_receiver) =
                    crossbeam::channel::bounded::<Result<(), lbd_serializer::SerializerError>>(2);
                let mut lbd_count = CountingWriter::default();
                let mut ifcowl_count = CountingWriter::default();
                if !options.low_memory_mode {
                    write_turtle_prefixes_for_stream(&mut lbd_count, Some(&options.base_uri))?;
                    write_turtle_prefixes_for_stream(&mut ifcowl_count, Some(&options.base_uri))?;
                }
                rayon::scope(|scope| -> Result<(), lbd_serializer::SerializerError> {
                    scope.spawn(move |_| {
                        let result = stream_step_and_model(step, model, options, &lbd_sender, Some(&ifcowl_sender))
                            .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into()));
                        drop(lbd_sender);
                        drop(ifcowl_sender);
                        let _ = producer_result_sender.send(result.map(|_| ()));
                    });
                    let lbd_forward_sender = merged_sender.clone();
                    let lbd_result_sender = forward_result_sender.clone();
                    scope.spawn(move |_| {
                        let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                            for batch in lbd_receiver {
                                lbd_forward_sender
                                    .send((TurtleBatchKind::Lbd, batch))
                                    .map_err(|_| {
                                        lbd_serializer::SerializerError::Io(
                                            io::ErrorKind::BrokenPipe.into(),
                                        )
                                    })?;
                            }
                            Ok(())
                        })();
                        let _ = lbd_result_sender.send(result);
                    });
                    let ifcowl_forward_sender = merged_sender.clone();
                    let ifcowl_result_sender = forward_result_sender.clone();
                    scope.spawn(move |_| {
                        let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                            for batch in ifcowl_receiver {
                                ifcowl_forward_sender
                                    .send((TurtleBatchKind::Ifcowl, batch))
                                    .map_err(|_| {
                                        lbd_serializer::SerializerError::Io(
                                            io::ErrorKind::BrokenPipe.into(),
                                        )
                                    })?;
                            }
                            Ok(())
                        })();
                        let _ = ifcowl_result_sender.send(result);
                    });
                    drop(merged_sender);
                    drop(forward_result_sender);
                    for (kind, batch) in merged_receiver {
                        match kind {
                            TurtleBatchKind::Lbd => {
                                if options.low_memory_mode {
                                    serialize_turtle_batch_raw_to_writer(&batch, &mut lbd_count)?
                                } else {
                                    serialize_turtle_batch_to_writer(
                                        &batch,
                                        &mut lbd_count,
                                        Some(&options.base_uri),
                                    )?
                                }
                            }
                            TurtleBatchKind::Ifcowl => {
                                if options.low_memory_mode {
                                    serialize_turtle_batch_raw_to_writer(&batch, &mut ifcowl_count)?
                                } else {
                                    serialize_turtle_batch_to_writer(
                                        &batch,
                                        &mut ifcowl_count,
                                        Some(&options.base_uri),
                                    )?
                                }
                            }
                        }
                    }
                    producer_result_receiver
                        .recv()
                        .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into()))??;
                    for _ in 0..2 {
                        forward_result_receiver
                            .recv()
                            .map_err(|_| {
                                lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
                            })??;
                    }
                    Ok(())
                })?;
                Ok(vec![
                    OutputFileSummary {
                        filename: format!("{}.ttl", settings.output_stem),
                        mime_type: "text/turtle;charset=utf-8".to_string(),
                        role: "lbd".to_string(),
                        bytes: lbd_count.bytes,
                    },
                    OutputFileSummary {
                        filename: format!("{}_ifcowl.ttl", settings.output_stem),
                        mime_type: "text/turtle;charset=utf-8".to_string(),
                        role: "ifcowl".to_string(),
                        bytes: ifcowl_count.bytes,
                    },
                ])
            } else {
                let (producer_result_sender, producer_result_receiver) =
                    crossbeam::channel::bounded::<Result<(), lbd_serializer::SerializerError>>(1);
                let mut lbd_count = CountingWriter::default();
                rayon::scope(|scope| -> Result<(), lbd_serializer::SerializerError> {
                    scope.spawn(move |_| {
                        let result = stream_step_and_model(step, model, options, &lbd_sender, None)
                            .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into()));
                        drop(lbd_sender);
                        let _ = producer_result_sender.send(result.map(|_| ()));
                    });
                    if options.low_memory_mode {
                        for batch in lbd_receiver {
                            serialize_turtle_batch_raw_to_writer(&batch, &mut lbd_count)?;
                        }
                    } else {
                        serialize_lbd_batches_incremental_to_writer(
                            lbd_receiver,
                            &mut lbd_count,
                            &options.base_uri,
                        )?;
                    }
                    producer_result_receiver
                        .recv()
                        .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into()))??;
                    Ok(())
                })?;
                Ok(vec![OutputFileSummary {
                    filename: format!("{}.ttl", settings.output_stem),
                    mime_type: "text/turtle;charset=utf-8".to_string(),
                    role: "lbd".to_string(),
                    bytes: lbd_count.bytes,
                }])
            }
        }
        OutputFormat::Nquads => {
            let normalized_base = normalize_base_for_graph_iri(base_uri);
            let lbd_graph = settings
                .nquads
                .lbd_graph_iri
                .clone()
                .unwrap_or_else(|| format!("{normalized_base}/lbd"));
            let ifcowl_graph = settings
                .nquads
                .ifcowl_graph_iri
                .clone()
                .unwrap_or_else(|| format!("{normalized_base}/ifcowl"));
            let (lbd_sender, lbd_receiver) = crossbeam::channel::bounded(8);
            let (ifcowl_sender, ifcowl_receiver) = crossbeam::channel::bounded(8);
            let (consumer_result_sender, consumer_result_receiver) =
                crossbeam::channel::bounded::<Result<u64, lbd_serializer::SerializerError>>(1);
            let emit_ifcowl = settings.emit_ifcowl;
            let lbd_graph_clone = lbd_graph.clone();
            let ifcowl_graph_clone = ifcowl_graph.clone();
            rayon::spawn(move || {
                let mut count_writer = CountingWriter::default();
                let result = if emit_ifcowl {
                    serialize_nquads_merged_batches_to_writer(
                        lbd_receiver,
                        ifcowl_receiver,
                        &mut count_writer,
                        &lbd_graph_clone,
                        &ifcowl_graph_clone,
                    )
                } else {
                    serialize_nquads_batches_to_writer(lbd_receiver, &mut count_writer, &lbd_graph_clone)
                }
                .map(|_| count_writer.bytes);
                let _ = consumer_result_sender.send(result);
            });

            let producer_result = if settings.emit_ifcowl {
                stream_step_and_model(step, model, options, &lbd_sender, Some(&ifcowl_sender))
            } else {
                stream_step_and_model(step, model, options, &lbd_sender, None)
            };
            drop(lbd_sender);
            drop(ifcowl_sender);

            if producer_result.is_err() {
                return Err(lbd_serializer::SerializerError::Io(
                    io::ErrorKind::BrokenPipe.into(),
                ));
            }

            let bytes = consumer_result_receiver
                .recv()
                .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into()))??;
            Ok(vec![OutputFileSummary {
                filename: format!("{}.nq", settings.output_stem),
                mime_type: "application/n-quads".to_string(),
                role: "merged".to_string(),
                bytes,
            }])
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn export_browser_files_to_sink_streaming(
    step: ifc_step::StepFile,
    model: ifc_model::IfcModel,
    options: ConvertOptions,
    base_uri: &str,
    settings: &ExecutionSettings,
    sink: &Function,
) -> Result<(Vec<OutputFileSummary>, usize, usize), lbd_serializer::SerializerError> {
    match settings.output_format {
        OutputFormat::Turtle => {
            let mut summaries = Vec::new();
            let chan_cap = if options.low_memory_mode { 4 } else { 16 };
            let instance_base = options.base_uri.clone();
            let mut lbd_writer = SinkChunkWriter::new(
                sink,
                format!("{}.ttl", settings.output_stem),
                "text/turtle;charset=utf-8",
                "lbd",
                1024 * 1024,
            )?;
            let (lbd_sender, lbd_receiver) = crossbeam::channel::bounded(chan_cap);
            if settings.emit_ifcowl {
                let (ifcowl_sender, ifcowl_receiver) = crossbeam::channel::bounded(chan_cap);
                let (merged_sender, merged_receiver) = crossbeam::channel::bounded(chan_cap * 2);
                let (producer_result_sender, producer_result_receiver) =
                    crossbeam::channel::bounded::<Result<(), lbd_serializer::SerializerError>>(1);
                let (forward_result_sender, forward_result_receiver) =
                    crossbeam::channel::bounded::<Result<(), lbd_serializer::SerializerError>>(2);
                let options_for_producer = options.clone();
                rayon::spawn(move || {
                    let result = stream_step_and_model(&step, &model, &options_for_producer, &lbd_sender, Some(&ifcowl_sender))
                        .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into()));
                    drop(lbd_sender);
                    drop(ifcowl_sender);
                    let _ = producer_result_sender.send(result.map(|_| ()));
                });
                if !options.low_memory_mode {
                    write_turtle_prefixes_for_stream(&mut lbd_writer, Some(&instance_base))?;
                }

                let mut ifcowl_writer = SinkChunkWriter::new(
                    sink,
                    format!("{}_ifcowl.ttl", settings.output_stem),
                    "text/turtle;charset=utf-8",
                    "ifcowl",
                    1024 * 1024,
                )?;
                if !options.low_memory_mode {
                    write_turtle_prefixes_for_stream(&mut ifcowl_writer, Some(&instance_base))?;
                }
                let lbd_forward_sender = merged_sender.clone();
                let lbd_result_sender = forward_result_sender.clone();
                rayon::spawn(move || {
                    let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                        for batch in lbd_receiver {
                            lbd_forward_sender
                                .send((TurtleBatchKind::Lbd, batch))
                                .map_err(|_| {
                                    lbd_serializer::SerializerError::Io(
                                        io::ErrorKind::BrokenPipe.into(),
                                    )
                                })?;
                        }
                        Ok(())
                    })();
                    let _ = lbd_result_sender.send(result);
                });
                let ifcowl_forward_sender = merged_sender.clone();
                let ifcowl_result_sender = forward_result_sender.clone();
                rayon::spawn(move || {
                    let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                        for batch in ifcowl_receiver {
                            ifcowl_forward_sender
                                .send((TurtleBatchKind::Ifcowl, batch))
                                .map_err(|_| {
                                    lbd_serializer::SerializerError::Io(
                                        io::ErrorKind::BrokenPipe.into(),
                                    )
                                })?;
                        }
                        Ok(())
                    })();
                    let _ = ifcowl_result_sender.send(result);
                });
                drop(merged_sender);
                drop(forward_result_sender);
                for (kind, batch) in merged_receiver {
                    match kind {
                        TurtleBatchKind::Lbd => {
                            if options.low_memory_mode {
                                serialize_turtle_batch_raw_to_writer(&batch, &mut lbd_writer)?
                            } else {
                                serialize_turtle_batch_to_writer(
                                    &batch,
                                    &mut lbd_writer,
                                    Some(&instance_base),
                                )?
                            }
                        }
                        TurtleBatchKind::Ifcowl => {
                            if options.low_memory_mode {
                                serialize_turtle_batch_raw_to_writer(&batch, &mut ifcowl_writer)?
                            } else {
                                serialize_turtle_batch_to_writer(
                                    &batch,
                                    &mut ifcowl_writer,
                                    Some(&instance_base),
                                )?
                            }
                        }
                    }
                }
                producer_result_receiver
                    .recv()
                    .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into()))??;
                for _ in 0..2 {
                    forward_result_receiver
                        .recv()
                        .map_err(|_| {
                            lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
                        })??;
                }
                let (lbd_summary, lbd_peak, lbd_chunk_size) = lbd_writer.finish()?;
                summaries.push(lbd_summary);
                let (ifcowl_summary, ifcowl_peak, ifcowl_chunk_size) = ifcowl_writer.finish()?;
                summaries.push(ifcowl_summary);
                Ok((summaries, lbd_peak.max(ifcowl_peak), lbd_chunk_size.max(ifcowl_chunk_size)))
            } else {
                let (producer_result_sender, producer_result_receiver) =
                    crossbeam::channel::bounded::<Result<(), lbd_serializer::SerializerError>>(1);
                let options_for_producer = options.clone();
                rayon::spawn(move || {
                    let result = stream_step_and_model(&step, &model, &options_for_producer, &lbd_sender, None)
                        .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into()));
                    drop(lbd_sender);
                    let _ = producer_result_sender.send(result.map(|_| ()));
                });
                serialize_lbd_batches_incremental_to_writer(
                    lbd_receiver,
                    &mut lbd_writer,
                    &instance_base,
                )?;
                producer_result_receiver
                    .recv()
                    .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into()))??;
                let (lbd_summary, lbd_peak, lbd_chunk_size) = lbd_writer.finish()?;
                summaries.push(lbd_summary);
                Ok((summaries, lbd_peak, lbd_chunk_size))
            }
        }
        OutputFormat::Nquads => {
            let normalized_base = normalize_base_for_graph_iri(base_uri);
            let lbd_graph = settings
                .nquads
                .lbd_graph_iri
                .clone()
                .unwrap_or_else(|| format!("{normalized_base}/lbd"));
            let ifcowl_graph = settings
                .nquads
                .ifcowl_graph_iri
                .clone()
                .unwrap_or_else(|| format!("{normalized_base}/ifcowl"));
            let mut writer = SinkChunkWriter::new(
                sink,
                format!("{}.nq", settings.output_stem),
                "application/n-quads",
                "merged",
                1024 * 1024,
            )?;
            let (lbd_sender, lbd_receiver) = crossbeam::channel::bounded(8);
            let (ifcowl_sender, ifcowl_receiver) = crossbeam::channel::bounded(8);
            let emit_ifcowl = settings.emit_ifcowl;
            let (producer_result_sender, producer_result_receiver) =
                crossbeam::channel::bounded::<Result<(), lbd_serializer::SerializerError>>(1);
            rayon::spawn(move || {
                let result = if emit_ifcowl {
                    stream_step_and_model(&step, &model, &options, &lbd_sender, Some(&ifcowl_sender))
                } else {
                    stream_step_and_model(&step, &model, &options, &lbd_sender, None)
                }
                .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into()));
                drop(lbd_sender);
                drop(ifcowl_sender);
                let _ = producer_result_sender.send(result.map(|_| ()));
            });
            let serializer_result = if settings.emit_ifcowl {
                serialize_nquads_merged_batches_to_writer(
                    lbd_receiver,
                    ifcowl_receiver,
                    &mut writer,
                    &lbd_graph,
                    &ifcowl_graph,
                )
            } else {
                serialize_nquads_batches_to_writer(lbd_receiver, &mut writer, &lbd_graph)
            };
            serializer_result?;
            producer_result_receiver
                .recv()
                .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into()))??;
            let (summary, peak, chunk_size) = writer.finish()?;
            Ok((vec![summary], peak, chunk_size))
        }
    }
}

#[derive(Default)]
struct CountingWriter {
    bytes: u64,
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
struct SinkChunkWriter<'a> {
    sink: &'a Function,
    filename: String,
    mime_type: String,
    role: String,
    bytes: u64,
    chunk_size: usize,
    pending: Vec<u8>,
    max_pending: usize,
}

#[cfg(target_arch = "wasm32")]
impl<'a> SinkChunkWriter<'a> {
    fn new(
        sink: &'a Function,
        filename: String,
        mime_type: &str,
        role: &str,
        chunk_size: usize,
    ) -> Result<Self, lbd_serializer::SerializerError> {
        let writer = Self {
            sink,
            filename,
            mime_type: mime_type.to_string(),
            role: role.to_string(),
            bytes: 0,
            chunk_size,
            pending: Vec::with_capacity(chunk_size),
            max_pending: 0,
        };
        writer.emit_start()?;
        Ok(writer)
    }

    fn finish(mut self) -> Result<(OutputFileSummary, usize, usize), lbd_serializer::SerializerError> {
        self.flush_pending()?;
        self.emit_end()?;
        Ok((
            OutputFileSummary {
                filename: self.filename,
                mime_type: self.mime_type,
                role: self.role,
                bytes: self.bytes,
            },
            self.max_pending,
            self.chunk_size,
        ))
    }

    fn flush_pending(&mut self) -> Result<(), lbd_serializer::SerializerError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let event = Object::new();
        set_event_str(&event, "type", "fileChunk")?;
        set_event_str(&event, "filename", &self.filename)?;
        let chunk = Uint8Array::from(self.pending.as_slice());
        Reflect::set(&event, &JsValue::from_str("chunk"), &chunk)
            .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::Other.into()))?;
        self.sink
            .call1(&JsValue::NULL, &event)
            .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::Other.into()))?;
        self.pending.clear();
        Ok(())
    }

    fn emit_start(&self) -> Result<(), lbd_serializer::SerializerError> {
        let event = Object::new();
        set_event_str(&event, "type", "fileStart")?;
        set_event_str(&event, "filename", &self.filename)?;
        set_event_str(&event, "mimeType", &self.mime_type)?;
        set_event_str(&event, "role", &self.role)?;
        self.sink
            .call1(&JsValue::NULL, &event)
            .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::Other.into()))?;
        Ok(())
    }

    fn emit_end(&self) -> Result<(), lbd_serializer::SerializerError> {
        let event = Object::new();
        set_event_str(&event, "type", "fileEnd")?;
        set_event_str(&event, "filename", &self.filename)?;
        Reflect::set(
            &event,
            &JsValue::from_str("bytes"),
            &JsValue::from_f64(self.bytes as f64),
        )
        .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::Other.into()))?;
        self.sink
            .call1(&JsValue::NULL, &event)
            .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::Other.into()))?;
        Ok(())
    }

}

#[cfg(target_arch = "wasm32")]
impl Write for SinkChunkWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        if self.pending.len() > self.max_pending {
            self.max_pending = self.pending.len();
        }
        self.bytes += buf.len() as u64;
        if self.pending.len() >= self.chunk_size {
            self.flush_pending()
                .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_pending()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))
    }
}

#[cfg(target_arch = "wasm32")]
fn set_event_str(event: &Object, key: &str, value: &str) -> Result<(), lbd_serializer::SerializerError> {
    Reflect::set(event, &JsValue::from_str(key), &JsValue::from_str(value))
        .map_err(|_| lbd_serializer::SerializerError::Io(io::ErrorKind::Other.into()))?;
    Ok(())
}

fn export_browser_files(
    conversion: &lbd_converter::ConversionResult,
    base_uri: &str,
    settings: &ExecutionSettings,
) -> Result<Vec<ExportedFile>, lbd_serializer::SerializerError> {
    match settings.output_format {
        OutputFormat::Turtle => {
            let mut lbd_bytes = Vec::new();
            serialize_turtle_to_writer(&conversion.triples, &mut lbd_bytes)?;
            let mut files = vec![ExportedFile {
                filename: format!("{}.ttl", settings.output_stem),
                mime_type: "text/turtle;charset=utf-8".to_string(),
                role: "lbd".to_string(),
                payload: lbd_bytes,
            }];
            if settings.emit_ifcowl {
                let mut ifcowl_bytes = Vec::new();
                serialize_turtle_to_writer(&conversion.ifcowl_triples, &mut ifcowl_bytes)?;
                files.push(ExportedFile {
                    filename: format!("{}_ifcowl.ttl", settings.output_stem),
                    mime_type: "text/turtle;charset=utf-8".to_string(),
                    role: "ifcowl".to_string(),
                    payload: ifcowl_bytes,
                });
            }
            Ok(files)
        }
        OutputFormat::Nquads => {
            let normalized_base = normalize_base_for_graph_iri(base_uri);
            let lbd_graph = settings
                .nquads
                .lbd_graph_iri
                .clone()
                .unwrap_or_else(|| format!("{normalized_base}/lbd"));
            let ifcowl_graph = settings
                .nquads
                .ifcowl_graph_iri
                .clone()
                .unwrap_or_else(|| format!("{normalized_base}/ifcowl"));
            let mut nq_bytes = Vec::new();
            if settings.emit_ifcowl {
                let (lbd_sender, lbd_receiver) = crossbeam::channel::unbounded();
                let (ifcowl_sender, ifcowl_receiver) = crossbeam::channel::unbounded();
                lbd_sender.send(conversion.triples.clone()).map_err(|_| {
                    lbd_serializer::SerializerError::Io(std::io::ErrorKind::BrokenPipe.into())
                })?;
                ifcowl_sender
                    .send(conversion.ifcowl_triples.clone())
                    .map_err(|_| {
                        lbd_serializer::SerializerError::Io(std::io::ErrorKind::BrokenPipe.into())
                    })?;
                drop(lbd_sender);
                drop(ifcowl_sender);
                serialize_nquads_merged_batches_to_writer(
                    lbd_receiver,
                    ifcowl_receiver,
                    &mut nq_bytes,
                    &lbd_graph,
                    &ifcowl_graph,
                )?;
            } else {
                let (lbd_sender, lbd_receiver) = crossbeam::channel::unbounded();
                lbd_sender.send(conversion.triples.clone()).map_err(|_| {
                    lbd_serializer::SerializerError::Io(std::io::ErrorKind::BrokenPipe.into())
                })?;
                drop(lbd_sender);
                serialize_nquads_batches_to_writer(lbd_receiver, &mut nq_bytes, &lbd_graph)?;
            }
            Ok(vec![ExportedFile {
                filename: format!("{}.nq", settings.output_stem),
                mime_type: "application/n-quads".to_string(),
                role: "merged".to_string(),
                payload: nq_bytes,
            }])
        }
    }
}

fn normalize_base_for_graph_iri(base_uri: &str) -> String {
    base_uri.trim_end_matches('/').to_string()
}

fn dedupe_modules(ids: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        if seen.insert(id.clone()) {
            out.push(id);
        }
    }
    out
}

fn validate_activation_plan(plan: &ActivationPlan) -> Result<(), WasmApiError> {
    let active: HashSet<&str> = plan.enabled_ids.iter().map(|id| id.as_str()).collect();
    if !active.contains(LBD_PRODUCER_ID) {
        return Err(WasmApiError::Message(format!(
            "module plan must include `{}`",
            LBD_PRODUCER_ID
        )));
    }
    if !active.contains(FILE_EXPORT_ID) {
        return Err(WasmApiError::Message(format!(
            "module plan must include `{}`",
            FILE_EXPORT_ID
        )));
    }
    let has_nquads = active.contains(NQUADS_SERIALIZER_ID);
    let has_turtle = active.contains(TURTLE_SERIALIZER_ID);
    if has_nquads == has_turtle {
        return Err(WasmApiError::Message(
            "module plan must include exactly one serializer".to_string(),
        ));
    }
    Ok(())
}

fn resolve_execution_settings(
    plan: &ActivationPlan,
    configs: &HashMap<String, HashMap<String, String>>,
    request: &ConversionRequest,
    warnings: &mut Vec<String>,
) -> Result<ExecutionSettings, WasmApiError> {
    let active: HashSet<&str> = plan.enabled_ids.iter().map(|id| id.as_str()).collect();
    let output_format = match (
        active.contains(TURTLE_SERIALIZER_ID),
        active.contains(NQUADS_SERIALIZER_ID),
    ) {
        (true, false) => OutputFormat::Turtle,
        (false, true) => OutputFormat::Nquads,
        _ => {
            return Err(WasmApiError::Message(
                "module plan must include exactly one serializer".to_string(),
            ))
        }
    };

    let nquads_entries = configs.get(NQUADS_SERIALIZER_ID);
    let nquads_chunking = nquads_entries
        .and_then(|m| m.get("chunking"))
        .cloned()
        .unwrap_or_else(|| "none".to_string());
    if nquads_chunking != "none" {
        warnings.push(format!(
            "neo-nquads-serializer.chunking={} is not implemented in wasm phase 1; falling back to none",
            nquads_chunking
        ));
    }

    let output_stem = configs
        .get(FILE_EXPORT_ID)
        .and_then(|m| m.get("output_stem"))
        .cloned()
        .or_else(|| request.output_stem.clone())
        .unwrap_or_else(|| "output".to_string());

    Ok(ExecutionSettings {
        output_format,
        emit_ifcowl: active.contains(IFCOWL_PRODUCER_ID),
        nquads: NquadsModuleOptions {
            lbd_graph_iri: nquads_entries.and_then(|m| m.get("lbd_graph_iri")).cloned(),
            ifcowl_graph_iri: nquads_entries
                .and_then(|m| m.get("ifcowl_graph_iri"))
                .cloned(),
        },
        output_stem,
    })
}

fn parse_module_configs(
    values: &[String],
) -> Result<HashMap<String, HashMap<String, String>>, WasmApiError> {
    let mut by_module: HashMap<String, HashMap<String, String>> = HashMap::new();
    for raw in values {
        let (module_id, rest) = raw.split_once('.').ok_or_else(|| {
            WasmApiError::Message(format!(
                "expected `<module-id>.<key>=<value>`, got `{}`",
                raw
            ))
        })?;
        let (key, value) = rest.split_once('=').ok_or_else(|| {
            WasmApiError::Message(format!("expected `<key>=<value>` in `{}`", raw))
        })?;
        if module_id.is_empty() || key.is_empty() {
            return Err(WasmApiError::Message(format!(
                "module id and key must be non-empty in module option `{}`",
                raw
            )));
        }
        by_module
            .entry(module_id.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
    }
    Ok(by_module)
}

fn validate_module_configs(
    plan: &ActivationPlan,
    configs: &HashMap<String, HashMap<String, String>>,
) -> Result<(), WasmApiError> {
    let active: HashSet<&str> = plan.enabled_ids.iter().map(|id| id.as_str()).collect();
    for module_id in configs.keys() {
        if !active.contains(module_id.as_str()) {
            return Err(WasmApiError::Message(format!(
                "module options provided for `{}` but module is not active",
                module_id
            )));
        }
    }
    Ok(())
}

fn validate_typed_module_configs(
    configs: &HashMap<String, HashMap<String, String>>,
) -> Result<(), WasmApiError> {
    for (module_id, entries) in configs {
        match module_id.as_str() {
            NQUADS_SERIALIZER_ID => validate_nquads_serializer_options(entries)?,
            FILE_EXPORT_ID => validate_file_export_options(entries)?,
            LBD_PRODUCER_ID | IFCOWL_PRODUCER_ID | TURTLE_SERIALIZER_ID => {
                if !entries.is_empty() {
                    return Err(WasmApiError::Message(format!(
                        "module `{}` does not support options in wasm phase 1",
                        module_id
                    )));
                }
            }
            _ => {
                return Err(WasmApiError::Message(format!(
                    "unsupported module `{}`",
                    module_id
                )))
            }
        }
    }
    Ok(())
}

fn validate_nquads_serializer_options(
    entries: &HashMap<String, String>,
) -> Result<(), WasmApiError> {
    let allowed = [
        "chunking",
        "chunk_size_lines",
        "chunk_size_bytes",
        "chunk_prefix",
        "chunk_min_count",
        "chunk_core_count",
        "lbd_graph_iri",
        "ifcowl_graph_iri",
    ];
    for (key, value) in entries {
        if !allowed.contains(&key.as_str()) {
            return Err(WasmApiError::Message(format!(
                "unsupported option `neo-nquads-serializer.{}` in wasm phase 1",
                key
            )));
        }
        if matches!(
            key.as_str(),
            "chunk_size_lines" | "chunk_size_bytes" | "chunk_min_count" | "chunk_core_count"
        ) {
            value.parse::<usize>().map_err(|_| {
                WasmApiError::Message(format!(
                    "invalid integer for `neo-nquads-serializer.{}`: `{}`",
                    key, value
                ))
            })?;
        }
        if key == "chunking" && !matches!(value.as_str(), "none" | "lines" | "bytes" | "cores") {
            return Err(WasmApiError::Message(format!(
                "invalid `neo-nquads-serializer.chunking={}` (expected none|lines|bytes|cores)",
                value
            )));
        }
    }
    Ok(())
}

fn validate_file_export_options(entries: &HashMap<String, String>) -> Result<(), WasmApiError> {
    for (key, value) in entries {
        if key != "output_stem" {
            return Err(WasmApiError::Message(format!(
                "unsupported option `neo-file-export.{}` in wasm phase 1",
                key
            )));
        }
        if value.trim().is_empty() {
            return Err(WasmApiError::Message(
                "`neo-file-export.output_stem` must be non-empty".to_string(),
            ));
        }
    }
    Ok(())
}

fn to_view(manifest: PluginManifest) -> ModuleManifestView {
    ModuleManifestView {
        id: manifest.id.to_string(),
        display_name: manifest.display_name.to_string(),
        stage: format!("{:?}", manifest.stage),
        description: manifest.description.to_string(),
        inputs: manifest.inputs.into_iter().map(str::to_string).collect(),
        outputs: manifest.outputs.into_iter().map(str::to_string).collect(),
        requires: manifest.requires.into_iter().map(str::to_string).collect(),
        conflicts_with: manifest
            .conflicts_with
            .into_iter()
            .map(str::to_string)
            .collect(),
        failure_policy: format!("{:?}", manifest.failure_policy),
        parallelism: format!("{:?}", manifest.parallelism),
        wasm_compatible: manifest.wasm_compatible,
        option_keys: module_option_keys(manifest.id),
    }
}

fn module_option_keys(module_id: &str) -> Vec<String> {
    match module_id {
        NQUADS_SERIALIZER_ID => vec![
            "chunking".to_string(),
            "chunk_size_lines".to_string(),
            "chunk_size_bytes".to_string(),
            "chunk_prefix".to_string(),
            "chunk_min_count".to_string(),
            "chunk_core_count".to_string(),
            "lbd_graph_iri".to_string(),
            "ifcowl_graph_iri".to_string(),
        ],
        FILE_EXPORT_ID => vec!["output_stem".to_string()],
        _ => Vec::new(),
    }
}

fn browser_registry() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry.register_producer(LbdProducerPlugin).unwrap();
    registry.register_producer(IfcowlProducerPlugin).unwrap();
    registry
        .register_serializer(TurtleSerializerPlugin)
        .unwrap();
    registry
        .register_serializer(NquadsSerializerPlugin)
        .unwrap();
    registry.register_export(FileExportPlugin).unwrap();
    registry
}

struct LbdProducerPlugin;
struct IfcowlProducerPlugin;
struct TurtleSerializerPlugin;
struct NquadsSerializerPlugin;
struct FileExportPlugin;

impl PipelinePlugin for LbdProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: LBD_PRODUCER_ID,
            display_name: "Built-in LBD producer",
            stage: PipelineStage::Produce,
            description: "Generates LBD triples from the typed IFC model.",
            inputs: vec!["ifc-model"],
            outputs: vec!["lbd-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
        }
    }
}
impl ProducerPlugin for LbdProducerPlugin {}

impl PipelinePlugin for IfcowlProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: IFCOWL_PRODUCER_ID,
            display_name: "Built-in IfcOWL producer",
            stage: PipelineStage::Produce,
            description: "Generates IfcOWL triples from parsed IFC STEP entities.",
            inputs: vec!["step-file", "ifc-model"],
            outputs: vec!["ifcowl-triples"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
        }
    }
}
impl ProducerPlugin for IfcowlProducerPlugin {}

impl PipelinePlugin for TurtleSerializerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: TURTLE_SERIALIZER_ID,
            display_name: "Built-in Turtle serializer",
            stage: PipelineStage::Serialize,
            description: "Serializes triples into Turtle output.",
            inputs: vec!["triples"],
            outputs: vec!["turtle-bytes"],
            requires: vec![LBD_PRODUCER_ID],
            conflicts_with: vec![NQUADS_SERIALIZER_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
        }
    }
}
impl SerializerPlugin for TurtleSerializerPlugin {}

impl PipelinePlugin for NquadsSerializerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: NQUADS_SERIALIZER_ID,
            display_name: "Built-in N-Quads serializer",
            stage: PipelineStage::Serialize,
            description: "Serializes graph streams into N-Quads output.",
            inputs: vec!["quads"],
            outputs: vec!["nquads-bytes"],
            requires: vec![LBD_PRODUCER_ID],
            conflicts_with: vec![TURTLE_SERIALIZER_ID],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByPartition,
            wasm_compatible: true,
        }
    }
}
impl SerializerPlugin for NquadsSerializerPlugin {}

impl PipelinePlugin for FileExportPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: FILE_EXPORT_ID,
            display_name: "Built-in file exporter",
            stage: PipelineStage::Export,
            description: "Exports browser-downloadable artifacts from serializer output.",
            inputs: vec!["turtle-bytes", "nquads-bytes"],
            outputs: vec!["browser-files"],
            requires: vec![],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
        }
    }
}
impl ExportPlugin for FileExportPlugin {}

fn js_err<E: ToString>(error: E) -> JsValue {
    JsValue::from_str(&error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_ifc() -> Vec<u8> {
        b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n#1=IFCPROJECT('0001',$,$,$,$,$,$,$,$);\nENDSEC;\nEND-ISO-10303-21;\n".to_vec()
    }

    #[test]
    fn list_modules_exposes_curated_browser_set() {
        let ids: HashSet<String> = browser_registry()
            .manifests()
            .into_iter()
            .map(|m| m.id.to_string())
            .collect();
        assert!(ids.contains(LBD_PRODUCER_ID));
        assert!(ids.contains(IFCOWL_PRODUCER_ID));
        assert!(ids.contains(TURTLE_SERIALIZER_ID));
        assert!(ids.contains(NQUADS_SERIALIZER_ID));
        assert!(ids.contains(FILE_EXPORT_ID));
        assert!(!ids.contains("neo-topology-lite-producer"));
        assert!(!ids.contains("neo-topology-full-producer"));
    }

    #[test]
    fn resolve_plan_rejects_unknown_module() {
        let result = resolve_plan_impl(vec!["neo-topology-lite-producer".to_string()], Vec::new());
        assert!(result.is_err());
    }

    #[test]
    fn convert_turtle_exports_ttl_file() {
        let bundle = convert_ifc_impl(
            &tiny_ifc(),
            ConversionRequest {
                module_ids: vec![
                    LBD_PRODUCER_ID.to_string(),
                    TURTLE_SERIALIZER_ID.to_string(),
                    FILE_EXPORT_ID.to_string(),
                ],
                module_options: Vec::new(),
                base_uri: Some("https://example.test/base/".to_string()),
                output_stem: Some("model".to_string()),
            },
        )
        .expect("conversion should succeed");
        assert_eq!(bundle.exported_files.len(), 1);
        assert_eq!(bundle.exported_files[0].filename, "model.ttl");
        assert_eq!(
            bundle.exported_files[0].mime_type,
            "text/turtle;charset=utf-8"
        );
        assert!(!bundle.exported_files[0].payload.is_empty());
    }

    #[test]
    fn convert_nquads_ifcowl_exports_single_nq_file() {
        let bundle = convert_ifc_impl(
            &tiny_ifc(),
            ConversionRequest {
                module_ids: vec![
                    LBD_PRODUCER_ID.to_string(),
                    IFCOWL_PRODUCER_ID.to_string(),
                    NQUADS_SERIALIZER_ID.to_string(),
                    FILE_EXPORT_ID.to_string(),
                ],
                module_options: Vec::new(),
                base_uri: Some("https://example.test/base/".to_string()),
                output_stem: Some("model".to_string()),
            },
        )
        .expect("conversion should succeed");
        assert_eq!(bundle.exported_files.len(), 1);
        assert_eq!(bundle.exported_files[0].filename, "model.nq");
        assert_eq!(bundle.exported_files[0].mime_type, "application/n-quads");
        assert!(!bundle.exported_files[0].payload.is_empty());
    }
}
