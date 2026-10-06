//! tier_3: [`SyncEngine::upload_file`]'s control-plane record-**success**
//! wiring — arm (a) of the leg-5 security-review finding entrusted to
//! the Windows shell-extension track.
//!
//! `fauna-sync-engine`'s own tests (`pull_remote_changes_test.rs`) pin
//! [`SyncEngine::commit_recorded_head`]'s *behavior* by calling it directly, and
//! `record_head_commit_wiring_test.rs`'s arm (b) pins the FAILURE half of the
//! wiring (an unconnected `nest_client`). Neither reaches the `Ok(_seq)` arm in
//! `upload_file`'s `record_change(...).await` match — every engine test in that
//! crate deliberately runs an unconnected `NestClient` (`download_file_bytes_test`
//! explains why: the byte-plane tests there are not about the control plane).
//! Proving the `Ok(_seq)` arm's own wiring — "does `upload_file` actually call
//! `commit_recorded_head` when the record genuinely lands" — needs a nest that
//! genuinely answers `fauna.sync.changes.record`, which means a real WS-RPC
//! connection. This file supplies exactly that, mirroring
//! `conformance_content_key_chunk_route.rs`'s `start_test_nest` (real axum
//! router + a real bound `TcpListener`, `NestClient::new` pointed at it).
//!
//! Proof: a `SyncEngine` with `folder = Some(..)` connected to a real
//! in-process nest uploads a file; the record's self-heal
//! (`fauna_client_sync::SyncClient::changes_record`) registers the
//! device and the record lands; `upload_file` reports `recorded = true`; and the
//! row's local `manifest_hash` equals the manifest the real nest's own
//! `changes.list` reports for that path — i.e. `commit_recorded_head` ran with
//! the correct hash off the real `Ok(_seq)` branch, not a stub.
//!
//! Regression pin: deleting the `commit_recorded_head` call from
//! `upload_file_inner`'s in-memory `Ok(_seq)` arm (the exact original
//! 2026-07-17 live bug) turns this test red — the row keeps the
//! seeded merge base instead of picking up the uploaded manifest.

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::sync::{SyncChangeRecordReply, SyncChangeRecordRequest};
use fauna_sync_engine::adaptive::AdaptiveConcurrency;
use fauna_sync_engine::db::SyncDb;
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::ignore::IgnoreMatcher;
use fauna_sync_engine::transfer::TransferPool;

mod common;

const OWNER_SECRET: [u8; 32] = [0x51; 32];
const DEVICE_ID: [u8; 32] = [0x09; 32];
const FOLDER: &str = "wiring_test";
/// The device the predecessor identity recorded from — not one of the
/// successor's, so its rows are never read as a successor device's own echoes.
const PREDECESSOR_DEVICE: [u8; 32] = [0x0F; 32];
/// The set nonce the fixture stores on `FOLDER`.
const SET_NONCE: [u8; 32] = [0x6E; 32];

/// Start a real in-process nest serving the auth + sync WS-RPC kinds and the
/// chunk-store HTTP routes over a real bound `TcpListener`, with the owner
/// registered and a folder pre-created (`changes.record`'s `owned_folder`
/// is a lookup, not an auto-create). Mirrors
/// `conformance_content_key_chunk_route.rs::start_test_nest`.
async fn start_test_nest(owner: [u8; 32]) -> (String, String) {
    let (url, token, _db) = start_test_nest_with_db(owner).await;
    (url, token)
}

/// [`start_test_nest`] also handing back the nest's store — for a test that
/// moves the set's stored nonce between records (the owner's
/// `FolderUpdateRequest::set_nonce`, applied directly).
async fn start_test_nest_with_db(owner: [u8; 32]) -> (String, String, Arc<CacheDb>) {
    start_test_nest_on(owner, nest_router()).await
}

/// The kinds the test nest serves: auth, discovery, sync, the file version
/// listing (what the take-over must leave a history row listed in), and the
/// folders kinds (the actor-roster read a reader admits a member's rows by).
fn nest_router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    fauna_nest::auth_handlers::register_auth_handlers(&mut b);
    fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
    fauna_nest::sync_handlers::register_sync_handlers(&mut b);
    fauna_nest::files_handlers::register_files_handlers(&mut b);
    fauna_nest::folder_handlers::register_folders_handlers(&mut b);
    b.build()
}

/// [`nest_router`] whose `fauna.sync.changes.record` answers a DELETE with the
/// seq held in the returned cell, recording nothing, whenever the cell is
/// non-zero — the record door as it stood before it learned the stored-nonce
/// condition (`delete-propagation.md` § Deletes propagate the same way), which
/// folded a delete re-signed under the live nonce into the tombstone signed
/// under a retired one. Every other request, and every request while the cell
/// is zero, goes to the real handler.
fn nest_router_swallowing_deletes() -> (RpcRouter, Arc<std::sync::atomic::AtomicI64>) {
    use std::sync::atomic::Ordering;
    const RECORD: &str = "fauna.sync.changes.record";
    let inner = Arc::new(nest_router());
    let swallow = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let mut b = RpcRouter::builder();
    for kind in inner.iter_kinds().map(str::to_owned).collect::<Vec<_>>() {
        let meta = inner.kind_meta(&kind).expect("listed kind");
        let (forbid_replay, default_deadline) = (meta.forbid_replay, meta.default_deadline);
        let kind: &'static str = Box::leak(kind.into_boxed_str());
        let (inner, swallow) = (Arc::clone(&inner), Arc::clone(&swallow));
        let handler: fauna_nest::rpc_router::RpcHandler = Box::new(move |state, actor, payload| {
            let seq = swallow.load(Ordering::SeqCst);
            if kind == RECORD
                && seq != 0
                && fauna_protocol::decode_strict::<SyncChangeRecordRequest>(&payload)
                    .is_ok_and(|req| req.change_type == "delete")
            {
                let reply = fauna_protocol::encode_canonical(&SyncChangeRecordReply {
                    seq,
                    extra: Default::default(),
                })
                .unwrap();
                return Box::pin(async move { Ok(bytes::Bytes::from(reply.to_vec())) });
            }
            (inner.kind_meta(kind).expect("listed kind").handler)(state, actor, payload)
        });
        b.add(
            kind,
            fauna_nest::rpc_router::RpcKindMeta {
                forbid_replay,
                default_deadline,
                handler,
            },
        );
    }
    (b.build(), swallow)
}

/// [`start_test_nest_with_db`] serving `rpc_router`.
async fn start_test_nest_on(
    owner: [u8; 32],
    rpc_router: RpcRouter,
) -> (String, String, Arc<CacheDb>) {
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
            set_nonce: Some(&SET_NONCE),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let token_store = Arc::new(TokenStore::new());
    // `owner` is already an actor id — re-deriving it through `from_secret`
    // minted for an actor with no `users` row, which every bearer door
    // refuses now that it asks the actor's standing.
    let http_token = token_store
        .insert(fauna_core::identity::ActorId(owner), 3600)
        .await;

    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        rpc_router: Arc::new(rpc_router),
        auth: fauna_nest::state::AuthState {
            token_store,
            ..Default::default()
        },
        ..AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), http_token, db)
}

/// An owner-only `backup_key` `SyncEngine` (no MLS/content-key complexity —
/// this test is about the control plane, not the seal path) bound to
/// `FOLDER`, whose `nest_client` is a **genuinely connected** `NestClient`
/// pointed at the real server — the piece `fauna-sync-engine`'s own tests never
/// build. Returns the engine plus a clone of that same connected client, so the
/// test can independently read back what the real nest recorded.
///
/// Production-shaped for writer-signed records: the engine signs every record
/// directly with the owner's identity key under the set's stored
/// [`SET_NONCE`], and reads under the same nonce and owner — the nest refuses
/// an unsigned record `signature_required`, and a reader refuses an unsigned
/// row. A test that wants another signing posture overrides it.
fn engine_and_control_client(
    dest_url: &str,
    http_token: &str,
    watch_path: std::path::PathBuf,
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    engine_with_db(
        dest_url,
        http_token,
        watch_path,
        SyncDb::open_in_memory().unwrap(),
    )
}

/// [`engine_and_control_client`] over a caller-supplied state DB — the
/// offline→reconnect arc (B2.5) spans two engine "sessions" over ONE on-disk
/// `SyncDb`, which an in-memory db cannot express.
fn engine_with_db(
    dest_url: &str,
    http_token: &str,
    watch_path: std::path::PathBuf,
    db: SyncDb,
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    engine_on_device(dest_url, http_token, watch_path, db, DEVICE_ID)
}

/// [`engine_with_db`] on another of the owner's devices — a reader whose
/// pull treats [`DEVICE_ID`]'s rows as a peer's, not as its own echoes.
fn engine_on_device(
    dest_url: &str,
    http_token: &str,
    watch_path: std::path::PathBuf,
    db: SyncDb,
    device_id: [u8; 32],
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    engine_sealing_under(
        dest_url,
        http_token,
        watch_path,
        db,
        device_id,
        BackupKey::derive(&OWNER_SECRET),
    )
}

/// [`engine_on_device`] whose owner key is `owner_key` — the bytes it uploads
/// seal under that root (a predecessor's era, modelled by its key).
fn engine_sealing_under(
    dest_url: &str,
    http_token: &str,
    watch_path: std::path::PathBuf,
    db: SyncDb,
    device_id: [u8; 32],
    owner_key: BackupKey,
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    let (engine_client, nest_client) =
        common::sync_engine_auth_client(dest_url, http_token, OWNER_SECRET, &device_id);

    let engine = SyncEngine::new(
        watch_path,
        db,
        engine_client,
        Some(FOLDER.to_string()),
        device_id,
        None, // mls
        None, // epoch_secret
        Some(owner_key.into()),
        None, // mls_group_id
        None, // content_keys
        fauna_core::format::ConflictPolicy::Auto,
        fauna_core::format::FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        Arc::clone(&nest_client),
        fauna_sync_engine::config::SyncMode::Sync,
    )
    // A full folder: the nest holds the bytes of this seat's own records.
    .with_metadata_only_residency(false);
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
    (engine, nest_client)
}

/// A nest's plant: strip the writer signature off the stored row `seq`, as a
/// hostile nest serving an unsigned row would. The nest refuses an unsigned
/// RECORD (`signature_required`), so an unsigned row can only reach a reader
/// this way — which is exactly the case the reader's refusal exists for.
async fn strip_signature(db: &CacheDb, seq: i64) {
    db.execute_batch(&format!(
        "UPDATE sync_changes SET signature = NULL, signer_key = NULL WHERE seq = {seq}"
    ))
    .await
    .unwrap();
}

/// The highest seq the real nest lists for `FOLDER` — the row the last record
/// landed as.
async fn newest_seq(nest_client: &Arc<fauna_client::NestClient>) -> i64 {
    fauna_client_sync::SyncClient::new(Arc::clone(nest_client))
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list")
        .changes
        .iter()
        .map(|c| c.seq)
        .max()
        .expect("a recorded row")
}

#[tokio::test]
async fn a_successful_record_commits_the_row_to_the_uploaded_manifest() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, token) = start_test_nest(owner).await;

    let watch = tempfile::tempdir().unwrap();
    let rel = "notes.txt";
    std::fs::write(watch.path().join(rel), b"a real record round trip").unwrap();

    let (engine, nest_client) = engine_and_control_client(&url, &token, watch.path().to_path_buf());

    // Force the WS auth handshake to complete up front (mirrors
    // `ws_challenge_bearer_roundtrip.rs`) rather than relying on the lazy
    // connect a bare `upload_file` call would trigger — without this, the
    // reconnect supervisor's first attempt doesn't land inside the record's
    // own request deadline and `record_change` times out never having sent
    // anything, which reads identically to a genuine record rejection.
    nest_client
        .connect()
        .await
        .expect("nest_client must reach Connected (WS auth handshake)");

    let outcome = engine.upload_file(rel).await.expect("upload_file");
    assert!(
        outcome.recorded,
        "the real nest must accept the record — the device self-heals via \
         `fauna.sync.register` on the first `device_unregistered` rejection, so no explicit pre-registration is needed"
    );

    // Read back what the real nest actually stored for this path, independent
    // of the engine's own local row.
    let ctl = fauna_client_sync::SyncClient::new(nest_client);
    let listed = ctl
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list");
    assert_eq!(
        listed.changes.len(),
        1,
        "the real nest recorded exactly one change"
    );
    let nest_manifest_hex = listed.changes[0]
        .manifest_hash
        .clone()
        .expect("a create change carries a manifest hash");

    let entry = engine
        .db()
        .get_entry(rel)
        .unwrap()
        .expect("the engine's local row exists");
    let local_manifest_hex = hex::encode(
        entry
            .manifest_hash
            .expect("a recorded upload must stamp the row's manifest_hash")
            .digest(),
    );

    assert_eq!(
        local_manifest_hex, nest_manifest_hex,
        "commit_recorded_head must stamp the row with the SAME manifest the \
         real nest recorded — proving upload_file's Ok(_seq) arm actually ran \
         commit_recorded_head (not a stub): deleting that call leaves the row \
         at whatever pre-upload merge base it started from instead"
    );
}

/// Writer-signed change records (`mls-group-key-material.md` § M2 → *Writer-
/// signed change records* (1)–(2)): an engine given a signer signs every record
/// it writes, the real nest verifies the signature against the set's STORED
/// nonce and keeps it, and the row it serves back verifies on the reader's
/// side under the same nonce — the whole round trip, signer to reader.
#[tokio::test]
async fn an_engine_with_a_signer_records_rows_that_verify_on_the_way_back() {
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner = owner_kp.actor_id().0;
    let (url, token) = start_test_nest(owner).await;

    let watch = tempfile::tempdir().unwrap();
    let rel = "signed.txt";
    std::fs::write(watch.path().join(rel), b"a signed record").unwrap();

    let (engine, nest_client) = engine_and_control_client(&url, &token, watch.path().to_path_buf());
    let engine = engine.with_change_signer(
        Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
            &owner_kp,
        )),
        Some(SET_NONCE),
    );
    nest_client.connect().await.expect("WS auth handshake");

    let outcome = engine.upload_file(rel).await.expect("upload_file");
    assert!(outcome.recorded, "the nest accepts the signed record");

    let listed = fauna_client_sync::SyncClient::new(nest_client)
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list");
    let row = &listed.changes[0];
    assert!(row.signature.is_some(), "the nest stored the signature");
    let certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    fauna_protocol::sync_writer_sig::verify_row(row, SET_NONCE, &certs, |a| *a == owner)
        .expect("the served row verifies under the set's nonce");
    assert!(
        fauna_protocol::sync_writer_sig::verify_row(row, [0; 32], &certs, |a| *a == owner).is_err(),
        "and under no other set's"
    );
}

/// The re-record leg of ruling (g) (`SyncEngine::rerecord_under_live_nonce`):
/// a head this device signed under a nonce the set has since retired — the
/// race loser's, here — is re-signed onto the live nonce over the same
/// manifest, and lands as the new head a reader verifies under the live
/// nonce; an unsigned head (a nest's plant — the nest no longer accepts an
/// unsigned record, so the fixture strips a landed row's signature) is not
/// this leg's to touch; a second run is a no-op. Seen red with the leg returning early (no re-record, the head keeps
/// verifying only under the retired nonce).
#[tokio::test]
async fn a_head_signed_under_a_retired_nonce_is_re_recorded_under_the_live_one() {
    const LIVE: [u8; 32] = [0x7A; 32];
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner = owner_kp.actor_id().0;
    let (url, token, db) = start_test_nest_with_db(owner).await;

    let watch = tempfile::tempdir().unwrap();
    std::fs::write(watch.path().join("loser.txt"), b"signed under the loser").unwrap();
    std::fs::write(watch.path().join("plain.txt"), b"served unsigned").unwrap();

    let (engine, nest_client) = engine_and_control_client(&url, &token, watch.path().to_path_buf());
    let signer = Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
        &owner_kp,
    ));
    nest_client.connect().await.expect("WS auth handshake");

    // The loser's head: signed under the nonce the set stored at the time.
    engine.set_change_signer(Some(Arc::clone(&signer)), Some(SET_NONCE));
    assert!(engine.upload_file("loser.txt").await.unwrap().recorded);
    // The unsigned head: recorded signed (the nest takes nothing else), then
    // served unsigned — the nest's plant.
    assert!(engine.upload_file("plain.txt").await.unwrap().recorded);
    strip_signature(&db, newest_seq(&nest_client).await).await;

    // The reconcile settles on LIVE: the nest's copy moves, and the engine
    // learns LIVE with the loser's nonce retired.
    db.update_folder_for_user(
        FOLDER,
        &owner,
        fauna_nest::db::FolderUpdate {
            set_nonce: Some(&LIVE),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    engine.set_change_signer(Some(signer), Some(LIVE));
    engine.set_retired_set_nonces(vec![SET_NONCE]);

    assert_eq!(engine.rerecord_under_live_nonce().await.unwrap(), 1);
    let rows = fauna_client_sync::SyncClient::new(Arc::clone(&nest_client))
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list")
        .changes;
    assert_eq!(rows.len(), 3, "two uploads + one re-record");
    let (first, rerecorded) = (&rows[0], &rows[2]);
    assert_eq!(rerecorded.path_hash, first.path_hash, "the loser's path");
    assert_eq!(
        rerecorded.manifest_hash, first.manifest_hash,
        "a re-sign, not a re-seal"
    );
    assert_eq!(rerecorded.is_resolution, Some(true), "no novel content");
    let certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    fauna_protocol::sync_writer_sig::verify_row(rerecorded, LIVE, &certs, |a| *a == owner)
        .expect("the new head verifies under the live nonce");
    assert!(
        fauna_protocol::sync_writer_sig::verify_row(first, LIVE, &certs, |a| *a == owner).is_err(),
        "the loser's own head never did"
    );

    assert_eq!(
        engine.rerecord_under_live_nonce().await.unwrap(),
        0,
        "the head is under the live nonce now"
    );
}

/// The re-record leg over a DELETE head (`writer-signed-change-records.md`
/// custody (g), and ruling (9)(e) finding (iii)): a door that answers the
/// re-signed delete with the retired tombstone's own seq has landed nothing,
/// so the leg counts nothing — it warns and retries at the next start —
/// and the real door, which lands it, is counted once. Seen red with the leg
/// counting any `Ok` (it reported 1 over the swallowing door).
#[tokio::test]
async fn a_delete_head_re_recorded_only_counts_when_the_door_lands_a_new_row() {
    const LIVE: [u8; 32] = [0x7B; 32];
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner = owner_kp.actor_id().0;
    let (router, swallow) = nest_router_swallowing_deletes();
    let (url, token, db) = start_test_nest_on(owner, router).await;

    let watch = tempfile::tempdir().unwrap();
    std::fs::write(watch.path().join("gone.txt"), b"deleted under the loser").unwrap();
    let (engine, nest_client) = engine_and_control_client(&url, &token, watch.path().to_path_buf());
    let signer = Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
        &owner_kp,
    ));
    nest_client.connect().await.expect("WS auth handshake");

    // The loser's delete: signed under the nonce the set stored at the time.
    engine.set_change_signer(Some(Arc::clone(&signer)), Some(SET_NONCE));
    assert!(engine.upload_file("gone.txt").await.unwrap().recorded);
    std::fs::remove_file(watch.path().join("gone.txt")).unwrap();
    assert!(engine.handle_delete("gone.txt").await.unwrap().recorded);
    let tombstone = newest_seq(&nest_client).await;

    db.update_folder_for_user(
        FOLDER,
        &owner,
        fauna_nest::db::FolderUpdate {
            set_nonce: Some(&LIVE),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    engine.set_change_signer(Some(signer), Some(LIVE));
    engine.set_retired_set_nonces(vec![SET_NONCE]);

    swallow.store(tombstone, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        engine.rerecord_under_live_nonce().await.unwrap(),
        0,
        "a door answering with the tombstone's own seq landed nothing"
    );
    assert_eq!(newest_seq(&nest_client).await, tombstone);

    swallow.store(0, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        engine.rerecord_under_live_nonce().await.unwrap(),
        1,
        "the real door lands the re-signed delete"
    );
    let rows = fauna_client_sync::SyncClient::new(Arc::clone(&nest_client))
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list")
        .changes;
    let head = rows.iter().max_by_key(|r| r.seq).unwrap();
    assert!(head.seq > tombstone, "a new head row");
    assert_eq!(head.change_type, "delete");
    let certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    fauna_protocol::sync_writer_sig::verify_row(head, LIVE, &certs, |a| *a == owner)
        .expect("the re-recorded delete verifies under the live nonce");
    assert_eq!(
        engine.rerecord_under_live_nonce().await.unwrap(),
        0,
        "the head is under the live nonce now"
    );
}

/// Re-sign stored row `seq` as `signer` — a predecessor's head as the set's
/// home nest serves it after a succession moved the corpus: the served author
/// stays the successor, the signature is over a statement naming `signer`
/// (`mls-group-key-material.md` § M2 → *Writer-signed change records*, ruling
/// (8)). The nest refuses such a record at ingest, so the fixture writes it in.
async fn resign_as(
    db: &CacheDb,
    nest_client: &Arc<fauna_client::NestClient>,
    seq: i64,
    signer: &ActorKeypair,
    nonce: [u8; 32],
) {
    let rows = fauna_client_sync::SyncClient::new(Arc::clone(nest_client))
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list")
        .changes;
    let mut row = rows.into_iter().find(|r| r.seq == seq).expect("the row");
    row.author_actor_id = Some(signer.actor_id().to_hex());
    let statement = fauna_protocol::sync_writer_sig::SignedChange::for_row(&row, nonce).unwrap();
    let key = fauna_protocol::sync_writer_sig::ChangeSigner::direct(signer);
    db.execute_batch(&format!(
        "UPDATE sync_changes SET signature = X'{}', signer_key = X'{}' WHERE seq = {seq}",
        hex::encode(key.sign_statement(&statement)),
        hex::encode(key.signer_key()),
    ))
    .await
    .unwrap();
}

/// Move the set nonce the nest stores on `FOLDER` — the custody write a
/// reconcile lands (a cut), or, in a fixture, the era a record is signed in.
async fn set_folder_nonce(db: &CacheDb, owner: [u8; 32], nonce: [u8; 32]) {
    db.update_folder_for_user(
        FOLDER,
        &owner,
        fauna_nest::db::FolderUpdate {
            set_nonce: Some(&nonce),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

/// The successor's engine as a cut leaves it (`writer-signed-change-records.md`
/// ruling (11)): it holds the predecessor's root, signs as the current identity
/// under `live`, and its reader holds `retired` in the lineage with the owner's
/// chain naming the predecessor — so the predecessor's rows under `retired`
/// judge history. Before a cut (`retired = None`) the same reader admits them.
fn successor_binding(
    engine: &mut SyncEngine,
    owner_kp: &ActorKeypair,
    predecessor: &ActorKeypair,
    predecessor_key: BackupKey,
    live: [u8; 32],
    retired: Option<[u8; 32]>,
) {
    let owner = owner_kp.actor_id().0;
    engine.set_predecessor_backup_keys(fauna_core::file_download::PredecessorSealKey::chain([(
        predecessor.actor_id(),
        predecessor_key,
    )]));
    engine.set_change_signer(
        Some(Arc::new(
            fauna_protocol::sync_writer_sig::ChangeSigner::direct(owner_kp),
        )),
        Some(live),
    );
    engine.set_retired_set_nonces(retired.into_iter().collect());
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(live),
        owner: Some(owner),
        account: Some(owner),
        account_predecessors: vec![predecessor.actor_id().0],
        retired_set_nonces: retired.into_iter().map(|n| (n, None)).collect(),
        ..Default::default()
    });
}

/// The take-over's ADOPTION arm (`writer-signed-change-records.md` ruling
/// (11)(d), with (8)(d) as amended; the ruling's (j)(3)): the device that
/// performed the cut and held no local state for the set adopts the nest's
/// history heads ONCE, under its adoption marker — each opened under its
/// signer's roots, re-sealed under the current one, recorded as the current
/// identity under the live nonce — and **the attack**: a predecessor-signed
/// head naming bytes the CURRENT root sealed opens nothing and is not carried.
/// The marker is spent on completion, so a re-pushed marker adopts nothing
/// more, and a fresh device with no marker never adopts at all.
#[tokio::test]
async fn a_device_with_no_state_adopts_the_nests_history_heads_once_under_its_marker() {
    const LIVE: [u8; 32] = [0x7A; 32];
    const PREDECESSOR_SECRET: [u8; 32] = [0x52; 32];
    const FRESH_DEVICE: [u8; 32] = [0x0D; 32];
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner = owner_kp.actor_id().0;
    let predecessor = ActorKeypair::from_secret(PREDECESSOR_SECRET);
    let predecessor_key = BackupKey::derive(&PREDECESSOR_SECRET);
    let (url, token, db) = start_test_nest_with_db(owner).await;

    // The predecessor's era: bytes sealed under ITS root.
    let inherited = b"sealed before the succession".to_vec();
    let watch_p = tempfile::tempdir().unwrap();
    std::fs::write(watch_p.path().join("inherited.txt"), &inherited).unwrap();
    std::fs::write(
        watch_p.path().join("later.txt"),
        b"served after the adoption",
    )
    .unwrap();
    let (pred_era, nest_client) = engine_sealing_under(
        &url,
        &token,
        watch_p.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
        PREDECESSOR_DEVICE,
        predecessor_key.clone(),
    );
    nest_client.connect().await.expect("WS auth handshake");
    assert!(
        pred_era
            .upload_file("inherited.txt")
            .await
            .unwrap()
            .recorded
    );
    let inherited_seq = newest_seq(&nest_client).await;
    // The plant: a current-root file under the predecessor's signature.
    let watch_c = tempfile::tempdir().unwrap();
    std::fs::write(watch_c.path().join("planted.txt"), b"created after it").unwrap();
    let (planter, planter_ctl) =
        engine_and_control_client(&url, &token, watch_c.path().to_path_buf());
    planter_ctl.connect().await.expect("WS auth handshake");
    assert!(planter.upload_file("planted.txt").await.unwrap().recorded);
    let planted_seq = newest_seq(&nest_client).await;
    resign_as(&db, &nest_client, inherited_seq, &predecessor, SET_NONCE).await;
    resign_as(&db, &nest_client, planted_seq, &predecessor, SET_NONCE).await;

    // The cut, on a device holding nothing for the set: the marker arrives
    // with the keys and is judged on arrival.
    set_folder_nonce(&db, owner, LIVE).await;
    let watch = tempfile::tempdir().unwrap();
    let (mut engine, engine_ctl) =
        engine_and_control_client(&url, &token, watch.path().to_path_buf());
    engine_ctl.connect().await.expect("WS auth handshake");
    successor_binding(
        &mut engine,
        &owner_kp,
        &predecessor,
        predecessor_key.clone(),
        LIVE,
        Some(SET_NONCE),
    );
    engine.set_adoption_marker(Some(LIVE));
    assert_eq!(
        engine.db().adoption_state(&LIVE).unwrap(),
        Some(fauna_sync_engine::db::AdoptionState::Begun),
        "no local state when the marker arrived"
    );

    assert_eq!(engine.rerecord_under_live_nonce().await.unwrap(), 1);
    let rows = fauna_client_sync::SyncClient::new(Arc::clone(&nest_client))
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list")
        .changes;
    assert_eq!(rows.len(), 3, "two records + one carried across");
    let (original, carried) = (
        rows.iter().find(|r| r.seq == inherited_seq).unwrap(),
        rows.last().unwrap(),
    );
    assert_eq!(carried.path_hash, original.path_hash, "the inherited path");
    assert_ne!(
        carried.manifest_hash, original.manifest_hash,
        "a re-seal, never a bare re-sign"
    );
    assert_eq!(carried.is_resolution, Some(true), "no novel content");
    let certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    fauna_protocol::sync_writer_sig::verify_row(carried, LIVE, &certs, |a| *a == owner)
        .expect("the carried head verifies under the live nonce as the current identity");
    let manifest = fauna_core::data::ContentHash::from_digest_raw(
        hex::decode(carried.manifest_hash.as_deref().unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let watch_r = tempfile::tempdir().unwrap();
    let (current_only, _) = engine_and_control_client(&url, &token, watch_r.path().to_path_buf());
    assert_eq!(
        current_only
            .download_file_bytes_by_manifest(manifest, None, "inherited.txt")
            .await
            .expect("the carried head opens under the current root alone"),
        inherited
    );
    let planted = rows.iter().find(|r| r.seq == planted_seq).unwrap();
    assert!(
        !rows
            .iter()
            .any(|r| r.seq > planted_seq && r.path_hash == planted.path_hash),
        "the planted head was not carried"
    );
    assert_eq!(
        engine.db().adoption_state(&LIVE).unwrap(),
        Some(fauna_sync_engine::db::AdoptionState::Spent),
        "the spend is recorded once the adoption completes"
    );

    // A predecessor's head the nest serves after the adoption — carriable
    // exactly as `inherited.txt` was.
    set_folder_nonce(&db, owner, SET_NONCE).await;
    assert!(pred_era.upload_file("later.txt").await.unwrap().recorded);
    let later_seq = newest_seq(&nest_client).await;
    set_folder_nonce(&db, owner, LIVE).await;
    resign_as(&db, &nest_client, later_seq, &predecessor, SET_NONCE).await;

    // The app re-pushes the keys, marker and all: spent, so nothing more.
    engine.set_adoption_marker(Some(LIVE));
    assert_eq!(
        engine.rerecord_under_live_nonce().await.unwrap(),
        0,
        "a spent marker never adopts again"
    );
    // A fresh device of the owner's, no marker: it vouches for nothing it
    // does not hold, and the nest's word is not adopted.
    let watch_f = tempfile::tempdir().unwrap();
    let (mut fresh, fresh_ctl) = engine_on_device(
        &url,
        &token,
        watch_f.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
        FRESH_DEVICE,
    );
    fresh_ctl.connect().await.expect("WS auth handshake");
    successor_binding(
        &mut fresh,
        &owner_kp,
        &predecessor,
        predecessor_key,
        LIVE,
        Some(SET_NONCE),
    );
    assert_eq!(
        fresh.rerecord_under_live_nonce().await.unwrap(),
        0,
        "no other device ever adopts history"
    );
    assert_eq!(newest_seq(&nest_client).await, later_seq);
}

/// The take-over's LOCAL-STATE arm (`writer-signed-change-records.md` ruling
/// (11)(d) and (e)): a device holding state for the set re-records only the
/// paths it holds, at the manifest its entry holds, under the roots its
/// persisted signer allows — and, after the cut on an owner-root set:
/// - the held predecessor head is re-sealed and recorded as the current
///   identity under the live nonce, the entry re-pointed at it;
/// - its history row stays a listed version (no supersede);
/// - a predecessor head the nest holds and the device does not stays history;
/// - an entry whose signer is unknown opens under no owner root: it is held
///   (`SIGNER_BOUND`), not carried, and never offered as new content;
/// - a second device holding the same path, passing after the first, skips it
///   (its newest record is now the first device's live head);
/// - a marker arriving on a device that held state is spent unspent.
#[tokio::test]
async fn a_device_holding_state_re_records_only_what_it_holds() {
    const LIVE: [u8; 32] = [0x7C; 32];
    const PREDECESSOR_SECRET: [u8; 32] = [0x53; 32];
    const SECOND_DEVICE: [u8; 32] = [0x0E; 32];
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner = owner_kp.actor_id().0;
    let predecessor = ActorKeypair::from_secret(PREDECESSOR_SECRET);
    let predecessor_key = BackupKey::derive(&PREDECESSOR_SECRET);
    let (url, token, db) = start_test_nest_with_db(owner).await;

    let watch_p = tempfile::tempdir().unwrap();
    let held_body = b"held on the successor's devices".to_vec();
    std::fs::write(watch_p.path().join("held.txt"), &held_body).unwrap();
    std::fs::write(watch_p.path().join("nest_only.txt"), b"never pulled").unwrap();
    std::fs::write(watch_p.path().join("unknown.txt"), b"signer unknown").unwrap();
    let (pred_era, nest_client) = engine_sealing_under(
        &url,
        &token,
        watch_p.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
        PREDECESSOR_DEVICE,
        predecessor_key.clone(),
    );
    nest_client.connect().await.expect("WS auth handshake");
    assert!(pred_era.upload_file("held.txt").await.unwrap().recorded);
    let held_seq = newest_seq(&nest_client).await;
    resign_as(&db, &nest_client, held_seq, &predecessor, SET_NONCE).await;

    // Before the cut both of the successor's devices pull: the predecessor's
    // row is admitted, folded, and its signer persisted on the entry.
    let watch = tempfile::tempdir().unwrap();
    let (mut engine, engine_ctl) =
        engine_and_control_client(&url, &token, watch.path().to_path_buf());
    engine_ctl.connect().await.expect("WS auth handshake");
    let watch_2 = tempfile::tempdir().unwrap();
    let (mut second, second_ctl) = engine_on_device(
        &url,
        &token,
        watch_2.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
        SECOND_DEVICE,
    );
    second_ctl.connect().await.expect("WS auth handshake");
    for device in [&mut engine, &mut second] {
        successor_binding(
            device,
            &owner_kp,
            &predecessor,
            predecessor_key.clone(),
            SET_NONCE,
            None,
        );
        device.pull_remote_changes().await.expect("pull");
        assert_eq!(
            device
                .db()
                .get_entry("held.txt")
                .unwrap()
                .unwrap()
                .head_signed_as,
            Some(predecessor.actor_id().0),
            "the fold persisted who the head was signed as"
        );
    }

    // Two more predecessor heads the devices never fold.
    assert!(
        pred_era
            .upload_file("nest_only.txt")
            .await
            .unwrap()
            .recorded
    );
    let nest_only_seq = newest_seq(&nest_client).await;
    resign_as(&db, &nest_client, nest_only_seq, &predecessor, SET_NONCE).await;
    assert!(pred_era.upload_file("unknown.txt").await.unwrap().recorded);
    let unknown_seq = newest_seq(&nest_client).await;
    resign_as(&db, &nest_client, unknown_seq, &predecessor, SET_NONCE).await;
    // An entry for `unknown.txt` at that head whose signer is not recorded.
    let unknown_row = fauna_client_sync::SyncClient::new(Arc::clone(&nest_client))
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list")
        .changes
        .into_iter()
        .find(|r| r.seq == unknown_seq)
        .unwrap();
    let unknown_manifest = fauna_core::data::ContentHash::from_digest_raw(
        hex::decode(unknown_row.manifest_hash.as_deref().unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    );
    std::fs::write(watch.path().join("unknown.txt"), b"signer unknown").unwrap();
    engine
        .db()
        .upsert_entry(
            "unknown.txt",
            None,
            None,
            Some(unknown_manifest),
            fauna_sync_engine::db::SyncState::Synced,
            0,
            0,
            14,
            1,
            None,
        )
        .unwrap();

    // The cut. The marker reaches a device that holds state: spent unspent.
    set_folder_nonce(&db, owner, LIVE).await;
    for device in [&mut engine, &mut second] {
        successor_binding(
            device,
            &owner_kp,
            &predecessor,
            predecessor_key.clone(),
            LIVE,
            Some(SET_NONCE),
        );
    }
    engine.set_adoption_marker(Some(LIVE));
    assert_eq!(
        engine.db().adoption_state(&LIVE).unwrap(),
        Some(fauna_sync_engine::db::AdoptionState::Spent),
        "a marker reaching a device that holds state adopts nothing"
    );

    assert_eq!(engine.rerecord_under_live_nonce().await.unwrap(), 1);
    let rows = fauna_client_sync::SyncClient::new(Arc::clone(&nest_client))
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list")
        .changes;
    let carried = rows.last().unwrap();
    let held_row = rows.iter().find(|r| r.seq == held_seq).unwrap();
    assert_eq!(carried.path_hash, held_row.path_hash, "the held path");
    assert_ne!(
        carried.manifest_hash, held_row.manifest_hash,
        "an owner-root head is re-sealed"
    );
    let certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    fauna_protocol::sync_writer_sig::verify_row(carried, LIVE, &certs, |a| *a == owner)
        .expect("the carried head verifies under the live nonce as the current identity");
    let entry = engine.db().get_entry("held.txt").unwrap().unwrap();
    assert_eq!(
        entry.manifest_hash.map(|m| hex::encode(m.digest())),
        carried.manifest_hash,
        "the entry is re-pointed at the head it recorded"
    );
    assert_eq!(entry.head_signed_as, Some(owner));
    for (seq, why) in [
        (
            nest_only_seq,
            "a head the device does not hold stays history",
        ),
        (unknown_seq, "a head whose signer is unknown is not carried"),
    ] {
        let hash = &rows.iter().find(|r| r.seq == seq).unwrap().path_hash;
        assert!(
            !rows.iter().any(|r| r.seq > seq && &r.path_hash == hash),
            "{why}"
        );
    }
    let versions = fauna_client_sync::SyncClient::new(Arc::clone(&nest_client))
        .versions_list(
            fauna_core::sync::path_hash("held.txt"),
            Some(FOLDER.to_string()),
        )
        .await
        .expect("versions.list")
        .versions;
    assert!(
        versions.iter().any(|v| v.version_num == held_seq),
        "the history row stays a listed version — no supersede"
    );

    // (e): the unknown-signer entry is held, and never offered as new content.
    assert!(engine.db().is_signer_bound("unknown.txt").unwrap());
    std::fs::write(watch.path().join("unknown.txt"), b"edited after the cut").unwrap();
    assert!(
        engine.upload_file("unknown.txt").await.is_err(),
        "a held path is never uploaded"
    );
    let newest = newest_seq(&nest_client).await;
    assert_eq!(newest, carried.seq, "nothing more landed");

    // The second device holds the same path; the first already took it.
    assert_eq!(
        second.rerecord_under_live_nonce().await.unwrap(),
        0,
        "another device took it first"
    );
    assert_eq!(newest_seq(&nest_client).await, newest);
}

/// A **bound** engine on `device_id`: no owner key, its bytes sealed under the
/// set's genesis content key and every record stamped with that generation —
/// [`engine_sealing_under`]'s content-keyed twin.
fn bound_engine_on(
    dest_url: &str,
    http_token: &str,
    watch_path: std::path::PathBuf,
    device_id: [u8; 32],
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    const GROUP_ID: [u8; 32] = [0x12; 32];
    const CONTENT_KEY: [u8; 32] = [0x7d; 32];
    let (engine_client, nest_client) =
        common::sync_engine_auth_client(dest_url, http_token, OWNER_SECRET, &device_id);
    let engine = SyncEngine::new(
        watch_path,
        SyncDb::open_in_memory().unwrap(),
        engine_client,
        Some(FOLDER.to_string()),
        device_id,
        None, // mls
        None, // epoch_secret
        None, // backup_key: the content-key seal path runs
        Some(GROUP_ID.to_vec()),
        Some(fauna_core::folder_keys::FolderContentKeys::genesis(
            CONTENT_KEY,
            1_000,
        )),
        fauna_core::format::ConflictPolicy::Auto,
        fauna_core::format::FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        Arc::clone(&nest_client),
        fauna_sync_engine::config::SyncMode::Sync,
    )
    .with_metadata_only_residency(false);
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
    (engine, nest_client)
}

/// The take-over over a CONTENT-KEYED head (`writer-signed-change-records.md`
/// ruling (11)(d), *how*): a predecessor's stamped head opens under its
/// generation whoever re-signs it, so it is **re-signed over the same
/// manifest** once the manifest has opened under that key — no chunk moved,
/// the stamp carried — recorded as the current identity under the live nonce,
/// and its history row stays a listed version (no supersede).
#[tokio::test]
async fn a_content_keyed_history_head_is_re_signed_over_the_same_manifest() {
    const LIVE: [u8; 32] = [0x7D; 32];
    const PREDECESSOR_SECRET: [u8; 32] = [0x54; 32];
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner = owner_kp.actor_id().0;
    let predecessor = ActorKeypair::from_secret(PREDECESSOR_SECRET);
    let (url, token, db) = start_test_nest_with_db(owner).await;

    let watch_p = tempfile::tempdir().unwrap();
    std::fs::write(watch_p.path().join("bound.txt"), b"under the content key").unwrap();
    let (pred_era, nest_client) = bound_engine_on(
        &url,
        &token,
        watch_p.path().to_path_buf(),
        PREDECESSOR_DEVICE,
    );
    nest_client.connect().await.expect("WS auth handshake");
    assert!(pred_era.upload_file("bound.txt").await.unwrap().recorded);
    let bound_seq = newest_seq(&nest_client).await;
    resign_as(&db, &nest_client, bound_seq, &predecessor, SET_NONCE).await;

    set_folder_nonce(&db, owner, LIVE).await;
    let watch = tempfile::tempdir().unwrap();
    let (engine, engine_ctl) = bound_engine_on(&url, &token, watch.path().to_path_buf(), DEVICE_ID);
    engine_ctl.connect().await.expect("WS auth handshake");
    engine.set_change_signer(
        Some(Arc::new(
            fauna_protocol::sync_writer_sig::ChangeSigner::direct(&owner_kp),
        )),
        Some(LIVE),
    );
    engine.set_retired_set_nonces(vec![SET_NONCE]);
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(LIVE),
        owner: Some(owner),
        account: Some(owner),
        account_predecessors: vec![predecessor.actor_id().0],
        retired_set_nonces: vec![(SET_NONCE, None)],
        ..Default::default()
    });
    engine.set_adoption_marker(Some(LIVE));

    assert_eq!(engine.rerecord_under_live_nonce().await.unwrap(), 1);
    let rows = fauna_client_sync::SyncClient::new(Arc::clone(&nest_client))
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list")
        .changes;
    let original = rows.iter().find(|r| r.seq == bound_seq).unwrap();
    let carried = rows.last().unwrap();
    assert!(carried.seq > bound_seq);
    assert_eq!(carried.path_hash, original.path_hash);
    assert_eq!(
        carried.manifest_hash, original.manifest_hash,
        "re-signed over the same manifest — no chunk moved"
    );
    assert_eq!(carried.content_key_version, original.content_key_version);
    assert!(original.content_key_version.is_some(), "a stamped head");
    let certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    fauna_protocol::sync_writer_sig::verify_row(carried, LIVE, &certs, |a| *a == owner)
        .expect("the re-signed head verifies under the live nonce as the current identity");
    let versions = fauna_client_sync::SyncClient::new(Arc::clone(&nest_client))
        .versions_list(
            fauna_core::sync::path_hash("bound.txt"),
            Some(FOLDER.to_string()),
        )
        .await
        .expect("versions.list")
        .versions;
    assert!(
        versions.iter().any(|v| v.version_num == bound_seq),
        "the history row stays a listed version — no supersede"
    );
}

/// The nest's rows for `FOLDER`, oldest first.
async fn listed_rows(
    nest_client: &Arc<fauna_client::NestClient>,
) -> Vec<fauna_protocol::sync::SyncChange> {
    fauna_client_sync::SyncClient::new(Arc::clone(nest_client))
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list")
        .changes
}

/// Share `FOLDER` with `member` as a `writer`, straight into the nest's store:
/// the set bound to a group, the owner and the member on the derived
/// channel's roster, the member's grant — what `fauna.folders.members.
/// list_actors` projects and a reader installs as its writer roster.
async fn seat_writer_member(db: &CacheDb, owner: [u8; 32], member: [u8; 32]) {
    const GROUP: [u8; 32] = [0x12; 32];
    assert!(
        db.set_folder_mls_group(FOLDER, &owner, Some(&GROUP))
            .await
            .unwrap()
    );
    let channel = fauna_mls::types::ChannelId::from_group_id(&GROUP).0;
    db.register_actor_channel(&owner, &channel).await.unwrap();
    db.register_actor_channel(&member, &channel).await.unwrap();
    db.set_folder_member_access(&channel, &member, "writer", None)
        .await
        .unwrap();
}

/// The take-over's DELETE arm (`succession-cut.md` ruling (11)(d), *Deletes
/// (finding (iii))*): on a shared set the cut makes a predecessor's admitted
/// delete history, so the member's older row it deleted resurfaces as the
/// head — and the take-over re-records that delete as the current identity's,
/// under the live nonce, where this device holds no live entry for the path.
/// The record lands over the retired-nonce tombstone and is counted once; a
/// path the device still holds live, and a delete over the owner's chain's own
/// row (an owner-only path), re-record nothing; a second pass adds nothing.
/// Seen red with `over_member` inverted (nothing re-recorded).
#[tokio::test]
async fn a_predecessors_delete_over_a_members_row_is_re_recorded_as_the_current_identitys() {
    const LIVE: [u8; 32] = [0x7E; 32];
    const PREDECESSOR_SECRET: [u8; 32] = [0x55; 32];
    const MEMBER_SECRET: [u8; 32] = [0x56; 32];
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner = owner_kp.actor_id().0;
    let predecessor = ActorKeypair::from_secret(PREDECESSOR_SECRET);
    let predecessor_key = BackupKey::derive(&PREDECESSOR_SECRET);
    let member = ActorKeypair::from_secret(MEMBER_SECRET);
    let (url, token, db) = start_test_nest_with_db(owner).await;

    // The predecessor's era: each path written, then deleted. `m.txt` and
    // `h.txt` were a member's; `o.txt` the owner's chain's own.
    let watch_p = tempfile::tempdir().unwrap();
    let (pred_era, nest_client) = engine_sealing_under(
        &url,
        &token,
        watch_p.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
        PREDECESSOR_DEVICE,
        predecessor_key.clone(),
    );
    nest_client.connect().await.expect("WS auth handshake");
    let mut written = std::collections::HashMap::new();
    for (rel, writer) in [
        ("m.txt", &member),
        ("h.txt", &member),
        ("o.txt", &predecessor),
    ] {
        std::fs::write(watch_p.path().join(rel), rel.as_bytes()).unwrap();
        assert!(pred_era.upload_file(rel).await.unwrap().recorded);
        let created = newest_seq(&nest_client).await;
        resign_as(&db, &nest_client, created, writer, SET_NONCE).await;
        std::fs::remove_file(watch_p.path().join(rel)).unwrap();
        assert!(pred_era.handle_delete(rel).await.unwrap().recorded);
        let tombstone = newest_seq(&nest_client).await;
        resign_as(&db, &nest_client, tombstone, &predecessor, SET_NONCE).await;
        written.insert(rel, (created, tombstone));
    }
    seat_writer_member(&db, owner, member.actor_id().0).await;

    // The cut, on a device that holds `h.txt` live and nothing else.
    set_folder_nonce(&db, owner, LIVE).await;
    let watch = tempfile::tempdir().unwrap();
    let (mut engine, engine_ctl) =
        engine_and_control_client(&url, &token, watch.path().to_path_buf());
    engine_ctl.connect().await.expect("WS auth handshake");
    successor_binding(
        &mut engine,
        &owner_kp,
        &predecessor,
        predecessor_key,
        LIVE,
        Some(SET_NONCE),
    );
    engine.refresh_reader_roster().await;
    let rows = listed_rows(&nest_client).await;
    let h_created = rows.iter().find(|r| r.seq == written["h.txt"].0).unwrap();
    let h_manifest = fauna_core::data::ContentHash::from_digest_raw(
        hex::decode(h_created.manifest_hash.as_deref().unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    );
    engine
        .db()
        .upsert_entry(
            "h.txt",
            None,
            None,
            Some(h_manifest),
            fauna_sync_engine::db::SyncState::Synced,
            0,
            0,
            5,
            1,
            None,
        )
        .unwrap();
    let before = newest_seq(&nest_client).await;

    assert_eq!(
        engine.rerecord_under_live_nonce().await.unwrap(),
        1,
        "the one delete over a member's row this device holds nothing for"
    );
    let rows = listed_rows(&nest_client).await;
    let landed: Vec<_> = rows.iter().filter(|r| r.seq > before).collect();
    assert_eq!(landed.len(), 1, "exactly one record landed: {landed:?}");
    let (m_tombstone, recorded) = (
        rows.iter().find(|r| r.seq == written["m.txt"].1).unwrap(),
        landed[0],
    );
    assert_eq!(recorded.path_hash, m_tombstone.path_hash, "`m.txt`'s path");
    assert_eq!(recorded.change_type, "delete");
    assert!(
        recorded.manifest_hash.is_none(),
        "a tombstone names no manifest"
    );
    let certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    fauna_protocol::sync_writer_sig::verify_row(recorded, LIVE, &certs, |a| *a == owner)
        .expect("the re-recorded delete verifies under the live nonce as the current identity");

    assert_eq!(
        engine.rerecord_under_live_nonce().await.unwrap(),
        0,
        "the path's newest record is the current identity's delete now"
    );
    assert_eq!(newest_seq(&nest_client).await, recorded.seq);
}

/// The take-over's TRIGGER on a running engine (`succession-cut.md` ruling
/// (11)(d), "run on every owner device when its reader's binding gains a
/// nonce"): an engine that pulled under the set's nonce folded a predecessor's
/// head; the cut reaches it as a binding that gains the live nonce with the
/// old one retired, and its next pull — with no explicit pass — re-records the
/// head it holds as the current identity under the live nonce. The owed marker
/// the gain wrote is clear once the pull's pass returned. Seen red with the
/// gain's `set_take_over_owed(true)` dropped (the head stays history), and
/// with the pass's clear dropped (the marker stays owed).
#[tokio::test]
async fn a_nonce_gained_by_a_running_engine_runs_the_take_over_on_its_next_pull() {
    const LIVE: [u8; 32] = [0x7F; 32];
    const PREDECESSOR_SECRET: [u8; 32] = [0x57; 32];
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner = owner_kp.actor_id().0;
    let predecessor = ActorKeypair::from_secret(PREDECESSOR_SECRET);
    let predecessor_key = BackupKey::derive(&PREDECESSOR_SECRET);
    let (url, token, db) = start_test_nest_with_db(owner).await;

    let watch_p = tempfile::tempdir().unwrap();
    std::fs::write(watch_p.path().join("held.txt"), b"folded before the cut").unwrap();
    let (pred_era, nest_client) = engine_sealing_under(
        &url,
        &token,
        watch_p.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
        PREDECESSOR_DEVICE,
        predecessor_key.clone(),
    );
    nest_client.connect().await.expect("WS auth handshake");
    assert!(pred_era.upload_file("held.txt").await.unwrap().recorded);
    let held_seq = newest_seq(&nest_client).await;
    resign_as(&db, &nest_client, held_seq, &predecessor, SET_NONCE).await;

    // The running engine pulls under the set's nonce and folds the head.
    let watch = tempfile::tempdir().unwrap();
    let (mut engine, engine_ctl) =
        engine_and_control_client(&url, &token, watch.path().to_path_buf());
    engine_ctl.connect().await.expect("WS auth handshake");
    successor_binding(
        &mut engine,
        &owner_kp,
        &predecessor,
        predecessor_key.clone(),
        SET_NONCE,
        None,
    );
    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(
        engine
            .db()
            .get_entry("held.txt")
            .unwrap()
            .unwrap()
            .head_signed_as,
        Some(predecessor.actor_id().0)
    );
    assert!(
        !engine.db().take_over_owed().unwrap(),
        "the first pull's pass ran and returned"
    );

    // The cut reaches the running engine: the binding gains LIVE, SET_NONCE
    // retired into the lineage. Nothing runs until the pull.
    set_folder_nonce(&db, owner, LIVE).await;
    successor_binding(
        &mut engine,
        &owner_kp,
        &predecessor,
        predecessor_key,
        LIVE,
        Some(SET_NONCE),
    );
    assert_eq!(newest_seq(&nest_client).await, held_seq);

    engine.pull_remote_changes().await.expect("pull");
    let rows = listed_rows(&nest_client).await;
    let held_row = rows.iter().find(|r| r.seq == held_seq).unwrap();
    let carried = rows.last().unwrap();
    assert!(carried.seq > held_seq, "the pull re-recorded the held head");
    assert_eq!(carried.path_hash, held_row.path_hash);
    let certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    fauna_protocol::sync_writer_sig::verify_row(carried, LIVE, &certs, |a| *a == owner)
        .expect("the carried head verifies under the live nonce as the current identity");
    let entry = engine.db().get_entry("held.txt").unwrap().unwrap();
    assert_eq!(
        entry.manifest_hash.map(|m| hex::encode(m.digest())),
        carried.manifest_hash,
        "the entry is re-pointed at the head it recorded"
    );
    assert!(
        !engine.db().take_over_owed().unwrap(),
        "the marker the gain owed is clear once the pass returned"
    );
}

/// **The reader half** (ruling (3) — "a record that does not verify is not a
/// record"), the planted-record negatives against a real nest. The owner's
/// laptop writes three rows the nest accepts: one signed under the set's
/// nonce; one nobody signed (a nest's plant — the nest refuses an unsigned
/// record `signature_required`, so the fixture strips the signature off a
/// landed row, as a hostile nest would); one signed under a DELETED
/// predecessor's nonce (what
/// a nest replaying a re-created same-name set's old rows serves — here the
/// stored nonce is moved for that one record). The owner's other device pulls:
/// only the signed row materializes; the plant and the predecessor's row are
/// absent, and the cursor passes them (a second pull re-lists nothing).
#[tokio::test]
async fn a_reader_applies_only_the_rows_that_verify() {
    const PREDECESSOR: [u8; 32] = [0x5E; 32];
    const READER_DEVICE: [u8; 32] = [0x0C; 32];
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner = owner_kp.actor_id().0;
    let (url, token, db) = start_test_nest_with_db(owner).await;
    let set_stored_nonce = |nonce: [u8; 32]| {
        let db = Arc::clone(&db);
        async move {
            db.update_folder_for_user(
                FOLDER,
                &owner,
                fauna_nest::db::FolderUpdate {
                    set_nonce: Some(&nonce),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
    };

    let watch = tempfile::tempdir().unwrap();
    for (rel, body) in [
        ("signed.txt", "the owner's"),
        ("planted.txt", "nobody's"),
        ("predecessor.txt", "a deleted set's"),
    ] {
        std::fs::write(watch.path().join(rel), body).unwrap();
    }
    let (writer, nest_client) = engine_and_control_client(&url, &token, watch.path().to_path_buf());
    nest_client.connect().await.expect("WS auth handshake");
    let signer = Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
        &owner_kp,
    ));
    writer.set_change_signer(Some(Arc::clone(&signer)), Some(SET_NONCE));
    assert!(writer.upload_file("signed.txt").await.unwrap().recorded);
    assert!(writer.upload_file("planted.txt").await.unwrap().recorded);
    strip_signature(&db, newest_seq(&nest_client).await).await;
    set_stored_nonce(PREDECESSOR).await;
    writer.set_change_signer(Some(signer), Some(PREDECESSOR));
    assert!(
        writer
            .upload_file("predecessor.txt")
            .await
            .unwrap()
            .recorded
    );
    set_stored_nonce(SET_NONCE).await;
    let last_seq = fauna_client_sync::SyncClient::new(Arc::clone(&nest_client))
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list")
        .changes
        .iter()
        .map(|c| c.seq)
        .max()
        .expect("three rows");

    let rwatch = tempfile::tempdir().unwrap();
    let (reader, nest) = engine_on_device(
        &url,
        &token,
        rwatch.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
        READER_DEVICE,
    );
    reader.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(owner),
        ..Default::default()
    });
    nest.connect().await.expect("reader connects");
    reader.pull_remote_changes().await.expect("pull");
    assert!(
        rwatch.path().join("signed.txt").exists(),
        "the signed row applies"
    );
    assert!(
        !rwatch.path().join("planted.txt").exists(),
        "an unsigned row is not a record"
    );
    assert!(
        !rwatch.path().join("predecessor.txt").exists(),
        "a row bound to another set's nonce is not a record"
    );
    assert_eq!(
        reader.db().get_anchor().unwrap(),
        last_seq,
        "the cursor passes the refused rows (the predecessor's is the last)"
    );
    assert_eq!(reader.pull_remote_changes().await.unwrap(), 0);
}

/// The negative: an engine signing under a nonce that is not the set's stored
/// one is refused `signature_invalid` — the record never lands.
#[tokio::test]
async fn a_record_signed_under_another_sets_nonce_is_refused() {
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let (url, token) = start_test_nest(owner_kp.actor_id().0).await;

    let watch = tempfile::tempdir().unwrap();
    let rel = "copied.txt";
    std::fs::write(watch.path().join(rel), b"a row copied from another set").unwrap();

    let (engine, nest_client) = engine_and_control_client(&url, &token, watch.path().to_path_buf());
    let engine = engine.with_change_signer(
        Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
            &owner_kp,
        )),
        Some([0x0D; 32]),
    );
    nest_client.connect().await.expect("WS auth handshake");

    let outcome = engine.upload_file(rel).await.expect("upload_file");
    assert!(!outcome.recorded, "a wrong-set binding never lands");
    let listed = fauna_client_sync::SyncClient::new(nest_client)
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list");
    assert!(listed.changes.is_empty());
}

/// The share leg's ROW-half retention (B2 — `p2p-shared-set-build.md` § *Build design — the
/// row half*): `record_change`'s `Ok(seq)` must write the row through to
/// `own_change_log`, so the share plane serves this replica's REAL recorded
/// rows — never rows synthesized from `sync_entries`. Proven against the real
/// nest: the retained row's seq, path-hash spelling and manifest equal what
/// the nest's own `changes.list` reports for the same record. Deleting the
/// retention call in `record_change` turns this red (nothing retained).
#[tokio::test]
async fn a_successful_record_retains_the_own_change_row() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, token) = start_test_nest(owner).await;

    let watch = tempfile::tempdir().unwrap();
    let rel = "notes.txt";
    std::fs::write(watch.path().join(rel), b"a real record round trip").unwrap();

    let (engine, nest_client) = engine_and_control_client(&url, &token, watch.path().to_path_buf());
    nest_client
        .connect()
        .await
        .expect("nest_client must reach Connected");

    let outcome = engine.upload_file(rel).await.expect("upload_file");
    assert!(outcome.recorded, "the real nest must accept the record");

    let ctl = fauna_client_sync::SyncClient::new(nest_client);
    let listed = ctl
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list");
    assert_eq!(listed.changes.len(), 1);
    let nest_row = &listed.changes[0];

    let retained = engine
        .db()
        .own_changes_since(0, 10)
        .expect("own_changes_since");
    assert_eq!(retained.len(), 1, "exactly the recorded row is retained");
    let row = &retained[0];
    assert_eq!(
        row.seq,
        Some(nest_row.seq),
        "the retained seq IS the nest-assigned one — never invented locally"
    );
    assert_eq!(row.path, rel);
    assert_eq!(
        row.path_hash, nest_row.path_hash,
        "one path-hash spelling on both sides (hex of fauna_core::sync::path_hash)"
    );
    assert_eq!(
        row.manifest_hash, nest_row.manifest_hash,
        "the retained manifest is the recorded one"
    );
    assert_eq!(row.change_type, "create");
    assert_eq!(
        row.author_actor_id,
        ActorKeypair::from_secret(OWNER_SECRET).actor_id_hex(),
        "own-authored by construction, in the wire's lowercase-hex spelling"
    );
}

/// The own-pending leg's full offline→reconnect arc (B2.5 — `p2p-shared-set-build.md`
/// § *Build design — the row half*, the own-pending bullet): authored in the
/// cabin (nothing listens on the nest URL — the upload fails at its first
/// network call, and the PENDING retention row is what remains), then the
/// same replica's next session re-drives the upload against the real nest.
/// The record's real `Ok(seq)` must upgrade the row IN PLACE: exactly ONE
/// retained row for the path, carrying the nest-assigned seq, with no
/// pending row left behind. Deleting the mint turns the first half red;
/// deleting `retain_own_change`'s pending retire leaves two rows and turns
/// the second half red.
#[tokio::test]
async fn an_offline_minted_pending_row_upgrades_in_place_at_the_real_ack() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, token) = start_test_nest(owner).await;

    let watch = tempfile::tempdir().unwrap();
    let rel = "notes.txt";
    std::fs::write(watch.path().join(rel), b"authored in the cabin").unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("sync.db");

    // Session 1 — the cabin. Port 9 (discard): the first network call
    // refuses, so the upload fails AFTER the local seal minted the row.
    {
        let (offline_engine, _) = engine_with_db(
            "http://127.0.0.1:9",
            &token,
            watch.path().to_path_buf(),
            SyncDb::open(&db_path).unwrap(),
        );
        offline_engine
            .upload_file(rel)
            .await
            .expect_err("no nest reachable — the upload must fail");
        let pending = offline_engine.db().own_pending_changes(10).unwrap();
        assert_eq!(
            pending.len(),
            1,
            "the offline upload minted the serveable pending row"
        );
        assert_eq!(pending[0].seq, None);
    }

    // Session 2 — reconnect: the SAME on-disk state DB, the real nest.
    let (engine, nest_client) = engine_with_db(
        &url,
        &token,
        watch.path().to_path_buf(),
        SyncDb::open(&db_path).unwrap(),
    );
    nest_client
        .connect()
        .await
        .expect("nest_client must reach Connected");

    let outcome = engine.upload_file(rel).await.expect("upload_file");
    assert!(outcome.recorded, "the real nest must accept the record");

    let ctl = fauna_client_sync::SyncClient::new(nest_client);
    let listed = ctl
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list");
    assert_eq!(listed.changes.len(), 1, "the nest holds exactly one record");
    let nest_seq = listed.changes[0].seq;

    let retained = engine.db().own_changes_since(0, 10).unwrap();
    assert_eq!(
        retained.len(),
        1,
        "ONE sequenced row for the path — the upgrade, never a duplicate"
    );
    assert_eq!(
        retained[0].seq,
        Some(nest_seq),
        "the upgraded row carries the nest-assigned seq"
    );
    assert_eq!(retained[0].path, rel);
    assert!(
        engine.db().own_pending_changes(10).unwrap().is_empty(),
        "the pending row retires in the same write that lands the sequenced one"
    );
}

/// The proven-head half of the dehydration gate, on the record-**success** path:
/// after a genuinely-recorded upload the file is provably synced end-to-end, so
/// `is_dehydration_safe` must return `true` — `commit_recorded_head` has to stamp
/// `recorded_content_hash` with the uploaded content, not just `manifest_hash`.
/// Red before that wiring: the row's `recorded_content_hash` stays NULL and the
/// gate fails closed. The mirror of `record_head_commit_wiring_test.rs`'s
/// `a_record_failed_edit_is_not_dehydration_safe` (which needs the real
/// connection to fail; this needs it to succeed).
#[tokio::test]
async fn a_successful_record_leaves_the_file_dehydration_safe() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, token) = start_test_nest(owner).await;

    let watch = tempfile::tempdir().unwrap();
    let rel = "notes.txt";
    std::fs::write(watch.path().join(rel), b"a real record round trip").unwrap();

    let (engine, nest_client) = engine_and_control_client(&url, &token, watch.path().to_path_buf());
    nest_client
        .connect()
        .await
        .expect("nest_client must reach Connected");

    let outcome = engine.upload_file(rel).await.expect("upload_file");
    assert!(outcome.recorded, "the real nest must accept the record");

    assert!(
        engine.is_dehydration_safe(rel),
        "a genuinely-recorded upload is provably synced end-to-end, so freeing \
         its bytes is lossless — commit_recorded_head must stamp \
         recorded_content_hash with the uploaded content for the gate to allow it"
    );
}
