//! Integration round-trip for `fauna.posts.{create,get,interact}` — a
//! behavior-preserving transport migration of the HTTP routes
//! `POST /api/v1/posts`, `GET /api/v1/posts/{id}`,
//! `POST /api/v1/posts/{id}/interact`. The handlers reuse the existing
//! ingest/interact pipeline (`routes::ingest_post_core`,
//! `routes::get_post_core`, `interact_routes::interact_with_post_core`) —
//! these tests exercise the WS-RPC layer: request decode, the reused
//! pipeline reaching real `CacheDb` + the one `Storage` impl
//! (`fauna_nest::storage::SealedStorage`), reply encoding, the quarantine
//! visibility gate keyed on the connection actor, and the allowlist.
//!
//! `AppState::for_test` installs `SealedStorage` and gives a real in-memory
//! `CacheDb`; `ingest_post` runs the strict seal-shape verifier
//! unconditionally — there is no storage-mode axis any more
//! (`docs/goal/architecture/nest/storage-modes.md`). Bodies are unique per
//! test because `post_id = blake3(body)` is content-addressed — re-posting
//! identical bytes can hit a duplicate-PK path (the known "publish_post 409
//! on duplicate" footgun).
//!
//! Authority for the wire types: `libs/fauna-protocol/src/posts.rs`.
//! Slice: tracked internally (§ T1).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` + real
//! `SealedStorage` — no mocks). Matches the conversations / search
//! conformance harnesses, which are likewise full-stack router dispatch.

mod common;
use common::{dispatch, signed_text_post, signed_text_post_with_origin};

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    posts_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    decode_strict as decode, encode_canonical,
    posts::{
        PostCreateReply, PostCreateRequest, PostGetReply, PostGetRequest, PostInteractReply,
        PostInteractRequest, PostsListReply, PostsListRequest,
    },
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    (b.build(), state)
}

fn create_payload(body: Vec<u8>) -> Bytes {
    let req = PostCreateRequest {
        body: serde_bytes::ByteBuf::from(body),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn get_payload(post_id_hex: &str) -> Bytes {
    let req = PostGetRequest {
        post_id: post_id_hex.into(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn interact_payload(post_id_hex: &str, action: &str, body: Option<&str>) -> Bytes {
    let req = PostInteractRequest {
        post_id: post_id_hex.into(),
        action: action.into(),
        body: body.map(|s| s.to_string()),
        media: None,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

// ── fauna.posts.create ─────────────────────────────────────────

#[tokio::test]
async fn create_returns_content_addressed_post_id() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let body = signed_text_post(&kp, "create_returns_content_addressed_post_id unique body");
    let expected = hex::encode(blake3::hash(&body).as_bytes());

    let reply_bytes = dispatch(
        &router,
        state,
        actor,
        "fauna.posts.create",
        create_payload(body),
    )
    .await
    .expect("create ok");
    let reply: PostCreateReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.post_id, expected, "post_id is hex(blake3(body))");
}

// ── fauna.posts.create → fauna.posts.get round-trip ────────────

#[tokio::test]
async fn create_then_get_round_trips_bytes() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let body = signed_text_post(&kp, "create_then_get_round_trips_bytes unique body payload");

    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.posts.create",
            create_payload(body.clone()),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();

    let get_reply: PostGetReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.posts.get",
            get_payload(&create_reply.post_id),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();
    assert_eq!(
        get_reply.body.into_vec(),
        body,
        "get returns the stored post bytes verbatim"
    );
}

#[tokio::test]
async fn get_unknown_post_is_not_found() {
    let (router, state) = router_with_db_only().await;
    let actor = [13u8; 32];
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.posts.get",
        get_payload(&"ab".repeat(32)),
    )
    .await
    .expect_err("unknown post not found");
    assert_eq!(err.code, "fauna.posts.not_found");
}

#[tokio::test]
async fn get_rejects_malformed_post_id() {
    let (router, state) = router_with_db_only().await;
    let actor = [14u8; 32];
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.posts.get",
        get_payload("not-hex"),
    )
    .await
    .expect_err("malformed post_id rejected");
    assert_eq!(err.code, "fauna.posts.invalid_params");
}

// ── quarantine visibility gate ─────────────────────────────────

#[tokio::test]
async fn get_quarantined_post_hidden_from_non_author() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let author = kp.actor_id().0;
    let body = signed_text_post(&kp, "get_quarantined_post_hidden_from_non_author");

    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            author,
            "fauna.posts.create",
            create_payload(body),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    let post_id = hex::decode(&create_reply.post_id).unwrap();
    let post_id: [u8; 32] = post_id.try_into().unwrap();

    // Quarantine the post directly. The signed-post ingest path created a
    // `content_meta` row (via `write_post_index`), so this UPDATE takes.
    state
        .db
        .set_post_quarantined(&post_id, true)
        .await
        .expect("quarantine");
    assert!(
        state.db.is_post_quarantined(&post_id).await.unwrap(),
        "post is quarantined after set_post_quarantined"
    );

    // A non-author / non-admin actor must NOT see the quarantined post —
    // the gate maps to not_found (the HTTP twin's 404).
    let other = [99u8; 32];
    let err = dispatch(
        &router,
        state.clone(),
        other,
        "fauna.posts.get",
        get_payload(&create_reply.post_id),
    )
    .await
    .expect_err("quarantined post hidden from non-author");
    assert_eq!(err.code, "fauna.posts.not_found");

    // The author (the signed post's actor) passes the gate and sees the post.
    let get_reply: PostGetReply = decode(
        &dispatch(
            &router,
            state,
            author,
            "fauna.posts.get",
            get_payload(&create_reply.post_id),
        )
        .await
        .expect("author sees quarantined post"),
    )
    .unwrap();
    assert!(
        !get_reply.body.is_empty(),
        "the author sees the quarantined post body"
    );
}

// ── legal-obligation takedown serve gate ───────────────────────

/// A legally-taken-down post has its body **withheld from every viewer** — the
/// author AND a stranger (unlike quarantine, which the author still sees) —
/// with the tombstone reference returned in its place (moderation.md
/// § Categories & enforcement item 1). The illegal content is never served.
/// `restore` re-serves it (tombstone, not delete).
#[tokio::test]
async fn get_legally_taken_down_post_withholds_body_and_returns_tombstone() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let author = kp.actor_id().0;
    let body = signed_text_post(&kp, "get_legally_taken_down_post withholds body");

    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            author,
            "fauna.posts.create",
            create_payload(body),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    let post_id: [u8; 32] = hex::decode(&create_reply.post_id)
        .unwrap()
        .try_into()
        .unwrap();

    // Live: the author gets the body, no tombstone.
    let live: PostGetReply = decode(
        &dispatch(
            &router,
            state.clone(),
            author,
            "fauna.posts.get",
            get_payload(&create_reply.post_id),
        )
        .await
        .expect("get live"),
    )
    .unwrap();
    assert!(!live.body.is_empty(), "a live post serves its body");
    assert!(live.legal_takedown.is_none());

    // Take it down under a legal obligation.
    state
        .db
        .set_post_legal_takedown(&post_id, Some("EU-DSA-2024/12345"))
        .await
        .expect("takedown");

    // Every viewer — the author AND a stranger — gets an EMPTY body + tombstone.
    for viewer in [author, [0x99u8; 32]] {
        let reply: PostGetReply = decode(
            &dispatch(
                &router,
                state.clone(),
                viewer,
                "fauna.posts.get",
                get_payload(&create_reply.post_id),
            )
            .await
            .expect("get taken-down"),
        )
        .unwrap();
        assert!(
            reply.body.is_empty(),
            "the illegal content body is withheld from {viewer:?}"
        );
        let marker = reply
            .legal_takedown
            .expect("the tombstone marker is present in place of the body");
        assert_eq!(marker.reference, "EU-DSA-2024/12345");
    }

    // Restore re-serves the body (tombstone, not delete).
    state
        .db
        .set_post_legal_takedown(&post_id, None)
        .await
        .expect("restore");
    let restored: PostGetReply = decode(
        &dispatch(
            &router,
            state,
            author,
            "fauna.posts.get",
            get_payload(&create_reply.post_id),
        )
        .await
        .expect("get restored"),
    )
    .unwrap();
    assert!(
        !restored.body.is_empty(),
        "restoring an overturned takedown re-serves the body"
    );
    assert!(restored.legal_takedown.is_none());
}

// ── fauna.posts.interact ───────────────────────────────────────

#[tokio::test]
async fn interact_like_native_fauna_returns_ok() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let body = signed_text_post(&kp, "interact_like_native_fauna_returns_ok unique body");

    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.posts.create",
            create_payload(body),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();

    let reply: PostInteractReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.posts.interact",
            interact_payload(&create_reply.post_id, "like", None),
        )
        .await
        .expect("interact like ok"),
    )
    .unwrap();
    assert_eq!(reply.action, "like");
    assert_eq!(
        reply.source, "fauna",
        "a nest-native post's interact source is fauna"
    );
    let result: serde_json::Value = serde_json::from_str(&reply.result).unwrap();
    assert_eq!(result, serde_json::json!({ "ok": true }));

    // The reply carries the target's counters AFTER the act, which is what lets
    // a client move the tapped count without re-querying the feed
    // (`ui/feed.md` § Interaction bar). Over the real router + db, not a mock.
    let counts = reply.counts.expect("a native like reports its counts");
    assert_eq!(counts.like_count, 1, "the like this call just applied");
    assert_eq!(
        (counts.reply_count, counts.repost_count, counts.quote_count),
        (0, 0, 0),
        "the untouched counters ride along at their real values"
    );
}

/// Ruling 1's other half (`archive-import.md` § Compatibility → *Slice-3
/// rulings*, § Testing): an archive-origin public post is **served on
/// Fauna** — present in the feed read — while absent from every off-box
/// surface (the outbox, projection and materializer pins live beside those
/// consumers). The local feed is the one feed read that needs no feed row,
/// and it runs `db/feeds.rs`'s gate, the site the servability predicate
/// deliberately leaves untouched.
#[tokio::test]
async fn an_archive_imported_public_post_is_served_in_the_feed_read() {
    use fauna_protocol::feed::{FeedLocalPostsReply, FeedLocalPostsRequest};

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    fauna_nest::feed_handlers::register_feed_handlers(&mut b);
    let router = b.build();

    let kp = fauna_core::identity::ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let body = signed_text_post_with_origin(
        &kp,
        "an_archive_imported_public_post_is_served_in_the_feed_read unique body",
        fauna_core::source::FACEBOOK,
    );
    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.posts.create",
            create_payload(body),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();

    let feed: FeedLocalPostsReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.local.posts",
            Bytes::from(
                encode_canonical(&FeedLocalPostsRequest {
                    cursor: None,
                    limit: Some(20),
                    search: None,
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("feed read ok"),
    )
    .unwrap();
    let item = feed
        .posts
        .iter()
        .find(|p| p.post_id == create_reply.post_id)
        .expect("the imported post is served in the feed read");
    assert_eq!(item.author, hex::encode(actor));
}

/// An archive-imported post is indexed under its origin platform and is still
/// NATIVE: the like lands on the nest's own counters, not on a bridge door
/// (`archive-import.md` § Compatibility; `interact_routes` native arm). This is
/// what every client gets — it routes by `source == "fauna"` and sends such a
/// like to `posts.interact`, which must not answer 501.
#[tokio::test]
async fn interact_like_on_an_archive_imported_post_routes_native() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let body = signed_text_post_with_origin(
        &kp,
        "interact_like_on_an_archive_imported_post_routes_native unique body",
        fauna_core::source::FACEBOOK,
    );

    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.posts.create",
            create_payload(body),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();

    let post_id: [u8; 32] = fauna_core::hex32::decode(&create_reply.post_id).unwrap();
    assert_eq!(
        state.db.get_post_source(&post_id).await.unwrap().as_deref(),
        Some("facebook"),
        "the nest indexes the origin platform as the post's source"
    );

    let reply: PostInteractReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.posts.interact",
            interact_payload(&create_reply.post_id, "like", None),
        )
        .await
        .expect("a like on an imported post is native, never a bridge 501"),
    )
    .unwrap();
    assert_eq!(reply.action, "like");
    assert_eq!(
        reply.source, "facebook",
        "the outcome echoes the indexed token"
    );
    let counts = reply.counts.expect("a native like reports its counts");
    assert_eq!(counts.like_count, 1);

    // Ruling 3 (`archive-import.md` § Compatibility → *Slice-3 rulings*): a
    // reply/repost/quote of an imported post is composed as a signed post by
    // the app (`FeedManager::compose_referencing_post`, native routing). The
    // only caller that reaches this door with one is a non-conforming client
    // that routes the post as bridged — it expects the "bridge" to post its
    // body, and the target-info answer would drop that body silently. So the door refuses, loudly, naming the cause. `fauna`-token
    // posts keep the legacy answer (pinned by the sibling test above).
    for action in ["reply", "repost", "quote"] {
        let err = dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.posts.interact",
            interact_payload(
                &create_reply.post_id,
                action,
                Some("from a caller that routed the post as bridged"),
            ),
        )
        .await
        .expect_err("an archive-token post refuses the door's compose actions");
        // `RpcError.message` is a `LocalizedText` key, not free text — the
        // actual refusal wording rides in `details` (`detail_or_code()` is
        // its decoder-side accessor, falling back to `code` when absent).
        let detail = err.detail_or_code();
        assert!(
            detail.contains("composed by the app as a signed post"),
            "{action}: the refusal names the cause: {detail}"
        );
        assert!(
            !detail.contains("501") && err.code != "fauna.protocol.unknown_kind",
            "{action}: a refusal, not a not-implemented"
        );
    }
    // `unlike` stays served on every native token.
    let unlike: PostInteractReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.posts.interact",
            interact_payload(&create_reply.post_id, "unlike", None),
        )
        .await
        .expect("unlike on an imported post is native"),
    )
    .unwrap();
    assert_eq!(unlike.counts.expect("counts").like_count, 0);
}

/// A discovery-indexed federation stub — `content.origin_nest_url` set, per
/// `insert_post_index_entry_with_origin` — stays refused with the legacy
/// not-implemented answer EVEN when its `source` token is `"fauna"`: unlike an
/// archive-imported post (native, above), this nest has no payload and no
/// custody of the underlying post, so a native-looking token must not open the
/// native arm and mutate counters this nest does not actually own.
#[tokio::test]
async fn interact_on_a_discovery_indexed_post_stays_refused_even_with_a_fauna_token() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let actor = kp.actor_id().0;

    let mut post_id = [0u8; 32];
    getrandom::fill(&mut post_id).unwrap();
    let mut author = [0u8; 32];
    getrandom::fill(&mut author).unwrap();
    state
        .db
        .insert_post_index_entry_with_origin(
            &post_id,
            &author,
            1_700_000_000,
            false,
            false,
            "fauna",
            &[],
            Some("https://peer.example"),
        )
        .await
        .unwrap();
    assert_eq!(
        state.db.get_post_source(&post_id).await.unwrap().as_deref(),
        Some("fauna"),
        "the token alone must look native"
    );

    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.posts.interact",
        interact_payload(&hex::encode(post_id), "like", None),
    )
    .await
    .expect_err("a discovery-indexed post must not open the native arm");
    assert_eq!(
        err.code, "fauna.posts.unsupported",
        "origin_nest_url gates it to the not-implemented answer, not native"
    );
}

/// The counters on the reply are the nest's own, and the nest's like counter is
/// **idempotent per (actor, post)** — so a repeat like reports the SAME number.
/// This is the case that makes a client-side optimistic `+1` wrong, and the
/// reason the wire carries the value at all rather than a "it moved" bit.
#[tokio::test]
async fn a_repeat_like_reports_the_same_count_rather_than_two() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let body = signed_text_post(&kp, "a_repeat_like_reports_the_same_count unique body");

    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.posts.create",
            create_payload(body),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();

    let mut seen = Vec::new();
    for _ in 0..2 {
        let reply: PostInteractReply = decode(
            &dispatch(
                &router,
                state.clone(),
                actor,
                "fauna.posts.interact",
                interact_payload(&create_reply.post_id, "like", None),
            )
            .await
            .expect("interact like ok"),
        )
        .unwrap();
        seen.push(reply.counts.expect("counts present").like_count);
    }
    assert_eq!(seen, vec![1, 1], "the same actor's second like counts once");
}

#[tokio::test]
async fn interact_reply_native_fauna_echoes_target() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let body = signed_text_post(&kp, "interact_reply_native_fauna_echoes_target unique body");

    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.posts.create",
            create_payload(body),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();

    let reply: PostInteractReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.posts.interact",
            interact_payload(&create_reply.post_id, "reply", Some("hi there")),
        )
        .await
        .expect("interact reply ok"),
    )
    .unwrap();
    assert_eq!(reply.action, "reply");
    assert_eq!(reply.source, "fauna");
    let result: serde_json::Value = serde_json::from_str(&reply.result).unwrap();
    assert_eq!(
        result,
        serde_json::json!({
            "action": "reply",
            "target_post_id": create_reply.post_id,
            "source": "fauna",
        }),
        "reply echoes the target post info so the client can compose"
    );
}

/// Build a real signed repost post (a `Reference::Repost` targeting
/// `target_digest`), the shape a client composes after the `repost` interact
/// action returns the target info (§ Interaction bar).
fn signed_repost_post(
    kp: &fauna_core::identity::ActorKeypair,
    target_digest: [u8; 32],
    marker: &str,
) -> Vec<u8> {
    use fauna_core::data::{Post, PostBody, PostId, Reference, Timestamp};
    use fauna_core::encoding::sign_and_pack;
    let post = Post {
        author: kp.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Text {
            content: marker.into(),
            facets: vec![],
        },
        references: vec![Reference::Repost {
            post_id: PostId::from_digest_dag_cbor(target_digest),
        }],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    sign_and_pack(kp, &post).unwrap()
}

#[tokio::test]
async fn unrepost_removes_the_repost_post_and_reverses_the_counter() {
    let (router, state) = router_with_db_only().await;
    let target_kp = fauna_core::identity::ActorKeypair::generate();
    let reposter_kp = fauna_core::identity::ActorKeypair::generate();

    let target_body = signed_text_post(&target_kp, "unrepost target");
    let target: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            target_kp.actor_id().0,
            "fauna.posts.create",
            create_payload(target_body),
        )
        .await
        .expect("create target ok"),
    )
    .unwrap();
    let mut target_digest = [0u8; 32];
    hex::decode_to_slice(&target.post_id, &mut target_digest).unwrap();

    let repost_body = signed_repost_post(&reposter_kp, target_digest, "unrepost repost");
    let repost: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            reposter_kp.actor_id().0,
            "fauna.posts.create",
            create_payload(repost_body),
        )
        .await
        .expect("create repost ok"),
    )
    .unwrap();

    let counts = state
        .db
        .get_engagement_counts(&target_digest)
        .await
        .expect("counts");
    assert_eq!(
        counts.repost_count, 1,
        "the repost bumped the target's counter"
    );

    let reply: PostInteractReply = decode(
        &dispatch(
            &router,
            state.clone(),
            reposter_kp.actor_id().0,
            "fauna.posts.interact",
            interact_payload(&repost.post_id, "unrepost", None),
        )
        .await
        .expect("unrepost ok"),
    )
    .unwrap();
    assert_eq!(reply.action, "unrepost");
    assert_eq!(reply.source, "fauna");
    let result: serde_json::Value = serde_json::from_str(&reply.result).unwrap();
    assert_eq!(result, serde_json::json!({ "ok": true }));

    // The repost post itself is gone.
    let mut repost_digest = [0u8; 32];
    hex::decode_to_slice(&repost.post_id, &mut repost_digest).unwrap();
    let get_err = dispatch(
        &router,
        state.clone(),
        reposter_kp.actor_id().0,
        "fauna.posts.get",
        get_payload(&repost.post_id),
    )
    .await
    .expect_err("the repost post is gone");
    assert_eq!(get_err.code, "fauna.posts.not_found");

    // The target's counter reversed.
    let counts = state
        .db
        .get_engagement_counts(&target_digest)
        .await
        .expect("counts");
    assert_eq!(counts.repost_count, 0, "unrepost reversed the counter");

    // Idempotent: a repeat unrepost is not an error (the post is already gone
    // — same idempotent semantics as `fauna.posts.delete`), and never
    // double-reverses the counter.
    let reply_again: PostInteractReply = decode(
        &dispatch(
            &router,
            state.clone(),
            reposter_kp.actor_id().0,
            "fauna.posts.interact",
            interact_payload(&repost.post_id, "unrepost", None),
        )
        .await
        .expect("repeat unrepost ok"),
    )
    .unwrap();
    let result_again: serde_json::Value = serde_json::from_str(&reply_again.result).unwrap();
    assert_eq!(result_again, serde_json::json!({ "ok": true }));
    let counts = state
        .db
        .get_engagement_counts(&target_digest)
        .await
        .expect("counts");
    assert_eq!(
        counts.repost_count, 0,
        "a repeat unrepost does not double-reverse"
    );
}

#[tokio::test]
async fn unrepost_rejects_a_non_author() {
    let (router, state) = router_with_db_only().await;
    let target_kp = fauna_core::identity::ActorKeypair::generate();
    let reposter_kp = fauna_core::identity::ActorKeypair::generate();
    let intruder_kp = fauna_core::identity::ActorKeypair::generate();

    let target_body = signed_text_post(&target_kp, "unrepost_rejects_a_non_author target");
    let target: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            target_kp.actor_id().0,
            "fauna.posts.create",
            create_payload(target_body),
        )
        .await
        .expect("create target ok"),
    )
    .unwrap();
    let mut target_digest = [0u8; 32];
    hex::decode_to_slice(&target.post_id, &mut target_digest).unwrap();

    let repost_body = signed_repost_post(
        &reposter_kp,
        target_digest,
        "unrepost_rejects_a_non_author repost",
    );
    let repost: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            reposter_kp.actor_id().0,
            "fauna.posts.create",
            create_payload(repost_body),
        )
        .await
        .expect("create repost ok"),
    )
    .unwrap();

    let err = dispatch(
        &router,
        state.clone(),
        intruder_kp.actor_id().0,
        "fauna.posts.interact",
        interact_payload(&repost.post_id, "unrepost", None),
    )
    .await
    .expect_err("only the repost's author may unrepost it");
    assert_eq!(err.code, "fauna.posts.permission_denied");

    let counts = state
        .db
        .get_engagement_counts(&target_digest)
        .await
        .expect("counts");
    assert_eq!(
        counts.repost_count, 1,
        "the rejected attempt did not reverse the counter"
    );
}

#[tokio::test]
async fn unrepost_rejects_a_non_repost_post() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let body = signed_text_post(&kp, "unrepost_rejects_a_non_repost_post plain post");

    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.posts.create",
            create_payload(body),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();

    let err = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.posts.interact",
        interact_payload(&create_reply.post_id, "unrepost", None),
    )
    .await
    .expect_err("a plain post is not a repost");
    assert_eq!(err.code, "fauna.posts.not_found");

    // The post survives — unrepost must never delete a non-repost post.
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.posts.get",
        get_payload(&create_reply.post_id),
    )
    .await
    .expect("the plain post still exists");
}

#[tokio::test]
async fn interact_rejects_invalid_action() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let body = signed_text_post(&kp, "interact_rejects_invalid_action unique body");

    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.posts.create",
            create_payload(body),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();

    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.posts.interact",
        interact_payload(&create_reply.post_id, "frobnicate", None),
    )
    .await
    .expect_err("invalid action rejected");
    assert_eq!(err.code, "fauna.posts.invalid_params");
}

#[tokio::test]
async fn interact_unknown_post_is_not_found() {
    let (router, state) = router_with_db_only().await;
    let actor = [34u8; 32];
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.posts.interact",
        interact_payload(&"cd".repeat(32), "like", None),
    )
    .await
    .expect_err("interact on unknown post not found");
    assert_eq!(err.code, "fauna.posts.not_found");
}

// ── replay metadata ────────────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_with_db_only().await;
    // get: pure read → replay-safe @5s.
    let get = router.kind_meta("fauna.posts.get").expect("get registered");
    assert!(!get.forbid_replay, "posts.get is a replay-safe pure read");
    assert_eq!(get.default_deadline, std::time::Duration::from_secs(5));
    // create: content-addressed → idempotent → replay-safe @30s.
    let create = router
        .kind_meta("fauna.posts.create")
        .expect("create registered");
    assert!(
        !create.forbid_replay,
        "posts.create is replay-safe (content-addressed post_id)"
    );
    assert_eq!(create.default_deadline, std::time::Duration::from_secs(30));
    // interact: like increments a non-idempotent score → forbids replay @5s.
    let interact = router
        .kind_meta("fauna.posts.interact")
        .expect("interact registered");
    assert!(
        interact.forbid_replay,
        "posts.interact forbids replay (like double-counts)"
    );
    assert_eq!(interact.default_deadline, std::time::Duration::from_secs(5));
}

// ── allowlist ──────────────────────────────────────────────────

#[tokio::test]
async fn posts_kinds_are_user_and_admin_at_allowlist_layer() {
    // `fauna.posts.*` are User-class kinds; Admin inherits them via the
    // deliberate admin ⊇ user override (`bridge_method_allowlist::is_permitted`
    // head, commit "admin ⊇ user — admins inherit User-class permissions").
    // Only the bridge classes (which serve no first-party post surface) are
    // denied. (This test predated the override and asserted Admin denied —
    // stale; corrected to match the override.)
    for kind in [
        "fauna.posts.create",
        "fauna.posts.get",
        "fauna.posts.interact",
    ] {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                is_permitted(class, kind),
                "{kind} should be permitted for {class:?}"
            );
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, kind),
                "{kind} should be denied for {class:?}"
            );
        }
    }
}

// ── fauna.posts.delete ─────────────────────────────────────────
//
// The author-only self-service post deletion (`feed.md` § State & data
// shape → *Post deletion*, ratified 2026-07-15): a signed
// `fauna_core::data::Tombstone` (embed-as-bytes wire, signed-only — no
// bare fallback) whose author must be the connection actor AND the stored
// post's author. Idempotent: `deleted:false` is the already-gone success.

fn signed_tombstone(kp: &fauna_core::identity::ActorKeypair, post_id_hex: &str) -> Vec<u8> {
    use fauna_core::data::{PostId, Timestamp, Tombstone};
    use fauna_core::encoding::sign_and_pack;
    let mut digest = [0u8; 32];
    hex::decode_to_slice(post_id_hex, &mut digest).unwrap();
    let tombstone = Tombstone {
        author: kp.actor_id(),
        post_id: PostId::from_digest_dag_cbor(digest),
        created_at: Timestamp::now(),
    };
    sign_and_pack(kp, &tombstone).unwrap()
}

fn delete_payload(tombstone_wire: Vec<u8>) -> Bytes {
    let req = fauna_protocol::posts::PostDeleteRequest {
        body: serde_bytes::ByteBuf::from(tombstone_wire),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

#[tokio::test]
async fn delete_own_post_removes_it_and_is_idempotent() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let body = signed_text_post(&kp, "delete_own_post unique body");

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.posts.create",
        create_payload(body),
    )
    .await
    .expect("create ok");
    let created: PostCreateReply = decode(&reply_bytes).unwrap();

    // Delete it — newly removed.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.posts.delete",
        delete_payload(signed_tombstone(&kp, &created.post_id)),
    )
    .await
    .expect("delete ok");
    let deleted: fauna_protocol::posts::PostDeleteReply = decode(&reply_bytes).unwrap();
    assert_eq!(deleted.post_id, created.post_id);
    assert!(deleted.deleted, "first delete newly removes");

    // The post is gone from the read path.
    let err = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.posts.get",
        get_payload(&created.post_id),
    )
    .await
    .expect_err("deleted post must not serve");
    assert_eq!(err.code, "fauna.posts.not_found");

    // A repeat delete is the idempotent already-gone success, not an error.
    let reply_bytes = dispatch(
        &router,
        state,
        actor,
        "fauna.posts.delete",
        delete_payload(signed_tombstone(&kp, &created.post_id)),
    )
    .await
    .expect("repeat delete ok");
    let deleted: fauna_protocol::posts::PostDeleteReply = decode(&reply_bytes).unwrap();
    assert!(!deleted.deleted, "already gone");
}

#[tokio::test]
async fn delete_requires_the_author() {
    let (router, state) = router_with_db_only().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let attacker = fauna_core::identity::ActorKeypair::generate();
    let body = signed_text_post(&author, "delete_requires_the_author unique body");

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.posts.create",
        create_payload(body),
    )
    .await
    .expect("create ok");
    let created: PostCreateReply = decode(&reply_bytes).unwrap();

    // (1) A non-author's own validly-signed tombstone naming someone else's
    // post: signature verifies (it's the attacker's), but the stored post's
    // author doesn't match → permission_denied.
    let err = dispatch(
        &router,
        state.clone(),
        attacker.actor_id().0,
        "fauna.posts.delete",
        delete_payload(signed_tombstone(&attacker, &created.post_id)),
    )
    .await
    .expect_err("non-author must not delete");
    assert_eq!(err.code, "fauna.posts.permission_denied");

    // (2) A replayed AUTHOR-signed tombstone submitted from another actor's
    // connection: the connection actor must BE the tombstone author.
    let err = dispatch(
        &router,
        state.clone(),
        attacker.actor_id().0,
        "fauna.posts.delete",
        delete_payload(signed_tombstone(&author, &created.post_id)),
    )
    .await
    .expect_err("foreign connection must not replay an author tombstone");
    assert_eq!(err.code, "fauna.posts.permission_denied");

    // (3) Garbage that decodes to no signed tombstone → malformed.
    let err = dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.posts.delete",
        delete_payload(vec![0xDE, 0xAD, 0xBE, 0xEF]),
    )
    .await
    .expect_err("garbage tombstone must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // The post survives all three attempts.
    let ok = dispatch(
        &router,
        state,
        author.actor_id().0,
        "fauna.posts.get",
        get_payload(&created.post_id),
    )
    .await
    .expect("post still served");
    let got: PostGetReply = decode(&ok).unwrap();
    assert!(!got.body.is_empty());
}

#[tokio::test]
async fn delete_reverses_reference_counters() {
    let (router, state) = router_with_db_only().await;
    let target_kp = fauna_core::identity::ActorKeypair::generate();
    let replier_kp = fauna_core::identity::ActorKeypair::generate();

    // Target post.
    let target_body = signed_text_post(&target_kp, "delete_reverses_counters target");
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        target_kp.actor_id().0,
        "fauna.posts.create",
        create_payload(target_body),
    )
    .await
    .expect("create target ok");
    let target: PostCreateReply = decode(&reply_bytes).unwrap();
    let mut target_digest = [0u8; 32];
    hex::decode_to_slice(&target.post_id, &mut target_digest).unwrap();

    // A reply post referencing the target bumps the target's reply_count.
    let reply_post = {
        use fauna_core::data::{Post, PostBody, PostId, Reference, Timestamp};
        use fauna_core::encoding::sign_and_pack;
        let post = Post {
            author: replier_kp.actor_id(),
            created_at: Timestamp::now(),
            body: PostBody::Text {
                content: "delete_reverses_counters reply".into(),
                facets: vec![],
            },
            references: vec![Reference::Reply {
                post_id: PostId::from_digest_dag_cbor(target_digest),
            }],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        sign_and_pack(&replier_kp, &post).unwrap()
    };
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        replier_kp.actor_id().0,
        "fauna.posts.create",
        create_payload(reply_post),
    )
    .await
    .expect("create reply ok");
    let reply_created: PostCreateReply = decode(&reply_bytes).unwrap();

    let counts = state
        .db
        .get_engagement_counts(&target_digest)
        .await
        .expect("counts");
    assert_eq!(counts.reply_count, 1, "reply bumped the target's counter");

    // Deleting the reply reverses the target's counter.
    dispatch(
        &router,
        state.clone(),
        replier_kp.actor_id().0,
        "fauna.posts.delete",
        delete_payload(signed_tombstone(&replier_kp, &reply_created.post_id)),
    )
    .await
    .expect("delete reply ok");

    let counts = state
        .db
        .get_engagement_counts(&target_digest)
        .await
        .expect("counts");
    assert_eq!(counts.reply_count, 0, "delete reversed the counter");
}

/// Whether a live `content_scores` trending row exists for `content_id` —
/// direct SQL check, since the recompute writer withdraws by row deletion
/// rather than zeroing a score (`db/trends.rs` § module doc).
async fn trending_row_exists(state: &Arc<AppState>, content_id: &[u8; 32]) -> bool {
    use rusqlite::OptionalExtension;
    let conn = state.db.conn().await;
    conn.query_row(
        "SELECT 1 FROM content_scores WHERE content_id = ?1 AND factor = ?2",
        rusqlite::params![content_id.as_slice(), fauna_core::scoring::factor::TRENDING],
        |_| Ok(()),
    )
    .optional()
    .unwrap()
    .is_some()
}

#[tokio::test]
async fn delete_withdraws_the_trending_row_immediately() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let author = kp.actor_id().0;
    let liker = [0x42u8; 32];
    let body = signed_text_post(&kp, "delete_withdraws_the_trending_row_immediately");

    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            author,
            "fauna.posts.create",
            create_payload(body),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    let mut digest = [0u8; 32];
    hex::decode_to_slice(&create_reply.post_id, &mut digest).unwrap();

    // A real engagement act trend-recomputes on insert
    // (`db::engagement::insert` -> `recompute_trend_score_locked`), so this
    // post now carries a live trending row.
    dispatch(
        &router,
        state.clone(),
        liker,
        "fauna.posts.interact",
        interact_payload(&create_reply.post_id, "like", None),
    )
    .await
    .expect("like ok");
    assert!(
        trending_row_exists(&state, &digest).await,
        "the like produced a live trending row"
    );

    dispatch(
        &router,
        state.clone(),
        author,
        "fauna.posts.delete",
        delete_payload(signed_tombstone(&kp, &create_reply.post_id)),
    )
    .await
    .expect("delete ok");

    assert!(
        !trending_row_exists(&state, &digest).await,
        "delete withdraws the stale trending row immediately, not after the ≤15-min sweep"
    );
}

#[tokio::test]
async fn delete_removes_the_post_from_public_search_with_no_orphan_window() {
    let (router, state) = router_with_db_only().await;
    let kp = fauna_core::identity::ActorKeypair::generate();
    let actor = kp.actor_id().0;
    let body = signed_text_post(
        &kp,
        "delete_removes_the_post_from_public_search unique marker zzyzx",
    );

    let create_reply: PostCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.posts.create",
            create_payload(body),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();

    // Indexed and publicly searchable before deletion.
    let results = state
        .db
        .search_with_scoping("zzyzx", &[0u8; 32], None, None, None, 10, 0)
        .await
        .expect("search ok");
    assert!(
        results
            .iter()
            .any(|r| r.content_id.eq_ignore_ascii_case(&create_reply.post_id)),
        "the post is indexed and publicly searchable before deletion"
    );

    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.posts.delete",
        delete_payload(signed_tombstone(&kp, &create_reply.post_id)),
    )
    .await
    .expect("delete ok");

    // Pins the end-state contract the security review's post-deletion
    // phase-1 finding cares about: a deleted post's
    // text must never resurface in public search. This happy-path assertion
    // held even before `delete_post_projection`'s six removals were wrapped in
    // one transaction (each `conn.execute` already ran, just not atomically),
    // so it does not by itself prove the crash-window is closed — that
    // requires fault-injecting a crash mid-removal, which this codebase has no
    // harness for. The transaction wrap is the structural fix the finding
    // asked for (verified by code inspection, mirroring
    // `put_spam_model_with_history`'s pattern); this test guards the
    // observable contract against a future regression that drops the FTS
    // removal (or reorders it) entirely.
    let results = state
        .db
        .search_with_scoping("zzyzx", &[0u8; 32], None, None, None, 10, 0)
        .await
        .expect("search ok");
    assert!(
        !results
            .iter()
            .any(|r| r.content_id.eq_ignore_ascii_case(&create_reply.post_id)),
        "the deleted post's text must never resurface in public search"
    );
}

#[tokio::test]
async fn delete_is_user_class_at_allowlist_layer() {
    for class in [CallerClass::User, CallerClass::Admin] {
        assert!(is_permitted(class, "fauna.posts.delete"), "{class:?}");
    }
    for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
        assert!(!is_permitted(class, "fauna.posts.delete"), "{class:?}");
    }
}

// ── fauna.posts.list ───────────────────────────────────────────
//
// The self-scoped author enumeration (`content-index.md` § Ingest triggers,
// v1 → *Posts are gated…*, ruled 2026-08-05): the door the client-side index
// builder walks the user's own post corpus through. Self-scoping is
// structural — the request type has no `actor_id` field, so these tests can
// only assert that the *authenticated* actor's corpus is what comes back.

/// Like [`signed_text_post`] but at a caller-chosen timestamp, so a test can
/// force two posts to share one `created_at` microsecond — the tie a key-only
/// cursor drops wholesale.
fn signed_text_post_at(
    kp: &fauna_core::identity::ActorKeypair,
    marker: &str,
    created_at: fauna_core::data::Timestamp,
) -> Vec<u8> {
    use fauna_core::data::{Post, PostBody};
    use fauna_core::encoding::sign_and_pack;
    let post = Post {
        author: kp.actor_id(),
        created_at,
        body: PostBody::Text {
            content: marker.into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    sign_and_pack(kp, &post).unwrap()
}

fn list_payload(cursor: Option<(i64, &str)>, limit: Option<u32>) -> Bytes {
    let req = PostsListRequest {
        cursor_created_at: cursor.map(|(ts, _)| ts),
        cursor_post_id: cursor.map(|(_, id)| id.to_string()),
        limit,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

async fn create_post(
    router: &RpcRouter,
    state: &Arc<AppState>,
    kp: &fauna_core::identity::ActorKeypair,
    body: Vec<u8>,
) -> String {
    let reply: PostCreateReply = decode(
        &dispatch(
            router,
            state.clone(),
            kp.actor_id().0,
            "fauna.posts.create",
            create_payload(body),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    reply.post_id
}

#[tokio::test]
async fn list_returns_only_the_calling_actors_own_posts() {
    let (router, state) = router_with_db_only().await;
    let me = fauna_core::identity::ActorKeypair::generate();
    let other = fauna_core::identity::ActorKeypair::generate();

    let mine_a = create_post(&router, &state, &me, signed_text_post(&me, "list mine a")).await;
    let mine_b = create_post(&router, &state, &me, signed_text_post(&me, "list mine b")).await;
    let theirs = create_post(
        &router,
        &state,
        &other,
        signed_text_post(&other, "list theirs"),
    )
    .await;

    let reply: PostsListReply = decode(
        &dispatch(
            &router,
            state,
            me.actor_id().0,
            "fauna.posts.list",
            list_payload(None, None),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();

    let ids: std::collections::HashSet<&str> =
        reply.posts.iter().map(|p| p.post_id.as_str()).collect();
    assert_eq!(ids.len(), 2, "exactly my two posts: {ids:?}");
    assert!(ids.contains(mine_a.as_str()) && ids.contains(mine_b.as_str()));
    assert!(
        !ids.contains(theirs.as_str()),
        "another actor's post must never appear in my enumeration"
    );
    assert!(
        reply.posts.iter().any(|p| p.body == "list mine a"),
        "rows carry body text so the walk needs no per-post get: {:?}",
        reply.posts
    );
}

#[tokio::test]
async fn list_paginates_exactly_across_a_created_at_tie() {
    let (router, state) = router_with_db_only().await;
    let me = fauna_core::identity::ActorKeypair::generate();

    // Three posts sharing ONE microsecond. `post_id` is content-addressed, so
    // their relative order is not knowable here — which is the point: the walk
    // must visit each exactly once whatever order the tiebreak imposes.
    let tie = fauna_core::data::Timestamp::now();
    let mut expected = std::collections::HashSet::new();
    for marker in ["tie one", "tie two", "tie three"] {
        expected
            .insert(create_post(&router, &state, &me, signed_text_post_at(&me, marker, tie)).await);
    }

    // Page through two at a time, so a boundary necessarily lands mid-tie.
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<(i64, String)> = None;
    for _ in 0..10 {
        let payload = list_payload(cursor.as_ref().map(|(ts, id)| (*ts, id.as_str())), Some(2));
        let reply: PostsListReply = decode(
            &dispatch(
                &router,
                state.clone(),
                me.actor_id().0,
                "fauna.posts.list",
                payload,
            )
            .await
            .expect("list ok"),
        )
        .unwrap();
        seen.extend(reply.posts.iter().map(|p| p.post_id.clone()));
        match (reply.cursor_created_at, reply.cursor_post_id) {
            (Some(ts), Some(id)) => cursor = Some((ts, id)),
            _ => break,
        }
    }

    let unique: std::collections::HashSet<String> = seen.iter().cloned().collect();
    assert_eq!(
        seen.len(),
        unique.len(),
        "no post is served twice across a mid-tie page boundary: {seen:?}"
    );
    assert_eq!(
        unique, expected,
        "every tied post is reached exactly once — a key-only cursor would \
         drop the rest of the tie here"
    );
}

#[tokio::test]
async fn list_omits_a_deleted_post() {
    let (router, state) = router_with_db_only().await;
    let me = fauna_core::identity::ActorKeypair::generate();

    let kept = create_post(&router, &state, &me, signed_text_post(&me, "list kept")).await;
    let doomed = create_post(&router, &state, &me, signed_text_post(&me, "list doomed")).await;

    dispatch(
        &router,
        state.clone(),
        me.actor_id().0,
        "fauna.posts.delete",
        delete_payload(signed_tombstone(&me, &doomed)),
    )
    .await
    .expect("delete ok");

    let reply: PostsListReply = decode(
        &dispatch(
            &router,
            state,
            me.actor_id().0,
            "fauna.posts.list",
            list_payload(None, None),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();

    let ids: Vec<&str> = reply.posts.iter().map(|p| p.post_id.as_str()).collect();
    assert_eq!(ids, vec![kept.as_str()], "the deleted post is gone");
}

#[tokio::test]
async fn list_refuses_a_tiebreak_cursor_with_no_key_half() {
    let (router, state) = router_with_db_only().await;
    let me = fauna_core::identity::ActorKeypair::generate();

    let req = PostsListRequest {
        cursor_created_at: None,
        cursor_post_id: Some("00".repeat(32)),
        limit: None,
        extra: std::collections::BTreeMap::new(),
    };
    let err = dispatch(
        &router,
        state,
        me.actor_id().0,
        "fauna.posts.list",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect_err("a half-cursor is refused, never silently ignored");
    assert_eq!(
        err.code, "fauna.posts.invalid_params",
        "a half-cursor is a malformed request, not an empty page"
    );
}

/// The key half alone — the pre-keyset client's shape, which the nest once
/// paged by `created_at < ts` — is refused too: it left the wire with the
/// compat-remnant sweep (`version-compatibility.md` § Dimension 2).
#[tokio::test]
async fn list_refuses_a_key_cursor_with_no_tiebreak_half() {
    let (router, state) = router_with_db_only().await;
    let me = fauna_core::identity::ActorKeypair::generate();

    let req = PostsListRequest {
        cursor_created_at: Some(1_700_000_000_000_000),
        cursor_post_id: None,
        limit: None,
        extra: std::collections::BTreeMap::new(),
    };
    let err = dispatch(
        &router,
        state,
        me.actor_id().0,
        "fauna.posts.list",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect_err("a key-only cursor is refused");
    assert_eq!(err.code, "fauna.posts.invalid_params");
}

#[test]
fn list_kind_is_user_only() {
    assert!(is_permitted(CallerClass::User, "fauna.posts.list"));
    for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
        assert!(!is_permitted(class, "fauna.posts.list"), "{class:?}");
    }
}
