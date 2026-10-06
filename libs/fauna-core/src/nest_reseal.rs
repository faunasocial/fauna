//! The **nest-sourced, windowed re-seal** of one recorded head — the byte half
//! every re-seal driver shares, whatever holds the change log.
//!
//! Two drivers move a file's at-rest seal from the root it was recorded under
//! to a set's current one without a local copy:
//!
//! - the sync engine's pass (`fauna_sync_engine::SyncEngine::reseal_pending_under_current`,
//!   its nest-bytes source) — `mls-group-key-material.md` § M2 → *Pre-bind
//!   re-seal migration*, part (D), and the owner-key succession re-seal;
//! - the flipping client's served-set walk (`fauna_media_machine::served_reseal`)
//!   — `webdav-server.md` § Key model, the app-side seams bullet, part (c): a
//!   file no driven engine holds, re-sealed by the app that served its set.
//!
//! Both need the same bytes: the head's manifest opened under whichever root
//! its stamp selects ([`ManifestWalk`] — the owner root for an owner's
//! unstamped record only when the record verified as the holder's own,
//! [`FileDownloadKeys::owner_signed_record`]), every chunk re-sealed through
//! the one per-chunk pipeline ([`crate::chunk_seal::seal_chunk_body`]) and the
//! new manifest posted only over stored chunks. Written once here so the two
//! cannot drift on the one property that makes them safe to run side by side:
//! the deterministic seal converges them on the **same store keys and the same
//! manifest hash** — dedup, never conflict.
//!
//! What stays with each driver is where the bytes go ([`ChunkStoreSink`]) and
//! everything around the byte move — which heads are owed, the change record,
//! the verify under the current root alone, the supersede.

use anyhow::{Context, Result};

use crate::chunk::ChunkManifest;
use crate::data::ContentHash;
use crate::file_download::{BlobFetcher, FileDownloadKeys, ManifestWalk, verify_file_by_manifest};

/// Where a re-seal's sealed artifacts go — the chunk-store write legs, as one
/// seam. The sync engine binds it to its pooled transfer path, the Media page's
/// seam to the byte routes it uploads through
/// (`POST /api/v1/chunks/{key}`, `POST /api/v1/manifests/{hash}`).
///
/// `MaybeSendSync` + the dual `async_trait` arm, exactly as [`BlobFetcher`].
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait ChunkStoreSink: crate::MaybeSendSync {
    /// Store one window of sealed chunk bodies, `(store key, body)`, and
    /// answer how many were actually uploaded (the rest were already stored).
    /// Idempotent: the store is content-addressed and the seal deterministic,
    /// so a re-run re-puts the same keys. `relative_path` is error context
    /// only.
    async fn put_chunks(
        &self,
        bodies: Vec<(ContentHash, Vec<u8>)>,
        relative_path: &str,
    ) -> Result<usize>;

    /// Store the canonical manifest under `manifest_hash` — called only after
    /// every chunk it names was stored.
    async fn put_manifest(&self, manifest_hash: ContentHash, manifest_bytes: Vec<u8>)
    -> Result<()>;
}

/// What one served-set walk did — "counted", in the verified-row ruling's
/// words.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServedSetConvergence {
    /// Heads re-sealed, verified and recorded at the current generation.
    pub resealed: usize,
    /// Heads not re-sealed on purpose: not a record this seat may open (the
    /// judge refused it, or an unstamped head that is not this account's own
    /// verified record), or a path that does not open. Warned.
    pub skipped: usize,
    /// Heads whose re-seal failed; the next walk retries them. Warned.
    pub failed: usize,
}

/// **The flipping client's served-set walk**, as the seam the serve
/// composition calls (`webdav-server.md` § Key model, the app-side seams
/// bullet, part (c)): converge one just-served set's nest-resident heads onto
/// its current content-key generation. Implemented over a seat's session by
/// `fauna_media_machine::served_reseal`; driven by
/// `fauna_client_folders::FoldersAuthor::serve_set`, which knows nothing of
/// bytes.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait ServedSetConverge: crate::MaybeSendSync {
    /// Converge `folder`. An error means the walk could not start (custody not
    /// synced, the change log unreadable); per-head failures are in the tally.
    async fn converge_served_set(&self, folder: &str) -> Result<ServedSetConvergence>;
}

/// The detail a restore reports when the version was recorded under a
/// previous identity of this account and its bytes do not open under any key
/// that identity could have sealed them with (`writer-signed-change-records.md`
/// ruling (8)(d)): the version is not restorable, and says so — it is never
/// re-signed unopened. One text for every door that opens before it restores
/// (the Media restore and the sync engine's, ruling (10)(b)).
pub const RESTORE_INHERITED_UNOPENABLE: &str = "this version was recorded under a previous \
     identity of this account and does not open under any key that identity held — it cannot \
     be restored";

/// What one [`reseal_nest_copy_windowed`] produced.
#[derive(Debug, Clone)]
pub struct ResealedManifest {
    /// The new manifest's plaintext view (the source's plaintext chunk list,
    /// the new store keys); the uploaded bytes are its
    /// [`ChunkManifest::wire_form`] under the new root.
    pub manifest: ChunkManifest,
    /// Its canonical content address — what the change record names.
    pub manifest_hash: ContentHash,
    /// Chunks actually uploaded (the rest were already stored).
    pub uploaded_count: usize,
    /// The generation the chunks sealed under, to stamp into the record;
    /// `None` for an owner-root or plaintext seal.
    pub content_key_version: Option<u64>,
}

/// Re-seal the nest's copy of one recorded head under `seal_root`, **one window
/// at a time**: peak memory is O(window × chunk), never O(file) — a capability
/// host re-seals under an extension's memory cap (part (D)).
///
/// 1. **Verify the whole source first**, in its own bounded walk, before one
///    chunk is sealed under the new root. A chunk re-sealed under a set's
///    generation is readable by every holder of that generation the moment it
///    is stored, whether or not a manifest ever names it — so a manifest that
///    lists real chunks under a false file address is refused before the first
///    upload, not after. Twice the fetch, O(window) memory either way.
/// 2. **Re-seal window by window**, each chunk's length checked against the
///    manifest's size table ([`ManifestWalk::window`]), the new manifest
///    keeping the source's plaintext chunk list (the chunking is a function of
///    the content, which is unchanged) with the new store keys — exactly the
///    manifest [`crate::blob_seal::seal_blob`] builds from the whole buffer, so
///    a re-run converges on the identical manifest hash.
/// 3. **Post the manifest only after** the running whole-file hash equals the
///    source's address: a nest that served other chunks under this head gets
///    nothing recorded.
///
/// `seal_root`: `(secret, generation)` — `None` frames the chunks plaintext
/// (a declassified folder's shape).
pub async fn reseal_nest_copy_windowed(
    fetcher: &dyn BlobFetcher,
    keys: &FileDownloadKeys,
    manifest_hash: ContentHash,
    content_key_version: Option<u64>,
    relative_path: &str,
    seal_root: Option<([u8; 32], Option<u64>)>,
    sink: &dyn ChunkStoreSink,
) -> Result<ResealedManifest> {
    // Redacted once and reused by every error string below
    // (`path-sealing.md` § Sealed names & paths, S7).
    let path_r = crate::log_redact::log_path(relative_path);
    verify_file_by_manifest(
        fetcher,
        keys,
        manifest_hash,
        content_key_version,
        relative_path,
    )
    .await
    .context("re-seal: the nest copy does not verify against its recorded head")?;
    let walk = ManifestWalk::open(
        fetcher,
        keys,
        manifest_hash,
        content_key_version,
        relative_path,
    )
    .await?;
    let mut hasher = blake3::Hasher::new();
    let mut total = 0u64;
    let mut stored = Vec::with_capacity(walk.manifest().chunk_hashes.len());
    let mut uploaded_count = 0usize;
    for i in 0..walk.window_count() {
        let mut bodies = Vec::with_capacity(ManifestWalk::WINDOW);
        for (plain_hash, plain) in walk.window(fetcher, i, relative_path).await? {
            hasher.update(&plain);
            total += plain.len() as u64;
            let (store_key, body) = match seal_root {
                Some((secret, _)) => {
                    crate::chunk_seal::seal_chunk_body(&plain_hash, &plain, &secret)?
                }
                None => (
                    plain_hash,
                    crate::chunk_seal::FramedChunk::frame(&plain_hash, &plain)?.into_body(),
                ),
            };
            stored.push(store_key);
            bodies.push((store_key, body));
        }
        uploaded_count += sink
            .put_chunks(bodies, relative_path)
            .await
            .with_context(|| format!("re-seal: storing the re-sealed chunks of {path_r}"))?;
    }
    let source = walk.manifest();
    let actual = ContentHash::from_digest_raw(*hasher.finalize().as_bytes());
    if actual != source.file_hash || total != source.total_size {
        anyhow::bail!(
            "re-seal: the nest copy of {path_r} does not address its recorded head \
             (expected {}, got {}) — nothing recorded",
            hex::encode(source.file_hash.digest()),
            hex::encode(actual.digest())
        );
    }
    let manifest = ChunkManifest {
        file_hash: source.file_hash,
        total_size: source.total_size,
        chunk_hashes: source.chunk_hashes.clone(),
        chunk_sizes: source.chunk_sizes.clone(),
        stored_hashes: seal_root.map(|_| stored),
        sealed_hashes: None,
        min_reader: None,
    };
    let manifest_bytes = crate::encoding::canonical_encode(
        &manifest.wire_form(seal_root.map(|(secret, _)| secret).as_ref())?,
    )
    .context("re-seal: serializing manifest")?;
    let new_hash = ContentHash::of_raw(&manifest_bytes);
    sink.put_manifest(new_hash, manifest_bytes)
        .await
        .context("re-seal: uploading manifest")?;
    Ok(ResealedManifest {
        manifest,
        manifest_hash: new_hash,
        uploaded_count,
        content_key_version: seal_root.and_then(|(_, version)| version),
    })
}
