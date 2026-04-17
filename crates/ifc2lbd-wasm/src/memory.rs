use crate::types::{ConversionRequest, ExecutionMode, ExecutionSettings, OutputFormat};

pub(crate) fn execution_mode_str(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Fast => "fast",
        ExecutionMode::Lowmem => "lowmem",
    }
}

pub(crate) fn effective_stream_batch_size(
    mode: ExecutionMode,
    request: &ConversionRequest,
) -> usize {
    if let Some(explicit) = request.stream_batch_size {
        return explicit.clamp(64, 32 * 1024);
    }
    let threads = rayon::current_num_threads().max(1);
    match mode {
        ExecutionMode::Fast => (threads * 1024).clamp(1024, 32 * 1024),
        ExecutionMode::Lowmem => (threads * 256).clamp(128, 8 * 1024),
    }
}

pub(crate) fn effective_ifcowl_workers(mode: ExecutionMode, request: &ConversionRequest) -> usize {
    if let Some(explicit) = request.ifcowl_max_workers {
        return explicit.clamp(1, 64);
    }
    let threads = rayon::current_num_threads().max(1);
    match mode {
        ExecutionMode::Fast => threads.max(1),
        ExecutionMode::Lowmem => threads.div_ceil(2).max(1),
    }
}

pub(crate) fn select_execution_mode(
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
