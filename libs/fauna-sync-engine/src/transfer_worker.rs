//! Background transfer processing for the upload queue.
//!
//! [`drain_pending_uploads`] processes pending transfers from the
//! `transfer_queue` table.  It re-reads files from disk, re-chunks to
//! find the target chunk data, applies the same compression and
//! encryption pipeline as `upload_file`, and uploads it.  Completed
//! transfers are removed from the queue.
//!
//! Called at engine startup to resume interrupted uploads, and
//! periodically on the rescan timer for retry.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use fauna_core::chunker::{chunk_file, extract_chunks};
use fauna_core::data::ContentHash;

use crate::db::SyncDb;
use crate::nest_client::SyncClient;

/// A drain-map candidate: the processed (compressed + sealed) chunk bytes and
/// whether they were sealed under the **current** content-key generation.
/// `current_generation` is `true` for every plaintext / `backup_key` chunk (no
/// generation axis) and for content-key candidates sealed under `roots[0]`
/// (current-first — [`crate::engine::SyncEngine::content_drain_roots`]).
struct ProcessedChunk {
    data: Vec<u8>,
    current_generation: bool,
}

/// Process all pending upload transfers.
///
/// Re-reads files from disk, re-chunks to find the matching chunk data,
/// applies compression and encryption (matching the inline upload path),
/// and uploads each pending chunk.  Stale entries (file changed or
/// deleted) are cleaned up automatically.
///
/// Returns the paths whose queued entries matched a **prior** content-key
/// generation:
/// those entries are neither uploaded nor completed here — publishing
/// prior-generation ciphertext *after* a rotate-on-removal would hand the
/// removed member (who holds that generation irrevocably, and chunk GET is
/// unauthenticated by design) content first published post-removal. The engine
/// wrapper re-seals each returned path under the current generation
/// (re-recording the change — the requeue-under-current) and only then
/// completes the stale entries.
pub async fn drain_pending_uploads(
    watch_dir: &Path,
    db: &SyncDb,
    client: &SyncClient,
    content_roots: Option<&[[u8; 32]]>,
    backup_key: Option<&fauna_core::crypto::OwnerSealKey>,
    transfer_pool: &crate::transfer::TransferPool,
) -> Result<Vec<String>> {
    let mut requeue_under_current: Vec<String> = Vec::new();
    let pending = db.eligible_transfers("upload")?;
    if pending.is_empty() {
        return Ok(requeue_under_current);
    }

    tracing::info!(count = pending.len(), "resuming pending uploads");

    // Group by path for efficient re-chunking
    let mut by_path: HashMap<String, Vec<(i64, ContentHash)>> = HashMap::new();
    for entry in &pending {
        by_path
            .entry(entry.path.clone())
            .or_default()
            .push((entry.id, entry.chunk_hash));
    }

    for (rel_path, chunks) in &by_path {
        if !crate::path_guard::is_safe_relative_path(rel_path) {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(rel_path),
                "unsafe path in queue, removing"
            );
            for (id, _) in chunks {
                let _ = db.complete_transfer(*id);
            }
            continue;
        }

        let full_path = watch_dir.join(rel_path);
        // Only the queued store keys matter — the wanted-filter caps the map
        // at the queued set instead of (generations × chunks)
        // residency.
        let wanted: HashSet<ContentHash> = chunks.iter().map(|(_, h)| *h).collect();
        let chunk_map =
            match build_processed_chunk_map(&full_path, content_roots, backup_key, &wanted) {
                Ok(map) => map,
                Err(_) => {
                    // File no longer exists or can't be read — clean up stale entries
                    tracing::warn!(
                        path = %fauna_core::log_redact::log_path(rel_path),
                        "file gone, clearing stale queue entries"
                    );
                    for (id, _) in chunks {
                        let _ = db.complete_transfer(*id);
                    }
                    continue;
                }
            };

        // Collect chunks to upload — CURRENT-generation matches only. A
        // prior-generation match defers the whole path to the engine's
        // requeue-under-current re-seal (see the fn doc); its entries stay
        // queued so a crash before the re-seal lands resumes here.
        let to_upload: Vec<(ContentHash, Vec<u8>)> = chunks
            .iter()
            .filter_map(|(_, hash)| match chunk_map.get(hash) {
                Some(pc) if pc.current_generation => Some((*hash, pc.data.clone())),
                _ => None,
            })
            .collect();
        if chunks
            .iter()
            .any(|(_, hash)| matches!(chunk_map.get(hash), Some(pc) if !pc.current_generation))
        {
            requeue_under_current.push(rel_path.clone());
        }

        let results = transfer_pool
            .upload_chunks(client, &to_upload, rel_path)
            .await;

        // Mark successful uploads as completed; record retry for failures
        for ur in &results {
            if let Some((id, _)) = chunks.iter().find(|(_, h)| *h == ur.hash) {
                if ur.success {
                    db.complete_transfer(*id)?;
                } else {
                    let _ = db.increment_retry(*id);
                }
            }
        }

        // Clean up stale entries: a queued store key absent from `chunk_map` is a
        // chunk no live generation reproduces, so it can never upload — complete
        // it to unblock the queue.
        //
        // INFO-2:
        // dropping is correct ONLY because `content_roots` carries a candidate per
        // **retained** generation (current-first, the whole back-catalogue — see
        // `build_processed_chunk_map`'s doc), so `chunk_map` reproduces every
        // still-openable store key. A `false` here therefore means "no retained
        // generation reseals to this key" — genuinely unreachable content — not
        // "the owner happened to prune the generation that made it." That premise
        // (the owner keeps the full generation history — `FolderContentKeys` never
        // drops a `prior`) is the load-bearing invariant; were a generation ever
        // pruned, a still-wanted chunk queued under it would be silently dropped
        // here instead of requeued. It holds today by construction; this comment
        // pins the dependency the code cannot otherwise express.
        for (id, hash) in chunks {
            if !chunk_map.contains_key(hash) {
                db.complete_transfer(*id)?;
            }
        }
    }

    Ok(requeue_under_current)
}

/// Read a file and build a **store-key → processed-data** map of its chunks.
///
/// The returned data has the same compression and encryption applied as the
/// inline upload path in `engine::upload_file`, so that resumed uploads produce
/// identical bytes on the server. Each entry is keyed by the **store key** the
/// upload path enqueued it under — the plaintext hash for plaintext chunks, the
/// **ciphertext** hash for sealed chunks (content-key bound sets AND owner
/// `backup_key` sets, both convergent) — so the drain loop's `transfer_queue`
/// lookups (keyed by store key) hit rather than miss.
///
/// `content_roots` carries a candidate root per **retained content-key
/// generation** (current-first), not just the current one: a rotate-on-removal
/// can land between the enqueue and the drain, and an entry enqueued under a
/// now-prior generation only matches a map candidate sealed under that same
/// generation — re-sealing solely under the new current key would produce
/// different ciphertext (different store keys), every queued lookup would
/// miss, and the stale-entry cleanup would silently drop the never-uploaded
/// chunks (the STOREKEY-GAP failure mode via generation mismatch). Store-key
/// equality picks the right candidate, exactly as the AEAD tag does for
/// `FolderContentKeys::keys_for`; the marked [`ProcessedChunk::current_generation`]
/// tells the drain whether the match may upload (current) or must requeue
/// under current (prior — see [`drain_pending_uploads`]).
///
/// `wanted` (the path's queued store keys) bounds retention: candidates are
/// sealed **one generation at a time** and only wanted matches are kept, so
/// peak residency is one generation's ciphertexts (transient) plus the
/// retained matches — not (generations × chunks). The
/// filter is behavior-preserving for the drain: it only ever looks up queued
/// store keys, and a queued key is in the filtered map iff it matched any
/// candidate.
fn build_processed_chunk_map(
    path: &Path,
    content_roots: Option<&[[u8; 32]]>,
    backup_key: Option<&fauna_core::crypto::OwnerSealKey>,
    wanted: &HashSet<ContentHash>,
) -> Result<HashMap<ContentHash, ProcessedChunk>> {
    let data = std::fs::read(path).with_context(|| {
        format!(
            "reading {}",
            fauna_core::log_redact::log_path(&path.to_string_lossy())
        )
    })?;
    let manifest = chunk_file(&data);
    let chunks = extract_chunks(&data, &manifest);

    // Apply the same pipeline as engine::upload_file, through the one seal
    // door (`fauna_core::chunk_seal`): frame ONCE, then seal per root —
    // backup_key takes priority over content_roots.
    let framed = fauna_core::chunk_seal::frame_chunks(&chunks)?;

    let mut map: HashMap<ContentHash, ProcessedChunk> = HashMap::new();
    if let Some(key) = backup_key {
        // Owner-only backup: the convergent `chunk_crypto` seal under the
        // `BackupKey`-derived root, re-keyed by the **ciphertext** hash —
        // exactly what `engine::upload_chunked_bytes`'s `effective_backup_key`
        // arm enqueues (FS-BIND FOLLOW-ON A) — so the ciphertext-keyed queue
        // entry matches this map. The AEAD is deterministic, so this
        // reproduces the exact ciphertext (hence store key) the upload used.
        // (`BackupKey` is seed-derived and never rotates — one candidate,
        // always "current".)
        let root = key.convergent_chunk_root();
        for chunk in &framed {
            let (sk, ct) = fauna_core::chunk_seal::seal_framed_chunk(chunk, &root)?;
            if wanted.contains(&sk) {
                map.insert(
                    sk,
                    ProcessedChunk {
                        data: ct,
                        current_generation: true,
                    },
                );
            }
        }
        // PLAINTEXT queue entries — uploads queued under a `public` audience
        // and still pending after a public-to-private flip,
        // store-keyed by the plaintext hash. Without a candidate here the stale-entry cleanup
        // would silently drop them (the STOREKEY-GAP failure mode via the
        // plaintext→BackupKey upgrade); publishing them as-is would rest the
        // user's content plaintext after the flip-back. So they match as
        // `current_generation: false` — the requeue-under-current leg re-seals
        // the path under the BackupKey root and supersedes the stale entries,
        // exactly like a prior-generation content-key match.
        for chunk in &framed {
            let h = chunk.plain_hash();
            if wanted.contains(h) {
                map.entry(*h).or_insert(ProcessedChunk {
                    data: chunk.body().to_vec(),
                    current_generation: false,
                });
            }
        }
    } else if let Some(roots) = content_roots {
        // Content-key (bound) set: seal each chunk under EVERY retained
        // generation root, then **re-key each candidate by its ciphertext
        // hash** — the same store key the upload path enqueues
        // (`engine::upload_chunked_bytes` / the streaming path record it in
        // `manifest.stored_hashes`; `mls-group-key-material.md` § M2, FS-BIND
        // PIECE 6). Keying by the plaintext hash would make the drain map
        // miss the ciphertext-keyed `transfer_queue` entry, and sealing only
        // under the current generation would miss entries enqueued before an
        // intervening rotation — either way `drain_pending_uploads` would
        // silently `complete_transfer` (drop) the never-uploaded
        // chunk. The AEAD is deterministic (key+nonce
        // derive from the root and the plaintext hash), so each candidate
        // reproduces the exact ciphertext — hence store key — an upload
        // under that generation used. `roots` is current-first; a
        // (theoretical) duplicate store key keeps its first — current —
        // classification.
        for (i, root) in roots.iter().enumerate() {
            for chunk in &framed {
                let (sk, ct) = fauna_core::chunk_seal::seal_framed_chunk(chunk, root)?;
                if wanted.contains(&sk) {
                    map.entry(sk).or_insert(ProcessedChunk {
                        data: ct,
                        current_generation: i == 0,
                    });
                }
            }
        }
    } else {
        // Plaintext: the store key IS the plaintext chunk hash; the framed
        // plaintext is the stored body.
        for chunk in framed {
            let h = *chunk.plain_hash();
            if wanted.contains(&h) {
                map.insert(
                    h,
                    ProcessedChunk {
                        data: chunk.into_body(),
                        current_generation: true,
                    },
                );
            }
        }
    }

    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact processed-candidate store keys the drain map would produce for
    /// `data` under `root` — the chunk → seal → ciphertext-hash pipeline of
    /// [`build_processed_chunk_map`]'s content-key arm, taken through the
    /// per-chunk door (`seal_chunk_bodies`) rather than the frame-once /
    /// seal-per-root two-step the map uses, so the two shapes are pinned equal.
    fn sealed_store_keys(data: &[u8], root: &[u8; 32]) -> Vec<ContentHash> {
        let manifest = chunk_file(data);
        let chunks = extract_chunks(data, &manifest);
        fauna_core::chunk_seal::seal_chunk_bodies(&chunks, root)
            .unwrap()
            .into_iter()
            .map(|(store_key, _)| store_key)
            .collect()
    }

    /// The map
    /// retains ONLY the wanted (queued) store keys — never one entry per
    /// (generation × chunk) — and marks each retained candidate with whether it
    /// was sealed under the CURRENT (first) root, which is what routes a
    /// prior-generation match to the requeue instead of an upload.
    #[test]
    fn map_retains_only_wanted_keys_and_marks_current_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let data: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let gen2 = [9u8; 32]; // current (roots are current-first)
        let gen1 = [7u8; 32]; // prior
        let roots = [gen2, gen1];
        let gen2_keys = sealed_store_keys(&data, &gen2);
        let gen1_keys = sealed_store_keys(&data, &gen1);
        assert!(!gen2_keys.is_empty() && gen1_keys[0] != gen2_keys[0]);

        // Want one gen-1 key (a pre-rotation queue entry) and one gen-2 key.
        let wanted: HashSet<ContentHash> = [gen1_keys[0], gen2_keys[0]].into_iter().collect();
        let map = build_processed_chunk_map(&path, Some(&roots), None, &wanted).unwrap();

        assert_eq!(
            map.len(),
            wanted.len(),
            "only wanted keys are retained — no (generation × chunk) residency"
        );
        assert!(
            map.get(&gen2_keys[0]).unwrap().current_generation,
            "a current-root match is marked current (uploadable)"
        );
        assert!(
            !map.get(&gen1_keys[0]).unwrap().current_generation,
            "a prior-root match is marked prior (requeue-under-current)"
        );

        // The plaintext arm keys by plaintext hash and is always current.
        let manifest = chunk_file(&data);
        let plain_hash = extract_chunks(&data, &manifest)[0].0;
        let wanted_plain: HashSet<ContentHash> = [plain_hash].into_iter().collect();
        let plain_map = build_processed_chunk_map(&path, None, None, &wanted_plain).unwrap();
        assert!(plain_map.get(&plain_hash).unwrap().current_generation);
    }
}
