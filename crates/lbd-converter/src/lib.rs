//! Core IFC-to-LBD conversion for the first end-to-end slice.
//!
//! Translates the internal IfcModel into RDF triples for both LBD graphs and linked ifcOWL.
//! Implements the producer logic that emits triples in bounded batches (streaming) and can run multiple producer paths (LBD, ifcOWL, topology) in parallel.
//! Follows an emitter-based pattern so LBD-specific concerns can be factored into separate modules while sharing buffering and streaming infrastructure.

mod modules;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::OnceLock;

use crossbeam::channel::Sender;
use ifc_model::{
    compress_uuid_string, expand_ifc_guid, IfcModel, PropertyEnumeratedValue, PropertySingleValue,
    Unit,
};
use ifc_schema::{product_type_name, SpatialType};
use ifc_step::{decode_ifc_unicode, EntityId, StepFile, StepSchema, StepValue};
use lbd_geometry::{
    derive_relations_from_bounding_boxes, BoundingBox, GeometryRelation, GeometryRelationKind,
    MapBoundingBoxProvider,
};
use lbd_ontology::{
    beo_class, bot_adjacent_element, bot_adjacent_zone, bot_building, bot_contains_element,
    bot_contains_zone, bot_has_sub_element, bot_interface, bot_interface_of,
    bot_intersecting_element, bot_site, bot_space, bot_storey, express_has_boolean,
    express_has_double, express_has_integer, express_has_logical, express_has_string,
    express_logical_value, furn_class, geo_as_wkt, geo_geometry, geo_wkt_literal,
    lbd_has_bounding_box, lbd_has_property_set, lbd_has_quantity_set, lbd_project,
    lbd_property_set, lbd_quantity_set, list_has_contents, list_has_next,
    opm_current_property_state, opm_current_property_state_predicate, opm_has_property_state,
    opm_property, owl_imports, owl_object_property, owl_ontology, props_property,
    prov_generated_at_time, rdf_member, rdf_type, rdfs_comment, rdfs_label, schema_value,
    smls_unit, unit_iri, Object, Triple, EXPRESS, XSD,
};
#[cfg(test)]
use lbd_ontology::{bot_has_building, bot_has_site, owl_same_as};
use lbd_topology::{
    build_topology, build_topology_with_enricher, IfcRelationEvidenceEnricher, TopologyEdgeKind,
    TopologyGraph, TopologyNodeKind,
};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use rio_api::model::{NamedNode, Subject, Term};
use rio_api::parser::TriplesParser;
use rio_turtle::TurtleParser;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub base_uri: String,
    pub emit_ifcowl_links: bool,
    pub enable_topology: bool,
    pub enable_topology_extension: bool,
    pub topology_only: bool,
    pub suppress_non_topology_fallback: bool,
    pub geometry_relations: Option<Arc<Vec<GeometryRelation>>>,
    pub geometry_bounding_boxes: Option<Arc<HashMap<EntityId, BoundingBox>>>,
    pub geometry_wkts: Option<Arc<HashMap<EntityId, String>>>,
    pub geometry_tolerance: f64,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            base_uri: "https://lbd.example.com/".to_string(),
            emit_ifcowl_links: true,
            enable_topology: false,
            enable_topology_extension: false,
            topology_only: false,
            suppress_non_topology_fallback: false,
            geometry_relations: None,
            geometry_bounding_boxes: None,
            geometry_wkts: None,
            geometry_tolerance: 1e-6,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConversionResult {
    pub triples: Vec<Triple>,
    pub ifcowl_triples: Vec<Triple>,
}

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("failed to send triple batch to serializer")]
    ChannelClosed,
}

// Larger batches reduce channel traffic and formatter call overhead on large exports.
const STREAM_BATCH_SIZE: usize = 8 * 1024;

pub fn convert_step_and_model(
    step: &StepFile,
    model: &IfcModel,
    options: &ConvertOptions,
) -> ConversionResult {
    let base = normalize_base_uri(&options.base_uri);
    let ifcowl_triples = modules::ifcowl::convert_ifcowl(step, &base, step.header.schema);
    let triples = convert_lbd(model, options, &base);
    ConversionResult {
        triples,
        ifcowl_triples,
    }
}

pub fn convert_model(model: &IfcModel, options: &ConvertOptions) -> ConversionResult {
    ConversionResult {
        triples: convert_lbd(model, options, &normalize_base_uri(&options.base_uri)),
        ifcowl_triples: Vec::new(),
    }
}

pub fn stream_step_and_model(
    step: &StepFile,
    model: &IfcModel,
    options: &ConvertOptions,
    lbd_sender: &Sender<Vec<Triple>>,
    ifcowl_sender: Option<&Sender<Vec<Triple>>>,
) -> Result<(), StreamError> {
    let base = normalize_base_uri(&options.base_uri);
    if let Some(sender) = ifcowl_sender {
        let ifcowl_sender = sender.clone();
        std::thread::scope(|scope| {
            let ifcowl_task = scope.spawn(|| {
                modules::ifcowl::stream_ifcowl(step, &base, step.header.schema, &ifcowl_sender)
            });
            let lbd_result = stream_lbd(model, options, &base, lbd_sender);
            let ifcowl_result = ifcowl_task.join().map_err(|_| StreamError::ChannelClosed)?;
            lbd_result?;
            ifcowl_result
        })
    } else {
        stream_lbd(model, options, &base, lbd_sender)
    }
}

pub fn stream_topology_model(
    model: &IfcModel,
    options: &ConvertOptions,
    sender: &Sender<Vec<Triple>>,
) -> Result<(), StreamError> {
    let mut topology_options = options.clone();
    topology_options.topology_only = true;
    stream_lbd(
        model,
        &topology_options,
        &normalize_base_uri(&topology_options.base_uri),
        sender,
    )
}

fn convert_lbd(model: &IfcModel, options: &ConvertOptions, base: &str) -> Vec<Triple> {
    let mut triples = Vec::new();
    emit_lbd(model, options, base, |triple| {
        triples.push(triple);
        Ok::<(), std::convert::Infallible>(())
    })
    .expect("infallible LBD conversion");
    triples
}

fn stream_lbd(
    model: &IfcModel,
    options: &ConvertOptions,
    base: &str,
    sender: &Sender<Vec<Triple>>,
) -> Result<(), StreamError> {
    let mut batch = Vec::with_capacity(STREAM_BATCH_SIZE);
    emit_lbd(model, options, base, |triple| {
        batch.push(triple);
        if batch.len() >= STREAM_BATCH_SIZE {
            sender
                .send(std::mem::take(&mut batch))
                .map_err(|_| StreamError::ChannelClosed)?;
        }
        Ok::<(), StreamError>(())
    })?;

    if !batch.is_empty() {
        sender.send(batch).map_err(|_| StreamError::ChannelClosed)?;
    }
    Ok(())
}

fn emit_lbd<E, F>(
    model: &IfcModel,
    options: &ConvertOptions,
    base: &str,
    mut emit: F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    let unit_by_type = build_unit_type_map(model);
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("RFC3339 formatting should always succeed");
    let mut declared_object_properties = HashSet::new();
    let mut declared_property_comments = HashSet::new();
    let mut property_state_counter = 0_u64;
    let mut attribute_state_counter = 0_u64;
    let mut declared_standard_attributes = HashSet::new();
    let mut declared_standard_attribute_comments = HashSet::new();
    let mut emitted_lbd_property_sets = HashSet::new();
    let mut emitted_lbd_quantity_sets = HashSet::new();

    if !options.topology_only {
        emit_geometry_declarations(&mut emit)?;
        modules::core_entities::emit_core_entities(model, options, base, &mut emit)?;
    }

    let mut topology = options.enable_topology.then(|| {
        if options.enable_topology_extension {
            build_topology_with_enricher(model, &IfcRelationEvidenceEnricher)
        } else {
            build_topology(model)
        }
    });
    if let Some(graph) = topology.as_mut() {
        if let Some(relations) = options.geometry_relations.as_deref() {
            // Exact-kernel-confirmed results → BOT core
            merge_geometry_relations_into_topology(graph, relations, true);
        } else if let Some(boxes) = options.geometry_bounding_boxes.clone() {
            // Bbox broad-phase only (no exact kernel) → extension only
            let provider = MapBoundingBoxProvider::new(boxes);
            let relations =
                derive_relations_from_bounding_boxes(model, &provider, options.geometry_tolerance);
            merge_geometry_relations_into_topology(graph, &relations, false);
        }
    }
    if options.enable_topology {
        emit_direct_sub_element_triples(model, base, topology.as_ref(), &mut emit)?;
    }

    if options.enable_topology {
        if let Some(topology) = topology.as_ref() {
            let mut emitted_interface_types = HashSet::new();
            for (parent_zone, child_zone) in
                topology.core_pairs_of_kind(TopologyEdgeKind::ContainsZone)
            {
                let Some(subject) = object_subject(model, base, parent_zone) else {
                    continue;
                };
                let Some(target_zone) = object_subject(model, base, child_zone) else {
                    continue;
                };
                emit(Triple {
                    subject,
                    predicate: bot_contains_zone(),
                    object: Object::Iri(target_zone),
                })?;
            }

            for (structure_id, element_id) in
                topology.core_pairs_of_kind(TopologyEdgeKind::ContainsElement)
            {
                let Some(subject) = object_subject(model, base, structure_id) else {
                    continue;
                };
                let Some(element_subject) = object_subject(model, base, element_id) else {
                    continue;
                };
                emit(Triple {
                    subject,
                    predicate: bot_contains_element(),
                    object: Object::Iri(element_subject),
                })?;
            }

            for (space_id, element_id) in
                topology.core_pairs_of_kind(TopologyEdgeKind::AdjacentElement)
            {
                let Some(subject) = object_subject(model, base, space_id) else {
                    continue;
                };
                let Some(element_subject) = object_subject(model, base, element_id) else {
                    continue;
                };
                emit(Triple {
                    subject,
                    predicate: bot_adjacent_element(),
                    object: Object::Iri(element_subject),
                })?;
            }

            for (left_element, right_element) in
                topology.core_pairs_of_kind(TopologyEdgeKind::IntersectingElement)
            {
                let Some(subject) = object_subject(model, base, left_element) else {
                    continue;
                };
                let Some(target) = object_subject(model, base, right_element) else {
                    continue;
                };
                emit(Triple {
                    subject,
                    predicate: bot_intersecting_element(),
                    object: Object::Iri(target),
                })?;
            }

            for (left_zone, right_zone) in
                topology.core_pairs_of_kind(TopologyEdgeKind::AdjacentZone)
            {
                let Some(subject) = object_subject(model, base, left_zone) else {
                    continue;
                };
                let Some(target_zone) = object_subject(model, base, right_zone) else {
                    continue;
                };
                emit(Triple {
                    subject,
                    predicate: bot_adjacent_zone(),
                    object: Object::Iri(target_zone),
                })?;
            }

            for (interface_id, target_id) in
                topology.core_pairs_of_kind(TopologyEdgeKind::InterfaceOf)
            {
                let interface_subject = topology_interface_resource_iri(base, interface_id);
                let Some(target) = object_subject(model, base, target_id) else {
                    continue;
                };
                if emitted_interface_types.insert(interface_subject.clone()) {
                    emit(Triple {
                        subject: interface_subject.clone(),
                        predicate: rdf_type(),
                        object: Object::Iri(bot_interface()),
                    })?;
                }
                emit(Triple {
                    subject: interface_subject,
                    predicate: bot_interface_of(),
                    object: Object::Iri(target),
                })?;
            }

            if options.enable_topology_extension {
                let mut extension_edges = topology.extension_edges.clone();
                extension_edges.sort_by(|left, right| {
                    left.source
                        .cmp(&right.source)
                        .then_with(|| left.target.cmp(&right.target))
                        .then_with(|| {
                            topology_edge_kind_rank(left.kind)
                                .cmp(&topology_edge_kind_rank(right.kind))
                        })
                        .then_with(|| left.derived_from.cmp(&right.derived_from))
                });

                for edge in extension_edges {
                    let Some(predicate) = topology_extension_predicate(edge.kind) else {
                        continue;
                    };
                    let Some(subject) = object_subject(model, base, edge.source) else {
                        continue;
                    };
                    let Some(target) = object_subject(model, base, edge.target) else {
                        continue;
                    };
                    emit(Triple {
                        subject,
                        predicate,
                        object: Object::Iri(target),
                    })?;
                }
            }
        }
    } else if !options.topology_only && !options.suppress_non_topology_fallback {
        let mut emitted_storey_contains = HashSet::new();
        let mut contained_pairs: Vec<_> = model
            .rel_contained
            .iter()
            .flat_map(|rel| {
                rel.elements
                    .iter()
                    .map(move |element_id| (*element_id, rel.structure))
            })
            .collect();
        contained_pairs.sort_unstable();
        for (element_id, structure_id) in contained_pairs {
            let Some(_element) = model.elements.get(&element_id) else {
                continue;
            };
            let Some(structure) = model.spatial_nodes.get(&structure_id) else {
                continue;
            };
            if matches!(
                structure.spatial_type,
                SpatialType::Storey | SpatialType::Space
            ) {
                let structure_subject =
                    spatial_resource_iri(base, structure.spatial_type, &structure.guid);
                for contained_id in baseline_containment_closure(model, element_id) {
                    let Some(contained_element) = model.elements.get(&contained_id) else {
                        continue;
                    };
                    if !emitted_storey_contains.insert((structure_id, contained_id)) {
                        continue;
                    }
                    emit(Triple {
                        subject: structure_subject.clone(),
                        predicate: bot_contains_element(),
                        object: Object::Iri(element_resource_iri(base, contained_element)),
                    })?;
                }
            }
        }

        let adjacent_by_space = adjacent_elements_by_space(model);
        let mut storey_ids: Vec<_> = model
            .spatial_nodes
            .iter()
            .filter_map(|(&id, node)| (node.spatial_type == SpatialType::Storey).then_some(id))
            .collect();
        storey_ids.sort_unstable();
        for storey_id in storey_ids {
            let Some(storey) = model.spatial_nodes.get(&storey_id) else {
                continue;
            };
            let structure_subject = spatial_resource_iri(base, storey.spatial_type, &storey.guid);
            let mut child_space_ids = model
                .children_of
                .get(&storey_id)
                .cloned()
                .unwrap_or_default();
            child_space_ids.sort_unstable();
            for space_id in child_space_ids {
                let Some(space) = model.spatial_nodes.get(&space_id) else {
                    continue;
                };
                if space.spatial_type != SpatialType::Space {
                    continue;
                }

                let mut space_element_ids: Vec<_> = model
                    .rel_contained
                    .iter()
                    .filter(|rel| rel.structure == space_id)
                    .flat_map(|rel| rel.elements.iter().copied())
                    .collect();
                if let Some(adjacent_ids) = adjacent_by_space.get(&space_id) {
                    space_element_ids.extend(adjacent_ids.iter().copied());
                }
                space_element_ids.sort_unstable();
                space_element_ids.dedup();

                for element_id in space_element_ids {
                    let Some(contained_element) = model.elements.get(&element_id) else {
                        continue;
                    };
                    if !matches!(
                        contained_element.entity_name.as_str(),
                        "IFCMEMBER" | "IFCRAILING" | "IFCSTAIRFLIGHT"
                    ) {
                        continue;
                    }
                    if !emitted_storey_contains.insert((storey_id, element_id)) {
                        continue;
                    }
                    emit(Triple {
                        subject: structure_subject.clone(),
                        predicate: bot_contains_element(),
                        object: Object::Iri(element_resource_iri(base, contained_element)),
                    })?;
                }
            }
        }
    }

    if options.topology_only {
        return Ok(());
    }

    let mut property_object_ids: Vec<_> = model.property_sets_for_object.keys().copied().collect();
    property_object_ids.sort_unstable();
    for object_id in property_object_ids {
        let Some((subject, object_guid)) = object_subject_and_guid(model, base, object_id) else {
            continue;
        };
        let mut property_set_ids = model.property_sets_for_object[&object_id].clone();
        property_set_ids.sort_unstable();
        for property_set_id in property_set_ids {
            let Some(property_set) = model.property_sets.get(&property_set_id) else {
                continue;
            };
            let set_subject = property_set_resource_iri(base, &property_set.guid);
            emit(Triple {
                subject: subject.clone(),
                predicate: lbd_has_property_set(),
                object: Object::Iri(set_subject.clone()),
            })?;
            if emitted_lbd_property_sets.insert(property_set.id) {
                emit(Triple {
                    subject: set_subject.clone(),
                    predicate: rdf_type(),
                    object: Object::Iri(lbd_property_set()),
                })?;
                if let Some(name) = property_set.name.as_ref() {
                    emit(Triple {
                        subject: set_subject.clone(),
                        predicate: rdfs_label(),
                        object: Object::Literal(name.to_string()),
                    })?;
                }
            }
            for property_id in &property_set.properties {
                // --- IfcPropertySingleValue ---
                if let Some(property) = model.property_single_values.get(property_id) {
                    if let Some(value) = property_value_object(property) {
                        if !should_skip_named_self_value(&property.name, &value) {
                            let predicate_local = property_local_name(&property.name);
                            emit_property_declaration(
                                &mut declared_object_properties,
                                &mut declared_property_comments,
                                &predicate_local,
                                property_set.name.as_deref().unwrap_or_default(),
                                &mut emit,
                            )?;
                            let property_subject = emit_property_state(
                                &subject,
                                base,
                                &predicate_local,
                                Some(property_label(
                                    property_set.name.as_deref(),
                                    &predicate_local,
                                )),
                                &object_guid,
                                &property_set.guid,
                                'p',
                                &mut property_state_counter,
                                value,
                                resolve_property_unit(property, &unit_by_type, model),
                                &generated_at,
                                &mut emit,
                            )?;
                            emit(Triple {
                                subject: set_subject.clone(),
                                predicate: rdf_member(),
                                object: Object::Iri(property_subject),
                            })?;
                        }
                    }
                    continue;
                }
                // --- IfcPropertyEnumeratedValue ---
                if let Some(property) = model.property_enumerated_values.get(property_id) {
                    if let Some(value) = enumerated_value_object(property) {
                        let predicate_local = property_local_name(&property.name);
                        emit_property_declaration(
                            &mut declared_object_properties,
                            &mut declared_property_comments,
                            &predicate_local,
                            property_set.name.as_deref().unwrap_or_default(),
                            &mut emit,
                        )?;
                        let property_subject = emit_property_state(
                            &subject,
                            base,
                            &predicate_local,
                            Some(property_label(
                                property_set.name.as_deref(),
                                &predicate_local,
                            )),
                            &object_guid,
                            &property_set.guid,
                            'p',
                            &mut property_state_counter,
                            value,
                            None, // enumerated values have no unit
                            &generated_at,
                            &mut emit,
                        )?;
                        emit(Triple {
                            subject: set_subject.clone(),
                            predicate: rdf_member(),
                            object: Object::Iri(property_subject),
                        })?;
                    }
                }
            }
        }
    }

    let mut quantity_object_ids: Vec<_> = model.quantities_for_object.keys().copied().collect();
    quantity_object_ids.sort_unstable();
    for object_id in quantity_object_ids {
        let Some((subject, object_guid)) = object_subject_and_guid(model, base, object_id) else {
            continue;
        };
        let mut quantity_set_ids = model.quantities_for_object[&object_id].clone();
        quantity_set_ids.sort_unstable();
        for quantity_set_id in quantity_set_ids {
            let Some(quantity_set) = model.element_quantities.get(&quantity_set_id) else {
                continue;
            };
            let set_subject = quantity_set_resource_iri(base, &quantity_set.guid);
            emit(Triple {
                subject: subject.clone(),
                predicate: lbd_has_quantity_set(),
                object: Object::Iri(set_subject.clone()),
            })?;
            if emitted_lbd_quantity_sets.insert(quantity_set.id) {
                emit(Triple {
                    subject: set_subject.clone(),
                    predicate: rdf_type(),
                    object: Object::Iri(lbd_quantity_set()),
                })?;
                if let Some(name) = quantity_set.name.as_ref() {
                    emit(Triple {
                        subject: set_subject.clone(),
                        predicate: rdfs_label(),
                        object: Object::Literal(name.to_string()),
                    })?;
                }
            }
            for quantity_id in &quantity_set.quantities {
                let Some(quantity) = model.physical_quantities.get(quantity_id) else {
                    continue;
                };
                if let Some(value) = quantity_value_object(quantity.value.as_ref()) {
                    if should_skip_named_self_value(&quantity.name, &value) {
                        continue;
                    }
                    let predicate_local = property_local_name(&quantity.name);
                    emit_property_declaration(
                        &mut declared_object_properties,
                        &mut declared_property_comments,
                        &predicate_local,
                        quantity_set.name.as_deref().unwrap_or_default(),
                        &mut emit,
                    )?;
                    let property_subject = emit_property_state(
                        &subject,
                        base,
                        &predicate_local,
                        Some(property_label(
                            quantity_set.name.as_deref(),
                            &predicate_local,
                        )),
                        &object_guid,
                        &quantity_set.guid,
                        'p',
                        &mut property_state_counter,
                        value,
                        resolve_quantity_unit(quantity.entity_name.as_str(), &unit_by_type),
                        &generated_at,
                        &mut emit,
                    )?;
                    emit(Triple {
                        subject: set_subject.clone(),
                        predicate: rdf_member(),
                        object: Object::Iri(property_subject),
                    })?;
                }
            }
        }
    }

    if options.geometry_bounding_boxes.is_some() || options.geometry_wkts.is_some() {
        emit_bounding_box_geometries(
            model,
            base,
            options.geometry_bounding_boxes.as_deref(),
            options.geometry_wkts.as_deref(),
            &mut emit,
        )?;
    }

    emit_standard_attribute_triples(
        model,
        base,
        &unit_by_type,
        &generated_at,
        &mut attribute_state_counter,
        &mut declared_standard_attributes,
        &mut declared_standard_attribute_comments,
        &mut emit,
    )?;

    Ok(())
}

fn topology_extension_predicate(kind: TopologyEdgeKind) -> Option<String> {
    let _ = kind;
    None
}

fn merge_geometry_relations_into_topology(
    topology: &mut TopologyGraph,
    relations: &[GeometryRelation],
    exact: bool,
) {
    for relation in relations {
        let kind = match relation.kind {
            GeometryRelationKind::AdjacentElement => TopologyEdgeKind::AdjacentElement,
            GeometryRelationKind::IntersectingElement => TopologyEdgeKind::IntersectingElement,
            GeometryRelationKind::InterfaceOf => TopologyEdgeKind::InterfaceOf,
        };
        if !topology.node_kinds.contains_key(&relation.target) {
            continue;
        }
        if exact {
            // OCC-confirmed results: register Interface nodes and promote to BOT core.
            if matches!(kind, TopologyEdgeKind::InterfaceOf) {
                topology
                    .node_kinds
                    .entry(relation.source)
                    .or_insert(TopologyNodeKind::Interface);
            } else if !topology.node_kinds.contains_key(&relation.source) {
                continue;
            }
            insert_unique_topology_edge(
                &mut topology.core_edges,
                relation.source,
                relation.target,
                kind,
                Some("GeometryProvider::occ"),
            );
        } else {
            // Bbox broad-phase only: candidates without exact-kernel confirmation go to
            // extension_edges. Promoting them to core_edges produces semantically wrong
            // BOT triples (doors adjacent to slabs, bot:Interface between two doors, etc.).
            if !topology.node_kinds.contains_key(&relation.source) {
                continue;
            }
            insert_unique_topology_edge(
                &mut topology.extension_edges,
                relation.source,
                relation.target,
                kind,
                Some("GeometryProvider::bbox"),
            );
        }
    }

    topology.core_edges.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| {
                topology_edge_kind_rank(left.kind).cmp(&topology_edge_kind_rank(right.kind))
            })
            .then_with(|| left.derived_from.cmp(&right.derived_from))
    });
    topology.extension_edges.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| {
                topology_edge_kind_rank(left.kind).cmp(&topology_edge_kind_rank(right.kind))
            })
            .then_with(|| left.derived_from.cmp(&right.derived_from))
    });
}

fn insert_unique_topology_edge(
    edges: &mut Vec<lbd_topology::TopologyEdge>,
    source: EntityId,
    target: EntityId,
    kind: TopologyEdgeKind,
    derived_from: Option<&'static str>,
) {
    if edges
        .iter()
        .any(|edge| edge.source == source && edge.target == target && edge.kind == kind)
    {
        return;
    }
    edges.push(lbd_topology::TopologyEdge {
        source,
        target,
        kind,
        derived_from,
    });
}

fn topology_edge_kind_rank(kind: TopologyEdgeKind) -> u8 {
    match kind {
        TopologyEdgeKind::ContainsZone => 0,
        TopologyEdgeKind::ContainsElement => 1,
        TopologyEdgeKind::AdjacentElement => 2,
        TopologyEdgeKind::AdjacentZone => 3,
        TopologyEdgeKind::HasSubElement => 4,
        TopologyEdgeKind::IntersectingElement => 5,
        TopologyEdgeKind::InterfaceOf => 6,
    }
}

fn emit_direct_sub_element_triples<E, F>(
    model: &IfcModel,
    base: &str,
    topology: Option<&TopologyGraph>,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    let mut seen_pairs = HashSet::new();

    for rel in &model.rel_aggregates {
        if !model.elements.contains_key(&rel.parent) {
            continue;
        }
        let mut child_ids = rel.children.clone();
        child_ids.sort_unstable();
        for child_id in child_ids {
            if !model.elements.contains_key(&child_id) {
                continue;
            }
            if seen_pairs.insert((rel.parent, child_id)) {
                emit_sub_element_triple(model, base, rel.parent, child_id, emit)?;
            }
        }
    }

    if let Some(topology) = topology {
        for (host_id, element_id) in topology.core_pairs_of_kind(TopologyEdgeKind::HasSubElement) {
            if seen_pairs.insert((host_id, element_id)) {
                emit_sub_element_triple(model, base, host_id, element_id, emit)?;
            }
        }
    } else {
        let topology = build_topology(model);
        for (host_id, element_id) in topology.core_pairs_of_kind(TopologyEdgeKind::HasSubElement) {
            if seen_pairs.insert((host_id, element_id)) {
                emit_sub_element_triple(model, base, host_id, element_id, emit)?;
            }
        }
    }

    Ok(())
}

fn emit_sub_element_triple<E, F>(
    model: &IfcModel,
    base: &str,
    parent_id: EntityId,
    child_id: EntityId,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    let Some(parent_subject) = object_subject(model, base, parent_id) else {
        return Ok(());
    };
    let Some(child_subject) = object_subject(model, base, child_id) else {
        return Ok(());
    };
    emit(Triple {
        subject: parent_subject,
        predicate: bot_has_sub_element(),
        object: Object::Iri(child_subject),
    })
}

fn baseline_containment_closure(model: &IfcModel, root_id: EntityId) -> Vec<EntityId> {
    let mut result = vec![root_id];
    let Some(root) = model.elements.get(&root_id) else {
        return result;
    };

    if !matches!(
        root.entity_name.as_str(),
        "IFCROOF" | "IFCSTAIR" | "IFCRAILING"
    ) {
        return result;
    }

    let children_of = element_children_map(model);
    let mut stack = children_of.get(&root_id).cloned().unwrap_or_default();
    while let Some(element_id) = stack.pop() {
        if result.contains(&element_id) {
            continue;
        }
        result.push(element_id);
        if let Some(children) = children_of.get(&element_id) {
            stack.extend(children.iter().copied());
        }
    }

    result.sort_unstable();
    result
}

fn element_children_map(model: &IfcModel) -> HashMap<EntityId, Vec<EntityId>> {
    let mut children_of = HashMap::new();
    for rel in &model.rel_aggregates {
        if !model.elements.contains_key(&rel.parent) {
            continue;
        }
        for &child_id in &rel.children {
            if model.elements.contains_key(&child_id) {
                children_of
                    .entry(rel.parent)
                    .or_insert_with(Vec::new)
                    .push(child_id);
            }
        }
    }
    children_of
}

fn adjacent_elements_by_space(model: &IfcModel) -> HashMap<EntityId, Vec<EntityId>> {
    let mut adjacent_by_space = HashMap::new();
    for rel in &model.rel_space_boundaries {
        let Some(element_id) = rel.element else {
            continue;
        };
        if model.spatial_nodes.contains_key(&rel.space) && model.elements.contains_key(&element_id)
        {
            adjacent_by_space
                .entry(rel.space)
                .or_insert_with(Vec::new)
                .push(element_id);
        }
    }
    for element_ids in adjacent_by_space.values_mut() {
        element_ids.sort_unstable();
        element_ids.dedup();
    }
    adjacent_by_space
}

fn normalize_base_uri(base_uri: &str) -> String {
    base_uri.trim_end_matches('/').to_string()
}

fn ifcowl_namespace(schema: StepSchema) -> String {
    match schema {
        StepSchema::Ifc2x3 => "https://standards.buildingsmart.org/IFC/DEV/IFC2x3/TC1/OWL#",
        StepSchema::Ifc4 => "https://standards.buildingsmart.org/IFC/DEV/IFC4/ADD2/OWL#",
        StepSchema::Ifc4x1 => "https://standards.buildingsmart.org/IFC/DEV/IFC4_1/OWL#",
        StepSchema::Ifc4x3Rc1 => "https://standards.buildingsmart.org/IFC/DEV/IFC4_3/RC1/OWL#",
        StepSchema::Ifc4x3Add2 => "https://w3id.org/ifc/IFC4X3_ADD2#",
    }
    .to_string()
}

#[derive(Debug)]
struct IfcOwlSchemaLookup {
    class_local_names: HashMap<String, String>,
    arg_predicates: HashMap<String, Vec<String>>,
    predicate_ranges: HashMap<String, String>,
    local_names: HashMap<String, String>,
}

impl IfcOwlSchemaLookup {
    fn class_local_name(&self, entity_name: &str) -> Option<&str> {
        self.class_local_names
            .get(&entity_name.to_ascii_uppercase())
            .map(String::as_str)
    }

    fn arg_predicates(&self, entity_name: &str) -> Option<&[String]> {
        self.arg_predicates
            .get(&entity_name.to_ascii_uppercase())
            .map(Vec::as_slice)
    }

    fn predicate_range(&self, predicate_local_name: &str) -> Option<&str> {
        self.predicate_ranges
            .get(predicate_local_name)
            .map(String::as_str)
    }

    fn canonical_local_name(&self, raw: &str) -> Option<&str> {
        self.local_names
            .get(&raw.to_ascii_uppercase())
            .map(String::as_str)
    }
}

fn ifcowl_lookup(schema: StepSchema) -> &'static IfcOwlSchemaLookup {
    static IFC2X3: OnceLock<IfcOwlSchemaLookup> = OnceLock::new();
    static IFC4: OnceLock<IfcOwlSchemaLookup> = OnceLock::new();
    static IFC4X1: OnceLock<IfcOwlSchemaLookup> = OnceLock::new();
    static IFC4X3_RC1: OnceLock<IfcOwlSchemaLookup> = OnceLock::new();
    static IFC4X3_ADD2: OnceLock<IfcOwlSchemaLookup> = OnceLock::new();

    match schema {
        StepSchema::Ifc2x3 => IFC2X3.get_or_init(|| {
            build_ifcowl_lookup(
                include_str!("../resources/proplistIFC2X3_TC1.csv"),
                include_str!("../resources/IFC2X3_TC1.ttl"),
            )
        }),
        StepSchema::Ifc4 => IFC4.get_or_init(|| {
            build_ifcowl_lookup(
                include_str!("../resources/proplistIFC4.csv"),
                include_str!("../resources/IFC4.ttl"),
            )
        }),
        StepSchema::Ifc4x1 => IFC4X1.get_or_init(|| {
            build_ifcowl_lookup(
                include_str!("../resources/proplistIFC4x1.csv"),
                include_str!("../resources/IFC4x1.ttl"),
            )
        }),
        StepSchema::Ifc4x3Rc1 => IFC4X3_RC1.get_or_init(|| {
            build_ifcowl_lookup(
                include_str!("../resources/proplistIFC4x3_RC1.csv"),
                include_str!("../resources/IFC4x3_RC1.ttl"),
            )
        }),
        StepSchema::Ifc4x3Add2 => IFC4X3_ADD2.get_or_init(|| {
            build_ifcowl_lookup(
                include_str!("../resources/proplistIFC4x3_ADD2.csv"),
                include_str!("../resources/IFC4x3_ADD2.ttl"),
            )
        }),
    }
}

fn build_ifcowl_lookup(csv: &str, ontology_ttl: &str) -> IfcOwlSchemaLookup {
    let mut class_local_names = HashMap::new();
    let mut arg_predicates = HashMap::new();
    let mut local_names = parse_ontology_local_names(ontology_ttl);

    for line in csv.lines() {
        let mut parts = line.split(',');
        let Some(entity_name) = parts.next() else {
            continue;
        };
        let _attribute_name = parts.next();
        let Some(predicate_name) = parts.next() else {
            continue;
        };

        let entity_key = entity_name.trim().to_ascii_uppercase();
        local_names
            .entry(entity_key.clone())
            .or_insert_with(|| entity_name.trim().to_string());
        class_local_names
            .entry(entity_key.clone())
            .or_insert_with(|| entity_name.trim().to_string());
        arg_predicates
            .entry(entity_key)
            .or_insert_with(Vec::new)
            .push(lowercase_initial_ascii(predicate_name.trim()));
    }

    let predicate_ranges = parse_predicate_ranges(ontology_ttl);

    IfcOwlSchemaLookup {
        class_local_names,
        arg_predicates,
        predicate_ranges,
        local_names,
    }
}

fn parse_ontology_local_names(ontology_ttl: &str) -> HashMap<String, String> {
    let mut local_names = HashMap::new();
    let mut parser = TurtleParser::new(ontology_ttl.as_bytes(), None);
    parser
        .parse_all(&mut |triple| {
            if let Subject::NamedNode(subject) = triple.subject {
                if let Some(local_name) = iri_local_name(subject) {
                    local_names
                        .entry(local_name.to_ascii_uppercase())
                        .or_insert_with(|| local_name.to_string());
                }
            }
            if let Some(local_name) = iri_local_name(triple.predicate) {
                local_names
                    .entry(local_name.to_ascii_uppercase())
                    .or_insert_with(|| local_name.to_string());
            }
            if let Term::NamedNode(object) = triple.object {
                if let Some(local_name) = iri_local_name(object) {
                    local_names
                        .entry(local_name.to_ascii_uppercase())
                        .or_insert_with(|| local_name.to_string());
                }
            }
            Ok(()) as Result<(), rio_turtle::TurtleError>
        })
        .expect("embedded IfcOWL ontology TTL should parse");
    local_names
}

fn parse_predicate_ranges(ontology_ttl: &str) -> HashMap<String, String> {
    const RDFS_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";

    let mut ranges = HashMap::new();
    let mut parser = TurtleParser::new(ontology_ttl.as_bytes(), None);
    parser
        .parse_all(&mut |triple| {
            if triple.predicate.iri != RDFS_RANGE {
                return Ok(()) as Result<(), rio_turtle::TurtleError>;
            }

            let Subject::NamedNode(subject) = triple.subject else {
                return Ok(());
            };
            let Term::NamedNode(range) = triple.object else {
                return Ok(());
            };

            let Some(subject_local_name) = iri_local_name(subject) else {
                return Ok(());
            };
            let Some(range_local_name) = iri_local_name(range) else {
                return Ok(());
            };

            ranges.insert(subject_local_name.to_string(), range_local_name.to_string());
            Ok(())
        })
        .expect("embedded IfcOWL ontology TTL should parse");
    ranges
}

fn iri_local_name(node: NamedNode<'_>) -> Option<&str> {
    node.iri.rsplit_once('#').map(|(_, local)| local)
}

fn ifcowl_class(namespace: &str, lookup: &IfcOwlSchemaLookup, entity_name: &str) -> String {
    let local_name = lookup
        .canonical_local_name(entity_name)
        .map(str::to_owned)
        .unwrap_or_else(|| pascal_ifc_name(entity_name));
    format!("{namespace}{local_name}")
}

fn ifcowl_property(namespace: &str, local_name: &str) -> String {
    format!("{namespace}{local_name}")
}

struct IfcOwlEmitter<'a> {
    base: &'a str,
    namespace: &'a str,
    lookup: &'a IfcOwlSchemaLookup,
    entity_subjects: &'a HashMap<EntityId, String>,
    triples: Vec<Triple>,
    node_counter: u64,
    /// ID of the entity currently being emitted; used for stable IRI generation.
    current_entity_id: EntityId,
    /// Per-entity sub-counter for list nodes (resets at start of each entity).
    entity_local_counter: u32,
    scalar_cache: HashMap<ScalarCacheValue, String>,
}

impl<'a> IfcOwlEmitter<'a> {
    fn new(
        base: &'a str,
        namespace: &'a str,
        lookup: &'a IfcOwlSchemaLookup,
        node_counter_start: EntityId,
        entity_subjects: &'a HashMap<EntityId, String>,
        emit_header: bool,
    ) -> Self {
        let mut emitter = Self {
            base,
            namespace,
            lookup,
            entity_subjects,
            triples: Vec::new(),
            node_counter: node_counter_start,
            current_entity_id: 0,
            entity_local_counter: 0,
            scalar_cache: HashMap::new(),
        };
        if emit_header {
            emitter.emit_ontology_header();
        }
        emitter
    }

    fn finish(self) -> Vec<Triple> {
        self.triples
    }

    fn pending_len(&self) -> usize {
        self.triples.len()
    }

    fn take_triples(&mut self) -> Vec<Triple> {
        std::mem::take(&mut self.triples)
    }

    fn emit_entity(&mut self, id: EntityId, entity: &ifc_step::RawEntity) {
        self.current_entity_id = id;
        self.entity_local_counter = 0;
        let subject = self.entity_subjects.get(&id).cloned().unwrap_or_else(|| {
            ifcowl_entity_iri(
                self.base,
                self.lookup.class_local_name(entity.entity_name.as_str()),
                id,
            )
        });
        self.push_type(
            &subject,
            ifcowl_class(self.namespace, self.lookup, entity.entity_name.as_str()),
        );

        if let Some(predicates) = self.lookup.arg_predicates(entity.entity_name.as_str()) {
            for (predicate, arg) in predicates.iter().zip(entity.args.iter()) {
                self.emit_predicate_value(&subject, predicate, arg);
            }

            for (index, arg) in entity.args.iter().enumerate().skip(predicates.len()) {
                self.emit_predicate_value(&subject, &format!("arg_{}", index + 1), arg);
            }
        } else {
            for (index, arg) in entity.args.iter().enumerate() {
                self.emit_predicate_value(&subject, &format!("arg_{}", index + 1), arg);
            }
        }
    }

    fn emit_predicate_value(
        &mut self,
        subject: &str,
        predicate_local_name: &str,
        value: &StepValue,
    ) {
        let expected_range = self.lookup.predicate_range(predicate_local_name);
        if let StepValue::List(items) = value {
            // Non-list-class predicates where every item is a Ref or a Typed value:
            // emit each item as a direct predicate value (no wrapper list node).
            // Covers pure-ref lists (e.g. IfcRelAggregates.relatedObjects) AND
            // typed-value SELECT lists (e.g. IfcTrimmedCurve.trim1/trim2 whose items
            // are IfcTrimmingSelect — a SELECT type, not an entity/list resource).
            // Bare-scalar lists (e.g. IFCSITE refLatitude = (41,52,27,840000)) fall
            // through to emit_value → emit_list so they create proper list nodes.
            let all_ref_or_typed = items
                .iter()
                .all(|item| matches!(item, StepValue::Ref(_) | StepValue::Typed { .. }));
            if !is_list_class_name(expected_range) && all_ref_or_typed {
                for item in items {
                    // For Ref items keep expected_range so the entity IRI is resolved;
                    // for Typed items pass None and let them emit their own scalar IRI.
                    let range = if matches!(item, StepValue::Ref(_)) {
                        expected_range
                    } else {
                        None
                    };
                    if let Some(object) = self.emit_value(range, item) {
                        self.triples.push(Triple {
                            subject: subject.to_string(),
                            predicate: ifcowl_property(self.namespace, predicate_local_name),
                            object,
                        });
                    }
                }
                return;
            }
        }

        if let Some(object) = self.emit_value(expected_range, value) {
            self.triples.push(Triple {
                subject: subject.to_string(),
                predicate: ifcowl_property(self.namespace, predicate_local_name),
                object,
            });
        }
    }

    fn emit_value(&mut self, expected_range: Option<&str>, value: &StepValue) -> Option<Object> {
        match value {
            StepValue::Ref(id) => self.entity_subjects.get(id).cloned().map(Object::Iri),
            StepValue::String(value) => {
                self.emit_scalar_resource(expected_range, ScalarValue::String(value.to_string()))
            }
            StepValue::Int(value) => {
                self.emit_scalar_resource(expected_range, ScalarValue::Integer(*value))
            }
            StepValue::Real(value) => {
                self.emit_scalar_resource(expected_range, ScalarValue::Double(*value))
            }
            StepValue::Bool(value) => {
                self.emit_scalar_resource(expected_range, ScalarValue::Boolean(*value))
            }
            StepValue::Enum(value) => Some(Object::Iri(format!("{}{value}", self.namespace))),
            StepValue::Null => None,
            StepValue::Derived => None,
            StepValue::List(items) => self.emit_list(expected_range, items),
            StepValue::Typed { type_name, value } => {
                self.emit_typed_value(expected_range, type_name.as_str(), value)
            }
        }
    }

    fn emit_typed_value(
        &mut self,
        expected_range: Option<&str>,
        type_name: &str,
        value: &StepValue,
    ) -> Option<Object> {
        let local_name = self
            .lookup
            .canonical_local_name(type_name)
            .map(str::to_owned)
            .unwrap_or_else(|| pascal_ifc_name(type_name));
        match value {
            StepValue::String(value) => {
                self.emit_scalar_resource(Some(&local_name), ScalarValue::String(value.to_string()))
            }
            StepValue::Int(value) => {
                self.emit_scalar_resource(Some(&local_name), ScalarValue::Integer(*value))
            }
            StepValue::Real(value) => {
                self.emit_scalar_resource(Some(&local_name), ScalarValue::Double(*value))
            }
            StepValue::Bool(value) => {
                self.emit_scalar_resource(Some(&local_name), ScalarValue::Boolean(*value))
            }
            StepValue::List(items) => self.emit_list(Some(&local_name), items),
            _ => self.emit_value(expected_range.or(Some(&local_name)), value),
        }
    }

    fn emit_list(&mut self, expected_range: Option<&str>, items: &[StepValue]) -> Option<Object> {
        if items.is_empty() {
            return None;
        }

        let (list_class, item_expected_range) = expected_range
            .and_then(compound_measure_list_shape)
            .map(|(list_class, item_range)| (list_class.to_string(), Some(item_range.to_string())))
            .unwrap_or_else(|| {
                let list_class = expected_range
                    .map(str::to_owned)
                    .or_else(|| infer_list_class_name(items))
                    .unwrap_or_else(|| "IfcValue_List".to_string());
                let item_expected_range = item_range_from_list_class(&list_class);
                (list_class, item_expected_range)
            });

        let mut node_subjects = Vec::with_capacity(items.len());
        if list_class == "IfcCompoundPlaneAngleMeasure" {
            // Use entity-scoped stable naming: IfcCompoundPlaneAngleMeasure_{entity_id}_{local_idx}
            // This makes IRIs deterministic regardless of emission order.
            for _ in items {
                let iri = format!(
                    "{}/{}_{}_{}",
                    self.base, list_class, self.current_entity_id, self.entity_local_counter
                );
                self.entity_local_counter += 1;
                node_subjects.push(iri);
            }
        } else {
            for _ in items {
                node_subjects.push(self.next_named_node(&list_class));
            }
        }

        for (index, (node_subject, item)) in node_subjects.iter().zip(items.iter()).enumerate() {
            self.push_type(node_subject, scalar_class_iri(self.namespace, &list_class));
            if let Some(object) = self.emit_value(item_expected_range.as_deref(), item) {
                self.triples.push(Triple {
                    subject: node_subject.clone(),
                    predicate: list_has_contents(),
                    object,
                });
            }
            if let Some(next_subject) = node_subjects.get(index + 1) {
                self.triples.push(Triple {
                    subject: node_subject.clone(),
                    predicate: list_has_next(),
                    object: Object::Iri(next_subject.clone()),
                });
            }
        }

        node_subjects.first().cloned().map(Object::Iri)
    }

    fn emit_scalar_resource(
        &mut self,
        expected_range: Option<&str>,
        value: ScalarValue,
    ) -> Option<Object> {
        let class_name = self.canonical_scalar_class(expected_range?)?.to_string();
        let cache_key = value.cache_value(&class_name);
        if let Some(subject) = self.scalar_cache.get(&cache_key) {
            return Some(Object::Iri(subject.clone()));
        }
        let subject = self.next_named_node(&class_name);

        self.push_type(&subject, scalar_class_iri(self.namespace, &class_name));
        let (predicate, object) = value.express_predicate_and_object(&class_name);
        self.triples.push(Triple {
            subject: subject.clone(),
            predicate,
            object,
        });
        self.scalar_cache.insert(cache_key, subject.clone());
        Some(Object::Iri(subject))
    }

    fn canonical_scalar_class<'b>(&'b self, class_name: &'b str) -> Option<&'b str> {
        self.lookup
            .canonical_local_name(class_name)
            .or(Some(class_name))
    }

    fn emit_ontology_header(&mut self) {
        let subject = format!("{}/", self.base);
        self.push_type(&subject, owl_ontology());
        self.triples.push(Triple {
            subject: subject.clone(),
            predicate: owl_imports(),
            object: Object::Iri(self.namespace.to_string()),
        });
    }

    fn next_named_node(&mut self, class_name: &str) -> String {
        self.node_counter += 1;
        format!("{}/{}_{}", self.base, class_name, self.node_counter)
    }

    fn push_type(&mut self, subject: &str, class_iri: String) {
        self.triples.push(Triple {
            subject: subject.to_string(),
            predicate: rdf_type(),
            object: Object::Iri(class_iri),
        });
    }
}

enum ScalarValue {
    String(String),
    Integer(i64),
    Double(f64),
    Boolean(bool),
}

impl ScalarValue {
    fn cache_value(&self, class_name: &str) -> ScalarCacheValue {
        match self {
            ScalarValue::String(value) => {
                ScalarCacheValue::String(value.clone(), class_name.to_string())
            }
            ScalarValue::Integer(value) => ScalarCacheValue::Integer(*value),
            ScalarValue::Double(value) => ScalarCacheValue::Double(value.to_bits()),
            ScalarValue::Boolean(value) => ScalarCacheValue::Boolean(
                *value,
                if class_name.eq_ignore_ascii_case("LOGICAL")
                    || class_name.eq_ignore_ascii_case("IfcLogical")
                {
                    BooleanScalarKind::Logical
                } else {
                    BooleanScalarKind::Boolean
                },
            ),
        }
    }

    fn express_predicate_and_object(self, class_name: &str) -> (String, Object) {
        match self {
            ScalarValue::String(value) => (express_has_string(), Object::Literal(value)),
            ScalarValue::Integer(value) => (
                express_has_integer(),
                Object::TypedLiteral {
                    value: value.to_string(),
                    datatype: format!("{XSD}integer"),
                },
            ),
            ScalarValue::Double(value) => (
                express_has_double(),
                Object::TypedLiteral {
                    value: canonicalize_decimal(value),
                    datatype: format!("{XSD}double"),
                },
            ),
            ScalarValue::Boolean(value) => {
                if class_name.eq_ignore_ascii_case("LOGICAL")
                    || class_name.eq_ignore_ascii_case("IfcLogical")
                {
                    (
                        express_has_logical(),
                        Object::Iri(express_logical_value(value)),
                    )
                } else {
                    (
                        express_has_boolean(),
                        Object::TypedLiteral {
                            value: value.to_string(),
                            datatype: format!("{XSD}boolean"),
                        },
                    )
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ScalarCacheValue {
    String(String, String), // (value, class_name)
    Integer(i64),
    Double(u64),
    Boolean(bool, BooleanScalarKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BooleanScalarKind {
    Boolean,
    Logical,
}

fn emit_property_declaration<E, F>(
    declared: &mut HashSet<String>,
    declared_comments: &mut HashSet<(String, String)>,
    predicate_local: &str,
    set_name: &str,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    let predicate = props_property(predicate_local);
    if declared.insert(predicate.clone()) {
        emit(Triple {
            subject: predicate.clone(),
            predicate: rdf_type(),
            object: Object::Iri(owl_object_property()),
        })?;
    }
    let comment = format!(
        "IFC property set {} property {predicate_local}",
        java_ifc_escape_text(set_name)
    );
    if declared_comments.insert((predicate.clone(), comment.clone())) {
        emit(Triple {
            subject: predicate,
            predicate: rdfs_comment(),
            object: Object::Literal(comment),
        })?;
    }
    Ok(())
}

fn emit_property_state<E, F>(
    subject: &str,
    base: &str,
    predicate_local: &str,
    label: Option<String>,
    object_guid: &str,
    set_scope: &str,
    state_kind: char,
    state_counter: &mut u64,
    value: Object,
    unit: Option<String>,
    generated_at: &str,
    emit: &mut F,
) -> Result<String, E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    let property_subject = property_resource_iri(base, predicate_local, object_guid, set_scope);
    let state_subject = property_state_iri(
        base,
        predicate_local,
        object_guid,
        set_scope,
        state_kind,
        *state_counter,
    );
    *state_counter += 1;

    emit(Triple {
        subject: subject.to_string(),
        predicate: props_property(predicate_local),
        object: Object::Iri(property_subject.clone()),
    })?;
    emit(Triple {
        subject: property_subject.clone(),
        predicate: rdf_type(),
        object: Object::Iri(opm_property()),
    })?;
    if let Some(label) = label {
        emit(Triple {
            subject: property_subject.clone(),
            predicate: rdfs_label(),
            object: Object::Literal(label),
        })?;
    }
    emit(Triple {
        subject: property_subject.clone(),
        predicate: opm_has_property_state(),
        object: Object::Iri(state_subject.clone()),
    })?;
    emit(Triple {
        subject: property_subject.clone(),
        predicate: opm_current_property_state_predicate(),
        object: Object::Iri(state_subject.clone()),
    })?;
    emit(Triple {
        subject: state_subject.clone(),
        predicate: rdf_type(),
        object: Object::Iri(opm_current_property_state()),
    })?;
    emit(Triple {
        subject: state_subject.clone(),
        predicate: prov_generated_at_time(),
        object: Object::TypedLiteral {
            value: generated_at.to_string(),
            datatype: format!("{XSD}dateTime"),
        },
    })?;
    emit(Triple {
        subject: state_subject.clone(),
        predicate: schema_value(),
        object: value,
    })?;
    if let Some(unit) = unit {
        emit(Triple {
            subject: state_subject,
            predicate: smls_unit(),
            object: Object::Iri(unit),
        })?;
    }
    Ok(property_subject)
}

fn emit_standard_attribute_triples<E, F>(
    model: &IfcModel,
    base: &str,
    unit_by_type: &HashMap<String, String>,
    generated_at: &str,
    attribute_state_counter: &mut u64,
    declared_object_properties: &mut HashSet<String>,
    declared_property_comments: &mut HashSet<(String, String)>,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    for node in sorted_values(&model.spatial_nodes) {
        let subject = spatial_resource_iri(base, node.spatial_type, &node.guid);
        emit_standard_attribute(
            &subject,
            base,
            "globalIdIfcRoot",
            &node.guid,
            'a',
            attribute_state_counter,
            Object::Literal(node.guid.to_string()),
            generated_at,
            None,
            declared_object_properties,
            declared_property_comments,
            emit,
        )?;
        if let Some(name) = node.name.as_ref() {
            emit_standard_attribute(
                &subject,
                base,
                "nameIfcRoot",
                &node.guid,
                'a',
                attribute_state_counter,
                Object::Literal(name.to_string()),
                generated_at,
                None,
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(description) = node.description.as_ref().filter(|d| !d.is_empty()) {
            emit_standard_attribute(
                &subject,
                base,
                "descriptionIfcRoot",
                &node.guid,
                'a',
                attribute_state_counter,
                Object::Literal(description.to_string()),
                generated_at,
                None,
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(object_type) = node.object_type.as_ref() {
            emit_standard_attribute(
                &subject,
                base,
                "objectTypeIfcObject",
                &node.guid,
                'a',
                attribute_state_counter,
                Object::Literal(object_type.to_string()),
                generated_at,
                None,
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(long_name) = node.long_name.as_ref() {
            emit_standard_attribute(
                &subject,
                base,
                match model.schema {
                    StepSchema::Ifc2x3 => "longNameIfcSpatialStructureElement",
                    _ => "longNameIfcSpatialElement",
                },
                &node.guid,
                'a',
                attribute_state_counter,
                Object::Literal(long_name.to_string()),
                generated_at,
                None,
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(elevation) = node.elevation {
            emit_standard_attribute(
                &subject,
                base,
                "elevationIfcBuildingStorey",
                &node.guid,
                'a',
                attribute_state_counter,
                Object::TypedLiteral {
                    value: elevation.to_string(),
                    datatype: format!("{XSD}double"),
                },
                generated_at,
                unit_by_type.get("LENGTHUNIT").cloned(),
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(ref_elevation) = node.ref_elevation {
            emit_standard_attribute(
                &subject,
                base,
                "refElevationIfcSite",
                &node.guid,
                'a',
                attribute_state_counter,
                Object::TypedLiteral {
                    value: ref_elevation.to_string(),
                    datatype: format!("{XSD}double"),
                },
                generated_at,
                unit_by_type.get("LENGTHUNIT").cloned(),
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(elevation_of_ref_height) = node.elevation_of_ref_height {
            emit_standard_attribute(
                &subject,
                base,
                "elevationOfRefHeightIfcBuilding",
                &node.guid,
                'a',
                attribute_state_counter,
                Object::TypedLiteral {
                    value: elevation_of_ref_height.to_string(),
                    datatype: format!("{XSD}double"),
                },
                generated_at,
                unit_by_type.get("LENGTHUNIT").cloned(),
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(elevation_of_terrain) = node.elevation_of_terrain {
            emit_standard_attribute(
                &subject,
                base,
                "elevationOfTerrainIfcBuilding",
                &node.guid,
                'a',
                attribute_state_counter,
                Object::TypedLiteral {
                    value: elevation_of_terrain.to_string(),
                    datatype: format!("{XSD}double"),
                },
                generated_at,
                unit_by_type.get("LENGTHUNIT").cloned(),
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
    }

    for element in sorted_values(&model.elements) {
        let subject = element_resource_iri(base, element);
        emit_standard_attribute(
            &subject,
            base,
            "globalIdIfcRoot",
            &element.guid,
            'a',
            attribute_state_counter,
            Object::Literal(element.guid.to_string()),
            generated_at,
            None,
            declared_object_properties,
            declared_property_comments,
            emit,
        )?;
        if let Some(name) = element.name.as_ref() {
            emit_standard_attribute(
                &subject,
                base,
                "nameIfcRoot",
                &element.guid,
                'a',
                attribute_state_counter,
                Object::Literal(name.to_string()),
                generated_at,
                None,
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(description) = element.description.as_ref().filter(|d| !d.is_empty()) {
            emit_standard_attribute(
                &subject,
                base,
                "descriptionIfcRoot",
                &element.guid,
                'a',
                attribute_state_counter,
                Object::Literal(description.to_string()),
                generated_at,
                None,
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(object_type) = element.object_type.as_ref() {
            emit_standard_attribute(
                &subject,
                base,
                "objectTypeIfcObject",
                &element.guid,
                'a',
                attribute_state_counter,
                Object::Literal(object_type.to_string()),
                generated_at,
                None,
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(tag) = element.tag.as_ref() {
            emit_standard_attribute(
                &subject,
                base,
                "batid",
                &element.guid,
                'a',
                attribute_state_counter,
                Object::Literal(tag.to_string()),
                generated_at,
                None,
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(overall_height) = element.overall_height {
            emit_standard_attribute(
                &subject,
                base,
                match element.entity_name.as_str() {
                    "IFCDOOR" => "overallHeightIfcDoor",
                    "IFCWINDOW" => "overallHeightIfcWindow",
                    _ => "overallHeight",
                },
                &element.guid,
                'a',
                attribute_state_counter,
                Object::TypedLiteral {
                    value: overall_height.to_string(),
                    datatype: format!("{XSD}double"),
                },
                generated_at,
                unit_by_type.get("LENGTHUNIT").cloned(),
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(overall_width) = element.overall_width {
            emit_standard_attribute(
                &subject,
                base,
                match element.entity_name.as_str() {
                    "IFCDOOR" => "overallWidthIfcDoor",
                    "IFCWINDOW" => "overallWidthIfcWindow",
                    _ => "overallWidth",
                },
                &element.guid,
                'a',
                attribute_state_counter,
                Object::TypedLiteral {
                    value: overall_width.to_string(),
                    datatype: format!("{XSD}double"),
                },
                generated_at,
                unit_by_type.get("LENGTHUNIT").cloned(),
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(number_of_risers) = element.number_of_risers {
            emit_standard_attribute(
                &subject,
                base,
                "numberOfRiserIfcStairFlight",
                &element.guid,
                'a',
                attribute_state_counter,
                Object::TypedLiteral {
                    value: number_of_risers.to_string(),
                    datatype: format!("{XSD}integer"),
                },
                generated_at,
                None,
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(number_of_treads) = element.number_of_treads {
            emit_standard_attribute(
                &subject,
                base,
                "numberOfTreadsIfcStairFlight",
                &element.guid,
                'a',
                attribute_state_counter,
                Object::TypedLiteral {
                    value: number_of_treads.to_string(),
                    datatype: format!("{XSD}integer"),
                },
                generated_at,
                None,
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(riser_height) = element.riser_height {
            emit_standard_attribute(
                &subject,
                base,
                "riserHeightIfcStairFlight",
                &element.guid,
                'a',
                attribute_state_counter,
                Object::TypedLiteral {
                    value: riser_height.to_string(),
                    datatype: format!("{XSD}double"),
                },
                generated_at,
                unit_by_type.get("LENGTHUNIT").cloned(),
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
        if let Some(tread_length) = element.tread_length {
            emit_standard_attribute(
                &subject,
                base,
                "treadLengthIfcStairFlight",
                &element.guid,
                'a',
                attribute_state_counter,
                Object::TypedLiteral {
                    value: tread_length.to_string(),
                    datatype: format!("{XSD}double"),
                },
                generated_at,
                unit_by_type.get("LENGTHUNIT").cloned(),
                declared_object_properties,
                declared_property_comments,
                emit,
            )?;
        }
    }

    Ok(())
}

fn emit_bounding_box_geometries<E, F>(
    model: &IfcModel,
    base: &str,
    boxes: Option<&HashMap<EntityId, BoundingBox>>,
    wkts: Option<&HashMap<EntityId, String>>,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    let mut object_ids: Vec<EntityId> = Vec::new();
    if let Some(map) = boxes {
        object_ids.extend(map.keys().copied());
    }
    if let Some(map) = wkts {
        object_ids.extend(map.keys().copied());
    }
    object_ids.sort_unstable();
    object_ids.dedup();
    for object_id in object_ids {
        let Some((subject, object_guid)) = object_subject_and_guid(model, base, object_id) else {
            continue;
        };
        let wkt = if let Some(wkt_map) = wkts.and_then(|m| m.get(&object_id)) {
            wkt_map.clone()
        } else if let Some(bbox) = boxes.and_then(|m| m.get(&object_id)) {
            let dx = (bbox.x_max - bbox.x_min).abs();
            let dy = (bbox.y_max - bbox.y_min).abs();
            let dz = (bbox.z_max - bbox.z_min).abs();
            if dx <= f64::EPSILON && dy <= f64::EPSILON && dz <= f64::EPSILON {
                continue;
            }
            bbox_wkt_polyhedral_surface(bbox)
        } else {
            continue;
        };

        let geometry_subject = geometry_resource_iri(base, &object_guid);
        emit(Triple {
            subject: subject.clone(),
            predicate: lbd_has_bounding_box(),
            object: Object::Iri(geometry_subject.clone()),
        })?;
        emit(Triple {
            subject: geometry_subject.clone(),
            predicate: rdf_type(),
            object: Object::Iri(geo_geometry()),
        })?;
        emit(Triple {
            subject: geometry_subject,
            predicate: geo_as_wkt(),
            object: Object::TypedLiteral {
                value: wkt,
                datatype: geo_wkt_literal(),
            },
        })?;
    }
    Ok(())
}

fn emit_standard_attribute<E, F>(
    subject: &str,
    base: &str,
    predicate_local: &str,
    object_guid: &str,
    state_kind: char,
    state_counter: &mut u64,
    value: Object,
    generated_at: &str,
    unit: Option<String>,
    declared_object_properties: &mut HashSet<String>,
    declared_property_comments: &mut HashSet<(String, String)>,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    let predicate = props_property(predicate_local);
    if declared_object_properties.insert(predicate.clone()) {
        emit(Triple {
            subject: predicate.clone(),
            predicate: rdf_type(),
            object: Object::Iri(owl_object_property()),
        })?;
    }
    let comment = format!("IFC standard attribute {predicate_local}");
    if declared_property_comments.insert((predicate.clone(), comment.clone())) {
        emit(Triple {
            subject: predicate,
            predicate: rdfs_comment(),
            object: Object::Literal(comment),
        })?;
    }

    let _ = emit_property_state(
        subject,
        base,
        predicate_local,
        None,
        object_guid,
        "standardAttributes",
        state_kind,
        state_counter,
        value,
        unit,
        generated_at,
        emit,
    )?;
    Ok(())
}

fn emit_geometry_declarations<E, F>(emit: &mut F) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    emit(Triple {
        subject: lbd_has_bounding_box(),
        predicate: rdf_type(),
        object: Object::Iri(owl_object_property()),
    })?;
    Ok(())
}

fn infer_list_class_name(items: &[StepValue]) -> Option<String> {
    let first = items.first()?;
    Some(match first {
        StepValue::Ref(_) => "IfcValue_List".to_string(),
        StepValue::String(_) => "IfcLabel_List".to_string(),
        StepValue::Int(_) => "IfcInteger_List".to_string(),
        StepValue::Real(_) => "IfcReal_List".to_string(),
        StepValue::Bool(_) => "IfcBoolean_List".to_string(),
        StepValue::Enum(_) => "IfcValue_List".to_string(),
        StepValue::List(nested) => format!("{}_List", infer_list_class_name(nested)?),
        StepValue::Typed { type_name, .. } => format!("{}_List", pascal_ifc_name(type_name)),
        StepValue::Null | StepValue::Derived => "IfcValue_List".to_string(),
    })
}

fn item_range_from_list_class(list_class: &str) -> Option<String> {
    list_class.strip_suffix("_List").map(str::to_owned)
}

fn is_list_class_name(class_name: Option<&str>) -> bool {
    class_name.is_some_and(|name| name.ends_with("_List"))
}

fn compound_measure_list_shape(class_name: &str) -> Option<(&'static str, &'static str)> {
    match class_name {
        "IfcCompoundPlaneAngleMeasure" => Some(("IfcCompoundPlaneAngleMeasure", "INTEGER")),
        _ => None,
    }
}

fn lowercase_initial_ascii(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => {
            let mut out = String::with_capacity(value.len());
            out.push(first.to_ascii_lowercase());
            out.push_str(chars.as_str());
            out
        }
        None => String::new(),
    }
}

fn pascal_ifc_name(entity_name: &str) -> String {
    let upper = entity_name.to_ascii_uppercase();
    if !upper.starts_with("IFC") {
        return entity_name.to_string();
    }
    let mut out = String::from("Ifc");
    let mut capitalize = true;
    for ch in upper[3..].chars() {
        if ch == '_' {
            capitalize = true;
        } else if ch.is_ascii_digit() {
            out.push(ch);
            capitalize = true;
        } else if capitalize {
            out.push(ch.to_ascii_uppercase());
            capitalize = false;
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

fn spatial_segment(spatial_type: SpatialType) -> &'static str {
    match spatial_type {
        SpatialType::Project => "project",
        SpatialType::Site => "site",
        SpatialType::Building => "building",
        SpatialType::Storey => "storey",
        SpatialType::Space => "space",
    }
}

pub(crate) fn spatial_class(spatial_type: SpatialType) -> String {
    match spatial_type {
        SpatialType::Project => lbd_project(),
        SpatialType::Site => bot_site(),
        SpatialType::Building => bot_building(),
        SpatialType::Storey => bot_storey(),
        SpatialType::Space => bot_space(),
    }
}

fn lbd_local_name(prefix: &str, guid: &str) -> String {
    let suffix = canonical_guid_token(guid);
    format!("{prefix}_{suffix}")
}

pub(crate) fn spatial_resource_iri(base: &str, spatial_type: SpatialType, guid: &str) -> String {
    format!(
        "{base}/{}",
        lbd_local_name(spatial_segment(spatial_type), guid)
    )
}

pub(crate) fn element_resource_iri(base: &str, element: &ifc_model::ElementNode) -> String {
    let prefix = match element.entity_name.as_str() {
        // Java uses the generic `buildingelement_<guid>` local name here.
        "IFCBUILDINGELEMENTPROXY" => "buildingelement".to_string(),
        _ => product_type_name(element.entity_name.as_str())
            .map(|name| name.to_ascii_lowercase())
            .unwrap_or_else(|| {
                format!(
                    "ifcowl_{}",
                    pascal_ifc_name(element.entity_name.as_str()).to_ascii_lowercase()
                )
            }),
    };
    format!("{base}/{}", lbd_local_name(&prefix, &element.guid))
}

fn property_resource_iri(base: &str, predicate_local: &str, guid: &str, set_scope: &str) -> String {
    let prop = short_property_key(predicate_local);
    let set_suffix = stable_short_guid_token(set_scope);
    let object_suffix = stable_short_guid_token(guid);
    format!("{base}/prop_{prop}_{set_suffix}_{object_suffix}")
}

fn property_set_resource_iri(base: &str, guid: &str) -> String {
    let suffix = canonical_guid_token(guid);
    format!("{base}/propertyset_{suffix}")
}

fn quantity_set_resource_iri(base: &str, guid: &str) -> String {
    let suffix = canonical_guid_token(guid);
    format!("{base}/quantityset_{suffix}")
}

fn geometry_resource_iri(base: &str, guid: &str) -> String {
    let suffix = canonical_guid_token(guid);
    format!("{base}/geometry_{suffix}")
}

fn canonical_guid_token(raw: &str) -> String {
    if raw.len() == 22 {
        return raw.to_string();
    }
    compress_uuid_string(raw).unwrap_or_else(|| raw.to_string())
}

fn bbox_wkt_polyhedral_surface(bbox: &BoundingBox) -> String {
    let x0 = canonicalize_decimal(bbox.x_min);
    let x1 = canonicalize_decimal(bbox.x_max);
    let y0 = canonicalize_decimal(bbox.y_min);
    let y1 = canonicalize_decimal(bbox.y_max);
    let z0 = canonicalize_decimal(bbox.z_min);
    let z1 = canonicalize_decimal(bbox.z_max);
    format!(
        "POLYHEDRALSURFACE Z ((({x0} {y0} {z0}, {x1} {y0} {z0}, {x1} {y1} {z0}, {x0} {y1} {z0}, {x0} {y0} {z0})), (({x0} {y0} {z1}, {x0} {y1} {z1}, {x1} {y1} {z1}, {x1} {y0} {z1}, {x0} {y0} {z1})), (({x0} {y0} {z0}, {x0} {y0} {z1}, {x1} {y0} {z1}, {x1} {y0} {z0}, {x0} {y0} {z0})), (({x1} {y0} {z0}, {x1} {y0} {z1}, {x1} {y1} {z1}, {x1} {y1} {z0}, {x1} {y0} {z0})), (({x1} {y1} {z0}, {x1} {y1} {z1}, {x0} {y1} {z1}, {x0} {y1} {z0}, {x1} {y1} {z0})), (({x0} {y1} {z0}, {x0} {y1} {z1}, {x0} {y0} {z1}, {x0} {y0} {z0}, {x0} {y1} {z0})))"
    )
}

fn topology_interface_resource_iri(base: &str, interface_id: EntityId) -> String {
    format!("{base}/interface_{interface_id}")
}

fn property_state_iri(
    base: &str,
    predicate_local: &str,
    guid: &str,
    set_scope: &str,
    state_kind: char,
    state_counter: u64,
) -> String {
    let prop = short_property_key(predicate_local);
    let object_suffix = stable_short_guid_token(guid);
    let set_suffix = stable_short_guid_token(set_scope);
    format!("{base}/state_{prop}_{set_suffix}_{object_suffix}_{state_kind}{state_counter}")
}

fn short_property_key(input: &str) -> String {
    let key: String = input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .take(20)
        .collect();
    if key.is_empty() {
        "value".to_string()
    } else {
        key
    }
}

fn stable_short_guid_token(raw: &str) -> String {
    let expanded = expand_ifc_guid(raw).unwrap_or_else(|| raw.to_string());
    format!("{:012x}", fnv1a64(expanded.as_bytes()))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn ifcowl_entity_iri(base: &str, class_local_name: Option<&str>, id: EntityId) -> String {
    let class_name = class_local_name.unwrap_or("IfcRoot");
    format!("{base}/{class_name}_{id}")
}

pub(crate) fn ifcowl_spatial_iri(base: &str, node: &ifc_model::SpatialNode) -> String {
    let class_name = match node.spatial_type {
        SpatialType::Project => "IfcProject",
        SpatialType::Site => "IfcSite",
        SpatialType::Building => "IfcBuilding",
        SpatialType::Storey => "IfcBuildingStorey",
        SpatialType::Space => "IfcSpace",
    };
    ifcowl_entity_iri(base, Some(class_name), node.id)
}

pub(crate) fn ifcowl_element_iri(base: &str, element: &ifc_model::ElementNode) -> String {
    let class_name = canonical_ifc_entity_local_name(element.entity_name.as_str());
    ifcowl_entity_iri(base, Some(&class_name), element.id)
}

pub(crate) fn lbd_product_class_iri(entity_name: &str, product_type: &str) -> String {
    match entity_name {
        "IFCFURNISHINGELEMENT" => furn_class(product_type),
        _ => beo_class(product_type),
    }
}

fn canonical_ifc_entity_local_name(entity_name: &str) -> String {
    match entity_name {
        "IFCBUILDINGELEMENTPROXY" => "IfcBuildingElementProxy".to_string(),
        "IFCCURTAINWALL" => "IfcCurtainWall".to_string(),
        "IFCELEMENTASSEMBLY" => "IfcElementAssembly".to_string(),
        "IFCFURNISHINGELEMENT" => "IfcFurnishingElement".to_string(),
        "IFCOPENINGELEMENT" => "IfcOpeningElement".to_string(),
        "IFCSTAIRFLIGHT" => "IfcStairFlight".to_string(),
        "IFCWALLSTANDARDCASE" => "IfcWallStandardCase".to_string(),
        _ => pascal_ifc_name(entity_name),
    }
}

fn ifcowl_entity_subjects(
    step: &StepFile,
    base: &str,
    lookup: &IfcOwlSchemaLookup,
) -> HashMap<EntityId, String> {
    step.entities
        .iter()
        .map(|(id, entity)| {
            (
                *id,
                ifcowl_entity_iri(
                    base,
                    lookup.class_local_name(entity.entity_name.as_str()),
                    *id,
                ),
            )
        })
        .collect()
}

fn scalar_class_iri(namespace: &str, class_name: &str) -> String {
    if let Some(item_class) = class_name.strip_suffix("_List") {
        if is_express_scalar_class(item_class) {
            return format!("{EXPRESS}{class_name}");
        }
    }

    if is_express_scalar_class(class_name) {
        return format!("{EXPRESS}{class_name}");
    }

    format!("{namespace}{class_name}")
}

fn is_express_scalar_class(class_name: &str) -> bool {
    match class_name {
        "INTEGER" | "REAL" | "NUMBER" | "BOOLEAN" | "LOGICAL" | "STRING" | "BINARY" => true,
        _ => false,
    }
}

fn object_subject(model: &IfcModel, base: &str, object_id: EntityId) -> Option<String> {
    if let Some(node) = model.spatial_nodes.get(&object_id) {
        return Some(spatial_resource_iri(base, node.spatial_type, &node.guid));
    }
    if let Some(element) = model.elements.get(&object_id) {
        return Some(element_resource_iri(base, element));
    }
    None
}

fn object_subject_and_guid(
    model: &IfcModel,
    base: &str,
    object_id: EntityId,
) -> Option<(String, String)> {
    if let Some(node) = model.spatial_nodes.get(&object_id) {
        return Some((
            spatial_resource_iri(base, node.spatial_type, &node.guid),
            node.guid.to_string(),
        ));
    }
    if let Some(element) = model.elements.get(&object_id) {
        return Some((
            element_resource_iri(base, element),
            element.guid.to_string(),
        ));
    }
    None
}

pub(crate) fn sorted_values<T>(map: &HashMap<EntityId, T>) -> Vec<&T> {
    let mut ids: Vec<_> = map.keys().copied().collect();
    ids.sort_unstable();
    ids.into_iter().map(|id| &map[&id]).collect()
}

fn property_label(set_name: Option<&str>, predicate_local: &str) -> String {
    match set_name {
        Some(name) if !name.is_empty() => {
            format!("{}:{predicate_local}", decode_ifc_unicode(name))
        }
        _ => predicate_local.to_string(),
    }
}

fn property_local_name(name: &str) -> String {
    let escaped = java_ifc_escape_text(&normalize_property_name_text(name));
    if escaped.to_ascii_uppercase() == escaped {
        let upper = escaped.replace(' ', "_");
        let local = if upper.is_empty() {
            "value".to_string()
        } else {
            utf8_percent_encode(&upper, NON_ALPHANUMERIC).to_string()
        };
        return ensure_predicate_starts_with_letter(local);
    }

    let mut out = String::new();
    let mut first_word = true;
    for word in escaped.split_whitespace() {
        let mut chars = word.chars().filter(|ch| ch.is_alphabetic());
        let Some(first) = chars.next() else {
            continue;
        };
        if first_word {
            out.extend(first.to_lowercase());
            first_word = false;
        } else {
            out.extend(first.to_uppercase());
        }
        out.extend(chars);
    }
    let local = if out.is_empty() {
        "value".to_string()
    } else {
        utf8_percent_encode(&out, NON_ALPHANUMERIC).to_string()
    };
    ensure_predicate_starts_with_letter(local)
}

fn ensure_predicate_starts_with_letter(local: String) -> String {
    match local.chars().next() {
        Some(ch) if ch.is_ascii_alphabetic() => local,
        Some(_) => format!("p_{local}"),
        None => "value".to_string(),
    }
}

fn normalize_property_name_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'Ä' => out.push_str("Ae"),
            'Ö' => out.push_str("Oe"),
            'Ü' => out.push_str("Ue"),
            'ß' => out.push_str("ss"),
            _ => out.push(ch),
        }
    }
    out
}

fn java_ifc_escape_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            out.push_str(&format!("\\X2\\{:04X}\\X0\\", ch as u32));
        }
    }
    out
}

fn property_value_object(property: &PropertySingleValue) -> Option<Object> {
    quantity_value_object(property.nominal_value.as_ref())
}

fn enumerated_value_object(property: &PropertyEnumeratedValue) -> Option<Object> {
    let v = property.first_value.as_ref()?;
    let trimmed = v.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(Object::Literal(trimmed.to_string()))
    }
}

fn quantity_value_object(value: Option<&StepValue>) -> Option<Object> {
    match value? {
        StepValue::String(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() || trimmed == "-1.#IND" {
                None
            } else {
                Some(Object::Literal(trimmed.to_string()))
            }
        }
        StepValue::Int(value) => Some(Object::TypedLiteral {
            value: value.to_string(),
            datatype: format!("{XSD}integer"),
        }),
        StepValue::Real(value) => value.is_finite().then(|| Object::TypedLiteral {
            value: canonicalize_decimal(*value),
            datatype: format!("{XSD}decimal"),
        }),
        StepValue::Bool(value) => Some(Object::TypedLiteral {
            value: value.to_string(),
            datatype: format!("{XSD}boolean"),
        }),
        StepValue::Enum(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() || trimmed == "-1.#IND" {
                None
            } else {
                Some(Object::Literal(trimmed.to_string()))
            }
        }
        StepValue::Typed { value, .. } => quantity_value_object(Some(value.as_ref())),
        _ => None,
    }
}

fn should_skip_named_self_value(name: &str, value: &Object) -> bool {
    match value {
        Object::Literal(literal) => literal.trim() == name.trim(),
        _ => false,
    }
}

fn canonicalize_decimal(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let rounded = (value * 1_000_000_000.0).round() / 1_000_000_000.0;
    let normalized = if rounded.abs() < 5e-10 { 0.0 } else { rounded };
    let mut out = format!("{normalized:.9}");
    while out.ends_with('0') {
        out.pop();
    }
    if out.ends_with('.') {
        out.pop();
    }
    if out == "-0" {
        "0".to_string()
    } else {
        out
    }
}

fn build_unit_type_map(model: &IfcModel) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for assignment in model.unit_assignments.values() {
        for unit_id in &assignment.units {
            let Some(unit) = model.units.get(unit_id) else {
                continue;
            };
            match unit {
                Unit::Si {
                    unit_type: Some(unit_type),
                    name: Some(name),
                    prefix,
                    ..
                } => {
                    if let Some(iri) = map_si_unit(name, prefix.as_deref()) {
                        map.insert(unit_type.to_string(), iri);
                    }
                }
                Unit::ConversionBased {
                    unit_type: Some(unit_type),
                    name: Some(name),
                    ..
                } => {
                    if let Some(iri) = map_conversion_unit(name) {
                        map.insert(unit_type.to_string(), iri);
                    }
                }
                _ => {}
            }
        }
    }
    map
}

fn map_si_unit(name: &str, prefix: Option<&str>) -> Option<String> {
    match (name, prefix) {
        ("METRE", None) => Some(unit_iri("M")),
        ("METRE", Some("MILLI")) => Some(unit_iri("MilliM")),
        ("SQUARE_METRE", None) => Some(unit_iri("M2")),
        ("SQUARE_METRE", Some("MILLI")) => Some(unit_iri("MilliM2")),
        ("CUBIC_METRE", None) => Some(unit_iri("M3")),
        ("CUBIC_METRE", Some("MILLI")) => Some(unit_iri("MilliM3")),
        ("RADIAN", None) => Some(unit_iri("RAD")),
        ("SECOND", None) => Some(unit_iri("SEC")),
        _ => None,
    }
}

fn map_conversion_unit(name: &str) -> Option<String> {
    match name {
        "DEGREE" => Some(unit_iri("DEG")),
        _ => None,
    }
}

fn resolve_property_unit(
    property: &PropertySingleValue,
    unit_by_type: &HashMap<String, String>,
    model: &IfcModel,
) -> Option<String> {
    if let Some(unit_id) = property.unit {
        return model.units.get(&unit_id).and_then(|unit| match unit {
            Unit::Si {
                name: Some(name),
                prefix,
                ..
            } => map_si_unit(name, prefix.as_deref()),
            Unit::ConversionBased {
                name: Some(name), ..
            } => map_conversion_unit(name),
            _ => None,
        });
    }
    match property.nominal_value.as_ref()? {
        StepValue::Typed { type_name, .. } => infer_unit_assignment_type(type_name)
            .and_then(|unit_type| unit_by_type.get(unit_type).cloned()),
        _ => None,
    }
}

fn resolve_quantity_unit(
    entity_name: &str,
    unit_by_type: &HashMap<String, String>,
) -> Option<String> {
    match entity_name {
        "IFCQUANTITYLENGTH" => unit_by_type.get("LENGTHUNIT").cloned(),
        "IFCQUANTITYAREA" => unit_by_type.get("AREAUNIT").cloned(),
        "IFCQUANTITYVOLUME" => unit_by_type.get("VOLUMEUNIT").cloned(),
        _ => None,
    }
}

fn infer_unit_assignment_type(type_name: &str) -> Option<&'static str> {
    let trimmed = type_name.trim();
    if trimmed.ends_with("LENGTHMEASURE") {
        Some("LENGTHUNIT")
    } else if trimmed.ends_with("AREAMEASURE") {
        Some("AREAUNIT")
    } else if trimmed.ends_with("VOLUMEMEASURE") {
        Some("VOLUMEUNIT")
    } else if trimmed.ends_with("PLANEANGLEMEASURE") {
        Some("PLANEANGLEUNIT")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ifc_model::build_model;
    use ifc_step::{parse_step_bytes, parse_step_file};
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn duplex_step_and_model() -> Option<(StepFile, IfcModel)> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Duplex.ifc");
        if !path.exists() {
            return None;
        }
        let step = parse_step_file(&path).ok()?;
        let model = build_model(&step).ok()?;
        Some((step, model))
    }

    #[test]
    fn test_convert_model_emits_bot_hierarchy() {
        let Some((step, model)) = duplex_step_and_model() else {
            return;
        };
        let result = convert_step_and_model(
            &step,
            &model,
            &ConvertOptions {
                base_uri: "https://example.test/base/".to_string(),
                emit_ifcowl_links: true,
                enable_topology: false,
                enable_topology_extension: false,
                topology_only: false,
                suppress_non_topology_fallback: false,
                geometry_relations: None,
                geometry_bounding_boxes: None,
                geometry_wkts: None,
                geometry_tolerance: 1e-6,
            },
        );

        assert!(result
            .triples
            .iter()
            .any(|triple| triple.predicate == bot_has_site()));
        assert!(result
            .triples
            .iter()
            .any(|triple| triple.predicate == bot_has_building()));
        assert!(result
            .triples
            .iter()
            .any(|triple| triple.predicate == bot_contains_element()));
        assert!(result
            .triples
            .iter()
            .any(|triple| triple.predicate == owl_same_as()));
        assert!(result.triples.iter().any(|triple| {
            matches!(&triple.object, Object::Iri(iri) if iri == "https://pi.pauwel.be/voc/buildingelement#Wall")
        }));
    }

    #[test]
    fn test_convert_model_uses_compressed_guid_iris() {
        let Some((_, model)) = duplex_step_and_model() else {
            return;
        };
        let project = model
            .spatial_nodes
            .values()
            .find(|node| node.spatial_type == SpatialType::Project)
            .unwrap();
        let subject = spatial_resource_iri(
            "https://example.test/base",
            project.spatial_type,
            &project.guid,
        );
        assert!(subject.contains(project.guid.as_str()));
        assert!(!subject.contains('-'));
    }

    #[test]
    fn test_canonical_guid_token_compresses_expanded_uuid() {
        let expanded = "7b7032cc-b822-417b-9aea-642906a29bd5";
        assert_eq!(canonical_guid_token(expanded), "1xS3BCk291UvhgP2a6eflL");
    }

    #[test]
    fn test_convert_model_emits_properties_and_units() {
        let Some((step, model)) = duplex_step_and_model() else {
            return;
        };
        let result = convert_step_and_model(&step, &model, &ConvertOptions::default());

        let area_property = result.triples.iter().find_map(|triple| {
            (triple.predicate == "http://lbd.arch.rwth-aachen.de/props#area")
                .then_some(triple.object.clone())
        });
        let Object::Iri(area_property) = area_property.expect("area property should be emitted")
        else {
            panic!("area property should point to an OPM property resource");
        };

        assert!(result.triples.iter().any(|triple| {
            triple.subject == area_property
                && triple.predicate == rdf_type()
                && matches!(&triple.object, Object::Iri(iri) if iri == &opm_property())
        }));
        assert!(result.triples.iter().any(|triple| {
            triple.subject == area_property
                && triple.predicate == opm_has_property_state()
                && matches!(&triple.object, Object::Iri(iri) if iri.contains("/state_area_") || iri.contains("/state_area"))
        }));
        assert!(result.triples.iter().any(|triple| {
            triple.subject == area_property
                && triple.predicate == opm_current_property_state_predicate()
                && matches!(&triple.object, Object::Iri(iri) if iri.contains("/state_area_") || iri.contains("/state_area"))
        }));
        assert!(result.triples.iter().any(|triple| {
            triple.predicate == schema_value()
                && matches!(&triple.object, Object::TypedLiteral { value, .. } if !value.is_empty())
        }));
        assert!(result.triples.iter().any(|triple| {
            triple.predicate == smls_unit()
                && matches!(&triple.object, Object::Iri(iri) if iri == "http://qudt.org/vocab/unit/M2")
        }));
        assert!(result.triples.iter().any(|triple| {
            triple.predicate == lbd_has_bounding_box()
                && matches!(&triple.object, Object::Iri(iri) if iri.contains("/geometry_"))
        }));
    }

    #[test]
    fn test_infer_unit_assignment_type_handles_measure_subtypes() {
        assert_eq!(
            infer_unit_assignment_type("IFCPOSITIVELENGTHMEASURE"),
            Some("LENGTHUNIT")
        );
        assert_eq!(
            infer_unit_assignment_type("IFCNONNEGATIVELENGTHMEASURE"),
            Some("LENGTHUNIT")
        );
        assert_eq!(
            infer_unit_assignment_type("IFCAREAMEASURE"),
            Some("AREAUNIT")
        );
        assert_eq!(
            infer_unit_assignment_type("IFCPLANEANGLEMEASURE"),
            Some("PLANEANGLEUNIT")
        );
        assert_eq!(infer_unit_assignment_type("IFCBOOLEAN"), None);
    }

    #[test]
    fn test_convert_model_emits_ifcowl_triples() {
        let Some((step, model)) = duplex_step_and_model() else {
            return;
        };
        let result = convert_step_and_model(&step, &model, &ConvertOptions::default());
        let namespace = ifcowl_namespace(step.header.schema);

        assert!(!result.ifcowl_triples.is_empty());
        assert!(result.ifcowl_triples.iter().any(|triple| {
            triple.predicate == rdf_type()
                && matches!(&triple.object, Object::Iri(iri) if iri.as_str() == format!("{namespace}IfcProject"))
        }));
        assert!(result
            .ifcowl_triples
            .iter()
            .any(|triple| { triple.predicate == format!("{namespace}globalId_IfcRoot") }));
        assert!(result.ifcowl_triples.iter().any(|triple| {
            triple.subject == "https://lbd.example.com/"
                && triple.predicate == rdf_type()
                && matches!(&triple.object, Object::Iri(iri) if iri.as_str() == owl_ontology())
        }));
        assert!(result.triples.iter().any(|triple| {
            triple.predicate == owl_same_as()
                && matches!(&triple.object, Object::Iri(iri) if iri.contains("/Ifc"))
        }));
    }

    #[test]
    fn test_convert_ifcowl_uses_schema_lookup_for_canonical_names() {
        let step = parse_step_bytes(
            b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n#1=IFCCARTESIANPOINT((0.,0.,0.));\nENDSEC;\n",
        )
        .unwrap();
        let namespace = ifcowl_namespace(step.header.schema);
        let triples =
            modules::ifcowl::convert_ifcowl(&step, "https://example.test/base", step.header.schema);

        assert!(triples.iter().any(|triple| {
            triple.subject == "https://example.test/base/IfcCartesianPoint_1"
                && triple.predicate == rdf_type()
                && matches!(&triple.object, Object::Iri(iri) if iri.as_str() == format!("{namespace}IfcCartesianPoint"))
        }));
        assert!(triples.iter().any(|triple| {
            triple.subject == "https://example.test/base/IfcCartesianPoint_1"
                && triple.predicate == format!("{namespace}coordinates_IfcCartesianPoint")
        }));
        assert!(triples.iter().any(|triple| {
            triple.subject == "https://example.test/base/"
                && triple.predicate == owl_imports()
                && matches!(&triple.object, Object::Iri(iri) if iri.as_str() == namespace)
        }));
        assert!(triples.iter().any(|triple| {
            triple.predicate == express_has_double()
                && matches!(&triple.object, Object::TypedLiteral { value, .. } if value == "0")
        }));
        assert!(triples
            .iter()
            .any(|triple| { triple.predicate == list_has_contents() }));
        let value_subjects: HashSet<_> = triples
            .iter()
            .filter(|triple| {
                triple.predicate == rdf_type()
                    && matches!(
                        &triple.object,
                        Object::Iri(iri) if iri.as_str() == format!("{namespace}IfcLengthMeasure")
                    )
            })
            .map(|triple| triple.subject.as_str())
            .collect();
        assert!(!value_subjects.is_empty());
    }

    #[test]
    fn test_convert_ifcowl_materializes_string_value_resources() {
        let step = parse_step_bytes(
            b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n#1=IFCORGANIZATION($,'ACME',$,$,$);\nENDSEC;\n",
        )
        .unwrap();
        let namespace = ifcowl_namespace(step.header.schema);
        let triples =
            modules::ifcowl::convert_ifcowl(&step, "https://example.test/base", step.header.schema);

        assert!(triples.iter().any(|triple| {
            triple.subject == "https://example.test/base/IfcOrganization_1"
                && triple.predicate == format!("{namespace}name_IfcOrganization")
                && matches!(&triple.object, Object::Iri(iri) if iri.contains("/IfcLabel_"))
        }));
        assert!(triples.iter().any(|triple| {
            triple.predicate == rdf_type()
                && matches!(&triple.object, Object::Iri(iri) if iri.as_str() == format!("{namespace}IfcLabel"))
        }));
        assert!(triples.iter().any(|triple| {
            triple.predicate == express_has_string()
                && matches!(&triple.object, Object::Literal(value) if value == "ACME")
        }));
    }

    #[test]
    fn test_convert_ifcowl_flattens_reference_lists() {
        let step = parse_step_bytes(
            b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n#1=IFCPOLYLINE((#2,#3));\n#2=IFCCARTESIANPOINT((0.,0.));\n#3=IFCCARTESIANPOINT((1.,1.));\n#4=IFCARBITRARYPROFILEDEFWITHVOIDS(.AREA.,$,#1,(#1));\nENDSEC;\n",
        )
        .unwrap();
        let namespace = ifcowl_namespace(step.header.schema);
        let triples =
            modules::ifcowl::convert_ifcowl(&step, "https://example.test/base", step.header.schema);

        assert!(triples.iter().any(|triple| {
            triple.subject == "https://example.test/base/IfcArbitraryProfileDefWithVoids_4"
                && triple.predicate == format!("{namespace}innerCurves_IfcArbitraryProfileDefWithVoids")
                && matches!(&triple.object, Object::Iri(iri) if iri == "https://example.test/base/IfcPolyline_1")
        }));
        assert!(!triples.iter().any(|triple| {
            triple.subject.contains("IfcCurve_List") || triple.subject.contains("IfcPolyline_List")
        }));
    }

    #[test]
    fn test_convert_ifcowl_keeps_reference_lists_when_schema_range_is_list() {
        let step = parse_step_bytes(
            b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n#1=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#2);\n#2=IFCPOLYLINE((#3,#4));\n#3=IFCCARTESIANPOINT((0.,0.));\n#4=IFCCARTESIANPOINT((1.,1.));\n#5=IFCCOMPOSITECURVE((#1),.F.);\nENDSEC;\n",
        )
        .unwrap();
        let namespace = ifcowl_namespace(step.header.schema);
        let triples =
            modules::ifcowl::convert_ifcowl(&step, "https://example.test/base", step.header.schema);

        assert!(triples.iter().any(|triple| {
            triple.subject == "https://example.test/base/IfcCompositeCurve_5"
                && triple.predicate == format!("{namespace}segments_IfcCompositeCurve")
                && matches!(&triple.object, Object::Iri(iri) if iri.contains("/IfcCompositeCurveSegment_List_"))
        }));
        assert!(triples.iter().any(|triple| {
            triple.predicate == list_has_contents()
                && matches!(&triple.object, Object::Iri(iri) if iri == "https://example.test/base/IfcCompositeCurveSegment_1")
        }));
    }

    #[test]
    fn test_convert_ifcowl_emits_logical_values() {
        let step = parse_step_bytes(
            b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n#1=IFCCOMPOSITECURVE((#2),.F.);\n#2=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#3);\n#3=IFCPOLYLINE((#4,#5));\n#4=IFCCARTESIANPOINT((0.,0.));\n#5=IFCCARTESIANPOINT((1.,1.));\nENDSEC;\n",
        )
        .unwrap();
        let triples =
            modules::ifcowl::convert_ifcowl(&step, "https://example.test/base", step.header.schema);

        assert!(triples.iter().any(|triple| {
            triple.predicate == express_has_logical()
                && matches!(&triple.object, Object::Iri(iri) if iri.as_str() == express_logical_value(false))
        }));
    }

    #[test]
    fn test_convert_ifcowl_materializes_compound_plane_angle_as_integer_list() {
        let step = parse_step_bytes(
            b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n#1=IFCLOCALTIME(12,0,0,$,IFCCOMPOUNDPLANEANGLEMEASURE((41,52,27,840000)),$);\nENDSEC;\n",
        )
        .unwrap();
        let triples =
            modules::ifcowl::convert_ifcowl(&step, "https://example.test/base", step.header.schema);

        assert!(triples.iter().any(|triple| {
            triple.subject.contains("/IfcCompoundPlaneAngleMeasure_")
                && triple.predicate == rdf_type()
                && matches!(&triple.object, Object::Iri(iri) if iri.ends_with("#IfcCompoundPlaneAngleMeasure"))
        }));
        assert!(triples.iter().any(|triple| {
            triple.predicate == list_has_contents()
                && matches!(&triple.object, Object::Iri(iri) if iri.contains("/INTEGER_"))
        }));
    }

    #[test]
    fn test_property_local_name_decodes_and_normalizes_unicode() {
        assert_eq!(property_local_name("Abhängigkeiten"), "abhaengigkeiten");
        assert_eq!(property_local_name("Tür Breite"), "tuerBreite");
        assert_eq!(property_local_name("Maß"), "mass");
        assert_eq!(property_local_name("1"), "p_1");
    }

    #[test]
    fn test_canonicalize_decimal_strips_float_noise() {
        assert_eq!(canonicalize_decimal(6.75000000000001), "6.75");
        assert_eq!(canonicalize_decimal(5.34999999999999), "5.35");
        assert_eq!(canonicalize_decimal(-0.0000000001), "0");
    }

    #[test]
    fn test_stable_short_guid_token_is_deterministic() {
        let token_a = stable_short_guid_token("2O2Fr$t4X7Zf8NOew3FNtn");
        let token_b = stable_short_guid_token("2O2Fr$t4X7Zf8NOew3FNtn");
        assert_eq!(token_a, token_b);
        assert_eq!(token_a.len(), 16);
    }

    #[test]
    fn test_element_resource_iri_uses_java_style_proxy_prefix() {
        let element = ifc_model::ElementNode {
            id: 1,
            guid: "3$3qM0qBX2JfSuV0oX6e4A".into(),
            entity_name: "IFCBUILDINGELEMENTPROXY".into(),
            name: None,
            description: None,
            object_type: None,
            predefined_type: None,
            tag: None,
            overall_height: None,
            overall_width: None,
            number_of_risers: None,
            number_of_treads: None,
            riser_height: None,
            tread_length: None,
        };

        let iri = element_resource_iri("https://example.test/base", &element);
        assert!(iri.contains("/buildingelement_"));
        assert!(!iri.contains("/buildingelementproxy_"));
    }

    #[test]
    fn test_convert_ifcowl_uses_express_namespace_for_generic_real_lists() {
        let step = parse_step_bytes(
            b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n#1=IFCDIRECTION((1.,0.,0.));\nENDSEC;\n",
        )
        .unwrap();
        let triples =
            modules::ifcowl::convert_ifcowl(&step, "https://example.test/base", step.header.schema);

        assert!(triples.iter().any(|triple| {
            triple.subject.contains("/REAL_List_")
                && triple.predicate == rdf_type()
                && matches!(&triple.object, Object::Iri(iri) if iri.as_str() == format!("{EXPRESS}REAL_List"))
        }));
    }

    #[test]
    fn test_convert_ifcowl_skips_derived_placeholder_values() {
        let step = parse_step_bytes(
            b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n#1=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\nENDSEC;\n",
        )
        .unwrap();
        let namespace = ifcowl_namespace(step.header.schema);
        let triples =
            modules::ifcowl::convert_ifcowl(&step, "https://example.test/base", step.header.schema);

        assert!(!triples.iter().any(|triple| {
            triple.subject == "https://example.test/base/IfcSIUnit_1"
                && triple.predicate == format!("{namespace}dimensions_IfcNamedUnit")
        }));
        assert!(!triples
            .iter()
            .any(|triple| matches!(&triple.object, Object::Literal(value) if value == "*")));
    }

    #[test]
    fn test_convert_model_emits_topology_when_enabled() {
        let Some((step, model)) = duplex_step_and_model() else {
            return;
        };
        let result = convert_step_and_model(
            &step,
            &model,
            &ConvertOptions {
                base_uri: "https://example.test/base/".to_string(),
                emit_ifcowl_links: true,
                enable_topology: true,
                enable_topology_extension: false,
                topology_only: false,
                suppress_non_topology_fallback: false,
                geometry_relations: None,
                geometry_bounding_boxes: None,
                geometry_wkts: None,
                geometry_tolerance: 1e-6,
            },
        );

        assert!(result
            .triples
            .iter()
            .any(|triple| triple.predicate == bot_adjacent_element()));
        assert!(result
            .triples
            .iter()
            .any(|triple| triple.predicate == bot_has_sub_element()));
        assert!(result.triples.iter().any(|triple| {
            triple.predicate == bot_contains_element()
                && matches!(&triple.subject, subject if subject.contains("/storey_"))
        }));
    }

    #[test]
    fn test_skip_named_self_value_for_literal_properties() {
        assert!(should_skip_named_self_value(
            "AccessibilityPerformance",
            &Object::Literal("AccessibilityPerformance".to_string())
        ));
        assert!(!should_skip_named_self_value(
            "AccessibilityPerformance",
            &Object::Literal("Different".to_string())
        ));
    }

    #[test]
    fn test_convert_model_omits_direct_sub_elements_without_topology() {
        let Some((step, model)) = duplex_step_and_model() else {
            return;
        };
        let result = convert_step_and_model(
            &step,
            &model,
            &ConvertOptions {
                base_uri: "https://example.test/base/".to_string(),
                emit_ifcowl_links: true,
                enable_topology: false,
                enable_topology_extension: false,
                topology_only: false,
                suppress_non_topology_fallback: false,
                geometry_relations: None,
                geometry_bounding_boxes: None,
                geometry_wkts: None,
                geometry_tolerance: 1e-6,
            },
        );

        assert!(!result
            .triples
            .iter()
            .any(|triple| triple.predicate == bot_has_sub_element()));
    }

    #[test]
    fn test_convert_model_emits_property_set_membership_links() {
        let Some((step, model)) = duplex_step_and_model() else {
            return;
        };
        let result = convert_step_and_model(
            &step,
            &model,
            &ConvertOptions {
                base_uri: "https://example.test/base/".to_string(),
                emit_ifcowl_links: true,
                enable_topology: true,
                enable_topology_extension: false,
                topology_only: false,
                suppress_non_topology_fallback: false,
                geometry_relations: None,
                geometry_bounding_boxes: None,
                geometry_wkts: None,
                geometry_tolerance: 1e-6,
            },
        );
        assert!(result.triples.iter().any(|triple| {
            triple.predicate == rdf_member() && triple.subject.contains("/propertyset_")
        }));
    }

    #[test]
    fn test_property_resource_iri_is_scoped_by_set() {
        let a = property_resource_iri(
            "https://example.test/base",
            "width",
            "2O2Fr$t4X7Zf8NOew3FNld",
            "set-a",
        );
        let b = property_resource_iri(
            "https://example.test/base",
            "width",
            "2O2Fr$t4X7Zf8NOew3FNld",
            "set-b",
        );
        assert_ne!(a, b);
    }

    #[test]
    fn test_merge_geometry_relations_puts_bbox_derived_edges_in_extension_not_core() {
        let mut topology = TopologyGraph::default();
        topology.node_kinds.insert(1, TopologyNodeKind::Element);
        topology.node_kinds.insert(2, TopologyNodeKind::Element);
        merge_geometry_relations_into_topology(
            &mut topology,
            &[
                GeometryRelation {
                    source: 1,
                    target: 2,
                    kind: GeometryRelationKind::AdjacentElement,
                },
                GeometryRelation {
                    source: 1,
                    target: 2,
                    kind: GeometryRelationKind::IntersectingElement,
                },
                GeometryRelation {
                    source: 1,
                    target: 2,
                    kind: GeometryRelationKind::InterfaceOf,
                },
            ],
            false, // bbox-only
        );
        // All bbox-derived candidates must stay in extension_edges, not core_edges.
        assert!(topology.core_edges.is_empty());
        assert_eq!(topology.extension_edges.len(), 3);

        // Exact-kernel results must go to core_edges.
        let mut topology2 = TopologyGraph::default();
        topology2.node_kinds.insert(1, TopologyNodeKind::Element);
        topology2.node_kinds.insert(2, TopologyNodeKind::Element);
        merge_geometry_relations_into_topology(
            &mut topology2,
            &[GeometryRelation {
                source: 1000,
                target: 1,
                kind: GeometryRelationKind::InterfaceOf,
            }],
            true, // exact
        );
        assert_eq!(
            topology2.node_kinds.get(&1000).copied(),
            Some(TopologyNodeKind::Interface)
        );
        assert!(topology2
            .core_edges
            .iter()
            .any(|edge| edge.kind == TopologyEdgeKind::InterfaceOf));
        assert!(topology2.extension_edges.is_empty());
    }

    #[test]
    fn test_convert_model_emits_wkt_geometry_from_bounding_box() {
        let Some((step, model)) = duplex_step_and_model() else {
            return;
        };
        let options = ConvertOptions {
            base_uri: "https://example.test/base/".to_string(),
            emit_ifcowl_links: true,
            enable_topology: true,
            enable_topology_extension: false,
            topology_only: false,
            suppress_non_topology_fallback: false,
            geometry_relations: None,
            geometry_bounding_boxes: Some(Arc::new(HashMap::from([(
                4131_u64,
                BoundingBox {
                    x_min: 0.0,
                    x_max: 1.0,
                    y_min: 0.0,
                    y_max: 2.0,
                    z_min: 0.0,
                    z_max: 3.0,
                },
            )]))),
            geometry_wkts: None,
            geometry_tolerance: 1e-6,
        };
        let result = convert_step_and_model(&step, &model, &options);
        assert!(result.triples.iter().any(|triple| {
            triple.predicate == lbd_has_bounding_box()
                && matches!(&triple.object, Object::Iri(iri) if iri.contains("/geometry_"))
        }));
        assert!(result.triples.iter().any(|triple| {
            triple.predicate == rdf_type()
                && matches!(&triple.object, Object::Iri(iri) if iri == &geo_geometry())
        }));
        assert!(result.triples.iter().any(|triple| {
            triple.predicate == geo_as_wkt()
                && matches!(&triple.object, Object::TypedLiteral { value, datatype } if value.starts_with("POLYHEDRALSURFACE Z") && datatype == &geo_wkt_literal())
        }));
    }
}
