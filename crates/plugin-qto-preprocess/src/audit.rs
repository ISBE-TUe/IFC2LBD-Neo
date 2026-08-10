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
        // No spec means bSDD defines no geometrically-derivable quantities for
        // this class (MEP devices define only weight and count). Nothing should
        // be computed or written for it.
        let Some(spec) = qto_spec_for(&entity_type) else {
            continue;
        };

        // Collect names of quantities already present on this element.
        let (existing_set_id, existing_names) =
            existing_quantities(model, element_id, spec.set_name);

        let missing: Vec<QuantityKind> = spec
            .quantities
            .iter()
            .copied()
            .filter(|kind| !has_quantity(&existing_names, kind.ifc_name()))
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
        let Some(spec) = qto_spec_for(entity_type) else {
            continue;
        };
        let (existing_set_id, existing_names) =
            existing_quantities(model, node_id, spec.set_name);
        let missing: Vec<QuantityKind> = spec
            .quantities
            .iter()
            .copied()
            .filter(|kind| !has_quantity(&existing_names, kind.ifc_name()))
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

/// Is this the IFC2X3 spelling of a base quantity set?
///
/// IFC4 names the set after its class — `Qto_WallBaseQuantities` — but IFC2X3
/// has no `Qto_` prefix and exporters write a bare `BaseQuantities` for every
/// class. Matching only the IFC4 name meant the set was never found in a 2X3
/// file, so the module added its own beside it: on one 96 MB ArchiCAD export
/// that produced **3,342 new sets and 0 extensions**, leaving every element with
/// two quantity sets under two IRIs, the authored quantities in one and the
/// computed ones in the other.
fn is_bare_base_quantities(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("BaseQuantities")
}

/// Case-insensitive membership test.
///
/// Exporters disagree on capitalisation — notably `GrossFootprintArea` (IFC4)
/// versus bSDD IFC4x3's `GrossFootPrintArea`. A case-sensitive comparison would
/// fail to see an authored quantity and add a second one beside it, which both
/// duplicates data and overwrites nothing — the worst of both.
fn has_quantity(existing: &HashSet<String>, name: &str) -> bool {
    existing.iter().any(|e| e.eq_ignore_ascii_case(name))
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
        // Match by set name. Prefer the exact standard name, but fall back to
        // the bare IFC2X3 spelling if that is all the file has.
        match qty_set.name.as_deref() {
            Some(actual) if actual.trim().eq_ignore_ascii_case(set_name) => {
                found_set_id = Some(qty_set_id);
            }
            Some(actual) if found_set_id.is_none() && is_bare_base_quantities(actual) => {
                found_set_id = Some(qty_set_id);
            }
            _ => {}
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
