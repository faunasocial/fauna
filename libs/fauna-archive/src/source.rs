//! `ArchiveSource` — the seam a parser reads the zip through: a length and
//! positional reads, nothing else. Backed by a `Vec<u8>` in tests, a
//! `std::fs::File` on native, and (slice 3) a browser `File` on web. The
//! `SourceCursor` adapter turns any source into the `Read + Seek` the `zip`
//! crate wants, so the central directory is read from the end of the file
//! and each member from its own offset — never the whole archive at once.

use std::io::{self, Read, Seek, SeekFrom};

/// A random-access byte source. Implementations are cheap to share by
/// reference; `read_at` takes `&self` so a source can back several cursors.
pub trait ArchiveSource {
    /// Total length in bytes.
    fn len(&self) -> u64;
    /// Reads up to `buf.len()` bytes starting at `offset`; returns how many
    /// were read (0 at or past the end).
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An in-memory source — tests and small archives.
pub struct VecSource(pub Vec<u8>);

impl ArchiveSource for VecSource {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        if start >= self.0.len() {
            return Ok(0);
        }
        let n = buf.len().min(self.0.len() - start);
        buf[..n].copy_from_slice(&self.0[start..start + n]);
        Ok(n)
    }
}

/// A file on disk, read positionally (no shared seek position, so the
/// source can be shared).
#[cfg(not(target_arch = "wasm32"))]
pub struct FileSource {
    file: std::fs::File,
    len: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileSource {
    pub fn open(path: impl AsRef<std::path::Path>) -> io::Result<FileSource> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        Ok(FileSource { file, len })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ArchiveSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    #[cfg(unix)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        use std::os::unix::fs::FileExt;
        self.file.read_at(buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        use std::os::windows::fs::FileExt;
        self.file.seek_read(buf, offset)
    }

    #[cfg(not(any(unix, windows)))]
    fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other(
            "positional file reads are unsupported on this target",
        ))
    }
}

/// `Read + Seek` over a borrowed source — what `zip::ZipArchive` consumes.
pub struct SourceCursor<'s> {
    source: &'s dyn ArchiveSource,
    pos: u64,
}

impl<'s> SourceCursor<'s> {
    pub fn new(source: &'s dyn ArchiveSource) -> Self {
        SourceCursor { source, pos: 0 }
    }
}

impl Read for SourceCursor<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.source.len() {
            return Ok(0);
        }
        let n = self.source.read_at(self.pos, buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for SourceCursor<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let (base, delta) = match from {
            SeekFrom::Start(p) => (p, 0i64),
            SeekFrom::End(d) => (self.source.len(), d),
            SeekFrom::Current(d) => (self.pos, d),
        };
        let target = if delta >= 0 {
            base.checked_add(delta as u64)
        } else {
            base.checked_sub(delta.unsigned_abs())
        };
        match target {
            Some(p) => {
                self.pos = p;
                Ok(p)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the archive",
            )),
        }
    }
}
