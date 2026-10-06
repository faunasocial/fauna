//! Error type for the CARv2 framing wrapper.

use fauna_cbor::Cid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("CARv2 pragma mismatch (expected the 11-byte CARv2 pragma)")]
    BadPragma,

    #[error("CARv2 header decode failed: {0}")]
    BadHeader(String),

    #[error("CARv1 inner-header decode failed: {0}")]
    BadV1Header(String),

    #[error("MultihashIndexSorted decode failed: {0}")]
    BadIndex(String),

    #[error("unsupported CID codec: 0x{0:02x} (fauna-carv2 only accepts dag-cbor 0x71)")]
    UnsupportedCodec(u64),

    #[error("unsupported multihash code: 0x{0:02x} (fauna-carv2 only accepts blake3-256 0x1e)")]
    UnsupportedMultihash(u64),

    #[error("CID mismatch reading block (expected {expected}, got {got})")]
    CidMismatch { expected: Cid, got: Cid },

    #[error("CID not found in index")]
    CidNotFound,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("dag-cbor codec error: {0}")]
    Cbor(String),
}
