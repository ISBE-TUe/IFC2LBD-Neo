//! Ontology mapping engine.
//!
//! Takes an alignment file (Turtle/RDF), an ontology file (Turtle/RDF),
//! and structured data input. Produces triples mapped to the target ontology.

use lbd_ontology::{Object, Triple};
use rio_api::model::{Subject as RioSubject, Term as RioTerm};
use rio_api::parser::TriplesParser;
use rio_turtle::{NTriplesParser, TurtleParser};
use std::collections::HashMap;
use std::io::{BufReader, Cursor};

/// Configuration for the ontology mapping.
#[derive(Clone, Debug)]
pub struct OntologyMappingConfig {
    /// Alignment file content (Turtle or RDF/XML)
    pub alignment_turtle: String,
    /// Ontology file content (Turtle or OWL)
    pub ontology_turtle: String,
}

/// Bidirectional mapping tables extracted from alignment + ontology files.
///
/// `property_map` maps predicates: if a triple has predicate X and X is in
/// the map, it is rewritten to property_map[X].
///
/// `class_map` maps classes for `rdf:type` triples: if a triple is
/// `<s> rdf:type <O>` and O is in the map, the object is rewritten to
/// class_map[O].
#[derive(Clone, Debug, Default)]
pub struct MappingTables {
    /// Predicate IRI → replacement predicate IRI (bidirectional)
    pub property_map: HashMap<String, String>,
    /// Class IRI → replacement class IRI (bidirectional, applied to rdf:type objects)
    pub class_map: HashMap<String, String>,
}

/// Build merged mapping tables from alignment + ontology file contents.
///
/// Parses both files and merges their mappings. Alignment entries take
/// priority (inserted first); ontology entries fill gaps (insert_or_insert_with).
pub fn build_mapping_tables(
    alignment_turtle: &str,
    ontology_turtle: &str,
) -> Result<MappingTables, String> {
    let alignment = parse_rdf_mappings(alignment_turtle)?;
    let ontology = parse_rdf_mappings(ontology_turtle)?;
    let mut tables = MappingTables::default();
    for (src, tgt) in alignment.property_maps {
        tables.property_map.insert(src, tgt);
    }
    for (src, tgt) in alignment.class_maps {
        tables.class_map.insert(src, tgt);
    }
    for (src, tgt) in ontology.property_maps {
        tables.property_map.entry(src).or_insert(tgt);
    }
    for (src, tgt) in ontology.class_maps {
        tables.class_map.entry(src).or_insert(tgt);
    }
    Ok(tables)
}

/// Parse RDF (Turtle or N-Triples) and extract property + class mappings:
/// - `owl:equivalentProperty` (bidirectional)
/// - `owl:equivalentClass` (bidirectional)
/// - `rdfs:subPropertyOf` (bidirectional)
/// - `rdfs:subClassOf` (bidirectional)
/// - `align:entity1` / `align:entity2` pairs (bidirectional)
///
/// Returns separate lists for property mappings and class mappings.
pub fn parse_rdf_mappings(turtle: &str) -> Result<RdfMappings, String> {
    let mut property_maps: Vec<(String, String)> = Vec::new();
    let mut class_maps: Vec<(String, String)> = Vec::new();
    let mut entity1_map: HashMap<String, String> = HashMap::new();
    let mut entity2_map: HashMap<String, String> = HashMap::new();

    let reader = BufReader::new(Cursor::new(turtle.as_bytes()));
    let base = oxiri::Iri::parse("http://ontology-mapper/base".to_string())
        .map_err(|e| format!("base IRI parse: {e}"))?;
    let mut parser = TurtleParser::new(reader, Some(base));

    parser
        .parse_all(&mut |triple| {
            let pred = triple.predicate.iri;

            // owl:equivalentProperty (bidirectional)
            if pred == "http://www.w3.org/2002/07/owl#equivalentProperty" {
                if let (RioSubject::NamedNode(s), RioTerm::NamedNode(o)) =
                    (triple.subject, triple.object)
                {
                    property_maps.push((s.iri.to_string(), o.iri.to_string()));
                    property_maps.push((o.iri.to_string(), s.iri.to_string()));
                }
            }

            // rdfs:subPropertyOf (bidirectional)
            if pred == "http://www.w3.org/2000/01/rdf-schema#subPropertyOf" {
                if let (RioSubject::NamedNode(s), RioTerm::NamedNode(o)) =
                    (triple.subject, triple.object)
                {
                    property_maps.push((s.iri.to_string(), o.iri.to_string()));
                    property_maps.push((o.iri.to_string(), s.iri.to_string()));
                }
            }

            // owl:equivalentClass (bidirectional)
            if pred == "http://www.w3.org/2002/07/owl#equivalentClass" {
                if let (RioSubject::NamedNode(s), RioTerm::NamedNode(o)) =
                    (triple.subject, triple.object)
                {
                    class_maps.push((s.iri.to_string(), o.iri.to_string()));
                    class_maps.push((o.iri.to_string(), s.iri.to_string()));
                }
            }

            // rdfs:subClassOf (bidirectional)
            if pred == "http://www.w3.org/2000/01/rdf-schema#subClassOf" {
                if let (RioSubject::NamedNode(s), RioTerm::NamedNode(o)) =
                    (triple.subject, triple.object)
                {
                    class_maps.push((s.iri.to_string(), o.iri.to_string()));
                    class_maps.push((o.iri.to_string(), s.iri.to_string()));
                }
            }

            // align:entity1 / align:entity2 (collect for pairing)
            let subj_key = match triple.subject {
                RioSubject::NamedNode(n) => n.iri.to_string(),
                RioSubject::BlankNode(b) => b.id.to_string(),
                _ => return Ok(()),
            };

            if pred == "http://knowledgeweb.semanticweb.org/heteroalignment#entity1" {
                if let RioTerm::NamedNode(o) = triple.object {
                    entity1_map.insert(subj_key, o.iri.to_string());
                }
            } else if pred == "http://knowledgeweb.semanticweb.org/heteroalignment#entity2" {
                if let RioTerm::NamedNode(o) = triple.object {
                    entity2_map.insert(subj_key, o.iri.to_string());
                }
            }

            Ok(()) as Result<(), Box<dyn std::error::Error>>
        })
        .map_err(|e| format!("RDF parse error: {e}"))?;

    // Pair entity1→entity2 by blank node subject (bidirectional).
    // Alignment entities are typically properties, so we put them in
    // property_maps. If the entities are classes, the user should use
    // owl:equivalentClass or rdfs:subClassOf directly in the alignment file.
    for (subj, e1) in &entity1_map {
        if let Some(e2) = entity2_map.get(subj) {
            property_maps.push((e1.clone(), e2.clone()));
            property_maps.push((e2.clone(), e1.clone()));
        }
    }

    Ok(RdfMappings {
        property_maps,
        class_maps,
    })
}

/// Intermediate result from parsing one RDF file.
struct RdfMappings {
    property_maps: Vec<(String, String)>,
    class_maps: Vec<(String, String)>,
}

/// Parse source data as RDF (N-Triples or Turtle).
fn parse_source_as_rdf(filename: &str, bytes: &[u8]) -> Result<Vec<Triple>, String> {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();

    match ext.as_str() {
        "nt" | "nq" => parse_ntriples(bytes),
        "ttl" | "turtle" | "n3" | "rdf" | "xml" | "owl" | "json" | "jsonld" | _ => {
            parse_turtle(bytes).or_else(|_| parse_ntriples(bytes))
        }
    }
}

fn parse_ntriples(bytes: &[u8]) -> Result<Vec<Triple>, String> {
    let mut triples = Vec::new();
    let reader = BufReader::new(Cursor::new(bytes));
    let mut parser = NTriplesParser::new(reader);

    parser
        .parse_all(&mut |triple| {
            triples.push(rio_triple_to_triple(triple));
            Ok(()) as Result<(), Box<dyn std::error::Error>>
        })
        .map_err(|e| format!("N-Triples parse error: {e}"))?;

    Ok(triples)
}

fn parse_turtle(bytes: &[u8]) -> Result<Vec<Triple>, String> {
    let mut triples = Vec::new();
    let reader = BufReader::new(Cursor::new(bytes));
    let base = oxiri::Iri::parse("http://source/base".to_string())
        .map_err(|e| format!("base IRI parse: {e}"))?;
    let mut parser = TurtleParser::new(reader, Some(base));

    parser
        .parse_all(&mut |triple| {
            triples.push(rio_triple_to_triple(triple));
            Ok(()) as Result<(), Box<dyn std::error::Error>>
        })
        .map_err(|e| format!("Turtle parse error: {e}"))?;

    Ok(triples)
}

fn rio_triple_to_triple(triple: rio_api::model::Triple) -> Triple {
    let subject = match triple.subject {
        RioSubject::NamedNode(n) => n.iri.to_string(),
        RioSubject::BlankNode(b) => format!("_:{}", b.id),
        _ => "http://unknown".to_string(),
    };

    let predicate = triple.predicate.iri.to_string();

    let object = match triple.object {
        RioTerm::NamedNode(n) => Object::Iri(n.iri.to_string()),
        RioTerm::BlankNode(b) => Object::Iri(format!("_:{}", b.id)),
        RioTerm::Literal(l) => match l {
            rio_api::model::Literal::Simple { value } => Object::Literal(value.to_string()),
            rio_api::model::Literal::LanguageTaggedString { value, language: _ } => {
                Object::TypedLiteral {
                    value: value.to_string(),
                    datatype: "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString".to_string(),
                }
            }
            rio_api::model::Literal::Typed { value, datatype } => Object::TypedLiteral {
                value: value.to_string(),
                datatype: datatype.iri.to_string(),
            },
        },
        _ => Object::Literal("unknown".to_string()),
    };

    Triple {
        subject,
        predicate,
        object,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_equivalent_property() {
        let alignment = r#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
<http://example.org/source/hasName> owl:equivalentProperty <http://example.org/target/name> .
"#;
        let mappings = parse_rdf_mappings(alignment).unwrap();
        assert!(mappings.property_maps.len() >= 2);
        assert!(mappings
            .property_maps
            .iter()
            .any(|(s, t)| s == "http://example.org/source/hasName"
                && t == "http://example.org/target/name"));
        // Bidirectional: reverse direction also present
        assert!(mappings
            .property_maps
            .iter()
            .any(|(s, t)| s == "http://example.org/target/name"
                && t == "http://example.org/source/hasName"));
    }

    #[test]
    fn test_parse_sub_property_bidirectional() {
        let ontology = r#"@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/source/hasFirstName> rdfs:subPropertyOf <http://example.org/target/name> .
"#;
        let mappings = parse_rdf_mappings(ontology).unwrap();
        // child → parent
        assert!(mappings
            .property_maps
            .iter()
            .any(|(s, t)| s == "http://example.org/source/hasFirstName"
                && t == "http://example.org/target/name"));
        // parent → child (bidirectional)
        assert!(mappings
            .property_maps
            .iter()
            .any(|(s, t)| s == "http://example.org/target/name"
                && t == "http://example.org/source/hasFirstName"));
    }

    #[test]
    fn test_parse_sub_class_bidirectional() {
        let alignment = r#"@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix bot: <https://w3id.org/bot#> .
@prefix saref: <https://saref.etsi.org/saref4bldg/> .
saref:Building rdfs:subClassOf bot:Building .
"#;
        let mappings = parse_rdf_mappings(alignment).unwrap();
        // saref:Building → bot:Building
        assert!(mappings
            .class_maps
            .iter()
            .any(|(s, t)| s == "https://saref.etsi.org/saref4bldg/Building"
                && t == "https://w3id.org/bot#Building"));
        // bot:Building → saref:Building (bidirectional)
        assert!(mappings
            .class_maps
            .iter()
            .any(|(s, t)| s == "https://w3id.org/bot#Building"
                && t == "https://saref.etsi.org/saref4bldg/Building"));
    }

    #[test]
    fn test_build_mapping_tables() {
        let alignment = r#"@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix bot: <https://w3id.org/bot#> .
@prefix saref: <https://saref.etsi.org/saref4bldg/> .
saref:hasSpace rdfs:subPropertyOf bot:containsZone .
saref:Building rdfs:subClassOf bot:Building .
"#;
        let ontology = "";
        let tables = build_mapping_tables(alignment, ontology).unwrap();
        // Property map: both directions
        assert_eq!(
            tables.property_map.get("https://saref.etsi.org/saref4bldg/hasSpace"),
            Some(&"https://w3id.org/bot#containsZone".to_string())
        );
        assert_eq!(
            tables.property_map.get("https://w3id.org/bot#containsZone"),
            Some(&"https://saref.etsi.org/saref4bldg/hasSpace".to_string())
        );
        // Class map: both directions
        assert_eq!(
            tables.class_map.get("https://saref.etsi.org/saref4bldg/Building"),
            Some(&"https://w3id.org/bot#Building".to_string())
        );
        assert_eq!(
            tables.class_map.get("https://w3id.org/bot#Building"),
            Some(&"https://saref.etsi.org/saref4bldg/Building".to_string())
        );
    }

    #[test]
    fn test_no_mappings_returns_empty() {
        let alignment = r#"@prefix ex: <http://example.org/> .
ex:thing a ex:Class .
"#;
        let ontology = "";
        let tables = build_mapping_tables(alignment, ontology).unwrap();
        assert!(tables.property_map.is_empty());
        assert!(tables.class_map.is_empty());
    }
}
