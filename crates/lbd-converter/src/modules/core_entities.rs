use ifc_model::IfcModel;
use lbd_ontology::{owl_same_as, Object, Triple};

use crate::{ifcowl_element_iri, ifcowl_spatial_iri, sorted_values, ConvertOptions};

/// Emit BOT entities + BEO product types + owl:sameAs links.
///
/// Backward-compat combined entry point used by the monolithic `emit_lbd`.
/// Calls `bot::emit_bot` then `beo::emit_beo`, then optionally adds
/// `owl:sameAs` links to IfcOWL IRIs when `options.emit_ifcowl_links` is true.
/// New plugins should call each sub-module's streaming function directly.
pub(crate) fn emit_core_entities<E, F>(
    model: &IfcModel,
    options: &ConvertOptions,
    base: &str,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    super::bot::emit_bot(model, options, base, emit)?;
    super::beo::emit_beo(model, options, base, emit)?;

    if options.emit_ifcowl_links {
        for node in sorted_values(&model.spatial_nodes) {
            let subject = crate::spatial_resource_iri(base, node.spatial_type, &node.guid);
            emit(Triple {
                subject,
                predicate: owl_same_as(),
                object: Object::Iri(ifcowl_spatial_iri(base, node)),
            })?;
        }
        for element in sorted_values(&model.elements) {
            let subject = crate::element_resource_iri(base, element);
            emit(Triple {
                subject,
                predicate: owl_same_as(),
                object: Object::Iri(ifcowl_element_iri(base, element)),
            })?;
        }
    }

    Ok(())
}
