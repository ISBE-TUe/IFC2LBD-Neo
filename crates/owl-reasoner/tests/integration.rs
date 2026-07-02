//! Integration tests for the OWL reasoner.
//!
//! End-to-end tests: parse alignment → build index → infer → verify output.

use lbd_ontology::{Object, Triple};
use owl_reasoner::{infer_types, parse_rules};

fn make_triple(s: &str, p: &str, o: Object) -> Triple {
    Triple {
        subject: s.to_string(),
        predicate: p.to_string(),
        object: o,
    }
}

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

const TTL_HEADER: &str = r#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ex: <http://ex.org/> .
"#;

/// The user's example from the plan:
/// An IfcWall with isExternal=True should create an ont:ExternalWall.
///
/// This tests the full OPM property reification chain:
/// wall → props:isExternal → property_node → opm:hasPropertyState → state → schema:value → "true"
#[test]
fn test_opm_property_reification_external_wall() {
    let alignment = format!(
        r#"{TTL_HEADER}
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

    let (rules, warnings) = parse_rules(&alignment, "").unwrap();
    assert_eq!(rules.len(), 1);
    assert!(warnings.is_empty());

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
        // wall3: is a Wall but has no property at all
        make_triple(
            "http://ex.org/wall3",
            RDF_TYPE,
            Object::Iri("http://ex.org/Wall".to_string()),
        ),
    ];

    let inferred = infer_types(&rules, &triples);

    // Only wall1 should be inferred as ExternalWall
    assert_eq!(inferred.len(), 1);
    assert_eq!(inferred[0].subject, "http://ex.org/wall1");
    assert_eq!(
        inferred[0].object,
        Object::Iri("http://ex.org/ExternalWall".to_string())
    );
}

/// Simple hasValue restriction (direct property, not OPM reification).
#[test]
fn test_simple_has_value_external_wall() {
    let alignment = format!(
        r#"{TTL_HEADER}
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

    let (rules, _) = parse_rules(&alignment, "").unwrap();
    assert_eq!(rules.len(), 1);

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

/// The plan's example: OwnedBuilding = Building AND has ownerHistory.
#[test]
fn test_plan_example_owned_building() {
    let alignment = format!(
        r#"{TTL_HEADER}
ex:Building owl:equivalentClass ex:Bldg .
ex:OwnedBuilding owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Building
        [ a owl:Restriction ;
          owl:onProperty ex:ownerHistory ;
          owl:someValuesFrom owl:Thing ]
    )
] .
"#
    );

    let (rules, _) = parse_rules(&alignment, "").unwrap();
    // Only the blank-node equivalentClass generates a rule.
    // The named↔named (Building ↔ Bldg) is handled by simple mapping.
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].inferred_class, "http://ex.org/OwnedBuilding");

    let triples = vec![
        // building1: is a Building AND has ownerHistory
        make_triple(
            "http://ex.org/building1",
            RDF_TYPE,
            Object::Iri("http://ex.org/Building".to_string()),
        ),
        make_triple(
            "http://ex.org/building1",
            "http://ex.org/ownerHistory",
            Object::Iri("http://ex.org/history1".to_string()),
        ),
        // building2: is a Building but no ownerHistory
        make_triple(
            "http://ex.org/building2",
            RDF_TYPE,
            Object::Iri("http://ex.org/Building".to_string()),
        ),
    ];

    let inferred = infer_types(&rules, &triples);
    assert_eq!(inferred.len(), 1);
    assert_eq!(inferred[0].subject, "http://ex.org/building1");
    assert_eq!(
        inferred[0].object,
        Object::Iri("http://ex.org/OwnedBuilding".to_string())
    );
}

/// Dedup: inferred type already asserted → not duplicated.
#[test]
fn test_dedup_existing_type() {
    let alignment = format!(
        r#"{TTL_HEADER}
ex:SpecialWall owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf ( ex:Wall )
] .
"#
    );

    let (rules, _) = parse_rules(&alignment, "").unwrap();

    let triples = vec![
        make_triple(
            "http://ex.org/wall1",
            RDF_TYPE,
            Object::Iri("http://ex.org/Wall".to_string()),
        ),
        // wall1 already has rdf:type ex:SpecialWall
        make_triple(
            "http://ex.org/wall1",
            RDF_TYPE,
            Object::Iri("http://ex.org/SpecialWall".to_string()),
        ),
    ];

    let inferred = infer_types(&rules, &triples);
    assert!(inferred.is_empty());
}

/// Parse failure: malformed expression → simple mapping still works (no rules).
#[test]
fn test_parse_failure_no_rules() {
    let bad_alignment = "this is not valid turtle {{{";
    // Parse failure returns Err — the caller handles it gracefully
    let result = parse_rules(bad_alignment, "");
    assert!(result.is_err());
}

/// Rules from both alignment and ontology files.
#[test]
fn test_rules_from_both_files() {
    let alignment = format!(
        r#"{TTL_HEADER}
ex:ExternalWall owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Wall
        [ a owl:Restriction ; owl:onProperty ex:isExternal ; owl:hasValue "true"^^xsd:boolean ]
    )
] .
"#
    );
    let ontology = format!(
        r#"{TTL_HEADER}
ex:InternalWall owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Wall
        [ a owl:Restriction ; owl:onProperty ex:isExternal ; owl:hasValue "false"^^xsd:boolean ]
    )
] .
"#
    );

    let (rules, _) = parse_rules(&alignment, &ontology).unwrap();
    assert_eq!(rules.len(), 2);
}

/// Multiple rules applied to the same triple set.
#[test]
fn test_multiple_rules_same_triples() {
    let alignment = format!(
        r#"{TTL_HEADER}
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
ex:LoadBearing owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Wall
        [ a owl:Restriction ; owl:onProperty ex:loadBearing ; owl:hasValue "true"^^xsd:boolean ]
    )
] .
"#
    );

    let (rules, _) = parse_rules(&alignment, "").unwrap();
    assert_eq!(rules.len(), 3);

    let triples = vec![
        // wall1: external + load-bearing
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
            "http://ex.org/wall1",
            "http://ex.org/loadBearing",
            Object::TypedLiteral {
                value: "true".to_string(),
                datatype: "http://www.w3.org/2001/XMLSchema#boolean".to_string(),
            },
        ),
        // wall2: internal
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
    // wall1 → ExternalWall + LoadBearing, wall2 → InternalWall
    assert_eq!(inferred.len(), 3);

    let subjects: Vec<&str> = inferred.iter().map(|i| i.subject.as_str()).collect();
    assert!(subjects.iter().any(|&s| s == "http://ex.org/wall1"));
    assert!(subjects.iter().any(|&s| s == "http://ex.org/wall2"));

    let wall1_types: Vec<&Object> = inferred
        .iter()
        .filter(|i| i.subject == "http://ex.org/wall1")
        .map(|i| &i.object)
        .collect();
    assert_eq!(wall1_types.len(), 2);
}

/// Union of classes: Door OR Window → Opening.
#[test]
fn test_union_opening() {
    let alignment = format!(
        r#"{TTL_HEADER}
ex:Opening owl:equivalentClass [
    a owl:Class ;
    owl:unionOf ( ex:Door ex:Window )
] .
"#
    );

    let (rules, _) = parse_rules(&alignment, "").unwrap();
    assert_eq!(rules.len(), 1);

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

/// Complement: NonWall = NOT Wall.
#[test]
fn test_complement_non_wall() {
    let alignment = format!(
        r#"{TTL_HEADER}
ex:NonWall owl:equivalentClass [
    a owl:Class ;
    owl:complementOf ex:Wall
] .
"#
    );

    let (rules, _) = parse_rules(&alignment, "").unwrap();
    assert_eq!(rules.len(), 1);

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
        make_triple(
            "http://ex.org/c",
            RDF_TYPE,
            Object::Iri("http://ex.org/Door".to_string()),
        ),
    ];

    let inferred = infer_types(&rules, &triples);
    // b and c are NOT walls → NonWall
    assert_eq!(inferred.len(), 2);
    let subjects: Vec<&str> = inferred.iter().map(|i| i.subject.as_str()).collect();
    assert!(subjects.contains(&"http://ex.org/b"));
    assert!(subjects.contains(&"http://ex.org/c"));
    assert!(!subjects.contains(&"http://ex.org/a"));
}

/// Cardinality: exactly 2 doors → TwoDoorRoom.
#[test]
fn test_cardinality_two_door_room() {
    let alignment = format!(
        r#"{TTL_HEADER}
ex:TwoDoorRoom owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Room
        [ a owl:Restriction ; owl:onProperty ex:hasDoor ; owl:cardinality "2"^^xsd:nonNegativeInteger ]
    )
] .
"#
    );

    let (rules, _) = parse_rules(&alignment, "").unwrap();

    let triples = vec![
        // room1: 2 distinct doors
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
        // room2: 1 door
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
        // room3: 3 doors (with duplicate)
        make_triple(
            "http://ex.org/room3",
            RDF_TYPE,
            Object::Iri("http://ex.org/Room".to_string()),
        ),
        make_triple(
            "http://ex.org/room3",
            "http://ex.org/hasDoor",
            Object::Iri("http://ex.org/d4".to_string()),
        ),
        make_triple(
            "http://ex.org/room3",
            "http://ex.org/hasDoor",
            Object::Iri("http://ex.org/d5".to_string()),
        ),
        make_triple(
            "http://ex.org/room3",
            "http://ex.org/hasDoor",
            Object::Iri("http://ex.org/d6".to_string()),
        ),
        make_triple(
            "http://ex.org/room3",
            "http://ex.org/hasDoor",
            Object::Iri("http://ex.org/d4".to_string()),
        ), // duplicate
    ];

    let inferred = infer_types(&rules, &triples);
    // Only room1 has exactly 2 distinct doors
    assert_eq!(inferred.len(), 1);
    assert_eq!(inferred[0].subject, "http://ex.org/room1");
}

/// someValuesFrom with named class: element has material of type Wood.
#[test]
fn test_some_values_from_named_class() {
    let alignment = format!(
        r#"{TTL_HEADER}
ex:WoodenElement owl:equivalentClass [
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

    let (rules, _) = parse_rules(&alignment, "").unwrap();

    let triples = vec![
        // elem1: has wood material
        make_triple(
            "http://ex.org/elem1",
            RDF_TYPE,
            Object::Iri("http://ex.org/Element".to_string()),
        ),
        make_triple(
            "http://ex.org/elem1",
            "http://ex.org/hasMaterial",
            Object::Iri("http://ex.org/mat1".to_string()),
        ),
        make_triple(
            "http://ex.org/mat1",
            RDF_TYPE,
            Object::Iri("http://ex.org/Wood".to_string()),
        ),
        // elem2: has concrete material
        make_triple(
            "http://ex.org/elem2",
            RDF_TYPE,
            Object::Iri("http://ex.org/Element".to_string()),
        ),
        make_triple(
            "http://ex.org/elem2",
            "http://ex.org/hasMaterial",
            Object::Iri("http://ex.org/mat2".to_string()),
        ),
        make_triple(
            "http://ex.org/mat2",
            RDF_TYPE,
            Object::Iri("http://ex.org/Concrete".to_string()),
        ),
    ];

    let inferred = infer_types(&rules, &triples);
    assert_eq!(inferred.len(), 1);
    assert_eq!(inferred[0].subject, "http://ex.org/elem1");
    assert_eq!(
        inferred[0].object,
        Object::Iri("http://ex.org/WoodenElement".to_string())
    );
}

/// allValuesFrom: all materials are wood.
#[test]
fn test_all_values_from() {
    let alignment = format!(
        r#"{TTL_HEADER}
ex:AllWoodElement owl:equivalentClass [
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

    let (rules, _) = parse_rules(&alignment, "").unwrap();

    let triples = vec![
        // elem1: all wood
        make_triple(
            "http://ex.org/elem1",
            RDF_TYPE,
            Object::Iri("http://ex.org/Element".to_string()),
        ),
        make_triple(
            "http://ex.org/elem1",
            "http://ex.org/hasMaterial",
            Object::Iri("http://ex.org/mat1".to_string()),
        ),
        make_triple(
            "http://ex.org/mat1",
            RDF_TYPE,
            Object::Iri("http://ex.org/Wood".to_string()),
        ),
        make_triple(
            "http://ex.org/elem1",
            "http://ex.org/hasMaterial",
            Object::Iri("http://ex.org/mat2".to_string()),
        ),
        make_triple(
            "http://ex.org/mat2",
            RDF_TYPE,
            Object::Iri("http://ex.org/Wood".to_string()),
        ),
        // elem2: mixed
        make_triple(
            "http://ex.org/elem2",
            RDF_TYPE,
            Object::Iri("http://ex.org/Element".to_string()),
        ),
        make_triple(
            "http://ex.org/elem2",
            "http://ex.org/hasMaterial",
            Object::Iri("http://ex.org/mat3".to_string()),
        ),
        make_triple(
            "http://ex.org/mat3",
            RDF_TYPE,
            Object::Iri("http://ex.org/Wood".to_string()),
        ),
        make_triple(
            "http://ex.org/elem2",
            "http://ex.org/hasMaterial",
            Object::Iri("http://ex.org/mat4".to_string()),
        ),
        make_triple(
            "http://ex.org/mat4",
            RDF_TYPE,
            Object::Iri("http://ex.org/Concrete".to_string()),
        ),
    ];

    let inferred = infer_types(&rules, &triples);
    assert_eq!(inferred.len(), 1);
    assert_eq!(inferred[0].subject, "http://ex.org/elem1");
}

/// hasValue with IRI: element has specific owner.
#[test]
fn test_has_value_iri() {
    let alignment = format!(
        r#"{TTL_HEADER}
ex:AcmeOwned owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Element
        [ a owl:Restriction ;
          owl:onProperty ex:hasOwner ;
          owl:hasValue ex:ACME ]
    )
] .
"#
    );

    let (rules, _) = parse_rules(&alignment, "").unwrap();

    let triples = vec![
        make_triple(
            "http://ex.org/elem1",
            RDF_TYPE,
            Object::Iri("http://ex.org/Element".to_string()),
        ),
        make_triple(
            "http://ex.org/elem1",
            "http://ex.org/hasOwner",
            Object::Iri("http://ex.org/ACME".to_string()),
        ),
        make_triple(
            "http://ex.org/elem2",
            RDF_TYPE,
            Object::Iri("http://ex.org/Element".to_string()),
        ),
        make_triple(
            "http://ex.org/elem2",
            "http://ex.org/hasOwner",
            Object::Iri("http://ex.org/OtherCorp".to_string()),
        ),
    ];

    let inferred = infer_types(&rules, &triples);
    assert_eq!(inferred.len(), 1);
    assert_eq!(inferred[0].subject, "http://ex.org/elem1");
}

/// minCardinality: room with at least 2 doors.
#[test]
fn test_min_cardinality() {
    let alignment = format!(
        r#"{TTL_HEADER}
ex:MultiDoorRoom owl:equivalentClass [
    a owl:Class ;
    owl:intersectionOf (
        ex:Room
        [ a owl:Restriction ; owl:onProperty ex:hasDoor ; owl:minCardinality "2"^^xsd:nonNegativeInteger ]
    )
] .
"#
    );

    let (rules, _) = parse_rules(&alignment, "").unwrap();

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
