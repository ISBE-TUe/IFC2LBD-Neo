//! Industry Foundation Classes (IFC)  STEP file parser (ISO 10303-21)
//! Parses IFC and normalizes entity data. Detects the IFC schema from the file header
//! (IFC2X3, IFC4, IFC4X1, IFC4X3; IFC4X2 normalized to IFC4X3).
//!
//! Parses IFC files in the STEP Physical File format into a collection of
//! [`RawEntity`] objects with resolved cross-references.

mod error;
mod header;
mod parser;
mod types;
mod unicode;

pub use error::StepError;
pub use header::{StepHeader, StepSchema};
pub use types::{EntityId, RawEntity, StepValue};
pub use unicode::decode_ifc_unicode;

use std::collections::HashMap;
use std::path::Path;

/// Parse an IFC STEP file and return all entities with resolved references.
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_step_file(path: &Path) -> Result<StepFile, StepError> {
    use std::fs::File;

    let file = File::open(path).map_err(|e| StepError::Io(e.to_string()))?;
    // SAFETY: The file stays open for the lifetime of the mapping and is read-only.
    let mmap = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| StepError::Io(e.to_string()))?;
    parse_step_bytes(&mmap)
}

/// Parse from a filesystem path is not available in browser Wasm targets.
#[cfg(target_arch = "wasm32")]
pub fn parse_step_file(_path: &Path) -> Result<StepFile, StepError> {
    Err(StepError::Io(
        "parse_step_file is unavailable on wasm32; use parse_step_bytes".to_string(),
    ))
}

/// Parse IFC STEP data from a byte slice.
pub fn parse_step_bytes(data: &[u8]) -> Result<StepFile, StepError> {
    let header = header::parse_header(data)?;
    let entities = parser::parse_entities(data)?;
    Ok(StepFile { header, entities })
}

/// A parsed IFC STEP file.
#[derive(Debug, Default)]
pub struct StepFile {
    /// The file header (schema version, description, etc.).
    pub header: StepHeader,
    /// All entities, keyed by their line number (e.g., `#123`).
    pub entities: HashMap<EntityId, RawEntity>,
}
