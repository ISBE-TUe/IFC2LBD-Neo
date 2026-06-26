//! RML Mapper Producer Plugin for IFC2LBD-Neo.
//!
//! Executes RML mappings to transform structured data (JSON, CSV, XML) into
//! RDF triples. The RML mapping engine is reused from the `rml-mapper-lib`
//! crate (adapted from `worker-rml-rust`).
//!
//! # Pipeline role
//!
//! - Stage: Produce
//! - Named graph slug: `rml` (→ graph IRI `{base_uri}/rml`)
//! - Reads: `StructuredDataInput` and `RmlMappingConfig` from `PipelineContext`
//! - Emits: RDF triples via `TaggedBatch` channel
//! - Failure policy: Required
//!
//! # Registration
//!
//! Register in both runners:
//!
//! ```rust,ignore
//! registry.register_producer(RmlMapperProducerPlugin).unwrap();
//! ```

use crossbeam::channel::Sender;
use lbd_converter::ConvertOptions;
use lbd_ontology::{Object, Triple};
use lbd_pipeline::{
    FailurePolicy, ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage,
    PluginManifest, ProducerError, ProducerPlugin, TaggedBatch, BatchKind,
};
use structured_data::{RmlMappingConfig, StructuredDataInput};

mod engine;
mod forward;

pub use engine::execute_rml;

/// Plugin ID — must be unique across all registered modules.
pub const RML_MAPPER_ID: &str = "neo-rml-mapper";

/// Graph URL slug — appended to `{base_uri}/` to form this module's named-graph IRI.
const GRAPH_SLUG: &str = "rml";

/// The RML mapper producer plugin.
pub struct RmlMapperProducerPlugin;

impl PipelinePlugin for RmlMapperProducerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: RML_MAPPER_ID,
            display_name: "RML Mapper",
            stage: PipelineStage::Produce,
            description: "Transforms structured data (JSON/CSV/XML) into RDF triples using RML mappings.",
            inputs: vec!["structured-data", "rml-mapping"],
            outputs: vec!["rml-triples"],
            requires: vec!["structured-data"],
            conflicts_with: vec![],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::ParallelByBatch,
            wasm_compatible: true,
            named_graph_slug: Some(GRAPH_SLUG),
            needs_full_graph: false,
        }
    }
}

impl ProducerPlugin for RmlMapperProducerPlugin {
    fn produce(
        &self,
        ctx: &PipelineContext,
        sender: &Sender<TaggedBatch>,
    ) -> Result<(), ProducerError> {
        let data = ctx
            .get::<StructuredDataInput>()
            .ok_or_else(|| ProducerError::Conversion("No structured data input".into()))?;

        let mapping_config = ctx
            .get::<RmlMappingConfig>()
            .ok_or_else(|| ProducerError::Conversion("No RML mapping config".into()))?;

        let options = ctx
            .get::<ConvertOptions>()
            .ok_or_else(|| ProducerError::Conversion("No ConvertOptions in context".into()))?;

        let graph_iri = BatchKind::new(format!(
            "{}{}",
            options.base_uri.trim_end_matches('/'),
            GRAPH_SLUG,
        ));

        let (raw_sender, raw_receiver) =
            crossbeam::channel::bounded(ctx.resource_limits.channel_capacity);

        // Forward raw triples as TaggedBatch with our graph IRI.
        forward::forward_as_tagged(raw_receiver, graph_iri, sender.clone());

        // Execute RML mapping for each input file.
        for file in &data.files {
            let ntriples_bytes = engine::execute_rml(
                &mapping_config.mapping_turtle,
                &file.filename,
                &file.bytes,
            )
            .map_err(ProducerError::Conversion)?;

            // Parse N-Triples output into Triple structs and send through channel.
            let triples = parse_ntriples(&ntriples_bytes);
            raw_sender
                .send(triples)
                .map_err(|_| ProducerError::ChannelClosed)?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// N-Triples parser — converts RML engine output into pipeline Triple structs
// ---------------------------------------------------------------------------

/// Parse N-Triples bytes into a Vec<Triple>.
fn parse_ntriples(bytes: &[u8]) -> Vec<Triple> {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .filter_map(|line| parse_ntriples_line(line.trim()))
        .collect()
}

fn parse_ntriples_line(line: &str) -> Option<Triple> {
    let line = line.trim_end_matches('.').trim();

    // Parse subject (IRI or blank node)
    let (subject, rest) = if line.starts_with('<') {
        let end = line.find('>')?;
        (line[1..end].to_string(), &line[end + 1..])
    } else if line.starts_with("_:") {
        let end = line.find(' ')?;
        (line[..end].to_string(), &line[end..])
    } else {
        return None;
    };

    // Parse predicate (IRI)
    let rest = rest.trim_start();
    if !rest.starts_with('<') {
        return None;
    }
    let pred_end = rest.find('>')?;
    let predicate = rest[1..pred_end].to_string();
    let rest = rest[pred_end + 1..].trim();

    // Parse object (IRI, literal, or typed literal)
    let object = if rest.starts_with('<') {
        let end = rest.find('>')?;
        Object::Iri(rest[1..end].to_string())
    } else if rest.starts_with('"') {
        let mut end = 1;
        let bytes = rest.as_bytes();
        while end < bytes.len() {
            if bytes[end] == b'\\' {
                end += 2;
                continue;
            }
            if bytes[end] == b'"' {
                break;
            }
            end += 1;
        }
        let value = unescape_ntriples(&rest[1..end]);
        let after = rest[end + 1..].trim();

        if after.starts_with("^^<") {
            let dt_end = after[3..].find('>')?;
            let datatype = after[3..3 + dt_end].to_string();
            Object::TypedLiteral { value, datatype }
        } else if after.starts_with('@') {
            Object::TypedLiteral {
                value,
                datatype: "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString".to_string(),
            }
        } else {
            Object::Literal(value)
        }
    } else {
        return None;
    };

    Some(Triple {
        subject,
        predicate,
        object,
    })
}

/// Unescape N-Triples literal escapes.
fn unescape_ntriples(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('r') => result.push('\r'),
                Some('t') => result.push('\t'),
                Some('"') => result.push('"'),
                Some('\\') => result.push('\\'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(code) {
                            result.push(ch);
                        }
                    }
                }
                Some('U') => {
                    let hex: String = chars.by_ref().take(8).collect();
                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(code) {
                            result.push(ch);
                        }
                    }
                }
                _ => {}
            }
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_triple() {
        let nq = b"<http://example.org/person/1> <http://schema.org/name> \"Alice\" .\n";
        let triples = parse_ntriples(nq);
        assert_eq!(triples.len(), 1);
        assert_eq!(triples[0].subject, "http://example.org/person/1");
        assert_eq!(triples[0].predicate, "http://schema.org/name");
        match &triples[0].object {
            Object::Literal(v) => assert_eq!(v, "Alice"),
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn test_parse_typed_literal() {
        let nq = b"<http://example.org/person/1> <http://schema.org/age> \"30\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n";
        let triples = parse_ntriples(nq);
        assert_eq!(triples.len(), 1);
        match &triples[0].object {
            Object::TypedLiteral { value, datatype } => {
                assert_eq!(value, "30");
                assert_eq!(datatype, "http://www.w3.org/2001/XMLSchema#integer");
            }
            _ => panic!("expected typed literal"),
        }
    }

    #[test]
    fn test_parse_iri_object() {
        let nq = b"<http://example.org/person/1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.org/Person> .\n";
        let triples = parse_ntriples(nq);
        assert_eq!(triples.len(), 1);
        match &triples[0].object {
            Object::Iri(iri) => assert_eq!(iri, "http://example.org/Person"),
            _ => panic!("expected IRI"),
        }
    }

    #[test]
    fn test_parse_multiple_lines() {
        let nq = b"<a:s> <a:p> \"one\" .\n<b:s> <b:p> \"two\" .\n";
        let triples = parse_ntriples(nq);
        assert_eq!(triples.len(), 2);
    }

    #[test]
    fn test_skip_empty_and_comments() {
        let nq = b"\n# comment\n<a:s> <a:p> \"x\" .\n";
        let triples = parse_ntriples(nq);
        assert_eq!(triples.len(), 1);
    }

    #[test]
    fn test_escape_sequences() {
        let nq = b"<a:s> <a:p> \"hello\\nworld\" .\n";
        let triples = parse_ntriples(nq);
        assert_eq!(triples.len(), 1);
        match &triples[0].object {
            Object::Literal(v) => assert_eq!(v, "hello\nworld"),
            _ => panic!("expected literal"),
        }
    }
}
