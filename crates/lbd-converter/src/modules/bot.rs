use std::collections::{BTreeMap, HashSet};

use crossbeam::channel::Sender;
use ifc_model::IfcModel;
use ifc_step::EntityId;
use ifc_schema::SpatialType;
use lbd_ontology::{
    bot_adjacent_element, bot_adjacent_zone, bot_contains_element, bot_contains_zone, bot_element,
    bot_has_building, bot_has_space, bot_has_storey, bot_has_sub_element, bot_interface,
    bot_interface_of, bot_intersecting_element, owl_same_as, rdf_type, Object, Triple,
};
use lbd_topology::{build_topology, TopologyEdgeKind, TopologyGraph};

use crate::{
    element_resource_iri, ifcowl_element_iri, ifcowl_spatial_iri, normalize_base_uri,
    object_subject, object_subject_and_guid, sorted_values, spatial_class, spatial_resource_iri,
    topology_interface_iri_for_guids, ConvertOptions, StreamError, MAX_STREAM_BATCH_SIZE,
    MIN_STREAM_BATCH_SIZE,
};

/// Emit BOT spatial-node types, spatial-hierarchy predicates and `bot:Element` typing.
///
/// This is the pure-topology part of the BOT ontology: site/building/storey/space
/// hierarchy and element containment typing. BEO product-class types and OPM
/// property sets are emitted by their own modules.
pub(crate) fn emit_bot<E, F>(
    model: &IfcModel,
    options: &ConvertOptions,
    base: &str,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    emit_bot_inner(model, options, base, None, emit)
}

pub(crate) fn emit_bot_with_topology<E, F>(
    model: &IfcModel,
    options: &ConvertOptions,
    base: &str,
    topology: &TopologyGraph,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    emit_bot_inner(model, options, base, Some(topology), emit)
}

fn emit_bot_inner<E, F>(
    model: &IfcModel,
    options: &ConvertOptions,
    base: &str,
    topology: Option<&TopologyGraph>,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    // Spatial node rdf:type (bot:Site, bot:Building, bot:Storey, bot:Space, bot:Zone)
    for node in sorted_values(&model.spatial_nodes) {
        let subject = spatial_resource_iri(base, &node.guid);
        emit(Triple {
            subject,
            predicate: rdf_type(),
            object: Object::Iri(spatial_class(node.spatial_type)),
        })?;
    }

    // Element rdf:type bot:Element
    for element in sorted_values(&model.elements) {
        let subject = element_resource_iri(base, element);
        emit(Triple {
            subject,
            predicate: rdf_type(),
            object: Object::Iri(bot_element()),
        })?;
    }

    // Spatial hierarchy predicates (bot:containsZone, bot:hasBuilding, etc.)
    let mut parent_ids: Vec<_> = model.children_of.keys().copied().collect();
    parent_ids.sort_unstable();
    for parent_id in parent_ids {
        let child_ids = &model.children_of[&parent_id];
        let Some(parent) = model.spatial_nodes.get(&parent_id) else {
            continue;
        };
        let parent_subject = spatial_resource_iri(base, &parent.guid);
        let mut sorted_child_ids = child_ids.clone();
        sorted_child_ids.sort_unstable();
        for child_id in sorted_child_ids {
            let Some(child) = model.spatial_nodes.get(&child_id) else {
                continue;
            };
            let predicate = match (parent.spatial_type, child.spatial_type) {
                // IfcProject is not a bot:Zone, so Project→Site has no BOT
                // containment predicate.
                (SpatialType::Project, SpatialType::Site) => None,
                (SpatialType::Site, SpatialType::Building) => Some(bot_has_building()),
                (SpatialType::Building, SpatialType::Storey) => Some(bot_has_storey()),
                (SpatialType::Storey, SpatialType::Space) => Some(bot_has_space()),
                _ => None,
            };
            if let Some(predicate) = predicate {
                emit(Triple {
                    subject: parent_subject.clone(),
                    predicate,
                    object: Object::Iri(spatial_resource_iri(base, &child.guid)),
                })?;
            }
        }
    }

    if let Some(topology) = topology {
        emit_topology_edges(model, base, topology, emit)?;
    }

    // bot:containsElement — emit for all spatial structures via contained_in map
    let mut pairs: Vec<_> = model
        .contained_in
        .iter()
        .filter_map(|(&element_id, &structure_id)| {
            if model.elements.contains_key(&element_id)
                && model.spatial_nodes.contains_key(&structure_id)
            {
                Some((structure_id, element_id))
            } else {
                None
            }
        })
        .collect();
    if let Some(topology) = topology {
        pairs.extend(topology.core_pairs_of_kind(TopologyEdgeKind::ContainsElement));
    }
    pairs.sort_unstable();
    pairs.dedup();
    for (structure_id, element_id) in pairs {
        let Some(structure) = model.spatial_nodes.get(&structure_id) else {
            continue;
        };
        let structure_subject = spatial_resource_iri(base, &structure.guid);
        let Some(contained_element) = model.elements.get(&element_id) else {
            continue;
        };
        emit(Triple {
            subject: structure_subject,
            predicate: bot_contains_element(),
            object: Object::Iri(element_resource_iri(base, contained_element)),
        })?;
    }

    if options.emit_ifcowl_links {
        for node in sorted_values(&model.spatial_nodes) {
            let subject = spatial_resource_iri(base, &node.guid);
            emit(Triple {
                subject,
                predicate: owl_same_as(),
                object: Object::Iri(ifcowl_spatial_iri(base, node)),
            })?;
        }
        for element in sorted_values(&model.elements) {
            let subject = element_resource_iri(base, element);
            emit(Triple {
                subject,
                predicate: owl_same_as(),
                object: Object::Iri(ifcowl_element_iri(base, element)),
            })?;
        }
    }

    Ok(())
}

fn emit_topology_edges<E, F>(
    model: &IfcModel,
    base: &str,
    topology: &TopologyGraph,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    let mappings = [
        (TopologyEdgeKind::ContainsZone, bot_contains_zone()),
        (TopologyEdgeKind::AdjacentElement, bot_adjacent_element()),
        (TopologyEdgeKind::AdjacentZone, bot_adjacent_zone()),
        (TopologyEdgeKind::HasSubElement, bot_has_sub_element()),
        (
            TopologyEdgeKind::IntersectingElement,
            bot_intersecting_element(),
        ),
    ];
    for (kind, predicate) in mappings {
        for (source, target) in topology.core_pairs_of_kind(kind) {
            let Some(subject) = object_subject(model, base, source) else {
                continue;
            };
            let Some(object) = object_subject(model, base, target) else {
                continue;
            };
            emit(Triple {
                subject,
                predicate: predicate.clone(),
                object: Object::Iri(object),
            })?;
        }
    }

    // Interfaces are derived nodes whose plugin-side id is a hash of STEP entity
    // numbers, which churn on every export. Collect both endpoints per interface
    // first so the emitted IRI can be keyed on their GUIDs instead — see
    // `topology_interface_iri_for_guids`.
    //
    // BTreeMap rather than HashMap so emission order is deterministic for a given
    // model, which keeps byte-comparison of two conversion runs meaningful.
    let mut interface_targets: BTreeMap<EntityId, Vec<(String, String)>> = BTreeMap::new();
    for (interface_id, target_id) in topology.core_pairs_of_kind(TopologyEdgeKind::InterfaceOf) {
        let Some((target_iri, target_guid)) = object_subject_and_guid(model, base, target_id) else {
            continue;
        };
        interface_targets
            .entry(interface_id)
            .or_default()
            .push((target_iri, target_guid));
    }

    let mut typed_interfaces = HashSet::new();
    for targets in interface_targets.values() {
        // An interface joins exactly two elements. Any other arity means one
        // endpoint failed to resolve above, or the topology pass produced
        // something we do not understand — skip rather than mint an IRI that
        // cannot be made stable.
        let [first, second] = targets.as_slice() else {
            continue;
        };
        let subject = topology_interface_iri_for_guids(base, &first.1, &second.1);
        if typed_interfaces.insert(subject.clone()) {
            emit(Triple {
                subject: subject.clone(),
                predicate: rdf_type(),
                object: Object::Iri(bot_interface()),
            })?;
        }
        for target in [first, second] {
            emit(Triple {
                subject: subject.clone(),
                predicate: bot_interface_of(),
                object: Object::Iri(target.0.clone()),
            })?;
        }
    }
    Ok(())
}

/// Stream semantic IFC-backed BOT triples in bounded batches.
pub fn stream_bot(
    model: &IfcModel,
    options: &ConvertOptions,
    sender: &Sender<Vec<Triple>>,
) -> Result<u64, StreamError> {
    let topology = build_topology(model);
    stream_bot_with_topology(model, options, &topology, sender)
}

pub fn stream_bot_with_topology(
    model: &IfcModel,
    options: &ConvertOptions,
    topology: &TopologyGraph,
    sender: &Sender<Vec<Triple>>,
) -> Result<u64, StreamError> {
    let base = normalize_base_uri(&options.base_uri);
    let batch_size = options
        .stream_batch_size
        .clamp(MIN_STREAM_BATCH_SIZE, MAX_STREAM_BATCH_SIZE);
    let mut batch = Vec::with_capacity(batch_size);
    let mut triple_count: u64 = 0;
    emit_bot_with_topology(model, options, &base, topology, &mut |triple| {
        triple_count += 1;
        batch.push(triple);
        if batch.len() >= batch_size {
            sender
                .send(std::mem::take(&mut batch))
                .map_err(|_| StreamError::ChannelClosed)?;
        }
        Ok::<(), StreamError>(())
    })?;
    if !batch.is_empty() {
        sender.send(batch).map_err(|_| StreamError::ChannelClosed)?;
    }
    Ok(triple_count)
}
