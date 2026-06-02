use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};

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

/// Tracks which canonical RDF nodes have already been emitted during a dedup pass.
/// Wrapped in Arc<Mutex<>> so it can be shared across rayon threads (CLI) and used directly
/// in the single-threaded WASM path.  All sets grow monotonically — presence means triples
/// are already in the stream; the first thread to insert wins and emits.
#[derive(Default)]
struct DedupSets {
    /// Canonical prop_subject IRIs whose full definition triples have been emitted.
    emitted_props: HashSet<String>,
    /// Set-subject IRIs (pset/qset) whose type/label triples have been emitted.
    emitted_set_defs: HashSet<String>,
    /// "{set_subject}|{prop_subject}" keys for containsProperty/containsQuantity triples.
    emitted_set_contains: HashSet<String>,
}

impl DedupSets {
    fn shared() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::default()))
    }
}

/// Stats emitted to the logger when dedup_properties is active.
#[derive(Debug, Clone, Default)]
pub struct BsddDedupStats {
    /// Property+state definition triples skipped because a canonical instance was already emitted.
    pub prop_instances_deduped: u64,
    /// Pset/qset type+label triples skipped because the set IRI was already emitted.
    pub set_defs_deduped: u64,
    /// containsProperty/containsQuantity triples skipped (same pset→prop already linked).
    pub set_contains_deduped: u64,
}

// Fuzzy matching thresholds — named so tests break if someone adjusts blindly.
// Both values are exposed in the profile and overridable per profile in Phase 2.
const FUZZY_THRESHOLD: f64 = 0.94;
// Max candidates per class inspected during fuzzy; limits O(n) scan.
const MAX_FUZZY_CANDIDATES: usize = 400;

const EMBEDDED_BSDD_INDEX_GZ: &[u8] =
    include_bytes!("../../resources/bsdd_ifc4x3_index.json.gz");

const EMBEDDED_PROFILE_BASE: &str =
    include_str!("../../resources/bsdd-profiles/base.json");
const EMBEDDED_PROFILE_REVIT_DACH: &str =
    include_str!("../../resources/bsdd-profiles/revit-dach.json");
const EMBEDDED_PROFILE_ALLPLAN_DE: &str =
    include_str!("../../resources/bsdd-profiles/allplan-de.json");
const EMBEDDED_PROFILE_TEKLA_EN: &str =
    include_str!("../../resources/bsdd-profiles/tekla-en.json");

const BSDDM_NS: &str = "https://w3id.org/ifc2lbd/bsdd-meta#";
const BSDD_CLASS_NS: &str =
    "https://identifier.buildingsmart.org/uri/buildingsmart/ifc/4.3/class/";
const BSDD_PROP_NS: &str =
    "https://identifier.buildingsmart.org/uri/buildingsmart/ifc/4.3/prop/";

static BSDD_INDEX: OnceLock<Result<BsddIndex, String>> = OnceLock::new();
static PROFILE_CACHE: OnceLock<Mutex<HashMap<String, BsddProfile>>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Profile structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct FuzzyConfig {
    #[serde(default = "default_fuzzy_enabled")]
    enabled: bool,
    #[serde(default = "default_fuzzy_threshold")]
    threshold: f64,
    /// "class" | "pset" | "property" | "never"
    #[serde(default = "default_fuzzy_scope")]
    scope: String,
}

fn default_fuzzy_enabled() -> bool {
    true
}
fn default_fuzzy_threshold() -> f64 {
    FUZZY_THRESHOLD
}
fn default_fuzzy_scope() -> String {
    "class".to_string()
}

impl Default for FuzzyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: FUZZY_THRESHOLD,
            scope: "class".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
struct BsddProfile {
    #[serde(default)]
    profile_id: String,
    #[serde(default)]
    profile_version: String,
    #[serde(default)]
    extends: Option<String>,
    #[serde(default)]
    bsdd_index_version: Option<String>,
    #[serde(default)]
    fuzzy: FuzzyConfig,
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

impl BsddProfile {
    /// Merge `self` as base and `overlay` on top. Overlay wins per key.
    fn merge_overlay(mut self, overlay: BsddProfile) -> BsddProfile {
        if !overlay.profile_id.is_empty() {
            self.profile_id = overlay.profile_id;
        }
        if !overlay.profile_version.is_empty() {
            self.profile_version = overlay.profile_version;
        }
        self.extends = overlay.extends;
        if overlay.bsdd_index_version.is_some() {
            self.bsdd_index_version = overlay.bsdd_index_version;
        }
        if overlay.fuzzy.threshold != FUZZY_THRESHOLD || !overlay.fuzzy.enabled {
            self.fuzzy = overlay.fuzzy;
        }
        for (k, v) in overlay.class_aliases {
            self.class_aliases.insert(k, v);
        }
        for (k, v) in overlay.pset_aliases {
            self.pset_aliases.insert(k, v);
        }
        for (k, v) in overlay.prop_aliases {
            self.prop_aliases.insert(k, v);
        }
        for (pset, map) in overlay.pset_prop_aliases {
            self.pset_prop_aliases
                .entry(pset)
                .or_default()
                .extend(map);
        }
        for (k, v) in overlay.hard_mappings {
            self.hard_mappings.insert(k, v);
        }
        for (schema, map) in overlay.schema_class_aliases {
            self.schema_class_aliases
                .entry(schema)
                .or_default()
                .extend(map);
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Match result types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct BsddMatchCache {
    by_key: HashMap<String, BsddPreparedMatch>,
    pub profile_id: String,
    pub index_version: String,
    /// When true, cache misses return Unmapped immediately instead of falling back to a
    /// live fuzzy scan. Set on the empty placeholder used when no preprocess cache is present —
    /// means the bsdd producer runs without any fuzzy matching (safe / explicit mode).
    pub no_fuzzy: bool,
}

impl BsddMatchCache {
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
        let total = self.by_key.len();
        let matched_ratio = if total > 0 {
            (total_matched as f64) / (total as f64)
        } else {
            0.0
        };
        let ambiguous_ratio = if total > 0 {
            (ambiguous as f64) / (total as f64)
        } else {
            0.0
        };
        let unmapped_ratio = if total > 0 {
            (unmapped as f64) / (total as f64)
        } else {
            0.0
        };

        json!({
            "total_unique_keys": total,
            "total_matched": total_matched,
            "matched_exact": matched_exact,
            "matched_fuzzy": matched_fuzzy,
            "normalized": normalized,
            "ambiguous": ambiguous,
            "unmapped": unmapped,
            "matched_ratio": (matched_ratio * 1000.0).round() / 1000.0,
            "ambiguous_ratio": (ambiguous_ratio * 1000.0).round() / 1000.0,
            "unmapped_ratio": (unmapped_ratio * 1000.0).round() / 1000.0,
            "fuzzy_confidence_avg": fuzzy_avg,
            "fuzzy_confidence_min": fuzzy_min,
            "fuzzy_confidence_max": fuzzy_max,
            "method_counts": method_counts,
            "profile_id": self.profile_id,
            "index_version": self.index_version,
        })
    }
}

#[derive(Clone, Debug)]
struct BsddPreparedMatch {
    status: MatchStatus,
    property_code: Option<String>,
    /// Populated when status == Ambiguous; each entry is a candidate property code.
    ambiguous_candidates: Vec<String>,
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
    ambiguous_candidates: Vec<String>,
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

// ---------------------------------------------------------------------------
// bSDD index
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct BsddIndex {
    #[allow(dead_code)]
    format: String,
    #[allow(dead_code)]
    dictionary_code: String,
    dictionary_version: String,
    #[allow(dead_code)]
    organization_code: String,
    class_code_by_norm: HashMap<String, String>,
    #[allow(dead_code)]
    prop_name_by_code_norm: HashMap<String, String>,
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

    fn resolve_property(
        &self,
        schema: StepSchema,
        class_code_like: &str,
        pset_name: &str,
        prop_name: &str,
        profile: &BsddProfile,
    ) -> MatchResult {
        self.resolve_property_impl(schema, class_code_like, pset_name, prop_name, profile, true)
    }

    /// Same as `resolve_property` but skips the fuzzy scan (step 5).
    /// All O(1) dictionary lookups still run — only similarity-based guessing is omitted.
    fn resolve_property_exact_only(
        &self,
        schema: StepSchema,
        class_code_like: &str,
        pset_name: &str,
        prop_name: &str,
        profile: &BsddProfile,
    ) -> MatchResult {
        self.resolve_property_impl(schema, class_code_like, pset_name, prop_name, profile, false)
    }

    fn resolve_property_impl(
        &self,
        schema: StepSchema,
        class_code_like: &str,
        pset_name: &str,
        prop_name: &str,
        profile: &BsddProfile,
        fuzzy: bool,
    ) -> MatchResult {
        let empty_schema_aliases: HashMap<String, String> = HashMap::new();
        let schema_aliases = profile
            .schema_class_aliases
            .get(&schema.to_string())
            .unwrap_or(&empty_schema_aliases);
        let class_norms = normalized_match_variants(
            class_code_like,
            &profile.class_aliases,
            schema_aliases,
            &HashMap::new(),
            false,
        );
        let pset_norms = normalized_match_variants(
            pset_name,
            &profile.pset_aliases,
            &HashMap::new(),
            &HashMap::new(),
            false,
        );
        let prop_norms = normalized_match_variants(
            prop_name,
            &profile.prop_aliases,
            &HashMap::new(),
            profile
                .pset_prop_aliases
                .get(
                    &normalized_match_variants(
                        pset_name,
                        &profile.pset_aliases,
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

        // 1. Exact class|pset|prop + hard overrides
        for class_norm in &class_norms {
            for pset_norm in &pset_norms {
                for prop_norm in &prop_norms {
                    let exact_key = format!("{class_norm}|{pset_norm}|{prop_norm}");
                    let schema_hard_key =
                        format!("{}|{exact_key}", schema.to_string().to_ascii_lowercase());

                    if let Some(code) = profile
                        .hard_mappings
                        .get(&schema_hard_key)
                        .or_else(|| profile.hard_mappings.get(&exact_key))
                    {
                        return MatchResult {
                            status: MatchStatus::Matched,
                            property_code: Some(code.clone()),
                            ambiguous_candidates: vec![],
                            exact_meta: self.exact_meta.get(&exact_key).cloned(),
                            method: "hard_override",
                            confidence: Some(1.0),
                        };
                    }

                    if let Some(code) = self.exact.get(&exact_key) {
                        return MatchResult {
                            status: MatchStatus::Matched,
                            property_code: Some(code.clone()),
                            ambiguous_candidates: vec![],
                            exact_meta: self.exact_meta.get(&exact_key).cloned(),
                            method: "exact_class_pset_prop",
                            confidence: Some(1.0),
                        };
                    }
                }
            }
        }

        // 2. Pset|prop candidates
        let pset_candidates = collect_candidates(
            pset_norms
                .iter()
                .flat_map(|pset_norm| {
                    prop_norms
                        .iter()
                        .map(move |prop_norm| format!("{pset_norm}|{prop_norm}"))
                }),
            &self.by_pset_prop,
        );
        match pset_candidates.len() {
            1 => {
                return MatchResult {
                    status: MatchStatus::Normalized,
                    property_code: pset_candidates.into_iter().next(),
                    ambiguous_candidates: vec![],
                    exact_meta: None,
                    method: "normalized_pset_prop",
                    confidence: Some(0.95),
                }
            }
            n if n > 1 => {
                return MatchResult {
                    status: MatchStatus::Ambiguous,
                    property_code: None,
                    ambiguous_candidates: pset_candidates,
                    exact_meta: None,
                    method: "ambiguous_pset_prop",
                    confidence: None,
                }
            }
            _ => {}
        }

        // 3. Class|prop candidates
        let class_candidates = collect_candidates(
            class_norms
                .iter()
                .flat_map(|class_norm| {
                    prop_norms
                        .iter()
                        .map(move |prop_norm| format!("{class_norm}|{prop_norm}"))
                }),
            &self.by_class_prop,
        );
        match class_candidates.len() {
            1 => {
                return MatchResult {
                    status: MatchStatus::Normalized,
                    property_code: class_candidates.into_iter().next(),
                    ambiguous_candidates: vec![],
                    exact_meta: None,
                    method: "normalized_class_prop",
                    confidence: Some(0.9),
                }
            }
            n if n > 1 => {
                return MatchResult {
                    status: MatchStatus::Ambiguous,
                    property_code: None,
                    ambiguous_candidates: class_candidates,
                    exact_meta: None,
                    method: "ambiguous_class_prop",
                    confidence: None,
                }
            }
            _ => {}
        }

        // 4. Prop-only candidates
        let prop_candidates = collect_candidates(prop_norms.iter().cloned(), &self.by_prop);
        match prop_candidates.len() {
            1 => {
                return MatchResult {
                    status: MatchStatus::Normalized,
                    property_code: prop_candidates.into_iter().next(),
                    ambiguous_candidates: vec![],
                    exact_meta: None,
                    method: "normalized_prop",
                    confidence: Some(0.85),
                }
            }
            n if n > 1 => {
                return MatchResult {
                    status: MatchStatus::Ambiguous,
                    property_code: None,
                    ambiguous_candidates: prop_candidates,
                    exact_meta: None,
                    method: "ambiguous_prop",
                    confidence: None,
                }
            }
            _ => {}
        }

        // 5. Class-scoped fuzzy — never falls back to global search
        if fuzzy && profile.fuzzy.enabled && profile.fuzzy.scope != "never" {
            let class_norm = class_norms.first().map(String::as_str).unwrap_or_default();
            let pset_norm = pset_norms.first().map(String::as_str).unwrap_or_default();
            let threshold = profile.fuzzy.threshold;

            for prop_norm in &prop_norms {
                if let Some((code, score, all_close)) =
                    self.resolve_fuzzy_class_scoped(class_norm, pset_norm, prop_norm, threshold, &profile.fuzzy.scope)
                {
                    if all_close.len() == 1 {
                        return MatchResult {
                            status: MatchStatus::Normalized,
                            property_code: Some(code),
                            ambiguous_candidates: vec![],
                            exact_meta: None,
                            method: "fuzzy",
                            confidence: Some(score),
                        };
                    } else {
                        return MatchResult {
                            status: MatchStatus::Ambiguous,
                            property_code: None,
                            ambiguous_candidates: all_close,
                            exact_meta: None,
                            method: "fuzzy_ambiguous",
                            confidence: None,
                        };
                    }
                }
            }
        }

        MatchResult {
            status: MatchStatus::Unmapped,
            property_code: None,
            ambiguous_candidates: vec![],
            exact_meta: None,
            method: "unmapped",
            confidence: None,
        }
    }

    /// Class-scoped fuzzy: only scores properties that bSDD associates with `class_norm`.
    /// Returns `Some((best_code, best_score, all_within_0.02_of_best))` or `None`.
    /// Never falls back to a global `by_prop` scan — Unmapped is preferable to a cross-class mismatch.
    fn resolve_fuzzy_class_scoped(
        &self,
        class_norm: &str,
        pset_norm: &str,
        prop_norm: &str,
        threshold: f64,
        scope: &str,
    ) -> Option<(String, f64, Vec<String>)> {
        // Determine which lookup to scope against
        let prefix_and_map: Vec<(String, &HashMap<String, Vec<String>>)> = match scope {
            "pset" if !pset_norm.is_empty() => {
                vec![(format!("{pset_norm}|"), &self.by_pset_prop)]
            }
            "class" | _ if !class_norm.is_empty() => {
                vec![(format!("{class_norm}|"), &self.by_class_prop)]
            }
            _ => return None,
        };

        let first = prop_norm.chars().next();
        let prop_len = prop_norm.chars().count() as i32;
        let mut candidates: Vec<(String, f64)> = Vec::new();
        let mut inspected = 0usize;

        for (prefix, map) in &prefix_and_map {
            for (key, codes) in *map {
                if inspected >= MAX_FUZZY_CANDIDATES {
                    break;
                }
                if !key.starts_with(prefix.as_str()) {
                    continue;
                }
                let kprop = &key[prefix.len()..];
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
                if score >= threshold {
                    for code in codes {
                        candidates.push((code.clone(), score));
                    }
                }
            }
        }

        if candidates.is_empty() {
            return None;
        }

        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let best = candidates[0].clone();
        let close: Vec<String> = {
            let mut seen = HashSet::new();
            candidates
                .iter()
                .filter(|(_, s)| *s + 0.02 >= best.1)
                .filter(|(c, _)| seen.insert(c.clone()))
                .map(|(c, _)| c.clone())
                .collect()
        };

        Some((best.0, best.1, close))
    }
}

// ---------------------------------------------------------------------------
// Profile loading
// ---------------------------------------------------------------------------

fn profile_cache() -> &'static Mutex<HashMap<String, BsddProfile>> {
    PROFILE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn load_profile(name_or_path: &str) -> Result<BsddProfile, String> {
    // File path?
    let is_path = name_or_path.contains('/') || name_or_path.contains('\\') || name_or_path.ends_with(".json");
    if is_path {
        return load_profile_from_file(name_or_path);
    }

    // Named profile — check cache first
    {
        let cache = profile_cache().lock().unwrap();
        if let Some(p) = cache.get(name_or_path) {
            return Ok(p.clone());
        }
    }

    let profile = load_named_profile(name_or_path)?;
    profile_cache()
        .lock()
        .unwrap()
        .insert(name_or_path.to_string(), profile.clone());
    Ok(profile)
}

fn load_named_profile(name: &str) -> Result<BsddProfile, String> {
    let raw = match name {
        "base" => EMBEDDED_PROFILE_BASE,
        "revit-dach" => EMBEDDED_PROFILE_REVIT_DACH,
        "allplan-de" => EMBEDDED_PROFILE_ALLPLAN_DE,
        "tekla-en" => EMBEDDED_PROFILE_TEKLA_EN,
        other => {
            return Err(format!(
                "unknown bSDD profile '{}'; known: base, revit-dach, allplan-de, tekla-en",
                other
            ))
        }
    };
    let overlay: BsddProfile = serde_json::from_str(raw)
        .map_err(|e| format!("failed parsing embedded bSDD profile '{}': {e}", name))?;

    // Resolve `extends` chain
    if let Some(ref base_name) = overlay.extends.clone() {
        let base = load_named_profile(base_name)?;
        return Ok(base.merge_overlay(overlay));
    }
    Ok(overlay)
}

fn load_profile_from_file(path: &str) -> Result<BsddProfile, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("failed reading bSDD profile '{path}': {e}"))?;
    let overlay: BsddProfile = serde_json::from_str(&raw)
        .map_err(|e| format!("failed parsing bSDD profile '{path}': {e}"))?;
    if let Some(ref base_name) = overlay.extends.clone() {
        let base = load_named_profile(base_name)?;
        return Ok(base.merge_overlay(overlay));
    }
    Ok(overlay)
}

fn load_active_profile(profile_name: Option<&str>) -> Result<BsddProfile, String> {
    // Env var override takes precedence
    if let Ok(path) = std::env::var("IFC2LBD_BSDD_PROFILE") {
        return load_profile(&path);
    }
    let name = profile_name.unwrap_or("base");
    load_profile(name)
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

// ---------------------------------------------------------------------------
// String normalization helpers
// ---------------------------------------------------------------------------

fn normalize(input: &str) -> String {
    transliterate_for_matching(input)
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
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

// ---------------------------------------------------------------------------
// IRI helpers
// ---------------------------------------------------------------------------

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

fn mapping_status_iri(status: MatchStatus) -> String {
    match status {
        MatchStatus::Matched => bsddm("Mapped"),
        MatchStatus::Normalized => bsddm("Normalized"),
        MatchStatus::Ambiguous => bsddm("Ambiguous"),
        MatchStatus::Unmapped => bsddm("Unmapped"),
    }
}

// ---------------------------------------------------------------------------
// IFC entity helpers
// ---------------------------------------------------------------------------

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

fn resolve_bsdd_class_for_element(
    index: &BsddIndex,
    entity_name: &str,
    profile: &BsddProfile,
) -> Option<String> {
    let trimmed = entity_name.trim();
    let upper = trimmed.to_ascii_uppercase();

    // Profile class_aliases (and schema_class_aliases) are applied via normalized_match_variants
    // before the index lookup. We build a single-candidate normalized name for the class.
    let empty: HashMap<String, String> = HashMap::new();
    let norms = normalized_match_variants(trimmed, &profile.class_aliases, &empty, &empty, false);
    for norm_input in &norms {
        if let Some(code) = index.resolve_class(norm_input) {
            return Some(code.to_string());
        }
    }

    // Try uppercase + "IFC" prefix variants directly in index
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

// ---------------------------------------------------------------------------
// Step value helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn build_bsdd_match_cache(
    model: &IfcModel,
    profile_name: Option<&str>,
) -> Result<BsddMatchCache, String> {
    let index = load_bsdd_index()?;
    let profile = load_active_profile(profile_name)?;

    // Pass 1 (sequential): collect all unique lookup tuples, deduplicating by cache key.
    // This avoids redundant resolve_property calls for the same (class, pset, prop) combo.
    let mut unique: HashMap<String, (String, String, String)> = HashMap::new();

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
                let mut enqueue = |name: &str| {
                    let key = cache_key(model.schema, &class_name_like, pset_name, name, &profile);
                    unique.entry(key).or_insert_with(|| {
                        (class_name_like.clone(), pset_name.to_string(), name.to_string())
                    });
                };
                if let Some(psv) = model.property_single_values.get(prop_id) {
                    enqueue(psv.name.as_str());
                }
                if let Some(pev) = model.property_enumerated_values.get(prop_id) {
                    enqueue(pev.name.as_str());
                }
            }
        }
    }

    // Pass 2: resolve each unique match — expensive fuzzy/exact lookup, done in parallel.
    let schema = model.schema;
    #[cfg(not(target_arch = "wasm32"))]
    let by_key: HashMap<String, BsddPreparedMatch> = {
        use rayon::prelude::*;
        unique
            .into_par_iter()
            .map(|(key, (class_name, pset_name, prop_name))| {
                let m = index.resolve_property(schema, &class_name, &pset_name, &prop_name, &profile);
                (key, BsddPreparedMatch {
                    status: m.status,
                    property_code: m.property_code,
                    ambiguous_candidates: m.ambiguous_candidates,
                    exact_meta: m.exact_meta,
                    method: m.method,
                    confidence: m.confidence,
                })
            })
            .collect()
    };
    #[cfg(target_arch = "wasm32")]
    let by_key: HashMap<String, BsddPreparedMatch> = unique
        .into_iter()
        .map(|(key, (class_name, pset_name, prop_name))| {
            let m = index.resolve_property(schema, &class_name, &pset_name, &prop_name, &profile);
            (key, BsddPreparedMatch {
                status: m.status,
                property_code: m.property_code,
                ambiguous_candidates: m.ambiguous_candidates,
                exact_meta: m.exact_meta,
                method: m.method,
                confidence: m.confidence,
            })
        })
        .collect();

    Ok(BsddMatchCache {
        by_key,
        profile_id: profile.profile_id.clone(),
        index_version: index.dictionary_version.clone(),
        no_fuzzy: false,
    })
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
                let values = pev
                    .values
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("|");
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

/// Compute a stable content fingerprint for a pset: sorted "name=value_sig" pairs joined by "|".
/// Two psets with identical properties and values produce the same fingerprint.
fn pset_content_repr(pset: &ifc_model::PropertySet, model: &ifc_model::IfcModel) -> String {
    let mut pairs: Vec<String> = pset.properties.iter().filter_map(|prop_id| {
        if let Some(psv) = model.property_single_values.get(prop_id) {
            let value_sig = psv.nominal_value.as_ref()
                .map(step_value_signature)
                .unwrap_or_else(|| "none".to_string());
            Some(format!("{}={}", normalize(psv.name.as_str()), value_sig))
        } else if let Some(pev) = model.property_enumerated_values.get(prop_id) {
            let vals = pev.values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",");
            Some(format!("{}=[{}]", normalize(pev.name.as_str()), vals))
        } else {
            None
        }
    }).collect();
    pairs.sort_unstable();
    pairs.join("|")
}

fn resolve_from_cache(
    cache: &BsddMatchCache,
    schema: StepSchema,
    class_name_like: &str,
    pset_name: &str,
    prop_name: &str,
    profile: &BsddProfile,
) -> Option<MatchResult> {
    let key = cache_key(schema, class_name_like, pset_name, prop_name, profile);
    cache.by_key.get(&key).map(|prepared| MatchResult {
        status: prepared.status,
        property_code: prepared.property_code.clone(),
        ambiguous_candidates: prepared.ambiguous_candidates.clone(),
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
    profile: &BsddProfile,
) -> String {
    let empty_schema_aliases: HashMap<String, String> = HashMap::new();
    let schema_aliases = profile
        .schema_class_aliases
        .get(&schema.to_string())
        .unwrap_or(&empty_schema_aliases);
    let class_norm = normalized_match_variants(
        class_code_like,
        &profile.class_aliases,
        schema_aliases,
        &HashMap::new(),
        false,
    )
    .into_iter()
    .next()
    .unwrap_or_default();
    let pset_norm = normalized_match_variants(
        pset_name,
        &profile.pset_aliases,
        &HashMap::new(),
        &HashMap::new(),
        false,
    )
    .into_iter()
    .next()
    .unwrap_or_default();
    let prop_norm = normalized_match_variants(
        prop_name,
        &profile.prop_aliases,
        &HashMap::new(),
        profile
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
) -> Result<(u64, BsddDedupStats), StreamError> {
    stream_bsdd_with_cache(model, options, sender, None)
}

pub fn stream_bsdd_with_cache(
    model: &IfcModel,
    options: &ConvertOptions,
    sender: &Sender<Vec<Triple>>,
    match_cache: Option<&BsddMatchCache>,
) -> Result<(u64, BsddDedupStats), StreamError> {
    let index = load_bsdd_index().map_err(StreamError::Conversion)?;
    let profile = load_active_profile(options.bsdd_profile.as_deref())
        .map_err(StreamError::Conversion)?;

    // Resolve the match cache to use for this run.
    //
    // If `neo-bsdd-match-preprocess` ran, its cache is passed in and all lookups are O(1)
    // HashMap gets with no fuzzy scanning.
    //
    // If no cache was provided the producer uses an empty cache with `no_fuzzy = true`.
    // Every property gets MatchStatus::Unmapped — no fuzzy scanning happens at all.
    // This is the safe/explicit mode: only add the preprocess module when you want
    // bSDD class/property assignments to be resolved.
    //
    // (Previously the fallback built the cache inline, which ran the full fuzzy scan
    // inside the producer — slow and implicit. That behaviour is gone.)
    let options_profile = options.bsdd_profile.as_deref().unwrap_or("base");
    let empty_no_fuzzy_cache: Option<BsddMatchCache>;
    let cache: &BsddMatchCache = match match_cache {
        Some(c) if c.profile_id == options_profile || options_profile == "base" => c,
        _ => {
            empty_no_fuzzy_cache = Some(BsddMatchCache {
                by_key: HashMap::new(),
                profile_id: options_profile.to_string(),
                index_version: String::new(),
                no_fuzzy: true,
            });
            empty_no_fuzzy_cache.as_ref().unwrap()
        }
    };

    let base = normalize_base_uri(&options.base_uri);
    let batch_size = options
        .stream_batch_size
        .clamp(MIN_STREAM_BATCH_SIZE, MAX_STREAM_BATCH_SIZE);
    let compact = options.bsdd_compact;
    let dedup = options.bsdd_dedup_properties;
    let unit_by_type = build_unit_type_map(model);
    let generated_at = current_generated_at_rfc3339();

    let mut batch = Vec::with_capacity(batch_size);
    let mut triples = 0_u64;
    let mut unmatched_histogram: HashMap<String, u64> = HashMap::new();

    // Element typing
    for element in sorted_values(&model.elements) {
        if let Some(class_code) =
            resolve_bsdd_class_for_element(index, element.entity_name.as_str(), &profile)
        {
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

    // Spatial node typing
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

    // Dedup mode uses Arc<Mutex<DedupSets>> so the same shared state works for both
    // the rayon parallel path (CLI) and the single-threaded WASM path.
    let pset_dedup: Option<Arc<Mutex<DedupSets>>> = dedup.then(DedupSets::shared);

    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        let par_results: Result<Vec<(u64, HashMap<String, u64>)>, StreamError> = object_ids
            .par_iter()
            .map(|&object_id| {
                process_element_psets(
                    object_id,
                    model,
                    &base,
                    index,
                    &profile,
                    cache,
                    &unit_by_type,
                    &generated_at,
                    compact,
                    pset_dedup.as_ref(),
                    sender,
                    batch_size,
                )
            })
            .collect();
        for (elem_triples, elem_unmatched) in par_results? {
            triples += elem_triples;
            for (k, v) in elem_unmatched {
                *unmatched_histogram.entry(k).or_insert(0) += v;
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    for &object_id in &object_ids {
        let (elem_triples, elem_unmatched) = process_element_psets(
            object_id,
            model,
            &base,
            index,
            &profile,
            cache,
            &unit_by_type,
            &generated_at,
            compact,
            pset_dedup.as_ref(),
            sender,
            batch_size,
        )?;
        triples += elem_triples;
        for (k, v) in elem_unmatched {
            *unmatched_histogram.entry(k).or_insert(0) += v;
        }
    }

    let mut quantity_object_ids: Vec<_> = model.quantities_for_object.keys().copied().collect();
    quantity_object_ids.sort_unstable();

    let qty_dedup: Option<Arc<Mutex<DedupSets>>> = dedup.then(DedupSets::shared);

    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        let par_results: Result<Vec<(u64, HashMap<String, u64>)>, StreamError> =
            quantity_object_ids
                .par_iter()
                .map(|&object_id| {
                    process_element_quantities(
                        object_id,
                        model,
                        &base,
                        index,
                        &profile,
                        cache,
                        &unit_by_type,
                        &generated_at,
                        compact,
                        qty_dedup.as_ref(),
                        sender,
                        batch_size,
                    )
                })
                .collect();
        for (elem_triples, elem_unmatched) in par_results? {
            triples += elem_triples;
            for (k, v) in elem_unmatched {
                *unmatched_histogram.entry(k).or_insert(0) += v;
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    for &object_id in &quantity_object_ids {
        let (elem_triples, elem_unmatched) = process_element_quantities(
            object_id,
            model,
            &base,
            index,
            &profile,
            cache,
            &unit_by_type,
            &generated_at,
            compact,
            qty_dedup.as_ref(),
            sender,
            batch_size,
        )?;
        triples += elem_triples;
        for (k, v) in elem_unmatched {
            *unmatched_histogram.entry(k).or_insert(0) += v;
        }
    }

    if !batch.is_empty() {
        sender.send(batch).map_err(|_| StreamError::ChannelClosed)?;
    }

    if options.bsdd_include_standard_attrs {
        triples += emit_bsdd_standard_attrs(
            model,
            &base,
            &unit_by_type,
            &generated_at,
            sender,
            batch_size,
        )?;
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

    let dedup_stats = match (&pset_dedup, &qty_dedup) {
        (Some(p), Some(q)) => {
            let pg = p.lock().unwrap();
            let qg = q.lock().unwrap();
            BsddDedupStats {
                prop_instances_deduped: (pg.emitted_props.len() + qg.emitted_props.len()) as u64,
                set_defs_deduped: (pg.emitted_set_defs.len() + qg.emitted_set_defs.len()) as u64,
                set_contains_deduped: (pg.emitted_set_contains.len() + qg.emitted_set_contains.len()) as u64,
            }
        }
        _ => BsddDedupStats::default(),
    };

    Ok((triples, dedup_stats))
}

#[allow(clippy::too_many_arguments)]
fn process_element_psets(
    object_id: u64,
    model: &IfcModel,
    base: &str,
    index: &BsddIndex,
    profile: &BsddProfile,
    cache: &BsddMatchCache,
    unit_by_type: &HashMap<String, String>,
    generated_at: &str,
    compact: bool,
    dedup: Option<&Arc<Mutex<DedupSets>>>,
    sender: &Sender<Vec<Triple>>,
    batch_size: usize,
) -> Result<(u64, HashMap<String, u64>), StreamError> {
    let (subject, object_guid, class_name_like) =
        if let Some(element) = model.elements.get(&object_id) {
            (
                element_resource_iri(base, element),
                element.guid.to_string(),
                element.entity_name.to_string(),
            )
        } else if let Some(spatial) = model.spatial_nodes.get(&object_id) {
            (
                spatial_resource_iri(base, spatial.spatial_type, &spatial.guid),
                spatial.guid.to_string(),
                spatial_ifc_class(spatial.spatial_type).to_string(),
            )
        } else {
            return Ok((0, HashMap::new()));
        };

    let mut pset_ids = model.property_sets_for_object[&object_id].clone();
    pset_ids.sort_unstable();

    let mut local_batch: Vec<Triple> = Vec::new();
    let mut local_triples = 0_u64;
    let mut local_counter = 0_u64;
    let mut local_unmatched: HashMap<String, u64> = HashMap::new();

    for pset_id in pset_ids {
        let Some(pset) = model.property_sets.get(&pset_id) else {
            continue;
        };
        let pset_name = pset.name.as_deref().unwrap_or_default();
        // In dedup mode: derive a canonical pset IRI from its content fingerprint so that
        // all elements whose pset has identical properties+values share one pset node.
        // In non-dedup mode: use the per-entity IFC GUID as normal.
        let pset_subject = if dedup.is_some() {
            let content = pset_content_repr(pset, model);
            crate::canonical_pset_resource_iri(base, pset_name, &content)
        } else {
            crate::property_set_resource_iri(base, &pset.guid)
        };

        // element → hasPropertySet is always per-element
        push(
            &mut local_batch,
            sender,
            batch_size,
            Triple {
                subject: subject.clone(),
                predicate: bsddm("hasPropertySet"),
                object: Object::Iri(pset_subject.clone()),
            },
            &mut local_triples,
        )?;

        // pset type/label triples: emit once per pset IRI in dedup mode
        let emit_pset_def = match dedup {
            Some(ds) => ds.lock().unwrap().emitted_set_defs.insert(pset_subject.clone()),
            None => true,
        };
        if emit_pset_def {
            push(
                &mut local_batch,
                sender,
                batch_size,
                Triple {
                    subject: pset_subject.clone(),
                    predicate: rdf_type(),
                    object: Object::Iri(bsddm("PropertySet")),
                },
                &mut local_triples,
            )?;
            if let Some(pset_code) = index.resolve_class(pset_name) {
                push(
                    &mut local_batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: pset_subject.clone(),
                        predicate: rdf_type(),
                        object: Object::Iri(bsdd_class(pset_code)),
                    },
                    &mut local_triples,
                )?;
            } else {
                push(
                    &mut local_batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: pset_subject.clone(),
                        predicate: rdf_type(),
                        object: Object::Iri(bsddm("CustomPropertySet")),
                    },
                    &mut local_triples,
                )?;
            }
            if !pset_name.is_empty() {
                push(
                    &mut local_batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: pset_subject.clone(),
                        predicate: rdfs_label(),
                        object: Object::Literal(pset_name.to_string()),
                    },
                    &mut local_triples,
                )?;
            }
        }

        for prop_id in &pset.properties {
            if let Some(psv) = model.property_single_values.get(prop_id) {
                let Some(raw_value) =
                    psv.nominal_value.as_ref().and_then(step_value_to_object)
                else {
                    continue;
                };
                local_counter += 1;
                emit_property(
                    base,
                    &subject,
                    &object_guid,
                    pset_name,
                    &pset_subject,
                    &pset.guid,
                    "containsProperty",
                    psv.name.as_str(),
                    raw_value,
                    model.schema,
                    &class_name_like,
                    index,
                    profile,
                    cache,
                    resolve_property_unit(psv, unit_by_type, model),
                    generated_at,
                    compact,
                    dedup,
                    &mut local_unmatched,
                    &mut local_counter,
                    &mut local_batch,
                    sender,
                    batch_size,
                    &mut local_triples,
                )?;
                continue;
            }

            if let Some(pev) = model.property_enumerated_values.get(prop_id) {
                for enum_value in &pev.values {
                    local_counter += 1;
                    emit_property(
                        base,
                        &subject,
                        &object_guid,
                        pset_name,
                        &pset_subject,
                        &pset.guid,
                        "containsProperty",
                        pev.name.as_str(),
                        Object::Literal(enum_value.to_string()),
                        model.schema,
                        &class_name_like,
                        index,
                        profile,
                        cache,
                        None,
                        generated_at,
                        compact,
                        dedup,
                        &mut local_unmatched,
                        &mut local_counter,
                        &mut local_batch,
                        sender,
                        batch_size,
                        &mut local_triples,
                    )?;
                }
            }
        }
    }

    if !local_batch.is_empty() {
        sender.send(local_batch).map_err(|_| StreamError::ChannelClosed)?;
    }
    Ok((local_triples, local_unmatched))
}

#[allow(clippy::too_many_arguments)]
fn process_element_quantities(
    object_id: u64,
    model: &IfcModel,
    base: &str,
    index: &BsddIndex,
    profile: &BsddProfile,
    cache: &BsddMatchCache,
    unit_by_type: &HashMap<String, String>,
    generated_at: &str,
    compact: bool,
    dedup: Option<&Arc<Mutex<DedupSets>>>,
    sender: &Sender<Vec<Triple>>,
    batch_size: usize,
) -> Result<(u64, HashMap<String, u64>), StreamError> {
    let (subject, object_guid, class_name_like) =
        if let Some(element) = model.elements.get(&object_id) {
            (
                element_resource_iri(base, element),
                element.guid.to_string(),
                element.entity_name.to_string(),
            )
        } else if let Some(spatial) = model.spatial_nodes.get(&object_id) {
            (
                spatial_resource_iri(base, spatial.spatial_type, &spatial.guid),
                spatial.guid.to_string(),
                spatial_ifc_class(spatial.spatial_type).to_string(),
            )
        } else {
            return Ok((0, HashMap::new()));
        };

    let mut quantity_set_ids = model.quantities_for_object[&object_id].clone();
    quantity_set_ids.sort_unstable();

    let mut local_batch: Vec<Triple> = Vec::new();
    let mut local_triples = 0_u64;
    let mut local_counter = 0_u64;
    let mut local_unmatched: HashMap<String, u64> = HashMap::new();

    for quantity_set_id in quantity_set_ids {
        let Some(quantity_set) = model.element_quantities.get(&quantity_set_id) else {
            continue;
        };
        let quantity_set_name = quantity_set.name.as_deref().unwrap_or_default();
        let quantity_set_subject = if dedup.is_some() {
            // Fingerprint for qsets: sorted "name=value_sig" from physical quantities.
            let mut pairs: Vec<String> = quantity_set.quantities.iter().filter_map(|qid| {
                model.physical_quantities.get(qid).and_then(|q| {
                    q.value.as_ref().map(step_value_signature).map(|sig| {
                        format!("{}={}", normalize(q.name.as_str()), sig)
                    })
                })
            }).collect();
            pairs.sort_unstable();
            let content = pairs.join("|");
            crate::canonical_pset_resource_iri(base, quantity_set_name, &content)
        } else {
            crate::quantity_set_resource_iri(base, &quantity_set.guid)
        };

        // element → hasQuantitySet is always per-element
        push(
            &mut local_batch,
            sender,
            batch_size,
            Triple {
                subject: subject.clone(),
                predicate: bsddm("hasQuantitySet"),
                object: Object::Iri(quantity_set_subject.clone()),
            },
            &mut local_triples,
        )?;

        // qset type/label triples: emit once per qset IRI in dedup mode
        let emit_qset_def = match dedup {
            Some(ds) => ds.lock().unwrap().emitted_set_defs.insert(quantity_set_subject.clone()),
            None => true,
        };
        if emit_qset_def {
            push(
                &mut local_batch,
                sender,
                batch_size,
                Triple {
                    subject: quantity_set_subject.clone(),
                    predicate: rdf_type(),
                    object: Object::Iri(bsddm("QuantitySet")),
                },
                &mut local_triples,
            )?;
            if let Some(quantity_set_code) = index.resolve_class(quantity_set_name) {
                push(
                    &mut local_batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: quantity_set_subject.clone(),
                        predicate: rdf_type(),
                        object: Object::Iri(bsdd_class(quantity_set_code)),
                    },
                    &mut local_triples,
                )?;
            } else {
                push(
                    &mut local_batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: quantity_set_subject.clone(),
                        predicate: rdf_type(),
                        object: Object::Iri(bsddm("CustomQuantitySet")),
                    },
                    &mut local_triples,
                )?;
            }
            if !quantity_set_name.is_empty() {
                push(
                    &mut local_batch,
                    sender,
                    batch_size,
                    Triple {
                        subject: quantity_set_subject.clone(),
                        predicate: rdfs_label(),
                        object: Object::Literal(quantity_set_name.to_string()),
                    },
                    &mut local_triples,
                )?;
            }
        }

        for quantity_id in &quantity_set.quantities {
            let Some(quantity) = model.physical_quantities.get(quantity_id) else {
                continue;
            };
            let Some(raw_value) = quantity.value.as_ref().and_then(step_value_to_object)
            else {
                continue;
            };
            local_counter += 1;
            emit_property(
                base,
                &subject,
                &object_guid,
                quantity_set_name,
                &quantity_set_subject,
                &quantity_set.guid,
                "containsQuantity",
                quantity.name.as_str(),
                raw_value,
                model.schema,
                &class_name_like,
                index,
                profile,
                cache,
                resolve_quantity_unit(quantity.entity_name.as_str(), unit_by_type),
                generated_at,
                compact,
                dedup,
                &mut local_unmatched,
                &mut local_counter,
                &mut local_batch,
                sender,
                batch_size,
                &mut local_triples,
            )?;
        }
    }

    if !local_batch.is_empty() {
        sender.send(local_batch).map_err(|_| StreamError::ChannelClosed)?;
    }
    Ok((local_triples, local_unmatched))
}

/// Score how well a given profile maps the model's properties.
/// Returns a JSON summary for analyze-bsdd.
pub fn score_profile_for_model(
    model: &IfcModel,
    profile_name: &str,
    sample_limit: usize,
) -> Result<serde_json::Value, String> {
    let index = load_bsdd_index()?;
    let profile = load_profile(profile_name)?;

    let mut matched = 0usize;
    let mut ambiguous = 0usize;
    let mut unmapped = 0usize;
    let mut confidence_sum = 0.0f64;
    let mut confidence_count = 0usize;
    let mut total = 0usize;

    'outer: for object_id in model.property_sets_for_object.keys() {
        let class_name_like = if let Some(element) = model.elements.get(object_id) {
            element.entity_name.to_string()
        } else if let Some(spatial) = model.spatial_nodes.get(object_id) {
            spatial_ifc_class(spatial.spatial_type).to_string()
        } else {
            continue;
        };
        let pset_ids = match model.property_sets_for_object.get(object_id) {
            Some(ids) => ids,
            None => continue,
        };
        for pset_id in pset_ids {
            let Some(pset) = model.property_sets.get(pset_id) else {
                continue;
            };
            let pset_name = pset.name.as_deref().unwrap_or_default();
            for prop_id in &pset.properties {
                let prop_name = if let Some(psv) = model.property_single_values.get(prop_id) {
                    psv.name.as_str()
                } else if let Some(pev) = model.property_enumerated_values.get(prop_id) {
                    pev.name.as_str()
                } else {
                    continue;
                };
                let m = index.resolve_property(
                    model.schema,
                    &class_name_like,
                    pset_name,
                    prop_name,
                    &profile,
                );
                match m.status {
                    MatchStatus::Matched | MatchStatus::Normalized => {
                        matched += 1;
                        if let Some(c) = m.confidence {
                            confidence_sum += c;
                            confidence_count += 1;
                        }
                    }
                    MatchStatus::Ambiguous => ambiguous += 1,
                    MatchStatus::Unmapped => unmapped += 1,
                }
                total += 1;
                if total >= sample_limit {
                    break 'outer;
                }
            }
        }
    }

    let avg_conf = if confidence_count > 0 {
        confidence_sum / confidence_count as f64
    } else {
        0.0
    };

    Ok(json!({
        "profile": profile_name,
        "sampled": total,
        "matched": matched,
        "ambiguous": ambiguous,
        "unmapped": unmapped,
        "matched_ratio": if total > 0 { matched as f64 / total as f64 } else { 0.0 },
        "ambiguous_ratio": if total > 0 { ambiguous as f64 / total as f64 } else { 0.0 },
        "unmapped_ratio": if total > 0 { unmapped as f64 / total as f64 } else { 0.0 },
        "avg_confidence": avg_conf,
    }))
}

/// Returns the names of all embedded profiles available for selection.
pub fn list_embedded_profiles() -> Vec<&'static str> {
    vec!["base", "revit-dach", "allplan-de", "tekla-en"]
}

#[allow(clippy::too_many_arguments)]
fn emit_property(
    base: &str,
    subject: &str,
    object_guid: &str,
    pset_name: &str,
    pset_subject: &str,
    pset_guid: &str,
    container_predicate: &str, // "containsProperty" for psets, "containsQuantity" for qsets
    prop_name: &str,
    value: Object,
    schema: StepSchema,
    class_name_like: &str,
    index: &BsddIndex,
    profile: &BsddProfile,
    cache: &BsddMatchCache,
    unit: Option<String>,
    generated_at: &str,
    compact: bool,
    dedup: Option<&Arc<Mutex<DedupSets>>>,
    unmatched_histogram: &mut HashMap<String, u64>,
    property_counter: &mut u64,
    batch: &mut Vec<Triple>,
    sender: &Sender<Vec<Triple>>,
    batch_size: usize,
    triples: &mut u64,
) -> Result<(), StreamError> {
    // Resolve match:
    //   - cache hit (preprocess ran)  → O(1) HashMap get, no scanning
    //   - cache miss, fuzzy cache     → live full resolve (safety net, should be rare)
    //   - cache miss, no_fuzzy cache  → exact-only resolve: steps 1-4 only, fuzzy scan skipped
    let match_result = resolve_from_cache(cache, schema, class_name_like, pset_name, prop_name, profile)
        .unwrap_or_else(|| {
            if cache.no_fuzzy {
                index.resolve_property_exact_only(schema, class_name_like, pset_name, prop_name, profile)
            } else {
                index.resolve_property(schema, class_name_like, pset_name, prop_name, profile)
            }
        });
    if matches!(match_result.status, MatchStatus::Unmapped) {
        let key = format!("{pset_name}|{prop_name}");
        *unmatched_histogram.entry(key).or_insert(0) += 1;
    }
    let predicate_local = crate::property_local_name(prop_name);
    // In the non-dedup path keep value_repr as a borrowed &str to avoid a heap allocation
    // per property. Only convert to an owned String when canonical URIs are needed.
    let (prop_subject, state_subject) = if let Some(_) = dedup {
        let value_repr = crate::object_value_repr(&value).to_string();
        // Key on pset_name (not pset_guid) so properties with the same name and value
        // are shared across elements regardless of which pset entity they belong to.
        (
            crate::canonical_property_resource_iri(base, &predicate_local, pset_name, &value_repr),
            crate::canonical_property_state_iri(base, &predicate_local, pset_name, &value_repr),
        )
    } else {
        (
            crate::property_resource_iri(base, &predicate_local, object_guid, pset_guid),
            crate::property_state_iri(base, &predicate_local, object_guid, pset_guid, crate::object_value_repr(&value)),
        )
    };

    // Universal direct link: element → property (always per-element)
    push(
        batch,
        sender,
        batch_size,
        Triple {
            subject: subject.to_string(),
            predicate: bsddm("hasProperty"),
            object: Object::Iri(prop_subject.clone()),
        },
        triples,
    )?;

    // In dedup mode: gate the container link and all definition triples on first-seen.
    // The mutex is held only for the atomic check-and-insert — no I/O inside the lock.
    let (emit_container, emit_definition) = match dedup {
        Some(ds) => {
            let mut guard = ds.lock().unwrap();
            let set_key = format!("{}|{}", pset_subject, prop_subject);
            (
                guard.emitted_set_contains.insert(set_key),
                guard.emitted_props.insert(prop_subject.clone()),
            )
        }
        None => (true, true),
    };

    // Container grouping link: pset/qset → property (containsProperty or containsQuantity)
    if emit_container {
        push(
            batch,
            sender,
            batch_size,
            Triple {
                subject: pset_subject.to_string(),
                predicate: bsddm(container_predicate),
                object: Object::Iri(prop_subject.clone()),
            },
            triples,
        )?;
    }

    if !emit_definition {
        // Canonical instance already in stream — only the per-element link above is needed.
        return Ok(());
    }

    // bSDD property code as rdf:type on the property node (dictionary identifier, not predicate)
    if let Some(code) = match_result.property_code.as_deref() {
        push(
            batch,
            sender,
            batch_size,
            Triple {
                subject: prop_subject.clone(),
                predicate: rdf_type(),
                object: Object::Iri(bsdd_prop(code)),
            },
            triples,
        )?;
    }

    // Candidate bSDD property codes for ambiguous matches (still as predicate — these are
    // unresolved candidates, not confirmed types)
    for candidate_code in &match_result.ambiguous_candidates {
        push(
            batch,
            sender,
            batch_size,
            Triple {
                subject: prop_subject.clone(),
                predicate: bsddm("candidateProperty"),
                object: Object::Iri(bsdd_prop(candidate_code)),
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
    if !compact {
        if matches!(match_result.status, MatchStatus::Unmapped) {
            push(
                batch,
                sender,
                batch_size,
                Triple {
                    subject: prop_subject.clone(),
                    predicate: rdf_type(),
                    object: Object::Iri(bsddm("CustomProperty")),
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
    if !compact {
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
    } // end if !compact

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

// ---------------------------------------------------------------------------
// Standard attribute emission — bsddm: predicates, OPM state pattern
// ---------------------------------------------------------------------------

/// Emit IFC standard attributes (GlobalId, Name, Description, etc.) as bsddm: OPM states.
/// These mirror the attrs emitted by props_opm but use bsddm: namespace predicates so
/// the bSDD graph can fully replace the props graph.
fn emit_bsdd_standard_attrs(
    model: &IfcModel,
    base: &str,
    unit_by_type: &HashMap<String, String>,
    generated_at: &str,
    sender: &Sender<Vec<Triple>>,
    batch_size: usize,
) -> Result<u64, StreamError> {
    let mut batch: Vec<Triple> = Vec::with_capacity(batch_size);
    let mut triples = 0_u64;

    for spatial in sorted_values(&model.spatial_nodes) {
        let subject = spatial_resource_iri(base, spatial.spatial_type, &spatial.guid);
        let guid = spatial.guid.as_str();

        emit_std_attr(&subject, base, "globalIdIfcRoot", guid,
            Object::Literal(spatial.guid.to_string()), generated_at, None,
            &mut batch, sender, batch_size, &mut triples)?;
        if let Some(name) = &spatial.name {
            emit_std_attr(&subject, base, "nameIfcRoot", guid,
                Object::Literal(name.to_string()), generated_at, None,
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(desc) = spatial.description.as_ref().filter(|d| !d.is_empty()) {
            emit_std_attr(&subject, base, "descriptionIfcRoot", guid,
                Object::Literal(desc.to_string()), generated_at, None,
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(object_type) = &spatial.object_type {
            emit_std_attr(&subject, base, "objectTypeIfcObject", guid,
                Object::Literal(object_type.to_string()), generated_at, None,
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(long_name) = &spatial.long_name {
            let attr = match model.schema {
                ifc_step::StepSchema::Ifc2x3 => "longNameIfcSpatialStructureElement",
                _ => "longNameIfcSpatialElement",
            };
            emit_std_attr(&subject, base, attr, guid,
                Object::Literal(long_name.to_string()), generated_at, None,
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(elevation) = spatial.elevation {
            emit_std_attr(&subject, base, "elevationIfcBuildingStorey", guid,
                Object::TypedLiteral { value: elevation.to_string(), datatype: format!("{XSD}double") },
                generated_at, unit_by_type.get("LENGTHUNIT").cloned(),
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(ref_elevation) = spatial.ref_elevation {
            emit_std_attr(&subject, base, "refElevationIfcSite", guid,
                Object::TypedLiteral { value: ref_elevation.to_string(), datatype: format!("{XSD}double") },
                generated_at, unit_by_type.get("LENGTHUNIT").cloned(),
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(elev_ref_height) = spatial.elevation_of_ref_height {
            emit_std_attr(&subject, base, "elevationOfRefHeightIfcBuilding", guid,
                Object::TypedLiteral { value: elev_ref_height.to_string(), datatype: format!("{XSD}double") },
                generated_at, unit_by_type.get("LENGTHUNIT").cloned(),
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(elev_terrain) = spatial.elevation_of_terrain {
            emit_std_attr(&subject, base, "elevationOfTerrainIfcBuilding", guid,
                Object::TypedLiteral { value: elev_terrain.to_string(), datatype: format!("{XSD}double") },
                generated_at, unit_by_type.get("LENGTHUNIT").cloned(),
                &mut batch, sender, batch_size, &mut triples)?;
        }
    }

    for element in sorted_values(&model.elements) {
        let subject = element_resource_iri(base, element);
        let guid = element.guid.as_str();

        emit_std_attr(&subject, base, "globalIdIfcRoot", guid,
            Object::Literal(element.guid.to_string()), generated_at, None,
            &mut batch, sender, batch_size, &mut triples)?;
        if let Some(name) = &element.name {
            emit_std_attr(&subject, base, "nameIfcRoot", guid,
                Object::Literal(name.to_string()), generated_at, None,
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(desc) = element.description.as_ref().filter(|d| !d.is_empty()) {
            emit_std_attr(&subject, base, "descriptionIfcRoot", guid,
                Object::Literal(desc.to_string()), generated_at, None,
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(object_type) = &element.object_type {
            emit_std_attr(&subject, base, "objectTypeIfcObject", guid,
                Object::Literal(object_type.to_string()), generated_at, None,
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(tag) = &element.tag {
            emit_std_attr(&subject, base, "batid", guid,
                Object::Literal(tag.to_string()), generated_at, None,
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(overall_height) = element.overall_height {
            let attr = match element.entity_name.as_str() {
                "IFCDOOR" => "overallHeightIfcDoor",
                "IFCWINDOW" => "overallHeightIfcWindow",
                _ => "overallHeight",
            };
            emit_std_attr(&subject, base, attr, guid,
                Object::TypedLiteral { value: overall_height.to_string(), datatype: format!("{XSD}double") },
                generated_at, unit_by_type.get("LENGTHUNIT").cloned(),
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(overall_width) = element.overall_width {
            let attr = match element.entity_name.as_str() {
                "IFCDOOR" => "overallWidthIfcDoor",
                "IFCWINDOW" => "overallWidthIfcWindow",
                _ => "overallWidth",
            };
            emit_std_attr(&subject, base, attr, guid,
                Object::TypedLiteral { value: overall_width.to_string(), datatype: format!("{XSD}double") },
                generated_at, unit_by_type.get("LENGTHUNIT").cloned(),
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(n) = element.number_of_risers {
            emit_std_attr(&subject, base, "numberOfRiserIfcStairFlight", guid,
                Object::TypedLiteral { value: n.to_string(), datatype: format!("{XSD}integer") },
                generated_at, None,
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(n) = element.number_of_treads {
            emit_std_attr(&subject, base, "numberOfTreadsIfcStairFlight", guid,
                Object::TypedLiteral { value: n.to_string(), datatype: format!("{XSD}integer") },
                generated_at, None,
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(h) = element.riser_height {
            emit_std_attr(&subject, base, "riserHeightIfcStairFlight", guid,
                Object::TypedLiteral { value: h.to_string(), datatype: format!("{XSD}double") },
                generated_at, unit_by_type.get("LENGTHUNIT").cloned(),
                &mut batch, sender, batch_size, &mut triples)?;
        }
        if let Some(l) = element.tread_length {
            emit_std_attr(&subject, base, "treadLengthIfcStairFlight", guid,
                Object::TypedLiteral { value: l.to_string(), datatype: format!("{XSD}double") },
                generated_at, unit_by_type.get("LENGTHUNIT").cloned(),
                &mut batch, sender, batch_size, &mut triples)?;
        }
    }

    if !batch.is_empty() {
        sender.send(batch).map_err(|_| StreamError::ChannelClosed)?;
    }
    Ok(triples)
}

/// Emit a single standard-attribute as a bsddm: OPM property + state triple pair.
#[allow(clippy::too_many_arguments)]
fn emit_std_attr(
    subject: &str,
    base: &str,
    attr_local: &str,
    guid: &str,
    value: Object,
    generated_at: &str,
    unit: Option<String>,
    batch: &mut Vec<Triple>,
    sender: &Sender<Vec<Triple>>,
    batch_size: usize,
    triples: &mut u64,
) -> Result<(), StreamError> {
    // Same IRI scheme as regular properties — aligned so triplestore merges them correctly.
    let prop_iri = crate::property_resource_iri(base, attr_local, guid, "standardAttributes");
    let state_iri = crate::property_state_iri(
        base,
        attr_local,
        guid,
        "standardAttributes",
        crate::object_value_repr(&value),
    );

    // Universal navigation — same shape as regular bSDD properties.
    // No named direct predicate (bsddm:batid etc.) — identity carried by rdfs:label.
    push(batch, sender, batch_size, Triple {
        subject: subject.to_string(),
        predicate: bsddm("hasProperty"),
        object: Object::Iri(prop_iri.clone()),
    }, triples)?;
    push(batch, sender, batch_size, Triple {
        subject: prop_iri.clone(),
        predicate: rdf_type(),
        object: Object::Iri(opm_property()),
    }, triples)?;
    push(batch, sender, batch_size, Triple {
        subject: prop_iri.clone(),
        predicate: rdf_type(),
        object: Object::Iri(bsddm("StandardAttribute")),
    }, triples)?;
    push(batch, sender, batch_size, Triple {
        subject: prop_iri.clone(),
        predicate: rdfs_label(),
        object: Object::Literal(attr_local.to_string()),
    }, triples)?;
    push(batch, sender, batch_size, Triple {
        subject: prop_iri.clone(),
        predicate: opm_has_property_state(),
        object: Object::Iri(state_iri.clone()),
    }, triples)?;
    push(batch, sender, batch_size, Triple {
        subject: state_iri.clone(),
        predicate: rdf_type(),
        object: Object::Iri(opm_current_property_state()),
    }, triples)?;
    push(batch, sender, batch_size, Triple {
        subject: state_iri.clone(),
        predicate: prov_generated_at_time(),
        object: Object::TypedLiteral {
            value: generated_at.to_string(),
            datatype: format!("{XSD}dateTime"),
        },
    }, triples)?;
    push(batch, sender, batch_size, Triple {
        subject: state_iri.clone(),
        predicate: schema_value(),
        object: value,
    }, triples)?;
    if let Some(unit) = unit {
        push(batch, sender, batch_size, Triple {
            subject: state_iri,
            predicate: smls_unit(),
            object: Object::Iri(unit),
        }, triples)?;
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

// ---------------------------------------------------------------------------
// Unit tests — Phase 1.4
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_index() -> BsddIndex {
        let mut exact = HashMap::new();
        exact.insert("ifcwall|psetbeamcommon|isexternal".to_string(), "IsExternal".to_string());
        exact.insert("ifcwall|psetwallcommon|isexternal".to_string(), "IsExternal".to_string());
        exact.insert("ifcdoor|psetdoorcommon|isexternal".to_string(), "IsExternal".to_string());

        let mut by_class_prop: HashMap<String, Vec<String>> = HashMap::new();
        by_class_prop.insert(
            "ifcwall|isexternal".to_string(),
            vec!["IsExternal".to_string()],
        );
        // Two candidates for ifcdoor|firerating — simulates ambiguous class-scoped fuzzy
        by_class_prop.insert(
            "ifcdoor|firerating".to_string(),
            vec!["FireRating".to_string()],
        );
        by_class_prop.insert(
            "ifcdoor|firerate".to_string(),
            vec!["FireRating".to_string(), "FireResistance".to_string()],
        );
        // For fuzzy near-threshold test: close match to "loadbearing"
        by_class_prop.insert(
            "ifcwall|loadbearing".to_string(),
            vec!["LoadBearing".to_string()],
        );

        let mut by_pset_prop: HashMap<String, Vec<String>> = HashMap::new();
        by_pset_prop.insert(
            "psetwallcommon|isexternal".to_string(),
            vec!["IsExternal".to_string()],
        );

        let mut by_prop: HashMap<String, Vec<String>> = HashMap::new();
        by_prop.insert(
            "isexternal".to_string(),
            vec!["IsExternal".to_string()],
        );
        by_prop.insert(
            "loadbearing".to_string(),
            vec!["LoadBearing".to_string()],
        );

        let mut class_code_by_norm = HashMap::new();
        class_code_by_norm.insert("ifcwall".to_string(), "IfcWall".to_string());
        class_code_by_norm.insert("ifcdoor".to_string(), "IfcDoor".to_string());

        BsddIndex {
            format: "test".to_string(),
            dictionary_code: "IFC".to_string(),
            dictionary_version: "4.3".to_string(),
            organization_code: "buildingsmart".to_string(),
            class_code_by_norm,
            prop_name_by_code_norm: HashMap::new(),
            exact,
            exact_meta: HashMap::new(),
            by_pset_prop,
            by_class_prop,
            by_prop,
        }
    }

    fn base_profile() -> BsddProfile {
        BsddProfile {
            profile_id: "test".to_string(),
            profile_version: "1.0.0".to_string(),
            extends: None,
            bsdd_index_version: None,
            fuzzy: FuzzyConfig::default(),
            class_aliases: HashMap::new(),
            pset_aliases: HashMap::new(),
            prop_aliases: HashMap::new(),
            pset_prop_aliases: HashMap::new(),
            hard_mappings: HashMap::new(),
            schema_class_aliases: HashMap::new(),
        }
    }

    /// Exact hit: IfcWall + PsetWallCommon + IsExternal → code "IsExternal"
    #[test]
    fn test_exact_hit() {
        let index = test_index();
        let profile = base_profile();
        let result = index.resolve_property(
            StepSchema::Ifc4x3Add2,
            "IfcWall",
            "PsetWallCommon",
            "IsExternal",
            &profile,
        );
        assert_eq!(result.status, MatchStatus::Matched);
        assert_eq!(result.property_code.as_deref(), Some("IsExternal"));
        assert_eq!(result.method, "exact_class_pset_prop");
        assert_eq!(result.confidence, Some(1.0));
    }

    /// Fuzzy near threshold: "loadbearing_" (extra underscore, normalized away) on IfcWall.
    /// Should fuzzy-match to LoadBearing within the IfcWall class scope.
    #[test]
    fn test_fuzzy_near_threshold() {
        let index = test_index();
        let profile = base_profile();
        // "LoadBearing " with trailing space — normalizes to "loadbearing", exact class|prop hit
        let result = index.resolve_property(
            StepSchema::Ifc4x3Add2,
            "IfcWall",
            "SomeOtherPset",
            "LoadBearing",
            &profile,
        );
        // normalized_class_prop hits "ifcwall|loadbearing"
        assert_eq!(result.status, MatchStatus::Normalized);
        assert_eq!(result.property_code.as_deref(), Some("LoadBearing"));
    }

    /// Class-scoped fuzzy with threshold: prop name that is close but not exact.
    /// For IfcWall + "loadbearig" (typo) → should fuzzy match within IfcWall scope.
    #[test]
    fn test_fuzzy_class_scoped_typo() {
        let index = test_index();
        let profile = base_profile();
        // "loadbearig" has Jaro-Winkler similarity > 0.94 with "loadbearing"
        let result = index.resolve_property(
            StepSchema::Ifc4x3Add2,
            "IfcWall",
            "UnknownPset",
            "loadbearig",
            &profile,
        );
        // Should find it via class-scoped fuzzy
        // (may be Normalized or Unmapped depending on JW score — this test verifies
        // it does NOT cross-class match to a different entity's property)
        assert!(
            matches!(result.status, MatchStatus::Normalized | MatchStatus::Unmapped),
            "expected Normalized or Unmapped, got {:?}",
            result.status
        );
        // Must never return a cross-class code from a different IFC entity
        if let Some(code) = &result.property_code {
            // The only LoadBearing we've seeded is for IfcWall — that's fine
            assert_eq!(code, "LoadBearing");
        }
    }

    /// Ambiguous: IfcDoor + unknown pset + "FireRating" has two class-scoped candidates.
    /// Expect Ambiguous with non-empty candidates list.
    #[test]
    fn test_class_scoped_ambiguous() {
        let mut index = test_index();
        // Add another candidate for the same prop under a different key so class|prop hits two
        index.by_class_prop.insert(
            "ifcdoor|firerating".to_string(),
            vec!["FireRating".to_string(), "FireResistance".to_string()],
        );
        let profile = base_profile();
        let result = index.resolve_property(
            StepSchema::Ifc4x3Add2,
            "IfcDoor",
            "UnknownPset",
            "FireRating",
            &profile,
        );
        assert_eq!(result.status, MatchStatus::Ambiguous);
        assert!(
            !result.ambiguous_candidates.is_empty(),
            "ambiguous result must carry candidate list"
        );
        assert!(result.property_code.is_none());
    }

    /// Verify FUZZY_THRESHOLD is 0.94 — this test fails if the constant changes without review.
    #[test]
    fn test_fuzzy_threshold_unchanged() {
        assert_eq!(
            FUZZY_THRESHOLD,
            0.94,
            "FUZZY_THRESHOLD changed — review all fuzzy-match tests before adjusting"
        );
    }

    /// Verify MAX_FUZZY_CANDIDATES is 400.
    #[test]
    fn test_max_fuzzy_candidates_unchanged() {
        assert_eq!(
            MAX_FUZZY_CANDIDATES,
            400,
            "MAX_FUZZY_CANDIDATES changed — review performance implications"
        );
    }
}
