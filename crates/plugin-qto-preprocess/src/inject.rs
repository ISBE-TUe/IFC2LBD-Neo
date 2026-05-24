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
use crate::qto_names::QuantityKind;

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
            QuantityKind::GrossPerimeter => self.gross_perimeter,
            QuantityKind::Perimeter => self.perimeter,
        }
    }
}

/// Apply all reports + their computed values to a cloned model.
///
/// Returns the augmented model and the total number of quantities injected.
pub fn inject(
    model: &IfcModel,
    step: &StepFile,
    reports: &[MissingQuantityReport],
    values: &[(usize, ComputedValues)], // (report_index, computed)
) -> (IfcModel, u64) {
    let mut out = model.clone();
    let mut next_id = max_entity_id(step) + 1;
    let mut total_injected: u64 = 0;

    for (report_idx, computed) in values {
        let report = &reports[*report_idx];

        // Determine which missing quantities we actually have values for.
        // Skip zero/negative results — they mean the geometry couldn't be computed
        // and must never appear in production output as misleading zeros.
        let injectable: Vec<(QuantityKind, f64)> = report
            .missing
            .iter()
            .filter_map(|&kind| computed.get(kind).filter(|&v| v > 0.0).map(|v| (kind, v)))
            .collect();

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

    (out, total_injected)
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
