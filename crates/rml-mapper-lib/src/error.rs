//! Error types for the RML Mapper
//!
//! This module defines all error types used throughout the library using thiserror.

use thiserror::Error;

/// Main error type for RML Mapper operations
#[derive(Error, Debug)]
pub enum RmlError {
    /// Error parsing RML mapping documents or data sources
    #[error("Parse error: {0}")]
    Parse(String),

    /// I/O error when reading/writing files
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Error accessing data sources
    #[error("Data access error: {0}")]
    Access(String),

    /// Error in mapping logic
    #[error("Mapping error: {0}")]
    Mapping(String),

    /// Error during execution of mappings
    #[error("Execution error: {0}")]
    Execution(String),

    /// Validation error
    #[error("Validation error: {0}")]
    Validation(String),

    /// Database connection or query error
    #[error("Database error: {0}")]
    Database(String),

    /// HTTP request error
    #[error("HTTP error: {0}")]
    Http(String),

    /// Function execution error
    #[error("Function error: {0}")]
    Function(String),

    /// Serialization/deserialization error
    #[error("Serialization error: {0}")]
    Serialization(String),
}

/// Result type alias for RML Mapper operations
pub type Result<T> = std::result::Result<T, RmlError>;

// Implement From traits for common error types
impl From<String> for RmlError {
    fn from(s: String) -> Self {
        RmlError::Parse(s)
    }
}

impl From<&str> for RmlError {
    fn from(s: &str) -> Self {
        RmlError::Parse(s.to_string())
    }
}
