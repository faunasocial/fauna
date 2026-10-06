//! The zip directory and the member reader every parser drives.

use std::io::Read;

use crate::error::ArchiveError;
use crate::source::{ArchiveSource, SourceCursor};

/// The largest member `read_member` materializes — category JSON files are
/// megabytes; anything past this is not a JSON export member and is refused
/// rather than allocated. Media is hashed with `blake3_member`, never read
/// whole here.
///
/// This is a **per-member** bound and nothing more. It is enforced here, on the
/// decompressed stream, not delegated to the decoder: both readers cap the bytes
/// they will take at the member's declared size and fail a member that inflates
/// past it, so *one* member costs at most its declared size, whatever the
/// decoder would otherwise hand over.
///
/// ⚠ It says nothing about how many members a caller holds AT ONCE, and reading
/// it as though it did is what left every aggregation above it unbounded until
/// 2026-09-09 — the archive's own records choose how many members a caller will
/// materialize together. The aggregate bounds live with the callers that
/// accumulate, and are listed in `archive-import.md` § Parser contract:
/// `facebook::posts::MAX_RECORD_MEDIA_REFS` caps the refs one record may name,
/// and `fauna_archive_import_machine::run::MAX_RECORD_MEDIA_BYTES` caps the
/// bytes one record's media may occupy at once.
pub const MAX_MEMBER_BYTES: u64 = 256 * 1024 * 1024;

const HASH_BUF: usize = 64 * 1024;

/// One file inside the zip (directory entries are not listed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: String,
    pub size: u64,
    /// The member's index in the zip's central directory.
    pub index: usize,
}

/// The central directory: every file member, sorted by name.
#[derive(Debug, Clone, Default)]
pub struct ZipDirectory {
    entries: Vec<ZipEntry>,
}

impl ZipDirectory {
    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    pub fn get(&self, name: &str) -> Option<&ZipEntry> {
        self.entries
            .binary_search_by(|e| e.name.as_str().cmp(name))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Every entry whose name satisfies `pred`, in name order.
    pub fn find(&self, pred: impl Fn(&str) -> bool) -> Vec<&ZipEntry> {
        self.entries.iter().filter(|e| pred(&e.name)).collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// An opened archive: the directory plus the ability to read or hash one
/// member at a time.
pub struct ArchiveReader<'s> {
    zip: zip::ZipArchive<SourceCursor<'s>>,
    dir: ZipDirectory,
}

impl<'s> ArchiveReader<'s> {
    /// Reads the central directory (from the end of the source) and nothing
    /// else.
    pub fn open(source: &'s dyn ArchiveSource) -> Result<Self, ArchiveError> {
        let mut zip = zip::ZipArchive::new(SourceCursor::new(source))
            .map_err(|e| ArchiveError::Zip(e.to_string()))?;
        let mut entries = Vec::with_capacity(zip.len());
        for index in 0..zip.len() {
            // `by_index_raw` exposes the header without preparing decompression.
            let file = zip
                .by_index_raw(index)
                .map_err(|e| ArchiveError::Zip(format!("member {index}: {e}")))?;
            if file.is_dir() || file.name().ends_with('/') {
                continue;
            }
            entries.push(ZipEntry {
                name: file.name().to_string(),
                size: file.size(),
                index,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(ArchiveReader {
            zip,
            dir: ZipDirectory { entries },
        })
    }

    pub fn directory(&self) -> &ZipDirectory {
        &self.dir
    }

    fn entry(&self, name: &str) -> Result<ZipEntry, ArchiveError> {
        self.dir
            .get(name)
            .cloned()
            .ok_or_else(|| ArchiveError::Member {
                member: name.to_string(),
                reason: "not in the archive".to_string(),
            })
    }

    /// A member's declared size — the most [`Self::read_member`] ever returns
    /// for it, since a member that inflates past it is an error. A caller that
    /// budgets several members checks this BEFORE reading the next one, so the
    /// member that would cross its budget is never materialized at all.
    pub fn member_size(&self, name: &str) -> Result<u64, ArchiveError> {
        self.entry(name).map(|e| e.size)
    }

    /// Reads one whole member (a category JSON file). Refuses members over
    /// [`MAX_MEMBER_BYTES`], and reads at most the member's declared size —
    /// a member that inflates past it is an error, not an allocation.
    pub fn read_member(&mut self, name: &str) -> Result<Vec<u8>, ArchiveError> {
        let entry = self.entry(name)?;
        if entry.size > MAX_MEMBER_BYTES {
            return Err(ArchiveError::Member {
                member: name.to_string(),
                reason: format!(
                    "{} bytes exceeds the {MAX_MEMBER_BYTES}-byte member cap",
                    entry.size
                ),
            });
        }
        let file = self
            .zip
            .by_index(entry.index)
            .map_err(|e| ArchiveError::Member {
                member: name.to_string(),
                reason: e.to_string(),
            })?;
        // Bounded to EXACTLY `entry.size` — never `+1` — so the pre-sized
        // buffer below never has to grow: `read_to_end` cannot return more
        // than `entry.size` bytes, so it cannot outgrow a `Vec` already
        // reserved for that many (the growth the old `+1` shape invited,
        // since std's `read_to_end` probes a full buffer for more data by
        // reserving further before finding EOF — see this row's own
        // measurement). "One member costs at most its declared size" is now
        // true of the allocation, not just of the returned `Vec`'s length.
        let mut bytes = Vec::with_capacity(entry.size as usize);
        let mut bounded = file.take(entry.size);
        bounded
            .read_to_end(&mut bytes)
            .map_err(|e| ArchiveError::Member {
                member: name.to_string(),
                reason: e.to_string(),
            })?;
        // The one byte `take(entry.size)` refused to ask for: `Ok(0)` means
        // the stream ended exactly at (or before) the declared size, the
        // honest case; `Ok(_)` means a real byte was waiting past it, i.e.
        // the member inflates past its declared size; `Err(e)` is the
        // decoder's own end-of-stream check (a CRC mismatch, a truncated
        // deflate stream) finally surfacing here rather than mid-read, and it
        // must reach the caller exactly like any other member error, never be
        // swallowed as a clean EOF.
        let mut probe = [0u8; 1];
        match bounded.into_inner().read(&mut probe) {
            Ok(0) => Ok(bytes),
            Ok(_) => Err(ArchiveError::Member {
                member: name.to_string(),
                reason: format!("inflates past its declared {} bytes", entry.size),
            }),
            Err(e) => Err(ArchiveError::Member {
                member: name.to_string(),
                reason: e.to_string(),
            }),
        }
    }

    /// Streams one member through BLAKE3 in 64 KiB pieces; returns the hex
    /// digest and the byte count actually read. Bounded by the member's
    /// declared size the same way [`Self::read_member`] is.
    pub fn blake3_member(&mut self, name: &str) -> Result<(String, u64), ArchiveError> {
        let entry = self.entry(name)?;
        let file = self
            .zip
            .by_index(entry.index)
            .map_err(|e| ArchiveError::Member {
                member: name.to_string(),
                reason: e.to_string(),
            })?;
        let mut file = file.take(entry.size.saturating_add(1));
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; HASH_BUF];
        let mut total = 0u64;
        loop {
            let n = file.read(&mut buf).map_err(|e| ArchiveError::Member {
                member: name.to_string(),
                reason: e.to_string(),
            })?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        if total > entry.size {
            return Err(ArchiveError::Member {
                member: name.to_string(),
                reason: format!("inflates past its declared {} bytes", entry.size),
            });
        }
        Ok((hasher.finalize().to_hex().to_string(), total))
    }
}
