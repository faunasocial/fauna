//! tier_3: a paged manifest end to end through the sync engine and a real nest
//! (`docs/goal/behavior/file-sync.md` § Paged manifests).
//!
//! The engine cuts its manifests under a small test paging rule
//! (`SyncEngine::set_paging_rule`), so a 24 MiB file — about a dozen chunks —
//! is written as a root and its pages, where production pages only past
//! 32,768 chunks. Over a real in-process nest (its page door, its root door's
//! walk, its `paged-manifests` token):
//!
//! - a writer uploads a paged file and a second engine on another of the
//!   owner's devices pulls it byte-identical — the sealed arm (owner key) and
//!   the public-folder arm (plaintext pages and root);
//! - an edit in the middle of the file re-sends only the pages it touched plus
//!   the root, and a file whose pages the nest already holds sends none (the
//!   existing existence check — what an interrupted upload resumes through);
//! - against a nest that does not advertise `paged-manifests` the paged file is
//!   refused with the typed too-large failure before any chunk is sent, and a
//!   file under the inline bound still syncs.

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::identity::ActorKeypair;
use fauna_core::manifest_paging::PagingRule;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::token_store::TokenStore;
use fauna_sync_engine::adaptive::AdaptiveConcurrency;
use fauna_sync_engine::db::SyncDb;
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::ignore::IgnoreMatcher;
use fauna_sync_engine::transfer::TransferPool;

mod common;

const OWNER_SECRET: [u8; 32] = [0x52; 32];
const WRITER_DEVICE: [u8; 32] = [0x0A; 32];
const READER_DEVICE: [u8; 32] = [0x0B; 32];
const FOLDER: &str = "paged_test";
const SET_NONCE: [u8; 32] = [0x6F; 32];
const MIB: usize = 1024 * 1024;

/// Pages a file of more than two chunks, two or three chunks a page.
fn small_rule() -> PagingRule {
    PagingRule::with_thresholds(2, 2, 3, 1)
}

/// Incompressible bytes (xorshift) from `seed`.
fn file_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = vec![0u8; len];
    for word in out.as_chunks_mut::<8>().0 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *word = state.to_le_bytes();
    }
    out
}

struct TestNest {
    url: String,
    token: String,
    state: Arc<AppState>,
}

/// A real in-process nest serving auth, discovery and sync over WS-RPC and
/// the chunk store over HTTP, the owner registered and `FOLDER` created.
async fn start_test_nest(public: bool) -> TestNest {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let base = AppState::for_test(db.clone());
    let blob_path = base.test_scratch_path("blobs");
    let backup_svc =
        Arc::new(BackupService::new(db.clone(), None, false, blob_path, None).unwrap());
    db.create_user(&owner, "free", "test").await.unwrap();
    db.create_folder(FOLDER, &owner).await.unwrap();
    db.update_folder_for_user(
        FOLDER,
        &owner,
        fauna_nest::db::FolderUpdate {
            set_nonce: Some(&SET_NONCE),
            audience: public.then_some(Some("public")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let token_store = Arc::new(TokenStore::new());
    let token = token_store
        .insert(fauna_core::identity::ActorId(owner), 3600)
        .await;
    let mut b = RpcRouter::builder();
    fauna_nest::auth_handlers::register_auth_handlers(&mut b);
    fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
    fauna_nest::sync_handlers::register_sync_handlers(&mut b);
    fauna_nest::files_handlers::register_files_handlers(&mut b);
    fauna_nest::folder_handlers::register_folders_handlers(&mut b);
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        rpc_router: Arc::new(b.build()),
        auth: fauna_nest::state::AuthState {
            token_store,
            ..Default::default()
        },
        ..base
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    TestNest {
        url: format!("http://{addr}"),
        token,
        state,
    }
}

/// An owner engine on `device`, cutting manifests under [`small_rule`],
/// signing and reading under the set's nonce; `public` arms the plaintext
/// public-folder write path. Connected before it is returned.
async fn engine_on(
    nest: &TestNest,
    watch: std::path::PathBuf,
    device: [u8; 32],
    public: bool,
) -> SyncEngine {
    let (engine_client, nest_client) =
        common::sync_engine_auth_client(&nest.url, &nest.token, OWNER_SECRET, &device);
    let engine = SyncEngine::new(
        watch,
        SyncDb::open_in_memory().unwrap(),
        engine_client,
        Some(FOLDER.to_string()),
        device,
        None,
        None,
        Some(BackupKey::derive(&OWNER_SECRET).into()),
        None,
        None,
        fauna_core::format::ConflictPolicy::Auto,
        fauna_core::format::FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        Arc::clone(&nest_client),
        fauna_sync_engine::config::SyncMode::Sync,
    )
    .with_metadata_only_residency(false)
    .with_public_audience(public);
    engine.set_change_signer(
        Some(Arc::new(
            fauna_protocol::sync_writer_sig::ChangeSigner::direct(&ActorKeypair::from_secret(
                OWNER_SECRET,
            )),
        )),
        Some(SET_NONCE),
    );
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(ActorKeypair::from_secret(OWNER_SECRET).actor_id().0),
        ..Default::default()
    });
    engine.set_paging_rule(small_rule());
    nest_client.connect().await.expect("engine connects");
    engine
}

/// Write, upload, and assert the record landed.
async fn put(engine: &SyncEngine, watch: &std::path::Path, rel: &str, bytes: &[u8]) {
    std::fs::write(watch.join(rel), bytes).unwrap();
    let outcome = engine.upload_file(rel).await.expect("upload_file");
    assert!(outcome.recorded, "the nest records {rel}");
}

async fn round_trip(public: bool) {
    round_trip_with(public, small_rule()).await;
}

async fn round_trip_with(public: bool, rule: PagingRule) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();
    let nest = start_test_nest(public).await;
    let wdir = tempfile::tempdir().unwrap();
    let writer = engine_on(&nest, wdir.path().to_path_buf(), WRITER_DEVICE, public).await;
    writer.set_paging_rule(rule);
    let bytes = file_bytes(24 * MIB, 7);
    put(&writer, wdir.path(), "big.bin", &bytes).await;
    let paged = rule != PagingRule::STANDARD;
    let first = writer.manifest_pages_sent();
    assert!(
        !paged || first >= 2,
        "the file went up paged ({first} pages)"
    );

    let rdir = tempfile::tempdir().unwrap();
    let reader = engine_on(&nest, rdir.path().to_path_buf(), READER_DEVICE, public).await;
    reader.pull_remote_changes().await.expect("pull");
    assert!(
        std::fs::read(rdir.path().join("big.bin")).unwrap() == bytes,
        "the reader pulls the paged file byte-identical"
    );

    // An edit in the middle re-sends only the pages it touched, plus the root.
    // The edited bytes go up under a new name: pages are content-addressed,
    // so what the nest already holds is what counts, and a fresh path keeps
    // this proof free of how an edit over an existing path is folded.
    let mut edited = bytes.clone();
    let mid = edited.len() / 2;
    edited[mid..mid + 64].copy_from_slice(&[0xEE; 64]);
    put(&writer, wdir.path(), "edited.bin", &edited).await;
    let resent = writer.manifest_pages_sent() - first;
    assert!(
        !paged || (1..first).contains(&resent),
        "a middle edit re-sends some pages but not all ({resent} of {first})"
    );
    reader.pull_remote_changes().await.expect("pull the edit");
    assert!(
        std::fs::read(rdir.path().join("edited.bin")).unwrap() == edited,
        "the reader pulls the edited paged file byte-identical"
    );

    // Every page already at rest: a second writer of the same bytes sends none
    // — the existence check an interrupted upload resumes through.
    let sdir = tempfile::tempdir().unwrap();
    let second = engine_on(&nest, sdir.path().to_path_buf(), [0x0C; 32], public).await;
    put(&second, sdir.path(), "copy.bin", &edited).await;
    assert_eq!(
        second.manifest_pages_sent(),
        0,
        "pages the nest holds are skipped"
    );
}

#[tokio::test]
async fn a_paged_sealed_file_round_trips_between_two_engines() {
    round_trip(false).await;
}

#[tokio::test]
async fn a_paged_public_folder_file_round_trips_between_two_engines() {
    round_trip(true).await;
}

#[tokio::test]
async fn a_nest_without_paged_manifests_refuses_the_large_file_before_any_chunk() {
    let nest = start_test_nest(false).await;
    nest.state
        .capabilities_withheld
        .lock()
        .unwrap()
        .insert(fauna_protocol::discovery::capability::PAGED_MANIFESTS.to_string());
    // The report door binds a report's `device_id` to the caller's own
    // devices, so the writer is a registered device of the owner.
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    nest.state
        .db
        .register_sync_device(&owner, &WRITER_DEVICE, "writer", None, "read,write")
        .await
        .unwrap();
    let wdir = tempfile::tempdir().unwrap();
    let writer = engine_on(&nest, wdir.path().to_path_buf(), WRITER_DEVICE, false).await;

    let bytes = file_bytes(24 * MIB, 11);
    std::fs::write(wdir.path().join("big.bin"), &bytes).unwrap();
    let err = writer
        .upload_file("big.bin")
        .await
        .expect_err("a paged file to a nest without the token is refused");
    assert!(
        fauna_sync_engine::engine::is_file_too_large_for_nest(&err),
        "the refusal is the typed too-large failure: {err:#}"
    );
    assert_eq!(writer.manifest_pages_sent(), 0);

    // Not one of its chunks reached the nest.
    let sealed = fauna_core::blob_seal::seal_blob(
        &bytes,
        Some((
            BackupKey::derive(&OWNER_SECRET).convergent_chunk_root(),
            None,
        )),
    )
    .unwrap();
    let keys: Vec<_> = sealed.chunks.iter().map(|(k, _)| *k).collect();
    let held = nest
        .state
        .backup_service
        .as_ref()
        .unwrap()
        .local_blob_store()
        .exists_batch(&keys)
        .await
        .unwrap();
    assert!(
        held.iter().all(|h| !h),
        "no chunk of the refused file is on the nest"
    );

    // The refusal reaches the person (`conflicts.md` § Refused uploads reach
    // the review list): one unresolved, candidate-free `too_large_for_nest`
    // row on the nest's review list, naming the refusing device.
    let open_refusals = || async {
        nest.state
            .db
            .list_conflicts_for_actor(&owner, false)
            .await
            .unwrap()
            .into_iter()
            .filter(|c| c.conflict_type == "too_large_for_nest")
            .collect::<Vec<_>>()
    };
    let listed = open_refusals().await;
    assert_eq!(
        listed.len(),
        1,
        "the refusal is on the review list: {listed:?}"
    );
    assert_eq!(listed[0].device_id, WRITER_DEVICE.to_vec());
    assert_eq!(
        listed[0].winning_manifest_hash, None,
        "candidate-free, unresolved"
    );

    // The unchanged file meeting the same nest is refused again from its
    // recorded refusal — not cut again — and files nothing new.
    let again = writer
        .upload_file("big.bin")
        .await
        .expect_err("the unchanged file is refused again");
    assert!(fauna_sync_engine::engine::is_file_too_large_for_nest(
        &again
    ));
    assert!(
        format!("{again:#}").contains("unchanged since it was refused"),
        "the repeat comes from the recorded refusal: {again:#}"
    );
    assert_eq!(open_refusals().await.len(), 1, "a repeat files nothing new");

    // A file under the inline bound still syncs.
    put(&writer, wdir.path(), "small.txt", b"two chunks at most").await;

    // Deleting the refused file cures it: the review row resolves.
    std::fs::remove_file(wdir.path().join("big.bin")).unwrap();
    let _ = writer.handle_delete("big.bin").await;
    assert!(
        open_refusals().await.is_empty(),
        "a deleted refused file leaves the review list"
    );
}

/// The unresolved review rows of `conflict_type` on the nest's list.
async fn open_rows_of(nest: &TestNest, conflict_type: &str) -> Vec<String> {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    nest.state
        .db
        .list_conflicts_for_actor(&owner, false)
        .await
        .unwrap()
        .into_iter()
        .filter(|c| c.conflict_type == conflict_type)
        .map(|c| c.conflict_type)
        .collect()
}

/// A writer on a nest that withholds `paged-manifests`, its big file refused.
async fn refused_writer(nest: &TestNest, wdir: &std::path::Path) -> SyncEngine {
    nest.state
        .capabilities_withheld
        .lock()
        .unwrap()
        .insert(fauna_protocol::discovery::capability::PAGED_MANIFESTS.to_string());
    register(nest, WRITER_DEVICE).await;
    let writer = engine_on(nest, wdir.to_path_buf(), WRITER_DEVICE, false).await;
    std::fs::write(wdir.join("big.bin"), file_bytes(24 * MIB, 11)).unwrap();
    // The scan takes the file up as a new local file; the pending pass meets
    // the refusal.
    writer.reconcile().await.unwrap();
    let _ = writer.upload_pending(1).await;
    assert_eq!(
        open_rows_of(nest, "too_large_for_nest").await.len(),
        1,
        "the refusal is on the review list"
    );
    writer
}

/// A refused file deleted while the engine is not running is cured at the
/// next reconcile (`conflicts.md` § Refused uploads reach the review list,
/// *Lifecycle*: cured "when the person deletes it"): no watcher reports the
/// delete, so the reconcile pass is the only door that can see it. The path
/// never reached the nest, so nothing is recorded for it and its pending row
/// is settled, not retried for ever.
#[tokio::test]
async fn a_refused_file_deleted_while_the_engine_is_down_is_cured_at_reconcile() {
    let nest = start_test_nest(false).await;
    let wdir = tempfile::tempdir().unwrap();
    let writer = refused_writer(&nest, wdir.path()).await;

    // Removed with no watcher running.
    std::fs::remove_file(wdir.path().join("big.bin")).unwrap();
    writer.reconcile().await.unwrap();

    assert!(
        open_rows_of(&nest, "too_large_for_nest").await.is_empty(),
        "the nest row gets the candidate-free resolve"
    );
    assert!(
        writer
            .db()
            .list_unresolved_conflicts()
            .unwrap()
            .iter()
            .all(|c| c.1 != "big.bin"),
        "the local refusal row is resolved"
    );
    assert!(
        writer
            .db()
            .get_entry("big.bin")
            .unwrap()
            .is_none_or(|e| e.state != fauna_sync_engine::db::SyncState::LocallyModified),
        "the pending row of a path the nest never held is settled"
    );
}

/// A change from another device over a refused file leaves ONE entry on the
/// review list — the refusal — not a second, candidate-free `concurrent_edit`
/// that nothing can clear; and the refused local file is kept (it exists
/// nowhere else: the nest cannot store it). Once the person removes the
/// refused file, the other device's version — the set's head, kept from
/// applying only by that file — reaches this device. `conflicts.md`
/// § Refused uploads reach the review list, *Lifecycle*.
#[tokio::test]
async fn a_change_from_another_device_over_a_refused_file_leaves_one_entry() {
    change_over_refused_file(Removal::AtReconcile).await;
}

/// The same, with the removal seen live (the watcher's delete event) rather
/// than by a reconcile pass: nothing is recorded on the nest for a path it
/// never held from this device — a delete there would erase the other
/// device's version everywhere — and that version reaches this device.
#[tokio::test]
async fn a_refused_file_removed_live_receives_the_other_devices_version() {
    change_over_refused_file(Removal::Live).await;
}

#[derive(Clone, Copy)]
enum Removal {
    AtReconcile,
    Live,
}

async fn change_over_refused_file(removal: Removal) {
    let nest = start_test_nest(false).await;
    let wdir = tempfile::tempdir().unwrap();
    let writer = refused_writer(&nest, wdir.path()).await;

    register(&nest, READER_DEVICE).await;
    let rdir = tempfile::tempdir().unwrap();
    let other = engine_on(&nest, rdir.path().to_path_buf(), READER_DEVICE, false).await;
    put(
        &other,
        rdir.path(),
        "big.bin",
        b"a small file from the other device",
    )
    .await;

    writer.pull_remote_changes().await.expect("pull");

    assert_eq!(
        std::fs::read(wdir.path().join("big.bin")).unwrap(),
        file_bytes(24 * MIB, 11),
        "the refused local file is kept"
    );
    assert!(
        open_rows_of(&nest, "concurrent_edit").await.is_empty(),
        "no second, unclearable entry"
    );
    assert_eq!(
        open_rows_of(&nest, "too_large_for_nest").await.len(),
        1,
        "the refusal stays the one entry"
    );
    let local: Vec<_> = writer
        .db()
        .list_unresolved_conflicts()
        .unwrap()
        .into_iter()
        .filter(|c| c.1 == "big.bin")
        .map(|c| c.2)
        .collect();
    assert_eq!(local, vec!["too_large_for_nest".to_string()]);

    // Keeping the file out of the folder cures it.
    std::fs::remove_file(wdir.path().join("big.bin")).unwrap();
    match removal {
        Removal::AtReconcile => {
            writer.reconcile().await.unwrap();
        }
        Removal::Live => {
            writer.handle_delete("big.bin").await.unwrap();
        }
    }
    assert!(
        open_rows_of(&nest, "too_large_for_nest").await.is_empty(),
        "deleting the refused file clears the review list"
    );

    // The other device's version, kept from applying only by the refused
    // file, is owed to this device now that the file is gone.
    writer
        .pull_remote_changes()
        .await
        .expect("pull after removal");
    assert_eq!(
        std::fs::read(wdir.path().join("big.bin")).ok().as_deref(),
        Some(&b"a small file from the other device"[..]),
        "the writer receives the set's head once its refused file is gone"
    );
    // And the other device still holds it: nothing erased it on the nest.
    other.pull_remote_changes().await.expect("other pulls");
    assert_eq!(
        std::fs::read(rdir.path().join("big.bin")).ok().as_deref(),
        Some(&b"a small file from the other device"[..]),
        "the other device's version is not erased"
    );
}

/// Register `device` for the owner, as every engine host does on start
/// (`fauna.sync.register`, re-registered under the app default each start).
async fn register(nest: &TestNest, device: [u8; 32]) {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    nest.state
        .db
        .register_sync_device(&owner, &device, "laptop", None, "read,write")
        .await
        .unwrap();
}

/// An edit written over an existing path reaches a device that joins after it
/// and a reader that holds the first version (`docs/goal/behavior/file-sync.md`
/// § 5 Offline Catch-Up: a catching-up device applies each path's latest
/// change). Unpaged, so no paging effect. `writer_pulls_echo` is the
/// production shape: the nest nudges every seat of the set on each record and
/// the writer's watch loop pulls its own echo, so its next edit is stamped
/// `derived_through` at or past its first. Without that pull (a second save
/// inside the echo window) the edit's stamp reaches across the writer's own
/// acknowledged run — the record reply's `prev_set_seq` lets the writer know
/// no foreign row lies between — so it is still stamped at or past its first
/// row (`docs/goal/behavior/conflicts.md` § the causal watermark, *An edit
/// stamp reaches across the writer's own acknowledged run*).
async fn edit_over_existing_path(writer_pulls_echo: bool) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();
    let nest = start_test_nest(false).await;
    let fresh_device = [0x0D; 32];
    for d in [WRITER_DEVICE, READER_DEVICE, fresh_device] {
        register(&nest, d).await;
    }
    let wdir = tempfile::tempdir().unwrap();
    let writer = engine_on(&nest, wdir.path().to_path_buf(), WRITER_DEVICE, false).await;
    writer.set_paging_rule(PagingRule::STANDARD);
    let bytes = file_bytes(24 * MIB, 7);
    put(&writer, wdir.path(), "big.bin", &bytes).await;
    if writer_pulls_echo {
        writer
            .pull_remote_changes()
            .await
            .expect("writer pulls its echo");
    }

    let rdir = tempfile::tempdir().unwrap();
    let reader = engine_on(&nest, rdir.path().to_path_buf(), READER_DEVICE, false).await;
    reader.pull_remote_changes().await.expect("pull");
    assert!(std::fs::read(rdir.path().join("big.bin")).unwrap() == bytes);

    let mut edited = bytes.clone();
    let mid = edited.len() / 2;
    edited[mid..mid + 64].copy_from_slice(&[0xEE; 64]);
    put(&writer, wdir.path(), "big.bin", &edited).await;

    let cdir = tempfile::tempdir().unwrap();
    let fresh = engine_on(&nest, cdir.path().to_path_buf(), fresh_device, false).await;
    fresh.pull_remote_changes().await.expect("fresh pull");
    assert!(
        std::fs::read(cdir.path().join("big.bin")).unwrap() == edited,
        "a device joining after the edit receives the edit"
    );

    reader.pull_remote_changes().await.expect("pull the edit");
    assert!(
        std::fs::read(rdir.path().join("big.bin")).unwrap() == edited,
        "the reader that held the first version takes the edit"
    );
}

#[tokio::test]
async fn an_edit_over_an_existing_path_reaches_a_device_that_joins_after_it() {
    edit_over_existing_path(true).await;
}

#[tokio::test]
async fn a_second_save_inside_the_writers_echo_window_reaches_a_device_that_joins_after_it() {
    edit_over_existing_path(false).await;
}
