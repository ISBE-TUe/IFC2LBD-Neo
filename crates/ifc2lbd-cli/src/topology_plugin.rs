use std::path::Path;
use std::sync::Arc;
use std::{collections::HashMap, time::Duration};

use anyhow::Context;
use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_geometry::GeometryRelation;

pub(crate) struct TopologyExecutionContext<'a> {
    pub model: &'a IfcModel,
    pub step: &'a StepFile,
    pub input_path: &'a Path,
    pub geometry_tolerance: f64,
    pub bbox_inflation_threshold: f64,
    pub bbox_report_path: Option<&'a Path>,
    pub write_report: bool,
    pub module_config: Option<&'a HashMap<String, String>>,
}

pub(crate) struct TopologyExecutionOutput {
    pub enable_topology_extension: bool,
    pub geometry_relations: Option<Arc<Vec<GeometryRelation>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct TopologyExecutionTuning {
    pub timeout: Duration,
    pub max_pairs_per_batch: usize,
}

type TopologyExecutorFn =
    fn(&TopologyExecutionContext<'_>) -> anyhow::Result<TopologyExecutionOutput>;

struct TopologyExecutor {
    plugin_id: &'static str,
    requires_geometry_relations: bool,
    execute: TopologyExecutorFn,
}

const TOPOLOGY_EXECUTORS: &[TopologyExecutor] = &[
    TopologyExecutor {
        plugin_id: lbd_pipeline::TOPOLOGY_LITE_PRODUCER_ID,
        requires_geometry_relations: false,
        execute: execute_topology_lite,
    },
    TopologyExecutor {
        plugin_id: lbd_pipeline::TOPOLOGY_FULL_PRODUCER_ID,
        requires_geometry_relations: true,
        execute: execute_topology_full,
    },
];

pub(crate) fn run_topology_plugin(
    plugin_id: &str,
    context: &TopologyExecutionContext<'_>,
) -> anyhow::Result<TopologyExecutionOutput> {
    let executor = TOPOLOGY_EXECUTORS
        .iter()
        .find(|executor| executor.plugin_id == plugin_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no topology producer executor is registered for plugin `{}`",
                plugin_id
            )
        })?;
    (executor.execute)(context)
}

pub(crate) fn plugin_requires_geometry_relations(plugin_id: &str) -> bool {
    TOPOLOGY_EXECUTORS
        .iter()
        .find(|executor| executor.plugin_id == plugin_id)
        .map(|executor| executor.requires_geometry_relations)
        .unwrap_or(false)
}

fn execute_topology_lite(
    _context: &TopologyExecutionContext<'_>,
) -> anyhow::Result<TopologyExecutionOutput> {
    Ok(TopologyExecutionOutput {
        enable_topology_extension: false,
        geometry_relations: None,
    })
}

fn execute_topology_full(
    context: &TopologyExecutionContext<'_>,
) -> anyhow::Result<TopologyExecutionOutput> {
    let tuning = parse_topology_full_module_config(context.module_config)
        .map_err(|error| anyhow::anyhow!(error))?;
    let relations = run_full_topology_plugin(
        context.model,
        context.step,
        context.input_path,
        context.geometry_tolerance,
        context.bbox_inflation_threshold,
        context.bbox_report_path,
        context.write_report,
        &tuning,
    )?;
    Ok(TopologyExecutionOutput {
        enable_topology_extension: true,
        geometry_relations: Some(relations),
    })
}

fn run_full_topology_plugin(
    model: &IfcModel,
    step: &StepFile,
    input_path: &Path,
    geometry_tolerance: f64,
    bbox_inflation_threshold: f64,
    bbox_report_path: Option<&Path>,
    write_report: bool,
    tuning: &TopologyExecutionTuning,
) -> anyhow::Result<Arc<Vec<GeometryRelation>>> {
    let result = plugin_topology_full::run_full_topology_plugin(
        model,
        step,
        input_path,
        geometry_tolerance,
        bbox_inflation_threshold,
        bbox_report_path,
        write_report,
        |model, step, input_path, geometry_tolerance, bbox_inflation_threshold| {
            let (relations, _mesh_bboxes, _mesh_wkts, report) = crate::topology_full_occ_relations(
                model,
                step,
                input_path,
                geometry_tolerance,
                bbox_inflation_threshold,
                tuning.timeout,
                tuning.max_pairs_per_batch,
            )?;
            Ok((relations, report))
        },
    )
    .context("full topology plugin execution failed")?;
    Ok(result.relations)
}

pub(crate) fn validate_typed_module_config(
    module_id: &str,
    entries: &HashMap<String, String>,
) -> Result<(), String> {
    match module_id {
        lbd_pipeline::TOPOLOGY_FULL_PRODUCER_ID => {
            parse_topology_full_module_config(Some(entries)).map(|_| ())
        }
        _ => Ok(()),
    }
}

fn parse_topology_full_module_config(
    entries: Option<&HashMap<String, String>>,
) -> Result<TopologyExecutionTuning, String> {
    let mut timeout_secs: u64 = 600;
    let mut max_pairs_per_batch: usize = 50_000;

    let Some(entries) = entries else {
        return Ok(TopologyExecutionTuning {
            timeout: Duration::from_secs(timeout_secs),
            max_pairs_per_batch,
        });
    };

    for key in entries.keys() {
        match key.as_str() {
            "kernel_timeout_secs" | "max_pairs_per_batch" => {}
            _ => {
                return Err(format!(
                    "unknown config key `{}` for `{}`; allowed keys: kernel_timeout_secs, max_pairs_per_batch",
                    key,
                    lbd_pipeline::TOPOLOGY_FULL_PRODUCER_ID
                ));
            }
        }
    }

    if let Some(raw) = entries.get("kernel_timeout_secs") {
        timeout_secs = raw
            .parse::<u64>()
            .map_err(|_| format!("kernel_timeout_secs must be an integer, got `{}`", raw))?;
        if timeout_secs == 0 {
            return Err("kernel_timeout_secs must be > 0".to_string());
        }
    }
    if let Some(raw) = entries.get("max_pairs_per_batch") {
        max_pairs_per_batch = raw
            .parse::<usize>()
            .map_err(|_| format!("max_pairs_per_batch must be an integer, got `{}`", raw))?;
        if max_pairs_per_batch == 0 {
            return Err("max_pairs_per_batch must be > 0".to_string());
        }
    }

    Ok(TopologyExecutionTuning {
        timeout: Duration::from_secs(timeout_secs),
        max_pairs_per_batch,
    })
}
