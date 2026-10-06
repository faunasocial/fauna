//! The one-step nonce derivation shared by every `derive_*_nonce` in this
//! crate: truncate an already content-derived, per-plaintext-unique 32-byte
//! digest to its first 12 bytes for a ChaCha20-Poly1305 nonce. Each caller
//! (`manifest_crypto::derive_manifest_nonce`, `path_crypto::derive_label_nonce`,
//! `chunk_crypto::derive_chunk_nonce`) owns its own digest source and the
//! reasoning for why that digest is safe to reuse as a nonce; this module
//! only owns the truncation.

use chacha20poly1305::Nonce;

pub(crate) fn nonce_from_digest(digest: &[u8; 32]) -> Nonce {
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&digest[..12]);
    Nonce::from(nonce)
}
