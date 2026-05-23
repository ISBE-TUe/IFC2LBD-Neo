use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::OnceLock;

use crossbeam::channel::Sender;
use flate2::read::GzDecoder;
use ifc_model::IfcModel;
use ifc_schema::SpatialType;
use ifc_step::{decode_ifc_unicode, StepSchema, StepValue};
use lbd_ontology::{
    opm_current_property_state, opm_has_property_state, opm_property, prov_generated_at_time,
    rdf_type, rdfs_label, schema_value, smls_unit, Object, Triple, XSD,
};
use serde::Deserialize;
use serde_json::json;
use strsim::jaro_winkler;
use tracing::info;
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

use crate::{
    build_unit_type_map, current_generated_at_rfc3339, element_resource_iri, normalize_base_uri,
    resolve_property_unit, resolve_quantity_unit, sorted_values, spatial_resource_iri,
    ConvertOptions, StreamError, MAX_STREAM_BATCH_SIZE, MIN_STREAM_BATCH_SIZE,
};

const EMBEDDED_BSDD_INDEX_GZ: &[u8] =
    include_bytes!("../../resources/bsdd_ifc4x3_index.json.gz");
const EMBEDDED_BSDD_MATCHING_JSON: &str =
    include_str!("../../resources/bsdd_matching.json");
const BSDDM_NS: &str = "https://w3id.org/ifc2lbd/bsdd-meta#";
const BSDD_CLASS_NS: &str = "https://identifier.buildingsmart.org/uri/buildingsmart/ifc/4.3/class/";
const BSDD_PROP_NS: &str = "https://identifier.buildingsmart.org/uri/buildingsmart/ifc/4.3/prop/";

static BSDD_INDEX: OnceLock<Result<BsddIndex, String>> = OnceLock::new();
static BSDD_MATCHING: OnceLock<Result<BsddMatchingConfig, String>> = OnceLock::new();

#[derive(Clone, Debug, Default)]
pub struct BsddMatchCache {
    by_key: HashMap<String, BsddPreparedMatch>,
}

impl BsddMatchCache {
    /// Returns a JSON summary of match statistics for the log exporter.
    pub fn stats(&self) -> serde_json::Value {
        let mut matched_exact: usize = 0;
        let mut matched_fuzzy: usize = 0;
        let mut normalized: usize = 0;
        let mut ambiguous: usize = 0;
        let mut unmapped: usize = 0;
        let mut fuzzy_scores: Vec<f64> = Vec::new();
        let mut method_counts: HashMap<&'static str, usize> = HashMap::new();

        for m in self.by_key.values() {
            *method_counts.entry(m.method).or_insert(0) += 1;
            match m.status {
                MatchStatus::Matched => {
                    if m.method == "fuzzy" {
                        matched_fuzzy += 1;
                        if let Some(s) = m.confidence {
                            fuzzy_scores.push(s);
                        }
                    } else {
                        matched_exact += 1;
                    }
                }
                MatchStatus::Normalized => normalized += 1,
                MatchStatus::Ambiguous => ambiguous += 1,
                MatchStatus::Unmapped => unmapped += 1,
            }
        }

        let total_matched = matched_exact + matched_fuzzy + normalized;
        let fuzzy_avg = if fuzzy_scores.is_empty() {
            None
        } else {
            Some(fuzzy_scores.iter().sum::<f64>() / fuzzy_scores.len() as f64)
        };
        let fuzzy_min = fuzzy_scores.iter().cloned().reduce(f64::min);
        let fuzzy_max = fuzzy_scores.iter().cloned().reduce(f64::max);

        json!({
            "total_unique_keys": self.by_key.len(),
            "total_matched": total_matched,
            "matched_exact": matched_exact,
            "matched_fuzzy": matched_fuzzy,
            "normalized": normalized,
            "ambiguous": ambiguous,
            "unmapped": unmapped,
            "fuzzy_confidence_avg": fuzzy_avg,
            "fuzzy_confidence_min": fuzzy_min,
            "fuzzy_confidence_max": fuzzy_max,
            "method_counts": method_counts,
        })
    }
}

#[derive(Clone, Debug)]
pub struct BsddPreparedMatch {
    status: MatchStatus,
    property_code: Option<String>,
    exact_meta: Option<ExactMeta>,
    method: &'static str,
    confidence: Option<f64>,
}

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
    method: &'static str,
    confidence: Option<f64>,
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
    prop_name_by_code_norm: HashMap<String, String>,
    exact: HashMap<String, String>,
    #[serde(default)]
    exact_meta: HashMap<String, ExactMeta>,
    by_pset_prop: HashMap<String, Vec<String>>,
    by_class_prop: HashMap<String, Vec<String>>,
    by_prop: HashMap<String, Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct BsddMatchingConfig {
    #[serde(default)]
    class_aliases: HashMap<String, String>,
    #[serde(default)]
    pset_aliases: HashMap<String, String>,
    #[serde(default)]
    prop_aliases: HashMap<String, String>,
    #[serde(default)]
    pset_prop_aliases: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    hard_mappings: HashMap<String, String>,
    #[serde(default)]
    schema_class_aliases: HashMap<String, HashMap<String, String>>,
}

impl BsddIndex {
    fn resolve_class(&self, class_code_like: &str) -> Option<&str> {
        self.class_code_by_norm
            .get(&normalize(class_code_like))
            .map(String::as_str)
    }

    fn resolve_property(
        &self,
        schema: StepSchema,
        class_code_like: &str,
        pset_name: &str,
        prop_name: &str,
        matching: &BsddMatchingConfig,
    ) -> MatchResult {
        let empty_schema_aliases: HashMap<String, String> = HashMap::new();
        let schema_aliases = matching
            .schema_class_aliases
            .get(&schema.to_string())
            .unwrap_or(&empty_schema_aliases);
        let class_norms = normalized_match_variants(
            class_code_like,
            &matching.class_aliases,
            schema_aliases,
            &HashMap::new(),
            false,
        );
        let pset_norms = normalized_match_variants(
            pset_name,
            &matching.pset_aliases,
            &HashMap::new(),
            &HashMap::new(),
            false,
        );
        let prop_norms = normalized_match_variants(
            prop_name,
            &matching.prop_aliases,
            &HashMap::new(),
            matching
                .pset_prop_aliases
                .get(
                    &normalized_match_variants(
                        pset_name,
                        &matching.pset_aliases,
                        &HashMap::new(),
                        &HashMap::new(),
                        false,
                    )
                    .into_iter()
                    .next()
                    .unwrap_or_default(),
                )
                .unwrap_or(&HashMap::new()),
            true,
        );

        for class_norm in &class_norms {
            for pset_norm in &pset_norms {
                for prop_norm in &prop_norms {
                    let exact_key = format!("{class_norm}|{pset_norm}|{prop_norm}");
                    let hard_key = exact_key.clone();
                    let schema_hard_key =
                        format!("{}|{hard_key}", schema.to_string().to_ascii_lowercase());

                    if let Some(code) = matching
                        .hard_mappings
                        .get(&schema_hard_key)
                        .or_else(|| matching.hard_mappings.get(&hard_key))
                    {
                        return MatchResult {
                            status: MatchStatus::Matched,
                            property_code: Some(code.clone()),
                            exact_meta: self.exact_meta.get(&exact_key).cloned(),
                            method: "hard_override",
                            confidence: Some(1.0),
                        };
                    }

                    if let Some(code) = self.exact.get(&exact_key) {
                        return MatchResult {
                            status: MatchStatus::Matched,
                            property_code: Some(code.clone()),
                            exact_meta: self.exact_meta.get(&exact_key).cloned(),
                            method: "exact_class_pset_prop",
                            confidence: Some(1.0),
                        };
                    }
                }
            }
        }

        let pset_candidates = collect_candidates(
            pset_norms
                .iter()
                .flat_map(|pset_norm| prop_norms.iter().map(move |prop_norm| format!("{pset_norm}|{prop_norm}"))),
            &self.by_pset_prop,
        );
        if pset_candidates.len() == 1 {
            return MatchResult {
                status: MatchStatus::Normalized,
                property_code: pset_candidates.first().cloned(),
                exact_meta: None,
                method: "normalized_pset_prop",
                confidence: Some(0.95),
            };
        }
        if pset_candidates.len() > 1 {
            return MatchResult {
                status: MatchStatus::Ambiguous,
                property_code: None,
                exact_meta: None,
                method: "ambiguous_pset_prop",
                confidence: None,
            };
        }

        let class_candidates = collect_candidates(
            class_norms
                .iter()
                .flat_map(|class_norm| prop_norms.iter().map(move |prop_norm| format!("{class_norm}|{prop_norm}"))),
            &self.by_class_prop,
        );
        if class_candidates.len() == 1 {
            return MatchResult {
                status: MatchStatus::Normalized,
                property_code: class_candidates.first().cloned(),
                exact_meta: None,
                method: "normalized_class_prop",
                confidence: Some(0.9),
            };
        }
        if class_candidates.len() > 1 {
            return MatchResult {
                status: MatchStatus::Ambiguous,
                property_code: None,
                exact_meta: None,
                method: "ambiguous_class_prop",
                confidence: None,
            };
        }

        let prop_candidates = collect_candidates(prop_norms.iter().cloned(), &self.by_prop);
        if prop_candidates.len() == 1 {
            return MatchResult {
                status: MatchStatus::Normalized,
                property_code: prop_candidates.first().cloned(),
                exact_meta: None,
                method: "normalized_prop",
                confidence: Some(0.85),
            };
        }
        if prop_candidates.len() > 1 {
            return MatchResult {
                status: MatchStatus::Ambiguous,
                property_code: None,
                exact_meta: None,
                method: "ambiguous_prop",
                confidence: None,
            };
        }

        let class_norm = class_norms.first().map(String::as_str).unwrap_or_default();
        let pset_norm = pset_norms.first().map(String::as_str).unwrap_or_default();
        for prop_norm in &prop_norms {
            if let Some((code, score)) = self.resolve_fuzzy(class_norm, pset_norm, prop_norm) {
                return MatchResult {
                    status: MatchStatus::Normalized,
                    property_code: Some(code),
                    exact_meta: None,
                    method: "fuzzy",
                    confidence: Some(score),
                };
            }
        }

        MatchResult {
            status: MatchStatus::Unmapped,
            property_code: None,
            exact_meta: None,
            method: "unmapped",
            confidence: None,
        }
    }

    fn resolve_fuzzy(&self, class_norm: &str, pset_norm: &str, prop_norm: &str) -> Option<(String, f64)> {
        let mut candidates: Vec<(String, f64)> = Vec::new();
        let _ = class_norm;
        let _ = pset_norm;
        let first = prop_norm.chars().next();
        let prop_len = prop_norm.chars().count() as i32;
        let mut inspected = 0usize;
        for (kprop, codes) in &self.by_prop {
            if inspected > 400 {
                break;
            }
            let kfirst = kprop.chars().next();
            if first.is_some() && kfirst != first {
                continue;
            }
            let klen = kprop.chars().count() as i32;
            if (klen - prop_len).abs() > 3 {
                continue;
            }
            inspected += 1;
            let score = string_similarity(prop_norm, kprop);
            if score >= 0.94 {
                for code in codes {
                    candidates.push((code.clone(), score));
                }
            }
        }
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let best = candidates[0].clone();
        let close: HashSet<&str> = candidates
            .iter()
            .filter(|(_, s)| *s + 0.02 >= best.1)
            .map(|(c, _)| c.as_str())
            .collect();
        if close.len() == 1 {
            Some(best)
        } else {
            None
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

fn load_bsdd_matching() -> Result<&'static BsddMatchingConfig, String> {
    let result = BSDD_MATCHING.get_or_init(|| {
        if let Ok(path) = std::env::var("IFC2LBD_BSDD_MATCHING_JSON") {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed reading bSDD matching config '{path}': {e}"))?;
            let cfg: BsddMatchingConfig = serde_json::from_str(&raw)
                .map_err(|e| format!("failed parsing bSDD matching config '{path}': {e}"))?;
            return Ok(cfg);
        }
        let cfg: BsddMatchingConfig = serde_json::from_str(EMBEDDED_BSDD_MATCHING_JSON)
            .map_err(|e| format!("failed parsing embedded bSDD matching config: {e}"))?;
        Ok(cfg)
    });
    result.as_ref().map_err(Clone::clone)
}

fn normalize(input: &str) -> String {
    transliterate_for_matching(input)
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn canonical_alias(
    input: &str,
    aliases: &HashMap<String, String>,
    schema_aliases: &HashMap<String, String>,
) -> String {
    let key = normalize(input);
    if let Some(v) = schema_aliases.get(&key) {
        return v.clone();
    }
    if let Some(v) = aliases.get(&key) {
        return v.clone();
    }
    input.to_string()
}

fn string_similarity(a: &str, b: &str) -> f64 {
    jaro_winkler(a, b)
}

fn transliterate_for_matching(input: &str) -> String {
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

fn normalized_match_variants(
    input: &str,
    aliases: &HashMap<String, String>,
    schema_aliases: &HashMap<String, String>,
    scoped_aliases: &HashMap<String, String>,
    strip_segments: bool,
) -> Vec<String> {
    let mut raw_candidates = vec![input.trim().to_string()];
    let key = normalize(input);
    if let Some(v) = schema_aliases.get(&key) {
        raw_candidates.insert(0, v.clone());
    }
    if let Some(v) = scoped_aliases.get(&key) {
        raw_candidates.insert(0, v.clone());
    }
    if let Some(v) = aliases.get(&key) {
        raw_candidates.insert(0, v.clone());
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for candidate in raw_candidates {
        for surface in surface_variants(&candidate, strip_segments) {
            let norm = normalize(&surface);
            if !norm.is_empty() && seen.insert(norm.clone()) {
                out.push(norm);
            }
        }
    }
    out
}

fn surface_variants(input: &str, strip_segments: bool) -> Vec<String> {
    let mut out = Vec::new();
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return out;
    }
    out.push(trimmed.to_string());
    let compact = trimmed.replace("::", ":");
    if compact != trimmed {
        out.push(compact.clone());
    }
    if let Some(stripped) = strip_software_prefix(&compact) {
        out.push(stripped.clone());
    }
    if strip_segments {
        let segment_sources = out.clone();
        for source in segment_sources {
            for sep in [':', '/', '\\', '|'] {
                if let Some(last) = source.rsplit(sep).next() {
                    let last = last.trim();
                    if !last.is_empty() && last != trimmed {
                        out.push(last.to_string());
                    }
                }
            }
            if let Some((_, suffix)) = source.split_once(' ') {
                let suffix = suffix.trim();
                if !suffix.is_empty() && suffix != trimmed {
                    out.push(suffix.to_string());
                }
            }
        }
    }
    out
}

fn strip_software_prefix(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if let Some((prefix, suffix)) = trimmed.split_once(' ') {
        let norm_prefix = normalize(prefix);
        if norm_prefix.starts_with("bsdpset") || norm_prefix.starts_with("psetrevit") {
            let suffix = suffix.trim();
            if !suffix.is_empty() {
                return Some(suffix.to_string());
            }
        }
    }
    None
}

fn collect_candidates<I>(keys: I, lookup: &HashMap<String, Vec<String>>) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for key in keys {
        if let Some(values) = lookup.get(&key) {
            for value in values {
                if seen.insert(value.clone()) {
                    out.push(value.clone());
                }
            }
        }
    }
    out
}

fn bsddm(local: &str) -> String {
    format!("{BSDDM_NS}{local}")
}

fn bsdd_class(code: &str) -> String {
    format!("{BSDD_CLASS_NS}{code}")
}

fn bsdd_prop(code: &str) -> String {
    format!("{BSDD_PROP_NS}{code}")
}

fn bsdd_local_instance(base: &str, kind: &str, local: &str) -> String {
    format!("{base}/bsdd_{kind}_{local}")
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

fn resolve_bsdd_class_for_element(index: &BsddIndex, entity_name: &str) -> Option<String> {
    let trimmed = entity_name.trim();
    if let Some(code) = index.resolve_class(trimmed) {
        return Some(code.to_string());
    }
    let upper = trimmed.to_ascii_uppercase();
    let prefixed = if upper.starts_with("IFC") {
        upper.clone()
    } else {
        format!("IFC{upper}")
    };
    let camel = normalize_ifc_entity(trimmed);
    for candidate in [&upper, &prefixed, &camel] {
        if let Some(code) = index.resolve_class(candidate) {
            return Some(code.to_string());
        }
    }
    // Common IFC alias fallback: IFCWALLSTANDARDCASE -> IfcWall in bSDD.
    if upper == "IFCWALLSTANDARDCASE" || prefixed == "IFCWALLSTANDARDCASE" {
        if let Some(code) = index.resolve_class("IfcWall") {
            return Some(code.to_string());
        }
    }
    None
}

fn normalize_ifc_entity(raw: &str) -> String {
    let upper = raw.trim().to_ascii_uppercase();
    let core = upper.strip_prefix("IFC").unwrap_or(upper.as_str());
    let mut out = String::from("Ifc");
    let mut next_upper = true;
    for ch in core.chars() {
        if !ch.is_ascii_alphanumeric() {
            next_upper = true;
            continue;
        }
        if next_upper {
            out.push(ch.to_ascii_uppercase());
            next_upper = false;
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

fn step_value_to_object(value: &StepValue) -> Option<Object> {
    match value {
        StepValue::String(s) => Some(Object::Literal(decode_ifc_unicode(s))),
        StepValue::Enum(s) => Some(Object::Literal(decode_ifc_unicode(s))),
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
        MatchStatus::Matched => bsddm("Mapped"),
        MatchStatus::Normalized => bsddm("Normalized"),
        MatchStatus::Ambiguous => bsddm("Ambiguous"),
        MatchStatus::Unmapped => bsddm("Unmapped"),
    }
}

pub fn build_bsdd_match_cache(model: &IfcModel) -> Result<BsddMatchCache, String> {
    let index = load_bsdd_index()?;
    let matching = load_bsdd_matching()?;
    let mut by_key = HashMap::new();

    let mut object_ids: Vec<_> = model.property_sets_for_object.keys().copied().collect();
    object_ids.sort_unstable();
    for object_id in object_ids {
        let class_name_like = if let Some(element) = model.elements.get(&object_id) {
            element.entity_name.to_string()
        } else if let Some(spatial) = model.spatial_nodes.get(&object_id) {
            spatial_ifc_class(spatial.spatial_type).to_string()
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
                    let key = cache_key(model.schema, &class_name_like, pset_name, psv.name.as_str(), matching);
                    by_key.entry(key).or_insert_with(|| {
                        let m = index.resolve_property(
                            model.schema,
                            &class_name_like,
                            pset_name,
                            psv.name.as_str(),
                            matching,
                        );
                        BsddPreparedMatch {
                            status: m.status,
                            property_code: m.property_code,
                            exact_meta: m.exact_meta,
                            method: m.method,
                            confidence: m.confidence,
                        }
                    });
                }
                if let Some(pev) = model.property_enumerated_values.get(prop_id) {
                    let key = cache_key(model.schema, &class_name_like, pset_name, pev.name.as_str(), matching);
                    by_key.entry(key).or_insert_with(|| {
                        let m = index.resolve_property(
                            model.schema,
                            &class_name_like,
                            pset_name,
                            pev.name.as_str(),
                            matching,
                        );
                        BsddPreparedMatch {
                            status: m.status,
                            property_code: m.property_code,
                            exact_meta: m.exact_meta,
                            method: m.method,
                            confidence: m.confidence,
                        }
                    });
                }
            }
        }
    }

    Ok(BsddMatchCache { by_key })
}

pub fn dedup_model_property_sets(model: &IfcModel) -> IfcModel {
    let mut updated = model.clone();
    for pset in updated.property_sets.values_mut() {
        let mut seen = HashSet::new();
        let mut deduped = Vec::with_capacity(pset.properties.len());
        for property_id in &pset.properties {
            let key = if let Some(psv) = updated.property_single_values.get(property_id) {
                let value_sig = psv
                    .nominal_value
                    .as_ref()
                    .map(step_value_signature)
                    .unwrap_or_else(|| "none".to_string());
                format!("single|{}|{value_sig}", normalize(psv.name.as_str()))
            } else if let Some(pev) = updated.property_enumerated_values.get(property_id) {
                let values = pev.values.iter().map(ToString::to_string).collect::<Vec<_>>().join("|");
                format!("enum|{}|{values}", normalize(pev.name.as_str()))
            } else {
                format!("unknown|{property_id}")
            };
            if seen.insert(key) {
                deduped.push(*property_id);
            }
        }
        pset.properties = deduped;
    }
    updated
}

fn resolve_from_cache(
    cache: &BsddMatchCache,
    schema: StepSchema,
    class_name_like: &str,
    pset_name: &str,
    prop_name: &str,
    matching: &BsddMatchingConfig,
) -> Option<MatchResult> {
    let key = cache_key(schema, class_name_like, pset_name, prop_name, matching);
    cache.by_key.get(&key).map(|prepared| MatchResult {
        status: prepared.status,
        property_code: prepared.property_code.clone(),
        exact_meta: prepared.exact_meta.clone(),
        method: prepared.method,
        confidence: prepared.confidence,
    })
}

fn step_value_signature(value: &StepValue) -> String {
    match value {
        StepValue::String(v) => format!("s:{v}"),
        StepValue::Enum(v) => format!("e:{v}"),
        StepValue::Bool(v) => format!("b:{v}"),
        StepValue::Int(v) => format!("i:{v}"),
        StepValue::Real(v) => format!("r:{v}"),
        StepValue::Typed { type_name, value } => {
            format!("t:{type_name}:{}", step_value_signature(value))
        }
        _ => format!("{value:?}"),
    }
}

fn cache_key(
    schema: StepSchema,
    class_code_like: &str,
    pset_name: &str,
    prop_name: &str,
    matching: &BsddMatchingConfig,
) -> String {
    let empty_schema_aliases: HashMap<String, String> = HashMap::new();
    let schema_aliases = matching
        .schema_class_aliases
        .get(&schema.to_string())
        .unwrap_or(&empty_schema_aliases);
    let class_norm = normalized_match_variants(
        class_code_like,
        &matching.class_aliases,
        schema_aliases,
        &HashMap::new(),
        false,
    )
    .into_iter()
    .next()
    .unwrap_or_default();
    let pset_norm = normalized_match_variants(
        pset_name,
        &matching.pset_aliases,
        &HashMap::new(),
        &HashMap::new(),
        false,
    )
    .into_iter()
    .next()
    .unwrap_or_default();
    let prop_norm = normalized_match_variants(
        prop_name,
        &matching.prop_aliases,
        &HashMap::new(),
        matching
            .pset_prop_aliases
            .get(&pset_norm)
            .unwrap_or(&HashMap::new()),
        true,
    )
    .into_iter()
    .next()
    .unwrap_or_default();
    format!(
        "{}|{}|{}|{}",
        schema.to_string().to_ascii_lowercase(),
        class_norm,
        pset_norm,
        prop_norm
    )
}

pub fn stream_bsdd(
    model: &IfcModel,
    options: &ConvertOptions,
    sender: &Sender<Vec<Triple>>,
) -> Result<u64, StreamError> {
    stream_bsdd_with_cache(model, options, sender, None)
}

pub fn stream_bsdd_with_cache(
    model: &IfcModel,
    options: &ConvertOptions,
    sender: &Sender<Vec<Triple>>,
    match_cache: Option<&BsddMatchCache>,
) -> Result<u64, StreamError> {
    let index = load_bsdd_index().map_err(StreamError::Conversion)?;
    let matching = load_bsdd_matching().map_err(StreamError::Conversion)?;
    let base = normalize_base_uri(&options.base_uri);
    let batch_size = options
        .stream_batch_size
        .clamp(MIN_STREAM_BATCH_SIZE, MAX_STREAM_BATCH_SIZE);
    let unit_by_type = build_unit_type_map(model);
    let generated_at = current_generated_at_rfc3339();

    let mut batch = Vec::with_capacity(batch_size);
    let mut triples = 0_u64;
    let mut property_counter = 0_u64;
    let mut quantity_counter = 0_u64;
    let mut unmatched_histogram: HashMap<String, u64> = HashMap::new();
    // Standalone typing: elements
    for element in sorted_values(&model.elements) {
        if let Some(class_code) = resolve_bsdd_class_for_element(index, element.entity_name.as_str()) {
            push(
                &mut batch,
                sender,
                batch_size,
                Triple {
                    subject: element_resource_iri(&base, element),
                    predicate: rdf_type(),
                    object: Object::Iri(bsdd_class(&class_code)),
                },
                &mut triples,
            )?;
        }
    }

    // Standalone typing: spatial nodes
    for spatial in sorted_values(&model.spatial_nodes) {
        let class_guess = spatial_ifc_class(spatial.spatial_type);
        if let Some(class_code) = index.resolve_class(class_guess) {
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
            let pset_subject = bsdd_local_instance(
                &base,
                "pset",
                &format!("{}_{}", sanitize(pset_name), sanitize(&object_guid)),
            );

            push(
                &mut batch,
                sender,
                batch_size,
                Triple {
                    subject: subject.clone(),
                    predicate: bsddm("hasPropertySet"),
                    object: Object::Iri(pset_subject.clone()),
                },
                &mut triples,
            )?;
            push(
                &mut batch,
                sender,
                batch_size,
                Triple {
                    subject: pset_subject.clone(),
                    predicate: rdf_type(),
                    object: Object::Iri(bsddm("PropertySet")),
                },
                &mut triples,
            )?;
            if let Some(pset_code) = index.resolve_class(pset_name) {
                push(
                    &mut batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: pset_subject.clone(),
                        predicate: rdf_type(),
                        object: Object::Iri(bsdd_class(pset_code)),
                    },
                    &mut triples,
                )?;
            } else {
                push(
                    &mut batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: pset_subject.clone(),
                        predicate: rdf_type(),
                        object: Object::Iri(bsddm("customPropertySet")),
                    },
                    &mut triples,
                )?;
            }
            if !pset_name.is_empty() {
                push(
                    &mut batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: pset_subject.clone(),
                        predicate: rdfs_label(),
                        object: Object::Literal(pset_name.to_string()),
                    },
                    &mut triples,
                )?;
            }

            for prop_id in &pset.properties {
                if let Some(psv) = model.property_single_values.get(prop_id) {
                    let Some(raw_value) = psv.nominal_value.as_ref().and_then(step_value_to_object) else {
                        continue;
                    };
                    property_counter += 1;
                    emit_property(
                        &base,
                        &subject,
                        &object_guid,
                        pset_name,
                        &pset_subject,
                        psv.name.as_str(),
                        raw_value,
                        model.schema,
                        &class_name_like,
                        index,
                        matching,
                        match_cache,
                        resolve_property_unit(psv, &unit_by_type, model),
                        &generated_at,
                        &mut unmatched_histogram,
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
                            &base,
                            &subject,
                            &object_guid,
                            pset_name,
                            &pset_subject,
                            pev.name.as_str(),
                            Object::Literal(enum_value.to_string()),
                            model.schema,
                            &class_name_like,
                            index,
                            matching,
                            match_cache,
                            None,
                            &generated_at,
                            &mut unmatched_histogram,
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

    let mut quantity_object_ids: Vec<_> = model.quantities_for_object.keys().copied().collect();
    quantity_object_ids.sort_unstable();

    for object_id in quantity_object_ids {
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

        let mut quantity_set_ids = model.quantities_for_object[&object_id].clone();
        quantity_set_ids.sort_unstable();

        for quantity_set_id in quantity_set_ids {
            let Some(quantity_set) = model.element_quantities.get(&quantity_set_id) else {
                continue;
            };
            let quantity_set_name = quantity_set.name.as_deref().unwrap_or_default();
            let quantity_set_subject = bsdd_local_instance(
                &base,
                "qset",
                &format!("{}_{}", sanitize(quantity_set_name), sanitize(&object_guid)),
            );

            push(
                &mut batch,
                sender,
                batch_size,
                Triple {
                    subject: subject.clone(),
                    predicate: bsddm("hasQuantitySet"),
                    object: Object::Iri(quantity_set_subject.clone()),
                },
                &mut triples,
            )?;
            push(
                &mut batch,
                sender,
                batch_size,
                Triple {
                    subject: quantity_set_subject.clone(),
                    predicate: rdf_type(),
                    object: Object::Iri(bsddm("QuantitySet")),
                },
                &mut triples,
            )?;
            if let Some(quantity_set_code) = index.resolve_class(quantity_set_name) {
                push(
                    &mut batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: quantity_set_subject.clone(),
                        predicate: rdf_type(),
                        object: Object::Iri(bsdd_class(quantity_set_code)),
                    },
                    &mut triples,
                )?;
            } else {
                push(
                    &mut batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: quantity_set_subject.clone(),
                        predicate: rdf_type(),
                        object: Object::Iri(bsddm("customQuantitySet")),
                    },
                    &mut triples,
                )?;
            }
            if !quantity_set_name.is_empty() {
                push(
                    &mut batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: quantity_set_subject.clone(),
                        predicate: rdfs_label(),
                        object: Object::Literal(quantity_set_name.to_string()),
                    },
                    &mut triples,
                )?;
            }

            for quantity_id in &quantity_set.quantities {
                let Some(quantity) = model.physical_quantities.get(quantity_id) else {
                    continue;
                };
                let Some(raw_value) = quantity.value.as_ref().and_then(step_value_to_object) else {
                    continue;
                };
                quantity_counter += 1;
                emit_property(
                    &base,
                    &subject,
                    &object_guid,
                    quantity_set_name,
                    &quantity_set_subject,
                    quantity.name.as_str(),
                    raw_value,
                    model.schema,
                    &class_name_like,
                    index,
                    matching,
                    match_cache,
                    resolve_quantity_unit(quantity.entity_name.as_str(), &unit_by_type),
                    &generated_at,
                    &mut unmatched_histogram,
                    &mut quantity_counter,
                    &mut batch,
                    sender,
                    batch_size,
                    &mut triples,
                )?;
            }
        }
    }

    if !batch.is_empty() {
        sender.send(batch).map_err(|_| StreamError::ChannelClosed)?;
    }
    if !unmatched_histogram.is_empty() {
        let mut top: Vec<_> = unmatched_histogram.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1));
        let preview = top
            .into_iter()
            .take(20)
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join(", ");
        info!("bSDD unmatched top20: {preview}");
    }
    Ok(triples)
}

#[allow(clippy::too_many_arguments)]
fn emit_property(
    base: &str,
    subject: &str,
    object_guid: &str,
    pset_name: &str,
    pset_subject: &str,
    prop_name: &str,
    value: Object,
    schema: StepSchema,
    class_name_like: &str,
    index: &BsddIndex,
    matching: &BsddMatchingConfig,
    match_cache: Option<&BsddMatchCache>,
    unit: Option<String>,
    generated_at: &str,
    unmatched_histogram: &mut HashMap<String, u64>,
    property_counter: &mut u64,
    batch: &mut Vec<Triple>,
    sender: &Sender<Vec<Triple>>,
    batch_size: usize,
    triples: &mut u64,
) -> Result<(), StreamError> {
    let match_result = if let Some(cache) = match_cache {
        resolve_from_cache(cache, schema, class_name_like, pset_name, prop_name, matching)
            .unwrap_or_else(|| index.resolve_property(schema, class_name_like, pset_name, prop_name, matching))
    } else {
        index.resolve_property(schema, class_name_like, pset_name, prop_name, matching)
    };
    if matches!(match_result.status, MatchStatus::Unmapped) {
        let key = format!("{pset_name}|{prop_name}");
        *unmatched_histogram.entry(key).or_insert(0) += 1;
    }
    let prop_subject = bsdd_local_instance(
        base,
        "property",
        &format!(
            "{}_{}_{}",
            sanitize(prop_name),
            sanitize(object_guid),
            property_counter
        ),
    );
    let state_subject = bsdd_local_instance(
        base,
        "state",
        &format!(
            "{}_{}_{}",
            sanitize(prop_name),
            sanitize(object_guid),
            property_counter
        ),
    );

    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: pset_subject.to_string(),
            predicate: bsddm("hasProperty"),
            object: Object::Iri(prop_subject.clone()),
        },
        triples,
    )?;

    if let Some(code) = match_result.property_code.as_deref() {
        push(
            batch,
            sender,
            batch_size,
            Triple {
                subject: subject.to_string(),
                predicate: bsdd_prop(code),
                object: Object::Iri(prop_subject.clone()),
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
            predicate: rdf_type(),
            object: Object::Iri(opm_property()),
        },
        triples,
    )?;
    if matches!(match_result.status, MatchStatus::Unmapped) {
        push(
            batch,
            sender,
            batch_size,
            Triple {
                subject: prop_subject.clone(),
                predicate: rdf_type(),
                object: Object::Iri(bsddm("customProperty")),
            },
            triples,
        )?;
    } else {
        push(
            batch,
            sender,
            batch_size,
            Triple {
                subject: prop_subject.clone(),
                predicate: rdf_type(),
                object: Object::Iri(bsddm("Property")),
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
    let _ = pset_name;
    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: prop_subject.clone(),
            predicate: bsddm("mappingStatus"),
            object: Object::Iri(mapping_status_iri(match_result.status)),
        },
        triples,
    )?;
    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: prop_subject.clone(),
            predicate: bsddm("matchingMethod"),
            object: Object::Literal(match_result.method.to_string()),
        },
        triples,
    )?;
    if let Some(confidence) = match_result.confidence {
        push(
            batch,
            sender,
            batch_size,
            Triple {
                subject: prop_subject.clone(),
                predicate: bsddm("matchingConfidence"),
                object: Object::TypedLiteral {
                    value: format!("{confidence:.4}"),
                    datatype: format!("{XSD}decimal"),
                },
            },
            triples,
        )?;
    }
    if let Some(meta) = match_result.exact_meta.as_ref() {
        if !meta.class_property_code.is_empty() {
            push(
                batch,
                sender,
                batch_size,
                Triple {
                    subject: prop_subject.clone(),
                    predicate: bsddm("classPropertyCode"),
                    object: Object::Literal(meta.class_property_code.clone()),
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
            subject: state_subject.clone(),
            predicate: prov_generated_at_time(),
            object: Object::TypedLiteral {
                value: generated_at.to_string(),
                datatype: format!("{XSD}dateTime"),
            },
        },
        triples,
    )?;
    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: state_subject.clone(),
            predicate: schema_value(),
            object: value,
        },
        triples,
    )?;
    if let Some(unit) = unit {
        push(
            batch,
            sender,
            batch_size,
            Triple {
                subject: state_subject,
                predicate: smls_unit(),
                object: Object::Iri(unit),
            },
            triples,
        )?;
    }

    Ok(())
}

fn sanitize(value: &str) -> String {
    let decoded = decode_ifc_unicode(value);
    let transliterated = transliterate_for_matching(&decoded);
    let mut out = String::with_capacity(transliterated.len());
    let mut last_was_underscore = false;
    for ch in transliterated.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_was_underscore = false;
        } else if !last_was_underscore {
            out.push('_');
            last_was_underscore = true;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "value".to_string()
    } else {
        trimmed.to_string()
    }
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
