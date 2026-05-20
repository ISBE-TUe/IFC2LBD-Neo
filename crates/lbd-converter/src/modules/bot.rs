use crossbeam::channel::Sender;
use ifc_model::IfcModel;
use ifc_schema::SpatialType;
use lbd_ontology::{bot_element, bot_has_building, bot_has_site, bot_has_space, bot_has_storey, rdf_type, Object, Triple};

use crate::{
    element_resource_iri, normalize_base_uri, sorted_values, spatial_class, spatial_resource_iri,
    ConvertOptions, StreamError, MIN_STREAM_BATCH_SIZE, MAX_STREAM_BATCH_SIZE,
};

/// Emit BOT spatial-node types, spatial-hierarchy predicates and `bot:Element` typing.
///
/// This is the pure-topology part of the BOT ontology: site/building/storey/space
/// hierarchy and element containment typing. BEO product-class types and OPM
/// property sets are emitted by their own modules.
pub(crate) fn emit_bot<E, F>(
    model: &IfcModel,
    _options: &ConvertOptions,
    base: &str,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    // Spatial node rdf:type (bot:Site, bot:Building, bot:Storey, bot:Space, bot:Zone)
    for node in sorted_values(&model.spatial_nodes) {
        let subject = spatial_resource_iri(base, node.spatial_type, &node.guid);
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

    // Spatial hierarchy predicates (bot:hasSite, bot:hasBuilding, etc.)
    let mut parent_ids: Vec<_> = model.children_of.keys().copied().collect();
    parent_ids.sort_unstable();
    for parent_id in parent_ids {
        let child_ids = &model.children_of[&parent_id];
        let Some(parent) = model.spatial_nodes.get(&parent_id) else {
            continue;
        };
        let parent_subject = spatial_resource_iri(base, parent.spatial_type, &parent.guid);
        let mut sorted_child_ids = child_ids.clone();
        sorted_child_ids.sort_unstable();
        for child_id in sorted_child_ids {
            let Some(child) = model.spatial_nodes.get(&child_id) else {
                continue;
            };
            let predicate = match (parent.spatial_type, child.spatial_type) {
                (SpatialType::Project, SpatialType::Site) => Some(bot_has_site()),
                (SpatialType::Site, SpatialType::Building) => Some(bot_has_building()),
                (SpatialType::Building, SpatialType::Storey) => Some(bot_has_storey()),
                (SpatialType::Storey, SpatialType::Space) => Some(bot_has_space()),
                _ => None,
            };
            if let Some(predicate) = predicate {
                emit(Triple {
                    subject: parent_subject.clone(),
                    predicate,
                    object: Object::Iri(spatial_resource_iri(base, child.spatial_type, &child.guid)),
                })?;
            }
        }
    }

    Ok(())
}

/// Stream BOT triples (spatial types + hierarchy + `bot:Element`) in bounded batches.
pub fn stream_bot(
    model: &IfcModel,
    options: &ConvertOptions,
    sender: &Sender<Vec<Triple>>,
) -> Result<u64, StreamError> {
    let base = normalize_base_uri(&options.base_uri);
    let batch_size = options
        .stream_batch_size
        .clamp(MIN_STREAM_BATCH_SIZE, MAX_STREAM_BATCH_SIZE);
    let mut batch = Vec::with_capacity(batch_size);
    let mut triple_count: u64 = 0;
    emit_bot(model, options, &base, &mut |triple| {
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
