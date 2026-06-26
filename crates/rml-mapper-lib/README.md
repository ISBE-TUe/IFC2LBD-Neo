# RML Mapper (Rust)

A high-performance [RML (RDF Mapping Language)](https://rml.io/) mapper written in Rust. Transforms heterogeneous data sources (CSV, JSON, XML) into RDF triples using W3C-standard RML mappings.

## Performance

Benchmarked against [RMLMapper-java v8.1.1](https://github.com/RMLio/rmlmapper-java) on 100,000 records producing 500,000 triples:

| Benchmark | Rust | Java | Speedup |
|---|---|---|---|
| CSV 100k rows | 1.06s | 3.9s | **3.7x faster** |
| JSON 100k records | 1.13s | 4.7s | **4.2x faster** |
| CSV 1k rows | 9.5ms | 1.2s | **127x faster** |

The Rust implementation uses parallel record processing via [rayon](https://github.com/rayon-rs/rayon) and zero-copy optimizations to achieve high throughput on multi-core systems, with near-instant startup for small workloads.

## Installation

```bash
cargo install --path .
```

Or build from source:

```bash
cargo build --release
# Binary at: target/release/rml_mapper
```

## Usage

### Execute a mapping

```bash
rml_mapper map -m mapping.ttl -o output.nq --workdir ./data
```

### Validate a mapping file

```bash
rml_mapper validate mapping.ttl
```

### Show mapping information

```bash
rml_mapper info mapping.ttl --detailed
```

### List capabilities

```bash
rml_mapper capabilities
```

### CLI options

```
rml_mapper map [OPTIONS] -m <FILE>...

Options:
  -m, --mapping <FILE>          One or more mapping files (Turtle format)
  -o, --output <FILE>           Output file (default: stdout)
  -s, --serialization <FORMAT>  Output format [default: nquads]
                                [nquads, ntriples, turtle, trig, rdfxml]
  -b, --base-iri <IRI>          Base IRI for relative IRIs
  -d, --duplicates              Remove duplicate triples
      --strict                  Enable strict mode (fail on invalid IRIs)
      --workdir <DIR>           Working directory for relative paths
      --stats                   Show execution statistics
  -v, --verbose                 Verbose output (-v, -vv, -vvv)
  -q, --quiet                   Quiet mode (only errors)
```

## Example

Given a CSV file `people.csv`:

```csv
Id,Name,Age,Email
1,Alice,30,alice@example.com
2,Bob,25,bob@example.com
```

And a mapping `mapping.ttl`:

```turtle
@prefix rr: <http://www.w3.org/ns/r2rml#> .
@prefix rml: <http://semweb.mmlab.be/ns/rml#> .
@prefix ql: <http://semweb.mmlab.be/ns/ql#> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

<PersonMapping> a rr:TriplesMap;
  rml:logicalSource [
    rml:source "people.csv";
    rml:referenceFormulation ql:CSV
  ];
  rr:subjectMap [
    rr:template "http://example.com/person/{Id}";
    rr:class foaf:Person
  ];
  rr:predicateObjectMap [
    rr:predicate foaf:name;
    rr:objectMap [ rml:reference "Name" ]
  ];
  rr:predicateObjectMap [
    rr:predicate foaf:age;
    rr:objectMap [ rml:reference "Age"; rr:datatype xsd:integer ]
  ].
```

Run:

```bash
rml_mapper map -m mapping.ttl --workdir .
```

Output:

```
<http://example.com/person/1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://xmlns.com/foaf/0.1/Person> .
<http://example.com/person/1> <http://xmlns.com/foaf/0.1/name> "Alice" .
<http://example.com/person/1> <http://xmlns.com/foaf/0.1/age> "30"^^<http://www.w3.org/2001/XMLSchema#integer> .
<http://example.com/person/2> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://xmlns.com/foaf/0.1/Person> .
<http://example.com/person/2> <http://xmlns.com/foaf/0.1/name> "Bob" .
<http://example.com/person/2> <http://xmlns.com/foaf/0.1/age> "25"^^<http://www.w3.org/2001/XMLSchema#integer> .
```

## Supported Features

### Data Sources

- CSV (Comma-Separated Values)
- JSON (with JSONPath iterators)
- XML (with XPath iterators)

### RML Features

- Template-based IRI generation
- Reference-based value extraction
- Constant values
- Joins between data sources (referencing object maps)
- Named graphs
- Blank nodes
- Language tags
- Datatype specification
- Multiple triples maps
- R2RML compatibility (rr: namespace)
- Old RML namespace auto-conforming to W3C RML

### Output Formats

- N-Quads (.nq)
- N-Triples (.nt)
- Turtle (.ttl)
- TriG (.trig)
- RDF/XML (.rdf)

## Architecture

```
src/
  main.rs            CLI entry point
  lib.rs             Library root
  mapping/           RML mapping document parser
  executor/          Parallel mapping execution engine
  termgenerator/     RDF term generation (IRIs, literals, blank nodes)
  records/           Data source record abstraction (CSV, JSON, XML)
  store/             RDF quad store (in-memory, HashSet-backed)
  conformer/         Old RML -> W3C RML namespace conformer
  access/            Data source access layer
  functions/         GREL function support
  term/              RDF term types (NamedNode, Literal, BlankNode, Quad)
  namespaces.rs      RML/RDF namespace constants
  error.rs           Error types
```

## Testing

Tests use the official [W3C RML test cases](https://rml.io/test-cases/):

```bash
cargo test
```

## License

MIT OR Apache-2.0
