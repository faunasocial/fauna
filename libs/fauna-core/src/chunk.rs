//! Chunk manifests, manifest diffs, and file version history types.
//!
//! These types support content-addressed chunked file storage,
//! enabling efficient deduplication and delta syncing.

use serde::{Deserialize, Serialize};

use crate::data::{ContentHash, Timestamp};

/// An ordered list of chunk hashes forming a complete file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkManifest {
    /// The content hash of the entire file.
    pub file_hash: ContentHash,
    /// Total size of the file in bytes.
    pub total_size: u64,
    /// Ordered list of chunk content hashes — the BLAKE3 of each chunk's
    /// **plaintext**. These are the file's content identity (diff/dedup) and,
    /// for a client-side-encrypted set, the AEAD key/nonce salt
    /// (`chunk_crypto::{encrypt,decrypt}_chunk` derive off the plaintext hash) and
    /// the post-decrypt integrity anchor.
    pub chunk_hashes: Vec<ContentHash>,
    /// Size of each chunk in bytes (parallel to `chunk_hashes`).
    pub chunk_sizes: Vec<u64>,
    /// The content-addressed **storage key** of each chunk — the BLAKE3 of the
    /// bytes actually uploaded to the blob store (`check_chunks` / `POST` /
    /// `download_chunk` all use this), parallel to `chunk_hashes`.
    ///
    /// `None` ⇒ the stored bytes *are* the (possibly compressed) plaintext, so the
    /// store key equals `chunk_hashes[i]` — the unencrypted path, and every
    /// manifest written before this field existed.
    ///
    /// `Some` ⇒ each chunk body was client-side-encrypted under a per-set content
    /// key (shared folders, M2), so the store key is the **ciphertext** hash
    /// while `chunk_hashes[i]` stays the plaintext hash. Keying the store by the
    /// ciphertext hash is what lets an AEAD body satisfy the nest's F9
    /// anti-poisoning check (`chunk_routes::resolve_verified_chunk_hash`:
    /// `blake3(body) == X-Content-Hash`) with **no route change** — the route stays
    /// a flat, content-addressed store; the group binding lives in this manifest
    /// (fetched by its own content hash from the group-gated change log) and the
    /// AEAD seal. See `docs/goal/architecture/mls-group-key-material.md` § M2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_hashes: Option<Vec<ContentHash>>,
    /// Sealed `{file_hash, chunk_hashes}` (`manifest_crypto`) — the closure of
    /// the destination's plaintext-hash confirmation oracle. Present on exactly the manifests
    /// that carry [`Self::stored_hashes`] (a sealed-chunk upload), whose
    /// plaintext `file_hash`/`chunk_hashes` are then blanked
    /// ([`Self::seal_hashes`]); readers call [`Self::unseal_hashes`] before
    /// using them, and [`Self::check_hash_shape`] refuses any other pairing.
    /// See `docs/goal/architecture/mls-group-key-material.md` § M2 *Sealed
    /// manifest hashes*.
    #[serde(default, with = "serde_bytes", skip_serializing_if = "Option::is_none")]
    pub sealed_hashes: Option<Vec<u8>>,
    /// The oldest manifest reader version that can operate this manifest
    /// (`None` = 1, a plaintext manifest). Mirrors the segment-store
    /// `(format_version, min_reader)` scheme: sealed-only manifests stamp
    /// `Some(2)` so a too-old reader gets the honest
    /// [`Self::check_min_reader`] error instead of a hash-mismatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_reader: Option<u32>,
}

/// The newest `ChunkManifest::min_reader` this binary can operate: 2 = it can
/// open `sealed_hashes` manifests (given the root). Bumped only when a future
/// manifest evolution makes older readers unsafe.
pub const MANIFEST_READER_VERSION: u32 = 2;

/// The `file_hash` a sealed-only manifest carries in place of the real one
/// (the all-zero digest): the field is not optional on the wire, so a blanked
/// manifest names no file. [`ChunkManifest::check_hash_shape`] requires it.
pub fn blank_file_hash() -> ContentHash {
    ContentHash::from_digest_raw([0u8; 32])
}

impl ChunkManifest {
    /// The content-addressed storage key for chunk `i` — its [`Self::stored_hashes`]
    /// entry when present (a client-side-encrypted chunk keyed by ciphertext hash),
    /// else `chunk_hashes[i]` (the plaintext == stored key for unencrypted chunks).
    pub fn store_key(&self, i: usize) -> ContentHash {
        self.stored_hashes
            .as_ref()
            .and_then(|s| s.get(i))
            .copied()
            .unwrap_or_else(|| self.chunk_hashes[i])
    }

    /// The full ordered list of storage keys (one per chunk) — [`Self::stored_hashes`]
    /// when present, else a clone of `chunk_hashes`. This is what `check_chunks` and
    /// the download path address the blob store by.
    pub fn store_keys(&self) -> Vec<ContentHash> {
        self.stored_hashes
            .clone()
            .unwrap_or_else(|| self.chunk_hashes.clone())
    }

    /// Whether this manifest carries sealed plaintext hashes that a reader
    /// must open (via [`Self::unseal_hashes`]) before trusting
    /// `file_hash`/`chunk_hashes`.
    pub fn is_sealed(&self) -> bool {
        self.sealed_hashes.is_some()
    }

    /// Honest too-new gate: `Err` when this manifest declares a
    /// [`Self::min_reader`] above [`MANIFEST_READER_VERSION`] — the typed
    /// "needs a newer client" surface, so a future format bump fails here
    /// instead of as a cryptic hash-mismatch downstream.
    pub fn check_min_reader(&self) -> anyhow::Result<()> {
        let required = self.min_reader.unwrap_or(1);
        if required > MANIFEST_READER_VERSION {
            anyhow::bail!(
                "manifest requires reader version {required} (this client supports \
                 {MANIFEST_READER_VERSION}) — update this client"
            );
        }
        Ok(())
    }

    /// The write half of the seal: move `file_hash`/`chunk_hashes` into
    /// [`Self::sealed_hashes`] under the set's chunk root, blank the plaintext
    /// fields, and stamp `min_reader = 2`. Every writer of a sealed-chunk
    /// manifest (one carrying [`Self::stored_hashes`]) calls this before it
    /// encodes the manifest — the shape [`Self::check_hash_shape`] then
    /// requires of every such manifest a reader fetches.
    pub fn seal_hashes(mut self, root_secret: &[u8; 32]) -> anyhow::Result<Self> {
        let stored = self.stored_hashes.as_deref().ok_or_else(|| {
            anyhow::anyhow!("sealing the hashes of a manifest without stored_hashes")
        })?;
        if self.sealed_hashes.is_some() {
            anyhow::bail!("manifest hashes are already sealed");
        }
        if stored.len() != self.chunk_hashes.len() {
            anyhow::bail!(
                "manifest lists {} store keys for {} chunk hashes",
                stored.len(),
                self.chunk_hashes.len()
            );
        }
        let sealed = crate::manifest_crypto::seal_manifest_hashes(
            root_secret,
            &self.file_hash,
            &self.chunk_hashes,
            stored,
        )?;
        self.sealed_hashes = Some(sealed);
        self.file_hash = blank_file_hash();
        self.chunk_hashes = Vec::new();
        self.min_reader = Some(MANIFEST_READER_VERSION);
        Ok(self)
    }

    /// The form a writer encodes and uploads: under `seal_root` (the root that
    /// sealed the chunks) a sealed-chunk manifest's [`Self::seal_hashes`]; a
    /// plaintext manifest (`seal_root` `None`) as it is. Every writer encodes
    /// this, never the plaintext view it keeps for its own use.
    pub fn wire_form(&self, seal_root: Option<&[u8; 32]>) -> anyhow::Result<Self> {
        match (seal_root, &self.stored_hashes) {
            (Some(root), Some(_)) => self.clone().seal_hashes(root),
            (None, None) => Ok(self.clone()),
            (Some(_), None) => anyhow::bail!("a sealing root for a plaintext manifest"),
            (None, Some(_)) => anyhow::bail!("a sealed-chunk manifest without its root"),
        }
    }

    /// The one shape check every fetched manifest passes before its hashes
    /// are trusted. A plaintext manifest (no [`Self::stored_hashes`]) carries
    /// its hashes in the clear and no sealed blob. A sealed-chunk manifest
    /// (with `stored_hashes`) carries [`Self::sealed_hashes`] and NO plaintext
    /// hashes: a sealed-chunk manifest still naming its plaintext
    /// `file_hash`/`chunk_hashes` is the confirmation oracle that field
    /// closes, so it is refused rather than tolerated.
    pub fn check_hash_shape(&self) -> anyhow::Result<()> {
        match (&self.stored_hashes, &self.sealed_hashes) {
            (None, None) => Ok(()),
            (None, Some(_)) => anyhow::bail!("sealed manifest without stored_hashes"),
            (Some(_), None) => {
                anyhow::bail!("sealed-chunk manifest carries plaintext hashes and no sealed_hashes")
            }
            (Some(_), Some(_)) => {
                if !self.chunk_hashes.is_empty() || self.file_hash != blank_file_hash() {
                    anyhow::bail!(
                        "sealed-chunk manifest carries plaintext hashes beside sealed_hashes"
                    );
                }
                Ok(())
            }
        }
    }

    /// Open [`Self::sealed_hashes`] under the set's chunk root and return the
    /// manifest with `file_hash`/`chunk_hashes` restored (the plaintext view
    /// every downstream consumer expects). A plaintext manifest is returned
    /// unchanged; any manifest [`Self::check_hash_shape`] refuses is an error.
    /// Fails closed on a missing/wrong root — never falls through to the
    /// blanked plaintext fields.
    pub fn unseal_hashes(mut self, root_secret: &[u8; 32]) -> anyhow::Result<Self> {
        self.check_hash_shape()?;
        let Some(sealed) = self.sealed_hashes.take() else {
            return Ok(self);
        };
        let stored = self
            .stored_hashes
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("sealed manifest without stored_hashes"))?;
        let opened = crate::manifest_crypto::open_manifest_hashes(root_secret, stored, &sealed)?;
        if opened.chunk_hashes.len() != stored.len() {
            anyhow::bail!(
                "sealed manifest opens to {} chunk hashes for {} store keys",
                opened.chunk_hashes.len(),
                stored.len()
            );
        }
        self.file_hash = opened.file_hash;
        self.chunk_hashes = opened.chunk_hashes;
        Ok(self)
    }
}

/// The result of diffing two chunk manifests.
#[derive(Debug)]
pub struct ManifestDiff {
    /// Chunks present in the new manifest but not the old.
    pub added: Vec<ContentHash>,
    /// Chunks present in the old manifest but not the new.
    pub removed: Vec<ContentHash>,
    /// Chunks present in both manifests.
    pub unchanged: Vec<ContentHash>,
}

impl ChunkManifest {
    /// Compare this manifest against an older one, returning which chunks
    /// were added, removed, or unchanged.
    pub fn diff(&self, older: &ChunkManifest) -> ManifestDiff {
        use std::collections::HashSet;

        let old_set: HashSet<ContentHash> = older.chunk_hashes.iter().copied().collect();
        let new_set: HashSet<ContentHash> = self.chunk_hashes.iter().copied().collect();

        let added: Vec<ContentHash> = self
            .chunk_hashes
            .iter()
            .copied()
            .filter(|h| !old_set.contains(h))
            .collect();

        let removed: Vec<ContentHash> = older
            .chunk_hashes
            .iter()
            .copied()
            .filter(|h| !new_set.contains(h))
            .collect();

        let unchanged: Vec<ContentHash> = self
            .chunk_hashes
            .iter()
            .copied()
            .filter(|h| old_set.contains(h))
            .collect();

        ManifestDiff {
            added,
            removed,
            unchanged,
        }
    }
}

/// A single version of a file, identified by its manifest hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileVersion {
    /// Content hash of the ChunkManifest for this version.
    pub manifest_hash: ContentHash,
    /// Total size of the file in this version.
    pub size_bytes: u64,
    /// When this version was created.
    pub created_at: Timestamp,
}

/// The version history of a file, tracking all known versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileVersionHistory {
    /// The file path this history tracks.
    pub path: String,
    /// Ordered list of versions (oldest first).
    pub versions: Vec<FileVersion>,
}

impl FileVersionHistory {
    /// Create a new empty version history for the given path.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            versions: Vec::new(),
        }
    }

    /// Append a new version to the history.
    pub fn add_version(
        &mut self,
        manifest_hash: ContentHash,
        size_bytes: u64,
        created_at: Timestamp,
    ) {
        self.versions.push(FileVersion {
            manifest_hash,
            size_bytes,
            created_at,
        });
    }

    /// Return the most recent version, if any.
    pub fn latest(&self) -> Option<&FileVersion> {
        self.versions.last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a ContentHash from a single byte (padded with zeros).
    fn hash_from_byte(b: u8) -> ContentHash {
        let mut arr = [0u8; 32];
        arr[0] = b;
        ContentHash::from_digest_raw(arr)
    }

    #[test]
    fn chunk_manifest_serialization() {
        let manifest = ChunkManifest {
            file_hash: hash_from_byte(0xAA),
            total_size: 4096,
            chunk_hashes: vec![hash_from_byte(1), hash_from_byte(2), hash_from_byte(3)],
            chunk_sizes: vec![1500, 1500, 1096],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };

        let encoded = crate::encoding::canonical_encode(&manifest).expect("serialize");
        let decoded: ChunkManifest =
            crate::encoding::canonical_decode(&encoded).expect("deserialize");

        assert_eq!(decoded.file_hash, manifest.file_hash);
        assert_eq!(decoded.total_size, manifest.total_size);
        assert_eq!(decoded.chunk_hashes.len(), 3);
        assert_eq!(decoded.chunk_hashes[0], hash_from_byte(1));
        assert_eq!(decoded.chunk_hashes[1], hash_from_byte(2));
        assert_eq!(decoded.chunk_hashes[2], hash_from_byte(3));
        assert_eq!(decoded.chunk_sizes, vec![1500, 1500, 1096]);
        assert_eq!(decoded.stored_hashes, None);
    }

    #[test]
    fn stored_hashes_round_trips_and_store_key_falls_back() {
        // Plaintext manifest: store key == plaintext hash.
        let plain = ChunkManifest {
            file_hash: hash_from_byte(0x01),
            total_size: 10,
            chunk_hashes: vec![hash_from_byte(1), hash_from_byte(2)],
            chunk_sizes: vec![5, 5],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        assert_eq!(plain.store_key(0), hash_from_byte(1));
        assert_eq!(plain.store_keys(), plain.chunk_hashes);

        // Encrypted manifest: store key is the ciphertext hash, distinct from the
        // plaintext hash; serde round-trips it.
        let enc = ChunkManifest {
            file_hash: hash_from_byte(0x02),
            total_size: 10,
            chunk_hashes: vec![hash_from_byte(1), hash_from_byte(2)],
            chunk_sizes: vec![5, 5],
            stored_hashes: Some(vec![hash_from_byte(0xF1), hash_from_byte(0xF2)]),
            sealed_hashes: None,
            min_reader: None,
        };
        assert_eq!(enc.store_key(0), hash_from_byte(0xF1));
        assert_eq!(enc.store_key(1), hash_from_byte(0xF2));
        let bytes = crate::encoding::canonical_encode(&enc).expect("serialize");
        let back: ChunkManifest = crate::encoding::canonical_decode(&bytes).expect("deserialize");
        assert_eq!(back.stored_hashes, enc.stored_hashes);
        assert_eq!(back.chunk_hashes, enc.chunk_hashes);
    }

    #[test]
    fn plaintext_manifest_wire_shape_unchanged() {
        // Old-manifest compat pin (both directions): a plaintext-hash manifest
        // serializes with NO new map keys — byte-identical to what every
        // pre-enablement binary wrote and expects — and pre-enablement bytes
        // (no `sealed_hashes`/`min_reader` keys) decode with the defaults.
        let manifest = ChunkManifest {
            file_hash: hash_from_byte(0xAA),
            total_size: 4096,
            chunk_hashes: vec![hash_from_byte(1)],
            chunk_sizes: vec![4096],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let encoded = crate::encoding::canonical_encode(&manifest).expect("serialize");
        // dag-cbor map keys are UTF-8 text on the wire, so a substring scan is
        // a faithful "key absent" check.
        let haystack = encoded.clone();
        for key in [b"sealed_hashes".as_slice(), b"min_reader".as_slice()] {
            assert!(
                !haystack.windows(key.len()).any(|w| w == key),
                "plaintext manifest must not carry the {} key",
                String::from_utf8_lossy(key)
            );
        }
        let decoded: ChunkManifest =
            crate::encoding::canonical_decode(&encoded).expect("legacy shape decodes");
        assert_eq!(decoded.sealed_hashes, None);
        assert_eq!(decoded.min_reader, None);
        assert!(!decoded.is_sealed());
        decoded
            .check_min_reader()
            .expect("legacy manifest readable");
    }

    #[test]
    fn min_reader_gate_is_honest() {
        let mut manifest = ChunkManifest {
            file_hash: hash_from_byte(0xAA),
            total_size: 1,
            chunk_hashes: vec![hash_from_byte(1)],
            chunk_sizes: vec![1],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: Some(MANIFEST_READER_VERSION),
        };
        manifest
            .check_min_reader()
            .expect("current version readable");
        manifest.min_reader = Some(MANIFEST_READER_VERSION + 1);
        let err = manifest.check_min_reader().expect_err("too-new must error");
        assert!(err.to_string().contains("update this client"), "{err}");
    }

    #[test]
    fn sealed_manifest_unseals_and_fails_closed() {
        // A sealed-only manifest (blanked plaintext fields, the shape every
        // writer emits) restores `file_hash`/`chunk_hashes` via the root.
        let root = [7u8; 32];
        let file_hash = hash_from_byte(0xAB);
        let chunk_hashes = vec![hash_from_byte(1), hash_from_byte(2)];
        let stored_hashes = vec![hash_from_byte(0xF1), hash_from_byte(0xF2)];
        let sealed = crate::manifest_crypto::seal_manifest_hashes(
            &root,
            &file_hash,
            &chunk_hashes,
            &stored_hashes,
        )
        .expect("seal");
        let manifest = ChunkManifest {
            file_hash: hash_from_byte(0x00), // blanked by a sealed-only writer
            total_size: 10,
            chunk_hashes: vec![], // blanked
            chunk_sizes: vec![5, 5],
            stored_hashes: Some(stored_hashes.clone()),
            sealed_hashes: Some(sealed.clone()),
            min_reader: Some(2),
        };
        assert!(manifest.is_sealed());
        let opened = manifest.clone().unseal_hashes(&root).expect("unseal");
        assert_eq!(opened.file_hash, file_hash);
        assert_eq!(opened.chunk_hashes, chunk_hashes);
        assert!(!opened.is_sealed());
        // Store addressing never depended on the sealed fields.
        assert_eq!(opened.store_keys(), stored_hashes);

        // Wrong root ⇒ fail closed, never fall through to the blanked fields.
        assert!(manifest.clone().unseal_hashes(&[9u8; 32]).is_err());
        // A sealed manifest without stored_hashes is malformed ⇒ fail closed.
        let mut malformed = manifest;
        malformed.stored_hashes = None;
        assert!(malformed.unseal_hashes(&root).is_err());
    }

    /// The write and read halves agree: `seal_hashes` produces exactly the
    /// shape `check_hash_shape` accepts, and a sealed-chunk manifest naming
    /// its plaintext hashes — beside the sealed blob or instead of it — is
    /// refused by the shape check and by `unseal_hashes` alike.
    #[test]
    fn sealed_chunk_manifests_carry_no_plaintext_hashes() {
        let root = [7u8; 32];
        let plain_view = ChunkManifest {
            file_hash: hash_from_byte(0xAB),
            total_size: 10,
            chunk_hashes: vec![hash_from_byte(1), hash_from_byte(2)],
            chunk_sizes: vec![5, 5],
            stored_hashes: Some(vec![hash_from_byte(0xF1), hash_from_byte(0xF2)]),
            sealed_hashes: None,
            min_reader: None,
        };
        // The pre-flip shape: store keys, plaintext hashes, no seal — refused.
        assert!(plain_view.check_hash_shape().is_err());
        assert!(plain_view.clone().unseal_hashes(&root).is_err());

        let sealed = plain_view.clone().seal_hashes(&root).expect("seal");
        assert!(sealed.chunk_hashes.is_empty());
        assert_eq!(sealed.file_hash, blank_file_hash());
        assert_eq!(sealed.min_reader, Some(MANIFEST_READER_VERSION));
        sealed.check_hash_shape().expect("the written shape");
        sealed.check_min_reader().expect("this reader opens it");
        let opened = sealed.clone().unseal_hashes(&root).expect("unseal");
        assert_eq!(opened.file_hash, plain_view.file_hash);
        assert_eq!(opened.chunk_hashes, plain_view.chunk_hashes);

        // Plaintext hashes left beside the seal — refused.
        let mut leaky = sealed.clone();
        leaky.chunk_hashes = plain_view.chunk_hashes.clone();
        assert!(leaky.check_hash_shape().is_err());
        assert!(leaky.unseal_hashes(&root).is_err());
        let mut leaky = sealed;
        leaky.file_hash = plain_view.file_hash;
        assert!(leaky.check_hash_shape().is_err());

        // Sealing twice, or sealing a plaintext manifest, is a writer bug.
        let plain = ChunkManifest {
            stored_hashes: None,
            ..plain_view
        };
        plain
            .check_hash_shape()
            .expect("a plaintext manifest stays plaintext");
        assert!(plain.seal_hashes(&root).is_err());
    }

    #[test]
    fn manifest_diff() {
        // v1 has chunks [A, B, C]
        let v1 = ChunkManifest {
            file_hash: hash_from_byte(0x10),
            total_size: 3000,
            chunk_hashes: vec![hash_from_byte(1), hash_from_byte(2), hash_from_byte(3)],
            chunk_sizes: vec![1000, 1000, 1000],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };

        // v2 has chunks [B, C, D] — A removed, D added, B and C unchanged
        let v2 = ChunkManifest {
            file_hash: hash_from_byte(0x20),
            total_size: 3000,
            chunk_hashes: vec![hash_from_byte(2), hash_from_byte(3), hash_from_byte(4)],
            chunk_sizes: vec![1000, 1000, 1000],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };

        let diff = v2.diff(&v1);

        // D (4) was added
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0], hash_from_byte(4));

        // A (1) was removed
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0], hash_from_byte(1));

        // B (2) and C (3) are unchanged
        assert_eq!(diff.unchanged.len(), 2);
        assert!(diff.unchanged.contains(&hash_from_byte(2)));
        assert!(diff.unchanged.contains(&hash_from_byte(3)));
    }

    #[test]
    fn file_version_history() {
        let mut history = FileVersionHistory::new("/photos/cat.jpg");

        // No versions yet
        assert!(history.latest().is_none());
        assert_eq!(history.path, "/photos/cat.jpg");

        // Add first version
        let hash_v1 = hash_from_byte(0x01);
        history.add_version(hash_v1, 1024, Timestamp(1_000_000));

        assert_eq!(history.versions.len(), 1);
        let latest = history.latest().expect("should have a version");
        assert_eq!(latest.manifest_hash, hash_v1);
        assert_eq!(latest.size_bytes, 1024);
        assert_eq!(latest.created_at, Timestamp(1_000_000));

        // Add second version
        let hash_v2 = hash_from_byte(0x02);
        history.add_version(hash_v2, 2048, Timestamp(2_000_000));

        assert_eq!(history.versions.len(), 2);
        let latest = history.latest().expect("should have a version");
        assert_eq!(latest.manifest_hash, hash_v2);
        assert_eq!(latest.size_bytes, 2048);
        assert_eq!(latest.created_at, Timestamp(2_000_000));
    }
}
