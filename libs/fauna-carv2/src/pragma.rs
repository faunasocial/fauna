//! CARv2 pragma — the 11-byte literal that distinguishes a v2 file from v1.
//!
//! Per the CARv2 spec § Pragma:
//!
//! ```text
//! 0x0a                                 // varint(10): length of the following dag-cbor map
//! 0xa1                                 // dag-cbor map of 1 pair
//! 0x67 0x76 0x65 0x72 0x73 0x69 0x6f 0x6e  // string "version" (7 bytes)
//! 0x02                                 // uint(2)
//! ```
//!
//! That is, a valid CARv1 file header for the document `{version: 2}` — older
//! v1 readers treat this as a v1 header whose version is unsupported, while
//! v2-aware readers recognize the literal and switch to v2 framing.

use crate::Error;

/// The 11-byte CARv2 pragma; bit-identical for every v2 file.
pub struct Pragma;

impl Pragma {
    pub const BYTES: [u8; 11] = [
        0x0a, 0xa1, 0x67, 0x76, 0x65, 0x72, 0x73, 0x69, 0x6f, 0x6e, 0x02,
    ];
    pub const LEN: usize = 11;
}

/// Validate that `bytes` starts with the CARv2 pragma. Returns
/// `Err(Error::BadPragma)` on any mismatch.
pub fn parse_pragma(bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() < Pragma::LEN || bytes[..Pragma::LEN] != Pragma::BYTES {
        return Err(Error::BadPragma);
    }
    Ok(())
}
