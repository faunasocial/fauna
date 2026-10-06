//! tier_3: **a legal takedown's withhold survives the author's own DELETE**
//! (`docs/goal/behavior/moderation.md` § Legal takedown → *Posts*;
//! `docs/goal/architecture/account-data-plane.md` § Nest-side requirements
//! item 1, *Payload stores* decision (6)).
//!
//! Every post-side takedown withhold keys on `content_meta.legal_takedown_ref`
//! — the export's segment-pair set and the blob door's rebuilt set alike — and
//! the author's own delete removes exactly that row while only *tombstoning*
//! the mirror, so the segment file keeps the compelled bytes until compaction
//! reclaims them. Before this the next `include_blobs` export therefore shipped
//! the `post` pair verbatim, with the taken-down body inside.
//!
//! The delete is not the door to close: it destroys where the moderation verbs
//! gate (`../../docs/goal/ui/feed.md` § Post deletion — "three verbs, three
//! owners, no overlap"), it is the *user always controls their data*
//! affordance, and the same core runs under account deletion and admin
//! eviction, which a refusal would wedge forever on one taken-down post. So the
//! FACT moves instead, captured inside the delete while the flag is still
//! authoritative.
//!
//! Driven over router dispatch, the production path: create → takedown →
//! delete, then the two reads the export's pair leg composes.

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{moderation_handlers, posts_handlers};
use fauna_protocol::{
    encode_canonical,
    moderation::ModerationLegalTakedownRequest,
    posts::{PostCreateRequest, PostDeleteRequest},
};

async fn nest() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    moderation_handlers::register_moderation_handlers(&mut b);
    (b.build(), state)
}

/// Create a post through the real `fauna.posts.create` wire path — fixture
/// setup, not the mutation under test (the takedown and the delete are).
/// Returns its id, hex and bytes.
async fn create_post(
    router: &RpcRouter,
    state: &Arc<AppState>,
    kp: &fauna_core::identity::ActorKeypair,
    text: &str,
) -> ([u8; 32], String) {
    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::encoding::sign_and_pack;
    let post = Post {
        author: kp.actor_id(),
        created_at: Timestamp(1_700_000_000_000_000),
        body: PostBody::Text {
            content: text.into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let body = sign_and_pack(kp, &post).unwrap();
    let post_id = *blake3::hash(&body).as_bytes();
    dispatch(
        router,
        state.clone(),
        kp.actor_id().0,
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
    .await
    .expect("create ok");
    (post_id, hex::encode(post_id))
}

fn takedown_payload(post_id_hex: &str, reference: &str) -> Bytes {
    Bytes::from(
        encode_canonical(&ModerationLegalTakedownRequest {
            content_id: post_id_hex.to_string(),
            content_type: "post".into(),
            legal_reference: reference.into(),
            restore: false,
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    )
}

fn signed_tombstone(kp: &fauna_core::identity::ActorKeypair, post_id: [u8; 32]) -> Bytes {
    use fauna_core::data::{PostId, Timestamp, Tombstone};
    use fauna_core::encoding::sign_and_pack;
    let tombstone = Tombstone {
        author: kp.actor_id(),
        post_id: PostId::from_digest_dag_cbor(post_id),
        created_at: Timestamp(1_700_000_001_000_000),
    };
    Bytes::from(
        encode_canonical(&PostDeleteRequest {
            body: serde_bytes::ByteBuf::from(sign_and_pack(kp, &tombstone).unwrap()),
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    )
}

/// The whole probe:
/// takedown → the author's own delete → the two reads the export's pair leg
/// composes both still answer, so the pair is still withheld.
///
/// **Why the second read is asserted separately.** The obvious fix — union the
/// deleted post's id into the withhold set — is a *no-op on its own*, because
/// the set is resolved to a segment through a lookup that filters
/// `tombstoned = 0`, and a deleted record's mirror row is tombstoned the
/// instant the delete lands. So the id would resolve to `None` and nothing
/// would be withheld, in the exact case the withhold exists for. The
/// tombstone-inclusive lookup is the load-bearing half, and this test pins the
/// live-only one still answering `None` so a future simplification cannot
/// quietly collapse the two.
#[tokio::test]
async fn a_taken_down_posts_pair_stays_withheld_after_its_author_deletes_it() {
    let (router, state) = nest().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let admin = [0x2Au8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();

    let (post_id, post_hex) = create_post(&router, &state, &author, "the compelled post").await;

    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        takedown_payload(&post_hex, "EU-DSA-2026/710"),
    )
    .await
    .expect("takedown ok");
    assert_eq!(
        state
            .db
            .get_post_legal_takedown(&post_id)
            .await
            .unwrap()
            .as_deref(),
        Some("EU-DSA-2026/710"),
        "the flag must land, or this test asserts a withhold no takedown exists for"
    );
    assert!(
        state
            .db
            .taken_down_deleted_post_ids_for_author(&author.actor_id().0)
            .await
            .unwrap()
            .is_empty(),
        "nothing is captured while the post is merely taken down — the live flag \
         is what the withhold reads, and it is still there"
    );

    // The author's own delete — the discretionary act that used to undo the
    // compelled one.
    dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.posts.delete",
        signed_tombstone(&author, post_id),
    )
    .await
    .expect("delete ok");
    assert!(
        state
            .db
            .get_post_legal_takedown(&post_id)
            .await
            .unwrap()
            .is_none(),
        "the delete takes the flag's row with it — which is the gap, not a bug \
         in the delete"
    );

    // Read 1: the export's pair set now names the post anyway.
    assert_eq!(
        state
            .db
            .taken_down_deleted_post_ids_for_author(&author.actor_id().0)
            .await
            .unwrap(),
        vec![post_id],
        "the delete captured the compelled fact on its way past"
    );

    // Read 2: and it resolves to the segment that still holds the bytes.
    let cid = fauna_cbor::Cid::from_digest_dag_cbor(post_id);
    assert!(
        state
            .db
            .segment_records_lookup_scope_and_segment(fauna_nest::segments::post::KIND, &cid)
            .await
            .unwrap()
            .is_none(),
        "the live-only lookup answers None for a deleted record — this is why \
         unioning the id alone would withhold nothing"
    );
    let (scope, _seg) = state
        .db
        .segment_records_lookup_scope_and_segment_including_tombstoned(
            fauna_nest::segments::post::KIND,
            &cid,
        )
        .await
        .unwrap()
        .expect("the tombstone-inclusive lookup finds the segment still holding the bytes");
    assert_eq!(
        scope,
        author.actor_id().0,
        "a post's segment scope is its author, which is what scopes the withhold"
    );
}

/// The opposite ordering:
/// the author's own delete beats the compelled order there — takedown is
/// dispatched AFTER the delete, against a post whose `content`/`content_meta`
/// rows are already gone. Before this the admin got `not_found` (nothing to
/// withhold, no transparency triple, no record) and the compelled bytes rode
/// the segment until compaction and the blob door until the next GC sweep,
/// same as any other deleted post.
///
/// Driven over router dispatch, the production path: create → delete →
/// takedown, then the same two reads
/// [`a_taken_down_posts_pair_stays_withheld_after_its_author_deletes_it`]
/// uses to prove the export's pair leg withholds it, plus the transparency
/// rows this ordering is what closes the gap for.
#[tokio::test]
async fn a_legal_takedown_still_lands_after_the_authors_delete_beat_it_there() {
    let (router, state) = nest().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let admin = [0x2Bu8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();

    let (post_id, post_hex) =
        create_post(&router, &state, &author, "deleted before the order arrived").await;

    // The author deletes it first — an ORDINARY delete, no takedown flag
    // exists yet.
    dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.posts.delete",
        signed_tombstone(&author, post_id),
    )
    .await
    .expect("delete ok");
    assert!(
        state
            .db
            .get_content_author(&post_id)
            .await
            .unwrap()
            .is_none(),
        "the content row is gone — this is the state a genuinely unknown \
         post and a deleted one share"
    );

    // The compelled order arrives afterward.
    let bytes = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        takedown_payload(&post_hex, "EU-DSA-2026/716"),
    )
    .await
    .expect("takedown still lands on a deleted post");
    let reply: fauna_protocol::moderation::ModerationLegalTakedownReply =
        fauna_protocol::decode_strict(&bytes).unwrap();
    assert_eq!(reply.status, "taken_down");

    // No content_meta row ever existed to flag.
    assert_eq!(
        state.db.get_post_legal_takedown(&post_id).await.unwrap(),
        None
    );

    // The author still sees it in their queue.
    let actions_bytes = dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.moderation.actions",
        Bytes::from(
            encode_canonical(&fauna_protocol::moderation::ModerationActionsRequest::default())
                .unwrap()
                .to_vec(),
        ),
    )
    .await
    .expect("author reads their queue");
    let actions: fauna_protocol::moderation::ModerationActionsReply =
        fauna_protocol::decode_strict(&actions_bytes).unwrap();
    assert!(
        actions.actions.iter().any(|a| a.content_id == post_hex
            && a.action == fauna_core::obligation::ObligationAction::TakenDown as u8),
        "the author's queue carries the takedown row even though no \
         content_meta row exists"
    );

    // The permanent audit row landed.
    let audits = state.db.list_audit(50, None).await.unwrap();
    assert!(
        audits
            .iter()
            .any(|a| a.action == "moderation:legal-takedown"),
        "a compelled order against a deleted post still audits"
    );

    // The export's pair set names the post — same proof the sibling test
    // above uses for the opposite ordering.
    assert_eq!(
        state
            .db
            .taken_down_deleted_post_ids_for_author(&author.actor_id().0)
            .await
            .unwrap(),
        vec![post_id],
        "the takedown-of-a-deleted-post arm captured the compelled fact"
    );

    // restore=true against this row is audit-only: the floor is unchanged.
    let bytes = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        Bytes::from(
            encode_canonical(&ModerationLegalTakedownRequest {
                content_id: post_hex.clone(),
                content_type: "post".into(),
                legal_reference: "appeal upheld".into(),
                restore: true,
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("restore against a deleted-and-taken-down post succeeds");
    let reply: fauna_protocol::moderation::ModerationLegalTakedownReply =
        fauna_protocol::decode_strict(&bytes).unwrap();
    assert_eq!(reply.status, "restored");
    assert_eq!(
        state
            .db
            .taken_down_deleted_post_ids_for_author(&author.actor_id().0)
            .await
            .unwrap(),
        vec![post_id],
        "the floor row is permanent and inert — a restore audits, it does \
         not clear the row (moderation.md § Legal takedown -> Posts)"
    );
}

/// Once physical reclaim has dropped the segment file, the deleted-post arm
/// must refuse `not_found` exactly as it would for a post this nest never
/// held, never fabricate a takedown it cannot back with digests.
///
/// **Why this deletes the segment file directly rather than calling
/// `compact_bucket`.** `segment_records` rows are tombstoned, never deleted
/// (`records_db::tombstone_input_segments`), so the tombstone-inclusive
/// LOOKUP stays `Some` forever regardless of compaction — and with zero
/// survivors in the bucket, `compact()` returns `Ok(None)` without writing or
/// removing anything (`fauna_segment_store::compaction::compact`'s own
/// doc: "No new segment was written"), so it would not reach this state
/// either. What actually removes the bytes is a later physical-reclaim sweep
/// (`FramedSegmentStore::delete_segment`, "used by GC after retention
/// window") — simulated directly here.
#[tokio::test]
async fn a_legal_takedown_after_physical_reclaim_is_not_found() {
    let (router, state) = nest().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let admin = [0x2Cu8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();

    let (post_id, post_hex) = create_post(
        &router,
        &state,
        &author,
        "deleted and then physically reclaimed",
    )
    .await;

    dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.posts.delete",
        signed_tombstone(&author, post_id),
    )
    .await
    .expect("delete ok");

    let cid = fauna_cbor::Cid::from_digest_dag_cbor(post_id);
    let (scope, seg_id) = state
        .db
        .segment_records_lookup_scope_and_segment_including_tombstoned(
            fauna_nest::segments::post::KIND,
            &cid,
        )
        .await
        .unwrap()
        .expect("the tombstoned record is still in a segment before reclaim");

    // Force the manager to finalize the (possibly still-open) segment —
    // writing its `.meta` sidecar — before the file is removed out from
    // under it; a physical reclaim sweep only ever runs against a finalized
    // segment.
    state
        .post_segments
        .finalize_open(&scope)
        .await
        .expect("finalize");

    std::fs::remove_file(state.post_segments.segment_file_path(&scope, seg_id))
        .expect("remove the segment file to simulate physical reclaim");
    std::fs::remove_file(state.post_segments.segment_meta_path(&scope, seg_id)).ok();

    // The mirror row outlives the file...
    assert!(
        state
            .db
            .segment_records_lookup_scope_and_segment_including_tombstoned(
                fauna_nest::segments::post::KIND,
                &cid,
            )
            .await
            .unwrap()
            .is_some(),
        "the tombstoned mirror row is never deleted — only the file is reclaimed"
    );
    // ...but the bytes are gone.
    assert_eq!(
        fauna_nest::segments::post::read_body_by_post_id_including_tombstoned(
            &state.post_segments,
            &state.db,
            &post_id,
        )
        .await
        .unwrap(),
        None,
        "the tombstone-inclusive reader must answer None once the file is gone"
    );

    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        takedown_payload(&post_hex, "EU-DSA-2026/716"),
    )
    .await
    .expect_err("a physically-reclaimed deleted post has nothing left to withhold");
    assert_eq!(err.code, "fauna.moderation.not_found");
}

/// The discriminator: an ordinary delete of a post nobody compelled captures
/// nothing, so no segment pair is ever withheld for it. What binds is the
/// takedown, never the deletion.
#[tokio::test]
async fn an_ordinary_delete_captures_nothing() {
    let (router, state) = nest().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let (post_id, _hex) = create_post(&router, &state, &author, "an ordinary post").await;

    dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.posts.delete",
        signed_tombstone(&author, post_id),
    )
    .await
    .expect("delete ok");

    assert!(
        state
            .db
            .taken_down_deleted_post_ids_for_author(&author.actor_id().0)
            .await
            .unwrap()
            .is_empty(),
        "an uncompelled post's delete must leave no withhold behind — the \
         author's archive keeps every pair it should"
    );
    assert!(
        state
            .db
            .taken_down_deleted_post_blob_digests()
            .await
            .unwrap()
            .is_empty(),
        "and nothing joins the blob door's withheld side"
    );
}
