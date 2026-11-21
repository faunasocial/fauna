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

/// The derive context of a paged manifest's ROOT seal — distinct from the
/// page / single-manifest context `"fauna.manifest.v1"`, so a root's key and
/// salt never meet a page's (`mls-group-key-material.md` § M2 *Sealed manifest
/// hashes* → *Paged manifests*).
const ROOT_CONTEXT: &str = "fauna.manifest.root.v1";

/// The plaintext payload sealed into a paged root's `sealed_hashes`: the
/// whole-file hash alone (a root lists no chunk hashes — its pages carry them).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SealedRootHash {
    file_hash: ContentHash,
}

/// The root's convergent salt: a BLAKE3 derive-key hash, under the root's own
/// context, over the ordered page hashes — the one identity of the root a
/// reader knows before opening. Domain-separated at the salt itself (not only
/// at the key), so a root's salt — and the nonce cut from it — never equals a
/// page's [`stored_hashes_salt`] even over the same digest sequence.
fn page_hashes_salt(page_hashes: &[ContentHash]) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new_derive_key(ROOT_CONTEXT);
    for h in page_hashes {
        hasher.update(&h.digest());
    }
    hasher.finalize()
}

fn derive_root_key(root_secret: &[u8; 32], salt: &blake3::Hash) -> [u8; 32] {
    crate::domain_key::derive_domain_key(ROOT_CONTEXT, root_secret, salt.as_bytes())
}

/// Seal a paged root's `file_hash` under the set's chunk root, salted by the
/// ordered page hashes the root names. Under a fixed root secret the page
/// hashes determine the file, so the deterministic nonce only ever reproduces
/// the identical blob.
pub fn seal_root_file_hash(
    root_secret: &[u8; 32],
    file_hash: &ContentHash,
    page_hashes: &[ContentHash],
) -> Result<Vec<u8>> {
    let plaintext = crate::encoding::canonical_encode(&SealedRootHash {
        file_hash: *file_hash,
    })
    .context("encoding sealed root hash payload")?;
    let salt = page_hashes_salt(page_hashes);
    let cipher =
        ChaCha20Poly1305::new_from_slice(&derive_root_key(root_secret, &salt)).expect("32 bytes");
    cipher
        .encrypt(&derive_manifest_nonce(&salt), plaintext.as_slice())
        .map_err(|e| anyhow::anyhow!("manifest root sealing failed: {e}"))
}

/// Open a paged root's sealed `file_hash`. `page_hashes` must be the root's
/// plaintext page list, in order: the salt binds the blob to it, so a
/// reordered, dropped or swapped page fails the tag check (fail-closed).
pub fn open_root_file_hash(
    root_secret: &[u8; 32],
    page_hashes: &[ContentHash],
    sealed: &[u8],
) -> Result<ContentHash> {
    let salt = page_hashes_salt(page_hashes);
    let cipher =
        ChaCha20Poly1305::new_from_slice(&derive_root_key(root_secret, &salt)).expect("32 bytes");
    let plaintext = cipher
        .decrypt(&derive_manifest_nonce(&salt), sealed)
        .map_err(|e| anyhow::anyhow!("manifest root unsealing failed: {e}"))?;
    let payload: SealedRootHash = crate::encoding::canonical_decode(&plaintext)
        .context("decoding sealed root hash payload")?;
    Ok(payload.file_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_seal_round_trips_and_binds_the_page_list() {
        let root = [7u8; 32];
        let file_hash = hash_of(b"whole file");
        let pages = vec![hash_of(b"page 0"), hash_of(b"page 1")];
        let sealed = seal_root_file_hash(&root, &file_hash, &pages).unwrap();
        assert_eq!(
            open_root_file_hash(&root, &pages, &sealed).unwrap(),
            file_hash
        );
        // Convergent: identical input, identical blob.
        assert_eq!(
            seal_root_file_hash(&root, &file_hash, &pages).unwrap(),
            sealed
        );
        // Wrong secret, reordered, dropped or swapped page ⇒ fail closed.
        assert!(open_root_file_hash(&[8u8; 32], &pages, &sealed).is_err());
        let reordered = vec![pages[1], pages[0]];
        assert!(open_root_file_hash(&root, &reordered, &sealed).is_err());
        assert!(open_root_file_hash(&root, &pages[..1], &sealed).is_err());
        let swapped = vec![pages[0], hash_of(b"attacker page")];
        assert!(open_root_file_hash(&root, &swapped, &sealed).is_err());
    }

    #[test]
    fn a_root_salt_never_equals_a_page_salt() {
        // Same digest sequence on both sides: the salts, the nonces cut from
        // them and the keys all differ, and neither seal opens as the other.
        let root = [3u8; 32];
        let seq = vec![hash_of(b"a"), hash_of(b"b")];
        let page_salt = stored_hashes_salt(&seq);
        let root_salt = page_hashes_salt(&seq);
        assert_ne!(page_salt, root_salt);
        assert_ne!(
            derive_manifest_nonce(&page_salt),
            derive_manifest_nonce(&root_salt)
        );
        assert_ne!(
            derive_manifest_key(&root, &page_salt),
            derive_root_key(&root, &root_salt)
        );
        assert_ne!(
            derive_manifest_key(&root, &root_salt),
            derive_root_key(&root, &root_salt)
        );
        let file_hash = hash_of(b"f");
        let page_seal = seal_manifest_hashes(&root, &file_hash, &[], &seq).unwrap();
        assert!(open_root_file_hash(&root, &seq, &page_seal).is_err());
        let root_seal = seal_root_file_hash(&root, &file_hash, &seq).unwrap();
        assert!(open_manifest_hashes(&root, &seq, &root_seal).is_err());
    }

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
