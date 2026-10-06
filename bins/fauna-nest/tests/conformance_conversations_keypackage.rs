//! Integration round-trip for `fauna.conversations.keypackage.*` —
//! the MLS key-package plane (publish own, FIFO-consume a recipient's,
//! count remaining). Mirrors the `conformance_conversations_channel.rs`
//! shape; reaches `CacheDb` directly through
//! `state.db.{put_key_package, take_key_package, count_key_packages}`
//! via the handlers.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/conversations.rs`.
//! Authority for the slice + per-kind replay semantics: tracked
//! internally (§ T2 + § Per-kind replay semantics).
//!
//! Uploads use REAL MLS KeyPackages minted from a real actor keypair (not
//! placeholder bytes): since the MLS-2 fix the
//! upload handler parses each KeyPackage and rejects it unless its inner
//! credential == its leaf signature key == the authenticated uploader
//! (`fauna_mls::engine::verify_uploaded_key_package`). The storage/FIFO/count
//! assertions are unchanged — they just ride genuine KP bytes now.

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;
use serde_bytes::ByteBuf;

use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;
use fauna_nest::{conversations_handlers, db::CacheDb, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    conversations::{
        KeypackageCountReply, KeypackageCountRequest, KeypackageFetchReply, KeypackageFetchRequest,
        KeypackageUploadReply, KeypackageUploadRequest,
    },
    decode_strict as decode, encode_canonical,
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    (b.build(), state)
}

/// A real in-memory MLS engine + its 32-byte ActorId, for honest
/// `keypackage.upload`. The MLS-2 nest check requires every uploaded KeyPackage
/// to carry this actor's identity (credential == leaf signature key == uploader).
fn real_actor_engine() -> (MlsEngine, [u8; 32]) {
    let engine = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let id = engine.identity_actor_id().0;
    (engine, id)
}

fn upload_payload(packages: Vec<Vec<u8>>) -> Bytes {
    upload_payload_kind(packages, false)
}

fn upload_payload_kind(packages: Vec<Vec<u8>>, last_resort: bool) -> Bytes {
    let req = KeypackageUploadRequest {
        packages: packages.into_iter().map(ByteBuf::from).collect(),
        last_resort,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn fetch_payload(actor_id_hex: &str) -> Bytes {
    let req = KeypackageFetchRequest {
        actor_id: actor_id_hex.into(),
        nest_url: None,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn count_payload(actor_id_hex: &str) -> Bytes {
    let req = KeypackageCountRequest {
        actor_id: actor_id_hex.into(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

// ── fauna.conversations.keypackage.upload ──────────────────────

#[tokio::test]
async fn upload_returns_stored_count() {
    let (router, state) = router_with_db_only().await;
    let (engine, actor) = real_actor_engine();
    let kps = engine.generate_key_packages_bytes(2).unwrap();
    let reply_bytes = dispatch(
        &router,
        state,
        actor,
        "fauna.conversations.keypackage.upload",
        upload_payload(kps),
    )
    .await
    .expect("upload ok");
    let reply: KeypackageUploadReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.stored, 2);
}

#[tokio::test]
async fn upload_then_count_reflects_uploaded() {
    let (router, state) = router_with_db_only().await;
    let (engine, actor) = real_actor_engine();
    let actor_hex = hex::encode(actor);
    let kps = engine.generate_key_packages_bytes(3).unwrap();

    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.conversations.keypackage.upload",
        upload_payload(kps),
    )
    .await
    .expect("upload ok");

    let reply_bytes = dispatch(
        &router,
        state,
        actor,
        "fauna.conversations.keypackage.count",
        count_payload(&actor_hex),
    )
    .await
    .expect("count ok");
    let reply: KeypackageCountReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.count, 3);
}

#[tokio::test]
async fn last_resort_upload_survives_fetch_and_excluded_from_count() {
    // Spec Y2: a last_resort upload is reusable (never consumed) and does not
    // count toward the one-time top-up gauge.
    let (router, state) = router_with_db_only().await;
    let (engine, actor) = real_actor_engine();
    let actor_hex = hex::encode(actor);
    let lr_kp = engine.generate_last_resort_key_package_bytes().unwrap();

    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.conversations.keypackage.upload",
        upload_payload_kind(vec![lr_kp], true),
    )
    .await
    .expect("last-resort upload ok");

    // Excluded from the one-time count.
    let count_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.conversations.keypackage.count",
        count_payload(&actor_hex),
    )
    .await
    .expect("count ok");
    let count: KeypackageCountReply = decode(&count_bytes).unwrap();
    assert_eq!(count.count, 0, "last-resort KP must not count as one-time");

    // Fetch returns it repeatedly without consuming.
    for _ in 0..2 {
        let fetch_bytes = dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.conversations.keypackage.fetch",
            fetch_payload(&actor_hex),
        )
        .await
        .expect("fetch ok");
        let fetched: KeypackageFetchReply = decode(&fetch_bytes).unwrap();
        assert!(
            fetched.key_package.is_some(),
            "last-resort KP must remain fetchable (reusable)"
        );
    }
}

#[tokio::test]
async fn upload_rejects_malformed_key_package_bytes() {
    // The MLS-2 parse step also rejects bytes that are not a valid KeyPackage at
    // all (previously the nest stored opaque bytes blindly).
    let (router, state) = router_with_db_only().await;
    let (_engine, actor) = real_actor_engine();
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.conversations.keypackage.upload",
        upload_payload(vec![vec![0xde, 0xad, 0xbe, 0xef]]),
    )
    .await
    .expect_err("malformed KeyPackage upload rejected");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
}

#[tokio::test]
async fn upload_rejects_forged_credential_key_package() {
    // MLS-2 attack at the wire boundary (the gap the engine-level
    // `mls2_verify_uploaded_key_package` unit test left at the nest handler): a
    // patched client authenticates as itself (Mallory) but uploads a *structurally
    // valid* KeyPackage whose inner credential names a victim, signed by Mallory's
    // own key. The handler must reject it — credential ≠ leaf signature key — so
    // the nest never stores or serves a forged-identity KeyPackage to a peer
    // building a group. (Distinct from `upload_rejects_malformed_key_package_bytes`,
    // which only proves un-parseable bytes are rejected.)
    let (router, state) = router_with_db_only().await;
    let (mallory_engine, mallory) = real_actor_engine();
    let victim = ActorKeypair::generate().actor_id();
    let forged = mallory_engine
        .forge_key_package_bytes_for_test(victim)
        .unwrap();

    let err = dispatch(
        &router,
        state,
        mallory,
        "fauna.conversations.keypackage.upload",
        upload_payload(vec![forged]),
    )
    .await
    .expect_err("forged-credential KeyPackage upload rejected");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
}

#[tokio::test]
async fn upload_rejects_honest_key_package_under_wrong_actor() {
    // The upload is bound to the *authenticated* uploader: an honest KeyPackage
    // (credential == its own leaf signature key) uploaded by a different actor is
    // rejected — credential ≠ authenticated uploader. Blocks replaying another
    // actor's honest KeyPackage under your own session to seed a forged-identity
    // pool. Needs no forge seam (the second binding check, `expected_actor`).
    let (router, state) = router_with_db_only().await;
    let (alice_engine, _alice) = real_actor_engine();
    let honest = alice_engine.generate_key_packages_bytes(1).unwrap()[0].clone();
    let (_bob_engine, bob) = real_actor_engine();

    let err = dispatch(
        &router,
        state,
        bob,
        "fauna.conversations.keypackage.upload",
        upload_payload(vec![honest]),
    )
    .await
    .expect_err("honest KeyPackage uploaded under wrong actor rejected");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
}

// ── fauna.conversations.keypackage.fetch ───────────────────────

#[tokio::test]
async fn fetch_returns_none_when_no_keypackages() {
    let (router, state) = router_with_db_only().await;
    let caller = [21u8; 32];
    let target = [22u8; 32];
    let target_hex = hex::encode(target);

    let reply_bytes = dispatch(
        &router,
        state,
        caller,
        "fauna.conversations.keypackage.fetch",
        fetch_payload(&target_hex),
    )
    .await
    .expect("fetch ok");
    let reply: KeypackageFetchReply = decode(&reply_bytes).unwrap();
    assert!(
        reply.key_package.is_none(),
        "no KPs → reply.key_package is None"
    );
}

#[tokio::test]
async fn fetch_consumes_one_keypackage_fifo() {
    let (router, state) = router_with_db_only().await;
    let (target_engine, target) = real_actor_engine();
    let caller = [32u8; 32];
    let target_hex = hex::encode(target);

    // Target uploads three KPs (FIFO order: kps[0] is oldest).
    let kps = target_engine.generate_key_packages_bytes(3).unwrap();
    for kp in &kps {
        dispatch(
            &router,
            state.clone(),
            target,
            "fauna.conversations.keypackage.upload",
            upload_payload(vec![kp.clone()]),
        )
        .await
        .expect("upload ok");
    }

    // Sender fetches one KP — should get the oldest (kps[0]).
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        caller,
        "fauna.conversations.keypackage.fetch",
        fetch_payload(&target_hex),
    )
    .await
    .expect("fetch ok");
    let reply: KeypackageFetchReply = decode(&reply_bytes).unwrap();
    assert_eq!(
        reply.key_package,
        Some(kps[0].clone()),
        "FIFO returns the oldest KP"
    );

    // Count must drop from 3 → 2 (consumed).
    let count_bytes = dispatch(
        &router,
        state,
        caller,
        "fauna.conversations.keypackage.count",
        count_payload(&target_hex),
    )
    .await
    .expect("count ok");
    let count: KeypackageCountReply = decode(&count_bytes).unwrap();
    assert_eq!(count.count, 2, "fetch consumed one KP");
}

#[tokio::test]
async fn fetch_drains_in_published_order() {
    let (router, state) = router_with_db_only().await;
    let (target_engine, target) = real_actor_engine();
    let caller = [42u8; 32];
    let target_hex = hex::encode(target);

    // Upload in three distinct calls so `published_at` differs per
    // entry (the FIFO order key). Same-call uploads share a timestamp
    // and would tie-break on insertion order, which is implementation-
    // dependent — separating them keeps the assertion robust.
    let kps = target_engine.generate_key_packages_bytes(3).unwrap();
    for kp in &kps {
        dispatch(
            &router,
            state.clone(),
            target,
            "fauna.conversations.keypackage.upload",
            upload_payload(vec![kp.clone()]),
        )
        .await
        .expect("upload ok");
        // One-second resolution on `published_at`; sleep briefly so
        // ordering is unambiguous.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    }

    let mut drained = Vec::new();
    loop {
        let reply_bytes = dispatch(
            &router,
            state.clone(),
            caller,
            "fauna.conversations.keypackage.fetch",
            fetch_payload(&target_hex),
        )
        .await
        .expect("fetch ok");
        let reply: KeypackageFetchReply = decode(&reply_bytes).unwrap();
        match reply.key_package {
            Some(bytes) => drained.push(bytes),
            None => break,
        }
    }
    assert_eq!(drained, kps, "fetch drains in published_at order");

    // Count must now be zero.
    let count_bytes = dispatch(
        &router,
        state,
        caller,
        "fauna.conversations.keypackage.count",
        count_payload(&target_hex),
    )
    .await
    .expect("count ok");
    let count: KeypackageCountReply = decode(&count_bytes).unwrap();
    assert_eq!(count.count, 0);
}

#[tokio::test]
async fn fetch_rejects_malformed_actor_id() {
    let (router, state) = router_with_db_only().await;
    let caller = [51u8; 32];
    let err = dispatch(
        &router,
        state,
        caller,
        "fauna.conversations.keypackage.fetch",
        fetch_payload("not-hex"),
    )
    .await
    .expect_err("malformed actor_id rejected");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
}

// ── fauna.conversations.keypackage.count ───────────────────────

#[tokio::test]
async fn count_zero_for_actor_with_no_uploads() {
    let (router, state) = router_with_db_only().await;
    let caller = [61u8; 32];
    let target = [62u8; 32];
    let reply_bytes = dispatch(
        &router,
        state,
        caller,
        "fauna.conversations.keypackage.count",
        count_payload(&hex::encode(target)),
    )
    .await
    .expect("count ok");
    let reply: KeypackageCountReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.count, 0);
}

#[tokio::test]
async fn count_does_not_consume() {
    let (router, state) = router_with_db_only().await;
    let (engine, actor) = real_actor_engine();
    let actor_hex = hex::encode(actor);
    let kps = engine.generate_key_packages_bytes(2).unwrap();

    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.conversations.keypackage.upload",
        upload_payload(kps),
    )
    .await
    .expect("upload ok");

    // Two counts in a row — both must return 2.
    for _ in 0..2 {
        let reply_bytes = dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.conversations.keypackage.count",
            count_payload(&actor_hex),
        )
        .await
        .expect("count ok");
        let reply: KeypackageCountReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.count, 2, "count is non-destructive");
    }
}

#[tokio::test]
async fn count_isolates_per_actor() {
    let (router, state) = router_with_db_only().await;
    let (engine_a, actor_a) = real_actor_engine();
    let actor_b = [82u8; 32];
    let kps = engine_a.generate_key_packages_bytes(3).unwrap();

    dispatch(
        &router,
        state.clone(),
        actor_a,
        "fauna.conversations.keypackage.upload",
        upload_payload(kps),
    )
    .await
    .expect("upload ok");

    let reply_bytes = dispatch(
        &router,
        state,
        actor_b,
        "fauna.conversations.keypackage.count",
        count_payload(&hex::encode(actor_b)),
    )
    .await
    .expect("count ok");
    let reply: KeypackageCountReply = decode(&reply_bytes).unwrap();
    assert_eq!(
        reply.count, 0,
        "actor_b's count is independent of actor_a's uploads"
    );
}
