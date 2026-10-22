//! The one AEAD shape every passphrase-rooted client store seals with:
//! ChaCha20-Poly1305 under a 32-byte key, a fresh random 12-byte nonce per
//! seal, and caller-supplied associated data that names what the value is and
//! where it rests (a versioned, NUL-terminated context string plus the slot it
//! belongs to — `key-material-hierarchy.md` § Architectural rules, rule 3).
//!
//! The terminal app's sealed file (`fauna-credential-store::sealed`) and the
//! web app's sealed identity store (`fauna-client-accounts::sealed_store`)
//! both seal through here, so the AEAD, its nonce discipline and its
//! salt length cannot drift between the two (priority #2). The passphrase
//! half — Argon2id and its parameter envelope — is [`crate::kdf`]'s.
//!
//! [`seal_token`] / [`open_token`] are the string form for stores whose rows
//! are strings (every `SecretStore`): `nonce || ciphertext+tag`, base64url
//! without padding.

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;
use zeroize::Zeroizing;

/// The ChaCha20-Poly1305 nonce length.
pub const NONCE_LEN: usize = 12;
/// The random salt length every passphrase-derived key in a client store is
/// derived under ([`crate::kdf::derive_key_argon2id`]).
pub const SALT_LEN: usize = 16;

/// A fresh random nonce.
#[must_use]
pub fn fresh_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce);
    nonce
}

/// A fresh random Argon2id salt.
#[must_use]
pub fn fresh_salt() -> [u8; SALT_LEN] {
    let mut salt = [0u8; SALT_LEN];
    rand::thread_rng().fill_bytes(&mut salt);
    salt
}

/// Seal `plaintext` under `key` with `nonce`, binding `aad`.
#[must_use]
pub fn seal_with_nonce(
    key: &[u8; 32],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    plaintext: &[u8],
) -> Vec<u8> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("ChaCha20-Poly1305 encryption of an in-memory buffer cannot fail")
}

/// Open `ciphertext` (with its tag) under `key` and `nonce`, checking `aad`.
/// `None` is every failure — a wrong key, a tampered value and a value moved
/// to another slot are one failure by construction.
#[must_use]
pub fn open_with_nonce(
    key: &[u8; 32],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    ciphertext: &[u8],
) -> Option<Zeroizing<Vec<u8>>> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .ok()
        .map(Zeroizing::new)
}

/// Seal `plaintext` under a fresh nonce into the string token
/// `base64url(nonce || ciphertext+tag)`.
#[must_use]
pub fn seal_token(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> String {
    let nonce = fresh_nonce();
    let mut out = nonce.to_vec();
    out.extend_from_slice(&seal_with_nonce(key, &nonce, aad, plaintext));
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out)
}

/// Open a [`seal_token`] token. `None` on a malformed token or any AEAD
/// failure.
#[must_use]
pub fn open_token(key: &[u8; 32], aad: &[u8], token: &str) -> Option<Zeroizing<Vec<u8>>> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .ok()?;
    if bytes.len() < NONCE_LEN {
        return None;
    }
    let (nonce, ciphertext) = bytes.split_at(NONCE_LEN);
    open_with_nonce(key, nonce.try_into().ok()?, aad, ciphertext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_round_trips_and_binds_key_and_aad() {
        let key = [7u8; 32];
        let token = seal_token(&key, b"ctx\0slot-a", b"secret");
        assert_eq!(
            open_token(&key, b"ctx\0slot-a", &token)
                .as_deref()
                .map(Vec::as_slice),
            Some(&b"secret"[..])
        );
        assert!(open_token(&[8u8; 32], b"ctx\0slot-a", &token).is_none());
        assert!(open_token(&key, b"ctx\0slot-b", &token).is_none());
        assert!(open_token(&key, b"ctx\0slot-a", "not base64!").is_none());
        assert!(open_token(&key, b"ctx\0slot-a", "AAAA").is_none());
    }

    #[test]
    fn every_seal_takes_a_fresh_nonce() {
        let key = [7u8; 32];
        assert_ne!(seal_token(&key, b"a", b"x"), seal_token(&key, b"a", b"x"));
    }
}
