use crossbeam::channel::Sender;
use ifc_model::IfcModel;
use ifc_schema::product_type_name;
use lbd_ontology::{rdf_type, Object, Triple};
use tracing::debug;

use crate::{
    element_resource_iri, lbd_predefined_type_class_iri, lbd_product_class_iri, normalize_base_uri,
    sorted_values, ConvertOptions, StreamError, MIN_STREAM_BATCH_SIZE, MAX_STREAM_BATCH_SIZE,
};

/// Emit BEO product-class `rdf:type` triples for IFC elements.
///
/// Each element whose `entity_name` maps to a product type BEO declares receives
/// one `rdf:type <beo:Class>` triple, plus a predefined-type subclass triple when
/// BEO declares that variant too. This is deliberately separate from the
/// `bot:Element` type emitted by the BOT module so that the two can be activated
/// independently.
///
/// Both the base class and the predefined-type variant are checked against BEO's
/// declared classes before being emitted. The product type and the suffix are
/// both *guesses* derived from IFC entity names and enum values — nothing in the
/// IFC data says the resulting IRI exists — and an undeclared type is silently
/// harmful: the triples load and the counts look right, but nothing can resolve,
/// subsume, or target the type. Suppressing it leaves the element with
/// `bot:Element` and its ifcOWL / bSDD typing, which is honest rather than
/// merely quiet.
pub(crate) fn emit_beo<E, F>(
    model: &IfcModel,
    _options: &ConvertOptions,
    base: &str,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    // Counted so a BEO version bump that drops or renames a class is diagnosable
    // rather than invisible — suppression is silent by design at the triple level.
    let mut suppressed_base: u64 = 0;
    let mut suppressed_variants: u64 = 0;

    for element in sorted_values(&model.elements) {
        let Some(product_type) = product_type_name(element.entity_name.as_str()) else {
            continue;
        };
        let Some(product_class) = lbd_product_class_iri(product_type) else {
            suppressed_base += 1;
            continue;
        };
        let subject = element_resource_iri(base, element);
        if let Some(predefined_type) = element.predefined_type.as_ref() {
            match lbd_predefined_type_class_iri(product_type, predefined_type) {
                Some(variant_class) => emit(Triple {
                    subject: subject.clone(),
                    predicate: rdf_type(),
                    object: Object::Iri(variant_class),
                })?,
                None => suppressed_variants += 1,
            }
        }
        emit(Triple {
            subject,
            predicate: rdf_type(),
            object: Object::Iri(product_class),
        })?;
    }

    if suppressed_base > 0 || suppressed_variants > 0 {
        debug!(
            suppressed_base_classes = suppressed_base,
            suppressed_predefined_type_variants = suppressed_variants,
            "BEO does not declare these classes; elements keep bot:Element and their ifcOWL / bSDD typing"
        );
    }

    Ok(())
}

/// Stream BEO product-class type triples in bounded batches.
pub fn stream_beo(
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
    emit_beo(model, options, &base, &mut |triple| {
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
