//! The two-step BLAKE3 domain-separated key derivation shared by every
//! `derive_*_key` in this crate: `derive_key(context, root_secret)` isolates
//! the root under a fixed, hardcoded context literal (so two callers with
//! different contexts never collide even on the same root), then
//! `keyed_hash(intermediate, salt)` folds in the per-item salt. Each caller
//! (`manifest_crypto::derive_manifest_key`, `chunk_crypto::derive_chunk_key`,
//! `path_crypto::derive_label_key`, `subscription::crypto::derive_post_key`,
//! `subscription::crypto::derive_web_render_key`) keeps its own context
//! literal and salt source — only this two-BLAKE3-call shape is shared.

pub(crate) fn derive_domain_key(context: &str, root_secret: &[u8; 32], salt: &[u8]) -> [u8; 32] {
    let intermediate = blake3::derive_key(context, root_secret);
    *blake3::keyed_hash(&intermediate, salt).as_bytes()
}
