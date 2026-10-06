//! CARv2 40-byte header that follows the pragma.
//!
//! Per the CARv2 spec § Header:
//!
//! ```text
//! +-----------------+-----------------+-----------------+-----------------+
//! | characteristics |   data_offset   |    data_size    |  index_offset   |
//! |    16 bytes     |    8 bytes      |    8 bytes      |    8 bytes      |
//! +-----------------+-----------------+-----------------+-----------------+
//! ```
//!
//! - `characteristics`: 128-bit bitfield (current spec defines only bit 7 of
//!   byte 0 — "fully indexed"; the rest reserved as zero).
//! - `data_offset`: byte offset of the first byte of the CARv1 inner payload
//!   from the start of the file. Little-endian u64.
//! - `data_size`: size of the CARv1 inner payload, in bytes. Little-endian u64.
//! - `index_offset`: byte offset of the index from the start of the file (0 if
//!   the file has no index). Little-endian u64.
//!
//! The writer writes a zero-filled header up front (so the data section starts
//! at a known fixed offset = 11 + 40 = 51), then seeks back and overwrites it
//! at `finalize()` once `data_size` and `index_offset` are known.

use crate::Error;

/// The 40-byte CARv2 header. Lives between the pragma and the CARv1 data
/// payload. Always 40 bytes; offsets in the file are byte counts from the
/// start of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub characteristics: [u8; 16],
    pub data_offset: u64,
    pub data_size: u64,
    pub index_offset: u64,
}

impl Header {
    pub const LEN: usize = 40;

    pub fn encode(&self) -> [u8; 40] {
        let mut buf = [0u8; 40];
        buf[0..16].copy_from_slice(&self.characteristics);
        buf[16..24].copy_from_slice(&self.data_offset.to_le_bytes());
        buf[24..32].copy_from_slice(&self.data_size.to_le_bytes());
        buf[32..40].copy_from_slice(&self.index_offset.to_le_bytes());
        buf
    }

    pub fn decode(bytes: &[u8; 40]) -> Result<Self, Error> {
        let mut characteristics = [0u8; 16];
        characteristics.copy_from_slice(&bytes[0..16]);
        let data_offset = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let data_size = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        let index_offset = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
        Ok(Header {
            characteristics,
            data_offset,
            data_size,
            index_offset,
        })
    }
}
