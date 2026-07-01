#![allow(unused_imports)]

use lbd_pipeline::IFCOWL_PRODUCER_ID;

use crate::types::{ConversionRequest, ExecutionMode, ExecutionSettings};

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

/// Practical WASM linear-memory ceiling in MB.
///
/// The wasm32-unknown-unknown target caps linear memory at
/// 4 294 901 760 bytes (≈ 4096 MB, set via `--max-memory` in
/// `.cargo/config.toml`). We reserve ~900 MB for the WASM runtime,
/// JS engine heap, rayon worker stacks, and the input buffer,
/// leaving this as the usable ceiling for pipeline data structures
/// (triple batches, IfcOWL graph, geometry buffers, serializer
/// buffers).
const WASM_MEMORY_CEILING_MB: u64 = 3200;

/// Safety multiplier applied to the estimated peak before comparing
/// against the ceiling. The `96 + MB × multiplier` estimate is a
/// linear heuristic that does not account for geometry tessellation,
/// channel backpressure, or IfcOWL graph construction overhead. Real
/// peak on complex files can run ~2× above the formula.
const ESTIMATE_SAFETY_FACTOR: u64 = 2;

pub(crate) fn select_execution_mode(
    input_size_bytes: u64,
    request: &ConversionRequest,
    settings: &ExecutionSettings,
) -> (ExecutionMode, u64, u64, String) {
    let input_mb = (input_size_bytes / (1024 * 1024)).max(1);
    let multiplier = if settings.output_formats.has_any_nquads() {
        if settings.has(IFCOWL_PRODUCER_ID) {
            26
        } else {
            16
        }
    } else {
        if settings.has(IFCOWL_PRODUCER_ID) {
            22
        } else {
            14
        }
    };
    let estimated_peak_mb = 96 + input_mb.saturating_mul(multiplier);

    // The feasibility check is the real memory ceiling — either
    // user-provided (they know their environment) or our default
    // based on the ~4 GB WASM linear-memory cap.
    let feasibility_check_mb = request
        .memory_feasibility_mb
        .unwrap_or(WASM_MEMORY_CEILING_MB);

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
            // Apply a safety margin because the linear heuristic
            // underestimates real peak on complex geometry.
            let safe_estimate_mb = estimated_peak_mb.saturating_mul(ESTIMATE_SAFETY_FACTOR);
            if safe_estimate_mb > feasibility_check_mb {
                (
                    ExecutionMode::Lowmem,
                    estimated_peak_mb,
                    feasibility_check_mb,
                    format!(
                        "auto selected lowmem: estimated peak {estimated_peak_mb} MB \
                         (×{ESTIMATE_SAFETY_FACTOR} safety = {safe_estimate_mb} MB) \
                         exceeds ceiling {feasibility_check_mb} MB"
                    ),
                )
            } else {
                (
                    ExecutionMode::Fast,
                    estimated_peak_mb,
                    feasibility_check_mb,
                    format!(
                        "auto selected fast: estimated peak {estimated_peak_mb} MB \
                         (×{ESTIMATE_SAFETY_FACTOR} safety = {safe_estimate_mb} MB) \
                         within ceiling {feasibility_check_mb} MB"
                    ),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ExecutionSettings, NquadsChunkingMode, NquadsGraphNaming, NquadsModuleOptions,
        OutputFormats, TurtleGrouping, TurtleLayout,
    };
    use lbd_converter::IfcowlMode;

    fn settings(nquads: bool, ifcowl: bool) -> ExecutionSettings {
        let mut active = std::collections::HashSet::new();
        if ifcowl {
            active.insert(IFCOWL_PRODUCER_ID.to_string());
        }
        ExecutionSettings {
            active_plugin_ids: active,
            output_formats: OutputFormats {
                turtle: !nquads,
                nquads,
                nquads_chunked: false,
            },
            module_configs: std::collections::HashMap::new(),
            nquads: NquadsModuleOptions {
                chunking: NquadsChunkingMode::None,
                chunk_size_lines: 10_000,
                chunk_size_bytes: 4 * 1024 * 1024,
                chunk_prefix: "chunk".to_string(),
                graph_naming: NquadsGraphNaming::Producers,
            },
            output_stem: "model".to_string(),
            turtle_grouping: TurtleGrouping::Sorted,
            turtle_layout: TurtleLayout::Joined,
            ifcowl_mode: IfcowlMode::Full,
            bsdd_profile: None,
            bsdd_compact: false,
            bsdd_include_standard_attrs: false,
            bsdd_dedup_properties: false,
        }
    }

    fn req(mode: Option<&str>, feasibility: Option<u64>) -> ConversionRequest {
        ConversionRequest {
            module_ids: Vec::new(),
            module_options: Vec::new(),
            base_uri: None,
            output_stem: None,
            execution_mode: mode.map(String::from),
            memory_feasibility_mb: feasibility,
            stream_batch_size: None,
            ifcowl_max_workers: None,
            sink_chunk_size_bytes: None,
            sink_max_pending_bytes: None,
            input_format: None,
            structured_data_files: Vec::new(),
        }
    }

    #[test]
    fn auto_fast_for_small_files() {
        // 5 MB, turtle only, multiplier 14 → estimate 166 MB, safe 332 MB < 3200
        let (mode, _, _, _) =
            select_execution_mode(5 * 1024 * 1024, &req(None, None), &settings(false, false));
        assert_eq!(mode, ExecutionMode::Fast);
    }

    #[test]
    fn auto_lowmem_for_large_nquads_ifcowl() {
        // 70 MB, nquads + ifcowl, multiplier 26 → estimate 1916 MB, safe 3832 MB > 3200
        let (mode, est, _, reason) =
            select_execution_mode(70 * 1024 * 1024, &req(None, None), &settings(true, true));
        assert_eq!(mode, ExecutionMode::Lowmem);
        assert_eq!(est, 1916);
        assert!(reason.contains("lowmem"));
    }

    #[test]
    fn auto_fast_for_large_turtle_only() {
        // 70 MB, turtle only, multiplier 14 → estimate 1076 MB, safe 2152 MB < 3200
        let (mode, _, _, _) =
            select_execution_mode(70 * 1024 * 1024, &req(None, None), &settings(false, false));
        assert_eq!(mode, ExecutionMode::Fast);
    }

    #[test]
    fn explicit_fast_overrides_auto() {
        // Would auto-select Lowmem, but explicit "fast" wins
        let (mode, _, _, _) = select_execution_mode(
            70 * 1024 * 1024,
            &req(Some("fast"), None),
            &settings(true, true),
        );
        assert_eq!(mode, ExecutionMode::Fast);
    }

    #[test]
    fn explicit_lowmem_overrides_auto() {
        // Would auto-select Fast, but explicit "lowmem" wins
        let (mode, _, _, _) = select_execution_mode(
            5 * 1024 * 1024,
            &req(Some("lowmem"), None),
            &settings(false, false),
        );
        assert_eq!(mode, ExecutionMode::Lowmem);
    }

    #[test]
    fn user_feasibility_override_respected() {
        // User says ceiling is 200 MB → even a 5 MB file exceeds it with safety factor
        let (mode, _, _, _) = select_execution_mode(
            5 * 1024 * 1024,
            &req(None, Some(200)),
            &settings(false, false),
        );
        assert_eq!(mode, ExecutionMode::Lowmem);
    }
}
