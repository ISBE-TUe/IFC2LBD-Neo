//! RML engine — adapted from worker-rml-rust/src/main.rs.
//!
//! This module wraps the `rml_mapper` library, executing RML mappings on
//! structured data files and returning N-Triples bytes.
//!
//! The execution flow (from the worker code):
//! 1. Parse mapping Turtle into InMemoryQuadStore
//! 2. Conform mapping (old RML namespace → W3C RML)
//! 3. Create mapping document via MappingFactory
//! 4. Execute mapping via Executor (writes to output store)
//! 5. Serialize output store as N-Triples

use std::io::Cursor;
use tempfile::TempDir;

use rml_mapper_lib::{
    conformer::MappingConformer,
    executor::Executor,
    mapping::{MappingFactory, StrictMode},
    store::{InMemoryQuadStore, QuadStore, RdfFormat},
};

/// Execute an RML mapping and return N-Triples bytes.
///
/// Adapted from `worker-rml-rust/src/main.rs::execute_rml_sync()`.
pub fn execute_rml(
    mapping_turtle: &str,
    source_filename: &str,
    source_bytes: &[u8],
) -> Result<Vec<u8>, String> {
    let temp_dir = TempDir::new().map_err(|e| format!("temp dir: {e}"))?;
    let work_dir = temp_dir.path().to_path_buf();

    // Write source file to temp directory
    let source_path = work_dir.join(source_filename);
    std::fs::write(&source_path, source_bytes)
        .map_err(|e| format!("write source: {e}"))?;

    // Replace placeholder source filenames in mapping with actual filename
    let mapping = prepare_mapping_for_source(mapping_turtle, source_filename);

    // Parse mapping into quad store
    let mut mapping_store = InMemoryQuadStore::new();
    let cursor = Cursor::new(mapping.as_bytes());
    mapping_store
        .read(cursor, None, RdfFormat::Turtle)
        .map_err(|e| format!("parse mapping: {e}"))?;

    // Conform mapping (old RML namespace → W3C RML)
    let mut conformer = MappingConformer::new(mapping_store, None);
    conformer
        .conform()
        .map_err(|e| format!("conform: {e}"))?;
    let mapping_store = conformer.into_store();

    // Create mapping document
    let factory = MappingFactory::new(None, StrictMode::BestEffort);
    let mapping = factory
        .create_mapping(&mapping_store)
        .map_err(|e| format!("create mapping: {e}"))?;

    // Execute mapping
    let mut executor = Executor::new(mapping, work_dir, StrictMode::BestEffort);
    executor
        .execute()
        .map_err(|e| format!("execute: {e}"))?;

    let output_store = executor.output_store();

    // Serialize as N-Triples
    let mut buffer = Vec::new();
    output_store
        .write(&mut buffer, RdfFormat::NTriples)
        .map_err(|e| format!("serialize: {e}"))?;

    Ok(buffer)
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
