use std::path::Path;
use std::sync::Arc;

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
}

pub(crate) struct TopologyExecutionOutput {
    pub enable_topology_extension: bool,
    pub geometry_relations: Option<Arc<Vec<GeometryRelation>>>,
}

type TopologyExecutorFn = fn(&TopologyExecutionContext<'_>) -> anyhow::Result<TopologyExecutionOutput>;

struct TopologyExecutor {
    plugin_id: &'static str,
    requires_geometry_relations: bool,
    execute: TopologyExecutorFn,
}

const TOPOLOGY_EXECUTORS: &[TopologyExecutor] = &[
    TopologyExecutor {
        plugin_id: crate::pipeline_plugins::TOPOLOGY_LITE_PRODUCER_ID,
        requires_geometry_relations: false,
        execute: execute_topology_lite,
    },
    TopologyExecutor {
        plugin_id: crate::pipeline_plugins::TOPOLOGY_FULL_PRODUCER_ID,
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
        .ok_or_else(|| anyhow::anyhow!("no topology producer executor is registered for plugin `{}`", plugin_id))?;
    (executor.execute)(context)
}

pub(crate) fn plugin_requires_geometry_relations(plugin_id: &str) -> bool {
    TOPOLOGY_EXECUTORS
        .iter()
        .find(|executor| executor.plugin_id == plugin_id)
        .map(|executor| executor.requires_geometry_relations)
        .unwrap_or(false)
}

fn execute_topology_lite(_context: &TopologyExecutionContext<'_>) -> anyhow::Result<TopologyExecutionOutput> {
    Ok(TopologyExecutionOutput {
        enable_topology_extension: false,
        geometry_relations: None,
    })
}

fn execute_topology_full(
    context: &TopologyExecutionContext<'_>,
) -> anyhow::Result<TopologyExecutionOutput> {
    let relations = run_full_topology_plugin(
        context.model,
        context.step,
        context.input_path,
        context.geometry_tolerance,
        context.bbox_inflation_threshold,
        context.bbox_report_path,
        context.write_report,
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
            )?;
            Ok((relations, report))
        },
    )
    .context("full topology plugin execution failed")?;
    Ok(result.relations)
}
