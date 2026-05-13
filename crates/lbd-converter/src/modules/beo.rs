use crossbeam::channel::Sender;
use ifc_model::IfcModel;
use ifc_schema::product_type_name;
use lbd_ontology::{rdf_type, Object, Triple};

use crate::{
    element_resource_iri, lbd_product_class_iri, normalize_base_uri, sorted_values,
    ConvertOptions, StreamError, STREAM_BATCH_SIZE, MIN_STREAM_BATCH_SIZE, MAX_STREAM_BATCH_SIZE,
};

/// Emit BEO / FURN product-class `rdf:type` triples for IFC elements.
///
/// Each element whose `entity_name` maps to a known product type receives one
/// `rdf:type <beo:Class>` triple (and a predefined-type subclass triple when
/// present). This is deliberately separate from the `bot:Element` type
/// emitted by the BOT module so that the two can be activated independently.
pub(crate) fn emit_beo<E, F>(
    model: &IfcModel,
    _options: &ConvertOptions,
    base: &str,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    for element in sorted_values(&model.elements) {
        let Some(product_type) = product_type_name(element.entity_name.as_str()) else {
            continue;
        };
        let subject = element_resource_iri(base, element);
        let product_class = lbd_product_class_iri(element.entity_name.as_str(), product_type);
        if let Some(predefined_type) = element.predefined_type.as_ref() {
            emit(Triple {
                subject: subject.clone(),
                predicate: rdf_type(),
                object: Object::Iri(format!("{product_class}-{predefined_type}")),
            })?;
        }
        emit(Triple {
            subject,
            predicate: rdf_type(),
            object: Object::Iri(product_class),
        })?;
    }
    Ok(())
}

/// Stream BEO / FURN product-class type triples in bounded batches.
pub fn stream_beo(
    model: &IfcModel,
    options: &ConvertOptions,
    sender: &Sender<Vec<Triple>>,
) -> Result<(), StreamError> {
    let base = normalize_base_uri(&options.base_uri);
    let batch_size = options
        .stream_batch_size
        .clamp(MIN_STREAM_BATCH_SIZE, MAX_STREAM_BATCH_SIZE);
    let mut batch = Vec::with_capacity(batch_size);
    emit_beo(model, options, &base, &mut |triple| {
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
    Ok(())
}
