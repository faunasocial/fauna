//! The x-coordinate-only ECDH shared secret NIP-04 and NIP-44 both derive
//! their message keys from — the one step the two schemes share before
//! diverging (NIP-04 uses it as a raw AES-256 key; NIP-44 HKDF-extracts it
//! into a conversation key).

use anyhow::Result;
use secp256k1::{PublicKey, SecretKey};

/// Compute the ECDH shared secret (x-coordinate only) from our secret key
/// and their x-only public key, assuming even y — the convention both
/// NIP-04 and NIP-44 use for x-only pubkeys.
pub(crate) fn compute_shared_secret(
    our_secret: &[u8; 32],
    their_pubkey: &[u8; 32],
) -> Result<[u8; 32]> {
    let sk = SecretKey::from_slice(our_secret)?;

    // Convert x-only pubkey to full compressed pubkey (assume even y)
    let mut compressed = [0u8; 33];
    compressed[0] = 0x02;
    compressed[1..].copy_from_slice(their_pubkey);
    let pk = PublicKey::from_slice(&compressed)?;

    let shared_point = secp256k1::ecdh::shared_secret_point(&pk, &sk);
    let mut shared = [0u8; 32];
    shared.copy_from_slice(&shared_point[..32]);
    Ok(shared)
}
