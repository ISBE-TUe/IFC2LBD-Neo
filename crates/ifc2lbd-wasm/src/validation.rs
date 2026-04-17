use std::collections::{HashMap, HashSet};

use crate::types::{
    ConversionRequest, ExecutionSettings, NquadsModuleOptions, OutputFormat, TurtleGrouping,
    WasmApiError,
};
use lbd_pipeline::ActivationPlan;
use lbd_pipeline::{
    FILE_EXPORT_ID, IFCOWL_PRODUCER_ID, LBD_PRODUCER_ID, NQUADS_SERIALIZER_ID, TURTLE_SERIALIZER_ID,
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

pub(crate) fn resolve_execution_settings(
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
        turtle_grouping,
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
            TURTLE_SERIALIZER_ID => validate_turtle_serializer_options(entries)?,
            FILE_EXPORT_ID => validate_file_export_options(entries)?,
            LBD_PRODUCER_ID | IFCOWL_PRODUCER_ID => {
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
            other => {
                return Err(WasmApiError::Message(format!(
                    "unknown option `neo-turtle-serializer.{}` (supported: grouping)",
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
