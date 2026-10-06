//! Integration round-trip for `fauna.search.query` — a faithful transport
//! migration of `GET /api/v1/search`. The handler is a thin wrapper over
//! `state.storage().search`, which every nest serves over the **floor-derived**
//! corpus (public post bodies, restricted-post public previews, profile
//! handles/bios) — it reads nothing sealed, so there is no posture on which it
//! could leak, and no `not_server_side` case survives the storage-mode axis's
//! retirement (`fauna_nest::storage::Storage::search` doc,
//! `docs/goal/architecture/nest/storage-modes.md`). Storage-layer search
//! behaviour (ranking, snippets) is covered by `storage_sealed_trait.rs`.
//! These tests exercise the WS-RPC layer: query validation, spec
//! construction, hit encoding (incl. the BM25 rank negation), and the
//! allowlist.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/search.rs`.
//! Slice: tracked internally.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    posts_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
    search_handlers,
};
use fauna_protocol::{
    RpcError, decode_strict as decode, encode_canonical,
    posts::{PostCreateReply, PostCreateRequest, PostGetReply, PostGetRequest},
    search::{SearchQueryReply, SearchQueryRequest},
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    search_handlers::register_search_handlers(&mut b);
    (b.build(), state)
}

/// The `content_id` pin below walks a post across three kinds, so it needs the
/// posts handlers registered beside search — the two families are separate
/// `register_*` calls in production too (`rpc_router` composition).
async fn router_with_search_and_posts() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    search_handlers::register_search_handlers(&mut b);
    posts_handlers::register_posts_handlers(&mut b);
    (b.build(), state)
}

async fn dispatch_kind(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    dispatch_kind(router, state, actor, "fauna.search.query", payload).await
}

fn query(q: &str) -> Bytes {
    let req = SearchQueryRequest {
        query: q.into(),
        content_type: None,
        before: None,
        after: None,
        limit: None,
        offset: None,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

#[tokio::test]
async fn empty_query_is_rejected() {
    let (router, state) = router_with_db_only().await;
    let err = dispatch(&router, state, [1u8; 32], query("   "))
        .await
        .expect_err("empty query rejected");
    assert_eq!(err.code, "fauna.search.invalid_params");
}

#[tokio::test]
async fn query_with_no_matches_returns_empty() {
    let (router, state) = router_with_db_only().await;
    let reply_bytes = dispatch(&router, state, [1u8; 32], query("nothingmatchesthis"))
        .await
        .expect("search ok");
    let reply: SearchQueryReply = decode(&reply_bytes).unwrap();
    assert!(reply.results.is_empty(), "no content indexed → no hits");
}

#[tokio::test]
async fn indexed_content_is_found_and_encoded() {
    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::encoding::sign_and_pack;
    use fauna_core::identity::ActorKeypair;

    let (router, state) = router_with_db_only().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;

    // Every nest serves floor-derived search unconditionally, so seeding is
    // just `put_post` — its projection writes the FTS row automatically
    // (`content::insert_and_index`, called from `CacheDb::put_post` for any
    // wire that decodes as a signed post). No storage-side ingest call
    // needed any more.
    let post = Post {
        author: kp.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Text {
            content: "hello searchable post body".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let wire_bytes = sign_and_pack(&kp, &post).unwrap();
    let post_id: [u8; 32] = *blake3::hash(&wire_bytes).as_bytes();
    state
        .db
        .put_post(&post_id, &wire_bytes, None)
        .await
        .expect("put_post");

    let reply_bytes = dispatch(&router, state.clone(), actor, query("searchable"))
        .await
        .expect("search ok");
    let reply: SearchQueryReply = decode(&reply_bytes).unwrap();
    assert!(!reply.results.is_empty(), "expected a hit for 'searchable'");
    let hit = &reply.results[0];
    // `content_type` is the `content` table's `schema` column verbatim
    // (`post_body_schema` for a `PostBody::Text`), not a coarse "post" family
    // label.
    assert_eq!(hit.content_type, "post/text");
    assert!(!hit.snippet.is_empty(), "snippet populated");
}

/// **The ratified `content_id` conformance pin** (`docs/goal/ui/search.md`
/// § The page's wire surface — "Post rows: `content_id` IS the real 64-hex post
/// id … a **ratified wire contract**, not an accident … the implementing slice
/// owes a nest conformance pin").
///
/// It is pinned the way the contract is *consumed*, not as a hash equality:
/// shared Rust mints `SearchNav::Post { post_id: r.content_id }` from a search
/// hit (`libs/fauna-client-search/src/manager.rs`), and the app feeds that
/// string straight back to `fauna.posts.get`. So the pin walks the real three-
/// kind journey — create → search → get — and never computes an id itself.
/// A re-hash on the post writer (the shape profile and bridge rows use) makes
/// the `get` leg 404, not merely change a string.
#[tokio::test]
async fn a_post_class_content_id_is_the_post_id_and_navigates() {
    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::encoding::sign_and_pack;
    use fauna_core::identity::ActorKeypair;

    let (router, state) = router_with_search_and_posts().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;

    let post = Post {
        author: kp.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Text {
            content: "navigable pinnedcontentid body".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let wire_bytes = sign_and_pack(&kp, &post).unwrap();

    // 1. The app creates a post and keeps the id the nest handed back.
    let create_reply: PostCreateReply = decode(
        &dispatch_kind(
            &router,
            state.clone(),
            actor,
            "fauna.posts.create",
            Bytes::from(
                encode_canonical(&PostCreateRequest {
                    body: serde_bytes::ByteBuf::from(wire_bytes.clone()),
                    extra: std::collections::BTreeMap::new(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();

    // 2. The same post comes back from search.
    let reply: SearchQueryReply = decode(
        &dispatch(&router, state.clone(), actor, query("pinnedcontentid"))
            .await
            .expect("search ok"),
    )
    .unwrap();
    let hit = reply
        .results
        .first()
        .expect("expected a hit for 'pinnedcontentid'");
    assert_eq!(hit.content_type, "post/text");

    // 3. The contract: the hit's `content_id` IS that post id — the same 32
    //    bytes, not a re-hash of them, and **spelled identically**. The string
    //    form is load-bearing, not cosmetic: the Search page's merge dedups
    //    post-class rows by the raw `(class, content_id)` string, so a nest that
    //    spells the same bytes differently from the rest of the system defeats
    //    the dedup silently. SQLite's `hex()` is uppercase, which is why
    //    `db/fts.rs` selects `lower(hex(...))` — reverting that reddens exactly
    //    here.
    assert_eq!(
        hit.content_id, create_reply.post_id,
        "post-class content_id must be the post id itself, same spelling \
         (search.md § The page's wire surface)"
    );
    assert!(
        hit.content_id.len() == 64
            && hit
                .content_id
                .bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()),
        "the wire spells 32-byte ids as 64 lowercase hex chars, like \
         fauna_core::hex32::encode: {}",
        hit.content_id
    );

    // 4. And it is navigable *verbatim*: the exact string shared Rust puts in
    //    `SearchNav::Post` resolves through `fauna.posts.get`.
    let get_reply: PostGetReply = decode(
        &dispatch_kind(
            &router,
            state.clone(),
            actor,
            "fauna.posts.get",
            Bytes::from(
                encode_canonical(&PostGetRequest {
                    post_id: hit.content_id.clone(),
                    extra: std::collections::BTreeMap::new(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("the content_id from a search hit must resolve through posts.get"),
    )
    .unwrap();
    assert_eq!(
        get_reply.body.as_ref(),
        wire_bytes.as_slice(),
        "navigating by the search hit's content_id returns that same post"
    );
}

#[tokio::test]
async fn search_kind_is_user_only_at_allowlist_layer() {
    let kind = "fauna.search.query";
    assert!(
        is_permitted(CallerClass::User, kind),
        "{kind} should be permitted for User"
    );
    // Admin ⊇ User: an admin is a user with an extra role, so it inherits every
    // User permission (api-layers.md § Caller-class authorization;
    // `is_permitted` short-circuits Admin→User). A User-min kind is therefore
    // permitted for Admin.
    assert!(
        is_permitted(CallerClass::Admin, kind),
        "{kind} should be permitted for Admin (Admin ⊇ User)"
    );
    // Bridge classes are not users — denied.
    for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
        assert!(
            !is_permitted(class, kind),
            "{kind} should be denied for {class:?}"
        );
    }
}
