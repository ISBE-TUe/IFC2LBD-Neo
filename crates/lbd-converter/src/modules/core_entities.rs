use ifc_model::IfcModel;
use ifc_schema::{product_type_name, SpatialType};
use lbd_ontology::{
    bot_element, bot_has_building, bot_has_site, bot_has_space, bot_has_storey, owl_same_as,
    rdf_type, Object, Triple,
};

use crate::{
    element_resource_iri, ifcowl_element_iri, ifcowl_spatial_iri, lbd_product_class_iri,
    sorted_values, spatial_class, spatial_resource_iri, ConvertOptions,
};

pub(crate) fn emit_core_entities<E, F>(
    model: &IfcModel,
    options: &ConvertOptions,
    base: &str,
    emit: &mut F,
) -> Result<(), E>
where
    F: FnMut(Triple) -> Result<(), E>,
{
    for node in sorted_values(&model.spatial_nodes) {
        let subject = spatial_resource_iri(base, node.spatial_type, &node.guid);
        emit(Triple {
            subject: subject.clone(),
            predicate: rdf_type(),
            object: Object::Iri(spatial_class(node.spatial_type)),
        })?;
        if options.emit_ifcowl_links {
            emit(Triple {
                subject,
                predicate: owl_same_as(),
                object: Object::Iri(ifcowl_spatial_iri(base, node)),
            })?;
        }
    }

    for element in sorted_values(&model.elements) {
        let subject = element_resource_iri(base, element);
        emit(Triple {
            subject: subject.clone(),
            predicate: rdf_type(),
            object: Object::Iri(bot_element()),
        })?;
        if let Some(product_type) = product_type_name(element.entity_name.as_str()) {
            let product_class = lbd_product_class_iri(element.entity_name.as_str(), product_type);
            if let Some(predefined_type) = element.predefined_type.as_ref() {
                emit(Triple {
                    subject: subject.clone(),
                    predicate: rdf_type(),
                    object: Object::Iri(format!("{product_class}-{predefined_type}")),
                })?;
            }
            emit(Triple {
                subject: subject.clone(),
                predicate: rdf_type(),
                object: Object::Iri(product_class),
            })?;
        }
        if options.emit_ifcowl_links {
            emit(Triple {
                subject,
                predicate: owl_same_as(),
                object: Object::Iri(ifcowl_element_iri(base, element)),
            })?;
        }
    }

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
                    object: Object::Iri(spatial_resource_iri(
                        base,
                        child.spatial_type,
                        &child.guid,
                    )),
                })?;
            }
        }
    }

    Ok(())
}
