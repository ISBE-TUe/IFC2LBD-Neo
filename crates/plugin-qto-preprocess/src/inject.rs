/// Insert computed quantities into a cloned IfcModel.
///
/// For each report: if `existing_set_id` is Some, extend that set's quantities
/// list. If None, create a new ElementQuantity with the standard Qto_* name.
/// Never touches values already present in the source file.

use ifc_model::{ElementQuantity, IfcModel, PhysicalQuantity};
use ifc_step::{EntityId, StepFile, StepValue};
use smol_str::SmolStr;
use uuid::Uuid;

use crate::audit::MissingQuantityReport;
use crate::gate;
use crate::qto_names::QuantityKind;
use crate::units::UnitScales;

/// Computed values for a single element, produced by the geometry pipeline.
#[derive(Debug, Default, Clone)]
pub struct ComputedValues {
    pub length: Option<f64>,
    pub height: Option<f64>,
    pub width: Option<f64>,
    pub depth: Option<f64>,
    pub gross_volume: Option<f64>,
    pub net_volume: Option<f64>,
    pub gross_area: Option<f64>,
    pub net_area: Option<f64>,
    pub area: Option<f64>,
    pub gross_footprint_area: Option<f64>,
    pub net_footprint_area: Option<f64>,
    pub gross_side_area: Option<f64>,
    pub net_side_area: Option<f64>,
    pub gross_floor_area: Option<f64>,
    pub net_floor_area: Option<f64>,
    pub cross_section_area: Option<f64>,
    pub outer_surface_area: Option<f64>,
    pub gross_surface_area: Option<f64>,
    pub net_surface_area: Option<f64>,
    pub gross_perimeter: Option<f64>,
    pub perimeter: Option<f64>,
}

impl ComputedValues {
    pub fn get(&self, kind: QuantityKind) -> Option<f64> {
        match kind {
            QuantityKind::Length => self.length,
            QuantityKind::Height => self.height,
            QuantityKind::Width => self.width,
            QuantityKind::Depth => self.depth,
            QuantityKind::GrossVolume => self.gross_volume,
            QuantityKind::NetVolume => self.net_volume,
            QuantityKind::GrossArea => self.gross_area,
            QuantityKind::NetArea => self.net_area,
            QuantityKind::Area => self.area,
            QuantityKind::GrossFootprintArea => self.gross_footprint_area,
            QuantityKind::NetFootprintArea => self.net_footprint_area,
            QuantityKind::GrossSideArea => self.gross_side_area,
            QuantityKind::NetSideArea => self.net_side_area,
            QuantityKind::GrossFloorArea => self.gross_floor_area,
            QuantityKind::NetFloorArea => self.net_floor_area,
            QuantityKind::CrossSectionArea => self.cross_section_area,
            QuantityKind::OuterSurfaceArea => self.outer_surface_area,
            QuantityKind::GrossSurfaceArea => self.gross_surface_area,
            QuantityKind::NetSurfaceArea => self.net_surface_area,
            QuantityKind::GrossPerimeter => self.gross_perimeter,
            QuantityKind::Perimeter => self.perimeter,

            // Defined by bSDD for one or more IFC classes, but no compute tier
            // produces them yet. `None` means "not computed", which the injector
            // turns into an omission — the correct outcome under the project rule
            // that a missing quantity beats a wrong one.
            //
            // Listed explicitly rather than caught by a wildcard so that adding a
            // tier forces a deliberate decision here instead of silently
            // continuing to omit.
            QuantityKind::NetPerimeter
            | QuantityKind::Thickness
            | QuantityKind::Diameter
            | QuantityKind::InnerDiameter
            | QuantityKind::OuterDiameter
            | QuantityKind::GrossHeight
            | QuantityKind::NetHeight
            | QuantityKind::EavesHeight
            | QuantityKind::FinishCeilingHeight
            | QuantityKind::FinishFloorHeight
            | QuantityKind::PlanLength
            | QuantityKind::Volume
            | QuantityKind::GrossCrossSectionArea
            | QuantityKind::NetCrossSectionArea
            | QuantityKind::GrossCeilingArea
            | QuantityKind::NetCeilingArea
            | QuantityKind::GrossWallArea
            | QuantityKind::NetWallArea
            | QuantityKind::FootprintArea
            | QuantityKind::TotalSurfaceArea
            | QuantityKind::ProjectedArea
            | QuantityKind::PlanArea
            | QuantityKind::SignArea => None,
        }
    }
}

/// Tally of values refused by the gate, for the run log.
#[derive(Debug, Default, Clone, Copy)]
pub struct RejectionCounts {
    pub not_finite: u64,
    pub non_positive: u64,
    pub net_exceeds_gross: u64,
}

impl RejectionCounts {
    fn record(&mut self, r: gate::Rejection) {
        match r {
            gate::Rejection::NotFinite => self.not_finite += 1,
            gate::Rejection::NonPositive => self.non_positive += 1,
            gate::Rejection::NetExceedsGross => self.net_exceeds_gross += 1,
            gate::Rejection::UnresolvableUnits => {}
        }
    }

    pub fn total(&self) -> u64 {
        self.not_finite + self.non_positive + self.net_exceeds_gross
    }
}

/// Apply all reports + their computed values to a cloned model.
///
/// Values arrive in raw geometry units and are converted to the units the model
/// declares before anything is written; see `gate`. Anything the gate refuses is
/// dropped and counted rather than corrected.
///
/// Returns the augmented model, the number of quantities injected, and the
/// rejection tally.
pub fn inject(
    model: &IfcModel,
    step: &StepFile,
    reports: &[MissingQuantityReport],
    values: &[(usize, ComputedValues)], // (report_index, computed)
    scales: &UnitScales,
) -> (IfcModel, u64, RejectionCounts) {
    let mut out = model.clone();
    let mut next_id = max_entity_id(step) + 1;
    let mut total_injected: u64 = 0;
    let mut rejections = RejectionCounts::default();

    for (report_idx, computed) in values {
        let report = &reports[*report_idx];

        // Convert into the model's declared quantity units, then gate. A value
        // that is geometrically right but expressed in raw geometry units is
        // still wrong as written, so conversion happens before any check.
        let mut injectable: Vec<(QuantityKind, f64)> = Vec::new();
        for &kind in &report.missing {
            let Some(raw) = computed.get(kind) else {
                continue;
            };
            let value = gate::to_declared_unit(raw, kind, scales);
            match gate::check_value(value) {
                Ok(()) => injectable.push((kind, value)),
                Err(r) => {
                    rejections.record(r);
                    tracing::debug!(
                        element = %report.element_id,
                        quantity = kind.ifc_name(),
                        reason = r.as_str(),
                        raw,
                        "qto value refused"
                    );
                }
            }
        }

        // Relations between an element's own quantities.
        for (kind, r) in gate::check_consistency(&injectable) {
            rejections.record(r);
            tracing::debug!(
                element = %report.element_id,
                quantity = kind.ifc_name(),
                reason = r.as_str(),
                "qto value refused"
            );
            injectable.retain(|(k, _)| *k != kind);
        }

        if injectable.is_empty() {
            continue;
        }

        // Create PhysicalQuantity entries.
        let qty_ids: Vec<EntityId> = injectable
            .iter()
            .map(|(kind, value)| {
                let id = next_id;
                next_id += 1;
                out.physical_quantities.insert(
                    id,
                    PhysicalQuantity {
                        id,
                        entity_name: SmolStr::new(kind.ifc_entity_name()),
                        name: SmolStr::new(kind.ifc_name()),
                        value: Some(StepValue::Real(*value)),
                    },
                );
                total_injected += 1;
                id
            })
            .collect();

        // Extend existing set or create a new one.
        match report.existing_set_id {
            Some(set_id) => {
                if let Some(qs) = out.element_quantities.get_mut(&set_id) {
                    qs.quantities.extend_from_slice(&qty_ids);
                }
            }
            None => {
                let set_id = next_id;
                next_id += 1;
                let guid = compressed_guid();
                out.element_quantities.insert(
                    set_id,
                    ElementQuantity {
                        id: set_id,
                        guid: SmolStr::new(&guid),
                        name: Some(SmolStr::new(report.qto_set_name)),
                        method_of_measurement: Some(SmolStr::new("Computed")),
                        quantities: qty_ids,
                    },
                );
                out.quantities_for_object
                    .entry(report.element_id)
                    .or_default()
                    .push(set_id);
                out.guid_to_entity.insert(SmolStr::new(&guid), set_id);
            }
        }
    }

    (out, total_injected, rejections)
}

/// Generate a valid IFC compressed GUID (22-character base64-like string).
fn compressed_guid() -> String {
    let uuid = Uuid::new_v4().to_string();
    ifc_model::compress_uuid_string(&uuid)
        .unwrap_or_else(|| uuid[..22].to_string())
}

fn max_entity_id(step: &StepFile) -> EntityId {
    step.entities.keys().copied().max().unwrap_or(0)
}
