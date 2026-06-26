# Access Module

The Access module provides a unified interface for accessing data from various sources in the RML Mapper. It follows the Java RML Mapper's `AccessFactory.java` design.

## Overview

The module supports multiple data source types:

- **Local Files**: Access files on the local file system
- **Remote Files**: Access files via HTTP/HTTPS
- **Databases**: Access relational databases (MySQL, PostgreSQL, SQLite, MSSQL)
- **SPARQL Endpoints**: Query SPARQL endpoints for RDF data
- **Web of Things**: Access IoT devices (planned)

## Architecture

### Core Trait

```rust
pub trait Access: Send + Sync {
    fn get_reader(&self) -> Result<Box<dyn Read + Send>>;
    fn content_type(&self) -> Option<&str>;
    fn cache_key(&self) -> String;
}
```

### Implementations

1. **LocalFileAccess**: Reads files from the local file system
   - Supports relative path resolution
   - Auto-detects content type from file extension
   - Thread-safe

2. **RemoteFileAccess**: Fetches files via HTTP/HTTPS
   - Uses `reqwest` blocking client
   - Supports content type detection from URL
   - Handles HTTP errors gracefully

3. **DatabaseAccess**: Queries relational databases
   - Supports MySQL, PostgreSQL, SQLite, MSSQL
   - Returns results as CSV format
   - Implementation pending (structure defined)

4. **SparqlAccess**: Queries SPARQL endpoints
   - Supports XML, JSON, and CSV result formats
   - Handles URL encoding of queries
   - Configurable Accept headers

### Factory Pattern

The `AccessFactory` creates appropriate `Access` instances from RML logical source descriptions:

```rust
let factory = AccessFactory::new();
let access = factory.create_access(&store, &logical_source)?;
```

## Usage Examples

### Local File Access

```rust
use rml_mapper::access::{Access, LocalFileAccess};
use std::path::PathBuf;

let access = LocalFileAccess::new(
    PathBuf::from("data.csv"),
    Some(PathBuf::from("/base/path")),
    Some("text/csv".to_string())
);

let mut reader = access.get_reader()?;
// Read data from reader
```

### Remote File Access

```rust
use rml_mapper::access::{Access, RemoteFileAccess};

let access = RemoteFileAccess::new(
    "https://example.org/data.json".to_string(),
    Some("application/json".to_string())
);

let mut reader = access.get_reader()?;
// Read data from reader
```

### Database Access

```rust
use rml_mapper::access::{DatabaseAccess, DatabaseType};

let access = DatabaseAccess::new(
    "postgresql://localhost/mydb".to_string(),
    DatabaseType::PostgreSQL,
    "user".to_string(),
    "password".to_string(),
    "SELECT * FROM users".to_string()
);

// Note: Database implementation is pending
```

### SPARQL Access

```rust
use rml_mapper::access::{SparqlAccess, SparqlResultFormat};

let access = SparqlAccess::new(
    "https://dbpedia.org/sparql".to_string(),
    "SELECT * WHERE { ?s ?p ?o } LIMIT 10".to_string(),
    SparqlResultFormat::JSON
);

let mut reader = access.get_reader()?;
// Read SPARQL results
```

### Using AccessFactory with RML

```rust
use rml_mapper::access::AccessFactory;
use rml_mapper::store::InMemoryQuadStore;
use rml_mapper::term::{NamedNode, Literal};

let mut store = InMemoryQuadStore::new();

// Add RML triples
let source = NamedNode::new("http://example.org/source1")?;
let rml_source = NamedNode::new("http://semweb.mmlab.be/ns/rml#source")?;

store.add_quad(
    source.clone().into(),
    rml_source,
    Literal::new("data.csv").into(),
    None
)?;

// Create access from RML description
let factory = AccessFactory::new();
let access = factory.create_access(&store, &source.into())?;
```

## RML Mapping Integration

The Access module integrates with RML mappings through the following predicates:

### Local/Remote Files

```turtle
@prefix rml: <http://semweb.mmlab.be/ns/rml#> .
@prefix dcat: <http://www.w3.org/ns/dcat#> .

:Source1 
    rml:source "data.csv" ;
    dcat:mediaType "text/csv" .
```

### Database Sources

```turtle
@prefix rr: <http://www.w3.org/ns/r2rml#> .
@prefix d2rq: <http://www.wiwiss.fu-berlin.de/suhl/bizer/D2RQ/0.1#> .

:Source1
    d2rq:jdbcDSN "postgresql://localhost/mydb" ;
    d2rq:username "user" ;
    d2rq:password "password" ;
    rr:sqlQuery "SELECT * FROM users" .
```

### SPARQL Endpoints

```turtle
@prefix sd: <http://www.w3.org/ns/sparql-service-description#> .

:Source1
    sd:endpoint <https://dbpedia.org/sparql> ;
    sd:query "SELECT * WHERE { ?s ?p ?o } LIMIT 10" .
```

## Content Type Detection

The module automatically detects content types from:

1. Explicit `dcat:mediaType` in RML mapping
2. File extension (for local files)
3. URL extension (for remote files)

Supported extensions:
- `.csv` → `text/csv`
- `.json` → `application/json`
- `.xml` → `application/xml`
- `.ttl` → `text/turtle`
- `.nt` → `application/n-triples`
- `.nq` → `application/n-quads`
- `.rdf` → `application/rdf+xml`
- `.jsonld` → `application/ld+json`

## Database Types

Supported database types with automatic detection:

| Database | URL Scheme | JDBC Driver |
|----------|-----------|-------------|
| MySQL | `mysql://` | `com.mysql.jdbc.Driver` |
| PostgreSQL | `postgresql://` or `postgres://` | `org.postgresql.Driver` |
| SQLite | `sqlite://` | `org.sqlite.JDBC` |
| MSSQL | `sqlserver://` | `com.microsoft.sqlserver.jdbc.SQLServerDriver` |

## SPARQL Result Formats

| Format | Content Type | Use Case |
|--------|-------------|----------|
| XML | `application/sparql-results+xml` | Standard SPARQL XML format |
| JSON | `application/sparql-results+json` | Modern web applications |
| CSV | `text/csv` | Simple tabular data |

## Error Handling

All access methods return `Result<T>` with appropriate error types:

- `RmlError::Access`: General access errors
- `RmlError::Http`: HTTP request failures
- `RmlError::Database`: Database connection/query errors
- `RmlError::Io`: File I/O errors

## Thread Safety

All `Access` implementations are `Send + Sync`, making them safe to use across threads.

## Caching

Each `Access` implementation provides a `cache_key()` method that returns a unique identifier for caching purposes:

- Local files: `file:///path/to/file`
- Remote files: Full URL
- Databases: `db://user@dsn/query_hash`
- SPARQL: `sparql://endpoint?query=query_hash`

## Future Enhancements

1. **Database Implementation**: Complete the database access implementation using `sqlx`
2. **Web of Things**: Add support for WoT Thing Descriptions
3. **Async Support**: Add async versions of access methods
4. **Compression**: Support for compressed data sources (gzip, bzip2)
5. **Authentication**: Enhanced authentication support for HTTP and databases
6. **Streaming**: Optimize for large data sources with streaming
7. **Caching**: Implement intelligent caching layer

## Testing

Run tests with:

```bash
cargo test --lib access
```

Run the demo example:

```bash
cargo run --example access_demo
```

## Dependencies

- `reqwest`: HTTP client (with `blocking` feature)
- `urlencoding`: URL encoding for SPARQL queries
- `sqlx`: Database access (pending implementation)

## References

- [RML Specification](https://rml.io/specs/rml/)
- [R2RML Specification](https://www.w3.org/TR/r2rml/)
- [SPARQL Protocol](https://www.w3.org/TR/sparql11-protocol/)
- [Java RML Mapper](https://github.com/RMLio/rmlmapper-java)
