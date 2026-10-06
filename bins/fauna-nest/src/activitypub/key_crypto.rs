//! Encryption/decryption of RSA private keys at rest.
//!
//! Uses ChaCha20-Poly1305 (via fauna_core::crypto) with a key derived from
//! the nest's Ed25519 signing key seed, domain-separated from backup keys
//! and Nostr key encryption keys.

use anyhow::Result;
use fauna_core::crypto::BackupKey;

/// Derive a symmetric encryption key for ActivityPub RSA private keys from the
/// nest's Ed25519 signing key bytes. Uses BLAKE3 key derivation with a unique
/// context string so this key is cryptographically independent from all other
/// derived keys.
fn derive_ap_key_encryption_key(nest_signing_key_bytes: &[u8; 32]) -> BackupKey {
    crate::nest_kek::derive(
        crate::nest_kek::ACTIVITYPUB_RSA_CONTEXT,
        nest_signing_key_bytes,
    )
}

/// Encrypt an RSA private key (DER-encoded) for storage in the database.
pub fn encrypt_rsa_privkey(
    nest_signing_key_bytes: &[u8; 32],
    privkey_der: &[u8],
) -> Result<Vec<u8>> {
    let encryption_key = derive_ap_key_encryption_key(nest_signing_key_bytes);
    fauna_core::crypto::encrypt_backup_chunk(&encryption_key, privkey_der)
}

/// Decrypt an RSA private key (DER-encoded) from the database.
pub fn decrypt_rsa_privkey(
    nest_signing_key_bytes: &[u8; 32],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    let encryption_key = derive_ap_key_encryption_key(nest_signing_key_bytes);
    fauna_core::crypto::decrypt_backup_chunk(&encryption_key, ciphertext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let nest_key = [42u8; 32];
        let privkey = vec![7u8; 256];
        let ciphertext = encrypt_rsa_privkey(&nest_key, &privkey).unwrap();
        assert_ne!(ciphertext.as_slice(), privkey.as_slice());
        let decrypted = decrypt_rsa_privkey(&nest_key, &ciphertext).unwrap();
        assert_eq!(decrypted, privkey);
    }

    #[test]
    fn wrong_nest_key_fails() {
        let nest_key = [42u8; 32];
        let wrong_key = [99u8; 32];
        let privkey = vec![7u8; 256];
        let ciphertext = encrypt_rsa_privkey(&nest_key, &privkey).unwrap();
        assert!(decrypt_rsa_privkey(&wrong_key, &ciphertext).is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let nest_key = [42u8; 32];
        let privkey = vec![7u8; 256];
        let mut ciphertext = encrypt_rsa_privkey(&nest_key, &privkey).unwrap();
        if let Some(byte) = ciphertext.last_mut() {
            *byte ^= 0xff;
        }
        assert!(decrypt_rsa_privkey(&nest_key, &ciphertext).is_err());
    }

    #[test]
    fn different_privkeys_produce_different_ciphertexts() {
        let nest_key = [42u8; 32];
        let ct1 = encrypt_rsa_privkey(&nest_key, &[1u8; 256]).unwrap();
        let ct2 = encrypt_rsa_privkey(&nest_key, &[2u8; 256]).unwrap();
        assert_ne!(ct1, ct2);
    }
}
