use crossbeam::channel::Sender;
use ifc_model::IfcModel;
use lbd_ontology::{omg_geometry, omg_has_geometry, rdf_type, Object, Triple};

use crate::{
    element_resource_iri, geometry_resource_iri, normalize_base_uri, sorted_values,
    spatial_resource_iri, ConvertOptions, StreamError, MAX_STREAM_BATCH_SIZE,
    MIN_STREAM_BATCH_SIZE, STREAM_BATCH_SIZE,
};

/// Emit OMG geometry-link triples for every element and spatial node.
///
/// For each entity:
///   `entity  omg:hasGeometry  geomNode`
///   `geomNode  rdf:type  omg:Geometry`
///
/// When `options.geometry_bounding_boxes` is populated (requires neo-bbox-enricher
/// to have run first), the geometry node also gets actual geometry content via the
/// existing `emit_bounding_box_geometries` path, which adds geo/fog literals. This
/// module only establishes the OMG structural links; literals are not duplicated here
/// because they are already emitted by the bbox enricher pass.
pub(crate) fn emit_omg_fog<E, F>(
    model: &IfcModel,
    _options: &ConvertOptions,
    base: &str,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    // Spatial nodes
    for node in sorted_values(&model.spatial_nodes) {
        let subject = spatial_resource_iri(base, node.spatial_type, &node.guid);
        let geom_node = geometry_resource_iri(base, &node.guid);
        emit(Triple {
            subject: subject.clone(),
            predicate: omg_has_geometry(),
            object: Object::Iri(geom_node.clone()),
        })?;
        emit(Triple {
            subject: geom_node,
            predicate: rdf_type(),
            object: Object::Iri(omg_geometry()),
        })?;
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
            subject: geom_node,
            predicate: rdf_type(),
            object: Object::Iri(omg_geometry()),
        })?;
    }

    Ok(())
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
