use std::io::{self, Write};

#[cfg(target_arch = "wasm32")]
use js_sys::{Function, Object, Reflect, Uint8Array};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;


/// Emit a `stageEvent` through the JS sink callback.
/// This allows the UI DAG to update in real-time as stages start/complete.
#[cfg(target_arch = "wasm32")]
pub(crate) fn emit_stage_event(
    sink: &Function,
    plugin_id: &str,
    stage: &str,
    status: &str,
    duration_ms: u64,
    bytes_out: u64,
    triples_out: u64,
    error: Option<&str>,
) -> Result<(), SerializerError> {
    let event = Object::new();
    set_event_str(&event, "type", "stageEvent")?;
    set_event_str(&event, "pluginId", plugin_id)?;
    set_event_str(&event, "stage", stage)?;
    set_event_str(&event, "status", status)?;
    Reflect::set(
        &event,
        &JsValue::from_str("durationMs"),
        &JsValue::from_f64(duration_ms as f64),
    )
    .map_err(|js_err| {
        let msg = js_err
            .as_string()
            .unwrap_or_else(|| "JS error setting durationMs".to_string());
        SerializerError::Io(io::Error::new(io::ErrorKind::Other, msg))
    })?;
    Reflect::set(
        &event,
        &JsValue::from_str("bytesOut"),
        &JsValue::from_f64(bytes_out as f64),
    )
    .map_err(|js_err| {
        let msg = js_err
            .as_string()
            .unwrap_or_else(|| "JS error setting bytesOut".to_string());
        SerializerError::Io(io::Error::new(io::ErrorKind::Other, msg))
    })?;
    Reflect::set(
        &event,
        &JsValue::from_str("triplesOut"),
        &JsValue::from_f64(triples_out as f64),
    )
    .map_err(|js_err| {
        let msg = js_err
            .as_string()
            .unwrap_or_else(|| "JS error setting triplesOut".to_string());
        SerializerError::Io(io::Error::new(io::ErrorKind::Other, msg))
    })?;
    if let Some(err_msg) = error {
        set_event_str(&event, "error", err_msg)?;
    }
    sink.call1(&JsValue::NULL, &event).map_err(|js_err| {
        let msg = js_err
            .as_string()
            .unwrap_or_else(|| "JS error in emit_stage_event".to_string());
        SerializerError::Io(io::Error::new(io::ErrorKind::Other, msg))
    })?;
    Ok(())
}

#[derive(Default)]
pub(crate) struct CountingWriter {
    pub bytes: u64,
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) struct SinkChunkWriter<'a> {
    sink: &'a Function,
    filename: String,
    mime_type: String,
    role: String,
    bytes: u64,
    chunk_size: usize,
    pending: Vec<u8>,
    max_pending: usize,
    /// Hard limit on pending bytes. If `pending.len()` would exceed this after a write,
    /// the writer flushes immediately regardless of chunk_size. Zero means "no limit"
    /// (backward compatible — behaves like before).
    max_pending_bytes: usize,
    /// When true, suppress intermediate chunk flushes and gzip-compress all accumulated
    /// bytes in `finish()` before sending to JS.
    compress: bool,
}

#[cfg(target_arch = "wasm32")]
impl<'a> SinkChunkWriter<'a> {
    pub fn new(
        sink: &'a Function,
        filename: String,
        mime_type: &str,
        role: &str,
        chunk_size: usize,
        max_pending_bytes: usize,
        compress: bool,
    ) -> Result<Self, SerializerError> {
        let writer = Self {
            sink,
            filename,
            mime_type: mime_type.to_string(),
            role: role.to_string(),
            bytes: 0,
            chunk_size,
            pending: Vec::with_capacity(chunk_size),
            max_pending: 0,
            max_pending_bytes,
            compress,
        };
        writer.emit_start()?;
        Ok(writer)
    }

    pub fn finish(mut self) -> Result<(OutputFileSummary, usize, usize), SerializerError> {
        if self.compress && !self.pending.is_empty() {
            let uncompressed = std::mem::take(&mut self.pending);
            let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
            enc.write_all(&uncompressed)
                .map_err(|e| SerializerError::Io(io::Error::new(io::ErrorKind::Other, e.to_string())))?;
            self.pending = enc.finish()
                .map_err(|e| SerializerError::Io(io::Error::new(io::ErrorKind::Other, e.to_string())))?;
        }
        self.flush_pending()?;
        self.emit_end()?;
        Ok((
            OutputFileSummary {
                filename: self.filename,
                mime_type: self.mime_type,
                role: self.role,
                bytes: self.bytes,
            },
            self.max_pending,
            self.chunk_size,
        ))
    }

    fn flush_pending(&mut self) -> Result<(), SerializerError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let event = Object::new();
        set_event_str(&event, "type", "fileChunk")?;
        set_event_str(&event, "filename", &self.filename)?;
        let chunk = Uint8Array::from(self.pending.as_slice());
        Reflect::set(&event, &JsValue::from_str("chunk"), &chunk).map_err(|js_err| {
            let msg = js_err
                .as_string()
                .unwrap_or_else(|| "unknown JS error in flush_pending".to_string());
            SerializerError::Io(io::Error::new(io::ErrorKind::Other, msg))
        })?;
        self.sink.call1(&JsValue::NULL, &event).map_err(|js_err| {
            let msg = js_err
                .as_string()
                .unwrap_or_else(|| "unknown JS error in sink.call1".to_string());
            SerializerError::Io(io::Error::new(io::ErrorKind::Other, msg))
        })?;
        self.pending.clear();
        Ok(())
    }

    fn emit_start(&self) -> Result<(), SerializerError> {
        let event = Object::new();
        set_event_str(&event, "type", "fileStart")?;
        set_event_str(&event, "filename", &self.filename)?;
        set_event_str(&event, "mimeType", &self.mime_type)?;
        set_event_str(&event, "role", &self.role)?;
        self.sink.call1(&JsValue::NULL, &event).map_err(|js_err| {
            let msg = js_err
                .as_string()
                .unwrap_or_else(|| "unknown JS error in emit_start".to_string());
            SerializerError::Io(io::Error::new(io::ErrorKind::Other, msg))
        })?;
        Ok(())
    }

    fn emit_end(&self) -> Result<(), SerializerError> {
        let event = Object::new();
        set_event_str(&event, "type", "fileEnd")?;
        set_event_str(&event, "filename", &self.filename)?;
        Reflect::set(
            &event,
            &JsValue::from_str("bytes"),
            &JsValue::from_f64(self.bytes as f64),
        )
        .map_err(|js_err| {
            let msg = js_err
                .as_string()
                .unwrap_or_else(|| "unknown JS error in emit_end set bytes".to_string());
            SerializerError::Io(io::Error::new(io::ErrorKind::Other, msg))
        })?;
        self.sink.call1(&JsValue::NULL, &event).map_err(|js_err| {
            let msg = js_err
                .as_string()
                .unwrap_or_else(|| "unknown JS error in emit_end call1".to_string());
            SerializerError::Io(io::Error::new(io::ErrorKind::Other, msg))
        })?;
        Ok(())
    }

    /// Whether a flush is needed right now — either the chunk is full or the
    /// pending-byte safety limit would be exceeded.
    fn should_flush(&self) -> bool {
        if self.pending.len() >= self.chunk_size {
            return true;
        }
        if self.max_pending_bytes > 0 && self.pending.len() >= self.max_pending_bytes {
            return true;
        }
        false
    }
}

#[cfg(target_arch = "wasm32")]
impl Write for SinkChunkWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        if self.pending.len() > self.max_pending {
            self.max_pending = self.pending.len();
        }
        self.bytes += buf.len() as u64;
        // When compressing, accumulate everything — gzip requires a single contiguous
        // stream finalized in finish(). Intermediate flushes would produce invalid chunks.
        if !self.compress && self.should_flush() {
            self.flush_pending()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_pending()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn set_event_str(event: &Object, key: &str, value: &str) -> Result<(), SerializerError> {
    Reflect::set(event, &JsValue::from_str(key), &JsValue::from_str(value)).map_err(|js_err| {
        let msg = js_err
            .as_string()
            .unwrap_or_else(|| "unknown JS error in set_event_str".to_string());
        SerializerError::Io(io::Error::new(io::ErrorKind::Other, msg))
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// SinkQuadChunkWriter — splits N-Quads into multiple chunk files through the
// JS sink, analogous to the CLI's QuadChunkWriter but writing to browser
// download chunks instead of filesystem.
// ---------------------------------------------------------------------------

/// Chunking policy for N-Quads output in the browser sink.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SinkChunkingMode {
    None,
    Lines,
    Bytes,
}

/// Metadata for one completed chunk file.
#[derive(Debug)]
pub(crate) struct SinkChunkEntry {
    pub filename: String,
    pub bytes: u64,
    pub lines: u64,
}

#[cfg(target_arch = "wasm32")]
pub(crate) struct SinkQuadChunkWriter<'a> {
    sink: &'a Function,
    chunk_prefix: String,
    mime_type: String,
    mode: SinkChunkingMode,
    lines_per_chunk: u64,
    bytes_per_chunk: u64,
    chunk_size: usize,
    max_pending_bytes: usize,
    /// Current open chunk writer (or None between chunks)
    current_writer: Option<SinkChunkWriter<'a>>,
    current_index: usize,
    current_bytes: u64,
    current_lines: u64,
    total_lines: u64,
    /// Pending partial line data
    pending_line: Vec<u8>,
    /// Completed chunk entries for manifest
    entries: Vec<SinkChunkEntry>,
    compress: bool,
}

#[cfg(target_arch = "wasm32")]
impl<'a> SinkQuadChunkWriter<'a> {
    pub fn new(
        sink: &'a Function,
        chunk_prefix: String,
        mode: SinkChunkingMode,
        lines_per_chunk: usize,
        bytes_per_chunk: usize,
        chunk_size: usize,
        max_pending_bytes: usize,
        compress: bool,
    ) -> Result<Self, SerializerError> {
        Ok(Self {
            sink,
            mime_type: "application/n-quads".to_string(),
            chunk_prefix,
            mode,
            lines_per_chunk: lines_per_chunk as u64,
            bytes_per_chunk: bytes_per_chunk as u64,
            chunk_size,
            max_pending_bytes,
            current_writer: None,
            current_index: 0,
            current_bytes: 0,
            current_lines: 0,
            total_lines: 0,
            pending_line: Vec::new(),
            entries: Vec::new(),
            compress,
        })
    }

    fn ensure_open(&mut self) -> Result<(), SerializerError> {
        if self.current_writer.is_none() {
            let gz = if self.compress { ".gz" } else { "" };
            let filename = format!("{}.part-{:03}.nq{gz}", self.chunk_prefix, self.current_index);
            let writer = SinkChunkWriter::new(
                self.sink,
                filename.clone(),
                &self.mime_type,
                "chunk",
                self.chunk_size,
                self.max_pending_bytes,
                self.compress,
            )?;
            self.current_writer = Some(writer);
        }
        Ok(())
    }

    fn should_rotate(&self) -> bool {
        if self.current_writer.is_none() || self.current_lines == 0 {
            return false;
        }
        match self.mode {
            SinkChunkingMode::None => false,
            SinkChunkingMode::Lines => self.current_lines >= self.lines_per_chunk,
            SinkChunkingMode::Bytes => self.current_bytes >= self.bytes_per_chunk,
        }
    }

    fn close_current(&mut self) -> Result<(), SerializerError> {
        if let Some(writer) = self.current_writer.take() {
            let filename = writer.filename.clone();
            let (summary, _peak, _chunk) = writer.finish()?;
            self.entries.push(SinkChunkEntry {
                filename,
                bytes: summary.bytes,
                lines: self.current_lines,
            });
            self.current_bytes = 0;
            self.current_lines = 0;
            self.current_index += 1;
        }
        Ok(())
    }

    fn write_complete_line(&mut self, line: &[u8]) -> Result<(), SerializerError> {
        if self.should_rotate() {
            self.close_current()?;
        }
        self.ensure_open()?;
        if let Some(ref mut writer) = self.current_writer {
            writer.write_all(line).map_err(|e| SerializerError::Io(e))?;
        }
        let line_len = line.len() as u64;
        self.current_bytes += line_len;
        self.current_lines += 1;
        self.total_lines += 1;
        Ok(())
    }

    /// Finalize all chunks and emit the manifest as a JSON file.
    pub fn finish(mut self) -> Result<Vec<OutputFileSummary>, SerializerError> {
        // Flush any remaining partial line
        if !self.pending_line.is_empty() {
            if !self.pending_line.ends_with(b"\n") {
                self.pending_line.push(b'\n');
            }
            let line = std::mem::take(&mut self.pending_line);
            self.write_complete_line(&line)?;
        }
        // Close the current open chunk
        self.close_current()?;

        // Emit manifest JSON file
        let manifest = self.build_manifest();
        let manifest_json = serde_json::to_string_pretty(&manifest).map_err(|e| {
            SerializerError::Io(io::Error::new(io::ErrorKind::Other, e.to_string()))
        })?;
        let manifest_filename = format!("{}.manifest.json", self.chunk_prefix);
        let manifest_bytes = manifest_json.as_bytes();
        let mut manifest_writer = SinkChunkWriter::new(
            self.sink,
            manifest_filename,
            "application/json",
            "manifest",
            self.chunk_size,
            self.max_pending_bytes,
            false,
        )?;
        manifest_writer
            .write_all(manifest_bytes)
            .map_err(|e| SerializerError::Io(e))?;
        let (manifest_summary, _peak, _chunk) = manifest_writer.finish()?;

        let mut summaries: Vec<OutputFileSummary> = self
            .entries
            .iter()
            .map(|e| OutputFileSummary {
                filename: e.filename.clone(),
                mime_type: self.mime_type.clone(),
                role: "chunk".to_string(),
                bytes: e.bytes,
            })
            .collect();
        summaries.push(manifest_summary);
        Ok(summaries)
    }

    fn build_manifest(&self) -> serde_json::Value {
        use serde_json::json;
        let mut files = Vec::new();
        for entry in &self.entries {
            files.push(json!({
                "file": entry.filename,
                "bytes": entry.bytes,
                "lines": entry.lines,
            }));
        }
        json!({
            "chunking": match self.mode {
                SinkChunkingMode::None => "none",
                SinkChunkingMode::Lines => "lines",
                SinkChunkingMode::Bytes => "bytes",
            },
            "chunk_size_lines": self.lines_per_chunk,
            "chunk_size_bytes": self.bytes_per_chunk,
            "chunk_prefix": self.chunk_prefix,
            "files": files,
            "total_lines": self.total_lines,
            "total_triples_estimate": self.total_lines,
        })
    }
}

#[cfg(target_arch = "wasm32")]
impl Write for SinkQuadChunkWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut cursor = 0usize;

        // If we have a pending partial line, extend it
        if !self.pending_line.is_empty() {
            if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                self.pending_line.extend_from_slice(&buf[..=pos]);
                let line = std::mem::take(&mut self.pending_line);
                self.write_complete_line(&line)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                cursor = pos + 1;
            } else {
                self.pending_line.extend_from_slice(buf);
                return Ok(buf.len());
            }
        }

        // Process complete lines from buf
        while cursor < buf.len() {
            let remainder = &buf[cursor..];
            let Some(pos) = remainder.iter().position(|&b| b == b'\n') else {
                break;
            };
            let end = cursor + pos + 1;
            self.write_complete_line(&buf[cursor..end])
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
            cursor = end;
        }

        // Remaining partial line
        if cursor < buf.len() {
            self.pending_line.extend_from_slice(&buf[cursor..]);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(ref mut writer) = self.current_writer {
            writer.flush()?;
        }
        Ok(())
    }
}
