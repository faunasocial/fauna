//! The `IndexSegment` bulk-byte mint purpose is **RETIRED** — pinned here over
//! the real HTTP route (`docs/goal/behavior/content-index.md` § Where the index
//! is built → the 2026-08-10 carrier ruling): the MDA's index build half is
//! removed, so no caller — not even a fully-approved MDA serving a real mail
//! recipient — may mint an index-segment write token, and the blob write route's
//! own auth discipline (read/write split, no anonymous writes) is unchanged by
//! the retirement.
//!
//! This file previously proved the *opposite* flow (mint → PUT → record) after
//! the 2026-08-03 discovery that the route was session-bearer-only; that history
//! and its lesson live in git. What survives it is the shape: these pins run
//! against the real router on a real socket, because a purpose gate exercised
//! through a stubbed store cannot fail when the gate's arm is wrong.
//!
//! Tier: tier_3 (real `fauna-nest` router over a real socket + real `CacheDb` +
//! real `DiskBlobStore` — no mocks).

mod common;
use common::approve_bridge_as;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_cbor::Cid;
use fauna_nest::{
    backup::service::BackupService,
    db::{CacheDb, bridge_service_users::BridgeRole},
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    RpcError, decode_strict as decode,
    wrapped_blob::{
        BulkByteAccess, BulkByteMintPurpose, MintBulkByteTokenReply, MintBulkByteTokenRequest,
    },
};

struct Fixture {
    rpc: RpcRouter,
    state: Arc<AppState>,
    addr: std::net::SocketAddr,
    _dir: tempfile::TempDir,
}

/// A real nest: the full HTTP router on a real socket (so the blob PUT's
/// extractor runs for real) plus the two WS-RPC routers this flow touches.
async fn fixture() -> Fixture {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });

    let mut b = RpcRouter::builder();
    fauna_nest::content_index_handlers::register_content_index_handlers(&mut b);
    fauna_nest::bridge_blob_handlers::register_bridge_blob_handlers(&mut b);

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    Fixture {
        rpc: b.build(),
        state,
        addr,
        _dir: dir,
    }
}

async fn dispatch(
    f: &Fixture,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    common::seed_dispatch_actor(&f.state.db, &actor).await;
    let meta = f.rpc.kind_meta(kind).expect("kind registered");
    (meta.handler)(Arc::clone(&f.state), actor, payload).await
}

/// A mail recipient is precisely an actor with a recipient seal key — the fact
/// both the `MailBody` and `IndexSegment` mint gates test.
async fn make_recipient(state: &Arc<AppState>, actor: [u8; 32]) {
    common::seed_recipient_seal_key(&state.db, &actor, &common::FIXTURE_MSEK).await;
}

async fn mint(
    f: &Fixture,
    bridge: [u8; 32],
    target: [u8; 32],
    access: BulkByteAccess,
    purpose: BulkByteMintPurpose,
) -> Result<String, RpcError> {
    let bytes = dispatch(
        f,
        bridge,
        "fauna.bridges.mint_bulk_byte_token",
        encode(&MintBulkByteTokenRequest {
            actor_id: target.to_vec(),
            folder: String::new(),
            access,
            purpose,
            ..Default::default()
        }),
    )
    .await?;
    let reply: MintBulkByteTokenReply = decode(&bytes).unwrap();
    Ok(reply.token)
}

/// Stand-in for a sealed mail/calendar segment: opaque bytes, which is all the
/// nest ever sees of one (it holds no index key).
fn sealed_segment() -> Vec<u8> {
    b"FXSG\x02\x00\x00\x00 sealed mail-slice segment bytes, opaque to the nest".to_vec()
}

/// **The retirement pin, at the strongest point.** Every OTHER gate is
/// satisfied — an approved MDA, a real mail recipient — so the only reason left
/// to refuse is the retirement itself. Reverting the mint arm to the old
/// BridgeMda-allowed shape turns exactly this red.
#[tokio::test]
async fn even_an_approved_mda_cannot_mint_an_index_segment_token() {
    let f = fixture().await;
    let mda = [11u8; 32];
    let user = [42u8; 32];
    approve_bridge_as(&f.state.db, &mda, BridgeRole::Mda, "mda-1", &[1u8; 32]).await;
    make_recipient(&f.state, user).await;

    let err = mint(
        &f,
        mda,
        user,
        BulkByteAccess::Write,
        BulkByteMintPurpose::IndexSegment,
    )
    .await
    .expect_err(
        "the index-segment purpose is retired with the MDA's build half \
         (content-index.md § Where the index is built, 2026-08-10) — nothing may mint it",
    );
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

/// The MTA arm of the same retirement: an MTA may call the mint kind (it must,
/// to stage a large sealed body), and the index-segment purpose refuses it like
/// every other caller. Kept beside the MDA pin so the refusal is proven for
/// both bridge classes that can reach the kind at all.
#[tokio::test]
async fn an_mta_may_not_mint_an_index_segment_token() {
    let f = fixture().await;
    let mta = [9u8; 32];
    let user = [42u8; 32];
    approve_bridge_as(&f.state.db, &mta, BridgeRole::Mta, "mta-1", &[1u8; 32]).await;
    // Everything else the gate asks for is satisfied, so the class is the only
    // reason left to refuse.
    make_recipient(&f.state, user).await;

    let err = mint(
        &f,
        mta,
        user,
        BulkByteAccess::Write,
        BulkByteMintPurpose::IndexSegment,
    )
    .await
    .expect_err("an MTA never builds an index");
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}

/// The route's `Read`/`Write` split survives the retirement: a `Read`-scoped
/// bulk token is a hard 403 on the byte *write*, exactly as on the chunk
/// routes. Minted under `MailBody` — the surviving bridge purpose — since the
/// index-segment purpose no longer mints anything.
#[tokio::test]
async fn a_read_scoped_token_cannot_put_a_blob() {
    let f = fixture().await;
    let mda = [11u8; 32];
    let user = [42u8; 32];
    approve_bridge_as(&f.state.db, &mda, BridgeRole::Mda, "mda-1", &[1u8; 32]).await;
    make_recipient(&f.state, user).await;

    let token = mint(
        &f,
        mda,
        user,
        BulkByteAccess::Read,
        BulkByteMintPurpose::MailBody,
    )
    .await
    .expect("a read-scoped mint is legal; spending it on a write is not");

    let bytes = sealed_segment();
    let cid = Cid::of_raw(&bytes).to_base32();
    let resp = reqwest::Client::new()
        .put(format!("http://{}/api/v1/blob/{cid}", f.addr))
        .bearer_auth(&token)
        .body(bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
}

/// An unauthenticated PUT stays 401 — the widening admits a second *kind* of
/// credential, never no credential.
#[tokio::test]
async fn an_unauthenticated_put_is_still_refused() {
    let f = fixture().await;
    let bytes = sealed_segment();
    let cid = Cid::of_raw(&bytes).to_base32();
    let resp = reqwest::Client::new()
        .put(format!("http://{}/api/v1/blob/{cid}", f.addr))
        .body(bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

// ── The byte door's GC bound (2026-09-20) ───────────
//
// The unmetered posture this route rests on (`webdav-server.md` § Bulk-byte
// plane, :66) is not "bytes are free" — it is "bytes are charged at record
// time, and anything never recorded is *collected*". The second half is the
// GC bound, and GC enumerates `blob_metadata` alone
// (`backup/gc.rs::do_gc_delete_phase` → `db/blobs.rs::list_all_blob_hashes`).
// So a route that stores bytes without writing that row has no bound at all:
// what it stores is neither charged nor collectable, permanently. The
// multipart POST twin has always written the row before its ACK
// (`blob_routes.rs`, "the blob's only durable trace until a reference row
// lands"); this pin holds the PUT to the same contract.

/// A zero-grace blob GC sweep — the admin `fauna.admin.gc` door's exact call.
async fn zero_grace_gc(state: &Arc<AppState>) -> fauna_nest::backup::gc::GcResult {
    let backup_svc = state.backup_service.as_ref().expect("backup service");
    let blob_store = backup_svc.local_blob_store();
    fauna_nest::backup::gc::garbage_collect(
        &state.db,
        &blob_store,
        fauna_nest::backup::gc::PostBodySource {
            segments: &state.post_segments,
        },
        0,
        backup_svc.encryption_key(),
        false,
    )
    .await
    .expect("gc sweep")
}

/// **An unreferenced PUT blob is collectible.** The original probe ran this and the blob *survived*: any principal holding a session
/// bearer or a `Write` bulk token could fill the nest's disk 10 MiB at a time,
/// outside quota and outside GC, permanently.
///
/// Nothing here references the blob — no `fauna.index.record`, no snapshot — so
/// a zero-grace sweep must take it. The blob is stored under a real
/// `DiskBlobStore` on a real router, so a metadata write that lands anywhere
/// other than the row GC reads would not save this test.
#[tokio::test]
async fn an_unreferenced_put_blob_is_collected() {
    let f = fixture().await;
    let mda = [11u8; 32];
    let user = [42u8; 32];
    approve_bridge_as(&f.state.db, &mda, BridgeRole::Mda, "mda-1", &[1u8; 32]).await;
    make_recipient(&f.state, user).await;

    let token = mint(
        &f,
        mda,
        user,
        BulkByteAccess::Write,
        BulkByteMintPurpose::MailBody,
    )
    .await
    .expect("a write-scoped mint is legal");

    let bytes = sealed_segment();
    let cid = Cid::of_raw(&bytes);
    let cid_b32 = cid.to_base32();
    let resp = reqwest::Client::new()
        .put(format!("http://{}/api/v1/blob/{cid_b32}", f.addr))
        .bearer_auth(&token)
        .body(bytes.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "the PUT must land");

    let hash = fauna_core::data::ContentHash::from_digest_raw(cid.digest());
    let store = f.state.backup_service.as_ref().unwrap().local_blob_store();
    assert!(
        store.exists(&hash).await.unwrap(),
        "the bytes must be on disk before the sweep, or this pin proves nothing"
    );

    zero_grace_gc(&f.state).await;

    assert!(
        !store.exists(&hash).await.unwrap(),
        "an unreferenced PUT blob survived a zero-grace GC — the route stores bytes that \
         neither quota nor GC can reach. The PUT \
         must write its `blob_metadata` row before the ACK, exactly as the multipart POST does."
    );
}

/// The metadata the PUT writes must be **the body's own length**, because that
/// figure is what GC bills the sweep for and what the storage stats report. A
/// row carrying someone else's number would make the bound arithmetic wrong
/// even while the blob is collectable.
#[tokio::test]
async fn the_put_records_the_bodys_true_length() {
    let f = fixture().await;
    let mda = [11u8; 32];
    let user = [42u8; 32];
    approve_bridge_as(&f.state.db, &mda, BridgeRole::Mda, "mda-1", &[1u8; 32]).await;
    make_recipient(&f.state, user).await;

    let token = mint(
        &f,
        mda,
        user,
        BulkByteAccess::Write,
        BulkByteMintPurpose::MailBody,
    )
    .await
    .expect("a write-scoped mint is legal");

    let bytes = sealed_segment();
    let cid = Cid::of_raw(&bytes);
    reqwest::Client::new()
        .put(format!("http://{}/api/v1/blob/{}", f.addr, cid.to_base32()))
        .bearer_auth(&token)
        .body(bytes.clone())
        .send()
        .await
        .unwrap();

    let meta = f
        .state
        .db
        .get_blob_metadata(&cid.digest())
        .await
        .expect("metadata read")
        .expect("the PUT must leave a blob_metadata row");
    assert_eq!(
        meta.size_bytes,
        bytes.len() as i64,
        "the recorded size must be the body's own length"
    );
}
