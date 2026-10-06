//! Tier_3 end-to-end proof of the posts segment-store **cutover**
//! (Plan B).
//!
//! Drives the production WS-RPC pipeline `fauna.posts.create` →
//! `routes::ingest_post_core` → `segments::post::store_post` (body →
//! `__post/<author_hex>` segment + `content`-row projection with an EMPTY
//! payload) → `fauna.posts.get` → `routes::get_post_core` (segment-first read).
//!
//! What this adds beyond `conformance_posts.rs` (the create/get *contract*) is
//! assertions on the CUTOVER INVARIANTS — that the body moved OFF
//! `content.payload`/the blob store and ONTO the `__post` segment store, that
//! the segment-store mirror + on-disk file landed, and that the retained
//! feed-index projection still serves `query_feed` (the read model is
//! unaffected — it never read `content.payload`). Both the small inline-sized
//! case and the >64 KiB case (which pre-cutover spilled to the blob store via
//! `payload_store`) are covered: post-cutover, both rest in the segment and
//! leave `content.payload` empty + `blob_hash` NULL.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` + real plaintext
//! `Storage` + real on-disk `SegmentManager` — no mocks). Matches the
//! `conformance_posts.rs` / `conv_segment_round_trip.rs` harnesses.

mod common;
use common::signed_text_post;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::scoring::FilterCombination;
use fauna_nest::{db::CacheDb, posts_handlers, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    decode_strict as decode, encode_canonical,
    posts::{PostCreateReply, PostCreateRequest, PostGetReply, PostGetRequest},
};

async fn router_with_posts() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
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

/// Create `body` via `fauna.posts.create`, then assert every cutover invariant.
/// `kp` is a fresh keypair per call so each post lands in its own
/// `__post/<author_hex>` dir (the `for_test` segment tempdir is shared across
/// instances in one process — distinct authors avoid collision).
async fn assert_post_cutover(content_len: usize, fill: char) {
    let (router, state) = router_with_posts().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let author = kp.actor_id().0;
    // The connection must be the post's author: post ingest is own-write
    // (`feed.md` § Post creation).
    let conn_actor = author;

    let content: String = std::iter::repeat_n(fill, content_len).collect();
    let body = signed_text_post(&kp, &content);
    let expected_post_id = *blake3::hash(&body).as_bytes();

    // 1. Create through the real ingest pipeline.
    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            conn_actor,
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
        .await,
    )
    .unwrap();
    assert_eq!(
        create_reply.post_id,
        hex::encode(expected_post_id),
        "post_id is hex(blake3(body))"
    );

    // 2. The body is NOT in `content.payload` and there is NO blob pointer —
    //    the storage-shape flip + `payload_store` retirement for posts. (This
    //    holds even for the >64 KiB body, which pre-cutover spilled to a blob.)
    let (payload, blob_hash) = state
        .db
        .get_post(&expected_post_id)
        .await
        .unwrap()
        .expect("content row exists (projection retained)");
    assert!(
        payload.is_empty(),
        "content.payload is empty after the cutover (body in the __post segment), got {} bytes",
        payload.len()
    );
    assert!(
        blob_hash.is_none(),
        "no blob_hash — payload_store is retired for posts"
    );

    // 3. The `segment_records` mirror row landed, scoped to the post's AUTHOR
    //    (not the connection actor), keyed by the record CID = of_dag_cbor(body)
    //    whose digest is the post_id.
    let cid = fauna_cbor::Cid::of_dag_cbor(&body);
    let scope = state
        .db
        .segment_records_lookup_scope_and_segment("post", &cid)
        .await
        .unwrap();
    assert_eq!(
        scope.map(|(a, _)| a),
        Some(author),
        "mirror row scoped to the post author's __post segment"
    );

    // 4. The on-disk `__post/<author_hex>/` segment file exists.
    state
        .post_segments
        .finalize_open(&author)
        .await
        .expect("finalize open post segment");
    let scope_dir = state
        .post_segments
        .data_dir()
        .join("__post")
        .join(hex::encode(author));
    let mut entries = std::fs::read_dir(&scope_dir)
        .unwrap_or_else(|e| panic!("read __post scope dir {}: {e}", scope_dir.display()));
    assert!(
        entries.next().is_some(),
        "__post scope dir {} must hold a segment file",
        scope_dir.display()
    );

    // 5. `fauna.posts.get` returns the body verbatim — the read path resolves
    //    it segment-first (the inline `content.payload` is empty, so this can
    //    only have come from the segment store).
    let get_reply: PostGetReply = decode(
        &dispatch(
            &router,
            state.clone(),
            conn_actor,
            "fauna.posts.get",
            Bytes::from(
                encode_canonical(&PostGetRequest {
                    post_id: hex::encode(expected_post_id),
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await,
    )
    .unwrap();
    assert_eq!(
        get_reply.body.into_vec(),
        body,
        "posts.get returns the body verbatim, read from the __post segment"
    );

    // 6. The retained feed-index projection still serves `query_feed` — the
    //    read model never read `content.payload`, so the cutover leaves it
    //    unaffected. The post is present in the cross-actor feed.
    let feed = state
        .db
        .query_feed(&[], FilterCombination::All, &[], None, 50)
        .await
        .unwrap();
    assert!(
        feed.iter().any(|p| p.post_id == expected_post_id),
        "query_feed returns the post from the retained projection"
    );
}

#[tokio::test]
async fn small_post_body_lives_in_segment_not_payload() {
    // Inline-sized (well under the 64 KiB pre-cutover inline threshold).
    assert_post_cutover(64, 's').await;
}

#[tokio::test]
async fn large_post_over_64kib_lives_in_segment_not_blob() {
    // > 64 KiB — pre-cutover this spilled to the blob store via payload_store;
    // post-cutover it rests in the segment with an empty content.payload.
    assert_post_cutover(80 * 1024, 'L').await;
}
