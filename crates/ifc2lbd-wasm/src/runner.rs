use std::collections::HashMap;
use std::io::{self};

use ifc_model::build_model;
use ifc_step::{parse_step_bytes, StepFile};
#[cfg(target_arch = "wasm32")]
use js_sys::Function;
use lbd_converter::{convert_step_and_model, stream_step_and_model, ConvertOptions};
use lbd_pipeline::BatchKind;
use lbd_serializer::{
    serialize_lbd_batches_incremental_to_writer, serialize_nquads_batches_to_writer,
    serialize_nquads_merged_batches_to_writer, serialize_turtle_batch_raw_to_writer,
    serialize_turtle_batch_to_writer, serialize_turtle_to_writer, write_turtle_prefixes_for_stream,
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
    BEO_PRODUCER_ID, BOT_PRODUCER_ID, FILE_EXPORT_ID, IFCOWL_PRODUCER_ID,
    IFC_TOPOLOGY_PRODUCER_ID, NQUADS_CHUNKED_SERIALIZER_ID, NQUADS_SERIALIZER_ID,
    PROPS_OPM_PRODUCER_ID, TURTLE_SERIALIZER_ID,
};

/// Returns true if the named-graph IRI belongs to an IfcOWL (or alignment) graph.
fn is_ifcowl_graph(iri: &str) -> bool {
    iri.ends_with("/ifcowl") || iri.ends_with("/alignment")
}

/// WASM-safe monotonic timestamp in milliseconds.
#[cfg(target_arch = "wasm32")]
fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> u64 {
    use std::time::Instant;
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let origin = ORIGIN.get_or_init(Instant::now);
    origin.elapsed().as_millis() as u64
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
    /// Maps plugin_id → (duration_ms, triple_count)
    by_producer: HashMap<String, (u64, u64)>,
    bbox_produce_ms: u64,
    serialize_ms: u64,
    export_ms: u64,
}

impl StageDurations {
    fn new() -> Self {
        Self {
            by_producer: HashMap::new(),
            bbox_produce_ms: 0,
            serialize_ms: 0,
            export_ms: 0,
        }
    }

    fn total_triples(&self) -> u64 {
        self.by_producer.values().map(|(_, t)| t).sum()
    }

    fn producer_ms(&self, plugin_id: &str) -> u64 {
        self.by_producer.get(plugin_id).map(|(ms, _)| *ms).unwrap_or(0)
    }

    fn producer_triples(&self, plugin_id: &str) -> u64 {
        self.by_producer.get(plugin_id).map(|(_, t)| *t).unwrap_or(0)
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
        let (plan, settings, mut warnings) = self.resolve_and_validate(request)?;
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
        let exported_files = export_browser_files(&conversion, &base_uri, &settings)
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
        let (plan, settings, warnings) = self.resolve_and_validate(request)?;
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
        let (plan, settings, mut warnings) = self.resolve_and_validate(request)?;
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

        // Emit "running" for produce stages
        if settings.emit_ifcowl {
            emit_stage_event(
                sink,
                IFCOWL_PRODUCER_ID,
                "Produce",
                "running",
                0,
                0,
                0,
                None,
            )
            .map_err(|e| WasmApiError::Serialization(e.to_string()))?;
        }
        if settings.emit_topology {
            emit_stage_event(
                sink,
                IFC_TOPOLOGY_PRODUCER_ID,
                "Produce",
                "running",
                0,
                0,
                0,
                None,
            )
            .map_err(|e| WasmApiError::Serialization(e.to_string()))?;
        }
        if settings.emit_bbox {
            emit_stage_event(
                sink,
                lbd_pipeline::BBOX_ENRICHER_ID,
                "Produce",
                "running",
                0,
                0,
                0,
                None,
            )
            .map_err(|e| WasmApiError::Serialization(e.to_string()))?;
        }

        let sink_config = SinkConfig::from_request(request);
        let stream_batch_size = options.stream_batch_size;
        let ifcowl_max_workers = options.ifcowl_max_workers;

        // If Bbox enricher is active, compute approximate bounding boxes from STEP data.
        // This populates `geometry_bounding_boxes` in ConvertOptions, which enables
        // topology enrichment (adjacency detection from bounding box overlaps).
        let mut bbox_produce_ms: u64 = 0;
        let options = if settings.emit_bbox {
            let bbox_t0 = now_ms();
            let bboxes = lbd_converter::compute_approximate_bboxes(&step, &model);
            bbox_produce_ms = now_ms() - bbox_t0;
            emit_stage_event(
                sink,
                lbd_pipeline::BBOX_ENRICHER_ID,
                "Produce",
                "success",
                bbox_produce_ms,
                0,
                bboxes.len() as u64,
                None,
            )
            .map_err(|e| WasmApiError::Serialization(e.to_string()))?;
            let mut opts = options;
            opts.geometry_bounding_boxes = Some(std::sync::Arc::new(bboxes));
            opts
        } else {
            options
        };

        let (output_files, sink_max_pending_bytes, sink_chunk_size_bytes, mut stage_durations) =
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

        // In Turtle mode, topology is bundled with LBD — if no dedicated timing, use 0
        if settings.emit_topology
            && stage_durations.producer_ms(IFC_TOPOLOGY_PRODUCER_ID) == 0
        {
            let topo_triples = stage_durations.producer_triples(IFC_TOPOLOGY_PRODUCER_ID);
            stage_durations
                .by_producer
                .insert(IFC_TOPOLOGY_PRODUCER_ID.to_string(), (0, topo_triples));
        }
        // Set bbox timing (computed before streaming function)
        stage_durations.bbox_produce_ms = bbox_produce_ms;

        // Note: topology "success" is emitted through the streaming path's
        // stage_rx drain. In Turtle mode where topology is bundled with LBD,
        // topology_produce_ms falls back to produce_ms above for telemetry.

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
        emit_stage_event(
            sink,
            FILE_EXPORT_ID,
            "Export",
            "success",
            stage_durations.export_ms,
            0,
            0,
            None,
        )
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
                if settings.emit_bbox {
                    tel.push(StageTelemetry {
                        plugin_id: lbd_pipeline::BBOX_ENRICHER_ID.to_string(),
                        stage: "Produce".to_string(),
                        status: "success".to_string(),
                        duration_ms: stage_durations.bbox_produce_ms,
                        bytes_out: 0,
                        triples_out: 0,
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
    ) -> Result<(lbd_pipeline::ActivationPlan, ExecutionSettings, Vec<String>), WasmApiError> {
        let requested = dedupe_modules(request.module_ids.clone());
        let plan = self.registry.resolve_activation(&requested)?;
        let configs = parse_module_configs(&request.module_options)?;
        validate_module_configs(&plan, &configs)?;
        validate_typed_module_configs(&configs)?;
        validate_activation_plan(&plan)?;
        let mut warnings = Vec::new();
        let settings = resolve_execution_settings(&plan, &configs, request, &mut warnings)?;
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
        // FullTopology uses topology extension (adjacency from bboxes).
        // IfcTopology lite does NOT use extension.
        // Bbox standalone does NOT enable topology — it only adds geometry triples to LBD.
        let enable_topology_extension = settings.emit_full_topology;
        ConvertOptions {
            base_uri: base_uri.to_string(),
            emit_ifcowl_links: settings.emit_ifcowl,
            enable_topology: settings.emit_topology,
            enable_topology_extension,
            topology_only: false,
            suppress_non_topology_fallback: false,
            geometry_relations: None,
            geometry_bounding_boxes: None, // Computed later from STEP data if bbox is active
            geometry_wkts: None,
            geometry_tolerance: 1e-6,
            low_memory_mode: mode == ExecutionMode::Lowmem,
            stream_batch_size,
            ifcowl_max_workers,
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
    resolve_execution_settings(&plan, &configs, request, &mut warnings)
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
    let mut summaries = Vec::new();
    let chan_cap = if options.low_memory_mode { 4 } else { 16 };
    let instance_base = options.base_uri.clone();
    let mut produce_durations: HashMap<&'static str, u64> = HashMap::new();

    let model = std::sync::Arc::new(model);

    let (lbd_sender, lbd_receiver) = crossbeam::channel::bounded(chan_cap);
    let (ifcowl_sender, ifcowl_receiver) = if settings.emit_ifcowl {
        let (tx, rx) = crossbeam::channel::bounded(chan_cap);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let (topology_sender, topology_receiver) = if settings.emit_topology {
        let (tx, rx) = crossbeam::channel::bounded(chan_cap);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let mut options_lbd = options.clone();
    if settings.emit_topology {
        options_lbd.enable_topology = false;
        options_lbd.enable_topology_extension = false;
    }

    // Spawn LBD+IfcOWL inside rayon::spawn (sequential on wasm32, parallel on native)
    let lbd_stage_tx = stage_tx.clone();
    let ifcowl_stage_tx = stage_tx.clone();
    let topo_stage_tx = stage_tx.clone();
    let model_prod = model.clone();
    let options_topo = options.clone();
    let emit_bot_turtle = settings.emit_bot;
    let emit_beo_turtle = settings.emit_beo;
    let emit_props_opm_turtle = settings.emit_props_opm;
    let emit_omg_fog_turtle = settings.emit_omg_fog;
    rayon::spawn(move || {
        // 1. IfcOWL
        if let Some(ifcowl_sender) = ifcowl_sender {
            let base = lbd_converter::normalize_base_uri(&options_lbd.base_uri);
            let t0 = now_ms();
            let result = lbd_converter::modules::ifcowl::stream_ifcowl(
                &step,
                &base,
                step.header.schema,
                &ifcowl_sender,
                options_lbd.stream_batch_size,
                options_lbd.ifcowl_max_workers,
            );
            let ms = now_ms() - t0;
            drop(ifcowl_sender);
            let _ = ifcowl_stage_tx.send(StageDoneEvent {
                plugin_id: IFCOWL_PRODUCER_ID,
                stage: "Produce",
                duration_ms: ms,
                triple_count: 0,
            });
            let _ = result;
        }

        // 2. Modular LBD producers (BOT / BEO / PROPS_OPM)
        if emit_bot_turtle {
            let t0 = now_ms();
            let _ = lbd_converter::stream_bot(&model_prod, &options_lbd, &lbd_sender);
            let ms = now_ms() - t0;
            let _ = lbd_stage_tx.send(StageDoneEvent {
                plugin_id: BOT_PRODUCER_ID,
                stage: "Produce",
                duration_ms: ms,
                triple_count: 0,
            });
        }
        if emit_beo_turtle {
            let t0 = now_ms();
            let _ = lbd_converter::stream_beo(&model_prod, &options_lbd, &lbd_sender);
            let ms = now_ms() - t0;
            let _ = lbd_stage_tx.send(StageDoneEvent {
                plugin_id: BEO_PRODUCER_ID,
                stage: "Produce",
                duration_ms: ms,
                triple_count: 0,
            });
        }
        if emit_props_opm_turtle {
            let t0 = now_ms();
            let _ = lbd_converter::stream_props_opm(&model_prod, &options_lbd, &lbd_sender);
            let ms = now_ms() - t0;
            let _ = lbd_stage_tx.send(StageDoneEvent {
                plugin_id: PROPS_OPM_PRODUCER_ID,
                stage: "Produce",
                duration_ms: ms,
                triple_count: 0,
            });
        }
        if emit_omg_fog_turtle {
            let t0 = now_ms();
            let _ = lbd_converter::stream_omg_fog(&model_prod, &options_lbd, &lbd_sender);
            let ms = now_ms() - t0;
            let _ = lbd_stage_tx.send(StageDoneEvent {
                plugin_id: lbd_pipeline::OMG_FOG_PRODUCER_ID,
                stage: "Produce",
                duration_ms: ms,
                triple_count: 0,
            });
        }
        drop(lbd_sender);

        // 3. Topology (sequential after LBD — lightweight, ~891 triples)
        if let Some(topology_sender) = topology_sender {
            let t0 = now_ms();
            let result =
                lbd_converter::stream_topology_model(&model_prod, &options_topo, &topology_sender);
            let ms = now_ms() - t0;
            drop(topology_sender);
            let _ = topo_stage_tx.send(StageDoneEvent {
                plugin_id: IFC_TOPOLOGY_PRODUCER_ID,
                stage: "Produce",
                duration_ms: ms,
                triple_count: 0,
            });
            let _ = result;
        }
    });

    // Triple count tracking (counted during serialization)
    let mut lbd_triple_count: u64 = 0;
    let mut ifcowl_triple_count: u64 = 0;
    let mut topology_triple_count: u64 = 0;

    // --- Serialize IfcOWL (separate file) ---
    let mut ifcowl_writer = None;
    if let Some(ifcowl_rx) = ifcowl_receiver {
        let mut w = SinkChunkWriter::new(
            sink,
            format!("{}_ifcowl.ttl", settings.output_stem),
            "text/turtle;charset=utf-8",
            "ifcowl",
            sink_config.chunk_size,
            sink_config.max_pending_bytes,
        )?;
        if !options.low_memory_mode {
            write_turtle_prefixes_for_stream(&mut w, Some(&instance_base))?;
        }
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
        let ser_t0 = now_ms();
        for batch in ifcowl_rx {
            ifcowl_triple_count += batch.len() as u64;
            if options.low_memory_mode {
                serialize_turtle_batch_raw_to_writer(&batch, &mut w)?
            } else {
                serialize_turtle_batch_to_writer(&batch, &mut w, Some(&instance_base))?
            }
        }
        let _ifcowl_ser_ms = now_ms() - ser_t0;
        ifcowl_writer = Some(w);
    }

    // --- Serialize LBD + Topology into same .ttl file ---
    let mut lbd_writer = SinkChunkWriter::new(
        sink,
        format!("{}.ttl", settings.output_stem),
        "text/turtle;charset=utf-8",
        "lbd",
        sink_config.chunk_size,
        sink_config.max_pending_bytes,
    )?;
    if !options.low_memory_mode {
        write_turtle_prefixes_for_stream(&mut lbd_writer, Some(&instance_base))?;
    }

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

    if let Some(topo_rx) = topology_receiver {
        // Merge LBD + topology: drain both channels
        let mut lbd_done = false;
        let mut topo_done = false;
        loop {
            let mut progress = false;
            if !lbd_done {
                match lbd_receiver.try_recv() {
                    Ok(batch) => {
                        lbd_triple_count += batch.len() as u64;
                        if options.low_memory_mode {
                            serialize_turtle_batch_raw_to_writer(&batch, &mut lbd_writer)?
                        } else {
                            serialize_turtle_batch_to_writer(
                                &batch,
                                &mut lbd_writer,
                                Some(&instance_base),
                            )?
                        }
                        progress = true;
                        continue;
                    }
                    Err(crossbeam::channel::TryRecvError::Empty) => {}
                    Err(crossbeam::channel::TryRecvError::Disconnected) => {
                        lbd_done = true;
                        progress = true;
                        continue;
                    }
                }
            }
            if !topo_done {
                match topo_rx.try_recv() {
                    Ok(batch) => {
                        topology_triple_count += batch.len() as u64;
                        if options.low_memory_mode {
                            serialize_turtle_batch_raw_to_writer(&batch, &mut lbd_writer)?
                        } else {
                            serialize_turtle_batch_to_writer(
                                &batch,
                                &mut lbd_writer,
                                Some(&instance_base),
                            )?
                        }
                        progress = true;
                        continue;
                    }
                    Err(crossbeam::channel::TryRecvError::Empty) => {}
                    Err(crossbeam::channel::TryRecvError::Disconnected) => {
                        topo_done = true;
                        progress = true;
                        continue;
                    }
                }
            }
            if lbd_done && topo_done {
                break;
            }
            if !progress {
                std::thread::yield_now();
            }
        }
    } else {
        for batch in lbd_receiver {
            lbd_triple_count += batch.len() as u64;
            if options.low_memory_mode {
                serialize_turtle_batch_raw_to_writer(&batch, &mut lbd_writer)?
            } else {
                serialize_turtle_batch_to_writer(&batch, &mut lbd_writer, Some(&instance_base))?
            }
        }
    }
    let serialize_ms = now_ms() - serialize_t0;

    let total_triple_count = lbd_triple_count + ifcowl_triple_count + topology_triple_count;

    // Drain stage events — emit "success" with real triple counts
    while let Ok(evt) = stage_rx.try_recv() {
        let triples = if evt.plugin_id == BOT_PRODUCER_ID
            || evt.plugin_id == BEO_PRODUCER_ID
            || evt.plugin_id == PROPS_OPM_PRODUCER_ID
        {
            lbd_triple_count
        } else if evt.plugin_id == IFCOWL_PRODUCER_ID {
            ifcowl_triple_count
        } else if evt.plugin_id == IFC_TOPOLOGY_PRODUCER_ID {
            topology_triple_count
        } else {
            0
        };
        produce_durations.insert(evt.plugin_id, evt.duration_ms);
        emit_stage_event(
            sink,
            evt.plugin_id,
            evt.stage,
            "success",
            evt.duration_ms,
            0,
            triples,
            None,
        )?;
    }

    // --- Export ---
    emit_stage_event(sink, FILE_EXPORT_ID, "Export", "running", 0, 0, 0, None)?;
    let export_t0 = now_ms();
    let (lbd_summary, lbd_peak, lbd_chunk_size) = lbd_writer.finish()?;
    summaries.push(lbd_summary);
    let mut peak = lbd_peak;
    let mut chunk_size = lbd_chunk_size;
    if let Some(w) = ifcowl_writer {
        let (s, p, c) = w.finish()?;
        summaries.push(s);
        peak = peak.max(p);
        chunk_size = chunk_size.max(c);
    }
    let export_ms = now_ms() - export_t0;

    let mut stage_durations = StageDurations::new();
    stage_durations.serialize_ms = serialize_ms;
    stage_durations.export_ms = export_ms;
    for (plugin_id, ms) in produce_durations {
        let triples = if plugin_id == BOT_PRODUCER_ID
            || plugin_id == BEO_PRODUCER_ID
            || plugin_id == PROPS_OPM_PRODUCER_ID
        {
            lbd_triple_count
        } else if plugin_id == IFCOWL_PRODUCER_ID {
            ifcowl_triple_count
        } else if plugin_id == IFC_TOPOLOGY_PRODUCER_ID {
            topology_triple_count
        } else {
            0
        };
        stage_durations
            .by_producer
            .insert(plugin_id.to_string(), (ms, triples));
    }
    Ok((summaries, peak, chunk_size, stage_durations))
}

// ---------------------------------------------------------------------------
// N-Quads streaming to sink (merged single-file or chunked multi-file)
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
    stage_tx: &crossbeam::channel::Sender<StageDoneEvent>,
    stage_rx: &crossbeam::channel::Receiver<StageDoneEvent>,
    is_chunked: bool,
    chunk_mode: SinkChunkingMode,
) -> Result<(Vec<OutputFileSummary>, usize, usize, StageDurations), lbd_serializer::SerializerError>
{
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

    let chunk_prefix = &settings.nquads.chunk_prefix;
    let chunk_size_lines = settings.nquads.chunk_size_lines;
    let chunk_size_bytes = settings.nquads.chunk_size_bytes;
    let topology_graph = format!("{normalized_base}/topology");

    let model = std::sync::Arc::new(model);
    let mut produce_durations: HashMap<&'static str, u64> = HashMap::new();

    // Triple count tracking (counted during serialization)
    let mut lbd_triple_count: u64 = 0;
    let mut ifcowl_triple_count: u64 = 0;
    let mut topology_triple_count: u64 = 0;

    // Helper: resolve triple count for a plugin — used in emit_stage_event calls.
    // Cannot be a closure because triple count variables are mutated in the loop.
    macro_rules! triple_count_for {
        ($plugin_id:expr) => {
            match $plugin_id as &str {
                BOT_PRODUCER_ID | BEO_PRODUCER_ID | PROPS_OPM_PRODUCER_ID => {
                    lbd_triple_count
                }
                IFCOWL_PRODUCER_ID => ifcowl_triple_count,
                IFC_TOPOLOGY_PRODUCER_ID => topology_triple_count,
                _ => 0,
            }
        };
    }

    // Create channels for each producer
    let chan_cap: usize = 8;
    let (lbd_sender, lbd_receiver) = crossbeam::channel::bounded(chan_cap);
    let (ifcowl_sender, ifcowl_receiver) = if settings.emit_ifcowl {
        let (tx, rx) = crossbeam::channel::bounded(chan_cap);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let (topology_sender, topology_receiver) = if settings.emit_topology {
        let (tx, rx) = crossbeam::channel::bounded(chan_cap);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    // Disable topology in LBD options to avoid duplicate triples
    let mut options_lbd = options.clone();
    if settings.emit_topology {
        options_lbd.enable_topology = false;
        options_lbd.enable_topology_extension = false;
    }
    let options_topo = options.clone();

    // ALL producers inside ONE rayon::spawn — sequential on wasm32 avoids contention.
    // Order: IfcOWL → LBD → Topology (each gets full rayon pool, independent timing).
    let lbd_stage_tx = stage_tx.clone();
    let ifcowl_stage_tx = stage_tx.clone();
    let topo_stage_tx = stage_tx.clone();
    let model_prod = model.clone();
    let emit_bot_nq = settings.emit_bot;
    let emit_beo_nq = settings.emit_beo;
    let emit_props_opm_nq = settings.emit_props_opm;
    let emit_omg_fog_nq = settings.emit_omg_fog;
    rayon::spawn(move || {
        // 1. IfcOWL
        if let Some(ifcowl_sender) = ifcowl_sender {
            let base = lbd_converter::normalize_base_uri(&options_lbd.base_uri);
            let t0 = now_ms();
            let result = lbd_converter::modules::ifcowl::stream_ifcowl(
                &step,
                &base,
                step.header.schema,
                &ifcowl_sender,
                options_lbd.stream_batch_size,
                options_lbd.ifcowl_max_workers,
            );
            let ms = now_ms() - t0;
            drop(ifcowl_sender);
            let _ = ifcowl_stage_tx.send(StageDoneEvent {
                plugin_id: IFCOWL_PRODUCER_ID,
                stage: "Produce",
                duration_ms: ms,
                triple_count: 0,
            });
            let _ = result;
        }

        // 2. Modular LBD producers (BOT / BEO / PROPS_OPM)
        if emit_bot_nq {
            let t0 = now_ms();
            let _ = lbd_converter::stream_bot(&model_prod, &options_lbd, &lbd_sender);
            let ms = now_ms() - t0;
            let _ = lbd_stage_tx.send(StageDoneEvent {
                plugin_id: BOT_PRODUCER_ID,
                stage: "Produce",
                duration_ms: ms,
                triple_count: 0,
            });
        }
        if emit_beo_nq {
            let t0 = now_ms();
            let _ = lbd_converter::stream_beo(&model_prod, &options_lbd, &lbd_sender);
            let ms = now_ms() - t0;
            let _ = lbd_stage_tx.send(StageDoneEvent {
                plugin_id: BEO_PRODUCER_ID,
                stage: "Produce",
                duration_ms: ms,
                triple_count: 0,
            });
        }
        if emit_props_opm_nq {
            let t0 = now_ms();
            let _ = lbd_converter::stream_props_opm(&model_prod, &options_lbd, &lbd_sender);
            let ms = now_ms() - t0;
            let _ = lbd_stage_tx.send(StageDoneEvent {
                plugin_id: PROPS_OPM_PRODUCER_ID,
                stage: "Produce",
                duration_ms: ms,
                triple_count: 0,
            });
        }
        if emit_omg_fog_nq {
            let t0 = now_ms();
            let _ = lbd_converter::stream_omg_fog(&model_prod, &options_lbd, &lbd_sender);
            let ms = now_ms() - t0;
            let _ = lbd_stage_tx.send(StageDoneEvent {
                plugin_id: lbd_pipeline::OMG_FOG_PRODUCER_ID,
                stage: "Produce",
                duration_ms: ms,
                triple_count: 0,
            });
        }
        drop(lbd_sender);

        // 3. Topology (sequential after LBD — lightweight)
        if let Some(topology_sender) = topology_sender {
            let t0 = now_ms();
            let result =
                lbd_converter::stream_topology_model(&model_prod, &options_topo, &topology_sender);
            let ms = now_ms() - t0;
            drop(topology_sender);
            let _ = topo_stage_tx.send(StageDoneEvent {
                plugin_id: IFC_TOPOLOGY_PRODUCER_ID,
                stage: "Produce",
                duration_ms: ms,
                triple_count: 0,
            });
            let _ = result;
        }
    });

    // ── Serialization paths ──
    // After producers are spawned, the main thread drains channels and writes output.

    if settings.emit_ifcowl {
        // IfcOWL is active — we have ifcowl_receiver
        let ifcowl_rx = ifcowl_receiver.unwrap();

        if is_chunked {
            // ── CHUNKED: Each producer writes to its own SinkQuadChunkWriter ──
            let mut lbd_chunk_writer = SinkQuadChunkWriter::new(
                sink,
                format!("{}-lbd", chunk_prefix),
                chunk_mode,
                chunk_size_lines,
                chunk_size_bytes,
                sink_config.chunk_size,
                sink_config.max_pending_bytes,
            )?;
            let mut ifcowl_chunk_writer = SinkQuadChunkWriter::new(
                sink,
                format!("{}-ifcowl", chunk_prefix),
                chunk_mode,
                chunk_size_lines,
                chunk_size_bytes,
                sink_config.chunk_size,
                sink_config.max_pending_bytes,
            )?;
            let mut topology_chunk_writer = if settings.emit_topology {
                Some(SinkQuadChunkWriter::new(
                    sink,
                    format!("{}-topology", chunk_prefix),
                    chunk_mode,
                    chunk_size_lines,
                    chunk_size_bytes,
                    sink_config.chunk_size,
                    sink_config.max_pending_bytes,
                )?)
            } else {
                None
            };

            let serializer_id = if settings.output_formats.nquads_chunked {
                NQUADS_CHUNKED_SERIALIZER_ID
            } else {
                NQUADS_SERIALIZER_ID
            };
            emit_stage_event(sink, serializer_id, "Serialize", "running", 0, 0, 0, None)?;
            let serialize_t0 = now_ms();

            let mut lbd_done = false;
            let mut ifcowl_done = false;
            let mut topology_done = !settings.emit_topology;
            while !lbd_done || !ifcowl_done || !topology_done {
                while let Ok(evt) = stage_rx.try_recv() {
                    produce_durations.insert(evt.plugin_id, evt.duration_ms);
                    emit_stage_event(
                        sink,
                        evt.plugin_id,
                        evt.stage,
                        "success",
                        evt.duration_ms,
                        0,
                        triple_count_for!(evt.plugin_id),
                        None,
                    )?;
                }
                if !lbd_done {
                    match lbd_receiver.try_recv() {
                        Ok(batch) => {
                            lbd_triple_count += batch.len() as u64;
                            lbd_serializer::write_nquads_batch(
                                &mut lbd_chunk_writer,
                                &batch,
                                &lbd_graph,
                            )?;
                        }
                        Err(crossbeam::channel::TryRecvError::Empty) => {}
                        Err(crossbeam::channel::TryRecvError::Disconnected) => {
                            lbd_done = true;
                        }
                    }
                }
                if !ifcowl_done {
                    match ifcowl_rx.try_recv() {
                        Ok(batch) => {
                            ifcowl_triple_count += batch.len() as u64;
                            lbd_serializer::write_nquads_batch(
                                &mut ifcowl_chunk_writer,
                                &batch,
                                &ifcowl_graph,
                            )?;
                        }
                        Err(crossbeam::channel::TryRecvError::Empty) => {}
                        Err(crossbeam::channel::TryRecvError::Disconnected) => {
                            ifcowl_done = true;
                        }
                    }
                }
                if !topology_done {
                    if let Some(ref mut topo_writer) = topology_chunk_writer {
                        if let Some(ref topo_rx) = topology_receiver {
                            match topo_rx.try_recv() {
                                Ok(batch) => {
                                    topology_triple_count += batch.len() as u64;
                                    lbd_serializer::write_nquads_batch(
                                        topo_writer,
                                        &batch,
                                        &topology_graph,
                                    )?;
                                }
                                Err(crossbeam::channel::TryRecvError::Empty) => {}
                                Err(crossbeam::channel::TryRecvError::Disconnected) => {
                                    topology_done = true;
                                }
                            }
                        }
                    }
                }
                if !lbd_done || !ifcowl_done || !topology_done {
                    std::thread::yield_now();
                }
            }
            let serialize_ms = now_ms() - serialize_t0;

            while let Ok(evt) = stage_rx.try_recv() {
                produce_durations.insert(evt.plugin_id, evt.duration_ms);
                emit_stage_event(
                    sink,
                    evt.plugin_id,
                    evt.stage,
                    "success",
                    evt.duration_ms,
                    0,
                    triple_count_for!(evt.plugin_id),
                    None,
                )?;
            }

            emit_stage_event(sink, FILE_EXPORT_ID, "Export", "running", 0, 0, 0, None)?;
            let export_t0 = now_ms();
            let mut summaries = lbd_chunk_writer.finish()?;
            summaries.extend(ifcowl_chunk_writer.finish()?);
            if let Some(topo_writer) = topology_chunk_writer {
                summaries.extend(topo_writer.finish()?);
            }
            let export_ms = now_ms() - export_t0;

            let mut sd = StageDurations::new();
            sd.serialize_ms = serialize_ms;
            sd.export_ms = export_ms;
            for (plugin_id, ms) in &produce_durations {
                let triples = triple_count_for!(plugin_id);
                sd.by_producer.insert(plugin_id.to_string(), (*ms, triples));
            }
            return Ok((summaries, 0, sink_config.chunk_size, sd));
        } else {
            // ── MERGED: LBD + IfcOWL + Topology into one .nq file ──
            let (merged_sender, merged_receiver) = crossbeam::channel::bounded(chan_cap * 2);
            let n_fwd: usize = 2 + if settings.emit_topology { 1 } else { 0 };
            let (fwd_result_sender, fwd_result_receiver) =
                crossbeam::channel::bounded::<Result<(), lbd_serializer::SerializerError>>(n_fwd);

            // LBD forwarder
            let lbd_fwd = merged_sender.clone();
            let lbd_res = fwd_result_sender.clone();
            let lbd_graph_fwd = lbd_graph.clone();
            rayon::spawn(move || {
                let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                    for batch in lbd_receiver {
                        lbd_fwd
                            .send((BatchKind::new(lbd_graph_fwd.clone()), batch))
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

            // IfcOWL forwarder
            let ifcowl_fwd = merged_sender.clone();
            let ifcowl_res = fwd_result_sender.clone();
            let ifcowl_graph_fwd2 = ifcowl_graph.clone();
            rayon::spawn(move || {
                let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                    for batch in ifcowl_rx {
                        ifcowl_fwd
                            .send((BatchKind::new(ifcowl_graph_fwd2.clone()), batch))
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

            // Topology forwarder
            if settings.emit_topology {
                if let Some(topo_rx) = topology_receiver {
                    let topo_fwd = merged_sender.clone();
                    let topo_res = fwd_result_sender.clone();
                    let topology_graph_fwd = topology_graph.clone();
                    rayon::spawn(move || {
                        let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                            for batch in topo_rx {
                                topo_fwd
                                    .send((BatchKind::new(topology_graph_fwd.clone()), batch))
                                    .map_err(|_| {
                                        lbd_serializer::SerializerError::Io(
                                            io::ErrorKind::BrokenPipe.into(),
                                        )
                                    })?;
                            }
                            Ok(())
                        })();
                        let _ = topo_res.send(result);
                    });
                }
            }

            drop(merged_sender);
            drop(fwd_result_sender);

            let mut writer = SinkChunkWriter::new(
                sink,
                format!("{}.nq", settings.output_stem),
                "application/n-quads",
                "merged",
                sink_config.chunk_size,
                sink_config.max_pending_bytes,
            )?;

            emit_stage_event(
                sink,
                NQUADS_SERIALIZER_ID,
                "Serialize",
                "running",
                0,
                0,
                0,
                None,
            )?;
            let serialize_t0 = now_ms();
            for (kind, batch) in merged_receiver.iter() {
                while let Ok(evt) = stage_rx.try_recv() {
                    produce_durations.insert(evt.plugin_id, evt.duration_ms);
                    // Don't emit topology yet — its count isn't final
                    if evt.plugin_id != IFC_TOPOLOGY_PRODUCER_ID {
                        emit_stage_event(
                            sink,
                            evt.plugin_id,
                            evt.stage,
                            "success",
                            evt.duration_ms,
                            0,
                            triple_count_for!(evt.plugin_id),
                            None,
                        )?;
                    }
                }
                let graph_iri = kind.iri().to_string();
                if graph_iri == lbd_graph {
                    lbd_triple_count += batch.len() as u64;
                } else if graph_iri == ifcowl_graph {
                    ifcowl_triple_count += batch.len() as u64;
                } else if graph_iri == topology_graph {
                    topology_triple_count += batch.len() as u64;
                }
                lbd_serializer::write_nquads_batch(&mut writer, &batch, &graph_iri)?;
            }
            let serialize_ms = now_ms() - serialize_t0;

            // Drain remaining stage events
            while let Ok(evt) = stage_rx.try_recv() {
                produce_durations.insert(evt.plugin_id, evt.duration_ms);
                if evt.plugin_id != IFC_TOPOLOGY_PRODUCER_ID {
                    emit_stage_event(
                        sink,
                        evt.plugin_id,
                        evt.stage,
                        "success",
                        evt.duration_ms,
                        0,
                        triple_count_for!(evt.plugin_id),
                        None,
                    )?;
                }
            }

            // Emit topology success with CORRECT triple count (now final)
            let topo_ms = produce_durations
                .get(IFC_TOPOLOGY_PRODUCER_ID)
                .copied()
                .unwrap_or(0);
            if topo_ms > 0 {
                emit_stage_event(
                    sink,
                    IFC_TOPOLOGY_PRODUCER_ID,
                    "Produce",
                    "success",
                    topo_ms,
                    0,
                    topology_triple_count,
                    None,
                )?;
            }

            for _ in 0..n_fwd {
                fwd_result_receiver.recv().map_err(|_| {
                    lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
                })??;
            }

            emit_stage_event(sink, FILE_EXPORT_ID, "Export", "running", 0, 0, 0, None)?;
            let export_t0 = now_ms();
            let (summary, peak, chunk_size) = writer.finish()?;
            let export_ms = now_ms() - export_t0;

            let mut sd = StageDurations::new();
            sd.serialize_ms = serialize_ms;
            sd.export_ms = export_ms;
            for (plugin_id, ms) in &produce_durations {
                let triples = triple_count_for!(plugin_id);
                sd.by_producer.insert(plugin_id.to_string(), (*ms, triples));
            }
            return Ok((vec![summary], peak, chunk_size, sd));
        }
    } else {
        // LBD-only N-Quads (optionally with topology)
        if is_chunked {
            let mut lbd_chunk_writer = SinkQuadChunkWriter::new(
                sink,
                format!("{}-lbd", chunk_prefix),
                chunk_mode,
                chunk_size_lines,
                chunk_size_bytes,
                sink_config.chunk_size,
                sink_config.max_pending_bytes,
            )?;
            let mut topology_chunk_writer = if settings.emit_topology {
                Some(SinkQuadChunkWriter::new(
                    sink,
                    format!("{}-topology", chunk_prefix),
                    chunk_mode,
                    chunk_size_lines,
                    chunk_size_bytes,
                    sink_config.chunk_size,
                    sink_config.max_pending_bytes,
                )?)
            } else {
                None
            };

            let serializer_id = if settings.output_formats.nquads_chunked {
                NQUADS_CHUNKED_SERIALIZER_ID
            } else {
                NQUADS_SERIALIZER_ID
            };
            emit_stage_event(sink, serializer_id, "Serialize", "running", 0, 0, 0, None)?;
            let serialize_t0 = now_ms();

            if settings.emit_topology {
                let mut lbd_done = false;
                let mut topology_done = false;
                while !lbd_done || !topology_done {
                    while let Ok(evt) = stage_rx.try_recv() {
                        produce_durations.insert(evt.plugin_id, evt.duration_ms);
                        emit_stage_event(
                            sink,
                            evt.plugin_id,
                            evt.stage,
                            "success",
                            evt.duration_ms,
                            0,
                            0,
                            None,
                        )?;
                    }
                    if !lbd_done {
                        match lbd_receiver.try_recv() {
                            Ok(batch) => {
                                lbd_triple_count += batch.len() as u64;
                                lbd_serializer::write_nquads_batch(
                                    &mut lbd_chunk_writer,
                                    &batch,
                                    &lbd_graph,
                                )?;
                            }
                            Err(crossbeam::channel::TryRecvError::Empty) => {}
                            Err(crossbeam::channel::TryRecvError::Disconnected) => {
                                lbd_done = true;
                            }
                        }
                    }
                    if !topology_done {
                        if let Some(ref mut topo_writer) = topology_chunk_writer {
                            if let Some(ref topo_rx) = topology_receiver {
                                match topo_rx.try_recv() {
                                    Ok(batch) => {
                                        topology_triple_count += batch.len() as u64;
                                        lbd_serializer::write_nquads_batch(
                                            topo_writer,
                                            &batch,
                                            &topology_graph,
                                        )?;
                                    }
                                    Err(crossbeam::channel::TryRecvError::Empty) => {}
                                    Err(crossbeam::channel::TryRecvError::Disconnected) => {
                                        topology_done = true;
                                    }
                                }
                            }
                        }
                    }
                    if !lbd_done || !topology_done {
                        std::thread::yield_now();
                    }
                }
            } else {
                for batch in lbd_receiver.iter() {
                    while let Ok(evt) = stage_rx.try_recv() {
                        produce_durations.insert(evt.plugin_id, evt.duration_ms);
                        emit_stage_event(
                            sink,
                            evt.plugin_id,
                            evt.stage,
                            "success",
                            evt.duration_ms,
                            0,
                            triple_count_for!(evt.plugin_id),
                            None,
                        )?;
                    }
                    lbd_triple_count += batch.len() as u64;
                    lbd_serializer::write_nquads_batch(&mut lbd_chunk_writer, &batch, &lbd_graph)?;
                }
            }
            let serialize_ms = now_ms() - serialize_t0;
            while let Ok(evt) = stage_rx.try_recv() {
                produce_durations.insert(evt.plugin_id, evt.duration_ms);
                emit_stage_event(
                    sink,
                    evt.plugin_id,
                    evt.stage,
                    "success",
                    evt.duration_ms,
                    0,
                    triple_count_for!(evt.plugin_id),
                    None,
                )?;
            }

            emit_stage_event(sink, FILE_EXPORT_ID, "Export", "running", 0, 0, 0, None)?;
            let export_t0 = now_ms();
            let mut summaries = lbd_chunk_writer.finish()?;
            if let Some(topo_writer) = topology_chunk_writer {
                summaries.extend(topo_writer.finish()?);
            }
            let export_ms = now_ms() - export_t0;

            let mut sd = StageDurations::new();
            sd.serialize_ms = serialize_ms;
            sd.export_ms = export_ms;
            for (plugin_id, ms) in &produce_durations {
                let triples = triple_count_for!(plugin_id);
                sd.by_producer.insert(plugin_id.to_string(), (*ms, triples));
            }
            return Ok((summaries, 0, sink_config.chunk_size, sd));
        } else {
            // LBD-only merged N-Quads
            if settings.emit_topology {
                let (merged_sender, merged_receiver) = crossbeam::channel::bounded(chan_cap * 2);
                let n_fwd: usize = 2;
                let (fwd_result_sender, fwd_result_receiver) = crossbeam::channel::bounded::<
                    Result<(), lbd_serializer::SerializerError>,
                >(n_fwd);

                let lbd_fwd = merged_sender.clone();
                let lbd_res = fwd_result_sender.clone();
                let lbd_graph_fwd2 = lbd_graph.clone();
                rayon::spawn(move || {
                    let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                        for batch in lbd_receiver {
                            lbd_fwd
                                .send((BatchKind::new(lbd_graph_fwd2.clone()), batch))
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

                if let Some(topo_rx) = topology_receiver {
                    let topo_fwd = merged_sender.clone();
                    let topo_res = fwd_result_sender.clone();
                    let topology_graph_fwd2 = topology_graph.clone();
                    rayon::spawn(move || {
                        let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                            for batch in topo_rx {
                                topo_fwd
                                    .send((BatchKind::new(topology_graph_fwd2.clone()), batch))
                                    .map_err(|_| {
                                        lbd_serializer::SerializerError::Io(
                                            io::ErrorKind::BrokenPipe.into(),
                                        )
                                    })?;
                            }
                            Ok(())
                        })();
                        let _ = topo_res.send(result);
                    });
                }

                drop(merged_sender);
                drop(fwd_result_sender);

                let mut writer = SinkChunkWriter::new(
                    sink,
                    format!("{}.nq", settings.output_stem),
                    "application/n-quads",
                    "merged",
                    sink_config.chunk_size,
                    sink_config.max_pending_bytes,
                )?;

                emit_stage_event(
                    sink,
                    NQUADS_SERIALIZER_ID,
                    "Serialize",
                    "running",
                    0,
                    0,
                    0,
                    None,
                )?;
                let serialize_t0 = now_ms();
                for (kind, batch) in merged_receiver.iter() {
                    while let Ok(evt) = stage_rx.try_recv() {
                        produce_durations.insert(evt.plugin_id, evt.duration_ms);
                        emit_stage_event(
                            sink,
                            evt.plugin_id,
                            evt.stage,
                            "success",
                            evt.duration_ms,
                            0,
                            0,
                            None,
                        )?;
                    }
                    let graph_iri = kind.iri().to_string();
                    if graph_iri == lbd_graph {
                        lbd_triple_count += batch.len() as u64;
                    } else if graph_iri == topology_graph {
                        topology_triple_count += batch.len() as u64;
                    }
                    lbd_serializer::write_nquads_batch(&mut writer, &batch, &graph_iri)?;
                }
                let serialize_ms = now_ms() - serialize_t0;
                while let Ok(evt) = stage_rx.try_recv() {
                    produce_durations.insert(evt.plugin_id, evt.duration_ms);
                    emit_stage_event(
                        sink,
                        evt.plugin_id,
                        evt.stage,
                        "success",
                        evt.duration_ms,
                        0,
                        triple_count_for!(evt.plugin_id),
                        None,
                    )?;
                }
                for _ in 0..n_fwd {
                    fwd_result_receiver.recv().map_err(|_| {
                        lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
                    })??;
                }

                emit_stage_event(sink, FILE_EXPORT_ID, "Export", "running", 0, 0, 0, None)?;
                let export_t0 = now_ms();
                let (summary, peak, chunk_size) = writer.finish()?;
                let export_ms = now_ms() - export_t0;

                let mut sd = StageDurations::new();
                sd.serialize_ms = serialize_ms;
                sd.export_ms = export_ms;
                for (plugin_id, ms) in &produce_durations {
                    let triples = triple_count_for!(plugin_id);
                    sd.by_producer.insert(plugin_id.to_string(), (*ms, triples));
                }
                return Ok((vec![summary], peak, chunk_size, sd));
            } else {
                // LBD-only, no topology — simplest path
                let mut writer = SinkChunkWriter::new(
                    sink,
                    format!("{}.nq", settings.output_stem),
                    "application/n-quads",
                    "merged",
                    sink_config.chunk_size,
                    sink_config.max_pending_bytes,
                )?;

                emit_stage_event(
                    sink,
                    NQUADS_SERIALIZER_ID,
                    "Serialize",
                    "running",
                    0,
                    0,
                    0,
                    None,
                )?;
                let serialize_t0 = now_ms();
                for batch in lbd_receiver {
                    lbd_triple_count += batch.len() as u64;
                    lbd_serializer::write_nquads_batch(&mut writer, &batch, &lbd_graph)?;
                }
                let serialize_ms = now_ms() - serialize_t0;

                while let Ok(evt) = stage_rx.try_recv() {
                    produce_durations.insert(evt.plugin_id, evt.duration_ms);
                    emit_stage_event(
                        sink,
                        evt.plugin_id,
                        evt.stage,
                        "success",
                        evt.duration_ms,
                        0,
                        triple_count_for!(evt.plugin_id),
                        None,
                    )?;
                }

                emit_stage_event(sink, FILE_EXPORT_ID, "Export", "running", 0, 0, 0, None)?;
                let export_t0 = now_ms();
                let (summary, peak, chunk_size) = writer.finish()?;
                let export_ms = now_ms() - export_t0;

                let mut sd = StageDurations::new();
                sd.serialize_ms = serialize_ms;
                sd.export_ms = export_ms;
                for (plugin_id, ms) in &produce_durations {
                    let triples = triple_count_for!(plugin_id);
                    sd.by_producer.insert(plugin_id.to_string(), (*ms, triples));
                }
                return Ok((vec![summary], peak, chunk_size, sd));
            }
        }
    }
}

// ===========================================================================
// In-memory export (non-streaming, used by run_memory)
// ===========================================================================

fn export_browser_files(
    conversion: &lbd_converter::ConversionResult,
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
        if settings.emit_ifcowl {
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
        let mut nq_bytes: Vec<u8> = Vec::new();
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
        files.push(ExportedFile {
            filename: format!("{}.nq", settings.output_stem),
            mime_type: "application/n-quads".to_string(),
            role: "merged".to_string(),
            payload: nq_bytes,
        });
    }

    Ok(files)
}
