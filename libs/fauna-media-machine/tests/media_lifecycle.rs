//! `MediaMachine` lifecycle tests over the in-memory `FakeMediaNestApi` — the
//! page-level analogue of `fauna-devices-machine`'s machine tests. Drives the
//! machine with no transport and asserts: refresh populates the rendered page,
//! the view-state setters re-derive it, the empty state, and a refresh failure
//! keeps prior data + surfaces the error.

use std::sync::Arc;

use fauna_core::crypto::{BackupKey, encrypt_backup_chunk};
use fauna_media_machine::nest_api::MediaApiError;
use fauna_media_machine::nest_api::fake::FAKE_PREDECESSOR_ID;
use fauna_media_machine::observer::CountingObserver;
use fauna_media_machine::{
    FakeMediaBlobFetcher, FakeMediaBlobUploader, FakeMediaNestApi, FolderKeyResolver,
    MediaBlobFetcher, MediaBlobUploader, MediaFolder, MediaItem, MediaMachine, MediaNestCall,
    MediaSnapshot, ResolvedCustody, ResolvedFolderKeys,
};

fn item(folder: &str, path: &str, size: i64, updated: i64) -> MediaItem {
    MediaItem {
        folder: folder.into(),
        path: path.into(),
        size_bytes: size,
        updated_at: updated,
        source_online: true,
        ..Default::default()
    }
}

/// One control-plane set row (`fauna.folders.list`) — a set that exists
/// whether or not it holds media.
fn folder(name: &str) -> MediaFolder {
    MediaFolder {
        // Stable per set and distinct across sets — all these tests need of it.
        id: name.len() as i64,
        name: name.into(),
        ..Default::default()
    }
}

/// A set the owner DECLASSIFIED — `FolderSummary::judge_declassification`
/// verified the owner's attestation at the control-plane seam, so everything this page writes into it
/// rests in the clear (`encryption-at-rest.md` § Readable classes →
/// *Owner-flipped public-audience folders*).
fn public_folder(name: &str) -> MediaFolder {
    MediaFolder {
        rests_unsealed: true,
        ..folder(name)
    }
}

/// A set whose residency is `metadata_only` — its content stays on the user's
/// devices, so the nest keeps no body for it (`file-sync.md` § Relay serving →
/// *A write door that keeps no body refuses a metadata-only folder*).
fn metadata_only_folder(name: &str) -> MediaFolder {
    MediaFolder {
        metadata_only: true,
        ..folder(name)
    }
}

fn fixture() -> MediaSnapshot {
    MediaSnapshot {
        items: vec![
            item("docs", "docs/notes.txt", 30, 100),
            item("photos", "photos/a.jpg", 10, 300),
            item("photos", "photos/b.png", 20, 200),
        ],
        ..Default::default()
    }
}

#[tokio::test]
async fn refresh_populates_default_all_media_view() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture());
    let machine = MediaMachine::new(observer.clone(), api, None, None, None, None);

    // Before refresh: empty.
    assert!(machine.snapshot().items.is_empty());

    machine.refresh(None).await;
    let snap = machine.snapshot();
    assert_eq!(snap.items.len(), 3, "all readable sets aggregated");
    assert_eq!(snap.filter, None, "defaults to the all-media view");
    assert_eq!(snap.sort, "name", "defaults to name sort");
    assert!(!snap.descending);
    assert!(!snap.view_grid, "defaults to list view");
    assert_eq!(snap.folders, vec!["docs".to_string(), "photos".to_string()]);
    assert!(snap.error.is_none());
    // Default name sort: notes.txt, a.jpg, b.png → a.jpg, b.png, notes.txt.
    assert_eq!(
        snap.items
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>(),
        vec!["a.jpg", "b.png", "notes.txt"]
    );
    assert_eq!(observer.count(), 1, "refresh notifies once");
}

#[tokio::test]
async fn view_state_setters_re_derive_the_page() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture());
    let machine = MediaMachine::new(observer.clone(), api, None, None, None, None);
    machine.refresh(None).await;

    // Filter to one set.
    machine.set_filter(Some("photos".into()));
    let snap = machine.snapshot();
    assert_eq!(snap.items.len(), 2);
    assert!(snap.items.iter().all(|i| i.folder == "photos"));
    assert_eq!(snap.filter.as_deref(), Some("photos"));

    // Sort by size descending.
    machine.set_sort("size".into());
    machine.set_descending(true);
    let snap = machine.snapshot();
    assert_eq!(snap.sort, "size");
    assert!(snap.descending);
    assert_eq!(
        snap.items.iter().map(|i| i.size_bytes).collect::<Vec<_>>(),
        vec![20, 10]
    );

    // Grid toggle is pure render state — item set unchanged.
    machine.set_view_grid(true);
    let snap = machine.snapshot();
    assert!(snap.view_grid);
    assert_eq!(snap.items.len(), 2);

    // Unknown sort value is ignored (no panic, key unchanged).
    machine.set_sort("bogus".into());
    assert_eq!(machine.snapshot().sort, "size");

    // refresh + 3 applied setters (filter, sort, descending, view_grid) = 5 ticks;
    // the ignored set_sort("bogus") does not tick.
    assert_eq!(observer.count(), 5);
}

#[tokio::test]
async fn empty_aggregate_renders_empty() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new()); // default empty snapshot
    let machine = MediaMachine::new(observer, api, None, None, None, None);
    machine.refresh(None).await;
    let snap = machine.snapshot();
    assert!(snap.items.is_empty());
    assert!(snap.folders.is_empty());
    assert!(snap.error.is_none());
}

// ── `loaded`: an unloaded page is NOT an empty one ────────────────────────────
//
// `items.is_empty()` is true both before the first refresh returns and after one
// that found nothing, so every app rendered "No media yet" while still loading
// and the e2e harness could not tell the two apart (`media.md` § Default view;
// `helpers/multiseat_config.py::settle_listing`). `loaded` is the distinction:
// it is the `media-empty-state` element's second painting condition.

#[tokio::test]
async fn a_page_that_has_never_refreshed_is_not_loaded() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture());
    let machine = MediaMachine::new(observer, api, None, None, None, None);

    let snap = machine.snapshot();
    assert!(snap.items.is_empty(), "nothing read yet");
    assert!(
        !snap.loaded,
        "an untouched page is UNLOADED, not empty — no client may paint media-empty-state here"
    );
}

#[tokio::test]
async fn a_successful_refresh_that_finds_nothing_is_loaded_and_empty() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new()); // default empty snapshot
    let machine = MediaMachine::new(observer, api, None, None, None, None);
    machine.refresh(None).await;

    let snap = machine.snapshot();
    assert!(snap.items.is_empty());
    assert!(
        snap.loaded,
        "a completed read that found nothing is the genuine empty state"
    );
}

#[tokio::test]
async fn a_failed_first_refresh_leaves_the_page_unloaded() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.fail(MediaApiError::Transient {
        detail: "network down".into(),
    });
    let machine = MediaMachine::new(observer, api.clone(), None, None, None, None);
    machine.refresh(None).await;

    let snap = machine.snapshot();
    assert!(
        snap.error.is_some(),
        "the failure surfaces as error-message"
    );
    assert!(
        !snap.loaded,
        "a page with nothing but an error has not loaded — error-message does the \
         talking, and claiming 'No media yet' beside it would be a lie"
    );

    // Recovery flips it, and only then.
    api.clear_failure();
    machine.refresh(None).await;
    assert!(machine.snapshot().loaded);
}

#[tokio::test]
async fn loaded_never_regresses_once_a_later_refresh_fails() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture());
    let machine = MediaMachine::new(observer, api.clone(), None, None, None, None);
    machine.refresh(None).await;
    assert!(machine.snapshot().loaded);

    // A later failure keeps the prior data (see
    // `refresh_failure_keeps_prior_data_and_sets_error`), so the page is still
    // showing a loaded list — `loaded` must not flap back and re-arm the
    // loading state under rows that are plainly on screen.
    api.fail(MediaApiError::Transient {
        detail: "network down".into(),
    });
    machine.refresh(None).await;
    let snap = machine.snapshot();
    assert_eq!(snap.items.len(), 3, "prior data kept");
    assert!(snap.loaded, "still loaded — the rows are right there");
}

#[tokio::test]
async fn refresh_failure_keeps_prior_data_and_sets_error() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture());
    let machine = MediaMachine::new(observer, api.clone(), None, None, None, None);
    machine.refresh(None).await;
    assert_eq!(machine.snapshot().items.len(), 3);

    // Next refresh fails — prior items stay, error surfaces.
    api.fail(MediaApiError::Transient {
        detail: "network down".into(),
    });
    machine.refresh(None).await;
    let snap = machine.snapshot();
    assert_eq!(snap.items.len(), 3, "prior data kept on read failure");
    assert!(snap.error.is_some(), "error surfaced");

    // Recovery: clear the failure, refresh succeeds, error clears.
    api.clear_failure();
    machine.refresh(None).await;
    assert!(machine.snapshot().error.is_none());

    // Three refreshes, each opening with `media_snapshot`; the two that
    // SUCCEEDED also read the control-plane set list (the failing one returns
    // before it, so a broken read costs exactly one call, not two).
    assert_eq!(
        api.calls()
            .iter()
            .filter(|c| **c == MediaNestCall::MediaSnapshot)
            .count(),
        3,
        "media_snapshot called once per refresh"
    );
    assert_eq!(
        api.calls()
            .iter()
            .filter(|c| **c == MediaNestCall::ListFolders)
            .count(),
        2,
        "the option list is read on each SUCCESSFUL refresh only"
    );
}

// ── Upload / delete gestures ────────────────────────────────────────────────

const KEY: [u8; 32] = [7u8; 32];

/// The `SealedLabel` the upload gesture must produce for `path` — sealed under
/// [`KEY`]'s `convergent_chunk_root()`, convergent mode, salted by the path's
/// own `path_hash` (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
///
/// Recomputed here rather than copied as a fixture: the seal is *convergent*, so
/// the derivation reproducing byte-identically IS the property — a golden blob
/// would pass just as happily if the gesture reached for the wrong root.
fn expected_path_seal(path: &str) -> Vec<u8> {
    fauna_core::path_crypto::seal_convergent(
        &fauna_core::path_crypto::LabelRoot::owner_of(&fauna_core::crypto::BackupKey::from_bytes(
            KEY,
        )),
        &fauna_core::sync::path_hash(path),
        fauna_core::path_crypto::LabelField::SyncChangePath,
        path.as_bytes(),
    )
    .unwrap()
    .to_bytes()
    .unwrap()
}

/// The one file an upload posted over the chunk routes: `(manifest hash,
/// manifest bytes, (store key, body) per chunk in manifest order)`. (A sealed
/// chunk's store key is its ciphertext hash, a plaintext one's its plaintext
/// hash over a framed body — so the readers below, not a hash check here, are
/// the proof the shape is right.)
#[allow(clippy::type_complexity)]
fn posted_file(uploader: &FakeMediaBlobUploader) -> ([u8; 32], Vec<u8>, Vec<([u8; 32], Vec<u8>)>) {
    let manifests = uploader.manifests();
    assert_eq!(manifests.len(), 1, "one manifest");
    let (manifest_hash, manifest_bytes) = manifests[0].clone();
    assert_eq!(*blake3::hash(&manifest_bytes).as_bytes(), manifest_hash);
    let chunks = uploader.chunks();
    assert!(!chunks.is_empty(), "the file was chunked");
    (manifest_hash, manifest_bytes, chunks)
}

/// Every chunk body of a SEALED upload rests under its ciphertext hash (the
/// F9 store-key contract) and is not the plaintext.
fn assert_sealed_chunks(chunks: &[([u8; 32], Vec<u8>)], raw: &[u8]) {
    for (store_key, body) in chunks {
        assert_eq!(*blake3::hash(body).as_bytes(), *store_key);
        assert_ne!(body.as_slice(), raw, "sealed, not the plaintext");
    }
}

/// A download fetcher stocked with everything `uploader` was handed — the nest
/// in miniature, as the byte routes would have filed it.
fn fetcher_over(uploader: &FakeMediaBlobUploader) -> FakeDownloadFetcher {
    let mut fetcher = FakeDownloadFetcher::default();
    for (hash, bytes) in uploader.manifests().into_iter().chain(uploader.chunks()) {
        fetcher.put(&fauna_core::data::ContentHash::from_digest_raw(hash), bytes);
    }
    fetcher
}

/// Open the one posted file through the shared walk under `keys`.
async fn open_posted_file(
    uploader: &FakeMediaBlobUploader,
    keys: &fauna_core::file_download::FileDownloadKeys,
) -> Vec<u8> {
    let (manifest_hash, _, _) = posted_file(uploader);
    fauna_core::file_download::download_file_bytes_by_manifest(
        &fetcher_over(uploader),
        keys,
        fauna_core::data::ContentHash::from_digest_raw(manifest_hash),
        None,
        "photos/c.jpg",
    )
    .await
    .expect("the walk opens what the upload posted")
}

#[tokio::test]
async fn upload_seals_posts_records_then_refreshes() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let uploader = Arc::new(FakeMediaBlobUploader::new("cafef00d"));
    let machine = MediaMachine::new(
        observer.clone(),
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );

    let raw = b"the original picked file bytes".to_vec();
    machine
        .upload(
            "photos".into(),
            "dev-1".into(),
            "photos/c.jpg".into(),
            raw.clone(),
            KEY.to_vec(),
        )
        .await;

    // The one at-rest shape (`media.md` § Encryption at rest → *One at-rest
    // shape*): chunks sealed under the owner's convergent chunk root behind a
    // canonical manifest — never a blob-store primary.
    assert!(
        uploader.posts().is_empty(),
        "a folder file is never a `POST /api/v1/blob` primary"
    );
    let (manifest_hash, _, chunks) = posted_file(&uploader);
    assert_sealed_chunks(&chunks, &raw);

    // The owner's own walk opens it under the owner root alone — the reader
    // every synced desktop, the restore and the private link share.
    let opened = open_posted_file(
        &uploader,
        &fauna_core::file_download::FileDownloadKeys::owner(BackupKey::from_bytes(KEY)),
    )
    .await;
    assert_eq!(opened, raw, "the owner walk round-trips the upload");

    // The member is recorded with the manifest's hash + the plaintext size
    // (the engine's record), unstamped, then the page refreshes.
    assert_eq!(
        api.calls(),
        vec![
            MediaNestCall::RecordMember {
                folder: "photos".into(),
                device_id: "dev-1".into(),
                path: "photos/c.jpg".into(),
                manifest_hash: hex::encode(manifest_hash),
                size_bytes: raw.len() as i64,
                content_key_version: None,
                // Non-image bytes → `process_media` produces no thumbnail.
                thumbnail_hash: None,
                // The path seals under the SAME owner root the blob did, so the
                // audience that can open the bytes renders the name and nobody
                // weaker (`file-sync.md` § Sealed names & paths).
                path_sealed: Some(expected_path_seal("photos/c.jpg")),
            },
            MediaNestCall::MediaSnapshot,
            // Every refresh also re-reads the control-plane option list, so a
            // set created since the last refresh becomes selectable.
            MediaNestCall::ListFolders,
        ]
    );
    assert!(
        machine.snapshot().error.is_none(),
        "success clears the error"
    );
}

#[tokio::test]
async fn upload_without_uploader_reports_unsupported() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    // No blob uploader wired (web today) → upload is unsupported.
    let machine = MediaMachine::new(observer, api.clone(), None, None, None, None);

    machine
        .upload(
            "photos".into(),
            "dev-1".into(),
            "photos/c.jpg".into(),
            b"bytes".to_vec(),
            KEY.to_vec(),
        )
        .await;

    assert!(
        machine.snapshot().error.is_some(),
        "unsupported surfaces as the page error"
    );
    assert!(
        api.calls().is_empty(),
        "nothing recorded + no refresh when upload is unsupported"
    );
}

#[tokio::test]
async fn upload_bad_key_length_is_rejected_before_sealing() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let uploader = Arc::new(FakeMediaBlobUploader::new("cafef00d"));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );

    // A 16-byte key is invalid (must be 32).
    machine
        .upload(
            "photos".into(),
            "dev-1".into(),
            "photos/c.jpg".into(),
            b"bytes".to_vec(),
            vec![0u8; 16],
        )
        .await;

    assert!(machine.snapshot().error.is_some(), "bad key → page error");
    assert!(uploader.posts().is_empty(), "no seal/POST on a bad key");
    assert!(uploader.chunks().is_empty(), "no chunk on a bad key");
    assert!(api.calls().is_empty(), "no record + no refresh");
}

#[tokio::test]
async fn upload_record_failure_sets_error() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.fail_writes(MediaApiError::Transient {
        detail: "node offline".into(),
    });
    let uploader = Arc::new(FakeMediaBlobUploader::new("cafef00d"));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );

    machine
        .upload(
            "photos".into(),
            "dev-1".into(),
            "photos/c.jpg".into(),
            b"bytes".to_vec(),
            KEY.to_vec(),
        )
        .await;

    // The bytes were posted, the record failed → page error, no refresh.
    assert!(machine.snapshot().error.is_some());
    let (manifest_hash, _, _) = posted_file(&uploader);
    assert_eq!(
        api.calls(),
        vec![MediaNestCall::RecordMember {
            folder: "photos".into(),
            device_id: "dev-1".into(),
            path: "photos/c.jpg".into(),
            manifest_hash: hex::encode(manifest_hash),
            size_bytes: b"bytes".len() as i64,
            content_key_version: None,
            thumbnail_hash: None,
            path_sealed: Some(expected_path_seal("photos/c.jpg")),
        }],
        "the failing record is recorded; no media_snapshot refresh"
    );
}

#[tokio::test]
async fn delete_tombstones_then_refreshes_without_an_uploader() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    // Delete is pure WS-RPC — works with no blob uploader (native + wasm alike).
    let machine = MediaMachine::new(observer, api.clone(), None, None, None, None);

    machine
        .delete("photos".into(), "dev-1".into(), "photos/b.png".into())
        .await;

    assert_eq!(
        api.calls(),
        vec![
            MediaNestCall::DeleteMember {
                folder: "photos".into(),
                device_id: "dev-1".into(),
                path: "photos/b.png".into(),
                // Keyless machine (no injected owner key) — the best-effort
                // degrade records the tombstone plaintext-only (S8 D2).
                path_sealed: None,
            },
            MediaNestCall::MediaSnapshot,
            // Every refresh also re-reads the control-plane option list, so a
            // set created since the last refresh becomes selectable.
            MediaNestCall::ListFolders,
        ]
    );
    assert!(machine.snapshot().error.is_none());
}

#[tokio::test]
async fn delete_failure_sets_error() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.fail_writes(MediaApiError::NotFound {
        detail: "no such member".into(),
    });
    let machine = MediaMachine::new(observer, api.clone(), None, None, None, None);

    machine
        .delete("photos".into(), "dev-1".into(), "photos/b.png".into())
        .await;

    assert!(machine.snapshot().error.is_some());
    assert_eq!(
        api.calls(),
        vec![MediaNestCall::DeleteMember {
            folder: "photos".into(),
            device_id: "dev-1".into(),
            path: "photos/b.png".into(),
            path_sealed: None,
        }],
        "the failing delete is recorded; no media_snapshot refresh"
    );
}

// ── delete/restore: the machine-minted gesture seal (S8 D2) ──────────────────
//
// These records are append-only nest-side and excluded from the idempotence
// compare, so the gesture is each row's ONLY chance to seal — which is why the
// pins below assert exact bytes by recomputing the derivation (a golden blob
// passes just as happily under the wrong root).

#[tokio::test]
async fn a_keyed_machines_delete_tombstone_seals_under_the_owner_root() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let machine = MediaMachine::new(observer, api.clone(), None, None, None, None);
    machine.set_owner_backup_key(KEY.to_vec());

    machine
        .delete("photos".into(), "dev-1".into(), "photos/b.png".into())
        .await;

    let sealed = match &api.calls()[0] {
        MediaNestCall::DeleteMember { path_sealed, .. } => path_sealed.clone(),
        other => panic!("expected DeleteMember, got {other:?}"),
    };
    assert_eq!(
        sealed,
        Some(expected_path_seal("photos/b.png")),
        "the injected owner key seals the tombstone's path"
    );
}

#[tokio::test]
async fn a_keyed_machines_restore_re_point_seals_under_the_owner_root() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let machine = MediaMachine::new(observer, api.clone(), None, None, None, None);
    machine.set_owner_backup_key(KEY.to_vec());

    machine
        .restore_version(
            "photos".into(),
            "dev-1".into(),
            "photos/b.png".into(),
            fauna_media_machine::snapshots::FileVersionSummary {
                version_num: 3,
                manifest_hash: "abc123".into(),
                size_bytes: 42,
                created_at: 1_000,
                content_key_version: None,
                author_display: "alice".into(),
                pruned: false,
                purge_after: None,
            },
        )
        .await;

    let sealed = match &api.calls()[0] {
        MediaNestCall::RestoreMember { path_sealed, .. } => path_sealed.clone(),
        other => panic!("expected RestoreMember, got {other:?}"),
    };
    assert_eq!(
        sealed,
        Some(expected_path_seal("photos/b.png")),
        "the injected owner key seals the re-pointed path"
    );
}

/// The **un-injected** machine — the state an app is in until its glue calls
/// `set_owner_backup_key`. Every other test in this file injects, so this branch
/// had no coverage at all, and post-S9-flip it is the difference between a
/// working gesture and a refused one.
///
/// With no owner key `seal_gesture_path` resolves no seal root, so the record
/// goes out with `path_sealed: None` — which the nest **refuses** with the typed
/// `fauna.sync.path_seal_required` (`bins/fauna-nest/src/sync_handlers.rs`,
/// pinned nest-side by `conformance_path_sealing.rs::a_sealless_record_is_refused_loudly`
/// and `tests/api/test_media_seed.py::test_sealless_record_is_refused_path_seal_required`).
/// This test is the **client half** of that contract: injection is not an
/// optimization, it is what makes `delete`/`restore_version` recordable at all.
///
/// The asymmetry is the trap worth pinning: `upload` is unaffected because it
/// seals from the key handed to it **per call**, so an app can look perfectly
/// healthy on uploads while every delete and restore fails. That is exactly the
/// state web, apple, android and windows were all in on 2026-08-02 — only tui
/// and linux injected (web's wasm seam landed with this test).
#[tokio::test]
async fn an_unkeyed_machines_restore_seals_nothing_so_the_post_flip_nest_refuses_it() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    // Deliberately NO `set_owner_backup_key` — that omission is the test.
    let machine = MediaMachine::new(observer, api.clone(), None, None, None, None);

    machine
        .restore_version(
            "photos".into(),
            "dev-1".into(),
            "photos/b.png".into(),
            fauna_media_machine::snapshots::FileVersionSummary {
                version_num: 3,
                manifest_hash: "abc123".into(),
                size_bytes: 42,
                created_at: 1_000,
                content_key_version: None,
                author_display: "alice".into(),
                pruned: false,
                purge_after: None,
            },
        )
        .await;

    let sealed = match &api.calls()[0] {
        MediaNestCall::RestoreMember { path_sealed, .. } => path_sealed.clone(),
        other => panic!("expected RestoreMember, got {other:?}"),
    };
    assert_eq!(
        sealed, None,
        "without an injected owner key the gesture has no seal root, so it sends \
         path_sealed: None — the post-flip nest refuses that record outright \
         (fauna.sync.path_seal_required). If this ever starts sealing, the machine \
         grew an internal key source and the app-side injection requirement (and \
         the tracks chasing it) should be revisited."
    );
}

// ── The gesture's own post-write refresh renders under the SAME custody ───────
//
// `delete`/`restore_version` seal their record with the machine's injected owner
// key, then refresh the page. If that refresh drops the key, every row the key
// was rendering omits — the ratified degrade firing on a reader that is in fact
// the label's audience. The set-name axis makes it worse than one missing row:
// `SetNameRender::Omit` skips *every* item of that set, so deleting one of two
// files empties the whole library. That is user-visible data-disappearance
// (`file-sync.md` § Sealed names & paths → the `Omit` DROPS the row paragraph),
// which is why these pin the post-gesture snapshot and not just the record.

/// Deleting one file must leave the set's **surviving** sealed rows listed. The
/// nest excludes the tombstone (the fake's snapshot is already the post-delete
/// truth); the only thing that can empty this listing is the client re-rendering
/// under weaker custody than it read with.
#[tokio::test]
async fn a_keyed_machines_delete_refresh_still_renders_the_sets_sealed_rows() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    // What `fauna.media.list` returns *after* the tombstone: the survivor only.
    api.set_snapshot(MediaSnapshot {
        items: vec![sealed_item("photos", "photos/survivor.jpg", "")],
        ..Default::default()
    });
    let machine = MediaMachine::new(observer, api.clone(), None, None, None, None);
    machine.set_owner_backup_key(KEY.to_vec());

    machine
        .delete("photos".into(), "dev-1".into(), "photos/doomed.png".into())
        .await;

    let snap = machine.snapshot();
    assert_eq!(
        snap.items.len(),
        1,
        "the survivor must still render after the delete's own refresh — a 0 here \
         is the whole set vanishing from the library because the post-gesture \
         refresh rendered keyless"
    );
    assert_eq!(snap.items[0].path, "photos/survivor.jpg");
    assert!(snap.error.is_none(), "an omission is not a page error");
}

/// The set-name axis of the same failure, and the shape the e2e actually caught:
/// with the set *name* sealed too, a keyless post-gesture refresh omits every
/// item of the set — 2 items become 0, not 1.
#[tokio::test]
async fn a_keyed_machines_delete_refresh_still_renders_the_sealed_set_name() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![sealed_set_item("Family photos", "photos/survivor.jpg")],
        ..Default::default()
    });
    let machine = MediaMachine::new(observer, api.clone(), None, None, None, None);
    machine.set_owner_backup_key(KEY.to_vec());

    machine
        .delete(
            "Family photos".into(),
            "dev-1".into(),
            "photos/doomed.png".into(),
        )
        .await;

    let snap = machine.snapshot();
    assert_eq!(
        snap.items.len(),
        1,
        "a set whose NAME only the owner key opens must survive its own delete"
    );
    assert_eq!(snap.items[0].folder, "Family photos");
    assert_eq!(snap.items[0].path, "photos/survivor.jpg");
}

/// The restore twin — same custody contract, and the reason
/// `test_file_version_history_and_restore` read an empty `media-item-size`: the
/// restored row was not missing a size, it was missing from the snapshot.
#[tokio::test]
async fn a_keyed_machines_restore_refresh_still_renders_the_sealed_row() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![sealed_set_item("Family photos", "photos/restored.jpg")],
        ..Default::default()
    });
    let machine = MediaMachine::new(observer, api.clone(), None, None, None, None);
    machine.set_owner_backup_key(KEY.to_vec());

    machine
        .restore_version(
            "Family photos".into(),
            "dev-1".into(),
            "photos/restored.jpg".into(),
            fauna_media_machine::snapshots::FileVersionSummary {
                version_num: 3,
                manifest_hash: "abc123".into(),
                size_bytes: 42,
                created_at: 1_000,
                content_key_version: None,
                author_display: "alice".into(),
                pruned: false,
                purge_after: None,
            },
        )
        .await;

    let snap = machine.snapshot();
    assert_eq!(
        snap.items.len(),
        1,
        "the restored row must still render after the restore's own refresh"
    );
    assert_eq!(snap.items[0].path, "photos/restored.jpg");
    assert_eq!(snap.items[0].size_bytes, 10, "and it carries its size");
}

/// A bound set's gesture record seals under the **content** root every roster
/// member holds — never the owner root only the owner could open (the mistake class), which is why `seal_gesture_path` routes through the shared
/// custody assembly instead of copying `upload()`'s inline owner-root shape.
#[tokio::test]
async fn a_bound_sets_delete_tombstone_seals_under_the_content_root_not_the_owners() {
    let content_key = [0x42u8; 32];
    let keys = fauna_core::folder_keys::FolderContentKeys::genesis(content_key, 1_000);
    let resolver = Arc::new(FakeFolderKeyResolver {
        folder: "shared-docs".into(),
        keys: Some(keys.clone()),
        home: None,
        home_actor_id: None,
        served: false,
    });
    let api = Arc::new(FakeMediaNestApi::new());
    let machine = MediaMachine::new(
        CountingObserver::new(),
        api.clone(),
        None,
        None,
        None,
        Some(resolver as Arc<dyn FolderKeyResolver>),
    );
    machine.set_owner_backup_key(KEY.to_vec());

    machine
        .delete("shared-docs".into(), "dev-1".into(), "docs/plan.md".into())
        .await;

    let sealed = match &api.calls()[0] {
        MediaNestCall::DeleteMember { path_sealed, .. } => {
            path_sealed.clone().expect("a bound set with keys seals")
        }
        other => panic!("expected DeleteMember, got {other:?}"),
    };
    let expected = fauna_core::path_crypto::seal_convergent(
        &fauna_core::path_crypto::LabelRoot::content_key(
            *keys.current_key(),
            keys.current_version(),
        ),
        &fauna_core::sync::path_hash("docs/plan.md"),
        fauna_core::path_crypto::LabelField::SyncChangePath,
        "docs/plan.md".as_bytes(),
    )
    .unwrap()
    .to_bytes()
    .unwrap();
    assert_eq!(sealed, expected, "sealed under the roster's content root");
    assert_ne!(
        sealed,
        expected_path_seal("docs/plan.md"),
        "and NOT under the owner root no other roster member could open"
    );
}

/// The cell at this site: a **bound** set whose content keys the
/// resolver cannot produce — owner key present, resolve answers
/// bound-but-unresolvable — records the tombstone **plaintext-only** (the
/// ratified degrade; a later backfill converges it), never sealed under the
/// owner root only the owner could open. Before the fix the resolver collapsed
/// this cell to "unbound" and the gesture stamped under the owner root.
#[tokio::test]
async fn a_bound_sets_gesture_with_unresolvable_keys_records_plaintext_only() {
    let resolver = Arc::new(FakeFolderKeyResolver {
        folder: "shared-docs".into(),
        keys: None, // bound, but this holder cannot resolve the content keys
        home: None,
        home_actor_id: None,
        served: false,
    });
    let api = Arc::new(FakeMediaNestApi::new());
    let machine = MediaMachine::new(
        CountingObserver::new(),
        api.clone(),
        None,
        None,
        None,
        Some(resolver as Arc<dyn FolderKeyResolver>),
    );
    machine.set_owner_backup_key(KEY.to_vec());

    machine
        .delete("shared-docs".into(), "dev-1".into(), "docs/plan.md".into())
        .await;

    let sealed = match &api.calls()[0] {
        MediaNestCall::DeleteMember { path_sealed, .. } => path_sealed.clone(),
        other => panic!("expected DeleteMember, got {other:?}"),
    };
    assert_eq!(
        sealed, None,
        "a bound set with unresolvable keys must record plaintext-only — \
         never stamp under the owner root no roster member could open"
    );
}

// ── upload_selected: shared "which set" target resolution ────────────────────

/// The recorded-member `(folder, path)` of the single `RecordMember` call, or
/// `None` if no member was recorded. Lets the `upload_selected` tests assert the
/// *target set* without pinning the surrounding refresh `MediaSnapshot` calls.
fn recorded_member(api: &FakeMediaNestApi) -> Option<(String, String)> {
    api.calls().into_iter().find_map(|c| match c {
        MediaNestCall::RecordMember { folder, path, .. } => Some((folder, path)),
        _ => None,
    })
}

#[tokio::test]
async fn upload_selected_targets_the_filter_set() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture()); // sets: docs, photos
    let uploader = Arc::new(FakeMediaBlobUploader::new("beef"));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );
    machine.refresh(None).await; // populate raw so folders() == [docs, photos]
    machine.set_filter(Some("photos".into()));

    machine
        .upload_selected(
            "dev-1".into(),
            "c.jpg".into(),
            b"raw".to_vec(),
            KEY.to_vec(),
        )
        .await;

    assert_eq!(
        recorded_member(&api),
        Some(("photos".into(), "c.jpg".into())),
        "upload targets the filter-selected set, not the first set"
    );
    posted_file(&uploader);
    assert!(
        machine.snapshot().error.is_none(),
        "success clears the error"
    );
}

#[tokio::test]
async fn upload_selected_all_media_defaults_to_first_set() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture()); // sets in order: docs, photos
    let uploader = Arc::new(FakeMediaBlobUploader::new("beef"));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );
    machine.refresh(None).await; // all-media view (filter == None) by default

    machine
        .upload_selected(
            "dev-1".into(),
            "c.jpg".into(),
            b"raw".to_vec(),
            KEY.to_vec(),
        )
        .await;

    assert_eq!(
        recorded_member(&api),
        Some(("docs".into(), "c.jpg".into())),
        "the all-media view defaults the target to the first set that has media"
    );
    assert!(machine.snapshot().error.is_none());
}

#[tokio::test]
async fn upload_selected_with_no_set_sets_error_and_uploads_nothing() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new()); // default empty snapshot — no sets
    let uploader = Arc::new(FakeMediaBlobUploader::new("beef"));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );
    // No refresh → no readable set has media → nothing to default the target to.
    machine
        .upload_selected(
            "dev-1".into(),
            "c.jpg".into(),
            b"raw".to_vec(),
            KEY.to_vec(),
        )
        .await;

    let err = machine
        .snapshot()
        .error
        .expect("no set → page error (media.error_no_set)");
    assert_eq!(
        err.key, "media.error_no_set",
        "the caller has NO sets at all — the create-one-first copy"
    );
    assert!(uploader.posts().is_empty(), "nothing sealed / POSTed");
    assert!(uploader.chunks().is_empty(), "nothing sealed / POSTed");
    assert!(api.calls().is_empty(), "nothing recorded, no refresh");
}

// ── producer: a real image yields a companion-thumbnail blob + recorded hash ──
//
// These run under the test-only `process_media` feature (the self dev-dep), so
// `process_media` actually renders a thumbnail — the native-client producer
// posture. They prove `MediaMachine::upload` POSTs the thumbnail as its own
// blob beside the file's chunks and records its hash on the member (the gate-(B) producer the five
// per-app folders-UI efforts consume; media.md § State & data shape).

#[tokio::test]
async fn upload_of_an_image_posts_a_thumbnail_blob_and_records_its_hash() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let uploader = Arc::new(FakeMediaBlobUploader::new("cafef00d"));
    let machine = MediaMachine::new(
        observer.clone(),
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );

    // A 400×400 image is over the 300px thumbnail threshold → the real
    // `process_media` renders a JPEG thumbnail.
    machine
        .upload(
            "photos".into(),
            "dev-1".into(),
            "photos/c.png".into(),
            build_png(400, 400),
            KEY.to_vec(),
        )
        .await;

    // The file goes over the chunk routes; the thumbnail stays a blob of its
    // own — the ONE blob POST (`media.md` § Encryption at rest → *One at-rest
    // shape*: a derived view only this page reads), sealed, never the JPEG.
    posted_file(&uploader);
    let posts = uploader.posts();
    assert_eq!(
        posts.len(),
        1,
        "the companion thumbnail is the only blob POST"
    );
    let thumb = &posts[0].1;
    assert_ne!(&thumb[..2], &[0xFF, 0xD8], "the thumbnail rests sealed");
    let opened = fauna_core::crypto::decrypt_backup_chunk(&BackupKey::from_bytes(KEY), thumb)
        .expect("sealed under the owner key the page's fetch_thumbnail opens");
    assert_eq!(&opened[..2], &[0xFF, 0xD8], "a JPEG thumbnail");

    // The member carries the companion thumbnail's hash (the fake returns its
    // canned hash; the real nest content-addresses it).
    let recorded_thumb = api
        .calls()
        .into_iter()
        .find_map(|c| match c {
            MediaNestCall::RecordMember { thumbnail_hash, .. } => Some(thumbnail_hash),
            _ => None,
        })
        .expect("a member was recorded");
    assert_eq!(
        recorded_thumb.as_deref(),
        Some("cafef00d"),
        "the companion thumbnail's hash rides the record → fauna.media.list"
    );
    assert!(
        machine.snapshot().error.is_none(),
        "success clears the error"
    );
}

#[tokio::test]
async fn upload_of_a_small_image_records_no_thumbnail() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let uploader = Arc::new(FakeMediaBlobUploader::new("cafef00d"));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );

    // A 100×100 image is under the 300px threshold → no thumbnail produced, so
    // only the primary is POSTed and the record carries `thumbnail_hash = None`.
    machine
        .upload(
            "photos".into(),
            "dev-1".into(),
            "photos/small.png".into(),
            build_png(100, 100),
            KEY.to_vec(),
        )
        .await;

    posted_file(&uploader);
    assert!(
        uploader.posts().is_empty(),
        "no thumbnail → no blob POST at all"
    );
    let recorded_thumb = api
        .calls()
        .into_iter()
        .find_map(|c| match c {
            MediaNestCall::RecordMember { thumbnail_hash, .. } => Some(thumbnail_hash),
            _ => None,
        })
        .expect("a member was recorded");
    assert_eq!(recorded_thumb, None, "no thumbnail → no recorded hash");
}

// Build a `w`×`h` PNG in memory, so the tests carry no opaque binary
// fixtures — `fauna-media`'s shared fixture builder (priority #2, one
// canonical body instead of a hand-rolled local copy).
use fauna_media::test_fixtures::build_png;

// ── fetch_thumbnail (the shared media-thumbnail render: fetch + decrypt) ──────

/// AEAD-seal `plaintext` under `key` with the producer's primitive — the exact
/// bytes the nest stores for a Library-audience thumbnail, so `fetch_thumbnail`
/// must fetch *and decrypt* to recover the original.
fn sealed(plaintext: &[u8], key: &[u8; 32]) -> Vec<u8> {
    encrypt_backup_chunk(&BackupKey::from_bytes(*key), plaintext).expect("seal fixture")
}

/// The nest content-addresses a stored blob by lowercase-hex `blake3(bytes)`
/// (`bins/fauna-nest/src/blob_routes.rs` POST → `hex::encode(blake3(stored))`),
/// which becomes the `MediaItem.thumbnail_hash` a client fetches by. Mirror it so
/// the render tests pass a *real* content hash the machine's content-address
/// check accepts, not a placeholder.
fn content_hash_hex(sealed_bytes: &[u8]) -> String {
    blake3::hash(sealed_bytes).to_hex().to_string()
}

#[tokio::test]
async fn fetch_thumbnail_fetches_then_decrypts_the_sealed_blob() {
    // The plaintext stands in for the decoded JPEG bytes a client paints.
    let plaintext = b"decoded thumbnail JPEG bytes".to_vec();
    let sealed_bytes = sealed(&plaintext, &KEY);
    let hash = content_hash_hex(&sealed_bytes);
    let fetcher = Arc::new(FakeMediaBlobFetcher::new(sealed_bytes));
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        Some(fetcher.clone() as Arc<dyn MediaBlobFetcher>),
        None,
        None,
    );

    let bytes = machine
        .fetch_thumbnail(hash.clone(), KEY.to_vec())
        .await
        .expect("fetch + decrypt succeeds");

    assert_eq!(
        bytes, plaintext,
        "the sealed blob is decrypted back to plaintext"
    );
    assert_eq!(
        fetcher.fetches(),
        vec![hash],
        "fetched direct-by-hash with the item's thumbnail_hash"
    );
}

#[tokio::test]
async fn fetch_thumbnail_rejects_a_content_hash_mismatch() {
    // The client asks for thumbnail A's content hash, but a malicious / buggy
    // nest serves a DIFFERENT blob B that is *also* sealed under the owner's own
    // backup key — so B decrypts cleanly (the AEAD tag alone can't catch the
    // swap; the frame carries no AAD binding it to a hash). The content-address
    // check (`blake3(fetched) == requested hash`) is the one thing that rejects
    // the substitution, so the client never renders the wrong owner image.
    let wanted = b"the thumbnail the client asked for".to_vec();
    let substituted = b"a DIFFERENT owner-sealed thumbnail".to_vec();
    let wanted_hash = content_hash_hex(&sealed(&wanted, &KEY));
    // The fetcher returns blob B, sealed under the SAME owner key so decrypt
    // would succeed — only the content-address check stands between it and paint.
    let fetcher = Arc::new(FakeMediaBlobFetcher::new(sealed(&substituted, &KEY)));
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        Some(fetcher.clone() as Arc<dyn MediaBlobFetcher>),
        None,
        None,
    );

    let err = machine
        .fetch_thumbnail(wanted_hash, KEY.to_vec())
        .await
        .expect_err("a substituted owner-sealed blob is rejected by content address");
    assert!(
        matches!(err, MediaApiError::BadRequest { .. }),
        "content-hash mismatch → non-retryable BadRequest, got {err:?}"
    );
}

#[tokio::test]
async fn fetch_thumbnail_without_a_fetcher_is_unsupported() {
    // No blob fetcher wired (web today) → a Transient "not supported", no panic.
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        None,
        None,
        None,
    );

    let err = machine
        .fetch_thumbnail("deadbeef".into(), KEY.to_vec())
        .await
        .expect_err("no fetcher → error");
    assert!(matches!(err, MediaApiError::Transient { .. }));
}

#[tokio::test]
async fn fetch_thumbnail_rejects_a_wrong_key_length() {
    let fetcher = Arc::new(FakeMediaBlobFetcher::new(sealed(b"x", &KEY)));
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        Some(fetcher.clone() as Arc<dyn MediaBlobFetcher>),
        None,
        None,
    );

    let err = machine
        .fetch_thumbnail("deadbeef".into(), vec![0u8; 16])
        .await
        .expect_err("a 16-byte key is rejected");
    assert!(matches!(err, MediaApiError::BadRequest { .. }));
    assert!(
        fetcher.fetches().is_empty(),
        "a bad key fails fast — no fetch is attempted"
    );
}

#[tokio::test]
async fn fetch_thumbnail_surfaces_a_fetch_error() {
    let fetcher = Arc::new(FakeMediaBlobFetcher::new(sealed(b"x", &KEY)));
    fetcher.fail(MediaApiError::NotFound {
        detail: "no such blob".into(),
    });
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        Some(fetcher.clone() as Arc<dyn MediaBlobFetcher>),
        None,
        None,
    );

    let err = machine
        .fetch_thumbnail("deadbeef".into(), KEY.to_vec())
        .await
        .expect_err("a fetch failure surfaces");
    assert!(matches!(err, MediaApiError::NotFound { .. }));
}

#[tokio::test]
async fn fetch_thumbnail_with_the_wrong_key_fails_decrypt() {
    // Sealed under KEY, but the client supplies a different owner key — the AEAD
    // tag check rejects it. A non-retryable BadRequest (not Transient). The hash
    // is the blob's *real* content hash, so the content-address check passes and
    // this genuinely exercises the decrypt-failure path (not the address check).
    const OTHER_KEY: [u8; 32] = [9u8; 32];
    let sealed_bytes = sealed(b"secret thumb", &KEY);
    let hash = content_hash_hex(&sealed_bytes);
    let fetcher = Arc::new(FakeMediaBlobFetcher::new(sealed_bytes));
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        Some(fetcher.clone() as Arc<dyn MediaBlobFetcher>),
        None,
        None,
    );

    let err = machine
        .fetch_thumbnail(hash, OTHER_KEY.to_vec())
        .await
        .expect_err("wrong key → decrypt fails");
    assert!(matches!(err, MediaApiError::BadRequest { .. }));
}

// ── the raw-AEAD Library plane's predecessor candidates (identity succession) ─
//
// `succession-aftermath.md` § Re-key scope's `BackupKey` corpus row: a succession
// re-points corpus *ownership* but moves no seal, so a successor's inherited
// thumbnails and Media-page blob primaries still rest under the PREDECESSOR's
// key while the successor derives its own. The chunk plane got its read
// candidates in leg 5's read half (`FileDownloadKeys::predecessor_backup_keys`),
// but that field reaches only the chunk walk — these two sites are a separate
// raw-AEAD seal under the bare `BackupKey`, so they need their own candidates or
// the successor's whole inherited library renders dark.

/// The predecessor's retired owner key, distinct from the successor's [`KEY`].
const RETIRED_KEY: [u8; 32] = [11u8; 32];

#[tokio::test]
async fn fetch_thumbnail_opens_a_predecessor_sealed_thumbnail() {
    // The thumbnail was sealed by the RETIRED identity; the successor renders it
    // with its OWN current key in hand. Without the candidate loop this is the
    // dark-library break — a decrypt failure indistinguishable from corruption.
    let plaintext = b"decoded thumbnail JPEG bytes".to_vec();
    let sealed_bytes = sealed(&plaintext, &RETIRED_KEY);
    let hash = content_hash_hex(&sealed_bytes);
    let fetcher = Arc::new(FakeMediaBlobFetcher::new(sealed_bytes));
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        Some(fetcher.clone() as Arc<dyn MediaBlobFetcher>),
        None,
        None,
    );
    machine.set_predecessor_backup_keys(vec![RETIRED_KEY.to_vec()]);

    let bytes = machine
        .fetch_thumbnail(hash, KEY.to_vec())
        .await
        .expect("the retired root opens what it sealed");

    assert_eq!(
        bytes, plaintext,
        "the successor renders the thumbnail it inherited"
    );
}

#[tokio::test]
async fn fetch_thumbnail_still_rejects_a_substitution_with_predecessors_offered() {
    // The candidate loop must NOT weaken the content-address anchor. The frame
    // carries no AAD, so an extra key only widens *which root may open already
    // address-verified bytes* — it can never widen *which bytes are served*.
    // Both blobs here are sealed under the retired root, so the swap would
    // decrypt cleanly; only the address check rejects it.
    let wanted_hash = content_hash_hex(&sealed(b"the thumbnail asked for", &RETIRED_KEY));
    let fetcher = Arc::new(FakeMediaBlobFetcher::new(sealed(
        b"a DIFFERENT retired-sealed thumbnail",
        &RETIRED_KEY,
    )));
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        Some(fetcher.clone() as Arc<dyn MediaBlobFetcher>),
        None,
        None,
    );
    machine.set_predecessor_backup_keys(vec![RETIRED_KEY.to_vec()]);

    let err = machine
        .fetch_thumbnail(wanted_hash, KEY.to_vec())
        .await
        .expect_err("the address check rejects the swap, predecessors or not");
    assert!(matches!(err, MediaApiError::BadRequest { .. }));
}

#[tokio::test]
async fn fetch_thumbnail_with_no_predecessors_still_fails_on_a_foreign_key() {
    // The fail-closed control: an account that never succeeded offers no extra
    // candidates, so a blob under a key it does not hold stays unreadable.
    let sealed_bytes = sealed(b"secret thumb", &RETIRED_KEY);
    let hash = content_hash_hex(&sealed_bytes);
    let fetcher = Arc::new(FakeMediaBlobFetcher::new(sealed_bytes));
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        Some(fetcher.clone() as Arc<dyn MediaBlobFetcher>),
        None,
        None,
    );

    let err = machine
        .fetch_thumbnail(hash, KEY.to_vec())
        .await
        .expect_err("no predecessors → no extra candidate");
    assert!(matches!(err, MediaApiError::BadRequest { .. }));
}

// ── download_file (the full-file walk over the download seam) ────────────────

/// An in-memory `fauna_core::file_download::BlobFetcher` — the walk's seam
/// stocked from a HashMap, mirroring fauna-core's own `FakeFetcher` (the whole
/// point of the seam: the walk runs end to end with no nest and no HTTP).
#[derive(Default, Clone)]
struct FakeDownloadFetcher {
    blobs: std::collections::HashMap<Vec<u8>, Vec<u8>>,
}

impl FakeDownloadFetcher {
    fn put(&mut self, hash: &fauna_core::data::ContentHash, bytes: Vec<u8>) {
        self.blobs.insert(hash.digest().to_vec(), bytes);
    }

    fn get(&self, hash: &fauna_core::data::ContentHash) -> anyhow::Result<Vec<u8>> {
        self.blobs
            .get(hash.digest().as_slice())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("404 {}", hex::encode(hash.digest())))
    }
}

#[async_trait::async_trait]
impl fauna_core::file_download::BlobFetcher for FakeDownloadFetcher {
    async fn fetch_manifest(
        &self,
        hash: &fauna_core::data::ContentHash,
    ) -> anyhow::Result<Vec<u8>> {
        self.get(hash)
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[fauna_core::data::ContentHash],
        _path: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        store_keys.iter().map(|k| self.get(k)).collect()
    }
}

/// Seal `chunks` under the owner `KEY`'s convergent chunk root exactly as the
/// upload path does, stock a fake fetcher with the manifest + chunk blobs, and
/// return (fetcher, hex manifest hash, expected plaintext) — the fixture recipe
/// of fauna-core's own `file_download` tests.
fn seal_owner_file(chunks: &[&[u8]]) -> (FakeDownloadFetcher, String, Vec<u8>) {
    seal_file_under_owner(chunks, &BackupKey::from_bytes(KEY))
}

/// [`seal_owner_file`] under any owner backup key — the share-link tests seal
/// under the key `ShareAuthor` derives from the account secret.
fn seal_file_under_owner(
    chunks: &[&[u8]],
    owner: &BackupKey,
) -> (FakeDownloadFetcher, String, Vec<u8>) {
    use fauna_core::data::ContentHash;
    let root = owner.convergent_chunk_root();
    let plaintext: Vec<Vec<u8>> = chunks.iter().map(|c| c.to_vec()).collect();
    let hashes: Vec<ContentHash> = plaintext.iter().map(|c| ContentHash::of_raw(c)).collect();
    let pairs: Vec<(ContentHash, Vec<u8>)> = hashes
        .iter()
        .cloned()
        .zip(plaintext.iter().cloned())
        .collect();
    // Through the one seal door (`fauna_core::chunk_seal`): frame → encrypt →
    // re-key by ciphertext — the exact bytes the upload path stores.
    let sealed = fauna_core::chunk_seal::seal_chunk_bodies(&pairs, &root).unwrap();
    let store_keys: Vec<ContentHash> = sealed.iter().map(|(k, _)| *k).collect();
    let ciphertexts: Vec<Vec<u8>> = sealed.into_iter().map(|(_, ct)| ct).collect();

    let file_data = fauna_core::chunker::reassemble_chunks(&plaintext);
    let manifest = fauna_core::chunk::ChunkManifest {
        file_hash: ContentHash::of_raw(&file_data),
        total_size: file_data.len() as u64,
        chunk_hashes: hashes,
        chunk_sizes: plaintext.iter().map(|c| c.len() as u64).collect(),
        stored_hashes: Some(store_keys.clone()),
        sealed_hashes: None,
        min_reader: None,
    };
    let manifest_bytes =
        fauna_core::encoding::canonical_encode(&manifest.wire_form(Some(&root)).unwrap()).unwrap();
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);

    let mut fetcher = FakeDownloadFetcher::default();
    fetcher.put(&manifest_hash, manifest_bytes);
    for (key, body) in store_keys.iter().zip(ciphertexts) {
        fetcher.put(key, body);
    }
    (fetcher, hex::encode(manifest_hash.digest()), file_data)
}

/// Seal `chunks` under a shared folder's `content_key` (the M2 content-key
/// generation IS the `chunk_crypto` root, fed verbatim) — the member-read fixture
/// twin of [`seal_owner_file`], for a set whose chunks are content-keyed rather
/// than owner-keyed. Returns (fetcher, hex manifest hash, expected plaintext).
fn seal_shared_file(
    chunks: &[&[u8]],
    content_key: [u8; 32],
) -> (FakeDownloadFetcher, String, Vec<u8>) {
    use fauna_core::data::ContentHash;
    // A content-key generation key is the chunk_crypto root directly (no
    // BackupKey derivation) — the only difference from the owner path.
    let root = content_key;
    let plaintext: Vec<Vec<u8>> = chunks.iter().map(|c| c.to_vec()).collect();
    let hashes: Vec<ContentHash> = plaintext.iter().map(|c| ContentHash::of_raw(c)).collect();
    let pairs: Vec<(ContentHash, Vec<u8>)> = hashes
        .iter()
        .cloned()
        .zip(plaintext.iter().cloned())
        .collect();
    // Through the one seal door (`fauna_core::chunk_seal`): frame → encrypt →
    // re-key by ciphertext — the exact bytes the upload path stores.
    let sealed = fauna_core::chunk_seal::seal_chunk_bodies(&pairs, &root).unwrap();
    let store_keys: Vec<ContentHash> = sealed.iter().map(|(k, _)| *k).collect();
    let ciphertexts: Vec<Vec<u8>> = sealed.into_iter().map(|(_, ct)| ct).collect();

    let file_data = fauna_core::chunker::reassemble_chunks(&plaintext);
    let manifest = fauna_core::chunk::ChunkManifest {
        file_hash: ContentHash::of_raw(&file_data),
        total_size: file_data.len() as u64,
        chunk_hashes: hashes,
        chunk_sizes: plaintext.iter().map(|c| c.len() as u64).collect(),
        stored_hashes: Some(store_keys.clone()),
        sealed_hashes: None,
        min_reader: None,
    };
    let manifest_bytes =
        fauna_core::encoding::canonical_encode(&manifest.wire_form(Some(&root)).unwrap()).unwrap();
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);

    let mut fetcher = FakeDownloadFetcher::default();
    fetcher.put(&manifest_hash, manifest_bytes);
    for (key, body) in store_keys.iter().zip(ciphertexts) {
        fetcher.put(key, body);
    }
    (fetcher, hex::encode(manifest_hash.digest()), file_data)
}

/// A resolver that reports one set as a content-keyed set — bound to a group
/// (`served: false`) or WebDAV-served with no group (`served: true`), with a
/// fixed content-key custody, or content-keyed-but-unresolvable when `keys:
/// None` — and any other set as positively owner-only; the test double for
/// `fauna_client_folders::NestFolderKeyResolver`. `home` marks the bound set
/// as FOREIGN (cross-nest).
struct FakeFolderKeyResolver {
    folder: String,
    keys: Option<fauna_core::folder_keys::FolderContentKeys>,
    home: Option<String>,
    /// The grant-delivered home identity the byte-plane dial is verified
    /// against (`ResolvedFolderKeys::home_nest_actor_id`).
    home_actor_id: Option<String>,
    /// The served-unshared shape: content-keyed at the serve pseudo-channel,
    /// no MLS group.
    served: bool,
}

impl FakeFolderKeyResolver {
    /// A same-nest set bound to a cross-user group.
    fn bound(folder: &str, keys: Option<fauna_core::folder_keys::FolderContentKeys>) -> Self {
        Self {
            folder: folder.into(),
            keys,
            home: None,
            home_actor_id: None,
            served: false,
        }
    }

    /// A WebDAV-served, unshared set (`webdav-server.md` § Key model).
    fn served(folder: &str, keys: Option<fauna_core::folder_keys::FolderContentKeys>) -> Self {
        Self {
            served: true,
            ..Self::bound(folder, keys)
        }
    }
}

#[async_trait::async_trait]
impl FolderKeyResolver for FakeFolderKeyResolver {
    async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        Ok(
            if *name_hash == fauna_core::path_crypto::set_name_hash(&self.folder) {
                ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
                    mls_group_id: (!self.served).then(|| b"raw-group-id".to_vec()),
                    content_keys: self.keys.clone(),
                    home_nest_url: self.home.clone(),
                    home_nest_actor_id: self.home_actor_id.clone(),
                })
            } else {
                ResolvedCustody::owner_only()
            },
        )
    }
}

/// Phase 0 — the member read leg. A shared set's file is content-keyed (not
/// owner-keyed); with a resolver reporting the set's content custody, `download_file`
/// opens the chunk-store walk under those content keys and round-trips the
/// plaintext — the member (or the owner reading their own shared set) decrypts
/// without an owner `BackupKey`.
#[tokio::test]
async fn download_file_member_decrypts_a_shared_set_via_content_keys() {
    let content_key = [0x42u8; 32];
    let (fetcher, manifest_hex, expected) =
        seal_shared_file(&[b"shared clip ", b"bytes here"], content_key);
    let resolver = Arc::new(FakeFolderKeyResolver {
        folder: "shared-docs".into(),
        keys: Some(fauna_core::folder_keys::FolderContentKeys::genesis(
            content_key,
            1_000,
        )),
        home: None,
        home_actor_id: None,
        served: false,
    });
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        None, // the shared read comes from the walk, not the blob primary
        Some(Arc::new(fetcher) as Arc<dyn fauna_core::file_download::BlobFetcher>),
        Some(resolver as Arc<dyn FolderKeyResolver>),
    );

    // The file is stamped generation 1; the member holds gen-1 custody. The
    // owner BackupKey they pass is irrelevant to a shared set.
    let got = machine
        .download_file(
            manifest_hex,
            Some(1),
            "shared-docs".into(),
            "photos/clip.mp4".into(),
            [9u8; 32].to_vec(),
        )
        .await
        .expect("member decrypts the shared set via content keys");
    assert_eq!(got, expected);
}

/// A member reading a generation they do NOT hold (removed before the rotation,
/// or the generation not yet synced) fails **closed** — no plaintext, and never a
/// fall-through to the owner `BackupKey` (FS-BIND-5). The `mls_group_id`'s
/// presence suppresses the owner-key path.
#[tokio::test]
async fn download_file_member_without_the_generation_fails_closed() {
    // The file is sealed under gen-2's key, but the member's custody holds only
    // gen-1 — `content_open_roots(Some(2))` finds no candidate and bails.
    let (fetcher, manifest_hex, _) = seal_shared_file(&[b"post-rotation secret"], [0x77u8; 32]);
    let resolver = Arc::new(FakeFolderKeyResolver {
        folder: "shared-docs".into(),
        keys: Some(fauna_core::folder_keys::FolderContentKeys::genesis(
            [0x42u8; 32],
            1_000,
        )),
        home: None,
        home_actor_id: None,
        served: false,
    });
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        None,
        Some(Arc::new(fetcher) as Arc<dyn fauna_core::file_download::BlobFetcher>),
        Some(resolver as Arc<dyn FolderKeyResolver>),
    );

    let err = machine
        .download_file(
            manifest_hex,
            Some(2),
            "shared-docs".into(),
            "a.bin".into(),
            KEY.to_vec(),
        )
        .await
        .expect_err("a generation this member lacks must fail closed");
    assert!(
        matches!(err, MediaApiError::Transient { .. }),
        "fail closed as a download error, never a plaintext/owner-key read"
    );
}

/// A foreign-fetcher factory that must never be consulted — installed where a
/// download is expected to stay on the caller's own nest.
struct PanickingForeignFactory;

impl fauna_media_machine::folder_keys::ForeignBlobFetcherFactory for PanickingForeignFactory {
    fn fetcher_for(
        &self,
        base_url: &str,
        _home_nest_actor_id: Option<&str>,
    ) -> Arc<dyn fauna_core::file_download::BlobFetcher> {
        panic!("foreign fetcher consulted for {base_url} — this download must stay home");
    }
}

/// A factory that records which home was requested — base URL plus the
/// grant-delivered identity the dial is verified against — and serves one
/// stocked fetcher: the cross-nest byte-route double.
struct RecordingForeignFactory {
    served: FakeDownloadFetcher,
    asked: std::sync::Mutex<Vec<(String, Option<String>)>>,
}

impl fauna_media_machine::folder_keys::ForeignBlobFetcherFactory for RecordingForeignFactory {
    fn fetcher_for(
        &self,
        base_url: &str,
        home_nest_actor_id: Option<&str>,
    ) -> Arc<dyn fauna_core::file_download::BlobFetcher> {
        self.asked
            .lock()
            .unwrap()
            .push((base_url.to_string(), home_nest_actor_id.map(str::to_string)));
        Arc::new(self.served.clone())
    }
}

/// The byte-path leg: an **unbound owned** set's download takes the
/// machine's own `download_fetcher` and never consults `foreign_fetchers` —
/// pinned by a panicking factory. Before the fix, a foreign record whose
/// display name collided with the owned set routed these fetches to the
/// foreign nest; the resolver's positive-unbound answer (`Ok(None)`) is what
/// guarantees the home route now.
#[tokio::test]
async fn an_unbound_owned_sets_download_never_touches_the_foreign_fetchers() {
    let (fetcher, manifest_hex, expected) = seal_owner_file(&[b"my own ", b"unbound bytes"]);
    // The resolver knows only some OTHER set — ours answers positively
    // unbound, exactly what `NestFolderKeyResolver` now returns for an owned
    // row with no `mls_group_id`, colliding foreign record or not.
    let resolver = Arc::new(FakeFolderKeyResolver {
        folder: "someone-elses-set".into(),
        keys: Some(fauna_core::folder_keys::FolderContentKeys::genesis(
            [0x42u8; 32],
            1_000,
        )),
        home: Some("https://stranger.example".into()),
        home_actor_id: None,
        served: false,
    });
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        None,
        Some(Arc::new(fetcher) as Arc<dyn fauna_core::file_download::BlobFetcher>),
        Some(resolver as Arc<dyn FolderKeyResolver>),
    );
    machine.set_foreign_fetchers(Arc::new(PanickingForeignFactory));

    let got = machine
        .download_file(
            manifest_hex,
            None,
            "my-docs".into(),
            "docs/own.bin".into(),
            KEY.to_vec(),
        )
        .await
        .expect("an unbound owned set downloads through the home fetcher");
    assert_eq!(got, expected);
}

/// The do-not-cheat control: a **genuinely foreign** set (no owned
/// row of that name) still resolves foreign and still downloads through
/// `foreign_fetchers`, addressed at its home nest.
#[tokio::test]
async fn a_genuinely_foreign_set_downloads_through_the_foreign_fetchers() {
    let content_key = [0x42u8; 32];
    let (fetcher, manifest_hex, expected) =
        seal_shared_file(&[b"foreign shared ", b"bytes"], content_key);
    let resolver = Arc::new(FakeFolderKeyResolver {
        folder: "their-docs".into(),
        keys: Some(fauna_core::folder_keys::FolderContentKeys::genesis(
            content_key,
            1_000,
        )),
        home: Some("https://peer.example".into()),
        home_actor_id: Some("cd".repeat(32)),
        served: false,
    });
    let factory = Arc::new(RecordingForeignFactory {
        served: fetcher,
        asked: std::sync::Mutex::new(Vec::new()),
    });
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        None,
        None, // no home fetcher at all — the foreign route is the only way
        Some(resolver as Arc<dyn FolderKeyResolver>),
    );
    machine.set_foreign_fetchers(factory.clone());

    let got = machine
        .download_file(
            manifest_hex,
            Some(1),
            "their-docs".into(),
            "docs/theirs.bin".into(),
            KEY.to_vec(),
        )
        .await
        .expect("a foreign set downloads through its home nest's fetcher");
    assert_eq!(got, expected);
    assert_eq!(
        factory.asked.lock().unwrap().as_slice(),
        [("https://peer.example".to_string(), Some("cd".repeat(32)))],
        "the byte route was the foreign set's home nest, dialed under the \
         grant-delivered identity (security.md § Transport trust, the \
         federation-granted row)"
    );
}

/// A machine whose download seam is stocked with `fetcher`.
fn machine_with_download(fetcher: FakeDownloadFetcher) -> Arc<MediaMachine> {
    MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        None,
        Some(Arc::new(fetcher) as Arc<dyn fauna_core::file_download::BlobFetcher>),
        None,
    )
}

/// The external-media / per-item download case end to end: a sealed owner-only
/// file walks back to its plaintext through the machine query, keyed by the hex
/// `manifest_hash` a `FileVersionSummary` carries.
#[tokio::test]
async fn download_file_round_trips_a_sealed_owner_file() {
    let (fetcher, manifest_hex, expected) = seal_owner_file(&[b"RIFF fake ", b"video bytes"]);
    let machine = machine_with_download(fetcher);

    let got = machine
        .download_file(
            manifest_hex,
            None,
            "set".into(),
            "photos/clip.mp4".into(),
            KEY.to_vec(),
        )
        .await
        .expect("sealed owner file downloads");
    assert_eq!(got, expected);
}

/// No download seam wired → a Transient "not supported", no panic — the same
/// contract as the thumbnail / upload seams.
#[tokio::test]
async fn download_file_without_a_fetcher_is_unsupported() {
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        None,
        None,
        None,
    );
    let err = machine
        .download_file(
            "ab".repeat(32),
            None,
            "set".into(),
            "a.mp4".into(),
            KEY.to_vec(),
        )
        .await
        .expect_err("no fetcher → error");
    assert!(matches!(err, MediaApiError::Transient { .. }));
}

/// Caller bugs — a non-hex / wrong-length manifest hash, a wrong-length key —
/// are BadRequest before any fetch is attempted.
#[tokio::test]
async fn download_file_rejects_bad_inputs_before_fetching() {
    let (fetcher, manifest_hex, _) = seal_owner_file(&[b"bytes"]);
    let machine = machine_with_download(fetcher);

    let err = machine
        .download_file(
            "not hex!".into(),
            None,
            "set".into(),
            "a.mp4".into(),
            KEY.to_vec(),
        )
        .await
        .expect_err("non-hex hash");
    assert!(matches!(err, MediaApiError::BadRequest { .. }));

    let err = machine
        .download_file(
            "abcd".into(),
            None,
            "set".into(),
            "a.mp4".into(),
            KEY.to_vec(),
        )
        .await
        .expect_err("wrong-length hash");
    assert!(matches!(err, MediaApiError::BadRequest { .. }));

    let err = machine
        .download_file(
            manifest_hex,
            None,
            "set".into(),
            "a.mp4".into(),
            vec![1, 2, 3],
        )
        .await
        .expect_err("wrong-length key");
    assert!(matches!(err, MediaApiError::BadRequest { .. }));
}

/// The wrong owner key must fail closed (the chunk seal is content-bound), and
/// the error must never be a ciphertext passthrough dressed up as success.
#[tokio::test]
async fn download_file_with_the_wrong_key_fails_closed() {
    let (fetcher, manifest_hex, _) = seal_owner_file(&[b"sealed secret"]);
    let machine = machine_with_download(fetcher);

    let err = machine
        .download_file(
            manifest_hex,
            None,
            "set".into(),
            "a.mp4".into(),
            [9u8; 32].to_vec(),
        )
        .await
        .expect_err("wrong key → fails");
    assert!(matches!(err, MediaApiError::Transient { .. }));
}

/// Arm 1: a Media-page upload's recorded hash names a sealed blob-store
/// primary — `download_file` resolves it via the blob fetcher (fetch → verify
/// the content address → owner-key decrypt), never touching the walk.
#[tokio::test]
async fn download_file_resolves_a_blob_store_primary() {
    let sealed_bytes = sealed(b"the full plaintext clip", &KEY);
    let hash = content_hash_hex(&sealed_bytes);
    let blob = Arc::new(FakeMediaBlobFetcher::new(sealed_bytes));
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        Some(blob.clone() as Arc<dyn MediaBlobFetcher>),
        None, // no walk seam at all — the blob arm must suffice
        None,
    );

    let got = machine
        .download_file(
            hash,
            None,
            "set".into(),
            "photos/clip.mp4".into(),
            KEY.to_vec(),
        )
        .await
        .expect("the blob primary downloads");
    assert_eq!(got, b"the full plaintext clip");
}

/// Arm 1 under a succession: the Media-page primary the successor inherited is
/// still sealed under the RETIRED identity's key. The chunk walk (arm 2) got its
/// retired-root candidates in leg 5's read half, but this arm is the separate
/// raw-AEAD seal that `FileDownloadKeys` does not reach — without its own
/// candidates a successor cannot download a single Media-page-uploaded file.
#[tokio::test]
async fn download_file_opens_a_predecessor_sealed_blob_primary() {
    let sealed_bytes = sealed(b"the full plaintext clip", &RETIRED_KEY);
    let hash = content_hash_hex(&sealed_bytes);
    let blob = Arc::new(FakeMediaBlobFetcher::new(sealed_bytes));
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        Some(blob.clone() as Arc<dyn MediaBlobFetcher>),
        None,
        None,
    );
    machine.set_predecessor_backup_keys(vec![RETIRED_KEY.to_vec()]);

    let got = machine
        .download_file(
            hash,
            None,
            "set".into(),
            "photos/clip.mp4".into(),
            KEY.to_vec(),
        )
        .await
        .expect("the retired root opens the primary it sealed");
    assert_eq!(got, b"the full plaintext clip");
}

/// The typed miss: a hash absent from the blob store falls through to the
/// chunk-store walk (the sync-engine provenance) and still round-trips.
#[tokio::test]
async fn download_file_falls_back_to_the_walk_on_a_blob_miss() {
    let (walk_fetcher, manifest_hex, expected) = seal_owner_file(&[b"chunked clip bytes"]);
    let blob = Arc::new(FakeMediaBlobFetcher::new(Vec::new()));
    blob.fail(MediaApiError::NotFound {
        detail: "no such blob".into(),
    });
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        Some(blob.clone() as Arc<dyn MediaBlobFetcher>),
        Some(Arc::new(walk_fetcher) as Arc<dyn fauna_core::file_download::BlobFetcher>),
        None,
    );

    let got = machine
        .download_file(
            manifest_hex,
            None,
            "set".into(),
            "docs/report.bin".into(),
            KEY.to_vec(),
        )
        .await
        .expect("the walk resolves what the blob store misses");
    assert_eq!(got, expected);
    assert_eq!(
        blob.fetches().len(),
        1,
        "the blob store was consulted first"
    );
}

/// A decrypt failure on the blob arm is a real error — it must NOT fall
/// through to the walk (the blob IS this hash's content).
#[tokio::test]
async fn download_file_blob_decrypt_failure_never_falls_through() {
    let sealed_bytes = sealed(b"secret", &KEY);
    let hash = content_hash_hex(&sealed_bytes);
    let blob = Arc::new(FakeMediaBlobFetcher::new(sealed_bytes));
    let (walk_fetcher, _, _) = seal_owner_file(&[b"unrelated"]);
    let machine = MediaMachine::new(
        CountingObserver::new(),
        Arc::new(FakeMediaNestApi::new()),
        None,
        Some(blob.clone() as Arc<dyn MediaBlobFetcher>),
        Some(Arc::new(walk_fetcher) as Arc<dyn fauna_core::file_download::BlobFetcher>),
        None,
    );

    let err = machine
        .download_file(hash, None, "set".into(), "a.mp4".into(), [9u8; 32].to_vec())
        .await
        .expect_err("wrong key → decrypt fails");
    assert!(matches!(err, MediaApiError::BadRequest { .. }));
}

// ── Sealed-first path render (S2b — the read half of expand) ─────────────────
//
// The write half (`upload_seals_posts_records_then_refreshes` above) proves a
// path leaves the client sealed. These prove the page renders it *back*, and —
// the part that actually gates the flip — that it still does so once the
// plaintext column is gone.

/// A listed item as the nest returns it after the scrub: sealed label + the
/// `path_hash` salt, and a **blanked** plaintext `path`. Blanking is what makes
/// this non-vacuous — a renderer that quietly preferred the plaintext would
/// render an empty name here, not the right one.
fn sealed_item(folder: &str, path: &str, plaintext: &str) -> MediaItem {
    MediaItem {
        folder: folder.into(),
        path: plaintext.into(),
        size_bytes: 10,
        updated_at: 100,
        source_online: true,
        path_sealed: Some(expected_path_seal(path).into()),
        path_hash: Some(fauna_core::sync::path_hash(path).to_vec().into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn refresh_renders_the_path_from_the_seal_with_the_plaintext_scrubbed() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![sealed_item("photos", "photos/eviction_notice.pdf", "")],
        ..Default::default()
    });
    let machine = MediaMachine::new(observer, api, None, None, None, None);

    machine.refresh(Some(KEY.to_vec())).await;

    let snap = machine.snapshot();
    assert_eq!(snap.items.len(), 1, "the sealed row is listable");
    assert_eq!(snap.items[0].path, "photos/eviction_notice.pdf");
    assert_eq!(
        snap.items[0].name, "eviction_notice.pdf",
        "the displayed name derives from the RENDERED path, not the blank one"
    );
}

/// The ratified degrade: a sealed-only row meeting a keyless reader is **omitted
/// from the listing**, not shown with an empty name and not turned into a page
/// error (`file-sync.md` § Sealed names & paths → *Migration*).
#[tokio::test]
async fn refresh_omits_a_sealed_only_row_for_a_keyless_reader() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![
            sealed_item("photos", "photos/a.jpg", ""),
            item("docs", "docs/notes.txt", 30, 100),
        ],
        ..Default::default()
    });
    let machine = MediaMachine::new(observer, api, None, None, None, None);

    machine.refresh(None).await;

    let snap = machine.snapshot();
    assert_eq!(
        snap.items.len(),
        1,
        "the unopenable sealed-only row is omitted; the plaintext row still lists"
    );
    assert_eq!(snap.items[0].path, "docs/notes.txt");
    assert!(snap.error.is_none(), "an omission is not a page error");
}

/// A reader holding the *wrong* key degrades identically — the omission is
/// about what this reader can open, not about whether it passed something.
#[tokio::test]
async fn refresh_omits_a_sealed_only_row_for_the_wrong_key() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![sealed_item("photos", "photos/a.jpg", "")],
        ..Default::default()
    });
    let machine = MediaMachine::new(observer, api, None, None, None, None);

    machine.refresh(Some([9u8; 32].to_vec())).await;

    assert!(machine.snapshot().items.is_empty());
}

/// The expand-phase norm, and the reason this slice ships dark: while the
/// plaintext still rests, a keyless refresh lists exactly what it listed before
/// — sealed rows included.
#[tokio::test]
async fn a_keyless_refresh_still_lists_sealed_rows_that_kept_their_plaintext() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![sealed_item("photos", "photos/a.jpg", "photos/a.jpg")],
        ..Default::default()
    });
    let machine = MediaMachine::new(observer, api, None, None, None, None);

    machine.refresh(None).await;

    let snap = machine.snapshot();
    assert_eq!(snap.items.len(), 1);
    assert_eq!(snap.items[0].path, "photos/a.jpg");
}

/// The salt comes from the wire's `path_hash`, not from the plaintext — proven
/// by perturbing the plaintext to something that hashes differently while
/// leaving the real hash in place. Deriving the salt from `path` would fail the
/// AEAD tag and omit the row.
#[tokio::test]
async fn the_render_salts_from_the_wire_path_hash_not_the_plaintext() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![sealed_item(
            "photos",
            "photos/a.jpg",
            "photos/STALE-DIFFERENT.jpg",
        )],
        ..Default::default()
    });
    let machine = MediaMachine::new(observer, api, None, None, None, None);

    machine.refresh(Some(KEY.to_vec())).await;

    let snap = machine.snapshot();
    assert_eq!(snap.items.len(), 1);
    assert_eq!(
        snap.items[0].path, "photos/a.jpg",
        "the seal wins over a stale plaintext, and it opened under the wire hash"
    );
}

/// A **bound** shared set's label seals under its M2 content-key generation, and
/// the render resolves that custody through the same resolver the byte download
/// uses — one custody object, never two (`FileDownloadKeys`).
#[tokio::test]
async fn refresh_renders_a_bound_shared_sets_label_under_its_content_keys() {
    let content_key = [0x42u8; 32];
    let path = "shared/report.pdf";
    let sealed = fauna_core::path_crypto::seal_convergent(
        &fauna_core::path_crypto::LabelRoot::content_key(content_key, 1),
        &fauna_core::sync::path_hash(path),
        fauna_core::path_crypto::LabelField::SyncChangePath,
        path.as_bytes(),
    )
    .unwrap()
    .to_bytes()
    .unwrap();

    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![MediaItem {
            folder: "shared-docs".into(),
            path: String::new(),
            size_bytes: 10,
            updated_at: 100,
            source_online: true,
            path_sealed: Some(sealed.into()),
            path_hash: Some(fauna_core::sync::path_hash(path).to_vec().into()),
            ..Default::default()
        }],
        ..Default::default()
    });
    let resolver = Arc::new(FakeFolderKeyResolver {
        folder: "shared-docs".into(),
        keys: Some(fauna_core::folder_keys::FolderContentKeys::genesis(
            content_key,
            1_000,
        )),
        home: None,
        home_actor_id: None,
        served: false,
    });
    let machine = MediaMachine::new(
        observer,
        api,
        None,
        None,
        None,
        Some(resolver as Arc<dyn FolderKeyResolver>),
    );

    // A member holds no owner key at all — the content custody is the whole
    // answer, exactly as it is for the set's bytes.
    machine.refresh(None).await;

    let snap = machine.snapshot();
    assert_eq!(snap.items.len(), 1);
    assert_eq!(snap.items[0].path, path);
    assert_eq!(snap.items[0].name, "report.pdf");
}

// ── Sealed-first SET-NAME render (S5c-1 — the set-name axis of the same page) ──
//
// The path tests above prove a file's own label renders back. These prove the
// set *name* on the same row does, under the same custody and the same degrade —
// the plane's second label axis, and the one a Q5 admin is deliberately not
// handed the salt for.

/// The `SealedLabel` an engine must produce for a set `name` sealed under
/// [`KEY`]'s owner root — the set-name twin of [`expected_path_seal`], and
/// recomputed for the same reason: the convergent derivation reproducing
/// byte-identically IS the property under test.
fn expected_set_name_seal(name: &str) -> Vec<u8> {
    fauna_core::label_custody::seal_set_name(
        &fauna_core::path_crypto::LabelRoot::owner_of(&fauna_core::crypto::BackupKey::from_bytes(
            KEY,
        )),
        name,
    )
    .unwrap()
    .expect("a user-chosen (non-reserved) name seals")
}

/// A listed item as the nest returns it to a set's **label audience** after the
/// scrub: both label axes sealed, both salts present, and both plaintexts
/// blanked. Blanking is what makes it non-vacuous — a renderer preferring the
/// plaintext would surface empty strings here, not the real name.
fn sealed_set_item(set_name: &str, path: &str) -> MediaItem {
    MediaItem {
        folder: String::new(),
        path: String::new(),
        size_bytes: 10,
        updated_at: 100,
        source_online: true,
        path_sealed: Some(expected_path_seal(path).into()),
        path_hash: Some(fauna_core::sync::path_hash(path).to_vec().into()),
        folder_sealed: Some(expected_set_name_seal(set_name).into()),
        folder_hash: Some(
            fauna_core::path_crypto::set_name_hash(set_name)
                .to_vec()
                .into(),
        ),
        ..Default::default()
    }
}

/// The audience case: with *both* plaintext columns scrubbed, the owner still
/// gets the set name and the path back.
#[tokio::test]
async fn refresh_renders_the_set_name_from_the_seal_with_the_plaintext_scrubbed() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![sealed_set_item(
            "Family photos",
            "photos/eviction_notice.pdf",
        )],
        ..Default::default()
    });
    let machine = MediaMachine::new(observer, api, None, None, None, None);

    machine.refresh(Some(KEY.to_vec())).await;

    let snap = machine.snapshot();
    assert_eq!(snap.items.len(), 1, "the fully sealed row is listable");
    assert_eq!(
        snap.items[0].folder, "Family photos",
        "the set name renders from its seal, not from the blanked column"
    );
    assert_eq!(snap.items[0].path, "photos/eviction_notice.pdf");
}

/// Two scrubbed sets on one page, one of them BOUND: both plaintext `folder`s
/// are the same blank sentinel, so custody must resolve — and cache — per
/// `folder_hash`. Keyed by the blank plaintext, the bound set resolved
/// owner-only custody (its rows omitted) and the two sets shared one cache
/// slot.
#[tokio::test]
async fn refresh_renders_a_scrubbed_bound_set_beside_an_owned_one_by_folder_hash() {
    let shared = "Shared photos";
    let content = fauna_core::folder_keys::FolderContentKeys::genesis([0x42; 32], 1_000);
    let content_root = fauna_core::path_crypto::LabelRoot::content_key(
        *content.current_key(),
        content.current_version(),
    );
    let bound_path = "trip/beach.jpg";
    let bound_item = MediaItem {
        folder: String::new(),
        path: String::new(),
        size_bytes: 10,
        updated_at: 100,
        source_online: true,
        path_sealed: Some(
            fauna_core::label_custody::seal_path(&content_root, bound_path)
                .unwrap()
                .into(),
        ),
        path_hash: Some(fauna_core::sync::path_hash(bound_path).to_vec().into()),
        folder_sealed: Some(
            fauna_core::label_custody::seal_set_name(&content_root, shared)
                .unwrap()
                .unwrap()
                .into(),
        ),
        folder_hash: Some(
            fauna_core::path_crypto::set_name_hash(shared)
                .to_vec()
                .into(),
        ),
        ..Default::default()
    };
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![
            bound_item,
            sealed_set_item("Family photos", "photos/eviction_notice.pdf"),
        ],
        ..Default::default()
    });
    let machine = MediaMachine::new(
        CountingObserver::new(),
        api,
        None,
        None,
        None,
        Some(
            Arc::new(FakeFolderKeyResolver::bound(shared, Some(content)))
                as Arc<dyn FolderKeyResolver>,
        ),
    );

    machine.refresh(Some(KEY.to_vec())).await;

    let snap = machine.snapshot();
    let mut got: Vec<(String, String)> = snap
        .items
        .iter()
        .map(|i| (i.folder.clone(), i.path.clone()))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            (
                "Family photos".to_string(),
                "photos/eviction_notice.pdf".to_string()
            ),
            (shared.to_string(), bound_path.to_string()),
        ],
        "both scrubbed sets render, each under its own custody"
    );
}

/// The **non-audience projection**, from the reader's side: the nest sends a Q5
/// admin neither the set-name seal nor its salt (`media_handlers.rs` decides
/// that; this pins that the client behaves sanely on the receiving end). During
/// expand the plaintext still rides, so the listing is unchanged — the point is
/// that nothing here depends on the withheld pair.
#[tokio::test]
async fn a_reader_sent_no_set_name_pair_still_lists_on_the_plaintext() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![MediaItem {
            folder: "Family photos".into(),
            path: "photos/a.jpg".into(),
            size_bytes: 10,
            updated_at: 100,
            source_online: true,
            // Exactly what a non-audience reader receives: no seal, no salt.
            folder_sealed: None,
            folder_hash: None,
            ..Default::default()
        }],
        ..Default::default()
    });
    let machine = MediaMachine::new(observer, api, None, None, None, None);

    machine.refresh(None).await;

    let snap = machine.snapshot();
    assert_eq!(snap.items.len(), 1);
    assert_eq!(snap.items[0].folder, "Family photos");
}

/// The degrade on the set-name axis: a reader who cannot open the set's name
/// omits the row rather than surfacing a file under an empty set. The path here
/// is deliberately left *renderable* by the plaintext, so the omission can only
/// come from the set-name axis — proving the two axes are independently
/// load-bearing rather than one masking the other.
#[tokio::test]
async fn refresh_omits_a_row_whose_set_name_this_reader_cannot_open() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![MediaItem {
            folder: String::new(),
            path: "photos/a.jpg".into(),
            size_bytes: 10,
            updated_at: 100,
            source_online: true,
            folder_sealed: Some(expected_set_name_seal("Family photos").into()),
            folder_hash: Some(
                fauna_core::path_crypto::set_name_hash("Family photos")
                    .to_vec()
                    .into(),
            ),
            ..Default::default()
        }],
        ..Default::default()
    });
    let machine = MediaMachine::new(observer, api, None, None, None, None);

    // Wrong key: opens neither axis.
    machine.refresh(Some([9u8; 32].to_vec())).await;

    let snap = machine.snapshot();
    assert!(
        snap.items.is_empty(),
        "an unrenderable set name omits the row, never lists it under an empty set"
    );
    assert!(snap.error.is_none(), "an omission is not a page error");
}

/// The set-name salt comes from the wire's `folder_hash`, not from the
/// plaintext — the hole this slice exists to avoid re-opening. Proven by
/// perturbing the plaintext to a name that hashes differently while leaving the
/// real salt in place: deriving the salt from `folder` would fail the AEAD tag
/// and omit the row.
#[tokio::test]
async fn the_set_name_render_salts_from_the_wire_hash_not_the_plaintext() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![MediaItem {
            folder: "a totally different name".into(),
            path: "photos/a.jpg".into(),
            size_bytes: 10,
            updated_at: 100,
            source_online: true,
            folder_sealed: Some(expected_set_name_seal("Family photos").into()),
            folder_hash: Some(
                fauna_core::path_crypto::set_name_hash("Family photos")
                    .to_vec()
                    .into(),
            ),
            ..Default::default()
        }],
        ..Default::default()
    });
    let machine = MediaMachine::new(observer, api, None, None, None, None);

    machine.refresh(Some(KEY.to_vec())).await;

    let snap = machine.snapshot();
    assert_eq!(snap.items.len(), 1);
    assert_eq!(
        snap.items[0].folder, "Family photos",
        "rendered from the seal under the WIRE salt, overriding the wrong plaintext"
    );
}

// ── the empty-set upload journey (the live all-7-app defect) ────────────────
//
// Found by a live human on tui, 2026-08-03: a folder created in
// Settings → Folders could never be chosen as a Media upload target, because
// both the filter options and the upload default were derived from the media
// items — and a brand-new set has none. The set was therefore invisible in
// `media-folder-filter` AND skipped by the default, so the one action that
// would have given it media was the one action it blocked.

#[tokio::test]
async fn a_brand_new_empty_set_is_offered_as_a_filter_option() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture()); // media lives in "docs" + "photos" only
    api.set_folders(vec![
        folder("docs"),
        folder("photos"),
        folder("fresh"), // just created, still empty
    ]);
    let machine = MediaMachine::new(observer.clone(), api, None, None, None, None);

    machine.refresh(None).await;

    assert_eq!(
        machine.snapshot().folders,
        vec![
            "docs".to_string(),
            "photos".to_string(),
            "fresh".to_string()
        ],
        "an empty set must be selectable in media-folder-filter"
    );
}

#[tokio::test]
async fn uploading_into_a_selected_empty_set_records_into_that_set() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let uploader = Arc::new(FakeMediaBlobUploader::new("cafef00d"));
    api.set_snapshot(fixture());
    api.set_folders(vec![folder("photos"), folder("fresh")]);
    let machine = MediaMachine::new(
        observer.clone(),
        api.clone(),
        Some(uploader.clone()),
        None,
        None,
        None,
    );
    machine.refresh(None).await;

    // The user picks the empty set in the filter, then uploads.
    machine.set_filter(Some("fresh".into()));
    machine
        .upload_selected(
            "dev-1".into(),
            "fresh/first.txt".into(),
            b"hello".to_vec(),
            vec![7u8; 32],
        )
        .await;

    assert!(
        machine.snapshot().error.is_none(),
        "the upload must not error: {:?}",
        machine.snapshot().error
    );
    let recorded: Vec<String> = api
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            MediaNestCall::RecordMember { folder, .. } => Some(folder),
            _ => None,
        })
        .collect();
    assert_eq!(
        recorded,
        vec!["fresh".to_string()],
        "the file lands in the set the user selected"
    );
}

#[tokio::test]
async fn with_only_empty_sets_the_upload_default_still_finds_a_target() {
    // The purest form of the defect: a fresh account whose sets hold nothing at
    // all. Before the fix this reported `media.error_no_set` and uploaded
    // nothing, so the account could never get its first file through Media.
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let uploader = Arc::new(FakeMediaBlobUploader::new("cafef00d"));
    api.set_snapshot(MediaSnapshot::default()); // no media anywhere
    api.set_folders(vec![folder("only-set")]);
    let machine = MediaMachine::new(
        observer.clone(),
        api.clone(),
        Some(uploader.clone()),
        None,
        None,
        None,
    );
    machine.refresh(None).await;

    machine
        .upload_selected(
            "dev-1".into(),
            "only-set/first.txt".into(),
            b"hello".to_vec(),
            vec![7u8; 32],
        )
        .await;

    assert!(
        machine.snapshot().error.is_none(),
        "no set to target is the WRONG answer here: {:?}",
        machine.snapshot().error
    );
    assert!(
        api.calls().iter().any(|c| matches!(
            c,
            MediaNestCall::RecordMember { folder, .. } if folder == "only-set"
        )),
        "the one existing set is the default target"
    );
}

/// A set created sealed rests no plaintext name, so `fauna.folders.list`
/// carries it with a blank `name` beside its seal and hash (`path-sealing.md`
/// § the set-name plane). The upload target renders through the same custody
/// seam as the item rows; before it did, the fresh folder was offered and
/// recorded under `""` — no address, so the nest answered `folder not found`
/// and the signer found no nonce for it.
#[tokio::test]
async fn a_sealed_fresh_folder_renders_and_is_uploaded_into_by_its_name() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let uploader = Arc::new(FakeMediaBlobUploader::new("cafef00d"));
    api.set_snapshot(MediaSnapshot::default());
    api.set_folders(vec![sealed_folder(7, "Holiday")]);
    let machine = MediaMachine::new(
        observer.clone(),
        api.clone(),
        Some(uploader.clone()),
        None,
        None,
        None,
    );
    machine.set_owner_backup_key(KEY.to_vec());
    machine.refresh(Some(KEY.to_vec())).await;
    assert_eq!(machine.snapshot().folders, vec!["Holiday".to_string()]);

    machine
        .upload_selected(
            "dev-1".into(),
            "Holiday/first.txt".into(),
            b"hello".to_vec(),
            vec![7u8; 32],
        )
        .await;

    assert!(
        machine.snapshot().error.is_none(),
        "the upload must not error: {:?}",
        machine.snapshot().error
    );
    assert!(
        api.calls().iter().any(|c| matches!(
            c,
            MediaNestCall::RecordMember { folder, .. } if folder == "Holiday"
        )),
        "the record names the set by its rendered name: {:?}",
        api.calls()
    );
}

/// A sealed set no custody opens is never offered nameless.
#[tokio::test]
async fn a_sealed_folder_no_key_opens_is_not_offered() {
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot::default());
    api.set_folders(vec![sealed_folder(7, "Holiday"), folder("plain")]);
    let machine = MediaMachine::new(CountingObserver::new(), api, None, None, None, None);
    machine.refresh(None).await;
    assert_eq!(machine.snapshot().folders, vec!["plain".to_string()]);
}

/// A control-plane row as the nest lists a set created sealed: the plaintext
/// name blank, the seal and the hash beside it.
fn sealed_folder(id: i64, name: &str) -> MediaFolder {
    MediaFolder {
        id,
        name: String::new(),
        name_sealed: Some(expected_set_name_seal(name)),
        name_hash: Some(fauna_core::path_crypto::set_name_hash(name).to_vec()),
        ..Default::default()
    }
}

#[tokio::test]
async fn every_own_folder_is_an_upload_default_candidate() {
    // A folder has no type (`media.md` § Layout & flow): every own folder
    // records to the one head plane Media reads, so the first listed folder —
    // the Photo Library included — is the upload default.
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let uploader = Arc::new(FakeMediaBlobUploader::new("cafef00d"));
    api.set_snapshot(MediaSnapshot::default());
    api.set_folders(vec![folder("photo-library"), folder("documents")]);
    let machine = MediaMachine::new(
        observer.clone(),
        api.clone(),
        Some(uploader.clone()),
        None,
        None,
        None,
    );
    machine.refresh(None).await;

    assert_eq!(
        machine.snapshot().folders,
        vec!["photo-library".to_string(), "documents".to_string()],
    );

    machine
        .upload_selected(
            "dev-1".into(),
            "photo-library/first.jpg".into(),
            b"hello".to_vec(),
            vec![7u8; 32],
        )
        .await;

    assert!(
        api.calls().iter().any(|c| matches!(
            c,
            MediaNestCall::RecordMember { folder, .. } if folder == "photo-library"
        )),
        "the first own folder is the default target"
    );
}

#[tokio::test]
async fn a_failed_option_list_read_leaves_the_browse_working() {
    // The control-plane read is deliberately non-fatal: media is the read that
    // matters. A transient failure degrades to the
    // item-derived options rather than erroring the whole page.
    let observer = CountingObserver::new();
    let api = Arc::new(FakeOptionListFailure {
        inner: FakeMediaNestApi::new(),
    });
    api.inner.set_snapshot(fixture());
    let machine = MediaMachine::new(observer.clone(), api, None, None, None, None);

    machine.refresh(None).await;
    let snap = machine.snapshot();
    assert!(snap.error.is_none(), "the page still renders");
    assert_eq!(snap.items.len(), 3, "the browse is intact");
    assert_eq!(
        snap.folders,
        vec!["docs".to_string(), "photos".to_string()],
        "options degrade to the sets that have media"
    );
}

/// A seam whose media read succeeds but whose option-list read fails — the
/// transient-failure shape the fake's shared error fixture cannot
/// express (it fails both).
#[derive(Debug)]
struct FakeOptionListFailure {
    inner: FakeMediaNestApi,
}

#[async_trait::async_trait]
impl fauna_media_machine::nest_api::MediaNestApi for FakeOptionListFailure {
    async fn media_snapshot(&self) -> Result<MediaSnapshot, MediaApiError> {
        self.inner.media_snapshot().await
    }
    async fn list_folders(&self) -> Result<Vec<MediaFolder>, MediaApiError> {
        Err(MediaApiError::Transient {
            detail: "unknown kind".into(),
        })
    }
    async fn record_member(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        thumbnail_hash: Option<String>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError> {
        self.inner
            .record_member(
                folder,
                device_id,
                path,
                manifest_hash,
                size_bytes,
                content_key_version,
                thumbnail_hash,
                path_sealed,
            )
            .await
    }
    async fn delete_member(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError> {
        self.inner
            .delete_member(folder, device_id, path, path_sealed)
            .await
    }
    async fn file_versions(
        &self,
        folder: &str,
        path: &str,
        include_pruned: bool,
    ) -> Result<Vec<fauna_media_machine::nest_api::ListedVersion>, MediaApiError> {
        self.inner.file_versions(folder, path, include_pruned).await
    }
    async fn undelete_version(&self, path: &str, version_num: i64) -> Result<(), MediaApiError> {
        self.inner.undelete_version(path, version_num).await
    }
    async fn restore_member(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError> {
        self.inner
            .restore_member(
                folder,
                device_id,
                path,
                manifest_hash,
                size_bytes,
                content_key_version,
                path_sealed,
            )
            .await
    }
    async fn share_register(
        &self,
        request: fauna_client_share::share::ShareCreateRequest,
    ) -> Result<fauna_client_share::share::ShareCreateReply, MediaApiError> {
        self.inner.share_register(request).await
    }
    async fn share_list(
        &self,
    ) -> Result<Vec<fauna_client_share::share::ShareRecord>, MediaApiError> {
        self.inner.share_list().await
    }
    async fn share_revoke(&self, token_id: &str) -> Result<(), MediaApiError> {
        self.inner.share_revoke(token_id).await
    }
}

// ── The followed browse scope (`media.md` § Followed public folders) ─────────
//
// A followed public folder is a parallel, identity-addressed, keyless browse
// SCOPE — never a set in the aggregate, never a custody-resolver key. These
// tests pin the machine half: the option list, the on-demand entry fetch, the
// two failure families (a revoke and a transport fault must never render
// alike), and the read-only routing guard on `download_followed`.

use fauna_media_machine::{
    FollowedFetchError, FollowedFileEntry, FollowedMediaScope, FollowedMediaSource,
};

struct FakeFollowedSource {
    scopes: Vec<FollowedMediaScope>,
    listing: std::sync::Mutex<Result<Vec<FollowedFileEntry>, FollowedFetchError>>,
}

impl FakeFollowedSource {
    fn new(
        scopes: Vec<FollowedMediaScope>,
        listing: Result<Vec<FollowedFileEntry>, FollowedFetchError>,
    ) -> Arc<Self> {
        Arc::new(Self {
            scopes,
            listing: std::sync::Mutex::new(listing),
        })
    }
}

#[async_trait::async_trait]
impl FollowedMediaSource for FakeFollowedSource {
    async fn followed_scopes(&self) -> Vec<FollowedMediaScope> {
        self.scopes.clone()
    }
    async fn fetch_listing(
        &self,
        _folder_id: i64,
        _home_nest_url: &str,
    ) -> Result<Vec<FollowedFileEntry>, FollowedFetchError> {
        self.listing.lock().unwrap().clone()
    }
}

fn followed_scope(name: &str, folder_id: i64) -> FollowedMediaScope {
    FollowedMediaScope {
        folder_id,
        home_nest_url: "https://home.example".into(),
        owner_actor_id: "aabbccdd00112233".into(),
        display_name: name.into(),
        available: true,
        ..Default::default()
    }
}

fn followed_entry(path: &str, seq: i64) -> FollowedFileEntry {
    FollowedFileEntry {
        path: path.into(),
        path_hash: format!("hash-{path}"),
        manifest_hash: format!("manifest-{path}"),
        size_bytes: 10 * seq,
        updated_at: 1_000 + seq,
        thumbnail_hash: None,
        seq,
    }
}

#[tokio::test]
async fn followed_options_render_after_refresh_when_the_source_is_wired() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture());
    let machine = MediaMachine::new(observer, api, None, None, None, None);

    // Unwired: no options — the correct render for an app without the surface.
    machine.refresh(None).await;
    assert!(machine.snapshot().followed.is_empty());
    assert!(machine.snapshot().followed_scope.is_none());

    machine.set_followed_media_source(FakeFollowedSource::new(
        vec![followed_scope("photos", 7)],
        Ok(vec![]),
    ));
    machine.refresh(None).await;
    let snap = machine.snapshot();
    assert_eq!(snap.followed.len(), 1);
    let opt = &snap.followed[0];
    assert!(opt.available);
    assert!(
        opt.label.contains("photos") && opt.label.contains("aabbccdd"),
        "the label disambiguates by owner: {}",
        opt.label
    );
    assert!(
        !snap.folders.contains(&opt.value),
        "a followed option is a scope value, never a set name"
    );
    assert!(snap.followed_scope.is_none(), "offering is not entering");
}

/// The option's owner half is the source's `owner_display` — the same string
/// the Folders page's followed row shows — so the handle, not an id prefix,
/// whenever the source verified one (`ui/media.md` § Followed public folders).
#[tokio::test]
async fn a_followed_option_names_its_owner_as_the_followed_row_does() {
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture());
    let machine = MediaMachine::new(CountingObserver::new(), api, None, None, None, None);
    machine.set_followed_media_source(FakeFollowedSource::new(
        vec![FollowedMediaScope {
            owner_display: "alice".into(),
            ..followed_scope("photos", 7)
        }],
        Ok(vec![]),
    ));
    machine.refresh(None).await;
    assert_eq!(machine.snapshot().followed[0].label, "photos (alice)");
}

#[tokio::test]
async fn selecting_a_followed_scope_fetches_its_listing_on_demand() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture());
    let machine = MediaMachine::new(observer, api, None, None, None, None);
    machine.set_followed_media_source(FakeFollowedSource::new(
        vec![followed_scope("photos", 7)],
        Ok(vec![followed_entry("b.png", 2), followed_entry("a.jpg", 1)]),
    ));
    machine.refresh(None).await;
    let value = machine.snapshot().followed[0].value.clone();

    machine.select_followed_scope(value.clone()).await;
    let snap = machine.snapshot();
    let scope = snap.followed_scope.expect("the scope is active");
    assert_eq!(scope.value, value);
    assert_eq!(
        snap.filter.as_deref(),
        Some(value.as_str()),
        "the filter carries the scope value so the select paints its selection"
    );
    // The listing rendered as ordinary items, name-sorted by the default key.
    let paths: Vec<&str> = snap.items.iter().map(|i| i.path.as_str()).collect();
    assert_eq!(paths, ["a.jpg", "b.png"]);
    assert!(
        snap.items.iter().all(|i| i.folder == "photos"),
        "folder carries the display name, for rendering only"
    );
    assert!(snap.error.is_none());

    // Leaving the scope restores the own-set browse.
    machine.set_filter(None);
    let snap = machine.snapshot();
    assert!(snap.followed_scope.is_none());
    assert_eq!(snap.items.len(), 3, "the aggregate browse is back");
}

#[tokio::test]
async fn an_unavailable_scope_enters_empty_and_loudly() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture());
    let machine = MediaMachine::new(observer, api, None, None, None, None);
    machine.set_followed_media_source(FakeFollowedSource::new(
        vec![followed_scope("photos", 7)],
        Err(FollowedFetchError::Unavailable),
    ));
    machine.refresh(None).await;
    let value = machine.snapshot().followed[0].value.clone();

    machine.select_followed_scope(value).await;
    let snap = machine.snapshot();
    let scope = snap
        .followed_scope
        .expect("the revoke still enters the scope");
    assert!(!scope.available, "the verdict flips on the option too");
    assert!(!snap.followed[0].available);
    assert!(snap.items.is_empty());
    assert_eq!(
        snap.error.expect("the revoke is loud").key,
        "media.error_followed_unavailable",
        "the folded wording — never a per-cause message"
    );
}

#[tokio::test]
async fn a_transport_fault_keeps_the_prior_browse_and_is_not_a_revoke() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(fixture());
    let machine = MediaMachine::new(observer, api, None, None, None, None);
    machine.set_followed_media_source(FakeFollowedSource::new(
        vec![followed_scope("photos", 7)],
        Err(FollowedFetchError::Transport("connection reset".into())),
    ));
    machine.refresh(None).await;
    let value = machine.snapshot().followed[0].value.clone();

    machine.select_followed_scope(value).await;
    let snap = machine.snapshot();
    assert!(
        snap.followed_scope.is_none(),
        "a transport fault does not enter the scope"
    );
    assert!(snap.followed[0].available, "and it is NOT the revoke");
    assert_eq!(snap.items.len(), 3, "the prior browse stands");
    assert_eq!(
        snap.error.expect("but it reports itself").key,
        "media.error_followed_fetch"
    );
}

#[tokio::test]
async fn an_unknown_scope_value_reports_rather_than_silently_no_opping() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let machine = MediaMachine::new(observer, api, None, None, None, None);
    machine.set_followed_media_source(FakeFollowedSource::new(vec![], Ok(vec![])));
    machine.refresh(None).await;

    machine
        .select_followed_scope("followed:99@https://gone.example".into())
        .await;
    let snap = machine.snapshot();
    assert!(snap.followed_scope.is_none());
    assert!(snap.error.is_some(), "never a silent no-op (convention 11)");
}

#[tokio::test]
async fn download_followed_requires_the_active_scope() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let machine = MediaMachine::new(observer, api, None, None, None, None);

    // No scope active: the guard fails loudly before any fetcher is touched —
    // a wrong-scope download must never reach a nest at all.
    let err = machine
        .download_followed("followed:7@https://home.example".into(), "a.jpg".into())
        .await
        .expect_err("no active scope");
    assert!(matches!(err, MediaApiError::BadRequest { .. }));
}

#[tokio::test]
async fn download_followed_resolves_the_manifest_from_the_retained_listing() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let machine = MediaMachine::new(observer, api, None, None, None, None);
    let entry = FollowedFileEntry {
        // A real 32-byte hex manifest, so a resolved path gets past the decode
        // guard to the fetcher pick.
        manifest_hash: "ab".repeat(32),
        ..followed_entry("a.jpg", 1)
    };
    machine.set_followed_media_source(FakeFollowedSource::new(
        vec![followed_scope("photos", 7)],
        Ok(vec![entry]),
    ));
    machine.refresh(None).await;
    let value = machine.snapshot().followed[0].value.clone();
    machine.select_followed_scope(value.clone()).await;

    // A path outside the folded listing fails loudly — the machine, not the
    // app, is the authority on what the followed folder currently holds.
    let err = machine
        .download_followed(value.clone(), "not-listed.jpg".into())
        .await
        .expect_err("unknown path");
    assert!(matches!(err, MediaApiError::BadRequest { .. }));

    // A listed path resolves its head manifest and reaches the fetcher pick;
    // with no fetcher wired on this fixture the refusal is the loud
    // unsupported one — proof the guard passed and the walk was attempted.
    let err = machine
        .download_followed(value, "a.jpg".into())
        .await
        .expect_err("no fetcher wired in this fixture");
    assert!(matches!(err, MediaApiError::Transient { .. }));
}

/// A follow homed on ANOTHER nest downloads through the foreign fetcher
/// factory, addressed at the scope's home and dialed under the identity the
/// public-fetch reply stamped (`security.md` § Transport trust, the
/// federation-granted row) — the follower holds no account on that nest, so
/// this identity is the byte plane's only trust root. The factory here serves
/// an empty fetcher: the assertion is about WHICH home was asked for, under
/// WHAT root, not about the bytes.
#[tokio::test]
async fn download_followed_dials_the_home_nest_under_the_stamped_identity() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let machine = MediaMachine::new(observer, api, None, None, None, None);
    let factory = Arc::new(RecordingForeignFactory {
        served: FakeDownloadFetcher::default(),
        asked: std::sync::Mutex::new(Vec::new()),
    });
    machine.set_foreign_fetchers(factory.clone());
    let entry = FollowedFileEntry {
        manifest_hash: "ab".repeat(32),
        ..followed_entry("a.jpg", 1)
    };
    machine.set_followed_media_source(FakeFollowedSource::new(
        vec![FollowedMediaScope {
            home_nest_actor_id: Some("cd".repeat(32)),
            ..followed_scope("photos", 7)
        }],
        Ok(vec![entry]),
    ));
    machine.refresh(None).await;
    let value = machine.snapshot().followed[0].value.clone();
    machine.select_followed_scope(value.clone()).await;

    // The stocked fetcher holds nothing, so the walk fails at the manifest —
    // AFTER the factory was consulted, which is the fact under test.
    let err = machine
        .download_followed(value, "a.jpg".into())
        .await
        .expect_err("the empty foreign fetcher cannot serve the manifest");
    assert!(matches!(err, MediaApiError::Transient { .. }));
    assert_eq!(
        factory.asked.lock().unwrap().as_slice(),
        [("https://home.example".to_string(), Some("cd".repeat(32)))],
        "the followed download dials the scope's home under the stamped identity"
    );
}

// ── the declassified-folder producer arm ───────────────────────────────────

/// A file uploaded into a folder its owner made public rests **plaintext**.
///
/// The rule is not Media's own: `encryption-at-rest.md` § Readable classes
/// (*Owner-flipped public-audience folders*) says a folder whose owner sets its
/// audience to `public` rests unsealed — content, names and paths — and the
/// sync engine has implemented it since phase 4
/// (`SyncEngine::with_public_audience`). Media is the OTHER producer for the
/// same folders (`media.md` § Layout & flow puts website-published folders in
/// the upload scope; the web-type exclusion was retired 2026-08-17) and sealed
/// `Library` unconditionally until this arm existed — so a file the user
/// dropped into their own public website folder through this page rested as
/// ciphertext, and the nest's `web_files` fan-out published bytes no reader
/// could ever open.
///
/// Asserted on the POSTed bytes rather than on a flag: the whole point is what
/// rests on the nest.
#[tokio::test]
async fn upload_into_a_declassified_folder_rests_plaintext() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_folders(vec![public_folder("site")]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("beef"));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );
    machine.refresh(None).await;

    let raw = b"<h1>hello world</h1>".to_vec();
    machine
        .upload(
            "site".into(),
            "dev-1".into(),
            "index.html".into(),
            raw.clone(),
            KEY.to_vec(),
        )
        .await;

    assert!(
        machine.snapshot().error.is_none(),
        "the gesture succeeds: {:?}",
        machine.snapshot().error
    );
    assert!(uploader.posts().is_empty(), "no blob primary");
    let opened = open_posted_file(
        &uploader,
        &fauna_core::file_download::FileDownloadKeys::default(),
    )
    .await;
    assert_eq!(
        opened, raw,
        "a declassified folder's bytes rest unsealed — sealing them here is what \
         made a public website folder serve ciphertext"
    );
}

/// The (b) leg of the one at-rest shape: a public folder's Media upload is
/// plaintext chunks behind a **plaintext** manifest (`stored_hashes = None`,
/// no sealed hashes) and the record names that manifest — exactly the shape
/// the web serve's one door (`file_bytes::read_file_by_manifest`) and the
/// public share arm read. A reader holding NO key opens it.
#[tokio::test]
async fn upload_into_a_declassified_folder_posts_a_plaintext_manifest_a_keyless_walk_opens() {
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_folders(vec![public_folder("site")]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("never-a-blob-primary"));
    let machine = MediaMachine::new(
        CountingObserver::new(),
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );
    machine.refresh(None).await;

    let raw = served_body();
    machine
        .upload(
            "site".into(),
            "dev-1".into(),
            "photos/c.jpg".into(),
            raw.clone(),
            KEY.to_vec(),
        )
        .await;
    assert!(machine.snapshot().error.is_none());
    assert!(uploader.posts().is_empty(), "no blob primary");

    let (manifest_hash, manifest_bytes, chunks) = posted_file(&uploader);
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).expect("a canonical manifest");
    assert_eq!(manifest.stored_hashes, None, "plaintext chunks");
    assert_eq!(manifest.sealed_hashes, None, "no sealed hashes");
    assert_eq!(manifest.total_size, raw.len() as u64);
    let keys: Vec<[u8; 32]> = chunks.iter().map(|(k, _)| *k).collect();
    let plain: Vec<[u8; 32]> = manifest.chunk_hashes.iter().map(|h| h.digest()).collect();
    assert_eq!(keys, plain, "each chunk keyed by its plaintext hash");

    let recorded = api.calls().into_iter().find_map(|c| match c {
        MediaNestCall::RecordMember {
            manifest_hash,
            size_bytes,
            content_key_version,
            ..
        } => Some((manifest_hash, size_bytes, content_key_version)),
        _ => None,
    });
    assert_eq!(
        recorded,
        Some((hex::encode(manifest_hash), raw.len() as i64, None))
    );

    let opened = open_posted_file(
        &uploader,
        &fauna_core::file_download::FileDownloadKeys::default(),
    )
    .await;
    assert_eq!(opened, raw, "no key needed — the bytes are a URL");
}

/// The same upload records its path **plaintext**, sealing no label.
///
/// The declassification rule covers "content, names, and paths (they are
/// URLs)", and the engine's public arm drops the label root for the same
/// reason. A sealed label here would make the two producers disagree about one
/// folder's rows.
#[tokio::test]
async fn upload_into_a_declassified_folder_records_no_sealed_path() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_folders(vec![public_folder("site"), folder("private")]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("beef"));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );
    machine.refresh(None).await;

    machine
        .upload(
            "site".into(),
            "dev-1".into(),
            "index.html".into(),
            b"<h1>hi</h1>".to_vec(),
            KEY.to_vec(),
        )
        .await;

    let sealed_path = api.calls().into_iter().find_map(|c| match c {
        MediaNestCall::RecordMember {
            folder,
            path_sealed,
            ..
        } if folder == "site" => Some(path_sealed),
        _ => None,
    });
    assert_eq!(
        sealed_path,
        Some(None),
        "a public folder's paths are URLs — the record carries no sealed label"
    );
}

/// A PRIVATE folder is untouched by the new arm: it still seals under the
/// owner's `BackupKey`.
///
/// The fail-closed direction is the whole safety argument — over-sealing a
/// public folder costs a re-record, under-sealing a private one is an
/// unrecoverable disclosure — so this is the assertion that must never flip.
#[tokio::test]
async fn upload_into_an_ordinary_folder_still_seals() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_folders(vec![folder("private")]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("beef"));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );
    machine.refresh(None).await;

    let raw = b"not for the world".to_vec();
    machine
        .upload(
            "private".into(),
            "dev-1".into(),
            "secret.txt".into(),
            raw.clone(),
            KEY.to_vec(),
        )
        .await;

    let (_, manifest_bytes, chunks) = posted_file(&uploader);
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).unwrap();
    assert!(
        manifest.stored_hashes.is_some() && manifest.is_sealed(),
        "a private folder's bytes are sealed before they leave the device"
    );
    assert_sealed_chunks(&chunks, &raw);
}

/// An UNKNOWN folder seals — the fail-closed default.
///
/// The control-plane list can be empty (a transient read
/// failure), and the degrade must never resolve to "rest this in the clear".
/// `FolderSummary::judge_declassification`'s own doc states that direction;
/// this pins that Media inherits it rather than guessing from the name.
#[tokio::test]
async fn upload_into_an_unlisted_folder_seals() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    // No control-plane rows at all — the degrade path.
    api.set_folders(vec![]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("beef"));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );
    machine.refresh(None).await;

    let raw = b"unknown provenance".to_vec();
    machine
        .upload(
            "mystery".into(),
            "dev-1".into(),
            "f.txt".into(),
            raw.clone(),
            KEY.to_vec(),
        )
        .await;

    let (_, manifest_bytes, chunks) = posted_file(&uploader);
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&manifest_bytes).unwrap();
    assert!(
        manifest.stored_hashes.is_some() && manifest.is_sealed(),
        "an unclassifiable folder seals — nothing unparseable may resolve to plaintext"
    );
    assert_sealed_chunks(&chunks, &raw);
}

// ── Content-keyed sets: WebDAV-served (and shared) folders ───────────────────
//
// `webdav-server.md` § Key model → *The app-side seams*; `media.md`
// § Encryption at rest → *Content-keyed sets*. A served set's files rest
// under its M2 content key as `ChunkManifest`s the sync engine's pipeline
// wrote; the Media page must read them under that custody and write into the
// set the same way, or the mount (which holds only the content key) and the
// page (which held only the owner key) each see half the folder.

/// A content-key generation for the served-set fixtures.
fn serve_keys(version: u64, key: [u8; 32]) -> fauna_core::folder_keys::FolderContentKeys {
    let mut keys = fauna_core::folder_keys::FolderContentKeys::genesis([0x11; 32], 1_000);
    for v in 2..=version {
        keys.rotate(if v == version { key } else { [v as u8; 32] }, 1_000 + v);
    }
    if version == 1 {
        keys = fauna_core::folder_keys::FolderContentKeys::genesis(key, 1_000);
    }
    keys
}

/// The `SealedLabel` a content-keyed upload must record for `path`: sealed
/// under the set's CURRENT generation root, convergent mode, salted by the
/// path's own `path_hash` — the root the MDA and every member open names under.
fn expected_content_path_seal(path: &str, key: [u8; 32], version: u64) -> Vec<u8> {
    fauna_core::path_crypto::seal_convergent(
        &fauna_core::path_crypto::LabelRoot::content_key(key, version),
        &fauna_core::sync::path_hash(path),
        fauna_core::path_crypto::LabelField::SyncChangePath,
        path.as_bytes(),
    )
    .unwrap()
    .to_bytes()
    .unwrap()
}

/// A body large enough to exercise chunking + the compressor's arm, and
/// compressible so the framed-seal door matters (`chunk_seal`).
fn served_body() -> Vec<u8> {
    b"a file dropped into a served folder from a phone, over and over. "
        .iter()
        .cycle()
        .take(50_000)
        .copied()
        .collect()
}

/// The (b) leg: an upload into a served set is NOT a Media blob primary — it
/// is the sync engine's own seal (chunks under the set's current content key,
/// a canonical manifest with `stored_hashes`), uploaded over the byte routes
/// and recorded stamped with the generation, its path sealed under the same
/// root. The proof that the mount can open it is the shared walk the MDA
/// runs (`webdav_open_file` IS `download_file_bytes_by_manifest`): fed the
/// posted artifacts and the served set's keys, it round-trips the plaintext.
#[tokio::test]
async fn upload_into_a_served_set_seals_through_the_engines_pipeline_and_the_mda_walk_opens_it() {
    let content_key = [0x5a; 32];
    let keys = serve_keys(3, content_key);
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let uploader = Arc::new(FakeMediaBlobUploader::new("never-a-blob-primary"));
    let resolver = Arc::new(FakeFolderKeyResolver::served("served", Some(keys.clone())));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        Some(resolver as Arc<dyn FolderKeyResolver>),
    );

    let raw = served_body();
    machine
        .upload(
            "served".into(),
            "dev-1".into(),
            "photos/from-phone.bin".into(),
            raw.clone(),
            KEY.to_vec(),
        )
        .await;
    assert!(
        machine.snapshot().error.is_none(),
        "the upload must succeed: {:?}",
        machine.snapshot().error
    );

    // No blob primary: a Media-only shape no manifest reader could open.
    assert!(
        uploader.posts().is_empty(),
        "no `POST /api/v1/blob` for a served set"
    );
    // Every chunk rests under its ciphertext hash (the F9 store-key contract),
    // and the manifest under its own hash.
    let chunks = uploader.chunks();
    assert!(!chunks.is_empty(), "the file was chunked and sealed");
    for (store_key, body) in &chunks {
        assert_eq!(
            *blake3::hash(body).as_bytes(),
            *store_key,
            "a sealed chunk's store key is its ciphertext hash"
        );
        assert_ne!(body.as_slice(), raw.as_slice(), "sealed, not plaintext");
    }
    let manifests = uploader.manifests();
    assert_eq!(manifests.len(), 1, "one manifest");
    let (manifest_hash, manifest_bytes) = &manifests[0];
    assert_eq!(*blake3::hash(manifest_bytes).as_bytes(), *manifest_hash);

    // The record carries the manifest hash, the PLAINTEXT size the engine
    // records, the generation the chunks were sealed under, no thumbnail, and
    // a path sealed under that same generation's root.
    let recorded = api.calls().into_iter().find_map(|c| match c {
        MediaNestCall::RecordMember {
            folder,
            manifest_hash,
            size_bytes,
            content_key_version,
            thumbnail_hash,
            path_sealed,
            ..
        } if folder == "served" => Some((
            manifest_hash,
            size_bytes,
            content_key_version,
            thumbnail_hash,
            path_sealed,
        )),
        _ => None,
    });
    let (rec_hash, size, version, thumb, path_sealed) = recorded.expect("the member is recorded");
    assert_eq!(rec_hash, hex::encode(manifest_hash));
    assert_eq!(size, raw.len() as i64);
    assert_eq!(version, Some(3), "stamped with the sealing generation");
    assert_eq!(thumb, None, "no thumbnail for a content-keyed set");
    assert_eq!(
        path_sealed,
        Some(expected_content_path_seal(
            "photos/from-phone.bin",
            content_key,
            3
        ))
    );

    // The MDA's read: the shared walk, under the served set's content keys
    // alone (no owner key — exactly the MDA's custody), over the posted bytes.
    let mut fetcher = FakeDownloadFetcher::default();
    fetcher.put(
        &fauna_core::data::ContentHash::from_digest_raw(*manifest_hash),
        manifest_bytes.clone(),
    );
    for (store_key, body) in chunks {
        fetcher.put(
            &fauna_core::data::ContentHash::from_digest_raw(store_key),
            body,
        );
    }
    let mda_keys = fauna_core::file_download::FileDownloadKeys {
        content_keys: Some(keys),
        served: true,
        ..Default::default()
    };
    let opened = fauna_core::file_download::download_file_bytes_by_manifest(
        &fetcher,
        &mda_keys,
        fauna_core::data::ContentHash::from_digest_raw(*manifest_hash),
        Some(3),
        "photos/from-phone.bin",
    )
    .await
    .expect("the mount opens what Media wrote");
    assert_eq!(opened, raw, "byte-identical through the served set's keys");
}

/// The fail-closed half: served, but the serve custody has not reached this
/// device's `fauna.state.folder-keys` custody yet. The gesture refuses with a page error and uploads
/// NOTHING — never an owner-key seal the mount could not open, never a
/// plaintext one (`EngineKeyBinding::ServedKeysMissing`'s read-side twin).
#[tokio::test]
async fn upload_into_a_served_set_with_custody_not_yet_synced_refuses_and_uploads_nothing() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let uploader = Arc::new(FakeMediaBlobUploader::new("cafef00d"));
    let resolver = Arc::new(FakeFolderKeyResolver::served("served", None));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        Some(resolver as Arc<dyn FolderKeyResolver>),
    );

    machine
        .upload(
            "served".into(),
            "dev-1".into(),
            "photos/x.bin".into(),
            served_body(),
            KEY.to_vec(),
        )
        .await;

    assert!(
        machine.snapshot().error.is_some(),
        "the refusal is a page error"
    );
    assert!(uploader.posts().is_empty(), "no blob primary");
    assert!(uploader.chunks().is_empty(), "no chunks");
    assert!(uploader.manifests().is_empty(), "no manifest");
    assert!(
        !api.calls()
            .iter()
            .any(|c| matches!(c, MediaNestCall::RecordMember { .. })),
        "nothing recorded"
    );
}

/// The Media upload door keeps no copy, so a metadata-only folder (content
/// stays on the user's devices) is refused in the machine BEFORE anything is
/// sealed or sent — one shared-Rust refusal, shown on `error-message`
/// (`media.md` § User actions, the `upload-button` row). The folder stays an
/// upload target: the user must be able to pick it and read why not.
#[tokio::test]
async fn upload_into_a_metadata_only_folder_refuses_and_sends_nothing() {
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_folders(vec![metadata_only_folder("local-only")]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("beef"));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );
    machine.refresh(None).await;

    machine
        .upload(
            "local-only".into(),
            "dev-1".into(),
            "a.bin".into(),
            b"payload".to_vec(),
            KEY.to_vec(),
        )
        .await;

    assert_eq!(
        machine.snapshot().error.expect("the refusal is loud").key,
        "media.error_metadata_only_folder"
    );
    assert!(uploader.posts().is_empty(), "no blob primary");
    assert!(uploader.chunks().is_empty(), "no chunks");
    assert!(uploader.manifests().is_empty(), "no manifest");
    assert!(
        !api.calls()
            .iter()
            .any(|c| matches!(c, MediaNestCall::RecordMember { .. })),
        "nothing recorded"
    );
    assert!(
        machine
            .snapshot()
            .folder_options
            .iter()
            .any(|o| o.name == "local-only"),
        "the folder stays an upload target — a refusal with a reason, never a missing option"
    );
}

/// The uniform shape (priority #3): a set bound to a sharing group is
/// content-keyed exactly as a served one, and Media's upload into it takes
/// the same engine pipeline — members open it under the group's content
/// keys, and an owner-key blob primary (what this page wrote until 2026-09-26)
/// is never minted for it.
#[tokio::test]
async fn upload_into_a_shared_set_seals_under_the_groups_content_key_never_a_blob_primary() {
    let content_key = [0x6b; 32];
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let uploader = Arc::new(FakeMediaBlobUploader::new("never-a-blob-primary"));
    let resolver = Arc::new(FakeFolderKeyResolver::bound(
        "shared-docs",
        Some(serve_keys(1, content_key)),
    ));
    let machine = MediaMachine::new(
        observer,
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        Some(resolver as Arc<dyn FolderKeyResolver>),
    );

    machine
        .upload(
            "shared-docs".into(),
            "dev-1".into(),
            "notes/plan.txt".into(),
            served_body(),
            KEY.to_vec(),
        )
        .await;

    assert!(machine.snapshot().error.is_none());
    assert!(
        uploader.posts().is_empty(),
        "no blob primary for a shared set"
    );
    assert_eq!(uploader.manifests().len(), 1);
    let version = api.calls().into_iter().find_map(|c| match c {
        MediaNestCall::RecordMember {
            content_key_version,
            path_sealed,
            ..
        } => Some((content_key_version, path_sealed)),
        _ => None,
    });
    assert_eq!(
        version,
        Some((
            Some(1),
            Some(expected_content_path_seal("notes/plan.txt", content_key, 1))
        ))
    );
}

/// The (a) leg on the page: rows the MOUNT wrote into a served set carry
/// names sealed under the set's content-key generation. With the resolver
/// answering the serve custody, the keyed refresh renders them; before the
/// resolver knew about served sets it answered "owner key" and every such row
/// was omitted — the folder view listed `[]` (the iOS witness).
#[tokio::test]
async fn a_served_sets_mount_written_rows_render_under_the_serve_custody() {
    let content_key = [0x7c; 32];
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    let sealed = |path: &str| MediaItem {
        folder: "served".into(),
        path: "".into(), // the plaintext is scrubbed — only the seal names it
        size_bytes: 10,
        updated_at: 100,
        source_online: true,
        path_sealed: Some(expected_content_path_seal(path, content_key, 2).into()),
        path_hash: Some(fauna_core::sync::path_hash(path).to_vec().into()),
        ..Default::default()
    };
    api.set_snapshot(MediaSnapshot {
        items: vec![sealed("keep.txt"), sealed("sub/moved.txt")],
        ..Default::default()
    });
    let resolver = Arc::new(FakeFolderKeyResolver::served(
        "served",
        Some(serve_keys(2, content_key)),
    ));
    let machine = MediaMachine::new(
        observer,
        api,
        None,
        None,
        None,
        Some(resolver as Arc<dyn FolderKeyResolver>),
    );

    machine.refresh(Some(KEY.to_vec())).await;

    let snap = machine.snapshot();
    let mut names: Vec<String> = snap.items.iter().map(|it| it.path.clone()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["keep.txt".to_string(), "sub/moved.txt".to_string()],
        "every mount-written row renders under the serve custody"
    );
    assert!(snap.error.is_none());
}

/// A resolver answering the served-then-UNFLAGGED shape: owner-only again,
/// with the served window's rotated-out generations as read candidates.
struct RetiredServeResolver {
    folder: String,
    retired: fauna_core::folder_keys::FolderContentKeys,
}

#[async_trait::async_trait]
impl FolderKeyResolver for RetiredServeResolver {
    async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        Ok(
            if *name_hash == fauna_core::path_crypto::set_name_hash(&self.folder) {
                ResolvedCustody::OwnerOnly {
                    retired_content_keys: Some(self.retired.clone()),
                }
            } else {
                ResolvedCustody::owner_only()
            },
        )
    }
}

/// Outcome 8's page half: after the owner stops serving, the rows the mount
/// wrote during the served window still render — under the retired
/// generations the owner's custody kept, beside the owner root (never as a
/// seal root). Without them the unflag would make the served-era files
/// vanish from the library.
#[tokio::test]
async fn an_unflagged_sets_served_era_rows_still_render_via_the_retired_generations() {
    let served_era_key = [0x8d; 32];
    let observer = CountingObserver::new();
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![
            MediaItem {
                folder: "was-served".into(),
                path: "".into(),
                size_bytes: 10,
                updated_at: 100,
                source_online: true,
                path_sealed: Some(
                    expected_content_path_seal("served-era.txt", served_era_key, 1).into(),
                ),
                path_hash: Some(
                    fauna_core::sync::path_hash("served-era.txt")
                        .to_vec()
                        .into(),
                ),
                ..Default::default()
            },
            // An owner-root row from before serving renders as ever.
            sealed_item("was-served", "pre-serve.txt", ""),
        ],
        ..Default::default()
    });
    // Unflagging rotated gen 1 → gen 2; gen 1 stays in the retired history.
    let mut retired = fauna_core::folder_keys::FolderContentKeys::genesis(served_era_key, 1_000);
    retired.rotate([0x8e; 32], 2_000);
    let resolver = Arc::new(RetiredServeResolver {
        folder: "was-served".into(),
        retired,
    });
    let machine = MediaMachine::new(
        observer,
        api,
        None,
        None,
        None,
        Some(resolver as Arc<dyn FolderKeyResolver>),
    );

    machine.refresh(Some(KEY.to_vec())).await;

    let snap = machine.snapshot();
    let mut names: Vec<String> = snap.items.iter().map(|it| it.path.clone()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["pre-serve.txt".to_string(), "served-era.txt".to_string()],
        "the served-era row renders via the retired generation, the owner-root row as ever"
    );
}

// ── share links (`share-links.md` § Flows) ──────────────────────────────────

const SHARE_SECRET: [u8; 32] = [3u8; 32];

fn version(num: i64, manifest_hash: String) -> fauna_media_machine::snapshots::FileVersionSummary {
    fauna_media_machine::snapshots::FileVersionSummary {
        version_num: num,
        manifest_hash,
        size_bytes: 42,
        created_at: 1_000,
        content_key_version: None,
        author_display: "alice".into(),
        pruned: false,
        purge_after: None,
    }
}

/// A machine over one public set (`site`, holding `site/index.jpg`) and one
/// private set (`docs`), author wired, the file's current version fixtured.
async fn share_machine() -> (Arc<MediaMachine>, Arc<FakeMediaNestApi>) {
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![
            item("site", "site/index.jpg", 10, 100),
            item("docs", "docs/notes.txt", 30, 100),
        ],
        ..Default::default()
    });
    api.set_folders(vec![public_folder("site"), folder("docs")]);
    api.set_versions(vec![
        version(1, "11".repeat(32)),
        version(2, "22".repeat(32)),
    ]);
    let machine = MediaMachine::new(CountingObserver::new(), api.clone(), None, None, None, None);
    machine.set_share_author(SHARE_SECRET.to_vec(), "https://nest.example".into());
    machine.refresh(None).await;
    (machine, api)
}

#[tokio::test]
async fn share_link_eligibility_is_the_public_audience_folder_and_nothing_else() {
    let (machine, _api) = share_machine().await;
    let snap = machine.snapshot();
    let eligible = |path: &str| {
        snap.items
            .iter()
            .find(|i| i.path == path)
            .unwrap()
            .share_link_eligible
    };
    assert!(eligible("site/index.jpg"));
    // `docs` is not reported owner-only (a bound or served set reads so).
    assert!(!eligible("docs/notes.txt"));
    // The create surface refuses to open on an ineligible file.
    machine.open_share_create("docs".into(), "docs/notes.txt".into());
    assert!(machine.snapshot().share_create.is_none());
    assert_eq!(snap.share_expiry_options, ["1d", "7d", "30d", "1y"]);
}

// ── Private (fragment-keyed) links — `share-links.md` § The private-file
//    extension: an owner-only folder's sealed file is linkable, the link
//    carries its key after `#`, and the create surface says so. ─────────────

/// A set the control plane reports owner-only — unbound, not served.
fn owner_only_folder(name: &str) -> MediaFolder {
    MediaFolder {
        owner_only: true,
        ..folder(name)
    }
}

/// A machine over one owner-only set (`vault`, holding `vault/plan.txt`, a
/// real chunk manifest sealed under the share author's own root) and one
/// public set, with the download fetcher the private arm reads the manifest
/// through. `author` false leaves the share author unwired.
async fn private_share_machine(
    author: bool,
) -> (Arc<MediaMachine>, Arc<FakeMediaNestApi>, Vec<u8>) {
    let owner = BackupKey::derive(&SHARE_SECRET);
    let (fetcher, manifest_hex, plaintext) =
        seal_file_under_owner(&[b"the first chunk ", b"and the second"], &owner);
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![
            item("vault", "vault/plan.txt", 30, 100),
            item("site", "site/index.jpg", 10, 100),
        ],
        ..Default::default()
    });
    api.set_folders(vec![owner_only_folder("vault"), public_folder("site")]);
    api.set_versions(vec![version(1, manifest_hex)]);
    let machine = MediaMachine::new(
        CountingObserver::new(),
        api.clone(),
        None,
        None,
        Some(Arc::new(fetcher)),
        None,
    );
    if author {
        machine.set_share_author(SHARE_SECRET.to_vec(), "https://nest.example".into());
    }
    machine.refresh(None).await;
    (machine, api, plaintext)
}

fn eligible(machine: &MediaMachine, path: &str) -> bool {
    machine
        .snapshot()
        .items
        .iter()
        .find(|i| i.path == path)
        .unwrap()
        .share_link_eligible
}

#[tokio::test]
async fn an_owner_only_file_is_linkable_once_the_seat_holds_the_owner_root() {
    let (machine, _api, _) = private_share_machine(false).await;
    assert!(
        !eligible(&machine, "vault/plan.txt"),
        "no root held, no private link"
    );
    machine.open_share_create("vault".into(), "vault/plan.txt".into());
    assert!(machine.snapshot().share_create.is_none());

    machine.set_share_author(SHARE_SECRET.to_vec(), "https://nest.example".into());
    assert!(eligible(&machine, "vault/plan.txt"));
    assert!(eligible(&machine, "site/index.jpg"));
}

#[tokio::test]
async fn a_private_link_carries_its_key_in_the_fragment_and_says_so() {
    let (machine, api, plaintext) = private_share_machine(true).await;

    // The public file's surface carries no key notice.
    machine.open_share_create("site".into(), "site/index.jpg".into());
    assert!(!machine.snapshot().share_create.unwrap().key_in_fragment);
    machine.close_share_create();

    machine.open_share_create("vault".into(), "vault/plan.txt".into());
    let open = machine.snapshot().share_create.expect("open");
    assert!(open.key_in_fragment, "the sealed arm paints the key notice");
    assert_eq!(open.url, None);

    machine.create_share_link().await;
    let url = machine
        .snapshot()
        .share_create
        .unwrap()
        .url
        .expect("revealed after registration");
    let (path, key) = url.split_once('#').expect("the key rides the fragment");
    let token = path.rsplit('/').next().unwrap();
    let minted = fauna_core::share::ShareToken::from_base64url(token).unwrap();
    assert!(minted.key_in_fragment);
    assert!(
        minted.filename.is_empty(),
        "the name rides the envelope only"
    );
    assert!(!key.is_empty());
    // The registered row: the sealed list name, flagged fragment-keyed, and
    // no Copy — the key is not on the nest to re-derive it from.
    let rows = api.shares();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].key_in_fragment);
    assert!(!rows[0].filename_sealed.is_empty());
    machine.close_share_create();
    machine.open_share_links().await;
    let list = machine.snapshot().share_links;
    assert_eq!(list.rows.len(), 1);
    assert_eq!(list.rows[0].name, "plan.txt");
    assert_eq!(list.rows[0].url, None, "no Copy on a private row");
    // Nothing the link carries is the plaintext.
    assert!(
        !url.as_bytes()
            .windows(8)
            .any(|w| plaintext.windows(8).any(|p| p == w))
    );
}

#[tokio::test]
async fn a_private_link_to_a_file_with_no_chunk_manifest_fails_without_a_url() {
    // A version naming no manifest the nest holds (a lying or pruned row) —
    // the create fails closed rather than register an envelope it never built.
    let (machine, api, _) = private_share_machine(true).await;
    api.set_versions(vec![version(1, "33".repeat(32))]);
    machine.open_share_create("vault".into(), "vault/plan.txt".into());
    machine.create_share_link().await;
    let snap = machine.snapshot();
    let create = snap.share_create.expect("stays open");
    assert_eq!(create.url, None);
    assert_eq!(
        snap.error.expect("error-message").key,
        "share_link.error_create"
    );
    assert!(api.shares().is_empty(), "nothing registered");
}

/// The (a) leg of the one at-rest shape (`media.md` § Encryption at rest →
/// *One at-rest shape*): a file the Media page ITSELF uploaded into an
/// owner-only folder takes a private link, and the link's holder — the key
/// from the URL fragment, the registered envelope, the chunks the upload
/// posted, and nothing else — opens the original bytes. The fixtures above
/// seal a manifest by hand; here the gesture must produce it.
#[tokio::test]
async fn a_media_upload_into_an_owner_only_folder_takes_a_private_link_its_holder_opens() {
    let owner = BackupKey::derive(&SHARE_SECRET);
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_folders(vec![owner_only_folder("vault")]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("never-a-blob-primary"));
    let upload_machine = MediaMachine::new(
        CountingObserver::new(),
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        None,
        None,
        None,
    );
    upload_machine.refresh(None).await;
    let raw = served_body();
    upload_machine
        .upload(
            "vault".into(),
            "dev-1".into(),
            "vault/plan.txt".into(),
            raw.clone(),
            owner.to_bytes().to_vec(),
        )
        .await;
    assert!(
        upload_machine.snapshot().error.is_none(),
        "{:?}",
        upload_machine.snapshot().error
    );
    let recorded = api
        .calls()
        .into_iter()
        .find_map(|c| match c {
            MediaNestCall::RecordMember { manifest_hash, .. } => Some(manifest_hash),
            _ => None,
        })
        .expect("recorded");

    // The seat that mints reads the nest the upload wrote: the listing, the
    // version naming the recorded manifest, the posted bytes.
    api.set_snapshot(MediaSnapshot {
        items: vec![item("vault", "vault/plan.txt", raw.len() as i64, 100)],
        ..Default::default()
    });
    api.set_versions(vec![version(1, recorded)]);
    let machine = MediaMachine::new(
        CountingObserver::new(),
        api.clone(),
        None,
        None,
        Some(Arc::new(fetcher_over(&uploader))),
        None,
    );
    machine.set_share_author(SHARE_SECRET.to_vec(), "https://nest.example".into());
    machine.refresh(None).await;
    assert!(eligible(&machine, "vault/plan.txt"));
    machine.open_share_create("vault".into(), "vault/plan.txt".into());
    machine.create_share_link().await;
    let snap = machine.snapshot();
    assert!(snap.error.is_none(), "{:?}", snap.error);
    let url = snap.share_create.unwrap().url.expect("a private link");
    let (_, fragment) = url.split_once('#').expect("the key rides the fragment");

    let envelopes = api.share_envelopes();
    assert_eq!(envelopes.len(), 1);
    let sealed = envelopes[0].clone().expect("a key envelope was registered");
    let link_key = fauna_core::share::decode_link_key(fragment).unwrap();
    let envelope = fauna_core::share::KeyEnvelope::open(&link_key, &sealed).unwrap();
    let ciphertexts: Vec<Vec<u8>> = uploader.chunks().into_iter().map(|(_, b)| b).collect();
    assert_eq!(
        envelope
            .open_file(&ciphertexts)
            .expect("the holder opens it"),
        raw,
        "the link's holder recovers the uploaded bytes"
    );
}

/// A deep link's located item opens the same detail as the listed one: the
/// Explorer Share hand-off lands on `locate_path` (windows.md § Shell Extension →
/// *The Share hand-off*) and must see the share verdict, or the detail it opens
/// would lack the very control the route asked for.
#[tokio::test]
async fn a_located_item_carries_the_share_verdict_the_browse_shows() {
    let (machine, _api) = share_machine().await;
    let site_id = folder("site").id;
    let located = machine
        .locate_path(site_id, "site/index.jpg".into())
        .expect("located by (set id, path)");
    assert_eq!(located.folder, "site");
    assert!(located.share_link_eligible);
    assert!(
        machine
            .locate_path(site_id, "site/missing.jpg".into())
            .is_none()
    );
    assert!(machine.locate_path(404, "site/index.jpg".into()).is_none());
}

#[tokio::test]
async fn a_created_link_names_the_current_version_and_reveals_only_after_registration() {
    let (machine, api) = share_machine().await;
    machine.open_share_create("site".into(), "site/index.jpg".into());
    let open = machine.snapshot().share_create.expect("open");
    assert_eq!(open.name, "index.jpg");
    assert_eq!(open.expiry, "7d");
    assert_eq!(open.url, None);

    machine.set_share_expiry("30d".into());
    machine.set_share_expiry("never".into()); // refused
    machine.create_share_link().await;

    let created = machine.snapshot().share_create.expect("still open");
    let url = created.url.expect("revealed after registration");
    assert!(url.starts_with("https://nest.example/share/"));
    let token = url.rsplit('/').next().unwrap();
    let minted = fauna_core::share::ShareToken::from_base64url(token).unwrap();
    assert_eq!(minted.manifest_hash, [0x22; 32], "the newest version");
    assert_eq!(minted.filename, "index.jpg");
    // The registry rests the seal, not the name.
    let rows = api.shares();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].filename_sealed.is_empty());
    // 30 days, not the default.
    let lifetime = minted.expires as i64 - fauna_core::data::Timestamp::now_secs();
    assert!((29 * 86_400..=30 * 86_400).contains(&lifetime));
}

#[tokio::test]
async fn a_failed_registration_shows_no_url_and_keeps_the_surface_open() {
    let (machine, api) = share_machine().await;
    api.fail_shares(Some(MediaApiError::Transient {
        detail: "offline".into(),
    }));
    machine.open_share_create("site".into(), "site/index.jpg".into());
    machine.create_share_link().await;
    let snap = machine.snapshot();
    let create = snap.share_create.expect("stays open for a retry");
    assert_eq!(create.url, None);
    assert!(!create.busy);
    assert_eq!(
        snap.error.expect("error-message").key,
        "share_link.error_create"
    );
    // A retry after the nest recovers succeeds.
    api.fail_shares(None);
    machine.create_share_link().await;
    assert!(machine.snapshot().share_create.unwrap().url.is_some());
}

#[tokio::test]
async fn the_list_loads_renders_the_sealed_name_and_revoke_needs_its_confirm() {
    let (machine, api) = share_machine().await;
    machine.open_share_create("site".into(), "site/index.jpg".into());
    machine.create_share_link().await;
    let url = machine.snapshot().share_create.unwrap().url.unwrap();
    machine.close_share_create();

    machine.open_share_links().await;
    let list = machine.snapshot().share_links;
    assert!(list.open && list.loaded);
    assert_eq!(list.rows.len(), 1);
    let row = &list.rows[0];
    assert_eq!(row.name, "index.jpg", "opened from the seal");
    assert_eq!(row.state, "active");
    assert_eq!(
        row.url.as_deref(),
        Some(url.as_str()),
        "Copy re-derives the URL"
    );

    let token_id = row.token_id.clone();
    machine.arm_share_revoke(token_id.clone());
    assert_eq!(
        machine.snapshot().share_links.revoke_confirm.as_deref(),
        Some(token_id.as_str())
    );
    machine.cancel_share_revoke();
    assert!(
        !api.calls()
            .iter()
            .any(|c| matches!(c, MediaNestCall::ShareRevoke { .. }))
    );

    machine.arm_share_revoke(token_id.clone());
    machine.confirm_share_revoke().await;
    let list = machine.snapshot().share_links;
    assert_eq!(list.rows[0].state, "revoked", "the row stays, revoked");
    assert_eq!(list.rows[0].url, None, "no copy on a revoked row");
    assert_eq!(list.revoke_confirm, None);
    // A revoked row cannot be re-armed.
    machine.arm_share_revoke(token_id);
    assert_eq!(machine.snapshot().share_links.revoke_confirm, None);
}

#[tokio::test]
async fn an_empty_share_list_is_loaded_and_empty_never_unloaded() {
    let (machine, _api) = share_machine().await;
    assert!(!machine.snapshot().share_links.open);
    machine.open_share_links().await;
    let list = machine.snapshot().share_links;
    assert!(list.open && list.loaded && list.rows.is_empty());
    machine.close_share_links();
    assert_eq!(
        machine.snapshot().share_links,
        fauna_media_machine::snapshots::ShareLinksSnapshot::default()
    );
}

// ── Ruling (8)(c)/(d): a row signed under a retired identity ────────────────
//
// `mls-group-key-material.md` § M2 → *Writer-signed change records*. After a
// succession the predecessor's rows verify as the account's own — and that is
// exactly what must NOT hand them the account's CURRENT owner root: the retired
// seed plus a lying nest could otherwise name, in one of the account's sets, the
// manifest, sealed path or thumbnail of a file the successor created after the
// ceremony, and this page would open it there. The seam reports who signed each
// row (`FakeMediaNestApi::set_predecessor_signed` stands for the judge's
// verdict); every open below is keyed on it.

/// `bytes` as one sealed chunk manifest under `key`'s owner root, stocked into
/// `fetcher`; the manifest's hex hash.
fn stock_owner_manifest(fetcher: &mut FakeDownloadFetcher, key: &[u8; 32], bytes: &[u8]) -> String {
    let sealed = fauna_core::blob_seal::seal_blob(
        bytes,
        Some((BackupKey::from_bytes(*key).convergent_chunk_root(), None)),
    )
    .expect("seal");
    for (store_key, body) in sealed.chunks {
        fetcher.put(&store_key, body);
    }
    fetcher.put(&sealed.manifest_hash, sealed.manifest_bytes);
    hex::encode(sealed.manifest_hash.digest())
}

/// The `SealedLabel` of `path` under `key`'s owner root ([`expected_path_seal`]
/// for any key).
fn path_seal_under(key: &[u8; 32], path: &str) -> Vec<u8> {
    fauna_core::path_crypto::seal_convergent(
        &fauna_core::path_crypto::LabelRoot::owner_of(&BackupKey::from_bytes(*key)),
        &fauna_core::sync::path_hash(path),
        fauna_core::path_crypto::LabelField::SyncChangePath,
        path.as_bytes(),
    )
    .unwrap()
    .to_bytes()
    .unwrap()
}

/// A listed item naming `manifest_hex`, its path sealed under `label_key`, the
/// plaintext column scrubbed.
fn item_naming(path: &str, manifest_hex: &str, label_key: &[u8; 32]) -> MediaItem {
    MediaItem {
        folder: "photos".into(),
        path: String::new(),
        size_bytes: 10,
        updated_at: 100,
        source_online: true,
        path_sealed: Some(path_seal_under(label_key, path).into()),
        path_hash: Some(fauna_core::sync::path_hash(path).to_vec().into()),
        manifest_hash: Some(hex::decode(manifest_hex).unwrap().into()),
        ..Default::default()
    }
}

fn hex_decode32(hex_hash: &str) -> [u8; 32] {
    hex::decode(hex_hash).unwrap().try_into().unwrap()
}

/// **The attack, at the Media page.** Three items the judge reported as signed
/// under a RETIRED identity: one naming a manifest the successor sealed under
/// its CURRENT root (planted), one whose *path* was sealed under the current
/// root (planted), and the genuine inherited file, sealed under the retired
/// root. The planted name does not render, the planted bytes do not open, the
/// inherited file does both — and the very same current-root manifest opens
/// for a row the current identity signed.
#[tokio::test]
async fn a_predecessor_signed_item_never_opens_under_the_current_root() {
    let mut fetcher = FakeDownloadFetcher::default();
    let after = stock_owner_manifest(&mut fetcher, &KEY, b"created after the succession");
    let before = stock_owner_manifest(&mut fetcher, &RETIRED_KEY, b"created before it");
    let mine = stock_owner_manifest(&mut fetcher, &KEY, b"the successor's own upload");

    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![
            // Bytes planted: a retired-root NAME over a current-root manifest.
            item_naming("photos/planted-bytes.bin", &after, &RETIRED_KEY),
            // Name planted: a current-root sealed path.
            item_naming("photos/planted-name.bin", &before, &KEY),
            // The inherited file: retired root for both.
            item_naming("photos/inherited.bin", &before, &RETIRED_KEY),
            // The successor's own row.
            item_naming("photos/mine.bin", &mine, &KEY),
        ],
        ..Default::default()
    });
    api.set_predecessor_signed(vec![after.clone(), before.clone()]);
    let machine = MediaMachine::new(
        CountingObserver::new(),
        api,
        None,
        None,
        Some(Arc::new(fetcher) as Arc<dyn fauna_core::file_download::BlobFetcher>),
        None,
    );
    machine.set_predecessor_chain(
        vec![FAKE_PREDECESSOR_ID.to_vec()],
        vec![RETIRED_KEY.to_vec()],
    );
    machine.refresh(Some(KEY.to_vec())).await;

    let mut listed: Vec<String> = machine
        .snapshot()
        .items
        .into_iter()
        .map(|i| i.path)
        .collect();
    listed.sort();
    assert_eq!(
        listed,
        [
            "photos/inherited.bin",
            "photos/mine.bin",
            "photos/planted-bytes.bin",
        ],
        "a path sealed under the CURRENT root does not render under a predecessor's signature"
    );

    let download = |manifest: &str, path: &str| {
        machine.download_file(
            manifest.to_string(),
            None,
            "photos".into(),
            path.to_string(),
            KEY.to_vec(),
        )
    };
    download(&after, "photos/planted-bytes.bin")
        .await
        .expect_err("a current-root manifest must not open under a predecessor's signature");
    assert_eq!(
        download(&before, "photos/inherited.bin")
            .await
            .expect("the retired root opens what it sealed"),
        b"created before it"
    );
    assert_eq!(
        download(&mine, "photos/mine.bin")
            .await
            .expect("the current identity's own row opens under its root"),
        b"the successor's own upload"
    );
}

/// The bare-key twin: a Media-page blob primary and a thumbnail that only a
/// predecessor's signature names are offered the retired keys alone.
#[tokio::test]
async fn a_predecessor_signed_blob_is_offered_the_retired_keys_alone() {
    for (seal_key, opens) in [(KEY, false), (RETIRED_KEY, true)] {
        let sealed_bytes = sealed(b"blob bytes", &seal_key);
        let hash = content_hash_hex(&sealed_bytes);
        let api = Arc::new(FakeMediaNestApi::new());
        api.set_snapshot(MediaSnapshot {
            items: vec![MediaItem {
                thumbnail_hash: Some(hash.clone()),
                manifest_hash: Some(hex::decode(&hash).unwrap().into()),
                ..item("photos", "photos/a.jpg", 10, 100)
            }],
            ..Default::default()
        });
        api.set_predecessor_signed(vec!["photos/a.jpg".into()]);
        let machine = MediaMachine::new(
            CountingObserver::new(),
            api,
            None,
            Some(Arc::new(FakeMediaBlobFetcher::new(sealed_bytes)) as Arc<dyn MediaBlobFetcher>),
            None,
            None,
        );
        machine.set_predecessor_chain(
            vec![FAKE_PREDECESSOR_ID.to_vec()],
            vec![RETIRED_KEY.to_vec()],
        );
        machine.refresh(Some(KEY.to_vec())).await;

        let thumbnail = machine.fetch_thumbnail(hash.clone(), KEY.to_vec()).await;
        let primary = machine
            .download_file(
                hash,
                None,
                "photos".into(),
                "photos/a.jpg".into(),
                KEY.to_vec(),
            )
            .await;
        assert_eq!(
            thumbnail.is_ok(),
            opens,
            "thumbnail sealed under {seal_key:?}"
        );
        assert_eq!(primary.is_ok(), opens, "primary sealed under {seal_key:?}");
    }
}

/// The grandpredecessor in the per-signer chain pins: successor ([`KEY`]) ←
/// predecessor ([`FAKE_PREDECESSOR_ID`], [`RETIRED_KEY`]) ← grandpredecessor.
const GRAND_ID: [u8; 32] = [0x6A; 32];
const GRAND_KEY: [u8; 32] = [0x6B; 32];

/// Ruling (8)(c), the per-signer bound, at the Media page: a row signed as
/// the grandpredecessor never opens what the predecessor (a LATER root in the
/// chain) sealed — nor the current root — while the predecessor's row opens
/// the grandpredecessor's bytes. The planted pairing is a name the right
/// identity sealed over bytes a later one did.
#[tokio::test]
async fn a_signer_opens_only_its_own_root_and_earlier_ones_on_the_media_page() {
    let mut fetcher = FakeDownloadFetcher::default();
    let by_pred = stock_owner_manifest(&mut fetcher, &RETIRED_KEY, b"the predecessor's file");
    let by_grand = stock_owner_manifest(&mut fetcher, &GRAND_KEY, b"the grandpredecessor's");
    let by_current = stock_owner_manifest(&mut fetcher, &KEY, b"after the ceremony");
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_snapshot(MediaSnapshot {
        items: vec![
            // Signed as the grandpredecessor, naming a later root's bytes.
            item_naming("photos/g-over-p.bin", &by_pred, &GRAND_KEY),
            item_naming("photos/g-over-s.bin", &by_current, &GRAND_KEY),
            // Signed as the predecessor, naming an earlier root's bytes.
            item_naming("photos/p-over-g.bin", &by_grand, &RETIRED_KEY),
        ],
        ..Default::default()
    });
    api.set_signed_as(vec![by_pred.clone(), by_current.clone()], GRAND_ID);
    api.add_signed_as(vec![by_grand.clone()], FAKE_PREDECESSOR_ID);
    let machine = MediaMachine::new(
        CountingObserver::new(),
        api,
        None,
        None,
        Some(Arc::new(fetcher) as Arc<dyn fauna_core::file_download::BlobFetcher>),
        None,
    );
    machine.set_predecessor_chain(
        vec![FAKE_PREDECESSOR_ID.to_vec(), GRAND_ID.to_vec()],
        vec![RETIRED_KEY.to_vec(), GRAND_KEY.to_vec()],
    );
    machine.refresh(Some(KEY.to_vec())).await;
    let download = |manifest: &str, path: &str| {
        machine.download_file(
            manifest.to_string(),
            None,
            "photos".into(),
            path.to_string(),
            KEY.to_vec(),
        )
    };
    download(&by_pred, "photos/g-over-p.bin")
        .await
        .expect_err("a grandpredecessor's signature must not open a later predecessor's root");
    download(&by_current, "photos/g-over-s.bin")
        .await
        .expect_err("…nor the current root");
    assert_eq!(
        download(&by_grand, "photos/p-over-g.bin")
            .await
            .expect("a predecessor's signature opens its own predecessor's root"),
        b"the grandpredecessor's"
    );
}

/// A retired key the app handed over **without its identity** opens nothing
/// a predecessor signed — it cannot be placed in the chain — and another
/// writer's row is offered no owner root at all, even over bytes the account's
/// own retired root sealed.
#[tokio::test]
async fn an_unpaired_key_or_another_writer_opens_no_predecessor_signed_row() {
    for (pairing, signer) in [
        ("unpaired", FAKE_PREDECESSOR_ID),
        ("stranger", [0x5Eu8; 32]),
    ] {
        let mut fetcher = FakeDownloadFetcher::default();
        let inherited = stock_owner_manifest(&mut fetcher, &RETIRED_KEY, b"inherited bytes");
        let api = Arc::new(FakeMediaNestApi::new());
        api.set_snapshot(MediaSnapshot {
            items: vec![item_naming("photos/x.bin", &inherited, &RETIRED_KEY)],
            ..Default::default()
        });
        api.set_signed_as(vec![inherited.clone()], signer);
        let machine = MediaMachine::new(
            CountingObserver::new(),
            api,
            None,
            None,
            Some(Arc::new(fetcher) as Arc<dyn fauna_core::file_download::BlobFetcher>),
            None,
        );
        if pairing == "unpaired" {
            machine.set_predecessor_backup_keys(vec![RETIRED_KEY.to_vec()]);
            machine.set_predecessor_actor_ids(vec![FAKE_PREDECESSOR_ID.to_vec()]);
        } else {
            machine.set_predecessor_chain(
                vec![FAKE_PREDECESSOR_ID.to_vec()],
                vec![RETIRED_KEY.to_vec()],
            );
        }
        machine.refresh(Some(KEY.to_vec())).await;
        machine
            .download_file(
                inherited.clone(),
                None,
                "photos".into(),
                "photos/x.bin".into(),
                KEY.to_vec(),
            )
            .await
            .expect_err(pairing);
    }
}

fn version_naming(manifest_hex: &str) -> fauna_media_machine::snapshots::FileVersionSummary {
    fauna_media_machine::snapshots::FileVersionSummary {
        version_num: 3,
        manifest_hash: manifest_hex.into(),
        size_bytes: 42,
        created_at: 1_000,
        content_key_version: None,
        author_display: "alice".into(),
        pruned: false,
        purge_after: None,
    }
}

/// The restore seams: the fake nest, the uploader the re-seal writes through,
/// and a machine keyed as a successor.
fn restore_machine(
    api: &Arc<FakeMediaNestApi>,
    uploader: &Arc<FakeMediaBlobUploader>,
    blob: Arc<FakeMediaBlobFetcher>,
    fetcher: FakeDownloadFetcher,
) -> Arc<MediaMachine> {
    let machine = MediaMachine::new(
        CountingObserver::new(),
        api.clone(),
        Some(uploader.clone() as Arc<dyn MediaBlobUploader>),
        Some(blob as Arc<dyn MediaBlobFetcher>),
        Some(Arc::new(fetcher) as Arc<dyn fauna_core::file_download::BlobFetcher>),
        None,
    );
    machine.set_owner_backup_key(KEY.to_vec());
    machine.set_predecessor_chain(
        vec![FAKE_PREDECESSOR_ID.to_vec()],
        vec![RETIRED_KEY.to_vec()],
    );
    machine
}

fn restores(api: &FakeMediaNestApi) -> Vec<(String, i64)> {
    api.calls()
        .into_iter()
        .filter_map(|c| match c {
            MediaNestCall::RestoreMember {
                manifest_hash,
                size_bytes,
                ..
            } => Some((manifest_hash, size_bytes)),
            _ => None,
        })
        .collect()
}

/// What the re-seal uploaded, as a fetcher — so the test reads the restored
/// head exactly as a later reader would.
fn uploaded(uploader: &FakeMediaBlobUploader) -> FakeDownloadFetcher {
    use fauna_core::data::ContentHash;
    let mut fetcher = FakeDownloadFetcher::default();
    for (key, body) in uploader.chunks() {
        fetcher.put(&ContentHash::from_digest_raw(key), body);
    }
    for (key, body) in uploader.manifests() {
        fetcher.put(&ContentHash::from_digest_raw(key), body);
    }
    fetcher
}

/// Ruling (8)(d), the restore sentence — **the attack**: a version a
/// predecessor's signature names whose bytes the successor sealed under its
/// CURRENT root (as a chunk manifest, and as a blob primary) is not
/// restorable. Nothing is recorded and nothing uploaded: a bare re-sign would
/// have made it a head the current root opens.
#[tokio::test]
async fn a_predecessor_signed_version_sealed_under_the_current_root_is_not_restorable() {
    // Chunk-manifest arm.
    let mut fetcher = FakeDownloadFetcher::default();
    let planted = stock_owner_manifest(&mut fetcher, &KEY, b"created after the succession");
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_versions(vec![version_naming(&planted)]);
    api.set_predecessor_signed(vec![planted.clone()]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("unused"));
    let blob = Arc::new(FakeMediaBlobFetcher::new(Vec::new()));
    blob.fail(MediaApiError::NotFound {
        detail: "not a primary".into(),
    });
    let machine = restore_machine(&api, &uploader, blob, fetcher);
    let versions = machine
        .file_versions("photos".into(), "photos/a.bin".into(), false)
        .await
        .expect("listed");
    machine
        .restore_version(
            "photos".into(),
            "dev-1".into(),
            "photos/a.bin".into(),
            versions[0].clone(),
        )
        .await;
    assert!(restores(&api).is_empty(), "nothing recorded");
    assert!(uploader.manifests().is_empty(), "nothing re-sealed");
    let error = machine.snapshot().error.expect("the refusal is surfaced");
    assert!(
        error
            .args
            .values()
            .any(|v| v.contains(fauna_media_machine::machine::RESTORE_INHERITED_UNOPENABLE)),
        "says why: {error:?}"
    );

    // Blob-primary arm.
    let primary = sealed(b"a Media-page upload after the succession", &KEY);
    let planted = content_hash_hex(&primary);
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_versions(vec![version_naming(&planted)]);
    api.set_predecessor_signed(vec![planted.clone()]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("unused"));
    let machine = restore_machine(
        &api,
        &uploader,
        Arc::new(FakeMediaBlobFetcher::new(primary)),
        FakeDownloadFetcher::default(),
    );
    let versions = machine
        .file_versions("photos".into(), "photos/a.bin".into(), false)
        .await
        .expect("listed");
    machine
        .restore_version(
            "photos".into(),
            "dev-1".into(),
            "photos/a.bin".into(),
            versions[0].clone(),
        )
        .await;
    assert!(restores(&api).is_empty(), "nothing recorded");
    assert!(uploader.manifests().is_empty(), "nothing re-sealed");
}

/// …and the same version sealed under the PREDECESSOR's root restores — as a
/// re-seal: the head the restore records is a new manifest that opens under
/// the current root alone, *because it was re-sealed*, with the original bytes.
#[tokio::test]
async fn a_predecessor_signed_version_restores_by_re_sealing_under_the_current_root() {
    let current_only =
        fauna_core::file_download::FileDownloadKeys::owner(BackupKey::from_bytes(KEY));

    // Chunk-manifest arm.
    let plaintext = b"the inherited file, as the predecessor sealed it".to_vec();
    let mut fetcher = FakeDownloadFetcher::default();
    let inherited = stock_owner_manifest(&mut fetcher, &RETIRED_KEY, &plaintext);
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_versions(vec![version_naming(&inherited)]);
    api.set_predecessor_signed(vec![inherited.clone()]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("unused"));
    let blob = Arc::new(FakeMediaBlobFetcher::new(Vec::new()));
    blob.fail(MediaApiError::NotFound {
        detail: "not a primary".into(),
    });
    let machine = restore_machine(&api, &uploader, blob, fetcher.clone());
    let versions = machine
        .file_versions("photos".into(), "photos/a.bin".into(), false)
        .await
        .expect("listed");
    machine
        .restore_version(
            "photos".into(),
            "dev-1".into(),
            "photos/a.bin".into(),
            versions[0].clone(),
        )
        .await;
    assert!(
        machine.snapshot().error.is_none(),
        "{:?}",
        machine.snapshot().error
    );
    let recorded = restores(&api);
    assert_eq!(recorded.len(), 1);
    assert_ne!(recorded[0].0, inherited, "a re-seal, never a bare re-sign");
    assert_eq!(recorded[0].1, plaintext.len() as i64);
    let restored = fauna_core::file_download::download_file_bytes_by_manifest(
        &uploaded(&uploader),
        &current_only,
        fauna_core::data::ContentHash::from_digest_raw(hex_decode32(&recorded[0].0)),
        None,
        "photos/a.bin",
    )
    .await
    .expect("the restored head opens under the current root alone");
    assert_eq!(restored, plaintext);
    // …which the predecessor's own manifest never did.
    fauna_core::file_download::download_file_bytes_by_manifest(
        &fetcher,
        &current_only,
        fauna_core::data::ContentHash::from_digest_raw(hex_decode32(&inherited)),
        None,
        "photos/a.bin",
    )
    .await
    .expect_err("the inherited manifest rests under the retired root");

    // Blob-primary arm.
    let plaintext = b"an inherited Media-page upload".to_vec();
    let primary = sealed(&plaintext, &RETIRED_KEY);
    let inherited = content_hash_hex(&primary);
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_versions(vec![version_naming(&inherited)]);
    api.set_predecessor_signed(vec![inherited.clone()]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("unused"));
    let machine = restore_machine(
        &api,
        &uploader,
        Arc::new(FakeMediaBlobFetcher::new(primary)),
        FakeDownloadFetcher::default(),
    );
    let versions = machine
        .file_versions("photos".into(), "photos/a.bin".into(), false)
        .await
        .expect("listed");
    machine
        .restore_version(
            "photos".into(),
            "dev-1".into(),
            "photos/a.bin".into(),
            versions[0].clone(),
        )
        .await;
    let recorded = restores(&api);
    assert_eq!(recorded.len(), 1);
    assert_ne!(recorded[0].0, inherited);
    let restored = fauna_core::file_download::download_file_bytes_by_manifest(
        &uploaded(&uploader),
        &current_only,
        fauna_core::data::ContentHash::from_digest_raw(hex_decode32(&recorded[0].0)),
        None,
        "photos/a.bin",
    )
    .await
    .expect("the restored head opens under the current root alone");
    assert_eq!(restored, plaintext);
}

/// A version the CURRENT identity signed restores as it always did: the
/// historical manifest verbatim, nothing opened, nothing uploaded.
#[tokio::test]
async fn a_current_identity_version_restores_verbatim() {
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_versions(vec![version_naming("abc123")]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("unused"));
    let machine = restore_machine(
        &api,
        &uploader,
        Arc::new(FakeMediaBlobFetcher::new(Vec::new())),
        FakeDownloadFetcher::default(),
    );
    let versions = machine
        .file_versions("photos".into(), "photos/a.bin".into(), false)
        .await
        .expect("listed");
    machine
        .restore_version(
            "photos".into(),
            "dev-1".into(),
            "photos/a.bin".into(),
            versions[0].clone(),
        )
        .await;
    assert_eq!(restores(&api), [("abc123".to_string(), 42)]);
    assert!(uploader.manifests().is_empty());
}

// ── Ruling (10)(c): the stamp binds the root ────────────────────────────────
//
// `writer-signed-change-records.md` § Writer-signed change records. A record
// carrying a `content_key_version` opens under that generation's content key or
// not at all — never under an owner root, whoever signed it — so a restore that
// re-points a stamped version verbatim cannot hand it the current root.

/// **The attack, end to end.** The retired seed and a lying nest stamp a
/// version naming a manifest the successor sealed under its CURRENT root, in an
/// owner-only set. The restore door re-points it verbatim (it is stamped), and
/// the resulting head — now the current identity's — still does not open:
/// the owner root is never offered to a stamped record.
#[tokio::test]
async fn a_stamped_version_restored_verbatim_never_opens_under_the_owner_root() {
    let mut fetcher = FakeDownloadFetcher::default();
    let planted = stock_owner_manifest(&mut fetcher, &KEY, b"sealed after the succession");
    let api = Arc::new(FakeMediaNestApi::new());
    api.set_versions(vec![fauna_media_machine::snapshots::FileVersionSummary {
        content_key_version: Some(5),
        ..version_naming(&planted)
    }]);
    api.set_predecessor_signed(vec![planted.clone()]);
    let uploader = Arc::new(FakeMediaBlobUploader::new("unused"));
    let blob = Arc::new(FakeMediaBlobFetcher::new(Vec::new()));
    blob.fail(MediaApiError::NotFound {
        detail: "not a primary".into(),
    });
    let machine = restore_machine(&api, &uploader, blob, fetcher.clone());
    let versions = machine
        .file_versions("photos".into(), "photos/a.bin".into(), false)
        .await
        .expect("listed");
    machine
        .restore_version(
            "photos".into(),
            "dev-1".into(),
            "photos/a.bin".into(),
            versions[0].clone(),
        )
        .await;
    assert_eq!(
        restores(&api),
        [(planted.clone(), 42)],
        "the door re-points a stamped version verbatim — the reader is the bound"
    );

    // The restored head, as every owner-only reader now sees it: signed as
    // the current identity, stamped.
    let reader_api = Arc::new(FakeMediaNestApi::new());
    reader_api.set_snapshot(MediaSnapshot {
        items: vec![MediaItem {
            content_key_version: Some(5),
            ..item_naming("photos/a.bin", &planted, &KEY)
        }],
        ..Default::default()
    });
    let reader = MediaMachine::new(
        CountingObserver::new(),
        reader_api,
        None,
        None,
        Some(Arc::new(fetcher) as Arc<dyn fauna_core::file_download::BlobFetcher>),
        None,
    );
    reader.refresh(Some(KEY.to_vec())).await;
    reader
        .download_file(
            planted.clone(),
            Some(5),
            "photos".into(),
            "photos/a.bin".into(),
            KEY.to_vec(),
        )
        .await
        .expect_err("a stamped head never opens under the owner root");
    // The very same bytes, unstamped and the current identity's, open.
    assert_eq!(
        reader
            .download_file(
                planted,
                None,
                "photos".into(),
                "photos/a.bin".into(),
                KEY.to_vec(),
            )
            .await
            .expect("an unstamped current-identity row opens under its root"),
        b"sealed after the succession"
    );
}

/// The bare-key arms: a Media-page blob primary, and a thumbnail no unstamped
/// row names, are refused under the owner key when the row naming them is
/// stamped — a stamped record is never an owner-sealed primary (a
/// content-keyed upload records a chunk manifest and no thumbnail) — while the
/// same bytes named by an unstamped row of the current identity open.
#[tokio::test]
async fn a_stamped_item_never_opens_a_blob_or_thumbnail_under_the_owner_key() {
    for stamp in [Some(5u64), None] {
        let sealed_bytes = sealed(b"blob bytes", &KEY);
        let hash = content_hash_hex(&sealed_bytes);
        let api = Arc::new(FakeMediaNestApi::new());
        api.set_snapshot(MediaSnapshot {
            items: vec![MediaItem {
                thumbnail_hash: Some(hash.clone()),
                manifest_hash: Some(hex::decode(&hash).unwrap().into()),
                content_key_version: stamp,
                ..item("photos", "photos/a.jpg", 10, 100)
            }],
            ..Default::default()
        });
        let machine = MediaMachine::new(
            CountingObserver::new(),
            api,
            None,
            Some(Arc::new(FakeMediaBlobFetcher::new(sealed_bytes)) as Arc<dyn MediaBlobFetcher>),
            Some(Arc::new(FakeDownloadFetcher::default())
                as Arc<dyn fauna_core::file_download::BlobFetcher>),
            None,
        );
        machine.refresh(Some(KEY.to_vec())).await;

        let thumbnail = machine.fetch_thumbnail(hash.clone(), KEY.to_vec()).await;
        let primary = machine
            .download_file(
                hash,
                stamp,
                "photos".into(),
                "photos/a.jpg".into(),
                KEY.to_vec(),
            )
            .await;
        assert_eq!(thumbnail.is_ok(), stamp.is_none(), "thumbnail, {stamp:?}");
        assert_eq!(primary.is_ok(), stamp.is_none(), "primary, {stamp:?}");
    }
}

/// The machine hands the seam's reader the attested predecessor ids it is
/// given, dropping one that is not an actor id.
#[tokio::test]
async fn the_attested_predecessor_ids_reach_the_seams_reader() {
    let api = Arc::new(FakeMediaNestApi::new());
    let machine = MediaMachine::new(CountingObserver::new(), api.clone(), None, None, None, None);
    machine.set_predecessor_actor_ids(vec![vec![3u8; 32], vec![1, 2, 3]]);
    assert_eq!(api.reader_predecessors(), vec![[3u8; 32]]);
}
