//! NIP-04 encrypted direct messages.
//!
//! Spec-conformant: secp256k1 ECDH shared secret (the raw x-coordinate,
//! deliberately unhashed — NIP-04's defining quirk) as the AES-256 key,
//! AES-256-CBC with PKCS#7 padding and a random 16-byte IV.
//!
//! Format: base64(ciphertext) + "?iv=" + base64(iv)
//!
//! NOTE: NIP-04 is deprecated in favor of NIP-44, but legacy NIP-46 clients
//! still commonly use it — the bunker answers in the scheme the request used,
//! so this must interoperate with real clients, not just round-trip.
//!
//! **History (2026-07-20, S6 interop harness):** the original module was an
//! explicitly-labeled SHA-256-XOR stand-in ("works for NIP-46 where we
//! control both ends") — the exact both-ends-ours assumption the harness
//! exists to break; the cross-impl pin in `nostr_relay_interop.rs` failed the
//! moment a real implementation's ciphertext hit it. It was wire-only (only
//! `nip46` calls it) and never interoperated, so it is replaced outright with
//! no legacy fallback — there is no at-rest or peer data in the old format.

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use anyhow::{Context, Result};

use crate::ecdh::compute_shared_secret;
use crate::signing::Keypair;

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

/// Encrypt a plaintext message using NIP-04 (ECDH + AES-256-CBC).
///
/// Returns the NIP-04 formatted string: base64(ciphertext) + "?iv=" + base64(iv)
pub fn encrypt(our_keypair: &Keypair, their_pubkey: &[u8; 32], plaintext: &str) -> Result<String> {
    let shared = compute_shared_secret(&our_keypair.secret_bytes(), their_pubkey)?;

    let mut iv = [0u8; 16];
    getrandom::fill(&mut iv).context("generate IV")?;

    let ciphertext = Aes256CbcEnc::new(shared.as_ref().into(), iv.as_ref().into())
        .encrypt_padded_vec_mut::<Pkcs7>(plaintext.as_bytes());

    let ct_b64 = base64_encode(&ciphertext);
    let iv_b64 = base64_encode(&iv);
    Ok(format!("{ct_b64}?iv={iv_b64}"))
}

/// Decrypt a NIP-04 formatted message.
pub fn decrypt(
    our_keypair: &Keypair,
    their_pubkey: &[u8; 32],
    nip04_message: &str,
) -> Result<String> {
    let (ct_b64, iv_b64) = nip04_message
        .split_once("?iv=")
        .context("invalid NIP-04 format: missing ?iv=")?;

    let ciphertext = base64_decode(ct_b64)?;
    let iv = base64_decode(iv_b64)?;
    if iv.len() != 16 {
        anyhow::bail!("invalid IV length: expected 16, got {}", iv.len());
    }
    let mut iv_arr = [0u8; 16];
    iv_arr.copy_from_slice(&iv);

    let shared = compute_shared_secret(&our_keypair.secret_bytes(), their_pubkey)?;

    let plaintext = Aes256CbcDec::new(shared.as_ref().into(), iv_arr.as_ref().into())
        .decrypt_padded_vec_mut::<Pkcs7>(&ciphertext)
        .map_err(|_| anyhow::anyhow!("AES-CBC decrypt/unpad failed"))?;

    String::from_utf8(plaintext).context("decrypted text is not valid UTF-8")
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .context("invalid base64")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let sender = Keypair::generate();
        let receiver = Keypair::generate();

        let plaintext = "hello NIP-46 bunker";
        let encrypted = encrypt(&sender, &receiver.public_key_bytes(), plaintext).unwrap();

        // Verify format
        assert!(encrypted.contains("?iv="));

        let decrypted = decrypt(&receiver, &sender.public_key_bytes(), &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypt_long_message() {
        let sender = Keypair::generate();
        let receiver = Keypair::generate();

        let plaintext = "a".repeat(200); // longer than one AES block
        let encrypted = encrypt(&sender, &receiver.public_key_bytes(), &plaintext).unwrap();
        let decrypted = decrypt(&receiver, &sender.public_key_bytes(), &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn ciphertext_is_block_padded() {
        let sender = Keypair::generate();
        let receiver = Keypair::generate();
        // 16 bytes of plaintext → PKCS#7 pads to a full extra block (32).
        let encrypted = encrypt(&sender, &receiver.public_key_bytes(), "0123456789abcdef").unwrap();
        let (ct_b64, _) = encrypted.split_once("?iv=").unwrap();
        assert_eq!(base64_decode(ct_b64).unwrap().len(), 32);
    }

    #[test]
    fn wrong_key_fails() {
        let sender = Keypair::generate();
        let receiver = Keypair::generate();
        let wrong = Keypair::generate();

        let encrypted = encrypt(&sender, &receiver.public_key_bytes(), "secret").unwrap();
        let decrypted = decrypt(&wrong, &sender.public_key_bytes(), &encrypted);
        // With a wrong key, PKCS#7 unpadding almost always fails; on the rare
        // valid-padding collision the plaintext is still garbage.
        if let Ok(s) = decrypted {
            assert_ne!(s, "secret");
        }
    }

    #[test]
    fn shared_secret_is_symmetric() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let s1 = compute_shared_secret(&kp1.secret_bytes(), &kp2.public_key_bytes()).unwrap();
        let s2 = compute_shared_secret(&kp2.secret_bytes(), &kp1.public_key_bytes()).unwrap();
        assert_eq!(s1, s2);
    }
}
