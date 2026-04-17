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
use crate::sink::SinkChunkWriter;
use crate::types::*;
use crate::validation::{
    dedupe_modules, normalize_base_for_graph_iri, parse_module_configs, resolve_execution_settings,
    validate_activation_plan, validate_module_configs, validate_typed_module_configs,
};
use crate::DEFAULT_BASE_URI;
use lbd_pipeline::{FILE_EXPORT_ID, NQUADS_SERIALIZER_ID, TURTLE_SERIALIZER_ID};

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

        let step = parse_step_bytes(input)?;
        let model = build_model(&step)?;
        let options = self.make_convert_options(&base_uri, mode, &settings, request);

        let serializer_id = match settings.output_format {
            OutputFormat::Turtle => TURTLE_SERIALIZER_ID,
            OutputFormat::Nquads => NQUADS_SERIALIZER_ID,
        };
        let sink_config = SinkConfig::from_request(request);
        let (output_files, sink_max_pending_bytes, sink_chunk_size_bytes) =
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
                stream_batch_size: options.stream_batch_size,
                ifcowl_max_workers: options.ifcowl_max_workers,
                sink_chunk_size_bytes,
                sink_max_pending_bytes,
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
        ConvertOptions {
            base_uri: base_uri.to_string(),
            emit_ifcowl_links: settings.emit_ifcowl,
            enable_topology: false,
            enable_topology_extension: false,
            topology_only: false,
            suppress_non_topology_fallback: false,
            geometry_relations: None,
            geometry_bounding_boxes: None,
            geometry_wkts: None,
            geometry_tolerance: 1e-6,
            low_memory_mode: mode == ExecutionMode::Lowmem,
            stream_batch_size,
            ifcowl_max_workers,
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
// Streaming export helpers
// ===========================================================================

fn export_browser_file_summaries_streaming(
    step: &StepFile,
    model: &ifc_model::IfcModel,
    options: &ConvertOptions,
    base_uri: &str,
    settings: &ExecutionSettings,
) -> Result<Vec<OutputFileSummary>, lbd_serializer::SerializerError> {
    match settings.output_format {
        OutputFormat::Turtle => turtle_file_summaries(step, model, options, base_uri, settings),
        OutputFormat::Nquads => nquads_file_summaries(step, model, options, base_uri, settings),
    }
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
                        lbd_fwd.send((BatchKind::Lbd, batch)).map_err(|_| {
                            lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
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
                        ifcowl_fwd.send((BatchKind::Ifcowl, batch)).map_err(|_| {
                            lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
                        })?;
                    }
                    Ok(())
                })();
                let _ = ifcowl_res.send(result);
            });

            drop(merged_sender);
            drop(forward_result_sender);

            for (kind, batch) in merged_receiver {
                let writer = match kind {
                    BatchKind::Lbd => &mut lbd_count,
                    BatchKind::Ifcowl => &mut ifcowl_count,
                    BatchKind::Topology => &mut lbd_count,
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

/// Configuration for the JS sink writer.
pub(crate) struct SinkConfig {
    pub chunk_size: usize,
    pub max_pending_bytes: usize,
}

impl SinkConfig {
    /// Derive sink config from request parameters with sensible defaults.
    /// Default chunk size: 1MB. Default max pending: 4× chunk size (4MB).
    pub fn from_request(request: &crate::types::ConversionRequest) -> Self {
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

#[cfg(target_arch = "wasm32")]
fn export_browser_files_to_sink_streaming(
    step: StepFile,
    model: ifc_model::IfcModel,
    options: ConvertOptions,
    base_uri: &str,
    settings: &ExecutionSettings,
    sink: &Function,
    sink_config: &SinkConfig,
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
                sink_config.chunk_size,
                sink_config.max_pending_bytes,
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
                    let result = stream_step_and_model(
                        &step,
                        &model,
                        &options_for_producer,
                        &lbd_sender,
                        Some(&ifcowl_sender),
                    )
                    .map_err(|_| {
                        lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
                    });
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
                    sink_config.chunk_size,
                    sink_config.max_pending_bytes,
                )?;
                if !options.low_memory_mode {
                    write_turtle_prefixes_for_stream(&mut ifcowl_writer, Some(&instance_base))?;
                }

                let lbd_fwd = merged_sender.clone();
                let lbd_res = forward_result_sender.clone();
                rayon::spawn(move || {
                    let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                        for batch in lbd_receiver {
                            lbd_fwd.send((BatchKind::Lbd, batch)).map_err(|_| {
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
                rayon::spawn(move || {
                    let result = (|| -> Result<(), lbd_serializer::SerializerError> {
                        for batch in ifcowl_receiver {
                            ifcowl_fwd.send((BatchKind::Ifcowl, batch)).map_err(|_| {
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
                    match kind {
                        BatchKind::Lbd => {
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
                        BatchKind::Ifcowl => {
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
                        BatchKind::Topology => {
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

                let (lbd_summary, lbd_peak, lbd_chunk_size) = lbd_writer.finish()?;
                summaries.push(lbd_summary);
                let (ifcowl_summary, ifcowl_peak, ifcowl_chunk_size) = ifcowl_writer.finish()?;
                summaries.push(ifcowl_summary);
                Ok((
                    summaries,
                    lbd_peak.max(ifcowl_peak),
                    lbd_chunk_size.max(ifcowl_chunk_size),
                ))
            } else {
                let (producer_result_sender, producer_result_receiver) =
                    crossbeam::channel::bounded::<Result<(), lbd_serializer::SerializerError>>(1);
                let options_for_producer = options.clone();
                rayon::spawn(move || {
                    let result = stream_step_and_model(
                        &step,
                        &model,
                        &options_for_producer,
                        &lbd_sender,
                        None,
                    )
                    .map_err(|_| {
                        lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
                    });
                    drop(lbd_sender);
                    let _ = producer_result_sender.send(result.map(|_| ()));
                });

                serialize_lbd_batches_incremental_to_writer(
                    lbd_receiver,
                    &mut lbd_writer,
                    &instance_base,
                )?;

                producer_result_receiver.recv().map_err(|_| {
                    lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
                })??;

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
                sink_config.chunk_size,
                sink_config.max_pending_bytes,
            )?;

            let (lbd_sender, lbd_receiver) = crossbeam::channel::bounded(8);
            let (ifcowl_sender, ifcowl_receiver) = crossbeam::channel::bounded(8);
            let emit_ifcowl = settings.emit_ifcowl;
            let (producer_result_sender, producer_result_receiver) =
                crossbeam::channel::bounded::<Result<(), lbd_serializer::SerializerError>>(1);

            rayon::spawn(move || {
                let result = if emit_ifcowl {
                    stream_step_and_model(
                        &step,
                        &model,
                        &options,
                        &lbd_sender,
                        Some(&ifcowl_sender),
                    )
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

            producer_result_receiver.recv().map_err(|_| {
                lbd_serializer::SerializerError::Io(io::ErrorKind::BrokenPipe.into())
            })??;

            let (summary, peak, chunk_size) = writer.finish()?;
            Ok((vec![summary], peak, chunk_size))
        }
    }
}

fn export_browser_files(
    conversion: &lbd_converter::ConversionResult,
    base_uri: &str,
    settings: &ExecutionSettings,
) -> Result<Vec<ExportedFile>, lbd_serializer::SerializerError> {
    match settings.output_format {
        OutputFormat::Turtle => {
            let mut lbd_bytes: Vec<u8> = Vec::new();
            serialize_turtle_to_writer(&conversion.triples, &mut lbd_bytes)?;
            let mut files = vec![ExportedFile {
                filename: format!("{}.ttl", settings.output_stem),
                mime_type: "text/turtle;charset=utf-8".to_string(),
                role: "lbd".to_string(),
                payload: lbd_bytes,
            }];
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
            Ok(vec![ExportedFile {
                filename: format!("{}.nq", settings.output_stem),
                mime_type: "application/n-quads".to_string(),
                role: "merged".to_string(),
                payload: nq_bytes,
            }])
        }
    }
}
