#![allow(unused_imports)]

//! Shared `ExportSession` wrapper used by all writer threads.
//!
//! Each writer thread (LBD serializer, IfcOWL sidecar, chunk writer worker)
//! holds an `Arc<SharedSession>`. It briefly locks the inner mutex to call
//! `open_sink()` for a new file handle, then writes to that handle freely.
//! After all writer threads have joined, the runner calls `finalize()` to
//! get back the file summaries from the underlying export plugin.

use std::io::Write;
use std::sync::{Arc, Mutex};

use lbd_pipeline::{DerivedFile, ExportError, ExportFileSummary, ExportSession};

pub type SharedSession = Arc<Mutex<Option<Box<dyn ExportSession>>>>;

pub fn new_shared(session: Box<dyn ExportSession>) -> SharedSession {
    Arc::new(Mutex::new(Some(session)))
}

/// Open a sink on the shared session. Lock is held only for the open_sink call.
pub fn open_sink(
    shared: &SharedSession,
    filename: &str,
    mime_type: &str,
    role: &str,
) -> Result<Box<dyn Write + Send>, ExportError> {
    let mut guard = shared
        .lock()
        .map_err(|_| ExportError::Export("export session mutex poisoned".to_string()))?;
    let session = guard
        .as_mut()
        .ok_or_else(|| ExportError::Export("export session already finalized".to_string()))?;
    session.open_sink(filename, mime_type, role)
}

/// Forward a producer-emitted sidecar file into the active export session.
pub fn accept_derived_file(shared: &SharedSession, file: DerivedFile) -> Result<(), ExportError> {
    let mut guard = shared
        .lock()
        .map_err(|_| ExportError::Export("export session mutex poisoned".to_string()))?;
    let session = guard
        .as_mut()
        .ok_or_else(|| ExportError::Export("export session already finalized".to_string()))?;
    session.accept_derived_file(file)
}

/// Consume the session and call `finalize()`. All clones of the Arc must be
/// dropped first (writer threads must be joined).
pub fn finalize(shared: SharedSession) -> Result<Vec<ExportFileSummary>, ExportError> {
    let mutex = Arc::try_unwrap(shared).map_err(|_| {
        ExportError::Export(
            "export session still has live references; thread joins missing?".to_string(),
        )
    })?;
    let session = mutex
        .into_inner()
        .map_err(|_| ExportError::Export("export session mutex poisoned".to_string()))?
        .ok_or_else(|| ExportError::Export("export session already taken".to_string()))?;
    session.finalize()
}
