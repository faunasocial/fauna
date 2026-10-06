//! tier_3: **a device-place change reaches a RUNNING engine, no process
//! restart** — the leg-1 pin (`file-sync.md` § 4).
//!
//! The failing shape: `HydrationHost::sync_mode` was read exactly once, above
//! the resident loop, and nothing ever re-read it — so a user opening the
//! wizard and making a seat an archive (no peer deletes) wrote the nest row
//! while the running engine kept applying peers' deletes until its process
//! restarted, with nothing in any app saying so. The guard did not arm when
//! the user armed it.
//!
//! The fix under test: the engine itself owns the resolution
//! (`SyncEngine::refresh_sync_mode` — the seat's place via the shared
//! `config::resolve_device_mode`), and `always_resident::run_watch_loop`
//! drives it at entry and on **every rescan tick**. This test drives the
//! tick's exact call (`refresh_sync_mode`) against a real in-process nest —
//! deliberately not a spun watcher loop, which would trade a deterministic
//! pin for a timing-dependent one (testing.md convention 14); the tick arm's
//! one-line call to it is source-visible in `run_watch_loop`.
//!
//! Also pinned end-to-end here: the write path IS `places.set`
//! (`INSERT OR REPLACE` — a re-set is the place change), and a fresh
//! authoritative answer persists to the engine's SyncDb
//! (`get_cached_sync_mode`), which is what re-arms an offline start
//! (contract item 3's authoritative-write half; the read-back half is
//! pinned engine-side in `pull_remote_changes_test.rs`).

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::identity::ActorKeypair;
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

const OWNER_SECRET: [u8; 32] = [0x53; 32];
const DEVICE_ID: [u8; 32] = [0x0a; 32];
const FOLDER: &str = "role_flip_test";

/// Start a real in-process nest serving the auth + sync + folder WS-RPC
/// kinds over a real bound `TcpListener`, with the owner registered and the
/// folder pre-created. Mirrors
/// `conformance_sync_engine_record_commit.rs::start_test_nest`, plus the
/// `fauna.folders.*` cluster this test's role writes and reads live on.
async fn start_test_nest(owner: [u8; 32]) -> (String, String) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlive the test; never deleted under test
    let backup_svc =
        Arc::new(BackupService::new(db.clone(), None, false, blob_path, None).unwrap());

    db.create_user(&owner, "free", "test").await.unwrap();
    db.create_folder(FOLDER, &owner).await.unwrap();

    let token_store = Arc::new(TokenStore::new());
    // `owner` is already an actor id — re-deriving it through `from_secret`
    // minted for an actor with no `users` row, which every bearer door
    // refuses now that it asks the actor's standing.
    let http_token = token_store
        .insert(fauna_core::identity::ActorId(owner), 3600)
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
    (format!("http://{addr}"), http_token)
}

/// An owner-only engine bound to `FOLDER` with a genuinely connected
/// control plane, exactly as `conformance_sync_engine_record_commit.rs`
/// builds one — but with a **file-backed** SyncDb so the persisted
/// last-authoritative mode is observable.
fn engine_and_control_client(
    dest_url: &str,
    http_token: &str,
    watch_path: std::path::PathBuf,
    db_path: std::path::PathBuf,
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    let (engine_client, nest_client) =
        common::sync_engine_auth_client(dest_url, http_token, OWNER_SECRET, &DEVICE_ID);

    let engine = SyncEngine::new(
        watch_path,
        SyncDb::open(db_path).unwrap(),
        engine_client,
        Some(FOLDER.to_string()),
        DEVICE_ID,
        None, // mls
        None, // epoch_secret
        Some(BackupKey::derive(&OWNER_SECRET).into()),
        None, // mls_group_id
        None, // content_keys
        fauna_core::format::ConflictPolicy::Auto,
        fauna_core::format::FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        Arc::clone(&nest_client),
        fauna_sync_engine::config::SyncMode::Sync,
    );
    (engine, nest_client)
}

#[tokio::test]
async fn a_place_change_reaches_a_running_engine_without_a_restart() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, token) = start_test_nest(owner).await;

    let watch = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let (engine, nest_client) = engine_and_control_client(
        &url,
        &token,
        watch.path().to_path_buf(),
        state_dir.path().join("fs-role-flip.db"),
    );
    nest_client
        .connect()
        .await
        .expect("nest_client must reach Connected (WS auth handshake)");

    // Enroll the seat the way the creation wizard does: register the device,
    // then write its place at the default point.
    let device_hex = hex::encode(DEVICE_ID);
    fauna_client_sync::SyncClient::new(Arc::clone(&nest_client))
        .register(device_hex.clone(), "role-flip-seat", None)
        .await
        .expect("fauna.sync.register");
    let folders = fauna_client_folders::FoldersClient::new(Arc::clone(&nest_client));
    folders
        .places_set(fauna_protocol::folders::PlacesSetRequest {
            name: FOLDER.to_string(),
            device_id: device_hex.clone(),
            flags: fauna_protocol::folders::PlaceFlags::default_place(),
            ..Default::default()
        })
        .await
        .expect("places.set (default)");

    // The loop's entry resolution: a default-point seat converges deletes.
    engine.refresh_sync_mode().await;
    assert!(
        engine.applies_remote_deletes(),
        "a default-point seat must converge deletes after the entry resolution"
    );

    // The user unticks "applies deletes" in the place editor — a places.set
    // re-set (INSERT OR REPLACE) IS that write.
    folders
        .places_set(fauna_protocol::folders::PlacesSetRequest {
            name: FOLDER.to_string(),
            device_id: device_hex.clone(),
            flags: fauna_protocol::folders::PlaceFlags::archive_place(),
            ..Default::default()
        })
        .await
        .expect("places.set (archive — the editor's place change)");

    // The SAME engine object — no rebuild, no process restart — re-resolves
    // exactly as the next rescan tick does, and the guard arms.
    engine.refresh_sync_mode().await;
    assert!(
        !engine.applies_remote_deletes(),
        "the place change must reach the RUNNING engine on its next resolution \
         — this was leg 1: the guard did not arm when the user armed it"
    );

    // The fresh authoritative answer persisted, so an offline start of this
    // seat re-arms from what the nest last said instead of the
    // delete-applying default (contract item 3).
    assert_eq!(
        engine.db().get_cached_sync_mode().unwrap().as_deref(),
        Some("backup"),
        "the authoritative answer must persist for the offline-start fallback"
    );

    // And back: a seat the user returns to Mirror converges again — the
    // refresh is a live two-way correction, not a one-way ratchet.
    folders
        .places_set(fauna_protocol::folders::PlacesSetRequest {
            name: FOLDER.to_string(),
            device_id: device_hex,
            flags: fauna_protocol::folders::PlaceFlags::default_place(),
            ..Default::default()
        })
        .await
        .expect("places.set (default)");
    engine.refresh_sync_mode().await;
    assert!(
        engine.applies_remote_deletes(),
        "a default-point seat converges deletes again"
    );
    assert_eq!(
        engine.db().get_cached_sync_mode().unwrap().as_deref(),
        Some("sync"),
        "the persisted answer follows the live one"
    );
}

/// The `accepts` gate's live flip — the sibling of the role-flip pin above,
/// for the flag phase 2 slice c made real: a seat the user flips to
/// **source** (originates only) stops accepting remote changes on its next
/// resolution, and a flip back reopens delivery — same running engine, no
/// restart, both directions (folders re-model § Places;
/// `file-sync.md` § 4 owns the posture).
#[tokio::test(flavor = "multi_thread")]
async fn an_accepts_flip_reaches_a_running_engine_without_a_restart() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, token) = start_test_nest(owner).await;

    let watch = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let (engine, nest_client) = engine_and_control_client(
        &url,
        &token,
        watch.path().to_path_buf(),
        state_dir.path().join("fs-accepts-flip.db"),
    );
    nest_client
        .connect()
        .await
        .expect("nest_client must reach Connected (WS auth handshake)");

    let device_hex = hex::encode(DEVICE_ID);
    fauna_client_sync::SyncClient::new(Arc::clone(&nest_client))
        .register(device_hex.clone(), "accepts-flip-seat", None)
        .await
        .expect("fauna.sync.register");
    let folders = fauna_client_folders::FoldersClient::new(Arc::clone(&nest_client));
    folders
        .places_set(fauna_protocol::folders::PlacesSetRequest {
            name: FOLDER.to_string(),
            device_id: device_hex.clone(),
            flags: fauna_protocol::folders::PlaceFlags::default_place(),
            ..Default::default()
        })
        .await
        .expect("places.set (default)");

    engine.refresh_sync_mode().await;
    assert!(
        engine.accepts_remote_changes(),
        "a default-point seat accepts remote changes"
    );

    // The user makes this seat source-only — the one `PlaceFlags` setting whose
    // place does not accept delivery.
    folders
        .places_set(fauna_protocol::folders::PlacesSetRequest {
            name: FOLDER.to_string(),
            device_id: device_hex.clone(),
            flags: fauna_protocol::folders::PlaceFlags::new(true, false, false),
            ..Default::default()
        })
        .await
        .expect("places.set (originates only)");
    engine.refresh_sync_mode().await;
    assert!(
        !engine.accepts_remote_changes(),
        "the source flip must close the delivery rails on the RUNNING engine's \
         next resolution — no restart"
    );

    // And back: delivery reopens, catching up from the held anchor.
    folders
        .places_set(fauna_protocol::folders::PlacesSetRequest {
            name: FOLDER.to_string(),
            device_id: device_hex,
            flags: fauna_protocol::folders::PlaceFlags::default_place(),
            ..Default::default()
        })
        .await
        .expect("places.set (default again)");
    engine.refresh_sync_mode().await;
    assert!(
        engine.accepts_remote_changes(),
        "the flip back must reopen delivery — a live two-way correction"
    );
}
