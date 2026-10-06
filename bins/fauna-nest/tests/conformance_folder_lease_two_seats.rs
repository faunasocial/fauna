//! **Two real seats through one folder's exclusive-edit lease** (tier_3) —
//! hold → refusal → release → takeover, driven by two production
//! [`SyncEngine`]s against a real in-process nest.
//!
//! `docs/goal/behavior/file-sync.md` § Exclusive editing owns the mechanism;
//! § Folders owns the sentence it keeps (*"while a lease is held, other devices
//! treat the folder as read-only"*). This is that sentence's witness on the
//! **client** side. The nest side already had one —
//! `tests/e2e-unified/tests/api/test_folder_exclusive_lease.py` pins the kinds'
//! own refusal semantics, and `conformance_folder_exclusive_editing.rs` pins the
//! property and the `FolderSummary.lease` projection — but both stop at the
//! wire. Nothing anywhere asserted that a seat *uses* any of it.
//!
//! **What only this test catches.** Every link between the projection and the
//! bytes on disk, composed: that a seat installs the governance flag and the
//! holder reading off its ordinary folder-list refresh; that a governed pass
//! takes the lease before writing; that the *second* seat is refused and
//! therefore uploads **nothing**; that its local edit is still on disk and still
//! `LocallyModified` afterwards — deferred, never dropped, which is the arm that
//! would silently cost a user their work; and that the moment the holder
//! releases, the deferred seat takes the folder over and the edit lands. A test
//! of any one link would pass against an engine that never called the next one.
//!
//! **No wall-clock anywhere** (e2e convention 14). Every step is driven — a
//! refresh, a window, a pass — and asserted on state, never on elapsed time. The
//! lease's 300 s TTL is deliberately never waited out: the takeover here is by
//! *release*, and the expiry arm is pinned in-process by
//! `conformance_folder_exclusive_editing.rs`.
//!
//! Tier: tier_3 (real nest binary surface, real wire, real seal + chunk upload;
//! the two seats are two real engines in one process, which is what makes the
//! interleaving deterministic).

mod common;

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::format::{ConflictPolicy, FormatRegistry};
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::folders::FolderUpdateRequest;
use fauna_sync_engine::adaptive::AdaptiveConcurrency;
use fauna_sync_engine::db::{SyncDb, SyncState};
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::folder_lease::LeaseWindow;
use fauna_sync_engine::ignore::IgnoreMatcher;
use fauna_sync_engine::transfer::TransferPool;

/// The owner's Ed25519 secret — both seats are devices of the SAME user, which
/// is the shape exclusive editing exists for (a SQLite database or a video
/// project synced across one person's laptop and desktop).
const OWNER_SECRET: [u8; 32] = [0x51; 32];
/// Seat A — the device that gets the lease first.
const DEVICE_A: [u8; 32] = [0x0A; 32];
/// Seat B — the device that is refused, keeps its work, and takes over.
const DEVICE_B: [u8; 32] = [0x0B; 32];
const FOLDER: &str = "vault";

/// Start a real in-process nest serving the auth + sync + folder WS-RPC kinds
/// and the chunk-store HTTP routes, with the owner registered and `FOLDER`
/// pre-created. Mirrors `conformance_file_provider_client.rs::start_test_nest`.
async fn start_test_nest() -> (String, String) {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlive the test; never deleted under test
    let backup_svc =
        Arc::new(BackupService::new(db.clone(), None, false, blob_path, None).unwrap());

    db.create_user(&owner, "free", "test").await.unwrap();
    db.create_folder(FOLDER, &owner).await.unwrap();
    // The set's stored nonce — what a signed record's statement is verified
    // under (the owner's create helper sends it; this fixture creates the row
    // directly, so it stores it the way the owner's update would).
    db.update_folder_for_user(
        FOLDER,
        &owner,
        fauna_nest::db::FolderUpdate {
            set_nonce: Some(&common::SET_NONCE),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let token_store = Arc::new(TokenStore::new());
    let owner_bearer = token_store
        .insert(ActorKeypair::from_secret(OWNER_SECRET).actor_id(), 3600)
        .await;

    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            fauna_nest::folder_handlers::register_folders_handlers(&mut b);
            b.build()
        }),
        auth: fauna_nest::state::AuthState {
            token_store,
            ..Default::default()
        },
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), owner_bearer)
}

/// One seat: a production owner-`BackupKey` engine on its own watch dir and its
/// own `SyncDb`, with its control plane genuinely connected.
async fn seat(
    url: &str,
    bearer: &str,
    device_id: [u8; 32],
) -> (SyncEngine, Arc<fauna_client::NestClient>, tempfile::TempDir) {
    let watch = tempfile::tempdir().unwrap();
    let (engine_client, nest_client) =
        fauna_nest::test_support::sync_engine_auth_client(url, bearer, OWNER_SECRET, &device_id);
    let ignore = IgnoreMatcher::load(watch.path()).unwrap_or_default();
    let engine = SyncEngine::new(
        watch.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
        engine_client,
        Some(FOLDER.to_string()),
        device_id,
        None, // mls
        None, // epoch_secret
        Some(BackupKey::derive(&OWNER_SECRET).into()),
        None, // mls_group_id
        None, // content_keys
        ConflictPolicy::Auto,
        FormatRegistry::new(),
        ignore,
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        Arc::clone(&nest_client),
        fauna_sync_engine::config::SyncMode::Sync,
    );
    // Writer-signed records: the nest refuses an unsigned change record, so the
    // seat signs as the owner under the set's stored nonce.
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    engine.set_change_signer(
        Some(common::direct_signer(&owner_kp)),
        Some(common::SET_NONCE),
    );
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(common::SET_NONCE),
        owner: Some(owner_kp.actor_id().0),
        ..Default::default()
    });
    // Force the WS auth handshake up front rather than relying on the lazy
    // connect: without it the first RPC can time out inside its own deadline and
    // read identically to a genuine refusal — and this test's whole subject is
    // telling a refusal apart from an unreachable nest.
    nest_client
        .connect()
        .await
        .expect("seat's nest_client must reach Connected (WS auth handshake)");
    // Bind by ref, as the resident agent does: the engine's per-tick row read
    // (which carries the governance flag and the lease holder) resolves the
    // set's row by its `FolderRef` alone, and an engine holding none reads no row.
    let row = fauna_client_folders::FoldersClient::new(Arc::clone(&nest_client))
        .list_owned_and_shared()
        .await
        .expect("list the owner's folders")
        .folders
        .into_iter()
        .find(|fs| fs.name == FOLDER)
        .expect("the fixture's one folder is listed");
    let engine = engine.with_binding_edge(fauna_sync_engine::binding_edge::BindingEdge {
        folder_ref: fauna_core::folder_keys::FolderRef::Local(row.id),
        basis: fauna_sync_engine::binding_edge::BindingBasis::of(&row),
        on_rebuild: Arc::new(|| {}),
    });
    (engine, nest_client, watch)
}

/// Stage a local edit on a seat and get it into the pending set the upload pass
/// reads, exactly as the watcher's reconcile backstop would.
async fn stage_local_edit(engine: &SyncEngine, watch: &tempfile::TempDir, rel: &str, body: &[u8]) {
    std::fs::write(watch.path().join(rel), body).unwrap();
    engine.reconcile().await.expect("reconcile stages the edit");
    assert!(
        engine
            .db()
            .list_by_state(SyncState::LocallyModified)
            .unwrap()
            .iter()
            .any(|e| e.path == rel),
        "the staged edit must be pending before the pass runs, or the test proves nothing"
    );
}

fn pending_paths(engine: &SyncEngine) -> Vec<String> {
    let mut v: Vec<String> = engine
        .db()
        .list_by_state(SyncState::LocallyModified)
        .unwrap()
        .into_iter()
        .map(|e| e.path)
        .collect();
    v.sort();
    v
}

/// The whole promise, as one interleaving: A holds, B is refused and keeps its
/// work, A releases, B takes over and its work lands.
#[tokio::test]
async fn two_seats_hold_refuse_release_and_take_over_one_governed_folder() {
    let (url, bearer) = start_test_nest().await;
    let (engine_a, nest_a, watch_a) = seat(&url, &bearer, DEVICE_A).await;
    let (engine_b, _nest_b, watch_b) = seat(&url, &bearer, DEVICE_B).await;

    // ── The owner turns exclusive editing on, through the production kind ──
    fauna_client_folders::FoldersClient::new(Arc::clone(&nest_a))
        .update(FolderUpdateRequest {
            name: FOLDER.into(),
            exclusive_editing: Some(true),
            ..Default::default()
        })
        .await
        .expect("the owner may put their own folder under exclusive editing");

    // Both seats learn it the only sanctioned way — off the folder-list read
    // their ordinary posture refresh already performs. Neither asks the acquire
    // kind whether the folder is free: asking would TAKE it.
    engine_a.refresh_sync_mode().await;
    engine_b.refresh_sync_mode().await;
    assert!(
        engine_a.lease_posture().is_governed() && engine_b.lease_posture().is_governed(),
        "both seats must install the governance flag off their own list read"
    );
    assert!(
        !engine_a.is_read_only_for_lease() && !engine_b.is_read_only_for_lease(),
        "a governed folder nobody holds is writable by either seat"
    );

    // ── A runs a whole pass: it takes the lease, writes, and gives it back ──
    //
    // The release at the end is the pass's own doing (`upload_pending` closes
    // the window it opened), which is why the assertion below it can be about
    // the *folder being free again* — a pass that kept the lease after draining
    // would lock the folder against its own user's other device for a TTL.
    const A_REL: &str = "ledger.sqlite";
    stage_local_edit(&engine_a, &watch_a, A_REL, b"seat A's authoritative rows\n").await;
    let (recorded_a, _) = engine_a.upload_pending(4).await.expect("A's pass runs");
    assert_eq!(
        recorded_a,
        vec![A_REL.to_string()],
        "the holder writes normally — a lease governs who writes, not whether writing works"
    );
    engine_b.refresh_sync_mode().await;
    assert!(
        !engine_b.is_read_only_for_lease(),
        "a drained pass gives the folder back, so the other seat is writable again"
    );

    // ── Now A is mid-pass: it holds the folder and has not drained yet ──
    assert_eq!(
        engine_a.open_lease_window().await,
        LeaseWindow::Held,
        "the first seat to ask for a free folder gets it"
    );

    // ── B is refused, and that is the whole sentence § Folders promises ──
    const B_REL: &str = "notes.bin";
    const B_BODY: &[u8] = b"seat B's edit, made while A held the folder\n";
    stage_local_edit(&engine_b, &watch_b, B_REL, B_BODY).await;

    assert_eq!(
        engine_b.open_lease_window().await,
        LeaseWindow::Refused,
        "a second device must be refused, not admitted alongside the holder"
    );
    // The projection — not the refusal — is where the holder's identity lives.
    engine_b.refresh_sync_mode().await;
    let holder = engine_b
        .lease_posture()
        .holder()
        .expect("the projection must name the holder on the owner arm");
    assert_eq!(
        holder.device_id,
        hex::encode(DEVICE_A),
        "the holder reading must name seat A's device, off the projection"
    );
    assert!(
        engine_b.is_read_only_for_lease(),
        "a seat a live lease is held against renders read-only"
    );
    assert!(
        !engine_a.is_read_only_for_lease(),
        "the holder itself is never read-only — it is the one device that may write"
    );

    // ⛔ The arm that would silently cost a user their work. B uploads NOTHING,
    // and its edit is untouched on disk and still pending.
    let (recorded_b, bytes_b) = engine_b
        .upload_pending(4)
        .await
        .expect("a refused pass is a deferral, not a failure");
    assert!(
        recorded_b.is_empty() && bytes_b == 0,
        "a refused pass must upload nothing at all, got {recorded_b:?} / {bytes_b} bytes"
    );
    assert_eq!(
        std::fs::read(watch_b.path().join(B_REL)).unwrap(),
        B_BODY,
        "the local edit must be byte-for-byte untouched — a lease may make a write wait, \
         never make it vanish"
    );
    assert_eq!(
        pending_paths(&engine_b),
        vec![B_REL.to_string()],
        "the deferred edit must still be pending, which is what makes the next pass re-drive it"
    );

    // ── A releases; B takes the folder over and its deferred work lands ──
    engine_a.close_lease_window().await;
    engine_b.refresh_sync_mode().await;
    assert!(
        engine_b.lease_posture().holder().is_none(),
        "a released lease must project as unheld, or no seat would ever write again"
    );
    assert!(
        !engine_b.is_read_only_for_lease(),
        "the refusal must not outlive the lease it was about"
    );

    assert_eq!(
        engine_b.open_lease_window().await,
        LeaseWindow::Held,
        "the freed folder must be takeable by the seat that was waiting for it"
    );
    let (recorded_b2, _) = engine_b
        .upload_pending(4)
        .await
        .expect("B's pass runs once it holds the folder");
    assert_eq!(
        recorded_b2,
        vec![B_REL.to_string()],
        "the edit deferred while A held the folder must land on the first pass after the \
         takeover — deferred, never dropped"
    );
    assert!(
        pending_paths(&engine_b).is_empty(),
        "nothing may be left pending once the deferred edit has landed"
    );
}

/// The per-caller choke point: a host that reaches past the passes and calls
/// `upload_file` directly on a folder another device holds is refused too.
///
/// `upload_file` is public and real hosts call it (the cfapi hydration host, the
/// apple in-process host), so a guard that lived only in the two upload passes
/// would be a guard with a door beside it. The refusal is a **local** read — it
/// never acquires, because acquiring per file is the one shape § Exclusive
/// editing's *Never* list forbids.
#[tokio::test]
async fn a_direct_upload_on_a_folder_another_device_holds_is_refused_and_stays_pending() {
    let (url, bearer) = start_test_nest().await;
    let (engine_a, nest_a, _watch_a) = seat(&url, &bearer, DEVICE_A).await;
    let (engine_b, _nest_b, watch_b) = seat(&url, &bearer, DEVICE_B).await;

    fauna_client_folders::FoldersClient::new(Arc::clone(&nest_a))
        .update(FolderUpdateRequest {
            name: FOLDER.into(),
            exclusive_editing: Some(true),
            ..Default::default()
        })
        .await
        .expect("exclusive editing on");

    engine_a.refresh_sync_mode().await;
    assert_eq!(engine_a.open_lease_window().await, LeaseWindow::Held);

    const REL: &str = "asset.blend";
    const BODY: &[u8] = b"a binary nobody can merge\n";
    stage_local_edit(&engine_b, &watch_b, REL, BODY).await;
    engine_b.refresh_sync_mode().await;

    let err = engine_b
        .upload_file(REL)
        .await
        .expect_err("a direct upload into a folder another device holds must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("exclusive editing") && msg.contains("untouched"),
        "the refusal must say why AND say the edit is safe — a message that reads as \
         'your change was rejected' is the one that costs a user their trust; got: {msg}"
    );
    assert_eq!(
        std::fs::read(watch_b.path().join(REL)).unwrap(),
        BODY,
        "the refused file is untouched on disk"
    );
    assert_eq!(
        pending_paths(&engine_b),
        vec![REL.to_string()],
        "a refused direct upload leaves the entry pending for the next pass, exactly as an \
         offline failure does"
    );
}

/// An **un-governed** folder — every folder until an owner opts in — takes no
/// lease at all.
///
/// This is the test that keeps the feature honest: the *Never* list's whole
/// point is that exclusive editing costs the ordinary case nothing, and the way
/// that regresses is somebody moving the acquire above the governance check. Two
/// seats writing the same un-governed folder concurrently is the before-picture,
/// and it must stay the picture.
#[tokio::test]
async fn an_ungoverned_folder_takes_no_lease_and_both_seats_write() {
    let (url, bearer) = start_test_nest().await;
    let (engine_a, _nest_a, watch_a) = seat(&url, &bearer, DEVICE_A).await;
    let (engine_b, _nest_b, watch_b) = seat(&url, &bearer, DEVICE_B).await;

    engine_a.refresh_sync_mode().await;
    engine_b.refresh_sync_mode().await;
    assert!(
        !engine_a.lease_posture().is_governed() && !engine_b.lease_posture().is_governed(),
        "a folder nobody opted in is un-governed — the flag's fail-open default"
    );

    assert_eq!(
        engine_a.open_lease_window().await,
        LeaseWindow::NotGoverned,
        "an un-governed folder must not reach the nest for a lease"
    );
    assert_eq!(
        engine_b.open_lease_window().await,
        LeaseWindow::NotGoverned,
        "…and neither seat locks the other out of it"
    );

    stage_local_edit(&engine_a, &watch_a, "a.txt", b"from A\n").await;
    stage_local_edit(&engine_b, &watch_b, "b.txt", b"from B\n").await;
    let (rec_a, _) = engine_a.upload_pending(4).await.unwrap();
    let (rec_b, _) = engine_b.upload_pending(4).await.unwrap();
    assert_eq!(rec_a, vec!["a.txt".to_string()]);
    assert_eq!(
        rec_b,
        vec!["b.txt".to_string()],
        "both seats write an un-governed folder concurrently, as they always have"
    );
    assert!(
        !engine_a.is_read_only_for_lease() && !engine_b.is_read_only_for_lease(),
        "no un-governed folder is ever read-only for a lease"
    );
}
