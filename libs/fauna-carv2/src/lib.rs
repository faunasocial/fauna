//! CARv2 framing wrapper — sync inline CARv1 codec + CARv2 outer framing.
//!
//! Fauna's at-rest container is standard CARv2 (https://ipld.io/specs/transport/car/carv2/):
//!
//! ```text
//! [pragma: 11 bytes]              // identifies the file as CARv2
//! [v2 header: 40 bytes]           // characteristics + data_offset + data_size + index_offset
//! [CARv1 data payload]            // varint(header_len) || dag_cbor({roots, version: 1})
//!                                 // then (varint(cid_len + block_len) || cid || block)*
//! [MultihashIndexSorted index]    // codec 0x0401, blake3-256 bucket, digest||offset entries
//! ```
//!
//! Any CARv2-capable tool (`go-car`, `iroh-car`, `kubo dag import`) reads
//! this format byte-identically — that's the whole point of leaving FNAS
//! behind in Layer 3.
//!
//! ## Why we reimplement the v1 codec inline
//!
//! The Layer 3 plan originally wrapped `iroh-car` 0.5 as the CARv1 codec.
//! That crate's `CarReader`/`CarWriter` are tokio-async (`tokio::io::AsyncWrite`),
//! but `fauna-segment-store` and `fauna-index` are sync (`std::fs::File`).
//! Rather than smuggle a tokio runtime into a sync wrapper, fauna-carv2
//! reimplements the ~50 LOC of v1 byte-framing inline. The byte layout is
//! identical; the cross-language go-car/v2 oracle (Layer 3 Task 3.6) is the
//! end-to-end conformance gate.
//!
//! ## Public API
//!
//! - [`Writer`] — incremental writer; `new` → `write_block`* → `finalize`.
//! - [`Reader`] — random-access reader; `new` parses pragma + header + index,
//!   `get(&cid)` does an O(log n) lookup, `iter()` walks in sorted order.
//! - [`Header`] — the 40-byte v2 header (encode/decode).
//! - [`Pragma`] — the 11-byte v2 pragma constant.
//! - [`Error`] — error type.

pub mod error;
pub mod header;
pub mod index;
pub mod pragma;
pub mod reader;
pub mod v1;
pub mod writer;

pub use error::Error;
pub use header::Header;
pub use index::{IndexEntry, MULTIHASH_BLAKE3_256, MULTIHASH_INDEX_SORTED_CODEC};
pub use pragma::{Pragma, parse_pragma};
pub use reader::{Iter, Reader};
pub use writer::Writer;

use fauna_cbor::Cid;

/// Convert a `fauna_cbor::Cid` to the upstream `cid::Cid` type. Cheap byte-
/// level conversion: both encode v1 + dag-cbor + blake3-256 + 32-byte digest
/// to the same 36 bytes; the only reason this dance exists is that
/// `serde_ipld_dagcbor`'s CID tag-42 encoder is keyed on `cid::Cid` (via
/// `ipld_core::cid::serde`), and the CARv1 per-record framing uses
/// `cid::Cid::write_bytes` for the on-disk CID prefix.
pub(crate) fn fauna_cid_to_upstream(cid: &Cid) -> cid::Cid {
    let digest = cid.digest();
    let mh = cid::multihash::Multihash::wrap(MULTIHASH_BLAKE3_256, &digest)
        .expect("32-byte blake3 digest fits in <=64-byte multihash");
    // dag-cbor codec = 0x71 (Cid::codec() returns u8; widen to u64).
    cid::Cid::new_v1(cid.codec() as u64, mh)
}

/// Convert the upstream `cid::Cid` back to `fauna_cbor::Cid`. Validates the
/// codec (dag-cbor) and multihash code (blake3-256) — wrong-codec or
/// wrong-hash CIDs come back as `Error::UnsupportedCodec` /
/// `Error::UnsupportedMultihash` so callers don't silently get garbage.
pub(crate) fn upstream_cid_to_fauna(cid: &cid::Cid) -> Result<Cid, Error> {
    if cid.codec() != 0x71 {
        return Err(Error::UnsupportedCodec(cid.codec()));
    }
    let mh = cid.hash();
    if mh.code() != MULTIHASH_BLAKE3_256 {
        return Err(Error::UnsupportedMultihash(mh.code()));
    }
    if mh.size() != 32 {
        return Err(Error::BadIndex(format!(
            "expected 32-byte blake3 digest, got {}",
            mh.size()
        )));
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&mh.digest()[..32]);
    Ok(Cid::from_digest_dag_cbor(digest))
}
