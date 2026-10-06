//! The export container — one deterministic zip inside one zstd stream.
//!
//! Authority: `docs/goal/behavior/mail-export.md` § Container shape (pinned
//! 2026-09-20) and § Compression wrapper. Entries are `Stored`; a single
//! level-9 zstd stream wraps the finished zip. What this module produces is
//! the `.zip.zst` the user ends up holding; the file that rests on nest disk
//! is its sealed form, `<session-id>.zip.zst.sealed` (§ Blob shape on disk —
//! the client frames and seals these bytes on the way up). One `zstd -d` on
//! the opened archive leaves the user a plain
//! `.zip` that Explorer and Finder open natively.
//!
//! Per-entry deflate is deliberately **not** used: it would compress every
//! message twice, and a single solid zstd window over the whole archive is the
//! ratio § Compression wrapper is buying.
//!
//! # Determinism
//!
//! Nothing here reads the clock, the hostname, a uid/gid or a random source.
//! Entry order is the caller's (the serializers emit the § Container shape
//! total order), each entry's timestamp is its own `mtime_epoch`, and the
//! permission bits are fixed constants. Timestamps outside the MS-DOS range
//! the zip format can represent clamp to its bounds rather than falling back to
//! "now" — [`clamp_to_dos`].
//!
//! # Streaming — built, and this module now delegates to it
//!
//! [`build_zip`] buffers, which is right for a test and for a caller that
//! already holds the run. The chunk-relay loop (§ Export pipeline) must not
//! buffer a 10 GiB export, and the established answer in this workspace is a
//! forward-only pseudo-`Seek` sink — `bins/fauna-nest/src/streaming.rs`'s
//! `ChannelWriter`, which is exactly how `GET /api/v1/export` streams its zip.
//! That shape landed 2026-09-21 as [`super::stream::ExportArchiveStream`], and
//! [`build_blob`] is now a wrapper over it rather than a second implementation
//! (its own doc comment says why the two could not both stand).

use std::io::{Cursor, Write};

use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, DateTime};

use super::{ExportEntry, ExportError};

/// § Compression wrapper: zstd level 9. A balance, not a tunable
/// (§ Compile-time decisions).
pub const ZSTD_LEVEL: i32 = 9;

/// Fixed permission bits. Real modes would vary by exporting machine, which the
/// determinism contract forbids.
const FILE_MODE: u32 = 0o644;
const DIR_MODE: u32 = 0o755;

/// The MS-DOS timestamp range a zip entry can represent.
const DOS_MIN_YEAR: i32 = 1980;
const DOS_MAX_YEAR: i32 = 2107;

/// Pack entries into the zip half of the container.
pub fn build_zip(entries: &[ExportEntry]) -> Result<Vec<u8>, ExportError> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for entry in entries {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .last_modified_time(clamp_to_dos(entry.mtime_epoch))
            .large_file(entry.bytes.len() > u32::MAX as usize - 1);
        if entry.is_dir {
            writer
                .add_directory(
                    entry.path.trim_end_matches('/'),
                    options.unix_permissions(DIR_MODE),
                )
                .map_err(|e| ExportError::Archive(e.to_string()))?;
            continue;
        }
        writer
            .start_file(&entry.path, options.unix_permissions(FILE_MODE))
            .map_err(|e| ExportError::Archive(e.to_string()))?;
        writer
            .write_all(&entry.bytes)
            .map_err(|e| ExportError::Archive(e.to_string()))?;
    }
    let cursor = writer
        .finish()
        .map_err(|e| ExportError::Archive(e.to_string()))?;
    Ok(cursor.into_inner())
}

/// The finished blob: the zip of [`build_zip`] inside one level-9 zstd stream.
/// This is the plaintext the client then seals under the per-session key
/// (§ Sealed-blob delivery) — sealing is the pipeline's job, not this module's.
///
/// **Delegates to [`super::stream`], deliberately.** A one-shot
/// `zstd::encode_all` over a finished zip is the obvious implementation and was
/// the original one, but it is not byte-identical to the streaming path the
/// drive loop actually uploads from: a one-shot compressor knows the total
/// input size and records it in the frame header, a streaming one does not. Two
/// implementations would mean the determinism contract (§ Container shape) held
/// of each path separately while the two disagreed with each other — so the
/// streaming writer is the single implementation and this is its whole-run
/// convenience form.
pub fn build_blob(entries: &[ExportEntry]) -> Result<Vec<u8>, ExportError> {
    super::stream::build_blob_streaming(entries)
}

/// Epoch seconds → a zip `DateTime`, clamped into the representable range.
///
/// A message older than 1980 or dated past 2107 is rare but real (a bad
/// `Date:` header, a clock-less device), and the alternative to clamping is
/// the `zip` crate's own fallback, which is the current time — an input the
/// determinism contract explicitly forbids.
pub(crate) fn clamp_to_dos(epoch: i64) -> DateTime {
    let (days, hour, minute, second) = fauna_core::caltime::epoch_secs_to_days_and_time(epoch);
    let (year, month, day) = fauna_core::caltime::civil_from_days(days);
    if year < DOS_MIN_YEAR {
        return dos_or_epoch(DOS_MIN_YEAR, 1, 1, 0, 0, 0);
    }
    if year > DOS_MAX_YEAR {
        return dos_or_epoch(DOS_MAX_YEAR, 12, 31, 23, 59, 58);
    }
    dos_or_epoch(year, month, day, hour, minute, second)
}

fn dos_or_epoch(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> DateTime {
    DateTime::from_date_and_time(
        year as u16,
        month as u8,
        day as u8,
        hour as u8,
        minute as u8,
        second as u8,
    )
    .unwrap_or_else(|_| {
        // Unreachable for a clamped, calendar-valid tuple; the fallback is the
        // floor of the representable range, never the wall clock.
        DateTime::from_date_and_time(DOS_MIN_YEAR as u16, 1, 1, 0, 0, 0)
            .expect("1980-01-01 is representable")
    })
}
