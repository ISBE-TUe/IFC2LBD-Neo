//! Turtle serialization for the first LBD conversion slice.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::io::{self, Write};

use crossbeam::channel::Receiver;
use lbd_ontology::{Object, Triple, PREFIXES};

const RDF_TYPE_IRI: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

#[derive(Debug, thiserror::Error)]
pub enum SerializerError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

pub fn serialize_turtle_to_writer<W: Write>(
    triples: &[Triple],
    mut writer: W,
) -> Result<(), SerializerError> {
    write_grouped_turtle(triples, &mut writer, None, true)
}

pub fn serialize_turtle_to_string(triples: &[Triple]) -> Result<String, SerializerError> {
    let mut buffer = Vec::new();
    serialize_turtle_to_writer(triples, &mut buffer)?;
    Ok(String::from_utf8(buffer).expect("serializer output is valid UTF-8"))
}

/// Stream IfcOWL triples to `writer` with prefix abbreviation.
///
/// Unlike the LBD serializer this path does **not** collect all triples before
/// writing; instead it writes each triple immediately so peak RAM stays low even
/// for very large IfcOWL outputs. Subject-grouping (`;` syntax) is therefore
/// skipped, but the dominant size saving comes from prefix abbreviation.
///
/// All IRI components are written directly as bytes to avoid per-triple heap
/// allocations; the BufWriter handles write coalescing.
pub fn serialize_turtle_batches_to_writer<W: Write>(
    receiver: Receiver<Vec<Triple>>,
    mut writer: W,
    instance_base: Option<&str>,
) -> Result<(), SerializerError> {
    write_prefixes(&mut writer, instance_base, &[], false)?;
    for batch in receiver {
        for triple in &batch {
            write_iri_compact(&mut writer, &triple.subject, instance_base)?;
            writer.write_all(b" ")?;
            write_iri_compact(&mut writer, &triple.predicate, instance_base)?;
            writer.write_all(b" ")?;
            write_object_compact(&mut writer, &triple.object, instance_base)?;
            writer.write_all(b" .\n")?;
        }
    }
    Ok(())
}

/// Write an IRI as a prefixed name or `<full-iri>` directly to `writer`
/// without any heap allocation.
fn write_iri_compact<W: Write>(
    writer: &mut W,
    iri: &str,
    instance_base: Option<&str>,
) -> io::Result<()> {
    if iri == RDF_TYPE_IRI {
        return writer.write_all(b"a");
    }
    if let Some(base) = instance_base {
        let base = ensure_trailing_slash(base);
        if let Some(local) = iri.strip_prefix(base) {
            if is_valid_prefixed_local(local) {
                writer.write_all(b"inst:")?;
                return writer.write_all(local.as_bytes());
            }
        }
    }
    for (prefix, namespace) in PREFIXES {
        if let Some(local) = iri.strip_prefix(namespace) {
            if is_valid_prefixed_local(local) {
                writer.write_all(prefix.as_bytes())?;
                writer.write_all(b":")?;
                return writer.write_all(local.as_bytes());
            }
        }
    }
    writer.write_all(b"<")?;
    writer.write_all(iri.as_bytes())?;
    writer.write_all(b">")
}

fn write_object_compact<W: Write>(
    writer: &mut W,
    object: &Object,
    instance_base: Option<&str>,
) -> io::Result<()> {
    match object {
        Object::Iri(iri) => write_iri_compact(writer, iri, instance_base),
        Object::Literal(value) => writer.write_all(escape_literal(value).as_bytes()),
        Object::TypedLiteral { value, datatype } => {
            writer.write_all(escape_literal(value).as_bytes())?;
            writer.write_all(b"^^")?;
            write_iri_compact(writer, datatype, instance_base)
        }
    }
}

pub fn serialize_lbd_batches_to_writer<W: Write>(
    receiver: Receiver<Vec<Triple>>,
    mut writer: W,
    instance_base: &str,
) -> Result<(), SerializerError> {
    let mut triples = Vec::new();
    for mut batch in receiver {
        triples.append(&mut batch);
    }
    write_grouped_turtle(&triples, &mut writer, Some(instance_base), true)
}

fn compare_triples(left: &Triple, right: &Triple) -> Ordering {
    left.subject
        .cmp(&right.subject)
        .then_with(|| left.predicate.cmp(&right.predicate))
        .then_with(|| compare_objects(&left.object, &right.object))
}

fn compare_objects(left: &Object, right: &Object) -> Ordering {
    match (left, right) {
        (Object::Iri(left), Object::Iri(right)) => left.cmp(right),
        (Object::Literal(left), Object::Literal(right)) => left.cmp(right),
        (
            Object::TypedLiteral {
                value: left_value,
                datatype: left_datatype,
            },
            Object::TypedLiteral {
                value: right_value,
                datatype: right_datatype,
            },
        ) => left_value
            .cmp(right_value)
            .then_with(|| left_datatype.cmp(right_datatype)),
        (Object::Iri(_), _) => Ordering::Less,
        (Object::Literal(_), Object::TypedLiteral { .. }) => Ordering::Less,
        (Object::Literal(_), Object::Iri(_)) => Ordering::Greater,
        (Object::TypedLiteral { .. }, _) => Ordering::Greater,
    }
}

fn write_grouped_turtle<W: Write>(
    triples: &[Triple],
    writer: &mut W,
    instance_base: Option<&str>,
    only_used_prefixes: bool,
) -> Result<(), SerializerError> {
    write_prefixes(writer, instance_base, triples, only_used_prefixes)?;

    let mut ordered = triples.to_vec();
    ordered.sort_by(compare_triples);

    let mut index = 0;
    while index < ordered.len() {
        let subject = &ordered[index].subject;
        write!(writer, "{}\n", compact_iri(subject, instance_base))?;

        let mut predicate_index = index;
        while predicate_index < ordered.len() && ordered[predicate_index].subject == *subject {
            let predicate = &ordered[predicate_index].predicate;
            write!(writer, "        {} ", compact_iri(predicate, instance_base))?;

            let mut object_index = predicate_index;
            let mut first_object = true;
            let mut last_object: Option<&Object> = None;
            while object_index < ordered.len()
                && ordered[object_index].subject == *subject
                && ordered[object_index].predicate == *predicate
            {
                let current_object = &ordered[object_index].object;
                if last_object.is_some_and(|previous| previous == current_object) {
                    object_index += 1;
                    continue;
                }
                if !first_object {
                    write!(writer, ", ")?;
                }
                write!(writer, "{}", format_object(current_object, instance_base))?;
                first_object = false;
                last_object = Some(current_object);
                object_index += 1;
            }

            if object_index < ordered.len() && ordered[object_index].subject == *subject {
                writeln!(writer, " ;")?;
            } else {
                writeln!(writer, " .")?;
            }
            predicate_index = object_index;
        }

        writeln!(writer)?;
        index = predicate_index;
    }

    Ok(())
}

fn write_prefixes<W: Write>(
    writer: &mut W,
    instance_base: Option<&str>,
    triples: &[Triple],
    only_used_prefixes: bool,
) -> Result<(), SerializerError> {
    let used_prefixes = if only_used_prefixes {
        collect_used_prefixes(triples, instance_base)
    } else {
        HashSet::new()
    };

    if let Some(base) = instance_base {
        if !only_used_prefixes || used_prefixes.contains("inst") {
            writeln!(writer, "@prefix inst: <{}> .", ensure_trailing_slash(base))?;
        }
    }
    for (prefix, iri) in PREFIXES {
        if only_used_prefixes && !used_prefixes.contains(prefix) {
            continue;
        }
        writeln!(writer, "@prefix {prefix}: <{iri}> .")?;
    }
    writeln!(writer)?;
    Ok(())
}

fn collect_used_prefixes(triples: &[Triple], instance_base: Option<&str>) -> HashSet<&'static str> {
    let mut out = HashSet::new();
    for triple in triples {
        mark_used_prefixes_for_iri(&triple.subject, instance_base, &mut out);
        mark_used_prefixes_for_iri(&triple.predicate, instance_base, &mut out);
        match &triple.object {
            Object::Iri(iri) => mark_used_prefixes_for_iri(iri, instance_base, &mut out),
            Object::TypedLiteral { datatype, .. } => {
                mark_used_prefixes_for_iri(datatype, instance_base, &mut out)
            }
            Object::Literal(_) => {}
        }
    }
    out
}

fn mark_used_prefixes_for_iri(
    iri: &str,
    instance_base: Option<&str>,
    out: &mut HashSet<&'static str>,
) {
    if let Some(base) = instance_base {
        let base = ensure_trailing_slash(base);
        if iri.starts_with(base) {
            out.insert("inst");
            return;
        }
    }
    for (prefix, namespace) in PREFIXES {
        if iri.starts_with(namespace) {
            out.insert(prefix);
            return;
        }
    }
}

fn compact_iri<'a>(iri: &'a str, instance_base: Option<&'a str>) -> Cow<'a, str> {
    if iri == RDF_TYPE_IRI {
        return Cow::Borrowed("a");
    }
    if let Some(base) = instance_base {
        let base = ensure_trailing_slash(base);
        if let Some(local) = iri.strip_prefix(base) {
            if is_valid_prefixed_local(local) {
                return Cow::Owned(format!("inst:{local}"));
            }
        }
    }

    for (prefix, namespace) in PREFIXES {
        if let Some(local) = iri.strip_prefix(namespace) {
            if is_valid_prefixed_local(local) {
                return Cow::Owned(format!("{prefix}:{local}"));
            }
        }
    }

    Cow::Owned(format!("<{iri}>"))
}

fn format_object(object: &Object, instance_base: Option<&str>) -> String {
    match object {
        Object::Iri(iri) => compact_iri(iri, instance_base).into_owned(),
        Object::Literal(value) => escape_literal(value),
        Object::TypedLiteral { value, datatype } => format!(
            "{}^^{}",
            escape_literal(value),
            compact_iri(datatype, instance_base)
        ),
    }
}

fn escape_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

fn is_valid_prefixed_local(local: &str) -> bool {
    let mut chars = local.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn ensure_trailing_slash(base: &str) -> &str {
    if base.ends_with('/') {
        base
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lbd_ontology::{rdf_type, Triple};

    #[test]
    fn test_serialize_turtle_to_string() {
        let ttl = serialize_turtle_to_string(&[Triple {
            subject: "https://example.test/project/1".to_string(),
            predicate: rdf_type(),
            object: Object::Iri("https://linkedbuildingdata.org/LBD#Project".to_string()),
        }])
        .unwrap();

        assert!(ttl.contains("@prefix lbd:"));
        assert!(!ttl.contains("@prefix bot:"));
        assert!(ttl.contains("<https://example.test/project/1>"));
        assert!(ttl.contains("a lbd:Project ."));
    }

    #[test]
    fn test_serialize_groups_subjects_and_predicates() {
        let ttl = serialize_turtle_to_string(&[
            Triple {
                subject: "https://example.test/project/1".to_string(),
                predicate: rdf_type(),
                object: Object::Iri("https://linkedbuildingdata.org/LBD#Project".to_string()),
            },
            Triple {
                subject: "https://example.test/project/1".to_string(),
                predicate: rdf_type(),
                object: Object::Iri("https://w3id.org/bot#Site".to_string()),
            },
            Triple {
                subject: "https://example.test/project/1".to_string(),
                predicate: "https://w3id.org/bot#hasSite".to_string(),
                object: Object::Iri("https://example.test/site/2".to_string()),
            },
        ])
        .unwrap();

        assert!(ttl.contains("a lbd:Project, bot:Site ;"));
        assert!(ttl.contains("bot:hasSite <https://example.test/site/2> ."));
    }

    #[test]
    fn test_serialize_turtle_batches_to_writer() {
        let (sender, receiver) = crossbeam::channel::bounded(2);
        sender
            .send(vec![Triple {
                subject: "https://example.test/project/1".to_string(),
                predicate: rdf_type(),
                object: Object::Iri("https://linkedbuildingdata.org/LBD#Project".to_string()),
            }])
            .unwrap();
        drop(sender);

        let mut buffer = Vec::new();
        serialize_turtle_batches_to_writer(receiver, &mut buffer, None).unwrap();
        let ttl = String::from_utf8(buffer).unwrap();

        assert!(ttl.contains("@prefix lbd:"));
        assert!(ttl.contains("<https://example.test/project/1>"));
    }

    #[test]
    fn test_serialize_lbd_batches_to_writer_uses_inst_prefix_and_grouping() {
        let (sender, receiver) = crossbeam::channel::bounded(2);
        sender
            .send(vec![
                Triple {
                    subject: "https://example.test/base/project_1".to_string(),
                    predicate: rdf_type(),
                    object: Object::Iri("https://linkedbuildingdata.org/LBD#Project".to_string()),
                },
                Triple {
                    subject: "https://example.test/base/project_1".to_string(),
                    predicate: "https://w3id.org/bot#hasSite".to_string(),
                    object: Object::Iri("https://example.test/base/site_2".to_string()),
                },
            ])
            .unwrap();
        drop(sender);

        let mut buffer = Vec::new();
        serialize_lbd_batches_to_writer(receiver, &mut buffer, "https://example.test/base/")
            .unwrap();
        let ttl = String::from_utf8(buffer).unwrap();

        assert!(ttl.contains("@prefix inst: <https://example.test/base/> ."));
        assert!(ttl.contains("inst:project_1"));
        assert!(ttl.contains("a lbd:Project ;"));
        assert!(ttl.contains("bot:hasSite inst:site_2 ."));
    }

    #[test]
    fn test_serialize_deduplicates_repeated_objects() {
        let ttl = serialize_turtle_to_string(&[
            Triple {
                subject: "https://example.test/s".to_string(),
                predicate: "https://w3id.org/bot#hasSite".to_string(),
                object: Object::Iri("https://example.test/o".to_string()),
            },
            Triple {
                subject: "https://example.test/s".to_string(),
                predicate: "https://w3id.org/bot#hasSite".to_string(),
                object: Object::Iri("https://example.test/o".to_string()),
            },
        ])
        .unwrap();

        let occurrences = ttl.matches("<https://example.test/o>").count();
        assert_eq!(occurrences, 1);
    }
}
