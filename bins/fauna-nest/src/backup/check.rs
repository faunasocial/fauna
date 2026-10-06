//! Integrity verification for backup snapshots.
//!
//! Walks all snapshots for a folder, verifies that every referenced
//! manifest and chunk exists in the blob store.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_core::data::ContentHash;
use fauna_protocol::ByteBuf;
use fauna_protocol::filesync::SnapshotCheckError;

use crate::blob_store::BlobStoreBackend;
use crate::db::CacheDb;

/// Result of an integrity check.
#[derive(Debug, Default)]
pub struct CheckResult {
    pub snapshots_checked: usize,
    pub files_checked: usize,
    pub manifests_checked: usize,
    pub chunks_checked: usize,
    pub missing_manifests: usize,
    pub missing_chunks: usize,
    pub corrupt_manifests: usize,
    /// Human-readable — and, since S7, hash-only: never carries a plaintext
    /// path (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
    pub errors: Vec<String>,
    /// Structured twin of `errors`, same order, one entry per finding.
    pub structured_errors: Vec<SnapshotCheckError>,
}

/// A file's `path_hash` companion for a structured finding — the stored
/// column — the PK component since the S9 flip rebuilt `snapshot_files` and
/// dropped the plaintext `path` (v32).
fn file_path_hash(file: &crate::db::SnapshotFileRow) -> ByteBuf {
    ByteBuf::from(file.path_hash.clone())
}

/// Check integrity of all snapshots for a folder.
///
/// When `verify_content` is true, downloads and re-hashes every chunk
/// (slow but thorough). When false, only checks existence.
///
/// When `encryption_key` is `Some`, stored blobs are decrypted before
/// re-hashing so we verify `blake3(decrypt(stored_blob)) == expected_hash`.
///
pub async fn integrity_check(
    db: &Arc<CacheDb>,
    blob_store: &Arc<dyn BlobStoreBackend>,
    folder_id: i64,
    verify_content: bool,
    encryption_key: Option<&fauna_core::crypto::BackupKey>,
) -> Result<CheckResult> {
    let mut result = CheckResult::default();

    let snapshots = db
        .list_snapshots(folder_id)
        .await
        .context("listing snapshots")?;
    result.snapshots_checked = snapshots.len();

    // Collect unique manifest hashes across all snapshots
    let mut seen_manifests: HashSet<[u8; 32]> = HashSet::new();

    for snap in &snapshots {
        let files = db
            .get_snapshot_files(snap.id)
            .await
            .context("getting snapshot files")?;
        result.files_checked += files.len();

        for file in &files {
            let hash_arr: [u8; 32] = match file.manifest_hash.as_slice().try_into() {
                Ok(h) => h,
                Err(_) => {
                    result.errors.push(format!(
                        "snapshot {}: file {} has invalid manifest hash length",
                        snap.id,
                        fauna_core::log_redact::log_hash_prefix("path", &file.path_hash)
                    ));
                    result.structured_errors.push(SnapshotCheckError {
                        kind: "invalid_manifest_hash_length".to_string(),
                        snapshot_id: Some(snap.id),
                        path_hash: Some(file_path_hash(file)),
                        manifest_hash: None,
                        chunk_hash: None,
                        ..Default::default()
                    });
                    continue;
                }
            };

            if seen_manifests.contains(&hash_arr) {
                continue; // Already checked this manifest
            }
            seen_manifests.insert(hash_arr);
            result.manifests_checked += 1;

            let manifest_hash = ContentHash::from_digest_raw(hash_arr);
            let manifest_hash_buf = ByteBuf::from(hash_arr.to_vec());

            // Check manifest exists
            let manifest_bytes = match blob_store.get(&manifest_hash).await {
                Ok(Some(b)) => b,
                Ok(None) => {
                    result.missing_manifests += 1;
                    result.errors.push(format!(
                        "snapshot {}: missing manifest {} for {}",
                        snap.id,
                        hex::encode(hash_arr),
                        fauna_core::log_redact::log_hash_prefix("path", &file.path_hash)
                    ));
                    result.structured_errors.push(SnapshotCheckError {
                        kind: "missing_manifest".to_string(),
                        snapshot_id: Some(snap.id),
                        path_hash: Some(file_path_hash(file)),
                        manifest_hash: Some(manifest_hash_buf.clone()),
                        chunk_hash: None,
                        ..Default::default()
                    });
                    continue;
                }
                Err(e) => {
                    result.errors.push(format!(
                        "snapshot {}: error reading manifest {}: {e}",
                        snap.id,
                        hex::encode(hash_arr)
                    ));
                    result.structured_errors.push(SnapshotCheckError {
                        kind: "manifest_read_error".to_string(),
                        snapshot_id: Some(snap.id),
                        path_hash: None,
                        manifest_hash: Some(manifest_hash_buf.clone()),
                        chunk_hash: None,
                        ..Default::default()
                    });
                    continue;
                }
            };

            // Decode manifest: decrypt (if encrypted) then decompress.
            let manifest_bytes = match super::decode_blob(&manifest_bytes, encryption_key) {
                Ok(pt) => pt,
                Err(e) => {
                    result.corrupt_manifests += 1;
                    result.errors.push(format!(
                        "snapshot {}: manifest {} decode failed: {e}",
                        snap.id,
                        hex::encode(hash_arr)
                    ));
                    result.structured_errors.push(SnapshotCheckError {
                        kind: "manifest_decode_failed".to_string(),
                        snapshot_id: Some(snap.id),
                        path_hash: None,
                        manifest_hash: Some(manifest_hash_buf.clone()),
                        chunk_hash: None,
                        ..Default::default()
                    });
                    continue;
                }
            };

            // Deserialize manifest
            let manifest: fauna_core::chunk::ChunkManifest =
                match fauna_core::encoding::canonical_decode(&manifest_bytes) {
                    Ok(m) => m,
                    Err(_) => {
                        result.corrupt_manifests += 1;
                        result.errors.push(format!(
                            "snapshot {}: manifest {} deserialization failed",
                            snap.id,
                            hex::encode(hash_arr)
                        ));
                        result.structured_errors.push(SnapshotCheckError {
                            kind: "manifest_deserialize_failed".to_string(),
                            snapshot_id: Some(snap.id),
                            path_hash: None,
                            manifest_hash: Some(manifest_hash_buf.clone()),
                            chunk_hash: None,
                            ..Default::default()
                        });
                        continue;
                    }
                };

            // Check each chunk by its STORE key (the ciphertext hash for a
            // sealed chunk, else the plaintext hash). The nest holds no chunk
            // root, so a sealed chunk can only be checked for existence — never
            // decoded to verify its plaintext content hash (FS-BIND, PIECE 6) —
            // and a sealed manifest names no plaintext hash at all (its
            // `chunk_hashes` ride sealed, `mls-group-key-material.md` § M2
            // *Sealed manifest hashes*): the walk is over `store_keys()`, and a
            // plaintext chunk's store key IS its plaintext hash.
            let encrypted = manifest.stored_hashes.is_some();
            for store_key in manifest.store_keys().iter() {
                result.chunks_checked += 1;

                if verify_content && !encrypted {
                    match blob_store.get(store_key).await {
                        Ok(Some(stored)) => {
                            // Decode: decrypt (if encrypted) then decompress.
                            // The content hash is always over the original
                            // uncompressed plaintext.
                            let plaintext = match super::decode_blob(&stored, encryption_key) {
                                Ok(pt) => pt,
                                Err(e) => {
                                    result.errors.push(format!(
                                        "chunk {}: decode failed: {e}",
                                        hex::encode(store_key.digest())
                                    ));
                                    result.structured_errors.push(SnapshotCheckError {
                                        kind: "chunk_decode_failed".to_string(),
                                        snapshot_id: Some(snap.id),
                                        path_hash: None,
                                        manifest_hash: Some(manifest_hash_buf.clone()),
                                        chunk_hash: Some(ByteBuf::from(
                                            store_key.digest().to_vec(),
                                        )),
                                        ..Default::default()
                                    });
                                    continue;
                                }
                            };
                            let actual = ContentHash::of_raw(&plaintext);
                            if actual != *store_key {
                                result.errors.push(format!(
                                    "chunk {}: content hash mismatch",
                                    hex::encode(store_key.digest())
                                ));
                                result.structured_errors.push(SnapshotCheckError {
                                    kind: "chunk_hash_mismatch".to_string(),
                                    snapshot_id: Some(snap.id),
                                    path_hash: None,
                                    manifest_hash: Some(manifest_hash_buf.clone()),
                                    chunk_hash: Some(ByteBuf::from(store_key.digest().to_vec())),
                                    ..Default::default()
                                });
                            }
                        }
                        Ok(None) => {
                            result.missing_chunks += 1;
                            result.errors.push(format!(
                                "snapshot {}: missing chunk {} (manifest {})",
                                snap.id,
                                hex::encode(store_key.digest()),
                                hex::encode(hash_arr)
                            ));
                            result.structured_errors.push(SnapshotCheckError {
                                kind: "missing_chunk".to_string(),
                                snapshot_id: Some(snap.id),
                                path_hash: None,
                                manifest_hash: Some(manifest_hash_buf.clone()),
                                chunk_hash: Some(ByteBuf::from(store_key.digest().to_vec())),
                                ..Default::default()
                            });
                        }
                        Err(e) => {
                            result.errors.push(format!(
                                "error reading chunk {}: {e}",
                                hex::encode(store_key.digest())
                            ));
                            result.structured_errors.push(SnapshotCheckError {
                                kind: "chunk_read_error".to_string(),
                                snapshot_id: Some(snap.id),
                                path_hash: None,
                                manifest_hash: Some(manifest_hash_buf.clone()),
                                chunk_hash: Some(ByteBuf::from(store_key.digest().to_vec())),
                                ..Default::default()
                            });
                        }
                    }
                } else {
                    // Existence-only: content verification is off, or the chunk is
                    // content-key-encrypted (the nest cannot decode it to verify).
                    match blob_store.exists(store_key).await {
                        Ok(true) => {}
                        Ok(false) => {
                            result.missing_chunks += 1;
                            result.errors.push(format!(
                                "snapshot {}: missing chunk {} (manifest {})",
                                snap.id,
                                hex::encode(store_key.digest()),
                                hex::encode(hash_arr)
                            ));
                            result.structured_errors.push(SnapshotCheckError {
                                kind: "missing_chunk".to_string(),
                                snapshot_id: Some(snap.id),
                                path_hash: None,
                                manifest_hash: Some(manifest_hash_buf.clone()),
                                chunk_hash: Some(ByteBuf::from(store_key.digest().to_vec())),
                                ..Default::default()
                            });
                        }
                        Err(e) => {
                            result.errors.push(format!(
                                "error checking chunk {}: {e}",
                                hex::encode(store_key.digest())
                            ));
                            result.structured_errors.push(SnapshotCheckError {
                                kind: "chunk_check_error".to_string(),
                                snapshot_id: Some(snap.id),
                                path_hash: None,
                                manifest_hash: Some(manifest_hash_buf.clone()),
                                chunk_hash: Some(ByteBuf::from(store_key.digest().to_vec())),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
    }

    let status = if result.errors.is_empty() {
        "OK"
    } else {
        "ERRORS"
    };
    tracing::info!(
        status,
        snapshots = result.snapshots_checked,
        files = result.files_checked,
        manifests = result.manifests_checked,
        chunks = result.chunks_checked,
        missing_manifests = result.missing_manifests,
        missing_chunks = result.missing_chunks,
        "integrity check complete"
    );

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_store::DiskBlobStore;
    use fauna_core::data::ContentHash;

    #[tokio::test]
    async fn check_reports_missing_manifest() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());

        let actor_id = [1u8; 32];
        db.create_folder("check-test", &actor_id).await.unwrap();
        let fs = db.get_folder("check-test").await.unwrap().unwrap();

        // Record a change referencing a manifest hash that doesn't exist in blob store
        let missing_hash = [0xFFu8; 32];
        let ph: [u8; 32] = *blake3::hash(b"missing.txt").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&missing_hash),
            100,
            "create",
            Some(fs.id),
            None,
            Some("missing.txt"),
        )
        .await
        .unwrap();
        let snap = db.create_snapshot(fs.id).await.unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = integrity_check(&db, &dyn_store, snap.folder_id, false, None)
            .await
            .unwrap();
        assert_eq!(result.missing_manifests, 1);
        assert!(!result.errors.is_empty());
    }

    #[tokio::test]
    async fn check_passes_with_all_blobs_present() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());

        let actor_id = [1u8; 32];
        db.create_folder("check-ok", &actor_id).await.unwrap();
        let fs = db.get_folder("check-ok").await.unwrap().unwrap();

        // Store a manifest blob
        let manifest_hash = ContentHash::from_digest_raw([0xAAu8; 32]);
        // Create a valid ChunkManifest
        let manifest = fauna_core::chunk::ChunkManifest {
            file_hash: ContentHash::from_digest_raw([0xCCu8; 32]),
            total_size: 10,
            chunk_hashes: vec![ContentHash::from_digest_raw([0xDDu8; 32])],
            chunk_sizes: vec![10],
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        let manifest_bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
        store.put(&manifest_hash, &manifest_bytes).await.unwrap();

        // Store the chunk
        let chunk_hash = ContentHash::from_digest_raw([0xDDu8; 32]);
        store.put(&chunk_hash, b"chunk data").await.unwrap();

        // Record change
        let ph: [u8; 32] = *blake3::hash(b"ok.txt").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&manifest_hash.digest()),
            10,
            "create",
            Some(fs.id),
            None,
            Some("ok.txt"),
        )
        .await
        .unwrap();
        let _snap = db.create_snapshot(fs.id).await.unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = integrity_check(&db, &dyn_store, fs.id, false, None)
            .await
            .unwrap();
        assert_eq!(result.missing_manifests, 0);
        assert_eq!(result.missing_chunks, 0);
        assert!(result.errors.is_empty());
    }

    /// A sealed-only manifest names no plaintext chunk hash (they ride in
    /// `sealed_hashes`, `mls-group-key-material.md` § M2 *Sealed manifest
    /// hashes*), so the check walks its store keys: every chunk is counted,
    /// and a missing one is found — never a silent clean over zero chunks.
    #[tokio::test]
    async fn a_sealed_only_manifest_is_checked_chunk_by_chunk() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());

        let actor_id = [1u8; 32];
        db.create_folder("check-sealed", &actor_id).await.unwrap();
        let fs = db.get_folder("check-sealed").await.unwrap().unwrap();

        let sealed = fauna_core::blob_seal::seal_blob(
            &vec![7u8; 9 * 1024 * 1024],
            Some(([0x5Au8; 32], Some(1))),
        )
        .unwrap();
        assert!(sealed.chunks.len() >= 2, "a multi-chunk fixture");
        let wire: fauna_core::chunk::ChunkManifest =
            fauna_core::encoding::canonical_decode(&sealed.manifest_bytes).unwrap();
        assert!(wire.chunk_hashes.is_empty(), "the fixture is sealed-only");
        store
            .put(&sealed.manifest_hash, &sealed.manifest_bytes)
            .await
            .unwrap();
        // Every chunk but the last is stored.
        for (key, body) in &sealed.chunks[..sealed.chunks.len() - 1] {
            store.put(key, body).await.unwrap();
        }

        let ph: [u8; 32] = *blake3::hash(b"sealed.bin").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&sealed.manifest_hash.digest()),
            9 * 1024 * 1024,
            "create",
            Some(fs.id),
            None,
            Some("sealed.bin"),
        )
        .await
        .unwrap();
        let _snap = db.create_snapshot(fs.id).await.unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = integrity_check(&db, &dyn_store, fs.id, true, None)
            .await
            .unwrap();
        assert_eq!(result.chunks_checked, sealed.chunks.len());
        assert_eq!(result.missing_chunks, 1);
    }

    /// S7 (the log + error-string scrub, `file-sync.md` § Sealed names &
    /// paths): `integrity_check`'s missing-manifest finding used to
    /// interpolate the file's plaintext path directly into `errors`. It must
    /// never do that again, and the new `structured_errors` twin must carry
    /// the file's `path_hash` instead so a client can still join it against
    /// its own decrypted snapshot listing.
    #[tokio::test]
    async fn missing_manifest_finding_never_leaks_the_plaintext_path() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());

        let actor_id = [1u8; 32];
        db.create_folder("check-leak-test", &actor_id)
            .await
            .unwrap();
        let fs = db.get_folder("check-leak-test").await.unwrap().unwrap();

        let path = "taxes/eviction_notice.pdf";
        let missing_hash = [0xFFu8; 32];
        let ph: [u8; 32] = fauna_core::sync::path_hash(path);
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&missing_hash),
            100,
            "create",
            Some(fs.id),
            None,
            Some(path),
        )
        .await
        .unwrap();
        let snap = db.create_snapshot(fs.id).await.unwrap();

        let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
        let result = integrity_check(&db, &dyn_store, snap.folder_id, false, None)
            .await
            .unwrap();

        assert_eq!(result.missing_manifests, 1);
        assert!(!result.errors.is_empty());
        for e in &result.errors {
            assert!(!e.contains("taxes"), "leaked path in errors: {e}");
            assert!(!e.contains("eviction_notice"), "leaked path in errors: {e}");
        }
        assert!(
            result.errors[0].contains(&fauna_core::log_redact::log_path(path)),
            "expected the redacted path-hash form in {:?}",
            result.errors
        );

        assert_eq!(result.structured_errors.len(), 1);
        let finding = &result.structured_errors[0];
        assert_eq!(finding.kind, "missing_manifest");
        assert_eq!(finding.snapshot_id, Some(snap.id));
        assert_eq!(
            finding.path_hash.as_ref().map(|b| b.to_vec()),
            Some(ph.to_vec())
        );
        assert_eq!(
            finding.manifest_hash.as_ref().map(|b| b.to_vec()),
            Some(missing_hash.to_vec())
        );
    }
}
