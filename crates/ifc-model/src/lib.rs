//! Typed IFC domain model for the first conversion slice.

mod guid;

use std::collections::HashMap;

use ifc_schema::{is_element, spatial_type, SpatialType};
use ifc_step::{EntityId, RawEntity, StepError, StepFile, StepSchema, StepValue};
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;
use smol_str::SmolStr;

pub use guid::{compress_uuid_string, expand_ifc_guid};

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error(transparent)]
    Step(#[from] StepError),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpatialNode {
    pub id: EntityId,
    pub spatial_type: SpatialType,
    pub guid: SmolStr,
    pub name: Option<SmolStr>,
    pub description: Option<SmolStr>,
    pub object_type: Option<SmolStr>,
    pub long_name: Option<SmolStr>,
    pub elevation: Option<f64>,
    pub ref_elevation: Option<f64>,
    pub elevation_of_ref_height: Option<f64>,
    pub elevation_of_terrain: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElementNode {
    pub id: EntityId,
    pub entity_name: SmolStr,
    pub guid: SmolStr,
    pub name: Option<SmolStr>,
    pub description: Option<SmolStr>,
    pub object_type: Option<SmolStr>,
    pub predefined_type: Option<SmolStr>,
    pub tag: Option<SmolStr>,
    pub overall_height: Option<f64>,
    pub overall_width: Option<f64>,
    pub number_of_risers: Option<i64>,
    pub number_of_treads: Option<i64>,
    pub riser_height: Option<f64>,
    pub tread_length: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelAggregates {
    pub id: EntityId,
    pub parent: EntityId,
    pub children: Vec<EntityId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelContainedInSpatialStructure {
    pub id: EntityId,
    pub structure: EntityId,
    pub elements: Vec<EntityId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelSpaceBoundary {
    pub id: EntityId,
    pub space: EntityId,
    pub element: Option<EntityId>,
    pub physical_or_virtual: Option<SmolStr>,
    pub internal_or_external: Option<SmolStr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelVoidsElement {
    pub id: EntityId,
    pub element: EntityId,
    pub opening: EntityId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelFillsElement {
    pub id: EntityId,
    pub opening: EntityId,
    pub element: EntityId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PropertySingleValue {
    pub id: EntityId,
    pub name: SmolStr,
    pub nominal_value: Option<StepValue>,
    pub unit: Option<EntityId>,
}

/// An IFC property whose value is one or more enumeration literals.
/// `#328=IFCPROPERTYENUMERATEDVALUE('Status',$,(IFCLABEL('UNSET')),#329);`
/// We store the first selected value as a plain string for LBD emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyEnumeratedValue {
    pub id: EntityId,
    pub name: SmolStr,
    /// First selected enumeration value, e.g. "UNSET"
    pub first_value: Option<SmolStr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertySet {
    pub id: EntityId,
    pub guid: SmolStr,
    pub name: Option<SmolStr>,
    pub properties: Vec<EntityId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhysicalQuantity {
    pub id: EntityId,
    pub entity_name: SmolStr,
    pub name: SmolStr,
    pub value: Option<StepValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementQuantity {
    pub id: EntityId,
    pub guid: SmolStr,
    pub name: Option<SmolStr>,
    pub method_of_measurement: Option<SmolStr>,
    pub quantities: Vec<EntityId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelDefinesByProperties {
    pub id: EntityId,
    pub related_objects: Vec<EntityId>,
    pub relating_property_definition: EntityId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelDefinesByType {
    pub id: EntityId,
    pub related_objects: Vec<EntityId>,
    pub relating_type: EntityId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unit {
    Si {
        id: EntityId,
        unit_type: Option<SmolStr>,
        prefix: Option<SmolStr>,
        name: Option<SmolStr>,
    },
    ConversionBased {
        id: EntityId,
        unit_type: Option<SmolStr>,
        name: Option<SmolStr>,
        conversion_factor: Option<EntityId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitAssignment {
    pub id: EntityId,
    pub units: Vec<EntityId>,
}

#[derive(Debug, Clone)]
pub struct IfcModel {
    pub schema: StepSchema,
    pub spatial_nodes: HashMap<EntityId, SpatialNode>,
    pub elements: HashMap<EntityId, ElementNode>,
    pub property_single_values: HashMap<EntityId, PropertySingleValue>,
    pub property_enumerated_values: HashMap<EntityId, PropertyEnumeratedValue>,
    pub property_sets: HashMap<EntityId, PropertySet>,
    pub element_quantities: HashMap<EntityId, ElementQuantity>,
    pub physical_quantities: HashMap<EntityId, PhysicalQuantity>,
    pub rel_defines_by_properties: Vec<RelDefinesByProperties>,
    pub rel_defines_by_type: Vec<RelDefinesByType>,
    pub units: HashMap<EntityId, Unit>,
    pub unit_assignments: HashMap<EntityId, UnitAssignment>,
    pub rel_aggregates: Vec<RelAggregates>,
    pub rel_contained: Vec<RelContainedInSpatialStructure>,
    pub rel_space_boundaries: Vec<RelSpaceBoundary>,
    pub rel_voids: Vec<RelVoidsElement>,
    pub rel_fills: Vec<RelFillsElement>,
    pub children_of: HashMap<EntityId, Vec<EntityId>>,
    pub contained_in: HashMap<EntityId, EntityId>,
    pub guid_to_entity: HashMap<SmolStr, EntityId>,
    pub property_sets_for_object: HashMap<EntityId, Vec<EntityId>>,
    pub quantities_for_object: HashMap<EntityId, Vec<EntityId>>,
}

impl IfcModel {
    pub fn from_step_file(step: &StepFile) -> Result<Self, ModelError> {
        build_model(step)
    }
}

#[derive(Debug, Default)]
struct PartialModelBuild {
    spatial_nodes: HashMap<EntityId, SpatialNode>,
    elements: HashMap<EntityId, ElementNode>,
    property_single_values: HashMap<EntityId, PropertySingleValue>,
    property_enumerated_values: HashMap<EntityId, PropertyEnumeratedValue>,
    property_sets: HashMap<EntityId, PropertySet>,
    element_quantities: HashMap<EntityId, ElementQuantity>,
    physical_quantities: HashMap<EntityId, PhysicalQuantity>,
    rel_defines_by_properties: Vec<RelDefinesByProperties>,
    rel_defines_by_type: Vec<RelDefinesByType>,
    units: HashMap<EntityId, Unit>,
    unit_assignments: HashMap<EntityId, UnitAssignment>,
    rel_aggregates: Vec<RelAggregates>,
    rel_contained: Vec<RelContainedInSpatialStructure>,
    rel_space_boundaries: Vec<RelSpaceBoundary>,
    rel_voids: Vec<RelVoidsElement>,
    rel_fills: Vec<RelFillsElement>,
    children_of: HashMap<EntityId, Vec<EntityId>>,
    contained_in: HashMap<EntityId, EntityId>,
    guid_to_entity: HashMap<SmolStr, EntityId>,
}

impl PartialModelBuild {
    fn merge(mut self, other: Self) -> Self {
        self.spatial_nodes.extend(other.spatial_nodes);
        self.elements.extend(other.elements);
        self.property_single_values
            .extend(other.property_single_values);
        self.property_enumerated_values
            .extend(other.property_enumerated_values);
        self.property_sets.extend(other.property_sets);
        self.element_quantities.extend(other.element_quantities);
        self.physical_quantities.extend(other.physical_quantities);
        self.rel_defines_by_properties
            .extend(other.rel_defines_by_properties);
        self.rel_defines_by_type.extend(other.rel_defines_by_type);
        self.units.extend(other.units);
        self.unit_assignments.extend(other.unit_assignments);
        self.rel_aggregates.extend(other.rel_aggregates);
        self.rel_contained.extend(other.rel_contained);
        self.rel_space_boundaries.extend(other.rel_space_boundaries);
        self.rel_voids.extend(other.rel_voids);
        self.rel_fills.extend(other.rel_fills);
        self.guid_to_entity.extend(other.guid_to_entity);

        for (parent, children) in other.children_of {
            self.children_of.entry(parent).or_default().extend(children);
        }
        self.contained_in.extend(other.contained_in);

        self
    }
}

pub fn build_model(step: &StepFile) -> Result<IfcModel, ModelError> {
    let entity_refs: Vec<_> = step.entities.values().collect();
    #[cfg(not(target_arch = "wasm32"))]
    let partial = entity_refs
        .par_iter()
        .map(|entity| classify_entity(entity))
        .reduce(PartialModelBuild::default, PartialModelBuild::merge);
    #[cfg(target_arch = "wasm32")]
    let partial = entity_refs
        .iter()
        .map(|entity| classify_entity(entity))
        .fold(PartialModelBuild::default(), PartialModelBuild::merge);

    let mut property_sets_for_object: HashMap<EntityId, Vec<EntityId>> = HashMap::new();
    let mut quantities_for_object: HashMap<EntityId, Vec<EntityId>> = HashMap::new();

    for rel in &partial.rel_defines_by_properties {
        for object_id in &rel.related_objects {
            if partial
                .property_sets
                .contains_key(&rel.relating_property_definition)
            {
                property_sets_for_object
                    .entry(*object_id)
                    .or_default()
                    .push(rel.relating_property_definition);
            }
            if partial
                .element_quantities
                .contains_key(&rel.relating_property_definition)
            {
                quantities_for_object
                    .entry(*object_id)
                    .or_default()
                    .push(rel.relating_property_definition);
            }
        }
    }

    for rel in &partial.rel_defines_by_type {
        let Some(type_entity) = step.entities.get(&rel.relating_type) else {
            continue;
        };
        let type_property_sets = referenced_property_definitions(
            type_entity,
            &partial.property_sets,
            &partial.element_quantities,
        );
        if type_property_sets.is_empty() {
            continue;
        }
        for object_id in &rel.related_objects {
            for property_definition in &type_property_sets {
                if partial.property_sets.contains_key(property_definition) {
                    property_sets_for_object
                        .entry(*object_id)
                        .or_default()
                        .push(*property_definition);
                }
                if partial.element_quantities.contains_key(property_definition) {
                    quantities_for_object
                        .entry(*object_id)
                        .or_default()
                        .push(*property_definition);
                }
            }
        }
    }

    for property_set_ids in property_sets_for_object.values_mut() {
        property_set_ids.sort_unstable();
        property_set_ids.dedup();
    }
    for quantity_set_ids in quantities_for_object.values_mut() {
        quantity_set_ids.sort_unstable();
        quantity_set_ids.dedup();
    }

    Ok(IfcModel {
        schema: step.header.schema,
        spatial_nodes: partial.spatial_nodes,
        elements: partial.elements,
        property_single_values: partial.property_single_values,
        property_enumerated_values: partial.property_enumerated_values,
        property_sets: partial.property_sets,
        element_quantities: partial.element_quantities,
        physical_quantities: partial.physical_quantities,
        rel_defines_by_properties: partial.rel_defines_by_properties,
        rel_defines_by_type: partial.rel_defines_by_type,
        units: partial.units,
        unit_assignments: partial.unit_assignments,
        rel_aggregates: partial.rel_aggregates,
        rel_contained: partial.rel_contained,
        rel_space_boundaries: partial.rel_space_boundaries,
        rel_voids: partial.rel_voids,
        rel_fills: partial.rel_fills,
        children_of: partial.children_of,
        contained_in: partial.contained_in,
        guid_to_entity: partial.guid_to_entity,
        property_sets_for_object,
        quantities_for_object,
    })
}

fn classify_entity(entity: &RawEntity) -> PartialModelBuild {
    let mut partial = PartialModelBuild::default();

    if let Some(kind) = spatial_type(entity.entity_name.as_str()) {
        let node = parse_spatial_node(entity, kind);
        partial.guid_to_entity.insert(node.guid.clone(), node.id);
        partial.spatial_nodes.insert(node.id, node);
        return partial;
    }

    if is_element(entity.entity_name.as_str()) {
        if let Some(node) = parse_element_node(entity) {
            partial.guid_to_entity.insert(node.guid.clone(), node.id);
            partial.elements.insert(node.id, node);
        }
        return partial;
    }

    match entity.entity_name.as_str() {
        "IFCPROPERTYSINGLEVALUE" => {
            if let Some(property) = parse_property_single_value(entity) {
                partial.property_single_values.insert(property.id, property);
            }
        }
        "IFCPROPERTYENUMERATEDVALUE" => {
            if let Some(property) = parse_property_enumerated_value(entity) {
                partial
                    .property_enumerated_values
                    .insert(property.id, property);
            }
        }
        "IFCPROPERTYSET" => {
            if let Some(property_set) = parse_property_set(entity) {
                partial.property_sets.insert(property_set.id, property_set);
            }
        }
        "IFCELEMENTQUANTITY" => {
            if let Some(quantity_set) = parse_element_quantity(entity) {
                partial
                    .element_quantities
                    .insert(quantity_set.id, quantity_set);
            }
        }
        name if name.starts_with("IFCQUANTITY") => {
            if let Some(quantity) = parse_physical_quantity(entity) {
                partial.physical_quantities.insert(quantity.id, quantity);
            }
        }
        "IFCRELDEFINESBYPROPERTIES" => {
            if let Some(rel) = parse_rel_defines_by_properties(entity) {
                partial.rel_defines_by_properties.push(rel);
            }
        }
        "IFCRELDEFINESBYTYPE" => {
            if let Some(rel) = parse_rel_defines_by_type(entity) {
                partial.rel_defines_by_type.push(rel);
            }
        }
        "IFCSIUNIT" => {
            if let Some(unit) = parse_si_unit(entity) {
                partial.units.insert(entity.id, unit);
            }
        }
        "IFCCONVERSIONBASEDUNIT" => {
            if let Some(unit) = parse_conversion_based_unit(entity) {
                partial.units.insert(entity.id, unit);
            }
        }
        "IFCUNITASSIGNMENT" => {
            if let Some(assignment) = parse_unit_assignment(entity) {
                partial.unit_assignments.insert(assignment.id, assignment);
            }
        }
        "IFCRELAGGREGATES" => {
            if let Some(rel) = parse_rel_aggregates(entity) {
                partial
                    .children_of
                    .entry(rel.parent)
                    .or_default()
                    .extend(rel.children.iter().copied());
                partial.rel_aggregates.push(rel);
            }
        }
        "IFCRELCONTAINEDINSPATIALSTRUCTURE" => {
            if let Some(rel) = parse_rel_contained(entity) {
                for element_id in &rel.elements {
                    partial.contained_in.insert(*element_id, rel.structure);
                }
                partial.rel_contained.push(rel);
            }
        }
        "IFCRELSPACEBOUNDARY" | "IFCRELSPACEBOUNDARY1STLEVEL" | "IFCRELSPACEBOUNDARY2NDLEVEL" => {
            if let Some(rel) = parse_rel_space_boundary(entity) {
                partial.rel_space_boundaries.push(rel);
            }
        }
        "IFCRELVOIDSELEMENT" => {
            if let Some(rel) = parse_rel_voids_element(entity) {
                partial.rel_voids.push(rel);
            }
        }
        "IFCRELFILLSELEMENT" => {
            if let Some(rel) = parse_rel_fills_element(entity) {
                partial.rel_fills.push(rel);
            }
        }
        _ => {}
    }

    partial
}

fn parse_spatial_node(entity: &RawEntity, spatial_type: SpatialType) -> SpatialNode {
    SpatialNode {
        id: entity.id,
        spatial_type,
        guid: required_guid(entity).unwrap_or_default(),
        name: optional_string(entity.args.get(2)),
        description: optional_string(entity.args.get(3)),
        object_type: optional_string(entity.args.get(4)),
        long_name: optional_string(entity.args.get(7)),
        elevation: match entity.entity_name.as_str() {
            "IFCBUILDINGSTOREY" => optional_number(entity.args.get(9)),
            _ => None,
        },
        ref_elevation: match entity.entity_name.as_str() {
            "IFCSITE" => optional_number(entity.args.get(11)),
            _ => None,
        },
        elevation_of_ref_height: match entity.entity_name.as_str() {
            "IFCBUILDING" => optional_number(entity.args.get(9)),
            _ => None,
        },
        elevation_of_terrain: match entity.entity_name.as_str() {
            "IFCBUILDING" => optional_number(entity.args.get(10)),
            _ => None,
        },
    }
}

fn parse_element_node(entity: &RawEntity) -> Option<ElementNode> {
    Some(ElementNode {
        id: entity.id,
        entity_name: entity.entity_name.clone(),
        guid: required_guid(entity)?,
        name: optional_string(entity.args.get(2)),
        description: optional_string(entity.args.get(3)),
        object_type: optional_string(entity.args.get(4)),
        predefined_type: element_predefined_type(entity),
        tag: optional_string(entity.args.get(7)),
        overall_height: match entity.entity_name.as_str() {
            "IFCDOOR" | "IFCWINDOW" => optional_number(entity.args.get(8)),
            _ => None,
        },
        overall_width: match entity.entity_name.as_str() {
            "IFCDOOR" | "IFCWINDOW" => optional_number(entity.args.get(9)),
            _ => None,
        },
        number_of_risers: match entity.entity_name.as_str() {
            "IFCSTAIRFLIGHT" => entity.args.get(8).and_then(StepValue::as_int),
            _ => None,
        },
        number_of_treads: match entity.entity_name.as_str() {
            "IFCSTAIRFLIGHT" => entity.args.get(9).and_then(StepValue::as_int),
            _ => None,
        },
        riser_height: match entity.entity_name.as_str() {
            "IFCSTAIRFLIGHT" => optional_number(entity.args.get(10)),
            _ => None,
        },
        tread_length: match entity.entity_name.as_str() {
            "IFCSTAIRFLIGHT" => optional_number(entity.args.get(11)),
            _ => None,
        },
    })
}

fn element_predefined_type(entity: &RawEntity) -> Option<SmolStr> {
    match entity.entity_name.as_str() {
        "IFCBEAM" => optional_enum(entity.args.get(8)),
        "IFCBUILDINGELEMENTPROXY" => optional_enum(entity.args.get(8)),
        "IFCCOLUMN" => optional_enum(entity.args.get(8)),
        "IFCCOVERING" => optional_enum(entity.args.get(8)),
        "IFCCURTAINWALL" => optional_enum(entity.args.get(8)),
        "IFCFOOTING" => optional_enum(entity.args.get(8)),
        "IFCMEMBER" => optional_enum(entity.args.get(8)),
        "IFCOPENINGELEMENT" => optional_enum(entity.args.get(8)),
        "IFCPLATE" => optional_enum(entity.args.get(8)),
        "IFCRAILING" => optional_enum(entity.args.get(8)),
        "IFCSLAB" => optional_enum(entity.args.get(8)),
        "IFCSTAIRFLIGHT" => optional_enum(entity.args.get(12)),
        "IFCDOOR" | "IFCWINDOW" => optional_enum(entity.args.get(10)),
        "IFCROOF" | "IFCSTAIR" => optional_enum(entity.args.get(8)),
        "IFCWALL" | "IFCWALLSTANDARDCASE" => optional_enum(entity.args.get(8)),
        _ => None,
    }
}

fn parse_rel_aggregates(entity: &RawEntity) -> Option<RelAggregates> {
    Some(RelAggregates {
        id: entity.id,
        parent: entity.args.get(4)?.as_ref()?,
        children: refs_from_list(entity.args.get(5)?),
    })
}

fn parse_rel_contained(entity: &RawEntity) -> Option<RelContainedInSpatialStructure> {
    Some(RelContainedInSpatialStructure {
        id: entity.id,
        structure: entity.args.get(5)?.as_ref()?,
        elements: refs_from_list(entity.args.get(4)?),
    })
}

fn parse_rel_space_boundary(entity: &RawEntity) -> Option<RelSpaceBoundary> {
    Some(RelSpaceBoundary {
        id: entity.id,
        space: entity.args.get(4)?.as_ref()?,
        element: entity.args.get(5).and_then(StepValue::as_ref),
        physical_or_virtual: optional_enum(entity.args.get(7)),
        internal_or_external: optional_enum(entity.args.get(8)),
    })
}

fn parse_rel_voids_element(entity: &RawEntity) -> Option<RelVoidsElement> {
    Some(RelVoidsElement {
        id: entity.id,
        element: entity.args.get(4)?.as_ref()?,
        opening: entity.args.get(5)?.as_ref()?,
    })
}

fn parse_rel_fills_element(entity: &RawEntity) -> Option<RelFillsElement> {
    Some(RelFillsElement {
        id: entity.id,
        opening: entity.args.get(4)?.as_ref()?,
        element: entity.args.get(5)?.as_ref()?,
    })
}

fn parse_property_single_value(entity: &RawEntity) -> Option<PropertySingleValue> {
    Some(PropertySingleValue {
        id: entity.id,
        name: optional_string(entity.args.first())?,
        nominal_value: entity.args.get(2).cloned().filter(|v| !v.is_null()),
        unit: entity.args.get(3).and_then(StepValue::as_ref),
    })
}

/// Parse `IFCPROPERTYENUMERATEDVALUE('Name',$,(IFCLABEL('VAL1'),...),#ref)`.
/// We extract the first selected value from the inline list (arg[2]).
fn parse_property_enumerated_value(entity: &RawEntity) -> Option<PropertyEnumeratedValue> {
    let name = optional_string(entity.args.first())?;
    // arg[2] is the selected values list: (IFCLABEL('UNSET')) or ($) if none selected
    let first_value = entity
        .args
        .get(2)
        .and_then(|v| v.as_list())
        .and_then(|items| items.first())
        .and_then(|item| {
            // Each item is a typed value like IFCLABEL('UNSET') or IFCIDENTIFIER('X')
            let inner = item.unwrap_typed();
            inner.as_str().map(SmolStr::new)
        });
    Some(PropertyEnumeratedValue {
        id: entity.id,
        name,
        first_value,
    })
}

fn parse_property_set(entity: &RawEntity) -> Option<PropertySet> {
    Some(PropertySet {
        id: entity.id,
        guid: required_guid(entity)?,
        name: optional_string(entity.args.get(2)),
        properties: refs_from_list(entity.args.get(4)?),
    })
}

fn parse_physical_quantity(entity: &RawEntity) -> Option<PhysicalQuantity> {
    Some(PhysicalQuantity {
        id: entity.id,
        entity_name: entity.entity_name.clone(),
        name: optional_string(entity.args.first())?,
        value: entity
            .args
            .iter()
            .rev()
            .find(|value| !value.is_null())
            .cloned(),
    })
}

fn parse_element_quantity(entity: &RawEntity) -> Option<ElementQuantity> {
    Some(ElementQuantity {
        id: entity.id,
        guid: required_guid(entity)?,
        name: optional_string(entity.args.get(2)),
        method_of_measurement: optional_string(entity.args.get(4)),
        quantities: refs_from_list(entity.args.get(5)?),
    })
}

fn parse_rel_defines_by_properties(entity: &RawEntity) -> Option<RelDefinesByProperties> {
    Some(RelDefinesByProperties {
        id: entity.id,
        related_objects: refs_from_list(entity.args.get(4)?),
        relating_property_definition: entity.args.get(5)?.as_ref()?,
    })
}

fn parse_rel_defines_by_type(entity: &RawEntity) -> Option<RelDefinesByType> {
    Some(RelDefinesByType {
        id: entity.id,
        related_objects: refs_from_list(entity.args.get(4)?),
        relating_type: entity.args.get(5)?.as_ref()?,
    })
}

fn parse_si_unit(entity: &RawEntity) -> Option<Unit> {
    Some(Unit::Si {
        id: entity.id,
        unit_type: optional_enum(entity.args.get(1)),
        prefix: optional_enum(entity.args.get(2)),
        name: optional_enum(entity.args.get(3)),
    })
}

fn parse_conversion_based_unit(entity: &RawEntity) -> Option<Unit> {
    Some(Unit::ConversionBased {
        id: entity.id,
        unit_type: optional_enum(entity.args.get(1)),
        name: optional_string(entity.args.get(2)),
        conversion_factor: entity.args.get(3).and_then(StepValue::as_ref),
    })
}

fn parse_unit_assignment(entity: &RawEntity) -> Option<UnitAssignment> {
    Some(UnitAssignment {
        id: entity.id,
        units: refs_from_list(entity.args.first()?),
    })
}

fn required_guid(entity: &RawEntity) -> Option<SmolStr> {
    optional_string(entity.args.first())
}

fn optional_string(value: Option<&StepValue>) -> Option<SmolStr> {
    value.and_then(StepValue::as_str).map(SmolStr::new)
}

fn optional_enum(value: Option<&StepValue>) -> Option<SmolStr> {
    value.and_then(StepValue::as_enum).map(SmolStr::new)
}

fn optional_number(value: Option<&StepValue>) -> Option<f64> {
    value
        .map(StepValue::unwrap_typed)
        .and_then(StepValue::as_real)
}

fn refs_from_list(value: &StepValue) -> Vec<EntityId> {
    match value {
        StepValue::List(items) => items.iter().filter_map(StepValue::as_ref).collect(),
        StepValue::Ref(id) => vec![*id],
        _ => Vec::new(),
    }
}

fn referenced_property_definitions(
    entity: &RawEntity,
    property_sets: &HashMap<EntityId, PropertySet>,
    quantity_sets: &HashMap<EntityId, ElementQuantity>,
) -> Vec<EntityId> {
    let mut refs = Vec::new();
    for arg in &entity.args {
        refs_from_value(arg, &mut refs, property_sets, quantity_sets);
    }
    refs.sort_unstable();
    refs.dedup();
    refs
}

fn refs_from_value(
    value: &StepValue,
    refs: &mut Vec<EntityId>,
    property_sets: &HashMap<EntityId, PropertySet>,
    quantity_sets: &HashMap<EntityId, ElementQuantity>,
) {
    match value {
        StepValue::Ref(id) => {
            if property_sets.contains_key(id) || quantity_sets.contains_key(id) {
                refs.push(*id);
            }
        }
        StepValue::List(items) => {
            for item in items {
                refs_from_value(item, refs, property_sets, quantity_sets);
            }
        }
        StepValue::Typed { value, .. } => {
            refs_from_value(value, refs, property_sets, quantity_sets)
        }
        StepValue::String(_)
        | StepValue::Int(_)
        | StepValue::Real(_)
        | StepValue::Bool(_)
        | StepValue::Enum(_)
        | StepValue::Null
        | StepValue::Derived => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ifc_step::parse_step_file;
    use std::path::PathBuf;

    fn duplex_path() -> Option<PathBuf> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Duplex.ifc");
        path.exists().then_some(path)
    }

    fn duplex_step_and_model() -> Option<(ifc_step::StepFile, IfcModel)> {
        let path = duplex_path()?;
        let step = parse_step_file(&path).ok()?;
        let model = build_model(&step).ok()?;
        Some((step, model))
    }

    #[test]
    fn test_build_model_from_duplex() {
        let Some((step, model)) = duplex_step_and_model() else {
            return;
        };

        assert_eq!(model.schema, StepSchema::Ifc2x3);
        assert!(model.spatial_nodes.len() >= 8);
        assert!(model.elements.len() > 100);
        assert!(model.rel_aggregates.len() >= 5);
        assert!(model.rel_contained.len() >= 4);
        assert!(model.rel_space_boundaries.len() > 100);
        assert!(!model.rel_voids.is_empty());
        assert!(!model.rel_fills.is_empty());
        assert!(model.property_sets.len() > 100);
        assert!(model.rel_defines_by_properties.len() > 100);
        assert!(!model.rel_defines_by_type.is_empty());
        assert!(!model.units.is_empty());
        assert!(!model.unit_assignments.is_empty());
    }

    #[test]
    fn test_duplex_contains_spatial_hierarchy() {
        let Some((_step, model)) = duplex_step_and_model() else {
            return;
        };

        let project_id = model.guid_to_entity["1xS3BCk291UvhgP2a6eflL"];
        let site_id = model.guid_to_entity["1xS3BCk291UvhgP2a6eflN"];
        let building_id = model.guid_to_entity["1xS3BCk291UvhgP2a6eflK"];
        let storey_id = model.guid_to_entity["1xS3BCk291UvhgP2dvNMKI"];

        assert!(model.children_of[&project_id].contains(&site_id));
        assert!(model.children_of[&site_id].contains(&building_id));
        assert!(model.children_of[&building_id].contains(&storey_id));
    }

    #[test]
    fn test_duplex_space_and_element_containment() {
        let Some((_step, model)) = duplex_step_and_model() else {
            return;
        };

        let space_id = model.guid_to_entity["0BTBFw6f90Nfh9rP1dlXr2"];
        let wall_id = model.guid_to_entity["2O2Fr$t4X7Zf8NOew3FNtn"];

        assert_eq!(
            model.spatial_nodes[&space_id].spatial_type,
            SpatialType::Space
        );
        assert_eq!(model.contained_in[&wall_id], 39);
    }

    #[test]
    fn test_duplex_space_has_property_sets_and_quantities() {
        let Some((_step, model)) = duplex_step_and_model() else {
            return;
        };

        let space_id = model.guid_to_entity["0BTBFw6f90Nfh9rP1dlXr2"];
        let property_set_ids = &model.property_sets_for_object[&space_id];
        let quantity_set_ids = &model.quantities_for_object[&space_id];

        assert!(property_set_ids.len() >= 5);
        assert_eq!(quantity_set_ids.len(), 1);

        let quantity_set = &model.element_quantities[&quantity_set_ids[0]];
        assert_eq!(quantity_set.name.as_deref(), Some("GSA Space Areas"));

        let pset = &model.property_sets[&property_set_ids[0]];
        assert!(!pset.properties.is_empty());
    }

    #[test]
    fn test_duplex_type_defined_objects_inherit_type_property_sets() {
        let Some((_step, model)) = duplex_step_and_model() else {
            return;
        };

        let window_id = 6426;
        let property_set_ids = model
            .property_sets_for_object
            .get(&window_id)
            .expect("window should have property sets");

        assert!(property_set_ids
            .iter()
            .any(
                |property_set_id| model.property_sets[property_set_id].name.as_deref()
                    == Some("PSet_Revit_Type_Identity Data")
            ));
    }

    #[test]
    fn test_duplex_property_value_and_units() {
        let Some((_step, model)) = duplex_step_and_model() else {
            return;
        };

        let property = model
            .property_single_values
            .values()
            .find(|property| property.name == "Area")
            .unwrap();
        let assignment = model.unit_assignments.get(&23).unwrap();

        assert!(matches!(
            property.nominal_value,
            Some(StepValue::Typed { .. })
        ));
        assert!(assignment.units.contains(&15));
        assert!(matches!(
            model.units.get(&15),
            Some(Unit::Si {
                unit_type: Some(unit_type),
                name: Some(name),
                ..
            }) if unit_type == "LENGTHUNIT" && name == "METRE"
        ));
        assert!(matches!(
            model.units.get(&21),
            Some(Unit::ConversionBased {
                name: Some(name),
                ..
            }) if name == "DEGREE"
        ));
    }

    #[test]
    fn test_duplex_topology_relationships_present() {
        let step = parse_step_file(duplex_path().as_ref().unwrap().as_path()).unwrap();
        let model = build_model(&step).unwrap();

        assert!(model
            .rel_space_boundaries
            .iter()
            .any(|rel| rel.element.is_some()
                && rel.physical_or_virtual.as_deref() == Some("PHYSICAL")));
        assert!(model
            .rel_voids
            .iter()
            .any(|rel| model.elements.contains_key(&rel.element)));
        assert!(model
            .rel_fills
            .iter()
            .any(|rel| model.elements.contains_key(&rel.element)));
    }

    #[test]
    fn test_parse_spatial_object_type() {
        let entity = RawEntity {
            id: 1,
            entity_name: SmolStr::new("IFCBUILDINGSTOREY"),
            args: vec![
                StepValue::String(SmolStr::new("guid")),
                StepValue::Null,
                StepValue::String(SmolStr::new("Storey")),
                StepValue::Null,
                StepValue::String(SmolStr::new("TypeA")),
                StepValue::Null,
                StepValue::Null,
                StepValue::String(SmolStr::new("Long")),
                StepValue::Null,
                StepValue::Real(0.0),
            ],
        };

        let node = parse_spatial_node(&entity, SpatialType::Storey);
        assert_eq!(node.object_type.as_deref(), Some("TypeA"));
    }

    #[test]
    fn test_parse_stairflight_predefined_type_ifc4() {
        let entity = RawEntity {
            id: 1,
            entity_name: SmolStr::new("IFCSTAIRFLIGHT"),
            args: vec![
                StepValue::String(SmolStr::new("guid")),
                StepValue::Null,
                StepValue::String(SmolStr::new("Flight")),
                StepValue::Null,
                StepValue::Null,
                StepValue::Null,
                StepValue::Null,
                StepValue::Null,
                StepValue::Int(12),
                StepValue::Int(11),
                StepValue::Real(0.17),
                StepValue::Real(0.29),
                StepValue::Enum(SmolStr::new("STRAIGHT")),
            ],
        };

        let node = parse_element_node(&entity).unwrap();
        assert_eq!(node.predefined_type.as_deref(), Some("STRAIGHT"));
    }

    #[test]
    fn test_parse_building_elevation_fields() {
        let entity = RawEntity {
            id: 1,
            entity_name: SmolStr::new("IFCBUILDING"),
            args: vec![
                StepValue::String(SmolStr::new("guid")),
                StepValue::Null,
                StepValue::String(SmolStr::new("Building")),
                StepValue::Null,
                StepValue::Null,
                StepValue::Null,
                StepValue::Null,
                StepValue::Null,
                StepValue::Enum(SmolStr::new("ELEMENT")),
                StepValue::Real(12.34),
                StepValue::Real(56.78),
                StepValue::Null,
            ],
        };

        let node = parse_spatial_node(&entity, SpatialType::Building);
        assert_eq!(node.elevation_of_ref_height, Some(12.34));
        assert_eq!(node.elevation_of_terrain, Some(56.78));
    }
}
