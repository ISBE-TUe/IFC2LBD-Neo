/// Errors that can occur during STEP file parsing.
#[derive(Debug, thiserror::Error)]
pub enum StepError {
    #[error("I/O error: {0}")]
    Io(String),

    #[error("Invalid STEP header: {0}")]
    InvalidHeader(String),

    #[error("Parse error at line {line}: {message}")]
    Parse { line: u64, message: String },

    #[error("Unresolved reference: #{0}")]
    UnresolvedReference(u64),

    #[error("Unsupported IFC schema: {0}")]
    UnsupportedSchema(String),
}
