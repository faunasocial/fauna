//! Inline CARv1 codec — sync, on top of `std::io::{Read, Write, Seek}`.
//!
//! Per the CARv1 spec (https://ipld.io/specs/transport/car/carv1/), a CARv1
//! file is:
//!
//! ```text
//! [varint(header_len) || dag_cbor({"roots": [Cid], "version": 1})]
//! ([varint(cid_len + block_len) || cid_bytes || block_bytes])*
//! ```
//!
//! In a CARv2 file, the "CARv1 data payload" sits between the v2 header and
//! the v2 index; the v2 header records its absolute byte offset and length.
//!
//! This module is the sync analog of `iroh-car` 0.5.1's writer/reader/header/
//! util modules, which are tokio-async and therefore don't fit the sync I/O
//! surface of fauna-segment-store + fauna-index. The byte format is the same
//! standard CARv1 frame — verified at Layer 3 close-out by Task 3.6's
//! cross-language go-car/v2 oracle.

use crate::Error;
use cid::Cid as UpstreamCid;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

/// CARv1 file-header struct. Encoded as dag-cbor under a varint length
/// prefix. `roots` may be empty (the CARv1 spec discourages but doesn't
/// forbid it; fauna-carv2 allows empty roots — segments use them, manifests
/// pass a single root).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct V1Header {
    pub roots: Vec<UpstreamCid>,
    pub version: u64,
}

/// Maximum length of a single varint-framed record we'll read. A CARv2 file
/// holding sealed mail segments tops out somewhere in the tens of MB per
/// record; 16 MB is well above any expected fauna payload but small enough
/// to protect against accidental garbage parses.
pub const MAX_RECORD_LEN: usize = 16 * 1024 * 1024;

/// Write the CARv1 file header (`varint(len) || dag_cbor({roots, version: 1})`)
/// to `w`. Returns the total number of bytes written.
pub fn write_file_header<W: Write>(w: &mut W, roots: &[UpstreamCid]) -> Result<usize, Error> {
    let header = V1Header {
        roots: roots.to_vec(),
        version: 1,
    };
    let cbor = serde_ipld_dagcbor::to_vec(&header).map_err(|e| Error::Cbor(e.to_string()))?;
    let mut written = write_varint_usize(w, cbor.len())?;
    w.write_all(&cbor)?;
    written += cbor.len();
    Ok(written)
}

/// Read the CARv1 file header from `r`. Validates that the decoded
/// `version` field is 1.
pub fn read_file_header<R: Read>(r: &mut R) -> Result<V1Header, Error> {
    let len = read_varint_usize(r)?;
    if len > MAX_RECORD_LEN {
        return Err(Error::BadV1Header(format!(
            "header length {len} exceeds MAX_RECORD_LEN {MAX_RECORD_LEN}"
        )));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    let header: V1Header =
        serde_ipld_dagcbor::from_slice(&buf).map_err(|e| Error::BadV1Header(e.to_string()))?;
    if header.version != 1 {
        return Err(Error::BadV1Header(format!(
            "version field is {}, expected 1",
            header.version
        )));
    }
    Ok(header)
}

/// Write a single CARv1 record (`varint(cid_len + block_len) || cid || block`)
/// to `w`. Returns the total number of bytes written. Uses the upstream
/// `cid::Cid` because that's the type the per-record framing speaks; convert
/// from `fauna_cbor::Cid` via [`crate::fauna_cid_to_upstream`].
pub fn write_record<W: Write>(w: &mut W, cid: &UpstreamCid, block: &[u8]) -> Result<usize, Error> {
    let mut cid_bytes = Vec::with_capacity(cid.encoded_len());
    cid.write_bytes(&mut cid_bytes)
        .map_err(|e| Error::Cbor(format!("cid encode: {e}")))?;
    let payload_len = cid_bytes.len() + block.len();
    let mut written = write_varint_usize(w, payload_len)?;
    w.write_all(&cid_bytes)?;
    w.write_all(block)?;
    written += payload_len;
    Ok(written)
}

/// Read a single CARv1 record from `r`. Returns the on-disk CID and the
/// block bytes.
pub fn read_record<R: Read>(r: &mut R) -> Result<(UpstreamCid, Vec<u8>), Error> {
    let len = read_varint_usize(r)?;
    if len > MAX_RECORD_LEN {
        return Err(Error::BadIndex(format!(
            "record length {len} exceeds MAX_RECORD_LEN {MAX_RECORD_LEN}"
        )));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    let mut cursor = std::io::Cursor::new(&buf[..]);
    let cid = UpstreamCid::read_bytes(&mut cursor)
        .map_err(|e| Error::BadIndex(format!("cid decode: {e}")))?;
    let pos = cursor.position() as usize;
    Ok((cid, buf[pos..].to_vec()))
}

/// Write `n` as an unsigned-varint to `w`. Returns the number of bytes
/// written.
pub fn write_varint_usize<W: Write>(w: &mut W, n: usize) -> Result<usize, Error> {
    let mut buf = unsigned_varint::encode::usize_buffer();
    let encoded = unsigned_varint::encode::usize(n, &mut buf);
    w.write_all(encoded)?;
    Ok(encoded.len())
}

/// Read an unsigned-varint as a usize from `r`.
pub fn read_varint_usize<R: Read>(r: &mut R) -> Result<usize, Error> {
    let mut buf = unsigned_varint::encode::usize_buffer();
    for i in 0..buf.len() {
        let n = r.read(&mut buf[i..i + 1])?;
        if n == 0 {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "unexpected EOF reading varint",
            )));
        }
        if unsigned_varint::decode::is_last(buf[i]) {
            let (value, _) = unsigned_varint::decode::usize(&buf[..=i])
                .map_err(|e| Error::BadIndex(format!("varint decode: {e}")))?;
            return Ok(value);
        }
    }
    Err(Error::BadIndex("varint overflow".into()))
}

/// Write an unsigned-varint as a u64.
pub fn write_varint_u64<W: Write>(w: &mut W, n: u64) -> Result<usize, Error> {
    let mut buf = unsigned_varint::encode::u64_buffer();
    let encoded = unsigned_varint::encode::u64(n, &mut buf);
    w.write_all(encoded)?;
    Ok(encoded.len())
}

/// Read an unsigned-varint as a u64.
pub fn read_varint_u64<R: Read>(r: &mut R) -> Result<u64, Error> {
    let mut buf = unsigned_varint::encode::u64_buffer();
    for i in 0..buf.len() {
        let n = r.read(&mut buf[i..i + 1])?;
        if n == 0 {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "unexpected EOF reading varint",
            )));
        }
        if unsigned_varint::decode::is_last(buf[i]) {
            let (value, _) = unsigned_varint::decode::u64(&buf[..=i])
                .map_err(|e| Error::BadIndex(format!("varint decode: {e}")))?;
            return Ok(value);
        }
    }
    Err(Error::BadIndex("varint overflow".into()))
}
