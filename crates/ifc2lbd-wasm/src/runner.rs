use std::collections::HashMap;
use std::io::{self};

use serde_json;

use ifc_model::build_model;
use ifc_step::{parse_step_bytes, StepFile};
#[cfg(target_arch = "wasm32")]
use js_sys::Function;
use lbd_converter::{convert_step_and_model, stream_step_and_model, ConvertOptions};
use lbd_pipeline::BatchKind;
use lbd_serializer::{
    serialize_lbd_batches_incremental_to_writer, serialize_nquads_batches_to_writer,
    serialize_nquads_merged_batches_to_writer, serialize_turtle_batch_raw_to_writer,
    serialize_turtle_batch_to_writer, serialize_turtle_grouped_to_writer, serialize_turtle_to_writer,
    write_turtle_prefixes_for_stream,
};

use crate::memory::{
    effective_ifcowl_workers, effective_stream_batch_size, execution_mode_str,
    select_execution_mode,
};
use crate::plugins::browser_registry;
use crate::sink::CountingWriter;
#[cfg(target_arch = "wasm32")]
use crate::sink::{emit_stage_event, SinkChunkWriter, SinkChunkingMode, SinkQuadChunkWriter};
use crate::types::*;
use crate::validation::{
    dedupe_modules, normalize_base_for_graph_iri, parse_module_configs, resolve_execution_settings,
    validate_activation_plan, validate_module_configs, validate_typed_module_configs,
};
use crate::DEFAULT_BASE_URI;
use lbd_pipeline::{
    BEO_PRODUCER_ID, BOT_PRODUCER_ID, BSDD_PRODUCER_ID,
    FILE_EXPORT_ID, IFCOWL_PRODUCER_ID, LOG_EXPORT_ID,
    NQUADS_CHUNKED_SERIALIZER_ID, NQUADS_SERIALIZER_ID, OMG_FOG_PRODUCER_ID,
    PipelineContext, PipelineLogBundle, PipelineStage, PluginRegistry, ResourceLimits,
    PROPS_OPM_PRODUCER_ID, STDOUT_EXPORT_ID, TURTLE_SERIALIZER_ID,
    spawn_preprocessors_with, spawn_producers,
};

/// Returns true if the named-graph IRI belongs to an IfcOWL (or alignment) graph.
fn is_ifcowl_graph(iri: &str) -> bool {
    iri.ends_with("/ifcowl") || iri.ends_with("/alignment")
}

/// Build a `PipelineContext` from the common pipeline inputs.
///
/// This is used by all dispatch sites (turtle/nquads) to create the shared
/// context that `ProducerPlugin::produce()` implementations read from.
fn make_pipeline_context(
    model: std::sync::Arc<ifc_model::IfcModel>,
    options: std::sync::Arc<ConvertOptions>,
    step: std::sync::Arc<StepFile>,
    chan_cap: usize,
) -> PipelineContext {
    let limits = ResourceLimits {
        memory_budget_bytes: 0,
        thread_count: rayon::current_num_threads().max(1),
        channel_capacity: chan_cap,
        batch_size: options.stream_batch_size,
    };
    let mut ctx = PipelineContext::new(limits);
    ctx.insert(model);
    ctx.insert(options);
    ctx.insert(step);
    ctx
}

/// Build a list of active producer IDs from execution settings.
///
/// The topology and bbox producers are intentionally excluded — they are
/// handled by the existing bespoke code paths that precompute geometry.
fn active_producer_ids_from_settings(settings: &ExecutionSettings) -> Vec<String> {
    let mut ids = Vec::new();
    for id in [
        BOT_PRODUCER_ID,
        BEO_PRODUCER_ID,
        BSDD_PRODUCER_ID,
        PROPS_OPM_PRODUCER_ID,
        OMG_FOG_PRODUCER_ID,
        IFCOWL_PRODUCER_ID,
    ] {
        if settings.has(id) {
            ids.push(id.to_string());
        }
    }
    ids
}

/// WASM-safe monotonic timestamp in milliseconds.
#[cfg(target_arch = "wasm32")]
fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// Run preprocess plugins one-by-one, emitting `running`/`success` stage events
/// per plugin (with per-plugin durations) and returning the (id, duration_ms)
/// pairs for `StageDurations::by_preprocess`.
#[cfg(target_arch = "wasm32")]
fn run_preprocess_with_events(
    sink: &js_sys::Function,
    ids: &[String],
    registry: &PluginRegistry,
    ctx: &mut PipelineContext,
) -> Result<Vec<(String, u64)>, lbd_serializer::SerializerError> {
    use std::cell::RefCell;
    use std::collections::HashMap;

    let starts: RefCell<HashMap<String, u64>> = RefCell::new(HashMap::new());
    let durations: RefCell<Vec<(String, u64)>> = RefCell::new(Vec::new());

    spawn_preprocessors_with(
        ids,
        registry,
        ctx,
        |id| {
            starts.borrow_mut().insert(id.to_string(), now_ms());
            let _ = emit_stage_event(sink, id, "Preprocess", "running", 0, 0, 0, None);
        },
        |id| {
            let t0 = starts.borrow_mut().remove(id).unwrap_or_else(now_ms);
            let dur = now_ms().saturating_sub(t0);
            durations.borrow_mut().push((id.to_string(), dur));
            let _ = emit_stage_event(sink, id, "Preprocess", "success", dur, 0, 0, None);
        },
    )
    .map_err(|e| lbd_serializer::SerializerError::Io(std::io::Error::other(e)))?;
    Ok(durations.into_inner())
}

/// Emit an Export stage event for every active export plugin (file/log/stdout).
#[cfg(target_arch = "wasm32")]
fn emit_export_events(
    sink: &js_sys::Function,
    settings: &ExecutionSettings,
    status: &str,
    duration_ms: u64,
) -> Result<(), lbd_serializer::SerializerError> {
    for id in [FILE_EXPORT_ID, LOG_EXPORT_ID, STDOUT_EXPORT_ID] {
        if settings.has(id) {
            emit_stage_event(sink, id, "Export", status, duration_ms, 0, 0, None)?;
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> u64 {
    use std::time::Instant;
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let origin = ORIGIN.get_or_init(Instant::now);
    origin.elapsed().as_millis() as u64
}

#[cfg(target_arch = "wasm32")]
fn effective_turtle_grouping(grouping: TurtleGrouping, options: &ConvertOptions) -> TurtleGrouping {
    if matches!(grouping, TurtleGrouping::Sorted) && options.low_memory_mode {
        TurtleGrouping::Streaming
    } else {
        grouping
    }
}

/// Stage completion event sent from rayon producer threads to main thread.
struct StageDoneEvent {
    plugin_id: &'static str,
    stage: &'static str,
    duration_ms: u64,
    triple_count: u64,
}

/// Per-stage measured durations and triple counts.
struct StageDurations {
    /// Maps preprocess plugin_id → duration_ms
    by_preprocess: Vec<(String, u64)>,
    /// Maps producer plugin_id → (duration_ms, triple_count)
    by_producer: HashMap<String, (u64, u64)>,
    serialize_ms: u64,
    export_ms: u64,
}

impl StageDurations {
    fn new() -> Self {
        Self {
            by_preprocess: Vec::new(),
            by_producer: HashMap::new(),
            serialize_ms: 0,
            export_ms: 0,
        }
    }

    fn total_triples(&self) -> u64 {
        self.by_producer.values().map(|(_, t)| t).sum()
    }
}

// ===========================================================================
// PipelineRunner — unified conversion orchestration
// ===========================================================================

pub(crate) struct PipelineRunner {
    registry: lbd_pipeline::PluginRegistry,
}

impl PipelineRunner {
    pub fn new() -> Self {
        Self {
            registry: browser_registry(),
        }
    }

    /// Resolve + validate + parse + convert (in-memory output).
    pub(crate) fn run_memory(
        &self,
        input: &[u8],
        request: &ConversionRequest,
    ) -> Result<ConversionBundle, WasmApiError> {
        let (plan, settings, mut warnings) = self.resolve_and_validate(request, input.len() as u64)?;
        let base_uri = request
            .base_uri
            .clone()
            .unwrap_or_else(|| DEFAULT_BASE_URI.to_string());

        let (mode, estimated_peak_mb, feasibility_check_mb, reason) =
            select_execution_mode(input.len() as u64, request, &settings);
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
        let options = self.make_convert_options(&base_uri, mode, &settings, request);
        let conversion = convert_step_and_model(&step, &model, &options);
        let exported_files = export_browser_files(&conversion, &step, &model, &options, &base_uri, &settings)
            .map_err(|err| WasmApiError::Serialization(err.to_string()))?;

        let primary_serializer = self.primary_serializer_id(&settings);

        Ok(ConversionBundle {
            resolved_plan: ResolvedPlan {
                enabled_ids: plan.enabled_ids,
                warnings: Vec::new(),
            },
            export: ExportMetadata {
                exporter_id: FILE_EXPORT_ID.to_string(),
                serializer_id: primary_serializer.to_string(),
            },
            exported_files,
            warnings,
        })
    }

    /// Resolve + validate + parse + convert (benchmark/counting output).
    pub(crate) fn run_benchmark(
        &self,
        input: &[u8],
        request: &ConversionRequest,
    ) -> Result<BenchmarkBundle, WasmApiError> {
        let (plan, settings, warnings) = self.resolve_and_validate(request, input.len() as u64)?;
        let base_uri = request
            .base_uri
            .clone()
            .unwrap_or_else(|| DEFAULT_BASE_URI.to_string());

        let (mode, _estimated_peak_mb, _feasibility_check_mb, _reason) =
            select_execution_mode(input.len() as u64, request, &settings);

        let step = parse_step_bytes(input)?;
        let model = build_model(&step)?;
        let options = self.make_convert_options(&base_uri, mode, &settings, request);

        let output_files =
            export_browser_file_summaries_streaming(&step, &model, &options, &base_uri, &settings)
                .map_err(|err| WasmApiError::Serialization(err.to_string()))?;
        let total_output_bytes = output_files.iter().map(|f| f.bytes).sum();
        let primary_serializer = self.primary_serializer_id(&settings);

        Ok(BenchmarkBundle {
            resolved_plan: ResolvedPlan {
                enabled_ids: plan.enabled_ids,
                warnings: Vec::new(),
            },
            export: ExportMetadata {
                exporter_id: FILE_EXPORT_ID.to_string(),
                serializer_id: primary_serializer.to_string(),
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
            stage_telemetry: Vec::new(),
        })
    }

    /// Resolve + validate + parse + convert (streaming to JS sink).
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn run_to_sink(
        &self,
        input: &[u8],
        request: &ConversionRequest,
        sink: &Function,
    ) -> Result<StreamConversionBundle, WasmApiError> {
        let (plan, settings, mut warnings) = self.resolve_and_validate(request, input.len() as u64)?;
        let base_uri = request
            .base_uri
            .clone()
            .unwrap_or_else(|| DEFAULT_BASE_URI.to_string());

        let (mode, estimated_peak_mb, feasibility_check_mb, reason) =
            select_execution_mode(input.len() as u64, request, &settings);
        warnings.push(format!(
            "execution mode={} (estimated_peak_mb={} feasibility_check_mb={}): {}",
            execution_mode_str(mode),
            estimated_peak_mb,
            feasibility_check_mb,
            reason
        ));

        // Parse stage
        emit_stage_event(sink, "parse", "Preprocess", "running", 0, 0, 0, None)
            .map_err(|e| WasmApiError::Serialization(e.to_string()))?;
        let parse_t0 = now_ms();
        let step = parse_step_bytes(input)?;
        let model = build_model(&step)?;
        let parse_ms = now_ms() - parse_t0;
        emit_stage_event(sink, "parse", "Preprocess", "success", parse_ms, 0, 0, None)
            .map_err(|e| WasmApiError::Serialization(e.to_string()))?;

        let options = self.make_convert_options(&base_uri, mode, &settings, request);
        if matches!(settings.turtle_grouping, TurtleGrouping::Sorted) && options.low_memory_mode {
            warnings.push(
                "neo-turtle-serializer.grouping=sorted was downgraded to streaming in WASM because low-memory mode was selected".to_string(),
            );
        }

        // Emit "running" for all active produce stages
        for id in [
            BOT_PRODUCER_ID,
            BEO_PRODUCER_ID,
            BSDD_PRODUCER_ID,
            PROPS_OPM_PRODUCER_ID,
            OMG_FOG_PRODUCER_ID,
            IFCOWL_PRODUCER_ID,
        ] {
            if settings.has(id) {
                emit_stage_event(sink, id, "Produce", "running", 0, 0, 0, None)
                    .map_err(|e| WasmApiError::Serialization(e.to_string()))?;
            }
        }
        let sink_config = SinkConfig::from_request(request);
        let stream_batch_size = options.stream_batch_size;
        let ifcowl_max_workers = options.ifcowl_max_workers;

        let (output_files, sink_max_pending_bytes, sink_chunk_size_bytes, stage_durations) =
            export_browser_files_to_sink_streaming(
                step,
                model,
                options,
                &base_uri,
                &settings,
                sink,
                &sink_config,
            )
            .map_err(|err| WasmApiError::Serialization(err.to_string()))?;

        // Emit completion events for serialize + export
        let total_output_bytes: u64 = output_files.iter().map(|f| f.bytes).sum();
        let primary_serializer = self.primary_serializer_id(&settings);

        emit_stage_event(
            sink,
            primary_serializer,
            "Serialize",
            "success",
            stage_durations.serialize_ms,
            total_output_bytes,
            stage_durations.total_triples(),
            None,
        )
        .map_err(|e| WasmApiError::Serialization(e.to_string()))?;
        emit_export_events(sink, &settings, "success", stage_durations.export_ms)
            .map_err(|e| WasmApiError::Serialization(e.to_string()))?;

        Ok(StreamConversionBundle {
            resolved_plan: ResolvedPlan {
                enabled_ids: plan.enabled_ids,
                warnings: Vec::new(),
            },
            export: ExportMetadata {
                exporter_id: FILE_EXPORT_ID.to_string(),
                serializer_id: primary_serializer.to_string(),
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
            stage_telemetry: {
                let mut tel = vec![StageTelemetry {
                    plugin_id: "parse".to_string(),
                    stage: "Preprocess".to_string(),
                    status: "success".to_string(),
                    duration_ms: parse_ms,
                    bytes_out: 0,
                    triples_out: 0,
                    error: None,
                }];
                for (plugin_id, duration_ms) in &stage_durations.by_preprocess {
                    tel.push(StageTelemetry {
                        plugin_id: plugin_id.clone(),
                        stage: "Preprocess".to_string(),
                        status: "success".to_string(),
                        duration_ms: *duration_ms,
                        bytes_out: 0,
                        triples_out: 0,
                        error: None,
                    });
                }
                for (plugin_id, (duration_ms, triples)) in &stage_durations.by_producer {
                    tel.push(StageTelemetry {
                        plugin_id: plugin_id.clone(),
                        stage: "Produce".to_string(),
                        status: "success".to_string(),
                        duration_ms: *duration_ms,
                        bytes_out: 0,
                        triples_out: *triples,
                        error: None,
                    });
                }
                tel.push(StageTelemetry {
                    plugin_id: primary_serializer.to_string(),
                    stage: "Serialize".to_string(),
                    status: "success".to_string(),
                    duration_ms: stage_durations.serialize_ms,
                    bytes_out: total_output_bytes,
                    triples_out: stage_durations.total_triples(),
                    error: None,
                });
                tel.push(StageTelemetry {
                    plugin_id: FILE_EXPORT_ID.to_string(),
                    stage: "Export".to_string(),
                    status: "success".to_string(),
                    duration_ms: stage_durations.export_ms,
                    bytes_out: 0,
                    triples_out: 0,
                    error: None,
                });
                tel
            },
        })
    }

    fn resolve_and_validate(
        &self,
        request: &ConversionRequest,
        input_size_bytes: u64,
    ) -> Result<(lbd_pipeline::ActivationPlan, ExecutionSettings, Vec<String>), WasmApiError> {
        let requested = dedupe_modules(request.module_ids.clone());
        let plan = self.registry.resolve_activation(&requested)?;
        let configs = parse_module_configs(&request.module_options)?;
        validate_module_configs(&plan, &configs)?;
        validate_typed_module_configs(&configs)?;
        validate_activation_plan(&plan)?;
        let mut warnings = Vec::new();
        let settings = resolve_execution_settings(&plan, &configs, request, &mut warnings, input_size_bytes)?;
        Ok((plan, settings, warnings))
    }

    fn make_convert_options(
        &self,
        base_uri: &str,
        mode: ExecutionMode,
        settings: &ExecutionSettings,
        request: &ConversionRequest,
    ) -> ConvertOptions {
        let stream_batch_size = effective_stream_batch_size(mode, request);
        let ifcowl_max_workers = effective_ifcowl_workers(mode, request);
        ConvertOptions {
            base_uri: base_uri.to_string(),
            emit_ifcowl_links: settings.has(IFCOWL_PRODUCER_ID),
            enable_topology: false,
            enable_topology_extension: false,
            topology_only: false,
            suppress_non_topology_fallback: false,
            geometry_relations: None,
            geometry_bounding_boxes: None, // Computed later from STEP data if bbox is active
            geometry_wkts: None,
            geometry_tolerance: 1e-6,
            low_memory_mode: mode == ExecutionMode::Lowmem,
            stream_batch_size,
            ifcowl_max_workers,
            ifcowl_mode: settings.ifcowl_mode,
            bsdd_profile: settings.bsdd_profile.clone(),
        }
    }

    /// Determine the primary serializer ID for event emissions.
    fn primary_serializer_id(&self, settings: &ExecutionSettings) -> &'static str {
        if settings.output_formats.nquads_chunked {
            NQUADS_CHUNKED_SERIALIZER_ID
        } else if settings.output_formats.nquads {
            NQUADS_SERIALIZER_ID
        } else {
            TURTLE_SERIALIZER_ID
        }
    }
}

// ===========================================================================
// Legacy free functions (delegate to PipelineRunner)
// ===========================================================================

pub(crate) fn resolve_plan_impl(
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

pub(crate) fn convert_ifc_impl(
    input: &[u8],
    request: ConversionRequest,
) -> Result<ConversionBundle, WasmApiError> {
    PipelineRunner::new().run_memory(input, &request)
}

pub(crate) fn benchmark_convert_ifc_impl(
    input: &[u8],
    request: ConversionRequest,
) -> Result<BenchmarkBundle, WasmApiError> {
    PipelineRunner::new().run_benchmark(input, &request)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn convert_ifc_to_sink_impl(
    input: &[u8],
    request: ConversionRequest,
    sink: &Function,
) -> Result<StreamConversionBundle, WasmApiError> {
    PipelineRunner::new().run_to_sink(input, &request, sink)
}

pub(crate) fn requested_settings_for_planning(
    request: &ConversionRequest,
) -> Result<ExecutionSettings, WasmApiError> {
    let registry = browser_registry();
    let requested = dedupe_modules(request.module_ids.clone());
    let plan = registry.resolve_activation(&requested)?;
    let configs = parse_module_configs(&request.module_options)?;
    validate_module_configs(&plan, &configs)?;
    validate_typed_module_configs(&configs)?;
    validate_activation_plan(&plan)?;
    let mut warnings = Vec::new();
    resolve_execution_settings(&plan, &configs, request, &mut warnings, 0)
}

// ===========================================================================
// Streaming export helpers (benchmark mode — counting writers, no JS sink)
// ===========================================================================

fn export_browser_file_summaries_streaming(
    step: &StepFile,
    model: &ifc_model::IfcModel,
    options: &ConvertOptions,
    base_uri: &str,
    settings: &ExecutionSettings,
) -> Result<Vec<OutputFileSummary>, lbd_serializer::SerializerError> {
    let mut summaries = Vec::new();
    if settings.output_formats.turtle {
        summaries.extend(turtle_file_summaries(
            step, model, options, base_uri, settings,
        )?);
    }
    if settings.output_formats.has_any_nquads() {
        summaries.extend(nquads_file_summaries(
            step, model, options, base_uri, settings,
        )?);
    }
    Ok(summaries)
}

fn turtle_file_summaries(
    step: &StepFile,
    model: &ifc_model::IfcModel,
    options: &ConvertOptions,
    _base_uri: &str,
    settings: &ExecutionSettings,
) -> Result<Vec<OutputFileSummary>, lbd_serializer::SerializerError> {
    let chan_cap = if options.low_memory_mode { 4 } else { 16 };
    let (lbd_sender, lbd_receiver) = crossbeam::channel::bounded(chan_cap);

    if settings.has(IFCOWL_PRODUCER_ID) {
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
                let result =
                    stream_step_and_model(step, model, options, &lbd_sender, Some(&ifcowl_sender))
                        .map_err(|_| {
                            lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
                        });
                drop(lbd_sender);
                drop(ifcowl_sender);
                let _ = producer_result_sender.send(result.map(|_| ()));
            });

            let lbd_fwd = merged_sender.clone();
            let lbd_res = forward_result_sender.clone();
            scope.spawn(move |_| {
                let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                    for batch in lbd_receiver {
                        lbd_fwd
                            .send((BatchKind::new("lbd"), batch))
                            .map_err(|_| {
                                lbd_serializer::SerializerError::Io(
                                    io::ErrorKind::BrokenPipe.into(),
                                )
                            })?;
                    }
                    Ok(())
                })();
                let _ = lbd_res.send(result);
            });

            let ifcowl_fwd = merged_sender.clone();
            let ifcowl_res = forward_result_sender.clone();
            scope.spawn(move |_| {
                let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                    for batch in ifcowl_receiver {
                        ifcowl_fwd
                            .send((BatchKind::new("x/ifcowl"), batch))
                            .map_err(|_| {
                                lbd_serializer::SerializerError::Io(
                                    io::ErrorKind::BrokenPipe.into(),
                                )
                            })?;
                    }
                    Ok(())
                })();
                let _ = ifcowl_res.send(result);
            });

            drop(merged_sender);
            drop(forward_result_sender);

            for (kind, batch) in merged_receiver {
                let writer = if is_ifcowl_graph(kind.iri()) {
                    &mut ifcowl_count
                } else {
                    &mut lbd_count
                };
                if options.low_memory_mode {
                    serialize_turtle_batch_raw_to_writer(&batch, writer)?
                } else {
                    serialize_turtle_batch_to_writer(&batch, writer, Some(&options.base_uri))?
                }
            }

            producer_result_receiver.recv().map_err(|_| {
                lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
            })??;
            for _ in 0..2 {
                forward_result_receiver.recv().map_err(|_| {
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
                    .map_err(|_| {
                        lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
                    });
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

            producer_result_receiver.recv().map_err(|_| {
                lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
            })??;
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

fn nquads_file_summaries(
    step: &StepFile,
    model: &ifc_model::IfcModel,
    options: &ConvertOptions,
    base_uri: &str,
    settings: &ExecutionSettings,
) -> Result<Vec<OutputFileSummary>, lbd_serializer::SerializerError> {
    let normalized_base = normalize_base_for_graph_iri(base_uri);
    let lbd_graph = format!("{normalized_base}/lbd");
    let ifcowl_graph = format!("{normalized_base}/ifcowl");

    let (lbd_sender, lbd_receiver) = crossbeam::channel::bounded(8);
    let (ifcowl_sender, ifcowl_receiver) = crossbeam::channel::bounded(8);
    let (consumer_result_sender, consumer_result_receiver) =
        crossbeam::channel::bounded::<Result<u64, lbd_serializer::SerializerError>>(1);
    let emit_ifcowl = settings.has(IFCOWL_PRODUCER_ID);
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

    let producer_result = if settings.has(IFCOWL_PRODUCER_ID) {
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

// ===========================================================================
// Sink config & streaming export (JS sink — INCREMENTAL, no full collect)
// ===========================================================================

/// Configuration for the JS sink writer.
pub(crate) struct SinkConfig {
    pub chunk_size: usize,
    pub max_pending_bytes: usize,
}

impl SinkConfig {
    pub fn from_request(request: &ConversionRequest) -> Self {
        let chunk_size = request
            .sink_chunk_size_bytes
            .unwrap_or(1024 * 1024)
            .max(64 * 1024);
        let max_pending_bytes = request
            .sink_max_pending_bytes
            .unwrap_or(chunk_size * 4)
            .max(chunk_size);
        Self {
            chunk_size,
            max_pending_bytes,
        }
    }
}

/// Streaming export to JS sink. Never collects all data into memory.
/// Producers stream batches through channels; serializers consume incrementally.
#[cfg(target_arch = "wasm32")]
fn export_browser_files_to_sink_streaming(
    step: StepFile,
    model: ifc_model::IfcModel,
    options: ConvertOptions,
    base_uri: &str,
    settings: &ExecutionSettings,
    sink: &Function,
    sink_config: &SinkConfig,
) -> Result<(Vec<OutputFileSummary>, usize, usize, StageDurations), lbd_serializer::SerializerError>
{
    // Stage event channel for async producer completion
    let (stage_tx, stage_rx) = crossbeam::channel::unbounded::<StageDoneEvent>();

    let is_chunked = settings.nquads.chunking != NquadsChunkingMode::None;
    let chunk_mode = match settings.nquads.chunking {
        NquadsChunkingMode::None => SinkChunkingMode::None,
        NquadsChunkingMode::Lines => SinkChunkingMode::Lines,
        NquadsChunkingMode::Bytes => SinkChunkingMode::Bytes,
    };

    // --- TURTLE PATH ---
    if settings.output_formats.turtle {
        return turtle_to_sink(
            step,
            model,
            options,
            settings,
            sink,
            sink_config,
            &stage_tx,
            &stage_rx,
        );
    }

    // --- N-QUADS PATH (merged or chunked) ---
    if settings.output_formats.has_any_nquads() {
        return nquads_to_sink(
            step,
            model,
            options,
            base_uri,
            settings,
            sink,
            sink_config,
            &stage_tx,
            &stage_rx,
            is_chunked,
            chunk_mode,
        );
    }

    Err(lbd_serializer::SerializerError::Io(io::Error::new(
        io::ErrorKind::InvalidInput,
        "no serializer active",
    )))
}

// ---------------------------------------------------------------------------
// Turtle streaming to sink
// ---------------------------------------------------------------------------
#[cfg(target_arch = "wasm32")]
fn turtle_to_sink(
    step: StepFile,
    model: ifc_model::IfcModel,
    options: ConvertOptions,
    settings: &ExecutionSettings,
    sink: &Function,
    sink_config: &SinkConfig,
    stage_tx: &crossbeam::channel::Sender<StageDoneEvent>,
    stage_rx: &crossbeam::channel::Receiver<StageDoneEvent>,
) -> Result<(Vec<OutputFileSummary>, usize, usize, StageDurations), lbd_serializer::SerializerError>
{
    if settings.turtle_layout == TurtleLayout::Joined {
        return turtle_to_sink_joined(
            step,
            model,
            options,
            settings,
            sink,
            sink_config,
            stage_tx,
            stage_rx,
        );
    }
    if settings.turtle_layout == TurtleLayout::Separate {
        return turtle_to_sink_separate(
            step,
            model,
            options,
            settings,
            sink,
            sink_config,
            stage_tx,
            stage_rx,
        );
    }

    // TurtleLayout has only Joined and Separate variants; both are handled above.
    unreachable!("all TurtleLayout variants handled")
}

/// Convert a `Receiver<TaggedBatch>` into a raw-triples receiver by spawning
/// a lightweight rayon forwarder task that strips the graph-IRI tag.
///
/// Used to bridge `spawn_producers` output back to the existing serializer
/// helpers that consume `Receiver<Vec<Triple>>`.
#[cfg(target_arch = "wasm32")]
fn to_raw_receiver(
    rx: crossbeam::channel::Receiver<lbd_pipeline::TaggedBatch>,
    cap: usize,
) -> crossbeam::channel::Receiver<Vec<lbd_ontology::Triple>> {
    let (tx, raw_rx) = crossbeam::channel::bounded(cap);
    rayon::spawn(move || {
        for batch in rx {
            if tx.send(batch.triples).is_err() {
                break;
            }
        }
    });
    raw_rx
}

#[cfg(target_arch = "wasm32")]
fn serialize_turtle_receiver_to_file(
    rx: crossbeam::channel::Receiver<Vec<lbd_ontology::Triple>>,
    filename: String,
    role: &'static str,
    sink: &Function,
    sink_config: &SinkConfig,
    options: &ConvertOptions,
    grouping: TurtleGrouping,
    instance_base: &str,
) -> Result<(OutputFileSummary, u64), lbd_serializer::SerializerError> {
    let mut writer = SinkChunkWriter::new(
        sink,
        filename,
        "text/turtle;charset=utf-8",
        role,
        sink_config.chunk_size,
        sink_config.max_pending_bytes,
    )?;
    let grouping = effective_turtle_grouping(grouping, options);
    let grouped = matches!(grouping, TurtleGrouping::Sorted);
    if !grouped && !options.low_memory_mode {
        write_turtle_prefixes_for_stream(&mut writer, Some(instance_base))?;
    }
    let mut triple_count: u64 = 0;
    if grouped {
        let mut all = Vec::new();
        for mut batch in rx {
            triple_count += batch.len() as u64;
            all.append(&mut batch);
        }
        serialize_turtle_grouped_to_writer(&all, &mut writer, Some(instance_base))?;
    } else {
        for batch in rx {
            triple_count += batch.len() as u64;
            if options.low_memory_mode {
                serialize_turtle_batch_raw_to_writer(&batch, &mut writer)?;
            } else {
                serialize_turtle_batch_to_writer(&batch, &mut writer, Some(instance_base))?;
            }
        }
    }
    let (summary, _, _) = writer.finish()?;
    Ok((summary, triple_count))
}

#[cfg(target_arch = "wasm32")]
fn serialize_turtle_receiver_to_writer(
    rx: crossbeam::channel::Receiver<Vec<lbd_ontology::Triple>>,
    writer: &mut SinkChunkWriter,
    options: &ConvertOptions,
    grouping: TurtleGrouping,
    instance_base: &str,
) -> Result<u64, lbd_serializer::SerializerError> {
    let grouping = effective_turtle_grouping(grouping, options);
    let grouped = matches!(grouping, TurtleGrouping::Sorted);
    let mut triple_count: u64 = 0;
    if grouped {
        let mut all = Vec::new();
        for mut batch in rx {
            triple_count += batch.len() as u64;
            all.append(&mut batch);
        }
        serialize_turtle_grouped_to_writer(&all, &mut *writer, Some(instance_base))?;
    } else {
        for batch in rx {
            triple_count += batch.len() as u64;
            if options.low_memory_mode {
                serialize_turtle_batch_raw_to_writer(&batch, &mut *writer)?;
            } else {
                serialize_turtle_batch_to_writer(&batch, &mut *writer, Some(instance_base))?;
            }
        }
    }
    Ok(triple_count)
}

#[cfg(target_arch = "wasm32")]
fn collect_turtle_receiver(
    rx: crossbeam::channel::Receiver<Vec<lbd_ontology::Triple>>,
) -> (Vec<lbd_ontology::Triple>, u64) {
    let mut all = Vec::new();
    let mut triple_count: u64 = 0;
    for mut batch in rx {
        triple_count += batch.len() as u64;
        all.append(&mut batch);
    }
    (all, triple_count)
}

#[cfg(target_arch = "wasm32")]
fn turtle_to_sink_joined(
    step: StepFile,
    model: ifc_model::IfcModel,
    options: ConvertOptions,
    settings: &ExecutionSettings,
    sink: &Function,
    sink_config: &SinkConfig,
    _stage_tx: &crossbeam::channel::Sender<StageDoneEvent>,
    _stage_rx: &crossbeam::channel::Receiver<StageDoneEvent>,
) -> Result<(Vec<OutputFileSummary>, usize, usize, StageDurations), lbd_serializer::SerializerError>
{
    let chan_cap = if options.low_memory_mode { 4 } else { 16 };
    let instance_base = options.base_uri.clone();
    let model = std::sync::Arc::new(model);
    let mut produce_durations: HashMap<&'static str, u64> = HashMap::new();
    let mut produce_triples: HashMap<&'static str, u64> = HashMap::new();

    // -----------------------------------------------------------------------
    // Dispatch LBD + IfcOWL producers via ProducerPlugin trait (spawn_producers).
    // Topology remains bespoke (requires precomputed geometry from ExecutionSettings).
    // -----------------------------------------------------------------------
    let options_arc = std::sync::Arc::new(options.clone());
    let step_arc = std::sync::Arc::new(step);
    let registry = crate::plugins::browser_registry();
    let mut ctx = make_pipeline_context(
        model.clone(),
        options_arc.clone(),
        step_arc.clone(),
        chan_cap,
    );
    let preprocess_ids: Vec<String> = registry
        .manifests_for_stage(PipelineStage::Preprocess)
        .into_iter()
        .filter(|m| settings.has(m.id))
        .map(|m| m.id.to_string())
        .collect();
    let preprocess_durations =
        run_preprocess_with_events(sink, &preprocess_ids, &registry, &mut ctx)?;
    let ctx = std::sync::Arc::new(ctx);

    // The context uses options without topology-disable (topology runs separately via
    // bespoke path). The ProducerPlugin::produce() implementations for BOT/BEO/PROPS/OMG
    // only use base_uri and do not check enable_topology, so passing options as-is is fine.
    let producer_ids = active_producer_ids_from_settings(settings);
    let mut raw_receivers: std::collections::HashMap<String, _> = {
        let mut map = std::collections::HashMap::new();
        for (id, rx) in spawn_producers(&producer_ids, &registry, &ctx, chan_cap) {
            map.insert(id, to_raw_receiver(rx, chan_cap));
        }
        map
    };

    let bot_receiver = raw_receivers.remove(BOT_PRODUCER_ID);
    let beo_receiver = raw_receivers.remove(BEO_PRODUCER_ID);
    let bsdd_receiver = raw_receivers.remove(BSDD_PRODUCER_ID);
    let props_receiver = raw_receivers.remove(PROPS_OPM_PRODUCER_ID);
    let omg_receiver = raw_receivers.remove(OMG_FOG_PRODUCER_ID);
    let ifcowl_receiver = raw_receivers.remove(IFCOWL_PRODUCER_ID);

    let mut writer = SinkChunkWriter::new(
        sink,
        format!("{}.ttl", settings.output_stem),
        "text/turtle;charset=utf-8",
        "joined",
        sink_config.chunk_size,
        sink_config.max_pending_bytes,
    )?;
    let effective_grouping = effective_turtle_grouping(settings.turtle_grouping, &options);
    if !options.low_memory_mode && !matches!(effective_grouping, TurtleGrouping::Sorted) {
        write_turtle_prefixes_for_stream(&mut writer, Some(&instance_base))?;
    }
    emit_stage_event(sink, TURTLE_SERIALIZER_ID, "Serialize", "running", 0, 0, 0, None)?;
    let serialize_t0 = now_ms();
    if matches!(effective_grouping, TurtleGrouping::Sorted) {
        let mut all_triples: Vec<lbd_ontology::Triple> = Vec::new();
        macro_rules! collect_and_emit {
            ($rx_opt:expr, $id:expr) => {
                if let Some(rx) = $rx_opt {
                    let t0 = now_ms();
                    let (triples, count) = collect_turtle_receiver(rx);
                    let ms = now_ms() - t0;
                    produce_triples.insert($id, count);
                    produce_durations.insert($id, ms);
                    emit_stage_event(sink, $id, "Produce", "success", ms, 0, count, None)?;
                    all_triples.extend(triples);
                }
            };
        }
        collect_and_emit!(bot_receiver, BOT_PRODUCER_ID);
        collect_and_emit!(beo_receiver, BEO_PRODUCER_ID);
        collect_and_emit!(bsdd_receiver, BSDD_PRODUCER_ID);
        collect_and_emit!(props_receiver, PROPS_OPM_PRODUCER_ID);
        collect_and_emit!(omg_receiver, OMG_FOG_PRODUCER_ID);
        // Write sorted LBD triples first (BOT/BEO/PROPS/OMG — bounded in size).
        serialize_turtle_grouped_to_writer(&all_triples, &mut writer, Some(&instance_base))?;
        drop(all_triples);
        // Stream IfcOWL batch-by-batch to avoid collecting potentially GBs into memory.
        if let Some(rx) = ifcowl_receiver {
            let t0 = now_ms();
            let mut count: u64 = 0;
            for batch in rx {
                count += batch.len() as u64;
                serialize_turtle_batch_raw_to_writer(&batch, &mut writer)?;
            }
            let ms = now_ms() - t0;
            produce_triples.insert(IFCOWL_PRODUCER_ID, count);
            produce_durations.insert(IFCOWL_PRODUCER_ID, ms);
            emit_stage_event(sink, IFCOWL_PRODUCER_ID, "Produce", "success", ms, 0, count, None)?;
        }
    } else {
        macro_rules! drain_and_emit {
            ($rx_opt:expr, $id:expr) => {
                if let Some(rx) = $rx_opt {
                    let t0 = now_ms();
                    let count = serialize_turtle_receiver_to_writer(rx, &mut writer, &options, effective_grouping, &instance_base)?;
                    let ms = now_ms() - t0;
                    produce_triples.insert($id, count);
                    produce_durations.insert($id, ms);
                    emit_stage_event(sink, $id, "Produce", "success", ms, 0, count, None)?;
                }
            };
        }
        drain_and_emit!(bot_receiver, BOT_PRODUCER_ID);
        drain_and_emit!(beo_receiver, BEO_PRODUCER_ID);
        drain_and_emit!(bsdd_receiver, BSDD_PRODUCER_ID);
        drain_and_emit!(props_receiver, PROPS_OPM_PRODUCER_ID);
        drain_and_emit!(omg_receiver, OMG_FOG_PRODUCER_ID);
        drain_and_emit!(ifcowl_receiver, IFCOWL_PRODUCER_ID);
    }
    let serialize_ms = now_ms() - serialize_t0;

    emit_export_events(sink, settings, "running", 0)?;
    let export_t0 = now_ms();
    let (summary, peak, chunk_size) = writer.finish()?;
    let export_ms = now_ms() - export_t0;

    let mut summaries = vec![summary];
    if settings.has(LOG_EXPORT_ID) {
        emit_log_sidecar(sink, &ctx, sink_config, &mut summaries)?;
    }
    let mut stage_durations = StageDurations::new();
    stage_durations.serialize_ms = serialize_ms;
    stage_durations.export_ms = export_ms;
    stage_durations.by_preprocess = preprocess_durations.clone();
    for (plugin_id, ms) in produce_durations {
        let triples = produce_triples.get(plugin_id).copied().unwrap_or(0);
        stage_durations.by_producer.insert(plugin_id.to_string(), (ms, triples));
    }
    Ok((summaries, peak, chunk_size, stage_durations))
}

#[cfg(target_arch = "wasm32")]
fn turtle_to_sink_separate(
    step: StepFile,
    model: ifc_model::IfcModel,
    options: ConvertOptions,
    settings: &ExecutionSettings,
    sink: &Function,
    sink_config: &SinkConfig,
    _stage_tx: &crossbeam::channel::Sender<StageDoneEvent>,
    _stage_rx: &crossbeam::channel::Receiver<StageDoneEvent>,
) -> Result<(Vec<OutputFileSummary>, usize, usize, StageDurations), lbd_serializer::SerializerError>
{
    let chan_cap = if options.low_memory_mode { 4 } else { 16 };
    let instance_base = options.base_uri.clone();
    let model = std::sync::Arc::new(model);
    let mut produce_durations: HashMap<&'static str, u64> = HashMap::new();
    let mut produce_triples: HashMap<&'static str, u64> = HashMap::new();
    let mut summaries = Vec::new();

    // -----------------------------------------------------------------------
    // Dispatch LBD + IfcOWL producers via ProducerPlugin trait (spawn_producers).
    // Topology remains bespoke (requires precomputed geometry from ExecutionSettings).
    // -----------------------------------------------------------------------
    let options_arc = std::sync::Arc::new(options.clone());
    let step_arc = std::sync::Arc::new(step);
    let registry = crate::plugins::browser_registry();
    let mut ctx = make_pipeline_context(
        model.clone(),
        options_arc.clone(),
        step_arc.clone(),
        chan_cap,
    );
    let preprocess_ids: Vec<String> = registry
        .manifests_for_stage(PipelineStage::Preprocess)
        .into_iter()
        .filter(|m| settings.has(m.id))
        .map(|m| m.id.to_string())
        .collect();
    let preprocess_durations =
        run_preprocess_with_events(sink, &preprocess_ids, &registry, &mut ctx)?;
    let ctx = std::sync::Arc::new(ctx);

    let producer_ids = active_producer_ids_from_settings(settings);
    let mut raw_receivers: std::collections::HashMap<String, _> = {
        let mut map = std::collections::HashMap::new();
        for (id, rx) in spawn_producers(&producer_ids, &registry, &ctx, chan_cap) {
            map.insert(id, to_raw_receiver(rx, chan_cap));
        }
        map
    };

    let bot_receiver = raw_receivers.remove(BOT_PRODUCER_ID);
    let beo_receiver = raw_receivers.remove(BEO_PRODUCER_ID);
    let bsdd_receiver = raw_receivers.remove(BSDD_PRODUCER_ID);
    let props_receiver = raw_receivers.remove(PROPS_OPM_PRODUCER_ID);
    let omg_receiver = raw_receivers.remove(OMG_FOG_PRODUCER_ID);
    let ifcowl_receiver = raw_receivers.remove(IFCOWL_PRODUCER_ID);

    emit_stage_event(
        sink,
        TURTLE_SERIALIZER_ID,
        "Serialize",
        "running",
        0,
        0,
        0,
        None,
    )?;
    let serialize_t0 = now_ms();

    macro_rules! drain_sep_and_emit {
        ($rx_opt:expr, $slug:literal, $id:expr, $grouping:expr) => {
            if let Some(rx) = $rx_opt {
                let t0 = now_ms();
                let (summary, triples) = serialize_turtle_receiver_to_file(
                    rx,
                    format!("{}_{}.ttl", settings.output_stem, $slug),
                    $slug,
                    sink,
                    sink_config,
                    &options,
                    $grouping,
                    &instance_base,
                )?;
                let ms = now_ms() - t0;
                produce_triples.insert($id, triples);
                produce_durations.insert($id, ms);
                emit_stage_event(sink, $id, "Produce", "success", ms, 0, triples, None)?;
                summaries.push(summary);
            }
        };
    }
    let effective_grouping = effective_turtle_grouping(settings.turtle_grouping, &options);
    drain_sep_and_emit!(bot_receiver, "bot", BOT_PRODUCER_ID, effective_grouping);
    drain_sep_and_emit!(beo_receiver, "beo", BEO_PRODUCER_ID, effective_grouping);
    drain_sep_and_emit!(bsdd_receiver, "bsdd", BSDD_PRODUCER_ID, effective_grouping);
    drain_sep_and_emit!(props_receiver, "props", PROPS_OPM_PRODUCER_ID, effective_grouping);
    drain_sep_and_emit!(omg_receiver, "omg", OMG_FOG_PRODUCER_ID, effective_grouping);
    // IfcOWL: always stream — sorted/grouped buffering 2.3M+ triples OOMs WASM.
    drain_sep_and_emit!(ifcowl_receiver, "ifcowl", IFCOWL_PRODUCER_ID, TurtleGrouping::Streaming);
    let serialize_ms = now_ms() - serialize_t0;

    emit_export_events(sink, settings, "running", 0)?;
    let export_t0 = now_ms();
    let peak = sink_config.max_pending_bytes;
    let chunk_size = sink_config.chunk_size;
    let export_ms = now_ms() - export_t0;

    if settings.has(LOG_EXPORT_ID) {
        emit_log_sidecar(sink, &ctx, sink_config, &mut summaries)?;
    }
    let mut stage_durations = StageDurations::new();
    stage_durations.serialize_ms = serialize_ms;
    stage_durations.export_ms = export_ms;
    stage_durations.by_preprocess = preprocess_durations.clone();
    for (plugin_id, ms) in produce_durations {
        let triples = produce_triples.get(plugin_id).copied().unwrap_or(0);
        stage_durations
            .by_producer
            .insert(plugin_id.to_string(), (ms, triples));
    }

    Ok((summaries, peak, chunk_size, stage_durations))
}

// ---------------------------------------------------------------------------
// N-Quads streaming helpers
// ---------------------------------------------------------------------------

/// Drain a triple receiver into an existing SinkChunkWriter, tagging every
/// triple with `graph_iri`. Returns the number of triples written.
#[cfg(target_arch = "wasm32")]
fn serialize_nquads_receiver_to_writer(
    rx: crossbeam::channel::Receiver<Vec<lbd_ontology::Triple>>,
    writer: &mut SinkChunkWriter,
    graph_iri: &str,
) -> Result<u64, lbd_serializer::SerializerError> {
    let mut triple_count: u64 = 0;
    for batch in rx {
        triple_count += batch.len() as u64;
        lbd_serializer::write_nquads_batch(writer, &batch, graph_iri)?;
    }
    Ok(triple_count)
}

/// Drain a triple receiver into a SinkQuadChunkWriter, tagging every triple
/// with `graph_iri`. Returns chunk file summaries and the triple count.
#[cfg(target_arch = "wasm32")]
fn serialize_nquads_receiver_to_chunks(
    rx: crossbeam::channel::Receiver<Vec<lbd_ontology::Triple>>,
    sink: &Function,
    chunk_prefix: String,
    graph_iri: &str,
    chunk_mode: SinkChunkingMode,
    chunk_size_lines: usize,
    chunk_size_bytes: usize,
    sink_config: &SinkConfig,
) -> Result<(Vec<OutputFileSummary>, u64), lbd_serializer::SerializerError> {
    let mut chunk_writer = SinkQuadChunkWriter::new(
        sink,
        chunk_prefix,
        chunk_mode,
        chunk_size_lines,
        chunk_size_bytes,
        sink_config.chunk_size,
        sink_config.max_pending_bytes,
    )?;
    let mut triple_count: u64 = 0;
    for batch in rx {
        triple_count += batch.len() as u64;
        lbd_serializer::write_nquads_batch(&mut chunk_writer, &batch, graph_iri)?;
    }
    let summaries = chunk_writer.finish()?;
    Ok((summaries, triple_count))
}

// ---------------------------------------------------------------------------
// N-Quads streaming to sink (per-producer named graphs, mirroring turtle_to_sink_separate)
// ---------------------------------------------------------------------------
#[cfg(target_arch = "wasm32")]
fn nquads_to_sink(
    step: StepFile,
    model: ifc_model::IfcModel,
    options: ConvertOptions,
    base_uri: &str,
    settings: &ExecutionSettings,
    sink: &Function,
    sink_config: &SinkConfig,
    _stage_tx: &crossbeam::channel::Sender<StageDoneEvent>,
    _stage_rx: &crossbeam::channel::Receiver<StageDoneEvent>,
    is_chunked: bool,
    chunk_mode: SinkChunkingMode,
) -> Result<(Vec<OutputFileSummary>, usize, usize, StageDurations), lbd_serializer::SerializerError>
{
    let normalized_base = normalize_base_for_graph_iri(base_uri);
    let chan_cap = if options.low_memory_mode { 4 } else { 16 };
    let model = std::sync::Arc::new(model);
    let mut produce_durations: HashMap<&'static str, u64> = HashMap::new();
    let mut produce_triples: HashMap<&'static str, u64> = HashMap::new();
    let mut summaries = Vec::new();

    let chunk_prefix = settings.nquads.chunk_prefix.clone();
    let chunk_size_lines = settings.nquads.chunk_size_lines;
    let chunk_size_bytes = settings.nquads.chunk_size_bytes;

    // -----------------------------------------------------------------------
    // Dispatch LBD + IfcOWL producers via ProducerPlugin trait (spawn_producers).
    // Topology remains bespoke (requires precomputed geometry from ExecutionSettings).
    // -----------------------------------------------------------------------
    let options_arc = std::sync::Arc::new(options.clone());
    let step_arc = std::sync::Arc::new(step);
    let registry = crate::plugins::browser_registry();
    let mut ctx = make_pipeline_context(
        model.clone(),
        options_arc.clone(),
        step_arc.clone(),
        chan_cap,
    );
    let preprocess_ids: Vec<String> = registry
        .manifests_for_stage(PipelineStage::Preprocess)
        .into_iter()
        .filter(|m| settings.has(m.id))
        .map(|m| m.id.to_string())
        .collect();
    let preprocess_durations =
        run_preprocess_with_events(sink, &preprocess_ids, &registry, &mut ctx)?;
    let ctx = std::sync::Arc::new(ctx);

    let producer_ids = active_producer_ids_from_settings(settings);
    let mut raw_receivers: std::collections::HashMap<String, _> = {
        let mut map = std::collections::HashMap::new();
        for (id, rx) in spawn_producers(&producer_ids, &registry, &ctx, chan_cap) {
            map.insert(id, to_raw_receiver(rx, chan_cap));
        }
        map
    };

    let bot_receiver = raw_receivers.remove(BOT_PRODUCER_ID);
    let beo_receiver = raw_receivers.remove(BEO_PRODUCER_ID);
    let bsdd_receiver = raw_receivers.remove(BSDD_PRODUCER_ID);
    let props_receiver = raw_receivers.remove(PROPS_OPM_PRODUCER_ID);
    let omg_receiver = raw_receivers.remove(OMG_FOG_PRODUCER_ID);
    let ifcowl_receiver = raw_receivers.remove(IFCOWL_PRODUCER_ID);

    let serializer_id = if settings.output_formats.nquads_chunked {
        NQUADS_CHUNKED_SERIALIZER_ID
    } else {
        NQUADS_SERIALIZER_ID
    };
    emit_stage_event(sink, serializer_id, "Serialize", "running", 0, 0, 0, None)?;
    let serialize_t0 = now_ms();

    if is_chunked {
        // Chunked: each active producer → its own set of chunk files
        macro_rules! drain_chunked {
            ($rx_opt:expr, $slug:literal, $producer_id:expr) => {
                if let Some(rx) = $rx_opt {
                    let t0 = now_ms();
                    let (file_summaries, triples) = serialize_nquads_receiver_to_chunks(
                        rx,
                        sink,
                        format!("{}-{}", chunk_prefix, $slug),
                        &format!("{}/{}", normalized_base, $slug),
                        chunk_mode,
                        chunk_size_lines,
                        chunk_size_bytes,
                        sink_config,
                    )?;
                    let ms = now_ms() - t0;
                    produce_triples.insert($producer_id, triples);
                    produce_durations.insert($producer_id, ms);
                    emit_stage_event(sink, $producer_id, "Produce", "success", ms, 0, triples, None)?;
                    summaries.extend(file_summaries);
                }
            };
        }
        drain_chunked!(bot_receiver, "bot", BOT_PRODUCER_ID);
        drain_chunked!(beo_receiver, "beo", BEO_PRODUCER_ID);
        drain_chunked!(bsdd_receiver, "bsdd", BSDD_PRODUCER_ID);
        drain_chunked!(props_receiver, "props", PROPS_OPM_PRODUCER_ID);
        drain_chunked!(omg_receiver, "omg", OMG_FOG_PRODUCER_ID);
        drain_chunked!(ifcowl_receiver, "ifcowl", IFCOWL_PRODUCER_ID);
        let serialize_ms = now_ms() - serialize_t0;

        emit_export_events(sink, settings, "running", 0)?;
        let export_t0 = now_ms();
        let export_ms = now_ms() - export_t0;

        if settings.has(LOG_EXPORT_ID) {
            emit_log_sidecar(sink, &ctx, sink_config, &mut summaries)?;
        }
        let mut sd = StageDurations::new();
        sd.serialize_ms = serialize_ms;
        sd.export_ms = export_ms;
        sd.by_preprocess = preprocess_durations.clone();
        for (plugin_id, ms) in produce_durations {
            let triples = produce_triples.get(plugin_id).copied().unwrap_or(0);
            sd.by_producer.insert(plugin_id.to_string(), (ms, triples));
        }
        Ok((summaries, 0, sink_config.chunk_size, sd))
    } else {
        // Merged: all active producers → one .nq file, each triple tagged with its producer's graph IRI
        let mut writer = SinkChunkWriter::new(
            sink,
            format!("{}.nq", settings.output_stem),
            "application/n-quads",
            "merged",
            sink_config.chunk_size,
            sink_config.max_pending_bytes,
        )?;
        macro_rules! drain_merged {
            ($rx_opt:expr, $slug:literal, $producer_id:expr) => {
                if let Some(rx) = $rx_opt {
                    let t0 = now_ms();
                    let triples = serialize_nquads_receiver_to_writer(
                        rx,
                        &mut writer,
                        &format!("{}/{}", normalized_base, $slug),
                    )?;
                    let ms = now_ms() - t0;
                    produce_triples.insert($producer_id, triples);
                    produce_durations.insert($producer_id, ms);
                    emit_stage_event(sink, $producer_id, "Produce", "success", ms, 0, triples, None)?;
                }
            };
        }
        drain_merged!(bot_receiver, "bot", BOT_PRODUCER_ID);
        drain_merged!(beo_receiver, "beo", BEO_PRODUCER_ID);
        drain_merged!(bsdd_receiver, "bsdd", BSDD_PRODUCER_ID);
        drain_merged!(props_receiver, "props", PROPS_OPM_PRODUCER_ID);
        drain_merged!(omg_receiver, "omg", OMG_FOG_PRODUCER_ID);
        drain_merged!(ifcowl_receiver, "ifcowl", IFCOWL_PRODUCER_ID);
        let serialize_ms = now_ms() - serialize_t0;

        emit_export_events(sink, settings, "running", 0)?;
        let export_t0 = now_ms();
        let (summary, peak, chunk_size) = writer.finish()?;
        let export_ms = now_ms() - export_t0;
        summaries.push(summary);

        if settings.has(LOG_EXPORT_ID) {
            emit_log_sidecar(sink, &ctx, sink_config, &mut summaries)?;
        }
        let mut sd = StageDurations::new();
        sd.serialize_ms = serialize_ms;
        sd.export_ms = export_ms;
        sd.by_preprocess = preprocess_durations.clone();
        for (plugin_id, ms) in produce_durations {
            let triples = produce_triples.get(plugin_id).copied().unwrap_or(0);
            sd.by_producer.insert(plugin_id.to_string(), (ms, triples));
        }
        Ok((summaries, peak, chunk_size, sd))
    }
}

/// Serialize per-module log JSON sidecars from `PipelineLogBundle` in context.
/// Writes one `{module_id}.log.json` file per module through the JS sink.
#[cfg(target_arch = "wasm32")]
fn emit_log_sidecar(
    sink: &js_sys::Function,
    ctx: &std::sync::Arc<PipelineContext>,
    sink_config: &SinkConfig,
    summaries: &mut Vec<OutputFileSummary>,
) -> Result<(), lbd_serializer::SerializerError> {
    use std::io::Write as _;
    let bundle = match ctx.get::<PipelineLogBundle>() {
        Some(b) => (*b).clone(),
        None => return Ok(()),
    };
    if bundle.modules.is_empty() {
        return Ok(());
    }
    let mut module_ids: Vec<&str> = bundle.modules.keys().map(String::as_str).collect();
    module_ids.sort_unstable();
    for module_id in module_ids {
        let stats = &bundle.modules[module_id];
        let json = serde_json::to_vec_pretty(stats).unwrap_or_default();
        if json.is_empty() {
            continue;
        }
        let filename = format!("{module_id}.log.json");
        let mut w = SinkChunkWriter::new(
            sink,
            filename,
            "application/json",
            "log",
            json.len().max(sink_config.chunk_size),
            json.len().max(sink_config.chunk_size),
        )?;
        w.write_all(&json).map_err(lbd_serializer::SerializerError::Io)?;
        let (summary, _, _) = w.finish()?;
        summaries.push(summary);
    }
    Ok(())
}

// ===========================================================================
// In-memory export (non-streaming, used by run_memory)
// ===========================================================================

fn export_browser_files(
    conversion: &lbd_converter::ConversionResult,
    step: &StepFile,
    model: &ifc_model::IfcModel,
    options: &ConvertOptions,
    base_uri: &str,
    settings: &ExecutionSettings,
) -> Result<Vec<ExportedFile>, lbd_serializer::SerializerError> {
    let mut files = Vec::new();

    if settings.output_formats.turtle {
        let mut lbd_bytes: Vec<u8> = Vec::new();
        serialize_turtle_to_writer(&conversion.triples, &mut lbd_bytes)?;
        files.push(ExportedFile {
            filename: format!("{}.ttl", settings.output_stem),
            mime_type: "text/turtle;charset=utf-8".to_string(),
            role: "lbd".to_string(),
            payload: lbd_bytes,
        });
        if settings.has(IFCOWL_PRODUCER_ID) {
            let mut ifcowl_bytes: Vec<u8> = Vec::new();
            serialize_turtle_to_writer(&conversion.ifcowl_triples, &mut ifcowl_bytes)?;
            files.push(ExportedFile {
                filename: format!("{}_ifcowl.ttl", settings.output_stem),
                mime_type: "text/turtle;charset=utf-8".to_string(),
                role: "ifcowl".to_string(),
                payload: ifcowl_bytes,
            });
        }
    }

    if settings.output_formats.has_any_nquads() {
        let normalized_base = normalize_base_for_graph_iri(base_uri);
        let base = lbd_converter::normalize_base_uri(base_uri);
        let mut nq_bytes: Vec<u8> = Vec::new();

        // Collect triples per producer using unbounded channels (single-threaded, no blocking).
        // Sort each producer's triples by subject then predicate for deterministic output.
        macro_rules! collect_producer {
            ($emit:expr, $stream_fn:expr) => {{
                if $emit {
                    let (tx, rx) = crossbeam::channel::unbounded::<Vec<lbd_ontology::Triple>>();
                    let _ = $stream_fn(&tx);
                    drop(tx);
                    let mut triples: Vec<lbd_ontology::Triple> = rx.into_iter().flatten().collect();
                    triples.sort_unstable_by(|a, b| {
                        a.subject.cmp(&b.subject).then_with(|| a.predicate.cmp(&b.predicate))
                    });
                    triples
                } else {
                    Vec::new()
                }
            }};
        }

        let bot_triples = collect_producer!(settings.has(BOT_PRODUCER_ID), |tx| lbd_converter::stream_bot(model, options, tx));
        let beo_triples = collect_producer!(settings.has(BEO_PRODUCER_ID), |tx| lbd_converter::stream_beo(model, options, tx));
        let bsdd_triples = collect_producer!(settings.has(BSDD_PRODUCER_ID), |tx| lbd_converter::stream_bsdd(model, options, tx));
        let props_triples = collect_producer!(settings.has(PROPS_OPM_PRODUCER_ID), |tx| lbd_converter::stream_props_opm(model, options, tx));
        let omg_triples = collect_producer!(settings.has(OMG_FOG_PRODUCER_ID), |tx| lbd_converter::stream_omg_fog(model, options, tx));
        let ifcowl_triples = collect_producer!(settings.has(IFCOWL_PRODUCER_ID), |tx| lbd_converter::modules::ifcowl::stream_ifcowl(
            step, &base, step.header.schema, tx,
            options.stream_batch_size, options.ifcowl_max_workers,
            options.ifcowl_mode,
        ));

        macro_rules! write_producer_nq {
            ($triples:expr, $slug:literal) => {
                if !$triples.is_empty() {
                    lbd_serializer::write_nquads_batch(
                        &mut nq_bytes,
                        &$triples,
                        &format!("{}/{}", normalized_base, $slug),
                    )?;
                }
            };
        }
        write_producer_nq!(bot_triples, "bot");
        write_producer_nq!(beo_triples, "beo");
        write_producer_nq!(bsdd_triples, "bsdd");
        write_producer_nq!(props_triples, "props");
        write_producer_nq!(omg_triples, "omg");
        write_producer_nq!(ifcowl_triples, "ifcowl");

        files.push(ExportedFile {
            filename: format!("{}.nq", settings.output_stem),
            mime_type: "application/n-quads".to_string(),
            role: "merged".to_string(),
            payload: nq_bytes,
        });
    }

    Ok(files)
}
