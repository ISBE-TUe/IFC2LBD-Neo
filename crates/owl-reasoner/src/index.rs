//! In-memory triple index for efficient rule evaluation.
//!
//! The index stores triples in a structured form that allows O(1) lookups
//! for the patterns the evaluator needs:
//! - "Does subject S have rdf:type C?" → `has_type`
//! - "What values does subject S have for predicate P?" → `get_objects`
//! - "Which subjects have rdf:type C?" → `subjects_of_type` (reverse index)
//! - "How many distinct values does S have for P?" → `count_distinct_objects`
//! - "Does S have any IRI-valued triple with predicate P?" → `has_iri_property`

use std::collections::{HashMap, HashSet};

use lbd_ontology::{Object, Triple};

/// Index of all triples, optimized for the access patterns the evaluator
/// needs.
///
/// Memory cost: roughly 3–5× the raw triple data because every subject,
/// predicate, and object string is cloned into the index plus class IRIs
/// are duplicated in the `types`/`by_type` maps. For 2M triples (~200 MB
/// of raw data) the index is ~600 MB–1 GB.
pub struct TripleIndex {
    /// subject IRI → (predicate IRI → Vec<Object>)
    by_subject: HashMap<String, HashMap<String, Vec<Object>>>,

    /// subject IRI → Set of rdf:type class IRIs
    types: HashMap<String, HashSet<String>>,

    /// class IRI → Set of subject IRIs (reverse index for O(1) candidate
    /// lookup)
    by_type: HashMap<String, HashSet<String>>,

    /// All distinct subject IRIs (for complement / full-scan evaluation)
    all_subjects: Vec<String>,
}

impl TripleIndex {
    /// Build the index from a slice of triples. Single O(n) pass.
    pub fn from_triples(triples: &[Triple]) -> Self {
        let mut index = TripleIndex {
            by_subject: HashMap::new(),
            types: HashMap::new(),
            by_type: HashMap::new(),
            all_subjects: Vec::new(),
        };

        let rdf_type = crate::expression::vocab::RDF_TYPE;

        for triple in triples {
            // Track all distinct subjects
            if !index.by_subject.contains_key(&triple.subject) {
                index.all_subjects.push(triple.subject.clone());
            }

            // by_subject: subject → predicate → objects
            let pred_map = index.by_subject.entry(triple.subject.clone()).or_default();
            pred_map
                .entry(triple.predicate.clone())
                .or_default()
                .push(triple.object.clone());

            // types + by_type for rdf:type triples
            if triple.predicate == rdf_type {
                if let Object::Iri(class_iri) = &triple.object {
                    index
                        .types
                        .entry(triple.subject.clone())
                        .or_default()
                        .insert(class_iri.clone());
                    index
                        .by_type
                        .entry(class_iri.clone())
                        .or_default()
                        .insert(triple.subject.clone());
                }
            }
        }

        index
    }

    /// Does `subject` have `rdf:type class`?
    pub fn has_type(&self, subject: &str, class: &str) -> bool {
        self.types
            .get(subject)
            .is_some_and(|set| set.contains(class))
    }

    /// Get all values of `predicate` for `subject`. Returns an empty slice
    /// if the subject has no triple with that predicate.
    pub fn get_objects<'a>(&'a self, subject: &str, predicate: &str) -> &'a [Object] {
        self.by_subject
            .get(subject)
            .and_then(|pm| pm.get(predicate))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get all subjects that have `rdf:type class` — O(1) via reverse index.
    pub fn subjects_of_type(&self, class: &str) -> Option<&HashSet<String>> {
        self.by_type.get(class)
    }

    /// Count distinct values of `predicate` for `subject`. Uses object
    /// equality (which includes datatype for TypedLiteral) for dedup.
    pub fn count_distinct_objects(&self, subject: &str, predicate: &str) -> usize {
        let objects = self.get_objects(subject, predicate);
        let mut seen: HashSet<&Object> = HashSet::new();
        for obj in objects {
            seen.insert(obj);
        }
        seen.len()
    }

    /// Does `subject` have any IRI-valued triple with `predicate`?
    /// Used for candidate filtering on `someValuesFrom owl:Thing`.
    pub fn has_iri_property(&self, subject: &str, predicate: &str) -> bool {
        self.get_objects(subject, predicate)
            .iter()
            .any(|obj| matches!(obj, Object::Iri(_)))
    }

    /// Get all distinct subject IRIs in the index.
    pub fn all_subjects(&self) -> &[String] {
        &self.all_subjects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_triple(s: &str, p: &str, o: Object) -> Triple {
        Triple {
            subject: s.to_string(),
            predicate: p.to_string(),
            object: o,
        }
    }

    #[test]
    fn test_has_type() {
        let triples = vec![
            make_triple(
                "http://ex.org/a",
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                Object::Iri("http://ex.org/Slab".to_string()),
            ),
        ];
        let idx = TripleIndex::from_triples(&triples);
        assert!(idx.has_type("http://ex.org/a", "http://ex.org/Wall"));
        assert!(!idx.has_type("http://ex.org/a", "http://ex.org/Slab"));
        assert!(!idx.has_type("http://ex.org/b", "http://ex.org/Wall"));
        assert!(idx.has_type("http://ex.org/b", "http://ex.org/Slab"));
    }

    #[test]
    fn test_get_objects() {
        let triples = vec![
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasColor",
                Object::Iri("http://ex.org/red".to_string()),
            ),
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasColor",
                Object::Iri("http://ex.org/blue".to_string()),
            ),
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasName",
                Object::Literal("wall1".to_string()),
            ),
        ];
        let idx = TripleIndex::from_triples(&triples);
        let colors = idx.get_objects("http://ex.org/a", "http://ex.org/hasColor");
        assert_eq!(colors.len(), 2);
        let names = idx.get_objects("http://ex.org/a", "http://ex.org/hasName");
        assert_eq!(names.len(), 1);
        // Missing predicate → empty
        let missing = idx.get_objects("http://ex.org/a", "http://ex.org/missing");
        assert!(missing.is_empty());
    }

    #[test]
    fn test_subjects_of_type() {
        let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
        let triples = vec![
            make_triple(
                "http://ex.org/a",
                rdf_type,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                rdf_type,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/c",
                rdf_type,
                Object::Iri("http://ex.org/Slab".to_string()),
            ),
        ];
        let idx = TripleIndex::from_triples(&triples);
        let walls = idx.subjects_of_type("http://ex.org/Wall").unwrap();
        assert_eq!(walls.len(), 2);
        assert!(walls.contains("http://ex.org/a"));
        assert!(walls.contains("http://ex.org/b"));
    }

    #[test]
    fn test_count_distinct_objects() {
        let triples = vec![
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasColor",
                Object::Iri("http://ex.org/red".to_string()),
            ),
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasColor",
                Object::Iri("http://ex.org/red".to_string()),
            ),
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasColor",
                Object::Iri("http://ex.org/blue".to_string()),
            ),
        ];
        let idx = TripleIndex::from_triples(&triples);
        // 3 triples, but only 2 distinct objects
        assert_eq!(
            idx.count_distinct_objects("http://ex.org/a", "http://ex.org/hasColor"),
            2
        );
    }

    #[test]
    fn test_has_iri_property() {
        let triples = vec![
            make_triple(
                "http://ex.org/a",
                "http://ex.org/hasRef",
                Object::Iri("http://ex.org/b".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                "http://ex.org/hasName",
                Object::Literal("foo".to_string()),
            ),
        ];
        let idx = TripleIndex::from_triples(&triples);
        assert!(idx.has_iri_property("http://ex.org/a", "http://ex.org/hasRef"));
        assert!(!idx.has_iri_property("http://ex.org/b", "http://ex.org/hasName"));
    }

    #[test]
    fn test_empty_index() {
        let idx = TripleIndex::from_triples(&[]);
        assert!(idx.all_subjects().is_empty());
        assert!(!idx.has_type("http://ex.org/a", "http://ex.org/Wall"));
        assert!(idx
            .get_objects("http://ex.org/a", "http://ex.org/p")
            .is_empty());
        assert!(idx.subjects_of_type("http://ex.org/Wall").is_none());
    }

    #[test]
    fn test_all_subjects() {
        let triples = vec![
            make_triple(
                "http://ex.org/a",
                "http://ex.org/p",
                Object::Iri("http://ex.org/b".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                "http://ex.org/p",
                Object::Iri("http://ex.org/c".to_string()),
            ),
        ];
        let idx = TripleIndex::from_triples(&triples);
        assert_eq!(idx.all_subjects().len(), 2);
    }
}
