//! Plaintext shape of a [`crate::wrapped_blob::format::WebdavKeysBlob`] after
//! MSEK AEAD-unwrap.
//!
//! Per `docs/goal/behavior/webdav-server.md` § Key model, the WebDAV MDA's
//! capability is the **content keys of the sets the user has explicitly flagged
//! for serving**. This struct is the wire shape the user's primary client emits
//! via `provision_webdav_keys_blob` and the MDA decodes after AEAD-unwrap: one
//! [`ServedSetKeys`] per served set, carrying the set's owner-local name, its
//! read-only serve flag, and the full [`FolderContentKeys`] generation history
//! (so the MDA can `key_for(version)` any historical file, not just `current`).
//!
//! Additive-growth discipline matches the MLS snapshot plaintext: the wire
//! versions via `v`, canonical dag-cbor (`fauna_cbor`) keeps map keys
//! length-first sorted, and serde ignores unknown fields — so an older decoder
//! still parses a newer blob and new per-set fields grow the struct additively.

use crate::wrapped_blob::format::{UnwrapError, WrapError};
use fauna_core::folder_keys::FolderContentKeys;
use serde::{Deserialize, Serialize};

/// Current WebDAV-keys-plaintext format version.
pub const WEBDAV_KEYS_PLAINTEXT_VERSION: u8 = 1;

/// One served set's key material: its owner-local name, its read-only serve
/// flag, and the content-key generation history the MDA decrypts under.
///
/// `keys` is the same [`FolderContentKeys`] a shared-set member holds — group
/// membership is not required to hold content keys (it is already group-free),
/// which is exactly what lets a *served-but-unshared* set carry keys here with
/// no MLS group. `read_only` is advisory metadata the MDA enforces at the PUT
/// boundary (a read-only served set rejects writes); it is **not** a key split
/// — `chunk_crypto` is symmetric AEAD, so the same generations serve both GET
/// and PUT (`webdav-server.md` § Key model).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedSetKeys {
    #[serde(rename = "n")]
    pub set_name: String,
    #[serde(rename = "ro")]
    pub read_only: bool,
    #[serde(rename = "k")]
    pub keys: FolderContentKeys,
}

/// AEAD-unwrapped plaintext of a [`crate::wrapped_blob::format::WebdavKeysBlob`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebdavKeysPlaintext {
    #[serde(rename = "v")]
    pub v: u8,
    #[serde(rename = "served_sets")]
    pub served_sets: Vec<ServedSetKeys>,
}

impl WebdavKeysPlaintext {
    /// Build a v1 plaintext from the served sets' key material.
    #[must_use]
    pub fn new(served_sets: Vec<ServedSetKeys>) -> Self {
        Self {
            v: WEBDAV_KEYS_PLAINTEXT_VERSION,
            served_sets,
        }
    }

    /// Encode to canonical DAG-CBOR bytes (the same codec every other
    /// wrapped-blob wire shape uses).
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure (practically
    /// unreachable for the fixed wire shape).
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode from canonical CBOR.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR or an unsupported
    /// version.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        let pt: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("webdav-keys-plaintext cbor: {e}")))?;
        if pt.v != WEBDAV_KEYS_PLAINTEXT_VERSION {
            return Err(UnwrapError::InvalidFormat(format!(
                "unsupported webdav-keys-plaintext version: {}",
                pt.v
            )));
        }
        Ok(pt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::folder_keys::FolderContentKeys;

    fn served(name: &str, read_only: bool, key: u8) -> ServedSetKeys {
        ServedSetKeys {
            set_name: name.to_string(),
            read_only,
            keys: FolderContentKeys::genesis([key; 32], 1_700_000_000),
        }
    }

    #[test]
    fn cbor_roundtrip_preserves_served_sets() {
        let pt = WebdavKeysPlaintext::new(vec![
            served("docs", false, 0xAA),
            served("photos", true, 0xBB),
        ]);
        let bytes = pt.to_canonical_bytes().unwrap();
        let decoded = WebdavKeysPlaintext::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded, pt);
        assert_eq!(decoded.served_sets.len(), 2);
        assert_eq!(decoded.served_sets[0].set_name, "docs");
        assert!(!decoded.served_sets[0].read_only);
        assert_eq!(decoded.served_sets[0].keys.current_key(), &[0xAA; 32]);
        assert!(decoded.served_sets[1].read_only);
    }

    #[test]
    fn preserves_full_generation_history() {
        // A rotated set carries `prior` generations so the MDA can decrypt
        // pre-rotation files via `key_for(version)`.
        let mut keys = FolderContentKeys::genesis([1u8; 32], 1_700_000_000);
        keys.rotate([2u8; 32], 1_700_000_100);
        let pt = WebdavKeysPlaintext::new(vec![ServedSetKeys {
            set_name: "docs".into(),
            read_only: false,
            keys: keys.clone(),
        }]);
        let bytes = pt.to_canonical_bytes().unwrap();
        let decoded = WebdavKeysPlaintext::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded.served_sets[0].keys.key_for(1), Some(&[1u8; 32]));
        assert_eq!(decoded.served_sets[0].keys.key_for(2), Some(&[2u8; 32]));
    }

    #[test]
    fn empty_served_sets_round_trips() {
        let pt = WebdavKeysPlaintext::new(vec![]);
        let bytes = pt.to_canonical_bytes().unwrap();
        let decoded = WebdavKeysPlaintext::from_canonical_bytes(&bytes).unwrap();
        assert!(decoded.served_sets.is_empty());
    }

    #[test]
    fn rejects_unknown_version() {
        let bad = WebdavKeysPlaintext {
            v: 99,
            served_sets: vec![],
        };
        let bytes = bad.to_canonical_bytes().unwrap();
        let err = WebdavKeysPlaintext::from_canonical_bytes(&bytes).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }
}
