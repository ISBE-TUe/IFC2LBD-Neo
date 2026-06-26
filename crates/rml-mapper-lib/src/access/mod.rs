//! Data Access Layer
//!
//! This module provides access to various data sources for RML mapping,
//! matching the Java RML Mapper's Access architecture.
//!
//! # Architecture
//!
//! The module follows the Java AccessFactory.java design:
//! - `Access` trait: Common interface for all data access implementations
//! - `LocalFileAccess`: Access to local file system
//! - `RemoteFileAccess`: Access to HTTP/HTTPS resources
//! - `DatabaseAccess`: Access to relational databases (MySQL, PostgreSQL, SQLite, MSSQL)
//! - `SparqlAccess`: Access to SPARQL endpoints
//! - `AccessFactory`: Factory for creating Access instances from RML logical sources
//!
//! # Examples
//!
//! ```
//! use rml_mapper::access::{Access, LocalFileAccess};
//! use std::path::PathBuf;
//!
//! // Access a local file
//! let access = LocalFileAccess::new(PathBuf::from("data.csv"), None, None);
//! // let reader = access.get_reader().unwrap(); // Would fail if file does not exist
//! ```

use crate::error::{Result, RmlError};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Trait for accessing data sources
///
/// This trait defines the interface that all data access implementations must provide,
/// matching the Java RML Mapper's Access interface.
pub trait Access: Send + Sync {
    /// Returns the data as a reader
    ///
    /// # Returns
    ///
    /// A boxed reader that can be used to read the data source
    fn get_reader(&self) -> Result<Box<dyn Read + Send>>;

    /// Returns the content type/MIME type if known
    ///
    /// # Returns
    ///
    /// Optional content type string (e.g., "text/csv", "application/json")
    fn content_type(&self) -> Option<&str>;

    /// Returns a unique identifier for caching purposes
    ///
    /// # Returns
    ///
    /// A string that uniquely identifies this data source
    fn cache_key(&self) -> String;
}

/// Access to local file system
///
/// Provides access to files on the local file system, with support for
/// relative path resolution against a base path.
///
/// # Examples
///
/// ```
/// use rml_mapper::access::LocalFileAccess;
/// use std::path::PathBuf;
///
/// let access = LocalFileAccess::new(
///     PathBuf::from("data.csv"),
///     Some(PathBuf::from("/base/path")),
///     Some("text/csv".to_string())
/// );
/// ```
#[derive(Debug, Clone)]
pub struct LocalFileAccess {
    /// Path to the file
    path: PathBuf,
    /// Base path for resolving relative paths
    base_path: Option<PathBuf>,
    /// Content type/MIME type
    content_type: Option<String>,
}

impl LocalFileAccess {
    /// Creates a new local file access
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the file
    /// * `base_path` - Optional base path for resolving relative paths
    /// * `content_type` - Optional content type/MIME type
    pub fn new(path: PathBuf, base_path: Option<PathBuf>, content_type: Option<String>) -> Self {
        Self {
            path,
            base_path,
            content_type,
        }
    }

    /// Resolves the file path, handling relative paths
    fn resolve_path(&self) -> PathBuf {
        if self.path.is_absolute() {
            self.path.clone()
        } else if let Some(base) = &self.base_path {
            base.join(&self.path)
        } else {
            self.path.clone()
        }
    }

    /// Detects content type from file extension
    pub(crate) fn detect_content_type(path: &Path) -> Option<String> {
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(|ext| match ext.to_lowercase().as_str() {
                "csv" => Some("text/csv"),
                "json" => Some("application/json"),
                "xml" => Some("application/xml"),
                "ttl" => Some("text/turtle"),
                "nt" => Some("application/n-triples"),
                "nq" => Some("application/n-quads"),
                "rdf" => Some("application/rdf+xml"),
                "jsonld" => Some("application/ld+json"),
                "html" | "htm" => Some("text/html"),
                "txt" => Some("text/plain"),
                _ => None,
            })
            .map(String::from)
    }
}

impl Access for LocalFileAccess {
    fn get_reader(&self) -> Result<Box<dyn Read + Send>> {
        let resolved_path = self.resolve_path();
        let file = File::open(&resolved_path).map_err(|e| {
            RmlError::Access(format!(
                "Failed to open file '{}': {}",
                resolved_path.display(),
                e
            ))
        })?;
        Ok(Box::new(file))
    }

    fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    fn cache_key(&self) -> String {
        format!("file://{}", self.resolve_path().display())
    }
}


#[cfg(feature = "remote")]
mod remote;
#[cfg(feature = "remote")]
pub use remote::RemoteFileAccess;

#[cfg(feature = "database")]
mod database;
#[cfg(feature = "database")]
pub use database::{DatabaseAccess, DatabaseType};
