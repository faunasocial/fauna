//! ChaCha20-Poly1305 AEAD bound to wrapped-blob AAD shape.
//!
//! All wrapped blobs (other than HPKE shapes) seal with this primitive.
//! AAD is constructed via `format::AadBinding` and bound into the
//! AEAD operation; tampering with `(version, kind, ix)` fails verify.

use crate::wrapped_blob::format::{AadBinding, UnwrapError, WrapError};
use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce,
    aead::{Aead, KeyInit, Payload},
};

/// AEAD nonce length (bytes).
pub const AEAD_NONCE_LEN: usize = 12;

/// AEAD authentication tag length (bytes).
pub const AEAD_TAG_LEN: usize = 16;

/// Encrypt `plaintext` with `key`, `nonce`, and the canonical-CBOR
/// encoding of `aad_binding` as additional data. Returns ciphertext
/// including the trailing 16-byte Poly1305 tag.
///
/// # Errors
///
/// Returns `WrapError::AeadFailed` if the underlying ChaCha20-Poly1305
/// implementation rejects the inputs. Practically unreachable for
/// the fixed shapes we encrypt, but propagated defensively.
pub fn aead_seal(
    key: &[u8; 32],
    nonce: &[u8; AEAD_NONCE_LEN],
    aad_binding: &AadBinding,
    plaintext: &[u8],
) -> Result<Vec<u8>, WrapError> {
    let aad = aad_binding.canonical_bytes();
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|e| WrapError::AeadFailed(format!("chacha20poly1305: {e}")))
}

/// Decrypt-and-verify. AEAD failure is mapped to `UnwrapError::AeadFailed`,
/// which callers treat as the authentication-failure signal.
///
/// # Errors
///
/// Returns `UnwrapError::AeadFailed` for any decrypt-or-verify failure.
/// The error carries no detail — see the type's doc-comment for the
/// constant-time / privacy rationale.
pub fn aead_open(
    key: &[u8; 32],
    nonce: &[u8; AEAD_NONCE_LEN],
    aad_binding: &AadBinding,
    ciphertext: &[u8],
) -> Result<Vec<u8>, UnwrapError> {
    let aad = aad_binding.canonical_bytes();
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| UnwrapError::AeadFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_succeeds() {
        let key = [0x42u8; 32];
        let nonce = [0x01u8; AEAD_NONCE_LEN];
        let aad = AadBinding::for_wrapped_msek(&[0u8; 32], "cred-1");
        let pt = b"hello wrapped world";
        let ct = aead_seal(&key, &nonce, &aad, pt).unwrap();
        assert_eq!(ct.len(), pt.len() + AEAD_TAG_LEN);
        let opened = aead_open(&key, &nonce, &aad, &ct).unwrap();
        assert_eq!(opened, pt);
    }

    #[test]
    fn wrong_key_fails_aead_verify() {
        let key = [0x42u8; 32];
        let other_key = [0x43u8; 32];
        let nonce = [0x01u8; AEAD_NONCE_LEN];
        let aad = AadBinding::for_wrapped_msek(&[0u8; 32], "cred-1");
        let pt = b"secret";
        let ct = aead_seal(&key, &nonce, &aad, pt).unwrap();
        let err = aead_open(&other_key, &nonce, &aad, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::AeadFailed));
    }

    #[test]
    fn tampered_ciphertext_fails_aead_verify() {
        let key = [0x42u8; 32];
        let nonce = [0x01u8; AEAD_NONCE_LEN];
        let aad = AadBinding::for_wrapped_msek(&[0u8; 32], "cred-1");
        let pt = b"secret";
        let mut ct = aead_seal(&key, &nonce, &aad, pt).unwrap();
        ct[0] ^= 0xff;
        let err = aead_open(&key, &nonce, &aad, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::AeadFailed));
    }

    #[test]
    fn cross_kind_aad_substitution_fails_verify() {
        // Sealed as wrapped-msek; opened as mls-snapshot — same actor,
        // but the AAD differs by `kind`. AEAD must fail.
        let key = [0x42u8; 32];
        let nonce = [0x01u8; AEAD_NONCE_LEN];
        let actor = [0u8; 32];
        let seal_aad = AadBinding::for_wrapped_msek(&actor, "cred-1");
        let open_aad = AadBinding::for_mls_snapshot(&actor);
        let ct = aead_seal(&key, &nonce, &seal_aad, b"x").unwrap();
        let err = aead_open(&key, &nonce, &open_aad, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::AeadFailed));
    }

    #[test]
    fn cross_index_aad_substitution_fails_verify() {
        // Same kind, different ix.
        let key = [0x42u8; 32];
        let nonce = [0x01u8; AEAD_NONCE_LEN];
        let aad_a = AadBinding::for_wrapped_msek(&[0xAA; 32], "cred-1");
        let aad_b = AadBinding::for_wrapped_msek(&[0xBB; 32], "cred-1");
        let ct = aead_seal(&key, &nonce, &aad_a, b"x").unwrap();
        let err = aead_open(&key, &nonce, &aad_b, &ct).unwrap_err();
        assert!(matches!(err, UnwrapError::AeadFailed));
    }
}
