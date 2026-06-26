//! RML Worker Service (Rust)
//!
//! High-performance HTTP service for executing RML mappings.
//! Drop-in replacement for worker-rml (Python + Java).
//!
//! # Endpoints
//!
//! - `GET /healthz` - Health check
//! - `POST /execute` - Execute RML mapping

use axum::{
    extract::{DefaultBodyLimit, Multipart},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use rml_mapper::{
    conformer::MappingConformer,
    executor::Executor,
    mapping::{MappingFactory, StrictMode},
    store::{InMemoryQuadStore, QuadStore, RdfFormat},
};
use serde::Serialize;
use std::{
    env,
    io::Cursor,
    net::SocketAddr,
    path::PathBuf,
    time::Instant,
};
use tempfile::TempDir;
use tokio::fs;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

// ── Response Types ───────────────────────────────────────────

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    /// For API compatibility with Java worker (always false for Rust)
    java_available: bool,
    /// Rust-specific: indicates this is the Rust implementation
    rust_native: bool,
    version: String,
}

#[derive(Serialize)]
struct ExecuteResponse {
    rdf: String,
    format: String,
    triple_count_estimate: usize,
    /// Execution time in milliseconds (Rust-specific diagnostic)
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_time_ms: Option<u64>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
    detail: Option<String>,
}

// ── Handlers ─────────────────────────────────────────────────

/// Health check endpoint
async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        java_available: false,
        rust_native: true,
        version: rml_mapper::VERSION.to_string(),
    })
}

/// Execute RML mapping
///
/// Expects multipart form with:
/// - `file`: Source data file (JSON, CSV, XML)
/// - `mapping`: RML mapping file (Turtle)
/// - `output_format`: Output format (optional, default: turtle)
async fn execute(mut multipart: Multipart) -> Result<Json<ExecuteResponse>, (StatusCode, Json<ErrorResponse>)> {
    let start = Instant::now();
    
    // Parse multipart form
    let mut source_data: Option<(String, Vec<u8>)> = None;
    let mut mapping_content: Option<String> = None;
    let mut output_format = "turtle".to_string();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| bad_request(format!("Failed to read multipart field: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();
        
        match name.as_str() {
            "file" => {
                let filename = field.file_name().unwrap_or("source.json").to_string();
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| bad_request(format!("Failed to read file data: {}", e)))?;
                source_data = Some((filename, data.to_vec()));
            }
            "mapping" => {
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| bad_request(format!("Failed to read mapping data: {}", e)))?;
                mapping_content = Some(
                    String::from_utf8(data.to_vec())
                        .map_err(|e| bad_request(format!("Invalid UTF-8 in mapping: {}", e)))?,
                );
            }
            "output_format" => {
                let data = field
                    .text()
                    .await
                    .map_err(|e| bad_request(format!("Failed to read output_format: {}", e)))?;
                output_format = data;
            }
            _ => {
                warn!("Ignoring unknown field: {}", name);
            }
        }
    }

    // Validate required fields
    let (source_filename, source_bytes) = source_data
        .ok_or_else(|| bad_request("Missing required field: file".to_string()))?;
    let mapping_turtle = mapping_content
        .ok_or_else(|| bad_request("Missing required field: mapping".to_string()))?;

    info!(
        "Executing RML mapping: source={} ({} bytes), mapping={} chars, format={}",
        source_filename,
        source_bytes.len(),
        mapping_turtle.len(),
        output_format
    );

    // Create temp directory for execution
    let temp_dir = TempDir::new()
        .map_err(|e| internal_error(format!("Failed to create temp directory: {}", e)))?;
    let work_dir = temp_dir.path().to_path_buf();

    // Write source file to temp directory
    let source_path = work_dir.join(&source_filename);
    fs::write(&source_path, &source_bytes)
        .await
        .map_err(|e| internal_error(format!("Failed to write source file: {}", e)))?;

    // Replace placeholder source filenames in mapping with actual filename
    let mapping_turtle = prepare_mapping_for_source(&mapping_turtle, &source_filename);

    // Clone output_format for use in closure (original kept for response)
    let output_format_for_exec = output_format.clone();

    // Execute mapping (blocking operation, run in spawn_blocking)
    let rdf_output = tokio::task::spawn_blocking(move || {
        execute_rml_sync(&mapping_turtle, &work_dir, &output_format_for_exec)
    })
    .await
    .map_err(|e| internal_error(format!("Task join error: {}", e)))?
    .map_err(|e| internal_error(e))?;

    let execution_time_ms = start.elapsed().as_millis() as u64;
    let triple_count = estimate_triple_count(&rdf_output, &output_format);

    info!(
        "Mapping complete: {} triples in {}ms",
        triple_count, execution_time_ms
    );

    Ok(Json(ExecuteResponse {
        rdf: rdf_output,
        format: output_format,
        triple_count_estimate: triple_count,
        execution_time_ms: Some(execution_time_ms),
    }))
}

/// Replace placeholder source filenames in mapping with actual filename.
/// 
/// Mirrors the Python worker's `prepare_mapping_for_source()` function.
fn prepare_mapping_for_source(mapping: &str, source_filename: &str) -> String {
    const PLACEHOLDERS: &[&str] = &[
        "source.xml", "source.json", "source.csv",
        "data.xml", "data.json", "data.csv",
        "input.xml", "input.json", "input.csv",
    ];

    let mut result = mapping.to_string();
    for placeholder in PLACEHOLDERS {
        // Guard: only replace if placeholder appears as standalone value,
        // not as substring of already-substituted source_filename
        if result.contains(*placeholder) 
            && *placeholder != source_filename 
            && !source_filename.contains(*placeholder) 
        {
            result = result.replace(*placeholder, source_filename);
            info!("Replaced placeholder '{}' with '{}'", placeholder, source_filename);
            break;
        }
    }
    result
}

/// Synchronous RML execution (called from spawn_blocking)
fn execute_rml_sync(
    mapping_turtle: &str,
    work_dir: &PathBuf,
    output_format: &str,
) -> Result<String, String> {
    // Parse mapping into quad store
    let mut mapping_store = InMemoryQuadStore::new();
    let mapping_cursor = Cursor::new(mapping_turtle.as_bytes());
    
    mapping_store
        .read(mapping_cursor, None, RdfFormat::Turtle)
        .map_err(|e| format!("Failed to parse mapping Turtle: {}", e))?;

    // Conform mapping (convert old RML namespace to W3C RML)
    let mut conformer = MappingConformer::new(mapping_store, None);
    conformer
        .conform()
        .map_err(|e| format!("Failed to conform mapping: {}", e))?;
    let mapping_store = conformer.into_store();

    // Create mapping document
    let factory = MappingFactory::new(None, StrictMode::BestEffort);
    let mapping = factory
        .create_mapping(&mapping_store)
        .map_err(|e| format!("Failed to create mapping document: {}", e))?;

    info!("Found {} triples maps", mapping.len());

    // Execute mapping
    let mut executor = Executor::new(mapping, work_dir.clone(), StrictMode::BestEffort);
    executor
        .execute()
        .map_err(|e| format!("Failed to execute mapping: {}", e))?;

    let output_store = executor.output_store();
    info!("Generated {} quads", output_store.size());

    // Serialize output
    let rdf_format = parse_output_format(output_format)?;
    let mut output_buffer = Vec::new();
    output_store
        .write(&mut output_buffer, rdf_format)
        .map_err(|e| format!("Failed to serialize RDF: {}", e))?;

    String::from_utf8(output_buffer)
        .map_err(|e| format!("Invalid UTF-8 in output: {}", e))
}

/// Parse output format string to RdfFormat
fn parse_output_format(format: &str) -> Result<RdfFormat, String> {
    match format.to_lowercase().as_str() {
        "turtle" | "ttl" => Ok(RdfFormat::Turtle),
        "ntriples" | "nt" => Ok(RdfFormat::NTriples),
        "nquads" | "nq" => Ok(RdfFormat::NQuads),
        "trig" => Ok(RdfFormat::TriG),
        "rdfxml" | "rdf" | "xml" => Ok(RdfFormat::RdfXml),
        // JSON-LD not supported by oxigraph serializer, fall back to NQuads
        "jsonld" | "json-ld" => {
            warn!("JSON-LD not supported, using N-Quads instead");
            Ok(RdfFormat::NQuads)
        }
        _ => Err(format!("Unsupported output format: {}", format)),
    }
}

/// Estimate triple count from RDF output
fn estimate_triple_count(rdf: &str, format: &str) -> usize {
    match format.to_lowercase().as_str() {
        "turtle" | "ttl" => {
            // Count statement terminators and predicate separators
            let dots = rdf.matches(" .\n").count() + rdf.matches(" .").count();
            let semicolons = rdf.matches(" ;\n").count();
            dots + semicolons
        }
        "ntriples" | "nt" | "nquads" | "nq" => {
            // Each line is a triple/quad
            rdf.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).count()
        }
        _ => {
            // Rough estimate based on size
            rdf.len() / 100
        }
    }
}

// ── Error Helpers ────────────────────────────────────────────

fn bad_request(message: String) -> (StatusCode, Json<ErrorResponse>) {
    error!("Bad request: {}", message);
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: "Bad Request".to_string(),
            detail: Some(message),
        }),
    )
}

fn internal_error(message: String) -> (StatusCode, Json<ErrorResponse>) {
    error!("Internal error: {}", message);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: "RML execution failed".to_string(),
            detail: Some(message),
        }),
    )
}

// ── Main ─────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // Initialize logging
    let log_level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&log_level));

    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(filter)
        .init();

    info!("Starting worker-rml-rust v{}", rml_mapper::VERSION);

    // Build router with large body limit (100MB for big datasets)
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/execute", post(execute))
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024)) // 100MB
        .layer(TraceLayer::new_for_http());

    // Bind to address
    let port: u16 = env::var("PORT")
        .unwrap_or_else(|_| "8000".to_string())
        .parse()
        .expect("Invalid PORT");
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    info!("Listening on http://{}", addr);

    // Start server
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind address");
    
    axum::serve(listener, app)
        .await
        .expect("Server error");
}
