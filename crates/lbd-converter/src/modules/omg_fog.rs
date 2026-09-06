use crossbeam::channel::Sender;
use ifc_model::IfcModel;
use ifc_step::EntityId;
use lbd_ontology::{omg_geometry, omg_has_geometry, rdf_type, Object, Triple};

use crate::{
    bounding_box_wkt_literal, current_generated_at_rfc3339, element_resource_iri,
    emit_bounding_box_wkt, geometry_resource_iri, geometry_state_iri, normalize_base_uri,
    sorted_values, spatial_resource_iri, ConvertOptions, StreamError, MAX_STREAM_BATCH_SIZE,
    MIN_STREAM_BATCH_SIZE,
};

/// Emit OMG geometry-link triples for every element and spatial node.
///
/// For each entity:
///   `entity  omg:hasGeometry  geomNode`
///   `geomNode  rdf:type  omg:Geometry`
///
/// When `options.geometry_bounding_boxes` is populated from the tessellated model,
/// each object is also linked to a dedicated GeoSPARQL bounding-box geometry.
pub(crate) fn emit_omg_fog<E, F>(
    model: &IfcModel,
    options: &ConvertOptions,
    base: &str,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    let generated_at = current_generated_at_rfc3339();

    // Spatial nodes
    for node in sorted_values(&model.spatial_nodes) {
        let subject = spatial_resource_iri(base, &node.guid);
        let geom_node = geometry_resource_iri(base, &node.guid);
        emit(Triple {
            subject: subject.clone(),
            predicate: omg_has_geometry(),
            object: Object::Iri(geom_node.clone()),
        })?;
        emit(Triple {
            subject: geom_node.clone(),
            predicate: rdf_type(),
            object: Object::Iri(omg_geometry()),
        })?;
        emit_bounding_box(options, base, &subject, &node.guid, node.id, emit)?;
        emit_geometry_state(options, base, &geom_node, node.id, &generated_at, emit)?;
    }

    // Building elements
    for element in sorted_values(&model.elements) {
        let subject = element_resource_iri(base, element);
        let geom_node = geometry_resource_iri(base, &element.guid);
        emit(Triple {
            subject: subject.clone(),
            predicate: omg_has_geometry(),
            object: Object::Iri(geom_node.clone()),
        })?;
        emit(Triple {
            subject: geom_node.clone(),
            predicate: rdf_type(),
            object: Object::Iri(omg_geometry()),
        })?;
        emit_bounding_box(options, base, &subject, &element.guid, element.id, emit)?;
        emit_geometry_state(options, base, &geom_node, element.id, &generated_at, emit)?;
    }

    Ok(())
}

fn emit_bounding_box<E, F>(
    options: &ConvertOptions,
    base: &str,
    feature_subject: &str,
    guid: &str,
    entity_id: EntityId,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    if !options.omg_emit_bounding_boxes {
        return Ok(());
    }
    let Some(bbox) = options
        .geometry_bounding_boxes
        .as_ref()
        .and_then(|boxes| boxes.get(&entity_id))
    else {
        return Ok(());
    };
    let Some(wkt) = bounding_box_wkt_literal(base, bbox) else {
        return Ok(());
    };
    emit_bounding_box_wkt(feature_subject, guid, base, wkt, emit)
}

/// Hang an OPM state carrying this element's geometry content hash off its geometry
/// node, when the geometry producer supplied one.
///
/// This is what makes geometry change detectable by query. Previously the `omg` graph
/// was identical whether a wall had moved ten metres or not at all, because it carried
/// only a link and a type — which is also why the module was disabled in the worker as
/// an "actively misleading signal". The state is the consumer that fixes that.
///
/// Absent from the map means the element has no geometry, so there is nothing to state.
/// The hash is tagged with its algorithm so a future change to it is distinguishable in
/// the data rather than silently comparable against old values.
fn emit_geometry_state<E, F>(
    options: &ConvertOptions,
    base: &str,
    geom_node: &str,
    entity_id: EntityId,
    generated_at: &str,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    let Some(hashes) = options.geometry_hashes.as_ref() else {
        return Ok(());
    };
    let Some(hash) = hashes.get(&entity_id) else {
        return Ok(());
    };

    let value = format!("fnv1a64:{hash:016x}");
    let state_subject = geometry_state_iri(base, geom_node, &value);

    crate::emit_opm_value(
        geom_node,
        &state_subject,
        Object::Literal(value),
        None,
        generated_at,
        options.omg_opm_level,
        emit,
    )
}

/// Stream OMG geometry-link triples in bounded batches.
///
/// This is the `neo-omg-fog` named-graph producer.
pub fn stream_omg_fog(
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
    emit_omg_fog(model, options, &base, &mut |triple| {
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
