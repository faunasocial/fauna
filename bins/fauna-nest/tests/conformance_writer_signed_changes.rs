//! **Writer-signed change records — the nest's ingest verify, storage and
//! projections** (`docs/goal/architecture/mls-group-key-material.md` § M2 →
//! *Multi-writer* → *Writer-signed change records*, rulings (1)–(3)).
//!
//! The chain under test is the production one: the real `fauna.folders.create`
//! / `fauna.folders.update` / `fauna.folders.list` handlers storing and echoing
//! the set nonce; the real `fauna.sync.register` +
//! `fauna.sync.device_grant.register` handlers carrying a `SyncWrite` principal
//! grant minted by the client-side ceremony builder
//! (`fauna_client_sync::build_principal_grant`); the real
//! `fauna.sync.changes.record` ingest core verifying the signature; the real
//! `fauna.sync.changes.list` projecting it with the `signer_certs` side table;
//! and `fauna.sync.devices.delete`'s tombstone. The signer side is the shared
//! statement every engine signs (`SignedChange::for_record`), so these tests
//! also pin that the nest rebuilds exactly the statement a writer signs.
//!
//! Every record kind (ruling (4)) refuses an unsigned record
//! `signature_required` and writes nothing — record, the resolved report and
//! the choose-winner. The resolved report's retained loser is signed by its
//! reporter too (`writer-signed-change-records.md` ruling (10)(d)), so the
//! version history lists it; the fold still exempts it by class.
//!
//! Tier: tier_3 (real handlers + real `CacheDb`, nothing stubbed). Every
//! negative was seen red against a mutated check before it went green.

mod common;

use std::sync::Arc;

use bytes::Bytes;
use fauna_core::identity::ActorKeypair;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{
    conversations_handlers, db::CacheDb, files_handlers, folder_handlers, media_handlers,
    recovery_handlers, sync_handlers,
};
use fauna_protocol::folders::{
    FolderCreateReply, FolderCreateRequest, FolderUpdateReply, FolderUpdateRequest,
    FoldersListReply, FoldersListRequest,
};
use fauna_protocol::sync::{
    DeviceGrantRegisterReply, DeviceGrantRegisterRequest, SyncChangeRecordReply,
    SyncChangeRecordRequest, SyncChangesListReply, SyncChangesListRequest, SyncDeviceDeleteReply,
    SyncDeviceDeleteRequest, SyncRegisterReply, SyncRegisterRequest,
};
use fauna_protocol::sync_writer_sig::SignedChange;
use fauna_protocol::{ByteBuf, RpcError, encode_canonical};

const SET: &str = "photos";
const NONCE: [u8; 32] = [0x6e; 32];

async fn nest() -> (RpcRouter, Arc<AppState>, ActorKeypair) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    media_handlers::register_media_handlers(&mut b);
    files_handlers::register_files_handlers(&mut b);
    // Ruling (8)'s legs: the succession ceremony and the Welcome/channel seat.
    recovery_handlers::register_recovery_handlers(&mut b);
    conversations_handlers::register_conversations_handlers(&mut b);
    let account = ActorKeypair::generate();
    common::seed_dispatch_actor(&state.db, &account.actor_id().0).await;
    (b.build(), state, account)
}

async fn call<Req: serde::Serialize, Reply: serde::de::DeserializeOwned>(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    req: Req,
) -> Result<Reply, RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    let bytes = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply = (meta.handler)(Arc::clone(state), actor, bytes).await?;
    Ok(fauna_protocol::decode_strict(&reply).expect("reply decodes"))
}

async fn create_set(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
    name: &str,
    nonce: Option<[u8; 32]>,
) {
    let _: FolderCreateReply = call(
        router,
        state,
        account.actor_id().0,
        "fauna.folders.create",
        FolderCreateRequest {
            name: name.into(),
            set_nonce: nonce.map(|n| ByteBuf::from(n.to_vec())),
            ..Default::default()
        },
    )
    .await
    .expect("set creates");
}

/// Register a write-capable device row for `account`.
async fn register_device(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
    device_hex: &str,
) {
    let _: SyncRegisterReply = call(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.register",
        SyncRegisterRequest {
            device_id: device_hex.into(),
            label: "machine".into(),
            capabilities: "read,write".into(),
            ..Default::default()
        },
    )
    .await
    .expect("device registers");
}

/// Enroll a machine principal the way the ceremony does: its writer key's row
/// (device id = the writer public key hex) carrying a root-signed
/// `[RenewBearer, SyncWrite]` grant over that key. Returns `(writer, device hex)`.
async fn enroll_principal(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
) -> (ActorKeypair, String) {
    let writer = ActorKeypair::generate();
    let device_hex = fauna_core::hex32::encode(&writer.actor_id().0);
    register_device(router, state, account, &device_hex).await;
    let grant = fauna_client_sync::build_principal_grant(account, &writer.actor_id().0)
        .expect("grant builds");
    let reply: DeviceGrantRegisterReply = call(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.device_grant.register",
        DeviceGrantRegisterRequest {
            device_id: device_hex.clone(),
            authorization: grant,
            extra: Default::default(),
        },
    )
    .await
    .expect("grant registers");
    assert!(reply.registered);
    (writer, device_hex)
}

fn record(device_hex: &str, path: &str, manifest: u8, size: i64) -> SyncChangeRecordRequest {
    SyncChangeRecordRequest {
        folder: SET.into(),
        device_id: device_hex.into(),
        path: path.into(),
        manifest_hash: Some(hex::encode([manifest; 32])),
        size_bytes: size,
        change_type: "create".into(),
        // S9 flip: a sealless record refuses; any opaque envelope satisfies
        // the nest, which never opens it.
        path_sealed: Some(ByteBuf::from(format!("seal:{path}").into_bytes())),
        derived_through: Some(0),
        ..Default::default()
    }
}

/// Sign `req` exactly as a writer engine does — the shared statement over the
/// request, under `nonce`, for the signing `actor` — with `key`, naming
/// `signer_key` as the key.
fn sign(
    mut req: SyncChangeRecordRequest,
    actor: &ActorKeypair,
    nonce: [u8; 32],
    key: &ed25519_dalek::SigningKey,
) -> SyncChangeRecordRequest {
    let statement = SignedChange::for_record(&req, actor.actor_id().0, nonce).unwrap();
    req.signature = Some(ByteBuf::from(statement.sign(key).to_vec()));
    req.signer_key = Some(ByteBuf::from(key.verifying_key().to_bytes().to_vec()));
    req
}

async fn record_as(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
    req: SyncChangeRecordRequest,
) -> Result<i64, RpcError> {
    let reply: SyncChangeRecordReply = call(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.changes.record",
        req,
    )
    .await?;
    Ok(reply.seq)
}

async fn list(
    router: &RpcRouter,
    state: &Arc<AppState>,
    account: &ActorKeypair,
) -> SyncChangesListReply {
    call(
        router,
        state,
        account.actor_id().0,
        "fauna.sync.changes.list",
        SyncChangesListRequest {
            folder: Some(SET.into()),
            since: 0,
            ..Default::default()
        },
    )
    .await
    .expect("list")
}

fn assert_code(err: RpcError, code: &str, what: &str) {
    assert_eq!(err.code, format!("fauna.sync.{code}"), "{what}: {err:?}");
}

/// The set nonce is stored at create, echoed on the owner's list row, and
/// overwritten by the owner's update (custody (f)); a nonce that is not 32
/// bytes is refused on both kinds.
#[tokio::test]
async fn the_set_nonce_is_stored_echoed_and_overwritten_by_the_owner() {
    let (router, state, account) = nest().await;
    let actor = account.actor_id().0;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;

    let echo = |reply: FoldersListReply| {
        reply
            .folders
            .into_iter()
            .find(|f| f.name == SET)
            .expect("the set lists")
            .set_nonce
            .map(|n| n.to_vec())
    };
    let listed: FoldersListReply = call(
        &router,
        &state,
        actor,
        "fauna.folders.list",
        FoldersListRequest {
            include_shared_with_me: None,
            extra: Default::default(),
        },
    )
    .await
    .unwrap();
    assert_eq!(echo(listed), Some(NONCE.to_vec()), "create stores + echoes");

    let fresh = [0x7f; 32];
    let _: FolderUpdateReply = call(
        &router,
        &state,
        actor,
        "fauna.folders.update",
        FolderUpdateRequest {
            name: SET.into(),
            set_nonce: Some(ByteBuf::from(fresh.to_vec())),
            ..Default::default()
        },
    )
    .await
    .expect("the owner overwrites");
    let listed: FoldersListReply = call(
        &router,
        &state,
        actor,
        "fauna.folders.list",
        FoldersListRequest {
            include_shared_with_me: None,
            extra: Default::default(),
        },
    )
    .await
    .unwrap();
    assert_eq!(echo(listed), Some(fresh.to_vec()), "update overwrites");

    let short = call::<_, FolderUpdateReply>(
        &router,
        &state,
        actor,
        "fauna.folders.update",
        FolderUpdateRequest {
            name: SET.into(),
            set_nonce: Some(ByteBuf::from(vec![1; 16])),
            ..Default::default()
        },
    )
    .await
    .expect_err("a 16-byte nonce is refused");
    assert_eq!(short.code, "fauna.folders.invalid_request");
    let short_create = call::<_, FolderCreateReply>(
        &router,
        &state,
        actor,
        "fauna.folders.create",
        FolderCreateRequest {
            name: "other".into(),
            set_nonce: Some(ByteBuf::from(vec![1; 33])),
            ..Default::default()
        },
    )
    .await
    .expect_err("a 33-byte nonce is refused at create");
    assert_eq!(short_create.code, "fauna.folders.invalid_request");
}

/// The succession cut at the nest (`writer-signed-change-records.md` ruling
/// (11)(a)): once the owner's `folders.update` carries the re-minted nonce, a
/// record signed under it lands, and one signed under the retired nonce is
/// refused `signature_invalid` and writes nothing — the nest's copy selects
/// what verifies at ingest, and a record signed ahead of the push is retried,
/// never a planted row.
#[tokio::test]
async fn after_the_cut_a_record_under_the_new_nonce_lands_and_the_old_is_refused() {
    let (router, state, account) = nest().await;
    let actor = account.actor_id().0;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    let device_hex = fauna_core::hex32::encode(&[0x11; 32]);
    register_device(&router, &state, &account, &device_hex).await;
    record_as(
        &router,
        &state,
        &account,
        sign(
            record(&device_hex, "before.jpg", 0xa1, 10),
            &account,
            NONCE,
            account.signing_key(),
        ),
    )
    .await
    .expect("under the pre-cut nonce");

    let cut = [0x7f; 32];
    let early = sign(
        record(&device_hex, "early.jpg", 0xa2, 10),
        &account,
        cut,
        account.signing_key(),
    );
    assert_code(
        record_as(&router, &state, &account, early.clone())
            .await
            .expect_err("signed under the new nonce before the nest holds it"),
        "signature_invalid",
        "a record ahead of the push",
    );
    let _: FolderUpdateReply = call(
        &router,
        &state,
        actor,
        "fauna.folders.update",
        FolderUpdateRequest {
            name: SET.into(),
            set_nonce: Some(ByteBuf::from(cut.to_vec())),
            ..Default::default()
        },
    )
    .await
    .expect("the owner pushes the cut");

    record_as(&router, &state, &account, early)
        .await
        .expect("the same record, retried after the push, lands");
    assert_code(
        record_as(
            &router,
            &state,
            &account,
            sign(
                record(&device_hex, "late.jpg", 0xa3, 10),
                &account,
                NONCE,
                account.signing_key(),
            ),
        )
        .await
        .expect_err("signed under the retired nonce"),
        "signature_invalid",
        "a record under the retired nonce",
    );
    let page = list(&router, &state, &account).await;
    let mut verified = 0;
    for row in &page.changes {
        let certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
        if fauna_protocol::sync_writer_sig::verify_row(row, cut, &certs, |_| true).is_ok() {
            verified += 1;
        }
    }
    assert_eq!(page.changes.len(), 2, "the refused record wrote nothing");
    assert_eq!(
        verified, 1,
        "one row under the cut, one under the retired nonce"
    );
}

/// A direct signature (the identity key, `signer_key == actor`) verifies with
/// no cert and no grant; the row stores and projects both fields verbatim, and
/// the side table stays empty (a direct signer needs no cert).
#[tokio::test]
async fn a_direct_signature_is_stored_and_projected_without_a_cert() {
    let (router, state, account) = nest().await;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    let device_hex = fauna_core::hex32::encode(&[0x11; 32]);
    register_device(&router, &state, &account, &device_hex).await;

    let req = sign(
        record(&device_hex, "a.jpg", 0xa1, 10),
        &account,
        NONCE,
        account.signing_key(),
    );
    let (sig, key) = (req.signature.clone(), req.signer_key.clone());
    record_as(&router, &state, &account, req)
        .await
        .expect("a direct signature verifies");

    let page = list(&router, &state, &account).await;
    assert_eq!(page.changes.len(), 1);
    assert_eq!(
        page.changes[0].signature, sig,
        "the signature rides verbatim"
    );
    assert_eq!(page.changes[0].signer_key, key, "the key rides verbatim");
    assert!(
        page.signer_certs.is_empty(),
        "a direct signer needs no cert"
    );
    // The reader's check passes over exactly what the nest served.
    let certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    fauna_protocol::sync_writer_sig::verify_row(&page.changes[0], NONCE, &certs, |_| true)
        .expect("a reader verifies the served row");
}

/// A delegated signer resolves BY REFERENCE over the actor's registered grants
/// — the machine principal's `SyncWrite` grant — and its cert rides the list
/// reply's side table, where a reader's cache verifies the served row with it.
#[tokio::test]
async fn a_delegated_signer_resolves_by_reference_and_rides_the_side_table() {
    let (router, state, account) = nest().await;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    let (writer, device_hex) = enroll_principal(&router, &state, &account).await;

    for (i, path) in ["a.jpg", "b.jpg"].into_iter().enumerate() {
        let req = sign(
            record(&device_hex, path, 0xb0 + i as u8, 20),
            &account,
            NONCE,
            writer.signing_key(),
        );
        record_as(&router, &state, &account, req)
            .await
            .expect("the principal's SyncWrite grant verifies");
    }

    let page = list(&router, &state, &account).await;
    assert_eq!(page.changes.len(), 2);
    assert_eq!(
        page.signer_certs.len(),
        1,
        "one cert per DISTINCT delegated signer, however many rows it signed"
    );
    let mut certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    certs.ingest_all(&page.signer_certs);
    for row in &page.changes {
        fauna_protocol::sync_writer_sig::verify_row(row, NONCE, &certs, |a| {
            *a == account.actor_id().0
        })
        .expect("a reader verifies every served row from the side table alone");
    }
}

/// Every way a signed record can fail to verify is refused typed
/// `signature_invalid`, and nothing lands: an altered field, a statement bound
/// to another set's nonce, a set with no stored nonce, a key with no grant,
/// and a key whose grant does not carry `SyncWrite`.
#[tokio::test]
async fn an_unverifiable_signed_record_is_refused_and_writes_nothing() {
    let (router, state, account) = nest().await;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    let (writer, device_hex) = enroll_principal(&router, &state, &account).await;

    // An altered field: signed over 20 bytes, sent as 21.
    let mut altered = sign(
        record(&device_hex, "a.jpg", 0xc1, 20),
        &account,
        NONCE,
        writer.signing_key(),
    );
    altered.size_bytes = 21;
    assert_code(
        record_as(&router, &state, &account, altered)
            .await
            .unwrap_err(),
        "signature_invalid",
        "an altered field",
    );

    // Bound to another set's nonce — a record copied from another set, or
    // from a deleted predecessor of this one.
    let copied = sign(
        record(&device_hex, "a.jpg", 0xc2, 20),
        &account,
        [0xee; 32],
        writer.signing_key(),
    );
    assert_code(
        record_as(&router, &state, &account, copied)
            .await
            .unwrap_err(),
        "signature_invalid",
        "another set's nonce",
    );

    // A key the actor never registered.
    let stranger = ActorKeypair::generate();
    let unknown = sign(
        record(&device_hex, "a.jpg", 0xc3, 20),
        &account,
        NONCE,
        stranger.signing_key(),
    );
    assert_code(
        record_as(&router, &state, &account, unknown)
            .await
            .unwrap_err(),
        "signature_invalid",
        "an unregistered signer key",
    );

    // A `[RenewBearer]`-only grant authors nothing (deny-by-default).
    let renew_hex = fauna_core::hex32::encode(&[0x22; 32]);
    register_device(&router, &state, &account, &renew_hex).await;
    let (grant, seed) = common::fresh_device_grant(&account);
    let _: DeviceGrantRegisterReply = call(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.device_grant.register",
        DeviceGrantRegisterRequest {
            device_id: renew_hex.clone(),
            authorization: grant,
            extra: Default::default(),
        },
    )
    .await
    .expect("a renewal grant registers");
    let renew_only = sign(
        record(&renew_hex, "a.jpg", 0xc4, 20),
        &account,
        NONCE,
        &ed25519_dalek::SigningKey::from_bytes(&seed),
    );
    assert_code(
        record_as(&router, &state, &account, renew_only)
            .await
            .unwrap_err(),
        "signature_invalid",
        "a grant without SyncWrite",
    );

    // A set with no stored nonce cannot verify anything signed.
    create_set(&router, &state, &account, "bare", None).await;
    let mut bare = record(&device_hex, "a.jpg", 0xc5, 20);
    bare.folder = "bare".into();
    let bare = sign(bare, &account, NONCE, writer.signing_key());
    assert_code(
        record_as(&router, &state, &account, bare)
            .await
            .unwrap_err(),
        "signature_invalid",
        "a set with no stored nonce",
    );

    assert!(
        list(&router, &state, &account).await.changes.is_empty(),
        "no refused record landed a row"
    );
}

/// The switch is on: an unsigned record is refused `signature_required` and
/// lands nothing — whether or not the set has a stored nonce, and from a
/// device with a live `SyncWrite` grant alike — and a half-signed one (a
/// signature with no key) is `signature_invalid`. The same request, signed,
/// lands.
#[tokio::test]
async fn an_unsigned_record_is_refused_signature_required_and_writes_nothing() {
    let (router, state, account) = nest().await;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    create_set(&router, &state, &account, "bare", None).await;
    let (writer, device_hex) = enroll_principal(&router, &state, &account).await;

    assert_code(
        record_as(
            &router,
            &state,
            &account,
            record(&device_hex, "a.jpg", 0xd1, 20),
        )
        .await
        .unwrap_err(),
        "signature_required",
        "an unsigned record",
    );
    let mut bare = record(&device_hex, "a.jpg", 0xd2, 20);
    bare.folder = "bare".into();
    assert_code(
        record_as(&router, &state, &account, bare)
            .await
            .unwrap_err(),
        "signature_required",
        "an unsigned record on a set with no stored nonce",
    );
    let mut half = sign(
        record(&device_hex, "a.jpg", 0xd3, 20),
        &account,
        NONCE,
        writer.signing_key(),
    );
    half.signer_key = None;
    assert_code(
        record_as(&router, &state, &account, half)
            .await
            .unwrap_err(),
        "signature_invalid",
        "a signature without its signer key",
    );
    assert!(
        list(&router, &state, &account).await.changes.is_empty(),
        "no refused record landed a row"
    );

    let signed = sign(
        record(&device_hex, "a.jpg", 0xd1, 20),
        &account,
        NONCE,
        writer.signing_key(),
    );
    record_as(&router, &state, &account, signed)
        .await
        .expect("the same record, signed, lands");
    assert_eq!(list(&router, &state, &account).await.changes.len(), 1);
}

/// Deleting a device revokes its key at ingest (the tombstone), across every
/// row — yet the rows it signed while authorized keep projecting the cert they
/// verified under, so every reader can still verify them.
#[tokio::test]
async fn a_deleted_devices_key_is_refused_but_its_earlier_rows_stay_verifiable() {
    let (router, state, account) = nest().await;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    let (writer, device_hex) = enroll_principal(&router, &state, &account).await;
    // A second row carrying the SAME key (the macOS extension recording under
    // the app's device id): resolution is by key across rows, and the
    // tombstone must beat it too.
    let other_hex = fauna_core::hex32::encode(&[0x33; 32]);
    register_device(&router, &state, &account, &other_hex).await;

    let before = sign(
        record(&other_hex, "a.jpg", 0xd1, 20),
        &account,
        NONCE,
        writer.signing_key(),
    );
    record_as(&router, &state, &account, before)
        .await
        .expect("a row recorded under another device id resolves the key's grant");

    let _: SyncDeviceDeleteReply = call(
        &router,
        &state,
        account.actor_id().0,
        "fauna.sync.devices.delete",
        SyncDeviceDeleteRequest {
            device_id: device_hex.clone(),
            extra: Default::default(),
        },
    )
    .await
    .expect("device deletes");

    let after = sign(
        record(&other_hex, "b.jpg", 0xd2, 20),
        &account,
        NONCE,
        writer.signing_key(),
    );
    assert_code(
        record_as(&router, &state, &account, after)
            .await
            .unwrap_err(),
        "signature_invalid",
        "a revoked key",
    );

    let page = list(&router, &state, &account).await;
    assert_eq!(page.changes.len(), 1);
    let mut certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    certs.ingest_all(&page.signer_certs);
    fauna_protocol::sync_writer_sig::verify_row(&page.changes[0], NONCE, &certs, |_| true)
        .expect("the row authored while authorized stays verifiable");
}

/// A signature is part of a record's identity for exactly-once: a retry of
/// the same signed record is idempotent, while the same content re-signed
/// (the owner's re-record under the live nonce, custody (g)) — lands a NEW head
/// row a reader can verify. The unsigned twin of the same content is no head at
/// all: the switch refuses it, so the first signed record is the first row.
#[tokio::test]
async fn a_resigned_record_of_identical_content_lands_a_new_head() {
    let (router, state, account) = nest().await;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    let device_hex = fauna_core::hex32::encode(&[0x44; 32]);
    register_device(&router, &state, &account, &device_hex).await;

    assert_code(
        record_as(
            &router,
            &state,
            &account,
            record(&device_hex, "a.jpg", 0xe1, 20),
        )
        .await
        .unwrap_err(),
        "signature_required",
        "the unsigned twin of the content",
    );
    assert!(
        list(&router, &state, &account).await.changes.is_empty(),
        "the refused unsigned record lands no head"
    );
    let signed_req = sign(
        record(&device_hex, "a.jpg", 0xe1, 20),
        &account,
        NONCE,
        account.signing_key(),
    );
    let signed = record_as(&router, &state, &account, signed_req.clone())
        .await
        .unwrap();
    let retry = record_as(&router, &state, &account, signed_req)
        .await
        .unwrap();
    assert_eq!(
        retry, signed,
        "a retry of the same signed record is idempotent"
    );

    // Re-signed under a fresh nonce (the owner's reconcile pushed it first).
    let fresh = [0x5f; 32];
    let _: FolderUpdateReply = call(
        &router,
        &state,
        account.actor_id().0,
        "fauna.folders.update",
        FolderUpdateRequest {
            name: SET.into(),
            set_nonce: Some(ByteBuf::from(fresh.to_vec())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let resigned = record_as(
        &router,
        &state,
        &account,
        sign(
            record(&device_hex, "a.jpg", 0xe1, 20),
            &account,
            fresh,
            account.signing_key(),
        ),
    )
    .await
    .unwrap();
    assert!(
        resigned > signed,
        "the re-sign under the live nonce lands a new head"
    );
}

fn delete(device_hex: &str, path: &str) -> SyncChangeRecordRequest {
    SyncChangeRecordRequest {
        manifest_hash: None,
        size_bytes: 0,
        change_type: "delete".into(),
        ..record(device_hex, path, 0, 0)
    }
}

/// The delete twin of the re-sign above, through the nest-side idempotency
/// guard (`delete-propagation.md` § Deletes propagate the same way): a signed
/// delete over a signed tombstone is an echo only while that tombstone still
/// verifies under the set's STORED nonce. Under a nonce the owner's reconcile
/// has since retired, the tombstone verifies nowhere as current
/// (`writer-signed-change-records.md` custody (e)), so the re-record under the
/// live nonce (custody (g)) must land a new head — else every reader skips the
/// stale tombstone and resurrects what lies beneath it. Two signed deletes
/// under the live nonce still dedupe, whoever signed them.
#[tokio::test]
async fn a_resigned_delete_over_a_retired_nonce_tombstone_lands_a_new_head() {
    let (router, state, account) = nest().await;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    let device_hex = fauna_core::hex32::encode(&[0x45; 32]);
    register_device(&router, &state, &account, &device_hex).await;
    let (writer, writer_hex) = enroll_principal(&router, &state, &account).await;
    let signed = |req, nonce, key| sign(req, &account, nonce, key);

    record_as(
        &router,
        &state,
        &account,
        signed(
            record(&device_hex, "d.jpg", 0xd1, 20),
            NONCE,
            account.signing_key(),
        ),
    )
    .await
    .unwrap();
    let tombstone = record_as(
        &router,
        &state,
        &account,
        signed(delete(&device_hex, "d.jpg"), NONCE, account.signing_key()),
    )
    .await
    .unwrap();

    // An echo under the same nonce, signed by another key of the account:
    // signatures differ, the tombstone still verifies — no new row.
    let echo = record_as(
        &router,
        &state,
        &account,
        signed(delete(&writer_hex, "d.jpg"), NONCE, writer.signing_key()),
    )
    .await
    .unwrap();
    assert_eq!(
        echo, tombstone,
        "a signed echo-delete under the live nonce dedupes onto the tombstone"
    );

    // The owner's reconcile retires the nonce and re-records the delete.
    let fresh = [0x5e; 32];
    let _: FolderUpdateReply = call(
        &router,
        &state,
        account.actor_id().0,
        "fauna.folders.update",
        FolderUpdateRequest {
            name: SET.into(),
            set_nonce: Some(ByteBuf::from(fresh.to_vec())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let resigned_req = signed(delete(&device_hex, "d.jpg"), fresh, account.signing_key());
    let resigned = record_as(&router, &state, &account, resigned_req.clone())
        .await
        .unwrap();
    assert!(
        resigned > tombstone,
        "the delete re-signed under the live nonce lands a new head, not the \
         retired tombstone's seq ({resigned} vs {tombstone})"
    );
    let retry = record_as(&router, &state, &account, resigned_req)
        .await
        .unwrap();
    assert_eq!(
        retry, resigned,
        "a retry of the re-signed delete is idempotent"
    );
    let echo = record_as(
        &router,
        &state,
        &account,
        signed(delete(&writer_hex, "d.jpg"), fresh, writer.signing_key()),
    )
    .await
    .unwrap();
    assert_eq!(
        echo, resigned,
        "and an echo under the live nonce dedupes onto the new tombstone"
    );
}

/// The choose-winner head row the nest mints is signed by the CHOOSER over
/// exactly that row — the winning candidate's device, size and generation, the
/// conflict's path hash and seal, `modify`, no causal stamp — and the nest
/// verifies it as at record and copies the pair onto the minted row (ruling
/// (1)(ii)). A signature over anything else refuses the resolve and leaves the
/// conflict open.
#[tokio::test]
async fn the_choose_winner_head_row_carries_the_choosers_verified_signature() {
    use fauna_protocol::folders::{ConflictResolveReply, ConflictResolveRequest};
    let (router, state, account) = nest().await;
    let actor = account.actor_id().0;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    let folder_id = state
        .db
        .get_folder_for_actor(SET, &actor)
        .await
        .unwrap()
        .expect("set")
        .id;
    let winner_device = [0x5d; 32];
    let win = [0x77; 32];
    let seal = b"conflict-seal".to_vec();
    let conflict_id = state
        .db
        .report_conflict(
            &actor,
            folder_id,
            &winner_device,
            "doc.txt",
            "modify-modify",
            None,
            &[fauna_nest::db::ConflictCandidateRow {
                manifest_hash: win.to_vec(),
                device_id: winner_device.to_vec(),
                size_bytes: 99,
                created_at: 0,
                content_key_version: Some(2),
            }],
            None,
            fauna_nest::db::SealedConflictLabels {
                path_sealed: Some(seal.clone()),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();

    // The statement a chooser's engine signs: the winner row as minted.
    let statement = |size: i64| SignedChange {
        set_nonce: NONCE,
        actor_id: actor,
        device_id: winner_device,
        path_hash: fauna_core::sync::path_hash("doc.txt"),
        manifest_hash: Some(win),
        change_type: "modify".into(),
        size_bytes: size,
        content_key_version: Some(2),
        path_sealed: Some(seal.clone()),
        thumbnail_hash: None,
        derived_through: None,
        is_resolution: false,
        is_retention: false,
    };
    let resolve = |sig: [u8; 64]| ConflictResolveRequest {
        id: conflict_id,
        winning_manifest_hash: Some(hex::encode(win)),
        winner_signature: Some(ByteBuf::from(sig.to_vec())),
        winner_signer_key: Some(ByteBuf::from(actor.to_vec())),
        ..Default::default()
    };

    // Unsigned: refused whole, the conflict stays open.
    let unsigned = call::<_, ConflictResolveReply>(
        &router,
        &state,
        actor,
        "fauna.sync.conflicts.resolve",
        ConflictResolveRequest {
            winner_signature: None,
            winner_signer_key: None,
            ..resolve([0; 64])
        },
    )
    .await
    .expect_err("an unsigned choose refuses");
    assert_code(unsigned, "signature_required", "an unsigned choose-winner");

    let wrong = call::<_, ConflictResolveReply>(
        &router,
        &state,
        actor,
        "fauna.sync.conflicts.resolve",
        resolve(statement(100).sign(account.signing_key())),
    )
    .await
    .expect_err("a signature over another row refuses");
    assert_code(
        wrong,
        "signature_invalid",
        "a signature over the wrong size",
    );

    let sig = statement(99).sign(account.signing_key());
    let ok: ConflictResolveReply = call(
        &router,
        &state,
        actor,
        "fauna.sync.conflicts.resolve",
        resolve(sig),
    )
    .await
    .expect("the refused attempt left the conflict open; the right signature resolves");
    assert!(ok.resolved);

    let page = list(&router, &state, &account).await;
    let head = page.changes.last().expect("the winner head row");
    assert_eq!(head.signature.as_deref().map(Vec::as_slice), Some(&sig[..]));
    let certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    fauna_protocol::sync_writer_sig::verify_row(head, NONCE, &certs, |_| true)
        .expect("a reader verifies the minted winner row");
}

/// The resolved report's winner head row is signed by the REPORTER over
/// exactly that row (ruling (1)(ii), the resolved-report clause): `device_id`
/// = the winner's (the reporter's own for a merged, non-candidate winner),
/// the request's size, generation, path hash and seal, `modify`, the claim
/// `winning_derived_through` AS SENT — the nest's winner-stamp upgrade is
/// retired, so no nest-assigned seq enters the statement — and `is_resolution
/// = !winning_carries_novelty`. The nest verifies it as at record, refuses a
/// mismatch whole (nothing lands), and copies the pair onto the minted winner.
/// The retained loser is signed too (ruling (10)(d)), by the same signer, over
/// the retention row as the nest mints it: none → `signature_required`, a
/// wrong one → `signature_invalid`, a valid one is stored on the retention row
/// — which the fold still exempts by class.
#[tokio::test]
async fn the_resolved_reports_winner_row_carries_the_reporters_verified_signature() {
    use fauna_protocol::folders::{ConflictCandidate, ConflictReportReply, ConflictReportRequest};
    use fauna_protocol::sync_writer_sig::{ExemptClass, exempt_class};
    let (router, state, account) = nest().await;
    let actor = account.actor_id().0;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    let reporter = [0x3a; 32];
    let merged = [0x66; 32];
    let seal = b"report-seal".to_vec();
    let path = "notes.txt";

    let statement = |derived_through: i64, is_resolution: bool| SignedChange {
        set_nonce: NONCE,
        actor_id: actor,
        device_id: reporter,
        path_hash: fauna_core::sync::path_hash(path),
        manifest_hash: Some(merged),
        change_type: "modify".into(),
        size_bytes: 15,
        content_key_version: Some(4),
        path_sealed: Some(seal.clone()),
        thumbnail_hash: None,
        derived_through: Some(derived_through),
        is_resolution,
        is_retention: false,
    };
    // The retention row the nest mints for the reporter's own losing
    // candidate (0x11): its device, manifest and size, the request's path hash
    // and seal, `modify`, `losing_derived_through`, the flag.
    let loser_statement = |is_retention: bool| SignedChange {
        set_nonce: NONCE,
        actor_id: actor,
        device_id: reporter,
        path_hash: fauna_core::sync::path_hash(path),
        manifest_hash: Some([0x11; 32]),
        change_type: "modify".into(),
        size_bytes: 10,
        content_key_version: None,
        path_sealed: Some(seal.clone()),
        thumbnail_hash: None,
        derived_through: Some(3),
        is_resolution: false,
        is_retention,
    };
    let loser_sig = loser_statement(true).sign(account.signing_key());
    let report = |sig: [u8; 64]| ConflictReportRequest {
        folder: SET.into(),
        device_id: hex::encode(reporter),
        path: path.into(),
        path_hash: Some(ByteBuf::from(fauna_core::sync::path_hash(path).to_vec())),
        path_sealed: Some(ByteBuf::from(seal.clone())),
        conflict_type: "concurrent_edit".into(),
        candidates: vec![
            ConflictCandidate {
                manifest_hash: hex::encode([0x11; 32]),
                device_id: hex::encode(reporter),
                size_bytes: 10,
                created_at: 100,
                ..Default::default()
            },
            ConflictCandidate {
                manifest_hash: hex::encode([0x22; 32]),
                device_id: hex::encode([0x4b; 32]),
                size_bytes: 20,
                created_at: 200,
                ..Default::default()
            },
        ],
        resolution: Some("merged".into()),
        winning_manifest_hash: Some(hex::encode(merged)),
        winning_size_bytes: Some(15),
        winning_content_key_version: Some(4),
        winning_derived_through: Some(7),
        losing_derived_through: Some(3),
        winning_carries_novelty: Some(true),
        winner_signature: Some(ByteBuf::from(sig.to_vec())),
        winner_signer_key: Some(ByteBuf::from(actor.to_vec())),
        loser_signature: Some(ByteBuf::from(loser_sig.to_vec())),
        ..Default::default()
    };

    // Unsigned: refused whole, before anything is minted.
    let unsigned = call::<_, ConflictReportReply>(
        &router,
        &state,
        actor,
        "fauna.sync.conflicts.report",
        ConflictReportRequest {
            winner_signature: None,
            winner_signer_key: None,
            loser_signature: None,
            ..report([0; 64])
        },
    )
    .await
    .expect_err("an unsigned resolved report refuses");
    assert_code(
        unsigned,
        "signature_required",
        "an unsigned resolved report",
    );

    // A signature over another row refuses the whole report: over the claim
    // the retired upgrade would have minted, and over the wrong class.
    for (w, res, what) in [
        (8, false, "a signature over an upgraded claim"),
        (
            7,
            true,
            "a signature over the resolution class of an edit-class winner",
        ),
    ] {
        let err = call::<_, ConflictReportReply>(
            &router,
            &state,
            actor,
            "fauna.sync.conflicts.report",
            report(statement(w, res).sign(account.signing_key())),
        )
        .await
        .expect_err(what);
        assert_code(err, "signature_invalid", what);
    }
    assert!(
        list(&router, &state, &account).await.changes.is_empty(),
        "a refused report lands no row"
    );

    let sig = statement(7, false).sign(account.signing_key());

    // Ruling (10)(d): a resolved report that retains a loser and carries no
    // loser signature refuses `signature_required`; one over another row (the
    // loser signed as an ordinary edit — the flag stripped) `signature_invalid`.
    let unsigned_loser = call::<_, ConflictReportReply>(
        &router,
        &state,
        actor,
        "fauna.sync.conflicts.report",
        ConflictReportRequest {
            loser_signature: None,
            ..report(sig)
        },
    )
    .await
    .expect_err("a retained loser with no signature refuses");
    assert_code(
        unsigned_loser,
        "signature_required",
        "a resolved report whose retained loser is unsigned",
    );
    let stripped = call::<_, ConflictReportReply>(
        &router,
        &state,
        actor,
        "fauna.sync.conflicts.report",
        ConflictReportRequest {
            loser_signature: Some(ByteBuf::from(
                loser_statement(false).sign(account.signing_key()).to_vec(),
            )),
            ..report(sig)
        },
    )
    .await
    .expect_err("a loser signed without its flag refuses");
    assert_code(
        stripped,
        "signature_invalid",
        "a loser signature over an ordinary row",
    );
    assert!(
        list(&router, &state, &account).await.changes.is_empty(),
        "a refused report lands no row"
    );

    let _: ConflictReportReply = call(
        &router,
        &state,
        actor,
        "fauna.sync.conflicts.report",
        report(sig),
    )
    .await
    .expect("the reporter's signatures over the minted rows land the report");

    let page = list(&router, &state, &account).await;
    assert_eq!(
        page.changes.len(),
        2,
        "the loser retention row + the winner head row"
    );
    let (loser, winner) = (&page.changes[0], &page.changes[1]);
    assert_eq!(
        winner.signature.as_deref().map(Vec::as_slice),
        Some(&sig[..])
    );
    assert_eq!(
        winner.signer_key.as_deref().map(Vec::as_slice),
        Some(&actor[..])
    );
    assert_eq!(winner.derived_through, Some(7), "minted exactly as signed");
    let certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    fauna_protocol::sync_writer_sig::verify_row(winner, NONCE, &certs, |_| true)
        .expect("a reader verifies the minted winner row");
    assert_eq!(
        loser.signature.as_deref().map(Vec::as_slice),
        Some(&loser_sig[..]),
        "the retention row carries the reporter's loser signature"
    );
    assert_eq!(
        loser.signer_key.as_deref().map(Vec::as_slice),
        Some(&actor[..])
    );
    assert_eq!(loser.is_retention, Some(true));
    fauna_protocol::sync_writer_sig::verify_row(loser, NONCE, &certs, |a| *a == actor)
        .expect("a reader verifies the minted retention row as its reporter");
    assert_eq!(
        exempt_class(loser),
        Some(ExemptClass::Retention),
        "the fold still exempts it by class"
    );

    // Ruling (10)(e): the version history serves the retained loser with its
    // flag, signature and key, and the projection judge admits it as its
    // reporter.
    let versions: fauna_protocol::files::FilesVersionsListReply = call(
        &router,
        &state,
        actor,
        "fauna.files.versions.list",
        fauna_protocol::files::FilesVersionsListRequest {
            path_hash: ByteBuf::from(fauna_core::sync::path_hash(path).to_vec()),
            folder: Some(SET.into()),
            include_pruned: None,
            ..Default::default()
        },
    )
    .await
    .expect("versions.list");
    let retained = versions
        .versions
        .iter()
        .find(|v| v.manifest_hash[..] == [0x11; 32])
        .expect("the retained loser is a listed version");
    assert_eq!(retained.is_retention, Some(true));
    assert_eq!(
        retained.signature.as_deref().map(Vec::as_slice),
        Some(&loser_sig[..])
    );
    assert_eq!(
        retained.signer_key.as_deref().map(Vec::as_slice),
        Some(&actor[..])
    );
    let mut reader = RowReader::new();
    reader.install_binding(ReaderBinding {
        set_nonce: Some(NONCE),
        owner: Some(actor),
        ..Default::default()
    });
    assert!(
        matches!(
            reader.judge_projection(retained.as_change_row().as_ref()),
            RowVerdict::Verified { writer, .. } if writer == actor
        ),
        "the judged version history admits the retained loser as its reporter"
    );
}

/// The engine's own conflict funnel round-trips (item 3d): a delegated machine
/// principal signs a resolved report through `ChangeSigner::sign_report` — the
/// call every reporter makes (the engine's `report_conflict_ws`,
/// `SyncClient::sign_report`) — winner and retained loser both, and a
/// choose-winner through `ChangeSigner::sign_choose_winner` over
/// the conflict exactly as `conflicts.list` serves it. The nest rebuilds both
/// statements from its own rows, so a field the client derives differently
/// from the nest (the winning candidate's device, size and generation; the
/// served path hash and seal) refuses here.
#[tokio::test]
async fn the_engines_signed_conflict_requests_land_verified_winner_rows() {
    use fauna_protocol::folders::{
        ConflictCandidate, ConflictReportReply, ConflictReportRequest, ConflictResolveReply,
        ConflictResolveRequest, ConflictsListReply, ConflictsListRequest,
    };
    use fauna_protocol::sync_writer_sig::ChangeSigner;
    let (router, state, account) = nest().await;
    let actor = account.actor_id().0;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    let (writer, device_hex) = enroll_principal(&router, &state, &account).await;
    let grant = fauna_client_sync::build_principal_grant(&account, &writer.actor_id().0)
        .expect("grant builds");
    let signer = ChangeSigner::delegated(actor, writer.signing_key().clone(), grant);
    let other_device = hex::encode([0x4b; 32]);
    let report = |path: &str, resolved: bool| ConflictReportRequest {
        folder: SET.into(),
        device_id: device_hex.clone(),
        path: path.into(),
        path_hash: Some(ByteBuf::from(fauna_core::sync::path_hash(path).to_vec())),
        path_sealed: Some(ByteBuf::from(format!("seal:{path}").into_bytes())),
        conflict_type: "concurrent_edit".into(),
        candidates: vec![
            ConflictCandidate {
                manifest_hash: hex::encode([0x11; 32]),
                device_id: device_hex.clone(),
                size_bytes: 10,
                created_at: 100,
                content_key_version: Some(2),
                ..Default::default()
            },
            ConflictCandidate {
                manifest_hash: hex::encode([0x22; 32]),
                device_id: other_device.clone(),
                size_bytes: 20,
                created_at: 200,
                content_key_version: Some(3),
                ..Default::default()
            },
        ],
        resolution: resolved.then(|| "latest_wins".into()),
        winning_manifest_hash: resolved.then(|| hex::encode([0x22; 32])),
        winning_derived_through: resolved.then_some(5),
        ..Default::default()
    };
    let mut certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();

    // A resolved report whose winner is the OTHER device's candidate: the
    // statement carries that candidate's device, size and generation.
    let mut resolved = report("a.txt", true);
    signer.sign_report(&mut resolved, NONCE).unwrap();
    let _: ConflictReportReply = call(
        &router,
        &state,
        actor,
        "fauna.sync.conflicts.report",
        resolved,
    )
    .await
    .expect("the engine-signed report lands");
    let page = list(&router, &state, &account).await;
    certs.ingest_all(&page.signer_certs);
    let winner = page.changes.last().expect("the winner head row");
    assert_eq!(winner.device_id.as_deref(), Some(other_device.as_str()));
    fauna_protocol::sync_writer_sig::verify_row(winner, NONCE, &certs, |a| *a == actor)
        .expect("a reader verifies the engine-signed winner row");
    // The engine's own candidate lost: `sign_report` signed its retention row
    // too (ruling (10)(d)), and the nest's pick agrees with the signer's.
    let retained = &page.changes[page.changes.len() - 2];
    assert_eq!(retained.is_retention, Some(true));
    assert_eq!(retained.device_id.as_deref(), Some(device_hex.as_str()));
    fauna_protocol::sync_writer_sig::verify_row(retained, NONCE, &certs, |a| *a == actor)
        .expect("a reader verifies the engine-signed retention row");

    // An unresolved report, then the owner's choose-winner over the conflict
    // as `conflicts.list` serves it.
    let mut open = report("b.txt", false);
    signer.sign_report(&mut open, NONCE).unwrap();
    assert_eq!(
        open.winner_signature, None,
        "an unresolved report mints no winner"
    );
    let opened: ConflictReportReply =
        call(&router, &state, actor, "fauna.sync.conflicts.report", open)
            .await
            .expect("the unresolved report lands");
    let listed: ConflictsListReply = call(
        &router,
        &state,
        actor,
        "fauna.sync.conflicts.list",
        ConflictsListRequest::default(),
    )
    .await
    .expect("list conflicts");
    let conflict = listed
        .conflicts
        .iter()
        .find(|c| c.id == opened.id)
        .expect("the open conflict is listed");
    let mut choose = ConflictResolveRequest {
        id: opened.id,
        winning_manifest_hash: Some(hex::encode([0x11; 32])),
        ..Default::default()
    };
    signer
        .sign_choose_winner(&mut choose, conflict, NONCE)
        .unwrap();
    let resolved: ConflictResolveReply = call(
        &router,
        &state,
        actor,
        "fauna.sync.conflicts.resolve",
        choose,
    )
    .await
    .expect("the engine-signed choose-winner resolves");
    assert!(resolved.resolved);
    let page = list(&router, &state, &account).await;
    certs.ingest_all(&page.signer_certs);
    let head = page.changes.last().expect("the choose-winner head row");
    assert!(head.signature.is_some());
    fauna_protocol::sync_writer_sig::verify_row(head, NONCE, &certs, |a| *a == actor)
        .expect("a reader verifies the engine-signed choose-winner row");
}

/// `fauna.media.list` — web's Media reader's only row source — carries the head
/// row's whole signed statement plus the page's `signer_certs` side table, so
/// the reader verifies an item through the same `verify_row` every
/// `changes.list` reader runs (`MediaItem::as_change_row`).
#[tokio::test]
async fn a_media_item_carries_its_head_rows_signed_statement() {
    use fauna_protocol::media::{MEDIA_LIST_CURSOR_V2, MediaListReply, MediaListRequest};
    let (router, state, account) = nest().await;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    let (writer, device_hex) = enroll_principal(&router, &state, &account).await;
    let req = sign(
        record(&device_hex, "pic.jpg", 0xf1, 30),
        &account,
        NONCE,
        writer.signing_key(),
    );
    record_as(&router, &state, &account, req).await.unwrap();

    let page: MediaListReply = call(
        &router,
        &state,
        account.actor_id().0,
        "fauna.media.list",
        MediaListRequest {
            cursor_version: MEDIA_LIST_CURSOR_V2,
            ..Default::default()
        },
    )
    .await
    .expect("media.list");
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.signer_certs.len(),
        1,
        "the delegated signer's cert rides"
    );
    let row = page.items[0]
        .as_change_row()
        .expect("the owner is the label audience: the statement rides");
    let mut certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    certs.ingest_all(&page.signer_certs);
    fauna_protocol::sync_writer_sig::verify_row(&row, NONCE, &certs, |a| {
        *a == account.actor_id().0
    })
    .expect("the Media reader verifies the item exactly as a changes.list reader");
}

/// The version-history projection carries each version row's whole signed
/// statement and a `signer_certs` side table, so a restore verifies the
/// version it re-points (`FileVersionInfo::as_change_row`).
#[tokio::test]
async fn every_listed_version_carries_its_rows_signed_statement() {
    use fauna_protocol::files::{FilesVersionsListReply, FilesVersionsListRequest};
    let (router, state, account) = nest().await;
    create_set(&router, &state, &account, SET, Some(NONCE)).await;
    let (writer, device_hex) = enroll_principal(&router, &state, &account).await;
    for (manifest, size) in [(0x91u8, 10i64), (0x92, 11)] {
        let mut req = record(&device_hex, "doc.txt", manifest, size);
        req.change_type = if manifest == 0x91 { "create" } else { "modify" }.into();
        let req = sign(req, &account, NONCE, writer.signing_key());
        record_as(&router, &state, &account, req).await.unwrap();
    }
    let reply: FilesVersionsListReply = call(
        &router,
        &state,
        account.actor_id().0,
        "fauna.files.versions.list",
        FilesVersionsListRequest {
            path_hash: ByteBuf::from(fauna_core::sync::path_hash("doc.txt").to_vec()),
            folder: Some(SET.into()),
            include_pruned: None,
            ..Default::default()
        },
    )
    .await
    .expect("versions.list");
    assert_eq!(reply.versions.len(), 2);
    assert_eq!(reply.signer_certs.len(), 1);
    let mut certs = fauna_protocol::sync_writer_sig::SignerCertCache::new();
    certs.ingest_all(&reply.signer_certs);
    for v in &reply.versions {
        let row = v.as_change_row().expect("the statement rides");
        fauna_protocol::sync_writer_sig::verify_row(&row, NONCE, &certs, |_| true)
            .expect("every version verifies");
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Ruling (8): rows signed under a retired identity — the nest's half
// ═════════════════════════════════════════════════════════════════════════════
//
// `mls-group-key-material.md` § M2 → *Writer-signed change records*, ruling
// (8)(b) source (i): the roster read carries, on the owner row and each
// `writer` row, the landed succession statements whose chain ends at that
// member — verbatim, oldest first — and the nest's record half keeps serving
// the cert a moved row verifies under.
//
// A succession ceremony alone seats nobody: `actor_channels` and
// `folder_member_access` both STAY with the retired id (`actor_tables.rs`,
// the 2026-08-14 succession rulings). So every test below reaches the state
// it asserts on through the real legs — the successor's own registration on
// the channel it now claims, the owner's Welcome for a member's successor, and
// the owner's `set_access` re-grant.

use fauna_core::recovery::{RecoveryKey, proven_predecessors_carried};
use fauna_protocol::conversations::{
    ChannelFetchReply, ChannelFetchRequest, WelcomeDeliverReply, WelcomeDeliverRequest, WelcomeKind,
};
use fauna_protocol::folders::{
    ActorMembersListReply, ActorMembersListRequest, FolderActorMember, FolderShareReply,
    FolderShareRequest, MemberSetAccessReply, MemberSetAccessRequest,
};
use fauna_protocol::sync_row_verify::{ReaderBinding, RowReader, RowVerdict, writer_roster};
use fauna_protocol::sync_writer_sig::{ChangeSigner, ChangeVerifyError};

const GROUP: [u8; 24] = [0x6b; 24];

/// A member account on this nest.
async fn member(state: &Arc<AppState>) -> ActorKeypair {
    let kp = ActorKeypair::generate();
    common::seed_dispatch_actor(&state.db, &kp.actor_id().0).await;
    kp
}

/// The owner shares [`SET`]: binds it to [`GROUP`] and claims the derived
/// channel. Returns the channel id (hex).
async fn share_set(router: &RpcRouter, state: &Arc<AppState>, owner: &ActorKeypair) -> String {
    let reply: FolderShareReply = call(
        router,
        state,
        owner.actor_id().0,
        "fauna.folders.share",
        FolderShareRequest {
            name: SET.into(),
            group_id: hex::encode(GROUP),
            ..Default::default()
        },
    )
    .await
    .expect("share");
    assert!(reply.ok);
    reply.channel_id
}

/// The owner (the channel's claimant) admits `recipient` — the real Welcome
/// delivery, whose roster registration is the only leg that seats a member.
async fn welcome(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: &ActorKeypair,
    channel: &str,
    recipient: &ActorKeypair,
) {
    let _: WelcomeDeliverReply = call(
        router,
        state,
        owner.actor_id().0,
        "fauna.conversations.welcome.deliver",
        WelcomeDeliverRequest {
            recipient_actor_id: recipient.actor_id().to_hex(),
            channel_id: channel.into(),
            welcome_bytes: vec![0x01, 0x02, 0x03],
            kind: WelcomeKind::Folder {
                group_id: hex::encode(GROUP),
            },
            nest_url: None,
            extra: Default::default(),
        },
    )
    .await
    .expect("the claimant's Welcome delivers");
}

/// The owner's successor takes its own seat: the claim moved to it with the
/// set, so its first read of the channel registers it (claimant-gated).
async fn register_self(
    router: &RpcRouter,
    state: &Arc<AppState>,
    successor: &ActorKeypair,
    channel: &str,
) {
    let _: ChannelFetchReply = call(
        router,
        state,
        successor.actor_id().0,
        "fauna.conversations.channel.fetch",
        ChannelFetchRequest {
            channel_id: channel.into(),
            after: 0,
            limit: 100,
            nest_url: None,
            extra: Default::default(),
        },
    )
    .await
    .expect("the claimant reads its channel");
}

async fn set_access(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: &ActorKeypair,
    grantee: [u8; 32],
    access: &str,
) {
    let reply: MemberSetAccessReply = call(
        router,
        state,
        owner.actor_id().0,
        "fauna.folders.members.set_access",
        MemberSetAccessRequest {
            name: SET.into(),
            actor_id: hex::encode(grantee),
            access: access.into(),
            ..Default::default()
        },
    )
    .await
    .expect("set_access");
    assert!(reply.ok);
}

async fn grant_writer(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: &ActorKeypair,
    grantee: [u8; 32],
) {
    set_access(router, state, owner, grantee, "writer").await;
}

/// The actor-roster read, as `caller`.
async fn roster(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: &ActorKeypair,
) -> Vec<FolderActorMember> {
    let reply: ActorMembersListReply = call(
        router,
        state,
        caller.actor_id().0,
        "fauna.folders.members.list_actors",
        ActorMembersListRequest {
            name: SET.into(),
            ..Default::default()
        },
    )
    .await
    .expect("list_actors");
    reply.members
}

fn seat(members: &[FolderActorMember], who: [u8; 32]) -> Option<&FolderActorMember> {
    members.iter().find(|m| m.actor_id == hex::encode(who))
}

fn carried(members: &[FolderActorMember], who: [u8; 32]) -> Vec<Vec<u8>> {
    seat(members, who)
        .unwrap_or_else(|| panic!("{} is on the roster", hex::encode(who)))
        .succession_statements
        .iter()
        .map(|s| s.to_vec())
        .collect()
}

/// Succeed `old` through the real ceremony — a registered RecoveryKey, then
/// the pre-identity `fauna.recovery.succession.submit`. Returns the successor
/// and the verbatim statement bytes that landed.
async fn succeed(
    router: &RpcRouter,
    state: &Arc<AppState>,
    old: &ActorKeypair,
) -> (ActorKeypair, Vec<u8>) {
    let recovery = RecoveryKey::generate();
    common::register_recovery_key(
        router,
        state,
        old.signing_key(),
        old.actor_id().0,
        &recovery,
        None,
        1,
    )
    .await;
    let new = ActorKeypair::generate();
    let bytes = common::succession_bytes(&recovery, old.actor_id().0, new.signing_key(), None, 2);
    common::submit_succession(router, state, bytes.clone())
        .await
        .expect("the succession lands");
    (new, bytes)
}

/// A peer-learned link `old` → `new`, as federation delivers it: a real signed
/// statement, landed by the peer half (`record_peer_succession`), which
/// re-points `channel_foreign_members`.
async fn peer_succession(state: &Arc<AppState>, old: [u8; 32], new: &ActorKeypair) -> Vec<u8> {
    let bytes = common::succession_bytes(&RecoveryKey::generate(), old, new.signing_key(), None, 2);
    assert!(
        state
            .db
            .record_peer_succession(&old, &new.actor_id().0, &bytes, 2)
            .await
            .unwrap()
            .is_ok(),
        "the peer link lands"
    );
    bytes
}

/// A local owner's succession: the owner row carries the chain that ends at
/// it, verbatim and oldest first — one statement after one hop, both after
/// two — and the chain is exactly what the shared verifier needs to prove the
/// predecessors.
#[tokio::test]
async fn the_owner_row_carries_the_owners_succession_chain_oldest_first() {
    let (router, state, a0) = nest().await;
    create_set(&router, &state, &a0, SET, Some(NONCE)).await;
    let channel = share_set(&router, &state, &a0).await;

    let (a1, s1) = succeed(&router, &state, &a0).await;
    // The ceremony alone seats nobody: the set and its claim moved to the
    // successor, the roster seat did not.
    let members = roster(&router, &state, &a1).await;
    assert!(
        seat(&members, a1.actor_id().0).is_none(),
        "the successor's seat waits for its own registration: {members:?}"
    );
    assert!(
        members.iter().all(|m| m.succession_statements.is_empty()),
        "no owner row yet, so nothing is carried: {members:?}"
    );

    register_self(&router, &state, &a1, &channel).await;
    let members = roster(&router, &state, &a1).await;
    assert_eq!(seat(&members, a1.actor_id().0).unwrap().role, "owner");
    assert_eq!(
        carried(&members, a1.actor_id().0),
        vec![s1.clone()],
        "the owner row carries the one landed statement, verbatim"
    );
    assert_eq!(
        proven_predecessors_carried(
            &a1.actor_id(),
            &seat(&members, a1.actor_id().0)
                .unwrap()
                .succession_statements
        ),
        vec![a0.actor_id()],
        "which is what proves the predecessor to a reader"
    );
    assert!(
        carried(&members, a0.actor_id().0).is_empty(),
        "the retired id's stale seat is a reader-access row: it carries none"
    );

    // A second hop: both statements, oldest first.
    let (a2, s2) = succeed(&router, &state, &a1).await;
    register_self(&router, &state, &a2, &channel).await;
    let members = roster(&router, &state, &a2).await;
    assert_eq!(carried(&members, a2.actor_id().0), vec![s1, s2]);
    assert_eq!(
        proven_predecessors_carried(
            &a2.actor_id(),
            &seat(&members, a2.actor_id().0)
                .unwrap()
                .succession_statements
        ),
        vec![a1.actor_id(), a0.actor_id()],
        "nearest hop first"
    );
}

/// A local writer member's succession: once the owner's Welcome has seated the
/// successor — which carries the grant, with no re-grant — its `writer` row
/// carries the statement; a reader-access member's row carries none (the read discloses a retired id
/// only for a writer); and a member reads exactly what the owner reads.
#[tokio::test]
async fn a_writer_row_carries_its_statement_and_a_reader_row_carries_none() {
    let (router, state, owner) = nest().await;
    create_set(&router, &state, &owner, SET, Some(NONCE)).await;
    let channel = share_set(&router, &state, &owner).await;
    let (w0, r0) = (member(&state).await, member(&state).await);
    for m in [&w0, &r0] {
        welcome(&router, &state, &owner, &channel, m).await;
    }
    grant_writer(&router, &state, &owner, w0.actor_id().0).await;

    let (w1, sw) = succeed(&router, &state, &w0).await;
    let (r1, _) = succeed(&router, &state, &r0).await;

    // Seat and grant both stay with the retired id until the Welcome lands.
    let members = roster(&router, &state, &owner).await;
    assert!(seat(&members, w1.actor_id().0).is_none());
    assert_eq!(
        seat(&members, w0.actor_id().0).unwrap().access.as_deref(),
        Some("writer")
    );
    assert!(
        carried(&members, w0.actor_id().0).is_empty(),
        "nobody succeeded INTO the retired id"
    );

    welcome(&router, &state, &owner, &channel, &w1).await;
    welcome(&router, &state, &owner, &channel, &r1).await;

    let members = roster(&router, &state, &owner).await;
    for retired in [&w0, &r0] {
        assert!(
            seat(&members, retired.actor_id().0).is_none(),
            "the Welcome that seats the successor retires the predecessor's seat"
        );
    }
    assert_eq!(
        seat(&members, w1.actor_id().0).unwrap().access.as_deref(),
        Some("writer"),
        "the grant followed the seat"
    );
    assert_eq!(carried(&members, w1.actor_id().0), vec![sw]);
    assert_eq!(
        proven_predecessors_carried(
            &w1.actor_id(),
            &seat(&members, w1.actor_id().0)
                .unwrap()
                .succession_statements
        ),
        vec![w0.actor_id()]
    );
    assert_eq!(
        seat(&members, r1.actor_id().0).unwrap().access.as_deref(),
        Some("reader")
    );
    assert!(
        carried(&members, r1.actor_id().0).is_empty(),
        "a reader-access row carries no statement"
    );
    assert_eq!(
        roster(&router, &state, &r1).await,
        members,
        "a member's read is the owner's read"
    );
}

/// A cross-nest writer's succession, delivered by federation. The peer half
/// re-points the roster (`channel_foreign_members`) and carries the grant with
/// it (ruling (8)(j)(3)), so the successor projects `writer` and its row
/// carries the peer-learned statement with no act by the owner, and no grant
/// row is left under the retired id. And a second peer-learned link naming
/// the same successor is refused at the door (ruling (8)(j)(1)): the
/// first-landed link stands and the row's chain is unchanged.
#[tokio::test]
async fn a_cross_nest_writers_successor_carries_the_grant_and_the_peer_statement() {
    let (router, state, owner) = nest().await;
    create_set(&router, &state, &owner, SET, Some(NONCE)).await;
    let channel = share_set(&router, &state, &owner).await;
    let channel_id = fauna_core::hex32::decode(&channel).unwrap();

    let f0 = ActorKeypair::generate().actor_id().0;
    state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &f0,
            &[0x77u8; 32],
            None,
            fauna_nest::db::channels::RebindPower::Standing,
        )
        .await
        .unwrap();
    grant_writer(&router, &state, &owner, f0).await;

    let f1 = ActorKeypair::generate();
    let sf = peer_succession(&state, f0, &f1).await;

    let members = roster(&router, &state, &owner).await;
    assert!(
        seat(&members, f0).is_none(),
        "the roster names the successor"
    );
    assert_eq!(
        seat(&members, f1.actor_id().0).unwrap().access.as_deref(),
        Some("writer"),
        "the grant followed the roster row"
    );
    assert!(
        state
            .db
            .get_folder_member_role(&channel_id, &f0)
            .await
            .unwrap()
            .is_none(),
        "no orphan grant is left under the retired id"
    );
    assert_eq!(seat(&members, f1.actor_id().0).unwrap().remote, Some(true));
    assert_eq!(carried(&members, f1.actor_id().0), vec![sf]);
    assert_eq!(
        proven_predecessors_carried(
            &f1.actor_id(),
            &seat(&members, f1.actor_id().0)
                .unwrap()
                .succession_statements
        ),
        vec![fauna_core::identity::ActorId(f0)]
    );

    // Fan-in: a second peer-learned link names the same successor and is
    // refused; the first-landed link still carries.
    let g0 = ActorKeypair::generate().actor_id().0;
    let bytes = common::succession_bytes(&RecoveryKey::generate(), g0, f1.signing_key(), None, 2);
    assert_eq!(
        state
            .db
            .record_peer_succession(&g0, &f1.actor_id().0, &bytes, 2)
            .await
            .unwrap()
            .unwrap_err(),
        fauna_nest::db::successions::SuccessionRefusal::NewAlreadySucceeded,
        "a second link into one id is refused"
    );
    let members = roster(&router, &state, &owner).await;
    assert_eq!(
        proven_predecessors_carried(
            &f1.actor_id(),
            &seat(&members, f1.actor_id().0)
                .unwrap()
                .succession_statements
        ),
        vec![fauna_core::identity::ActorId(f0)],
        "the first-landed link still carries"
    );
}

/// The collision rule on the home leg (ruling (8)(j)(4)): a successor the
/// owner granted before the Welcome keeps its own grant; the predecessor's is
/// dropped with its seat.
#[tokio::test]
async fn a_successor_already_granted_keeps_its_own_grant_at_the_welcome() {
    let (router, state, owner) = nest().await;
    create_set(&router, &state, &owner, SET, Some(NONCE)).await;
    let channel = share_set(&router, &state, &owner).await;
    let channel_id = fauna_core::hex32::decode(&channel).unwrap();
    let w0 = member(&state).await;
    welcome(&router, &state, &owner, &channel, &w0).await;
    grant_writer(&router, &state, &owner, w0.actor_id().0).await;

    let (w1, _) = succeed(&router, &state, &w0).await;
    state
        .db
        .set_folder_member_access(&channel_id, &w1.actor_id().0, "reader", None)
        .await
        .unwrap();
    welcome(&router, &state, &owner, &channel, &w1).await;

    let members = roster(&router, &state, &owner).await;
    assert!(seat(&members, w0.actor_id().0).is_none());
    assert_eq!(
        seat(&members, w1.actor_id().0).unwrap().access.as_deref(),
        Some("reader"),
        "the grant the owner minted for the successor stands"
    );
    assert!(
        state
            .db
            .get_folder_member_role(&channel_id, &w0.actor_id().0)
            .await
            .unwrap()
            .is_none(),
        "the predecessor's grant is dropped, not left behind"
    );
}

/// The collision rule on the peer leg: a cross-nest successor that already
/// holds its own grant on the channel keeps it, and the predecessor's is
/// dropped as the roster row moves.
#[tokio::test]
async fn a_cross_nest_successor_already_granted_keeps_its_own_grant() {
    let (router, state, owner) = nest().await;
    create_set(&router, &state, &owner, SET, Some(NONCE)).await;
    let channel = share_set(&router, &state, &owner).await;
    let channel_id = fauna_core::hex32::decode(&channel).unwrap();

    let f0 = ActorKeypair::generate().actor_id().0;
    state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &f0,
            &[0x77u8; 32],
            None,
            fauna_nest::db::channels::RebindPower::Standing,
        )
        .await
        .unwrap();
    grant_writer(&router, &state, &owner, f0).await;
    let f1 = ActorKeypair::generate();
    state
        .db
        .set_folder_member_access(&channel_id, &f1.actor_id().0, "reader", None)
        .await
        .unwrap();

    peer_succession(&state, f0, &f1).await;

    let members = roster(&router, &state, &owner).await;
    assert!(seat(&members, f0).is_none());
    assert_eq!(
        seat(&members, f1.actor_id().0).unwrap().access.as_deref(),
        Some("reader"),
        "the successor's own grant stands"
    );
    assert!(
        state
            .db
            .get_folder_member_role(&channel_id, &f0)
            .await
            .unwrap()
            .is_none()
    );
}

/// What the Welcome's carry does NOT reach: a recipient nobody succeeded into
/// moves nothing, and a non-claimant's Welcome for a successor neither seats
/// it nor retires its predecessor.
#[tokio::test]
async fn a_welcome_carries_nothing_without_a_predecessor_or_from_a_non_claimant() {
    let (router, state, owner) = nest().await;
    create_set(&router, &state, &owner, SET, Some(NONCE)).await;
    let channel = share_set(&router, &state, &owner).await;
    let (w0, bystander) = (member(&state).await, member(&state).await);
    welcome(&router, &state, &owner, &channel, &w0).await;
    grant_writer(&router, &state, &owner, w0.actor_id().0).await;

    // No predecessor: the bystander is seated as a reader, w0 untouched.
    welcome(&router, &state, &owner, &channel, &bystander).await;
    let members = roster(&router, &state, &owner).await;
    assert_eq!(
        seat(&members, bystander.actor_id().0)
            .unwrap()
            .access
            .as_deref(),
        Some("reader")
    );
    assert_eq!(
        seat(&members, w0.actor_id().0).unwrap().access.as_deref(),
        Some("writer")
    );

    // A member that is not the claimant addresses a Welcome to the successor.
    // Whatever the door answers, the roster does not move.
    let (w1, _) = succeed(&router, &state, &w0).await;
    let _: Result<WelcomeDeliverReply, _> = call(
        &router,
        &state,
        bystander.actor_id().0,
        "fauna.conversations.welcome.deliver",
        WelcomeDeliverRequest {
            recipient_actor_id: w1.actor_id().to_hex(),
            channel_id: channel.clone(),
            welcome_bytes: vec![0x01, 0x02, 0x03],
            kind: WelcomeKind::Folder {
                group_id: hex::encode(GROUP),
            },
            nest_url: None,
            extra: Default::default(),
        },
    )
    .await;
    let members = roster(&router, &state, &owner).await;
    assert!(
        seat(&members, w1.actor_id().0).is_none(),
        "a non-claimant seats nobody on a claimed channel"
    );
    assert_eq!(
        seat(&members, w0.actor_id().0).unwrap().access.as_deref(),
        Some("writer"),
        "and so retires no seat and carries no grant"
    );
}

/// End to end with the shared judge: a member's reader, given nothing but the
/// nest's roster read and its change list, admits the earlier rows of a
/// succeeded owner and of a succeeded writer as their successors' — the
/// writer's once the Welcome that seats its successor has retired the old seat
/// and carried the grant, with no act by the owner — and refuses a stranger's. Without the carried statements it admits neither.
#[tokio::test]
async fn a_member_reader_admits_succeeded_writers_earlier_rows_and_refuses_a_strangers() {
    let (router, state, a0) = nest().await;
    create_set(&router, &state, &a0, SET, Some(NONCE)).await;
    let channel = share_set(&router, &state, &a0).await;
    let (w0, reader_member) = (member(&state).await, member(&state).await);
    for m in [&w0, &reader_member] {
        welcome(&router, &state, &a0, &channel, m).await;
    }
    grant_writer(&router, &state, &a0, w0.actor_id().0).await;

    // Each writer records one row under its own identity key.
    for (who, device, path, manifest) in [
        (&a0, "0a".repeat(32), "owner.jpg", 0xa0u8),
        (&w0, "0b".repeat(32), "member.jpg", 0xb0),
    ] {
        register_device(&router, &state, who, &device).await;
        let req = sign(
            record(&device, path, manifest, 10),
            who,
            NONCE,
            who.signing_key(),
        );
        record_as(&router, &state, who, req)
            .await
            .unwrap_or_else(|e| panic!("{path} records: {e:?}"));
    }

    let (a1, _) = succeed(&router, &state, &a0).await;
    let (w1, _) = succeed(&router, &state, &w0).await;
    register_self(&router, &state, &a1, &channel).await;

    // The member reads the roster and the rows; that is all its reader gets.
    let page = list(&router, &state, &reader_member).await;
    // A member's succession leaves the RETIRED id its seat and its grant until
    // the Welcome that seats its successor, so in that window the retired id
    // is a writer itself — and ruling (8)(b)'s precedence attributes its rows
    // to it, not to the successor (ruled correct, (8)(j)(2)).
    let members = roster(&router, &state, &reader_member).await;
    {
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            ..Default::default()
        });
        r.install_roster(writer_roster(&members));
        let hash = fauna_core::hex32::encode(&fauna_core::sync::path_hash("member.jpg"));
        let row = page.changes.iter().find(|c| c.path_hash == hash).unwrap();
        assert!(
            matches!(
                r.judge(row),
                RowVerdict::Verified { writer, signed_as, .. }
                    if writer == w0.actor_id().0 && signed_as == writer
            ),
            "the retired id still holds `writer`: {:?}",
            r.judge(row)
        );
    }
    // The sweep's Welcome ends the window: w0 leaves the roster and w1 holds
    // the carried grant, with no `set_access` and no re-grant.
    welcome(&router, &state, &a1, &channel, &w1).await;
    let members = roster(&router, &state, &reader_member).await;
    assert!(
        seat(&members, w0.actor_id().0).is_none(),
        "the retired id is off the roster"
    );
    assert_eq!(
        seat(&members, w1.actor_id().0).unwrap().access.as_deref(),
        Some("writer")
    );
    assert_eq!(page.changes.len(), 2);
    let row_at = |path: &str| {
        let hash = fauna_core::hex32::encode(&fauna_core::sync::path_hash(path));
        page.changes
            .iter()
            .find(|r| r.path_hash == hash)
            .unwrap_or_else(|| panic!("{path} is served"))
            .clone()
    };
    let reader_over = |members: &[FolderActorMember]| {
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            ..Default::default()
        });
        r.ingest_certs(&page.signer_certs);
        r.install_roster(writer_roster(members));
        r
    };

    let r = reader_over(&members);
    for (path, signed, now) in [("owner.jpg", &a0, &a1), ("member.jpg", &w0, &w1)] {
        let row = row_at(path);
        assert_eq!(
            row.author_actor_id.as_deref(),
            Some(now.actor_id().to_hex().as_str()),
            "{path}: the stamp moved with the account"
        );
        assert!(
            matches!(
                r.judge(&row),
                RowVerdict::Verified { writer, signed_as, .. }
                    if writer == now.actor_id().0 && signed_as == signed.actor_id().0
            ),
            "{path}: admitted as the successor's, signed as the predecessor: {:?}",
            r.judge(&row)
        );
    }

    // A stranger re-signs a served row as itself: no writer, no chain.
    let stranger = ActorKeypair::generate();
    let mut forged = row_at("owner.jpg");
    forged.author_actor_id = Some(stranger.actor_id().to_hex());
    ChangeSigner::direct(&stranger)
        .sign_row(&mut forged, NONCE)
        .expect("signs");
    assert_eq!(
        r.judge(&forged),
        RowVerdict::Refused(ChangeVerifyError::NotAWriter)
    );

    // The carriage is what admits: the same roster with the statements
    // stripped proves nothing, and both rows are refused.
    let bare: Vec<FolderActorMember> = members
        .iter()
        .cloned()
        .map(|mut m| {
            m.succession_statements.clear();
            m
        })
        .collect();
    let r = reader_over(&bare);
    for path in ["owner.jpg", "member.jpg"] {
        assert_eq!(
            r.judge(&row_at(path)),
            RowVerdict::Refused(ChangeVerifyError::NotAWriter),
            "{path}: no statement, no link"
        );
    }
}

/// The moved cert is served (ruling (8) preamble): a succession moves a
/// delegated signer's rows AND its cert to the successor's key, so every list
/// reply still carries the cert that names — and was root-signed by — the
/// PREDECESSOR beside a row stamped with the successor. Pinned on all three
/// same-nest projections; presence, not table length.
#[tokio::test]
async fn a_moved_rows_predecessor_cert_is_still_served_after_a_succession() {
    use fauna_protocol::files::{FilesVersionsListReply, FilesVersionsListRequest};
    use fauna_protocol::media::{MEDIA_LIST_CURSOR_V2, MediaListReply, MediaListRequest};
    let (router, state, a0) = nest().await;
    create_set(&router, &state, &a0, SET, Some(NONCE)).await;
    let (writer, device_hex) = enroll_principal(&router, &state, &a0).await;
    for (manifest, change_type) in [(0x91u8, "create"), (0x92, "modify")] {
        let mut req = record(&device_hex, "pic.jpg", manifest, 10);
        req.change_type = change_type.into();
        let req = sign(req, &a0, NONCE, writer.signing_key());
        record_as(&router, &state, &a0, req).await.unwrap();
    }
    let before = list(&router, &state, &a0).await.signer_certs;
    assert_eq!(before.len(), 1);
    let cert = before[0].clone();

    let (a1, _) = succeed(&router, &state, &a0).await;

    // The successor's own reader: its id, and the predecessor it attests.
    let reader = |certs: &[fauna_core::encoding::EmbedAsBytes]| {
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            set_nonce: Some(NONCE),
            owner: Some(a1.actor_id().0),
            account: Some(a1.actor_id().0),
            account_predecessors: [a0.actor_id().0].into_iter().collect(),
            ..Default::default()
        });
        r.ingest_certs(certs);
        r
    };
    let assert_verifies = |what: &str, r: &RowReader, row: &fauna_protocol::sync::SyncChange| {
        assert_eq!(
            row.author_actor_id.as_deref(),
            Some(a1.actor_id().to_hex().as_str()),
            "{what}: the row is stamped with the successor"
        );
        assert!(
            matches!(
                r.judge(row),
                RowVerdict::Verified { writer, signed_as, .. }
                    if writer == a1.actor_id().0 && signed_as == a0.actor_id().0
            ),
            "{what}: verifies through the predecessor-named cert: {:?}",
            r.judge(row)
        );
    };

    let page = list(&router, &state, &a1).await;
    assert_eq!(page.changes.len(), 2);
    assert!(page.signer_certs.contains(&cert), "changes.list");
    let r = reader(&page.signer_certs);
    for row in &page.changes {
        assert_verifies("changes.list", &r, row);
    }

    let media: MediaListReply = call(
        &router,
        &state,
        a1.actor_id().0,
        "fauna.media.list",
        MediaListRequest {
            cursor_version: MEDIA_LIST_CURSOR_V2,
            ..Default::default()
        },
    )
    .await
    .expect("media.list");
    assert_eq!(media.items.len(), 1);
    assert!(media.signer_certs.contains(&cert), "media.list");
    assert_verifies(
        "media.list",
        &reader(&media.signer_certs),
        &media.items[0].as_change_row().expect("the statement rides"),
    );

    let versions: FilesVersionsListReply = call(
        &router,
        &state,
        a1.actor_id().0,
        "fauna.files.versions.list",
        FilesVersionsListRequest {
            path_hash: ByteBuf::from(fauna_core::sync::path_hash("pic.jpg").to_vec()),
            folder: Some(SET.into()),
            include_pruned: None,
            ..Default::default()
        },
    )
    .await
    .expect("versions.list");
    assert_eq!(versions.versions.len(), 2);
    assert!(versions.signer_certs.contains(&cert), "versions.list");
    let r = reader(&versions.signer_certs);
    for v in &versions.versions {
        assert_verifies(
            "versions.list",
            &r,
            &v.as_change_row().expect("the statement rides"),
        );
    }
}

/// A successor's cert for a predecessor's device key never displaces the moved
/// one (ruling (8)(i)): the nest's cert table keys a cert by the identity it
/// names beside the row's actor and the device key. The predecessor's machine
/// key K signs a row as P; the account succeeds to S; S certifies the SAME K
/// (no shipped client does — the writer key is one keypair per (machine,
/// account) — but the nest checks none of that) and records under it. The
/// list reply then carries BOTH certs, and a reader recovers each row's
/// signed actor through its own.
#[tokio::test]
async fn a_successors_cert_for_a_predecessors_device_key_never_displaces_the_moved_one() {
    use fauna_protocol::sync_writer_sig::{SignerCertCache, recover_signed_actor};
    let (router, state, a0) = nest().await;
    create_set(&router, &state, &a0, SET, Some(NONCE)).await;
    let (writer, device_hex) = enroll_principal(&router, &state, &a0).await;
    let first = sign(
        record(&device_hex, "before.jpg", 0xc1, 10),
        &a0,
        NONCE,
        writer.signing_key(),
    );
    record_as(&router, &state, &a0, first).await.unwrap();

    let (a1, _) = succeed(&router, &state, &a0).await;

    // The successor certifies the predecessor's device key K by hand.
    register_device(&router, &state, &a1, &device_hex).await;
    let grant =
        fauna_client_sync::build_principal_grant(&a1, &writer.actor_id().0).expect("grant builds");
    let reply: DeviceGrantRegisterReply = call(
        &router,
        &state,
        a1.actor_id().0,
        "fauna.sync.device_grant.register",
        DeviceGrantRegisterRequest {
            device_id: device_hex.clone(),
            authorization: grant,
            extra: Default::default(),
        },
    )
    .await
    .expect("the successor's grant over K registers");
    assert!(reply.registered);
    let second = sign(
        record(&device_hex, "after.jpg", 0xc2, 10),
        &a1,
        NONCE,
        writer.signing_key(),
    );
    record_as(&router, &state, &a1, second)
        .await
        .expect("K records as the successor");

    let page = list(&router, &state, &a1).await;
    assert_eq!(page.changes.len(), 2);
    assert_eq!(
        page.signer_certs.len(),
        2,
        "one cert per identity that certified K in the row's actor's history"
    );
    let mut certs = SignerCertCache::new();
    certs.ingest_all(&page.signer_certs);
    for (path, signed) in [("before.jpg", &a0), ("after.jpg", &a1)] {
        let hash = fauna_core::hex32::encode(&fauna_core::sync::path_hash(path));
        let row = page
            .changes
            .iter()
            .find(|c| c.path_hash == hash)
            .unwrap_or_else(|| panic!("{path} is served"));
        assert_eq!(
            recover_signed_actor(row, NONCE, &certs)
                .unwrap_or_else(|e| panic!("{path} recovers its signed actor: {e:?}"))
                .signed_as,
            signed.actor_id().0,
            "{path}"
        );
    }
}

// ── Ruling (11)(g): the charge follows the (path, manifest) pair ─────────────

async fn used(state: &Arc<AppState>, who: &ActorKeypair) -> i64 {
    state
        .db
        .get_user(&who.actor_id().0)
        .await
        .unwrap()
        .expect("user row")
        .storage_bytes_used
}

async fn member_used(state: &Arc<AppState>, channel: &str, who: &ActorKeypair) -> i64 {
    let channel: [u8; 32] = hex::decode(channel).unwrap().try_into().unwrap();
    state
        .db
        .get_folder_member_role(&channel, &who.actor_id().0)
        .await
        .unwrap()
        .expect("member role row")
        .bytes_used
}

/// Cap the `free` tier (every seeded test actor's) at `max` bytes.
async fn cap_free_tier(state: &Arc<AppState>, max: i64) {
    let mut tier = state.db.get_tier("free").await.unwrap().expect("free tier");
    tier.max_storage_bytes = max;
    state.db.update_tier(&tier).await.unwrap();
}

/// `who` records `manifest` at `path`, signed directly under [`NONCE`].
async fn record_signed(
    router: &RpcRouter,
    state: &Arc<AppState>,
    who: &ActorKeypair,
    device_hex: &str,
    path: &str,
    manifest: u8,
    size: i64,
) -> Result<i64, RpcError> {
    let req = SyncChangeRecordRequest {
        change_type: "modify".into(),
        ..record(device_hex, path, manifest, size)
    };
    record_as(router, state, who, sign(req, who, NONCE, who.signing_key())).await
}

/// Soft-prune `seq` — the hop where a flag moves or a credit lands — then run
/// the purge over it with its window forced shut, asserting the purge itself
/// moves the owner's meter not at all.
async fn prune_then_purge(state: &Arc<AppState>, owner: &ActorKeypair, seq: i64) {
    assert!(state.db.soft_prune_version(seq).await.unwrap());
    let before = used(state, owner).await;
    state.db.set_version_purge_after(seq, 1).await.unwrap();
    let (purged, guarded) = state.db.purge_expired_soft_pruned_versions().await.unwrap();
    assert_eq!((purged, guarded), (1, 0), "the purge terminates seq {seq}");
    assert_eq!(
        used(state, owner).await,
        before,
        "the purge of an already-soft-pruned row moves nothing"
    );
}

/// Ruling (11)(g): a record over the path head's own manifest TRANSFERS the
/// head's charge — one listable row of the (path, manifest) pair holds it, a
/// release moves the flag to a surviving same-manifest row before it credits,
/// and `undelete` charges only an uncharged pair. Both refused shapes are
/// walked: the skipped charge (the former head's prune must credit nothing) and
/// transfer-and-forget (the re-record's prune must not free bytes the former
/// head still lists).
#[tokio::test]
async fn a_same_manifest_record_transfers_the_heads_charge_and_releases_follow_the_pair() {
    const S: i64 = 100;
    const S2: i64 = 30;
    let (router, state, owner) = nest().await;
    create_set(&router, &state, &owner, SET, Some(NONCE)).await;
    let channel = share_set(&router, &state, &owner).await;
    let writer = member(&state).await;
    welcome(&router, &state, &owner, &channel, &writer).await;
    grant_writer(&router, &state, &owner, writer.actor_id().0).await;
    let (od, wd) = ("0a".repeat(32), "0b".repeat(32));
    register_device(&router, &state, &owner, &od).await;
    register_device(&router, &state, &writer, &wd).await;

    // ── Forward: the former head leaves first.
    let x1 = record_signed(&router, &state, &owner, &od, "a.jpg", 0xe1, S)
        .await
        .unwrap();
    assert_eq!(used(&state, &owner).await, S);
    let x2 = record_signed(&router, &state, &writer, &wd, "a.jpg", 0xe1, S)
        .await
        .unwrap();
    assert!(x2 > x1, "another actor's record of the same manifest lands");
    assert_eq!(
        used(&state, &owner).await,
        S,
        "a same-manifest record adds no bytes, so it adds no charge"
    );
    assert_eq!(
        member_used(&state, &channel, &writer).await,
        S,
        "the recorder's member half carries the transferred charge"
    );
    prune_then_purge(&state, &owner, x1).await;
    assert_eq!(
        used(&state, &owner).await,
        S,
        "a flagged-off former head credits nothing when it leaves"
    );
    record_signed(&router, &state, &owner, &od, "a.jpg", 0xe2, S2)
        .await
        .unwrap();
    assert_eq!(used(&state, &owner).await, S + S2);
    prune_then_purge(&state, &owner, x2).await;
    assert_eq!(
        used(&state, &owner).await,
        S2,
        "the last row of the pair credits its bytes"
    );
    assert_eq!(member_used(&state, &channel, &writer).await, 0);

    // ── Reverse: the re-record leaves first, the former head still lists.
    let y1 = record_signed(&router, &state, &writer, &wd, "b.jpg", 0xf1, S)
        .await
        .unwrap();
    assert_eq!(member_used(&state, &channel, &writer).await, S);
    let y2 = record_signed(&router, &state, &owner, &od, "b.jpg", 0xf1, S)
        .await
        .unwrap();
    assert_eq!(used(&state, &owner).await, S2 + S);
    assert_eq!(
        member_used(&state, &channel, &writer).await,
        0,
        "the former head's member half is released on the transfer"
    );
    record_signed(&router, &state, &owner, &od, "b.jpg", 0xf2, S2)
        .await
        .unwrap();
    let base = S2 + S + S2;
    assert_eq!(used(&state, &owner).await, base);

    assert!(state.db.soft_prune_version(y2).await.unwrap());
    assert_eq!(
        used(&state, &owner).await,
        base,
        "the flag moves back to the former head instead of crediting"
    );
    assert_eq!(member_used(&state, &channel, &writer).await, S);
    assert!(state.db.undelete_version(y2).await.unwrap());
    assert_eq!(
        used(&state, &owner).await,
        base,
        "undelete charges nothing while a listable row of the pair is charged"
    );
    assert!(state.db.soft_prune_version(y1).await.unwrap());
    assert_eq!(used(&state, &owner).await, base, "the flag moves again");
    assert_eq!(member_used(&state, &channel, &writer).await, 0);
    assert!(state.db.soft_prune_version(y2).await.unwrap());
    assert_eq!(
        used(&state, &owner).await,
        base - S,
        "the pair is released once"
    );
    assert!(state.db.undelete_version(y1).await.unwrap());
    assert_eq!(
        used(&state, &owner).await,
        base,
        "undelete charges an uncharged pair"
    );
    assert!(state.db.undelete_version(y2).await.unwrap());
    assert_eq!(used(&state, &owner).await, base, "and never twice");

    // ── An owner at exactly their quota still re-signs a same-manifest head
    // (net zero), and a genuinely new manifest is still metered.
    cap_free_tier(&state, base).await;
    record_signed(&router, &state, &writer, &wd, "b.jpg", 0xf2, S2)
        .await
        .expect("a move is never refused at a full quota");
    assert_eq!(used(&state, &owner).await, base);
    assert_code(
        record_signed(&router, &state, &owner, &od, "c.jpg", 0xc1, S2)
            .await
            .unwrap_err(),
        "storage_quota_exceeded",
        "a new manifest at a full quota",
    );
    cap_free_tier(&state, base + S2).await;
    record_signed(&router, &state, &owner, &od, "c.jpg", 0xc1, S2)
        .await
        .unwrap();
    assert_eq!(used(&state, &owner).await, base + S2);

    // Folder deletion reclaims exactly what is charged — an uncharged listable
    // row of a pair is not credited a second time.
    assert!(
        state
            .db
            .delete_folder_for_user(SET, &owner.actor_id().0)
            .await
            .unwrap()
    );
    assert_eq!(used(&state, &owner).await, 0);
}

/// Ruling (9)(e)'s finding (vii), closed by (11)(g): a successor one byte under
/// its quota can still re-sign a same-manifest head — the take-over moves no
/// bytes, so it is checked on a net charge of zero.
#[tokio::test]
async fn a_successor_near_quota_can_still_resign_a_same_manifest_head() {
    const S: i64 = 60;
    let (router, state, a0) = nest().await;
    create_set(&router, &state, &a0, SET, Some(NONCE)).await;
    let device = "0a".repeat(32);
    register_device(&router, &state, &a0, &device).await;
    record_signed(&router, &state, &a0, &device, "a.jpg", 0xe1, S)
        .await
        .unwrap();
    cap_free_tier(&state, S + 1).await;

    let (a1, _) = succeed(&router, &state, &a0).await;
    assert_eq!(
        used(&state, &a1).await,
        S,
        "the meter moved with the account"
    );
    let device = "0c".repeat(32);
    register_device(&router, &state, &a1, &device).await;
    record_signed(&router, &state, &a1, &device, "a.jpg", 0xe1, S)
        .await
        .expect("the re-sign over the same manifest lands at quota - 1");
    assert_eq!(used(&state, &a1).await, S, "and charges nothing net");
}

/// Ruling (11)(g) at the conflict doors (`file-versions.md` § Retention (4),
/// "charged like a metered content record" — the same-manifest transfer
/// included): a choose-winner whose winning candidate is the manifest the
/// path's head already holds mints a new head over the same bytes, so it takes
/// the head's charge instead of adding one — never refused at a full quota —
/// and the pair's bytes are credited exactly once, when its last row leaves.
#[tokio::test]
async fn a_choose_winner_over_the_heads_own_manifest_transfers_the_charge() {
    use fauna_protocol::folders::{ConflictResolveReply, ConflictResolveRequest};
    const S: i64 = 100;
    const S2: i64 = 30;
    let (router, state, owner) = nest().await;
    let actor = owner.actor_id().0;
    create_set(&router, &state, &owner, SET, Some(NONCE)).await;
    let device = [0x0a; 32];
    let device_hex = hex::encode(device);
    register_device(&router, &state, &owner, &device_hex).await;
    let path = "doc.txt";
    let win = [0x77; 32];

    let x1 = record_signed(&router, &state, &owner, &device_hex, path, 0x77, S)
        .await
        .unwrap();
    assert_eq!(used(&state, &owner).await, S);

    let folder_id = state
        .db
        .get_folder_for_actor(SET, &actor)
        .await
        .unwrap()
        .expect("set")
        .id;
    let seal = b"conflict-seal".to_vec();
    let conflict_id = state
        .db
        .report_conflict(
            &actor,
            folder_id,
            &device,
            path,
            "modify-modify",
            None,
            &[fauna_nest::db::ConflictCandidateRow {
                manifest_hash: win.to_vec(),
                device_id: device.to_vec(),
                size_bytes: S,
                created_at: 0,
                content_key_version: None,
            }],
            None,
            fauna_nest::db::SealedConflictLabels {
                path_sealed: Some(seal.clone()),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    let sig = SignedChange {
        set_nonce: NONCE,
        actor_id: actor,
        device_id: device,
        path_hash: fauna_core::sync::path_hash(path),
        manifest_hash: Some(win),
        change_type: "modify".into(),
        size_bytes: S,
        content_key_version: None,
        path_sealed: Some(seal),
        thumbnail_hash: None,
        derived_through: None,
        is_resolution: false,
        is_retention: false,
    }
    .sign(owner.signing_key());

    // The owner is at exactly their quota: the choose moves no bytes.
    cap_free_tier(&state, S).await;
    let ok: ConflictResolveReply = call(
        &router,
        &state,
        actor,
        "fauna.sync.conflicts.resolve",
        ConflictResolveRequest {
            id: conflict_id,
            winning_manifest_hash: Some(hex::encode(win)),
            winner_signature: Some(ByteBuf::from(sig.to_vec())),
            winner_signer_key: Some(ByteBuf::from(actor.to_vec())),
            ..Default::default()
        },
    )
    .await
    .expect("a choose over the head's own manifest is never refused at a full quota");
    assert!(ok.resolved);
    assert_eq!(
        used(&state, &owner).await,
        S,
        "the winner head takes the pair's charge instead of adding a second"
    );
    let x2 = list(&router, &state, &owner)
        .await
        .changes
        .last()
        .expect("the winner head row")
        .seq;
    assert!(x2 > x1);

    // Released exactly once: the flagged-off former head credits nothing, the
    // winner row — the pair's last — credits the bytes.
    cap_free_tier(&state, i64::MAX).await;
    prune_then_purge(&state, &owner, x1).await;
    assert_eq!(used(&state, &owner).await, S);
    record_signed(&router, &state, &owner, &device_hex, path, 0xe2, S2)
        .await
        .unwrap();
    assert_eq!(used(&state, &owner).await, S + S2);
    prune_then_purge(&state, &owner, x2).await;
    assert_eq!(used(&state, &owner).await, S2, "the pair is released once");
}

/// The resolved report's half of the same rule: a latest-wins report whose
/// winner is the remote version the path's head already holds charges the
/// reporter's retained loser (new bytes) in full and transfers the head's
/// charge to the winner row — the owner pays for each set of bytes once.
#[tokio::test]
async fn a_resolved_report_whose_winner_is_the_head_charges_only_the_loser() {
    use fauna_protocol::folders::{ConflictCandidate, ConflictReportReply, ConflictReportRequest};
    const HEAD: i64 = 20;
    const LOSER: i64 = 10;
    const S2: i64 = 30;
    let (router, state, owner) = nest().await;
    let actor = owner.actor_id().0;
    create_set(&router, &state, &owner, SET, Some(NONCE)).await;
    let remote = [0x4b; 32];
    let remote_hex = hex::encode(remote);
    register_device(&router, &state, &owner, &remote_hex).await;
    let reporter = [0x3a; 32];
    let path = "notes.txt";
    let seal = b"report-seal".to_vec();

    let x1 = record_signed(&router, &state, &owner, &remote_hex, path, 0x22, HEAD)
        .await
        .unwrap();
    assert_eq!(used(&state, &owner).await, HEAD);

    let row = |device: [u8; 32], manifest: u8, size: i64, w: i64, retention: bool| SignedChange {
        set_nonce: NONCE,
        actor_id: actor,
        device_id: device,
        path_hash: fauna_core::sync::path_hash(path),
        manifest_hash: Some([manifest; 32]),
        change_type: "modify".into(),
        size_bytes: size,
        content_key_version: None,
        path_sealed: Some(seal.clone()),
        thumbnail_hash: None,
        derived_through: Some(w),
        is_resolution: !retention,
        is_retention: retention,
    };
    let winner_sig = row(remote, 0x22, HEAD, x1, false).sign(owner.signing_key());
    let loser_sig = row(reporter, 0x11, LOSER, 0, true).sign(owner.signing_key());

    // The loser's bytes are new, so the report needs room for exactly them.
    cap_free_tier(&state, HEAD + LOSER).await;
    let _: ConflictReportReply = call(
        &router,
        &state,
        actor,
        "fauna.sync.conflicts.report",
        ConflictReportRequest {
            folder: SET.into(),
            device_id: hex::encode(reporter),
            path: path.into(),
            path_hash: Some(ByteBuf::from(fauna_core::sync::path_hash(path).to_vec())),
            path_sealed: Some(ByteBuf::from(seal.clone())),
            conflict_type: "concurrent_edit".into(),
            candidates: vec![
                ConflictCandidate {
                    manifest_hash: hex::encode([0x11; 32]),
                    device_id: hex::encode(reporter),
                    size_bytes: LOSER,
                    created_at: 100,
                    ..Default::default()
                },
                ConflictCandidate {
                    manifest_hash: hex::encode([0x22; 32]),
                    device_id: remote_hex.clone(),
                    size_bytes: HEAD,
                    created_at: 200,
                    ..Default::default()
                },
            ],
            resolution: Some("latest_wins".into()),
            winning_manifest_hash: Some(hex::encode([0x22; 32])),
            winning_size_bytes: Some(HEAD),
            winning_derived_through: Some(x1),
            losing_derived_through: Some(0),
            winner_signature: Some(ByteBuf::from(winner_sig.to_vec())),
            winner_signer_key: Some(ByteBuf::from(actor.to_vec())),
            loser_signature: Some(ByteBuf::from(loser_sig.to_vec())),
            ..Default::default()
        },
    )
    .await
    .expect("the report needs room for the loser's bytes only");
    assert_eq!(
        used(&state, &owner).await,
        HEAD + LOSER,
        "the loser is new bytes; the winner takes the head's charge"
    );
    let x3 = list(&router, &state, &owner)
        .await
        .changes
        .last()
        .expect("the winner head row")
        .seq;

    cap_free_tier(&state, i64::MAX).await;
    prune_then_purge(&state, &owner, x1).await;
    assert_eq!(used(&state, &owner).await, HEAD + LOSER);
    record_signed(&router, &state, &owner, &remote_hex, path, 0xe2, S2)
        .await
        .unwrap();
    prune_then_purge(&state, &owner, x3).await;
    assert_eq!(
        used(&state, &owner).await,
        LOSER + S2,
        "the head's bytes are released once"
    );
}
