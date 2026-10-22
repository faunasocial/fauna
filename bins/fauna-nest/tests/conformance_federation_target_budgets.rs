//! **tier_3 — the open-federation kinds a stranger floods, bounded per TARGET**
//! (`deployment-threat-model.md` § Concern 1a, finding H1's audit sentence;
//! `federation.md` § Security → *Rate limiting is the primary DoS defense*:
//! "the per-nest bucket can never be the only dimension").
//!
//! Any fresh keypair passes the federation hello, so a bound keyed on the
//! origin `nest_id` bounds nothing. Each test here dials one nest from several
//! freshly-minted nest identities over the real channel, pushes past the bound
//! together, and shows the bound holds over all of them — while a legitimate
//! caller (the paired relay, a subscriber already seen, a re-delivered report,
//! an honest peer's next batch) still succeeds.
//!
//! The harness mirrors `conformance_federation_channel.rs`'s `start_nest`: a
//! listener served without connect info, so the per-source-IP meter (which
//! every request here would share on loopback) stays out of the way and only
//! the per-target bound under test can refuse.

mod common;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_core::data::Timestamp;
use fauna_core::encoding::EmbedAsBytes;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::subscription::FOLLOWERS_TIER;
use fauna_core::subscription::types::{FollowIntent, FollowIntentKind};
use fauna_nest::config::SubmissionPolicy;
use fauna_nest::db::CacheDb;
use fauna_nest::federation_channel::{FederationConnection, dial};
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::AppState;
use fauna_protocol::{RpcError, Value, decode_strict, encode_canonical};
use serde::Serialize;

fn to_value<T: Serialize>(t: &T) -> Value {
    decode_strict::<Value>(&encode_canonical(t).unwrap()).unwrap()
}

async fn start_nest() -> (String, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        // The default policy — the open federation these bounds must keep open.
        config: common::test_config(SubmissionPolicy::Open),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), state)
}

/// A fresh stranger: a new nest identity dialled into `target`. Each carries
/// its own per-nest budget (30 per kind per minute), so a test spreads its
/// flood over several — exactly what an attacker does.
async fn stranger(
    target_url: &str,
    target: &AppState,
) -> (Arc<AppState>, Arc<FederationConnection>) {
    let (_url, state) = start_nest().await;
    let target_id = hex::encode(target.nest_identity.public_key_bytes());
    let conn = dial(&state, target_url, &target_id).await.unwrap();
    (state, conn)
}

async fn call<T: Serialize>(
    conn: &FederationConnection,
    kind: &str,
    key: [u8; 16],
    req: &T,
) -> Result<Value, RpcError> {
    conn.dispatcher
        .request_raw(kind, key, to_value(req), None)
        .await
        .unwrap()
        .await_reply()
        .await
}

fn key(stranger: u8, i: usize) -> [u8; 16] {
    let mut k = [0u8; 16];
    k[0] = stranger;
    k[1..9].copy_from_slice(&(i as u64).to_be_bytes());
    k
}

fn is_rate_limited(e: &RpcError) -> bool {
    e.code.ends_with(".rate_limited")
}

// ── post.forward ────────────────────────────────────────────────────────

fn forward_request(
    author: &ActorKeypair,
    n: usize,
) -> (
    fauna_nest::federation_handlers::FedPostForwardRequest,
    [u8; 32],
) {
    let (wire_bytes, post_id) = common::signed_post_wire(author, &format!("flood {n}"));
    let wire: EmbedAsBytes = fauna_core::encoding::canonical_decode(&wire_bytes).unwrap();
    (
        fauna_nest::federation_handlers::FedPostForwardRequest {
            post_bytes: wire.bytes,
            post_envelope: wire.envelope,
            signer_auth: None,
        },
        post_id,
    )
}

/// Under the default `open` policy any peer may relay a validly-authored post
/// (`nest/private-mode.md` § Post Forwarding, step 2) — so posts relayed with
/// no `post_forward` pairing share ONE whole-nest budget, whatever nest or
/// author they claim. The real producer, a private nest relaying for an author
/// who granted it `post_forward`, never spends it.
#[tokio::test]
async fn unpaired_post_forwards_from_fresh_keypairs_share_one_nest_budget() {
    let (f_url, f_state) = start_nest().await;
    let budget = fauna_nest::bridge_rate_limit::POST_FORWARD_UNPAIRED_TARGET_MAX as usize;

    let mut stored = 0usize;
    let mut refused = 0usize;
    let mut n = 0usize;
    for s in 0u8..4 {
        let (_state, conn) = stranger(&f_url, &f_state).await;
        for _ in 0..(budget / 3 + 2) {
            // A fresh author per post — the author key is as free as the nest's.
            let (req, post_id) = forward_request(&ActorKeypair::generate(), n);
            match call(&conn, "fauna.federation.post.forward", key(s, n), &req).await {
                Ok(_) => {
                    assert!(f_state.db.post_exists(&post_id).await.unwrap());
                    stored += 1;
                }
                Err(e) if is_rate_limited(&e) => {
                    assert!(!f_state.db.post_exists(&post_id).await.unwrap());
                    refused += 1;
                }
                Err(e) => panic!("unexpected refusal {e:?}"),
            }
            n += 1;
        }
    }
    assert_eq!(
        stored, budget,
        "strangers together store at most the nest budget"
    );
    assert_eq!(
        refused,
        n - budget,
        "every forward past it is refused rate_limited"
    );

    // The real producer: its author's pairing names this relay with
    // `post_forward`, so it is not a stranger's forward and the spent budget
    // does not touch it.
    let (relay_state, conn) = stranger(&f_url, &f_state).await;
    let author = ActorKeypair::generate();
    f_state
        .db
        .store_pairing(
            &author.actor_id().0,
            &relay_state.nest_identity.public_key_bytes(),
            &[fauna_protocol::pair::capability::POST_FORWARD.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    let (req, post_id) = forward_request(&author, 9_999);
    call(&conn, "fauna.federation.post.forward", key(9, 0), &req)
        .await
        .expect("a paired relay's forward is admitted past the spent budget");
    assert!(f_state.db.post_exists(&post_id).await.unwrap());
}

// ── follow.subscribe ────────────────────────────────────────────────────

fn intent_request(
    subscriber: &ActorKeypair,
    author: [u8; 32],
    home: [u8; 32],
    kind: FollowIntentKind,
) -> fauna_nest::follow_federation::FedFollowIntentRequest {
    intent_request_at(
        subscriber,
        author,
        home,
        kind,
        Timestamp::now().0 - 5_000_000,
    )
}

fn intent_request_at(
    subscriber: &ActorKeypair,
    author: [u8; 32],
    home: [u8; 32],
    kind: FollowIntentKind,
    created_at: u64,
) -> fauna_nest::follow_federation::FedFollowIntentRequest {
    let intent = FollowIntent {
        subscriber: subscriber.actor_id(),
        author: ActorId(author),
        tier: FOLLOWERS_TIER.to_string(),
        kind,
        home_nest_id: home,
        created_at: Timestamp(created_at),
    };
    let (bytes, env) = fauna_core::encoding::sign_envelope(subscriber, &intent).unwrap();
    let wire = EmbedAsBytes::from_signed(bytes, env);
    fauna_nest::follow_federation::FedFollowIntentRequest {
        intent_bytes: wire.bytes,
        intent_envelope: wire.envelope,
        signer_auth: None,
        mlkem_encaps_key: None,
        origin_nest_url: None,
    }
}

async fn pending_requests(state: &AppState, author: &[u8; 32]) -> usize {
    state
        .db
        .list_subscribe_requests(author)
        .await
        .unwrap()
        .len()
}

/// Each fresh subscriber keypair a stranger mints would otherwise queue one
/// request for the author to approve. Intents from subscribers this nest has
/// never seen spend the author's budget, summed over every origin — a
/// subscribe and an unsubscribe alike, since each writes a seen-stamp row. A
/// subscriber already seen is free, and another author's budget is untouched.
#[tokio::test]
async fn follow_intents_from_fresh_keypairs_cannot_flood_an_authors_queue() {
    let (f_url, f_state) = start_nest().await;
    let budget = fauna_nest::bridge_rate_limit::FOLLOW_INTENT_TARGET_MAX as usize;
    let author = ActorKeypair::generate().actor_id().0;
    f_state
        .db
        .create_user(&author, "free", "author")
        .await
        .unwrap();

    let mut admitted = Vec::new();
    let mut refused = 0usize;
    let mut n = 0usize;
    let mut strangers = Vec::new();
    for s in 0u8..3 {
        let (h_state, conn) = stranger(&f_url, &f_state).await;
        let home = h_state.nest_identity.public_key_bytes();
        for _ in 0..(budget / 2 + 3) {
            let sub = ActorKeypair::generate();
            // Every third stranger sends a leave for a follow it never had.
            let kind = if n % 3 == 2 {
                FollowIntentKind::Unsubscribe
            } else {
                FollowIntentKind::Subscribe
            };
            let wire_kind = match kind {
                FollowIntentKind::Subscribe => "fauna.federation.follow.subscribe",
                _ => "fauna.federation.follow.unsubscribe",
            };
            let req = intent_request(&sub, author, home, kind);
            match call(&conn, wire_kind, key(s, n), &req).await {
                Err(e) if is_rate_limited(&e) => refused += 1,
                // A leave with nothing to leave is answered `not_subscribed`
                // once admitted — it still spent the budget.
                _ => admitted.push((s, sub, kind)),
            }
            n += 1;
        }
        strangers.push((home, conn));
    }
    assert_eq!(
        admitted.len(),
        budget,
        "strangers' intents together spend at most the author's budget"
    );
    assert_eq!(refused, n - budget);
    let subscribes = admitted
        .iter()
        .filter(|(_, _, k)| matches!(k, FollowIntentKind::Subscribe))
        .count();
    assert_eq!(pending_requests(&f_state, &author).await, subscribes);

    // A subscriber already seen re-sends its subscribe past the spent budget:
    // idempotent, and free.
    let (s, sub, _) = admitted
        .iter()
        .find(|(_, _, k)| matches!(k, FollowIntentKind::Subscribe))
        .unwrap();
    let (home, conn) = &strangers[*s as usize];
    call(
        conn,
        "fauna.federation.follow.subscribe",
        key(*s, 10_000),
        &intent_request(sub, author, *home, FollowIntentKind::Subscribe),
    )
    .await
    .expect("a subscriber already seen never spends the budget");
    assert_eq!(pending_requests(&f_state, &author).await, subscribes);

    // Another author's budget is its own.
    let other_author = ActorKeypair::generate().actor_id().0;
    f_state
        .db
        .create_user(&other_author, "free", "other")
        .await
        .unwrap();
    let (h_state, conn) = stranger(&f_url, &f_state).await;
    call(
        &conn,
        "fauna.federation.follow.subscribe",
        key(7, 0),
        &intent_request(
            &ActorKeypair::generate(),
            other_author,
            h_state.nest_identity.public_key_bytes(),
            FollowIntentKind::Subscribe,
        ),
    )
    .await
    .expect("another author's budget is untouched");
    assert_eq!(pending_requests(&f_state, &other_author).await, 1);
}

/// The window bounds the rate; the cap bounds the total an author who never
/// approves can be left holding. Past it, a new remote subscriber is refused
/// until the author drains the queue.
#[tokio::test]
async fn an_authors_pending_remote_requests_are_capped() {
    let (f_url, f_state) = start_nest().await;
    let cap = fauna_nest::follow_federation::MAX_REMOTE_PENDING_SUBSCRIBE_REQUESTS;
    let author = ActorKeypair::generate().actor_id().0;
    f_state
        .db
        .create_user(&author, "free", "author")
        .await
        .unwrap();
    // Seed the queue to the cap, as weeks of slow arrivals would.
    let (h_state, conn) = stranger(&f_url, &f_state).await;
    let home = h_state.nest_identity.public_key_bytes();
    call(
        &conn,
        "fauna.federation.follow.subscribe",
        key(0, 0),
        &intent_request(
            &ActorKeypair::generate(),
            author,
            home,
            FollowIntentKind::Subscribe,
        ),
    )
    .await
    .expect("the first remote follow is queued");
    for i in 1..cap {
        let mut subscriber = [0u8; 32];
        subscriber[..8].copy_from_slice(&(i as u64).to_be_bytes());
        let id = f_state
            .db
            .insert_subscribe_request(&author, &subscriber, FOLLOWERS_TIER, "subscribe", None)
            .await
            .unwrap();
        f_state
            .db
            .set_subscribe_request_home(id, &home, None)
            .await
            .unwrap();
    }
    let err = call(
        &conn,
        "fauna.federation.follow.subscribe",
        key(0, 1),
        &intent_request(
            &ActorKeypair::generate(),
            author,
            home,
            FollowIntentKind::Subscribe,
        ),
    )
    .await
    .expect_err("a new remote subscriber past the cap is refused");
    assert!(is_rate_limited(&err), "{err:?}");
    assert_eq!(pending_requests(&f_state, &author).await, cap);
}

/// A stranger's leave writes a seen-stamp though it ends nothing, so "seen" is
/// not "holds a request": a subscribe that follows the leave, newer than it,
/// still meets the cap when the subscriber holds no pending or approved row.
#[tokio::test]
async fn a_leave_primed_stranger_subscribe_still_meets_the_pending_cap() {
    let (f_url, f_state) = start_nest().await;
    let cap = fauna_nest::follow_federation::MAX_REMOTE_PENDING_SUBSCRIBE_REQUESTS;
    let author = ActorKeypair::generate().actor_id().0;
    f_state
        .db
        .create_user(&author, "free", "author")
        .await
        .unwrap();
    let (h_state, conn) = stranger(&f_url, &f_state).await;
    let home = h_state.nest_identity.public_key_bytes();
    // The first real subscribe provisions the followers tier the seeds need.
    call(
        &conn,
        "fauna.federation.follow.subscribe",
        key(0, 2),
        &intent_request(
            &ActorKeypair::generate(),
            author,
            home,
            FollowIntentKind::Subscribe,
        ),
    )
    .await
    .expect("the first remote follow is queued");
    for i in 1..cap {
        let mut subscriber = [0u8; 32];
        subscriber[..8].copy_from_slice(&(i as u64).to_be_bytes());
        let id = f_state
            .db
            .insert_subscribe_request(&author, &subscriber, FOLLOWERS_TIER, "subscribe", None)
            .await
            .unwrap();
        f_state
            .db
            .set_subscribe_request_home(id, &home, None)
            .await
            .unwrap();
    }
    let primed = ActorKeypair::generate();
    let base = Timestamp::now().0 - 5_000_000;
    // The leave is a stranger's intent: it spends the budget and stamps, no cap.
    let _ = call(
        &conn,
        "fauna.federation.follow.unsubscribe",
        key(0, 0),
        &intent_request_at(&primed, author, home, FollowIntentKind::Unsubscribe, base),
    )
    .await;
    let err = call(
        &conn,
        "fauna.federation.follow.subscribe",
        key(0, 1),
        &intent_request_at(
            &primed,
            author,
            home,
            FollowIntentKind::Subscribe,
            base + 1_000_000,
        ),
    )
    .await
    .expect_err("a leave-primed subscriber holding no request is refused at the cap");
    assert!(is_rate_limited(&err), "{err:?}");
    assert_eq!(pending_requests(&f_state, &author).await, cap);
}

// ── abuse_report.deliver ────────────────────────────────────────────────

fn report(
    subject: [u8; 32],
    report_ref: String,
) -> fauna_protocol::moderation::AbuseReportDeliverRequest {
    fauna_protocol::moderation::AbuseReportDeliverRequest {
        report_ref,
        subject: fauna_protocol::moderation::AbuseReportSubject::Actor {
            actor_id: hex::encode(subject),
        },
        reason: fauna_protocol::moderation::AbuseReportReason::Harassment,
        note: None,
        excerpt: None,
        origin_nest_id: "stranger.example".into(),
        origin_nest_url: None,
        subject_actor: None,
        extra: Default::default(),
    }
}

/// The only dedupe is `(origin, report_ref)` and both are the caller's to
/// choose, so each stored report and each ring of the admin's doorbell is
/// bounded twice: per hosted account reported, and over the whole nest — the
/// admin's queue. A re-delivery of a report already stored stays free, so the
/// sender's retry queue is never refused for what it already delivered.
#[tokio::test]
async fn forwarded_abuse_reports_from_fresh_keypairs_are_bounded_per_subject_and_nest() {
    let (f_url, f_state) = start_nest().await;
    let per_subject = fauna_nest::bridge_rate_limit::ABUSE_REPORT_SUBJECT_TARGET_MAX as usize;
    let per_nest = fauna_nest::bridge_rate_limit::ABUSE_REPORT_NEST_TARGET_MAX as usize;
    let kind = fauna_nest::abuse_report_federation::DELIVER_KIND;

    // One victim: strangers bury them, but only up to the per-subject budget.
    let victim = ActorKeypair::generate().actor_id().0;
    f_state
        .db
        .create_user(&victim, "free", "victim")
        .await
        .unwrap();
    let mut n = 0usize;
    let mut stored = 0usize;
    for s in 0u8..2 {
        let (_state, conn) = stranger(&f_url, &f_state).await;
        for _ in 0..per_subject {
            let r = report(victim, format!("ref-{n}"));
            match call(&conn, kind, key(s, n), &r).await {
                Ok(_) => stored += 1,
                Err(e) if is_rate_limited(&e) => {}
                Err(e) => panic!("unexpected refusal {e:?}"),
            }
            n += 1;
        }
    }
    assert_eq!(stored, per_subject, "one account takes at most its budget");

    // Many subjects: the nest-wide budget caps the admin's queue.
    let mut subjects = Vec::new();
    for i in 0..(per_nest / per_subject + 2) {
        let a = ActorKeypair::generate().actor_id().0;
        f_state
            .db
            .create_user(&a, "free", &format!("s{i}"))
            .await
            .unwrap();
        subjects.push(a);
    }
    let mut s = 2u8;
    let mut conn = stranger(&f_url, &f_state).await.1;
    let mut on_conn = 0usize;
    for subject in &subjects {
        for _ in 0..per_subject {
            if on_conn == 25 {
                s += 1;
                conn = stranger(&f_url, &f_state).await.1;
                on_conn = 0;
            }
            match call(
                &conn,
                kind,
                key(s, n),
                &report(*subject, format!("ref-{n}")),
            )
            .await
            {
                Ok(_) => stored += 1,
                Err(e) if is_rate_limited(&e) => {}
                Err(e) => panic!("unexpected refusal {e:?}"),
            }
            n += 1;
            on_conn += 1;
        }
    }
    assert_eq!(
        stored, per_nest,
        "the admin's queue takes at most the nest budget"
    );
    assert_eq!(
        f_state.db.list_open_abuse_reports().await.unwrap().len(),
        per_nest
    );
}

/// A re-delivery of a stored `(origin, ref)` — what the sender's durable queue
/// does after a lost ack — is answered past a spent budget and stores nothing.
#[tokio::test]
async fn a_redelivered_abuse_report_is_free() {
    let (f_url, f_state) = start_nest().await;
    let per_subject = fauna_nest::bridge_rate_limit::ABUSE_REPORT_SUBJECT_TARGET_MAX as usize;
    let kind = fauna_nest::abuse_report_federation::DELIVER_KIND;
    let victim = ActorKeypair::generate().actor_id().0;
    f_state
        .db
        .create_user(&victim, "free", "victim")
        .await
        .unwrap();
    let (_state, conn) = stranger(&f_url, &f_state).await;
    for i in 0..per_subject {
        call(&conn, kind, key(0, i), &report(victim, format!("ref-{i}")))
            .await
            .expect("within the budget");
    }
    let err = call(&conn, kind, key(0, 100), &report(victim, "ref-new".into()))
        .await
        .expect_err("the budget is spent");
    assert!(is_rate_limited(&err));
    call(&conn, kind, key(0, 101), &report(victim, "ref-0".into()))
        .await
        .expect("a re-delivery of a stored report is acked");
    assert_eq!(
        f_state.db.list_open_abuse_reports().await.unwrap().len(),
        per_subject
    );
}

// ── reports.exchange / trends.exchange ──────────────────────────────────

/// Each peer's rows are capped, but the peer id is self-minted, so the TABLE
/// is capped too: fresh identities pushing full batches never grow it past
/// the cap, the oldest rows leaving first, and every batch is still imported.
#[tokio::test]
async fn report_and_trend_imports_from_fresh_keypairs_are_capped_table_wide() {
    use fauna_nest::federation_handlers::{
        FedReportEntry, FedReportsExchangeRequest, FedTrendEntry, FedTrendsExchangeRequest,
    };
    let (f_url, f_state) = start_nest().await;
    let report_cap = fauna_nest::db::reports::MAX_PEER_REPORT_ROWS_TOTAL as u64;
    let trend_cap = fauna_nest::db::trends::MAX_PEER_TREND_ROWS_TOTAL as u64;

    // Seed both tables to their caps, as many earlier peers would have.
    for i in 0..report_cap {
        let mut peer = [0u8; 32];
        peer[..8].copy_from_slice(&(i / 64).to_be_bytes());
        let mut hash = [0u8; 32];
        hash[..8].copy_from_slice(&i.to_be_bytes());
        f_state
            .db
            .upsert_peer_report(&peer, &hash, "report:spam", 100, 1)
            .await
            .unwrap();
    }
    for i in 0..trend_cap {
        let mut peer = [0u8; 32];
        peer[..8].copy_from_slice(&(i / 64).to_be_bytes());
        let mut id = [0u8; 32];
        id[..8].copy_from_slice(&i.to_be_bytes());
        f_state
            .db
            .upsert_peer_trend(&peer, &id, 100, 5, 1, 1)
            .await
            .unwrap();
    }

    // Two strangers, each pushing a full batch of each.
    for s in 0u8..2 {
        let (_state, conn) = stranger(&f_url, &f_state).await;
        let reports = FedReportsExchangeRequest {
            epoch: 1,
            entries: (0..fauna_nest::db::reports::MAX_REPORT_EXCHANGE_ENTRIES)
                .map(|i| FedReportEntry {
                    content_hash: hex::encode(
                        [s.wrapping_add(0xA0), (i >> 8) as u8, i as u8, 0xFF].repeat(8),
                    ),
                    factor: "report:spam".into(),
                    count: 100,
                })
                .collect(),
        };
        call(
            &conn,
            "fauna.federation.reports.exchange",
            key(s, 0),
            &reports,
        )
        .await
        .expect("an import batch lands");
        let trends = FedTrendsExchangeRequest {
            epoch: 1,
            entries: (0..fauna_nest::db::trends::MAX_TREND_EXCHANGE_ENTRIES)
                .map(|i| FedTrendEntry {
                    content_id: hex::encode(
                        [s.wrapping_add(0xB0), (i >> 8) as u8, i as u8, 0xFF].repeat(8),
                    ),
                    score_pm: 100,
                    engager_count: 5,
                })
                .collect(),
        };
        call(
            &conn,
            "fauna.federation.trends.exchange",
            key(s, 1),
            &trends,
        )
        .await
        .expect("an import batch lands");
    }

    assert_eq!(f_state.db.count_peer_reports().await.unwrap(), report_cap);
    assert_eq!(f_state.db.count_peer_trends().await.unwrap(), trend_cap);
}
