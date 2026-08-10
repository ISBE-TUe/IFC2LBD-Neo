//! QTO validation harness.
//!
//! Scores a QTO backend against the quantities **already authored** in an IFC
//! file by its exporter. Those quantities are never modified by the converter,
//! which makes them a free ground-truth corpus: strip them from a copy of the
//! model, ask the backend to recompute them, and compare.
//!
//! This is a dev tool. It is not part of the conversion pipeline and never
//! writes to the model that gets converted.
//!
//! Caveats that shape how the output should be read:
//!   * Authored quantities are not infallible — exporters disagree, some are
//!     stale relative to the geometry, and measurement conventions differ. The
//!     report gives distributions and outliers so disagreement prompts an
//!     investigation rather than an automatic verdict.
//!   * A quantity the backend does not attempt is a *coverage* result, not an
//!     accuracy one. The two are reported separately and must not be conflated.

mod names;
mod report;
mod representation;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use ifc_model::IfcModel;
use ifc_step::{parse_step_file, EntityId, StepFile, StepValue};
use lbd_pipeline::{PipelineContext, PreprocessPlugin, ResourceLimits};
use plugin_qto_preprocess::{QtoOptions, QtoPreprocessPlugin};

use report::{Comparison, Outcome, Report};
use representation::classify;
use plugin_qto_preprocess::units::{self, Dimension};

#[derive(Parser)]
#[command(
    name = "qto-validate",
    about = "Score QTO computation against the quantities already authored in IFC files"
)]
struct Args {
    /// IFC files to score.
    #[arg(required = true)]
    files: Vec<PathBuf>,

    /// Write the full per-quantity results as JSON.
    #[arg(long)]
    json: Option<PathBuf>,

    /// How many worst-error outliers to list per quantity kind.
    #[arg(long, default_value_t = 5)]
    top: usize,

    /// Relative error at or below which a value counts as matching.
    #[arg(long, default_value_t = 0.001)]
    tolerance: f64,

    /// Skip Tier-3 mesh volume (much faster on large models).
    #[arg(long)]
    no_mesh: bool,
}

fn main() {
    let args = Args::parse();
    let mut report = Report::new(args.tolerance);

    for path in &args.files {
        eprintln!("── {} ─────────────", path.display());
        match score_file(path, &args) {
            Ok(comparisons) => {
                eprintln!("   {} comparable quantities", comparisons.len());
                report.add(path.display().to_string(), comparisons);
            }
            Err(e) => eprintln!("   SKIPPED: {e}"),
        }
    }

    report.print(args.top);

    if let Some(json_path) = &args.json {
        match report.write_json(json_path) {
            Ok(()) => eprintln!("\nwrote {}", json_path.display()),
            Err(e) => eprintln!("\nfailed to write JSON: {e}"),
        }
    }
}

/// An authored quantity, as found in the file.
struct Authored {
    element_id: EntityId,
    guid: String,
    ifc_type: String,
    set_name: String,
    name: String,
    /// IFC entity (IFCQUANTITYLENGTH/AREA/VOLUME) — authoritative for dimension.
    entity_name: String,
    value: f64,
}

fn score_file(path: &PathBuf, args: &Args) -> Result<Vec<Comparison>, String> {
    let step = parse_step_file(path).map_err(|e| format!("parse failed: {e:?}"))?;
    let model = IfcModel::from_step_file(&step).map_err(|e| format!("model build failed: {e:?}"))?;

    let scales = units::scales_for(&model)?;
    let authored = collect_authored(&model);
    if authored.is_empty() {
        return Err("no authored quantities in this file — nothing to score".into());
    }
    eprintln!(
        "   {} elements, {} authored quantities  (to SI: length x{}, area x{}, volume x{})",
        model.elements.len(),
        authored.len(),
        scales.length,
        scales.area,
        scales.volume
    );

    // Classify representations before stripping — classification reads the STEP
    // file, but doing it once here keeps the hot loop cheap.
    let mut rep_by_element: BTreeMap<EntityId, String> = BTreeMap::new();
    for a in &authored {
        rep_by_element
            .entry(a.element_id)
            .or_insert_with(|| classify(&step, a.element_id).to_string());
    }

    // `step` is moved into the pipeline context from here on; all reads of it
    // (classification above) must already have happened.
    let computed = recompute_with_quantities_stripped(step, &model, args)?;

    let mut out = Vec::with_capacity(authored.len());
    for a in authored {
        let key = (a.element_id, a.name.clone());
        let representation = rep_by_element
            .get(&a.element_id)
            .cloned()
            .unwrap_or_else(|| "unclassified".into());

        let dim = Dimension::from_quantity_entity(&a.entity_name);

        // Both sides are in the model's declared quantity unit, so they compare
        // directly. The backend converting correctly is precisely what makes this
        // true, so a regression shows up as an error close to `unit_factor` —
        // detected below rather than silently absorbed.
        let outcome = match computed.get(&key) {
            None => Outcome::NotComputed,
            Some(&value) => {
                let err = relative_error(a.value, value);
                let unit_factor = scales.geometry_to_quantity_factor(dim);
                Outcome::Computed {
                    value,
                    relative_error: err,
                    looks_like_unit_error: looks_like_unit_error(a.value, value, unit_factor),
                    unit_factor,
                }
            }
        };

        out.push(Comparison {
            file: path.display().to_string(),
            guid: a.guid,
            ifc_type: a.ifc_type,
            representation,
            set_name: a.set_name,
            standard: names::is_standard_quantity(&a.name),
            quantity: a.name,
            authored: a.value,
            outcome,
        });
    }
    Ok(out)
}

/// Whether a disagreement is explained by the backend having skipped unit
/// conversion — i.e. computed/authored lands on the geometry-to-quantity factor.
///
/// This is a regression detector: once the backend converts correctly this must
/// stay at zero, and any reappearance points at the conversion rather than at the
/// geometry.
fn looks_like_unit_error(authored: f64, computed: f64, unit_factor: f64) -> bool {
    if (unit_factor - 1.0).abs() < 1e-12 || authored.abs() < 1e-12 {
        return false;
    }
    let ratio = computed / authored;
    // Within 1% of the factor, in either direction.
    (ratio / (1.0 / unit_factor) - 1.0).abs() < 0.01 || (ratio * unit_factor - 1.0).abs() < 0.01
}

/// Depth-first search for a key anywhere in a JSON tree.
fn find_key<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => {
            if let Some(found) = m.get(key) {
                return Some(found);
            }
            m.values().find_map(|x| find_key(x, key))
        }
        serde_json::Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

/// Relative error, falling back to absolute when the authored value is ~zero.
fn relative_error(authored: f64, computed: f64) -> f64 {
    if authored.abs() > 1e-9 {
        (computed - authored).abs() / authored.abs()
    } else {
        (computed - authored).abs()
    }
}

fn collect_authored(model: &IfcModel) -> Vec<Authored> {
    let mut out = Vec::new();
    for (&object_id, set_ids) in &model.quantities_for_object {
        // Only elements — spatial nodes are scored separately once IfcSpace is
        // actually computed by a backend.
        let Some(element) = model.elements.get(&object_id) else {
            continue;
        };
        for set_id in set_ids {
            let Some(set) = model.element_quantities.get(set_id) else {
                continue;
            };
            let set_name = set.name.as_deref().unwrap_or("").to_string();
            for qty_id in &set.quantities {
                let Some(qty) = model.physical_quantities.get(qty_id) else {
                    continue;
                };
                let Some(value) = numeric_value(qty.value.as_ref()) else {
                    continue;
                };
                out.push(Authored {
                    element_id: object_id,
                    guid: element.guid.to_string(),
                    ifc_type: element.entity_name.to_uppercase(),
                    set_name: set_name.clone(),
                    name: qty.name.to_string(),
                    entity_name: qty.entity_name.to_uppercase(),
                    value,
                });
            }
        }
    }
    out.sort_by(|a, b| (a.element_id, &a.name).cmp(&(b.element_id, &b.name)));
    out
}

fn numeric_value(v: Option<&StepValue>) -> Option<f64> {
    match v? {
        StepValue::Real(r) => Some(*r),
        StepValue::Int(i) => Some(*i as f64),
        StepValue::Typed { value, .. } => numeric_value(Some(value)),
        _ => None,
    }
}

/// Strip every authored quantity from a copy of the model, then run the QTO
/// plugin over it so it recomputes them all from geometry.
///
/// Going through the plugin's real entry point rather than its internals means
/// the harness measures the production path — audit, tier selection, injection
/// and all — not a reimplementation of it that could differ.
fn recompute_with_quantities_stripped(
    step: StepFile,
    model: &IfcModel,
    args: &Args,
) -> Result<BTreeMap<(EntityId, String), f64>, String> {
    let mut stripped = model.clone();
    stripped.quantities_for_object.clear();
    stripped.element_quantities.clear();
    stripped.physical_quantities.clear();

    let mut ctx = PipelineContext::new(ResourceLimits::default());
    ctx.insert(Arc::new(step));
    ctx.insert(Arc::new(stripped));
    ctx.insert(Arc::new(QtoOptions {
        compute_mesh_volume: !args.no_mesh,
    }));

    QtoPreprocessPlugin
        .preprocess(&mut ctx)
        .map_err(|e| format!("qto preprocess failed: {e:?}"))?;

    // Surface the plugin's own run log, so a backend that is compiled in but
    // never used is visible instead of silently idle.
    let bundle = ctx.read_log_bundle();
    if let Ok(v) = serde_json::to_value(&bundle) {
        if let Some(o) = find_key(&v, "occt") {
            eprintln!("   occt: {o}");
        }
    }

    let result = ctx
        .get::<IfcModel>()
        .ok_or_else(|| "model missing from context after preprocess".to_string())?;

    let mut out = BTreeMap::new();
    for (&object_id, set_ids) in &result.quantities_for_object {
        for set_id in set_ids {
            let Some(set) = result.element_quantities.get(set_id) else {
                continue;
            };
            for qty_id in &set.quantities {
                let Some(qty) = result.physical_quantities.get(qty_id) else {
                    continue;
                };
                if let Some(v) = numeric_value(qty.value.as_ref()) {
                    out.insert((object_id, qty.name.to_string()), v);
                }
            }
        }
    }
    Ok(out)
}
