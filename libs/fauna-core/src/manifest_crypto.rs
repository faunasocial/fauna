//! Sealing a `ChunkManifest`'s plaintext hashes (`file_hash` + `chunk_hashes`)
//! so the destination holding the manifest cannot use them as a
//! candidate-plaintext confirmation oracle.
//!
//! The destination only ever addresses the blob store by `stored_hashes`
//! (GC / existence-check / restore / forward), so those stay plaintext; the
//! plaintext hashes are needed only by readers that already hold the set's
//! chunk root (the M2 content key or `BackupKey::convergent_chunk_root()`),
//! which is exactly the key this module seals under — no new key category
//! (`docs/goal/architecture/mls-group-key-material.md` § M2, *Sealed manifest
//! hashes*).
//!
//! Convergent by construction, like `chunk_crypto`: the key + nonce derive
//! from the root and the manifest's `stored_hashes` sequence — the one
//! identity of the sealed content a reader knows *before* opening (the
//! plaintext `file_hash` cannot salt the seal: the reader would need it to
//! derive the open key — circular). Under a fixed root, identical
//! `stored_hashes` imply identical chunk ciphertexts, hence identical chunk
//! plaintexts (deterministic AEAD), hence an identical `{file_hash,
//! chunk_hashes}` payload — so deterministic nonce reuse only ever reproduces
//! the identical sealed blob (idempotent retry), and distinct payloads always
//! get a distinct key *and* nonce.

use crate::data::ContentHash;
use anyhow::{Context, Result};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};
use serde::{Deserialize, Serialize};

/// The plaintext payload sealed into `ChunkManifest::sealed_hashes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealedManifestHashes {
    /// The content hash of the entire file (`ChunkManifest::file_hash`).
    pub file_hash: ContentHash,
    /// The plaintext chunk hashes (`ChunkManifest::chunk_hashes`) — the AEAD
    /// key/nonce salts for the set's chunks and the post-decrypt anchors.
    pub chunk_hashes: Vec<ContentHash>,
}

/// The convergent salt: BLAKE3 over the concatenated `stored_hashes` digests.
fn stored_hashes_salt(stored_hashes: &[ContentHash]) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    for h in stored_hashes {
        hasher.update(&h.digest());
    }
    hasher.finalize()
}

/// Derive the per-manifest key: domain-separated from the chunk seal
/// (`"fauna.manifest.v1"` vs `chunk_crypto`'s `"fauna.chunk.v1"`), then salted
/// by the `stored_hashes` identity — the same two-step shape as
/// `chunk_crypto::derive_chunk_key`.
fn derive_manifest_key(root_secret: &[u8; 32], salt: &blake3::Hash) -> [u8; 32] {
    crate::domain_key::derive_domain_key("fauna.manifest.v1", root_secret, salt.as_bytes())
}

fn derive_manifest_nonce(salt: &blake3::Hash) -> Nonce {
    crate::nonce_truncate::nonce_from_digest(salt.as_bytes())
}

/// Seal `{file_hash, chunk_hashes}` under the set's chunk root, salted by the
/// manifest's `stored_hashes`. Returns the AEAD blob for
/// `ChunkManifest::sealed_hashes`.
pub fn seal_manifest_hashes(
    root_secret: &[u8; 32],
    file_hash: &ContentHash,
    chunk_hashes: &[ContentHash],
    stored_hashes: &[ContentHash],
) -> Result<Vec<u8>> {
    let payload = SealedManifestHashes {
        file_hash: *file_hash,
        chunk_hashes: chunk_hashes.to_vec(),
    };
    let plaintext = crate::encoding::canonical_encode(&payload)
        .context("encoding sealed manifest hashes payload")?;
    let salt = stored_hashes_salt(stored_hashes);
    let key = derive_manifest_key(root_secret, &salt);
    let cipher = ChaCha20Poly1305::new_from_slice(&key).expect("32-byte key is valid");
    cipher
        .encrypt(&derive_manifest_nonce(&salt), plaintext.as_slice())
        .map_err(|e| anyhow::anyhow!("manifest hash sealing failed: {e}"))
}

/// Open a `ChunkManifest::sealed_hashes` blob. `stored_hashes` must be the
/// manifest's plaintext `stored_hashes` (the salt binds the blob to them — a
/// tampered store-key list fails the tag check, fail-closed).
pub fn open_manifest_hashes(
    root_secret: &[u8; 32],
    stored_hashes: &[ContentHash],
    sealed: &[u8],
) -> Result<SealedManifestHashes> {
    let salt = stored_hashes_salt(stored_hashes);
    let key = derive_manifest_key(root_secret, &salt);
    let cipher = ChaCha20Poly1305::new_from_slice(&key).expect("32-byte key is valid");
    let plaintext = cipher
        .decrypt(&derive_manifest_nonce(&salt), sealed)
        .map_err(|e| anyhow::anyhow!("manifest hash unsealing failed: {e}"))?;
    crate::encoding::canonical_decode(&plaintext).context("decoding sealed manifest hashes payload")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_of(data: &[u8]) -> ContentHash {
        ContentHash::of_raw(data)
    }

    fn fixture() -> (ContentHash, Vec<ContentHash>, Vec<ContentHash>) {
        let file_hash = hash_of(b"whole file");
        let chunk_hashes = vec![hash_of(b"chunk a"), hash_of(b"chunk b")];
        let stored_hashes = vec![hash_of(b"ct a"), hash_of(b"ct b")];
        (file_hash, chunk_hashes, stored_hashes)
    }

    #[test]
    fn round_trip() {
        let root = [7u8; 32];
        let (file_hash, chunk_hashes, stored_hashes) = fixture();
        let sealed =
            seal_manifest_hashes(&root, &file_hash, &chunk_hashes, &stored_hashes).unwrap();
        let opened = open_manifest_hashes(&root, &stored_hashes, &sealed).unwrap();
        assert_eq!(opened.file_hash, file_hash);
        assert_eq!(opened.chunk_hashes, chunk_hashes);
    }

    #[test]
    fn wrong_root_fails_closed() {
        let (file_hash, chunk_hashes, stored_hashes) = fixture();
        let sealed =
            seal_manifest_hashes(&[1u8; 32], &file_hash, &chunk_hashes, &stored_hashes).unwrap();
        assert!(open_manifest_hashes(&[2u8; 32], &stored_hashes, &sealed).is_err());
    }

    #[test]
    fn tampered_stored_hashes_fail_closed() {
        // The salt binds the sealed blob to the plaintext `stored_hashes` — a
        // destination swapping a store key breaks the open, it cannot splice.
        let root = [7u8; 32];
        let (file_hash, chunk_hashes, stored_hashes) = fixture();
        let sealed =
            seal_manifest_hashes(&root, &file_hash, &chunk_hashes, &stored_hashes).unwrap();
        let mut tampered = stored_hashes.clone();
        tampered[1] = hash_of(b"attacker ct");
        assert!(open_manifest_hashes(&root, &tampered, &sealed).is_err());
    }

    #[test]
    fn convergent_identical_input_identical_blob() {
        // Idempotent retry: re-sealing the same payload under the same root
        // yields byte-identical output (deterministic key + nonce).
        let root = [9u8; 32];
        let (file_hash, chunk_hashes, stored_hashes) = fixture();
        let a = seal_manifest_hashes(&root, &file_hash, &chunk_hashes, &stored_hashes).unwrap();
        let b = seal_manifest_hashes(&root, &file_hash, &chunk_hashes, &stored_hashes).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn distinct_manifests_distinct_key_and_nonce() {
        // Distinct payloads always have distinct `stored_hashes` under a fixed
        // root (distinct chunk plaintext ⇒ distinct convergent ciphertext ⇒
        // distinct store key), so key + nonce never collide across them.
        let salt_a = stored_hashes_salt(&[hash_of(b"ct a")]);
        let salt_b = stored_hashes_salt(&[hash_of(b"ct b")]);
        assert_ne!(
            derive_manifest_key(&[3u8; 32], &salt_a),
            derive_manifest_key(&[3u8; 32], &salt_b)
        );
        assert_ne!(
            derive_manifest_nonce(&salt_a),
            derive_manifest_nonce(&salt_b)
        );
    }

    #[test]
    fn domain_separated_from_chunk_seal() {
        // A manifest-seal key never equals a chunk-seal key for the same salt
        // material: the derive contexts differ.
        let root = [5u8; 32];
        let salt = stored_hashes_salt(&[hash_of(b"x")]);
        let manifest_key = derive_manifest_key(&root, &salt);
        let chunk_key =
            crate::domain_key::derive_domain_key("fauna.chunk.v1", &root, salt.as_bytes());
        assert_ne!(manifest_key, chunk_key);
    }
}
