//! Tier_3 end-to-end proof of the posts segment-store **snapshot/restore**
//! (Plan C C1).
//!
//! Drives the production WS-RPC pipeline through ONE router that has both the
//! posts handlers and the filesync snapshot handlers registered (so the
//! kind-string dispatch, the shared `AppState` threading `post_segments` into
//! both handler families, and the request/reply DAG-CBOR all participate):
//!
//! `fauna.posts.create` ×N            → body → `__post/<author>` segment + projection
//! `fauna.filesync.snapshot.create_message_kind {kind:"post", actor_id:author}`
//!                                     → pins the author's post `Manifest`
//! (wipe `segment_records` + `content` for the author — simulate a fresh /
//!  recovery nest that has pulled the segment FILES but holds no mirror or
//!  feed-index projection)
//! `fauna.filesync.snapshot.restore_message_kind {snapshot_id, confirm_id}`
//!                                     → `restore_post` → additive rebuild of the
//!                                       mirror + projection from the on-disk segments
//! `fauna.posts.get` / `query_feed`    → every restored post is served again.
//!
//! This is the post sibling of `conv_snapshot_restore_round_trip.rs`. Its
//! post-specific assertions: a restore rebuilds BOTH the body-resolving mirror
//! (`load_post_body` by CID) AND the cross-actor feed-index projection
//! (`query_feed`) — conv has only the former (its mirror IS the read model). And
//! the rebuild is **additive** (no DELETE), the no-data-loss recovery semantics.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` + real on-disk
//! `SegmentManager` — no mocks). Matches the `segment_post_cutover.rs` harness.

mod common;
use common::signed_text_post;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::scoring::FilterCombination;
use fauna_nest::{
    db::CacheDb, filesync_handlers, posts_handlers, routes::AppState, rpc_router::RpcRouter,
};
use fauna_protocol::{
    decode_strict as decode, encode_canonical,
    filesync::{
        SnapshotCreateMessageKindReply, SnapshotCreateMessageKindRequest,
        SnapshotRestoreMessageKindReply, SnapshotRestoreMessageKindRequest,
    },
    posts::{PostCreateReply, PostCreateRequest, PostGetReply, PostGetRequest},
};

/// `(tempdir, router, state)` with the posts + filesync handlers on one router
/// and `post_segments` rooted in a per-test tempdir (the `for_test` post-segment
/// dir is PID-shared across instances in one process; a per-test tempdir gives
/// the snapshot a stable, isolated `__post/<author_hex>/` to pin and restore
/// from). Hold the `TempDir` for the test's lifetime so the on-disk segment
/// files outlive the wipe (restore reads them back).
fn fixture() -> (tempfile::TempDir, Arc<RpcRouter>, Arc<AppState>) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
    let router = Arc::new({
        let mut b = RpcRouter::builder();
        posts_handlers::register_posts_handlers(&mut b);
        filesync_handlers::register_filesync_handlers(&mut b);
        b.build()
    });
    let mut state = AppState::for_test(db);
    state.post_segments = Arc::new(fauna_segment_store::SegmentManager::new(
        tmp.path().to_path_buf(),
        "post",
    ));
    (tmp, router, Arc::new(state))
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

async fn create_post(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32], body: &[u8]) {
    let reply: PostCreateReply = decode(
        &dispatch(
            router,
            state.clone(),
            actor,
            "fauna.posts.create",
            Bytes::from(
                encode_canonical(&PostCreateRequest {
                    body: serde_bytes::ByteBuf::from(body.to_vec()),
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
        reply.post_id,
        hex::encode(blake3::hash(body).as_bytes()),
        "post_id is hex(blake3(body))"
    );
}

async fn feed_post_ids(state: &Arc<AppState>) -> Vec<Vec<u8>> {
    state
        .db
        .query_feed(&[], FilterCombination::All, &[], None, 50)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.post_id)
        .collect()
}

#[tokio::test]
async fn post_snapshot_restore_round_trip() {
    let (_tmp, router, state) = fixture();
    let kp = fauna_core::identity::ActorKeypair::generate();
    let author = kp.actor_id().0;

    // 1. Create 3 posts authored by `author` (connection actor == author so the
    //    bearer can snapshot its own `__post` scope).
    let bodies: Vec<Vec<u8>> = (0..3)
        .map(|i| signed_text_post(&kp, &format!("snapshot-restore post {i}")))
        .collect();
    let post_ids: Vec<[u8; 32]> = bodies.iter().map(|b| *blake3::hash(b).as_bytes()).collect();
    for body in &bodies {
        create_post(&router, &state, author, body).await;
    }

    // Sanity: all 3 are in the cross-actor feed and individually readable.
    let feed = feed_post_ids(&state).await;
    for pid in &post_ids {
        assert!(
            feed.iter().any(|p| p == pid),
            "post {pid:?} present in feed pre-snapshot"
        );
    }

    // 2. Snapshot the author's `__post` segment (bearer == author).
    let create_reply: SnapshotCreateMessageKindReply = decode(
        &dispatch(
            &router,
            state.clone(),
            author,
            "fauna.filesync.snapshot.create_message_kind",
            Bytes::from(
                encode_canonical(&SnapshotCreateMessageKindRequest {
                    kind: "post".into(),
                    actor_id: Some(fauna_core::identity::ActorId(author)),
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await,
    )
    .unwrap();
    let snapshot_id = create_reply.snapshot_id;
    assert_eq!(create_reply.kind, "post");
    assert!(snapshot_id > 0, "a post snapshot row was created");

    // 3. Simulate a fresh / recovery nest: drop the mirror + feed-index
    //    projection for the author's posts, but KEEP the on-disk segment files
    //    (the snapshot's pinned data, as if pulled via fauna-sync) and the
    //    snapshot row. After this `query_feed` returns nothing for the author.
    {
        let conn = state.db.conn().await;
        conn.execute(
            "DELETE FROM segment_records WHERE scope_id = ?1 AND kind = 'post'",
            rusqlite::params![author.as_slice()],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM content WHERE author = ?1 AND schema LIKE 'post/%'",
            rusqlite::params![author.as_slice()],
        )
        .unwrap();
    }
    let feed_after_wipe = feed_post_ids(&state).await;
    for pid in &post_ids {
        assert!(
            !feed_after_wipe.iter().any(|p| p == pid),
            "post {pid:?} gone from feed after the wipe (fresh-nest simulation)"
        );
    }

    // 4. Restore (bearer == author). No bridge precondition, config_present N/A.
    let restore_reply: SnapshotRestoreMessageKindReply = decode(
        &dispatch(
            &router,
            state.clone(),
            author,
            "fauna.filesync.snapshot.restore_message_kind",
            Bytes::from(
                encode_canonical(&SnapshotRestoreMessageKindRequest {
                    snapshot_id,
                    confirm_id: snapshot_id.to_string(),
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await,
    )
    .unwrap();
    assert_eq!(restore_reply.kind, "post");
    assert!(
        restore_reply.config_present,
        "posts have no bridge AUTH dependency, so config_present is true"
    );

    // 5a. The body-resolving mirror was rebuilt: every post is readable again by
    //     id, segment-first (the inline content.payload was wiped too).
    for (body, pid) in bodies.iter().zip(&post_ids) {
        let get_reply: PostGetReply = decode(
            &dispatch(
                &router,
                state.clone(),
                author,
                "fauna.posts.get",
                Bytes::from(
                    encode_canonical(&PostGetRequest {
                        post_id: hex::encode(pid),
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
            *body,
            "posts.get serves the restored body from the rebuilt mirror"
        );
    }

    // 5b. The cross-actor feed-index projection was rebuilt: every post is back
    //     in query_feed (the post-specific half conv has no analogue of).
    let feed_after_restore = feed_post_ids(&state).await;
    for pid in &post_ids {
        assert!(
            feed_after_restore.iter().any(|p| p == pid),
            "post {pid:?} back in the feed after restore (projection rebuilt)"
        );
    }
}
