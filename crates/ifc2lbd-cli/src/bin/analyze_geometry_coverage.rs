#![allow(unused_imports)]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use ifc_geometry::{
    analyze_geometry_coverage, GeometryCoverageEntry, GeometryCoverageStatus, GeometryMissingReason,
};
use ifc_model::build_model;
use ifc_step::parse_step_file;

#[derive(Debug, Parser)]
#[command(name = "analyze-geometry-coverage")]
#[command(about = "Report which selected IFC elements are missing from the geometry pipeline and why")]
struct Args {
    input: PathBuf,

    #[arg(long, default_value_t = 20)]
    sample_limit: usize,

    #[arg(long)]
    only_missing: bool,

    #[arg(long)]
    missing_out: Option<PathBuf>,

    #[arg(long)]
    contains: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let input = args.input.canonicalize().with_context(|| {
        format!("failed to resolve input path {}", args.input.display())
    })?;
    let step = parse_step_file(&input)
        .with_context(|| format!("failed to parse STEP file {}", input.display()))?;
    let model = build_model(&step).context("failed to build IFC model")?;
    let ifc_content = Arc::new(
        fs::read_to_string(&input)
            .with_context(|| format!("failed to read IFC content {}", input.display()))?,
    );

    let mut element_ids: Vec<u64> = model
        .elements
        .keys()
        .copied()
        .filter(|id| {
            !step.entities
                .get(id)
                .map(|e| e.entity_name == "IFCOPENINGELEMENT")
                .unwrap_or(false)
        })
        .collect();

    for id in model.spatial_nodes.keys().copied() {
        if let Some(e) = step.entities.get(&id) {
            if e.entity_name != "IFCPROJECT" {
                element_ids.push(id);
            }
        }
    }

    element_ids.sort_unstable();
    element_ids.dedup();

    let mut entries = analyze_geometry_coverage(ifc_content, &element_ids);

    if let Some(needle) = args.contains.as_deref() {
        let needle = needle.to_ascii_lowercase();
        entries.retain(|entry| {
            entry.category.to_ascii_lowercase().contains(&needle)
                || entry.guid.to_ascii_lowercase().contains(&needle)
                || reason_label(&entry.status)
                    .to_ascii_lowercase()
                    .contains(&needle)
        });
    }

    print_summary(&entries, args.sample_limit, args.only_missing);

    if let Some(path) = args.missing_out {
        write_missing_report(&path, &entries)?;
        println!("missing_report={}", path.display());
    }

    Ok(())
}

fn print_summary(entries: &[GeometryCoverageEntry], sample_limit: usize, only_missing: bool) {
    let with_geometry = entries
        .iter()
        .filter(|entry| matches!(entry.status, GeometryCoverageStatus::HasGeometry { .. }))
        .count();
    let missing: Vec<&GeometryCoverageEntry> = entries
        .iter()
        .filter(|entry| matches!(entry.status, GeometryCoverageStatus::Missing { .. }))
        .collect();

    println!("selected_elements={}", entries.len());
    println!("with_geometry={}", with_geometry);
    println!("missing={}", missing.len());

    let mut by_category: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_reason: BTreeMap<String, usize> = BTreeMap::new();

    for entry in &missing {
        *by_category.entry(entry.category.clone()).or_default() += 1;
        *by_reason.entry(reason_label(&entry.status).to_string()).or_default() += 1;
    }

    let mut category_rows: Vec<_> = by_category.into_iter().collect();
    category_rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    println!("missing_by_category={}", category_rows.len());
    for (category, count) in category_rows.iter().take(sample_limit) {
        println!("  category\t{}\t{}", count, category);
    }

    let mut reason_rows: Vec<_> = by_reason.into_iter().collect();
    reason_rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    println!("missing_by_reason={}", reason_rows.len());
    for (reason, count) in reason_rows.iter().take(sample_limit) {
        println!("  reason\t{}\t{}", count, reason);
    }

    if only_missing {
        for entry in missing.iter().take(sample_limit) {
            println!(
                "  missing\t#{}\t{}\t{}\t{}",
                entry.express_id,
                entry.category,
                entry.guid,
                reason_detail(&entry.status),
            );
        }
    } else {
        for entry in entries.iter().take(sample_limit) {
            match &entry.status {
                GeometryCoverageStatus::HasGeometry { geometry_count } => {
                    println!(
                        "  ok\t#{}\t{}\t{}\tgeometries={}",
                        entry.express_id, entry.category, entry.guid, geometry_count
                    );
                }
                GeometryCoverageStatus::Missing { .. } => {
                    println!(
                        "  missing\t#{}\t{}\t{}\t{}",
                        entry.express_id,
                        entry.category,
                        entry.guid,
                        reason_detail(&entry.status),
                    );
                }
            }
        }
    }
}

fn write_missing_report(path: &PathBuf, entries: &[GeometryCoverageEntry]) -> anyhow::Result<()> {
    let mut out = String::from("express_id\tcategory\tguid\treason\tdetail\n");
    for entry in entries {
        if let GeometryCoverageStatus::Missing { .. } = &entry.status {
            let detail = reason_detail(&entry.status).replace(['\t', '\n'], " ");
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                entry.express_id,
                entry.category,
                entry.guid,
                reason_label(&entry.status),
                detail,
            ));
        }
    }
    fs::write(path, out)
        .with_context(|| format!("failed to write missing report {}", path.display()))
}

fn reason_label(status: &GeometryCoverageStatus) -> &'static str {
    match status {
        GeometryCoverageStatus::HasGeometry { .. } => "has-geometry",
        GeometryCoverageStatus::Missing { reason } => match reason {
            GeometryMissingReason::DecodeFailed(_) => "decode-failed",
            GeometryMissingReason::EmptySubmeshes => "empty-submeshes",
            GeometryMissingReason::GeometryError(_) => "geometry-error",
        },
    }
}

fn reason_detail(status: &GeometryCoverageStatus) -> String {
    match status {
        GeometryCoverageStatus::HasGeometry { geometry_count } => {
            format!("geometries={geometry_count}")
        }
        GeometryCoverageStatus::Missing { reason } => match reason {
            GeometryMissingReason::DecodeFailed(msg) => msg.clone(),
            GeometryMissingReason::EmptySubmeshes => "no submeshes produced".to_string(),
            GeometryMissingReason::GeometryError(msg) => msg.clone(),
        },
    }
}
