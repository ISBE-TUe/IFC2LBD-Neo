//! RML Mapper - A Rust implementation of the RML (RDF Mapping Language) specification
//!
//! This library provides functionality for mapping data from various sources (CSV, JSON, XML, databases)
//! to RDF triples using RML mapping documents.

// Module declarations
pub mod error;
pub mod namespaces;
pub mod term;
pub mod store;
pub mod access;
pub mod records;
pub mod mapping;
pub mod executor;
pub mod termgenerator;
pub mod functions;
pub mod target;
pub mod conformer;

// Re-export key types for convenience
pub use error::{RmlError, Result};
pub use term::Term;
pub use store::{QuadStore, InMemoryQuadStore};
pub use mapping::MappingDocument;
pub use executor::Executor;

/// Library version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
