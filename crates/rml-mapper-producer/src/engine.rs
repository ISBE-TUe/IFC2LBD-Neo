//! RML engine — adapted from worker-rml-rust/src/main.rs.
//!
//! This module wraps the `rml_mapper_lib` library, executing RML mappings on
//! structured data files and streaming RDF triples through a channel.
//!
//! The execution flow:
//! 1. Parse mapping Turtle into InMemoryQuadStore (via rio_turtle)
//! 2. Conform mapping (old RML namespace → W3C RML)
//! 3. Create mapping document via MappingFactory
//! 4. Execute mapping via Executor (streams Quads through a channel)
//! 5. Convert Quads to pipeline Triple structs and forward through channel

use std::io::Cursor;

#[cfg(not(target_family = "wasm"))]
use tempfile::TempDir;

use crossbeam::channel::Sender;
use lbd_ontology::{Object, Triple};
use rml_mapper_lib::{
    conformer::MappingConformer,
    executor::Executor,
    mapping::{MappingFactory, StrictMode},
    store::{InMemoryQuadStore, QuadStore, RdfFormat},
    term::{Quad, Term, TermRef},
};

/// Execute an RML mapping and stream triples through a channel.
///
/// This is the primary entry point for the pipeline producer. It:
/// - Parses the mapping Turtle into a quad store
/// - Conforms the mapping (old RML → W3C RML)
/// - Executes the mapping, streaming Quads through an internal channel
/// - Converts Quads to `lbd_ontology::Triple` structs in a background thread
/// - Sends batches of Triples through `triple_sender`
///
/// # Arguments
///
/// * `mapping_turtle` - RML mapping document in Turtle format
/// * `source_filename` - Filename of the source data file
/// * `source_bytes` - Raw bytes of the source data file
/// * `triple_sender` - Channel sender for batches of triples
/// * `batch_size` - Number of triples per batch sent through the channel
pub fn execute_rml_streaming(
    mapping_turtle: &str,
    source_filename: &str,
    source_bytes: &[u8],
    triple_sender: &Sender<Vec<Triple>>,
    batch_size: usize,
) -> Result<(), String> {
    #[cfg(not(target_family = "wasm"))]
    let temp_dir = TempDir::new().map_err(|e| format!("temp dir: {e}"))?;
    #[cfg(not(target_family = "wasm"))]
    let work_dir = temp_dir.path().to_path_buf();
    #[cfg(target_family = "wasm")]
    let work_dir = std::path::PathBuf::new(); // no filesystem on WASM

    // Write source file to temp directory
    #[cfg(not(target_family = "wasm"))]
    {
        let source_path = work_dir.join(source_filename);
        std::fs::write(&source_path, source_bytes).map_err(|e| format!("write source: {e}"))?;
    };

    // Replace placeholder source filenames in mapping with actual filename
    let mapping = prepare_mapping_for_source(mapping_turtle, source_filename);

    // Parse mapping into quad store
    // Use a base IRI to resolve relative IRIs in the mapping (e.g. <#TriplesMap>)
    let mut mapping_store = InMemoryQuadStore::new();
    let cursor = Cursor::new(mapping.as_bytes());
    mapping_store
        .read(cursor, Some("http://rml-mapper/base"), RdfFormat::Turtle)
        .map_err(|e| format!("parse mapping: {e}"))?;

    // Conform mapping (old RML namespace → W3C RML)
    let mut conformer = MappingConformer::new(mapping_store, None);
    conformer.conform().map_err(|e| format!("conform: {e}"))?;
    let mapping_store = conformer.into_store();

    // Create mapping document
    let factory = MappingFactory::new(None, StrictMode::BestEffort);
    let mapping_doc = factory
        .create_mapping(&mapping_store)
        .map_err(|e| format!("create mapping: {e}"))?;

    // Channel between executor (produces Quads) and our converter (produces Triples)
    let (quad_tx, quad_rx) = crossbeam::channel::bounded(batch_size * 2);

    let mut executor =
        Executor::new(mapping_doc, work_dir, StrictMode::BestEffort).with_output_sender(quad_tx);

    // Spawn a thread to convert Quads to Triples
    let sender = triple_sender.clone();
    let converter = std::thread::spawn(move || {
        let mut batch = Vec::with_capacity(batch_size);
        for quads in quad_rx {
            for quad in quads {
                let triple = convert_quad_to_triple(&quad);
                batch.push(triple);
                if batch.len() >= batch_size && sender.send(std::mem::take(&mut batch)).is_err() {
                    return;
                }
            }
        }
        if !batch.is_empty() {
            let _ = sender.send(batch);
        }
    });

    executor.execute().map_err(|e| format!("execute: {e}"))?;

    // Drop the executor to close the quad channel, signaling the converter thread.
    drop(executor);
    converter
        .join()
        .map_err(|_| "converter thread panicked".to_string())?;

    Ok(())
}

/// Execute an RML mapping and return N-Triples bytes.
///
/// This is a backward-compatibility wrapper around `execute_rml_streaming`
/// that collects all triples and serializes them as N-Triples.
pub fn execute_rml(
    mapping_turtle: &str,
    source_filename: &str,
    source_bytes: &[u8],
) -> Result<Vec<u8>, String> {
    let (tx, rx) = crossbeam::channel::unbounded();

    execute_rml_streaming(mapping_turtle, source_filename, source_bytes, &tx, 1024)?;
    drop(tx); // close the channel so rx iteration terminates

    let mut triples = Vec::new();
    for batch in rx {
        triples.extend(batch);
    }

    Ok(serialize_triples_as_ntriples(&triples))
}

/// Serializes a slice of `Triple` as N-Triples bytes.
fn serialize_triples_as_ntriples(triples: &[Triple]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(triples.len() * 128);
    for triple in triples {
        // Subject
        if triple.subject.starts_with("_:") {
            buf.extend_from_slice(triple.subject.as_bytes());
        } else {
            buf.push(b'<');
            buf.extend_from_slice(triple.subject.as_bytes());
            buf.push(b'>');
        }
        buf.push(b' ');

        // Predicate
        buf.push(b'<');
        buf.extend_from_slice(triple.predicate.as_bytes());
        buf.push(b'>');
        buf.push(b' ');

        // Object
        match &triple.object {
            Object::Iri(iri) => {
                buf.push(b'<');
                buf.extend_from_slice(iri.as_bytes());
                buf.push(b'>');
            }
            Object::Literal(value) => {
                buf.push(b'"');
                write_escaped_string(&mut buf, value);
                buf.push(b'"');
            }
            Object::TypedLiteral { value, datatype } => {
                buf.push(b'"');
                write_escaped_string(&mut buf, value);
                buf.extend_from_slice(b"\"^^<");
                buf.extend_from_slice(datatype.as_bytes());
                buf.push(b'>');
            }
        }

        buf.extend_from_slice(b" .\n");
    }
    buf
}

/// Writes an escaped string for N-Triples format.
fn write_escaped_string(buf: &mut Vec<u8>, s: &str) {
    for c in s.chars() {
        match c {
            '\\' => buf.extend_from_slice(b"\\\\"),
            '"' => buf.extend_from_slice(b"\\\""),
            '\n' => buf.extend_from_slice(b"\\n"),
            '\r' => buf.extend_from_slice(b"\\r"),
            '\t' => buf.extend_from_slice(b"\\t"),
            c if c.is_control() => {
                buf.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes());
            }
            c => {
                let mut tmp = [0u8; 4];
                let s = c.encode_utf8(&mut tmp);
                buf.extend_from_slice(s.as_bytes());
            }
        }
    }
}

/// Converts an `rml_mapper_lib::term::Quad` to an `lbd_ontology::Triple`.
fn convert_quad_to_triple(quad: &Quad) -> Triple {
    Triple {
        subject: convert_term_to_subject_string(quad.subject()),
        predicate: quad.predicate().iri().to_string(),
        object: convert_term_to_object(quad.object()),
    }
}

/// Converts a `TermRef` to a subject string (IRI or blank node label).
fn convert_term_to_subject_string(term: &TermRef) -> String {
    match term {
        TermRef::NamedNode(n) => n.iri().to_string(),
        TermRef::BlankNode(b) => format!("_:{}", b.id()),
        TermRef::Literal(_) => {
            // Literals cannot be subjects; this should not happen in valid RDF
            term.value().to_string()
        }
    }
}

/// Converts a `TermRef` to an `lbd_ontology::Object`.
fn convert_term_to_object(term: &TermRef) -> Object {
    match term {
        TermRef::NamedNode(n) => Object::Iri(n.iri().to_string()),
        TermRef::BlankNode(b) => Object::Iri(format!("_:{}", b.id())),
        TermRef::Literal(l) => {
            if let Some(_lang) = l.language() {
                // Language-tagged literal → rdf:langString datatype
                Object::TypedLiteral {
                    value: l.value().to_string(),
                    datatype: "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString".to_string(),
                }
            } else if let Some(datatype) = l.datatype() {
                Object::TypedLiteral {
                    value: l.value().to_string(),
                    datatype: datatype.to_string(),
                }
            } else {
                Object::Literal(l.value().to_string())
            }
        }
    }
}

/// Replace placeholder source filenames in mapping with actual filename.
///
/// From worker-rml-rust/src/main.rs.
fn prepare_mapping_for_source(mapping: &str, source_filename: &str) -> String {
    const PLACEHOLDERS: &[&str] = &[
        "source.xml",
        "source.json",
        "source.csv",
        "data.xml",
        "data.json",
        "data.csv",
        "input.xml",
        "input.json",
        "input.csv",
    ];
    let mut result = mapping.to_string();
    for placeholder in PLACEHOLDERS {
        if result.contains(*placeholder)
            && *placeholder != source_filename
            && !source_filename.contains(*placeholder)
        {
            result = result.replace(*placeholder, source_filename);
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execute_rml_streaming_csv() {
        // Simple RML mapping for a CSV source
        let mapping = r#"
@prefix rml: <http://w3id.org/rml/> .
@prefix ql: <http://semweb.mmlab.be/ns/ql#> .
@prefix ex: <http://example.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .

<#TriplesMap> a rml:TriplesMap ;
  rml:logicalSource [
    rml:source "source.csv" ;
    rml:referenceFormulation ql:CSV ;
  ] ;
  rml:subjectMap [
    rml:template "http://example.org/person/{id}" ;
    rml:class foaf:Person ;
  ] ;
  rml:predicateObjectMap [
    rml:predicateMap [ rml:constant foaf:name ] ;
    rml:objectMap [ rml:reference "name" ] ;
  ] .
"#;

        let csv = b"id,name\n1,Alice\n2,Bob\n";

        let (tx, rx) = crossbeam::channel::unbounded();
        execute_rml_streaming(mapping, "source.csv", csv, &tx, 10).unwrap();
        drop(tx); // close the channel so rx iteration terminates

        let triples: Vec<Triple> = rx.into_iter().flatten().collect();

        // 2 records × 2 quads (rdf:type + name) = 4 triples
        assert_eq!(triples.len(), 4);

        // Check for rdf:type triples
        let type_triples: Vec<_> = triples
            .iter()
            .filter(|t| t.predicate == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type")
            .collect();
        assert_eq!(type_triples.len(), 2);
    }

    #[test]
    fn test_execute_rml_backward_compat() {
        let mapping = r#"
@prefix rml: <http://w3id.org/rml/> .
@prefix ql: <http://semweb.mmlab.be/ns/ql#> .
@prefix ex: <http://example.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .

<#TriplesMap> a rml:TriplesMap ;
  rml:logicalSource [
    rml:source "source.csv" ;
    rml:referenceFormulation ql:CSV ;
  ] ;
  rml:subjectMap [
    rml:template "http://example.org/person/{id}" ;
  ] ;
  rml:predicateObjectMap [
    rml:predicateMap [ rml:constant foaf:name ] ;
    rml:objectMap [ rml:reference "name" ] ;
  ] .
"#;

        let csv = b"id,name\n1,Alice\n";

        let ntriples = execute_rml(mapping, "source.csv", csv).unwrap();
        let text = std::str::from_utf8(&ntriples).unwrap();

        // Should contain one triple
        assert!(text.contains("<http://example.org/person/1>"));
        assert!(text.contains("<http://xmlns.com/foaf/0.1/name>"));
        assert!(text.contains("Alice"));
    }

    #[test]
    fn test_prepare_mapping_for_source() {
        let mapping = r#"<#TM> rml:source "source.csv" ."#;
        let result = prepare_mapping_for_source(mapping, "data.csv");
        assert!(result.contains("data.csv"));
        assert!(!result.contains("source.csv"));
    }

    #[test]
    fn test_serialize_triples_as_ntriples() {
        let triples = vec![
            Triple {
                subject: "http://example.org/s".to_string(),
                predicate: "http://example.org/p".to_string(),
                object: Object::Literal("hello".to_string()),
            },
            Triple {
                subject: "_:b1".to_string(),
                predicate: "http://example.org/p".to_string(),
                object: Object::Iri("http://example.org/o".to_string()),
            },
        ];

        let bytes = serialize_triples_as_ntriples(&triples);
        let text = std::str::from_utf8(&bytes).unwrap();

        assert!(text.contains("<http://example.org/s> <http://example.org/p> \"hello\" ."));
        assert!(text.contains("_:b1 <http://example.org/p> <http://example.org/o> ."));
    }

    #[test]
    fn test_serialize_triples_with_typed_literal() {
        let triples = vec![Triple {
            subject: "http://example.org/s".to_string(),
            predicate: "http://example.org/age".to_string(),
            object: Object::TypedLiteral {
                value: "30".to_string(),
                datatype: "http://www.w3.org/2001/XMLSchema#integer".to_string(),
            },
        }];

        let bytes = serialize_triples_as_ntriples(&triples);
        let text = std::str::from_utf8(&bytes).unwrap();

        assert!(text.contains("\"30\"^^<http://www.w3.org/2001/XMLSchema#integer>"));
    }

    #[test]
    fn test_serialize_triples_with_escapes() {
        let triples = vec![Triple {
            subject: "http://example.org/s".to_string(),
            predicate: "http://example.org/p".to_string(),
            object: Object::Literal("hello\nworld\"quote".to_string()),
        }];

        let bytes = serialize_triples_as_ntriples(&triples);
        let text = std::str::from_utf8(&bytes).unwrap();

        assert!(text.contains("hello\\nworld\\\"quote"));
    }
}
