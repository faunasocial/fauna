//! Integration round-trip for `fauna.feed.*` — a behavior-preserving
//! transport migration of the HTTP routes `/api/v1/feeds`,
//! `/api/v1/feeds/{id}`, `/api/v1/feeds/{id}/posts`,
//! `/api/v1/feeds/local/posts`, `/api/v1/feeds/{id}/contributors`. The
//! handlers reuse the existing feed CRUD/query pipeline
//! (`feed_routes::*_core`) — these tests exercise the WS-RPC layer: request
//! decode, the reused pipeline reaching the real `CacheDb`, reply encoding,
//! the owner-scoping checks, and the allowlist.
//!
//! `rules` ride typed on the wire (`Vec<FilterRule>`; the JSON-string
//! `rules_json` was removed in place 2026-09-24, `schemas/ratified-breaks.txt`).
//! These tests assert the create→get round-trip preserves a typed rule and
//! that a payload still carrying only `rules_json` is malformed.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/feed.rs`.
//! Slice: tracked internally (§ T2).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).
//! Matches the posts / conversations / search conformance harnesses.

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use fauna_core::scoring::FilterRule;
use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    feed_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    Value, decode_strict as decode,
    feed::{
        FeedContributorGrantReply, FeedContributorGrantRequest, FeedContributorRevokeRequest,
        FeedContributorsListReply, FeedContributorsListRequest, FeedCreateReply, FeedCreateRequest,
        FeedDeleteRequest, FeedGetReply, FeedGetRequest, FeedListReply, FeedListRequest,
        FeedLocalPostsReply, FeedLocalPostsRequest, FeedPostsReply, FeedPostsRequest,
        FeedUpdateRequest,
    },
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    feed_handlers::register_feed_handlers(&mut b);
    (b.build(), state)
}

// ── create → list → get → update → get → delete → get/list ─────

#[tokio::test]
async fn feed_crud_round_trip() {
    let (router, state) = router_with_db_only().await;
    let actor = [11u8; 32];

    // create — rules carry a per-mille u16 confidence
    // (LabelBelow.max_confidence_permille), typed on the wire.
    let rules = vec![FilterRule::LabelBelow {
        category: "spam".into(),
        max_confidence_permille: 250,
    }];
    let create_reply: FeedCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.create",
            encode(&FeedCreateRequest {
                name: "My Feed".into(),
                rules: rules.clone(),
                combination: "all".into(),
                scope: None,
                contributor_seeds: None,
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    let feed_id = create_reply.feed_id;
    assert!(!feed_id.is_empty(), "create returns a feed_id");

    // list — the created feed appears.
    let list_reply: FeedListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.list",
            encode(&FeedListRequest {
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    let summary = list_reply
        .feeds
        .iter()
        .find(|f| f.feed_id == feed_id)
        .expect("created feed appears in list");
    assert_eq!(summary.name, "My Feed");
    assert_eq!(summary.owner, hex::encode(actor));
    assert_eq!(summary.scope, "local");

    // get — the typed rules round-trip (the per-mille confidence survives).
    let get_reply: FeedGetReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.get",
            encode(&FeedGetRequest {
                feed_id: feed_id.clone(),
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();
    assert_eq!(get_reply.name, "My Feed");
    // The nest decodes its stored canonical rules back onto the typed field.
    assert_eq!(
        get_reply.rules, rules,
        "rules round-trip the per-mille rule"
    );

    // update — rename + change combination.
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.feed.update",
        encode(&FeedUpdateRequest {
            feed_id: feed_id.clone(),
            name: "Renamed Feed".into(),
            rules: vec![],
            combination: "any".into(),
            ..Default::default()
        }),
    )
    .await
    .expect("update ok");

    // get — reflects the update.
    let get_reply: FeedGetReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.get",
            encode(&FeedGetRequest {
                feed_id: feed_id.clone(),
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();
    assert_eq!(get_reply.name, "Renamed Feed");
    assert_eq!(get_reply.combination, "any");

    // delete.
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.feed.delete",
        encode(&FeedDeleteRequest {
            feed_id: feed_id.clone(),
            extra: std::collections::BTreeMap::new(),
        }),
    )
    .await
    .expect("delete ok");

    // get — now not found.
    let err = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.feed.get",
        encode(&FeedGetRequest {
            feed_id: feed_id.clone(),
            extra: std::collections::BTreeMap::new(),
        }),
    )
    .await
    .expect_err("get of deleted feed is not found");
    assert_eq!(err.code, "fauna.feed.not_found");

    // list — gone.
    let list_reply: FeedListReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.feed.list",
            encode(&FeedListRequest {
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert!(
        list_reply.feeds.iter().all(|f| f.feed_id != feed_id),
        "deleted feed gone from list"
    );
}

// ── update of an unowned/missing feed → not_found ──────────────

#[tokio::test]
async fn update_unowned_feed_is_not_found() {
    let (router, state) = router_with_db_only().await;
    let err = dispatch(
        &router,
        state,
        [22u8; 32],
        "fauna.feed.update",
        encode(&FeedUpdateRequest {
            feed_id: "nonexistent".into(),
            name: "x".into(),
            rules: vec![],
            combination: "all".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("update of missing feed not found");
    assert_eq!(err.code, "fauna.feed.not_found");
}

// ── the retired rules_json string → malformed ──────────────────

/// `rules_json` was removed in place 2026-09-24 (`ratified-breaks.txt`): a
/// create carrying only the JSON string — no typed `rules` — is a malformed
/// payload, never parsed as rules.
#[tokio::test]
async fn create_with_retired_rules_json_is_malformed() {
    let (router, state) = router_with_db_only().await;
    let mut map = std::collections::BTreeMap::new();
    map.insert("name".to_string(), Value::String("legacy".into()));
    map.insert("rules_json".to_string(), Value::String("[]".into()));
    map.insert("combination".to_string(), Value::String("all".into()));
    let err = dispatch(
        &router,
        state,
        [23u8; 32],
        "fauna.feed.create",
        encode(&Value::Map(map)),
    )
    .await
    .expect_err("a rules_json-only create is rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

/// A newer app's rule list, as the nest's older build meets it: one rule it
/// knows and one it has never heard of (`transport.md` § Rule 3 in full).
fn newer_writer_rules() -> Value {
    let mut known = std::collections::BTreeMap::new();
    let mut has_media = std::collections::BTreeMap::new();
    has_media.insert("required".to_string(), Value::Bool(false));
    known.insert("HasMedia".to_string(), Value::Map(has_media));
    let mut unknown = std::collections::BTreeMap::new();
    let mut has_poll = std::collections::BTreeMap::new();
    has_poll.insert("min_options".to_string(), Value::Integer(3));
    unknown.insert("HasPoll".to_string(), Value::Map(has_poll));
    Value::List(vec![Value::Map(known), Value::Map(unknown)])
}

async fn feed_post_count(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    feed_id: &str,
) -> usize {
    let reply: FeedPostsReply = decode(
        &dispatch(
            router,
            state,
            actor,
            "fauna.feed.posts",
            encode(&FeedPostsRequest {
                feed_id: feed_id.to_string(),
                cursor: None,
                limit: Some(50),
                order: None,
                score_cursor: None,
                score_cursor_created_at: None,
                search: None,
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("feed.posts answers a feed holding an unknown rule"),
    )
    .unwrap();
    reply.posts.len()
}

/// Authoring a NEW rule this nest cannot evaluate is refused typed, for that
/// one request — a feed built around it would silently show nothing.
#[tokio::test]
async fn create_with_a_rule_this_nest_cannot_evaluate_is_refused() {
    let (router, state) = router_with_db_only().await;
    let mut map = std::collections::BTreeMap::new();
    map.insert("name".to_string(), Value::String("newer".into()));
    map.insert("rules".to_string(), newer_writer_rules());
    map.insert("combination".to_string(), Value::String("all".into()));
    let err = dispatch(
        &router,
        state,
        [23u8; 32],
        "fauna.feed.create",
        encode(&Value::Map(map)),
    )
    .await
    .expect_err("a new unknown rule is refused");
    assert_eq!(err.code, "fauna.feed.invalid_params");
}

/// A feed already holding a rule this nest cannot evaluate (a newer build
/// stored it): `feed.get` returns it intact, `feed.update` echoing what get
/// returned stores the identical bytes, and the feed matches nothing — never
/// the post the unknown rule might have hidden. An update trying to ADD a
/// different unknown rule is still refused.
#[tokio::test]
async fn a_stored_unknown_rule_round_trips_byte_identically_and_matches_nothing() {
    let (router, state) = router_with_db_only().await;
    let actor = [25u8; 32];

    let create_reply: FeedCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.create",
            encode(&FeedCreateRequest {
                name: "Newer".into(),
                rules: vec![],
                combination: "any".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    let feed_id = create_reply.feed_id;

    // A newer nest build wrote these bytes to the same row.
    let newer_bytes = fauna_protocol::encode_canonical(&newer_writer_rules())
        .unwrap()
        .to_vec();
    assert!(
        state
            .db
            .update_feed(&feed_id, &actor, "Newer", &newer_bytes, "any", None)
            .await
            .unwrap()
    );

    // A post the known rule (`HasMedia { required: false }`) would admit.
    state
        .db
        .insert_post_index_entry(
            &[0xE5u8; 32],
            &[0xF6u8; 32],
            1_700_002,
            false,
            false,
            "fauna",
            &[],
        )
        .await
        .unwrap();

    let get_reply: FeedGetReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.get",
            encode(&FeedGetRequest {
                feed_id: feed_id.clone(),
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("get answers a feed holding an unknown rule"),
    )
    .unwrap();
    assert_eq!(get_reply.rules.len(), 2);
    assert_eq!(get_reply.rules[0], FilterRule::HasMedia { required: false });
    assert!(!get_reply.rules[1].is_known(), "the newer rule is carried");

    // The app echoes the rules whole on an unrelated edit (a rename).
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.feed.update",
        encode(&FeedUpdateRequest {
            feed_id: feed_id.clone(),
            name: "Renamed".into(),
            rules: get_reply.rules.clone(),
            combination: "any".into(),
            ..Default::default()
        }),
    )
    .await
    .expect("an update echoing the carried rule is accepted");
    let stored = state.db.get_feed(&feed_id).await.unwrap().unwrap();
    assert_eq!(stored.name, "Renamed");
    assert_eq!(
        stored.rules, newer_bytes,
        "the stored rules are byte-identical"
    );

    // Under `any`, the unknown condition is false and the known one admits the
    // post — the unknown rule hides nothing a known rule shows. Under `all`,
    // the unknown condition never matches, so nothing shows.
    assert_eq!(
        feed_post_count(&router, state.clone(), actor, &feed_id).await,
        1,
        "`any`: the known rule still admits"
    );
    state
        .db
        .update_feed(&feed_id, &actor, "Renamed", &newer_bytes, "all", None)
        .await
        .unwrap();
    assert_eq!(
        feed_post_count(&router, state.clone(), actor, &feed_id).await,
        0,
        "`all`: the unknown rule matches nothing"
    );

    // Adding a DIFFERENT unknown rule is new authoring, refused.
    let mut other = std::collections::BTreeMap::new();
    other.insert("HasQuiz".to_string(), Value::Map(Default::default()));
    let mut req = std::collections::BTreeMap::new();
    req.insert("feed_id".to_string(), Value::String(feed_id.clone()));
    req.insert("name".to_string(), Value::String("Renamed".into()));
    req.insert("rules".to_string(), Value::List(vec![Value::Map(other)]));
    req.insert("combination".to_string(), Value::String("all".into()));
    let err = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.feed.update",
        encode(&Value::Map(req)),
    )
    .await
    .expect_err("a new unknown rule is refused on update");
    assert_eq!(err.code, "fauna.feed.invalid_params");
    let stored = state.db.get_feed(&feed_id).await.unwrap().unwrap();
    assert_eq!(
        stored.rules, newer_bytes,
        "the refused update changed nothing"
    );
}

// ── invalid combination → invalid_params ───────────────────────

#[tokio::test]
async fn create_rejects_invalid_combination() {
    let (router, state) = router_with_db_only().await;
    let err = dispatch(
        &router,
        state,
        [24u8; 32],
        "fauna.feed.create",
        encode(&FeedCreateRequest {
            name: "bad".into(),
            rules: vec![],
            combination: "frobnicate".into(),
            scope: None,
            contributor_seeds: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("invalid combination rejected");
    assert_eq!(err.code, "fauna.feed.invalid_params");
}

// ── composition lifecycle (frame § Composition): create-with →
//    get echoes → update-None preserves → update-empty clears ────

#[tokio::test]
async fn feed_composition_lifecycle() {
    let (router, state) = router_with_db_only().await;
    let actor = [25u8; 32];

    let composition = vec![
        fauna_protocol::feed::FeedCompositionEntry {
            factor: "engagement".into(),
            weight_permille: 1000,
            ..Default::default()
        },
        fauna_protocol::feed::FeedCompositionEntry {
            factor: format!("labeler:{}", "ab".repeat(32)),
            weight_permille: -1000,
            ..Default::default()
        },
    ];

    // create — carries a typed composition.
    let create_reply: FeedCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.create",
            encode(&FeedCreateRequest {
                name: "Cats".into(),
                rules: vec![],
                combination: "all".into(),
                composition: Some(composition.clone()),
                ..Default::default()
            }),
        )
        .await
        .expect("create with composition ok"),
    )
    .unwrap();
    let feed_id = create_reply.feed_id;

    let get = |state: Arc<AppState>| {
        let router = &router;
        let feed_id = feed_id.clone();
        async move {
            let reply: FeedGetReply = decode(
                &dispatch(
                    router,
                    state,
                    actor,
                    "fauna.feed.get",
                    encode(&FeedGetRequest {
                        feed_id,
                        extra: Default::default(),
                    }),
                )
                .await
                .expect("get ok"),
            )
            .unwrap();
            reply
        }
    };

    // get — echoes the composition through the at-rest dag-cbor round trip.
    assert_eq!(
        get(state.clone()).await.composition,
        Some(composition.clone())
    );

    // update with composition: None — the stored composition is preserved
    // (an update that edits only name/rules leaves the stored composition).
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.feed.update",
        encode(&FeedUpdateRequest {
            feed_id: feed_id.clone(),
            name: "Cats renamed".into(),
            rules: vec![],
            combination: "all".into(),
            composition: None,
            ..Default::default()
        }),
    )
    .await
    .expect("update without composition ok");
    let reply = get(state.clone()).await;
    assert_eq!(reply.name, "Cats renamed");
    assert_eq!(
        reply.composition,
        Some(composition.clone()),
        "update composition:None must preserve the stored composition"
    );

    // update with composition: Some([]) — the explicit clear.
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.feed.update",
        encode(&FeedUpdateRequest {
            feed_id: feed_id.clone(),
            name: "Cats renamed".into(),
            rules: vec![],
            combination: "all".into(),
            composition: Some(vec![]),
            ..Default::default()
        }),
    )
    .await
    .expect("update clearing composition ok");
    assert_eq!(
        get(state.clone()).await.composition,
        None,
        "update composition:Some([]) must clear to no-composition"
    );
}

#[tokio::test]
async fn create_rejects_invalid_composition() {
    let (router, state) = router_with_db_only().await;
    // Duplicate factor keys are ambiguous — rejected by the shared
    // `fauna_core::scoring::validate_composition` at the create gate.
    let dup = fauna_protocol::feed::FeedCompositionEntry {
        factor: "engagement".into(),
        weight_permille: 1000,
        ..Default::default()
    };
    let err = dispatch(
        &router,
        state,
        [26u8; 32],
        "fauna.feed.create",
        encode(&FeedCreateRequest {
            name: "bad".into(),
            rules: vec![],
            combination: "all".into(),
            composition: Some(vec![dup.clone(), dup]),
            ..Default::default()
        }),
    )
    .await
    .expect_err("duplicate composition factor rejected");
    assert_eq!(err.code, "fauna.feed.invalid_params");
}

// ── global factor set: get empty → set → fold into order=score → clear ──

#[tokio::test]
async fn feed_global_factors_lifecycle_and_fold() {
    use fauna_protocol::feed::{FeedFactorsGetReply, FeedFactorsGetRequest, FeedFactorsSetRequest};
    let (router, state) = router_with_db_only().await;
    let actor = [27u8; 32];

    let get_factors = |state: Arc<AppState>| {
        let router = &router;
        async move {
            let reply: FeedFactorsGetReply = decode(
                &dispatch(
                    router,
                    state,
                    actor,
                    "fauna.feed.factors.get",
                    encode(&FeedFactorsGetRequest {
                        extra: Default::default(),
                    }),
                )
                .await
                .expect("factors.get ok"),
            )
            .unwrap();
            reply.factors
        }
    };

    // Fresh user — empty set.
    assert!(get_factors(state.clone()).await.is_empty());

    // Seed two posts: A older with a high labeler factor row, B newer bare.
    // (Public-post bus rows: content_kind='post', actor_id NULL.)
    let pa = [0xF1u8; 32];
    let pb = [0xF2u8; 32];
    let author = [28u8; 32];
    for (pid, created_at) in [(pa, 1_000_000), (pb, 2_000_000)] {
        state
            .db
            .insert_post_index_entry(&pid, &author, created_at, false, false, "fauna", &[])
            .await
            .unwrap();
    }
    state
        .db
        .insert_content_scores(
            &pa,
            "post",
            None,
            1_000_000,
            &[fauna_core::scoring::ScoreEntry {
                factor: format!("labeler:{}", "ab".repeat(32)),
                score: 900,
                tier: fauna_core::scoring::TIER_COMMUNITY,
                scorer_version: 1,
            }],
        )
        .await
        .unwrap();

    // A feed WITHOUT a composition of its own.
    let create_reply: FeedCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.create",
            encode(&FeedCreateRequest {
                name: "Plain".into(),
                rules: vec![],
                combination: "all".into(),
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();

    let posts_scored = |state: Arc<AppState>| {
        let router = &router;
        let feed_id = create_reply.feed_id.clone();
        async move {
            let reply: FeedPostsReply = decode(
                &dispatch(
                    router,
                    state,
                    actor,
                    "fauna.feed.posts",
                    encode(&FeedPostsRequest {
                        feed_id,
                        cursor: None,
                        limit: None,
                        order: Some("score".into()),
                        score_cursor: None,
                        score_cursor_created_at: None,
                        search: None,
                        extra: Default::default(),
                    }),
                )
                .await
                .expect("posts ok"),
            )
            .unwrap();
            reply
                .posts
                .iter()
                .map(|p| p.post_id.clone())
                .collect::<Vec<_>>()
        }
    };

    // No global factors: legacy single-score ordering (both scores are 0 →
    // created_at DESC tiebreak, B first).
    assert_eq!(
        posts_scored(state.clone()).await,
        vec![hex::encode(pb), hex::encode(pa)]
    );

    // Set a global promote on the labeler factor → it folds into the feed
    // (implicit [(engagement, 1000)] base) and A (900‰) overtakes B.
    let factors = vec![fauna_protocol::feed::FeedCompositionEntry {
        factor: format!("labeler:{}", "ab".repeat(32)),
        weight_permille: 1000,
        ..Default::default()
    }];
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.feed.factors.set",
        encode(&FeedFactorsSetRequest {
            factors: factors.clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("factors.set ok");
    assert_eq!(get_factors(state.clone()).await, factors);
    assert_eq!(
        posts_scored(state.clone()).await,
        vec![hex::encode(pa), hex::encode(pb)],
        "the global factor folds into a feed with no composition of its own"
    );

    // Clear (empty set) → legacy ordering returns.
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.feed.factors.set",
        encode(&FeedFactorsSetRequest {
            factors: vec![],
            extra: Default::default(),
        }),
    )
    .await
    .expect("factors.set clear ok");
    assert!(get_factors(state.clone()).await.is_empty());
    assert_eq!(
        posts_scored(state.clone()).await,
        vec![hex::encode(pb), hex::encode(pa)]
    );
}

// ── contributors: create discovery feed → grant → list → grant-again
//    (added=false) → revoke → list empty ─────────────────────────

#[tokio::test]
async fn contributors_round_trip() {
    let (router, state) = router_with_db_only().await;
    let actor = [31u8; 32];

    // create a discovery feed.
    let create_reply: FeedCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.create",
            encode(&FeedCreateRequest {
                name: "Discovery".into(),
                rules: vec![],
                combination: "all".into(),
                scope: Some("discovery".into()),
                contributor_seeds: None,
                ..Default::default()
            }),
        )
        .await
        .expect("create discovery feed ok"),
    )
    .unwrap();
    let feed_id = create_reply.feed_id;

    // grant a contributor.
    let grant: FeedContributorGrantReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.contributors.grant",
            encode(&FeedContributorGrantRequest {
                feed_id: feed_id.clone(),
                nest_url: "https://peer.example".into(),
                author_id: None,
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("grant ok"),
    )
    .unwrap();
    assert!(grant.added, "first grant adds the contributor");

    // list shows it.
    let list: FeedContributorsListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.contributors.list",
            encode(&FeedContributorsListRequest {
                feed_id: feed_id.clone(),
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(list.contributors.len(), 1);
    assert_eq!(list.contributors[0].nest_url, "https://peer.example");

    // grant again returns added=false with a reason.
    let grant2: FeedContributorGrantReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.contributors.grant",
            encode(&FeedContributorGrantRequest {
                feed_id: feed_id.clone(),
                nest_url: "https://peer.example".into(),
                author_id: None,
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("grant-again ok"),
    )
    .unwrap();
    assert!(!grant2.added, "re-grant reports added=false");
    assert!(grant2.reason.is_some(), "re-grant carries a reason");

    // revoke.
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.feed.contributors.revoke",
        encode(&FeedContributorRevokeRequest {
            feed_id: feed_id.clone(),
            nest_url: "https://peer.example".into(),
            author_id: None,
            extra: std::collections::BTreeMap::new(),
        }),
    )
    .await
    .expect("revoke ok");

    // list empty.
    let list: FeedContributorsListReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.feed.contributors.list",
            encode(&FeedContributorsListRequest {
                feed_id,
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert!(list.contributors.is_empty(), "contributor revoked");
}

// ── revoke of a missing contributor → not_found ────────────────

#[tokio::test]
async fn revoke_missing_contributor_is_not_found() {
    let (router, state) = router_with_db_only().await;
    let actor = [32u8; 32];

    let create_reply: FeedCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.create",
            encode(&FeedCreateRequest {
                name: "Discovery".into(),
                rules: vec![],
                combination: "all".into(),
                scope: Some("discovery".into()),
                contributor_seeds: None,
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();

    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.feed.contributors.revoke",
        encode(&FeedContributorRevokeRequest {
            feed_id: create_reply.feed_id,
            nest_url: "https://absent.example".into(),
            author_id: None,
            extra: std::collections::BTreeMap::new(),
        }),
    )
    .await
    .expect_err("revoke of missing contributor not found");
    assert_eq!(err.code, "fauna.feed.not_found");
}

// ── feed.posts / local.posts: seed a post into the index → query ───

#[tokio::test]
async fn local_feed_returns_seeded_post() {
    let (router, state) = router_with_db_only().await;
    let actor = [41u8; 32];

    // Seed a post directly into the feed index (the seam the production
    // ingest pipeline + scanner write to and `query_feed`/`query_local_feed`
    // read from). Matches the existing feed integration tests
    // (`spam_feed_filter.rs`, `scored_feed.rs`).
    let post_id = [0xA1u8; 32];
    let author = [0xB2u8; 32];
    state
        .db
        .insert_post_index_entry(&post_id, &author, 1_700_000, false, false, "fauna", &[])
        .await
        .unwrap();

    let reply: FeedLocalPostsReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.feed.local.posts",
            encode(&FeedLocalPostsRequest {
                cursor: None,
                limit: Some(50),
                search: None,
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("local.posts ok"),
    )
    .unwrap();

    let seen = reply
        .posts
        .iter()
        .find(|p| p.post_id == hex::encode(post_id));
    let seen = seen.expect("seeded post appears in local feed");
    assert_eq!(seen.author, hex::encode(author));
    assert_eq!(seen.source, "fauna");
    assert!(
        seen.score.is_none(),
        "chronological local feed leaves score unset"
    );
}

#[tokio::test]
async fn feed_posts_chrono_returns_seeded_post_without_score() {
    let (router, state) = router_with_db_only().await;
    let actor = [42u8; 32];

    // create a local feed with empty rules.
    let create_reply: FeedCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.create",
            encode(&FeedCreateRequest {
                name: "Chrono".into(),
                rules: vec![],
                combination: "all".into(),
                scope: None,
                contributor_seeds: None,
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    let feed_id = create_reply.feed_id;

    // seed a post into the index.
    let post_id = [0xC3u8; 32];
    let author = [0xD4u8; 32];
    state
        .db
        .insert_post_index_entry(&post_id, &author, 1_700_001, false, false, "fauna", &[])
        .await
        .unwrap();

    // chronological (no order=score) — post appears, no score, cursor set.
    let reply: FeedPostsReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.feed.posts",
            encode(&FeedPostsRequest {
                feed_id,
                cursor: None,
                limit: Some(50),
                order: None,
                score_cursor: None,
                score_cursor_created_at: None,
                search: None,
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("feed.posts ok"),
    )
    .unwrap();
    let seen = reply
        .posts
        .iter()
        .find(|p| p.post_id == hex::encode(post_id))
        .expect("seeded post appears in feed.posts");
    assert!(
        seen.score.is_none(),
        "chronological order leaves score unset"
    );
    assert!(reply.cursor.is_some(), "chronological order sets cursor");
    assert!(
        reply.score_cursor.is_none(),
        "chronological order leaves score_cursor unset"
    );
}

/// The score-order keyset cursor travels as a pair: a key half alone (the
/// pre-keyset client's shape, which left the wire with the compat-remnant
/// sweep) or a tiebreak half alone is refused, never paged by a made-up
/// predicate — on both scored kinds.
#[tokio::test]
async fn a_half_score_cursor_is_refused() {
    let (router, state) = router_with_db_only().await;
    let actor = [0xD5u8; 32];
    state.db.create_user(&actor, "free", "test").await.unwrap();

    for (key, tiebreak) in [(Some(1_000_000_i64), None), (None, Some(1_700_001_i64))] {
        let err = dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.posts",
            encode(&FeedPostsRequest {
                feed_id: "any".into(),
                cursor: None,
                limit: Some(50),
                order: Some("score".into()),
                score_cursor: key,
                score_cursor_created_at: tiebreak,
                search: None,
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect_err("a half score cursor is refused on feed.posts");
        assert_eq!(err.code, "fauna.feed.invalid_params");

        let err = dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.feed.trending.posts",
            encode(&fauna_protocol::feed::FeedTrendingPostsRequest {
                limit: Some(50),
                score_cursor: key,
                score_cursor_created_at: tiebreak,
                ..Default::default()
            }),
        )
        .await
        .expect_err("a half score cursor is refused on feed.trending.posts");
        assert_eq!(err.code, "fauna.feed.invalid_params");
    }
}

#[tokio::test]
async fn feed_posts_project_quoted_post_id() {
    let (router, state) = router_with_db_only().await;
    let actor = [43u8; 32];

    // Helper: build a signed Post, ingest it via the real `put_post` path
    // (which runs `extract_post_metadata` → `write_post_index`, the ingest
    // projection this test exercises), and return its 32-byte content id.
    async fn ingest(
        db: &CacheDb,
        body: &str,
        created_at: u64,
        references: Vec<fauna_core::data::Reference>,
    ) -> ([u8; 32], fauna_core::data::PostId) {
        let kp = fauna_core::identity::ActorKeypair::generate();
        let post = fauna_core::data::Post {
            author: kp.actor_id(),
            created_at: fauna_core::data::Timestamp(created_at),
            body: fauna_core::data::PostBody::Text {
                content: body.into(),
                facets: vec![],
            },
            references,
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let cid = fauna_core::encoding::compute_post_id(&post).unwrap();
        // References store the full 36-byte CID; the `content` table is keyed
        // on the 32-byte BLAKE3 digest (strip the 4-byte multicodec prefix).
        let digest: [u8; 32] = cid.as_bytes()[4..].try_into().unwrap();
        let bytes = fauna_core::encoding::sign_and_pack(&kp, &post).unwrap();
        db.put_post(&digest, &bytes, None).await.unwrap();
        (digest, cid)
    }

    // The quoted (target) post, then a post that quotes it via Reference::Quote.
    let (target_digest, target_cid) =
        ingest(&state.db, "the original post", 1_700_001, vec![]).await;
    let (quoting_digest, _) = ingest(
        &state.db,
        "quoting the original",
        1_700_002,
        vec![fauna_core::data::Reference::Quote {
            post_id: target_cid,
        }],
    )
    .await;

    let reply: FeedLocalPostsReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.feed.local.posts",
            encode(&FeedLocalPostsRequest {
                cursor: None,
                limit: Some(50),
                search: None,
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("local.posts ok"),
    )
    .unwrap();

    let quoting = reply
        .posts
        .iter()
        .find(|p| p.post_id == hex::encode(quoting_digest))
        .expect("quoting post appears in local feed");
    assert_eq!(
        quoting.quoted_post_id,
        Some(hex::encode(target_digest)),
        "the feed projection carries the quote target's 32-byte id (hex)"
    );

    let target = reply
        .posts
        .iter()
        .find(|p| p.post_id == hex::encode(target_digest))
        .expect("target post appears in local feed");
    assert_eq!(
        target.quoted_post_id, None,
        "a post with no Reference::Quote has no quoted_post_id"
    );
}

/// Flow (`ui/feed.md` § Interaction bar → Repost, ratified 2026-08-10): a
/// signed empty-body `Reference::Repost` post → `put_post` (runs
/// `extract_post_metadata` → `write_post_index`, minting the actor-keyed
/// `content_links link_type='repost'` row) → `fauna.feed.local.posts` serves
/// `FeedPostItem.reposted_post_id` on the repost row (→ the apps'
/// attribution + embedded-original render) and, **for the connection actor**,
/// `viewer_repost_id` on the ORIGINAL's row (→ the repost button's toggle
/// state, and the exact id `unrepost` takes) plus `viewer_liked` off the
/// `engagement_events` like-toggle row `fauna.posts.interact` maintains.
#[tokio::test]
async fn feed_posts_project_repost_carrier_and_viewer_state() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    feed_handlers::register_feed_handlers(&mut b);
    fauna_nest::posts_handlers::register_posts_handlers(&mut b);
    let router = b.build();

    async fn ingest(
        db: &CacheDb,
        kp: &fauna_core::identity::ActorKeypair,
        body: &str,
        created_at: u64,
        references: Vec<fauna_core::data::Reference>,
    ) -> ([u8; 32], fauna_core::data::PostId) {
        let post = fauna_core::data::Post {
            author: kp.actor_id(),
            created_at: fauna_core::data::Timestamp(created_at),
            body: fauna_core::data::PostBody::Text {
                content: body.into(),
                facets: vec![],
            },
            references,
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let cid = fauna_core::encoding::compute_post_id(&post).unwrap();
        let digest: [u8; 32] = cid.as_bytes()[4..].try_into().unwrap();
        let bytes = fauna_core::encoding::sign_and_pack(kp, &post).unwrap();
        db.put_post(&digest, &bytes, None).await.unwrap();
        (digest, cid)
    }

    let author_kp = fauna_core::identity::ActorKeypair::generate();
    let reposter_kp = fauna_core::identity::ActorKeypair::generate();
    let reposter = reposter_kp.actor_id().0;

    let (target_digest, target_cid) = ingest(
        &state.db,
        &author_kp,
        "the original post",
        1_700_001,
        vec![],
    )
    .await;
    // A repost is an EMPTY-BODY post carrying Reference::Repost (the frozen
    // 2026-03-06 data model; `build_referencing_post` produces this shape).
    let (repost_digest, _) = ingest(
        &state.db,
        &reposter_kp,
        "",
        1_700_002,
        vec![fauna_core::data::Reference::Repost {
            post_id: target_cid,
        }],
    )
    .await;

    let local_posts = |actor: [u8; 32]| {
        let router = &router;
        let state = state.clone();
        async move {
            let reply: FeedLocalPostsReply = decode(
                &dispatch(
                    router,
                    state,
                    actor,
                    "fauna.feed.local.posts",
                    encode(&FeedLocalPostsRequest {
                        cursor: None,
                        limit: Some(50),
                        search: None,
                        extra: std::collections::BTreeMap::new(),
                    }),
                )
                .await
                .expect("local.posts ok"),
            )
            .unwrap();
            reply
        }
    };

    // Queried AS the reposter: the repost row carries the carrier, the
    // original's row carries the viewer state.
    let reply = local_posts(reposter).await;
    let repost_row = reply
        .posts
        .iter()
        .find(|p| p.post_id == hex::encode(repost_digest))
        .expect("the repost post appears in the local feed");
    assert_eq!(
        repost_row.reposted_post_id,
        Some(hex::encode(target_digest)),
        "the repost row projects its target (the render carrier)"
    );
    assert_eq!(repost_row.body, "", "a bare repost has no body of its own");
    let target_row = reply
        .posts
        .iter()
        .find(|p| p.post_id == hex::encode(target_digest))
        .expect("the original appears in the local feed");
    assert_eq!(
        target_row.viewer_repost_id,
        Some(hex::encode(repost_digest)),
        "the original's row tells the reposter their own repost's id — unrepost's argument"
    );
    assert!(
        !target_row.viewer_liked,
        "no like recorded yet — viewer_liked stays false"
    );
    assert_eq!(
        target_row.reposted_post_id, None,
        "the original is not itself a repost"
    );

    // A like through the production interact door flips viewer_liked for the
    // actor who liked — and only for them.
    let _ = dispatch(
        &router,
        state.clone(),
        reposter,
        "fauna.posts.interact",
        encode(&fauna_protocol::posts::PostInteractRequest {
            post_id: hex::encode(target_digest),
            action: "like".into(),
            body: None,
            media: None,
            extra: std::collections::BTreeMap::new(),
        }),
    )
    .await
    .expect("like ok");
    let reply = local_posts(reposter).await;
    let target_row = reply
        .posts
        .iter()
        .find(|p| p.post_id == hex::encode(target_digest))
        .unwrap();
    assert!(
        target_row.viewer_liked,
        "the like-toggle row projects as viewer_liked for its actor"
    );

    // A different viewer sees the carrier (viewer-independent) but no viewer
    // state — the pair is per connection actor.
    let other = [77u8; 32];
    let reply = local_posts(other).await;
    let repost_row = reply
        .posts
        .iter()
        .find(|p| p.post_id == hex::encode(repost_digest))
        .unwrap();
    assert_eq!(
        repost_row.reposted_post_id,
        Some(hex::encode(target_digest)),
        "the carrier is viewer-independent"
    );
    let target_row = reply
        .posts
        .iter()
        .find(|p| p.post_id == hex::encode(target_digest))
        .unwrap();
    assert_eq!(
        target_row.viewer_repost_id, None,
        "another viewer has no repost of their own here"
    );
    assert!(!target_row.viewer_liked, "another viewer has not liked it");
}

/// Flow: signed gated post → `put_post` (runs `extract_post_metadata` →
/// `write_post_index`, projecting `Post.gated.tier` into
/// `content_meta.gated_tier`) → `fauna.feed.local.posts` serves
/// `FeedPostItem.gated_tier` → the apps' `gated-post-badge`
/// (`ui/feed.md` § Encryption at rest — the tier name is plaintext floor).
#[tokio::test]
async fn feed_posts_project_gated_tier() {
    let (router, state) = router_with_db_only().await;
    let actor = [44u8; 32];

    async fn ingest(
        db: &CacheDb,
        body: &str,
        created_at: u64,
        gated: Option<fauna_core::subscription::types::GatedInfo>,
    ) -> [u8; 32] {
        let kp = fauna_core::identity::ActorKeypair::generate();
        let post = fauna_core::data::Post {
            author: kp.actor_id(),
            created_at: fauna_core::data::Timestamp(created_at),
            body: fauna_core::data::PostBody::Text {
                content: body.into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated,
            content_warning: None,
            origin: None,
        };
        let cid = fauna_core::encoding::compute_post_id(&post).unwrap();
        let digest: [u8; 32] = cid.as_bytes()[4..].try_into().unwrap();
        let bytes = fauna_core::encoding::sign_and_pack(&kp, &post).unwrap();
        db.put_post(&digest, &bytes, None).await.unwrap();
        digest
    }

    use fauna_core::data::ContentHash;
    use fauna_core::subscription::types::{GatedInfo, KeyAccess};
    let gated_digest = ingest(
        &state.db,
        "public teaser body",
        1_700_011,
        Some(GatedInfo {
            encrypted_ref: ContentHash::from_digest_raw([9u8; 32]),
            key_access: KeyAccess::Broadcast {
                key_blob_ref: ContentHash::from_digest_raw([8u8; 32]),
            },
            tier: "gold".into(),
            tier_rank: 2,
            seal_id: ContentHash::from_digest_raw([7u8; 32]),
            attachment_refs: vec![],
        }),
    )
    .await;
    let public_digest = ingest(&state.db, "plain public body", 1_700_012, None).await;
    // A room-restricted post (`ui/feed.md` § Encryption at rest → *Room-restricted
    // — the ruling*, ruling 3): the reserved tier `room`, and the room's channel
    // id on the plaintext floor — projected beside the tier so a member's card
    // can name the room without decoding the body (the card bullet).
    let room = [0xC7u8; 32];
    let room_digest = ingest(
        &state.db,
        "a teaser for the room",
        1_700_013,
        Some(GatedInfo {
            encrypted_ref: ContentHash::from_digest_raw([6u8; 32]),
            key_access: KeyAccess::Room {
                group_id: fauna_core::subscription::types::MlsGroupId(room.to_vec()),
                epoch: 3,
                generation: None,
            },
            tier: fauna_core::subscription::ROOM_POST_TIER.into(),
            tier_rank: 0,
            seal_id: ContentHash::from_digest_raw([5u8; 32]),
            attachment_refs: vec![],
        }),
    )
    .await;

    let reply: FeedLocalPostsReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.feed.local.posts",
            encode(&FeedLocalPostsRequest {
                cursor: None,
                limit: Some(50),
                search: None,
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("local.posts ok"),
    )
    .unwrap();

    let gated = reply
        .posts
        .iter()
        .find(|p| p.post_id == hex::encode(gated_digest))
        .expect("gated post appears in local feed");
    assert_eq!(
        gated.gated_tier.as_deref(),
        Some("gold"),
        "the feed projection carries the gating tier name"
    );
    assert_eq!(
        gated.body, "public teaser body",
        "the list card body is the plaintext teaser, never the sealed full body"
    );
    assert_eq!(
        gated.gated_room, None,
        "a subscriber-tier post addresses no room"
    );

    let room_post = reply
        .posts
        .iter()
        .find(|p| p.post_id == hex::encode(room_digest))
        .expect("room post appears in local feed");
    assert_eq!(
        room_post.gated_tier.as_deref(),
        Some(fauna_core::subscription::ROOM_POST_TIER),
        "a room post carries the reserved tier"
    );
    assert_eq!(
        room_post.gated_room.as_deref(),
        Some(hex::encode(room).as_str()),
        "the feed projection names the room the post addresses (its channel id)"
    );

    let public = reply
        .posts
        .iter()
        .find(|p| p.post_id == hex::encode(public_digest))
        .expect("public post appears in local feed");
    assert_eq!(
        public.gated_tier, None,
        "a public post carries no gated_tier"
    );
    assert_eq!(public.gated_room, None, "nor a room");
}

// ── allowlist ──────────────────────────────────────────────────

const FEED_KINDS: [&str; 10] = [
    "fauna.feed.list",
    "fauna.feed.create",
    "fauna.feed.get",
    "fauna.feed.update",
    "fauna.feed.delete",
    "fauna.feed.posts",
    "fauna.feed.local.posts",
    "fauna.feed.contributors.list",
    "fauna.feed.contributors.grant",
    "fauna.feed.contributors.revoke",
];

#[tokio::test]
async fn feed_kinds_are_user_facing_at_allowlist_layer() {
    for kind in FEED_KINDS {
        // Feed kinds are user-facing. Admin ⊇ User (an admin is a user who
        // additionally holds the admin role — `is_permitted`'s short-circuit),
        // so every feed kind is permitted for both User and Admin.
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                is_permitted(class, kind),
                "{kind} should be permitted for {class:?}"
            );
        }
        // Bridges are service identities, not feed consumers — denied.
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, kind),
                "{kind} should be denied for {class:?}"
            );
        }
    }
}

// ── replay metadata ────────────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_with_db_only().await;
    for kind in FEED_KINDS {
        let meta = router.kind_meta(kind).expect("kind registered");
        assert!(
            !meta.forbid_replay,
            "{kind} is replay-safe (owner-keyed mutation / pure read)"
        );
        assert_eq!(
            meta.default_deadline,
            std::time::Duration::from_secs(5),
            "{kind} deadline is 5s"
        );
    }
}

// ── block also hides (moderation.md § Corollary — block also hides) ─────

async fn local_post_ids(
    router: &RpcRouter,
    state: &Arc<AppState>,
    viewer: [u8; 32],
) -> Vec<String> {
    let reply: FeedLocalPostsReply = decode(
        &dispatch(
            router,
            state.clone(),
            viewer,
            "fauna.feed.local.posts",
            encode(&FeedLocalPostsRequest {
                cursor: None,
                limit: Some(50),
                search: None,
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("local.posts ok"),
    )
    .unwrap();
    reply.posts.into_iter().map(|p| p.post_id).collect()
}

async fn feed_post_ids(
    router: &RpcRouter,
    state: &Arc<AppState>,
    viewer: [u8; 32],
    feed_id: &str,
    order: Option<&str>,
) -> Vec<String> {
    let reply: FeedPostsReply = decode(
        &dispatch(
            router,
            state.clone(),
            viewer,
            "fauna.feed.posts",
            encode(&FeedPostsRequest {
                feed_id: feed_id.to_string(),
                cursor: None,
                limit: Some(50),
                order: order.map(str::to_string),
                score_cursor: None,
                score_cursor_created_at: None,
                search: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("feed.posts ok"),
    )
    .unwrap();
    reply.posts.into_iter().map(|p| p.post_id).collect()
}

/// Every feed read `viewer` makes: the local feed, and `feed_id` read
/// chronologically and by score.
async fn all_reads(
    router: &RpcRouter,
    state: &Arc<AppState>,
    viewer: [u8; 32],
    feed_id: &str,
) -> Vec<Vec<String>> {
    vec![
        local_post_ids(router, state, viewer).await,
        feed_post_ids(router, state, viewer, feed_id, None).await,
        feed_post_ids(router, state, viewer, feed_id, Some("score")).await,
    ]
}

/// Blocking an author removes their posts from the blocker's own feed reads —
/// the local feed and a feed of the viewer's own, chronological and scored,
/// even under an `Any` combination that could otherwise OR a filter away —
/// and nobody else's; unblocking brings them back on the next read.
#[tokio::test]
async fn blocking_an_author_hides_their_posts_from_the_blockers_feeds_only() {
    let (router, state) = router_with_db_only().await;
    let viewer = [0x51u8; 32];
    let bystander = [0x52u8; 32];
    let blocked = [0x53u8; 32];
    let other = [0x54u8; 32];
    let (blocked_post, other_post) = ([0xD1u8; 32], [0xD2u8; 32]);
    for (post, author, at) in [
        (blocked_post, blocked, 1_700_001),
        (other_post, other, 1_700_002),
    ] {
        state
            .db
            .insert_post_index_entry(&post, &author, at, false, false, "fauna", &[])
            .await
            .unwrap();
    }
    let (blocked_hex, other_hex) = (hex::encode(blocked_post), hex::encode(other_post));

    let create: FeedCreateReply = decode(
        &dispatch(
            &router,
            state.clone(),
            viewer,
            "fauna.feed.create",
            encode(&FeedCreateRequest {
                name: "Anything".into(),
                rules: vec![FilterRule::HasMedia { required: false }],
                combination: "any".into(),
                scope: None,
                contributor_seeds: None,
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    let feed_id = create.feed_id;

    for ids in all_reads(&router, &state, viewer, &feed_id).await {
        assert!(
            ids.contains(&blocked_hex) && ids.contains(&other_hex),
            "precondition: {ids:?}"
        );
    }

    state.db.block_contact(&viewer, &blocked).await.unwrap();
    for ids in all_reads(&router, &state, viewer, &feed_id).await {
        assert!(
            !ids.contains(&blocked_hex),
            "the blocked author's post is hidden: {ids:?}"
        );
        assert!(ids.contains(&other_hex), "everyone else's stays: {ids:?}");
    }
    assert!(
        local_post_ids(&router, &state, bystander)
            .await
            .contains(&blocked_hex),
        "a block hides nothing for anyone else"
    );

    state.db.unblock_contact(&viewer, &blocked).await.unwrap();
    for ids in all_reads(&router, &state, viewer, &feed_id).await {
        assert!(
            ids.contains(&blocked_hex),
            "unblocked: the post returns: {ids:?}"
        );
    }
}
