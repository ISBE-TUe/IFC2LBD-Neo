/// Scan the IfcModel and produce a per-element report of missing quantities.

use std::collections::HashSet;

use ifc_model::IfcModel;
use ifc_schema::SpatialType;
use ifc_step::EntityId;
use smol_str::SmolStr;

use crate::qto_names::{qto_spec_for, QuantityKind};

#[derive(Debug)]
pub struct MissingQuantityReport {
    pub element_id: EntityId,
    /// Uppercase IFC entity type, e.g. "IFCWALL".
    pub entity_type: SmolStr,
    /// The standard Qto_* set name for this element type.
    pub qto_set_name: &'static str,
    /// If a matching-named ElementQuantity already exists, its ID — extend it.
    /// If None — create a new set.
    pub existing_set_id: Option<EntityId>,
    /// Quantity kinds that are absent from the existing set (or all if new).
    pub missing: Vec<QuantityKind>,
}

/// Collect missing-quantity reports for all elements in the model.
pub fn audit(model: &IfcModel) -> Vec<MissingQuantityReport> {
    let mut reports = Vec::new();

    for (&element_id, element) in &model.elements {
        let entity_type = element.entity_name.to_uppercase();
        let spec = qto_spec_for(&entity_type);

        // Collect names of quantities already present on this element.
        let (existing_set_id, existing_names) =
            existing_quantities(model, element_id, spec.set_name);

        let missing: Vec<QuantityKind> = spec
            .quantities
            .iter()
            .copied()
            .filter(|kind| !existing_names.contains(kind.ifc_name()))
            .collect();

        if !missing.is_empty() {
            reports.push(MissingQuantityReport {
                element_id,
                entity_type: element.entity_name.clone(),
                qto_set_name: spec.set_name,
                existing_set_id,
                missing,
            });
        }
    }

    // Also audit spatial nodes (IfcSpace).
    for (&node_id, node) in &model.spatial_nodes {
        if node.spatial_type != SpatialType::Space {
            continue;
        }
        let entity_type = "IFCSPACE";
        let spec = qto_spec_for(entity_type);
        let (existing_set_id, existing_names) =
            existing_quantities(model, node_id, spec.set_name);
        let missing: Vec<QuantityKind> = spec
            .quantities
            .iter()
            .copied()
            .filter(|kind| !existing_names.contains(kind.ifc_name()))
            .collect();
        if !missing.is_empty() {
            reports.push(MissingQuantityReport {
                element_id: node_id,
                entity_type: SmolStr::new("IfcSpace"),
                qto_set_name: spec.set_name,
                existing_set_id,
                missing,
            });
        }
    }

    reports
}

/// Find any existing ElementQuantity named `set_name` linked to `object_id`,
/// and collect the quantity names already present in it.
///
/// Returns `(existing_set_id, set_of_existing_quantity_names)`.
fn existing_quantities(
    model: &IfcModel,
    object_id: EntityId,
    set_name: &str,
) -> (Option<EntityId>, HashSet<String>) {
    let mut names = HashSet::new();
    let mut found_set_id: Option<EntityId> = None;

    let qty_set_ids = match model.quantities_for_object.get(&object_id) {
        Some(ids) => ids,
        None => return (None, names),
    };

    for &qty_set_id in qty_set_ids {
        let qty_set = match model.element_quantities.get(&qty_set_id) {
            Some(qs) => qs,
            None => continue,
        };
        // Match by set name.
        if qty_set.name.as_deref() == Some(set_name) {
            found_set_id = Some(qty_set_id);
        }
        // Collect all existing quantity names (from all sets, not just matching ones),
        // so we don't re-emit a quantity that lives in any set for this object.
        for &qty_id in &qty_set.quantities {
            if let Some(qty) = model.physical_quantities.get(&qty_id) {
                names.insert(qty.name.to_string());
            }
        }
    }

    (found_set_id, names)
}
