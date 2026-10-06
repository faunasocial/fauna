//! tier_3: the post-succession corpus re-seal's **success** arm, against a nest
//! that genuinely answers `fauna.sync.changes.record`.
//!
//! `succession-aftermath.md` § Re-key scope's `BackupKey` corpus row: a succession
//! re-points corpus *ownership* in one nest-side transaction but moves no
//! **seal**, so every chunk already at rest stays sealed under the predecessor's
//! `BackupKey`. The re-seal is the write half that ends that window — and until
//! it completes the retired seed is client-only-resident key material guarding
//! user-irrecoverable data (the device-loss race the ratified blockquote there
//! describes).
//!
//! **Why this file exists rather than another `fauna-sync-engine` unit test.**
//! `SyncEngine::reseal_predecessor_sealed` marks an entry done only when the
//! change record LANDS — an unrecorded re-seal leaves the nest's change-log head
//! naming the predecessor-sealed manifest, so every other device would still
//! hydrate the retired-root copy and a completion check built on the observable
//! would license `sync-agent.md` bound (3) to drop the only keys that can open
//! it. Every engine test in that crate deliberately runs an **unconnected**
//! `NestClient` (`download_file_bytes_test`'s doc explains why), so the whole
//! marked/drained half of the pass is unreachable there: those tests pin the
//! byte plane and the fail-closed gate, and this one pins the arm where the
//! record succeeds. Same split, for the same reason, as
//! `record_head_commit_wiring_test.rs` ↔ `conformance_sync_engine_record_commit.rs`,
//! whose `start_test_nest` shape this mirrors.
//!
//! **What the fixture varies, and what it deliberately does not.** The two
//! engines differ in exactly one thing: the `BackupKey` their chunks seal under.
//! They share an actor, because the ownership re-point is a nest-side
//! transaction that is already built and pinned elsewhere (§ Re-key scope's
//! ratified blockquote) — re-deriving it here would test that transaction
//! instead of the seal move, which is the one variable this file is about.
//!
//! Regression pins:
//!   * deleting the nest-sourced arm leaves the successor's row owed forever —
//!     its bytes are on no disk, so the local force-upload leg cannot touch it;
//!   * dropping the record gate marks it while the nest head still names the
//!     predecessor-sealed manifest, which the `changes.list` read-back catches.

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::token_store::TokenStore;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_sync_engine::adaptive::AdaptiveConcurrency;
use fauna_sync_engine::db::{SyncDb, SyncState};
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::ignore::IgnoreMatcher;
use fauna_sync_engine::nest_client::SyncClient;
use fauna_sync_engine::transfer::TransferPool;

mod common;

/// The account's secret. One actor across the ceremony: what moves here is the
/// seal, not the ownership (see the module doc).
const OWNER_SECRET: [u8; 32] = [0x71; 32];
/// The identity seed the corpus was sealed under before the succession.
const PREDECESSOR_SEED: [u8; 32] = [0xA1; 32];
/// The successor's own seed — what every re-sealed chunk must land under.
const SUCCESSOR_SEED: [u8; 32] = [0xB2; 32];
const PREDECESSOR_DEVICE: [u8; 32] = [0x0A; 32];
const SUCCESSOR_DEVICE: [u8; 32] = [0x0B; 32];
const FOLDER: &str = "reseal_test";

/// Start a real in-process nest serving the auth + sync WS-RPC kinds and the
/// chunk-store HTTP routes, owner registered and folder pre-created.
/// Mirrors `conformance_sync_engine_record_commit.rs::start_test_nest`.
async fn start_test_nest(owner: [u8; 32]) -> (String, String) {
    let (url, token, _db) = start_test_nest_with_db(owner).await;
    (url, token)
}

/// [`start_test_nest`] also handing back the nest's store — for a fixture that
/// writes what the nest's own move leaves behind (a row re-signed as the
/// predecessor, its cert keyed under the moved author).
async fn start_test_nest_with_db(owner: [u8; 32]) -> (String, String, Arc<CacheDb>) {
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

    // The HTTP bearer is the registered owner's. `owner` is already an actor id;
    // re-deriving it through `from_secret` minted for an actor with no `users`
    // row, which every bearer door now refuses (it asks the actor's standing).
    let token_store = Arc::new(TokenStore::new());
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
            // The reader's writer-roster leg (`members.list_actors`): an
            // owner-only set answers `not_shared`, which is what lets a reader
            // REFUSE a row by no writer instead of holding below it.
            fauna_nest::folder_handlers::register_folders_handlers(&mut b);
            b.build()
        }),
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

/// Re-sign stored row `seq` as `signer` — what a predecessor's row is once a
/// succession moved the corpus (`mls-group-key-material.md` § M2 →
/// *Writer-signed change records*, ruling (8)): the nest serves it under the
/// successor's id, its signature still over a statement naming the
/// predecessor, and a delegated signer's cert keyed under the moved author.
/// The nest refuses such a record at ingest, so the fixture writes it in.
async fn resign_as_predecessor(
    db: &CacheDb,
    nest_client: &Arc<fauna_client::NestClient>,
    seq: i64,
    signer: &fauna_protocol::sync_writer_sig::ChangeSigner,
) {
    let rows = fauna_client_sync::SyncClient::new(Arc::clone(nest_client))
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list")
        .changes;
    let mut row = rows.into_iter().find(|r| r.seq == seq).expect("the row");
    let moved_author = fauna_core::hex32::decode(row.author_actor_id.as_deref().unwrap()).unwrap();
    row.author_actor_id = Some(hex::encode(signer.actor_id()));
    let statement =
        fauna_protocol::sync_writer_sig::SignedChange::for_row(&row, common::SET_NONCE).unwrap();
    db.execute_batch(&format!(
        "UPDATE sync_changes SET signature = X'{}', signer_key = X'{}' WHERE seq = {seq}",
        hex::encode(signer.sign_statement(&statement)),
        hex::encode(signer.signer_key()),
    ))
    .await
    .unwrap();
    if let Some(cert) = signer.carried_cert() {
        db.upsert_sync_signer_cert(
            &moved_author,
            &signer.signer_key(),
            &signer.actor_id(),
            &fauna_core::encoding::canonical_encode(cert).unwrap(),
        )
        .await
        .unwrap();
    }
}

/// The newest seq the nest lists for [`FOLDER`].
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

/// An owner-only engine sealing under `seed`'s `BackupKey`, with a genuinely
/// connected `NestClient` — the piece `fauna-sync-engine`'s own tests never
/// build, and the whole reason the marked/drained half of the pass lives here.
async fn connected_engine(
    dest_url: &str,
    http_token: &str,
    watch_path: std::path::PathBuf,
    seed: [u8; 32],
    device_id: [u8; 32],
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
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
        Some(BackupKey::derive(&seed).into()),
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
    // A writer engine signs every record it sends (the nest refuses an
    // unsigned one `signature_required`), directly under the account's key and
    // the set's stored nonce; its reader judges served rows against the same.
    // The seal seed varies per engine; the signing identity does not (one
    // actor across the ceremony — see the module doc).
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

    // Complete the WS auth handshake up front rather than relying on the lazy
    // connect a bare record would trigger — without this the reconnect
    // supervisor's first attempt does not land inside the record's own request
    // deadline, and the timeout reads identically to a genuine rejection
    // (`conformance_sync_engine_record_commit.rs` records the same trap).
    nest_client
        .connect()
        .await
        .expect("nest_client must reach Connected (WS auth handshake)");
    (engine, nest_client)
}

/// The thumbnail hex the nest's own change log currently names for `path`, or
/// `None` when the head names none. The [`nest_head_manifest`] twin, same join.
async fn nest_head_thumbnail(
    nest_client: Arc<fauna_client::NestClient>,
    path: &str,
) -> Option<String> {
    let listed = fauna_client_sync::SyncClient::new(nest_client)
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list");
    let want = hex::encode(fauna_core::sync::path_hash(path));
    listed
        .changes
        .iter()
        .rfind(|c| c.path_hash == want)
        .and_then(|c| c.thumbnail_hash.clone())
}

/// Hex manifest hash (the wire shape) → `ContentHash`.
fn manifest_hash_of(hex_str: &str) -> fauna_core::data::ContentHash {
    let bytes = hex::decode(hex_str).expect("manifest hex");
    let digest: [u8; 32] = bytes.try_into().expect("32-byte digest");
    fauna_core::data::ContentHash::from_digest_raw(digest)
}

/// Stamp a hand-seeded entry's head with the identity the nest's record of it
/// was signed as — what the fold persists for every head it lands
/// (`succession-cut.md` ruling (11)(d)); an entry with no signer recorded opens
/// under no owner root, so an unstamped seed leaves the pass nothing to re-seal.
///
/// Here that identity is the shared [`OWNER_SECRET`] actor: every engine in this
/// file signs as it and only the seal varies (the module doc), so the honest
/// stamp reads as the successor's current identity — not a predecessor's, whose
/// signature the change log does not carry. The predecessor-signed arm is
/// `a_successor_pulls_hydrates_and_reseals_a_predecessor_signed_corpus`'s.
fn stamp_head_signed_as_owner(engine: &SyncEngine, path: &str, manifest_hex: &str) {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    assert!(
        engine
            .db()
            .set_head_signed_as(path, &manifest_hash_of(manifest_hex), &owner)
            .expect("stamp the seeded head's signer"),
        "the seeded entry holds the head being stamped"
    );
}

/// The manifest hex the nest's own change log currently names for `path` — the
/// authoritative head, read independently of any engine's local row.
async fn nest_head_manifest(nest_client: Arc<fauna_client::NestClient>, path: &str) -> String {
    let listed = fauna_client_sync::SyncClient::new(nest_client)
        .changes_list(Some(FOLDER.to_string()), None, 0)
        .await
        .expect("changes.list");
    // Keyed on `path_hash`, not the plaintext `path`: `changes.list` does not
    // echo the plaintext back (it stores the hash plus the sealed path), so the
    // shared derivation is the only stable join here.
    let want = hex::encode(fauna_core::sync::path_hash(path));
    let seen: Vec<String> = listed
        .changes
        .iter()
        .map(|c| {
            format!(
                "seq={} path_hash={} manifest={:?} type={}",
                c.seq, c.path_hash, c.manifest_hash, c.change_type
            )
        })
        .collect();
    listed
        .changes
        .iter()
        .rfind(|c| c.path_hash == want)
        .and_then(|c| c.manifest_hash.clone())
        .unwrap_or_else(|| {
            panic!("no change names a manifest for {path} ({want}); nest returned: {seen:#?}")
        })
}

/// A successor re-seals a corpus it holds no bytes of, and the completion
/// observable drains — the state `sync-agent.md` bound (3) waits on.
///
/// The fresh-device shape end to end: the predecessor uploads a file (chunks
/// seal under its `BackupKey`, the record lands on a real nest), then the
/// successor — a *different* seed, an empty watch dir, nothing materialized —
/// runs the pass with the retired root offered only as a read candidate.
///
/// ⚠ The head is read back from the nest's own `changes.list`, not from the
/// engine's local row, and is then opened by an engine holding **no**
/// predecessor material. That is what makes this a statement about the corpus
/// rather than about the pass's bookkeeping: a re-seal that uploaded but never
/// recorded leaves the old hex here, and a re-seal that somehow sealed under the
/// retired root fails to open.
#[tokio::test]
async fn a_successor_reseals_a_corpus_it_holds_no_bytes_of() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, token) = start_test_nest(owner).await;

    let rel = "photos/inherited.bin";
    let original: Vec<u8> = (0..120_000u32)
        .map(|i| (i.wrapping_mul(37) % 251) as u8)
        .collect();

    // ── The predecessor's corpus, sealed under the retired identity's key ──
    let pred_watch = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(pred_watch.path().join("photos")).unwrap();
    std::fs::write(pred_watch.path().join(rel), &original).unwrap();
    let (pred_engine, pred_ctl) = connected_engine(
        &url,
        &token,
        pred_watch.path().to_path_buf(),
        PREDECESSOR_SEED,
        PREDECESSOR_DEVICE,
    )
    .await;
    let outcome = pred_engine.upload_file(rel).await.expect("upload_file");
    assert!(
        outcome.recorded,
        "the predecessor's corpus must genuinely land on the nest"
    );
    let pred_head = nest_head_manifest(Arc::clone(&pred_ctl), rel).await;

    // ── The successor: new seed, EMPTY watch dir, nothing materialized ──
    let succ_watch = tempfile::tempdir().unwrap();
    let (mut succ_engine, succ_ctl) = connected_engine(
        &url,
        &token,
        succ_watch.path().to_path_buf(),
        SUCCESSOR_SEED,
        SUCCESSOR_DEVICE,
    )
    .await;
    succ_engine.set_predecessor_backup_keys(vec![BackupKey::derive(&PREDECESSOR_SEED).into()]);

    // The row a fold would leave: the head is known, the bytes are not here.
    succ_engine
        .db()
        .upsert_entry(
            rel,
            None, // local_hash — nothing on this disk
            None,
            Some(manifest_hash_of(&pred_head)),
            SyncState::Placeholder,
            0,
            0,
            original.len() as i64,
            1,
            None,
        )
        .expect("seed the successor's placeholder row");
    stamp_head_signed_as_owner(&succ_engine, rel, &pred_head);

    assert_eq!(
        succ_engine
            .reseal_predecessor_sealed()
            .await
            .expect("re-seal pass"),
        1,
        "the successor re-seals the entry it holds no bytes of"
    );

    // ── The nest's head moved, and opens under the successor's key ALONE ──
    let new_head = nest_head_manifest(Arc::clone(&succ_ctl), rel).await;
    assert_ne!(
        new_head, pred_head,
        "the nest's change-log head must name the re-sealed manifest — an upload \
         whose record never landed leaves the predecessor-sealed head in place, \
         and every other device would keep hydrating the retired-root copy"
    );

    let reader_watch = tempfile::tempdir().unwrap();
    let (reader, _) = connected_engine(
        &url,
        &token,
        reader_watch.path().to_path_buf(),
        SUCCESSOR_SEED,
        [0x0C; 32],
    )
    .await;
    // No `set_predecessor_backup_keys`: this reader is the post-bound-(3) world,
    // holding nothing of the retired identity.
    let bytes = reader
        .download_file_bytes_by_manifest(manifest_hash_of(&new_head), None, rel)
        .await
        .expect("the re-sealed corpus opens with no predecessor material at all");
    assert_eq!(bytes, original, "same content, moved seal");

    // ── …and the completion observable drains ──
    assert!(
        succ_engine
            .db()
            .get_entry(rel)
            .unwrap()
            .unwrap()
            .current_root_sealed,
    );
    assert!(
        succ_engine
            .db()
            .list_pending_current_root_reseal()
            .unwrap()
            .is_empty(),
        "nothing owed — the statement bound (3) needs before it may drop the retired keys"
    );
    assert_eq!(
        succ_engine
            .reseal_predecessor_sealed()
            .await
            .expect("steady-state pass"),
        0,
        "and it terminates: the next pass is one indexed query"
    );
}

/// A successor's inherited **thumbnail** moves off the retired root with its
/// file, and the nest's head names the moved blob.
///
/// The record-gated half of the thumbnail MOVE (`identity-succession.md`
/// § Implementation status today, the raw-AEAD Library plane bullets). The
/// `fauna-sync-engine` unit tests pin its byte plane, but two of its properties
/// are only observable where a change record actually lands, and both matter:
/// the **nest's head** must name the moved thumbnail (a head still naming the
/// predecessor-sealed blob leaves every other device fetching a copy that goes
/// dark the moment `sync-agent.md` bound (3) drops the retired keys), and the
/// row's **cached pointer** must follow it (a stale cache sends the next pass
/// chasing the pre-move blob). Both stamps sit behind the record's success arm,
/// which an unconnected `NestClient` can never reach.
///
/// ⚠ The thumbnail is seeded as a Library blob under the retired key rather than
/// produced by an image upload, deliberately: what is under test is the *move*,
/// whose input is an already-sealed thumbnail and whose trigger is "this build
/// regenerated nothing". Driving it through the thumbnailer would make the test
/// depend on the `process_media` feature being compiled in — the very condition
/// the move exists to survive.
#[tokio::test]
async fn a_successors_inherited_thumbnail_moves_with_its_file() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, token) = start_test_nest(owner).await;

    let rel = "photos/with-thumb.bin";
    let original: Vec<u8> = (0..90_000u32)
        .map(|i| (i.wrapping_mul(29) % 251) as u8)
        .collect();
    let thumb_pixels: Vec<u8> = (0..2_048u32).map(|i| (i % 193) as u8).collect();

    let pred_key = BackupKey::derive(&PREDECESSOR_SEED);
    let succ_key = BackupKey::derive(&SUCCESSOR_SEED);

    // ── The predecessor's corpus + its thumbnail, both under the retired key ──
    let pred_watch = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(pred_watch.path().join("photos")).unwrap();
    std::fs::write(pred_watch.path().join(rel), &original).unwrap();
    let (pred_engine, pred_ctl) = connected_engine(
        &url,
        &token,
        pred_watch.path().to_path_buf(),
        PREDECESSOR_SEED,
        PREDECESSOR_DEVICE,
    )
    .await;
    assert!(
        pred_engine
            .upload_file(rel)
            .await
            .expect("upload_file")
            .recorded,
        "the predecessor's corpus must genuinely land on the nest"
    );
    let pred_head = nest_head_manifest(Arc::clone(&pred_ctl), rel).await;

    // The thumbnail the predecessor's build recorded, sealed under its bare key.
    let blob_client = SyncClient::new(
        Arc::new(fauna_client::AuthClient::with_bearer_source(
            url.clone(),
            ActorKeypair::from_secret(OWNER_SECRET),
            Arc::new(StaticBearer(token.clone())) as Arc<dyn BearerSource>,
            reqwest::Client::new(),
        )),
        &PREDECESSOR_DEVICE,
    );
    let sealed_thumb = fauna_media::pipeline::seal_rendered_thumbnail(
        &thumb_pixels,
        &fauna_media::audience::Audience::Library {
            backup_key: pred_key.clone(),
        },
    );
    let pred_thumb_hex = blob_client
        .upload_blob_multipart(&sealed_thumb.sidecar.to_dag_cbor(), &sealed_thumb.bytes)
        .await
        .expect("seed the predecessor's thumbnail blob");

    // ── The successor: new seed, EMPTY watch dir, nothing materialized ──
    let succ_watch = tempfile::tempdir().unwrap();
    let (mut succ_engine, succ_ctl) = connected_engine(
        &url,
        &token,
        succ_watch.path().to_path_buf(),
        SUCCESSOR_SEED,
        SUCCESSOR_DEVICE,
    )
    .await;
    succ_engine.set_predecessor_backup_keys(vec![pred_key.clone().into()]);
    succ_engine
        .db()
        .upsert_entry(
            rel,
            None, // local_hash — nothing on this disk
            None,
            Some(manifest_hash_of(&pred_head)),
            SyncState::Placeholder,
            0,
            0,
            original.len() as i64,
            1,
            None,
        )
        .expect("seed the successor's placeholder row");
    stamp_head_signed_as_owner(&succ_engine, rel, &pred_head);
    // The pointer a fold would have cached from the change log.
    succ_engine
        .db()
        .set_thumbnail_hash(rel, Some(&pred_thumb_hex))
        .expect("seed the recorded head's thumbnail pointer");

    assert_eq!(
        succ_engine
            .reseal_predecessor_sealed()
            .await
            .expect("re-seal pass"),
        1,
        "the successor re-seals the entry it holds no bytes of"
    );

    // ── The nest's head names a DIFFERENT thumbnail, and it opens under the
    //    successor's key with no predecessor material at all ──
    let new_thumb_hex = nest_head_thumbnail(Arc::clone(&succ_ctl), rel)
        .await
        .expect(
            "the re-recorded head must still name a thumbnail — dropping the pointer is the bug",
        );
    assert_ne!(
        new_thumb_hex, pred_thumb_hex,
        "the head must name the MOVED thumbnail; naming the predecessor-sealed one \
         leaves every other device fetching a blob that goes dark at bound (3)"
    );
    let moved = blob_client
        .download_blob(&manifest_hash_of(&new_thumb_hex))
        .await
        .expect("the moved thumbnail is fetchable by the hash the head names");
    assert_eq!(
        fauna_core::crypto::decrypt_backup_chunk(&succ_key, &moved)
            .expect("…and opens under the SUCCESSOR's bare key alone"),
        thumb_pixels,
        "…carrying the original pixels — a move, not a re-render"
    );
    assert!(
        fauna_core::crypto::decrypt_backup_chunk(&pred_key, &moved).is_err(),
        "the retired root must no longer open it, or the seed stays load-bearing"
    );

    // ── …and the row's cached pointer followed the move ──
    assert_eq!(
        succ_engine
            .db()
            .get_entry(rel)
            .unwrap()
            .unwrap()
            .thumbnail_hash,
        Some(new_thumb_hex),
        "a stale cache would send the next pass chasing the pre-move blob"
    );
}

/// The predecessor's era, as a succession leaves it on the nest: two files
/// sealed under the predecessor's root, their rows signed by the predecessor —
/// one directly by its identity key, one by a machine principal it certified —
/// and served moved (author re-pointed to the successor, the delegated cert
/// keyed under it). Returns each file's path and bytes.
async fn record_predecessor_signed_corpus(
    url: &str,
    token: &str,
    db: &CacheDb,
    predecessor: &ActorKeypair,
) -> [(&'static str, Vec<u8>); 2] {
    let files = [
        ("photos/direct.bin", vec![0x31u8; 70_000]),
        ("photos/delegated.bin", vec![0x32u8; 90_000]),
    ];
    let pred_watch = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(pred_watch.path().join("photos")).unwrap();
    let (pred_engine, ctl) = connected_engine(
        url,
        token,
        pred_watch.path().to_path_buf(),
        PREDECESSOR_SEED,
        PREDECESSOR_DEVICE,
    )
    .await;
    let mut seqs = Vec::new();
    for (rel, body) in &files {
        std::fs::write(pred_watch.path().join(rel), body).unwrap();
        assert!(pred_engine.upload_file(rel).await.unwrap().recorded);
        seqs.push(newest_seq(&ctl).await);
    }
    let direct = fauna_protocol::sync_writer_sig::ChangeSigner::direct(predecessor);
    let principal = ActorKeypair::from_secret([0xC3; 32]);
    let grant = fauna_client_sync::build_principal_grant(predecessor, &principal.actor_id().0)
        .expect("grant builds");
    let delegated = fauna_protocol::sync_writer_sig::ChangeSigner::delegated(
        predecessor.actor_id().0,
        principal.signing_key().clone(),
        grant,
    );
    resign_as_predecessor(db, &ctl, seqs[0], &direct).await;
    resign_as_predecessor(db, &ctl, seqs[1], &delegated).await;
    files
}

/// **Ruling (8)(f), end to end** (`mls-group-key-material.md` § M2 →
/// *Writer-signed change records*): a successor device whose cursor passed
/// the inherited rows BEFORE the predecessor reached it — each refused for
/// want of a writer, its skip noted — shows the inherited corpus once the
/// predecessor is proven and its root paired: the head re-judge folds the
/// heads at the cursor the device already had, with nothing replayed.
#[tokio::test]
async fn a_successor_whose_cursor_passed_the_inherited_rows_shows_them_once_the_predecessor_arrives()
 {
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner = owner_kp.actor_id().0;
    let (url, token, db) = start_test_nest_with_db(owner).await;
    let predecessor = ActorKeypair::from_secret(PREDECESSOR_SEED);
    let files = record_predecessor_signed_corpus(&url, &token, &db, &predecessor).await;

    // ── The device meets the rows first: no predecessor proven, no root ──
    let watch = tempfile::tempdir().unwrap();
    let (mut late, _) = connected_engine(
        &url,
        &token,
        watch.path().to_path_buf(),
        SUCCESSOR_SEED,
        [0x0E; 32],
    )
    .await;
    late.pull_remote_changes().await.expect("pull");
    let passed = late.db().get_anchor().unwrap();
    assert!(passed > 0, "the cursor passed the inherited rows");
    for (rel, _) in &files {
        assert!(!watch.path().join(rel).exists(), "{rel} refused");
    }

    // ── The predecessor reaches it: the id proven, the root paired ──
    late.set_predecessor_backup_keys(fauna_core::file_download::PredecessorSealKey::chain([(
        predecessor.actor_id(),
        BackupKey::derive(&PREDECESSOR_SEED),
    )]));
    late.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(common::SET_NONCE),
        owner: Some(owner),
        account: Some(owner),
        account_predecessors: vec![predecessor.actor_id().0],
        ..Default::default()
    });
    late.pull_remote_changes().await.expect("pull");
    for (rel, body) in &files {
        assert_eq!(
            std::fs::read(watch.path().join(rel)).expect("the inherited head materialises"),
            *body,
            "{rel}: folded by the head re-judge"
        );
    }
    assert_eq!(
        late.db().get_anchor().unwrap(),
        passed,
        "by heads at the cursor it already had — nothing replayed"
    );
    assert!(
        !late.db().head_rejudge_owed().unwrap(),
        "the pass completed"
    );
}

/// **The restore re-seal, against a real byte plane** (ruling (8)(d), the
/// restore sentence — the byte seam the sync agent's Explorer restore routes
/// to its running engine): a version the predecessor signed, resting under
/// the predecessor's root, is re-sealed into a NEW manifest that a device
/// holding the current root alone opens, and the nest's head does not move —
/// the restore's record is the caller's. **The attack**: the successor's own
/// file, named under the predecessor's signature, opens under no root that
/// signature may reach and is refused.
#[tokio::test]
async fn a_successor_reseals_one_inherited_version_for_a_restore_and_refuses_the_planted_one() {
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner = owner_kp.actor_id().0;
    let (url, token, db) = start_test_nest_with_db(owner).await;
    let predecessor = ActorKeypair::from_secret(PREDECESSOR_SEED);
    let files = record_predecessor_signed_corpus(&url, &token, &db, &predecessor).await;
    let (rel, body) = &files[0];

    let succ_watch = tempfile::tempdir().unwrap();
    let (mut succ, succ_ctl) = connected_engine(
        &url,
        &token,
        succ_watch.path().to_path_buf(),
        SUCCESSOR_SEED,
        SUCCESSOR_DEVICE,
    )
    .await;
    succ.set_predecessor_backup_keys(fauna_core::file_download::PredecessorSealKey::chain([(
        predecessor.actor_id(),
        BackupKey::derive(&PREDECESSOR_SEED),
    )]));
    succ.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(common::SET_NONCE),
        owner: Some(owner),
        account: Some(owner),
        account_predecessors: vec![predecessor.actor_id().0],
        ..Default::default()
    });

    // A reader holding the current root alone — no predecessor material.
    let reader_watch = tempfile::tempdir().unwrap();
    let (reader, _) = connected_engine(
        &url,
        &token,
        reader_watch.path().to_path_buf(),
        SUCCESSOR_SEED,
        [0x0C; 32],
    )
    .await;

    let inherited = nest_head_manifest(Arc::clone(&succ_ctl), rel).await;
    assert!(
        reader
            .download_file_bytes_by_manifest(manifest_hash_of(&inherited), None, rel)
            .await
            .is_err(),
        "the fixture: the current root does not open the inherited version"
    );
    let resealed = succ
        .reseal_inherited_version(
            rel,
            manifest_hash_of(&inherited),
            Some(predecessor.actor_id().0),
        )
        .await
        .expect("the predecessor's root opens what it sealed");
    assert_ne!(resealed.manifest_hash, manifest_hash_of(&inherited));
    assert_eq!(resealed.size_bytes, body.len() as i64);
    assert_eq!(
        reader
            .download_file_bytes_by_manifest(resealed.manifest_hash, None, rel)
            .await
            .expect("opens under the current root alone"),
        *body
    );
    assert_eq!(
        nest_head_manifest(Arc::clone(&succ_ctl), rel).await,
        inherited,
        "the re-seal records nothing — the restore's record is the caller's"
    );

    // ── The attack: bytes the CURRENT root sealed, under the predecessor's
    //    signature ──
    let own = "photos/after-the-ceremony.bin";
    std::fs::create_dir_all(succ_watch.path().join("photos")).unwrap();
    std::fs::write(succ_watch.path().join(own), vec![0x44u8; 60_000]).unwrap();
    assert!(succ.upload_file(own).await.unwrap().recorded);
    let planted = nest_head_manifest(Arc::clone(&succ_ctl), own).await;
    let err = succ
        .reseal_inherited_version(
            own,
            manifest_hash_of(&planted),
            Some(predecessor.actor_id().0),
        )
        .await
        .expect_err("a predecessor's signature must not open a current-root version");
    assert!(
        format!("{err:#}").contains(fauna_core::nest_reseal::RESTORE_INHERITED_UNOPENABLE),
        "refused with the reason: {err:#}"
    );
}

/// **The engine's half of ruling (8), end to end** (`mls-group-key-material.md`
/// § M2 → *Writer-signed change records*): a successor's engine on a FRESH
/// device pulls, hydrates and re-seals a corpus its predecessor SIGNED — one
/// row signed directly by the predecessor's identity key, one by a machine
/// principal the predecessor certified — served moved (author re-pointed to
/// the successor, the delegated cert keyed under it). Without the predecessor
/// in its binding the same engine materializes none of it.
#[tokio::test]
async fn a_successor_pulls_hydrates_and_reseals_a_predecessor_signed_corpus() {
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let owner = owner_kp.actor_id().0;
    let (url, token, db) = start_test_nest_with_db(owner).await;
    let predecessor = ActorKeypair::from_secret(PREDECESSOR_SEED);
    let files = record_predecessor_signed_corpus(&url, &token, &db, &predecessor).await;

    // ── A binding that names no predecessor materializes none of it ──
    let blind_watch = tempfile::tempdir().unwrap();
    let (blind, _) = connected_engine(
        &url,
        &token,
        blind_watch.path().to_path_buf(),
        SUCCESSOR_SEED,
        [0x0D; 32],
    )
    .await;
    blind.pull_remote_changes().await.expect("pull");
    for (rel, _) in &files {
        assert!(!blind_watch.path().join(rel).exists(), "{rel} refused");
    }

    // ── The successor's fresh device: the predecessor bound and paired ──
    let succ_watch = tempfile::tempdir().unwrap();
    let (mut succ, succ_ctl) = connected_engine(
        &url,
        &token,
        succ_watch.path().to_path_buf(),
        SUCCESSOR_SEED,
        SUCCESSOR_DEVICE,
    )
    .await;
    succ.set_predecessor_backup_keys(fauna_core::file_download::PredecessorSealKey::chain([(
        predecessor.actor_id(),
        BackupKey::derive(&PREDECESSOR_SEED),
    )]));
    succ.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(common::SET_NONCE),
        owner: Some(owner),
        account: Some(owner),
        account_predecessors: vec![predecessor.actor_id().0],
        ..Default::default()
    });
    succ.pull_remote_changes().await.expect("pull");
    for (rel, body) in &files {
        assert_eq!(
            std::fs::read(succ_watch.path().join(rel)).expect("hydrated"),
            *body,
            "{rel}: pulled and opened under the predecessor's root"
        );
    }

    // ── …and re-sealed: the nest's heads move and open under the current
    //    root alone ──
    let before: Vec<String> = {
        let mut v = Vec::new();
        for (rel, _) in &files {
            v.push(nest_head_manifest(Arc::clone(&succ_ctl), rel).await);
        }
        v
    };
    assert_eq!(
        succ.reseal_predecessor_sealed()
            .await
            .expect("re-seal pass"),
        files.len()
    );
    let reader_watch = tempfile::tempdir().unwrap();
    let (reader, _) = connected_engine(
        &url,
        &token,
        reader_watch.path().to_path_buf(),
        SUCCESSOR_SEED,
        [0x0C; 32],
    )
    .await;
    for ((rel, body), old) in files.iter().zip(&before) {
        let head = nest_head_manifest(Arc::clone(&succ_ctl), rel).await;
        assert_ne!(&head, old, "{rel}: the head moved");
        assert_eq!(
            reader
                .download_file_bytes_by_manifest(manifest_hash_of(&head), None, rel)
                .await
                .expect("opens with no predecessor material"),
            *body
        );
    }
}
