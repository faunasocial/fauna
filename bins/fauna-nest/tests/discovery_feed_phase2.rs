use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use fauna_core::data::{Facet, FacetFeature, Post, PostBody, Reference, Timestamp};
use fauna_core::encoding::{canonical_encode, compute_post_id};
use fauna_core::identity::ActorKeypair;
use fauna_core::scoring::{FilterCombination, FilterRule};
use fauna_nest::db::CacheDb;
use fauna_nest::discovery::{PollConfig, spawn_discovery_poller};
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, FeedEvent};
use fauna_nest::token_store::TokenStore;

/// Create a **channel-capable** test AppState with an in-memory database,
/// returning the state and the receiver end of the feed-event channel.
///
/// The nest is made a real federation peer — mirroring
/// `conformance_federation_channel.rs`'s `start_nest` — so the discovery poller
/// can reach it over the production carrier (the federation WS-RPC channel, the
/// sole Fauna↔Fauna carrier since Spec Y2 slice 5 retired the HTTP interim):
///
/// - a **distinct** `nest_identity` (the shared `for_test`
///   `cached_test_nest_identity` gives every nest the SAME `nest_id`, but the
///   handshake's "did I reach the intended nest?" check — and the pool keying —
///   require distinct peer identities);
/// - the `fauna.federation.*` handlers (incl. `fauna.federation.feed.query`) on
///   the `federation_router` (the bare `for_test` router is empty, so a dialed
///   peer would reject the feed query);
/// - the anonymous `fauna.nest.info` on the `rpc_router`, so the pool can
///   resolve this nest's `nest_id` from its URL before dialing.
fn test_state_with_events() -> (Arc<AppState>, tokio::sync::mpsc::Receiver<FeedEvent>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let token_store = Arc::new(TokenStore::new());

    let (feed_event_tx, feed_event_rx) = tokio::sync::mpsc::channel(100);

    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();

    let state = Arc::new(AppState {
        auth: fauna_nest::state::AuthState {
            token_store: token_store.clone(),
            ..Default::default()
        },
        feed_event_tx,
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(db.clone())
    });

    (state, feed_event_rx)
}

/// Spawn a test server and return the base URL.
async fn spawn_server(state: Arc<AppState>) -> String {
    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}")
}

/// Create a signed test post with tags, store it in the DB, index it, and
/// return the hex-encoded post_id.
async fn create_tagged_post(
    state: &Arc<AppState>,
    keypair: &ActorKeypair,
    content: &str,
    tags: &[&str],
) -> String {
    let facets: Vec<Facet> = tags
        .iter()
        .map(|tag| Facet {
            byte_start: 0,
            byte_end: 0,
            feature: FacetFeature::Tag {
                name: tag.to_string(),
            },
        })
        .collect();

    let post = Post {
        author: keypair.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Text {
            content: content.to_string(),
            facets,
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let encoded = fauna_core::encoding::sign_and_pack(keypair, &post).unwrap();

    let post_id = compute_post_id(&post).unwrap();
    let post_id_bytes: [u8; 32] = {
        let b = post_id.as_bytes();
        let mut d = [0u8; 32];
        d.copy_from_slice(&b[4..]);
        d
    };
    state
        .db
        .put_post(&post_id_bytes, &encoded, None)
        .await
        .unwrap();
    state.db.index_post(&post_id_bytes, &post).await.unwrap();

    hex::encode(post_id_bytes)
}

/// Create a signed test post with a Repost reference and tags, store and index
/// it, and return the hex-encoded post_id.
async fn create_repost_with_reference(
    state: &Arc<AppState>,
    keypair: &ActorKeypair,
    referenced_post_id_hex: &str,
    content: &str,
    tags: &[&str],
) -> String {
    let facets: Vec<Facet> = tags
        .iter()
        .map(|tag| Facet {
            byte_start: 0,
            byte_end: 0,
            feature: FacetFeature::Tag {
                name: tag.to_string(),
            },
        })
        .collect();

    let ref_bytes = hex::decode(referenced_post_id_hex).unwrap();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&ref_bytes);
    let ref_post_id = fauna_cbor::Cid::from_digest_dag_cbor(arr);

    let post = Post {
        author: keypair.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Text {
            content: content.to_string(),
            facets,
        },
        references: vec![Reference::Repost {
            post_id: ref_post_id,
        }],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let encoded = fauna_core::encoding::sign_and_pack(keypair, &post).unwrap();

    let post_id = compute_post_id(&post).unwrap();
    let post_id_bytes: [u8; 32] = {
        let b = post_id.as_bytes();
        let mut d = [0u8; 32];
        d.copy_from_slice(&b[4..]);
        d
    };
    state
        .db
        .put_post(&post_id_bytes, &encoded, None)
        .await
        .unwrap();
    state.db.index_post(&post_id_bytes, &post).await.unwrap();

    hex::encode(post_id_bytes)
}

/// The success criterion: a bluesky-bridged, `#rust`-tagged
/// candidate a peer serves keeps its REAL advertised token in the QUERYING
/// nest's own local index — never the peer's fetch URL (the pre-740 bug on
/// the RECEIVING side) and never a hard-coded `"fauna"` (post 633's bug, on
/// the SERVING side, already pinned by `conformance_federation_channel.rs`'s
/// `feed_query_over_channel_serves`). This is the end-to-end pin that a
/// serving-side fix and a parsing-side fix are actually wired together
/// through the discovery poller.
#[tokio::test]
async fn discovery_ingested_post_indexes_the_peers_real_source_token() {
    let (nest_a, event_rx) = test_state_with_events();
    let (nest_b, _rx_b) = test_state_with_events();

    let bluesky_kp = ActorKeypair::generate();
    let bluesky_post = Post {
        author: bluesky_kp.actor_id(),
        created_at: Timestamp::now(),
        body: PostBody::Text {
            content: "bridged from bluesky".into(),
            facets: vec![Facet {
                byte_start: 0,
                byte_end: 0,
                feature: FacetFeature::Tag {
                    name: "rust".to_string(),
                },
            }],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let bluesky_data = canonical_encode(&bluesky_post).unwrap();
    let bluesky_post_id: [u8; 32] = *blake3::hash(&bluesky_data).as_bytes();
    nest_b
        .db
        .put_post_with_source(&bluesky_post_id, &bluesky_data, "bluesky")
        .await
        .unwrap();

    let nest_b_url = spawn_server(nest_b.clone()).await;

    let alice = ActorKeypair::generate();
    let rules = vec![FilterRule::HasHashtag {
        tags: vec!["rust".to_string()],
    }];
    let rules_encoded = canonical_encode(&rules).unwrap();
    let seeds = serde_json::to_string(&vec![&nest_b_url]).unwrap();
    let feed_id = nest_a
        .db
        .create_feed(
            &alice.actor_id().0,
            "Bluesky Discovery",
            &rules_encoded,
            "all",
            "discovery",
            &seeds,
            None,
        )
        .await
        .unwrap();
    nest_a
        .db
        .upsert_contributor(&feed_id, &nest_b_url, None, "seed")
        .await
        .unwrap();

    let config = PollConfig {
        hot_interval: Duration::from_secs(1),
        warm_interval: Duration::from_secs(3),
        cold_interval: Duration::from_secs(5),
        priority_recalc_interval: Duration::from_secs(10),
    };
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let poller_handle = spawn_discovery_poller(
        nest_a.db.clone(),
        nest_a.clone(),
        config,
        event_rx,
        shutdown_rx,
    );

    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        nest_a
            .db
            .get_post_source(&bluesky_post_id)
            .await
            .unwrap()
            .as_deref(),
        Some("bluesky"),
        "the querying nest must index the peer's real token, not the fetch URL"
    );
    assert_eq!(
        nest_a
            .db
            .get_post_origin_nest_url(&bluesky_post_id)
            .await
            .unwrap()
            .as_deref(),
        Some(nest_b_url.as_str()),
        "the origin nest is recorded separately from the badge token"
    );

    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), poller_handle)
        .await
        .expect("poller should shut down within 5 seconds")
        .expect("poller task should not panic");
}

#[tokio::test]
async fn scenario_8_2_discovery_feed_with_referral_chain() {
    // ── 1. Create 3 nests ──────────────────────────────────────────────
    let (nest_a, event_rx) = test_state_with_events();
    let (nest_b, _rx_b) = test_state_with_events();
    let (nest_c, _rx_c) = test_state_with_events();

    let alice = ActorKeypair::generate();
    let bob = ActorKeypair::generate();
    let charlie = ActorKeypair::generate();

    // ── 2. Bob creates a #rust post on nest_b ──────────────────────────
    create_tagged_post(&nest_b, &bob, "Bob loves Rust!", &["rust"]).await;

    // ── 3. Charlie creates a #rust post on nest_c ──────────────────────
    let charlie_post_id_hex =
        create_tagged_post(&nest_c, &charlie, "Charlie on Rust", &["rust"]).await;

    // Small delay to ensure distinct timestamps
    tokio::time::sleep(Duration::from_millis(10)).await;

    // ── 4. Spawn nest_c server first (need URL for step 5) ─────────────
    let nest_c_url = spawn_server(nest_c.clone()).await;

    // ── 5. Index Charlie's post into nest_b's post_index with a real
    //       `origin_nest_url`, the same shape the discovery poller itself
    //       would write — replacing the old scheme,
    //       a hand-written fetch URL smuggled through `source` and parsed
    //       back out by `resolve_nest_from_post_index`'s `LIKE 'http%'` scan,
    //       with a dedicated column. This still writes the row directly rather
    //       than running nest_b's own discovery poller against nest_c: doing
    //       that for real would tag-match Charlie's post into nest_b's OWN
    //       `remote_query_feed_core` results too (that handler filters the
    //       whole local index by the CALLER's rules — `feed_routes.rs`'s
    //       `remote_query_feed_core` — with no carve-out for discovery-
    //       ingested rows), so nest_a would find Charlie directly off nest_b
    //       instead of via the referral chain this scenario exists to pin.
    //       No tags here for the same reason the pre-740 version had none:
    //       this post must be absent from nest_b's own `HasHashtag["rust"]`
    //       matches so 12b–12d below genuinely exercise `follow_references`.
    let charlie_post_id_bytes = hex::decode(&charlie_post_id_hex).unwrap();
    let mut charlie_pid_arr = [0u8; 32];
    charlie_pid_arr.copy_from_slice(&charlie_post_id_bytes);
    let charlie_author_bytes = charlie.actor_id().0;
    nest_b
        .db
        .insert_post_index_entry_with_origin(
            &charlie_pid_arr,
            &charlie_author_bytes,
            Timestamp::now().0 as i64,
            false,
            false,
            "fauna",
            &[],
            Some(nest_c_url.as_str()),
        )
        .await
        .unwrap();

    // ── 6. Bob creates a repost referencing Charlie's post on nest_b ───
    create_repost_with_reference(
        &nest_b,
        &bob,
        &charlie_post_id_hex,
        "Check out Charlie's post!",
        &["rust"],
    )
    .await;

    // ── 7. Spawn nest_b server ─────────────────────────────────────────
    let nest_b_url = spawn_server(nest_b.clone()).await;

    // ── 8. Alice creates a discovery feed on nest_a ────────────────────
    let rules = vec![FilterRule::HasHashtag {
        tags: vec!["rust".to_string()],
    }];
    let rules_encoded = canonical_encode(&rules).unwrap();
    let seeds = serde_json::to_string(&vec![&nest_b_url]).unwrap();

    let feed_id = nest_a
        .db
        .create_feed(
            &alice.actor_id().0,
            "Rust Discovery",
            &rules_encoded,
            "all",
            "discovery",
            &seeds,
            None,
        )
        .await
        .unwrap();

    // ── 9. Seed the contributor ────────────────────────────────────────
    nest_a
        .db
        .upsert_contributor(&feed_id, &nest_b_url, None, "seed")
        .await
        .unwrap();

    // ── 10. Start discovery poller with fast intervals ─────────────────
    let config = PollConfig {
        hot_interval: Duration::from_secs(1),
        warm_interval: Duration::from_secs(3),
        cold_interval: Duration::from_secs(5),
        priority_recalc_interval: Duration::from_secs(10),
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // The poller queries the peer over the federation WS-RPC channel — the sole
    // Fauna↔Fauna carrier since Spec Y2 slice 5 retired the HTTP interim. Each
    // spawned nest is channel-capable (`test_state_with_events`), so the pool
    // resolves the peer's nest_id via `fauna.nest.info`, dials the channel, and
    // `fauna.federation.feed.query` serves the referral chain end-to-end over the
    // production carrier.
    let poller_handle = spawn_discovery_poller(
        nest_a.db.clone(),
        nest_a.clone(),
        config,
        event_rx,
        shutdown_rx,
    );

    // ── 11. Wait for initial poll + referral chain discovery ───────────
    tokio::time::sleep(Duration::from_secs(4)).await;

    // ── 12. Assert all spec assertions (§12.3) ─────────────────────────

    // 12a. Bob's post is in nest_a's feed
    let feed_posts = nest_a
        .db
        .query_feed(&rules, FilterCombination::All, &[], None, 50)
        .await
        .unwrap();
    assert!(
        !feed_posts.is_empty(),
        "nest_a should have posts after initial poll"
    );

    // Check that Bob's post is present (at least one post authored by Bob)
    let bob_author = bob.actor_id().0.to_vec();
    let bob_posts: Vec<_> = feed_posts
        .iter()
        .filter(|p| p.author == bob_author)
        .collect();
    assert!(
        !bob_posts.is_empty(),
        "Bob's posts should be in nest_a's feed"
    );

    // 12b. Charlie discovered via referral chain
    let contributors = nest_a.db.list_contributors(&feed_id).await.unwrap();
    let charlie_author = charlie.actor_id().0.to_vec();
    let charlie_contributor = contributors
        .iter()
        .find(|c| c.author_id.as_deref() == Some(&charlie_author[..]));
    assert!(
        charlie_contributor.is_some(),
        "Charlie should be discovered as a contributor via referral chain"
    );
    let charlie_contributor = charlie_contributor.unwrap();

    // 12c. Bob's discovered_via is "seed"
    let bob_author_bytes = bob.actor_id().0.to_vec();
    let bob_contributor = contributors
        .iter()
        .find(|c| c.author_id.as_deref() == Some(&bob_author_bytes[..]));
    assert!(
        bob_contributor.is_some(),
        "Bob should be a specific-author contributor"
    );
    assert_eq!(
        bob_contributor.unwrap().discovered_via,
        "seed",
        "Bob should have discovered_via = 'seed'"
    );

    // 12d. Charlie's discovered_via is "referral"
    assert_eq!(
        charlie_contributor.discovered_via, "referral",
        "Charlie should have discovered_via = 'referral'"
    );

    // 12e. Both hit_count >= 1
    assert!(
        bob_contributor.unwrap().hit_count >= 1,
        "Bob's hit_count should be >= 1, got {}",
        bob_contributor.unwrap().hit_count
    );
    assert!(
        charlie_contributor.hit_count >= 1,
        "Charlie's hit_count should be >= 1, got {}",
        charlie_contributor.hit_count
    );

    // 12f. Both poll_priority == "hot"
    assert_eq!(
        bob_contributor.unwrap().poll_priority,
        "hot",
        "Bob's poll_priority should be 'hot'"
    );
    assert_eq!(
        charlie_contributor.poll_priority, "hot",
        "Charlie's poll_priority should be 'hot'"
    );

    // ── 13. Charlie creates another #rust post on nest_c ───────────────
    create_tagged_post(&nest_c, &charlie, "More Rust from Charlie!", &["rust"]).await;

    // ── 14. Wait for follow-up poll ────────────────────────────────────
    tokio::time::sleep(Duration::from_secs(3)).await;

    // ── 15. Assert >= 3 posts in nest_a's feed ─────────────────────────
    let feed_posts_after = nest_a
        .db
        .query_feed(&rules, FilterCombination::All, &[], None, 50)
        .await
        .unwrap();
    assert!(
        feed_posts_after.len() >= 3,
        "nest_a should have >= 3 posts after follow-up poll, got {}",
        feed_posts_after.len()
    );

    // ── 16. Assert 2 specific-author contributors ──────────────────────
    let contributors_after = nest_a.db.list_contributors(&feed_id).await.unwrap();
    let specific_authors: Vec<_> = contributors_after
        .iter()
        .filter(|c| c.author_id.is_some() && c.author_id.as_ref().map(|a| a.len()) == Some(32))
        .collect();
    assert!(
        specific_authors.len() >= 2,
        "should have >= 2 specific-author contributors, got {}",
        specific_authors.len()
    );

    // ── 17. Shutdown poller and await with timeout ─────────────────────
    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), poller_handle)
        .await
        .expect("poller should shut down within 5 seconds")
        .expect("poller task should not panic");
}
