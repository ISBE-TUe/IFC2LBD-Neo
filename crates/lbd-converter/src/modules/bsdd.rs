use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

use crossbeam::channel::Sender;
use flate2::read::GzDecoder;
use ifc_model::IfcModel;
use ifc_schema::SpatialType;
use ifc_step::StepValue;
use lbd_ontology::{
    opm_current_property_state, opm_current_property_state_predicate, opm_has_property_state,
    opm_property, rdf_type, rdfs_comment, rdfs_label, schema_value, Object, Triple, XSD,
};
use serde::Deserialize;

use crate::{
    element_resource_iri, normalize_base_uri, sorted_values, spatial_resource_iri, ConvertOptions,
    StreamError, MAX_STREAM_BATCH_SIZE, MIN_STREAM_BATCH_SIZE,
};

const EMBEDDED_BSDD_INDEX_GZ: &[u8] =
    include_bytes!("../../resources/bsdd_ifc4x3_index.json.gz");
const BM_NS: &str = "https://w3id.org/ifc2lbd/bsdd-meta#";
const BSDD_CLASS_NS: &str = "https://identifier.buildingsmart.org/uri/buildingsmart/ifc/4.3/class/";
const BSDD_PROP_NS: &str = "https://identifier.buildingsmart.org/uri/buildingsmart/ifc/4.3/prop/";

static BSDD_INDEX: OnceLock<Result<BsddIndex, String>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchStatus {
    Matched,
    Normalized,
    Ambiguous,
    Unmapped,
}

#[derive(Clone, Debug)]
struct MatchResult {
    status: MatchStatus,
    property_code: Option<String>,
    exact_meta: Option<ExactMeta>,
}

#[derive(Clone, Debug, Deserialize)]
struct ClassMeta {
    #[serde(default)]
    name: String,
    #[serde(default)]
    definition: String,
}

#[derive(Clone, Debug, Deserialize)]
struct PropertyMeta {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    definition: String,
    #[serde(default)]
    value_kind: String,
}

#[derive(Clone, Debug, Deserialize)]
struct ExactMeta {
    #[serde(default)]
    property_set: String,
    #[serde(default)]
    class_property_code: String,
}

#[derive(Debug, Deserialize)]
struct BsddIndex {
    #[allow(dead_code)]
    format: String,
    #[allow(dead_code)]
    dictionary_code: String,
    #[allow(dead_code)]
    dictionary_version: String,
    #[allow(dead_code)]
    organization_code: String,
    class_code_by_norm: HashMap<String, String>,
    #[serde(default)]
    class_meta_by_code_norm: HashMap<String, ClassMeta>,
    prop_name_by_code_norm: HashMap<String, String>,
    #[serde(default)]
    prop_meta_by_code_norm: HashMap<String, PropertyMeta>,
    exact: HashMap<String, String>,
    #[serde(default)]
    exact_meta: HashMap<String, ExactMeta>,
    by_pset_prop: HashMap<String, Vec<String>>,
    by_class_prop: HashMap<String, Vec<String>>,
    by_prop: HashMap<String, Vec<String>>,
}

impl BsddIndex {
    fn resolve_class(&self, class_code_like: &str) -> Option<&str> {
        self.class_code_by_norm
            .get(&normalize(class_code_like))
            .map(String::as_str)
    }

    fn resolve_property(&self, class_code_like: &str, pset_name: &str, prop_name: &str) -> MatchResult {
        let class_norm = normalize(class_code_like);
        let pset_norm = normalize(pset_name);
        let prop_norm = normalize(prop_name);
        let pset_prop_key = format!("{pset_norm}|{prop_norm}");
        let class_prop_key = format!("{class_norm}|{prop_norm}");
        let exact_key = format!("{class_norm}|{pset_norm}|{prop_norm}");

        if let Some(code) = self.exact.get(&exact_key) {
            return MatchResult {
                status: MatchStatus::Matched,
                property_code: Some(code.clone()),
                exact_meta: self.exact_meta.get(&exact_key).cloned(),
            };
        }

        let pset_candidates = self.by_pset_prop.get(&pset_prop_key).cloned().unwrap_or_default();
        if pset_candidates.len() == 1 {
            return MatchResult {
                status: MatchStatus::Normalized,
                property_code: pset_candidates.first().cloned(),
                exact_meta: None,
            };
        }
        if pset_candidates.len() > 1 {
            return MatchResult {
                status: MatchStatus::Ambiguous,
                property_code: None,
                exact_meta: None,
            };
        }

        let class_candidates = self.by_class_prop.get(&class_prop_key).cloned().unwrap_or_default();
        if class_candidates.len() == 1 {
            return MatchResult {
                status: MatchStatus::Normalized,
                property_code: class_candidates.first().cloned(),
                exact_meta: None,
            };
        }
        if class_candidates.len() > 1 {
            return MatchResult {
                status: MatchStatus::Ambiguous,
                property_code: None,
                exact_meta: None,
            };
        }

        let prop_candidates = self.by_prop.get(&prop_norm).cloned().unwrap_or_default();
        if prop_candidates.len() == 1 {
            return MatchResult {
                status: MatchStatus::Normalized,
                property_code: prop_candidates.first().cloned(),
                exact_meta: None,
            };
        }
        if prop_candidates.len() > 1 {
            return MatchResult {
                status: MatchStatus::Ambiguous,
                property_code: None,
                exact_meta: None,
            };
        }

        MatchResult {
            status: MatchStatus::Unmapped,
            property_code: None,
            exact_meta: None,
        }
    }
}

fn load_bsdd_index() -> Result<&'static BsddIndex, String> {
    let result = BSDD_INDEX.get_or_init(|| {
        if let Ok(path) = std::env::var("IFC2LBD_BSDD_INDEX_JSON") {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed reading bSDD index '{path}': {e}"))?;
            let index: BsddIndex = serde_json::from_str(&raw)
                .map_err(|e| format!("failed parsing bSDD index '{path}': {e}"))?;
            return Ok(index);
        }

        let mut decoder = GzDecoder::new(EMBEDDED_BSDD_INDEX_GZ);
        let mut raw = String::new();
        decoder
            .read_to_string(&mut raw)
            .map_err(|e| format!("failed decompressing embedded bSDD index: {e}"))?;
        let index: BsddIndex = serde_json::from_str(&raw)
            .map_err(|e| format!("failed parsing embedded bSDD index: {e}"))?;
        Ok(index)
    });
    result.as_ref().map_err(Clone::clone)
}

fn normalize(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn bm(local: &str) -> String {
    format!("{BM_NS}{local}")
}

fn bsdd_class(code: &str) -> String {
    format!("{BSDD_CLASS_NS}{code}")
}

fn bsdd_prop(code: &str) -> String {
    format!("{BSDD_PROP_NS}{code}")
}

fn spatial_ifc_class(spatial_type: SpatialType) -> &'static str {
    match spatial_type {
        SpatialType::Project => "IfcProject",
        SpatialType::Site => "IfcSite",
        SpatialType::Building => "IfcBuilding",
        SpatialType::Storey => "IfcBuildingStorey",
        SpatialType::Space => "IfcSpace",
        SpatialType::Zone => "IfcSpatialZone",
        SpatialType::Facility => "IfcFacility",
        SpatialType::FacilityPart => "IfcFacilityPart",
        SpatialType::ExternalSpatialElement => "IfcExternalSpatialElement",
    }
}

fn step_value_to_object(value: &StepValue) -> Option<Object> {
    match value {
        StepValue::String(s) => Some(Object::Literal(s.to_string())),
        StepValue::Enum(s) => Some(Object::Literal(s.to_string())),
        StepValue::Bool(v) => Some(Object::TypedLiteral {
            value: v.to_string(),
            datatype: format!("{XSD}boolean"),
        }),
        StepValue::Int(v) => Some(Object::TypedLiteral {
            value: v.to_string(),
            datatype: format!("{XSD}integer"),
        }),
        StepValue::Real(v) => Some(Object::TypedLiteral {
            value: v.to_string(),
            datatype: format!("{XSD}decimal"),
        }),
        StepValue::Typed { value, .. } => step_value_to_object(value),
        _ => None,
    }
}

fn mapping_status_iri(status: MatchStatus) -> String {
    match status {
        MatchStatus::Matched => bm("Matched"),
        MatchStatus::Normalized => bm("Normalized"),
        MatchStatus::Ambiguous => bm("Ambiguous"),
        MatchStatus::Unmapped => bm("Unmapped"),
    }
}

pub fn stream_bsdd(
    model: &IfcModel,
    options: &ConvertOptions,
    sender: &Sender<Vec<Triple>>,
) -> Result<u64, StreamError> {
    let index = load_bsdd_index().map_err(StreamError::Conversion)?;
    let base = normalize_base_uri(&options.base_uri);
    let batch_size = options
        .stream_batch_size
        .clamp(MIN_STREAM_BATCH_SIZE, MAX_STREAM_BATCH_SIZE);

    let mut batch = Vec::with_capacity(batch_size);
    let mut triples = 0_u64;
    let mut property_counter = 0_u64;
    let mut emitted_class_meta = std::collections::HashSet::<String>::new();
    let mut emitted_property_meta = std::collections::HashSet::<String>::new();

    // Standalone typing: elements
    for element in sorted_values(&model.elements) {
        if let Some(class_code) = index.resolve_class(element.entity_name.as_str()) {
            emit_class_metadata(
                index,
                class_code,
                &mut emitted_class_meta,
                &mut batch,
                sender,
                batch_size,
                &mut triples,
            )?;
            push(
                &mut batch,
                sender,
                batch_size,
                Triple {
                    subject: element_resource_iri(&base, element),
                    predicate: rdf_type(),
                    object: Object::Iri(bsdd_class(class_code)),
                },
                &mut triples,
            )?;
        }
    }

    // Standalone typing: spatial nodes
    for spatial in sorted_values(&model.spatial_nodes) {
        let class_guess = spatial_ifc_class(spatial.spatial_type);
        if let Some(class_code) = index.resolve_class(class_guess) {
            emit_class_metadata(
                index,
                class_code,
                &mut emitted_class_meta,
                &mut batch,
                sender,
                batch_size,
                &mut triples,
            )?;
            push(
                &mut batch,
                sender,
                batch_size,
                Triple {
                    subject: spatial_resource_iri(&base, spatial.spatial_type, &spatial.guid),
                    predicate: rdf_type(),
                    object: Object::Iri(bsdd_class(class_code)),
                },
                &mut triples,
            )?;
        }
    }

    let mut object_ids: Vec<_> = model.property_sets_for_object.keys().copied().collect();
    object_ids.sort_unstable();

    for object_id in object_ids {
        let (subject, object_guid, class_name_like) = if let Some(element) = model.elements.get(&object_id) {
            (
                element_resource_iri(&base, element),
                element.guid.to_string(),
                element.entity_name.to_string(),
            )
        } else if let Some(spatial) = model.spatial_nodes.get(&object_id) {
            (
                spatial_resource_iri(&base, spatial.spatial_type, &spatial.guid),
                spatial.guid.to_string(),
                spatial_ifc_class(spatial.spatial_type).to_string(),
            )
        } else {
            continue;
        };

        let mut pset_ids = model.property_sets_for_object[&object_id].clone();
        pset_ids.sort_unstable();

        for pset_id in pset_ids {
            let Some(pset) = model.property_sets.get(&pset_id) else {
                continue;
            };
            let pset_name = pset.name.as_deref().unwrap_or_default();

            for prop_id in &pset.properties {
                if let Some(psv) = model.property_single_values.get(prop_id) {
                    let Some(raw_value) = psv.nominal_value.as_ref().and_then(step_value_to_object) else {
                        continue;
                    };
                    property_counter += 1;
                    emit_property(
                        &subject,
                        &object_guid,
                        pset_name,
                        psv.name.as_str(),
                        raw_value,
                        &class_name_like,
                        index,
                        &mut emitted_property_meta,
                        &mut property_counter,
                        &mut batch,
                        sender,
                        batch_size,
                        &mut triples,
                    )?;
                    continue;
                }

                if let Some(pev) = model.property_enumerated_values.get(prop_id) {
                    for enum_value in &pev.values {
                        property_counter += 1;
                        emit_property(
                            &subject,
                            &object_guid,
                            pset_name,
                            pev.name.as_str(),
                            Object::Literal(enum_value.to_string()),
                            &class_name_like,
                        index,
                        &mut emitted_property_meta,
                        &mut property_counter,
                            &mut batch,
                            sender,
                            batch_size,
                            &mut triples,
                        )?;
                    }
                }
            }
        }
    }

    if !batch.is_empty() {
        sender.send(batch).map_err(|_| StreamError::ChannelClosed)?;
    }
    Ok(triples)
}

#[allow(clippy::too_many_arguments)]
fn emit_property(
    subject: &str,
    object_guid: &str,
    pset_name: &str,
    prop_name: &str,
    value: Object,
    class_name_like: &str,
    index: &BsddIndex,
    emitted_property_meta: &mut std::collections::HashSet<String>,
    property_counter: &mut u64,
    batch: &mut Vec<Triple>,
    sender: &Sender<Vec<Triple>>,
    batch_size: usize,
    triples: &mut u64,
) -> Result<(), StreamError> {
    let match_result = index.resolve_property(class_name_like, pset_name, prop_name);
    let prop_subject = format!(
        "{subject}/bsdd_property_{}_{}_{}",
        sanitize(prop_name),
        sanitize(object_guid),
        property_counter
    );
    let state_subject = format!(
        "{subject}/bsdd_state_{}_{}_{}",
        sanitize(prop_name),
        sanitize(object_guid),
        property_counter
    );

    let predicate = match match_result.property_code.as_deref() {
        Some(code) => bsdd_prop(code),
        None => bm("hasUnmappedProperty"),
    };

    if let Some(code) = match_result.property_code.as_deref() {
        emit_property_metadata(
            index,
            code,
            emitted_property_meta,
            batch,
            sender,
            batch_size,
            triples,
        )?;
    }

    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: subject.to_string(),
            predicate,
            object: Object::Iri(prop_subject.clone()),
        },
        triples,
    )?;

    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: prop_subject.clone(),
            predicate: rdf_type(),
            object: Object::Iri(opm_property()),
        },
        triples,
    )?;
    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: prop_subject.clone(),
            predicate: opm_current_property_state_predicate(),
            object: Object::Iri(state_subject.clone()),
        },
        triples,
    )?;
    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: prop_subject.clone(),
            predicate: opm_has_property_state(),
            object: Object::Iri(state_subject.clone()),
        },
        triples,
    )?;
    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: prop_subject.clone(),
            predicate: rdfs_label(),
            object: Object::Literal(format!("{pset_name}:{prop_name}")),
        },
        triples,
    )?;
    if !pset_name.is_empty() {
        push(
            batch,
            sender,
            batch_size,
            Triple {
                subject: prop_subject.clone(),
                predicate: bm("propertySet"),
                object: Object::Literal(pset_name.to_string()),
            },
            triples,
        )?;
    }
    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: prop_subject.clone(),
            predicate: bm("mappingStatus"),
            object: Object::Iri(mapping_status_iri(match_result.status)),
        },
        triples,
    )?;
    if let Some(meta) = match_result.exact_meta.as_ref() {
        if !meta.class_property_code.is_empty() {
            push(
                batch,
                sender,
                batch_size,
                Triple {
                    subject: prop_subject.clone(),
                    predicate: bm("classPropertyCode"),
                    object: Object::Literal(meta.class_property_code.clone()),
                },
                triples,
            )?;
        }
        if !meta.property_set.is_empty() {
            push(
                batch,
                sender,
                batch_size,
                Triple {
                    subject: prop_subject.clone(),
                    predicate: bm("matchedPropertySet"),
                    object: Object::Literal(meta.property_set.clone()),
                },
                triples,
            )?;
        }
    }

    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: state_subject.clone(),
            predicate: rdf_type(),
            object: Object::Iri(opm_current_property_state()),
        },
        triples,
    )?;
    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: state_subject,
            predicate: schema_value(),
            object: value,
        },
        triples,
    )?;

    if let Some(code) = match_result.property_code.as_deref() {
        if let Some(name) = index.prop_name_by_code_norm.get(&normalize(code)) {
            if !name.is_empty() {
                push(
                    batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: subject.to_string(),
                        predicate: bm("matchedPropertyName"),
                        object: Object::Literal(name.clone()),
                    },
                    triples,
                )?;
            }
        }
    }

    Ok(())
}

fn emit_class_metadata(
    index: &BsddIndex,
    class_code: &str,
    emitted: &mut std::collections::HashSet<String>,
    batch: &mut Vec<Triple>,
    sender: &Sender<Vec<Triple>>,
    batch_size: usize,
    triples: &mut u64,
) -> Result<(), StreamError> {
    if !emitted.insert(class_code.to_string()) {
        return Ok(());
    }
    let class_iri = bsdd_class(class_code);
    let key = normalize(class_code);
    if let Some(meta) = index.class_meta_by_code_norm.get(&key) {
        if !meta.name.is_empty() {
            push(
                batch,
                sender,
                batch_size,
                Triple {
                    subject: class_iri.clone(),
                    predicate: rdfs_label(),
                    object: Object::Literal(meta.name.clone()),
                },
                triples,
            )?;
        }
        if !meta.definition.is_empty() {
            push(
                batch,
                sender,
                batch_size,
                Triple {
                    subject: class_iri,
                    predicate: rdfs_comment(),
                    object: Object::Literal(meta.definition.clone()),
                },
                triples,
            )?;
        }
    }
    Ok(())
}

fn emit_property_metadata(
    index: &BsddIndex,
    prop_code: &str,
    emitted: &mut std::collections::HashSet<String>,
    batch: &mut Vec<Triple>,
    sender: &Sender<Vec<Triple>>,
    batch_size: usize,
    triples: &mut u64,
) -> Result<(), StreamError> {
    if !emitted.insert(prop_code.to_string()) {
        return Ok(());
    }
    let prop_iri = bsdd_prop(prop_code);
    let key = normalize(prop_code);
    if let Some(meta) = index.prop_meta_by_code_norm.get(&key) {
        if !meta.name.is_empty() {
            push(
                batch,
                sender,
                batch_size,
                Triple {
                    subject: prop_iri.clone(),
                    predicate: rdfs_label(),
                    object: Object::Literal(meta.name.clone()),
                },
                triples,
            )?;
        }
        if !meta.description.is_empty() {
            push(
                batch,
                sender,
                batch_size,
                Triple {
                    subject: prop_iri.clone(),
                    predicate: rdfs_comment(),
                    object: Object::Literal(meta.description.clone()),
                },
                triples,
            )?;
        } else if !meta.definition.is_empty() {
            push(
                batch,
                sender,
                batch_size,
                Triple {
                    subject: prop_iri.clone(),
                    predicate: rdfs_comment(),
                    object: Object::Literal(meta.definition.clone()),
                },
                triples,
            )?;
        }
        if !meta.value_kind.is_empty() {
            push(
                batch,
                sender,
                batch_size,
                Triple {
                    subject: prop_iri,
                    predicate: bm("propertyValueKind"),
                    object: Object::Literal(meta.value_kind.clone()),
                },
                triples,
            )?;
        }
    }
    Ok(())
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn push(
    batch: &mut Vec<Triple>,
    sender: &Sender<Vec<Triple>>,
    batch_size: usize,
    triple: Triple,
    counter: &mut u64,
) -> Result<(), StreamError> {
    *counter += 1;
    batch.push(triple);
    if batch.len() >= batch_size {
        sender
            .send(std::mem::take(batch))
            .map_err(|_| StreamError::ChannelClosed)?;
    }
    Ok(())
}
