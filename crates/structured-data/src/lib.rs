//! Structured data input types for the IFC2LBD-Neo pipeline.
//!
//! This crate provides types for non-IFC structured data input (JSON, XML, CSV).
//! The runner creates a [`StructuredDataInput`] from raw file bytes and inserts
//! it into [`PipelineContext`] as `Arc<StructuredDataInput>`. Producer plugins
//! (e.g. the RML mapper) read it via `ctx.get::<StructuredDataInput>()`.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Input format detected from file extension or content sniffing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StructuredDataFormat {
    Json,
    Xml,
    Csv,
}

impl fmt::Display for StructuredDataFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StructuredDataFormat::Json => write!(f, "json"),
            StructuredDataFormat::Xml => write!(f, "xml"),
            StructuredDataFormat::Csv => write!(f, "csv"),
        }
    }
}

impl StructuredDataFormat {
    /// Detect format from a file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().trim_start_matches('.') {
            "json" | "jsonld" | "json-ld" => Some(Self::Json),
            "xml" | "rdf" | "rss" => Some(Self::Xml),
            "csv" | "tsv" => Some(Self::Csv),
            _ => None,
        }
    }

    /// Detect format from a filename.
    pub fn from_filename(filename: &str) -> Option<Self> {
        let ext = filename.rsplit('.').next()?;
        Self::from_extension(ext)
    }

    /// Detect format by sniffing the first bytes of the content.
    pub fn from_content(bytes: &[u8]) -> Option<Self> {
        let head = std::str::from_utf8(&bytes[..bytes.len().min(512)]).ok()?;
        let trimmed = head.trim_start();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            Some(Self::Json)
        } else if trimmed.starts_with('<') || trimmed.starts_with("<?xml") {
            Some(Self::Xml)
        } else if head.contains(',') && head.contains('\n') {
            Some(Self::Csv)
        } else {
            None
        }
    }
}

/// A single structured data file with its detected format.
#[derive(Clone, Debug)]
pub struct StructuredDataFile {
    pub filename: String,
    pub format: StructuredDataFormat,
    pub bytes: Vec<u8>,
}

/// Container for all structured data files in a pipeline run.
///
/// Inserted into `PipelineContext` as `Arc<StructuredDataInput>` by the runner
/// when `input_format == StructuredData`. Producer plugins read it via
/// `ctx.get::<StructuredDataInput>()`.
#[derive(Clone, Debug, Default)]
pub struct StructuredDataInput {
    pub files: Vec<StructuredDataFile>,
}

impl StructuredDataInput {
    /// Create from raw file bytes and filenames.
    ///
    /// Format is detected from the filename extension, falling back to content
    /// sniffing.
    pub fn from_raw(files: Vec<(String, Vec<u8>)>) -> Self {
        let files = files
            .into_iter()
            .map(|(filename, bytes)| {
                let format = StructuredDataFormat::from_filename(&filename)
                    .or_else(|| StructuredDataFormat::from_content(&bytes))
                    .unwrap_or(StructuredDataFormat::Csv); // fallback: treat as CSV
                StructuredDataFile {
                    filename,
                    format,
                    bytes,
                }
            })
            .collect();
        Self { files }
    }
}

/// RML mapping configuration.
///
/// The runner reads the `rml_mapping` module option (UTF-8 Turtle text),
/// creates this config, and inserts it into `PipelineContext` as
/// `Arc<RmlMappingConfig>`. The RML mapper producer reads it via
/// `ctx.get::<RmlMappingConfig>()`.
///
/// This follows the geometry-producer pattern: the runner reads the option,
/// creates a typed config, and inserts it into context. The producer plugin
/// does not access `ExecutionSettings` directly (which lives in the WASM
/// runner crate and is not available to standalone plugin crates).
#[derive(Clone, Debug)]
pub struct RmlMappingConfig {
    /// RML mapping file content (Turtle, UTF-8).
    pub mapping_turtle: String,
}
