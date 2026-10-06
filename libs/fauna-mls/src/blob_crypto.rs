//! Blob content encryption using epoch-derived symmetric keys.

use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};

/// Derive a symmetric encryption key from an MLS epoch secret.
pub fn derive_blob_key(epoch_secret: &[u8]) -> [u8; 32] {
    blake3::derive_key("fauna.blob.v1", epoch_secret)
}

/// Derive the nonce-derivation key from the same epoch secret, domain-
/// separated from the AEAD key (`fauna.blob.v1`) so neither derivation
/// constrains the other.
fn derive_blob_nonce_key(epoch_secret: &[u8]) -> [u8; 32] {
    blake3::derive_key("fauna.blob.nonce.v1", epoch_secret)
}

/// Encrypt blob content with a key derived from the epoch secret.
/// Returns: nonce (12 bytes) || ciphertext.
///
/// The nonce is derived deterministically as
/// `blake3::keyed_hash(nonce_key, plaintext)[..12]`, keyed under the
/// epoch-derived [`derive_blob_nonce_key`]. Identical plaintext under the
/// same epoch still produces identical ciphertext — a content-addressed
/// blob store dedups by construction, and random nonce management stays
/// avoided — but the cleartext 12-byte prefix is computable only by
/// holders of the epoch's key material. An UNKEYED plaintext hash here
/// was a keyless candidate-plaintext confirmation oracle and a
/// cross-group correlator for anyone holding the sealed bytes
/// (`owner-key-material.md`'s ratified no-confirmation/no-correlation
/// property; keyed 2026-08-31). Nonce reuse across
/// different epoch keys (different derived keys) is safe for
/// ChaCha20-Poly1305.
pub fn encrypt_blob(epoch_secret: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let key = derive_blob_key(epoch_secret);
    let cipher = ChaCha20Poly1305::new((&key).into());
    let nonce_key = derive_blob_nonce_key(epoch_secret);
    let nonce_bytes = blake3::keyed_hash(&nonce_key, plaintext);
    let nonce = Nonce::from_slice(&nonce_bytes.as_bytes()[..12]);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| format!("encrypt: {e}"))?;
    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes.as_bytes()[..12]);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt blob content with a key derived from the epoch secret.
/// Input: nonce (12 bytes) || ciphertext.
///
/// The nonce-prefixed AEAD-open step is byte-identical to
/// `fauna_core::subscription::crypto::decrypt_content`'s (both read a
/// 12-byte nonce prefix, no AAD), so this delegates there rather than
/// re-implementing it; only key sourcing differs (epoch-derived here vs.
/// already-derived for `decrypt_content`'s callers) and stays separate.
pub fn decrypt_blob(epoch_secret: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    let key = derive_blob_key(epoch_secret);
    fauna_core::subscription::crypto::decrypt_content(&key, data).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_encrypt_decrypt_roundtrip() {
        let epoch_secret = b"test-epoch-secret-32-bytes-long!";
        let plaintext = b"Hello, this is a blob content for testing.";

        let encrypted = encrypt_blob(epoch_secret, plaintext).unwrap();
        assert_ne!(&encrypted[..], &plaintext[..]);
        assert!(encrypted.len() > plaintext.len());

        let decrypted = decrypt_blob(epoch_secret, &encrypted).unwrap();
        assert_eq!(&decrypted[..], &plaintext[..]);
    }

    #[test]
    fn wrong_epoch_secret_fails() {
        let epoch_secret = b"test-epoch-secret-32-bytes-long!";
        let wrong_secret = b"wrong-epoch-secret-32-bytes!!!!";
        let plaintext = b"secret blob";

        let encrypted = encrypt_blob(epoch_secret, plaintext).unwrap();
        let result = decrypt_blob(wrong_secret, &encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn different_epochs_produce_different_ciphertext() {
        let secret1 = b"epoch-1-secret-aaaaaaaaaaaaaaaaa";
        let secret2 = b"epoch-2-secret-bbbbbbbbbbbbbbbbb";
        let plaintext = b"same content";

        let enc1 = encrypt_blob(secret1, plaintext).unwrap();
        let enc2 = encrypt_blob(secret2, plaintext).unwrap();
        assert_ne!(enc1, enc2);
    }

    /// The cleartext nonce prefix must be computable only by key holders:
    /// identical plaintext sealed under two different epoch secrets must not
    /// carry the same 12-byte prefix, or the blob at rest is a key-independent
    /// cross-group/cross-owner correlator (`owner-key-material.md`'s ratified
    /// no-correlation property).
    #[test]
    fn identical_plaintext_under_different_epochs_yields_different_prefixes() {
        let secret1 = b"epoch-1-secret-aaaaaaaaaaaaaaaaa";
        let secret2 = b"epoch-2-secret-bbbbbbbbbbbbbbbbb";
        let plaintext = b"same attachment bytes";

        let enc1 = encrypt_blob(secret1, plaintext).unwrap();
        let enc2 = encrypt_blob(secret2, plaintext).unwrap();
        assert_ne!(
            &enc1[..12],
            &enc2[..12],
            "an epoch-independent nonce prefix correlates identical content across groups"
        );
    }

    /// The prefix must not equal the unkeyed `blake3::hash(plaintext)[..12]`
    /// — that value is a keyless ~96-bit candidate-plaintext confirmation
    /// oracle for anyone holding the sealed blob (the nest, a backup thief).
    #[test]
    fn prefix_is_not_the_unkeyed_plaintext_hash() {
        let epoch_secret = b"test-epoch-secret-32-bytes-long!";
        let plaintext = b"guessable public file bytes";

        let encrypted = encrypt_blob(epoch_secret, plaintext).unwrap();
        let unkeyed = blake3::hash(plaintext);
        assert_ne!(
            &encrypted[..12],
            &unkeyed.as_bytes()[..12],
            "the cleartext prefix confirms a candidate plaintext with no key at all"
        );
    }
}
