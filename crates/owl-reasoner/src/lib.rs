//! # OWL Reasoner
//!
//! A pure-logic OWL reasoning engine for IFC2LBD-Neo's ontology mapper.
//!
//! Parses OWL class expressions from alignment + ontology files (Turtle)
//! and infers new `rdf:type` triples based on `owl:equivalentClass` axioms
//! with blank-node (complex) right sides.
//!
//! ## Supported OWL constructs
//!
//! | Construct | Syntax | Semantics |
//! |-----------|--------|----------|
//! | `owl:equivalentClass` (named ↔ expression) | `A owl:equivalentClass [expr]` | A ≡ expr |
//! | `owl:intersectionOf` | `( C1 C2 … )` | ALL must match |
//! | `owl:unionOf` | `( C1 C2 … )` | ANY must match |
//! | `owl:complementOf` | `B` | NOT B |
//! | `owl:someValuesFrom` | `P someValuesFrom C` | ≥1 P-value of type C |
//! | `owl:allValuesFrom` | `P allValuesFrom C` | ALL P-values of type C |
//! | `owl:hasValue` | `P hasValue V` | P-value exactly V |
//! | `owl:cardinality` | `P cardinality N` | exactly N distinct P-values |
//! | `owl:minCardinality` | `P minCardinality N` | ≥ N distinct P-values |
//! | `owl:maxCardinality` | `P maxCardinality N` | ≤ N distinct P-values |
//!
//! ## Limitations (Phase 1)
//!
//! - **No fixpoint iteration** — single pass. Rules whose conditions
//!   reference classes inferred by other rules will not fire. Order
//!   alignment axioms so base `equivalentClass` mappings come before derived
//!   ones.
//! - **Closed-world cardinality** — counts asserted triples, not possible
//!   values. Values are deduplicated.
//! - **`someValuesFrom owl:Thing` ignores literals** — `owl:Thing` in OWL
//!   DL matches IRI values only, not literals.
//! - **No complex `rdfs:subClassOf` expressions** — only
//!   `equivalentClass` with blank-node right sides generates rules.
//! - **No transitive `subClassOf` closure** — deferred to Phase 2.
//! - **No `owl:propertyChainAxiom`** — deferred to Phase 2.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use owl_reasoner::{parse_rules, infer_types};
//!
//! // Parse alignment + ontology files for reasoning rules.
//! let (rules, warnings) = parse_rules(&alignment_turtle, &ontology_turtle)?;
//!
//! // Run the reasoner over a set of triples.
//! let inferred = infer_types(&rules, &triples);
//! ```

pub mod evaluator;
pub mod expression;
pub mod index;
pub mod parser;

pub use expression::{ClassExpression, Restriction, RestrictionKind};
pub use parser::{parse_rules, Rule};

use lbd_ontology::{Object, Triple};

/// The `rdf:type` predicate IRI.
pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// An inferred `rdf:type` triple.
#[derive(Clone, Debug)]
pub struct InferredTriple {
    /// The subject that satisfies the rule's condition.
    pub subject: String,
    /// The inferred class IRI (the `rdf:type` object).
    pub object: Object,
}

/// Run the reasoner over a set of triples.
///
/// Builds an index, evaluates each rule's condition against candidate
/// subjects, and returns new `rdf:type` triples for satisfied conditions.
///
/// **Single pass** — no fixpoint iteration. Rules whose conditions reference
/// classes inferred by other rules will not fire. Users should order
/// alignment axioms so that base `equivalentClass` mappings come before
/// derived ones.
///
/// Existing `rdf:type` assertions are **not** duplicated — only truly new
/// type assertions are returned.
pub fn infer_types(rules: &[Rule], triples: &[Triple]) -> Vec<InferredTriple> {
    if rules.is_empty() || triples.is_empty() {
        return Vec::new();
    }

    let index = index::TripleIndex::from_triples(triples);
    let all_subjects = index.all_subjects().to_vec();

    // Collect existing (subject, class) type pairs to avoid duplicates.
    let mut existing_types: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for triple in triples {
        if triple.predicate == RDF_TYPE {
            if let Object::Iri(class_iri) = &triple.object {
                existing_types.insert((triple.subject.clone(), class_iri.clone()));
            }
        }
    }

    let mut inferred = Vec::new();

    for rule in rules {
        let candidates = evaluator::candidate_subjects(&rule.condition, &index, &all_subjects);

        for subject in candidates {
            // Full evaluation (candidate filtering is a filter, not a
            // complete evaluator).
            if !evaluator::evaluate(&subject, &rule.condition, &index) {
                continue;
            }

            // Skip if the subject already has this type.
            if existing_types.contains(&(subject.clone(), rule.inferred_class.clone())) {
                continue;
            }

            inferred.push(InferredTriple {
                subject,
                object: Object::Iri(rule.inferred_class.clone()),
            });
        }
    }

    inferred
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

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

    const TTL_PREFIX: &str = r#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ex: <http://ex.org/> .
"#;

    #[test]
    fn test_infer_types_empty_rules() {
        let triples = vec![make_triple(
            "http://ex.org/a",
            RDF_TYPE,
            Object::Iri("http://ex.org/Wall".to_string()),
        )];
        let inferred = infer_types(&[], &triples);
        assert!(inferred.is_empty());
    }

    #[test]
    fn test_infer_types_empty_triples() {
        let (rules, _) = parse_rules(
            &format!("{TTL_PREFIX} ex:A owl:equivalentClass [ a owl:Class ; owl:intersectionOf ( ex:X ) ] ."),
            "",
        ).unwrap();
        let inferred = infer_types(&rules, &[]);
        assert!(inferred.is_empty());
    }

    #[test]
    fn test_infer_types_basic() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:ExternalWall owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Wall
        [ a owl:Restriction ;
          owl:onProperty ex:isExternal ;
          owl:someValuesFrom owl:Thing ]
    )
] .
"#
        );
        let (rules, _) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 1);

        let triples = vec![
            // wall1: is a Wall AND has isExternal (IRI value)
            make_triple(
                "http://ex.org/wall1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/wall1",
                "http://ex.org/isExternal",
                Object::Iri("http://ex.org/true".to_string()),
            ),
            // wall2: is a Wall but no isExternal
            make_triple(
                "http://ex.org/wall2",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            // slab1: has isExternal but is not a Wall
            make_triple(
                "http://ex.org/slab1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Slab".to_string()),
            ),
            make_triple(
                "http://ex.org/slab1",
                "http://ex.org/isExternal",
                Object::Iri("http://ex.org/true".to_string()),
            ),
        ];

        let inferred = infer_types(&rules, &triples);
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].subject, "http://ex.org/wall1");
        assert_eq!(
            inferred[0].object,
            Object::Iri("http://ex.org/ExternalWall".to_string())
        );
    }

    #[test]
    fn test_infer_types_dedup() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:DoubleWall owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf ( ex:Wall )
] .
"#
        );
        let (rules, _) = parse_rules(&turtle, "").unwrap();

        let triples = vec![
            make_triple(
                "http://ex.org/wall1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            // wall1 already has rdf:type ex:DoubleWall → should NOT be duplicated
            make_triple(
                "http://ex.org/wall1",
                RDF_TYPE,
                Object::Iri("http://ex.org/DoubleWall".to_string()),
            ),
        ];

        let inferred = infer_types(&rules, &triples);
        assert!(
            inferred.is_empty(),
            "existing type should not be duplicated"
        );
    }

    #[test]
    fn test_infer_types_has_value() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:ExternalWall owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Wall
        [ a owl:Restriction ;
          owl:onProperty ex:isExternal ;
          owl:hasValue "true"^^xsd:boolean ]
    )
] .
"#
        );
        let (rules, _) = parse_rules(&turtle, "").unwrap();

        let triples = vec![
            make_triple(
                "http://ex.org/wall1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/wall1",
                "http://ex.org/isExternal",
                Object::TypedLiteral {
                    value: "true".to_string(),
                    datatype: "http://www.w3.org/2001/XMLSchema#boolean".to_string(),
                },
            ),
            make_triple(
                "http://ex.org/wall2",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/wall2",
                "http://ex.org/isExternal",
                Object::TypedLiteral {
                    value: "false".to_string(),
                    datatype: "http://www.w3.org/2001/XMLSchema#boolean".to_string(),
                },
            ),
        ];

        let inferred = infer_types(&rules, &triples);
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].subject, "http://ex.org/wall1");
    }

    #[test]
    fn test_infer_types_nested_opm() {
        // Full OPM property reification: wall → hasProp → property → hasState → state → value → "true"
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:ExternalWall owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Wall
        [ a owl:Restriction ;
          owl:onProperty ex:hasProp ;
          owl:someValuesFrom [
              a owl:Class ;
              owl:intersectionOf (
                  ex:Property
                  [ a owl:Restriction ;
                    owl:onProperty ex:hasState ;
                    owl:someValuesFrom [
                        a owl:Class ;
                        owl:intersectionOf (
                            ex:CurrentState
                            [ a owl:Restriction ;
                              owl:onProperty ex:value ;
                              owl:hasValue "true"^^xsd:boolean ]
                        )
                    ] ]
              )
          ] ]
    )
] .
"#
        );
        let (rules, _) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 1);

        let triples = vec![
            // wall1: isExternal = true (via OPM reification)
            make_triple(
                "http://ex.org/wall1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/wall1",
                "http://ex.org/hasProp",
                Object::Iri("http://ex.org/prop1".to_string()),
            ),
            make_triple(
                "http://ex.org/prop1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Property".to_string()),
            ),
            make_triple(
                "http://ex.org/prop1",
                "http://ex.org/hasState",
                Object::Iri("http://ex.org/state1".to_string()),
            ),
            make_triple(
                "http://ex.org/state1",
                RDF_TYPE,
                Object::Iri("http://ex.org/CurrentState".to_string()),
            ),
            make_triple(
                "http://ex.org/state1",
                "http://ex.org/value",
                Object::TypedLiteral {
                    value: "true".to_string(),
                    datatype: "http://www.w3.org/2001/XMLSchema#boolean".to_string(),
                },
            ),
            // wall2: isExternal = false
            make_triple(
                "http://ex.org/wall2",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/wall2",
                "http://ex.org/hasProp",
                Object::Iri("http://ex.org/prop2".to_string()),
            ),
            make_triple(
                "http://ex.org/prop2",
                RDF_TYPE,
                Object::Iri("http://ex.org/Property".to_string()),
            ),
            make_triple(
                "http://ex.org/prop2",
                "http://ex.org/hasState",
                Object::Iri("http://ex.org/state2".to_string()),
            ),
            make_triple(
                "http://ex.org/state2",
                RDF_TYPE,
                Object::Iri("http://ex.org/CurrentState".to_string()),
            ),
            make_triple(
                "http://ex.org/state2",
                "http://ex.org/value",
                Object::TypedLiteral {
                    value: "false".to_string(),
                    datatype: "http://www.w3.org/2001/XMLSchema#boolean".to_string(),
                },
            ),
        ];

        let inferred = infer_types(&rules, &triples);
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].subject, "http://ex.org/wall1");
        assert_eq!(
            inferred[0].object,
            Object::Iri("http://ex.org/ExternalWall".to_string())
        );
    }

    #[test]
    fn test_infer_types_multiple_rules() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:ExternalWall owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Wall
        [ a owl:Restriction ; owl:onProperty ex:isExternal ; owl:hasValue "true"^^xsd:boolean ]
    )
] .
ex:InternalWall owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Wall
        [ a owl:Restriction ; owl:onProperty ex:isExternal ; owl:hasValue "false"^^xsd:boolean ]
    )
] .
"#
        );
        let (rules, _) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 2);

        let triples = vec![
            make_triple(
                "http://ex.org/wall1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/wall1",
                "http://ex.org/isExternal",
                Object::TypedLiteral {
                    value: "true".to_string(),
                    datatype: "http://www.w3.org/2001/XMLSchema#boolean".to_string(),
                },
            ),
            make_triple(
                "http://ex.org/wall2",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/wall2",
                "http://ex.org/isExternal",
                Object::TypedLiteral {
                    value: "false".to_string(),
                    datatype: "http://www.w3.org/2001/XMLSchema#boolean".to_string(),
                },
            ),
        ];

        let inferred = infer_types(&rules, &triples);
        assert_eq!(inferred.len(), 2);
        let subjects: Vec<&str> = inferred.iter().map(|i| i.subject.as_str()).collect();
        assert!(subjects.contains(&"http://ex.org/wall1"));
        assert!(subjects.contains(&"http://ex.org/wall2"));
    }

    #[test]
    fn test_infer_types_union() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:Opening owl:equivalentClass [
    a owl:Class ;
    owl:unionOf ( ex:Door ex:Window )
] .
"#
        );
        let (rules, _) = parse_rules(&turtle, "").unwrap();

        let triples = vec![
            make_triple(
                "http://ex.org/d1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Door".to_string()),
            ),
            make_triple(
                "http://ex.org/w1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Window".to_string()),
            ),
            make_triple(
                "http://ex.org/wall1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
        ];

        let inferred = infer_types(&rules, &triples);
        assert_eq!(inferred.len(), 2);
        let subjects: Vec<&str> = inferred.iter().map(|i| i.subject.as_str()).collect();
        assert!(subjects.contains(&"http://ex.org/d1"));
        assert!(subjects.contains(&"http://ex.org/w1"));
    }

    #[test]
    fn test_infer_types_complement() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:NonWall owl:equivalentClass [
    a owl:Class ;
    owl:complementOf ex:Wall
] .
"#
        );
        let (rules, _) = parse_rules(&turtle, "").unwrap();

        let triples = vec![
            make_triple(
                "http://ex.org/a",
                RDF_TYPE,
                Object::Iri("http://ex.org/Wall".to_string()),
            ),
            make_triple(
                "http://ex.org/b",
                RDF_TYPE,
                Object::Iri("http://ex.org/Slab".to_string()),
            ),
        ];

        let inferred = infer_types(&rules, &triples);
        // 'a' is a Wall → complement is false. 'b' is NOT a Wall → complement is true.
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].subject, "http://ex.org/b");
        assert_eq!(
            inferred[0].object,
            Object::Iri("http://ex.org/NonWall".to_string())
        );
    }

    #[test]
    fn test_infer_types_cardinality() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:TwoDoorRoom owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Room
        [ a owl:Restriction ; owl:onProperty ex:hasDoor ; owl:cardinality "2"^^xsd:nonNegativeInteger ]
    )
] .
"#
        );
        let (rules, _) = parse_rules(&turtle, "").unwrap();

        let triples = vec![
            make_triple(
                "http://ex.org/room1",
                RDF_TYPE,
                Object::Iri("http://ex.org/Room".to_string()),
            ),
            make_triple(
                "http://ex.org/room1",
                "http://ex.org/hasDoor",
                Object::Iri("http://ex.org/d1".to_string()),
            ),
            make_triple(
                "http://ex.org/room1",
                "http://ex.org/hasDoor",
                Object::Iri("http://ex.org/d2".to_string()),
            ),
            make_triple(
                "http://ex.org/room2",
                RDF_TYPE,
                Object::Iri("http://ex.org/Room".to_string()),
            ),
            make_triple(
                "http://ex.org/room2",
                "http://ex.org/hasDoor",
                Object::Iri("http://ex.org/d3".to_string()),
            ),
        ];

        let inferred = infer_types(&rules, &triples);
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].subject, "http://ex.org/room1");
    }
}
