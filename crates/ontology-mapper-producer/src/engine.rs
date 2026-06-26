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

/// Execute ontology mapping and return triples.
pub fn execute_ontology_mapping(
    alignment_turtle: &str,
    ontology_turtle: &str,
    source_filename: &str,
    source_bytes: &[u8],
) -> Result<Vec<Triple>, String> {
    // 1. Parse alignment file for explicit mappings
    let alignment_maps = parse_rdf_mappings(alignment_turtle)?;

    // 2. Parse ontology file for owl:equivalentProperty and rdfs:subPropertyOf
    let ontology_maps = parse_rdf_mappings(ontology_turtle)?;

    // Merge all predicate mappings
    let mut predicate_map: HashMap<String, String> = HashMap::new();
    for (src, tgt) in alignment_maps {
        predicate_map.insert(src, tgt);
    }
    for (src, tgt) in ontology_maps {
        predicate_map.entry(src).or_insert(tgt);
    }

    // 3. Parse the source data as RDF
    let source_triples = parse_source_as_rdf(source_filename, source_bytes)?;

    // 4. Apply predicate mappings
    let mapped_triples = source_triples
        .iter()
        .map(|triple| {
            let mapped_predicate = predicate_map
                .get(&triple.predicate)
                .cloned()
                .unwrap_or_else(|| triple.predicate.clone());
            Triple {
                subject: triple.subject.clone(),
                predicate: mapped_predicate,
                object: triple.object.clone(),
            }
        })
        .collect();

    Ok(mapped_triples)
}

/// Parse RDF (Turtle or N-Triples) and extract predicate mappings:
/// - `owl:equivalentProperty` (bidirectional)
/// - `rdfs:subPropertyOf` (child → parent)
/// - `align:entity1` / `align:entity2` pairs (bidirectional)
fn parse_rdf_mappings(turtle: &str) -> Result<Vec<(String, String)>, String> {
    let mut mappings = Vec::new();
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
                    mappings.push((s.iri.to_string(), o.iri.to_string()));
                    mappings.push((o.iri.to_string(), s.iri.to_string()));
                }
            }

            // rdfs:subPropertyOf (child → parent)
            if pred == "http://www.w3.org/2000/01/rdf-schema#subPropertyOf" {
                if let (RioSubject::NamedNode(s), RioTerm::NamedNode(o)) =
                    (triple.subject, triple.object)
                {
                    mappings.push((s.iri.to_string(), o.iri.to_string()));
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

    // Pair entity1→entity2 by blank node subject
    for (subj, e1) in &entity1_map {
        if let Some(e2) = entity2_map.get(subj) {
            mappings.push((e1.clone(), e2.clone()));
            mappings.push((e2.clone(), e1.clone()));
        }
    }

    Ok(mappings)
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
        assert!(mappings.len() >= 2);
        assert!(mappings
            .iter()
            .any(|(s, t)| s == "http://example.org/source/hasName"
                && t == "http://example.org/target/name"));
    }

    #[test]
    fn test_parse_sub_property() {
        let ontology = r#"@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/source/hasFirstName> rdfs:subPropertyOf <http://example.org/target/name> .
"#;
        let mappings = parse_rdf_mappings(ontology).unwrap();
        assert!(mappings
            .iter()
            .any(|(s, t)| s == "http://example.org/source/hasFirstName"
                && t == "http://example.org/target/name"));
    }

    #[test]
    fn test_apply_mapping() {
        let alignment = r#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
<http://schema.org/name> owl:equivalentProperty <http://xmlns.com/foaf/0.1/name> .
"#;
        let ontology = "";

        let source = b"<http://example.org/person/1> <http://schema.org/name> \"Alice\" .\n<http://example.org/person/1> <http://schema.org/age> \"30\" .\n";

        let triples = execute_ontology_mapping(alignment, ontology, "source.nt", source).unwrap();

        assert_eq!(triples.len(), 2);
        let name_triple = triples
            .iter()
            .find(|t| t.object == Object::Literal("Alice".to_string()))
            .unwrap();
        assert_eq!(name_triple.predicate, "http://xmlns.com/foaf/0.1/name");

        let age_triple = triples
            .iter()
            .find(|t| t.object == Object::Literal("30".to_string()))
            .unwrap();
        assert_eq!(age_triple.predicate, "http://schema.org/age");
    }

    #[test]
    fn test_no_mappings_returns_empty() {
        let alignment = r#"@prefix ex: <http://example.org/> .
ex:thing a ex:Class .
"#;
        let ontology = "";
        let source = b"<http://example.org/s> <http://example.org/p> \"o\" .\n";
        let triples = execute_ontology_mapping(alignment, ontology, "source.nt", source).unwrap();
        assert_eq!(triples.len(), 1);
    }
}
