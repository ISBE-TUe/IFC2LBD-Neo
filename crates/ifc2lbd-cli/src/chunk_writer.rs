//! N-Quads file chunking writer.
//!
//! Splits N-Quads output into multiple files based on line count, byte size,
//! or core-count heuristics. Used when `--module-opt neo-nquads-serializer.chunking`
//! is set to a non-None value.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::thread;

use anyhow::Context;
use clap::ValueEnum;
use serde::Serialize;

const SERIALIZER_BUFFER_BYTES: usize = 1024 * 1024;
const CORE_CHUNK_BLOCK_LINES: u64 = 4096;
const CORE_CHUNK_BATCH_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum QuadChunkingMode {
    None,
    Lines,
    Bytes,
    Cores,
}

#[derive(Debug, Serialize)]
pub(crate) struct QuadChunkManifest {
    chunking: String,
    chunk_size_lines: u64,
    chunk_size_bytes: u64,
    chunk_prefix: String,
    min_chunk_count: u64,
    core_chunk_count: u64,
    files: Vec<QuadChunkEntry>,
    total_lines: u64,
    total_triples_estimate: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct QuadChunkEntry {
    file: String,
    bytes: u64,
    lines: u64,
}

#[derive(Debug)]
pub(crate) struct QuadChunkWriter {
    output_dir: PathBuf,
    chunk_prefix: String,
    mode: QuadChunkingMode,
    lines_per_chunk: u64,
    bytes_per_chunk: u64,
    min_chunk_count: u64,
    core_chunk_count: u64,
    current_index: usize,
    current_file: Option<BufWriter<File>>,
    current_bytes: u64,
    current_lines: u64,
    pending_buffer: Vec<u8>,
    manifest_entries: Vec<QuadChunkEntry>,
    total_lines: u64,
    core_current_writer: usize,
    core_lines_in_block: u64,
    core_sender: Option<crossbeam::channel::Sender<CoreChunkWriteMsg>>,
    core_writer_thread: Option<thread::JoinHandle<anyhow::Result<()>>>,
    core_pending_buffers: Vec<Vec<u8>>,
    core_bytes: Vec<u64>,
    core_lines: Vec<u64>,
}

#[derive(Debug)]
pub(crate) enum CoreChunkWriteMsg {
    Data { index: usize, bytes: Vec<u8> },
}

impl QuadChunkWriter {
    pub(crate) fn new(
        output_dir: PathBuf,
        chunk_prefix: String,
        mode: QuadChunkingMode,
        lines_per_chunk: usize,
        bytes_per_chunk: usize,
        min_chunk_count: usize,
        core_count_override: Option<usize>,
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&output_dir).with_context(|| {
            format!(
                "failed to create quad chunk output dir {}",
                output_dir.display()
            )
        })?;
        let available_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let selected_cores = core_count_override.unwrap_or(available_cores);
        let core_chunk_count = if mode == QuadChunkingMode::Cores {
            selected_cores.max(min_chunk_count) as u64
        } else {
            0
        };

        let mut writer = Self {
            output_dir,
            chunk_prefix,
            mode,
            lines_per_chunk: lines_per_chunk as u64,
            bytes_per_chunk: bytes_per_chunk as u64,
            min_chunk_count: min_chunk_count as u64,
            core_chunk_count,
            current_index: 0,
            current_file: None,
            current_bytes: 0,
            current_lines: 0,
            pending_buffer: Vec::new(),
            manifest_entries: Vec::new(),
            total_lines: 0,
            core_current_writer: 0,
            core_lines_in_block: 0,
            core_sender: None,
            core_writer_thread: None,
            core_pending_buffers: Vec::new(),
            core_bytes: Vec::new(),
            core_lines: Vec::new(),
        };
        if writer.mode == QuadChunkingMode::Cores {
            writer.start_core_chunk_writer_thread(core_chunk_count as usize)?;
        }
        Ok(writer)
    }

    pub(crate) fn finish(&mut self) -> anyhow::Result<()> {
        if !self.pending_buffer.is_empty() {
            if !self.pending_buffer.ends_with(b"\n") {
                self.pending_buffer.push(b'\n');
            }
            self.consume_complete_lines()?;
        }
        if self.mode == QuadChunkingMode::Cores {
            self.flush_core_pending_buffers()?;
            self.close_core_chunk_files()?;
        } else {
            self.close_current_file()?;
        }
        let manifest_path = self
            .output_dir
            .join(format!("{}.manifest.json", self.chunk_prefix));
        let manifest = QuadChunkManifest {
            chunking: match self.mode {
                QuadChunkingMode::None => "none".to_string(),
                QuadChunkingMode::Lines => "lines".to_string(),
                QuadChunkingMode::Bytes => "bytes".to_string(),
                QuadChunkingMode::Cores => "cores".to_string(),
            },
            chunk_size_lines: self.lines_per_chunk,
            chunk_size_bytes: self.bytes_per_chunk,
            chunk_prefix: self.chunk_prefix.clone(),
            min_chunk_count: self.min_chunk_count,
            core_chunk_count: self.core_chunk_count,
            files: self.manifest_entries.clone(),
            total_lines: self.total_lines,
            total_triples_estimate: self.total_lines,
        };
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .context("failed to serialize quad chunk manifest JSON")?;
        std::fs::write(&manifest_path, manifest_json)
            .with_context(|| format!("failed to write manifest {}", manifest_path.display()))?;
        Ok(())
    }

    fn write_complete_line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        if self.mode == QuadChunkingMode::Cores {
            return self.write_round_robin_line(line);
        }
        if self.current_file.is_none() {
            self.open_next_chunk_file()?;
        }
        let line_len = line.len() as u64;
        if self.should_rotate(line_len) {
            self.close_current_file()?;
            self.open_next_chunk_file()?;
        }
        if let Some(file) = self.current_file.as_mut() {
            file.write_all(line)?;
        }
        self.current_bytes += line_len;
        self.current_lines += 1;
        self.total_lines += 1;
        Ok(())
    }

    fn consume_complete_lines(&mut self) -> anyhow::Result<()> {
        loop {
            let Some(pos) = self.pending_buffer.iter().position(|&b| b == b'\n') else {
                break;
            };
            let line = self.pending_buffer[..=pos].to_vec();
            self.pending_buffer.drain(..=pos);
            self.write_complete_line(&line)?;
        }
        Ok(())
    }

    fn should_rotate(&self, next_line_len: u64) -> bool {
        if self.current_file.is_none() || self.current_lines == 0 {
            return false;
        }
        match self.mode {
            QuadChunkingMode::None => false,
            QuadChunkingMode::Lines => self.current_lines >= self.lines_per_chunk,
            QuadChunkingMode::Bytes => self.current_bytes + next_line_len > self.bytes_per_chunk,
            QuadChunkingMode::Cores => false,
        }
    }

    fn open_next_chunk_file(&mut self) -> anyhow::Result<()> {
        let file_name = format!("{}.part-{:03}.nq", self.chunk_prefix, self.current_index);
        let path = self.output_dir.join(file_name);
        let file = File::create(&path)
            .with_context(|| format!("failed to create quad chunk {}", path.display()))?;
        self.current_file = Some(BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, file));
        self.current_bytes = 0;
        self.current_lines = 0;
        self.current_index += 1;
        Ok(())
    }

    fn close_current_file(&mut self) -> anyhow::Result<()> {
        if let Some(mut file) = self.current_file.take() {
            file.flush()?;
            let file_name = format!(
                "{}.part-{:03}.nq",
                self.chunk_prefix,
                self.current_index - 1
            );
            self.manifest_entries.push(QuadChunkEntry {
                file: file_name,
                bytes: self.current_bytes,
                lines: self.current_lines,
            });
            self.current_bytes = 0;
            self.current_lines = 0;
        }
        Ok(())
    }

    fn start_core_chunk_writer_thread(&mut self, count: usize) -> anyhow::Result<()> {
        let mut paths = Vec::with_capacity(count);
        self.core_bytes = vec![0; count];
        self.core_lines = vec![0; count];
        self.core_pending_buffers = (0..count)
            .map(|_| Vec::with_capacity(CORE_CHUNK_BATCH_BYTES))
            .collect();
        for i in 0..count {
            let file_name = format!("{}.part-{:03}.nq", self.chunk_prefix, i);
            let path = self.output_dir.join(&file_name);
            paths.push(path);
        }
        let (sender, receiver) = crossbeam::channel::bounded::<CoreChunkWriteMsg>(64);
        let writer_thread = thread::spawn(move || -> anyhow::Result<()> {
            let mut writers = Vec::with_capacity(paths.len());
            for path in &paths {
                let file = File::create(path)
                    .with_context(|| format!("failed to create quad chunk {}", path.display()))?;
                writers.push(BufWriter::with_capacity(SERIALIZER_BUFFER_BYTES, file));
            }
            for msg in receiver {
                match msg {
                    CoreChunkWriteMsg::Data { index, bytes } => {
                        let writer = writers.get_mut(index).ok_or_else(|| {
                            anyhow::anyhow!("invalid chunk index {} in writer thread", index)
                        })?;
                        writer.write_all(&bytes)?;
                    }
                }
            }
            for writer in &mut writers {
                writer.flush()?;
            }
            Ok(())
        });
        self.core_sender = Some(sender);
        self.core_writer_thread = Some(writer_thread);
        Ok(())
    }

    fn write_round_robin_line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        if self.core_pending_buffers.is_empty() {
            return Ok(());
        }
        let idx = self.core_current_writer % self.core_pending_buffers.len();
        self.core_pending_buffers[idx].extend_from_slice(line);
        let line_len = line.len() as u64;
        self.core_bytes[idx] += line_len;
        self.core_lines[idx] += 1;
        self.total_lines += 1;
        if self.core_pending_buffers[idx].len() >= CORE_CHUNK_BATCH_BYTES {
            self.flush_core_buffer(idx)?;
        }
        self.core_lines_in_block += 1;
        if self.core_lines_in_block >= CORE_CHUNK_BLOCK_LINES
            && !self.core_pending_buffers.is_empty()
        {
            self.core_current_writer =
                (self.core_current_writer + 1) % self.core_pending_buffers.len();
            self.core_lines_in_block = 0;
        }
        Ok(())
    }

    fn close_core_chunk_files(&mut self) -> anyhow::Result<()> {
        self.core_sender.take();
        if let Some(handle) = self.core_writer_thread.take() {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("core chunk writer thread panicked"))??;
        }
        for idx in 0..self.core_bytes.len() {
            let file_name = format!("{}.part-{:03}.nq", self.chunk_prefix, idx);
            self.manifest_entries.push(QuadChunkEntry {
                file: file_name,
                bytes: self.core_bytes[idx],
                lines: self.core_lines[idx],
            });
        }
        Ok(())
    }

    fn flush_core_buffer(&mut self, index: usize) -> anyhow::Result<()> {
        let bytes = std::mem::take(
            self.core_pending_buffers
                .get_mut(index)
                .ok_or_else(|| anyhow::anyhow!("invalid pending buffer index {}", index))?,
        );
        if bytes.is_empty() {
            return Ok(());
        }
        let sender = self
            .core_sender
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing core chunk sender"))?;
        sender
            .send(CoreChunkWriteMsg::Data { index, bytes })
            .map_err(|_| anyhow::anyhow!("core chunk writer channel closed"))?;
        Ok(())
    }

    fn flush_core_pending_buffers(&mut self) -> anyhow::Result<()> {
        for idx in 0..self.core_pending_buffers.len() {
            self.flush_core_buffer(idx)?;
        }
        Ok(())
    }
}

impl Write for QuadChunkWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut cursor = 0usize;
        if !self.pending_buffer.is_empty() {
            if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                self.pending_buffer.extend_from_slice(&buf[..=pos]);
                let line = std::mem::take(&mut self.pending_buffer);
                self.write_complete_line(&line)
                    .map_err(std::io::Error::other)?;
                cursor = pos + 1;
            } else {
                self.pending_buffer.extend_from_slice(buf);
                return Ok(buf.len());
            }
        }

        while cursor < buf.len() {
            let remainder = &buf[cursor..];
            let Some(pos) = remainder.iter().position(|&b| b == b'\n') else {
                break;
            };
            let end = cursor + pos + 1;
            self.write_complete_line(&buf[cursor..end])
                .map_err(std::io::Error::other)?;
            cursor = end;
        }

        if cursor < buf.len() {
            self.pending_buffer.extend_from_slice(&buf[cursor..]);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(file) = self.current_file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

const MIN_CORE_CHUNK_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CORE_CHUNK_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) fn resolve_quad_chunk_output_dir(output_file: Option<&Path>) -> PathBuf {
    output_file
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn resolve_effective_core_chunk_count_for_estimated_bytes(
    mode: QuadChunkingMode,
    requested_core_count: Option<usize>,
    min_chunk_count: usize,
    estimated_nq_bytes: u64,
) -> Option<usize> {
    if mode != QuadChunkingMode::Cores {
        return requested_core_count;
    }
    let available_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let requested = requested_core_count
        .unwrap_or(available_cores)
        .max(min_chunk_count);
    let min_chunks_by_max_size =
        ((estimated_nq_bytes + (MAX_CORE_CHUNK_BYTES - 1)) / MAX_CORE_CHUNK_BYTES).max(1) as usize;
    let max_chunks_by_min_size = (estimated_nq_bytes / MIN_CORE_CHUNK_BYTES).max(1) as usize;
    let floor = min_chunk_count.max(min_chunks_by_max_size);
    let ceiling = max_chunks_by_min_size.max(floor);
    let effective = requested.clamp(floor, ceiling);
    Some(effective)
}
