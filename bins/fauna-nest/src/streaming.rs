//! Shared streaming utilities: ChannelWriter and HTTP body helpers.
//!
//! ChannelWriter adapts std::io::Write + Seek to a tokio::sync::mpsc channel,
//! enabling synchronous zip/tar writers to stream data as async HTTP responses.

use std::io::{Read, Seek, SeekFrom, Write};

use axum::body::Body;
use bytes::Bytes;
use tokio_stream::wrappers::ReceiverStream;

pub const FLUSH_THRESHOLD: usize = 64 * 1024;

/// Adapter: std::io::Write (+ Seek) → tokio::sync::mpsc::Sender<Result<Bytes>>.
/// Uses blocking_send() — safe from spawn_blocking.
///
/// Two drain modes, because the two kinds of archive writer have opposite needs:
///
/// - **Sequential sinks** (tar — [`ChannelWriter::new`]): never seek backward,
///   so buffered bytes are safe to send the moment they accumulate. Drains
///   eagerly at the frontier every `FLUSH_THRESHOLD` bytes → constant memory.
///
/// - **Seek-back sinks** (zip — [`ChannelWriter::seekable`]): the `zip` crate
///   patches each file's local header (CRC32, sizes) **after** writing that
///   file's body — it seeks back to `header_start`, rewrites the 12-byte
///   CRC/size field, then seeks forward to the file end. So the header bytes of
///   the file currently being written must stay buffered until that patch
///   lands. This mode therefore drains **only on `flush()`/`Drop`**, never
///   mid-write, and the zip writer must be driven with
///   `set_flush_on_finish_file(true)` so `flush()` fires once per file — right
///   after `finish_file` patches the header and seeks to the file end (so
///   `pos == high_water` and the whole file, header included, drains together).
///   Peak buffer is one file's bytes; constant memory would require a
///   data-descriptor (non-seeking) zip writer, which the 2.x `zip` crate does
///   not emit. Using the eager (sequential) mode for a zip writer advances
///   `base_offset` past the header of any file larger than `FLUSH_THRESHOLD`, so
///   the post-body header patch hits "seek into already-drained region" and
///   corrupts the archive — the bug that broke restore for every file over 64 KB.
pub struct ChannelWriter {
    pub tx: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    /// Buffered bytes not yet sent. Index 0 corresponds to `base_offset`.
    pub buf: Vec<u8>,
    /// Absolute byte offset of `buf[0]`.
    pub base_offset: u64,
    /// Current write/seek position as absolute offset.
    pub pos: u64,
    /// Furthest position ever reached — data before this is safe to drain
    /// only when pos == high_water (i.e., we are not mid-patch).
    pub high_water: u64,
    /// When true (seek-back/zip mode), do NOT drain during `write`; wait for
    /// `flush()`/`Drop` so a backward header patch can still reach earlier bytes.
    /// When false (sequential/tar mode), drain eagerly at the frontier.
    pub defer_drain: bool,
}

impl ChannelWriter {
    /// Sequential sink (tar, etc.): drains eagerly at `FLUSH_THRESHOLD` for
    /// constant memory. Must NOT be used with a writer that seeks backward —
    /// see [`ChannelWriter::seekable`].
    pub fn new(tx: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>) -> Self {
        Self {
            tx,
            buf: Vec::new(),
            base_offset: 0,
            pos: 0,
            high_water: 0,
            defer_drain: false,
        }
    }

    /// Seek-back sink (zip): defers draining to `flush()`/`Drop` so a file's
    /// local header survives until its post-body CRC/size patch. Pair with
    /// `ZipWriter::set_flush_on_finish_file(true)` so each file drains as it is
    /// finished (peak memory = one file's bytes).
    pub fn seekable(tx: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>) -> Self {
        Self {
            tx,
            buf: Vec::new(),
            base_offset: 0,
            pos: 0,
            high_water: 0,
            defer_drain: true,
        }
    }

    /// Drain and send bytes up to `up_to` absolute offset.
    pub fn drain_up_to(&mut self, up_to: u64) -> std::io::Result<()> {
        if up_to <= self.base_offset {
            return Ok(());
        }
        let drain_end = (up_to - self.base_offset) as usize;
        let drain_end = drain_end.min(self.buf.len());
        if drain_end == 0 {
            return Ok(());
        }
        // Send in FLUSH_THRESHOLD chunks to avoid one giant allocation.
        let mut sent = 0usize;
        while sent < drain_end {
            let end = (sent + FLUSH_THRESHOLD).min(drain_end);
            let chunk = Bytes::copy_from_slice(&self.buf[sent..end]);
            self.tx.blocking_send(Ok(chunk)).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver dropped")
            })?;
            sent = end;
        }
        self.buf.drain(..drain_end);
        self.base_offset += drain_end as u64;
        Ok(())
    }

    /// Drain everything committed so far (safe when pos == high_water).
    pub fn try_drain(&mut self) -> std::io::Result<()> {
        if self.pos == self.high_water {
            self.drain_up_to(self.high_water)?;
        }
        Ok(())
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let rel = (self.pos - self.base_offset) as usize;
        let end = rel + data.len();
        if end > self.buf.len() {
            self.buf.resize(end, 0);
        }
        self.buf[rel..end].copy_from_slice(data);
        self.pos += data.len() as u64;
        if self.pos > self.high_water {
            self.high_water = self.pos;
        }

        // Sequential (eager) mode only: drain at the frontier once enough has
        // accumulated. In seek-back (deferred) mode we must NOT drain here — the
        // zip writer still seeks back to the current file's header (before this
        // data) to patch its CRC/size once the body is done, and draining would
        // advance `base_offset` past that header ("seek into already-drained
        // region"). Deferred mode drains on `flush()` (per-file via
        // flush_on_finish_file) and on `Drop` (trailing central directory).
        if !self.defer_drain
            && self.pos == self.high_water
            && self.high_water - self.base_offset >= FLUSH_THRESHOLD as u64
        {
            self.try_drain()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.try_drain()
    }
}

impl Read for ChannelWriter {
    /// `ChannelWriter` is a write-only streaming sink. The `zip` crate requires
    /// `Read` only to satisfy the trait bound on `set_flush_on_finish_file`
    /// (which never reads); the read-back paths (`deep_copy_file`, `new_append`)
    /// are never used on a restore writer. Fail loudly if one ever is, rather
    /// than silently returning EOF and corrupting the archive.
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "ChannelWriter is write-only (streaming sink)",
        ))
    }
}

impl Seek for ChannelWriter {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let total_len = self.base_offset + self.buf.len() as u64;
        let new_pos = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::End(n) => {
                if n < 0 && (-n) as u64 > total_len {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "seek before start",
                    ));
                }
                (total_len as i64 + n) as u64
            }
            SeekFrom::Current(n) => {
                if n < 0 && (-n) as u64 > self.pos {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "seek before start",
                    ));
                }
                (self.pos as i64 + n) as u64
            }
        };

        // Guard: cannot seek before already-drained data.
        if new_pos < self.base_offset {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek into already-drained region",
            ));
        }

        // If seeking forward past our buffer, expand it.
        let rel = (new_pos - self.base_offset) as usize;
        if rel > self.buf.len() {
            self.buf.resize(rel, 0);
        }

        self.pos = new_pos;
        Ok(self.pos)
    }
}

impl Drop for ChannelWriter {
    fn drop(&mut self) {
        // Drain everything remaining.
        let end = self.base_offset + self.buf.len() as u64;
        let _ = self.drain_up_to(end);
    }
}

/// Create a streaming HTTP body backed by an mpsc channel.
/// Returns (sender for ChannelWriter, Body for axum response).
pub fn streaming_body(
    buffer: usize,
) -> (
    tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    Body,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(buffer);
    let stream = ReceiverStream::new(rx);
    let body = Body::from_stream(stream);
    (tx, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn channel_writer_write_and_drain() {
        // ChannelWriter uses blocking_send, so it must run from spawn_blocking.
        let (tx, mut rx) = mpsc::channel(16);

        tokio::task::spawn_blocking(move || {
            let mut writer = ChannelWriter::new(tx);

            // Write less than FLUSH_THRESHOLD — nothing sent yet.
            let small = vec![0xABu8; 100];
            writer.write_all(&small).unwrap();

            // Flush explicitly triggers drain.
            writer.flush().unwrap();
        })
        .await
        .unwrap();

        let chunk = rx.recv().await.unwrap().unwrap();
        assert_eq!(chunk.len(), 100);
    }

    #[test]
    fn channel_writer_buffers_until_flush() {
        // The zip writer seeks back into a file's local header to patch CRC/size
        // *after* writing its body, so ChannelWriter must NOT drain mid-write —
        // everything stays buffered until flush()/Drop. (Pre-fix it auto-drained
        // at FLUSH_THRESHOLD, which made the header patch of any file > 64 KB
        // seek into already-drained bytes and corrupt the archive.) No drain
        // fires here, so no blocking_send — safe outside a runtime; Drop drains
        // into the still-live `_rx` at end of scope.
        let (tx, _rx) = mpsc::channel(16);
        let mut writer = ChannelWriter::seekable(tx);
        writer
            .write_all(&vec![0xCDu8; 2 * FLUSH_THRESHOLD])
            .unwrap();
        assert_eq!(writer.buf.len(), 2 * FLUSH_THRESHOLD, "no mid-write drain");
        assert_eq!(writer.base_offset, 0, "nothing drained before flush");

        // The header patch: seek back into the buffered region and overwrite —
        // must succeed now that the region is never drained out from under it.
        writer.seek(SeekFrom::Start(8)).unwrap();
        writer.write_all(&[0xEEu8; 4]).unwrap();
        assert_eq!(writer.base_offset, 0);
    }

    #[test]
    fn channel_writer_sequential_drains_at_threshold() {
        // Sequential (tar) mode keeps constant memory: writing FLUSH_THRESHOLD
        // bytes drains eagerly during the write — no flush needed. `_rx` stays
        // live so blocking_send buffers into the channel. (Distinct from the
        // seek-back mode above, which must NOT drain mid-write.)
        let (tx, _rx) = mpsc::channel(16);
        let mut writer = ChannelWriter::new(tx); // sequential / eager
        writer.write_all(&vec![0u8; FLUSH_THRESHOLD]).unwrap();
        assert!(writer.buf.is_empty(), "eager drain empties the buffer");
        assert_eq!(writer.base_offset, FLUSH_THRESHOLD as u64);
    }

    #[tokio::test]
    async fn streaming_body_returns_sender_and_body() {
        // Verify that streaming_body doesn't panic and that the sender works.
        let (tx, _body) = streaming_body(4);
        tx.send(Ok(Bytes::from_static(b"hello"))).await.unwrap();
        // Body is consumed by axum; just verify construction is sound.
    }
}
