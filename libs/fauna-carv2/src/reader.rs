//! CARv2 reader over `std::io::{Read, Seek}`.
//!
//! Lifecycle:
//!
//! 1. `Reader::new(r)` — validates the pragma, decodes the 40-byte header,
//!    parses the CARv1 inner-header (must say `version: 1`), parses the
//!    MultihashIndexSorted index. After this, the reader holds the in-memory
//!    `(digest, offset)` table; block bytes stay on disk and are pulled on
//!    demand.
//!
//! 2. `reader.get(cid)` — binary-searches the digest list, seeks to the
//!    record offset, reads `varint(len) || cid_bytes || block_bytes`, and
//!    returns the block bytes. Verifies the on-disk CID matches the
//!    requested CID; mismatch is `Error::CidMismatch`.
//!
//! 3. `reader.iter()` — walks the index in sorted-digest order, yielding
//!    `(Cid, Vec<u8>)` per block. Each iteration step does one seek + read.
//!
//! NOTE: `get()` does NOT re-hash the block bytes to verify they match the
//! CID's digest — it only compares the on-disk CID prefix bytes to the
//! requested CID. The caller is responsible for the second-tier check (e.g.,
//! the `fauna-segment-store` consumer can call `cid.matches(&bytes)` after
//! retrieval if it wants belt-and-suspenders integrity). This split keeps
//! the reader cheap on the hot path; segment-store reads thousands of blocks
//! per scan and the seal layer already authenticates payloads.

use crate::error::Error;
use crate::header::Header;
use crate::index::{IndexEntry, read_index};
use crate::pragma::Pragma;
use crate::upstream_cid_to_fauna;
use crate::v1::{read_file_header, read_record, read_varint_usize};
use cid::Cid as UpstreamCid;
use fauna_cbor::Cid;
use std::io::{Read, Seek, SeekFrom};

/// CARv2 reader. Generic over any sync `Read + Seek`. Typical concrete type:
/// `std::fs::File`.
pub struct Reader<R: Read + Seek> {
    r: R,
    header: Header,
    /// Index entries, sorted ascending by digest (invariant verified at
    /// `Reader::new` time by `index::read_index`).
    entries: Vec<IndexEntry>,
}

impl<R: Read + Seek> Reader<R> {
    pub fn new(mut r: R) -> Result<Self, Error> {
        // Pragma.
        let mut pragma = [0u8; Pragma::LEN];
        r.read_exact(&mut pragma)?;
        if pragma != Pragma::BYTES {
            return Err(Error::BadPragma);
        }

        // 40-byte v2 header.
        let mut header_bytes = [0u8; Header::LEN];
        r.read_exact(&mut header_bytes)?;
        let header = Header::decode(&header_bytes)?;

        // CARv1 file header at data_offset.
        r.seek(SeekFrom::Start(header.data_offset))?;
        let _v1_header = read_file_header(&mut r)?;

        // MultihashIndexSorted index at index_offset.
        r.seek(SeekFrom::Start(header.index_offset))?;
        let entries = read_index(&mut r)?;

        Ok(Reader { r, header, entries })
    }

    /// The decoded 40-byte v2 header (useful for debug / introspection).
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Look up a block by CID. Returns `Err(Error::CidNotFound)` if the
    /// digest isn't in the index; `Err(Error::CidMismatch)` if the on-disk
    /// CID at the indexed offset doesn't match the requested one (corruption
    /// or wrong index entry).
    pub fn get(&mut self, cid: &Cid) -> Result<Vec<u8>, Error> {
        let digest = cid.digest();
        let entry = match self.entries.binary_search_by(|e| e.digest.cmp(&digest)) {
            Ok(idx) => self.entries[idx],
            Err(_) => return Err(Error::CidNotFound),
        };

        self.r.seek(SeekFrom::Start(entry.offset))?;
        let (on_disk_cid, block) = read_record(&mut self.r)?;
        let on_disk = upstream_cid_to_fauna(&on_disk_cid)?;
        if on_disk != *cid {
            return Err(Error::CidMismatch {
                expected: *cid,
                got: on_disk,
            });
        }
        Ok(block)
    }

    /// Look up a block's payload byte-length by CID **without reading the
    /// block body**. Binary-searches the index, seeks to the record offset,
    /// reads only the record's length-prefix varint and the on-disk CID
    /// header, and returns `block_len = total_record_len - cid_len`.
    ///
    /// This is the size source IMAP RFC822.SIZE / SEARCH `LARGER`|`SMALLER` /
    /// the STORAGE quota walk use (per `imap-server.md` §§ SEARCH, QUOTA):
    /// the sealed record's ciphertext byte length, looked up by
    /// `segment_records.record_cid` through the index — never a mirrored SQL
    /// column. It equals the length of the bytes the writer passed to
    /// `write_block` (the record frame is
    /// `varint(cid_len + block_len) || cid_bytes || block_bytes`), so the
    /// body is never read off disk.
    ///
    /// `Err(Error::CidNotFound)` if the digest isn't in the index;
    /// `Err(Error::CidMismatch)` if the on-disk CID at the indexed offset
    /// doesn't match the requested one (corruption or wrong index entry) —
    /// the same verification `get()` performs.
    pub fn block_len(&mut self, cid: &Cid) -> Result<u64, Error> {
        let digest = cid.digest();
        let entry = match self.entries.binary_search_by(|e| e.digest.cmp(&digest)) {
            Ok(idx) => self.entries[idx],
            Err(_) => return Err(Error::CidNotFound),
        };

        self.r.seek(SeekFrom::Start(entry.offset))?;
        // Record frame: varint(cid_len + block_len) || cid_bytes || block_bytes.
        let total_len = read_varint_usize(&mut self.r)?;
        // Read only the CID header (it sits right after the varint). This is
        // how we learn cid_len without touching the block body, and it lets
        // us verify the on-disk CID matches the requested one (as get() does).
        let on_disk_cid = UpstreamCid::read_bytes(&mut self.r)
            .map_err(|e| Error::BadIndex(format!("cid decode: {e}")))?;
        let on_disk = upstream_cid_to_fauna(&on_disk_cid)?;
        if on_disk != *cid {
            return Err(Error::CidMismatch {
                expected: *cid,
                got: on_disk,
            });
        }
        let cid_len = on_disk_cid.encoded_len();
        let block_len = total_len.checked_sub(cid_len).ok_or_else(|| {
            Error::BadIndex(format!(
                "record frame length {total_len} smaller than its {cid_len}-byte CID header"
            ))
        })?;
        Ok(block_len as u64)
    }

    /// Iterate every (cid, block) in the file, in index order (ascending by
    /// digest). Each step does a seek + read; the index entries themselves
    /// are already in memory.
    pub fn iter(&mut self) -> Iter<'_, R> {
        Iter {
            reader: self,
            pos: 0,
        }
    }

    /// The number of indexed blocks.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub struct Iter<'a, R: Read + Seek> {
    reader: &'a mut Reader<R>,
    pos: usize,
}

impl<'a, R: Read + Seek> Iterator for Iter<'a, R> {
    type Item = Result<(Cid, Vec<u8>), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.reader.entries.len() {
            return None;
        }
        let entry = self.reader.entries[self.pos];
        self.pos += 1;
        let result = (|| -> Result<(Cid, Vec<u8>), Error> {
            self.reader.r.seek(SeekFrom::Start(entry.offset))?;
            let (on_disk_cid, block) = read_record(&mut self.reader.r)?;
            let fauna = upstream_cid_to_fauna(&on_disk_cid)?;
            // Sanity: on-disk digest must match the index entry.
            if fauna.digest() != entry.digest {
                return Err(Error::BadIndex(
                    "index digest does not match on-disk CID digest".into(),
                ));
            }
            Ok((fauna, block))
        })();
        Some(result)
    }
}
