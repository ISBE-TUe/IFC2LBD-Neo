//! Template: Export plugin for IFC2LBD-Neo.
//!
//! # What export plugins do
//!
//! An export plugin defines **where** the final output goes. After the
//! serializer stage writes triples into byte streams, the active export
//! plugin's session provides writable sinks (`open_sink()`) and receives any
//! sidecar artefacts emitted by producers (`accept_derived_file()`).
//!
//! Common export targets:
//! - **File system** — write `.ttl` / `.nq` / `.frag` files to disk.
//! - **Blob storage** — upload to Azure Blob, AWS S3, GCS.
//! - **Database API** — POST N-Quads to an RDF store (Oxigraph, Stardog, etc.).
//! - **Browser download** — buffer bytes in WASM memory for download.
//!
//! # How the session API works
//!
//! 1. The orchestrator calls `ExportPlugin::start_session(ctx)` once before
//!    serialization begins. The session owns all mutable state.
//!
//! 2. For each output file, the orchestrator calls
//!    `ExportSession::open_sink(filename, mime_type, role)` and passes the
//!    returned `Box<dyn Write>` to the serializer. The serializer writes
//!    directly into it; the orchestrator drops the sink when serialization is
//!    done.
//!
//! 3. After all producers finish, sidecar files (emitted via
//!    `ctx.sidecar_tx` in producer plugins) are passed one-by-one to
//!    `ExportSession::accept_derived_file(file)`.
//!
//! 4. Finally, `ExportSession::finalize()` is called. Return a summary of
//!    every exported artefact.
//!
//! # Registration
//!
//! ```rust,ignore
//! registry.register_export(TemplateExportPlugin).unwrap();
//! ```
//!
//! Note: only **one** export plugin may be active per run. Use `conflicts_with`
//! in the manifest to declare mutual exclusion with the built-in exporters.
//!
//! # Adapting this template
//!
//! 1. Rename the structs and `TEMPLATE_EXPORT_ID`.
//! 2. Update `open_sink()` to open the desired destination (file, socket, etc.).
//! 3. Update `accept_derived_file()` to handle sidecar artefacts.
//! 4. Update `finalize()` to flush, close connections, and return summaries.
//! 5. Add `conflicts_with: vec![FILE_EXPORT_ID, STDOUT_EXPORT_ID]` to avoid
//!    multiple exporters being active simultaneously.

use std::io::{self, Write};

use lbd_pipeline::{
    DerivedFile, ExportError, ExportFileSummary, ExportPlugin, ExportSession, FailurePolicy,
    ParallelismMode, PipelineContext, PipelinePlugin, PipelineStage, PluginManifest,
};

/// Plugin ID — must be unique across all registered modules.
pub const TEMPLATE_EXPORT_ID: &str = "template-export-plugin";

// ---------------------------------------------------------------------------
// Plugin struct
// ---------------------------------------------------------------------------

/// A template export plugin that collects bytes in-memory.
///
/// Replace the in-memory collection with real upload/write logic.
/// For a blob-storage uploader, `open_sink()` would open a streaming upload
/// request and return a `Box<dyn Write>` that feeds bytes to the upload.
pub struct TemplateExportPlugin;

impl PipelinePlugin for TemplateExportPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: TEMPLATE_EXPORT_ID,
            display_name: "Template exporter",
            stage: PipelineStage::Export,
            description: "Example export plugin — replace with your implementation.",
            inputs: vec!["turtle-bytes", "nquads-bytes"],
            outputs: vec!["custom-destination"],
            requires: vec![],
            // Declare conflicts with built-in exporters so only one is active:
            conflicts_with: vec![
                lbd_pipeline::FILE_EXPORT_ID,
                lbd_pipeline::STDOUT_EXPORT_ID,
                lbd_pipeline::GRAFEO_EXPORT_ID,
            ],
            failure_policy: FailurePolicy::Required,
            parallelism: ParallelismMode::Serial,
            wasm_compatible: true,
            named_graph_slug: None,
            needs_full_graph: false,
        }
    }
}

impl ExportPlugin for TemplateExportPlugin {
    fn start_session(
        &self,
        _ctx: &PipelineContext,
    ) -> Result<Box<dyn ExportSession>, ExportError> {
        // Read any plugin-specific config from `ctx` here.
        // For example, retrieve a target URL from context:
        //   let url = ctx.get::<UploadTarget>()
        //       .ok_or_else(|| ExportError::Export("missing UploadTarget".into()))?;
        Ok(Box::new(TemplateExportSession {
            collected: Vec::new(),
            sidecar_summaries: Vec::new(),
        }))
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// An in-memory export session (replace with real upload logic).
struct TemplateExportSession {
    /// (filename, mime_type, role, bytes) — collected during the run.
    collected: Vec<(String, String, String, Vec<u8>)>,
    sidecar_summaries: Vec<ExportFileSummary>,
}

impl ExportSession for TemplateExportSession {
    fn open_sink(
        &mut self,
        filename: &str,
        mime_type: &str,
        role: &str,
    ) -> Result<Box<dyn Write + Send>, ExportError> {
        // For a real blob-storage upload, open a streaming upload here and
        // return a writer that feeds bytes to the upload stream.
        //
        // This template buffers everything in memory for simplicity.
        let entry_index = self.collected.len();
        self.collected.push((
            filename.to_string(),
            mime_type.to_string(),
            role.to_string(),
            Vec::new(),
        ));
        Ok(Box::new(InMemorySink {
            target: &mut self.collected[entry_index].3 as *mut Vec<u8>,
            _phantom: std::marker::PhantomData,
        }))
    }

    /// Handle a sidecar artefact emitted by a producer.
    ///
    /// Typical sidecar use-case: a geometry producer generates a `.frag` file
    /// (for a 3D viewer) and also emits RDF triples linking the IFC element
    /// IRIs to the geometry artefact IRI. The export plugin uploads the `.frag`
    /// to blob storage and the RDF triples go to the graph database.
    fn accept_derived_file(&mut self, file: DerivedFile) -> Result<(), ExportError> {
        // Replace this with a real upload:
        //   upload_to_blob_storage(&file.filename, &file.bytes)?;
        let bytes = file.bytes.len() as u64;
        self.sidecar_summaries.push(ExportFileSummary {
            filename: file.filename,
            mime_type: file.mime_type.to_string(),
            role: "derived".to_string(),
            bytes,
        });
        Ok(())
    }

    fn finalize(self: Box<Self>) -> Result<Vec<ExportFileSummary>, ExportError> {
        // Flush and close any open connections here.
        // For a blob-storage upload: call finish() on the upload stream.
        let mut summaries = self.sidecar_summaries;
        for (filename, mime_type, role, bytes) in self.collected {
            summaries.push(ExportFileSummary {
                filename,
                mime_type,
                role,
                bytes: bytes.len() as u64,
            });
        }
        Ok(summaries)
    }
}

// ---------------------------------------------------------------------------
// InMemorySink helper
// ---------------------------------------------------------------------------

/// A `Write` implementation that writes into a `Vec<u8>` via a raw pointer.
///
/// This is a minimal in-memory sink for the template. In a real export plugin,
/// replace this with a writer that streams bytes to the actual destination.
struct InMemorySink<'a> {
    target: *mut Vec<u8>,
    _phantom: std::marker::PhantomData<&'a mut Vec<u8>>,
}

// SAFETY: `InMemorySink` holds a raw pointer to a `Vec<u8>` that is owned
// by `TemplateExportSession`. The session lives on the stack in the current
// thread; we never send the sink across thread boundaries (callers hold it
// as `Box<dyn Write + Send>`, but within a single-threaded export).
unsafe impl Send for InMemorySink<'_> {}

impl Write for InMemorySink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: the pointer is valid for the lifetime of the session.
        unsafe { (*self.target).extend_from_slice(buf) };
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
