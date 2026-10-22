//! The share spool as a LOCAL STORE (`file-sync.md` § *A failed change must
//! not strand the device*, the TRANSIENT IS THE DEFAULT paragraph): a spool
//! body is checked against its content name before the walk sees it, and one
//! that does not address its name is an UNMARKED miss — evicted, the row held
//! for a retry — never handed to the walk, whose per-chunk check would class
//! it under the record's signer bound and mark it final.

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::file_download::{
    BlobFetcher, FileDownloadKeys, PredecessorSealKey, RecordSigner,
    download_file_bytes_by_manifest,
};
use fauna_core::identity::ActorKeypair;

use crate::peer_share_store::{SpoolFetcher, spool_chunk_path, spool_manifest_path};

fn predecessor() -> ActorKeypair {
    ActorKeypair::from_secret([0x52; 32])
}

fn predecessor_key() -> BackupKey {
    BackupKey::from_bytes([0x62; 32])
}

/// Spool one file sealed under `root` the way the pump lands it; returns the
/// manifest hash and the chunk store keys.
fn spool_sealed(
    spool: &std::path::Path,
    original: &[u8],
    root: &[u8; 32],
) -> (ContentHash, Vec<ContentHash>) {
    std::fs::create_dir_all(spool.join("manifests")).unwrap();
    std::fs::create_dir_all(spool.join("chunks")).unwrap();
    let plain = fauna_core::chunker::chunk_file(original);
    let mut stored_hashes = Vec::new();
    for (hash, data) in fauna_core::chunker::extract_chunks(original, &plain) {
        let (store_key, ciphertext) = crate::seal::seal_chunk_body(&hash, &data, root).unwrap();
        std::fs::write(spool_chunk_path(spool, &store_key), ciphertext).unwrap();
        stored_hashes.push(store_key);
    }
    let manifest = fauna_core::chunk::ChunkManifest {
        stored_hashes: Some(stored_hashes.clone()),
        ..plain
    };
    let manifest_bytes =
        fauna_core::encoding::canonical_encode(&manifest.wire_form(Some(root)).unwrap()).unwrap();
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);
    std::fs::write(spool_manifest_path(spool, &manifest_hash), &manifest_bytes).unwrap();
    (manifest_hash, stored_hashes)
}

/// The keys a successor host opens a predecessor-signed, unstamped record
/// with — the arm whose failed open `finalize_open` marks `SIGNER_BOUND`.
fn predecessor_signed_keys() -> FileDownloadKeys {
    FileDownloadKeys {
        backup_key: Some(BackupKey::from_bytes([0x61; 32]).into()),
        predecessor_backup_keys: PredecessorSealKey::chain([(
            predecessor().actor_id(),
            predecessor_key(),
        )]),
        record_signer: RecordSigner::Predecessor(predecessor().actor_id()),
        ..Default::default()
    }
}

/// A torn spool body under a predecessor-signed record: before the spool
/// checked its bodies the walk's per-chunk check failed INSIDE the signer
/// bound and the miss came back `SIGNER_BOUND` — a final mark, so the share
/// ingest let the cursor pass the row and the change was lost on this device.
/// It must be an unmarked miss, the torn body evicted, and the row's own
/// bytes must still open once the body is whole again.
#[tokio::test]
async fn a_torn_spool_body_is_an_unmarked_miss_never_a_signer_bound_mark() {
    let spool = tempfile::tempdir().unwrap();
    let body: Vec<u8> = (0..4_000u32).map(|i| (i % 251) as u8).collect();
    let (manifest_hash, store_keys) = spool_sealed(
        spool.path(),
        &body,
        &predecessor_key().convergent_chunk_root(),
    );
    let fetcher = SpoolFetcher::new(spool.path().to_path_buf());
    let keys = predecessor_signed_keys();
    assert_eq!(
        download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, None, "inherited.bin")
            .await
            .expect("a whole spool opens under the signer's own root"),
        body
    );

    let torn = spool_chunk_path(spool.path(), &store_keys[0]);
    let whole = std::fs::read(&torn).unwrap();
    std::fs::write(&torn, &whole[..whole.len() / 2]).unwrap();

    let err =
        download_file_bytes_by_manifest(&fetcher, &keys, manifest_hash, None, "inherited.bin")
            .await
            .expect_err("a torn body must not open");
    assert_eq!(
        fauna_core::apply_failure::permanent_reason(&err),
        None,
        "a local store's mismatch is a transient miss, never a permanent mark: {err:#}"
    );
    assert!(!torn.exists(), "the mismatching body is evicted");
}

/// The manifest twin: a spool manifest that does not hash to its name is an
/// unmarked miss and evicted, exactly as a missing one is answered.
#[tokio::test]
async fn a_torn_spool_manifest_is_an_unmarked_miss() {
    let spool = tempfile::tempdir().unwrap();
    let (manifest_hash, _) = spool_sealed(
        spool.path(),
        b"some bytes",
        &predecessor_key().convergent_chunk_root(),
    );
    let path = spool_manifest_path(spool.path(), &manifest_hash);
    let whole = std::fs::read(&path).unwrap();
    std::fs::write(&path, &whole[..whole.len() - 1]).unwrap();

    let fetcher = SpoolFetcher::new(spool.path().to_path_buf());
    let err = fetcher
        .fetch_manifest(&manifest_hash)
        .await
        .expect_err("a torn manifest is a miss");
    assert_eq!(
        fauna_core::apply_failure::permanent_reason(&err),
        None,
        "{err:#}"
    );
    assert!(!path.exists(), "the mismatching manifest is evicted");
}
