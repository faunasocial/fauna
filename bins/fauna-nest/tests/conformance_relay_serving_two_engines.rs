//! **Two real engines pass a metadata-only folder's file through the relay**
//! (tier_3) — seat A writes, seat B hydrates, and the nest's store never holds
//! a chunk of it.
//!
//! `docs/goal/behavior/file-sync.md` § Relay serving owns the mechanism; this
//! is its end-to-end witness on the **seat** side. The nest half has its own
//! (`sync_relay_serving.rs`, where a test client plays the seat) and the
//! engine's serve core has its own (`fauna-sync-engine`'s `relay_serve_test`,
//! which calls `serve_chunk` by hand). Neither proves that a seat *announces*,
//! that the ask reaches the right engine, or that the engine *answers* — the
//! three links `fauna_sync_engine::relay_seat` adds, and the ones without which
//! a metadata-only folder's bytes cannot move between app-only devices at all.
//!
//! **What only this test catches**, composed: seat A's upload skips the chunk
//! bytes and still indexes them; both seats announce the folder on their own
//! WS-RPC connections; seat B's ordinary pull `GET`s each chunk with the folder
//! hint; the nest asks the announced connections; A's seat routes the ask to
//! the folder's engine, which serves the range off the file and posts it; B
//! reassembles the file — while B, itself an announced seat that is asked for
//! the very chunks it is fetching, declines at once instead of holding the
//! relay's window until the deadline.
//!
//! **The residency assertion names hashes, never totals the store**
//! (`file-sync.md` § Content residency's status paragraph): the blob root also
//! holds the nest's own backups, so its size says nothing about this folder.
//! The file's store keys are read off seat A's own index.
//!
//! Tier: tier_3 (real nest surface, real wire, real seal; the two seats are
//! two production engines in one process, which is what makes the interleaving
//! deterministic). No settle-sleep: every wait is on the seat's own admitted
//! announce or on the pull itself.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::folder_keys::FolderRef;
use fauna_core::format::{ConflictPolicy, FormatRegistry};
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::blob_store::BlobStoreBackend;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::token_store::TokenStore;
use fauna_sync_engine::adaptive::AdaptiveConcurrency;
use fauna_sync_engine::db::SyncDb;
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::ignore::IgnoreMatcher;
use fauna_sync_engine::relay_seat::RelaySeat;
use fauna_sync_engine::transfer::TransferPool;

mod common;

/// Both seats are devices of the SAME user — two desktops of one account, the
/// deployment the section exists for.
const OWNER_SECRET: [u8; 32] = [0x52; 32];
/// Seat A — the device that writes the file and holds its only copy.
const DEVICE_A: [u8; 32] = [0x1A; 32];
/// Seat B — the device that hydrates it by relay.
const DEVICE_B: [u8; 32] = [0x1B; 32];
const FOLDER: &str = "vault";
const REL: &str = "ledger.bin";

/// Generous next to a healthy round trip, short of the relay's 30 s per-seat
/// fetch deadline — so a read that was made to wait out a silent seat fails
/// the assertion rather than passing slowly. Not 15 s: a 12 MiB relayed pull of
/// a debug build took 15.5 s on a 4-core CI runner beside the rest of the
/// suite, with every seat answering; the discriminated event is ≥ 30 s.
const PROMPT: Duration = Duration::from_secs(25);

/// Past the 8 MiB single-chunk threshold and varied, so the file is several
/// chunks and the relay is asked more than once.
fn body() -> Vec<u8> {
    (0..12 * 1024 * 1024u64)
        .map(|i| ((i.wrapping_mul(31) ^ (i / 251)) % 251) as u8)
        .collect()
}

struct Nest {
    url: String,
    bearer: String,
    folder: FolderRef,
    store: Arc<dyn BlobStoreBackend>,
    db: Arc<CacheDb>,
}

/// A real in-process nest serving the auth, sync and folder kinds and the
/// chunk routes, its relay wired as `build_app_state` wires it, with the
/// owner, both devices and a **metadata-only** `FOLDER` in place.
async fn start_test_nest() -> Nest {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let base = AppState::for_test(db.clone());
    let blob_path = base.test_scratch_path("blobs");
    let backup_svc =
        Arc::new(BackupService::new(db.clone(), None, false, blob_path, None).unwrap());
    let store = backup_svc.local_blob_store();

    db.create_user(&owner, "free", "test").await.unwrap();
    for (device, label) in [(DEVICE_A, "laptop"), (DEVICE_B, "desktop")] {
        db.register_sync_device(&owner, &device, label, None, "write")
            .await
            .unwrap();
    }
    let folder_id = db.create_folder(FOLDER, &owner).await.unwrap();
    assert!(
        db.update_folder_for_user(
            FOLDER,
            &owner,
            fauna_nest::db::FolderUpdate {
                residency: Some(Some("metadata_only")),
                // The set's stored nonce — what a signed record's statement
                // is verified under.
                set_nonce: Some(&common::SET_NONCE),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
        "the owner's folder row must exist"
    );

    let token_store = Arc::new(TokenStore::new());
    let bearer = token_store
        .insert(ActorKeypair::from_secret(OWNER_SECRET).actor_id(), 3600)
        .await;

    let resolver = Arc::new(fauna_nest::chunk_relay::ChunkResolver::new(
        Some(store.clone()),
        None,
        false,
    ));
    let nest_db = db.clone();
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
        sync: fauna_nest::state::SyncState {
            chunk_resolver: resolver,
        },
        ..base
    });

    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Nest {
        url: format!("http://{addr}"),
        bearer,
        folder: FolderRef::Local(folder_id),
        store,
        db: nest_db,
    }
}

/// One seat: a production owner-`BackupKey` engine on its own watch dir and its
/// own `SyncDb`, its control plane connected and its residency posture read
/// off the nest.
async fn seat(
    nest: &Nest,
    device_id: [u8; 32],
) -> (SyncEngine, Arc<fauna_client::NestClient>, tempfile::TempDir) {
    let watch = tempfile::tempdir().unwrap();
    let (engine, nest_client) = seat_over(
        nest,
        device_id,
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
    )
    .await;
    (engine, nest_client, watch)
}

/// [`seat`] over a given root and state DB — an on-demand replica's engine,
/// rooted at its kept root over a file DB its body source reads beside it.
async fn seat_over(
    nest: &Nest,
    device_id: [u8; 32],
    root: &std::path::Path,
    db: SyncDb,
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    let (engine_client, nest_client) = fauna_nest::test_support::sync_engine_auth_client(
        &nest.url,
        &nest.bearer,
        OWNER_SECRET,
        &device_id,
    );
    let ignore = IgnoreMatcher::load(root).unwrap_or_default();
    let engine = SyncEngine::new(
        root.to_path_buf(),
        db,
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
    // A writer engine signs every record it sends (the nest refuses an
    // unsigned one), directly under the owner's key and the set's stored
    // nonce; its reader judges served rows against the same.
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
    nest_client
        .connect()
        .await
        .expect("seat's nest_client must authenticate (WS auth handshake)");
    // Bind by ref, as the resident agent does: the engine's posture read
    // resolves the folder's row by its `FolderRef` alone, and an engine
    // holding none reads no row — so no residency. The list request also waits
    // for the connection, which the posture read below does not.
    let row = fauna_client_folders::FoldersClient::new(Arc::clone(&nest_client))
        .list_owned_and_shared()
        .await
        .expect("list the owner's folders")
        .folders
        .into_iter()
        .find(|fs| fs.name == FOLDER)
        .expect("the fixture's one folder is listed");
    assert_eq!(FolderRef::Local(row.id), nest.folder);
    let engine = engine.with_binding_edge(fauna_sync_engine::binding_edge::BindingEdge {
        folder_ref: nest.folder,
        basis: fauna_sync_engine::binding_edge::BindingBasis::of(&row),
        on_rebuild: Arc::new(|| {}),
    });
    engine.refresh_sync_mode().await;
    assert!(
        engine.is_metadata_only_residency(),
        "the seat must read the folder's residency off its own list refresh, or its upload \
         would rest the bytes this test asserts never rest"
    );
    (engine, nest_client)
}

async fn store_holds_any(store: &Arc<dyn BlobStoreBackend>, keys: &[[u8; 32]]) -> bool {
    for key in keys {
        if store
            .exists(&ContentHash::from_digest_raw(*key))
            .await
            .unwrap()
        {
            return true;
        }
    }
    false
}

/// A reader's hinted GET of one store key, as every engine sends it.
async fn hinted_get(nest: &Nest, store_key_hex: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .get(format!(
            "{}{}",
            nest.url,
            fauna_nest_http::paths::chunk_store::chunk_by_hash(store_key_hex)
        ))
        .query(&[(
            fauna_nest_http::paths::chunk_store::FOLDER_HINT_PARAM,
            FOLDER,
        )])
        .bearer_auth(&nest.bearer)
        .send()
        .await
        .expect("GET completes")
        .status()
}

/// The whole flow, as one interleaving: A writes and keeps the only copy, both
/// seats announce, B hydrates by relay, and no chunk of the file ever rests.
#[tokio::test]
async fn seat_b_hydrates_seat_as_file_by_relay_and_the_store_holds_no_chunk_of_it() {
    let nest = start_test_nest().await;
    let (engine_a, nest_a, watch_a) = seat(&nest, DEVICE_A).await;
    let (engine_b, nest_b, watch_b) = seat(&nest, DEVICE_B).await;
    let body = body();

    // ── A writes: the manifest goes up, the bytes stay on A's disk ──
    std::fs::write(watch_a.path().join(REL), &body).unwrap();
    engine_a
        .reconcile()
        .await
        .expect("reconcile stages the file");
    let outcome = engine_a
        .upload_file(REL)
        .await
        .expect("A seals the file and records the change");
    assert!(outcome.recorded, "the change record reached the nest");
    let keys = engine_a.db().held_store_keys(REL).unwrap();
    assert!(
        keys.len() > 1,
        "the file must be several chunks or the relay is asked once and the test proves less \
         than it claims (got {})",
        keys.len()
    );
    assert!(
        !store_holds_any(&nest.store, &keys).await,
        "a metadata-only folder's seat uploads no chunk bytes"
    );

    // ── Before any seat announces, the relay has nobody to ask ──
    assert_eq!(
        hinted_get(&nest, &hex::encode(keys[0])).await,
        reqwest::StatusCode::NOT_FOUND,
        "with no announced seat the hinted read is a plain miss — the state every app-only \
         deployment was in before the serving seat existed"
    );

    // ── Both seats announce and serve, each beside its own engine ──
    let wire = nest.folder.to_wire();
    let (seat_a, seat_b) = (RelaySeat::new(), RelaySeat::new());
    let mut inbox_a = seat_a.register(&nest.folder, None);
    let mut inbox_b = seat_b.register(&nest.folder, None);
    let (device_a, device_b) = (hex::encode(DEVICE_A), hex::encode(DEVICE_B));
    // Boxed, not stack-pinned: dropping it below is what ends A's serving.
    let mut serving = Box::pin(async {
        tokio::join!(
            seat_a.run(&nest_a, &device_a),
            inbox_a.serve(&engine_a),
            seat_b.run(&nest_b, &device_b),
            inbox_b.serve(&engine_b),
        );
    });

    for seat in [&seat_a, &seat_b] {
        let mut admitted = seat.subscribe_admitted();
        tokio::select! {
            _ = &mut serving => unreachable!("serving never ends on its own"),
            waited = tokio::time::timeout(PROMPT, admitted.wait_for(|a| a.contains(&wire))) => {
                waited
                    .expect("the nest admits the announce in time")
                    .expect("the seat is alive");
            }
        }
    }

    // ── B pulls: every chunk is asked of the seats and served off A's file ──
    let started = Instant::now();
    tokio::select! {
        _ = &mut serving => unreachable!("serving never ends on its own"),
        pulled = engine_b.pull_remote_changes() => {
            pulled.expect("B's pull runs");
        }
    }
    assert_eq!(
        std::fs::read(watch_b.path().join(REL)).expect("B holds the file after its pull"),
        body,
        "B reassembles A's file byte for byte from relayed chunks"
    );
    assert!(
        started.elapsed() < PROMPT,
        "the pull must not wait out a silent seat's deadline ({:?})",
        started.elapsed()
    );
    assert!(
        !store_holds_any(&nest.store, &keys).await,
        "relayed bytes of a metadata-only folder never rest on the nest"
    );

    // ── A chunk nobody holds: every announced seat declines at once ──
    //
    // B's index names every key of the file now (a download apply indexes it),
    // so the key asked here is one neither seat has ever seen. Both are asked,
    // both answer `DELETE`, and the reader gets its miss without the deadline.
    let unheld = "5e".repeat(32);
    let started = Instant::now();
    let status = tokio::select! {
        _ = &mut serving => unreachable!("serving never ends on its own"),
        status = hinted_get(&nest, &unheld) => status,
    };
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert!(
        started.elapsed() < PROMPT,
        "a seat that holds no such chunk must decline at once, not go silent ({:?})",
        started.elapsed()
    );

    // ── And B is now a holder too: A stops serving, B answers for the file ──
    drop(serving);
    drop(inbox_a);
    let mut serving_b = Box::pin(async {
        tokio::join!(seat_b.run(&nest_b, &device_b), inbox_b.serve(&engine_b));
    });
    let first_key = hex::encode(keys[0]);
    let status = tokio::select! {
        _ = &mut serving_b => unreachable!("serving never ends on its own"),
        status = hinted_get(&nest, &first_key) => status,
    };
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "a seat that applied a download serves it — the second device is a holder"
    );
}

/// A replica host the seat reaches through a door, as an app reaches its
/// provider's hosts (`fauna-ffi`'s file-provider host): the engine stays on
/// the test's own task, and each ask crosses to it with a signal back.
struct DoorHost {
    folder: FolderRef,
    asks: tokio::sync::mpsc::Sender<(
        fauna_sync_engine::relay_seat::ServeAsk,
        tokio::sync::oneshot::Sender<()>,
    )>,
}

#[async_trait::async_trait]
impl fauna_sync_engine::relay_seat::ServingHost for DoorHost {
    async fn serving_folder(&self) -> Option<fauna_sync_engine::relay_seat::ServedFolder> {
        Some(fauna_sync_engine::relay_seat::ServedFolder {
            folder: self.folder,
            home_nest_url: None,
        })
    }

    async fn answer_serve_ask(&self, ask: fauna_sync_engine::relay_seat::ServeAsk) {
        let (answered, done) = tokio::sync::oneshot::channel();
        if self.asks.send((ask, answered)).await.is_ok() {
            let _ = done.await;
        }
    }
}

struct OneHost(Arc<DoorHost>);

impl fauna_sync_engine::relay_seat::ServingHosts for OneHost {
    fn hosts(&self) -> Vec<Arc<dyn fauna_sync_engine::relay_seat::ServingHost>> {
        vec![Arc::clone(&self.0) as Arc<dyn fauna_sync_engine::relay_seat::ServingHost>]
    }
}

/// **An on-demand replica serves the relay** (`file-sync.md` § Relay serving →
/// *Which seats hold*, the on-demand rows): seat C owns its tree — a kept root
/// and a cache root, the shape of android's provider — and holds only rows
/// until it opens the file, which hydrates into its cache root by relay from
/// seat A. Then A stops, and C, tracked through a door by
/// `RelaySeat::serve_hosts`, answers the relay from its cache root through its
/// body source; once the body is freed back to a placeholder, C declines at
/// once rather than hydrating on the asker's behalf.
#[tokio::test]
async fn an_on_demand_replica_serves_a_body_it_hydrated_and_declines_a_placeholder() {
    let nest = start_test_nest().await;
    let (engine_a, nest_a, watch_a) = seat(&nest, DEVICE_A).await;
    let body = body();
    std::fs::write(watch_a.path().join(REL), &body).unwrap();
    engine_a
        .reconcile()
        .await
        .expect("reconcile stages the file");
    assert!(engine_a.upload_file(REL).await.expect("A records").recorded);
    let keys = engine_a.db().held_store_keys(REL).unwrap();

    // ── C: an owned tree over a file DB, its serve reading through the tree ──
    let dir = tempfile::tempdir().unwrap();
    let (kept, cache) = (dir.path().join("kept"), dir.path().join("cache"));
    for root in [&kept, &cache] {
        std::fs::create_dir_all(root).unwrap();
    }
    let db_path = dir.path().join("fsid-c.db");
    let (engine_c, nest_c) =
        seat_over(&nest, DEVICE_B, &kept, SyncDb::open(&db_path).unwrap()).await;
    engine_c.set_serve_body(Arc::new(
        fauna_sync_engine::share_body::OnDemandBodySource::new(
            kept.clone(),
            cache.clone(),
            SyncDb::open(&db_path).unwrap(),
        ),
    ));
    let tree = fauna_sync_engine::provider_face::owned_tree::OwnedTree::new(kept, cache);
    engine_c
        .populate_placeholders_from_nest()
        .await
        .expect("C records the file's row");

    // ── C opens the file while A serves: it hydrates into C's cache root ──
    let wire = nest.folder.to_wire();
    let seat_a = RelaySeat::new();
    let mut inbox_a = seat_a.register(&nest.folder, None);
    let device_a = hex::encode(DEVICE_A);
    let mut serving_a = Box::pin(async {
        tokio::join!(seat_a.run(&nest_a, &device_a), inbox_a.serve(&engine_a));
    });
    let mut admitted = seat_a.subscribe_admitted();
    tokio::select! {
        _ = &mut serving_a => unreachable!("serving never ends on its own"),
        waited = tokio::time::timeout(PROMPT, admitted.wait_for(|a| a.contains(&wire))) => {
            waited.expect("A's announce is admitted in time").expect("A's seat is alive");
        }
    }
    let opened = tokio::select! {
        _ = &mut serving_a => unreachable!("serving never ends on its own"),
        opened = tree.open_for_read(&engine_c, REL) => opened.expect("C hydrates by relay"),
    };
    assert!(
        opened.starts_with(tree.cache_root()),
        "C's body is in its cache root"
    );
    assert_eq!(std::fs::read(&opened).unwrap(), body);
    // A leaves as a foreground seat does: its serving stops and its announce
    // is withdrawn, or the nest would keep asking a connection nobody answers.
    drop(serving_a);
    drop(inbox_a);
    fauna_sync_engine::relay_seat::withdraw(&nest_a, &device_a)
        .await
        .expect("A's withdrawing announce reaches the nest");
    assert!(
        !store_holds_any(&nest.store, &keys).await,
        "relayed bytes of a metadata-only folder never rest on the nest"
    );

    // ── C serves alone, through the door, from its cache root ──
    let (asks, mut asked) = tokio::sync::mpsc::channel(4);
    let host = Arc::new(DoorHost {
        folder: nest.folder,
        asks,
    });
    let seat_c = RelaySeat::new();
    let mut admitted = seat_c.subscribe_admitted();
    let device_c = hex::encode(DEVICE_B);
    let answering = async {
        while let Some((ask, answered)) = asked.recv().await {
            engine_c.answer_serve_ask(ask).await;
            let _ = answered.send(());
        }
    };
    let mut serving_c = Box::pin(async {
        tokio::join!(
            seat_c.serve_hosts(
                &nest_c,
                &device_c,
                Arc::new(OneHost(Arc::clone(&host))),
                Duration::from_millis(50),
            ),
            answering,
        );
    });
    tokio::select! {
        _ = &mut serving_c => unreachable!("serving never ends on its own"),
        waited = tokio::time::timeout(PROMPT, admitted.wait_for(|a| a.contains(&wire))) => {
            waited.expect("C's announce is admitted in time").expect("C's seat is alive");
        }
    }
    let first_key = hex::encode(keys[0]);
    let status = tokio::select! {
        _ = &mut serving_c => unreachable!("serving never ends on its own"),
        status = hinted_get(&nest, &first_key) => status,
    };
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "the on-demand replica serves the body it hydrated"
    );

    // ── Freed back to a placeholder: C declines at once ──
    tree.record_placeholder(&engine_c, REL).unwrap();
    let started = Instant::now();
    let status = tokio::select! {
        _ = &mut serving_c => unreachable!("serving never ends on its own"),
        status = hinted_get(&nest, &first_key) => status,
    };
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert!(
        started.elapsed() < PROMPT,
        "a placeholder declines at once, never hydrates for the asker ({:?})",
        started.elapsed()
    );
}

/// A third device of the account, which syncs nothing and only opens files.
const READER_DEVICE: [u8; 32] = [0x1C; 32];

/// The reader of step (4) that is not an engine: a device that does not sync
/// the folder, on its own authenticated connection.
async fn reader_connection(nest: &Nest) -> Arc<fauna_client::NestClient> {
    let (_, reader) = fauna_nest::test_support::sync_engine_auth_client(
        &nest.url,
        &nest.bearer,
        OWNER_SECRET,
        &READER_DEVICE,
    );
    reader
        .connect()
        .await
        .expect("the reader's connection authenticates");
    reader
}

/// The by-manifest download of `REL` as the Media page's chunk walk runs it —
/// the production fetcher over the reader's session, the shared walk, the
/// owner's key — reading for `FOLDER` when `hinted`, as `MediaMachine::
/// download_file` reads, or plainly, as every reader did before.
async fn by_manifest_download(
    reader: &Arc<fauna_client::NestClient>,
    manifest_hex: &str,
    hinted: bool,
) -> anyhow::Result<Vec<u8>> {
    use fauna_core::file_download::{FileDownloadKeys, FolderHint, FolderHinted};
    let fetcher = fauna_client::NestPublicChunkFetcher::new(reader);
    let hint = FolderHinted::new(&fetcher, FolderHint::new(FOLDER));
    let walk: &dyn fauna_core::file_download::BlobFetcher = if hinted { &hint } else { &fetcher };
    let digest: [u8; 32] = hex::decode(manifest_hex).unwrap().try_into().unwrap();
    fauna_core::file_download::download_file_bytes_by_manifest(
        walk,
        &FileDownloadKeys::owner(BackupKey::derive(&OWNER_SECRET)),
        ContentHash::from_digest_raw(digest),
        None,
        REL,
    )
    .await
}

/// Seat A writes the file and keeps its only copy; returns the seat's engine,
/// connection and watch dir, the file's recorded manifest (hex) and its store
/// keys.
async fn written_by_seat_a(
    nest: &Nest,
) -> (
    SyncEngine,
    Arc<fauna_client::NestClient>,
    tempfile::TempDir,
    String,
    Vec<[u8; 32]>,
) {
    let (engine_a, nest_a, watch_a) = seat(nest, DEVICE_A).await;
    std::fs::write(watch_a.path().join(REL), body()).unwrap();
    engine_a
        .reconcile()
        .await
        .expect("reconcile stages the file");
    assert!(engine_a.upload_file(REL).await.expect("A records").recorded);
    let keys = engine_a.db().held_store_keys(REL).unwrap();
    let manifest = engine_a
        .db()
        .get_entry(REL)
        .unwrap()
        .and_then(|e| e.manifest_hash)
        .expect("A's row names its recorded manifest");
    (
        engine_a,
        nest_a,
        watch_a,
        hex::encode(manifest.digest()),
        keys,
    )
}

/// Seat A announcing and serving, admitted before it returns; dropping the
/// future ends the serving.
async fn serve_a<'a>(
    nest: &Nest,
    engine_a: &'a SyncEngine,
    nest_a: &'a Arc<fauna_client::NestClient>,
    seat_a: &'a Arc<RelaySeat>,
    device_a: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
    let wire = nest.folder.to_wire();
    let mut inbox_a = seat_a.register(&nest.folder, None);
    let mut serving: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> =
        Box::pin(async move {
            tokio::join!(seat_a.run(nest_a, device_a), inbox_a.serve(engine_a));
        });
    let mut admitted = seat_a.subscribe_admitted();
    tokio::select! {
        _ = &mut serving => unreachable!("serving never ends on its own"),
        waited = tokio::time::timeout(PROMPT, admitted.wait_for(|a| a.contains(&wire))) => {
            waited.expect("A's announce is admitted in time").expect("A's seat is alive");
        }
    }
    serving
}

/// **Step (4)'s by-manifest reader** (`file-sync.md` § Relay serving): a device
/// that does not sync a metadata-only folder opens one of its files through
/// the by-manifest download while the holder's seat runs — every chunk GET
/// carries the folder hint under the session bearer, the nest relays it from
/// the seat, and none of the file's chunks rests on the nest. The same
/// download without the hint — what every such reader sent before — is a miss
/// even with the seat serving.
#[tokio::test]
async fn a_by_manifest_download_opens_a_metadata_only_folders_file_from_its_seat() {
    let nest = start_test_nest().await;
    let (engine_a, nest_a, _watch_a, manifest, keys) = written_by_seat_a(&nest).await;
    let reader = reader_connection(&nest).await;

    assert!(
        by_manifest_download(&reader, &manifest, true)
            .await
            .is_err(),
        "with no announced seat the file's chunks are nowhere the nest can reach"
    );

    let seat_a = RelaySeat::new();
    let device_a = hex::encode(DEVICE_A);
    let mut serving = serve_a(&nest, &engine_a, &nest_a, &seat_a, &device_a).await;
    let plain = tokio::select! {
        _ = &mut serving => unreachable!("serving never ends on its own"),
        plain = by_manifest_download(&reader, &manifest, false) => plain,
    };
    assert!(
        plain.is_err(),
        "an unhinted read is a plain miss — the nest asks no seat for it"
    );
    let started = Instant::now();
    let opened = tokio::select! {
        _ = &mut serving => unreachable!("serving never ends on its own"),
        opened = by_manifest_download(&reader, &manifest, true) => opened,
    };
    assert_eq!(
        opened.expect("the hinted download opens the file by relay"),
        body(),
        "the reader reassembles A's file byte for byte"
    );
    assert!(
        started.elapsed() < PROMPT,
        "the download must not wait out a silent seat's deadline ({:?})",
        started.elapsed()
    );
    assert!(
        !store_holds_any(&nest.store, &keys).await,
        "relayed bytes of a metadata-only folder never rest on the nest"
    );
}

/// The same reader on a **sealed** set — its name rests blank on the nest, so
/// only the hint's address form (`folder_hash`) finds the row: a name-only
/// hint is a miss, while the by-manifest download, which sends both forms,
/// opens the file from the seat.
#[tokio::test]
async fn a_by_manifest_download_finds_a_sealed_sets_seat_by_the_hints_address() {
    let nest = start_test_nest().await;
    let (engine_a, nest_a, _watch_a, manifest, keys) = written_by_seat_a(&nest).await;
    let reader = reader_connection(&nest).await;
    let FolderRef::Local(folder_id) = nest.folder else {
        unreachable!("the fixture's folder is a local row")
    };
    nest.db
        .conn()
        .await
        .execute("UPDATE folders SET name = '' WHERE id = ?1", [folder_id])
        .expect("blank the set's name, as a sealed set rests");

    let seat_a = RelaySeat::new();
    let device_a = hex::encode(DEVICE_A);
    let mut serving = serve_a(&nest, &engine_a, &nest_a, &seat_a, &device_a).await;
    let first_key = hex::encode(keys[0]);
    let status = tokio::select! {
        _ = &mut serving => unreachable!("serving never ends on its own"),
        status = hinted_get(&nest, &first_key) => status,
    };
    assert_eq!(
        status,
        reqwest::StatusCode::NOT_FOUND,
        "a name-only hint cannot find a set whose name rests blank"
    );
    let opened = tokio::select! {
        _ = &mut serving => unreachable!("serving never ends on its own"),
        opened = by_manifest_download(&reader, &manifest, true) => opened,
    };
    assert_eq!(
        opened.expect("the hinted download finds the sealed set by its address"),
        body()
    );
    assert!(!store_holds_any(&nest.store, &keys).await);
}

/// The Media page's observer seam; the download notifies nothing.
struct Quiet;

impl fauna_media_machine::MediaObserver for Quiet {
    fn on_changed(&self) {}
}

/// The Media page's download of `REL` — the real `MediaMachine` the apps
/// build over their session (`build_media_machine_with_folder_keys`, no
/// custody resolver: an owner-only set), its `download_file` given the
/// recorded manifest, the folder and the owner key as the detail pane gives
/// them. The machine, not the test, decides to hint the walk.
async fn media_download(
    reader: &Arc<fauna_client::NestClient>,
    manifest_hex: &str,
) -> Result<Vec<u8>, fauna_media_machine::MediaApiError> {
    let media = fauna_media_machine::build_media_machine_with_folder_keys(
        Arc::clone(reader),
        Arc::new(Quiet),
        None,
    );
    media
        .download_file(
            manifest_hex.to_string(),
            None,
            FOLDER.to_string(),
            REL.to_string(),
            BackupKey::derive(&OWNER_SECRET).to_bytes().to_vec(),
        )
        .await
}

/// **The Media page opens a metadata-only folder's file** (`file-sync.md`
/// § Relay serving, step (4), the Media reader): an owner-only set's
/// engine-recorded file, downloaded through `MediaMachine::download_file`
/// while the holder's seat serves, reassembles byte for byte and rests no
/// chunk on the nest. Before the machine's blob-primary arm was retired the
/// nest answered that arm with the manifest's framed bytes and the download
/// refused with a content-hash mismatch, never reaching the hinted walk.
#[tokio::test]
async fn the_media_page_downloads_a_metadata_only_folders_file_from_its_seat() {
    let nest = start_test_nest().await;
    let (engine_a, nest_a, _watch_a, manifest, keys) = written_by_seat_a(&nest).await;
    let reader = reader_connection(&nest).await;

    let seat_a = RelaySeat::new();
    let device_a = hex::encode(DEVICE_A);
    let mut serving = serve_a(&nest, &engine_a, &nest_a, &seat_a, &device_a).await;
    let started = Instant::now();
    let opened = tokio::select! {
        _ = &mut serving => unreachable!("serving never ends on its own"),
        opened = media_download(&reader, &manifest) => opened,
    };
    assert_eq!(
        opened.expect("the Media download opens the file by relay"),
        body(),
        "the Media page reassembles A's file byte for byte"
    );
    assert!(
        started.elapsed() < PROMPT,
        "the download must not wait out a silent seat's deadline ({:?})",
        started.elapsed()
    );
    assert!(
        !store_holds_any(&nest.store, &keys).await,
        "relayed bytes of a metadata-only folder never rest on the nest"
    );
}

/// The Media download on a **sealed** set: the name rests blank, and the
/// machine's hint still finds the seat by the set's address.
#[tokio::test]
async fn the_media_page_finds_a_sealed_sets_seat_by_the_hints_address() {
    let nest = start_test_nest().await;
    let (engine_a, nest_a, _watch_a, manifest, keys) = written_by_seat_a(&nest).await;
    let reader = reader_connection(&nest).await;
    let FolderRef::Local(folder_id) = nest.folder else {
        unreachable!("the fixture's folder is a local row")
    };
    nest.db
        .conn()
        .await
        .execute("UPDATE folders SET name = '' WHERE id = ?1", [folder_id])
        .expect("blank the set's name, as a sealed set rests");

    let seat_a = RelaySeat::new();
    let device_a = hex::encode(DEVICE_A);
    let mut serving = serve_a(&nest, &engine_a, &nest_a, &seat_a, &device_a).await;
    let opened = tokio::select! {
        _ = &mut serving => unreachable!("serving never ends on its own"),
        opened = media_download(&reader, &manifest) => opened,
    };
    assert_eq!(
        opened.expect("the Media download finds the sealed set by its address"),
        body()
    );
    assert!(!store_holds_any(&nest.store, &keys).await);
}
