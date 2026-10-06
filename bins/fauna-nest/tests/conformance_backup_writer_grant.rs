//! **tier_3** — the destination-side nest-writer backup plane (nest-side segment
//! backup, slice 3): `fauna.backup.writer_grant.{register,revoke,list}` (the
//! owner's client → destination, USER-class) and the two federation kinds a
//! source nest speaks, `fauna.federation.backup.{changes.record,write_token.mint}`.
//!
//! Goal docs: `federation.md` § Nest-writer backup plane (kinds + gates),
//! `segment-backup-protocol.md` § Cross-location backup protocol (writer
//! identity, ratified 2026-07-23; *The writer seat*, ruled 2026-10-01).
//!
//! **The property under test is the gate.** A federated backup write arrives with
//! a *verified* `origin_nest_id` — the handshake proves which nest is calling —
//! but a nest id is self-minted and free, so the signature is attribution and the
//! **grant row this nest wrote at the owner's direction** is the authorization.
//! Everything here exists to pin that separation:
//!
//! - an ungranted nest is refused both kinds (the typed
//!   `fauna.backup.writer_not_seated`), and refused *before* any custody row or
//!   token exists;
//! - a granted nest records custody into the owner's reserved custody-copy set
//!   set, which is created lazily on first use and owned by the **owner**;
//! - revoking at the destination refuses the next mint — the freeze-the-backup
//!   affordance, which must work with the source nest fully hostile;
//! - a grant for one owner authorizes nothing for another owner;
//! - an owner has ONE writer seat: a second source box is refused
//!   (`fauna.backup.writer_seat_held`) while the owner's custody holds a live
//!   path, a revoke keeps the seat held, and only `succeeds` naming the holder
//!   moves it;
//! - the client-facing register/revoke/list kinds are owner-scoped and
//!   User-class.
//!
//! The final test drives the whole chain over a **real federation handshake**
//! between two in-process nests, so the gate is proven against the actual
//! `origin_nest_id` the channel verifies rather than a hand-passed value.

mod common;
use common::encode;
use common::register_user;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_core::chunk::ChunkManifest;
use fauna_core::data::ContentHash;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::federation_channel::dial;
use fauna_nest::federation_handlers::{
    FedBackupChangesRecordReply, FedBackupChangesRecordRequest, FedBackupWriteTokenMintReply,
    FedBackupWriteTokenMintRequest,
};
use fauna_nest::federation_pool::{
    originate_backup_changes_record, originate_backup_write_token_mint,
};
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{backup_handlers, federation_handlers};
use fauna_protocol::backup::{
    GenerationRestoreReply, GenerationRestoreRequest, WRITER_SEAT_HELD_HOLDER,
    WriterGrantListReply, WriterGrantListRequest, WriterGrantRegisterReply,
    WriterGrantRegisterRequest, WriterGrantRevokeReply, WriterGrantRevokeRequest,
};
use fauna_protocol::{RpcError, Value, decode_strict as decode, encode_canonical};
use serde::Serialize;
use serde::de::DeserializeOwned;

const OWNER: [u8; 32] = [0x11; 32];
const OTHER_OWNER: [u8; 32] = [0x22; 32];
const SOURCE_NEST: [u8; 32] = [0x51; 32];
const STRANGER_NEST: [u8; 32] = [0xF0; 32];

// ═════════════════════════════════════════════════════════════════════════════
// Harness
// ═════════════════════════════════════════════════════════════════════════════

/// A real disk blob store for a test destination, `for_test`-style (pid +
/// sequence tempdir, no cleanup — the same pattern as `AppState::for_test`'s
/// own segment/acme dirs). The custody charge is derived from what this store
/// actually holds, so a destination fixture without one refuses every record.
fn test_backup_service(db: &Arc<CacheDb>) -> Option<Arc<BackupService>> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "fauna-test-custody-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    Some(Arc::new(
        BackupService::new(db.clone(), None, false, root, None).unwrap(),
    ))
}

/// A destination nest: real `AppState` + real `CacheDb` + real blob store, with
/// both the client-facing backup router and the federation router populated.
async fn destination() -> (
    RpcRouter,
    fauna_nest::federation_router::FederationRouter,
    Arc<AppState>,
) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        backup_service: test_backup_service(&db),
        ..AppState::for_test(db)
    });
    let mut b = RpcRouter::builder();
    backup_handlers::register_backup_handlers(&mut b);
    let mut fb = fauna_nest::federation_router::FederationRouter::builder();
    federation_handlers::register_federation_handlers(&mut fb);
    (b.build(), fb.build(), state)
}

/// Store one body the way the chunk/manifest POST routes do — at-rest framed in
/// the destination's store, wire length metered in `blob_metadata` — and return
/// its digest.
async fn store_metered_blob(state: &Arc<AppState>, body: &[u8], kind: &str) -> [u8; 32] {
    let svc = state.backup_service.as_ref().expect("fixture has a store");
    let digest = ContentHash::of_raw(body).digest();
    svc.local_blob_store()
        .put(
            &ContentHash::from_digest_raw(digest),
            &fauna_nest::backup::encode_blob(body, None, false).unwrap(),
        )
        .await
        .unwrap();
    state
        .db
        .put_blob_metadata(&digest, body.len() as i64, kind, None, None)
        .await
        .unwrap();
    digest
}

fn manifest_bytes(chunks: &[([u8; 32], u64)]) -> Vec<u8> {
    let m = ChunkManifest {
        file_hash: ContentHash::from_digest_raw([0x0F; 32]),
        total_size: chunks.iter().map(|(_, s)| *s).sum(),
        chunk_hashes: chunks
            .iter()
            .map(|(h, _)| ContentHash::from_digest_raw(*h))
            .collect(),
        chunk_sizes: chunks.iter().map(|(_, s)| *s).collect(),
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: None,
    };
    fauna_core::encoding::canonical_encode(&m).unwrap()
}

/// Upload real held bytes (chunks + a manifest over them) to the destination,
/// returning the manifest's hex hash and the total the destination now holds
/// under it — the only charge an honest destination may take for a custody row
/// naming it.
async fn seed_held_bytes(state: &Arc<AppState>, tag: u8, chunk_sizes: &[usize]) -> (String, i64) {
    let mut held: i64 = 0;
    let mut chunks = Vec::new();
    for (i, size) in chunk_sizes.iter().enumerate() {
        let body = vec![tag ^ (i as u8); *size];
        let digest = store_metered_blob(state, &body, "chunk").await;
        held += body.len() as i64;
        chunks.push((digest, body.len() as u64));
    }
    let body = manifest_bytes(&chunks);
    let digest = store_metered_blob(state, &body, "manifest").await;
    held += body.len() as i64;
    (hex::encode(digest), held)
}

/// The owner's `storage_bytes_used` on this destination.
async fn used(state: &Arc<AppState>, actor: [u8; 32]) -> i64 {
    state
        .db
        .list_users()
        .await
        .unwrap()
        .into_iter()
        .find(|u| u.actor_id == actor.to_vec())
        .unwrap()
        .storage_bytes_used
}

/// Dispatch a client-facing (USER-class) kind as `actor`.
async fn client_call<Req: Serialize, Rep: DeserializeOwned>(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    req: &Req,
) -> Result<Rep, RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    let out = (meta.handler)(state.clone(), actor, encode(req)).await?;
    Ok(decode(&out).unwrap())
}

/// Dispatch a federation kind as if it arrived from `origin_nest_id` — i.e. from
/// a peer whose identity the channel handshake has already verified.
async fn fed_call<Req: Serialize, Rep: DeserializeOwned>(
    fed: &fauna_nest::federation_router::FederationRouter,
    state: &Arc<AppState>,
    origin_nest_id: [u8; 32],
    kind: &str,
    req: &Req,
) -> Result<Rep, RpcError> {
    let meta = fed.kind_meta(kind).expect("federation kind registered");
    let out = (meta.handler)(state.clone(), origin_nest_id, encode(req)).await?;
    Ok(decode(&out).unwrap())
}

async fn register_grant(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: [u8; 32],
    writer: [u8; 32],
) -> Result<WriterGrantRegisterReply, RpcError> {
    client_call(
        router,
        state,
        owner,
        "fauna.backup.writer_grant.register",
        &WriterGrantRegisterRequest {
            writer_nest_id: hex::encode(writer),
            ..Default::default()
        },
    )
    .await
}

async fn revoke_grant(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: [u8; 32],
    writer: [u8; 32],
) -> Result<WriterGrantRevokeReply, RpcError> {
    client_call(
        router,
        state,
        owner,
        "fauna.backup.writer_grant.revoke",
        &WriterGrantRevokeRequest {
            writer_nest_id: hex::encode(writer),
            extra: Default::default(),
        },
    )
    .await
}

async fn list_grants(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: [u8; 32],
) -> WriterGrantListReply {
    client_call(
        router,
        state,
        owner,
        "fauna.backup.writer_grant.list",
        &WriterGrantListRequest {
            extra: Default::default(),
        },
    )
    .await
    .expect("list succeeds")
}

fn record_req_with(
    owner: [u8; 32],
    path: &str,
    manifest_hex: &str,
    declared_size: i64,
) -> FedBackupChangesRecordRequest {
    FedBackupChangesRecordRequest {
        owner_actor_id: hex::encode(owner),
        kind: "mail".to_string(),
        scope_id: hex::encode(owner),
        device_id: hex::encode([0xD1u8; 32]),
        path: path.to_string(),
        manifest_hash: Some(manifest_hex.to_string()),
        size_bytes: declared_size,
        change_type: "create".to_string(),
        folder_id: None,
        path_sealed: None,
    }
}

/// A record naming a manifest nobody ever uploaded — the shape a gate refusal
/// must reject *before* the charge derivation even matters.
fn record_req(owner: [u8; 32], path: &str) -> FedBackupChangesRecordRequest {
    record_req_with(owner, path, &hex::encode([0xAB; 32]), 4096)
}

async fn record_custody(
    fed: &fauna_nest::federation_router::FederationRouter,
    state: &Arc<AppState>,
    origin: [u8; 32],
    req: &FedBackupChangesRecordRequest,
) -> Result<FedBackupChangesRecordReply, RpcError> {
    fed_call(
        fed,
        state,
        origin,
        "fauna.federation.backup.changes.record",
        req,
    )
    .await
}

async fn mint_token(
    fed: &fauna_nest::federation_router::FederationRouter,
    state: &Arc<AppState>,
    origin: [u8; 32],
    owner: [u8; 32],
) -> Result<FedBackupWriteTokenMintReply, RpcError> {
    fed_call(
        fed,
        state,
        origin,
        "fauna.federation.backup.write_token.mint",
        &FedBackupWriteTokenMintRequest {
            owner_actor_id: hex::encode(owner),
        },
    )
    .await
}

/// The owner's reserved backup set on this destination, if it exists.
async fn custody_set(state: &Arc<AppState>, owner: [u8; 32]) -> Option<fauna_nest::db::FolderRow> {
    state
        .db
        .get_folder_for_actor("__mail", &owner)
        .await
        .unwrap()
}

// ═════════════════════════════════════════════════════════════════════════════
// The gate — an ungranted nest writes nothing
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn an_ungranted_nest_is_refused_both_backup_kinds() {
    let (_router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;

    let err = record_custody(&fed, &state, STRANGER_NEST, &record_req(OWNER, "seg-0.dat"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.backup.writer_not_seated");

    let err = mint_token(&fed, &state, STRANGER_NEST, OWNER)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.backup.writer_not_seated");

    // Refused BEFORE any state exists: no custody set was lazily created, so a
    // rejected writer leaves no footprint at all.
    assert!(
        custody_set(&state, OWNER).await.is_none(),
        "a refused writer must not create the owner's custody set"
    );
}

#[tokio::test]
async fn a_grant_authorizes_only_the_named_owners_custody() {
    // The grant is per-(owner, writer). A source nest legitimately backing up
    // Alice must not thereby gain write power over Bob's custody on the same
    // destination.
    let (router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_user(&state, OTHER_OWNER, "bob").await;
    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();

    let (mh, _) = seed_held_bytes(&state, 0xA1, &[64]).await;
    record_custody(
        &fed,
        &state,
        SOURCE_NEST,
        &record_req_with(OWNER, "seg-0.dat", &mh, 4096),
    )
    .await
    .expect("granted for Alice");

    let err = record_custody(
        &fed,
        &state,
        SOURCE_NEST,
        &record_req(OTHER_OWNER, "seg-0.dat"),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.code, "fauna.backup.writer_not_seated",
        "Alice's grant must not authorize writes against Bob's custody"
    );
    assert!(custody_set(&state, OTHER_OWNER).await.is_none());
}

// ═════════════════════════════════════════════════════════════════════════════
// The happy path — custody lands in an owner-owned, custody-copy set
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn a_granted_nest_records_custody_into_the_owners_backup_set() {
    let (router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();

    let (mh, _) = seed_held_bytes(&state, 0xA2, &[64]).await;
    record_custody(
        &fed,
        &state,
        SOURCE_NEST,
        &record_req_with(OWNER, "seg-0.dat", &mh, 4096),
    )
    .await
    .expect("granted writer records custody");

    // The set was created lazily, owned by the OWNER (not the writing nest, not
    // the scope) and marked a custody copy — which is what routes the record into
    // the `backup_custody` projection and what the four capability gates read.
    let fs = custody_set(&state, OWNER)
        .await
        .expect("custody set created on first use");
    assert_eq!(fs.actor_id, OWNER.to_vec(), "custody set is owner-owned");
    assert!(
        fs.custody_copy,
        "the provisioner marks the row a custody copy"
    );
    assert!(
        state
            .db
            .is_pure_backup_destination("mail", &OWNER)
            .await
            .unwrap(),
        "the lazily-created set must satisfy the pure-backup-destination predicate"
    );

    // The record reached the custody projection GC walks — the whole point of
    // record-on-upload (`message-segment-store.md` § GC-safety).
    let live = state.db.backup_custody_manifest_hashes().await.unwrap();
    assert!(
        !live.is_empty(),
        "recorded custody must appear in the live custody manifest set"
    );
}

#[tokio::test]
async fn a_replayed_record_is_exactly_once_by_content() {
    // Reconnect / redelivery / crashed-ack retry must not double-charge or
    // duplicate — the same exactly-once-by-content contract the folder write
    // plane runs, because both call the same `record_change_core`.
    let (router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();
    let (mh, held) = seed_held_bytes(&state, 0xA3, &[128]).await;
    let req = record_req_with(OWNER, "seg-0.dat", &mh, 4096);

    let first = record_custody(&fed, &state, SOURCE_NEST, &req)
        .await
        .unwrap();
    assert_eq!(used(&state, OWNER).await, held);
    let replay = record_custody(&fed, &state, SOURCE_NEST, &req)
        .await
        .unwrap();
    assert_eq!(
        first.seq, replay.seq,
        "a content-identical replay returns the original seq"
    );
    assert_eq!(
        used(&state, OWNER).await,
        held,
        "a content-identical replay charges nothing"
    );
    assert!(
        state
            .db
            .backup_custody_generation_manifest_hashes()
            .await
            .unwrap()
            .is_empty(),
        "a replay must not retain the still-live manifest as a generation"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// The charge — destination-derived, never writer-declared (review (xxxi-c))
// ═════════════════════════════════════════════════════════════════════════════

/// The ratified supersede rate cap is "quota-charging IS the rate cap"
/// (`message-segment-store.md` § Reclaim rate-cap) — which binds only if the
/// charge is real. A writer declaring `size_bytes: 0` must still be charged
/// every byte the destination actually holds (and this row pins) under the
/// named manifest: the manifest blob plus its distinct chunks, as the
/// destination itself measured them at upload.
#[tokio::test]
async fn a_record_is_charged_what_the_destination_holds_not_what_the_writer_declares() {
    let (router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();
    let (mh, held) = seed_held_bytes(&state, 0xB0, &[100, 250]).await;

    record_custody(
        &fed,
        &state,
        SOURCE_NEST,
        &record_req_with(OWNER, "seg-0.dat", &mh, 0),
    )
    .await
    .expect("a record naming held bytes is served");

    assert!(held > 0);
    assert_eq!(
        used(&state, OWNER).await,
        held,
        "the charge is what the destination holds, not the declared zero"
    );
}

/// Reviewer trap (i): an absent manifest must never fail OPEN to a zero (or
/// declared) charge. A record naming bytes this destination does not hold is
/// refused — typed and retryable, because an honest writer only ever records
/// after its upload (record-on-upload), so hitting this means the bytes are
/// missing, not late.
#[tokio::test]
async fn a_record_naming_bytes_the_destination_does_not_hold_is_refused() {
    let (router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();

    // The phantom-manifest request: declared 4096, nothing ever uploaded.
    let err = record_custody(&fed, &state, SOURCE_NEST, &record_req(OWNER, "seg-0.dat"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.sync.backup_bytes_not_held");
    assert_eq!(
        used(&state, OWNER).await,
        0,
        "a refused record charges nothing"
    );
    assert!(
        state
            .db
            .backup_custody_manifest_hashes()
            .await
            .unwrap()
            .is_empty(),
        "a refused record pins nothing in the custody projection"
    );
}

/// The chunk half of the same trap: a held, decodable manifest whose chunk set
/// is not fully held refuses — otherwise a writer could record first and ship
/// the (GC-pinned) chunks after the charge was derived from their absence.
#[tokio::test]
async fn a_record_whose_manifest_references_an_unheld_chunk_is_refused() {
    let (router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();

    let held_chunk = store_metered_blob(&state, &[0xB1u8; 64], "chunk").await;
    let body = manifest_bytes(&[(held_chunk, 64), ([0x99u8; 32], 4096)]);
    let mh = store_metered_blob(&state, &body, "manifest").await;

    let err = record_custody(
        &fed,
        &state,
        SOURCE_NEST,
        &record_req_with(OWNER, "seg-0.dat", &hex::encode(mh), 0),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.sync.backup_bytes_not_held");
    assert_eq!(used(&state, OWNER).await, 0);
}

#[tokio::test]
async fn a_granted_nest_mints_a_write_only_bulk_token() {
    let (router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();

    let minted = mint_token(&fed, &state, SOURCE_NEST, OWNER)
        .await
        .expect("granted writer mints");
    assert!(!minted.token.is_empty());
    assert!(
        minted.expires_at > 0,
        "the token carries an absolute expiry (TTL is a Rust constant, not a knob)"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Revocation — the freeze-the-backup affordance
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn revoking_at_the_destination_refuses_the_next_mint_and_record() {
    // This is the affordance that has to work with the SOURCE NEST FULLY
    // HOSTILE: the owner's client talks to the destination directly, and the
    // destination stops honouring the source's writes — no cooperation from the
    // source required.
    let (router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();
    mint_token(&fed, &state, SOURCE_NEST, OWNER)
        .await
        .expect("granted: mint succeeds");

    assert!(
        revoke_grant(&router, &state, OWNER, SOURCE_NEST)
            .await
            .unwrap()
            .revoked
    );

    let err = mint_token(&fed, &state, SOURCE_NEST, OWNER)
        .await
        .unwrap_err();
    assert_eq!(
        err.code, "fauna.backup.writer_not_seated",
        "revocation refuses the NEXT mint"
    );
    let err = record_custody(&fed, &state, SOURCE_NEST, &record_req(OWNER, "seg-1.dat"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.backup.writer_not_seated");

    // Idempotent: a second revoke is success-with-false, not an error.
    assert!(
        !revoke_grant(&router, &state, OWNER, SOURCE_NEST)
            .await
            .unwrap()
            .revoked
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// The client-facing register/revoke/list plane
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_is_owner_scoped_and_register_is_idempotent() {
    let (router, _fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_user(&state, OTHER_OWNER, "bob").await;

    register_grant(&router, &state, OWNER, STRANGER_NEST)
        .await
        .unwrap();
    register_grant(&router, &state, OTHER_OWNER, SOURCE_NEST)
        .await
        .unwrap();

    let alice = list_grants(&router, &state, OWNER).await;
    assert_eq!(alice.grants.len(), 1);
    assert_eq!(alice.grants[0].writer_nest_id, hex::encode(STRANGER_NEST));
    let bob = list_grants(&router, &state, OTHER_OWNER).await;
    assert_eq!(bob.grants.len(), 1, "Bob never sees Alice's seat");
    assert_eq!(bob.grants[0].writer_nest_id, hex::encode(SOURCE_NEST));

    // Re-registering refreshes rather than duplicating (enroll is retry-safe).
    register_grant(&router, &state, OTHER_OWNER, SOURCE_NEST)
        .await
        .unwrap();
    assert_eq!(
        list_grants(&router, &state, OTHER_OWNER).await.grants.len(),
        1
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// The writer seat — one source box per owner
// ═════════════════════════════════════════════════════════════════════════════

/// Record one live custody path for `OWNER` as `origin`, over real held bytes.
/// Returns the manifest's hex hash.
async fn record_live_path(
    fed: &fauna_nest::federation_router::FederationRouter,
    state: &Arc<AppState>,
    origin: [u8; 32],
    path: &str,
    tag: u8,
) -> String {
    let (mh, held) = seed_held_bytes(state, tag, &[64]).await;
    record_custody(fed, state, origin, &record_req_with(OWNER, path, &mh, held))
        .await
        .expect("the seated writer records custody");
    mh
}

/// Register `writer` for `OWNER`, naming `succeeds` when given.
async fn register_succeeding(
    router: &RpcRouter,
    state: &Arc<AppState>,
    writer: [u8; 32],
    succeeds: Option<[u8; 32]>,
) -> Result<WriterGrantRegisterReply, RpcError> {
    client_call(
        router,
        state,
        OWNER,
        "fauna.backup.writer_grant.register",
        &WriterGrantRegisterRequest {
            writer_nest_id: hex::encode(writer),
            succeeds: succeeds.map(hex::encode),
            ..Default::default()
        },
    )
    .await
}

/// A registration refused `fauna.backup.writer_seat_held`, and the holder its
/// details name.
fn seat_held_holder(err: &RpcError) -> Option<String> {
    assert_eq!(err.code, "fauna.backup.writer_seat_held");
    let Some(Value::Map(details)) = err.details.as_deref() else {
        panic!("the refusal carries a details map: {err:?}");
    };
    match details.get(WRITER_SEAT_HELD_HOLDER) {
        Some(Value::String(holder)) => Some(holder.clone()),
        None => None,
        other => panic!("the holder is a hex string: {other:?}"),
    }
}

#[tokio::test]
async fn a_revoke_keeps_the_seat_against_another_writer_while_custody_is_live() {
    // A revoke says *this box may no longer write*, never *another box may now
    // overwrite what it wrote*.
    let (router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();
    let live = record_live_path(&fed, &state, SOURCE_NEST, "seg-1.dat", 0xB1).await;

    assert!(
        revoke_grant(&router, &state, OWNER, SOURCE_NEST)
            .await
            .unwrap()
            .revoked
    );
    let seat = list_grants(&router, &state, OWNER).await.grants;
    assert_eq!(seat.len(), 1, "a revoke keeps the seat's row");
    assert_eq!(seat[0].writer_nest_id, hex::encode(SOURCE_NEST));
    assert!(seat[0].revoked);

    let err = register_grant(&router, &state, OWNER, STRANGER_NEST)
        .await
        .unwrap_err();
    assert_eq!(
        seat_held_holder(&err),
        Some(hex::encode(SOURCE_NEST)),
        "the refusal names the box that holds the seat"
    );
    let err = mint_token(&fed, &state, STRANGER_NEST, OWNER)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.backup.writer_not_seated");
    assert_eq!(
        state.db.backup_custody_manifest_hashes().await.unwrap(),
        vec![hex::decode(&live).unwrap()],
        "the holder's live copy is untouched"
    );
}

#[tokio::test]
async fn a_revoked_holder_registering_again_is_re_granted() {
    let (router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();
    record_live_path(&fed, &state, SOURCE_NEST, "seg-1.dat", 0xB2).await;
    revoke_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();
    let err = mint_token(&fed, &state, SOURCE_NEST, OWNER)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.backup.writer_not_seated");

    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .expect("the holder is re-granted over its own live custody");
    mint_token(&fed, &state, SOURCE_NEST, OWNER)
        .await
        .expect("re-granted: mint succeeds");
    let seat = list_grants(&router, &state, OWNER).await.grants;
    assert_eq!(seat.len(), 1);
    assert!(!seat[0].revoked);
}

#[tokio::test]
async fn another_writer_takes_a_torn_down_seat_and_its_old_generations_refuse_restore() {
    let (router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();
    let old = record_live_path(&fed, &state, SOURCE_NEST, "seg-1.dat", 0xB3).await;

    // While the path is live the second box is refused.
    let err = register_grant(&router, &state, OWNER, STRANGER_NEST)
        .await
        .unwrap_err();
    assert_eq!(seat_held_holder(&err), Some(hex::encode(SOURCE_NEST)));

    // The removal teardown: the writer tombstones every path it wrote. The
    // generation is retained under the grace window.
    record_custody(
        &fed,
        &state,
        SOURCE_NEST,
        &FedBackupChangesRecordRequest {
            manifest_hash: None,
            size_bytes: 0,
            change_type: "delete".to_string(),
            ..record_req(OWNER, "seg-1.dat")
        },
    )
    .await
    .expect("the seated writer tombstones its path");
    assert!(
        state
            .db
            .backup_custody_manifest_hashes()
            .await
            .unwrap()
            .is_empty()
    );
    let retained = state
        .db
        .list_backup_custody_generations(&OWNER, None, 0)
        .await
        .unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(hex::encode(&retained[0].manifest_hash), old);
    // The teardown happened before the seat changes hands, not in its second.
    state
        .db
        .backdate_backup_custody_generations_for_test(60)
        .await
        .unwrap();

    // No live path left: the second box takes the seat, and the first is out.
    register_grant(&router, &state, OWNER, STRANGER_NEST)
        .await
        .expect("a seat over custody with no live path can be taken");
    let seat = list_grants(&router, &state, OWNER).await.grants;
    assert_eq!(seat.len(), 1);
    assert_eq!(seat[0].writer_nest_id, hex::encode(STRANGER_NEST));
    let err = mint_token(&fed, &state, SOURCE_NEST, OWNER)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.backup.writer_not_seated");

    // The previous writer's generation is not restorable into the new seat's
    // numbering; it stays retained and ages out under T.
    let err = client_call::<_, GenerationRestoreReply>(
        &router,
        &state,
        OWNER,
        "fauna.backup.generation.restore",
        &GenerationRestoreRequest {
            folder_name: "__mail".to_string(),
            path_hash: hex::encode(&retained[0].path_hash),
            manifest_hash: old.clone(),
            extra: Default::default(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.backup.generation_before_seat");
    assert!(
        state
            .db
            .backup_custody_manifest_hashes()
            .await
            .unwrap()
            .is_empty(),
        "a refused restore makes nothing live"
    );
    assert_eq!(
        state
            .db
            .list_backup_custody_generations(&OWNER, None, 0)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn succeeds_naming_a_non_holder_changes_nothing() {
    let (router, fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;
    register_grant(&router, &state, OWNER, SOURCE_NEST)
        .await
        .unwrap();
    record_live_path(&fed, &state, SOURCE_NEST, "seg-1.dat", 0xB4).await;
    let before = list_grants(&router, &state, OWNER).await;

    let successor = [0x52u8; 32];
    let err = register_succeeding(&router, &state, successor, Some(STRANGER_NEST))
        .await
        .unwrap_err();
    assert_eq!(seat_held_holder(&err), Some(hex::encode(SOURCE_NEST)));
    assert_eq!(list_grants(&router, &state, OWNER).await, before);
    mint_token(&fed, &state, SOURCE_NEST, OWNER)
        .await
        .expect("the holder still writes");

    // Naming the holder moves the seat, over live custody, with no revoke.
    register_succeeding(&router, &state, successor, Some(SOURCE_NEST))
        .await
        .expect("the holder's successor takes the seat");
    let seat = list_grants(&router, &state, OWNER).await.grants;
    assert_eq!(seat.len(), 1);
    assert_eq!(seat[0].writer_nest_id, hex::encode(successor));
    mint_token(&fed, &state, successor, OWNER)
        .await
        .expect("the successor writes");
    let err = mint_token(&fed, &state, SOURCE_NEST, OWNER)
        .await
        .unwrap_err();
    assert_eq!(
        err.code, "fauna.backup.writer_not_seated",
        "the predecessor's authority ends with the handover"
    );
    // Repeating the completed handover is a plain refresh.
    register_succeeding(&router, &state, successor, Some(SOURCE_NEST))
        .await
        .expect("a repeated handover is a refresh");

    // A malformed `succeeds` is reported, never ignored.
    let err = client_call::<_, WriterGrantRegisterReply>(
        &router,
        &state,
        OWNER,
        "fauna.backup.writer_grant.register",
        &WriterGrantRegisterRequest {
            writer_nest_id: hex::encode(successor),
            succeeds: Some("not-hex".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.protocol.malformed");
}

#[tokio::test]
async fn register_refuses_a_malformed_writer_nest_id() {
    // A malformed id must be reported, never stored — a row that can never match
    // an `origin_nest_id` would be a silently dead grant the owner believes is
    // live.
    let (router, _fed, state) = destination().await;
    register_user(&state, OWNER, "alice").await;

    let err: RpcError = client_call::<_, WriterGrantRegisterReply>(
        &router,
        &state,
        OWNER,
        "fauna.backup.writer_grant.register",
        &WriterGrantRegisterRequest {
            writer_nest_id: "not-hex".to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.protocol.malformed");
    assert!(list_grants(&router, &state, OWNER).await.grants.is_empty());
}

#[tokio::test]
async fn the_client_plane_is_user_class() {
    // An actor with no `users` row resolves to no caller class and is denied at
    // dispatch, before any handler body.
    let (router, _fed, state) = destination().await;
    let stranger = [0x99u8; 32];

    let err = register_grant(&router, &state, stranger, SOURCE_NEST)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

// ═════════════════════════════════════════════════════════════════════════════
// End-to-end over a real federation handshake
// ═════════════════════════════════════════════════════════════════════════════

/// Spin a real in-process nest on loopback with its own identity, federation
/// router and anonymous discovery surface — the shape `dial` needs.
async fn start_nest() -> (String, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();
    let state = Arc::new(AppState {
        backup_service: test_backup_service(&db),
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            backup_handlers::register_backup_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), state)
}

fn to_value<T: Serialize>(t: &T) -> Value {
    decode::<Value>(&encode_canonical(t).unwrap()).unwrap()
}

fn from_value<T: DeserializeOwned>(v: &Value) -> T {
    decode::<T>(&encode_canonical(v).unwrap()).unwrap()
}

/// **The capstone.** Source nest S dials destination D over the *ordinary*
/// `fauna.federation.hello` handshake — no pairing — and D gates on the writer
/// grant its own USER-class plane wrote. Proves the ratified transport decision
/// end to end: the gate reads the `origin_nest_id` the channel actually
/// verified, not a value the caller supplied.
#[tokio::test]
async fn backup_writes_ride_the_ordinary_federation_handshake_gated_on_the_grant() {
    let (_s_url, s_state) = start_nest().await;
    let (d_url, d_state) = start_nest().await;
    let d_nest_id = hex::encode(d_state.nest_identity.public_key_bytes());
    let s_nest_id: [u8; 32] = s_state.nest_identity.public_key_bytes();

    register_user(&d_state, OWNER, "alice").await;

    // S dials D. No pairing is established anywhere — `is_paired` stays scoped
    // to the private nest-sync surface, exactly as ratified.
    let conn = dial(&s_state, &d_url, &d_nest_id)
        .await
        .expect("ordinary federation channel establishes without pairing");

    // Before the owner grants: D refuses S's write even though the handshake
    // succeeded. Signature is attribution; the grant is authorization.
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.backup.write_token.mint",
            [1u8; 16],
            to_value(&FedBackupWriteTokenMintRequest {
                owner_actor_id: hex::encode(OWNER),
            }),
            None,
        )
        .await
        .expect("request rides the channel");
    let err = call
        .await_reply()
        .await
        .expect_err("an authenticated but ungranted nest must be refused");
    assert_eq!(err.code, "fauna.backup.writer_not_seated");

    // The owner's client registers the grant on D over its OWN authed
    // connection (modelled here by dispatching D's USER-class kind directly —
    // the client↔destination plane, never the federation channel).
    let meta = d_state
        .rpc_router
        .kind_meta("fauna.backup.writer_grant.register")
        .unwrap();
    (meta.handler)(
        d_state.clone(),
        OWNER,
        encode(&WriterGrantRegisterRequest {
            writer_nest_id: hex::encode(s_nest_id),
            ..Default::default()
        }),
    )
    .await
    .expect("owner registers the writer grant at the destination");

    // Now the same handshake-authenticated S is served.
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.backup.write_token.mint",
            [2u8; 16],
            to_value(&FedBackupWriteTokenMintRequest {
                owner_actor_id: hex::encode(OWNER),
            }),
            None,
        )
        .await
        .unwrap();
    let reply: FedBackupWriteTokenMintReply = from_value(&call.await_reply().await.unwrap());
    assert!(!reply.token.is_empty(), "granted source nest gets a token");

    // …and its custody record lands in the owner's backup set on D — naming
    // bytes D actually holds (record-on-upload: the bytes precede the record).
    let (mh, _) = seed_held_bytes(&d_state, 0xA4, &[64]).await;
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.backup.changes.record",
            [3u8; 16],
            to_value(&record_req_with(OWNER, "seg-0.dat", &mh, 4096)),
            None,
        )
        .await
        .unwrap();
    let _: FedBackupChangesRecordReply = from_value(&call.await_reply().await.unwrap());

    let fs = custody_set(&d_state, OWNER)
        .await
        .expect("custody set exists on the destination");
    assert!(
        fs.custody_copy,
        "the provisioner marks the row a custody copy"
    );
    assert!(
        !d_state
            .db
            .backup_custody_manifest_hashes()
            .await
            .unwrap()
            .is_empty()
    );
}

/// **The source side of the same plane.** The capstone above proves the
/// destination *serves* the two federation kinds; this proves the source nest's
/// typed originator wrappers — the ones the in-process backup coordinator drives
/// — actually reach them through the pooled federation channel, with the grant
/// as the gate. The pool resolves the peer `nest_id` over anonymous
/// `fauna.nest.info` and dials the channel itself, so this exercises the exact
/// path the coordinator will take (no hand-built `dial`, no hand-passed origin).
#[tokio::test]
async fn the_originator_wrappers_drive_the_backup_plane_through_the_pool() {
    let (_s_url, s_state) = start_nest().await;
    let (d_url, d_state) = start_nest().await;
    let s_nest_id: [u8; 32] = s_state.nest_identity.public_key_bytes();
    let d_nest_id: [u8; 32] = d_state.nest_identity.public_key_bytes();
    let owner_hex = hex::encode(OWNER);

    register_user(&d_state, OWNER, "alice").await;

    // Ungranted: the destination refuses, and the wrapper surfaces that refusal
    // rather than silently succeeding. (Attribution ≠ authorization.)
    let err = originate_backup_write_token_mint(
        &s_state.federation_pool,
        &s_state,
        &d_url,
        &d_nest_id,
        &owner_hex,
    )
    .await
    .expect_err("an ungranted source nest must be refused");
    assert!(
        err.to_string().contains("writer_not_seated"),
        "expected the peer's typed refusal to surface, got: {err}"
    );

    // The owner registers the grant at the destination over its OWN authed
    // connection (the client↔destination plane, never the federation channel).
    let meta = d_state
        .rpc_router
        .kind_meta("fauna.backup.writer_grant.register")
        .unwrap();
    (meta.handler)(
        d_state.clone(),
        OWNER,
        encode(&WriterGrantRegisterRequest {
            writer_nest_id: hex::encode(s_nest_id),
            ..Default::default()
        }),
    )
    .await
    .expect("owner registers the writer grant");

    // Now both originators succeed through the pool.
    let minted = originate_backup_write_token_mint(
        &s_state.federation_pool,
        &s_state,
        &d_url,
        &d_nest_id,
        &owner_hex,
    )
    .await
    .expect("granted source nest mints a bulk-write token");
    assert!(!minted.token.is_empty());
    assert!(minted.expires_at > 0, "token carries an absolute expiry");

    let (mh, _) = seed_held_bytes(&d_state, 0xA5, &[64]).await;
    let req = record_req_with(OWNER, "seg-0.dat", &mh, 4096);
    let first = originate_backup_changes_record(
        &s_state.federation_pool,
        &s_state,
        &d_url,
        &d_nest_id,
        &req,
    )
    .await
    .expect("granted source nest records custody");

    // Exactly-once by content: a re-record of the identical change returns the
    // same seq (the retry-safe policy the wrapper declares depends on this).
    let replay = originate_backup_changes_record(
        &s_state.federation_pool,
        &s_state,
        &d_url,
        &d_nest_id,
        &req,
    )
    .await
    .expect("replay succeeds");
    assert_eq!(
        first.seq, replay.seq,
        "a content-identical replay must return the original seq"
    );

    // …and the custody actually landed in the owner's backup set on D.
    let fs = custody_set(&d_state, OWNER)
        .await
        .expect("custody set exists on the destination");
    assert!(
        fs.custody_copy,
        "the provisioner marks the row a custody copy"
    );
    assert_eq!(fs.actor_id, OWNER.to_vec(), "custody set is owner-owned");
}

/// **The registered `nest_id` pin is enforced on the dial.**
///
/// A destination row stores both the URL and the 32-byte nest id
/// (`db/backup_destinations.rs` — "the nest id it must pin as the handshake's
/// expected peer"). Without spending that pin, an attacker who takes over the
/// URL's resolution and serves a valid cert for its domain is indistinguishable
/// from the real destination: the federation `hello` re-binds via the served
/// SPKI, and `resolve_peer_nest_id` asks *the impostor* who it is. The
/// impostor then fabricates its own local writer grant and is served.
///
/// The damage is bounded — chunks are opaque ciphertext, no source data is lost
/// — but the owner's genuine backup **silently stalls**, visible only as a
/// backlog that never drains. Enforcing the pin makes it loud.
///
/// The impostor here is fully cooperative and grants the source nest everything,
/// so the only thing that can refuse the write is the pin itself.
#[tokio::test]
async fn a_destination_url_answering_as_another_nest_is_refused_before_any_write() {
    let (_s_url, s_state) = start_nest().await;
    let (_d_url, d_state) = start_nest().await;
    let (impostor_url, impostor_state) = start_nest().await;
    let s_nest_id: [u8; 32] = s_state.nest_identity.public_key_bytes();
    // The pin the owner registered: the REAL destination.
    let pinned_nest_id: [u8; 32] = d_state.nest_identity.public_key_bytes();
    let owner_hex = hex::encode(OWNER);

    // The impostor is maximally accommodating: the owner exists there and the
    // source nest holds a writer grant, so every gate downstream of the pin
    // would say yes.
    register_user(&impostor_state, OWNER, "alice").await;
    let meta = impostor_state
        .rpc_router
        .kind_meta("fauna.backup.writer_grant.register")
        .unwrap();
    (meta.handler)(
        impostor_state.clone(),
        OWNER,
        encode(&WriterGrantRegisterRequest {
            writer_nest_id: hex::encode(s_nest_id),
            ..Default::default()
        }),
    )
    .await
    .expect("the impostor fabricates a grant for the source nest");

    let err = originate_backup_write_token_mint(
        &s_state.federation_pool,
        &s_state,
        &impostor_url,
        &pinned_nest_id,
        &owner_hex,
    )
    .await
    .expect_err("a URL answering as a different nest must be refused");
    assert!(
        matches!(
            err,
            fauna_nest::federation_pool::PoolError::PeerMismatch { .. }
        ),
        "expected the pin to refuse, got: {err}"
    );

    let err = originate_backup_changes_record(
        &s_state.federation_pool,
        &s_state,
        &impostor_url,
        &pinned_nest_id,
        &record_req(OWNER, "seg-0.dat"),
    )
    .await
    .expect_err("the custody relay is pinned too, not just the mint");
    assert!(matches!(
        err,
        fauna_nest::federation_pool::PoolError::PeerMismatch { .. }
    ));

    // Nothing reached the impostor: the refusal is before the dial, so no
    // custody row, no set, no token.
    assert!(
        custody_set(&impostor_state, OWNER).await.is_none(),
        "a refused dial must leave no custody at the impostor"
    );
}
