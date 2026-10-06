#![cfg(feature = "nostr")]
//! tier_3: `store::materialize_account` must read a real created post's body
//! **segment-first** (`docs/goal/ui/nostr.md` § The relay event store,
//! materialization bullets; `docs/goal/ui/feed.md` § State & data shape → the
//! read model, adjacent-gap bullet).
//!
//! Drives the REAL production write path: `fauna.posts.create` →
//! `routes::ingest_post_core` → `segments::post::store_post`, which appends
//! the body to the author's `__post` segment and leaves `content.payload`
//! EMPTY (the posts at-rest cutover — `feed.md` § State & data shape). Before
//! this fix, `materialize_account` read the body straight off
//! `content.payload` via raw SQL — always empty for a post created this way —
//! so a real created post materialized NOTHING (0 derived events); the
//! store's own unit test only passed because it inserted a bare `Post`
//! directly into `content.payload`, bypassing the segment path entirely. This
//! test fails on that code (0 materialized) and passes once
//! `materialize_account` resolves the body through
//! `segments::post::load_post_body` (segment-first, `content.payload`
//! fallback for an inline body with no decodable author — the same entry point
//! `routes::get_post_core` uses).

mod common;
use common::signed_text_post;

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;

use fauna_bridge_nostr::signing::{Keypair, verify_event};
use fauna_bridge_nostr::types::Filter;
use fauna_nest::bridge_management::BridgeProviderRegistry;
use fauna_nest::bridges_ui_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::nostr::bridge_provider::NostrProvider;
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::nostr::{self, db as nostr_db, store};
use fauna_nest::posts_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::{
    Value, bridges_ui::SetSettingsRequest, decode_strict as decode, encode_canonical,
    posts::PostCreateRequest,
};

async fn router_with_posts() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    nostr::init_db(&db).await.expect("init nostr tables");
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    (b.build(), state)
}

/// Like [`router_with_posts`] but also registers the REAL `NostrProvider` as
/// a bridge, so `fauna.bridges.set_settings` reaches
/// `NostrProvider::update_settings` for real (not a test double).
async fn router_with_posts_and_nostr_bridge() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    nostr::init_db(&db).await.expect("init nostr tables");
    let mut state = AppState::for_test(db);
    let mut registry = BridgeProviderRegistry::new();
    registry.register(Box::new(NostrProvider));
    state.bridge.providers = Some(Arc::new(registry));
    let state = Arc::new(state);
    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    bridges_ui_handlers::register_bridges_ui_handlers(&mut b);
    (b.build(), state)
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Bytes {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload)
        .await
        .unwrap_or_else(|e| panic!("{kind} handler: {e:?}"))
}

#[tokio::test]
async fn materialize_reads_a_real_created_post_segment_first() {
    let (router, state) = router_with_posts().await;
    let author_kp = fauna_core::identity::ActorKeypair::generate();
    let author = author_kp.actor_id().0;
    let nostr_kp = Keypair::generate();

    // Create through the real wire path — the body lands in the `__post`
    // segment; `content.payload` is left empty (the cutover shape, asserted
    // below to pin the fixture against a future regression).
    let body = signed_text_post(&author_kp, "hello from the real create path");
    let reply = dispatch(
        &router,
        state.clone(),
        author,
        "fauna.posts.create",
        Bytes::from(
            encode_canonical(&PostCreateRequest {
                body: serde_bytes::ByteBuf::from(body.clone()),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await;
    let _: fauna_protocol::posts::PostCreateReply = decode(&reply).unwrap();

    let post_id = *blake3::hash(&body).as_bytes();
    let (payload, blob_hash) = state
        .db
        .get_post(&post_id)
        .await
        .unwrap()
        .expect("content row exists (projection retained)");
    assert!(
        payload.is_empty() && blob_hash.is_none(),
        "fixture sanity: the cutover leaves content.payload empty + no blob pointer"
    );

    // The bug: materialize_account used to read straight off content.payload
    // (always empty here) and silently materialize nothing. The fix: it reads
    // segment-first via segments::post::load_post_body.
    let n = store::materialize_account(
        &state.db,
        &state.post_segments,
        &hex::encode(author),
        &nostr_kp,
    )
    .await
    .expect("materialize ok");
    assert_eq!(
        n, 1,
        "the real created post materializes into the relay store"
    );

    let conn = state.db.conn().await;
    let f = Filter {
        authors: Some(vec![nostr_kp.public_key_hex()]),
        ..Default::default()
    };
    let got = store::query_events(&conn, &[f], 100).unwrap();
    assert_eq!(got.len(), 1, "the derived event is stored and queryable");
    assert!(
        verify_event(&got[0]),
        "the derived event carries a valid signature"
    );
    assert!(got[0].content.contains("hello from the real create path"));
    drop(conn);

    // Idempotent — a second run materializes nothing (deduped via the
    // outbound nostr_event_map).
    let n_again = store::materialize_account(
        &state.db,
        &state.post_segments,
        &hex::encode(author),
        &nostr_kp,
    )
    .await
    .expect("materialize ok");
    assert_eq!(n_again, 0, "a second run is a no-op");
}

/// Exercises the OTHER production caller of `materialize_account` —
/// `NostrProvider::update_settings` reacting to the `expose_content` toggle
/// (the immediate path; `materialize_all_exposed` is the sync worker's
/// periodic-sweep twin). This pins that `bridge_provider.rs` drops its
/// `CacheDb` connection guard before calling the now-async
/// `materialize_account` — holding it across that call would deadlock (both
/// acquire the same `CacheDb` mutex) rather than error, so a regression would
/// hang forever; wrapped in a bounded timeout so that fails loudly instead of
/// wedging the test run.
#[tokio::test]
async fn expose_content_toggle_materializes_via_the_real_bridge_provider() {
    let (router, state) = router_with_posts_and_nostr_bridge().await;
    let author_kp = fauna_core::identity::ActorKeypair::generate();
    let author = author_kp.actor_id().0;
    let nostr_kp = Keypair::generate();

    let body = signed_text_post(&author_kp, "expose-content sweep post");
    dispatch(
        &router,
        state.clone(),
        author,
        "fauna.posts.create",
        Bytes::from(
            encode_canonical(&PostCreateRequest {
                body: serde_bytes::ByteBuf::from(body),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await;

    // Deposit the author's nsec (the trust act that unlocks bridging /
    // materialize — `nostr.md` § The bridging gate) before toggling expose.
    {
        let nest_key = state.nest_identity.signing_key.to_bytes();
        let encrypted = encrypt_nostr_privkey(&nest_key, &nostr_kp.secret_bytes()).unwrap();
        let conn = state.db.conn().await;
        nostr_db::link_account(
            &conn,
            &hex::encode(author),
            &nostr_kp.public_key_hex(),
            "generated",
            Some(&encrypted),
            None,
            None,
        )
        .unwrap();
    }

    let req = SetSettingsRequest {
        bridge_id: "nostr".into(),
        settings: Value::Map(BTreeMap::from([(
            "expose_content".to_string(),
            Value::Bool(true),
        )])),
        extra: Default::default(),
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        dispatch(
            &router,
            state.clone(),
            author,
            "fauna.bridges.set_settings",
            Bytes::from(encode_canonical(&req).unwrap().to_vec()),
        ),
    )
    .await
    .expect("update_settings must not deadlock on its own CacheDb connection");

    let conn = state.db.conn().await;
    let f = Filter {
        authors: Some(vec![nostr_kp.public_key_hex()]),
        ..Default::default()
    };
    let got = store::query_events(&conn, &[f], 100).unwrap();
    assert_eq!(
        got.len(),
        1,
        "expose_content=true materializes the back-catalogue via the real bridge provider"
    );
}
