//! Integration round-trip for the authenticated web-content-publishing surface
//! — `fauna.web.{publish.{set,unset,list},domain.{set,get}}`. A
//! behavior-preserving transport migration of the 5 bearer-authed
//! web-content-hosting HTTP routes (`web_content::{publish_routes, domain}`).
//! The handlers reuse the same core fns the HTTP twins call — these tests
//! exercise the WS-RPC layer: request decode (raw-bytes `post_id` + the actor
//! scope keyed on the connection actor), reply encoding, the default-slug
//! behavior, the domain limit/duplicate error mapping, replay metadata, and the
//! `User | Admin` allowlist.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/web.rs`.
//! Slice: tracked internally (Track B18 of the WS-RPC-everywhere
//! migration).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    moderation_handlers,
    pending_actions::{ActionType, execute_ready_actions},
    posts_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
    web_content::serve::HostResolver,
    web_handlers,
};
use fauna_protocol::{
    ByteBuf, decode_strict as decode, encode_canonical,
    moderation::{ModerationLegalTakedownReply, ModerationLegalTakedownRequest},
    posts::PostCreateRequest,
    web::{
        WebDomainDeleteReply, WebDomainDeleteRequest, WebDomainGetReply, WebDomainGetRequest,
        WebDomainSetReply, WebDomainSetRequest, WebFilesPruneSealedReply,
        WebFilesPruneSealedRequest, WebPublishListReply, WebPublishListRequest, WebPublishSetReply,
        WebPublishSetRequest, WebPublishUnsetReply, WebPublishUnsetRequest,
    },
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    web_handlers::register_web_handlers(&mut b);
    (b.build(), state)
}

/// [`router_and_state`] with a live `HostResolver` installed, as `start_server`
/// always builds one — `AppState::for_test` leaves it `None` (the dormant
/// pre-activation state), which would make every routing assertion vacuous.
async fn router_and_state_with_resolver() -> (RpcRouter, Arc<AppState>, Arc<HostResolver>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let resolver = Arc::new(HostResolver::new("example.com".to_string()));
    let mut st = AppState::for_test(db);
    st.host_resolver = Some(resolver.clone());
    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    web_handlers::register_web_handlers(&mut b);
    (b.build(), Arc::new(st), resolver)
}

/// [`router_and_state_with_resolver`] plus a live `WebContentService` (a real
/// disk blob store in a tempdir) — what a post delete's re-render needs to
/// run, and what the real HTTP serve door (`build_router`'s `Host`-header
/// fallback) needs to answer anything but the built-in info page. The
/// `TempDir` must be kept alive by the caller for the blob store to stay
/// valid.
async fn router_and_state_with_web_site() -> (
    RpcRouter,
    Arc<AppState>,
    Arc<HostResolver>,
    tempfile::TempDir,
) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let resolver = Arc::new(HostResolver::new("example.com".to_string()));
    let blobs = tempfile::tempdir().unwrap();
    let store = Arc::new(fauna_nest::blob_store::DiskBlobStore::new(blobs.path()).unwrap());
    let mut st = AppState::for_test(db.clone());
    st.host_resolver = Some(resolver.clone());
    // `fauna.posts.create` always writes bodies through `state.post_segments`
    // (never the legacy `content.payload` column — see `routes.rs`'s
    // `store_post`), so the render must read through the SAME segment store
    // or every post it enumerates looks bodyless and is silently skipped
    // (`render_published_posts`'s `let Some(body) = body else { continue }`).
    let post_segments = st.post_segments.clone();
    st.web_content_service = Some(Arc::new(
        fauna_nest::web_content::service::WebContentService::new(db, store)
            .with_post_body_source(post_segments),
    ));
    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    web_handlers::register_web_handlers(&mut b);
    (b.build(), Arc::new(st), resolver, blobs)
}

/// GET `path` over the real `build_router` app with `Host: <host>` — the exact
/// seam `web_content_or_info` resolves (§ `domain_delete_stops_routing_the_domain_immediately`
/// above), so a re-render that ran but left a stale page in place still fails
/// this, unlike a bare `web_rendered` row check.
async fn page_status(state: &Arc<AppState>, host: &str, path: &str) -> axum::http::StatusCode {
    use tower::ServiceExt;
    let app = fauna_nest::build_router(state.clone());
    let req = axum::http::Request::builder()
        .uri(path)
        .header("host", host)
        .body(axum::body::Body::empty())
        .unwrap();
    app.oneshot(req).await.unwrap().status()
}

/// Build a real signed text post (embed-as-bytes wire) and ingest it via
/// `fauna.posts.create`, returning its content-addressed id — the shape the
/// authorship gate needs to resolve a real author (mirrors
/// `conformance_posts.rs::signed_text_post`). The `marker` keeps each post
/// content-addressed-unique across tests.
async fn create_real_post(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: &fauna_core::identity::ActorKeypair,
    marker: &str,
) -> [u8; 32] {
    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::encoding::sign_and_pack;
    let post = Post {
        author: author.actor_id(),
        created_at: Timestamp::now(),
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
    let body = sign_and_pack(author, &post).unwrap();
    let expected = *blake3::hash(&body).as_bytes();
    let req = PostCreateRequest {
        body: serde_bytes::ByteBuf::from(body),
        extra: Default::default(),
    };
    dispatch(
        router,
        state,
        author.actor_id().0,
        "fauna.posts.create",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect("posts.create ok");
    expected
}

const ACTOR: [u8; 32] = [11u8; 32];

// ── publish.set / list / unset ──────────────────────────────────

#[tokio::test]
async fn publish_set_returns_slug_then_list_shows_it() {
    let (router, state) = router_and_state().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let post = create_real_post(
        &router,
        state.clone(),
        &author,
        "publish_set_returns_slug_then_list_shows_it",
    )
    .await;

    let reply: WebPublishSetReply = decode(
        &dispatch(
            &router,
            state.clone(),
            author.actor_id().0,
            "fauna.web.publish.set",
            encode(&WebPublishSetRequest {
                post_id: ByteBuf::from(post.to_vec()),
                slug: Some("hello-world".into()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("publish.set ok"),
    )
    .unwrap();
    assert_eq!(reply.slug, "hello-world");

    let list: WebPublishListReply = decode(
        &dispatch(
            &router,
            state,
            author.actor_id().0,
            "fauna.web.publish.list",
            encode(&WebPublishListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("publish.list ok"),
    )
    .unwrap();
    assert_eq!(list.posts.len(), 1);
    assert_eq!(list.posts[0].post_id.as_ref(), &post);
    assert_eq!(list.posts[0].slug, "hello-world");
}

#[tokio::test]
async fn publish_set_defaults_slug_to_post_id_hex() {
    let (router, state) = router_and_state().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let post = create_real_post(
        &router,
        state.clone(),
        &author,
        "publish_set_defaults_slug_to_post_id_hex",
    )
    .await;

    let reply: WebPublishSetReply = decode(
        &dispatch(
            &router,
            state,
            author.actor_id().0,
            "fauna.web.publish.set",
            encode(&WebPublishSetRequest {
                post_id: ByteBuf::from(post.to_vec()),
                slug: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("publish.set ok"),
    )
    .unwrap();
    assert_eq!(reply.slug, hex::encode(post));
}

#[tokio::test]
async fn publish_unset_removes_and_is_idempotent() {
    let (router, state) = router_and_state().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let post = create_real_post(
        &router,
        state.clone(),
        &author,
        "publish_unset_removes_and_is_idempotent",
    )
    .await;
    let actor = author.actor_id().0;

    // Publish, then unpublish.
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.web.publish.set",
        encode(&WebPublishSetRequest {
            post_id: ByteBuf::from(post.to_vec()),
            slug: Some("s".into()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("set ok");

    let r: WebPublishUnsetReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.web.publish.unset",
            encode(&WebPublishUnsetRequest {
                post_id: ByteBuf::from(post.to_vec()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("unset ok"),
    )
    .unwrap();
    assert!(r.ok);

    // Idempotent: unsetting again still succeeds.
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.web.publish.unset",
        encode(&WebPublishUnsetRequest {
            post_id: ByteBuf::from(post.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("second unset ok");

    let list: WebPublishListReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.web.publish.list",
            encode(&WebPublishListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert!(list.posts.is_empty());
}

#[tokio::test]
async fn publish_set_rejects_wrong_length_post_id() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        ACTOR,
        "fauna.web.publish.set",
        encode(&WebPublishSetRequest {
            post_id: ByteBuf::from(vec![0x01; 16]),
            slug: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("short post_id rejected");
    assert_eq!(err.code, "fauna.web.invalid_request");
}

#[tokio::test]
async fn publish_set_refuses_a_post_the_caller_did_not_author() {
    let (router, state) = router_and_state().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let stranger = fauna_core::identity::ActorKeypair::generate();
    let post = create_real_post(
        &router,
        state.clone(),
        &author,
        "publish_set_refuses_a_post_the_caller_did_not_author",
    )
    .await;

    let err = dispatch(
        &router,
        state.clone(),
        stranger.actor_id().0,
        "fauna.web.publish.set",
        encode(&WebPublishSetRequest {
            post_id: ByteBuf::from(post.to_vec()),
            slug: Some("stolen-slug".into()),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("a stranger republishing someone else's post is refused");
    assert_eq!(err.code, "fauna.web.permission_denied");

    // The author themself still succeeds — the gate is authorship-scoped, not
    // a lockout.
    dispatch(
        &router,
        state,
        author.actor_id().0,
        "fauna.web.publish.set",
        encode(&WebPublishSetRequest {
            post_id: ByteBuf::from(post.to_vec()),
            slug: Some("my-own-slug".into()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("the author may publish their own post");
}

#[tokio::test]
async fn publish_set_refuses_an_unknown_post_id() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        ACTOR,
        "fauna.web.publish.set",
        encode(&WebPublishSetRequest {
            post_id: ByteBuf::from([0xEEu8; 32].to_vec()),
            slug: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("a post nobody ever created cannot be published");
    assert_eq!(err.code, "fauna.web.not_found");
}

// ── files.prune_sealed ──────────────────────────────────────────

/// An empty `paths` is refused at the handler, not honoured as
/// "this folder's sealed corpus is now empty". Red-verify: delete the
/// `req.paths.is_empty()` guard in `files_prune_sealed_handler` and this fails
/// with `dropped: 0` (an empty `keep` set against a folder with no sealed rows
/// yet drops nothing) rather than an error — the DB-layer pin
/// (`db/web.rs::an_empty_declaration_clears_the_folders_sealed_rows_at_the_db_layer`)
/// is what would go on to prove the deletion once a sealed row exists to lose.
#[tokio::test]
async fn prune_sealed_refuses_empty_paths() {
    let (router, state) = router_and_state().await;
    state.db.create_folder("my_site", &ACTOR).await.unwrap();

    let err = dispatch(
        &router,
        state,
        ACTOR,
        "fauna.web.files.prune_sealed",
        encode(&WebFilesPruneSealedRequest {
            folder: "my_site".into(),
            paths: vec![],
            ..Default::default()
        }),
    )
    .await
    .expect_err("an empty declaration is indistinguishable from a seat that has not caught up");
    assert_eq!(err.code, "fauna.web.invalid_request");
}

/// The prune resolves the owner's set by its hash address alone (S5b) — the
/// address that survives the plaintext name blanking — and a hash naming no
/// set of the caller's is the same refusal as an unknown name.
#[tokio::test]
async fn prune_sealed_resolves_the_set_by_hash() {
    let (router, state) = router_and_state().await;
    state.db.create_folder("my_site", &ACTOR).await.unwrap();
    let by_hash = |name: &str| WebFilesPruneSealedRequest {
        paths: vec!["index.md".into()],
        name_hash: Some(ByteBuf::from(
            fauna_core::path_crypto::set_name_hash(name).to_vec(),
        )),
        ..Default::default()
    };

    let reply: WebFilesPruneSealedReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            ACTOR,
            "fauna.web.files.prune_sealed",
            encode(&by_hash("my_site")),
        )
        .await
        .expect("a hash-addressed prune resolves the set"),
    )
    .unwrap();
    assert_eq!(reply.dropped, 0);

    let err = dispatch(
        &router,
        state,
        ACTOR,
        "fauna.web.files.prune_sealed",
        encode(&by_hash("not_mine")),
    )
    .await
    .expect_err("an unknown hash is no such folder");
    assert_eq!(err.code, "fauna.web.invalid_request");
}

// ── domain.set / get ────────────────────────────────────────────

#[tokio::test]
async fn domain_set_returns_token_then_get_lists_it() {
    let (router, state) = router_and_state().await;
    let reply: WebDomainSetReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR,
            "fauna.web.domain.set",
            encode(&WebDomainSetRequest {
                domain: "example.com".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("domain.set ok"),
    )
    .unwrap();
    assert_eq!(reply.domain, "example.com");
    assert_eq!(reply.txt_record, "_fauna-verify.example.com");
    assert_eq!(reply.status, "pending");
    assert!(reply.verify_token.starts_with("fauna-verify-"));

    let get: WebDomainGetReply = decode(
        &dispatch(
            &router,
            state,
            ACTOR,
            "fauna.web.domain.get",
            encode(&WebDomainGetRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("domain.get ok"),
    )
    .unwrap();
    assert_eq!(get.domains.len(), 1);
    assert_eq!(get.domains[0].domain, "example.com");
    assert_eq!(get.domains[0].txt_record, "_fauna-verify.example.com");
    assert_eq!(get.domains[0].status, "pending");
    assert!(get.domains[0].verified_at.is_none());
}

#[tokio::test]
async fn domain_set_empty_is_invalid_request() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        ACTOR,
        "fauna.web.domain.set",
        encode(&WebDomainSetRequest {
            domain: "".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("empty domain rejected");
    assert_eq!(err.code, "fauna.web.invalid_request");
}

#[tokio::test]
async fn domain_set_duplicate_is_conflict() {
    let (router, state) = router_and_state().await;
    let req = || {
        encode(&WebDomainSetRequest {
            domain: "dup.example.com".into(),
            extra: Default::default(),
        })
    };
    dispatch(&router, state.clone(), ACTOR, "fauna.web.domain.set", req())
        .await
        .expect("first registration ok");

    let err = dispatch(&router, state, ACTOR, "fauna.web.domain.set", req())
        .await
        .expect_err("duplicate rejected");
    assert_eq!(err.code, "fauna.web.conflict");
}

// ── domain.delete → live routing ────────────────────────────────

/// Deregistering a custom domain stops the nest serving it **immediately**, not
/// on the next 5-minute reconcile and not on the next restart.
///
/// `HostResolver::resolve` is the exact seam `web_content_or_info` calls to turn
/// a `Host` header into an actor, so a `Some` here after the delete means the
/// nest still serves a site its owner just withdrew. The per-domain cert stays
/// installed for that same window, so HTTPS does not mask the exposure.
#[tokio::test]
async fn domain_delete_stops_routing_the_domain_immediately() {
    let (router, state, resolver) = router_and_state_with_resolver().await;

    dispatch(
        &router,
        state.clone(),
        ACTOR,
        "fauna.web.domain.set",
        encode(&WebDomainSetRequest {
            domain: "mine.example.com".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("domain.set ok");

    // Bring it live exactly as the lifecycle task's verification pass does,
    // then route it as that pass's reconcile does.
    state
        .db
        .update_web_domain_status("mine.example.com", "active")
        .await
        .unwrap();
    fauna_nest::web_content::domain::reconcile_custom_domain_routing_once(&state.db, &resolver)
        .await
        .unwrap();
    assert_eq!(
        resolver.resolve("mine.example.com").await,
        Some(ACTOR),
        "an active domain routes to its owner"
    );

    let reply: WebDomainDeleteReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR,
            "fauna.web.domain.delete",
            encode(&WebDomainDeleteRequest {
                domain: "mine.example.com".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("domain.delete ok"),
    )
    .unwrap();
    assert!(reply.ok, "the row was removed");

    assert_eq!(
        resolver.resolve("mine.example.com").await,
        None,
        "a deregistered domain must stop being served at once"
    );
}

// ── post delete re-renders the author's web site ────────────────

/// Deleting a still-published post must re-render the author's web site so
/// the stale static page stops serving at once — nothing re-renders on a
/// schedule (`web-content-hosting.md` § Routing, render, serving; `feed.md` §
/// Post deletion → Propagation). Goes
/// through the REAL HTTP serve door, the same seam
/// `domain_delete_stops_routing_the_domain_immediately` exercises for
/// domains, so a re-render that ran but left the OLD static page in place
/// would still fail this.
#[tokio::test]
async fn post_delete_re_renders_and_the_stale_page_stops_serving() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();

    // Route a custom domain to the author, exactly as
    // `domain_delete_stops_routing_the_domain_immediately` does above, so the
    // real serve door has a `Host` to resolve.
    dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.web.domain.set",
        encode(&WebDomainSetRequest {
            domain: "mine.example.com".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("domain.set ok");
    state
        .db
        .update_web_domain_status("mine.example.com", "active")
        .await
        .unwrap();
    fauna_nest::web_content::domain::reconcile_custom_domain_routing_once(&state.db, &resolver)
        .await
        .unwrap();

    let post = create_real_post(
        &router,
        state.clone(),
        &author,
        "post_delete_re_renders_and_the_stale_page_stops_serving",
    )
    .await;
    dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.web.publish.set",
        encode(&WebPublishSetRequest {
            post_id: ByteBuf::from(post.to_vec()),
            slug: Some("the-post".into()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("publish.set ok");

    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-post.html").await,
        axum::http::StatusCode::OK,
        "precondition: the published post rendered a page"
    );

    let tombstone = fauna_core::data::Tombstone {
        author: author.actor_id(),
        post_id: fauna_core::data::PostId::from_digest_dag_cbor(post),
        created_at: fauna_core::data::Timestamp::now(),
    };
    let tombstone_wire = fauna_core::encoding::sign_and_pack(&author, &tombstone).unwrap();
    let reply: fauna_protocol::posts::PostDeleteReply = decode(
        &dispatch(
            &router,
            state.clone(),
            author.actor_id().0,
            "fauna.posts.delete",
            encode(&fauna_protocol::posts::PostDeleteRequest {
                body: ByteBuf::from(tombstone_wire),
                extra: Default::default(),
            }),
        )
        .await
        .expect("delete ok"),
    )
    .unwrap();
    assert!(
        reply.deleted,
        "the post was live, so this delete newly removes it"
    );

    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-post.html").await,
        axum::http::StatusCode::NOT_FOUND,
        "a delete must re-render the site so the stale page stops serving, not wait for an \
         unrelated publish to trigger the next render"
    );
}

// ── a revoking render is owed durably, drained at boot, and fails closed ──

/// GET `path` through the real serve door and say whether the answer is a
/// served page carrying `needle` — a 404 carries nothing.
async fn page_carries(state: &Arc<AppState>, host: &str, path: &str, needle: &str) -> bool {
    use tower::ServiceExt;
    let app = fauna_nest::build_router(state.clone());
    let req = axum::http::Request::builder()
        .uri(path)
        .header("host", host)
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    if resp.status() != axum::http::StatusCode::OK {
        return false;
    }
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&body).contains(needle)
}

/// A routed site (`mine.example.com`) whose author has published one post at
/// slug `the-post` — plus, with `sibling`, a second one at `the-sibling` that
/// stays published. Returns the first post's id.
async fn published_site(
    router: &RpcRouter,
    state: &Arc<AppState>,
    resolver: &Arc<HostResolver>,
    author: &fauna_core::identity::ActorKeypair,
    marker: &str,
    sibling: bool,
) -> [u8; 32] {
    let slugs: &[&str] = if sibling {
        &["the-post", "the-sibling"]
    } else {
        &["the-post"]
    };
    published_site_slugs(router, state, resolver, author, marker, slugs).await[0]
}

/// [`published_site`]'s general form: one published post per slug, ids in the
/// same order, so a caller that needs the *sibling's* id has it.
async fn published_site_slugs(
    router: &RpcRouter,
    state: &Arc<AppState>,
    resolver: &Arc<HostResolver>,
    author: &fauna_core::identity::ActorKeypair,
    marker: &str,
    slugs: &[&str],
) -> Vec<[u8; 32]> {
    dispatch(
        router,
        state.clone(),
        author.actor_id().0,
        "fauna.web.domain.set",
        encode(&WebDomainSetRequest {
            domain: "mine.example.com".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("domain.set ok");
    state
        .db
        .update_web_domain_status("mine.example.com", "active")
        .await
        .unwrap();
    fauna_nest::web_content::domain::reconcile_custom_domain_routing_once(&state.db, resolver)
        .await
        .unwrap();

    let mut ids = Vec::with_capacity(slugs.len());
    for slug in slugs {
        let post =
            create_real_post(router, state.clone(), author, &format!("{marker} {slug}")).await;
        dispatch(
            router,
            state.clone(),
            author.actor_id().0,
            "fauna.web.publish.set",
            encode(&WebPublishSetRequest {
                post_id: ByteBuf::from(post.to_vec()),
                slug: Some((*slug).into()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("publish.set ok");
        ids.push(post);
    }
    for slug in slugs {
        assert_eq!(
            page_status(state, "mine.example.com", &format!("/post/{slug}.html")).await,
            axum::http::StatusCode::OK,
            "precondition: the published post {slug} rendered a page"
        );
        assert!(
            page_carries(state, "mine.example.com", "/feed.xml", slug).await,
            "precondition: the feed carries {slug}"
        );
    }
    ids
}

/// `fauna.posts.delete` for `post`, signed by `author`.
async fn delete_post(
    router: &RpcRouter,
    state: &Arc<AppState>,
    author: &fauna_core::identity::ActorKeypair,
    post: [u8; 32],
) -> fauna_protocol::posts::PostDeleteReply {
    let tombstone = fauna_core::data::Tombstone {
        author: author.actor_id(),
        post_id: fauna_core::data::PostId::from_digest_dag_cbor(post),
        created_at: fauna_core::data::Timestamp::now(),
    };
    let tombstone_wire = fauna_core::encoding::sign_and_pack(author, &tombstone).unwrap();
    decode(
        &dispatch(
            router,
            state.clone(),
            author.actor_id().0,
            "fauna.posts.delete",
            encode(&fauna_protocol::posts::PostDeleteRequest {
                body: ByteBuf::from(tombstone_wire),
                extra: Default::default(),
            }),
        )
        .await
        .expect("delete ok"),
    )
    .unwrap()
}

/// Every surface of the site that named the deleted post: its own page, the
/// index and the feed, all through the real serve door.
async fn assert_the_post_is_revoked(state: &Arc<AppState>, why: &str) {
    assert_eq!(
        page_status(state, "mine.example.com", "/post/the-post.html").await,
        axum::http::StatusCode::NOT_FOUND,
        "the deleted post's page must stop serving: {why}"
    );
    assert!(
        !page_carries(state, "mine.example.com", "/feed.xml", "the-post").await,
        "feed.xml must no longer carry the deleted post: {why}"
    );
    assert!(
        !page_carries(state, "mine.example.com", "/", "the-post").await,
        "the index must no longer carry the deleted post: {why}"
    );
}

/// The nest restarts between a post delete's projection commit and its render
/// (simulated by committing the delete's own transaction alone — the exact
/// statement `delete_post_core` step 1 runs). The revoke the delete owes must
/// survive that restart: the owed-render marker rides the delete's
/// transaction, and the boot drain renders every site still owed
/// (`web-content-hosting.md` § Routing, render, serving → *A revoke is
/// durable*).
///
/// This is the LAST-published-post case on purpose: the actor leaves
/// `list_web_publishing_actors` with the delete's link cascade, so a drain
/// keyed on that list would never reach the stale site.
#[tokio::test]
async fn a_post_delete_torn_before_its_render_is_revoked_by_the_boot_drain() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let post = published_site(&router, &state, &resolver, &author, "torn-boot", false).await;

    assert!(state.db.delete_post_projection(&post).await.unwrap());
    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-post.html").await,
        axum::http::StatusCode::OK,
        "the torn state: the projection is gone and the rendered page still serves"
    );

    let wcs = state.web_content_service.as_ref().unwrap();
    assert_eq!(
        wcs.drain_owed_renders().await.unwrap().failed,
        0,
        "no site failed to render"
    );
    assert_the_post_is_revoked(&state, "the boot drain renders every owed site").await;
    assert!(
        state.db.list_web_render_owed().await.unwrap().is_empty(),
        "a successful render discharges the marker"
    );
}

/// The same restart with a second post still published: the drain re-renders
/// the site rather than clearing it, so the sibling keeps serving.
#[tokio::test]
async fn the_boot_drain_keeps_the_rest_of_the_site_serving() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let post = published_site(&router, &state, &resolver, &author, "torn-sibling", true).await;

    assert!(state.db.delete_post_projection(&post).await.unwrap());
    let wcs = state.web_content_service.as_ref().unwrap();
    assert_eq!(wcs.drain_owed_renders().await.unwrap().failed, 0);

    assert_the_post_is_revoked(&state, "the boot drain renders every owed site").await;
    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::OK,
        "the still-published sibling keeps its page"
    );
    assert!(page_carries(&state, "mine.example.com", "/feed.xml", "the-sibling").await);
}

/// The same restart, recovered by the client instead of the boot drain: the
/// retried `fauna.posts.delete` finds the first attempt's link cascade already
/// done, and must revoke the page anyway — the door renders whatever its actor
/// is still owed, not only what this call removed.
#[tokio::test]
async fn a_post_delete_torn_before_its_render_is_revoked_by_the_clients_retry() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let post = published_site(&router, &state, &resolver, &author, "torn-retry", false).await;

    assert!(state.db.delete_post_projection(&post).await.unwrap());
    delete_post(&router, &state, &author, post).await;

    assert_the_post_is_revoked(&state, "the retry renders what the actor is still owed").await;
}

/// Make every render fail BEFORE `render_for_actor`'s clear, with a real
/// storage error and no test hook: the render reads the declared region
/// (`situs_policies`) ahead of the clear, and no revoking door touches that
/// table.
async fn break_the_render(state: &Arc<AppState>) {
    state
        .db
        .conn()
        .await
        .execute_batch("ALTER TABLE nest_region RENAME TO nest_region_broken")
        .unwrap();
    let wcs = state.web_content_service.as_ref().unwrap();
    assert!(
        wcs.render_published_posts(&[0u8; 32]).await.is_err(),
        "precondition: the render now errors before its clear"
    );
}

/// A post delete whose render errors before the clear must take the site dark
/// rather than keep serving the post the author just deleted: a revoking door
/// fails closed, whoever's act the revoke is.
#[tokio::test]
async fn a_post_delete_fails_closed_when_its_render_errors() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let post = published_site(&router, &state, &resolver, &author, "closed-delete", true).await;

    break_the_render(&state).await;
    let reply = delete_post(&router, &state, &author, post).await;
    assert!(reply.deleted, "a render failure never fails the delete");

    assert_the_post_is_revoked(&state, "the failed render cleared the site").await;
    assert!(
        state.db.list_web_render_owed().await.unwrap().is_empty(),
        "a fail-closed clear is a completed revoke and discharges the marker, so a \
         persistently failing render cannot hold the boot drain forever"
    );
}

/// `publish.unset` is a revoke too, and fails closed the same way.
#[tokio::test]
async fn publish_unset_fails_closed_when_its_render_errors() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let post = published_site(&router, &state, &resolver, &author, "closed-unset", true).await;

    break_the_render(&state).await;
    dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.web.publish.unset",
        encode(&WebPublishUnsetRequest {
            post_id: ByteBuf::from(post.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("publish.unset ok");

    assert_the_post_is_revoked(&state, "the failed render cleared the site").await;
    assert!(state.db.list_web_render_owed().await.unwrap().is_empty());
}

// ── a blanked site is owed its restore ──

/// Undo [`break_the_render`]: the storage fault was transient.
async fn heal_the_render(state: &Arc<AppState>) {
    state
        .db
        .conn()
        .await
        .execute_batch("ALTER TABLE nest_region_broken RENAME TO nest_region")
        .unwrap();
}

/// A two-post site whose author deleted one post while every render was
/// failing: the door failed closed, so the whole site — the still-published
/// sibling included — is dark. Returns the author's actor id.
async fn a_site_blanked_by_a_failed_revoke(
    router: &RpcRouter,
    state: &Arc<AppState>,
    resolver: &Arc<HostResolver>,
    marker: &str,
) -> [u8; 32] {
    let author = fauna_core::identity::ActorKeypair::generate();
    let post = published_site(router, state, resolver, &author, marker, true).await;
    break_the_render(state).await;
    delete_post(router, state, &author, post).await;
    assert_eq!(
        page_status(state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::NOT_FOUND,
        "precondition: the fail-closed clear took the whole site dark"
    );
    author.actor_id().0
}

/// The clear that blanks a site records, in its own transaction, that the
/// site is owed its restore — and once the fault has passed, the boot drain
/// brings the site back through the real serve door, without the post the
/// author deleted (`web-content-hosting.md` § Routing, render, serving → *A
/// blanked site is owed its restore*).
#[tokio::test]
async fn a_site_a_failed_revoke_blanked_is_restored_by_the_boot_drain() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let actor = a_site_blanked_by_a_failed_revoke(&router, &state, &resolver, "restore").await;
    assert_eq!(
        state.db.list_web_restore_owed().await.unwrap(),
        vec![actor],
        "the fail-closed clear leaves the restore owed"
    );

    heal_the_render(&state).await;
    let wcs = state.web_content_service.as_ref().unwrap();
    assert_eq!(wcs.drain_owed_renders().await.unwrap().failed, 0);

    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::OK,
        "the boot drain restores the blanked site"
    );
    assert_the_post_is_revoked(&state, "a restore renders only what is still published").await;
    assert!(
        state.db.list_web_restore_owed().await.unwrap().is_empty(),
        "the render that restores the site discharges the restore"
    );
}

/// A restore whose render STILL fails leaves the site dark and the restore
/// owed — and costs exactly one render attempt per drain, never a loop.
#[tokio::test]
async fn a_still_failing_restore_stays_dark_and_owed_after_one_attempt() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let actor = a_site_blanked_by_a_failed_revoke(&router, &state, &resolver, "dark").await;

    let wcs = state.web_content_service.as_ref().unwrap();
    let before = wcs.render_call_count();
    assert_eq!(
        wcs.drain_owed_renders().await.unwrap().failed,
        1,
        "the one owed site failed to render"
    );
    assert_eq!(
        wcs.render_call_count(),
        before + 1,
        "one attempt per owed site per drain"
    );
    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::NOT_FOUND,
    );
    assert_eq!(state.db.list_web_restore_owed().await.unwrap(), vec![actor]);
}

/// Any successful render restores the site, not only the drain: the author's
/// next publish discharges the restore like the drain would have.
#[tokio::test]
async fn any_successful_render_discharges_the_owed_restore() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let actor = a_site_blanked_by_a_failed_revoke(&router, &state, &resolver, "door").await;
    heal_the_render(&state).await;

    let wcs = state.web_content_service.as_ref().unwrap();
    wcs.render_published_posts(&actor).await.unwrap();

    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::OK,
    );
    assert!(state.db.list_web_restore_owed().await.unwrap().is_empty());
}

/// `fauna.web.publish.list`'s `rendered_pages_down`, as `actor` reads it.
async fn rendered_pages_down(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32]) -> bool {
    let list: WebPublishListReply = decode(
        &dispatch(
            router,
            state.clone(),
            actor,
            "fauna.web.publish.list",
            encode(&WebPublishListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("publish.list ok"),
    )
    .unwrap();
    list.rendered_pages_down
}

/// A blanked site tells its author: the read the `web-settings` page already
/// makes turns true with the fail-closed clear and false with the render that
/// restores the site — and says nothing about anyone else's site
/// (`web-content-hosting.md` § Routing, render, serving → *A blanked site tells
/// its author*).
#[tokio::test]
async fn publish_list_tells_the_author_their_rendered_pages_are_down() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let bystander = fauna_core::identity::ActorKeypair::generate().actor_id().0;
    assert!(!rendered_pages_down(&router, &state, bystander).await);

    let actor = a_site_blanked_by_a_failed_revoke(&router, &state, &resolver, "told").await;
    assert!(
        rendered_pages_down(&router, &state, actor).await,
        "the fail-closed clear is what the author is told about"
    );
    assert!(
        !rendered_pages_down(&router, &state, bystander).await,
        "the state is the caller's own, never the nest's"
    );

    heal_the_render(&state).await;
    let wcs = state.web_content_service.as_ref().unwrap();
    assert_eq!(wcs.drain_owed_renders().await.unwrap().failed, 0);
    assert!(
        !rendered_pages_down(&router, &state, actor).await,
        "the render that restores the site clears the status"
    );
}

/// Between boots the restore is retried by the task the boot drain leaves
/// running: a site blanked while the nest is up gets a fresh render attempt
/// with no restart and no door. (The attempt here still fails — the fault has
/// not passed — and what it would do on success is the drain's own pass,
/// pinned above; the pacing between attempts is `next_attempt_delay`'s.)
#[tokio::test]
async fn a_blanked_site_is_retried_without_a_restart() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let wcs = state.web_content_service.as_ref().unwrap().clone();
    assert_eq!(wcs.drain_owed_renders().await.unwrap().failed, 0);

    let author = fauna_core::identity::ActorKeypair::generate();
    let post = published_site(&router, &state, &resolver, &author, "retry", true).await;
    break_the_render(&state).await;
    // Counted from BEFORE the door: the retry may run while the door's own
    // reply is still in flight. The door renders once; the retry is the second.
    let before_the_door = wcs.render_call_count();
    delete_post(&router, &state, &author, post).await;

    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while wcs.render_call_count() < before_the_door + 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the restore retry attempts the blanked site without a restart");

    // And it does not spin: the failed attempt is on the backoff's log, so the
    // retry is now asleep. On this single-threaded runtime every yield hands
    // it the thread — a retry that looped would render on each one.
    for _ in 0..1000 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        wcs.render_call_count(),
        before_the_door + 2,
        "a failed restore attempt is paced, never retried in a loop"
    );
}

// ── account-deletion retraction pass renders once, not per post ──

/// An account deletion's post-retraction pass
/// (`pending_actions::retract_actor_posts`) must not re-render the author's
/// site once per still-published post — `delete_post_core`'s inline render is skipped for every post in
/// the pass, and the pass renders the site itself at most ONCE afterward.
/// Goes through the REAL pending-action path (`account.delete` + the
/// executor), not a direct call, so this also proves the purge sweep that
/// follows leaves no stale page or `web_rendered` row.
#[tokio::test]
async fn account_deletion_renders_the_site_once_not_per_post() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();

    dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.web.domain.set",
        encode(&WebDomainSetRequest {
            domain: "purge.example.com".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("domain.set ok");
    state
        .db
        .update_web_domain_status("purge.example.com", "active")
        .await
        .unwrap();
    fauna_nest::web_content::domain::reconcile_custom_domain_routing_once(&state.db, &resolver)
        .await
        .unwrap();

    let post_a = create_real_post(
        &router,
        state.clone(),
        &author,
        "account_deletion_renders_the_site_once_not_per_post-a",
    )
    .await;
    let post_b = create_real_post(
        &router,
        state.clone(),
        &author,
        "account_deletion_renders_the_site_once_not_per_post-b",
    )
    .await;
    for (post, slug) in [(post_a, "a"), (post_b, "b")] {
        dispatch(
            &router,
            state.clone(),
            author.actor_id().0,
            "fauna.web.publish.set",
            encode(&WebPublishSetRequest {
                post_id: ByteBuf::from(post.to_vec()),
                slug: Some(slug.into()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("publish.set ok");
    }

    assert_eq!(
        page_status(&state, "purge.example.com", "/post/a.html").await,
        axum::http::StatusCode::OK,
        "precondition: post a rendered"
    );
    assert_eq!(
        page_status(&state, "purge.example.com", "/post/b.html").await,
        axum::http::StatusCode::OK,
        "precondition: post b rendered"
    );

    let wcs = state.web_content_service.as_ref().unwrap().clone();
    let renders_before = wcs.render_call_count();

    let id = state
        .db
        .create_pending_action(
            &ActionType::AccountDelete,
            &author.actor_id().0,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    state.db.test_set_execute_after(id, 1).await.unwrap();

    let executed = execute_ready_actions(&state).await.unwrap();
    assert_eq!(executed, 1, "the account deletion should have executed");

    assert_eq!(
        wcs.render_call_count() - renders_before,
        1,
        "two still-published posts retracted in one pass must render the site exactly \
         once, not once per post"
    );

    assert_eq!(
        page_status(&state, "purge.example.com", "/post/a.html").await,
        axum::http::StatusCode::NOT_FOUND,
        "the deleted account's site must stop serving"
    );
    assert_eq!(
        page_status(&state, "purge.example.com", "/post/b.html").await,
        axum::http::StatusCode::NOT_FOUND,
        "the deleted account's site must stop serving"
    );
    assert!(
        state
            .db
            .get_web_rendered(&author.actor_id().0, "index.html")
            .await
            .unwrap()
            .is_none(),
        "the purge must leave no web_rendered row behind, including whatever the \
         retraction pass's own render just produced"
    );
}

/// A retraction pass that fails partway must still stop serving whatever it
/// DID retract before returning — correction to the row's original "one
/// retry window" framing (retries are uncapped and the action can be
/// cancelled, so a bare skip-with-no-render would leave a partly-retracted
/// account's pages stale indefinitely). Corrupts the SECOND post
/// `retract_actor_posts` enumerates (by `list_posts_by_author`'s own order,
/// queried directly rather than assumed) to a non-32-byte stored id, so the
/// pass retracts the first published post, then hits the "content row id is
/// not a 32-byte post id" bail on the second — the deterministic way to
/// exercise a mid-pass failure (a genuine storage error is not reproducible
/// from a test). `post_delete_re_renders_and_the_stale_page_stops_serving`
/// stays green — the single-post door's `RenderSite::Now` is untouched.
#[tokio::test]
async fn account_deletion_renders_what_it_retracted_before_failing_mid_pass() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();

    dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.web.domain.set",
        encode(&WebDomainSetRequest {
            domain: "partial.example.com".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("domain.set ok");
    state
        .db
        .update_web_domain_status("partial.example.com", "active")
        .await
        .unwrap();
    fauna_nest::web_content::domain::reconcile_custom_domain_routing_once(&state.db, &resolver)
        .await
        .unwrap();

    create_real_post(
        &router,
        state.clone(),
        &author,
        "account_deletion_renders_what_it_retracted_before_failing_mid_pass-1",
    )
    .await;
    create_real_post(
        &router,
        state.clone(),
        &author,
        "account_deletion_renders_what_it_retracted_before_failing_mid_pass-2",
    )
    .await;

    // Ask the DB for the pass's own enumeration order rather than assuming
    // creation order survived it.
    let ordered = state
        .db
        .list_posts_by_author(&author.actor_id().0)
        .await
        .unwrap();
    assert_eq!(ordered.len(), 2, "precondition: two posts authored");
    let first: [u8; 32] = ordered[0].as_slice().try_into().unwrap();
    let second: [u8; 32] = ordered[1].as_slice().try_into().unwrap();

    dispatch(
        &router,
        state.clone(),
        author.actor_id().0,
        "fauna.web.publish.set",
        encode(&WebPublishSetRequest {
            post_id: ByteBuf::from(first.to_vec()),
            slug: Some("a".into()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("publish.set ok");

    assert_eq!(
        page_status(&state, "partial.example.com", "/post/a.html").await,
        axum::http::StatusCode::OK,
        "precondition: the first post rendered"
    );

    // Corrupt the SECOND post's stored id to a non-32-byte blob — the
    // deterministic mid-pass failure this test needs (`db.conn()` is the
    // established raw-SQL fault-injection seam, e.g.
    // `conformance_at_rest_byte_scan.rs`).
    {
        let conn = state.db.conn().await;
        conn.execute(
            "UPDATE content SET id = x'ffff' WHERE id = ?1",
            rusqlite::params![second.to_vec()],
        )
        .unwrap();
    }

    let wcs = state.web_content_service.as_ref().unwrap().clone();
    let renders_before = wcs.render_call_count();

    let id = state
        .db
        .create_pending_action(
            &ActionType::AccountDelete,
            &author.actor_id().0,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    state.db.test_set_execute_after(id, 1).await.unwrap();

    let executed = execute_ready_actions(&state).await.unwrap();
    assert_eq!(
        executed, 0,
        "the malformed row must fail the pass, leaving the action pending for retry"
    );

    assert_eq!(
        wcs.render_call_count() - renders_before,
        1,
        "the pass must render once for what it DID retract before bailing, not zero \
         times and not wait for a future successful pass"
    );
    assert_eq!(
        page_status(&state, "partial.example.com", "/post/a.html").await,
        axum::http::StatusCode::NOT_FOUND,
        "the first post was retracted before the failure, so its page must already have \
         stopped serving rather than waiting on a pass that may never succeed"
    );

    let pending = state
        .db
        .list_pending_actions_for_actor(&author.actor_id().0)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "the action is left pending, not lost");
    assert_eq!(pending[0].status, "pending");
}

// ── malformed payload ───────────────────────────────────────────

#[tokio::test]
async fn rejects_malformed_payload() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        ACTOR,
        "fauna.web.publish.list",
        Bytes::from_static(&[0xff, 0xff, 0xff]),
    )
    .await
    .expect_err("malformed payload rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── replay metadata ─────────────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_and_state().await;
    for kind in [
        "fauna.web.publish.set",
        "fauna.web.publish.unset",
        "fauna.web.publish.list",
        "fauna.web.domain.get",
    ] {
        let m = router.kind_meta(kind).expect("kind registered");
        assert!(!m.forbid_replay, "{kind} does not forbid replay");
        assert_eq!(m.default_deadline, std::time::Duration::from_secs(5));
    }
    // domain.set is a non-idempotent registration write @30s.
    let m = router
        .kind_meta("fauna.web.domain.set")
        .expect("kind registered");
    assert!(!m.forbid_replay);
    assert_eq!(m.default_deadline, std::time::Duration::from_secs(30));
}

// ── allowlist ───────────────────────────────────────────────────

#[tokio::test]
async fn allowlist_user_admin_permitted_bridges_denied() {
    for kind in [
        "fauna.web.publish.set",
        "fauna.web.publish.unset",
        "fauna.web.publish.list",
        "fauna.web.domain.set",
        "fauna.web.domain.get",
    ] {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(is_permitted(class, kind), "{kind} permitted for {class:?}");
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(!is_permitted(class, kind), "{kind} denied for {class:?}");
        }
    }
}

// ── one render writes at a time per actor ───────────────────────

// The render-generation fence: an actor's renders may overlap in time, but only
// the newest listing may write. The pins below drive two real renders of one
// actor through the real doors, park the older one mid-write on a gated blob
// store, and assert through the real `Host`-header serve door that it writes
// nothing back. Owner: `web-content-hosting.md` § Routing, render, serving →
// *One render writes at a time*.

/// A disk blob store that parks the FIRST `put` after it is armed, holding one
/// render exactly where `render_for_actor` has listed and is about to
/// store its first page — the window the fence has to close. Every other call
/// passes straight through, so the *second* render runs to completion while the
/// first is parked. No product test hook: the blob store is a real seam every
/// render's writes go through.
///
/// It also COUNTS puts, and can park at the *n*-th one instead
/// ([`Self::park_a_render_at_put`]). A render writes its site once, at its end,
/// so a render parked at its first `put` meets its own next page's staging —
/// a claim check of its own — before it ever reaches that write; only a park at
/// its LAST `put` leaves the one write, and its claim check, as the only thing
/// standing between a supersession and the site.
///
/// Latency-independent (convention 14,
/// `e2e-latency-independent-assertions.md`): the handoff is two `Notify`
/// rendezvous, never a sleep.
struct GatedBlobs {
    inner: fauna_nest::blob_store::DiskBlobStore,
    armed: std::sync::atomic::AtomicBool,
    /// Every `put` so far, counted from the last reset.
    seen: std::sync::atomic::AtomicUsize,
    /// Park the `put` that makes [`Self::seen`] this; `usize::MAX` = never.
    /// One-shot like `armed`: the put that parks resets it.
    park_at: std::sync::atomic::AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl GatedBlobs {
    /// Arm the gate, start `render_published_posts` for `actor` in a task of its
    /// own, and return once that render has listed and reached its
    /// first page write. Released with `self.release.notify_one()`.
    async fn park_a_render(
        self: &Arc<Self>,
        wcs: Arc<fauna_nest::web_content::service::WebContentService>,
        actor: [u8; 32],
    ) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        self.armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let handle = tokio::spawn(async move { wcs.render_published_posts(&actor).await });
        self.entered.notified().await;
        handle
    }

    /// How many blob `put`s one unparked render of `actor`'s site makes — the
    /// calibration [`Self::park_a_render_at_put`] is given, measured rather
    /// than assumed, since a template or another post changes it.
    async fn puts_of_one_render(
        &self,
        wcs: &fauna_nest::web_content::service::WebContentService,
        actor: [u8; 32],
    ) -> usize {
        self.seen.store(0, std::sync::atomic::Ordering::SeqCst);
        wcs.render_published_posts(&actor)
            .await
            .expect("the calibrating render completes");
        self.seen.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// [`Self::park_a_render`], parked at the render's `nth` blob `put`
    /// (1-based) instead of its first.
    async fn park_a_render_at_put(
        self: &Arc<Self>,
        wcs: Arc<fauna_nest::web_content::service::WebContentService>,
        actor: [u8; 32],
        nth: usize,
    ) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        self.seen.store(0, std::sync::atomic::Ordering::SeqCst);
        self.park_at.store(nth, std::sync::atomic::Ordering::SeqCst);
        let handle = tokio::spawn(async move { wcs.render_published_posts(&actor).await });
        self.entered.notified().await;
        handle
    }
}

#[async_trait::async_trait]
impl fauna_nest::blob_store::BlobStoreBackend for GatedBlobs {
    async fn put(&self, hash: &fauna_core::data::ContentHash, data: &[u8]) -> anyhow::Result<()> {
        use std::sync::atomic::Ordering::SeqCst;
        let nth = self.seen.fetch_add(1, SeqCst) + 1;
        let at_nth = self
            .park_at
            .compare_exchange(nth, usize::MAX, SeqCst, SeqCst)
            .is_ok();
        if self.armed.swap(false, SeqCst) || at_nth {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.put(hash, data).await
    }
    async fn get(&self, hash: &fauna_core::data::ContentHash) -> anyhow::Result<Option<Vec<u8>>> {
        self.inner.get(hash).await
    }
    async fn exists(&self, hash: &fauna_core::data::ContentHash) -> anyhow::Result<bool> {
        self.inner.exists(hash).await
    }
    async fn exists_batch(
        &self,
        hashes: &[fauna_core::data::ContentHash],
    ) -> anyhow::Result<Vec<bool>> {
        self.inner.exists_batch(hashes).await
    }
    async fn delete(&self, hash: &fauna_core::data::ContentHash) -> anyhow::Result<()> {
        self.inner.delete(hash).await
    }
    async fn usage_bytes(&self) -> anyhow::Result<u64> {
        self.inner.usage_bytes().await
    }
}

/// [`router_and_state_with_web_site`] over a [`GatedBlobs`] store, with the
/// moderation door registered too (the legal takedown is the adversarial arm).
async fn router_and_state_with_gated_web_site() -> (
    RpcRouter,
    Arc<AppState>,
    Arc<HostResolver>,
    Arc<GatedBlobs>,
    tempfile::TempDir,
) {
    router_and_state_with_gated_web_site_and_deadline(None).await
}

/// [`router_and_state_with_gated_web_site`] with the render's withdrawal
/// deadline chosen by the pin (`None` = the production constant), so a pin
/// states which side of the deadline it is on instead of racing a clock.
async fn router_and_state_with_gated_web_site_and_deadline(
    deadline: Option<std::time::Duration>,
) -> (
    RpcRouter,
    Arc<AppState>,
    Arc<HostResolver>,
    Arc<GatedBlobs>,
    tempfile::TempDir,
) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let resolver = Arc::new(HostResolver::new("example.com".to_string()));
    let blobs = tempfile::tempdir().unwrap();
    let gated = Arc::new(GatedBlobs {
        inner: fauna_nest::blob_store::DiskBlobStore::new(blobs.path()).unwrap(),
        armed: std::sync::atomic::AtomicBool::new(false),
        seen: std::sync::atomic::AtomicUsize::new(0),
        park_at: std::sync::atomic::AtomicUsize::new(usize::MAX),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let mut st = AppState::for_test(db.clone());
    st.host_resolver = Some(resolver.clone());
    let post_segments = st.post_segments.clone();
    let mut wcs = fauna_nest::web_content::service::WebContentService::new(db, gated.clone())
        .with_post_body_source(post_segments);
    if let Some(deadline) = deadline {
        wcs = wcs.with_withdrawal_deadline(deadline);
    }
    st.web_content_service = Some(Arc::new(wcs));
    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    web_handlers::register_web_handlers(&mut b);
    moderation_handlers::register_moderation_handlers(&mut b);
    (b.build(), Arc::new(st), resolver, gated, blobs)
}

/// Undo [`break_the_render`] — the storage fault passes, the way a transient
/// one does, and renders work again.
async fn unbreak_the_render(state: &Arc<AppState>) {
    state
        .db
        .conn()
        .await
        .execute_batch("ALTER TABLE nest_region_broken RENAME TO nest_region")
        .unwrap();
}

/// Every surface of the site that named `slug`, through the real serve door,
/// plus the two owed states — one string, so a failure diagnoses itself
/// (convention 6) instead of sending the reader to a debugger.
async fn site_report(state: &Arc<AppState>, actor: &[u8; 32], slug: &str) -> (bool, String) {
    let page = page_status(state, "mine.example.com", &format!("/post/{slug}.html")).await;
    let feed = page_carries(state, "mine.example.com", "/feed.xml", slug).await;
    let index = page_carries(state, "mine.example.com", "/", slug).await;
    let owed = state.db.list_web_render_owed().await.unwrap();
    let restore = state.db.web_restore_owed_nonce(actor).await.unwrap();
    let gone = page == axum::http::StatusCode::NOT_FOUND && !feed && !index;
    (
        gone,
        format!(
            "post/{slug}.html -> {page}; feed.xml carries it: {feed}; index carries it: {index}; \
             owed-render markers left: {}; restore owed: {}",
            owed.len(),
            restore.is_some()
        ),
    )
}

/// **Arm 1 — an ordinary render in flight across a post delete writes nothing
/// back** (`web-content-hosting.md` § Routing, render, serving → *One render
/// writes at a time*).
///
/// Render A — any door's: the author's own `publish.set`, a template sync, the
/// region walk — has listed both posts and is rendering its pages when the
/// author deletes one. The delete's own render lists without it, writes the
/// site and discharges the marker. A then resumes. Before the fence, A's
/// `store_rendered` upserts brought the deleted post's page, index entry and
/// feed item back with **no marker and no restore owed**, so nothing would ever
/// reconcile it.
#[tokio::test]
async fn a_render_in_flight_across_a_post_delete_writes_nothing_back() {
    let (router, state, resolver, gated, _blobs) = router_and_state_with_gated_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    let ids = published_site_slugs(
        &router,
        &state,
        &resolver,
        &author,
        "fence-one",
        &["the-post", "the-sibling"],
    )
    .await;
    let wcs = state.web_content_service.clone().unwrap();

    let a = gated.park_a_render(wcs, actor).await;

    assert!(delete_post(&router, &state, &author, ids[0]).await.deleted);
    let (mid, mid_report) = site_report(&state, &actor, "the-post").await;
    assert!(
        mid,
        "precondition — the revoke's own render completed while A was parked: {mid_report}"
    );

    gated.release.notify_one();
    a.await
        .unwrap()
        .expect("the parked render finishes cleanly");

    let (gone, after) = site_report(&state, &actor, "the-post").await;
    assert!(
        gone,
        "the render already in flight across the delete wrote the deleted post back: {after}"
    );
    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::OK,
        "the rest of the site must still serve — the fence withholds the stale render's \
         writes, it does not blank the site"
    );
}

/// **Arm 2 — the restore pass is a render source like any other.** A site a
/// failed revoke blanked is being restored by the boot drain when a second post
/// is deleted. The drain's render listed the sibling before that delete, so
/// without the fence it writes the sibling's page back after the delete's own
/// render removed it — the *restore* resurrecting content, which § *A blanked
/// site is owed its restore* says it cannot do.
#[tokio::test]
async fn a_restore_in_flight_across_a_post_delete_writes_nothing_back() {
    let (router, state, resolver, gated, _blobs) = router_and_state_with_gated_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    let ids = published_site_slugs(
        &router,
        &state,
        &resolver,
        &author,
        "fence-two",
        &["the-post", "the-sibling"],
    )
    .await;
    let wcs = state.web_content_service.clone().unwrap();

    // Blank the site with a failed revoke — the fail-closed clear the existing
    // error-arm pins use, with the same real storage fault and no test hook.
    break_the_render(&state).await;
    assert!(delete_post(&router, &state, &author, ids[0]).await.deleted);
    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::NOT_FOUND,
        "precondition: the fail-closed clear blanked the whole site"
    );
    assert!(
        state
            .db
            .web_restore_owed_nonce(&actor)
            .await
            .unwrap()
            .is_some(),
        "precondition: the blanked site is owed its restore"
    );
    unbreak_the_render(&state).await;

    // The boot drain's restore pass is the render in flight.
    gated.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    let a = tokio::spawn({
        let wcs = wcs.clone();
        async move { wcs.drain_owed_renders().await }
    });
    gated.entered.notified().await;

    assert!(delete_post(&router, &state, &author, ids[1]).await.deleted);
    let (mid, mid_report) = site_report(&state, &actor, "the-sibling").await;
    assert!(
        mid,
        "precondition — the revoke's own render completed while the restore was parked: \
         {mid_report}"
    );

    gated.release.notify_one();
    let _drain = a
        .await
        .unwrap()
        .expect("the parked restore finishes cleanly");

    let (gone, after) = site_report(&state, &actor, "the-sibling").await;
    assert!(
        gone,
        "the restore pass in flight across the delete wrote the deleted post back: {after}"
    );
}

/// **Arm 3 — the adversarial one: a legal takedown.** The author controls both
/// how long a render runs and how often one starts (the § Routing, render,
/// serving safety limits), so they can keep a render in flight across a
/// moderator's takedown and have its per-post stage land after the takedown's
/// own render. `"taken_down"` must mean the page is gone and stays gone.
#[tokio::test]
async fn a_render_in_flight_across_a_legal_takedown_writes_nothing_back() {
    let (router, state, resolver, gated, _blobs) = router_and_state_with_gated_web_site().await;
    let admin = [0x74u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    let ids = published_site_slugs(
        &router,
        &state,
        &resolver,
        &author,
        "fence-three",
        &["the-post", "the-sibling"],
    )
    .await;
    let wcs = state.web_content_service.clone().unwrap();

    let a = gated.park_a_render(wcs, actor).await;

    let reply: ModerationLegalTakedownReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.moderation.legal_takedown",
            encode(&ModerationLegalTakedownRequest {
                content_id: hex::encode(ids[0]),
                content_type: "post".into(),
                legal_reference: "EU-DSA-2024/902".into(),
                restore: false,
                extra: Default::default(),
            }),
        )
        .await
        .expect("admin takedown ok"),
    )
    .unwrap();
    assert_eq!(reply.status, "taken_down");
    let (mid, mid_report) = site_report(&state, &actor, "the-post").await;
    assert!(
        mid,
        "precondition — the takedown's own render completed while A was parked: {mid_report}"
    );

    gated.release.notify_one();
    a.await
        .unwrap()
        .expect("the parked render finishes cleanly");

    let (gone, after) = site_report(&state, &actor, "the-post").await;
    assert!(
        gone,
        "a render the author kept in flight across the takedown served the withheld post \
         again: {after}"
    );
}

/// **Arm 4 — the fail-closed clear supersedes too.** A revoking door whose
/// render errors blanks the site and owes its restore; a render already in
/// flight must not write the blanked pages back, which would resurrect exactly
/// what the failed revoke withdrew and leave the site looking healthy while its
/// restore is still owed. This is the arm that fails if the fence covers only
/// the render path and not `clear_web_rendered_owing_restore`.
#[tokio::test]
async fn a_render_in_flight_across_a_fail_closed_clear_writes_nothing_back() {
    let (router, state, resolver, gated, _blobs) = router_and_state_with_gated_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    let ids = published_site_slugs(
        &router,
        &state,
        &resolver,
        &author,
        "fence-four",
        &["the-post", "the-sibling"],
    )
    .await;
    let wcs = state.web_content_service.clone().unwrap();

    let a = gated.park_a_render(wcs, actor).await;

    // The delete's own render now errors before its clear, so the door fails
    // closed: the site goes dark and is owed its restore.
    break_the_render(&state).await;
    assert!(delete_post(&router, &state, &author, ids[0]).await.deleted);
    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::NOT_FOUND,
        "precondition: the fail-closed clear blanked the site while A was parked"
    );
    assert!(
        state
            .db
            .web_restore_owed_nonce(&actor)
            .await
            .unwrap()
            .is_some(),
        "precondition: the blanked site is owed its restore"
    );

    gated.release.notify_one();
    a.await
        .unwrap()
        .expect("the parked render finishes cleanly");

    let (gone, after) = site_report(&state, &actor, "the-post").await;
    assert!(
        gone,
        "the render in flight across the fail-closed clear wrote the withdrawn post back: \
         {after}"
    );
    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::NOT_FOUND,
        "the blanked site must stay dark until a render of its own restores it"
    );
    assert!(
        state
            .db
            .web_restore_owed_nonce(&actor)
            .await
            .unwrap()
            .is_some(),
        "and it must still be owed that restore — a stale render neither pays it nor cancels it"
    );
}

/// **Arm 5 — the render's one write refuses on its own claim**. Arms 1–4 park a render at its FIRST
/// blob `put`, and a render parked there that has been superseded is stopped
/// at its NEXT page's staging, which checks the claim before every `put` —
/// so none of them ever reaches `replace_web_rendered`, and a replace that
/// stopped checking its claim would leave all four green. This one parks the
/// render at its LAST `put`: every page's staging is behind it, and the one
/// write is the only claim check between the delete's supersession and a
/// stale site. It is the window the fence exists for — a check-then-write
/// race a render can only lose there.
///
/// The put count is calibrated from an unparked render of the same site, not
/// assumed: an identical listing makes identical puts, so the calibration's
/// total IS this render's last put. Nothing observable at HEAD can confirm the
/// park landed there — a superseded render parked at ANY put is stopped at its
/// next staging before it puts again — so what shows this pin reaches the
/// window is its mutation record in the owning doc: it alone reddens when the
/// replace stops checking its claim and the staging still does. The withdrawal
/// deadline is stated at an hour so the delete's own render can never be the
/// thing that takes the site dark.
#[tokio::test]
async fn a_render_parked_at_its_last_write_across_a_post_delete_writes_nothing_back() {
    let (router, state, resolver, gated, _blobs) =
        router_and_state_with_gated_web_site_and_deadline(Some(std::time::Duration::from_secs(
            3600,
        )))
        .await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    let ids = published_site_slugs(
        &router,
        &state,
        &resolver,
        &author,
        "fence-five",
        &["the-post", "the-sibling"],
    )
    .await;
    let wcs = state.web_content_service.clone().unwrap();

    let last = gated.puts_of_one_render(&wcs, actor).await;
    assert!(
        last > 0,
        "precondition: a two-post site's render stores pages"
    );
    let a = gated.park_a_render_at_put(wcs, actor, last).await;

    assert!(delete_post(&router, &state, &author, ids[0]).await.deleted);
    let (mid, mid_report) = site_report(&state, &actor, "the-post").await;
    assert!(
        mid,
        "precondition — the revoke's own render completed while A was parked: {mid_report}"
    );

    gated.release.notify_one();
    a.await
        .unwrap()
        .expect("the parked render finishes cleanly");

    let (gone, after) = site_report(&state, &actor, "the-post").await;
    assert!(
        gone,
        "the render parked at its last write ({last} of {last} puts) across the delete wrote \
         the deleted post back: {after}"
    );
    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::OK,
        "the rest of the site must still serve — the fence withholds the stale render's \
         write, it does not blank the site"
    );
}

// ── the marker's own two unwitnessed halves ─────────────────────

/// **A legal takedown's mark rides its own transaction, and the boot drain
/// pays it** (`web-content-hosting.md` § Routing, render, serving → *A revoke
/// is durable*).
///
/// The takedown door renders unconditionally — `rerender_after_moderation_change`,
/// not `render_owed` — so the happy-path takedown pin above stays green with
/// the mark deleted, and nothing saw it. What the mark is FOR is the nest that
/// stops between the takedown's commit and its render: this drives the
/// takedown's transaction alone (the exact call `moderation_handlers` makes
/// before it renders) and then lets the boot drain be the only thing that runs.
#[tokio::test]
async fn a_takedown_torn_before_its_render_is_revoked_by_the_boot_drain() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    let post = published_site(&router, &state, &resolver, &author, "torn-takedown", true).await;
    assert!(
        state.db.list_web_render_owed().await.unwrap().is_empty(),
        "precondition: the publishes are additive and owe nothing"
    );

    // The torn half: the takedown's own transaction, with no render after it.
    state
        .db
        .post_legal_takedown_txn(
            &post,
            &hex::encode(post),
            Some("EU-DSA-2024/902"),
            &actor,
            &[0x74u8; 32],
            "torn before its render",
            0,
        )
        .await
        .expect("the takedown transaction lands");
    assert_eq!(
        state.db.list_web_render_owed().await.unwrap(),
        vec![actor],
        "the takedown's own transaction records the owed render — without it the \
         withheld post's page serves until some unrelated render of this actor runs"
    );
    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-post.html").await,
        axum::http::StatusCode::OK,
        "precondition: the torn state — the post is withheld and its page still serves"
    );

    let wcs = state.web_content_service.clone().unwrap();
    assert_eq!(wcs.drain_owed_renders().await.unwrap().failed, 0);
    assert_the_post_is_revoked(&state, "the boot drain paid the torn takedown").await;
    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::OK,
        "and the rest of the site still serves"
    );
}

/// **The marker's nonce is read BEFORE the render's listing** (§ *A revoke is
/// durable*: "A discharge removes only the marker generation it read before
/// its render began, so a revoke committed *while* a render is running is
/// never discharged by that render").
///
/// The rule had one pin, and it performed the ordering by hand at the database
/// layer — so moving the service's own read to after the render stayed green
/// and the discharge could swallow a revoke it never rendered. This drives the
/// real service: a render is parked at its first page write (past its nonce
/// read and its listing), a revoke commits while it is parked, and the render
/// then completes. The revoke must still be owed.
///
/// The revoke is the DB writer alone, deliberately — a door would render, and
/// a render of its own takes the site over (§ *One render writes at a time*),
/// which would make the parked render abandon before it ever reached the
/// discharge this pin is about.
#[tokio::test]
async fn a_revoke_committed_during_a_render_is_not_discharged_by_it() {
    let (router, state, resolver, gated, _blobs) = router_and_state_with_gated_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    let ids = published_site_slugs(
        &router,
        &state,
        &resolver,
        &author,
        "nonce-order",
        &["the-post", "the-sibling"],
    )
    .await;
    let wcs = state.web_content_service.clone().unwrap();

    let a = gated.park_a_render(wcs.clone(), actor).await;

    // A revoke that MARKS and does not render — what a nest that stopped
    // between a revoking commit and its render leaves behind.
    state.db.unpublish_web_post(&actor, &ids[0]).await.unwrap();
    assert_eq!(
        state.db.list_web_render_owed().await.unwrap(),
        vec![actor],
        "precondition: the unpublish's own transaction recorded the owed render"
    );

    gated.release.notify_one();
    a.await
        .unwrap()
        .expect("the parked render finishes cleanly");

    assert_eq!(
        state.db.list_web_render_owed().await.unwrap(),
        vec![actor],
        "the render read its nonce before its listing, so it discharged the generation \
         it answered and NOT the revoke that landed mid-render — a discharge here would \
         leave the withdrawn post serving with nothing owed and nothing to reconcile it"
    );

    // And the boot drain pays what the mid-render revoke is still owed.
    assert_eq!(wcs.drain_owed_renders().await.unwrap().failed, 0);
    assert_the_post_is_revoked(&state, "the boot drain paid the mid-render revoke").await;
}

// ── a reader sees a whole site or none ──────────────────────────

// A render replaces the site in one transaction as its last act, so a request
// arriving mid-render sees the last completed render's site — and, for a render
// answering an owed revoke that outlasts the withdrawal deadline, no site at
// all — but never a mixture. The pins below park a real render on the gated
// blob store and read the site through the real `Host`-header serve door while
// it is parked. Owner: `web-content-hosting.md` § Routing, render, serving →
// *A reader sees a whole site or none*.

/// Both posts of a two-post site on every surface that names them, through the
/// real serve door — one string, so a failure diagnoses itself (convention 6).
async fn whole_site_report(state: &Arc<AppState>) -> (bool, String) {
    let mut whole = true;
    let mut report = String::new();
    for slug in ["the-post", "the-sibling"] {
        let page = page_status(state, "mine.example.com", &format!("/post/{slug}.html")).await;
        let feed = page_carries(state, "mine.example.com", "/feed.xml", slug).await;
        let index = page_carries(state, "mine.example.com", "/", slug).await;
        whole &= page == axum::http::StatusCode::OK && feed && index;
        report.push_str(&format!(
            "post/{slug}.html -> {page}; feed.xml carries it: {feed}; index carries it: {index}; "
        ));
    }
    (whole, report)
}

/// No page of the site answers: its posts' pages, the index and the feed.
async fn dark_site_report(state: &Arc<AppState>) -> (bool, String) {
    let mut dark = true;
    let mut report = String::new();
    for path in [
        "/post/the-post.html",
        "/post/the-sibling.html",
        "/",
        "/feed.xml",
    ] {
        let status = page_status(state, "mine.example.com", path).await;
        dark &= status == axum::http::StatusCode::NOT_FOUND;
        report.push_str(&format!("{path} -> {status}; "));
    }
    (dark, report)
}

/// **A request arriving mid-render sees the whole old site.** The render is
/// parked at its first blob write — past its listing, with every page still to
/// come. Until the replacement became one transaction the clear had already
/// landed there, and every page of a site that exists before and after the
/// render answered 404 for as long as the render ran.
#[tokio::test]
async fn a_reader_sees_the_whole_old_site_while_a_render_runs() {
    let (router, state, resolver, gated, _blobs) = router_and_state_with_gated_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    published_site_slugs(
        &router,
        &state,
        &resolver,
        &author,
        "whole-site",
        &["the-post", "the-sibling"],
    )
    .await;
    let wcs = state.web_content_service.clone().unwrap();

    let a = gated.park_a_render(wcs, actor).await;

    let (whole, mid) = whole_site_report(&state).await;
    assert!(
        whole,
        "a reader mid-render must see the site the last completed render wrote, not a \
         cleared-but-unwritten one: {mid}"
    );

    gated.release.notify_one();
    a.await
        .unwrap()
        .expect("the parked render finishes cleanly");
    let (whole, after) = whole_site_report(&state).await;
    assert!(whole, "and the site it commits is whole: {after}");
}

/// **A revoke inside its withdrawal deadline keeps the old site whole until the
/// new one commits** — the grace the ruling grants, stated as a pin so nobody
/// mistakes it for a leak: the withdrawn post still serves while its render is
/// in flight and the deadline has not passed, and is gone from every surface
/// the moment the render commits. The deadline here is a day, so the pin never
/// turns on how long the machine takes to reach its assertions (convention 14).
#[tokio::test]
async fn a_revoke_inside_its_deadline_keeps_the_old_site_whole_until_it_commits() {
    let (router, state, resolver, gated, _blobs) =
        router_and_state_with_gated_web_site_and_deadline(Some(std::time::Duration::from_secs(
            86_400,
        )))
        .await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    let ids = published_site_slugs(
        &router,
        &state,
        &resolver,
        &author,
        "inside-deadline",
        &["the-post", "the-sibling"],
    )
    .await;
    let wcs = state.web_content_service.clone().unwrap();

    // A revoke that MARKS and does not render, then the render that answers it.
    state.db.unpublish_web_post(&actor, &ids[0]).await.unwrap();
    let a = gated.park_a_render(wcs, actor).await;

    let (whole, mid) = whole_site_report(&state).await;
    assert!(
        whole,
        "inside the deadline the old site serves whole — no page of it may go missing \
         ahead of the commit: {mid}"
    );
    assert!(
        state
            .db
            .web_restore_owed_nonce(&actor)
            .await
            .unwrap()
            .is_none(),
        "and nothing took the site dark, so no restore is owed"
    );

    gated.release.notify_one();
    a.await
        .unwrap()
        .expect("the parked render finishes cleanly");
    assert_the_post_is_revoked(&state, "the revoke's render committed").await;
    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::OK,
        "the rest of the site serves"
    );
}

/// **A revoke that outlasts its withdrawal deadline takes the site dark — whole
/// dark, owed its restore, and never half-written.** The deadline is zero here,
/// so it has passed before the render's first page (latency-independent: no
/// clock is raced). Parked at its FIRST blob write, the site must already be
/// dark and owed its restore; parked again at its SECOND — one page rendered
/// and stored, the rest to come — it must STILL be dark, because a render
/// writes no row before its one replacing transaction. When it commits the
/// whole new site appears and both debts are paid.
#[tokio::test]
async fn a_revoke_outlasting_its_deadline_takes_the_site_dark_never_half_written() {
    let (router, state, resolver, gated, _blobs) =
        router_and_state_with_gated_web_site_and_deadline(Some(std::time::Duration::ZERO)).await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    let ids = published_site_slugs(
        &router,
        &state,
        &resolver,
        &author,
        "past-deadline",
        &["the-post", "the-sibling"],
    )
    .await;
    let wcs = state.web_content_service.clone().unwrap();

    state.db.unpublish_web_post(&actor, &ids[0]).await.unwrap();
    let a = gated.park_a_render(wcs, actor).await;

    let (dark, first) = dark_site_report(&state).await;
    assert!(
        dark,
        "past its deadline the revoke's render must have withdrawn the old site: {first}"
    );
    assert!(
        state
            .db
            .web_restore_owed_nonce(&actor)
            .await
            .unwrap()
            .is_some(),
        "no dark site without the row that brings it back — the deadline clear owes the \
         restore in its own transaction"
    );

    // Let the first page through and park the render again on its second.
    gated.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    gated.release.notify_one();
    gated.entered.notified().await;
    let (dark, second) = dark_site_report(&state).await;
    assert!(
        dark,
        "one page rendered, the rest to come: the site must still be dark, not half-written: \
         {second}"
    );

    gated.release.notify_one();
    a.await
        .unwrap()
        .expect("the parked render finishes cleanly");
    assert_the_post_is_revoked(&state, "the revoke's render committed").await;
    assert_eq!(
        page_status(&state, "mine.example.com", "/post/the-sibling.html").await,
        axum::http::StatusCode::OK,
        "the new site is committed whole"
    );
    assert!(
        state.db.list_web_render_owed().await.unwrap().is_empty(),
        "the commit discharged the owed render"
    );
    assert!(
        state
            .db
            .web_restore_owed_nonce(&actor)
            .await
            .unwrap()
            .is_none(),
        "and the restore its own deadline clear owed"
    );
}

// ── a rendered page rests inside the sweep's world ─────────────────

// The render's bodies are blob-store writes like any other, so each owes the
// sweep its `blob_metadata` row — and the sweep's reachability oracle owes each
// live page its reference, or the row it now carries is exactly what makes the
// page a deletion candidate. The two halves land as a pair; these pins drive a
// real render and a real zero-grace sweep and read the answer through the real
// `Host`-header serve door. Owners: `backup-restore.md` § 9 (the reference set
// and the sweep's premise) and `web-content-hosting.md` § Routing, render,
// serving → *A reader sees a whole site or none*.

/// The rendered `path`'s body hash, read from its `web_rendered` row.
async fn rendered_body_hash(state: &Arc<AppState>, actor: &[u8; 32], path: &str) -> [u8; 32] {
    let row = state
        .db
        .get_web_rendered(actor, path)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("{path} has a web_rendered row"));
    row.blob_hash
        .as_slice()
        .try_into()
        .expect("a 32-byte body hash")
}

/// One real GC run over `store` with ZERO grace — every blob the oracle does
/// not reference is deleted, however new, so a live page survives only by
/// being referenced.
async fn sweep_with_zero_grace(
    state: &Arc<AppState>,
    store: Arc<dyn fauna_nest::blob_store::BlobStoreBackend>,
) -> fauna_nest::backup::gc::GcResult {
    fauna_nest::backup::gc::garbage_collect(
        &state.db,
        &store,
        fauna_nest::backup::gc::PostBodySource {
            segments: &state.post_segments,
        },
        0,
        None,
        false,
    )
    .await
    .expect("the sweep runs")
}

/// The disk store behind [`router_and_state_with_web_site`]'s service — a
/// second handle on the same directory, which is all a disk store is.
fn disk_store(blobs: &tempfile::TempDir) -> Arc<dyn fauna_nest::blob_store::BlobStoreBackend> {
    Arc::new(fauna_nest::blob_store::DiskBlobStore::new(blobs.path()).unwrap())
}

/// **A rendered body carries its `blob_metadata` row** — the sweep's premise:
/// every blob-store write registers its row in the same breath, because that
/// table IS the sweep's world. Without it a rendered body is neither collected
/// when its page is replaced nor counted, for good.
#[tokio::test]
async fn a_rendered_pages_body_carries_its_blob_metadata_row() {
    let (router, state, resolver, _blobs) = router_and_state_with_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    published_site(&router, &state, &resolver, &author, "metered", false).await;

    for path in ["feed.xml", "index.html", "post/the-post.html"] {
        let hash = rendered_body_hash(&state, &actor, path).await;
        let meta = state.db.get_blob_metadata(&hash).await.unwrap();
        assert!(
            meta.is_some(),
            "the rendered {path}'s body must carry its blob_metadata row"
        );
    }
}

/// **A live page survives a sweep after anyone makes its bytes a GC
/// candidate.** A published page's bytes are public, and `PUT
/// /api/v1/blob/{cid}` writes a `blob_metadata` row for arbitrary bytes under
/// the same raw content address the render uses — the upload's only effect the
/// sweep can see, reproduced here directly. The page is referenced by its
/// `web_rendered` row, so the sweep must keep it; the measured failure was a
/// `200 → 404` with the row left pointing at nothing and nothing marking the
/// site owed.
#[tokio::test]
async fn a_live_page_survives_a_sweep_after_its_bytes_are_made_a_gc_candidate() {
    let (router, state, resolver, blobs) = router_and_state_with_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    published_site(&router, &state, &resolver, &author, "candidate", false).await;
    assert_eq!(
        page_status(&state, "mine.example.com", "/feed.xml").await,
        axum::http::StatusCode::OK,
        "precondition: the feed serves"
    );

    let hash = rendered_body_hash(&state, &actor, "feed.xml").await;
    let store = disk_store(&blobs);
    let bytes = store
        .get(&fauna_core::data::ContentHash::from_digest_raw(hash))
        .await
        .unwrap()
        .expect("the feed's body is stored");
    state
        .db
        .put_blob_metadata(&hash, bytes.len() as i64, "chunk", None, None)
        .await
        .unwrap();

    let result = sweep_with_zero_grace(&state, store.clone()).await;
    let still_stored = store
        .exists(&fauna_core::data::ContentHash::from_digest_raw(hash))
        .await
        .unwrap();
    assert_eq!(
        page_status(&state, "mine.example.com", "/feed.xml").await,
        axum::http::StatusCode::OK,
        "a live page must survive the sweep — bytes still stored: {still_stored}, \
         deleted_blobs: {}, manifest_decode_failures: {}, web_rendered_refs: {}",
        result.deleted_blobs,
        result.manifest_decode_failures,
        result.web_rendered_refs,
    );
}

/// **A replaced page's body is collected** — the other half of the bound. The
/// page's reference goes with its `web_rendered` row when a later render
/// replaces the site, so the next sweep past grace takes the old body and its
/// row; the page that replaced it keeps serving.
#[tokio::test]
async fn a_replaced_pages_body_is_collected_by_the_sweep() {
    let (router, state, resolver, blobs) = router_and_state_with_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    let ids = published_site_slugs(
        &router,
        &state,
        &resolver,
        &author,
        "replaced",
        &["the-post", "the-sibling"],
    )
    .await;
    let old_feed = rendered_body_hash(&state, &actor, "feed.xml").await;

    // Withdraw one post: the re-render's feed no longer names it, so its body
    // is new bytes and the old body is referenced by nothing.
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.web.publish.unset",
        encode(&WebPublishUnsetRequest {
            post_id: ByteBuf::from(ids[0].to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("publish.unset ok");
    let new_feed = rendered_body_hash(&state, &actor, "feed.xml").await;
    assert_ne!(old_feed, new_feed, "precondition: the feed was re-rendered");

    let store = disk_store(&blobs);
    let result = sweep_with_zero_grace(&state, store.clone()).await;
    let old_stored = store
        .exists(&fauna_core::data::ContentHash::from_digest_raw(old_feed))
        .await
        .unwrap();
    let old_row = state.db.get_blob_metadata(&old_feed).await.unwrap();
    assert!(
        !old_stored && old_row.is_none(),
        "the replaced feed's body must be collected, bytes and row — bytes still stored: \
         {old_stored}, row still there: {}, deleted_blobs: {}",
        old_row.is_some(),
        result.deleted_blobs,
    );
    assert!(
        page_carries(&state, "mine.example.com", "/feed.xml", "the-sibling").await,
        "the feed that replaced it keeps serving"
    );
}

/// **A body an in-flight render has stored survives a sweep that runs before
/// the render commits.** A render streams its bodies to the store as it goes
/// and writes no row until its one replacing transaction, so between the two
/// the body is named by nothing on the site — and it already carries its
/// `blob_metadata` row. What keeps it is the render's staged reference, taken
/// under its claim before the body is stored. Parked after its first page's
/// write (the index, new bytes because a post was withdrawn) and swept with
/// zero grace — the position a render outlasting the grace period is in when a
/// production sweep lands mid-render — the committed site must serve that page.
#[tokio::test]
async fn a_body_an_in_flight_render_has_stored_survives_a_sweep_before_its_commit() {
    let (router, state, resolver, gated, _blobs) = router_and_state_with_gated_web_site().await;
    let author = fauna_core::identity::ActorKeypair::generate();
    let actor = author.actor_id().0;
    let ids = published_site_slugs(
        &router,
        &state,
        &resolver,
        &author,
        "in-flight",
        &["the-post", "the-sibling"],
    )
    .await;
    let wcs = state.web_content_service.clone().unwrap();

    state.db.unpublish_web_post(&actor, &ids[0]).await.unwrap();
    // Parked at its first write (the index) — let that one through and park
    // the render again on its second, the index stored and not yet on the site.
    let render = gated.park_a_render(wcs, actor).await;
    gated.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    gated.release.notify_one();
    gated.entered.notified().await;

    let result = sweep_with_zero_grace(&state, gated.clone()).await;

    gated.release.notify_one();
    render
        .await
        .unwrap()
        .expect("the parked render finishes cleanly");
    assert_eq!(
        page_status(&state, "mine.example.com", "/").await,
        axum::http::StatusCode::OK,
        "the render's index, stored before the sweep, must serve once the render commits — \
         deleted_blobs: {}, web_rendered_refs: {}",
        result.deleted_blobs,
        result.web_rendered_refs,
    );
    assert!(
        !page_carries(&state, "mine.example.com", "/", "the-post").await
            && page_carries(&state, "mine.example.com", "/", "the-sibling").await,
        "and it is the new render's index"
    );
}
