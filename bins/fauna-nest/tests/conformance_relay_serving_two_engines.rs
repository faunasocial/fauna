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

/// Generous next to a healthy round trip, far short of the relay's 30 s
/// per-seat fetch deadline — so a read that was made to wait out a silent seat
/// fails the assertion rather than passing slowly.
const PROMPT: Duration = Duration::from_secs(15);

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
}

/// A real in-process nest serving the auth, sync and folder kinds and the
/// chunk routes, its relay wired as `build_app_state` wires it, with the
/// owner, both devices and a **metadata-only** `FOLDER` in place.
async fn start_test_nest() -> Nest {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlive the test; never deleted under test
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
        ..AppState::for_test(db)
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
    let (engine_client, nest_client) = fauna_nest::test_support::sync_engine_auth_client(
        &nest.url,
        &nest.bearer,
        OWNER_SECRET,
        &device_id,
    );
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
    (engine, nest_client, watch)
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
    let mut inbox_a = seat_a.register(&nest.folder);
    let mut inbox_b = seat_b.register(&nest.folder);
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
