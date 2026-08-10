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
                let guid = deterministic_guid(model, report);
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

/// A stable IFC GlobalId for a quantity set this module creates.
///
/// Derived from what the set *is* — the object it belongs to and its name —
/// rather than drawn at random, because the GlobalId becomes the set's IRI
/// (`<base>/qs_<guid>`). A random one made the converter non-reproducible: the
/// same file yielded different IRIs on every run, so re-ingesting a model added
/// a second `Qto_WallBaseQuantities` node beside the first instead of matching
/// it, and a wall accumulated one more set per conversion.
///
/// UUIDv5 is a namespaced SHA-1, so this is stable across runs, machines and
/// releases, and distinct for every (object, set) pair.
fn deterministic_guid(model: &IfcModel, report: &MissingQuantityReport) -> String {
    // A fixed namespace of this module's own, so these GUIDs cannot collide
    // with any other v5 derivation elsewhere.
    const NAMESPACE: Uuid = Uuid::from_u128(0x9d1f_4a3e_7c62_4d18_9b5e_2f8a_6c04_71e3);

    let object_guid = model
        .elements
        .get(&report.element_id)
        .map(|e| e.guid.as_str())
        .or_else(|| {
            model
                .spatial_nodes
                .get(&report.element_id)
                .map(|n| n.guid.as_str())
        })
        .unwrap_or_default();

    let seed = format!("{object_guid}|{}", report.qto_set_name);
    let uuid = Uuid::new_v5(&NAMESPACE, seed.as_bytes()).to_string();
    ifc_model::compress_uuid_string(&uuid).unwrap_or_else(|| uuid[..22].to_string())
}

fn max_entity_id(step: &StepFile) -> EntityId {
    step.entities.keys().copied().max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A created set's GlobalId becomes its IRI (`<base>/qs_<guid>`), so it must
    /// depend only on what the set *is*. Drawing it at random made every
    /// conversion of the same file emit different IRIs, so re-ingesting a model
    /// piled up a second `Qto_WallBaseQuantities` beside the first each time.
    #[test]
    fn injected_set_guids_are_stable_and_distinct() {
        // Two objects, distinguished only by their GlobalId — the input the
        // derivation is supposed to depend on.
        let step = ifc_step::parse_step_bytes(
            b"ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((\'\'),\'2;1\');\n\
              FILE_NAME(\'\',\'\',(\'\'),(\'\'),\'\',\' \',\'\');\nFILE_SCHEMA((\'IFC4\'));\nENDSEC;\n\
              DATA;\n\
              #1=IFCWALL(\'2O2Fr$t4X7Zf8NOew3FNtn\',$,\'A\',$,$,$,$,$,$);\n\
              #2=IFCWALL(\'3LF03GdXv2GhSTK1xTZzXp\',$,\'B\',$,$,$,$,$,$);\n\
              ENDSEC;\nEND-ISO-10303-21;\n",
        )
        .expect("parse");
        let model = IfcModel::from_step_file(&step).expect("model");
        let id_of = |guid: &str| -> EntityId {
            *model
                .elements
                .iter()
                .find(|(_, e)| e.guid.as_str() == guid)
                .expect("element")
                .0
        };
        let (one, two) = (
            id_of("2O2Fr$t4X7Zf8NOew3FNtn"),
            id_of("3LF03GdXv2GhSTK1xTZzXp"),
        );
        let report = |id| MissingQuantityReport {
            element_id: id,
            entity_type: SmolStr::new("IFCWALL"),
            qto_set_name: "Qto_WallBaseQuantities",
            existing_set_id: None,
            missing: Vec::new(),
        };

        let a1 = deterministic_guid(&model, &report(one));
        let a2 = deterministic_guid(&model, &report(one));
        let b = deterministic_guid(&model, &report(two));

        assert_eq!(a1, a2, "the same set must always get the same GlobalId");
        assert_ne!(a1, b, "different objects must not share one");
        assert_eq!(a1.len(), 22, "must be a compressed IFC GlobalId: {a1}");
    }
}
