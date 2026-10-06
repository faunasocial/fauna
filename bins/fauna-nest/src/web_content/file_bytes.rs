//! Reading a **synced file's** bytes out of the nest's blob store.
//!
//! A `web_files` row (like every sync change) points at a *manifest* hash, not
//! at the file body: the body lives in the content-addressed chunk blobs the
//! manifest lists, and every blob the chunk-upload route wrote is wrapped in the
//! nest's at-rest framing (`backup::encode_blob` — compress, then encrypt when
//! the deployment has a backup key). So "serve the file at `web_files.blob_hash`"
//! is a four-step walk, not a `get`:
//!
//! ```text
//! get(manifest_hash) → decode_blob → ChunkManifest
//!   → for each chunk: get(store_key) → decode_blob → [decrypt_chunk]
//!                     → decompress → verify blake3(chunk) == chunk_hashes[i]
//!   → concat → verify blake3(bytes) == manifest.file_hash
//! ```
//!
//! The `decompress` step is not optional and its order is fixed: the client
//! writes `compress -> encrypt`, so opening is `decrypt -> decompress`
//! (`fauna_core::compress` § Pipeline order). It was missing here until
//! 2026-07-31 — see [`unframe`].
//!
//! Two classes of file flow through here, discriminated by the manifest:
//!
//! - **Plaintext** (`stored_hashes == None`): the store key *is* the plaintext
//!   chunk hash and the decoded bytes are the content. Today's public web
//!   hosting — an unpaywalled `web` set.
//! - **Content-key-sealed** (`stored_hashes == Some`): the chunks are
//!   `chunk_crypto`-AEAD ciphertext under an M2 per-set content-key generation
//!   (`mls-group-key-material.md` § M2). The nest can open these **only** with a
//!   key a user's client granted it — the web-paywall holder's
//!   `content.read{folder:set}` grant (`monetization.md` § Pillar 2).
//!
//! **Fail-closed rule (`web-content-hosting.md` § Sealed static files):
//! ciphertext is never served.** A sealed manifest with no candidate key is
//! [`FileOpenError::Sealed`] — never a fallthrough that emits the ciphertext (or
//! the framed manifest) as a body. This is the same refusal the public
//! share-link path makes (`share_routes.rs` step 8b, 403), stated once here so
//! the two rails cannot drift.

use std::sync::Arc;

use fauna_core::chunk::ChunkManifest;
use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;

use crate::blob_store::BlobStoreBackend;

/// The most a sealed chunk's stored ciphertext exceeds its plaintext: the
/// one-byte compression frame (whose raw fallback is never longer than its
/// input plus that byte) and the AEAD tag (`fauna_core::chunk_seal`, frame
/// then encrypt).
const SEALED_CHUNK_OVERHEAD: u64 = 1 + fauna_core::chunk_crypto::ENCRYPTION_OVERHEAD as u64;

/// Why a manifest-addressed file could not be produced as plaintext bytes.
#[derive(Debug, thiserror::Error)]
pub enum FileOpenError {
    /// The manifest blob is absent from this nest's store (the change was
    /// recorded but the manifest never uploaded).
    #[error("manifest blob {0} not found")]
    MissingManifest(String),
    /// A chunk the manifest lists is absent.
    #[error("chunk blob {0} not found")]
    MissingChunk(String),
    /// The manifest blob did not decode/parse (at-rest framing or CBOR).
    #[error("manifest decode: {0}")]
    Decode(String),
    /// The manifest is content-key-sealed and this caller supplied no key that
    /// opens it — the **fail-closed** verdict. Never serve anything in
    /// response to this; the content is dark by design.
    #[error("sealed manifest: no candidate content key opens generation")]
    Sealed,
    /// Decoded, but the reassembled bytes do not hash to `manifest.file_hash`.
    #[error("file hash mismatch")]
    Integrity,
    /// A blob this walk must read is under a **legal-takedown withhold**
    /// (`moderation.md` § Legal takedown → *The blob-serve door*). The bytes
    /// are still on the box — tombstone-not-delete keeps the GC pin flag-blind
    /// — but no route may serve them: answer 451, never a fallthrough that
    /// emits them.
    #[error("legally withheld blob")]
    Withheld,
    /// The manifest's chunk list does not fit its own declared `total_size`:
    /// more chunks than the size has bytes, or chunks whose bytes pass it.
    /// Refused before the walk reads further (a manifest may name one stored
    /// chunk tens of thousands of times, and each repeat is a full read).
    #[error("manifest chunk list exceeds its declared size")]
    Oversized,
}

/// Fetch one blob and strip the nest's at-rest framing — **after** the
/// legal-takedown withhold, which binds this walk exactly as it binds the four
/// routes onto the store (`moderation.md` § Legal takedown → *The blob-serve
/// door*).
///
/// Gating the seam rather than the callers is deliberate. The walk reads the
/// manifest **and** every chunk it names, and `POST /api/v1/manifests` is
/// `BulkWriteAuth`: any registered user can upload a manifest naming a
/// withheld digest as one of its chunks — for a single-chunk file the declared
/// `file_hash` *is* that digest, so the craft needs nothing beyond the hex the
/// requester already holds — and then publish the compelled bytes under a
/// public share link or a site path. One gate here binds all three consumers
/// of the walk, and the next one to be written — which is why the share
/// link's ciphertext arm (`share_routes.rs`, the manifest and the chunk it
/// serves by index) reads through this seam rather than the store.
pub(crate) async fn fetch_decoded(
    db: &crate::db::CacheDb,
    store: &Arc<dyn BlobStoreBackend>,
    at_rest_key: Option<&BackupKey>,
    hash: &ContentHash,
) -> Result<Option<Vec<u8>>, FileOpenError> {
    if crate::blob_routes::is_legally_withheld(db, &hash.digest()).await {
        tracing::info!(
            blob = hex::encode(hash.digest()),
            "file walk withheld — legal takedown"
        );
        return Err(FileOpenError::Withheld);
    }
    let Some(raw) = store.get(hash).await.ok().flatten() else {
        return Ok(None);
    };
    Ok(crate::backup::decode_blob(&raw, at_rest_key).ok())
}

/// Read a synced file's plaintext bytes given its manifest hash.
///
/// `content_keys` are the candidate M2 content-key generations for this file's
/// `content_key_version` — empty for a plaintext file. Several candidates are
/// legal and expected: a concurrent-rotation CRDT merge can leave two distinct
/// keys sharing one version (KMH § M2 *Generations*), so each is tried and the
/// AEAD tag disambiguates. Mirrors the sync engine's `content_open_roots` shape
/// (`fauna-sync-engine/src/engine.rs`), which is the client-side twin of this
/// walk.
///
/// A sealed manifest with no working key returns [`FileOpenError::Sealed`] —
/// the caller must NOT fall back to serving raw bytes. A manifest, or any
/// chunk it names, under a legal-takedown withhold returns
/// [`FileOpenError::Withheld`], which every caller refuses on — 451 wherever
/// the arm shapes its own response, and the built-in 404 page in the one arm
/// where the withheld file *is* the site's custom `404.html` template (see
/// [`fetch_decoded`] for why the gate is on the seam).
pub async fn read_file_by_manifest(
    db: &crate::db::CacheDb,
    store: &Arc<dyn BlobStoreBackend>,
    at_rest_key: Option<&BackupKey>,
    manifest_hash: &[u8; 32],
    content_keys: &[&[u8]],
) -> Result<Vec<u8>, FileOpenError> {
    let mhash = ContentHash::from_digest_raw(*manifest_hash);
    let manifest_bytes = fetch_decoded(db, store, at_rest_key, &mhash)
        .await?
        .ok_or_else(|| FileOpenError::MissingManifest(hex::encode(manifest_hash)))?;
    let manifest: ChunkManifest = fauna_core::encoding::canonical_decode(&manifest_bytes)
        .map_err(|e| FileOpenError::Decode(e.to_string()))?;
    manifest
        .check_min_reader()
        .map_err(|e| FileOpenError::Decode(e.to_string()))?;

    // The manifest is uploader-authored and reachable by an anonymous
    // GET, and nothing about its chunk list is self-verifying: it may name one
    // stored chunk tens of thousands of times, and every repeat is a full read.
    // Repeats are legal in an honest plaintext file (a run of identical bytes
    // chunks identically), so the walk bounds by size, not by distinctness:
    // before any read, no more chunks than the declared size has bytes (every
    // chunk carries at least one); during it, a running total that stops at the
    // chunk carrying the walk past the declared size.
    let listed = manifest
        .chunk_hashes
        .len()
        .max(manifest.stored_hashes.as_ref().map_or(0, Vec::len));
    if listed as u64 > manifest.total_size.max(1) {
        return Err(FileOpenError::Oversized);
    }

    // Plaintext: the store key is the plaintext chunk hash; decoded bytes are
    // the content. (`stored_hashes` absent ⇒ no client-side encryption.)
    if manifest.stored_hashes.is_none() {
        let mut out = Vec::with_capacity(manifest.total_size as usize);
        for hash in &manifest.chunk_hashes {
            let chunk = fetch_decoded(db, store, at_rest_key, hash)
                .await?
                .ok_or_else(|| FileOpenError::MissingChunk(hex::encode(hash.digest())))?;
            out.extend_from_slice(&unframe(chunk, hash)?);
            if out.len() as u64 > manifest.total_size {
                return Err(FileOpenError::Oversized);
            }
        }
        return verify(out, &manifest);
    }

    // Sealed: no key ⇒ dark. Never emit ciphertext (the fail-closed rule).
    if content_keys.is_empty() {
        return Err(FileOpenError::Sealed);
    }

    // Fetch the ciphertext chunks once (by STORE key = ciphertext hash), then
    // try each candidate generation key over them.
    // The running total here is over ciphertext, which may exceed its
    // plaintext by at most the frame prefix and the AEAD tag per chunk
    // (`SEALED_CHUNK_OVERHEAD`).
    let mut ciphertexts = Vec::with_capacity(manifest.chunk_hashes.len());
    let mut read: u64 = 0;
    for store_key in manifest.store_keys() {
        let chunk = fetch_decoded(db, store, at_rest_key, &store_key)
            .await?
            .ok_or_else(|| FileOpenError::MissingChunk(hex::encode(store_key.digest())))?;
        read += chunk.len() as u64;
        let allowed = manifest.total_size + (ciphertexts.len() as u64 + 1) * SEALED_CHUNK_OVERHEAD;
        if read > allowed {
            return Err(FileOpenError::Oversized);
        }
        ciphertexts.push(chunk);
    }

    for key in content_keys {
        let Ok(root) = <[u8; 32]>::try_from(*key) else {
            continue;
        };
        // A sealed-hash manifest hides its plaintext hashes too; open them
        // under the same root before they are used as AEAD salts. Fails closed
        // on the wrong root rather than falling through to blanked fields.
        let opened = if manifest.is_sealed() {
            match manifest.clone().unseal_hashes(&root) {
                Ok(m) => m,
                Err(_) => continue,
            }
        } else {
            manifest.clone()
        };
        // The per-chunk key/nonce derive from the PLAINTEXT hash, while the
        // bytes were addressed by the ciphertext hash — a wrong root fails the
        // Poly1305 tag here, which is what disambiguates same-version
        // duplicate generations.
        match fauna_core::chunk_crypto::decrypt_chunks(&root, &opened.chunk_hashes, &ciphertexts) {
            Ok(chunks) => {
                let mut out = Vec::with_capacity(opened.total_size as usize);
                for (c, hash) in chunks.into_iter().zip(&opened.chunk_hashes) {
                    out.extend_from_slice(&unframe(c, hash)?);
                    if out.len() as u64 > opened.total_size {
                        return Err(FileOpenError::Oversized);
                    }
                }
                return verify(out, &opened);
            }
            Err(_) => continue,
        }
    }
    Err(FileOpenError::Sealed)
}

/// Strip a chunk's self-describing compression frame — the stage that makes this
/// walk the exact inverse of the client's `compress -> encrypt` write pipeline
/// (`fauna_core::compress` § Pipeline order; `file-sync.md` § Content-Addressed
/// Storage).
///
/// Missing here until 2026-07-31, in both arms above, so a synced file served
/// through web hosting reassembled to `0x00 ‖ plaintext` and failed
/// [`FileOpenError::Integrity`] — the *serving* twin of the same omission found
/// the same day in `bins/fauna-sync`. It stayed invisible because
/// `seed_synced_file` skipped compression too, so the tests wrote a shape no
/// client produces; that helper's own doc comment already warned this had
/// happened once before in this very file.
///
/// The frame is not decidable by inspection (an unframed chunk can begin `0x00`),
/// so the manifest's plaintext chunk hash is the oracle: framed first, the raw
/// body as the fallback, fail closed when neither addresses the recorded
/// content. The raw arm is live: this store is first-writer-wins and a
/// headerless chunk upload rests a raw body under the very key a public chunk
/// takes (`fauna_core::compress::unframe_verified_chunk` says why).
fn unframe(body: Vec<u8>, want: &ContentHash) -> Result<Vec<u8>, FileOpenError> {
    fauna_core::compress::unframe_verified_chunk(body, want).ok_or(FileOpenError::Integrity)
}

/// The post-decrypt integrity anchor: the reassembled bytes must hash to the
/// manifest's `file_hash` (the same check the sync engine's download path
/// makes). Catches a truncated/reordered/substituted chunk set that individually
/// passed its AEAD tags.
fn verify(bytes: Vec<u8>, manifest: &ChunkManifest) -> Result<Vec<u8>, FileOpenError> {
    if ContentHash::of_raw(&bytes) != manifest.file_hash {
        return Err(FileOpenError::Integrity);
    }
    Ok(bytes)
}

/// Seed a file into the blob store **exactly as the production ingest does** —
/// chunk it, wrap every chunk and the manifest in the nest's at-rest framing
/// (`backup::encode_blob`, what `chunk_routes` writes), and return the
/// **manifest** hash, which is what a `web_files` row holds.
///
/// `content_key`: `None` = a plaintext set (store key == plaintext chunk hash);
/// `Some(k)` = a content-key-sealed set (chunks AEAD-sealed, addressed by their
/// ciphertext hash via `stored_hashes` — the M2 shape).
///
/// Test-only, but deliberately living beside the read path: a seeding helper
/// that writes a *different* shape than production is how the "web serving
/// works" tests stayed green while synced files served framed CBOR.
#[cfg(test)]
pub(crate) async fn seed_synced_file(
    store: &Arc<dyn BlobStoreBackend>,
    at_rest_key: Option<&BackupKey>,
    content: &[u8],
    content_key: Option<&[u8; 32]>,
) -> [u8; 32] {
    let mut manifest = fauna_core::chunker::chunk_file(content);
    let chunks = fauna_core::chunker::extract_chunks(content, &manifest);

    match content_key {
        None => {
            for (hash, data) in &chunks {
                // Production frames BEFORE the (absent) seal, through the one
                // door (`fauna_core::chunk_seal`) — see the sealed arm below. A
                // seeder that skips it writes a shape no client produces, which
                // is the trap this helper's doc comment names.
                let body = fauna_core::chunk_seal::FramedChunk::frame(hash, data)
                    .unwrap()
                    .into_body();
                let framed = crate::backup::encode_blob(&body, at_rest_key, false).unwrap();
                store.put(hash, &framed).await.unwrap();
            }
        }
        Some(key) => {
            let mut stored = Vec::with_capacity(chunks.len());
            for (hash, data) in &chunks {
                // The client's ONE seal door (`fauna_core::chunk_seal`): frame
                // -> encrypt, the plaintext hash as the AEAD salt, so the frame
                // rides INSIDE the ciphertext — byte-identical to what the sync
                // engine and the Go WebDAV MDA upload (`fauna_core::compress`
                // § Pipeline order; `file-sync.md` § Content-Addressed Storage).
                let (store_key, ciphertext) =
                    fauna_core::chunk_seal::seal_chunk_body(hash, data, key).unwrap();
                let framed = crate::backup::encode_blob(&ciphertext, at_rest_key, false).unwrap();
                store.put(&store_key, &framed).await.unwrap();
                stored.push(store_key);
            }
            manifest.stored_hashes = Some(stored);
        }
    }

    let manifest_bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);
    let framed = crate::backup::encode_blob(&manifest_bytes, at_rest_key, false).unwrap();
    store.put(&manifest_hash, &framed).await.unwrap();
    manifest_hash.digest()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_store::DiskBlobStore;

    fn store() -> (Arc<dyn BlobStoreBackend>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        (store, dir)
    }

    /// A db with an empty withhold set — the walk consults it on every blob it
    /// reads (`fetch_decoded`), so every test needs one.
    fn db() -> Arc<crate::db::CacheDb> {
        Arc::new(crate::db::CacheDb::open_in_memory().unwrap())
    }

    /// The plaintext walk: what public web hosting serves. Before this path
    /// existed the serve layer returned the framed CBOR manifest verbatim.
    #[tokio::test]
    async fn plaintext_file_round_trips_through_the_manifest_walk() {
        let (store, _dir) = store();
        let content = b"<h1>hello</h1>".repeat(500);
        let hash = seed_synced_file(&store, None, &content, None).await;

        let out = read_file_by_manifest(&db(), &store, None, &hash, &[])
            .await
            .unwrap();
        assert_eq!(
            out, content,
            "the walk must reproduce the file byte-for-byte"
        );
        assert_ne!(
            out.first(),
            Some(&0x00),
            "a framed-manifest regression would start with the compression prefix"
        );
    }

    /// The at-rest key is applied to every blob, manifest included.
    #[tokio::test]
    async fn plaintext_file_round_trips_with_at_rest_encryption() {
        let (store, _dir) = store();
        let key = fauna_core::crypto::BackupKey::from_bytes([7u8; 32]);
        let content = b"encrypted at rest".to_vec();
        let hash = seed_synced_file(&store, Some(&key), &content, None).await;

        assert_eq!(
            read_file_by_manifest(&db(), &store, Some(&key), &hash, &[])
                .await
                .unwrap(),
            content
        );
    }

    /// The paywall path: a sealed file opens under its generation key.
    #[tokio::test]
    async fn sealed_file_opens_under_its_content_key() {
        let (store, _dir) = store();
        let key = [0xC1u8; 32];
        let content = b"members-only report".repeat(300);
        let hash = seed_synced_file(&store, None, &content, Some(&key)).await;

        let out = read_file_by_manifest(&db(), &store, None, &hash, &[&key[..]])
            .await
            .unwrap();
        assert_eq!(out, content);
    }

    /// **The fail-closed rule.** A sealed file with no key must never yield
    /// bytes — not the ciphertext, not the framed manifest, nothing.
    #[tokio::test]
    async fn sealed_file_without_a_key_fails_closed() {
        let (store, _dir) = store();
        let content = b"members-only report".to_vec();
        let hash = seed_synced_file(&store, None, &content, Some(&[0xC1u8; 32])).await;

        assert!(
            matches!(
                read_file_by_manifest(&db(), &store, None, &hash, &[]).await,
                Err(FileOpenError::Sealed)
            ),
            "a sealed manifest with no candidate key must be dark"
        );
    }

    /// A grant for the WRONG generation (or the wrong set) opens nothing: the
    /// AEAD tag refuses, and the refusal is `Sealed`, not a fallthrough.
    #[tokio::test]
    async fn sealed_file_under_a_wrong_key_fails_closed() {
        let (store, _dir) = store();
        let content = b"members-only report".to_vec();
        let hash = seed_synced_file(&store, None, &content, Some(&[0xC1u8; 32])).await;

        let wrong = [0xC2u8; 32];
        assert!(matches!(
            read_file_by_manifest(&db(), &store, None, &hash, &[&wrong[..]]).await,
            Err(FileOpenError::Sealed)
        ));
    }

    /// Same-version duplicate generations (the CRDT-merge edge, KMH § M2): the
    /// holder wields several candidates and the AEAD picks the right one.
    #[tokio::test]
    async fn sealed_file_tries_every_candidate_generation() {
        let (store, _dir) = store();
        let real = [0xC3u8; 32];
        let content = b"duplicate-generation content".to_vec();
        let hash = seed_synced_file(&store, None, &content, Some(&real)).await;

        let decoy = [0xC4u8; 32];
        let out = read_file_by_manifest(&db(), &store, None, &hash, &[&decoy[..], &real[..]])
            .await
            .unwrap();
        assert_eq!(out, content, "the losing candidate must not abort the open");
    }

    /// **A manifest naming a withheld digest never reassembles** — the fifth
    /// avenue onto the blob store.
    ///
    /// `moderation.md` § Legal takedown → *The blob-serve door* binds the
    /// BYTES, so it binds this walk too, and the walk is where it has to bind:
    /// `POST /api/v1/manifests` is `BulkWriteAuth`, so any registered user can
    /// name a withheld digest as a chunk of a manifest of their own and then
    /// publish it — under a public share link, or as a file on their own web
    /// site — where nothing downstream ever looks at the digests the manifest
    /// walked.
    ///
    /// Both ends of the walk are asserted: the manifest blob itself withheld,
    /// and a chunk it names withheld. The restore half is not decoration —
    /// tombstone-not-delete keeps the bytes on the box, so a "gate" that
    /// evicted them would pass the withhold assertion and fail here.
    #[tokio::test]
    async fn a_manifest_naming_a_withheld_chunk_never_reassembles() {
        let (store, _dir) = store();
        let db = db();
        let content = b"the compelled bytes, reassembled by a crafted manifest".repeat(40);
        let manifest_hash = seed_synced_file(&store, None, &content, None).await;

        // Baseline: the walk reads it.
        let out = read_file_by_manifest(&db, &store, None, &manifest_hash, &[])
            .await
            .expect("an unwithheld file reassembles");
        assert_eq!(out, content);

        // The manifest names its chunks by their plaintext hashes; a plaintext
        // set stores each chunk under that very digest, so this is the digest
        // the withhold set would carry.
        let manifest: fauna_core::chunk::ChunkManifest = fauna_core::encoding::canonical_decode(
            &fetch_decoded(
                &db,
                &store,
                None,
                &ContentHash::from_digest_raw(manifest_hash),
            )
            .await
            .unwrap()
            .unwrap(),
        )
        .unwrap();
        let chunk_digest = manifest.chunk_hashes[0].digest();

        db.replace_blob_legal_withhold(&[chunk_digest])
            .await
            .unwrap();
        assert!(
            matches!(
                read_file_by_manifest(&db, &store, None, &manifest_hash, &[]).await,
                Err(FileOpenError::Withheld)
            ),
            "a chunk under a withhold stops the walk — the compelled bytes must \
             not be reassembled into a share link or a site path"
        );

        // The manifest blob itself, withheld in its own right (a video post's
        // manifest hash is in the set alongside its segments).
        db.replace_blob_legal_withhold(&[manifest_hash])
            .await
            .unwrap();
        assert!(
            matches!(
                read_file_by_manifest(&db, &store, None, &manifest_hash, &[]).await,
                Err(FileOpenError::Withheld)
            ),
            "and a withheld manifest is refused before its chunks are walked"
        );

        // Restore re-serves the very same bytes: nothing was deleted.
        db.replace_blob_legal_withhold(&[]).await.unwrap();
        assert_eq!(
            read_file_by_manifest(&db, &store, None, &manifest_hash, &[])
                .await
                .expect("a restored file reassembles again"),
            content,
            "restore=true reassembles the same file, byte for byte"
        );
    }

    /// Store `manifest` as a manifest blob (at-rest framed, no at-rest key) and
    /// return its hash — for crafted manifests no client would write.
    async fn put_manifest(
        store: &Arc<dyn BlobStoreBackend>,
        manifest: &fauna_core::chunk::ChunkManifest,
    ) -> [u8; 32] {
        let bytes = fauna_core::encoding::canonical_encode(manifest).unwrap();
        let hash = ContentHash::of_raw(&bytes);
        let framed = crate::backup::encode_blob(&bytes, None, false).unwrap();
        store.put(&hash, &framed).await.unwrap();
        hash.digest()
    }

    /// **The repeat walk, web arm** — reachable by an anonymous GET of a site path
    /// or share link. A Web-set manifest that repeats one staged chunk past its
    /// declared `total_size` is refused before it is read in full, in both
    /// arms: the running total stops the walk at the chunk that passes the
    /// declared size. Observed by what comes after it — every later entry names
    /// a chunk that is not stored, so a walk that kept reading would answer
    /// `MissingChunk`.
    #[tokio::test]
    async fn a_manifest_repeating_one_chunk_past_its_size_is_refused_before_read_in_full() {
        for content_key in [None, Some([0xC5u8; 32])] {
            let (store, _dir) = store();
            // Incompressible, so a sealed chunk's ciphertext tracks its size
            // (a compressible repeat is caught after decrypt instead).
            let mut content = vec![0u8; 7000];
            blake3::Hasher::new()
                .update(b"cmxi")
                .finalize_xof()
                .fill(&mut content);
            let honest_hash = seed_synced_file(&store, None, &content, content_key.as_ref()).await;
            let mut manifest: fauna_core::chunk::ChunkManifest =
                fauna_core::encoding::canonical_decode(
                    &fetch_decoded(
                        &db(),
                        &store,
                        None,
                        &ContentHash::from_digest_raw(honest_hash),
                    )
                    .await
                    .unwrap()
                    .unwrap(),
                )
                .unwrap();
            assert_eq!(manifest.chunk_hashes.len(), 1, "fixture is one chunk");

            // Two real repeats, then entries naming a chunk that is not stored.
            let absent = ContentHash::from_digest_raw([0xeeu8; 32]);
            let repeat = |real: ContentHash| {
                let mut v = vec![real, real];
                v.extend(std::iter::repeat_n(absent, 1_000));
                v
            };
            manifest.chunk_hashes = repeat(manifest.chunk_hashes[0]);
            manifest.chunk_sizes = vec![content.len() as u64; manifest.chunk_hashes.len()];
            if let Some(stored) = manifest.stored_hashes.as_mut() {
                *stored = repeat(stored[0]);
            }
            manifest.total_size = content.len() as u64 + 1;
            let crafted = put_manifest(&store, &manifest).await;

            let keys: Vec<&[u8]> = content_key.iter().map(|k| &k[..]).collect();
            let err = read_file_by_manifest(&db(), &store, None, &crafted, &keys)
                .await
                .expect_err("a repeat walk past the declared size must be refused");
            assert!(
                matches!(err, FileOpenError::Oversized),
                "sealed={}: refused at the chunk that passed the declared size, got {err:?}",
                content_key.is_some()
            );
        }
    }

    /// The count bound: a manifest listing more chunks than its declared size
    /// has bytes (every chunk carries at least one) is refused before any
    /// chunk is read — here every entry names a chunk that is not stored.
    #[tokio::test]
    async fn a_manifest_listing_more_chunks_than_bytes_is_refused_before_any_read() {
        let (store, _dir) = store();
        let absent = ContentHash::from_digest_raw([0xeeu8; 32]);
        let manifest = fauna_core::chunk::ChunkManifest {
            file_hash: absent,
            total_size: 10,
            chunk_hashes: vec![absent; 1_000],
            chunk_sizes: vec![0; 1_000],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let crafted = put_manifest(&store, &manifest).await;
        let err = read_file_by_manifest(&db(), &store, None, &crafted, &[])
            .await
            .unwrap_err();
        assert!(matches!(err, FileOpenError::Oversized), "got {err:?}");
    }

    /// A raw pre-seed does not darken a public file. A public
    /// (unsealed) chunk rests under its plaintext hash, which is also the key a
    /// headerless `POST /api/v1/chunks` stores its body under. The store is
    /// first-writer-wins, so a registered user who knows a chunk's plaintext
    /// `P` and uploads it raw first leaves `P`, unframed, where the owner's
    /// framed chunk would rest, and the owner's later put is skipped. The walk
    /// must still serve the file: the body's `blake3` IS the manifest's
    /// recorded hash, so the hash check that decides the reading is the
    /// integrity guarantee. Both first-byte shapes: one with no frame prefix,
    /// and one led by a frame byte (its framed reading misses the hash).
    #[tokio::test]
    async fn a_raw_pre_seed_under_a_public_chunk_key_still_serves_the_file() {
        for content in [
            b"<h1>a well-known public asset</h1>".repeat(20),
            [&[0x00u8][..], &b"frame-byte-led plaintext".repeat(20)].concat(),
        ] {
            let (store, _dir) = store();
            let manifest = fauna_core::chunker::chunk_file(&content);
            let chunks = fauna_core::chunker::extract_chunks(&content, &manifest);
            // The pre-seed: each plaintext chunk stored as-is under blake3(P),
            // through the same at-rest encoding `upload_chunk` applies.
            for (hash, data) in &chunks {
                assert_eq!(*hash, ContentHash::of_raw(data));
                let at_rest = crate::backup::encode_blob(data, None, false).unwrap();
                store.put(hash, &at_rest).await.unwrap();
            }
            // The owner's framed upload lands after it; its chunk puts are
            // skipped because the keys already exist.
            let manifest_hash = seed_synced_file(&store, None, &content, None).await;

            let out = read_file_by_manifest(&db(), &store, None, &manifest_hash, &[])
                .await
                .expect("a raw body addressing the recorded hash is served");
            assert_eq!(out, content, "the public file reads back byte-for-byte");
        }
    }
}
