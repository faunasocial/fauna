//! Integration round-trip for the **Bluesky-native thread view** kind —
//! `bluesky.feed.thread`. The WS-RPC successor to the deprecated HTTP twins
//! `GET /api/v1/bluesky/feed/thread/{uri}` + `GET /api/v1/bluesky/thread?post_id={hex}`.
//!
//! The thread fetch itself makes a live `app.bsky.feed.getPostThread` XRPC call
//! to Bluesky, which isn't reachable in a hermetic test. So these tests pin the
//! parts that ARE deterministic without a live Bluesky session: the kind is
//! registered with the right replay/deadline metadata + allowlist, the request
//! decodes, the two address modes resolve (a Fauna `PostId` resolves its AT-URI
//! through the `bluesky_posts` crosspost mapping; a missing mapping is
//! `not_found`), and an unconfigured agent degrades to a graceful
//! `bluesky.upstream` error rather than panicking. (The `bridge → protocol`
//! `BlueskyPost` field mapping is covered by the `post_to_proto` unit test in
//! `bluesky::bluesky_handlers`.)
//!
//! Authority for the wire types: `libs/fauna-protocol/src/bluesky.rs`.
//! Slice tracked internally (Commit B).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real in-memory `CacheDb` —
//! no mocks). Matches `conformance_inbox.rs`. Feature-gated `bluesky`.
#![cfg(feature = "bluesky")]

mod common;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bluesky::{bluesky_handlers, db_helpers, init_db},
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{RpcError, bluesky::BlueskyThreadRequest};

const KIND: &str = "bluesky.feed.thread";

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    bluesky_handlers::register_bluesky_handlers(&mut b);
    (b.build(), state)
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router.kind_meta(KIND).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

// ── kind metadata + allowlist ──────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_with_db().await;
    let meta = router.kind_meta(KIND).expect("kind registered");
    // Idempotent read (re-fetch is side-effect-free), but a 30 s deadline: the
    // handler makes a live getPostThread XRPC round-trip to Bluesky.
    assert!(
        !meta.forbid_replay,
        "thread fetch is replay-safe (idempotent)"
    );
    assert_eq!(meta.default_deadline, std::time::Duration::from_secs(30));
}

#[tokio::test]
async fn thread_kind_user_only_at_allowlist_layer() {
    assert!(is_permitted(CallerClass::User, KIND), "{KIND} for User");
    // Admin inherits every User kind.
    assert!(is_permitted(CallerClass::Admin, KIND), "{KIND} for Admin");
    for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
        assert!(!is_permitted(class, KIND), "{KIND} denied for {class:?}");
    }
}

// ── request decoding + address-mode resolution ─────────────────────

#[tokio::test]
async fn malformed_payload_is_rejected() {
    let (router, state) = router_with_db().await;
    // A bare integer doesn't decode as the externally-tagged request enum.
    let err = dispatch(&router, state, [7u8; 32], encode(&42u64))
        .await
        .expect_err("malformed payload must error");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

#[tokio::test]
async fn post_id_must_be_32_bytes() {
    let (router, state) = router_with_db().await;
    let req = BlueskyThreadRequest::PostId {
        post_id: vec![1, 2, 3, 4],
    };
    let err = dispatch(&router, state, [7u8; 32], encode(&req))
        .await
        .expect_err("short post_id must error");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

#[tokio::test]
async fn post_id_without_crosspost_mapping_is_not_found() {
    let (router, state) = router_with_db().await;
    init_db(&state.db).await.unwrap(); // create the bluesky_posts table (empty)

    let req = BlueskyThreadRequest::PostId {
        post_id: vec![0xab; 32],
    };
    let err = dispatch(&router, state, [7u8; 32], encode(&req))
        .await
        .expect_err("unmapped post must error");
    assert_eq!(err.code, "fauna.bluesky.not_found");
}

#[tokio::test]
async fn post_id_with_mapping_resolves_then_hits_unconfigured_agent() {
    // With a crosspost mapping present, resolution succeeds (proving the
    // PostId → AT-URI path), so the handler advances to the agent restore —
    // which fails gracefully (Bluesky not configured in the test AppState)
    // as `bluesky.upstream`, NOT `not_found` (which would mean resolution
    // failed) and NOT a panic.
    let (router, state) = router_with_db().await;
    init_db(&state.db).await.unwrap();

    let post_hex = hex::encode([0xcd; 32]);
    {
        let conn = state.db.conn().await;
        db_helpers::store_crosspost_mapping(
            &conn,
            &post_hex,
            "at://did:plc:abc/app.bsky.feed.post/xyz",
            "did:plc:abc",
            "bafy-abc",
        )
        .unwrap();
    }

    let req = BlueskyThreadRequest::PostId {
        post_id: vec![0xcd; 32],
    };
    let err = dispatch(&router, state, [7u8; 32], encode(&req))
        .await
        .expect_err("unconfigured agent must error, not succeed");
    assert_eq!(
        err.code, "fauna.bluesky.upstream",
        "mapping resolved (not not_found) but the agent is unconfigured"
    );
}

#[tokio::test]
async fn at_uri_hits_unconfigured_agent_gracefully() {
    let (router, state) = router_with_db().await;
    let req = BlueskyThreadRequest::AtUri {
        uri: "at://did:plc:abc/app.bsky.feed.post/xyz".into(),
    };
    let err = dispatch(&router, state, [7u8; 32], encode(&req))
        .await
        .expect_err("unconfigured agent must error, not panic");
    assert_eq!(err.code, "fauna.bluesky.upstream");
}
