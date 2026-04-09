use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use ifc_model::IfcModel;
use ifc_step::StepFile;
use lbd_geometry::GeometryRelation;

#[derive(Debug, Clone)]
pub struct FullTopologyPluginResult<R> {
    pub relations: Arc<Vec<GeometryRelation>>,
    pub report: R,
}

pub fn run_full_topology_plugin<R>(
    model: &IfcModel,
    step: &StepFile,
    input_path: &Path,
    geometry_tolerance: f64,
    bbox_inflation_threshold: f64,
    bbox_report_path: Option<&Path>,
    write_report: bool,
    derive_relations_and_report: impl FnOnce(
        &IfcModel,
        &StepFile,
        &Path,
        f64,
        f64,
    ) -> anyhow::Result<(Vec<GeometryRelation>, R)>,
) -> anyhow::Result<FullTopologyPluginResult<R>>
where
    R: serde::Serialize,
{
    let full_start = Instant::now();
    let (relations, report) = derive_relations_and_report(
        model,
        step,
        input_path,
        geometry_tolerance,
        bbox_inflation_threshold,
    )?;
    tracing::info!(
        "topology-full OCC produced {} relations in {:.3}s",
        relations.len(),
        full_start.elapsed().as_secs_f64(),
    );
    if write_report {
        if let Some(path) = bbox_report_path {
            let report_json = serde_json::to_string_pretty(&report)
                .context("failed to serialize bbox report JSON")?;
            std::fs::write(path, report_json)
                .with_context(|| format!("failed to write bbox report {}", path.display()))?;
        }
    }
    Ok(FullTopologyPluginResult {
        relations: Arc::new(relations),
        report,
    })
}
