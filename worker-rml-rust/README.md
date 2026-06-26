# OntoCore RML Worker (Rust)

HTTP wrapper around the `rust_rml_mapper` library. Executes RML mappings and returns
generated RDF to the OntoCore ingest pipeline. This is the active RML worker — there is
no Python or Java RML worker.

## Quick Start

### 0. Bootstrap the sibling `rust_rml_mapper` repo (first clone only)

`Cargo.toml` depends on `rml_mapper` via a sibling path:

```toml
rml_mapper = { path = "../../../rust_rml_mapper" }
```

Run the bootstrap script once from the repo root — it links the worker
to the sibling `rust_rml_mapper` repo:

```bash
chmod +x workers/worker-rml-rust/bootstrap.sh   # first time only
./workers/worker-rml-rust/bootstrap.sh
```

### 1. Build the Docker image

```bash
./workers/worker-rml-rust/build.sh
```

The build script handles the extended Docker context because this package depends on the
sibling `rust_rml_mapper` repo — see [Directory Structure](#directory-structure) below.

### 2. Start via compose

The worker is part of the default OntoCore dev stack and is started automatically:

```bash
make up
```

To run the container standalone:

```bash
docker run --rm -p 8010:8000 ontocore-worker-rml-rust
```

## API Endpoints

### `GET /healthz`

Health check endpoint.

**Response:**
```json
{
  "status": "ok",
  "rust_native": true,
  "version": "0.1.0"
}
```

### `POST /execute`

Execute an RML mapping and return the generated RDF.

**Request (multipart/form-data):**
- `file`: Source data file (JSON, CSV, XML)
- `mapping`: RML mapping file (Turtle)
- `output_format`: Output format (optional, default: `turtle`)

**Response:**
```json
{
  "rdf": "@prefix schema: <https://schema.org/> ...",
  "format": "turtle",
  "triple_count_estimate": 42,
  "execution_time_ms": 15
}
```

## Configuration

| Environment Variable | Default | Description |
|---------------------|---------|-------------|
| `PORT` | `8000` | HTTP server port (inside the container) |
| `LOG_LEVEL` | `info` | Log level (trace, debug, info, warn, error) |
| `RUST_BACKTRACE` | `1` | Show backtraces on panic |

OntoCore core points at this worker via `WORKER_RML_URL` (default `http://localhost:8010`).

## Directory Structure

> The `rust_rml_mapper` library lives as a sibling repo at the workspace root.
> `Cargo.toml` references it via a relative path.

```
fresh_context/               # Workspace root
├── cn3-pt1/
│   └── workers/
│       └── worker-rml-rust/ # This package (HTTP wrapper)
└── rust_rml_mapper/         # RML Mapper library (sibling repo)
```

## Supported Features

### Input Formats
- JSON (with JSONPath iterators)
- CSV (Comma-Separated Values)
- XML (with XPath iterators)

### Output Formats
- Turtle (`.ttl`) — default
- N-Triples (`.nt`)
- N-Quads (`.nq`)
- TriG (`.trig`)
- RDF/XML (`.rdf`)

### RML Features
- Template-based IRI generation
- Reference-based value extraction
- Constant values
- Joins between data sources
- Named graphs
- Blank nodes
- Language tags
- Datatype specification
- Old RML namespace auto-conforming to W3C RML

## Troubleshooting

### Build fails with "rml_mapper not found"

Ensure the `rust_rml_mapper` directory exists as a sibling of `ontocore/`:

```bash
ls -la ../../rust_rml_mapper
```

### Container won't start

```bash
docker logs ontocore-worker-rml-rust
```

### Mapping errors

The Rust mapper uses `BestEffort` mode by default, which skips invalid IRIs.

## License

Apache 2.0
