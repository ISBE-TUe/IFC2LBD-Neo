use std::io::{self, Write};

#[cfg(target_arch = "wasm32")]
use js_sys::{Function, Object, Reflect, Uint8Array};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;

use crate::types::OutputFileSummary;
use lbd_serializer::SerializerError;

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
        };
        writer.emit_start()?;
        Ok(writer)
    }

    pub fn finish(mut self) -> Result<(OutputFileSummary, usize, usize), SerializerError> {
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
        if self.should_flush() {
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
