//! RDF → ClassExpression tree parser.
//!
//! Parses Turtle/N-Triples RDF containing OWL axioms and builds
//! `ClassExpression` trees from blank-node class expressions.
//!
//! The parser handles:
//! - `owl:equivalentClass` where the right side is a blank node (complex
//!   expression). Named↔named `equivalentClass` is **excluded** — it's
//!   handled by the existing simple IRI mapping in the ontology mapper.
//! - `owl:intersectionOf` / `owl:unionOf` (RDF list syntax)
//! - `owl:Restriction` with `someValuesFrom`, `allValuesFrom`, `hasValue`,
//!   `cardinality`, `minCardinality`, `maxCardinality`
//! - `owl:complementOf`
//! - Recursive nesting (arbitrary depth)

use std::collections::HashMap;
use std::io::Cursor;

use lbd_ontology::Object;
use rio_api::model::{Subject as RioSubject, Term as RioTerm};
use rio_api::parser::TriplesParser;
use rio_turtle::TurtleParser;

use crate::expression::vocab::*;
use crate::expression::{BlankNodeKind, ClassExpression, Restriction, RestrictionKind};

/// A flat triple stored during initial parsing, keyed by subject.
/// Subjects can be named nodes (IRI) or blank nodes (`_:id`).
#[derive(Clone, Debug)]
struct FlatTriple {
    predicate: String,
    object: FlatTerm,
}

#[derive(Clone, Debug)]
enum FlatTerm {
    Iri(String),
    BlankNode(String),
    Literal(String),
    TypedLiteral { value: String, datatype: String },
}

/// Parsed raw triples grouped by subject: subject → Vec<(predicate, object)>.
type TripleStore = HashMap<String, Vec<FlatTriple>>;

/// Parse alignment + ontology Turtle files and extract reasoning rules.
///
/// Returns only rules for `equivalentClass` with blank-node (complex) right
/// sides. Named↔named mappings are handled by the caller's simple mapping
/// logic.
///
/// Unknown blank node constructs are skipped with a warning — they do NOT
/// cause an error (the caller continues with whatever rules were parsed).
///
/// # Returns
/// `(Vec<Rule>, Vec<String> warnings)`
pub fn parse_rules(
    alignment_turtle: &str,
    ontology_turtle: &str,
) -> Result<(Vec<Rule>, Vec<String>), String> {
    let mut warnings = Vec::new();

    // Parse both files into flat triple stores
    let alignment_store = parse_turtle_to_store(alignment_turtle, "alignment")
        .map_err(|e| format!("alignment parse error: {e}"))?;
    let ontology_store = parse_turtle_to_store(ontology_turtle, "ontology")
        .map_err(|e| format!("ontology parse error: {e}"))?;

    let mut rules = Vec::new();

    // Extract rules from both stores
    for store in [&alignment_store, &ontology_store] {
        for (subject, triples) in store {
            // Look for owl:equivalentClass where subject is a named node (IRI)
            // and object is a blank node.
            if subject.starts_with("_:") {
                continue; // Skip blank-node subjects for rule extraction
            }

            for triple in triples {
                if triple.predicate != OWL_EQUIVALENT_CLASS {
                    continue;
                }
                match &triple.object {
                    FlatTerm::BlankNode(bn) => {
                        // Complex expression — parse it
                        match parse_expression(bn, store) {
                            Ok(expr) => {
                                rules.push(Rule {
                                    inferred_class: subject.clone(),
                                    condition: expr,
                                });
                            }
                            Err(e) => {
                                warnings
                                    .push(format!("Skipped equivalentClass for <{subject}>: {e}"));
                            }
                        }
                    }
                    FlatTerm::Iri(_) => {
                        // Named↔named — skip, handled by simple mapping
                    }
                    FlatTerm::Literal(_) | FlatTerm::TypedLiteral { .. } => {
                        warnings.push(format!(
                            "Skipped equivalentClass for <{subject}>: object is a literal, not a class expression"
                        ));
                    }
                }
            }
        }
    }

    Ok((rules, warnings))
}

/// A reasoning rule: if a subject satisfies `condition`, infer
/// `rdf:type inferred_class`.
#[derive(Clone, Debug)]
pub struct Rule {
    /// The class to infer (left side of `equivalentClass` with blank node).
    pub inferred_class: String,
    /// The condition expression (right side — must be a complex expression).
    pub condition: ClassExpression,
}

/// Parse a Turtle string into a flat triple store grouped by subject.
fn parse_turtle_to_store(turtle: &str, _label: &str) -> Result<TripleStore, String> {
    let mut store: TripleStore = HashMap::new();

    if turtle.trim().is_empty() {
        return Ok(store);
    }

    let reader = std::io::BufReader::new(Cursor::new(turtle.as_bytes()));
    let base = oxiri::Iri::parse("http://owl-reasoner/base".to_string())
        .map_err(|e| format!("base IRI parse: {e}"))?;
    let mut parser = TurtleParser::new(reader, Some(base));

    parser
        .parse_all(&mut |triple| {
            let subject = match triple.subject {
                RioSubject::NamedNode(n) => n.iri.to_string(),
                RioSubject::BlankNode(b) => format!("_:{}", b.id),
                _ => return Ok(()) as Result<(), Box<dyn std::error::Error>>,
            };

            let predicate = triple.predicate.iri.to_string();

            let object = match triple.object {
                RioTerm::NamedNode(n) => FlatTerm::Iri(n.iri.to_string()),
                RioTerm::BlankNode(b) => FlatTerm::BlankNode(format!("_:{}", b.id)),
                RioTerm::Literal(l) => match l {
                    rio_api::model::Literal::Simple { value } => {
                        FlatTerm::Literal(value.to_string())
                    }
                    rio_api::model::Literal::LanguageTaggedString { value, .. } => {
                        FlatTerm::TypedLiteral {
                            value: value.to_string(),
                            datatype: "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString"
                                .to_string(),
                        }
                    }
                    rio_api::model::Literal::Typed { value, datatype } => FlatTerm::TypedLiteral {
                        value: value.to_string(),
                        datatype: datatype.iri.to_string(),
                    },
                },
                _ => return Ok(()),
            };

            store
                .entry(subject)
                .or_default()
                .push(FlatTriple { predicate, object });

            Ok(()) as Result<(), Box<dyn std::error::Error>>
        })
        .map_err(|e| format!("RDF parse error: {e}"))?;

    Ok(store)
}

/// Recursively parse a blank node into a ClassExpression.
///
/// Looks at the blank node's triples to determine what kind of expression it
/// represents:
/// 1. `owl:intersectionOf` → follow RDF list
/// 2. `owl:unionOf` → follow RDF list
/// 3. `owl:complementOf` → parse the object
/// 4. `owl:Restriction` (via rdf:type) → parse restriction
/// 5. Fallback: error (unknown construct)
fn parse_expression(blank_node: &str, store: &TripleStore) -> Result<ClassExpression, String> {
    let triples = store
        .get(blank_node)
        .ok_or_else(|| format!("blank node {blank_node} has no triples"))?;

    // Collect rdf:type values to determine the construct kind
    let mut type_iris: Vec<&str> = Vec::new();
    for t in triples {
        if t.predicate == RDF_TYPE {
            if let FlatTerm::Iri(iri) = &t.object {
                type_iris.push(iri.as_str());
            }
        }
    }

    // Check for owl:intersectionOf
    for t in triples {
        if t.predicate == OWL_INTERSECTION_OF {
            let list = parse_rdf_list(&t.object, store)?;
            let parts: Vec<ClassExpression> = list
                .into_iter()
                .map(|item| parse_term_as_expression(&item, store))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(ClassExpression::Intersection(parts));
        }
    }

    // Check for owl:unionOf
    for t in triples {
        if t.predicate == OWL_UNION_OF {
            let list = parse_rdf_list(&t.object, store)?;
            let parts: Vec<ClassExpression> = list
                .into_iter()
                .map(|item| parse_term_as_expression(&item, store))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(ClassExpression::Union(parts));
        }
    }

    // Check for owl:complementOf
    for t in triples {
        if t.predicate == OWL_COMPLEMENT_OF {
            let inner = parse_term_as_expression(&t.object, store)?;
            return Ok(ClassExpression::Complement(Box::new(inner)));
        }
    }

    // Check for owl:Restriction (via rdf:type)
    if type_iris.iter().any(|t| *t == OWL_RESTRICTION) {
        return parse_restriction(triples, store);
    }

    // If it's typed as owl:Class but has no intersectionOf/unionOf/complementOf,
    // it might be a named class disguised as a blank node — not supported.
    let kind = crate::expression::is_owl_class_or_restriction(&type_iris);
    match kind {
        Some(BlankNodeKind::Class) => {
            Err("owl:Class blank node without intersectionOf/unionOf/complementOf".to_string())
        }
        Some(BlankNodeKind::Restriction) => parse_restriction(triples, store),
        None => Err(format!(
            "unknown blank node construct (types: {})",
            type_iris.join(", ")
        )),
    }
}

/// Parse a restriction from a blank node's triples.
fn parse_restriction(
    triples: &[FlatTriple],
    store: &TripleStore,
) -> Result<ClassExpression, String> {
    // Find owl:onProperty
    let mut property: Option<String> = None;
    let mut kind: Option<RestrictionKind> = None;

    for t in triples {
        match t.predicate.as_str() {
            OWL_ON_PROPERTY => {
                if let FlatTerm::Iri(iri) = &t.object {
                    property = Some(iri.clone());
                } else {
                    return Err("owl:onProperty must be a named IRI".to_string());
                }
            }
            OWL_SOME_VALUES_FROM => {
                let class_expr = parse_term_as_expression(&t.object, store)?;
                kind = Some(RestrictionKind::SomeValuesFrom(class_expr));
            }
            OWL_ALL_VALUES_FROM => {
                let class_expr = parse_term_as_expression(&t.object, store)?;
                kind = Some(RestrictionKind::AllValuesFrom(class_expr));
            }
            OWL_HAS_VALUE => {
                let obj = flat_term_to_object(&t.object);
                kind = Some(RestrictionKind::HasValue(obj));
            }
            OWL_CARDINALITY => {
                let n = parse_non_negative_integer(&t.object)?;
                kind = Some(RestrictionKind::ExactCardinality(n));
            }
            OWL_MIN_CARDINALITY => {
                let n = parse_non_negative_integer(&t.object)?;
                kind = Some(RestrictionKind::MinCardinality(n));
            }
            OWL_MAX_CARDINALITY => {
                let n = parse_non_negative_integer(&t.object)?;
                kind = Some(RestrictionKind::MaxCardinality(n));
            }
            _ => {}
        }
    }

    let property = property.ok_or("owl:Restriction missing owl:onProperty")?;
    let kind = kind.ok_or_else(|| {
        "owl:Restriction missing restriction kind (someValuesFrom/allValuesFrom/hasValue/cardinality)".to_string()
    })?;

    Ok(ClassExpression::Restriction(Box::new(Restriction {
        property,
        kind,
    })))
}

/// Parse an RDF list (rdf:first / rdf:rest chain) starting from a term.
///
/// Returns the list elements as FlatTerms. Follows the chain until rdf:nil.
fn parse_rdf_list(start: &FlatTerm, store: &TripleStore) -> Result<Vec<FlatTerm>, String> {
    let mut result = Vec::new();
    let mut current = start.clone();

    loop {
        match &current {
            FlatTerm::Iri(iri) if iri == RDF_NIL => break,
            FlatTerm::BlankNode(bn) => {
                let triples = store
                    .get(bn)
                    .ok_or_else(|| format!("RDF list node {bn} has no triples"))?;

                // Find rdf:first
                let mut found_first: Option<FlatTerm> = None;
                let mut found_rest: Option<FlatTerm> = None;

                for t in triples {
                    if t.predicate == RDF_FIRST {
                        found_first = Some(t.object.clone());
                    }
                    if t.predicate == RDF_REST {
                        found_rest = Some(t.object.clone());
                    }
                }

                let first =
                    found_first.ok_or_else(|| format!("RDF list node {bn} missing rdf:first"))?;
                result.push(first);

                current =
                    found_rest.ok_or_else(|| format!("RDF list node {bn} missing rdf:rest"))?;
            }
            FlatTerm::Iri(iri) => {
                // A named node where we expected a list node — could be
                // an implicit singleton or an error. Treat as end of list.
                return Err(format!("expected RDF list node but got IRI <{iri}>"));
            }
            FlatTerm::Literal(_) | FlatTerm::TypedLiteral { .. } => {
                return Err("RDF list node is a literal".to_string());
            }
        }
    }

    Ok(result)
}

/// Parse a FlatTerm as a ClassExpression.
///
/// - IRI → `ClassExpression::Named(iri)`
/// - BlankNode → recursively parse
/// - Literal → error (literals can't be class expressions)
fn parse_term_as_expression(
    term: &FlatTerm,
    store: &TripleStore,
) -> Result<ClassExpression, String> {
    match term {
        FlatTerm::Iri(iri) => Ok(ClassExpression::Named(iri.clone())),
        FlatTerm::BlankNode(bn) => parse_expression(bn, store),
        FlatTerm::Literal(_) | FlatTerm::TypedLiteral { .. } => {
            Err("literal cannot be a class expression".to_string())
        }
    }
}

/// Convert a FlatTerm to an `lbd_ontology::Object`.
fn flat_term_to_object(term: &FlatTerm) -> Object {
    match term {
        FlatTerm::Iri(iri) => Object::Iri(iri.clone()),
        FlatTerm::BlankNode(bn) => Object::Iri(bn.clone()),
        FlatTerm::Literal(value) => Object::Literal(value.clone()),
        FlatTerm::TypedLiteral { value, datatype } => Object::TypedLiteral {
            value: value.clone(),
            datatype: datatype.clone(),
        },
    }
}

/// Parse a typed literal as a non-negative integer (for cardinality).
fn parse_non_negative_integer(term: &FlatTerm) -> Result<usize, String> {
    let value = match term {
        FlatTerm::Literal(v) => v,
        FlatTerm::TypedLiteral { value, datatype } => {
            if datatype != XSD_NON_NEGATIVE_INTEGER
                && datatype != "http://www.w3.org/2001/XMLSchema#integer"
            {
                // Be lenient — still try to parse the value
            }
            value
        }
        FlatTerm::Iri(_) | FlatTerm::BlankNode(_) => {
            return Err("cardinality must be a literal integer".to_string());
        }
    };
    value
        .parse::<usize>()
        .map_err(|e| format!("invalid cardinality value '{value}': {e}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::is_owl_thing;

    const TTL_PREFIX: &str = r#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ex: <http://example.org/> .
"#;

    #[test]
    fn test_parse_equivalent_class_blank_node() {
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
        let (rules, warnings) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].inferred_class, "http://example.org/ExternalWall");
        assert!(warnings.is_empty());
        // Verify the expression tree
        match &rules[0].condition {
            ClassExpression::Intersection(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(
                    parts[0],
                    ClassExpression::Named("http://example.org/Wall".to_string())
                );
                match &parts[1] {
                    ClassExpression::Restriction(r) => {
                        assert_eq!(r.property, "http://example.org/isExternal");
                        match &r.kind {
                            RestrictionKind::SomeValuesFrom(expr) => {
                                assert!(is_owl_thing(expr));
                            }
                            _ => panic!("expected SomeValuesFrom"),
                        }
                    }
                    _ => panic!("expected Restriction"),
                }
            }
            _ => panic!("expected Intersection"),
        }
    }

    #[test]
    fn test_parse_named_equivalent_class_skipped() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:Wall owl:equivalentClass ex:WallType .
"#
        );
        let (rules, warnings) = parse_rules(&turtle, "").unwrap();
        // Named↔named should NOT generate a rule
        assert_eq!(rules.len(), 0);
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_parse_union_of() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:Opening owl:equivalentClass [
    a owl:Class ;
    owl:unionOf ( ex:Door ex:Window )
] .
"#
        );
        let (rules, _w) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 1);
        match &rules[0].condition {
            ClassExpression::Union(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(
                    parts[0],
                    ClassExpression::Named("http://example.org/Door".to_string())
                );
                assert_eq!(
                    parts[1],
                    ClassExpression::Named("http://example.org/Window".to_string())
                );
            }
            _ => panic!("expected Union"),
        }
    }

    #[test]
    fn test_parse_complement_of() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:NonWall owl:equivalentClass [
    a owl:Class ;
    owl:complementOf ex:Wall
] .
"#
        );
        let (rules, _w) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 1);
        match &rules[0].condition {
            ClassExpression::Complement(inner) => {
                assert_eq!(
                    **inner,
                    ClassExpression::Named("http://example.org/Wall".to_string())
                );
            }
            _ => panic!("expected Complement"),
        }
    }

    #[test]
    fn test_parse_has_value_literal() {
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
        let (rules, _w) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 1);
        match &rules[0].condition {
            ClassExpression::Intersection(parts) => match &parts[1] {
                ClassExpression::Restriction(r) => match &r.kind {
                    RestrictionKind::HasValue(Object::TypedLiteral { value, datatype }) => {
                        assert_eq!(value, "true");
                        assert_eq!(datatype, "http://www.w3.org/2001/XMLSchema#boolean");
                    }
                    _ => panic!("expected HasValue with TypedLiteral"),
                },
                _ => panic!("expected Restriction"),
            },
            _ => panic!("expected Intersection"),
        }
    }

    #[test]
    fn test_parse_cardinality() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:ExactlyTwoDoors owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Room
        [ a owl:Restriction ;
          owl:onProperty ex:hasDoor ;
          owl:cardinality "2"^^xsd:nonNegativeInteger ]
    )
] .
"#
        );
        let (rules, _w) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 1);
        match &rules[0].condition {
            ClassExpression::Intersection(parts) => match &parts[1] {
                ClassExpression::Restriction(r) => {
                    assert_eq!(r.kind, RestrictionKind::ExactCardinality(2));
                }
                _ => panic!("expected Restriction"),
            },
            _ => panic!("expected Intersection"),
        }
    }

    #[test]
    fn test_parse_min_max_cardinality() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:MultiDoorRoom owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Room
        [ a owl:Restriction ; owl:onProperty ex:hasDoor ; owl:minCardinality "2"^^xsd:nonNegativeInteger ]
        [ a owl:Restriction ; owl:onProperty ex:hasDoor ; owl:maxCardinality "5"^^xsd:nonNegativeInteger ]
    )
] .
"#
        );
        let (rules, _w) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 1);
        match &rules[0].condition {
            ClassExpression::Intersection(parts) => {
                assert_eq!(parts.len(), 3);
                match &parts[1] {
                    ClassExpression::Restriction(r) => {
                        assert_eq!(r.kind, RestrictionKind::MinCardinality(2));
                    }
                    _ => panic!("expected Restriction"),
                }
                match &parts[2] {
                    ClassExpression::Restriction(r) => {
                        assert_eq!(r.kind, RestrictionKind::MaxCardinality(5));
                    }
                    _ => panic!("expected Restriction"),
                }
            }
            _ => panic!("expected Intersection"),
        }
    }

    #[test]
    fn test_parse_all_values_from() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:AllWoodElements owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Element
        [ a owl:Restriction ;
          owl:onProperty ex:hasMaterial ;
          owl:allValuesFrom ex:Wood ]
    )
] .
"#
        );
        let (rules, _w) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 1);
        match &rules[0].condition {
            ClassExpression::Intersection(parts) => match &parts[1] {
                ClassExpression::Restriction(r) => match &r.kind {
                    RestrictionKind::AllValuesFrom(ClassExpression::Named(iri)) => {
                        assert_eq!(iri, "http://example.org/Wood");
                    }
                    _ => panic!("expected AllValuesFrom with Named"),
                },
                _ => panic!("expected Restriction"),
            },
            _ => panic!("expected Intersection"),
        }
    }

    #[test]
    fn test_parse_nested_expression() {
        // Deeply nested: intersection containing someValuesFrom → intersection → restriction
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:OwnedBuilding owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Building
        [ a owl:Restriction ;
          owl:onProperty ex:hasOwner ;
          owl:someValuesFrom [
              a owl:Class ;
              owl:intersectionOf (
                  ex:Owner
                  [ a owl:Restriction ;
                    owl:onProperty ex:hasName ;
                    owl:hasValue "ACME" ]
              )
          ] ]
    )
] .
"#
        );
        let (rules, _w) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 1);
        // Verify nesting depth
        match &rules[0].condition {
            ClassExpression::Intersection(parts) => {
                assert_eq!(parts.len(), 2);
                match &parts[1] {
                    ClassExpression::Restriction(r) => match &r.kind {
                        RestrictionKind::SomeValuesFrom(inner) => match inner {
                            ClassExpression::Intersection(inner_parts) => {
                                assert_eq!(inner_parts.len(), 2);
                                match &inner_parts[1] {
                                    ClassExpression::Restriction(inner_r) => match &inner_r.kind {
                                        RestrictionKind::HasValue(Object::Literal(v)) => {
                                            assert_eq!(v, "ACME");
                                        }
                                        _ => panic!("expected HasValue"),
                                    },
                                    _ => panic!("expected inner Restriction"),
                                }
                            }
                            _ => panic!("expected inner Intersection"),
                        },
                        _ => panic!("expected SomeValuesFrom"),
                    },
                    _ => panic!("expected Restriction"),
                }
            }
            _ => panic!("expected Intersection"),
        }
    }

    #[test]
    fn test_parse_unknown_construct_warns() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:Mystery owl:equivalentClass [
    a ex:UnknownThing ;
    ex:hasFoo "bar"
] .
"#
        );
        let (rules, warnings) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 0);
        assert!(!warnings.is_empty());
    }

    #[test]
    fn test_parse_empty_files() {
        let (rules, warnings) = parse_rules("", "").unwrap();
        assert!(rules.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_parse_some_values_from_named_class() {
        let turtle = format!(
            r#"{TTL_PREFIX}
ex:HasWoodElement owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Element
        [ a owl:Restriction ;
          owl:onProperty ex:hasMaterial ;
          owl:someValuesFrom ex:Wood ]
    )
] .
"#
        );
        let (rules, _w) = parse_rules(&turtle, "").unwrap();
        assert_eq!(rules.len(), 1);
        match &rules[0].condition {
            ClassExpression::Intersection(parts) => match &parts[1] {
                ClassExpression::Restriction(r) => match &r.kind {
                    RestrictionKind::SomeValuesFrom(ClassExpression::Named(iri)) => {
                        assert_eq!(iri, "http://example.org/Wood");
                    }
                    _ => panic!("expected SomeValuesFrom with Named"),
                },
                _ => panic!("expected Restriction"),
            },
            _ => panic!("expected Intersection"),
        }
    }

    #[test]
    fn test_rules_from_both_files() {
        let alignment = format!(
            r#"{TTL_PREFIX}
ex:A owl:equivalentClass [ a owl:Class ; owl:intersectionOf ( ex:X ) ] .
"#
        );
        let ontology = format!(
            r#"{TTL_PREFIX}
ex:B owl:equivalentClass [ a owl:Class ; owl:unionOf ( ex:Y ex:Z ) ] .
"#
        );
        let (rules, _w) = parse_rules(&alignment, &ontology).unwrap();
        assert_eq!(rules.len(), 2);
        let classes: Vec<&str> = rules.iter().map(|r| r.inferred_class.as_str()).collect();
        assert!(classes.contains(&"http://example.org/A"));
        assert!(classes.contains(&"http://example.org/B"));
    }
}
