//! tier_3: **a folder's selective-sync filter reaches a RUNNING engine off the
//! nest row** — closing `file-sync.md` § Status *Selective sync filters nothing
//! on the deployments that matter*.
//!
//! The failing shape: `include_paths`/`exclude_paths` are set from any app's
//! Folders UI and stored on the nest folder row, but the only production
//! consumer was the LEGACY `bins/fauna-sync` daemon. Both live desktop
//! deployments — `engine_lifecycle::build_engine` (linux + apple in-process)
//! and `fauna-sync-agent`'s hydration host (all three desktops) — built their
//! `IgnoreMatcher` with a bare `load()` and never applied the row, so a user's
//! selective-sync setting round-tripped through the UI with **zero effect** on
//! what synced.
//!
//! The fix under test: the lists ride the SAME `fauna.folders.list` read the
//! mode, audience, residency and accepts postures already ride
//! (`config::resolve_device_mode_from_nest` → `SeatResolution::selective_sync`)
//! and install into the running engine's matcher in
//! `SyncEngine::refresh_sync_mode`. Neither engine builder holds a control
//! plane, so this refresh — driven at loop entry and on every rescan tick — is
//! the only place either deployment can learn the row. This test drives that
//! exact call against a real in-process nest, deliberately not a spun watcher
//! loop, which would trade a deterministic pin for a timing-dependent one
//! (testing.md convention 14).
//!
//! Five contract items, one per test:
//! 1. the filter **arms** from the row on the next resolution, and a
//!    non-excluded path is untouched;
//! 2. the user **clearing** the lists un-arms the running seat — the filter
//!    must not latch;
//! 3. the read is **sealed-first**: a row whose seal and plaintext column
//!    disagree filters by the SEAL (`path-sealing.md` § `folders.include_paths`/
//!    `exclude_paths` — the read seam is `render_include_paths`/
//!    `render_exclude_paths`, never the raw column);
//! 4. a reader that **cannot open** the seal keeps its armed filter rather than
//!    blanking it — the destructive-blank guard the same doc names as "the
//!    recurring failure of this whole flip", which on this plane means syncing
//!    exactly what the user excluded;
//! 5. a row whose seal was **deleted** — not corrupted — keeps the armed
//!    filter too: an absent seal and a row that
//!    never carried one are byte-identical to a reader, so whoever can write
//!    the nest-served row must not be able to un-filter a seat just by
//!    deleting the seal field.

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::token_store::TokenStore;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::folders::FolderUpdateRequest;
use fauna_sync_engine::adaptive::AdaptiveConcurrency;
use fauna_sync_engine::db::SyncDb;
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::ignore::IgnoreMatcher;
use fauna_sync_engine::nest_client::SyncClient;
use fauna_sync_engine::transfer::TransferPool;

const OWNER_SECRET: [u8; 32] = [0x5e; 32];
const DEVICE_ID: [u8; 32] = [0x0c; 32];
const FOLDER: &str = "selective_sync_test";

/// A path under the excluded prefix, and one that is not. Written as files the
/// upload leg would otherwise pick up: `SyncEngine::is_ignored` is the funnel
/// `watcher::full_scan_filtered` (the reconcile scan) and every
/// `provider_face::serve_upload_*` gate consult.
const EXCLUDED: &str = "private/secret.txt";
const KEPT: &str = "shared/notes.txt";

/// Start a real in-process nest serving the auth + sync + folder WS-RPC kinds
/// over a real bound `TcpListener`, with the owner registered and the folder
/// pre-created. Mirrors `conformance_sync_mode_role_flip.rs::start_test_nest`.
///
/// Also returns the raw `CacheDb` handle: item 5 below needs to reach a state
/// no `fauna.folders.update` request can reach — a row whose seal is deleted
/// but whose plaintext is left untouched — which only a direct DB write can
/// construct (`folder_handlers.rs`'s pair-moves-together rule refuses it),
/// standing in for any nest-side writer of the row, not only the ordinary
/// handler.
async fn start_test_nest(owner: [u8; 32]) -> (String, String, Arc<CacheDb>) {
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
    (format!("http://{addr}"), http_token, state.db.clone())
}

/// An owner-only engine bound to `FOLDER` with a genuinely connected control
/// plane. `backup_key` is the reader's own root — the tests hand it the real
/// owner key, except item 4, which hands it a stranger's so the seal will not
/// open. `ignore` is the matcher the *builder* installs, standing in for the
/// bare `IgnoreMatcher::load(&watch_dir)` both live builders do.
fn engine_and_control_client(
    dest_url: &str,
    http_token: &str,
    watch_path: std::path::PathBuf,
    db_path: std::path::PathBuf,
    backup_key: BackupKey,
    ignore: IgnoreMatcher,
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    let owner_kp = || ActorKeypair::from_secret(OWNER_SECRET);

    let http_bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer(http_token.to_string()));
    let http_auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        dest_url.to_string(),
        owner_kp(),
        http_bearer,
        reqwest::Client::new(),
    ));
    let engine_client = SyncClient::new(http_auth, &DEVICE_ID);
    let nest_client = fauna_client::NestClient::new(dest_url.to_string(), owner_kp());

    let engine = SyncEngine::new(
        watch_path,
        SyncDb::open(db_path).unwrap(),
        engine_client,
        Some(FOLDER.to_string()),
        DEVICE_ID,
        None, // mls
        None, // epoch_secret
        Some(backup_key.into()),
        None, // mls_group_id
        None, // content_keys
        fauna_core::format::ConflictPolicy::Auto,
        fauna_core::format::FormatRegistry::new(),
        ignore,
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        Arc::clone(&nest_client),
        fauna_sync_engine::config::SyncMode::Sync,
    );
    (engine, nest_client)
}

/// The whole fixture: a nest, a connected engine, and the folder's row id (the
/// selective-sync seal's salt, minted by the nest at INSERT — which is why a
/// create request can never carry these seals and they arrive on the first
/// `fauna.folders.update`).
struct Fixture {
    engine: SyncEngine,
    folders: fauna_client_folders::FoldersClient<Arc<fauna_client::NestClient>>,
    folder_id: i64,
    /// The raw DB handle — item 5's route to a state no WS-RPC request can
    /// reach (see `start_test_nest`'s doc comment).
    db: Arc<CacheDb>,
    _watch: tempfile::TempDir,
    _state: tempfile::TempDir,
}

async fn fixture(reader_key: BackupKey, ignore: IgnoreMatcher) -> Fixture {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, token, db) = start_test_nest(owner).await;

    let watch = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let (engine, nest_client) = engine_and_control_client(
        &url,
        &token,
        watch.path().to_path_buf(),
        state_dir.path().join("fs-selective-sync.db"),
        reader_key,
        ignore,
    );
    nest_client
        .connect()
        .await
        .expect("nest_client must reach Connected (WS auth handshake)");

    // Enroll the seat the way the creation wizard does, so the seat resolves
    // through the ordinary owner path rather than a degraded read.
    let device_hex = hex::encode(DEVICE_ID);
    fauna_client_sync::SyncClient::new(Arc::clone(&nest_client))
        .register(device_hex.clone(), "selective-sync-seat", None)
        .await
        .expect("fauna.sync.register");
    let folders = fauna_client_folders::FoldersClient::new(Arc::clone(&nest_client));
    folders
        .places_set(fauna_protocol::folders::PlacesSetRequest {
            name: FOLDER.to_string(),
            device_id: device_hex,
            flags: fauna_protocol::folders::PlaceFlags::default_place(),
            ..Default::default()
        })
        .await
        .expect("places.set (default)");

    let row = folders
        .list_owned_and_shared()
        .await
        .expect("fauna.folders.list")
        .folders
        .into_iter()
        .find(|fs| fs.name == FOLDER)
        .expect("the pre-created set must be listed");
    let folder_id = row.id;
    // Bind by ref, as the resident agent does: the engine's per-tick row read
    // (which carries the selective-sync lists) resolves the set's row by its
    // `FolderRef` alone, and an engine holding none reads no row.
    let engine = engine.with_binding_edge(fauna_sync_engine::binding_edge::BindingEdge {
        folder_ref: fauna_core::folder_keys::FolderRef::Local(row.id),
        basis: fauna_sync_engine::binding_edge::BindingBasis::of(&row),
        on_rebuild: Arc::new(|| {}),
    });

    Fixture {
        engine,
        folders,
        folder_id,
        db,
        _watch: watch,
        _state: state_dir,
    }
}

/// The user's selective-sync save, as the app performs it
/// (`DevicesMachine::set_folder_paths` — the plane's only durable writer): the
/// plaintext and its seal move together, the seal minted under the owner root
/// salted by the row id. `sealed_as` lets item 3 seal a list the plaintext
/// column disagrees with, which is what makes "sealed-first" observable.
async fn save_excludes(
    fx: &Fixture,
    owner: &BackupKey,
    plaintext: Option<Vec<String>>,
    sealed_as: Option<Vec<String>>,
) {
    let exclude_paths_sealed = sealed_as.map(|list| {
        fauna_protocol::ByteBuf::from(
            fauna_core::label_custody::seal_exclude_paths(owner, fx.folder_id, &list)
                .expect("seal exclude_paths"),
        )
    });
    fx.folders
        .update(FolderUpdateRequest {
            name: FOLDER.to_string(),
            exclude_paths: plaintext,
            exclude_paths_sealed,
            ..Default::default()
        })
        .await
        .expect("fauna.folders.update (the selective-sync save)");
}

/// Contract item 1 + 2: the filter arms from the row on a running engine, and
/// un-arms when the user clears it.
#[tokio::test]
async fn the_row_arms_a_running_engines_filter_and_clearing_it_un_arms() {
    let owner = BackupKey::derive(&OWNER_SECRET);
    let fx = fixture(owner.clone(), IgnoreMatcher::default()).await;

    // Before any resolution the builder's matcher governs — the bare `load()`
    // shape both live deployments ship, which filters nothing.
    assert!(
        !fx.engine.is_ignored(EXCLUDED),
        "precondition: the builder's matcher must not already filter — otherwise \
         this test could pass without reading the row at all"
    );

    save_excludes(
        &fx,
        &owner,
        Some(vec!["private".to_string()]),
        Some(vec!["private".to_string()]),
    )
    .await;

    // The rescan tick's exact call.
    fx.engine.refresh_sync_mode().await;
    assert!(
        fx.engine.is_ignored(EXCLUDED),
        "the user's exclude_paths must reach the RUNNING engine on its next \
         resolution — this is the whole gap: before the fix the row round-tripped \
         through the UI with zero effect on what synced"
    );
    assert!(
        !fx.engine.is_ignored(KEPT),
        "a path outside the excluded prefix must still sync — an over-broad \
         filter would silently stop backing up the user's files"
    );

    // The user clears the exclusions in the same editor. A keyed save seals the
    // now-empty list, so the row's authoritative answer is "nothing excluded".
    save_excludes(&fx, &owner, Some(vec![]), Some(vec![])).await;
    fx.engine.refresh_sync_mode().await;
    assert!(
        !fx.engine.is_ignored(EXCLUDED),
        "clearing the lists must un-arm the RUNNING seat — a filter that only \
         ever tightens would strand the user's files out of sync with no way \
         back short of a process restart"
    );
}

/// Contract item 3: the read is sealed-first. The row's plaintext column
/// and its seal are deliberately made to disagree; the engine must filter by
/// the SEAL. Pins that the install goes through
/// `label_custody::render_exclude_paths` with the owner key rather than reading
/// `FolderSummary::exclude_paths` raw — the mistake `path-sealing.md` names as
/// the recurring failure of this flip.
#[tokio::test]
async fn the_filter_renders_sealed_first_not_from_the_plaintext_column() {
    let owner = BackupKey::derive(&OWNER_SECRET);
    let fx = fixture(owner.clone(), IgnoreMatcher::default()).await;

    save_excludes(
        &fx,
        &owner,
        Some(vec!["decoy".to_string()]),
        Some(vec!["private".to_string()]),
    )
    .await;

    fx.engine.refresh_sync_mode().await;
    assert!(
        fx.engine.is_ignored(EXCLUDED),
        "the SEALED list must govern — a reader that took the plaintext column \
         would filter `decoy/` and sync `private/`, exactly inverting the user's \
         intent once S9 scrubs the plaintext"
    );
    assert!(
        !fx.engine.is_ignored("decoy/x.txt"),
        "the plaintext column must NOT govern while a seal is openable"
    );
}

/// Contract item 4: a reader that cannot open the seal keeps its armed filter.
///
/// The row here carries a seal and **no** plaintext (the S8 seal-only update
/// shape, which is also what every row looks like after the S9 scrub), and the
/// engine holds a stranger's key. A render that degraded to "no patterns" would
/// install a blank filter and upload precisely the files the user excluded;
/// the resolution must instead keep the armed posture, exactly as the mode,
/// audience, residency and accepts postures do on an unreadable read.
#[tokio::test]
async fn an_unopenable_seal_keeps_the_armed_filter_rather_than_blanking_it() {
    let stranger = BackupKey::from_bytes([0x77; 32]);
    let armed = IgnoreMatcher::from_config_patterns(&[], &["private".to_string()]);
    let fx = fixture(stranger, armed).await;

    assert!(
        fx.engine.is_ignored(EXCLUDED),
        "precondition: this engine starts with the filter armed"
    );

    // Seal-only update: the seal is stamped, the plaintext column is left unset.
    save_excludes(
        &fx,
        &BackupKey::derive(&OWNER_SECRET),
        None,
        Some(vec!["private".to_string()]),
    )
    .await;

    fx.engine.refresh_sync_mode().await;
    assert!(
        fx.engine.is_ignored(EXCLUDED),
        "a seal this reader cannot open must leave the armed filter standing — \
         blanking it would sync exactly what the user excluded, and no later \
         pass could re-derive the list (the nest row is its only home)"
    );
}

/// Contract item 5: a row whose seal was DELETED — not corrupted —
/// keeps the armed filter, exactly like an unopenable one.
///
/// Reaches a row shape no `fauna.folders.update` request can produce: the
/// seal gone and the plaintext column also absent. `folder_handlers.rs`'s
/// pair-moves-together rule means every ordinary write of the plaintext
/// writes the seal alongside it (`None` included, which clears it), so the
/// only way to null the seal ALONE, leaving the plaintext column untouched,
/// is a direct DB write standing in for any writer with row access — exactly
/// the threat charter finding names. Before the fix, `config.rs`'s
/// `selective_list` read this shape identically to a row that never had
/// selective-sync configured and installed an empty filter, un-filtering a
/// running seat with no signal anywhere.
#[tokio::test]
async fn a_deleted_seal_keeps_the_armed_filter_rather_than_blanking_it() {
    let owner = BackupKey::derive(&OWNER_SECRET);
    let fx = fixture(owner.clone(), IgnoreMatcher::default()).await;

    // Arm the filter the ordinary way: a seal-only update (the S8 backfill
    // shape, same as item 4) with no plaintext column at all.
    save_excludes(&fx, &owner, None, Some(vec!["private".to_string()])).await;
    fx.engine.refresh_sync_mode().await;
    assert!(
        fx.engine.is_ignored(EXCLUDED),
        "precondition: the sealed row must have armed the filter"
    );

    // A nest-side row edit that deletes JUST the seal, leaving the (already
    // absent) plaintext column untouched — unreachable via the ordinary
    // handler, reachable by anything with direct row access.
    let owner_actor = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    fx.db
        .update_folder_for_user(
            FOLDER,
            &owner_actor,
            fauna_nest::db::FolderUpdate {
                exclude_paths_sealed: Some(None),
                ..Default::default()
            },
        )
        .await
        .expect("nest-side row edit deleting just the seal");

    fx.engine.refresh_sync_mode().await;
    assert!(
        fx.engine.is_ignored(EXCLUDED),
        "a row whose seal was deleted (not corrupted) must leave the armed \
         filter standing — an absent seal and a row that never carried one \
         are byte-identical to this reader, so blanking here would sync \
         exactly what the user excluded, on every seat, with no signal \
         anywhere"
    );
}
