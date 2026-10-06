//! Decode a hex string into a fixed 32-byte array.
//!
//! Many Fauna identifiers — `ActorId`, an `ActorKeypair` secret, an MLS
//! `ChannelId`, a `calendar_id` / `uid_hash`, a `PostId` — are 32-byte values
//! exchanged across the client glue as lowercase hex. Every consumer used to
//! hand-roll `hex::decode(s).try_into::<[u8; 32]>()` with its own error
//! handling; this is the one canonical decoder so the parse (and its
//! whitespace/length contract) can't drift per call site.
//!
//! The meaningful domain constructors compose this primitive:
//! [`crate::identity::ActorId::from_hex`],
//! [`crate::identity::ActorKeypair::from_secret_hex`], and
//! `fauna_mls::types::ChannelId::from_hex`.

/// Error decoding a hex string into a 32-byte array.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Hex32Error {
    /// The string was not valid hexadecimal.
    #[error("not valid hex: {0}")]
    NotHex(String),
    /// The decoded bytes were not exactly 32 long.
    #[error("expected 32 bytes (64 hex chars), got {0}")]
    WrongLength(usize),
}

/// Decode a hex string into a 32-byte array.
///
/// Leading/trailing ASCII whitespace is trimmed before decoding (so a value
/// pasted with a stray newline still parses); the input must otherwise be
/// exactly 64 hex digits.
pub fn decode(s: &str) -> Result<[u8; 32], Hex32Error> {
    let bytes = hex::decode(s.trim()).map_err(|e| Hex32Error::NotHex(e.to_string()))?;
    let len = bytes.len();
    bytes.try_into().map_err(|_| Hex32Error::WrongLength(len))
}

/// Encode a 32-byte array as lowercase hex.
pub fn encode(bytes: &[u8; 32]) -> String {
    hex::encode(bytes)
}

/// Whether `s` is exactly 64 hex digits, with no whitespace tolerance —
/// several callers want this validity check rather than the decoded bytes.
/// The length gate runs first, so `decode`'s own whitespace trim never
/// changes the answer: a value with leading/trailing whitespace is already
/// longer than 64 and fails here regardless.
pub fn is_hex64(s: &str) -> bool {
    s.len() == 64 && decode(s).is_ok()
}

/// Whether `s` is exactly 64 **lowercase-only** hex digits — stricter than
/// [`is_hex64`], which (via [`decode`]'s underlying `hex::decode`) accepts
/// either case. Several on-disk/on-wire keying schemes mint their spelling as
/// lowercase specifically so two case variants of one value can never fork a
/// lookup keyed on the string itself (a filename, a lock-file name); those
/// callers need this narrower check, not `is_hex64`.
pub fn is_lowercase_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_64_hex_chars() {
        let h = hex::encode([0xABu8; 32]);
        assert_eq!(decode(&h), Ok([0xABu8; 32]));
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let h = format!("  {}\n", hex::encode([0x01u8; 32]));
        assert_eq!(decode(&h), Ok([0x01u8; 32]));
    }

    #[test]
    fn rejects_non_hex() {
        assert!(matches!(decode("zz"), Err(Hex32Error::NotHex(_))));
    }

    #[test]
    fn rejects_wrong_length() {
        // valid hex, but only 4 bytes
        assert_eq!(decode("deadbeef"), Err(Hex32Error::WrongLength(4)));
    }

    #[test]
    fn encode_round_trips_through_decode() {
        let bytes = [0x3Cu8; 32];
        assert_eq!(decode(&encode(&bytes)), Ok(bytes));
    }

    #[test]
    fn encode_is_lowercase() {
        assert_eq!(encode(&[0xABu8; 32]), "ab".repeat(32));
    }

    #[test]
    fn is_lowercase_hex64_rejects_uppercase_unlike_is_hex64() {
        let upper = "AB".repeat(32);
        assert!(is_hex64(&upper));
        assert!(!is_lowercase_hex64(&upper));
    }

    #[test]
    fn is_lowercase_hex64_accepts_lowercase_and_rejects_wrong_length() {
        assert!(is_lowercase_hex64(&"ab".repeat(32)));
        assert!(!is_lowercase_hex64("deadbeef"));
        assert!(!is_lowercase_hex64(""));
    }
}
