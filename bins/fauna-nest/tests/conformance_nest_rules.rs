//! The nest's published rules, nest side — `docs/goal/behavior/nest-rules.md`
//! § The nest content policy (*Nest state, written in the app*; *Carriage — the
//! self-description*; *The publisher leg*) and § Acceptance at joining, and a
//! change of rules (*A change is announced*).
//!
//! **What these tests are for.** The grammar, the signed carriage and the
//! composed fold are shared Rust with their own tier_1 tests; what is new here
//! is the nest holding the document: that the admin is the only writer, that a
//! malformed document never becomes the nest's rules, that what the nest signs
//! verifies against the identity its self-description names, that a change is
//! announced to every connected member, and that the nest applies its own rules
//! to the public pages it publishes — and to nothing else.
//!
//! Tier: tier_2 for the two admin kinds and the self-description (real
//! `AppState` + real `CacheDb`, the handlers called through the router); tier_3
//! for the publisher leg (the real web-content render into a disk blob store,
//! re-rendered by the write itself).

use std::collections::BTreeMap;
use std::sync::Arc;

use fauna_core::content_policy::{
    BundledScorer, ContentPolicyDocument, ContentRule, ContentVerdict, GRAMMAR_VERSION, Publisher,
    REASON_DEFAULT_KEY, ScorerKind, join_keyword_terms, scorer_factor,
};
use fauna_core::identity::ActorKeypair;
use fauna_nest::{db::CacheDb, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    Frame,
    content_policy::{
        AdminContentPolicyGetReply, AdminContentPolicyGetRequest, AdminContentPolicySetReply,
        AdminContentPolicySetRequest, SignedContentPolicy,
    },
    decode_frame, decode_strict as decode,
    discovery::{NestInfoReply, NestInfoRequest},
    encode_canonical,
};

mod common;

const DOMAIN: &str = "rules.example";
const KIND_GET: &str = "fauna.admin.content_policy.get";
const KIND_SET: &str = "fauna.admin.content_policy.set";

fn build_router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    fauna_nest::content_policy_handlers::register_content_policy_handlers(&mut b);
    fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
    fauna_nest::posts_handlers::register_posts_handlers(&mut b);
    b.build()
}

/// A nest that has claimed [`DOMAIN`] — the publisher its rules are written for.
fn claimed(state: AppState) -> AppState {
    state
        .identity_domain
        .store(Some(Arc::new(DOMAIN.to_string())));
    state
}

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    (build_router(), Arc::new(claimed(AppState::for_test(db))))
}

async fn seed_admin(state: &Arc<AppState>) -> [u8; 32] {
    let admin = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&admin, "free", "admin").await.unwrap();
    state.db.add_admin_actor(&admin[..]).await.unwrap();
    admin
}

async fn seed_user(state: &Arc<AppState>, handle: &str) -> [u8; 32] {
    let user = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&user, "free", handle).await.unwrap();
    user
}

fn publisher() -> Publisher {
    Publisher::Nest(DOMAIN.to_string())
}

fn reason(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([(REASON_DEFAULT_KEY.to_string(), text.to_string())])
}

fn rule(factor: &str, verdict: ContentVerdict, reason_text: &str) -> ContentRule {
    ContentRule {
        factor: factor.into(),
        min_permille: 500,
        verdict,
        reason_code: "R-1".into(),
        reason: reason(reason_text),
        extra: Default::default(),
    }
}

/// One keyword rule: block any post containing `term`, with the admin's reason.
fn keyword_document(term: &str) -> ContentPolicyDocument {
    ContentPolicyDocument {
        version: GRAMMAR_VERSION,
        rules: vec![rule(
            &scorer_factor(&publisher(), "banned-terms"),
            ContentVerdict::Block,
            "Not on this nest, by the rules you accepted at joining.",
        )],
        scorers: vec![BundledScorer {
            name: "banned-terms".into(),
            kind: ScorerKind::Keywords,
            bytes: join_keyword_terms(&[term.to_string()]),
            extra: Default::default(),
        }],
        extra: Default::default(),
    }
}

/// A category rule — nothing bundled.
fn category_document(category: &str) -> ContentPolicyDocument {
    ContentPolicyDocument {
        version: GRAMMAR_VERSION,
        rules: vec![rule(
            category,
            ContentVerdict::Collapse,
            "Hidden on this nest.",
        )],
        scorers: Vec::new(),
        extra: Default::default(),
    }
}

async fn set(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
    document: ContentPolicyDocument,
) -> Result<AdminContentPolicySetReply, fauna_protocol::RpcError> {
    let req = encode_canonical(&AdminContentPolicySetRequest {
        document,
        extra: Default::default(),
    })
    .unwrap();
    common::call_raw(router, state.clone(), KIND_SET, caller, req)
        .await
        .map(|raw| decode(&raw).unwrap())
}

async fn accepted(
    router: &RpcRouter,
    state: &Arc<AppState>,
    admin: [u8; 32],
    document: ContentPolicyDocument,
) -> SignedContentPolicy {
    let reply = set(router, state, admin, document)
        .await
        .expect("set answers");
    assert_eq!(reply.defect, None, "the document is accepted");
    reply
        .policy
        .expect("an accepted set returns the signed document")
}

async fn get(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
) -> Result<AdminContentPolicyGetReply, fauna_protocol::RpcError> {
    let req = encode_canonical(&AdminContentPolicyGetRequest::default()).unwrap();
    common::call_raw(router, state.clone(), KIND_GET, caller, req)
        .await
        .map(|raw| decode(&raw).unwrap())
}

async fn nest_info(router: &RpcRouter, state: &Arc<AppState>, caller: [u8; 32]) -> NestInfoReply {
    let req = encode_canonical(&NestInfoRequest::default()).unwrap();
    let raw = common::call_raw(router, state.clone(), "fauna.nest.info", caller, req)
        .await
        .expect("the self-description answers");
    decode(&raw).unwrap()
}

fn nest_key(state: &Arc<AppState>) -> [u8; 32] {
    state.nest_identity.public_key_bytes()
}

// ── The two admin kinds ─────────────────────────────────────────────────────

/// A fresh nest has no rules; an accepted write is signed by the identity the
/// self-description names, decodes to the document written, and is what `get`
/// answers from then on.
#[tokio::test]
async fn an_accepted_document_is_signed_stored_and_read_back() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    assert_eq!(get(&router, &state, admin).await.unwrap().policy, None);

    let document = keyword_document("forbidden-term");
    let signed = accepted(&router, &state, admin, document.clone()).await;
    assert_eq!(signed.sequence, 1, "the first accepted write is sequence 1");
    assert!(signed.issued_at > 0);
    assert_eq!(
        signed
            .verify(&nest_key(&state))
            .expect("verifies with the nest's identity key"),
        document
    );
    assert_eq!(
        get(&router, &state, admin).await.unwrap().policy,
        Some(signed)
    );
}

/// Each accepted change is a strictly newer sequence; re-sending the document
/// in force is no change, so it announces nothing new to members.
#[tokio::test]
async fn the_sequence_strictly_increases_and_a_replay_is_no_change() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;

    let first = accepted(&router, &state, admin, category_document("nsfw")).await;
    let second = accepted(&router, &state, admin, category_document("spam")).await;
    assert!(
        second.sequence > first.sequence,
        "{second:?} after {first:?}"
    );
    let replay = accepted(&router, &state, admin, category_document("spam")).await;
    assert_eq!(
        replay, second,
        "the same document re-sent keeps its signed record"
    );
    // Withdrawing every rule is a write like any other.
    let empty = accepted(&router, &state, admin, ContentPolicyDocument::empty()).await;
    assert!(empty.sequence > second.sequence);
}

/// A content-id list is a government's instrument only (decision 2): the nest
/// refuses it with the defect named and keeps the rules it had.
#[tokio::test]
async fn a_list_scorer_is_refused_with_the_defect_named() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let before = accepted(&router, &state, admin, category_document("nsfw")).await;

    let mut document = category_document("nsfw");
    document.scorers.push(BundledScorer {
        name: "ids".into(),
        kind: ScorerKind::List,
        bytes: fauna_core::scoring::build_list_artifact(None, vec![([1u8; 32], 1000)]).unwrap(),
        extra: Default::default(),
    });
    let reply = set(&router, &state, admin, document)
        .await
        .expect("set answers");
    assert_eq!(reply.policy, None);
    let defect = reply.defect.expect("the refusal names the defect");
    assert!(
        defect.contains("list") && defect.contains("nest"),
        "{defect}"
    );
    assert_eq!(
        get(&router, &state, admin).await.unwrap().policy,
        Some(before)
    );
}

/// Every rule needs a reason the blocked member reads; a document without one
/// is malformed and refused.
#[tokio::test]
async fn a_rule_without_a_reason_is_refused() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let mut document = category_document("nsfw");
    document.rules[0].reason.clear();
    let reply = set(&router, &state, admin, document)
        .await
        .expect("set answers");
    assert_eq!(reply.policy, None);
    assert!(reply.defect.expect("a defect").contains("reason"));
    assert_eq!(get(&router, &state, admin).await.unwrap().policy, None);
}

/// A nest with no domain has no publisher name for its rules to be written
/// under, and no member to bind: it refuses rather than stamping a placeholder.
#[tokio::test]
async fn an_unclaimed_nest_refuses_a_document() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let router = build_router();
    let admin = seed_admin(&state).await;
    let reply = set(&router, &state, admin, category_document("nsfw"))
        .await
        .expect("set answers");
    assert_eq!(reply.policy, None);
    assert!(reply.defect.expect("a defect").contains("domain"));
}

/// Both kinds are the admin's alone.
#[tokio::test]
async fn an_ordinary_member_can_neither_write_nor_read_the_admin_surface() {
    let (router, state) = router_with_db().await;
    let user = seed_user(&state, "member").await;
    let err = set(&router, &state, user, category_document("nsfw"))
        .await
        .expect_err("a member's write is refused");
    assert!(
        err.code.contains("permission") || err.code.contains("denied"),
        "{:?}",
        err.code
    );
    get(&router, &state, user)
        .await
        .expect_err("the admin read is admin-only");
    assert_eq!(state.db.get_nest_content_policy().await.unwrap(), None);
}

/// An accepted write is audited under its own action.
#[tokio::test]
async fn an_accepted_write_is_audited() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let signed = accepted(&router, &state, admin, category_document("nsfw")).await;
    let rows = state.db.list_audit(10, None).await.unwrap();
    let row = rows
        .iter()
        .find(|r| r.action == "moderation:content-policy-set")
        .expect("the write is audited");
    assert_eq!(row.actor_id.as_deref(), Some(&admin[..]));
    assert_eq!(
        row.target.as_deref(),
        Some(signed.sequence.to_string().as_str())
    );
}

// ── The self-description and the change notice ──────────────────────────────

/// The self-description carries the signed document — absent until one is
/// written — and a restart over the same database serves the same bytes.
#[tokio::test]
async fn the_self_description_carries_the_signed_rules_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nest.db");
    let db = Arc::new(CacheDb::open(&path).unwrap());
    let state = Arc::new(claimed(AppState::for_test(db)));
    let router = build_router();
    let admin = seed_admin(&state).await;
    let member = seed_user(&state, "member").await;
    assert_eq!(
        nest_info(&router, &state, member)
            .await
            .moderation
            .content_policy,
        None
    );

    let signed = accepted(&router, &state, admin, keyword_document("forbidden-term")).await;
    let carried = nest_info(&router, &state, member).await;
    assert_eq!(carried.nest_id, hex::encode(nest_key(&state)));
    assert_eq!(carried.moderation.content_policy.as_ref(), Some(&signed));

    // The restart: a fresh process over the same file, the same identity.
    let identity = Arc::clone(&state.nest_identity);
    drop(router);
    drop(state);
    let db = Arc::new(CacheDb::open(&path).unwrap());
    let restarted = Arc::new(AppState {
        nest_identity: identity,
        ..claimed(AppState::for_test(db))
    });
    let again = nest_info(&build_router(), &restarted, member).await;
    assert_eq!(
        encode_canonical(&again.moderation.content_policy).unwrap(),
        encode_canonical(&Some(signed)).unwrap(),
        "the same signed bytes after a restart"
    );
}

/// The next push frame on `rx`, skipping nothing: the change notice must be
/// the push the write sends.
fn next_push_kind(rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>) -> Option<String> {
    let bytes = rx.try_recv().ok()?;
    match decode_frame(&bytes).expect("a frame") {
        Frame::Push(push) => Some(push.kind),
        other => panic!("expected a push, got {other:?}"),
    }
}

/// An accepted write tells every connected member that the self-description is
/// stale, so a device re-reads it and finds the newer sequence; a refused one
/// and a replay tell nobody anything.
#[tokio::test]
async fn an_accepted_change_stales_every_connected_members_self_description() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let alice = seed_user(&state, "alice").await;
    let bob = seed_user(&state, "bob").await;
    let (_a, mut alice_rx) = state.ws.subscribe(alice);
    let (_b, mut bob_rx) = state.ws.subscribe(bob);

    accepted(&router, &state, admin, category_document("nsfw")).await;
    for rx in [&mut alice_rx, &mut bob_rx] {
        let kind = next_push_kind(rx).expect("the change is pushed");
        assert_eq!(kind, "fauna.nest.info_changed");
        assert!(
            fauna_protocol::push_events::StaleSurfaces::for_kind(&kind).nest_info,
            "the push stales the self-description"
        );
    }

    accepted(&router, &state, admin, category_document("nsfw")).await;
    let mut bad = category_document("nsfw");
    bad.rules[0].reason_code.clear();
    set(&router, &state, admin, bad).await.unwrap();
    assert_eq!(
        next_push_kind(&mut alice_rx),
        None,
        "a replay or a refusal is no change"
    );
}

// ── The publisher leg ───────────────────────────────────────────────────────
//
// `nest-rules.md` § The nest content policy → *The publisher leg*: the nest
// applies its own document to the public web pages it renders from public
// posts, through the composed engine with the nest source — exactly as
// `region-blocking.md` § The nest-as-publisher leg applies a situs's region
// policy there — and to nothing else.

async fn router_with_web_site() -> (RpcRouter, Arc<AppState>, tempfile::TempDir) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let blobs = tempfile::tempdir().unwrap();
    let store = Arc::new(fauna_nest::blob_store::DiskBlobStore::new(blobs.path()).unwrap());
    let state = AppState {
        web_content_service: Some(Arc::new(
            fauna_nest::web_content::service::WebContentService::new(db.clone(), store),
        )),
        ..AppState::for_test(db)
    };
    (build_router(), Arc::new(claimed(state)), blobs)
}

async fn publish_text_post(
    state: &Arc<AppState>,
    author: [u8; 32],
    slug: &str,
    text: &str,
) -> ([u8; 32], Vec<u8>) {
    use fauna_core::data::{Post, PostBody, Timestamp};
    let post = Post {
        author: fauna_core::identity::ActorId(author),
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
    let data = fauna_core::encoding::canonical_encode(&post).unwrap();
    let post_id: [u8; 32] = *blake3::hash(&data).as_bytes();
    state.db.put_post(&post_id, &data, None).await.unwrap();
    state
        .db
        .publish_web_post(&author, &post_id, slug)
        .await
        .unwrap();
    (post_id, data)
}

async fn public_page(state: &Arc<AppState>, author: &[u8; 32], path: &str) -> String {
    let row = state
        .db
        .get_web_rendered(author, path)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("{path} is rendered"));
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&row.blob_hash);
    let bytes = state
        .web_content_service
        .as_ref()
        .unwrap()
        .blob_store()
        .get(&fauna_core::data::ContentHash::from_digest_raw(hash))
        .await
        .unwrap()
        .expect("the rendered blob exists");
    String::from_utf8(bytes).unwrap()
}

/// **The nest's keyword rule governs its public page, and nothing else** — end
/// to end over the real handlers, with no test-side render after the first: the
/// admin's write re-renders the site by itself. The flagged post's page and
/// every listing show the nest's placeholder with the admin's reason; its
/// unflagged sibling renders untouched; the authenticated `fauna.posts.get`
/// still returns the post's bytes verbatim (the in-app surface is the member's
/// own app's to judge); and withdrawing the rule brings the body back.
#[tokio::test]
async fn the_nests_own_rules_govern_its_public_page_and_nothing_else() {
    let (router, state, _blobs) = router_with_web_site().await;
    let admin = seed_admin(&state).await;
    let author = seed_user(&state, "publisher").await;
    const BODY: &str = "a body with forbidden-term-6b2 in it";
    const CLEAN: &str = "an-unflagged-sibling-body-41c7";
    let (flagged, flagged_bytes) = publish_text_post(&state, author, "flagged", BODY).await;
    publish_text_post(&state, author, "clean", CLEAN).await;
    state
        .web_content_service
        .as_ref()
        .unwrap()
        .render_published_posts(&author)
        .await
        .unwrap();
    const PAGE: &str = "post/flagged.html";
    assert!(public_page(&state, &author, PAGE).await.contains(BODY));

    accepted(
        &router,
        &state,
        admin,
        keyword_document("FORBIDDEN-TERM-6B2"),
    )
    .await;
    let page = public_page(&state, &author, PAGE).await;
    let frame = fauna_i18n::strings::nest_rules::blocked_notice(DOMAIN);
    assert!(page.contains(&frame), "the nest's placeholder: {page}");
    assert!(
        page.contains("Not on this nest, by the rules you accepted at joining."),
        "the admin's reason, verbatim: {page}"
    );
    assert!(!page.contains(BODY), "the body is withheld: {page}");
    for listing in ["index.html", "feed.xml"] {
        let bytes = public_page(&state, &author, listing).await;
        assert!(!bytes.contains(BODY), "{listing}: {bytes}");
        assert!(
            bytes.contains(CLEAN),
            "the unflagged sibling renders in {listing}: {bytes}"
        );
    }
    assert!(
        public_page(&state, &author, "post/clean.html")
            .await
            .contains(CLEAN)
    );

    // The in-app read is untouched: the member's own app applies the rules.
    let raw = common::dispatch(
        &router,
        state.clone(),
        author,
        "fauna.posts.get",
        bytes::Bytes::from(
            encode_canonical(&fauna_protocol::posts::PostGetRequest {
                post_id: hex::encode(flagged),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("the author reads their own post");
    let reply: fauna_protocol::posts::PostGetReply = decode(&raw).unwrap();
    assert_eq!(reply.body.into_vec(), flagged_bytes);

    // Withdrawing every rule re-renders the body back, by itself.
    accepted(&router, &state, admin, ContentPolicyDocument::empty()).await;
    assert!(public_page(&state, &author, PAGE).await.contains(BODY));
}
