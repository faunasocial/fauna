//! CARv2 writer over `std::io::{Write, Seek}`.
//!
//! Lifecycle:
//!
//! 1. `Writer::new(w, roots)` — writes the pragma (11 bytes), a zero-filled
//!    40-byte header placeholder, and the CARv1 file header (`varint(len) ||
//!    dag_cbor({roots, version: 1})`). Records the data section's start
//!    offset.
//!
//! 2. `writer.write_block(cid, bytes)` (any number of times) — writes one
//!    CARv1 record (`varint(cid_len + block_len) || cid || block`) and
//!    records `(digest, offset)` for the index. The offset is the byte offset
//!    of the varint length prefix (i.e., where the *record* starts, not where
//!    the cid/block start) — this matches the CARv2 `MultihashIndexSorted`
//!    spec's convention.
//!
//! 3. `writer.finalize()` — seeks to the end of the data section, writes the
//!    `MultihashIndexSorted` index (sorted ascending by digest), seeks back
//!    to byte 11, and overwrites the placeholder header with the real one
//!    (data_offset / data_size / index_offset).

use crate::error::Error;
use crate::header::Header;
use crate::index::{IndexEntry, write_index};
use crate::pragma::Pragma;
use crate::v1::write_file_header;
use crate::{fauna_cid_to_upstream, v1};
use fauna_cbor::Cid;
use std::io::{Seek, SeekFrom, Write};

/// CARv2 writer. Generic over any sync `Write + Seek`. Typical concrete
/// type: `std::fs::File`.
pub struct Writer<W: Write + Seek> {
    w: W,
    data_offset: u64,
    entries: Vec<IndexEntry>,
}

impl<W: Write + Seek> Writer<W> {
    /// Open a fresh CARv2 file: write pragma + zero header + v1 file header.
    /// `roots` may be empty for segment-store-style files (no semantic root);
    /// pass a single CID for single-block manifests.
    pub fn new(mut w: W, roots: &[&Cid]) -> Result<Self, Error> {
        // Pragma (11 bytes).
        w.write_all(&Pragma::BYTES)?;

        // Zero-filled v2 header placeholder (40 bytes). Backfilled in finalize().
        w.write_all(&[0u8; Header::LEN])?;

        // Record the data section's start offset (always 11 + 40 = 51 because
        // the pragma + header are fixed-width, but we compute it for clarity
        // and so a future spec-extension that grows the header doesn't silently
        // mis-fill data_offset).
        let data_offset = w.stream_position()?;

        // CARv1 file header.
        let upstream_roots: Vec<cid::Cid> =
            roots.iter().map(|c| fauna_cid_to_upstream(c)).collect();
        write_file_header(&mut w, &upstream_roots)?;

        Ok(Writer {
            w,
            data_offset,
            entries: Vec::new(),
        })
    }

    /// Write a single block to the data section and remember its offset for
    /// the index. The CID's codec + multihash MUST be dag-cbor + blake3-256
    /// (fauna_cbor::Cid guarantees this by construction); the CID's digest
    /// is what the index sorts on.
    pub fn write_block(&mut self, cid: &Cid, block: &[u8]) -> Result<(), Error> {
        let offset = self.w.stream_position()?;
        let upstream = fauna_cid_to_upstream(cid);
        v1::write_record(&mut self.w, &upstream, block)?;
        self.entries.push(IndexEntry {
            digest: cid.digest(),
            offset,
        });
        Ok(())
    }

    /// Finalize the file: write the index, then backfill the v2 header.
    /// Consumes `self` (the file is now sealed).
    pub fn finalize(mut self) -> Result<W, Error> {
        // Position at the end of the data section.
        let index_offset = self.w.stream_position()?;
        let data_size = index_offset - self.data_offset;

        // Write the index in place.
        write_index(&mut self.w, &mut self.entries)?;

        // Build the final header. "Fully indexed" characteristics bit is the
        // high bit of byte 0 (bit 7 of byte 0 in MSB ordering per the CARv2
        // spec § Header / characteristics). Set it because we always write the
        // MultihashIndexSorted index.
        let mut characteristics = [0u8; 16];
        characteristics[0] = 0x80;
        let header = Header {
            characteristics,
            data_offset: self.data_offset,
            data_size,
            index_offset,
        };

        // Seek back to byte 11 (immediately after the pragma) and overwrite
        // the placeholder. Then leave the cursor at the index offset so the
        // caller can flush / drop in a known position.
        self.w.seek(SeekFrom::Start(Pragma::LEN as u64))?;
        self.w.write_all(&header.encode())?;
        self.w.flush()?;
        Ok(self.w)
    }
}
