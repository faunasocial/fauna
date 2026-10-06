//! Encryption/decryption of Nostr private keys at rest.
//!
//! Uses ChaCha20-Poly1305 (via fauna_core::crypto) with a key derived from
//! the nest's Ed25519 signing key seed, domain-separated from backup keys.

use anyhow::Result;

/// Encrypt a bunker signer private key for storage in the database.
pub fn encrypt_bunker_signer_privkey(
    nest_signing_key_bytes: &[u8; 32],
    privkey: &[u8; 32],
) -> Result<Vec<u8>> {
    crate::nest_kek::wrap_32(
        crate::nest_kek::BUNKER_SIGNER_CONTEXT,
        nest_signing_key_bytes,
        privkey,
    )
}

/// Decrypt a bunker signer private key from the database.
pub fn decrypt_bunker_signer_privkey(
    nest_signing_key_bytes: &[u8; 32],
    ciphertext: &[u8],
) -> Result<[u8; 32]> {
    crate::nest_kek::unwrap_32(
        crate::nest_kek::BUNKER_SIGNER_CONTEXT,
        nest_signing_key_bytes,
        ciphertext,
    )
}

/// Encrypt a Nostr secp256k1 private key for storage in the database.
pub fn encrypt_nostr_privkey(
    nest_signing_key_bytes: &[u8; 32],
    privkey: &[u8; 32],
) -> Result<Vec<u8>> {
    crate::nest_kek::wrap_32(
        crate::nest_kek::NOSTR_NSEC_CONTEXT,
        nest_signing_key_bytes,
        privkey,
    )
}

/// Decrypt a Nostr secp256k1 private key from the database.
pub fn decrypt_nostr_privkey(
    nest_signing_key_bytes: &[u8; 32],
    ciphertext: &[u8],
) -> Result<[u8; 32]> {
    crate::nest_kek::unwrap_32(
        crate::nest_kek::NOSTR_NSEC_CONTEXT,
        nest_signing_key_bytes,
        ciphertext,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let nest_key = [42u8; 32];
        let privkey = [7u8; 32];
        let ciphertext = encrypt_nostr_privkey(&nest_key, &privkey).unwrap();
        assert_ne!(ciphertext.as_slice(), privkey.as_slice()); // not plaintext
        let decrypted = decrypt_nostr_privkey(&nest_key, &ciphertext).unwrap();
        assert_eq!(decrypted, privkey);
    }

    #[test]
    fn wrong_nest_key_fails() {
        let nest_key = [42u8; 32];
        let wrong_key = [99u8; 32];
        let privkey = [7u8; 32];
        let ciphertext = encrypt_nostr_privkey(&nest_key, &privkey).unwrap();
        assert!(decrypt_nostr_privkey(&wrong_key, &ciphertext).is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let nest_key = [42u8; 32];
        let privkey = [7u8; 32];
        let mut ciphertext = encrypt_nostr_privkey(&nest_key, &privkey).unwrap();
        // Flip a byte in the ciphertext
        if let Some(byte) = ciphertext.last_mut() {
            *byte ^= 0xff;
        }
        assert!(decrypt_nostr_privkey(&nest_key, &ciphertext).is_err());
    }

    #[test]
    fn different_privkeys_produce_different_ciphertexts() {
        let nest_key = [42u8; 32];
        let ct1 = encrypt_nostr_privkey(&nest_key, &[1u8; 32]).unwrap();
        let ct2 = encrypt_nostr_privkey(&nest_key, &[2u8; 32]).unwrap();
        assert_ne!(ct1, ct2);
    }

    #[test]
    fn bunker_signer_roundtrip() {
        let nest_key = [42u8; 32];
        let privkey = [7u8; 32];
        let ciphertext = encrypt_bunker_signer_privkey(&nest_key, &privkey).unwrap();
        let decrypted = decrypt_bunker_signer_privkey(&nest_key, &ciphertext).unwrap();
        assert_eq!(decrypted, privkey);
    }

    #[test]
    fn bunker_signer_context_is_domain_separated() {
        // Ciphertext under one context must not open under the other.
        let nest_key = [42u8; 32];
        let privkey = [7u8; 32];
        let nsec_ct = encrypt_nostr_privkey(&nest_key, &privkey).unwrap();
        assert!(decrypt_bunker_signer_privkey(&nest_key, &nsec_ct).is_err());
        let signer_ct = encrypt_bunker_signer_privkey(&nest_key, &privkey).unwrap();
        assert!(decrypt_nostr_privkey(&nest_key, &signer_ct).is_err());
    }
}
