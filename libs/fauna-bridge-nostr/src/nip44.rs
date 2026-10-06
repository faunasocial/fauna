//! NIP-44 versioned encrypted direct messages (v2).
//!
//! Spec-conformant v2 (audited spec, 2023-12): secp256k1 ECDH x-coordinate →
//! `conversation_key = hkdf_extract(salt: "nip44-v2", ikm: shared_x)` (the PRK
//! itself — no expand step) → per-message
//! `hkdf_expand(prk: conversation_key, info: nonce, 76)` =
//! chacha_key(32) ‖ chacha_nonce(12) ‖ hmac_key(32) → **ChaCha20** (IETF,
//! 12-byte nonce) over the chunk-padded plaintext → HMAC-SHA256 with the nonce
//! as AAD (`hmac(key, nonce ‖ ciphertext)`).
//!
//! Wire format: base64(version(0x02) ‖ nonce(32) ‖ ciphertext ‖ mac(32)).
//!
//! **History (2026-07-20, N2/S6 interop harness):** the original
//! implementation diverged from the spec in three ways (extract+expand for the
//! conversation key, salt/IKM-swapped 88-byte message-key derive feeding
//! XChaCha20, and power-of-two padding), so it round-tripped against itself
//! but rejected every real client's ciphertext — caught the moment a real
//! NIP-46 client (`nostr-connect`) drove the bunker. A decrypt-only legacy
//! fallback for the pre-fix format existed 2026-07-20 → 2026-07-22 and was
//! then **deleted entirely on user ruling** (alpha deletion carve-out: no
//! pre-fix build is a live DM peer). This module is spec-conformant only,
//! both directions.

use anyhow::{Context, Result, bail};
use chacha20::ChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::ecdh::compute_shared_secret;

const VERSION: u8 = 0x02;
/// Spec bounds on the unpadded plaintext length.
const MIN_PLAINTEXT_LEN: usize = 1;
const MAX_PLAINTEXT_LEN: usize = 65535;

/// Conversation key per spec: the HKDF-SHA256 **extract** PRK with salt
/// "nip44-v2" — deliberately no expand step.
fn compute_conversation_key(shared_secret: &[u8; 32]) -> [u8; 32] {
    let (prk, _) = Hkdf::<Sha256>::extract(Some(b"nip44-v2"), shared_secret);
    prk.into()
}

/// Per-message keys per spec: `hkdf_expand(prk: conversation_key,
/// info: nonce, 76)` = chacha_key(32) ‖ chacha_nonce(12) ‖ hmac_key(32).
fn derive_message_keys(
    conversation_key: &[u8; 32],
    nonce: &[u8; 32],
) -> Result<([u8; 32], [u8; 12], [u8; 32])> {
    let hk = Hkdf::<Sha256>::from_prk(conversation_key)
        .map_err(|e| anyhow::anyhow!("HKDF from_prk failed: {e}"))?;
    let mut keys = [0u8; 76];
    hk.expand(nonce, &mut keys)
        .map_err(|e| anyhow::anyhow!("HKDF expand for message keys failed: {e}"))?;

    let mut chacha_key = [0u8; 32];
    chacha_key.copy_from_slice(&keys[..32]);

    let mut chacha_nonce = [0u8; 12];
    chacha_nonce.copy_from_slice(&keys[32..44]);

    let mut hmac_key = [0u8; 32];
    hmac_key.copy_from_slice(&keys[44..76]);

    Ok((chacha_key, chacha_nonce, hmac_key))
}

/// Spec `calc_padded_len`: 32-byte floor, then chunked — chunk 32 up to a
/// 256-byte next-power-of-two, `next_power / 8` above that.
fn calc_padded_len(unpadded_len: usize) -> usize {
    if unpadded_len <= 32 {
        return 32;
    }
    let next_power = usize::pow(2, (unpadded_len as f64 - 1.0).log2().floor() as u32 + 1);
    let chunk = if next_power <= 256 {
        32
    } else {
        next_power / 8
    };
    chunk * ((unpadded_len - 1) / chunk + 1)
}

/// Pad plaintext per spec: [u16 big-endian unpadded length] [plaintext]
/// [zeros to `calc_padded_len`].
fn pad_plaintext(plaintext: &[u8]) -> Vec<u8> {
    let padded_len = calc_padded_len(plaintext.len());
    let mut padded = vec![0u8; 2 + padded_len];
    let len = plaintext.len() as u16;
    padded[0] = (len >> 8) as u8;
    padded[1] = (len & 0xff) as u8;
    padded[2..2 + plaintext.len()].copy_from_slice(plaintext);
    padded
}

/// Remove padding and extract original plaintext, enforcing the spec's exact
/// padded length (a wrong-length padding is a malformed message, not slack).
fn unpad_plaintext(padded: &[u8]) -> Result<Vec<u8>> {
    if padded.len() < 2 {
        bail!("padded data too short");
    }
    let len = ((padded[0] as usize) << 8) | (padded[1] as usize);
    if len < MIN_PLAINTEXT_LEN || len + 2 > padded.len() || padded.len() != 2 + calc_padded_len(len)
    {
        bail!(
            "invalid padding: declared length {len} vs padded data ({})",
            padded.len()
        );
    }
    Ok(padded[2..2 + len].to_vec())
}

/// Encrypt a message for a recipient using NIP-44 v2.
///
/// Returns a base64-encoded payload containing:
/// version(1) || nonce(32) || ciphertext || mac(32)
pub fn nip44_encrypt(
    our_secret: &[u8; 32],
    their_pubkey: &[u8; 32],
    plaintext: &str,
) -> Result<String> {
    if plaintext.is_empty() {
        bail!("plaintext must not be empty");
    }
    if plaintext.len() > MAX_PLAINTEXT_LEN {
        bail!("plaintext exceeds NIP-44 maximum ({MAX_PLAINTEXT_LEN} bytes)");
    }

    let shared = compute_shared_secret(our_secret, their_pubkey)?;
    let conversation_key = compute_conversation_key(&shared);

    // Generate random 32-byte nonce
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut nonce).context("generate nonce")?;

    encrypt_with_nonce(&conversation_key, &nonce, plaintext)
}

/// Internal encrypt with explicit nonce (useful for testing determinism).
fn encrypt_with_nonce(
    conversation_key: &[u8; 32],
    nonce: &[u8; 32],
    plaintext: &str,
) -> Result<String> {
    let (chacha_key, chacha_nonce, hmac_key) = derive_message_keys(conversation_key, nonce)?;

    // Pad the plaintext
    let mut padded = pad_plaintext(plaintext.as_bytes());

    // Encrypt in-place with ChaCha20 (IETF, 12-byte nonce)
    let mut cipher = ChaCha20::new(chacha_key.as_ref().into(), chacha_nonce.as_ref().into());
    cipher.apply_keystream(&mut padded);
    let ciphertext = padded;

    // HMAC-SHA256 over nonce + ciphertext (the nonce is the spec's AAD)
    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(&hmac_key).map_err(|e| anyhow::anyhow!("HMAC init: {e}"))?;
    mac.update(nonce);
    mac.update(&ciphertext);
    let mac_bytes = mac.finalize().into_bytes();

    // Assemble: version || nonce || ciphertext || mac
    let mut payload = Vec::with_capacity(1 + 32 + ciphertext.len() + 32);
    payload.push(VERSION);
    payload.extend_from_slice(nonce);
    payload.extend_from_slice(&ciphertext);
    payload.extend_from_slice(&mac_bytes);

    // Base64 encode
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(&payload))
}

/// Decrypt a NIP-44 v2 message.
///
/// Expects a base64-encoded payload: version(1) || nonce(32) || ciphertext || mac(32).
/// Spec-conformant format only (module doc § History — the pre-fix legacy
/// fallback was deleted 2026-07-22 on user ruling).
pub fn nip44_decrypt(
    our_secret: &[u8; 32],
    their_pubkey: &[u8; 32],
    payload: &str,
) -> Result<String> {
    let shared = compute_shared_secret(our_secret, their_pubkey)?;
    let conversation_key = compute_conversation_key(&shared);
    decrypt_with_conversation_key(&conversation_key, payload)
}

/// Internal decrypt using a conversation key directly.
fn decrypt_with_conversation_key(conversation_key: &[u8; 32], payload: &str) -> Result<String> {
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .context("invalid base64 payload")?;

    // Minimum: version(1) + nonce(32) + min_padded(2 + 32) + mac(32) = 99
    if raw.len() < 99 {
        bail!("payload too short: {} bytes", raw.len());
    }

    let version = raw[0];
    if version != VERSION {
        bail!("unsupported NIP-44 version: {version:#04x}");
    }

    let nonce: [u8; 32] = raw[1..33].try_into().unwrap();
    let mac_start = raw.len() - 32;
    let ciphertext = &raw[33..mac_start];
    let received_mac = &raw[mac_start..];

    let (chacha_key, chacha_nonce, hmac_key) = derive_message_keys(conversation_key, &nonce)?;

    // Verify HMAC first (before decryption)
    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(&hmac_key).map_err(|e| anyhow::anyhow!("HMAC init: {e}"))?;
    mac.update(&nonce);
    mac.update(ciphertext);
    mac.verify_slice(received_mac)
        .map_err(|_| anyhow::anyhow!("HMAC verification failed"))?;

    // Decrypt with ChaCha20
    let mut buffer = ciphertext.to_vec();
    let mut cipher = ChaCha20::new(chacha_key.as_ref().into(), chacha_nonce.as_ref().into());
    cipher.apply_keystream(&mut buffer);

    // Unpad to recover original plaintext
    let plaintext_bytes = unpad_plaintext(&buffer)?;
    String::from_utf8(plaintext_bytes).context("decrypted text is not valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::Keypair;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let sender = Keypair::generate();
        let receiver = Keypair::generate();

        let plaintext = "hello";
        let encrypted = nip44_encrypt(
            &sender.secret_bytes(),
            &receiver.public_key_bytes(),
            plaintext,
        )
        .unwrap();

        let decrypted = nip44_decrypt(
            &receiver.secret_bytes(),
            &sender.public_key_bytes(),
            &encrypted,
        )
        .unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let sender = Keypair::generate();
        let receiver = Keypair::generate();
        let wrong = Keypair::generate();

        let encrypted = nip44_encrypt(
            &sender.secret_bytes(),
            &receiver.public_key_bytes(),
            "secret message",
        )
        .unwrap();

        // Decrypting with the wrong key should fail HMAC verification
        let result = nip44_decrypt(
            &wrong.secret_bytes(),
            &sender.public_key_bytes(),
            &encrypted,
        );
        assert!(result.is_err(), "decryption with wrong key should fail");
    }

    #[test]
    fn padding_follows_spec_table() {
        // (unpadded_len, calc_padded_len) rows from the NIP-44 spec's padding
        // scheme; the padded buffer is 2 bytes (length prefix) longer.
        for (unpadded, padded) in [
            (1usize, 32usize),
            (2, 32),
            (16, 32),
            (32, 32),
            (33, 64),
            (37, 64),
            (45, 64),
            (49, 64),
            (64, 64),
            (65, 96),
            (100, 128),
            (111, 128),
            (200, 224),
            (250, 256),
            (320, 320),
            (383, 384),
            (384, 384),
            (400, 448),
            (500, 512),
            (512, 512),
            (515, 640),
            (700, 768),
            (800, 896),
            (900, 1024),
            (1020, 1024),
            (65536 / 2, 32768),
        ] {
            assert_eq!(
                calc_padded_len(unpadded),
                padded,
                "calc_padded_len({unpadded})"
            );
            let msg = vec![b'x'; unpadded];
            assert_eq!(pad_plaintext(&msg).len(), 2 + padded, "pad({unpadded})");
        }
    }

    #[test]
    fn unpad_recovers_original() {
        let originals: &[&[u8]] = &[b"hi", b"hello world", &[b'z'; 100], b"x"];
        for &original in originals {
            let padded = pad_plaintext(original);
            let recovered = unpad_plaintext(&padded).unwrap();
            assert_eq!(recovered, original);
        }
    }

    #[test]
    fn shared_secret_is_symmetric() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();

        let s1 = compute_shared_secret(&kp1.secret_bytes(), &kp2.public_key_bytes()).unwrap();
        let s2 = compute_shared_secret(&kp2.secret_bytes(), &kp1.public_key_bytes()).unwrap();
        assert_eq!(s1, s2);

        // Also verify conversation keys are symmetric
        let ck1 = compute_conversation_key(&s1);
        let ck2 = compute_conversation_key(&s2);
        assert_eq!(ck1, ck2);
    }

    #[test]
    fn output_is_base64() {
        let sender = Keypair::generate();
        let receiver = Keypair::generate();

        let encrypted = nip44_encrypt(
            &sender.secret_bytes(),
            &receiver.public_key_bytes(),
            "test base64 output",
        )
        .unwrap();

        // Verify it's valid base64
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD.decode(&encrypted);
        assert!(decoded.is_ok(), "output should be valid base64");
    }

    #[test]
    fn version_byte_is_0x02() {
        let sender = Keypair::generate();
        let receiver = Keypair::generate();

        let encrypted = nip44_encrypt(
            &sender.secret_bytes(),
            &receiver.public_key_bytes(),
            "version check",
        )
        .unwrap();

        use base64::Engine;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&encrypted)
            .unwrap();
        assert_eq!(raw[0], 0x02, "first byte should be version 0x02");
    }

    #[test]
    fn encrypt_decrypt_long_message() {
        let sender = Keypair::generate();
        let receiver = Keypair::generate();

        let plaintext = "a]".repeat(500); // 1000 bytes
        let encrypted = nip44_encrypt(
            &sender.secret_bytes(),
            &receiver.public_key_bytes(),
            &plaintext,
        )
        .unwrap();

        let decrypted = nip44_decrypt(
            &receiver.secret_bytes(),
            &sender.public_key_bytes(),
            &encrypted,
        )
        .unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn sender_can_decrypt_own_message() {
        // Both parties can derive the same conversation key, so the sender
        // should also be able to decrypt.
        let sender = Keypair::generate();
        let receiver = Keypair::generate();

        let encrypted = nip44_encrypt(
            &sender.secret_bytes(),
            &receiver.public_key_bytes(),
            "self decrypt",
        )
        .unwrap();

        let decrypted = nip44_decrypt(
            &sender.secret_bytes(),
            &receiver.public_key_bytes(),
            &encrypted,
        )
        .unwrap();

        assert_eq!(decrypted, "self decrypt");
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let sender = Keypair::generate();
        let receiver = Keypair::generate();

        let encrypted = nip44_encrypt(
            &sender.secret_bytes(),
            &receiver.public_key_bytes(),
            "do not tamper",
        )
        .unwrap();

        // Decode, flip a ciphertext byte, re-encode
        use base64::Engine;
        let mut raw = base64::engine::general_purpose::STANDARD
            .decode(&encrypted)
            .unwrap();
        // Flip a byte in the ciphertext area (after version + nonce, before mac)
        raw[40] ^= 0xff;
        let tampered = base64::engine::general_purpose::STANDARD.encode(&raw);

        let result = nip44_decrypt(
            &receiver.secret_bytes(),
            &sender.public_key_bytes(),
            &tampered,
        );
        assert!(result.is_err(), "tampered ciphertext should fail HMAC");
    }

    #[test]
    fn empty_plaintext_rejected() {
        let sender = Keypair::generate();
        let receiver = Keypair::generate();

        let result = nip44_encrypt(&sender.secret_bytes(), &receiver.public_key_bytes(), "");
        assert!(result.is_err(), "empty plaintext should be rejected");
    }
}
