use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use anyhow::Context;
use clap::Parser;
use rio_api::parser::TriplesParser;
use rio_turtle::TurtleParser;

#[derive(Debug, Parser)]
#[command(name = "compare-turtle")]
#[command(about = "Compare two Turtle files as normalized triple sets")]
struct Args {
    left: String,
    right: String,

    #[arg(long)]
    left_base: Option<String>,

    #[arg(long)]
    right_base: Option<String>,

    #[arg(long, default_value_t = 20)]
    sample_limit: usize,

    #[arg(long)]
    normalize_ifcowl_scalars: bool,

    #[arg(long)]
    normalize_lbd_opm: bool,

    #[arg(long = "ignore-predicate")]
    ignore_predicates: Vec<String>,

    #[arg(long, default_value_t = false)]
    ignore_topology_enrichment: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let left = load_turtle(
        &args.left,
        args.left_base.as_deref(),
        args.normalize_ifcowl_scalars,
        args.normalize_lbd_opm,
        &ignored_predicates(&args),
    )
    .with_context(|| format!("failed to parse Turtle file {}", args.left))?;
    let right = load_turtle(
        &args.right,
        args.right_base.as_deref(),
        args.normalize_ifcowl_scalars,
        args.normalize_lbd_opm,
        &ignored_predicates(&args),
    )
    .with_context(|| format!("failed to parse Turtle file {}", args.right))?;

    print_summary("left", &left);
    print_summary("right", &right);

    let missing_from_right = diff(&left.triples, &right.triples);
    let missing_from_left = diff(&right.triples, &left.triples);

    println!("missing_from_right={}", missing_from_right.len());
    for triple in missing_from_right.iter().take(args.sample_limit) {
        println!("  - {}", triple);
    }

    println!("missing_from_left={}", missing_from_left.len());
    for triple in missing_from_left.iter().take(args.sample_limit) {
        println!("  - {}", triple);
    }

    if missing_from_right.is_empty() && missing_from_left.is_empty() {
        println!("result=identical");
    } else {
        println!("result=different");
    }

    Ok(())
}

#[derive(Debug)]
struct ParsedTurtle {
    triples: HashSet<String>,
    subjects: HashSet<String>,
}

#[derive(Debug, Clone)]
struct TripleRow {
    subject: String,
    predicate: String,
    object: String,
}

fn load_turtle(
    path: impl AsRef<Path>,
    base_iri: Option<&str>,
    normalize_ifcowl_scalars: bool,
    normalize_lbd_opm: bool,
    ignored_predicates: &HashSet<String>,
) -> anyhow::Result<ParsedTurtle> {
    let file = File::open(path.as_ref())?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();

    TurtleParser::new(reader, None).parse_all(&mut |triple| {
        let predicate = normalize_text(&triple.predicate.to_string(), base_iri);
        if !ignored_predicates.contains(&predicate) {
            rows.push(TripleRow {
                subject: normalize_text(&triple.subject.to_string(), base_iri),
                predicate,
                object: normalize_text(&triple.object.to_string(), base_iri),
            });
        }
        Ok(()) as Result<(), rio_turtle::TurtleError>
    })?;

    if normalize_ifcowl_scalars {
        normalize_numeric_literals(&mut rows);
        normalize_scalar_nodes(&mut rows);
        normalize_list_nodes(&mut rows);
    }
    if normalize_lbd_opm {
        normalize_numeric_literals(&mut rows);
        normalize_lbd_opm_rows(&mut rows);
    }

    let mut triples = HashSet::new();
    let mut subjects = HashSet::new();
    for row in rows {
        subjects.insert(row.subject.clone());
        triples.insert(format!(
            "{} {} {} .",
            row.subject, row.predicate, row.object
        ));
    }

    Ok(ParsedTurtle { triples, subjects })
}

fn normalize_lbd_opm_rows(rows: &mut [TripleRow]) {
    const OPM_HAS_PROPERTY_STATE: &str = "<https://w3id.org/opm#hasPropertyState>";
    const OPM_VALUE: &str = "<https://w3id.org/opm#value>";
    const PROV_GENERATED_AT_TIME: &str = "<http://www.w3.org/ns/prov#generatedAtTime>";
    const SCHEMA_VALUE: &str = "<http://schema.org/value>";
    const XSD_DATETIME: &str = "^^<http://www.w3.org/2001/XMLSchema#dateTime>";

    let state_subjects: HashMap<String, String> = rows
        .iter()
        .filter(|row| row.predicate == OPM_HAS_PROPERTY_STATE)
        .filter_map(|row| {
            if row.object.starts_with('<') && row.object.ends_with('>') {
                Some((
                    row.object.clone(),
                    format!("<urn:compare-opm-state:{}>", row.subject),
                ))
            } else {
                None
            }
        })
        .collect();

    for row in rows.iter_mut() {
        if let Some(replacement) = state_subjects.get(&row.subject) {
            row.subject = replacement.clone();
        }
        if let Some(replacement) = state_subjects.get(&row.object) {
            row.object = replacement.clone();
        }
        if row.predicate == SCHEMA_VALUE {
            row.predicate = OPM_VALUE.to_string();
        }
        if row.predicate == PROV_GENERATED_AT_TIME {
            let _ = XSD_DATETIME;
            row.object = "\"normalized\"".to_string();
        }
    }
}

fn normalize_scalar_nodes(rows: &mut [TripleRow]) {
    const RDF_TYPE: &str = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>";
    const EXPRESS_PREFIX: &str = "<https://w3id.org/express#";

    #[derive(Default)]
    struct ScalarInfo {
        type_iri: Option<String>,
        literal_predicate: Option<String>,
        literal_object: Option<String>,
    }

    let mut infos: HashMap<String, ScalarInfo> = HashMap::new();
    for row in rows.iter() {
        let info = infos.entry(row.subject.clone()).or_default();
        if row.predicate == RDF_TYPE {
            info.type_iri = Some(row.object.clone());
        } else if row.predicate.starts_with(EXPRESS_PREFIX) {
            info.literal_predicate = Some(row.predicate.clone());
            info.literal_object = Some(row.object.clone());
        }
    }

    let replacements: HashMap<String, String> = infos
        .into_iter()
        .filter_map(|(subject, info)| {
            let type_iri = info.type_iri?;
            let literal_predicate = info.literal_predicate?;
            let literal_object = info.literal_object?;
            Some((
                subject,
                format!(
                    "<urn:compare-scalar:{}:{}:{}>",
                    type_iri, literal_predicate, literal_object
                ),
            ))
        })
        .collect();

    for row in rows.iter_mut() {
        if let Some(replacement) = replacements.get(&row.subject) {
            row.subject = replacement.clone();
        }
        if let Some(replacement) = replacements.get(&row.object) {
            row.object = replacement.clone();
        }
    }
}

fn normalize_numeric_literals(rows: &mut [TripleRow]) {
    for row in rows.iter_mut() {
        row.object = normalize_numeric_literal(&row.object);
    }
}

fn normalize_numeric_literal(value: &str) -> String {
    const XSD_DOUBLE: &str = "^^<http://www.w3.org/2001/XMLSchema#double>";
    const XSD_INTEGER: &str = "^^<http://www.w3.org/2001/XMLSchema#integer>";
    const XSD_BOOLEAN: &str = "^^<http://www.w3.org/2001/XMLSchema#boolean>";

    if let Some(inner) = value.strip_suffix(XSD_DOUBLE) {
        if let Some(literal) = inner.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            if let Ok(parsed) = literal.parse::<f64>() {
                return format!("\"{}\"{XSD_DOUBLE}", parsed);
            }
        }
    }
    if let Some(inner) = value.strip_suffix(XSD_INTEGER) {
        if let Some(literal) = inner.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            if let Ok(parsed) = literal.parse::<i64>() {
                return format!("\"{}\"{XSD_INTEGER}", parsed);
            }
        }
    }
    if let Some(inner) = value.strip_suffix(XSD_BOOLEAN) {
        if let Some(literal) = inner.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            return format!("\"{}\"{XSD_BOOLEAN}", literal.to_ascii_lowercase());
        }
    }
    value.to_string()
}

fn normalize_list_nodes(rows: &mut [TripleRow]) {
    const RDF_TYPE: &str = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>";
    const LIST_HAS_CONTENTS: &str = "<https://w3id.org/list#hasContents>";
    const LIST_HAS_NEXT: &str = "<https://w3id.org/list#hasNext>";

    #[derive(Default, Clone)]
    struct ListInfo {
        type_iri: Option<String>,
        contents: Option<String>,
        next: Option<String>,
    }

    let mut infos: HashMap<String, ListInfo> = HashMap::new();
    for row in rows.iter() {
        let info = infos.entry(row.subject.clone()).or_default();
        match row.predicate.as_str() {
            RDF_TYPE => {
                // Match both _List-typed nodes and compound-measure list nodes
                // (e.g. IfcCompoundPlaneAngleMeasure which acts as a list node).
                if row.object.contains("_List>")
                    || row.object.contains("_List#")
                    || row.object.contains("IfcCompoundPlaneAngleMeasure")
                {
                    info.type_iri = Some(row.object.clone());
                }
            }
            LIST_HAS_CONTENTS => {
                info.contents = Some(row.object.clone());
            }
            LIST_HAS_NEXT => {
                info.next = Some(row.object.clone());
            }
            _ => {}
        }
    }

    fn signature(
        subject: &str,
        infos: &HashMap<String, ListInfo>,
        memo: &mut HashMap<String, String>,
    ) -> Option<String> {
        if let Some(cached) = memo.get(subject) {
            return Some(cached.clone());
        }
        let info = infos.get(subject)?;
        let type_iri = info.type_iri.clone()?;
        let contents = info.contents.clone()?;
        let next_sig = info
            .next
            .as_ref()
            .and_then(|next| signature(next, infos, memo))
            .unwrap_or_else(|| "nil".to_string());
        let sig = format!("<urn:compare-list:{type_iri}:{contents}:{next_sig}>");
        memo.insert(subject.to_string(), sig.clone());
        Some(sig)
    }

    let mut memo = HashMap::new();
    let replacements: HashMap<String, String> = infos
        .keys()
        .filter_map(|subject| {
            signature(subject, &infos, &mut memo).map(|sig| (subject.clone(), sig))
        })
        .collect();

    for row in rows.iter_mut() {
        if let Some(replacement) = replacements.get(&row.subject) {
            row.subject = replacement.clone();
        }
        if let Some(replacement) = replacements.get(&row.object) {
            row.object = replacement.clone();
        }
    }
}

fn normalize_text(value: &str, base_iri: Option<&str>) -> String {
    const NORMALIZED_BASE: &str = "urn:compare-base:";

    if let Some(base_iri) = base_iri {
        value.replace(base_iri, NORMALIZED_BASE)
    } else {
        value.to_string()
    }
}

fn print_summary(label: &str, parsed: &ParsedTurtle) {
    println!(
        "{}: triples={}, subjects={}",
        label,
        parsed.triples.len(),
        parsed.subjects.len()
    );
}

fn diff(left: &HashSet<String>, right: &HashSet<String>) -> Vec<String> {
    let mut out: Vec<_> = left.difference(right).cloned().collect();
    out.sort();
    out
}

fn ignored_predicates(args: &Args) -> HashSet<String> {
    let mut ignored: HashSet<String> = args.ignore_predicates.iter().cloned().collect();
    if args.ignore_topology_enrichment {
        ignored.extend([
            "<https://w3id.org/bot#adjacentElement>".to_string(),
            "<https://w3id.org/bot#hasSubElement>".to_string(),
        ]);
    }
    ignored
}
