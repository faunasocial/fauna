//! `MultihashIndexSorted` — the CARv2 index Fauna writes.
//!
//! Per the CARv2 spec § "MultihashIndexSorted" (codec `0x0401`):
//!
//! ```text
//! [varint(0x0401)]                            // index codec marker
//! [u32 LE bucket_count]                       // (some impls use u64; spec says u32)
//! for each bucket:
//!     [varint(multihash_code)]                // e.g. 0x1e for blake3-256
//!     [u32 LE digest_width]                   // size in bytes of each digest
//!     [u64 LE count]                          // entries in this bucket
//!     for each entry (sorted ascending by digest):
//!         [digest_width bytes digest]
//!         [u64 LE offset]                     // offset into the file
//! ```
//!
//! Fauna writes one bucket per file (blake3-256, 32-byte digest) since every
//! block's CID uses the same hash.
//!
//! Spec source: https://ipld.io/specs/transport/car/carv2/#format-1-multihashindexsorted

use crate::Error;
use crate::v1::{read_varint_u64, write_varint_u64};
use std::io::{Read, Write};

/// CARv2 index codec marker for `MultihashIndexSorted`.
pub const MULTIHASH_INDEX_SORTED_CODEC: u64 = 0x0401;

/// Multihash code for BLAKE3-256 (the only hash Fauna uses for content
/// addressing).
pub const MULTIHASH_BLAKE3_256: u64 = 0x1e;

/// Digest width (bytes) of BLAKE3-256.
pub const BLAKE3_256_DIGEST_WIDTH: u32 = 32;

#[derive(Debug, Clone, Copy)]
pub struct IndexEntry {
    pub digest: [u8; 32],
    pub offset: u64,
}

/// Write a single-bucket MultihashIndexSorted with the given (digest, offset)
/// entries. The entries are sorted in place before writing, ascending by
/// digest (spec requirement so the reader can binary-search).
pub fn write_index<W: Write>(w: &mut W, entries: &mut [IndexEntry]) -> Result<(), Error> {
    entries.sort_unstable_by_key(|e| e.digest);

    // Index codec marker (varint).
    write_varint_u64(w, MULTIHASH_INDEX_SORTED_CODEC)?;
    // Number of buckets (u32 LE). We always write exactly one bucket.
    w.write_all(&1u32.to_le_bytes())?;

    // Bucket header.
    write_varint_u64(w, MULTIHASH_BLAKE3_256)?;
    w.write_all(&BLAKE3_256_DIGEST_WIDTH.to_le_bytes())?;
    w.write_all(&(entries.len() as u64).to_le_bytes())?;

    // Entries: digest || offset_u64_LE, in sorted order.
    for entry in entries.iter() {
        w.write_all(&entry.digest)?;
        w.write_all(&entry.offset.to_le_bytes())?;
    }
    Ok(())
}

/// Read a single-bucket MultihashIndexSorted from `r`. Returns the bucket's
/// entries in the same sorted order they were written in. Validates that the
/// codec marker, multihash code, and digest width match Fauna's profile
/// (blake3-256, 32 bytes).
pub fn read_index<R: Read>(r: &mut R) -> Result<Vec<IndexEntry>, Error> {
    let codec = read_varint_u64(r)?;
    if codec != MULTIHASH_INDEX_SORTED_CODEC {
        return Err(Error::BadIndex(format!(
            "expected MultihashIndexSorted codec 0x{MULTIHASH_INDEX_SORTED_CODEC:x}, got 0x{codec:x}"
        )));
    }

    let mut bucket_count_buf = [0u8; 4];
    r.read_exact(&mut bucket_count_buf)?;
    let bucket_count = u32::from_le_bytes(bucket_count_buf);
    if bucket_count != 1 {
        return Err(Error::BadIndex(format!(
            "expected exactly 1 bucket (Fauna only uses blake3-256), got {bucket_count}"
        )));
    }

    let multihash_code = read_varint_u64(r)?;
    if multihash_code != MULTIHASH_BLAKE3_256 {
        return Err(Error::UnsupportedMultihash(multihash_code));
    }

    let mut width_buf = [0u8; 4];
    r.read_exact(&mut width_buf)?;
    let width = u32::from_le_bytes(width_buf);
    if width != BLAKE3_256_DIGEST_WIDTH {
        return Err(Error::BadIndex(format!(
            "expected blake3-256 digest width {BLAKE3_256_DIGEST_WIDTH}, got {width}"
        )));
    }

    let mut count_buf = [0u8; 8];
    r.read_exact(&mut count_buf)?;
    let count = u64::from_le_bytes(count_buf);
    // Sanity bound: avoid allocating a multi-GB vec on a corrupt file.
    if count > 1 << 30 {
        return Err(Error::BadIndex(format!("absurd entry count: {count}")));
    }

    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let mut digest = [0u8; 32];
        r.read_exact(&mut digest)?;
        let mut off_buf = [0u8; 8];
        r.read_exact(&mut off_buf)?;
        entries.push(IndexEntry {
            digest,
            offset: u64::from_le_bytes(off_buf),
        });
    }

    // Verify ascending-digest invariant; the reader's get() depends on it for
    // binary search.
    for w in entries.windows(2) {
        if w[0].digest >= w[1].digest {
            return Err(Error::BadIndex(
                "index entries are not strictly ascending by digest".into(),
            ));
        }
    }

    Ok(entries)
}
