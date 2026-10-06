//! `ArchiveReader::read_member`'s per-member bound (`archive-import.md` §
//! Parser contract rule 9; `reader.rs::MAX_MEMBER_BYTES`'s own doc comment):
//! "one member costs at most its declared size". This probe builds a member
//! whose zip metadata LIES about its own uncompressed size — patched at the
//! byte level, in both the local file header and the central directory, per
//! the public ZIP format (PKWARE APPNOTE.TXT), deliberately never by reading
//! the third-party `zip` crate's own source — and counts the heap
//! `read_member` actually holds while reading it.
//!
//! Its own test binary for the same reason `thread_aggregate_bound.rs` is: a
//! counting global allocator, one `#[test]` per binary so no sibling test's
//! allocations land in the count.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Cursor, Write};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use fauna_archive::reader::ArchiveReader;
use fauna_archive::source::VecSource;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let now = LIVE.fetch_add(by, Relaxed) + by;
    PEAK.fetch_max(now, Relaxed);
}

// SAFETY: every call forwards to `System` unchanged; the counters only observe.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            if new_size >= layout.size() {
                grew(new_size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Relaxed);
            }
        }
        moved
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// The most bytes live at once while `f` runs, above what was live before it.
fn peak_during(f: impl FnOnce()) -> usize {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    f();
    PEAK.load(Relaxed) - base
}

/// Overwrite the little-endian u32 at `offset` with `value`.
fn patch_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// The byte offset of the local file header's `uncompressed size` field,
/// relative to the header's own `PK\x03\x04` signature — fixed by the public
/// ZIP format (PKWARE APPNOTE.TXT § 4.3.7), not a `zip`-crate implementation
/// detail.
const LOCAL_UNCOMPRESSED_SIZE_OFFSET: usize = 22;
const LOCAL_CRC32_OFFSET: usize = 14;

/// The byte offset of the central directory file header's `uncompressed
/// size` field, relative to the header's own `PK\x01\x02` signature — same
/// spec authority as above (APPNOTE.TXT § 4.3.12).
const CENTRAL_UNCOMPRESSED_SIZE_OFFSET: usize = 24;
const CENTRAL_CRC32_OFFSET: usize = 16;

const LOCAL_SIG: [u8; 4] = *b"PK\x03\x04";
const CENTRAL_SIG: [u8; 4] = *b"PK\x01\x02";
const EOCD_SIG: [u8; 4] = *b"PK\x05\x06";

/// One single-member, Deflated zip carrying `content`, then re-stamped so
/// BOTH the local header and the central directory's `uncompressed size`
/// field read `declared_size` instead of `content.len()` — the "lying
/// member" a malicious or corrupt export would carry. The local header is
/// found at byte 0 (the archive's first and only member); the central
/// directory is found via the EOCD's own "offset of start of central
/// directory" field, never by scanning compressed bytes for a signature that
/// could coincidentally recur inside them.
fn zip_with_lied_size(name: &str, content: &[u8], declared_size: u32) -> VecSource {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    w.start_file(name, deflated).unwrap();
    w.write_all(content).unwrap();
    let mut bytes = w.finish().unwrap().into_inner();

    assert_eq!(
        &bytes[0..4],
        &LOCAL_SIG,
        "member is the archive's first local header"
    );
    patch_u32(&mut bytes, LOCAL_UNCOMPRESSED_SIZE_OFFSET, declared_size);

    let eocd_at = bytes
        .windows(4)
        .rposition(|w| w == EOCD_SIG)
        .expect("a freshly-written single-member zip carries one EOCD record");
    let cd_offset =
        u32::from_le_bytes(bytes[eocd_at + 16..eocd_at + 20].try_into().unwrap()) as usize;
    assert_eq!(
        &bytes[cd_offset..cd_offset + 4],
        &CENTRAL_SIG,
        "the EOCD's own offset field names the central directory's start"
    );
    patch_u32(
        &mut bytes,
        cd_offset + CENTRAL_UNCOMPRESSED_SIZE_OFFSET,
        declared_size,
    );

    VecSource(bytes)
}

/// [`zip_with_lied_size`], additionally corrupting the CRC-32 the decoder
/// checks against — for provoking a genuine decode error on the byte AFTER
/// the declared size (correction 3: that error must surface as
/// [`fauna_archive::error::ArchiveError::Member`], never be swallowed).
fn zip_with_bad_crc(name: &str, content: &[u8]) -> VecSource {
    let declared_size = u32::try_from(content.len()).unwrap();
    let VecSource(mut bytes) = zip_with_lied_size(name, content, declared_size);
    let cd_offset = {
        let eocd_at = bytes.windows(4).rposition(|w| w == EOCD_SIG).unwrap();
        u32::from_le_bytes(bytes[eocd_at + 16..eocd_at + 20].try_into().unwrap()) as usize
    };
    // Flip every bit of both CRC fields — any value but the true CRC refuses.
    for off in [LOCAL_CRC32_OFFSET, cd_offset + CENTRAL_CRC32_OFFSET] {
        for b in &mut bytes[off..off + 4] {
            *b = !*b;
        }
    }
    VecSource(bytes)
}

/// A member big enough that "peaked at 2x declared" and "peaked at declared
/// plus a small constant" are unmistakably different numbers (correction 2)
/// — too small a fixture leaves both readings inside the same tolerance band.
const DECLARED_SIZE: usize = 1024 * 1024;
const ACTUAL_SIZE: usize = 2 * 1024 * 1024;
/// Generous slack for the allocator's own bookkeeping and the read buffer —
/// nowhere near enough to hide a doubling of a 1 MiB declared size.
const TOLERANCE: usize = 64 * 1024;

#[test]
fn read_member_never_holds_more_than_the_declared_size_even_when_it_lies() {
    // Compressible content (a repeated byte) — declared_size still forces a
    // real Deflate decode; the ratio is not the point, the POST-declared
    // byte's existence is.
    let content = vec![b'x'; ACTUAL_SIZE];
    let src = zip_with_lied_size("category.json", &content, DECLARED_SIZE as u32);
    let mut reader = ArchiveReader::open(&src).unwrap();

    let mut result = None;
    let peak = peak_during(|| {
        result = Some(reader.read_member("category.json"));
    });

    let err = result
        .unwrap()
        .expect_err("a member that inflates past its declared size refuses");
    let msg = format!("{err}");
    assert!(
        msg.contains("inflates past its declared"),
        "wrong refusal reason: {msg}"
    );
    assert!(
        peak <= DECLARED_SIZE + TOLERANCE,
        "read_member peaked at {peak} B reading a member that declares {DECLARED_SIZE} B and \
         actually decodes to {ACTUAL_SIZE} B — MAX_MEMBER_BYTES's own doc comment says one \
         member costs AT MOST its declared size, not ~2x it"
    );
}

#[test]
fn read_member_refuses_a_well_formed_member_whose_crc_is_wrong() {
    // NOT a lying declared size — the byte AFTER the declared size probe is
    // what must catch this: the compressed stream's own end-of-stream CRC
    // check moves to that probe read once the primary read is bounded to
    // EXACTLY the declared size (the fix this row's parent row calls for).
    let content = vec![b'y'; DECLARED_SIZE];
    let src = zip_with_bad_crc("category.json", &content);
    let mut reader = ArchiveReader::open(&src).unwrap();

    let result = reader.read_member("category.json");
    let err = result.expect_err("a CRC-corrupt member must not be handed to a caller as valid");
    // Whatever the exact reason (zip's own CRC-mismatch message, or the
    // decoder's own end-of-stream I/O error), it must be a proper member
    // refusal on the SAME member name — never a panic, and never silently
    // treated as a clean, undersized member.
    match err {
        fauna_archive::error::ArchiveError::Member { member, .. } => {
            assert_eq!(member, "category.json");
        }
        other => panic!("expected ArchiveError::Member, got {other:?}"),
    }
}
