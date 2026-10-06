//! The container, written as a stream — the shape the chunk-relay loop uploads
//! from.
//!
//! Authority: `docs/goal/behavior/mail-export.md` § Container shape (one zip
//! inside **one** level-9 zstd stream, and an uploaded chunk is a byte SLICE of
//! that single stream, never an independently-compressed unit) and § Export
//! pipeline (the client converts and uploads as it goes).
//!
//! [`archive::build_blob`](super::archive::build_blob) buffers the whole run,
//! which is right for a test and for a caller that already holds every entry.
//! The drive loop cannot: § Quota composition caps an export at **10 GiB**, and
//! a client that buffered one before uploading a byte would do it on a phone as
//! readily as a desktop — the same argument that retired the single-pass seal
//! (§ Blob shape on disk). So this module is the streaming half, and
//! `build_blob` is now a thin wrapper over it rather than a second
//! implementation: two ways to build the container is exactly the divergence
//! the determinism contract cannot survive (priority #1/#3).
//!
//! # Why the sink buffers at all, and how much
//!
//! The `zip` writer patches each file's local header **after** writing that
//! file's body — it seeks back to the header, rewrites the CRC/size field, then
//! seeks forward again. So the bytes of the file currently being written must
//! stay reachable until that patch lands. This is the same constraint
//! `bins/fauna-nest/src/streaming.rs`'s `ChannelWriter` documents for the
//! `GET /api/v1/export` zip, and the answer is the same one: a forward-only
//! pseudo-`Seek` sink that drains **only on `flush()`**, driven with
//! `set_flush_on_finish_file(true)` so a flush fires once per finished file —
//! right after the patch, when the write position is back at the frontier.
//!
//! **Peak memory is therefore one message, not one export.** Draining eagerly
//! at a byte threshold instead would advance the drained frontier past the
//! header of any file larger than that threshold, and the post-body patch would
//! then seek into already-drained bytes — the bug that broke restore for every
//! file over 64 KB (`ChannelWriter`'s own doc comment records it). Not repeated
//! here.
//!
//! # What a chunk is
//!
//! Drained zip bytes go straight into one long-lived zstd encoder, and its
//! output accumulates in a tail buffer the caller pulls fixed-size slices off.
//! Those slices are the chunks — cut at an arbitrary byte boundary of the one
//! compressed stream, which is precisely what § Container shape pins and what
//! makes the nest's "append opaque bytes, parse nothing" contract sound.

use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

use super::archive::{ZSTD_LEVEL, clamp_to_dos};
use super::{ExportEntry, ExportError};

/// The plaintext slice size one uploaded chunk carries — § Export pipeline's
/// "16 MiB chunks".
///
/// ⚠ **A sealed frame is larger than this.** The frame on disk is
/// `len u32 BE || 24-byte nonce || ciphertext`, and the ciphertext carries the
/// 16-byte Poly1305 tag — so a full chunk's *declared* length is
/// `EXPORT_CHUNK_BYTES + 16`. Any maximum-frame-length bound has to be at least that, or a conforming writer's own full chunk
/// would trip the reader's bound.
pub const EXPORT_CHUNK_BYTES: usize = 16 * 1024 * 1024;

/// Fixed permission bits — real modes vary by exporting machine, which the
/// determinism contract forbids. Mirrors [`super::archive`]'s constants; kept
/// beside the writer that applies them.
const FILE_MODE: u32 = 0o644;
const DIR_MODE: u32 = 0o755;

/// The compressed bytes produced but not yet handed to the caller.
///
/// Shared (rather than reached through the writer stack) because the `zip`
/// writer owns its sink, the sink owns the encoder and the encoder owns this —
/// three layers of inner-accessor to reach a `Vec`. An `Arc<Mutex<_>>` keeps
/// the whole stack `Send`, which the native drive loop's `async_trait` future
/// needs and the wasm one does not mind.
#[derive(Clone, Default)]
struct ChunkTail(Arc<Mutex<Vec<u8>>>);

impl Write for ChunkTail {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("chunk tail mutex")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The forward-only pseudo-`Seek` sink the zip writer writes through.
///
/// Absolute offsets are tracked so the zip writer's backward header patch lands
/// in `buf`; everything at or before `base_offset` has already gone into the
/// encoder and is unreachable. `flush` is the only drain point (see the module
/// docs).
struct ZipSink {
    /// Bytes not yet handed to the encoder. Index 0 is absolute `base_offset`.
    buf: Vec<u8>,
    /// Absolute offset of `buf[0]`.
    base_offset: u64,
    /// Current write/seek position, absolute.
    pos: u64,
    /// Furthest position ever reached. Draining is safe only at the frontier
    /// (`pos == high_water`), i.e. when we are not mid-patch.
    high_water: u64,
    encoder: zstd::Encoder<'static, ChunkTail>,
}

impl ZipSink {
    fn new(tail: ChunkTail) -> Result<Self, ExportError> {
        let encoder = zstd::Encoder::new(tail, ZSTD_LEVEL)
            .map_err(|e| ExportError::Compress(e.to_string()))?;
        Ok(Self {
            buf: Vec::new(),
            base_offset: 0,
            pos: 0,
            high_water: 0,
            encoder,
        })
    }
}

impl Write for ZipSink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let at = (self.pos - self.base_offset) as usize;
        if at + data.len() > self.buf.len() {
            self.buf.resize(at + data.len(), 0);
        }
        self.buf[at..at + data.len()].copy_from_slice(data);
        self.pos += data.len() as u64;
        self.high_water = self.high_water.max(self.pos);
        Ok(data.len())
    }

    /// The one drain point. Only at the frontier: a flush while the zip writer
    /// sits mid-patch would drain bytes it is about to rewrite.
    fn flush(&mut self) -> std::io::Result<()> {
        if self.pos != self.high_water {
            return Ok(());
        }
        if !self.buf.is_empty() {
            self.encoder.write_all(&self.buf)?;
            self.base_offset += self.buf.len() as u64;
            self.buf.clear();
        }
        Ok(())
    }
}

/// The `zip` writer's seekable mode reads back what it has written (it rereads
/// a local header before patching it), so the sink has to serve the buffered
/// region as well as accept writes. Anything at or before `base_offset` has
/// gone to the encoder and is genuinely unreadable — an error, never a silent
/// zero fill, because a header patch that read zeros would corrupt the archive
/// exactly as quietly as a bad seek would.
impl Read for ZipSink {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos < self.base_offset {
            return Err(std::io::Error::other(format!(
                "export zip read at {} is behind the drained frontier {}",
                self.pos, self.base_offset
            )));
        }
        let at = (self.pos - self.base_offset) as usize;
        let available = self.buf.len().saturating_sub(at);
        let n = available.min(out.len());
        out[..n].copy_from_slice(&self.buf[at..at + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for ZipSink {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let want = match to {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(d) => self.pos.saturating_add_signed(d),
            SeekFrom::End(d) => self.high_water.saturating_add_signed(d),
        };
        if want < self.base_offset {
            return Err(std::io::Error::other(format!(
                "export zip seek to {want} is behind the drained frontier {}",
                self.base_offset
            )));
        }
        self.pos = want;
        Ok(want)
    }

    fn stream_position(&mut self) -> std::io::Result<u64> {
        Ok(self.pos)
    }
}

/// Builds the container incrementally and hands out uploadable chunks.
///
/// One instance per export session. Entries go in in the § Container shape
/// total order (the caller's [`ExportSerializer`](super::ExportSerializer)
/// already enforces it); chunks come out as the compressed stream fills.
pub struct ExportArchiveStream {
    zip: ZipWriter<ZipSink>,
    tail: ChunkTail,
    chunk_bytes: usize,
}

impl ExportArchiveStream {
    /// `chunk_bytes` is the plaintext slice size for a full chunk;
    /// [`EXPORT_CHUNK_BYTES`] is the pipeline's figure.
    pub fn new(chunk_bytes: usize) -> Result<Self, ExportError> {
        assert!(chunk_bytes > 0, "chunk size must be positive");
        let tail = ChunkTail::default();
        let mut zip = ZipWriter::new(ZipSink::new(tail.clone())?);
        // Pairs with `ZipSink::flush` being the only drain point: one flush per
        // finished file, fired right after the header patch (module docs).
        zip.set_flush_on_finish_file(true);
        Ok(Self {
            zip,
            tail,
            chunk_bytes,
        })
    }

    /// Append one entry. Order is the caller's responsibility — the serializer
    /// that produces these already refuses an out-of-order message.
    pub fn push_entry(&mut self, entry: &ExportEntry) -> Result<(), ExportError> {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .last_modified_time(clamp_to_dos(entry.mtime_epoch))
            .large_file(entry.bytes.len() > u32::MAX as usize - 1);
        if entry.is_dir {
            return self
                .zip
                .add_directory(
                    entry.path.trim_end_matches('/'),
                    options.unix_permissions(DIR_MODE),
                )
                .map_err(|e| ExportError::Archive(e.to_string()));
        }
        self.zip
            .start_file(&entry.path, options.unix_permissions(FILE_MODE))
            .map_err(|e| ExportError::Archive(e.to_string()))?;
        self.zip
            .write_all(&entry.bytes)
            .map_err(|e| ExportError::Archive(e.to_string()))
    }

    /// Every **full** chunk available now. The caller seals and uploads each
    /// one; a short remainder stays buffered for [`Self::finish`].
    pub fn take_full_chunks(&mut self) -> Vec<Vec<u8>> {
        let mut tail = self.tail.0.lock().expect("chunk tail mutex");
        let mut out = Vec::new();
        while tail.len() >= self.chunk_bytes {
            out.push(tail.drain(..self.chunk_bytes).collect());
        }
        out
    }

    /// Close the zip's central directory and the zstd stream, returning every
    /// remaining chunk. The last one is short unless the stream happened to end
    /// on a boundary.
    ///
    /// **No chunk is ever empty.** A zero-length chunk would be sealed as the
    /// blob's *terminator* ([`super::seal::ExportBlobSealer::finish`] owns
    /// that frame, and `seal_chunk` refuses an empty one), so handing a caller
    /// an empty remainder would put a second terminator in the middle of the
    /// blob. In practice zstd always emits at least a frame header, so the
    /// guard is belt-and-braces rather than a live path.
    pub fn finish(self) -> Result<Vec<Vec<u8>>, ExportError> {
        let Self {
            zip,
            tail,
            chunk_bytes,
        } = self;
        // `finish` writes the central directory through the sink; the sink's
        // own flush then hands those bytes to the encoder.
        let mut sink = zip
            .finish()
            .map_err(|e| ExportError::Archive(e.to_string()))?;
        sink.flush()
            .map_err(|e| ExportError::Archive(e.to_string()))?;
        sink.encoder
            .finish()
            .map_err(|e| ExportError::Compress(e.to_string()))?;

        let mut buf = tail.0.lock().expect("chunk tail mutex");
        let mut out = Vec::new();
        while buf.len() > chunk_bytes {
            out.push(buf.drain(..chunk_bytes).collect());
        }
        let remainder = std::mem::take(&mut *buf);
        if !remainder.is_empty() {
            out.push(remainder);
        }
        Ok(out)
    }
}

/// Every entry through the streaming writer, concatenated — the whole
/// container in memory.
///
/// This is what [`build_blob`](super::build_blob) is: the streaming path is the
/// only implementation, so a caller that already holds the run gets exactly the
/// bytes the drive loop would have uploaded, and the determinism contract has
/// one thing to be true of rather than two.
pub(crate) fn build_blob_streaming(entries: &[ExportEntry]) -> Result<Vec<u8>, ExportError> {
    let mut stream = ExportArchiveStream::new(EXPORT_CHUNK_BYTES)?;
    for entry in entries {
        stream.push_entry(entry)?;
    }
    Ok(stream.finish()?.concat())
}
