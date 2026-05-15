use std::collections::{HashMap, HashSet};

use crate::types::{
    ConversionRequest, ExecutionSettings, NquadsChunkingMode, NquadsModuleOptions, OutputFormats,
    TurtleGrouping, TurtleLayout, WasmApiError,
};
use lbd_pipeline::ActivationPlan;
use lbd_pipeline::{
    BEO_PRODUCER_ID, BOT_PRODUCER_ID, FILE_EXPORT_ID, IFCOWL_PRODUCER_ID,
    IFC_TOPOLOGY_PRODUCER_ID, NQUADS_CHUNKED_SERIALIZER_ID,
    NQUADS_SERIALIZER_ID, OMG_FOG_PRODUCER_ID, PROPS_OPM_PRODUCER_ID,
    TOPOLOGY_FULL_PRODUCER_ID, TURTLE_SERIALIZER_ID,
};

pub(crate) fn normalize_base_for_graph_iri(base_uri: &str) -> String {
    base_uri.trim_end_matches('/').to_string()
}

pub(crate) fn dedupe_modules(ids: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        if seen.insert(id.clone()) {
            out.push(id);
        }
    }
    out
}

pub(crate) fn validate_activation_plan(plan: &ActivationPlan) -> Result<(), WasmApiError> {
    let active: HashSet<&str> = plan.enabled_ids.iter().map(|id| id.as_str()).collect();
    let has_any_producer = active.contains(BOT_PRODUCER_ID)
        || active.contains(BEO_PRODUCER_ID)
        || active.contains(PROPS_OPM_PRODUCER_ID)
        || active.contains(OMG_FOG_PRODUCER_ID)
        || active.contains(IFCOWL_PRODUCER_ID)
        || active.contains(IFC_TOPOLOGY_PRODUCER_ID);
    if !has_any_producer {
        return Err(WasmApiError::Message(format!(
            "module plan must include at least one producer (`{}`, `{}`, `{}`, `{}`, …)",
            BOT_PRODUCER_ID, BEO_PRODUCER_ID, PROPS_OPM_PRODUCER_ID, IFCOWL_PRODUCER_ID
        )));
    }
    if !active.contains(FILE_EXPORT_ID) {
        return Err(WasmApiError::Message(format!(
            "module plan must include `{}`",
            FILE_EXPORT_ID
        )));
    }
    let has_nquads =
        active.contains(NQUADS_SERIALIZER_ID) || active.contains(NQUADS_CHUNKED_SERIALIZER_ID);
    let has_turtle = active.contains(TURTLE_SERIALIZER_ID);
    if !has_nquads && !has_turtle {
        return Err(WasmApiError::Message(
            "module plan must include at least one serializer".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn resolve_execution_settings(
    plan: &ActivationPlan,
    configs: &HashMap<String, HashMap<String, String>>,
    request: &ConversionRequest,
    warnings: &mut Vec<String>,
) -> Result<ExecutionSettings, WasmApiError> {
    let active: HashSet<&str> = plan.enabled_ids.iter().map(|id| id.as_str()).collect();
    let output_formats = OutputFormats {
        turtle: active.contains(TURTLE_SERIALIZER_ID),
        nquads: active.contains(NQUADS_SERIALIZER_ID),
        nquads_chunked: active.contains(NQUADS_CHUNKED_SERIALIZER_ID),
    };
    if output_formats.is_empty() {
        return Err(WasmApiError::Message(
            "module plan must include at least one serializer".to_string(),
        ));
    }

    // Chunking config: check both neo-nquads-serializer and neo-nquads-chunked-serializer
    let nquads_entries = configs.get(NQUADS_SERIALIZER_ID);
    let chunked_entries = configs.get(NQUADS_CHUNKED_SERIALIZER_ID);
    // If the chunked serializer is active, its options take priority
    let effective_nquads_entries = if active.contains(NQUADS_CHUNKED_SERIALIZER_ID) {
        chunked_entries.or(nquads_entries)
    } else {
        nquads_entries
    };
    let nquads_chunking_str = effective_nquads_entries
        .and_then(|m| m.get("chunking"))
        .cloned()
        .unwrap_or_else(|| {
            if active.contains(NQUADS_CHUNKED_SERIALIZER_ID) {
                "lines".to_string() // default for chunked serializer
            } else {
                "none".to_string()
            }
        });
    let nquads_chunking = match nquads_chunking_str.as_str() {
        "none" => NquadsChunkingMode::None,
        "lines" => NquadsChunkingMode::Lines,
        "bytes" => NquadsChunkingMode::Bytes,
        // "cores" mode maps to "lines" in WASM (no thread-based round-robin in browser)
        "cores" => {
            warnings.push("neo-nquads-serializer.chunking=cores maps to lines in WASM (no thread round-robin in browser)".to_string());
            NquadsChunkingMode::Lines
        }
        other => {
            return Err(WasmApiError::Message(format!(
                "invalid `neo-nquads-serializer.chunking={}` (expected none|lines|bytes|cores)",
                other
            )));
        }
    };
    let chunk_size_lines = effective_nquads_entries
        .and_then(|m| m.get("chunk_size_lines"))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2_000_000);
    let chunk_size_bytes = effective_nquads_entries
        .and_then(|m| m.get("chunk_size_bytes"))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(268_435_456);
    let chunk_prefix = effective_nquads_entries
        .and_then(|m| m.get("chunk_prefix"))
        .cloned()
        .unwrap_or_else(|| "out".to_string());

    let output_stem = configs
        .get(FILE_EXPORT_ID)
        .and_then(|m| m.get("output_stem"))
        .cloned()
        .or_else(|| request.output_stem.clone())
        .unwrap_or_else(|| "output".to_string());

    let turtle_entries = configs.get(TURTLE_SERIALIZER_ID);
    let turtle_grouping = match turtle_entries
        .and_then(|m| m.get("grouping"))
        .map(String::as_str)
        .unwrap_or("sorted")
    {
        "sorted" => TurtleGrouping::Sorted,
        "streaming" => TurtleGrouping::Streaming,
        other => {
            return Err(WasmApiError::Message(format!(
                "invalid `neo-turtle-serializer.grouping={}` (expected sorted|streaming)",
                other
            )));
        }
    };
    let turtle_layout = match turtle_entries
        .and_then(|m| m.get("layout"))
        .map(String::as_str)
        .unwrap_or("joined")
    {
        "joined" => TurtleLayout::Joined,
        "separate" => TurtleLayout::Separate,
        other => {
            return Err(WasmApiError::Message(format!(
                "invalid `neo-turtle-serializer.layout={}` (expected joined|separate)",
                other
            )));
        }
    };

    Ok(ExecutionSettings {
        output_formats,
        // Modular LBD producers — add new producers here as emit_<name> flags
        emit_bot: active.contains(BOT_PRODUCER_ID),
        emit_beo: active.contains(BEO_PRODUCER_ID),
        emit_props_opm: active.contains(PROPS_OPM_PRODUCER_ID),
        emit_omg_fog: active.contains(OMG_FOG_PRODUCER_ID),
        emit_ifcowl: active.contains(IFCOWL_PRODUCER_ID),
        emit_topology: active.contains(IFC_TOPOLOGY_PRODUCER_ID),
        emit_bbox: active.contains(lbd_pipeline::BBOX_ENRICHER_ID),
        emit_full_topology: active.contains(lbd_pipeline::TOPOLOGY_FULL_PRODUCER_ID),
        nquads: NquadsModuleOptions {
            chunking: nquads_chunking,
            chunk_size_lines,
            chunk_size_bytes,
            chunk_prefix,
        },
        output_stem,
        turtle_grouping,
        turtle_layout,
    })
}

pub(crate) fn parse_module_configs(
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

pub(crate) fn validate_module_configs(
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

pub(crate) fn validate_typed_module_configs(
    configs: &HashMap<String, HashMap<String, String>>,
) -> Result<(), WasmApiError> {
    for (module_id, entries) in configs {
        match module_id.as_str() {
            NQUADS_SERIALIZER_ID => validate_nquads_serializer_options(entries)?,
            NQUADS_CHUNKED_SERIALIZER_ID => validate_nquads_chunked_serializer_options(entries)?,
            TURTLE_SERIALIZER_ID => validate_turtle_serializer_options(entries)?,
            FILE_EXPORT_ID => validate_file_export_options(entries)?,
            BOT_PRODUCER_ID
            | BEO_PRODUCER_ID
            | PROPS_OPM_PRODUCER_ID
            | OMG_FOG_PRODUCER_ID
            | IFCOWL_PRODUCER_ID
            | IFC_TOPOLOGY_PRODUCER_ID
            | lbd_pipeline::BBOX_ENRICHER_ID => {
                if !entries.is_empty() {
                    return Err(WasmApiError::Message(format!(
                        "module `{}` does not support options",
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

pub(crate) fn validate_turtle_serializer_options(
    entries: &HashMap<String, String>,
) -> Result<(), WasmApiError> {
    for (key, value) in entries {
        match key.as_str() {
            "grouping" => {
                if !matches!(value.as_str(), "sorted" | "streaming") {
                    return Err(WasmApiError::Message(format!(
                        "`neo-turtle-serializer.grouping` must be one of sorted|streaming, got `{}`",
                        value
                    )));
                }
            }
            "layout" => {
                if !matches!(value.as_str(), "joined" | "separate") {
                    return Err(WasmApiError::Message(format!(
                        "`neo-turtle-serializer.layout` must be one of joined|separate, got `{}`",
                        value
                    )));
                }
            }
            other => {
                return Err(WasmApiError::Message(format!(
                    "unknown option `neo-turtle-serializer.{}` (supported: grouping, layout)",
                    other
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_nquads_serializer_options(
    entries: &HashMap<String, String>,
) -> Result<(), WasmApiError> {
    if !entries.is_empty() {
        return Err(WasmApiError::Message(
            "neo-nquads-serializer has no configurable options (chunking options belong on neo-nquads-chunked-serializer)".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_nquads_chunked_serializer_options(
    entries: &HashMap<String, String>,
) -> Result<(), WasmApiError> {
    let allowed = [
        "chunking",
        "chunk_size_lines",
        "chunk_size_bytes",
        "chunk_prefix",
    ];
    for (key, value) in entries {
        if !allowed.contains(&key.as_str()) {
            return Err(WasmApiError::Message(format!(
                "unsupported option `neo-nquads-chunked-serializer.{}`",
                key
            )));
        }
        if matches!(key.as_str(), "chunk_size_lines" | "chunk_size_bytes") {
            value.parse::<usize>().map_err(|_| {
                WasmApiError::Message(format!(
                    "invalid integer for `neo-nquads-chunked-serializer.{}`: `{}`",
                    key, value
                ))
            })?;
        }
        if key == "chunking" && !matches!(value.as_str(), "none" | "lines" | "bytes" | "cores") {
            return Err(WasmApiError::Message(format!(
                "invalid `neo-nquads-chunked-serializer.chunking={}` (expected none|lines|bytes|cores)",
                value
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_file_export_options(
    entries: &HashMap<String, String>,
) -> Result<(), WasmApiError> {
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
