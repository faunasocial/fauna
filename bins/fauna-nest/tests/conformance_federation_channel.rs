//! **Spec Y2 slice 4 (hub Track D) — tier_3** federation WS-RPC channel:
//! handshake + capability-negotiation, two in-process nests over loopback `ws://`.
//!
//! This is the acceptance test for the channel *carrier* (the slice-4 capstone
//! migrates the 13 residue kinds onto it and proves a cross-nest MLS group forms
//! over the channel; since slice 5 retired the HTTP interim, the channel is the
//! sole Fauna↔Fauna carrier). Here we prove the channel itself:
//!
//! - a real WS upgrade on `GET /api/v1/federation/ws` + the mutual
//!   `fauna.federation.hello` handshake completes between two distinct nest
//!   identities (both sides verify each other);
//! - the initiator proves it reached the *intended* nest (a wrong
//!   `expected_peer_nest_id` is rejected);
//! - a non-`fauna.federation.*` kind over the channel is `unauthenticated`
//!   (the kind allowlist boundary);
//! - a peer with no `/api/v1/federation/ws` route → `ChannelUnsupported`
//!   (capability negotiation; no fallback since slice 5).

mod common;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::scoring::{FilterCombination, FilterRule};
use fauna_mls::engine::MlsEngine;
use fauna_nest::db::CacheDb;
use fauna_nest::db::channels::RebindPower;
use fauna_nest::federation_channel::{DialError, dial};
use fauna_nest::federation_handlers::{
    FedChannelFetchReply, FedChannelFetchRequest, FedInboxDeliverReply, FedInboxDeliverRequest,
    FedKeypackageFetchReply, FedKeypackageFetchRequest, FedMlsAckReply, FedMlsAckRequest,
    FedMlsPullReply, FedMlsPullRequest, FedPostGetReply, FedPostGetRequest, FedSyncPullReply,
    FedSyncPullRequest, FedSyncPushEntry, FedSyncPushReply, FedSyncPushRequest,
};
use fauna_nest::feed_routes::RemoteQueryRequest;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::AppState;
use fauna_protocol::conversations::ChannelSendRequest;
use fauna_protocol::inbox::{InboxSendReply, InboxSendRequest};
use fauna_protocol::{Value, decode_strict, encode_canonical};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Encode a typed payload into an L3 `Value` for `request_raw`.
fn to_value<T: Serialize>(t: &T) -> Value {
    decode_strict::<Value>(&encode_canonical(t).unwrap()).unwrap()
}

/// Decode a typed reply payload from an L3 `Value`.
fn from_value<T: DeserializeOwned>(v: &Value) -> T {
    decode_strict::<T>(&encode_canonical(v).unwrap()).unwrap()
}

/// Spin a real in-process nest (its full router, incl. `/api/v1/federation/ws`)
/// on a loopback socket with a distinct nest identity. Mirrors the slice-2
/// capstone's `start_nest`.
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
        // Populate the federation kind allowlist + handlers (slice 4); the bare
        // `for_test` router is empty. This is what lets a peer's
        // `fauna.federation.*` request actually serve instead of being rejected.
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        // Register the pre-identity discovery handlers (`fauna.nest.info`, …) on
        // the anonymous WS surface; `for_test`'s `rpc_router` is empty. The
        // slice-4 capstone's pool resolves a peer's `nest_id` from its URL via
        // the anon `fauna.nest.info` kind before dialing the channel.
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
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

fn nest_id_hex(state: &AppState) -> String {
    hex::encode(state.nest_identity.public_key_bytes())
}

/// H dials F's federation channel and the mutual handshake completes over a real
/// WS upgrade; H records F's `nest_id`. (F's verify of H's signature is implicit:
/// it only replies — which H then verifies — after verifying H's hello.)
#[tokio::test]
async fn channel_handshake_succeeds_between_two_nests() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);

    let conn = dial(&h_state, &f_url, &f_nest_id)
        .await
        .expect("federation channel established");
    assert_eq!(
        hex::encode(conn.peer_nest_id),
        f_nest_id,
        "H must record F's nest_id from the verified handshake reply"
    );
}

/// The initiator proves it reached the nest it intended: dialing F but expecting
/// a *different* `nest_id` fails the reply verification (§4.B — defeats a
/// DNS-hijack/MITM substituting a hostile nest even on a self-signed cert). It is
/// a `Handshake` failure, NOT `ChannelUnsupported` (F does offer the channel).
#[tokio::test]
async fn dial_rejects_reaching_an_unexpected_nest() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, _f_state) = start_nest().await;
    // We expect some *other* nest's id, not the one F actually serves.
    let wrong_nest_id = hex::encode([7u8; 32]);

    // `Arc<FederationConnection>` isn't `Debug`, so map the Ok away for the panic.
    match dial(&h_state, &f_url, &wrong_nest_id).await {
        Err(DialError::Handshake(_)) => {}
        other => panic!(
            "expected Handshake failure, got {:?}",
            other.map(|_| "Ok(channel established)")
        ),
    }
}

/// After a verified handshake, a client-actor kind over the federation channel is
/// rejected `unauthenticated` (slice-3 contract; the connection stays open). In
/// slice 4 the kind allowlist + per-nest throttle + the 13 `fauna.federation.*`
/// handlers replace the blanket reject.
#[tokio::test]
async fn non_federation_kind_over_channel_is_unauthenticated() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let call = conn
        .dispatcher
        .request_raw("fauna.conversations.send", [2u8; 16], Value::Null, None)
        .await
        .expect("send request over the channel");
    let err = call
        .await_reply()
        .await
        .expect_err("a client-actor kind over the federation channel must be rejected");
    assert_eq!(err.code, "fauna.protocol.unauthenticated");
}

/// A peer that does not offer the channel (no `/api/v1/federation/ws` route → 404
/// on the upgrade) is surfaced as `ChannelUnsupported`. Since slice 5 retired the
/// HTTP interim this is a hard error — the pool has no fallback carrier (§4.F).
/// Modelled with a bare router that 404s every path.
#[tokio::test]
async fn dial_to_peer_without_channel_route_is_unsupported() {
    let app = axum::Router::new(); // no routes → every path 404s
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    let (_h_url, h_state) = start_nest().await;
    let unreached_nest_id = hex::encode([1u8; 32]); // never used — connect 404s first

    match dial(&h_state, &format!("http://{addr}"), &unreached_nest_id).await {
        Err(DialError::ChannelUnsupported) => {}
        other => panic!(
            "expected ChannelUnsupported (a hard error — no HTTP fallback since slice 5), got {:?}",
            other.map(|_| "Ok(channel established)")
        ),
    }
}

/// **Slice 4 serving capstone (this session):** after the handshake, H originates
/// `fauna.federation.keypackage.fetch` over the channel and F's `FederationRouter`
/// serves it — running the **same** `take_key_package` DB op the HTTP twin does —
/// returning Bob's key package over the wire, no HTTP federation call. The second
/// fetch consumes the one-time pool (now empty) → `None`, proving the destructive
/// take runs over the channel exactly as on the HTTP interim.
#[tokio::test]
async fn keypackage_fetch_over_channel_serves_the_db_op() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);

    // Seed a one-time key package for Bob on F (the same write the upload path does).
    let bob = [0xBBu8; 32];
    let kp_bytes = b"bob-key-package-bytes".to_vec();
    f_state
        .db
        // expires_at must be a positive i64 once stored (`u64::MAX as i64` = -1,
        // which the lazy `expires_at < now` cleanup would wipe immediately).
        .put_key_package("bob-kp-1", &bob, &kp_bytes, 0, 9_000_000_000)
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    let req = FedKeypackageFetchRequest {
        target_actor_id: hex::encode(bob),
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.keypackage.fetch",
            [1u8; 16],
            to_value(&req),
            None,
        )
        .await
        .expect("fetch request over the channel");
    let reply: FedKeypackageFetchReply = from_value(&call.await_reply().await.expect("ok reply"));
    assert_eq!(
        reply.key_package.as_deref(),
        Some(kp_bytes.as_slice()),
        "the channel must serve the same take_key_package result as the HTTP twin"
    );

    // Second fetch (distinct idempotency_key so it re-dispatches, not replays):
    // the one-time pool is now drained and there is no last-resort row → None.
    let call2 = conn
        .dispatcher
        .request_raw(
            "fauna.federation.keypackage.fetch",
            [2u8; 16],
            to_value(&req),
            None,
        )
        .await
        .unwrap();
    let reply2: FedKeypackageFetchReply = from_value(&call2.await_reply().await.expect("ok reply"));
    assert!(
        reply2.key_package.is_none(),
        "the one-time KP was consumed by the first fetch (destructive take over the channel)"
    );
}

/// A repeated `idempotency_key` replays the cached Reply over the channel — the
/// op runs once. (The serving side caches `(payload, ok)` and rebuilds the Reply
/// with the retry's fresh correlation_id, §4.B.)
#[tokio::test]
async fn keypackage_fetch_over_channel_replays_on_repeated_idempotency_key() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);

    let bob = [0xCCu8; 32];
    f_state
        .db
        .put_key_package("bob-kp-1", &bob, b"one-time-kp", 0, 9_000_000_000)
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let req = FedKeypackageFetchRequest {
        target_actor_id: hex::encode(bob),
    };

    // Same idempotency_key twice: the first consumes the KP; the second must
    // replay the cached reply (NOT a second take, which would return None).
    let key = [7u8; 16];
    let first: FedKeypackageFetchReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.keypackage.fetch",
                key,
                to_value(&req),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("ok"),
    );
    let second: FedKeypackageFetchReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.keypackage.fetch",
                key,
                to_value(&req),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("ok"),
    );
    assert_eq!(
        first.key_package, second.key_package,
        "a repeated idempotency_key must replay the cached reply, not re-run take_key_package"
    );
    assert!(first.key_package.is_some(), "first fetch returns the KP");
}

/// The per-originating-nest throttle (keyed on the verified peer `nest_id`) trips
/// after the default 30-event/60s budget for a given kind, returning
/// `rate_limited` while the connection stays open (§4.C). Distinct
/// idempotency_keys so each request re-dispatches (not an idempotency replay).
#[tokio::test]
async fn federation_kind_is_throttled_per_nest() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    // No KP seeded — the throttle gate runs before the handler, so the result is
    // `Ok(key_package: None)` until the budget trips; we only care about the trip.
    let req = FedKeypackageFetchRequest {
        target_actor_id: hex::encode([0xDDu8; 32]),
    };

    let mut saw_rate_limited = false;
    for i in 0u16..40 {
        let mut key = [0u8; 16];
        key[0] = i as u8;
        key[1] = (i >> 8) as u8;
        let call = conn
            .dispatcher
            .request_raw(
                "fauna.federation.keypackage.fetch",
                key,
                to_value(&req),
                None,
            )
            .await
            .unwrap();
        if let Err(e) = call.await_reply().await
            && e.code == "fauna.protocol.rate_limited"
        {
            saw_rate_limited = true;
            break;
        }
    }
    // Getting a `rate_limited` *reply* (rather than `disconnected`) is itself the
    // proof that the throttle trips with the connection left open (§4.C).
    assert!(
        saw_rate_limited,
        "the per-nest throttle must trip within the 30/60s default budget"
    );
}

// ── the other §4.E handlers ──────────────────────────────────────────────────
//
// Each proves a `fauna.federation.*` kind serves its DB op over the channel —
// the same op its HTTP twin runs, minus the per-request signature (channel-authed
// once at handshake). Together with the conversations pair above, every
// surviving residue row is exercised over the channel (the reputation pair left
// with the federation reputation leg, 2026-10-02).

/// One stored copy of a campaign message carrying `report_hash` and the
/// perimeter's bus `scores` — the local item a peer's report entry joins to.
fn report_campaign_mail(
    report_hash: [u8; 32],
    scores: Vec<fauna_core::scoring::ScoreEntry>,
) -> fauna_nest::db::bridge_routing::InboundMailFields {
    fauna_nest::db::bridge_routing::InboundMailFields {
        actor_id: [0x77u8; 32],
        timestamp: 1_700_000_000,
        ciphertext_size: 4,
        encrypted_body: fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"body".to_vec(),
        ),
        encrypted_index_hint: fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"hint".to_vec(),
        ),
        sender_domain: "example.com".into(),
        spf: "none".into(),
        dkim: "none".into(),
        dmarc: "none".into(),
        dmarc_policy: "none".into(),
        arc: "none".into(),
        spam_score: 0,
        spam_disposition: "accept".into(),
        is_own_submission: false,
        scores,
        report_hash: report_hash.to_vec(),
    }
}

/// **Distributed report sharing (report-sharing.md § Federation exchange).**
/// The two invariants that make the exchange safe, proven over the real
/// channel: a peer's claimed count NEVER scales local consensus (any number
/// of peers, any magnitude, collapses into one flat corroboration bucket),
/// and the export carries only LOCAL k-gate-passed counts (no laundering, no
/// below-k disclosure).
#[tokio::test]
async fn reports_exchange_is_non_scaling_and_export_never_launders() {
    use fauna_nest::db::reports::{ReportKey, capture_report};
    use fauna_nest::federation_handlers::{
        FedReportEntry, FedReportsExchangeReply, FedReportsExchangeRequest, FedReportsExportReply,
    };

    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    // F holds one stored copy of the campaign message + 3 opted-in local
    // reporters → the local consensus bus row is 200‰.
    let hash = [0x42u8; 32];
    let fields = report_campaign_mail(hash, vec![]);
    let message_id = f_state
        .db
        .insert_inbound_mail(&f_state.mail_segments, &fields)
        .await
        .unwrap()
        .message_id;
    let key = ReportKey {
        content_hash: hash,
        factor: "report:spam".into(),
        content_kind: "mail".into(),
    };
    for reporter in [[0x01u8; 32], [0x02; 32], [0x03; 32]] {
        f_state.db.set_share_reports(&reporter, true).await.unwrap();
        capture_report(&f_state.db, &reporter, &key, true)
            .await
            .unwrap();
    }
    let report_score = |db: Arc<CacheDb>, id: [u8; 32]| async move {
        db.get_content_scores(&id)
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.factor == "report:spam")
            .map(|e| e.score)
    };
    assert_eq!(
        report_score(f_state.db.clone(), message_id).await,
        Some(200),
        "k=3 local consensus"
    );

    // H pushes an absurd claimed count for the same hash, plus an entry for
    // content F has never seen, plus a below-k entry (skipped, not imported).
    let unknown_hash = [0x43u8; 32];
    let exchange = FedReportsExchangeRequest {
        epoch: 1,
        entries: vec![
            FedReportEntry {
                content_hash: hex::encode(hash),
                factor: "report:spam".into(),
                count: 100_000,
            },
            FedReportEntry {
                content_hash: hex::encode(unknown_hash),
                factor: "report:spam".into(),
                count: 100,
            },
            FedReportEntry {
                content_hash: hex::encode([0x44u8; 32]),
                factor: "report:spam".into(),
                count: 2, // below the exporter-side k floor → not corroboration
            },
        ],
    };
    let imported: FedReportsExchangeReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.reports.exchange",
                [3u8; 16],
                to_value(&exchange),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("exchange ok"),
    );
    assert_eq!(imported.imported, 2, "below-k entry skipped");

    // Non-scaling: 200 (local) + flat 100 (ONE peer bucket) = 300 — the
    // claimed 100 000 bought nothing beyond presence.
    assert_eq!(
        report_score(f_state.db.clone(), message_id).await,
        Some(300),
        "peer corroborates, never scales"
    );

    // Export: only F's LOCAL gate-passed aggregate, count 3 — never
    // local+peer, never the peer-only unknown hash (no laundering).
    let export: FedReportsExportReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.reports.export",
                [4u8; 16],
                Value::Null,
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("export ok"),
    );
    assert_eq!(export.entries.len(), 1);
    assert_eq!(export.entries[0].content_hash, hex::encode(hash));
    assert_eq!(
        export.entries[0].count, 3,
        "local count only — no laundering"
    );
}

/// **The exchange writes only the exchanged factor family
/// (report-sharing.md § Federation exchange).** A peer entry naming any factor
/// outside `report:spam` + `signal:{watch-complete,skip}` — here the
/// perimeter's `clamav` verdict — imports nothing and leaves the local row
/// intact; a `report:spam` entry in the same batch still lands.
#[tokio::test]
async fn reports_exchange_never_overwrites_a_non_report_factor() {
    use fauna_core::scoring::{ScoreEntry, TIER_ADMIN, factor, scorer_version};
    use fauna_nest::federation_handlers::{
        FedReportEntry, FedReportsExchangeReply, FedReportsExchangeRequest,
    };

    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    // F stores the attacker's own mail with the perimeter's "infected" verdict.
    let hash = [0x51u8; 32];
    let clamav = ScoreEntry {
        factor: factor::CLAMAV.into(),
        score: 1000,
        tier: TIER_ADMIN,
        scorer_version: scorer_version::CLAMAV,
    };
    let message_id = f_state
        .db
        .insert_inbound_mail(
            &f_state.mail_segments,
            &report_campaign_mail(hash, vec![clamav.clone()]),
        )
        .await
        .unwrap()
        .message_id;
    // The ingest handler lands the floor's scores on the bus separately.
    f_state
        .db
        .insert_content_scores(
            &message_id,
            "mail",
            Some(&[0x77u8; 32]),
            1_700_000_000,
            std::slice::from_ref(&clamav),
        )
        .await
        .unwrap();

    // H names `clamav` (and a labeler factor) through the reports door,
    // alongside one legitimate `report:spam` entry.
    let entry = |factor: &str| FedReportEntry {
        content_hash: hex::encode(hash),
        factor: factor.into(),
        count: 100,
    };
    let exchange = FedReportsExchangeRequest {
        epoch: 1,
        entries: vec![
            entry(factor::CLAMAV),
            entry("labeler:someone"),
            entry(factor::REPORT_SPAM),
        ],
    };
    let imported: FedReportsExchangeReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.reports.exchange",
                [5u8; 16],
                to_value(&exchange),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("exchange ok — a foreign factor is skipped, not an error"),
    );
    let scores = f_state.db.get_content_scores(&message_id).await.unwrap();
    let row = |f: &str| scores.iter().find(|e| e.factor == f).cloned();
    assert_eq!(
        row(factor::CLAMAV).map(|e| (e.score, e.scorer_version)),
        Some((clamav.score, clamav.scorer_version)),
        "the perimeter verdict survives a peer naming its factor"
    );
    assert_eq!(
        row("labeler:someone").map(|e| e.score),
        None,
        "no foreign factor row minted"
    );
    assert_eq!(
        row(factor::REPORT_SPAM).map(|e| e.score),
        Some(fauna_core::scoring::reports::PEER_CORROBORATION_PM),
        "report:spam still imports as the flat peer bucket"
    );
    assert_eq!(imported.imported, 1, "only the report:spam entry imports");
}

/// **Distributed trending (trending.md § Federation exchange).** The two
/// invariants that make the trend exchange safe, proven over the real channel:
/// a peer's claimed magnitude NEVER scales local consensus (presence only — the
/// distinct-peer ramp, never the claimed `score_pm`/`engager_count`), and the
/// export carries only the exporter's UN-composed `local_pm` (no laundering, no
/// below-k disclosure). Mirrors `reports_exchange_is_non_scaling_and_export_never_launders`.
#[tokio::test]
async fn trends_exchange_is_presence_only_and_export_never_launders() {
    use fauna_nest::federation_handlers::{
        FedTrendEntry, FedTrendsExchangeReply, FedTrendsExchangeRequest, FedTrendsExportReply,
    };

    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    // F ingests one PUBLIC post (signed → `content_meta.gated_tier IS NULL`) via
    // the real `put_post` path, then 3 distinct fresh likes → its local trending
    // row rises. v ≈ 3 → local_pm = round(3000/23) = 130 (decay over the test's
    // wall-clock is negligible against the 6 h half-life).
    let kp = ActorKeypair::generate();
    let post = fauna_core::data::Post {
        author: kp.actor_id(),
        created_at: fauna_core::data::Timestamp(1_700_000_000),
        body: fauna_core::data::PostBody::Text {
            content: "hot post".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let cid = fauna_core::encoding::compute_post_id(&post).unwrap();
    let digest: [u8; 32] = cid.as_bytes()[4..].try_into().unwrap();
    let bytes = fauna_core::encoding::sign_and_pack(&kp, &post).unwrap();
    f_state.db.put_post(&digest, &bytes, None).await.unwrap();
    let now_us = fauna_core::data::Timestamp::now().as_i64();
    for actor in [0x01u8, 0x02, 0x03] {
        let mut event_id = digest;
        event_id[0] = 0xE0;
        event_id[1] = actor;
        f_state
            .db
            .insert_engagement_event(&event_id, &digest, Some(&[actor; 32]), "like", None, now_us)
            .await
            .unwrap();
    }
    let trend_score = |db: Arc<CacheDb>, id: [u8; 32]| async move {
        db.get_content_scores(&id)
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.factor == "trending")
            .map(|e| e.score)
    };
    assert_eq!(
        trend_score(f_state.db.clone(), digest).await,
        Some(130),
        "3 fresh local likes → local_pm only"
    );

    // F's export over the channel: exactly its LOCAL k-gate-passed entry.
    let export: FedTrendsExportReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.trends.export",
                [4u8; 16],
                Value::Null,
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("export ok"),
    );
    assert_eq!(export.entries.len(), 1);
    assert_eq!(export.entries[0].content_id, hex::encode(digest));
    assert_eq!(export.entries[0].score_pm, 130, "un-composed local_pm");
    assert_eq!(export.entries[0].engager_count, 3);

    // H pushes an absurd claimed magnitude for F's post, an entry for a post F
    // has never seen, and a below-k entry (skipped, not imported).
    let unseen = [0x51u8; 32];
    let exchange = FedTrendsExchangeRequest {
        epoch: 1,
        entries: vec![
            FedTrendEntry {
                content_id: hex::encode(digest),
                score_pm: 60_000,      // absurd hint — never summed
                engager_count: 90_000, // absurd, but ≥ k → presence only
            },
            FedTrendEntry {
                content_id: hex::encode(unseen),
                score_pm: 500,
                engager_count: 40,
            },
            FedTrendEntry {
                content_id: hex::encode([0x52u8; 32]),
                score_pm: 100,
                engager_count: 2, // below the k floor → not corroboration
            },
        ],
    };
    let imported: FedTrendsExchangeReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.trends.exchange",
                [3u8; 16],
                to_value(&exchange),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("exchange ok"),
    );
    assert_eq!(imported.imported, 2, "below-k entry skipped");

    // Presence-only: 130 (local) + flat 100 (ONE distinct peer's ramp) = 230 —
    // the claimed 90 000 engagers bought nothing beyond a single presence bucket.
    assert_eq!(
        trend_score(f_state.db.clone(), digest).await,
        Some(230),
        "peer ramps by presence, never by claimed magnitude"
    );
    // The unseen id got a peer row but NO blind local trending row.
    assert_eq!(
        trend_score(f_state.db.clone(), unseen).await,
        None,
        "no blind row for a post this nest has never seen"
    );

    // Re-export: still F's LOCAL local_pm (130), never the 230 composed with the
    // peer ramp (no laundering), never the peer-only unseen id.
    let export2: FedTrendsExportReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.trends.export",
                [5u8; 16],
                Value::Null,
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("export ok"),
    );
    assert_eq!(export2.entries.len(), 1);
    assert_eq!(export2.entries[0].content_id, hex::encode(digest));
    assert_eq!(
        export2.entries[0].score_pm, 130,
        "export local_pm only — no laundering of the peer ramp"
    );
}

/// **Post fetch (row 9).** H fetches a post resident on F by id — the same
/// `get_post_core` the HTTP twin runs (caller = None, since a peer is never the
/// author/admin). A missing post surfaces `fauna.federation.not_found`.
#[tokio::test]
async fn post_get_over_channel() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    let post_id = [0x22u8; 32];
    let post_bytes = b"raw-post-bytes".to_vec();
    f_state
        .db
        .put_post(&post_id, &post_bytes, None)
        .await
        .unwrap();

    let req = FedPostGetRequest {
        post_id: hex::encode(post_id),
    };
    let reply: FedPostGetReply = from_value(
        &conn
            .dispatcher
            .request_raw("fauna.federation.post.get", [1u8; 16], to_value(&req), None)
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("post found"),
    );
    assert_eq!(reply.post, post_bytes);

    let missing = FedPostGetRequest {
        post_id: hex::encode([0x99u8; 32]),
    };
    let err = conn
        .dispatcher
        .request_raw(
            "fauna.federation.post.get",
            [2u8; 16],
            to_value(&missing),
            None,
        )
        .await
        .unwrap()
        .await_reply()
        .await
        .expect_err("a missing post must error");
    assert_eq!(err.code, "fauna.federation.not_found");
}

/// **Post fetch of a legally taken-down post — DISCLOSE, don't hide** (F2,
/// posture ratified 2026-07-06; `moderation.md` § Implementation status
/// today). A legal takedown is the transparent, non-silent carve-out: the
/// peer fetch withholds the body (empty `post`) but surfaces the additive
/// `legal_takedown` marker with the tombstone reference — consistent with
/// the HTTP twin's 451 and the conv-federation marker
/// (`FedChannelFetchMessage.legal_takedown`), deliberately unlike
/// policy-quarantine's existence-hiding `not_found`.
#[tokio::test]
async fn post_get_of_taken_down_post_discloses_over_channel() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    // Seed a decodable post on F (so its `content_meta` row co-writes and the
    // takedown flag has a row to land on), then take it down.
    let post = fauna_core::data::Post {
        author: ActorId([0x33u8; 32]),
        created_at: fauna_core::data::Timestamp(1_000_000),
        body: fauna_core::data::PostBody::Text {
            content: "genuinely illegal content".to_string(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let post_id = [0x34u8; 32];
    f_state
        .db
        .put_post(
            &post_id,
            &fauna_core::encoding::canonical_encode(&post).unwrap(),
            None,
        )
        .await
        .unwrap();
    f_state
        .db
        .set_post_legal_takedown(&post_id, Some("EU-DSA-2024/451"))
        .await
        .unwrap();

    let req = FedPostGetRequest {
        post_id: hex::encode(post_id),
    };
    let reply: FedPostGetReply = from_value(
        &conn
            .dispatcher
            .request_raw("fauna.federation.post.get", [3u8; 16], to_value(&req), None)
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("a taken-down post DISCLOSES (not not_found — that would hide existence)"),
    );
    // Body withheld; withholding disclosed with the tombstone reference.
    assert!(reply.post.is_empty(), "the body must be withheld");
    let marker = reply
        .legal_takedown
        .expect("the legal_takedown marker must disclose the withholding");
    assert_eq!(marker.reference, "EU-DSA-2024/451");
}

/// **Nest-sync authz (rows 3–4), the §4.C hardening.** An unpaired peer's
/// `sync.pull` is `forbidden` — the gate keys on the connection's **verified**
/// nest_id, not a body-claimed one. Then, once F pairs the actor with H's nest,
/// a `sync.push` followed by a `sync.pull` round-trips the entry.
#[tokio::test]
async fn sync_pull_unpaired_is_forbidden_then_round_trips_when_paired() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id = h_state.nest_identity.public_key_bytes();
    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    let actor = [0x55u8; 32];
    // The actor's self-namespace — the only one a pairing reaches.
    let namespace = actor;

    // Unpaired → forbidden.
    let pull = FedSyncPullRequest {
        actor_id: hex::encode(actor),
        namespace: hex::encode(namespace),
        since: 0,
    };
    let err = conn
        .dispatcher
        .request_raw(
            "fauna.federation.sync.pull",
            [1u8; 16],
            to_value(&pull),
            None,
        )
        .await
        .unwrap()
        .await_reply()
        .await
        .expect_err("unpaired pull is forbidden");
    assert_eq!(err.code, "fauna.federation.forbidden");

    // Pair the actor with H's verified nest on F, then push + pull.
    f_state
        .db
        .store_pairing(&actor, &h_nest_id, &["sync".to_string()], None, None, None)
        .await
        .unwrap();

    let push = FedSyncPushRequest {
        actor_id: hex::encode(actor),
        namespace: hex::encode(namespace),
        entries: vec![FedSyncPushEntry {
            entry_id: vec![1, 2, 3],
            ciphertext: b"ct".to_vec(),
            actor_sig: b"sig".to_vec(),
        }],
    };
    let push_reply: FedSyncPushReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.sync.push",
                [2u8; 16],
                to_value(&push),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("push ok"),
    );
    assert!(push_reply.up_to > 0);

    let pull_reply: FedSyncPullReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.sync.pull",
                [3u8; 16],
                to_value(&pull),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("pull ok"),
    );
    assert_eq!(pull_reply.entries.len(), 1);
    assert_eq!(pull_reply.entries[0].ciphertext, b"ct".to_vec());
    assert_eq!(pull_reply.up_to, push_reply.up_to);
}

/// Build a valid signed inbox payload — the canonical `(EmbedAsBytes-cr,
/// EmbedAsBytes-post)` tuple the inbox handler decodes (mirrors
/// `inbox_signature::build_valid_payload`). `schema` selects the post body
/// schema.
///
/// `schema` is a free-form string the *sender* signs over their *own* post, so
/// it must never influence how the recipient's nest routes the arrival — see
/// `group_v1_schema_does_not_bypass_inbox_mode_over_the_channel`.
fn build_inbox_payload(sender: &ActorKeypair, recipient: &ActorId, schema: &str) -> Vec<u8> {
    use fauna_core::data::{ContactRequest, Post, PostBody, StructuredField, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, compute_post_id, sign_envelope};

    let author = sender.actor_id();
    let post = Post {
        author,
        created_at: Timestamp::now(),
        body: PostBody::Structured {
            schema: schema.into(),
            fields: vec![StructuredField {
                key: "to".into(),
                value: hex::encode(recipient.0),
            }],
            content: Some("hello".into()),
            facets: vec![],
            items: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let (post_bytes, post_env) = sign_envelope(sender, &post).unwrap();
    let post_wire = EmbedAsBytes::from_signed(post_bytes, post_env);
    let post_id = compute_post_id(&post).unwrap();

    let cr = ContactRequest {
        sender: author,
        post_id,
        sender_node: b"http://localhost:3000".to_vec(),
        summary: "hi".into(),
        created_at: Timestamp::now(),
    };
    let (cr_bytes, cr_env) = sign_envelope(sender, &cr).unwrap();
    let cr_wire = EmbedAsBytes::from_signed(cr_bytes, cr_env);

    canonical_encode(&(&cr_wire, &post_wire)).unwrap()
}

/// **Inbox delivery (cross-nest social inbox).** H delivers a Fauna-native signed
/// `(ContactRequest, Post)` payload to an `open`-inbox recipient on F over
/// `fauna.federation.inbox.deliver` — the migration of the `POST /api/v1/inbox/
/// {actor}` fan-out. Proves the shared `routes::deliver_inbox_payload_core` runs
/// over the channel: the row lands in F's inbox and the reply carries its id.
#[tokio::test]
async fn inbox_deliver_over_channel() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    let sender = ActorKeypair::from_secret([0x11u8; 32]);
    let recipient = ActorId([0x77u8; 32]);
    // The recipient holds an account on F: existence is judged at the
    // (terminal) recipient id before routing, so a never-registered id is
    // refused rather than knocked on.
    f_state
        .db
        .create_user_with_handle(&recipient.0, "free", "recipient", None)
        .await
        .unwrap();
    f_state
        .db
        .set_inbox_mode(&recipient.0, "open")
        .await
        .unwrap();
    let payload = build_inbox_payload(&sender, &recipient, "note/v1");

    let req = FedInboxDeliverRequest {
        recipient_actor_id: hex::encode(recipient.0),
        payload_bytes: payload.clone(),
    };
    let reply: FedInboxDeliverReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.inbox.deliver",
                [1u8; 16],
                to_value(&req),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("inbox delivered over the channel"),
    );
    assert!(
        reply.inbox_id.is_some(),
        "an open-mode recipient is delivered (not knock-stored) → an inbox row id"
    );

    let inbox = f_state.db.list_inbox_all(&recipient.0).await.unwrap();
    assert_eq!(
        inbox.len(),
        1,
        "exactly one payload delivered to the recipient on F"
    );
}

/// Regression pin for F2/F3, cross-nest leg.
///
/// This test used to be the exploit: it delivered a `group/v1` payload from a
/// keypair with **no relationship whatsoever** to the recipient and asserted the
/// row landed in their inbox — the `is_group_payload` bypass returned before both
/// the `InboxMode` routing and the family reach gate. `verify_inbox_payload`
/// constrains the two signatures, `cr.sender == post.author`, and the post id;
/// **nothing constrains `schema`, and no third party signs it.** The bypass is
/// gone: the very same payload must now be routed by the recipient's inbox mode.
#[tokio::test]
async fn group_v1_schema_does_not_bypass_inbox_mode_over_the_channel() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    let sender = ActorKeypair::from_secret([0x11u8; 32]);
    let recipient = ActorId([0x77u8; 32]);
    f_state
        .db
        .set_inbox_mode(&recipient.0, "contacts_only")
        .await
        .unwrap();

    let req = FedInboxDeliverRequest {
        recipient_actor_id: hex::encode(recipient.0),
        payload_bytes: build_inbox_payload(&sender, &recipient, "group/v1"),
    };
    let err = conn
        .dispatcher
        .request_raw(
            "fauna.federation.inbox.deliver",
            [1u8; 16],
            to_value(&req),
            None,
        )
        .await
        .unwrap()
        .await_reply()
        .await
        .expect_err("a self-declared `group/v1` schema must not defeat contacts_only");
    assert_eq!(err.code, "fauna.federation.forbidden");

    assert!(
        f_state
            .db
            .list_inbox_all(&recipient.0)
            .await
            .unwrap()
            .is_empty(),
        "a stranger's relabelled post reached a contacts_only inbox"
    );
}

/// **Inbox-mode routing runs over the channel.** A non-group payload to a
/// `closed`-inbox recipient surfaces `fauna.federation.forbidden` — proving the
/// receiver applies the recipient's `InboxMode` via the shared core (the same
/// 403 the HTTP twin produced), not a blind `push_inbox`.
#[tokio::test]
async fn inbox_deliver_closed_inbox_is_forbidden() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    let sender = ActorKeypair::from_secret([0x22u8; 32]);
    let recipient = ActorId([0x66u8; 32]);
    f_state
        .db
        .set_inbox_mode(&recipient.0, "closed")
        .await
        .unwrap();

    let req = FedInboxDeliverRequest {
        recipient_actor_id: hex::encode(recipient.0),
        payload_bytes: build_inbox_payload(&sender, &recipient, "email/v1"),
    };
    let err = conn
        .dispatcher
        .request_raw(
            "fauna.federation.inbox.deliver",
            [1u8; 16],
            to_value(&req),
            None,
        )
        .await
        .unwrap()
        .await_reply()
        .await
        .expect_err("a closed inbox must reject the delivery");
    assert_eq!(err.code, "fauna.federation.forbidden");

    let inbox = f_state.db.list_inbox_all(&recipient.0).await.unwrap();
    assert!(inbox.is_empty(), "nothing delivered to a closed inbox");
}

/// **The `closed` arm reaches the Welcome door too — the twin of
/// `inbox_deliver_closed_inbox_is_forbidden` above.** `fauna.federation.
/// welcome.deliver` is the *other* open-federation ingress that lands a row in
/// a stranger's inbox, and it lands considerably more than a row: an inbox
/// write charged against the recipient's quota, a `PushEvent::Welcome` on their
/// live socket, a device push, and — for an unclaimed DM channel —
/// `register_actor_channel`, which seats them on the channel so the peer's
/// subsequent ciphertext fetches resolve. Under `closed` ("No new messages
/// accepted", `inbox_privacy.closed_desc`, the strongest setting the product
/// offers) none of that may happen, exactly as the inbox door already refuses.
///
/// Scope, deliberately narrow: only the `closed` arm is pinned here.
/// `allow_knock` / `contacts_only` remain declared cross-nest gaps
/// (`direct-messages.md` § Reach policy) because their verdicts turn on a
/// *contact edge* and this wire carries no signed sender — refusing on them
/// would refuse cross-nest contacts too. `closed` needs no sender identity:
/// it is a fact about the recipient alone.
#[tokio::test]
async fn welcome_deliver_closed_inbox_is_forbidden() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;

    let recipient = [0x66u8; 32];
    f_state
        .db
        .create_user_with_handle(&recipient, "free", "carol", None)
        .await
        .unwrap();
    f_state
        .db
        .set_inbox_mode(&recipient, "closed")
        .await
        .unwrap();

    let dm_channel = [0x88u8; 32];
    let err = fauna_nest::federation_pool::originate_welcome_deliver(
        &h_state.federation_pool,
        &h_state,
        &f_url,
        fauna_nest::federation_handlers::FedWelcomeDeliverRequest {
            recipient_actor_id: hex::encode(recipient).to_string(),
            channel_id: Some(hex::encode(dm_channel).to_string()),
            welcome_bytes: b"welcome-bytes".to_vec(),
            channel_type: Some("dm".to_string()),
            group_id: None,
            origin_nest_url: None,
            set_name: None,
            set_name_sealed: None,
            set_name_hash: None,
            access: None,
            home_nest_actor_id: None,
            residency: None,
            ..Default::default()
        },
    )
    .await
    .expect_err("a closed inbox must reject a cross-nest Welcome");
    assert!(
        format!("{err:?}").contains("fauna.federation.forbidden"),
        "the peer's refusal must surface as the federation forbidden code, got {err:?}"
    );

    // Nothing landed: no inbox row (so no quota charge, no push fan-out) ...
    assert!(
        f_state
            .db
            .list_inbox_all(&recipient)
            .await
            .unwrap()
            .is_empty(),
        "nothing delivered to a closed inbox over the Welcome door"
    );
    // ... and — the half a bare delivery check would miss — no roster seat.
    assert!(
        !f_state
            .db
            .list_actor_channels(&recipient)
            .await
            .unwrap()
            .contains(&dm_channel),
        "a closed recipient must not be seated on the channel"
    );
}

/// **Feed query (row 8), the second hardening dividend.** Unauthenticated on the
/// HTTP interim; over the channel it serves through the shared
/// `remote_query_feed_core` with mutual auth — i.e. the kind is on the allowlist
/// and dispatches, NOT `unauthenticated`. (`RemoteQueryResponse` is crate-private,
/// so we assert on the decoded `Value` shape.)
///
/// Also pins each served candidate's `source` to its own indexed token
/// (`feed.md` § Posts) rather than `remote_query_feed_core`'s former
/// hard-coded `"fauna"`: a bridge-ingested post (`put_post_with_source`) and
/// an archive-imported one (a `PostOrigin`-carrying post through plain
/// `put_post`, which runs the slice-2 `source_token` derivation —
/// `archive-import.md` § What each category becomes) must each be advertised
/// to a peer nest under their real protocol, not native `fauna`.
#[tokio::test]
async fn feed_query_over_channel_serves() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);

    // A bluesky-bridged post: bare canonical-encoded `Post`, indexed via the
    // bridge ingest path.
    let bluesky_kp = ActorKeypair::generate();
    let bluesky_post = fauna_core::data::Post {
        author: bluesky_kp.actor_id(),
        created_at: fauna_core::data::Timestamp(1_700_000_001_000_000),
        body: fauna_core::data::PostBody::Text {
            content: "bridged from bluesky".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let bluesky_data = fauna_core::encoding::canonical_encode(&bluesky_post).unwrap();
    let bluesky_post_id: [u8; 32] = *blake3::hash(&bluesky_data).as_bytes();
    f_state
        .db
        .put_post_with_source(&bluesky_post_id, &bluesky_data, "bluesky")
        .await
        .unwrap();

    // A facebook archive-imported post: a signed `Post` carrying a
    // `PostOrigin`, indexed via plain `put_post` so `extract_post_metadata`
    // derives `source` from `Post::source_token()` itself.
    let facebook_kp = ActorKeypair::generate();
    let facebook_bytes = common::signed_text_post_with_origin(
        &facebook_kp,
        "re-authored from an export",
        "facebook",
    );
    let facebook_post_id: [u8; 32] = *blake3::hash(&facebook_bytes).as_bytes();
    f_state
        .db
        .put_post(&facebook_post_id, &facebook_bytes, None)
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    let req = RemoteQueryRequest {
        rules: vec![],
        combination: "all".into(),
        authors: None,
        limit: Some(10),
        cursor: None,
    };
    let reply: Value = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.feed.query",
                [1u8; 16],
                to_value(&req),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("feed.query serves (not unauthenticated)"),
    );
    let candidates = match &reply {
        Value::Map(m) => match m.get("candidates") {
            Some(Value::List(l)) => l,
            other => panic!("expected a candidates list, got {other:?}"),
        },
        other => panic!("expected a candidates-bearing reply, got {other:?}"),
    };
    assert_eq!(
        candidates.len(),
        2,
        "both the bridged and the imported post must be served"
    );
    let sources: std::collections::BTreeSet<String> = candidates
        .iter()
        .map(|c| match c {
            Value::Map(m) => match m.get("source") {
                Some(Value::String(s)) => s.clone(),
                other => panic!("expected a text source field, got {other:?}"),
            },
            other => panic!("expected a candidate map, got {other:?}"),
        })
        .collect();
    assert_eq!(
        sources,
        ["bluesky".to_string(), "facebook".to_string()]
            .into_iter()
            .collect(),
        "each candidate's source must equal its own indexed token, never the native default"
    );
}

/// The author-scoped twin of [`feed_query_over_channel_serves`] — the ONLY
/// query shape `discovery.rs::follow_references` ever sends a peer (it always
/// names one specific author). `query_feed_for_authors` used to omit
/// `content.source` from its SQL projection entirely, hard-coding an empty
/// string onto every `FeedPostRow` it built — silently harmless while
/// `parse_peer_candidate` ignored the wire `source` field outright, but a
/// silent candidate-drop the moment it started validating that field via
/// `fauna_core::source::normalize`: an empty string
/// fails normalization. This pins the SQL fix rather than the parsing side.
#[tokio::test]
async fn feed_query_over_channel_serves_source_for_author_scoped_queries() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);

    let bluesky_kp = ActorKeypair::generate();
    let bluesky_post = fauna_core::data::Post {
        author: bluesky_kp.actor_id(),
        created_at: fauna_core::data::Timestamp(1_700_000_002_000_000),
        body: fauna_core::data::PostBody::Text {
            content: "bridged from bluesky, author-scoped".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let bluesky_data = fauna_core::encoding::canonical_encode(&bluesky_post).unwrap();
    let bluesky_post_id: [u8; 32] = *blake3::hash(&bluesky_data).as_bytes();
    f_state
        .db
        .put_post_with_source(&bluesky_post_id, &bluesky_data, "bluesky")
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    let req = RemoteQueryRequest {
        rules: vec![],
        combination: "all".into(),
        authors: Some(vec![hex::encode(bluesky_kp.actor_id().0)]),
        limit: Some(10),
        cursor: None,
    };
    let reply: Value = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.feed.query",
                [1u8; 16],
                to_value(&req),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("feed.query serves (not unauthenticated)"),
    );
    let candidates = match &reply {
        Value::Map(m) => match m.get("candidates") {
            Some(Value::List(l)) => l,
            other => panic!("expected a candidates list, got {other:?}"),
        },
        other => panic!("expected a candidates-bearing reply, got {other:?}"),
    };
    assert_eq!(
        candidates.len(),
        1,
        "the one author-matching post is served"
    );
    let source = match &candidates[0] {
        Value::Map(m) => match m.get("source") {
            Some(Value::String(s)) => s.clone(),
            other => panic!("expected a text source field, got {other:?}"),
        },
        other => panic!("expected a candidate map, got {other:?}"),
    };
    assert_eq!(
        source, "bluesky",
        "the author-scoped projection must carry the real indexed token, not an empty default"
    );
}

// ── Slice-4 capstone: a cross-nest MLS group forms over the channel ────────────

const FAR_FUTURE: u64 = u64::MAX / 2;

/// **Slice-4 capstone (§6).** The same end goal as the HTTP-interim capstone
/// `conformance_cross_nest_conversations::alice_on_home_forms_mls_group_with_bob_on_foreign_nest`
/// — Alice on home nest H forms one MLS group with Bob on foreign nest F — but
/// the key-package fetch and the Welcome delivery ride the **federation WS-RPC
/// channel** (the `FederationChannelPool` originators), not the HTTP relay.
///
/// "No HTTP federation call" is the only path: since slice 5 retired the HTTP
/// interim, the pool originators have no fallback — they return the channel's
/// reply or a `PoolError`. The pool also resolved F's `nest_id` from its URL via
/// the anon `fauna.nest.info` kind and dialed F's `/api/v1/federation/ws` to get
/// there.
#[tokio::test]
async fn alice_forms_cross_nest_mls_group_over_the_channel() {
    let (_h_url, h_state) = start_nest().await; // home nest H (Alice)
    let (f_url, f_state) = start_nest().await; // foreign nest F (Bob)

    // Bob lives on F: real MLS engine, one key package published to F.
    let bob_kp = ActorKeypair::generate();
    let bob_id = bob_kp.actor_id();
    let bob_hex = hex::encode(bob_id.0);
    f_state
        .db
        .create_user_with_handle(&bob_id.0, "free", "bob", None)
        .await
        .unwrap();
    let bob_engine = MlsEngine::new_in_memory(bob_kp).expect("bob engine");
    let bob_pkgs = bob_engine.generate_key_packages_bytes(1).expect("bob KP");
    f_state
        .db
        .put_key_package("bob-kp-0", &bob_id.0, &bob_pkgs[0], 0, FAR_FUTURE)
        .await
        .unwrap();

    // Alice lives on H.
    let alice_kp = ActorKeypair::generate();
    let alice_engine = MlsEngine::new_in_memory(alice_kp).expect("alice engine");

    // (1) H fetches Bob's KP from F OVER THE CHANNEL (pool dials F, resolving F's
    //     nest_id via nest.info + the hello handshake).
    let fetched = fauna_nest::federation_pool::originate_keypackage_fetch(
        &h_state.federation_pool,
        &h_state,
        &f_url,
        &bob_hex,
    )
    .await
    .expect("channel keypackage.fetch")
    .expect("F has a key package for Bob");
    let bob_validated_kp = alice_engine
        .key_package_from_bytes(&fetched)
        .expect("validate Bob's fetched KP");

    // (2) Alice builds the MLS group including Bob → produces a Welcome.
    let (channel_id, welcome) = alice_engine
        .create_group(&[bob_validated_kp])
        .expect("create cross-nest group");
    let welcome_bytes = welcome.to_bytes().expect("welcome bytes");

    // (3) H delivers the Welcome to F OVER THE SAME CHANNEL (reuses the pooled
    //     connection).
    fauna_nest::federation_pool::originate_welcome_deliver(
        &h_state.federation_pool,
        &h_state,
        &f_url,
        fauna_nest::federation_handlers::FedWelcomeDeliverRequest {
            recipient_actor_id: bob_hex.to_string(),
            channel_id: Some(hex::encode(channel_id.0).to_string()),
            welcome_bytes: welcome_bytes.to_vec(),
            channel_type: Some("dm".to_string()),
            group_id: None,
            origin_nest_url: None,
            set_name: None,
            set_name_sealed: None,
            set_name_hash: None,
            access: None,
            home_nest_actor_id: None,
            residency: None,
            ..Default::default()
        },
    )
    .await
    .expect("channel welcome.deliver");

    // (4) Bob (on F) reads the Welcome from his inbox and joins the group.
    let inbox = f_state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one welcome delivered to Bob on F");
    let delivered_welcome = common::welcome_bytes_from_inbox(&inbox[0].1);
    let bob_channel = bob_engine
        .join_from_welcome_bytes(&delivered_welcome)
        .expect("Bob joins the cross-nest group from the Welcome");

    // End goal: one MLS group spanning H and F, formed entirely over the channel.
    assert_eq!(
        bob_channel.0, channel_id.0,
        "Alice and Bob share the same MLS channel id across nests"
    );
    let alice_members = alice_engine.group_members(&channel_id);
    assert!(
        alice_members.iter().any(|m| m.0 == bob_id.0),
        "Alice's group view includes Bob"
    );
    assert_eq!(
        alice_members.len(),
        2,
        "the cross-nest group has Alice + Bob"
    );
    assert_eq!(
        bob_engine.group_members(&bob_channel).len(),
        2,
        "Bob's group view also has both members"
    );
}

/// F10: `welcome.deliver` from an unauthenticated peer must NOT deliver to a
/// recipient that doesn't exist on this nest — otherwise a peer floods
/// `actor_channels`/the inbox + push fan-out for arbitrary 32-byte ids.
#[tokio::test]
async fn welcome_deliver_rejects_unregistered_recipient() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;

    let stranger = [0x99u8; 32]; // never registered on F
    let result = fauna_nest::federation_pool::originate_welcome_deliver(
        &h_state.federation_pool,
        &h_state,
        &f_url,
        fauna_nest::federation_handlers::FedWelcomeDeliverRequest {
            recipient_actor_id: hex::encode(stranger).to_string(),
            channel_id: Some(hex::encode([0u8; 32]).to_string()),
            welcome_bytes: b"welcome-bytes".to_vec(),
            channel_type: Some("dm".to_string()),
            group_id: None,
            origin_nest_url: None,
            set_name: None,
            set_name_sealed: None,
            set_name_hash: None,
            access: None,
            home_nest_actor_id: None,
            residency: None,
            ..Default::default()
        },
    )
    .await;

    assert!(
        result.is_err(),
        "welcome to an unregistered recipient must be rejected"
    );
    let inbox = f_state.db.list_inbox_all(&stranger).await.unwrap();
    assert!(
        inbox.is_empty(),
        "no inbox row may be created for an unregistered recipient"
    );
    assert!(
        f_state
            .db
            .list_actor_channels(&stranger)
            .await
            .unwrap()
            .is_empty(),
        "no channel auto-registration for an unregistered recipient"
    );
}

/// A folder share always KNOCKS cross-nest (`shared_by = None`,
/// manual accept), so a relayed `welcome.deliver` for a **claimed folder
/// channel** must NOT pre-register the recipient onto the owner-managed
/// `actor_channels` roster — else an evicted member colluding with an
/// open-federation peer re-inserts themselves and re-surfaces the set's
/// discovery metadata (`media.list` / `members.list_actors` / `folders.list`
/// all gate on the roster, not on MLS-join). A legit **unclaimed** DM welcome
/// still auto-registers, since its cross-nest delivery depends on the roster row.
#[tokio::test]
async fn welcome_deliver_skips_roster_for_claimed_folder_channel() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;

    // F hosts a folder: an owner claims the derived channel; the evicted
    // member `evil` is a real local actor on F (so the F10 gate passes).
    let owner = [0x11u8; 32];
    let evil = [0x22u8; 32];
    let folder_channel = [0x33u8; 32];
    f_state
        .db
        .create_user_with_handle(&owner, "free", "owner", None)
        .await
        .unwrap();
    f_state
        .db
        .create_user_with_handle(&evil, "free", "evil", None)
        .await
        .unwrap();
    f_state
        .db
        .claim_folder_channel(&owner, &folder_channel)
        .await
        .unwrap();
    assert_eq!(
        f_state
            .db
            .folder_channel_claimed_by(&folder_channel)
            .await
            .unwrap(),
        Some(owner),
        "owner holds the first-binder claim on the folder channel"
    );

    // The colluding peer H relays a folder Welcome to F for `evil` on the
    // claimed channel — the attack: re-insert onto the roster via federation.
    fauna_nest::federation_pool::originate_welcome_deliver(
        &h_state.federation_pool,
        &h_state,
        &f_url,
        fauna_nest::federation_handlers::FedWelcomeDeliverRequest {
            recipient_actor_id: hex::encode(evil).to_string(),
            channel_id: Some(hex::encode(folder_channel).to_string()),
            welcome_bytes: b"welcome-bytes".to_vec(),
            channel_type: Some("folder".to_string()),
            group_id: None,
            origin_nest_url: None,
            set_name: None,
            set_name_sealed: None,
            set_name_hash: None,
            access: None,
            home_nest_actor_id: None,
            residency: None,
            ..Default::default()
        },
    )
    .await
    .expect("the knock is still delivered to the inbox");

    // The knock reaches the inbox (delivery is unchanged) ...
    assert_eq!(
        f_state.db.list_inbox_all(&evil).await.unwrap().len(),
        1,
        "the knock welcome is still delivered to the inbox"
    );
    // ... but the roster is UNTOUCHED — no self-insert onto the claimed channel.
    assert!(
        !f_state
            .db
            .list_actor_channels(&evil)
            .await
            .unwrap()
            .contains(&folder_channel),
        "a cross-nest knock must NOT roster-register onto a claimed folder channel"
    );

    // Contrast: a legit unclaimed DM welcome still auto-registers the recipient
    // (its cross-nest delivery depends on the roster row).
    let dave = [0x44u8; 32];
    let dm_channel = [0x55u8; 32];
    f_state
        .db
        .create_user_with_handle(&dave, "free", "dave", None)
        .await
        .unwrap();
    fauna_nest::federation_pool::originate_welcome_deliver(
        &h_state.federation_pool,
        &h_state,
        &f_url,
        fauna_nest::federation_handlers::FedWelcomeDeliverRequest {
            recipient_actor_id: hex::encode(dave).to_string(),
            channel_id: Some(hex::encode(dm_channel).to_string()),
            welcome_bytes: b"welcome-bytes".to_vec(),
            channel_type: Some("dm".to_string()),
            group_id: None,
            origin_nest_url: None,
            set_name: None,
            set_name_sealed: None,
            set_name_hash: None,
            access: None,
            home_nest_actor_id: None,
            residency: None,
            ..Default::default()
        },
    )
    .await
    .expect("dm welcome.deliver");
    assert!(
        f_state
            .db
            .list_actor_channels(&dave)
            .await
            .unwrap()
            .contains(&dm_channel),
        "an unclaimed DM welcome still auto-registers the recipient"
    );
}

/// The federation twin of row 433 contract item 1
/// (`conversations_handlers::claim_read_error_does_not_register_a_non_claimant`,
/// the same-nest case): an unresolvable claim read — the
/// `folder_channel_claims` table dropped out from under a live claim, so
/// `folder_channel_claim` reads `Unknown` rather than `Unclaimed` — must fail
/// **closed** over a relayed `welcome.deliver` too, never re-admitting a
/// non-claimant onto the roster. `Unknown` gates exactly like a real
/// claimant would (`federation_handlers.rs`'s `claim == Unclaimed` check).
#[tokio::test]
async fn welcome_deliver_does_not_register_a_non_claimant_on_claim_read_error() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;

    let owner = [0x66u8; 32];
    let evicted = [0x77u8; 32];
    let folder_channel = [0x88u8; 32];
    f_state
        .db
        .create_user_with_handle(&owner, "free", "owner2", None)
        .await
        .unwrap();
    f_state
        .db
        .create_user_with_handle(&evicted, "free", "evicted", None)
        .await
        .unwrap();
    f_state
        .db
        .claim_folder_channel(&owner, &folder_channel)
        .await
        .unwrap();

    // Simulate the claim-read error the finding describes — drop the table
    // out from under the live claim so `folder_channel_claimed_by` returns
    // `Err`, not `Ok(None)` (the fail-open trap row 433 fixed same-nest).
    {
        let conn = f_state.db.conn().await;
        conn.execute_batch("DROP TABLE folder_channel_claims")
            .unwrap();
    }
    assert!(
        f_state
            .db
            .folder_channel_claimed_by(&folder_channel)
            .await
            .is_err(),
        "test setup: the drop must turn the read into an Err"
    );

    fauna_nest::federation_pool::originate_welcome_deliver(
        &h_state.federation_pool,
        &h_state,
        &f_url,
        fauna_nest::federation_handlers::FedWelcomeDeliverRequest {
            recipient_actor_id: hex::encode(evicted).to_string(),
            channel_id: Some(hex::encode(folder_channel).to_string()),
            welcome_bytes: b"welcome-bytes".to_vec(),
            channel_type: Some("folder".to_string()),
            group_id: None,
            origin_nest_url: None,
            set_name: None,
            set_name_sealed: None,
            set_name_hash: None,
            access: None,
            home_nest_actor_id: None,
            residency: None,
            ..Default::default()
        },
    )
    .await
    .expect("the knock is still delivered to the inbox despite the unresolvable claim");

    assert_eq!(
        f_state.db.list_inbox_all(&evicted).await.unwrap().len(),
        1,
        "delivery is unchanged by the claim-read failure"
    );
    assert!(
        !f_state
            .db
            .list_actor_channels(&evicted)
            .await
            .unwrap()
            .contains(&folder_channel),
        "an unresolvable claim state must fail CLOSED — never re-admit onto \
         the roster via a relayed welcome"
    );
}

/// Path-sealing S5c-2: the origin nest's set-name seal + salt pair rides the
/// `fauna.federation.welcome.deliver` relay verbatim into the receiving
/// nest's local `WelcomeInbox` envelope — opaque to F exactly as it is to H,
/// the same "carries the blob it cannot read" property S4 pinned for the
/// WebDAV leg. A shared set's seal is under the M2 content key every roster
/// member holds, so once Bob (here, a stand-in recipient) joins the group he
/// can open it — this test only proves the relay doesn't drop or mutate it.
#[tokio::test]
async fn welcome_deliver_relays_the_set_name_seal_and_salt_pair_verbatim() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;

    let recipient = [0x66u8; 32];
    f_state
        .db
        .create_user_with_handle(&recipient, "free", "carol", None)
        .await
        .unwrap();

    let sealed = vec![0xEDu8; 48];
    let hash = vec![0x5Au8; 32];
    fauna_nest::federation_pool::originate_welcome_deliver(
        &h_state.federation_pool,
        &h_state,
        &f_url,
        fauna_nest::federation_handlers::FedWelcomeDeliverRequest {
            recipient_actor_id: hex::encode(recipient).to_string(),
            channel_id: Some(hex::encode([0x77u8; 32]).to_string()),
            welcome_bytes: b"welcome-bytes".to_vec(),
            channel_type: Some("folder".to_string()),
            group_id: Some("grp-1".to_string()),
            origin_nest_url: Some("https://h.example".to_string()),
            set_name: Some("Shared docs".to_string()),
            set_name_sealed: Some(serde_bytes::ByteBuf::from(sealed.clone())),
            set_name_hash: Some(serde_bytes::ByteBuf::from(hash.clone())),
            access: Some("writer".to_string()),
            home_nest_actor_id: None,
            residency: None,
            ..Default::default()
        },
    )
    .await
    .expect("folder welcome.deliver");

    let inbox = f_state.db.list_inbox_all(&recipient).await.unwrap();
    assert_eq!(inbox.len(), 1, "the relayed welcome lands in the inbox");
    let env = fauna_protocol::inbox::InboxEnvelope::from_canonical_bytes(&inbox[0].1)
        .expect("canonical inbox envelope");
    let staged = env.decode_welcome().expect("welcome envelope");
    assert_eq!(
        staged.set_name_sealed.as_deref().map(|b| b.to_vec()),
        Some(sealed),
        "the origin's seal rides the relay byte-for-byte"
    );
    assert_eq!(
        staged.set_name_hash.as_deref().map(|b| b.to_vec()),
        Some(hash),
        "and the salt that opens it"
    );
}

/// **Slice-4 originator: namespace sync over the channel.** The nest-sync
/// worker's pull/push now originate via `federation_pool::originate_sync_{pull,
/// push}`. H pushes an entry to F and pulls it back over the channel. F pairs the
/// actor with H's verified nest_id so the `is_paired` gate admits the sync.
#[tokio::test]
async fn nest_sync_pull_push_over_the_channel() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let h_nest_id = h_state.nest_identity.public_key_bytes();

    let actor = [0x42u8; 32];
    let actor_hex = hex::encode(actor);
    // The actor's self-namespace — the only one a pairing reaches.
    let namespace = actor;

    // F pairs the actor with H's verified nest so the sync gate admits it.
    f_state
        .db
        .store_pairing(&actor, &h_nest_id, &["sync".to_string()], None, None, None)
        .await
        .unwrap();

    // H pushes one entry to F over the channel.
    let entries = vec![fauna_nest::db::NamespaceEntry {
        entry_id: vec![9, 9, 9],
        seq: 0,
        ciphertext: b"sync-ct".to_vec(),
        actor_sig: b"sync-sig".to_vec(),
        source: "local".to_string(),
        updated_at: 0,
    }];
    let up_to = fauna_nest::federation_pool::originate_sync_push(
        &h_state.federation_pool,
        &h_state,
        &f_url,
        &actor_hex,
        &namespace,
        &entries,
    )
    .await
    .expect("channel sync.push");
    assert!(up_to > 0, "the peer reports a high-water sequence");

    // H pulls it back over the (reused) channel.
    let reply = fauna_nest::federation_pool::originate_sync_pull(
        &h_state.federation_pool,
        &h_state,
        &f_url,
        &actor_hex,
        &namespace,
        0,
    )
    .await
    .expect("channel sync.pull");
    assert_eq!(reply.entries.len(), 1, "the pushed entry round-trips");
    assert_eq!(reply.entries[0].ciphertext, b"sync-ct".to_vec());
    assert_eq!(reply.up_to, up_to);
}

/// Seed one channel message on `state` via the WS-RPC `channel.send` handler,
/// which also auto-registers `actor` in the channel (so the federation
/// `mls_pull` handler resolves the actor's channels). `actor` must be an
/// enrolled User on `state` for the caller-class gate to admit the send.
async fn seed_channel_message(
    state: &Arc<AppState>,
    actor: [u8; 32],
    channel_id: &[u8; 32],
    body: &[u8],
) {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let mut builder = fauna_nest::rpc_router::RpcRouter::builder();
    fauna_nest::conversations_handlers::register_conversations_handlers(&mut builder);
    let router = builder.build();
    let meta = router
        .kind_meta("fauna.conversations.channel.send")
        .expect("channel.send kind registered");
    // The strict `SealedStorage::ingest_channel_envelope` verifier requires the
    // wire body to dag-cbor-decode as a `ChannelEnvelope` whose inner bytes
    // clear the AEAD-shape floor (length >= 28 = 12-byte nonce + 16-byte tag,
    // no plaintext-content magic prefix) — raw marker text no longer passes
    // because `AppState::for_test` now installs the one and only, strict,
    // `SealedStorage`. Pad the caller's marker bytes up to the floor (no test
    // in this file asserts byte-identity on the envelope, only seq/channel_id).
    let mut inner = body.to_vec();
    inner.resize(inner.len().max(32), 0);
    let envelope = fauna_mls::types::ChannelEnvelope::Application(inner)
        .to_bytes()
        .expect("encode ChannelEnvelope::Application");
    let req = ChannelSendRequest {
        channel_id: hex::encode(channel_id),
        envelope,
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
        extra: std::collections::BTreeMap::new(),
    };
    let payload = bytes::Bytes::from(encode_canonical(&req).unwrap().to_vec());
    (meta.handler)(state.clone(), actor, payload)
        .await
        .expect("channel.send seed ok");
}

/// Inject a channel record **directly into the `__conv` segment store**, bypassing
/// `fauna.conversations.channel.send`'s serve-page-budget door. `channel_send_core`
/// now refuses any envelope over `SERVE_PAGE_BUDGET_BYTES - RECORD_WIRE_OVERHEAD`
///, so an over-frame record can no longer
/// be seeded through [`seed_channel_message`]. This is the injection path such a
/// record actually reaches the store by — a hostile peer or any
/// non-validated write — the exact scenario the pull-side freeze defense
/// (`fed_mls_pull_freezes_on_a_single_over_frame_record_instead_of_skipping`) must
/// still hold against; the send-side door is a *separate, additive* ingress defense,
/// not a substitute for it. Registers the actor↔channel so `mls_pull`'s
/// `list_actor_channels` scope includes it (the send path does this via
/// `register_actor_channel_gated`).
async fn seed_channel_record_direct(
    state: &Arc<AppState>,
    actor: [u8; 32],
    channel_id: &[u8; 32],
    body: &[u8],
) {
    state
        .db
        .register_actor_channel(&actor, channel_id)
        .await
        .expect("register_actor_channel");
    let envelope = fauna_mls::types::ChannelEnvelope::Application(body.to_vec())
        .to_bytes()
        .expect("encode ChannelEnvelope::Application");
    // `received_at` is not load-bearing for the pull (it reads and orders by seq);
    // a fixed value keeps the seed deterministic.
    fauna_nest::segments::conv::append(
        &state.conv_segments,
        &state.db,
        channel_id,
        &envelope,
        1_700_000_000_000,
    )
    .await
    .expect("direct __conv append");
}

/// **MLS buffer pull (row 5) over the channel.** Two channel messages are seeded
/// on F; H (paired) pulls them over the channel and gets both in seq order. The
/// federation `mls_pull` handler gates `is_paired` on the verified dialing nest
/// (H), then reads F's buffered conv ciphertext — the channel-borne replacement
/// for the retired `/api/v1/nest-sync/mls-pull` HTTP twin (slice 5).
#[tokio::test]
async fn mls_pull_returns_buffered_messages_over_the_channel() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id = h_state.nest_identity.public_key_bytes();

    // Bob is an enrolled user on F with two buffered channel messages.
    let bob = ActorKeypair::generate();
    let bob_id = bob.actor_id().0;
    f_state
        .db
        .create_user_with_handle(&bob_id, "free", "bob", None)
        .await
        .unwrap();
    let channel_id = [0xDDu8; 32];
    seed_channel_message(&f_state, bob_id, &channel_id, b"message one").await;
    seed_channel_message(&f_state, bob_id, &channel_id, b"message two").await;

    // F pairs Bob with H's verified nest so the mls_pull gate admits it.
    f_state
        .db
        .store_pairing(&bob_id, &h_nest_id, &["sync".to_string()], None, None, None)
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let req = FedMlsPullRequest {
        actor_id: hex::encode(bob_id),
        since_seq: 0,
    };
    let reply: FedMlsPullReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.sync.mls_pull",
                [1u8; 16],
                to_value(&req),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("mls_pull ok"),
    );
    assert_eq!(
        reply.messages.len(),
        2,
        "both buffered messages pulled over the channel"
    );
    assert_eq!(reply.messages[0].seq, 1);
    assert_eq!(reply.messages[1].seq, 2);
    assert_eq!(reply.messages[0].channel_id, channel_id.to_vec());
}

/// **MLS buffer ack (row 6) over the channel.** After pulling, H acks up to the
/// high-water seq; F purges the buffered messages, so a re-pull is empty — the
/// channel-borne replacement for the retired `/api/v1/nest-sync/mls-ack` HTTP
/// twin (slice 5).
#[tokio::test]
async fn mls_ack_purges_buffered_messages_over_the_channel() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id = h_state.nest_identity.public_key_bytes();

    let bob = ActorKeypair::generate();
    let bob_id = bob.actor_id().0;
    f_state
        .db
        .create_user_with_handle(&bob_id, "free", "bob", None)
        .await
        .unwrap();
    let channel_id = [0xEEu8; 32];
    seed_channel_message(&f_state, bob_id, &channel_id, b"ack test message").await;
    f_state
        .db
        .store_pairing(&bob_id, &h_nest_id, &["sync".to_string()], None, None, None)
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let actor_hex = hex::encode(bob_id);

    let pull = FedMlsPullRequest {
        actor_id: actor_hex.clone(),
        since_seq: 0,
    };
    let reply: FedMlsPullReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.sync.mls_pull",
                [1u8; 16],
                to_value(&pull),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("mls_pull ok"),
    );
    assert_eq!(reply.messages.len(), 1);
    let up_to_seq = reply.messages[0].seq;

    let ack = FedMlsAckRequest {
        actor_id: actor_hex,
        up_to_seq,
    };
    let ack_reply: FedMlsAckReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.sync.mls_ack",
                [2u8; 16],
                to_value(&ack),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("mls_ack ok"),
    );
    assert!(ack_reply.purged >= 1, "at least one message purged");

    let reply2: FedMlsPullReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.sync.mls_pull",
                [3u8; 16],
                to_value(&pull),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("mls_pull ok"),
    );
    assert!(reply2.messages.is_empty(), "no messages remain after ack");
}

// ── conv serve pages: the 2 MiB frame budget (transport.md § Max frame) ──
//
// The federation channel rides the same 2 MiB WS frame as every WS-RPC
// surface, and both conv serve kinds paged by count only — so a page whose
// assembled envelopes exceed the frame was unsendable, permanently stalling
// the puller at that page (the conv twin of the ratified `mail_pull` budget,
// `deployment-home-with-public-relay.md` § Relay frame budget). The three
// tests below pin: close-early on both kinds, and freeze-not-skip on
// `mls_pull` (its ack PURGES the source, so a skipped record is
// irrecoverable loss).

/// A federation `channel.fetch` page closes early before the record that
/// would overflow the 2 MiB frame; the follow-up fetch serves the rest —
/// nothing is skipped (a skipped record would vanish from the foreign
/// member's cursor-ordered walk: silent chat loss).
#[tokio::test]
async fn fed_channel_fetch_page_closes_early_on_the_frame_budget() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id = h_state.nest_identity.public_key_bytes();

    // Bob is enrolled on F with three buffered records: two ~1.1 MB (fine
    // alone, together over the ~1.94 MiB budget — every envelope rides as a
    // byte string, so a record costs its raw size plus
    // `segments::RECORD_WIRE_OVERHEAD`) and one small.
    let bob = ActorKeypair::generate();
    let bob_id = bob.actor_id().0;
    f_state
        .db
        .create_user_with_handle(&bob_id, "free", "bob", None)
        .await
        .unwrap();
    let channel_id = [0xEEu8; 32];
    seed_channel_message(&f_state, bob_id, &channel_id, &vec![0x11u8; 1_100_000]).await;
    seed_channel_message(&f_state, bob_id, &channel_id, &vec![0x22u8; 1_100_000]).await;
    seed_channel_message(&f_state, bob_id, &channel_id, b"small").await;

    // Bob is a recorded foreign member whose home is H — the fetch gate.
    f_state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &bob_id,
            &h_nest_id,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let fetch = |after: i64, key: u8| {
        let conn = &conn;
        let bob_hex = hex::encode(bob_id);
        let channel_hex = hex::encode(channel_id);
        async move {
            let req = FedChannelFetchRequest {
                requesting_actor_id: bob_hex,
                channel_id: channel_hex,
                after,
                limit: 500,
                requesting_handle: None,
                requesting_domain: None,
            };
            let reply: FedChannelFetchReply = from_value(
                &tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    conn.dispatcher
                        .request_raw(
                            "fauna.federation.channel.fetch",
                            [key; 16],
                            to_value(&req),
                            None,
                        )
                        .await
                        .unwrap()
                        .await_reply()
                        .await
                })
                .await
                .expect(
                    "reply never arrived — an over-budget page is unframeable and \
                     stalls the pull permanently (the bug this test pins closed)",
                )
                .expect("channel.fetch ok"),
            );
            reply
        }
    };

    let page1 = fetch(0, 0x51).await;
    assert_eq!(
        page1.messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
        vec![1],
        "the page closes early before the record that would overflow the frame"
    );
    let page2 = fetch(1, 0x52).await;
    assert_eq!(
        page2.messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
        vec![2, 3],
        "nothing was skipped — the follow-up fetch serves the rest in order"
    );
}

/// An `mls_pull` page closes early on the frame budget with `up_to` = the
/// last record actually included, so the puller's contiguous ack never
/// purges past an undelivered record.
#[tokio::test]
async fn fed_mls_pull_page_closes_early_on_the_frame_budget() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id = h_state.nest_identity.public_key_bytes();

    let bob = ActorKeypair::generate();
    let bob_id = bob.actor_id().0;
    f_state
        .db
        .create_user_with_handle(&bob_id, "free", "bob", None)
        .await
        .unwrap();
    let channel_id = [0xEFu8; 32];
    // Same ~1.1 MB calibration as `fed_channel_fetch_page_closes_early_on_the_frame_budget`
    // above — see its comment.
    seed_channel_message(&f_state, bob_id, &channel_id, &vec![0x11u8; 1_100_000]).await;
    seed_channel_message(&f_state, bob_id, &channel_id, &vec![0x22u8; 1_100_000]).await;
    seed_channel_message(&f_state, bob_id, &channel_id, b"small").await;
    f_state
        .db
        .store_pairing(&bob_id, &h_nest_id, &["sync".to_string()], None, None, None)
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let pull = |since_seq: i64, key: u8| {
        let conn = &conn;
        let bob_hex = hex::encode(bob_id);
        async move {
            let req = FedMlsPullRequest {
                actor_id: bob_hex,
                since_seq,
            };
            let reply: FedMlsPullReply = from_value(
                &tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    conn.dispatcher
                        .request_raw(
                            "fauna.federation.sync.mls_pull",
                            [key; 16],
                            to_value(&req),
                            None,
                        )
                        .await
                        .unwrap()
                        .await_reply()
                        .await
                })
                .await
                .expect(
                    "reply never arrived — an over-budget page is unframeable and \
                     stalls the relay permanently (the bug this test pins closed)",
                )
                .expect("mls_pull ok"),
            );
            reply
        }
    };

    let page1 = pull(0, 0x61).await;
    assert_eq!(
        page1.messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
        vec![1],
        "the page closes early before the record that would overflow the frame"
    );
    assert_eq!(page1.up_to, 1, "up_to = the last record actually included");
    let page2 = pull(1, 0x62).await;
    assert_eq!(
        page2.messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
        vec![2, 3],
        "nothing was skipped — the follow-up pull serves the rest in order"
    );
    assert_eq!(page2.up_to, 3);
}

/// A single stored record whose wire size alone exceeds the frame budget
/// freezes the `mls_pull` page (records empty, `up_to` unmoved) instead of
/// being skipped: the puller's ack PURGES the source, so a skipped record
/// the cursor passes is irrecoverable loss — freeze loudly, never skip
/// (the conv twin of `a_single_over_frame_record_freezes_the_pull_instead_of_
/// skipping` on the mail relay).
#[tokio::test]
async fn fed_mls_pull_freezes_on_a_single_over_frame_record_instead_of_skipping() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id = h_state.nest_identity.public_key_bytes();

    let bob = ActorKeypair::generate();
    let bob_id = bob.actor_id().0;
    f_state
        .db
        .create_user_with_handle(&bob_id, "free", "bob", None)
        .await
        .unwrap();
    let channel_id = [0xFAu8; 32];
    // One record over the whole 2 MiB frame by itself, then a small one the
    // freeze must NOT leapfrog to. The over-frame record is injected DIRECTLY into
    // the store (bypassing `channel.send`'s serve-page-budget door, which now
    // refuses it) — that is the only way such a record reaches the store, and the
    // pull-side freeze is exactly the defense-in-depth this test proves. The small
    // record still rides the real `channel.send` path.
    seed_channel_record_direct(&f_state, bob_id, &channel_id, &vec![0x11u8; 2_200_000]).await;
    seed_channel_message(&f_state, bob_id, &channel_id, b"small").await;
    f_state
        .db
        .store_pairing(&bob_id, &h_nest_id, &["sync".to_string()], None, None, None)
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let req = FedMlsPullRequest {
        actor_id: hex::encode(bob_id),
        since_seq: 0,
    };
    let reply: FedMlsPullReply = from_value(
        &tokio::time::timeout(std::time::Duration::from_secs(30), async {
            conn.dispatcher
                .request_raw(
                    "fauna.federation.sync.mls_pull",
                    [0x63u8; 16],
                    to_value(&req),
                    None,
                )
                .await
                .unwrap()
                .await_reply()
                .await
        })
        .await
        .expect("reply never arrived — an over-budget page is unframeable")
        .expect("mls_pull ok"),
    );
    assert!(
        reply.messages.is_empty(),
        "the over-frame head record freezes the page — it is never skipped \
         (ack purges the source; a skipped record is irrecoverable loss)"
    );
    assert_eq!(
        reply.up_to, 0,
        "up_to unmoved — the ack cannot purge past it"
    );
}

/// **Slice-4 originator: discovery-feed query over the channel (§6).** The
/// discovery poller's peer query now goes channel-first
/// (`peer_query::query_peer_channel_first` → `federation_pool::originate_feed_query`).
/// H seeds nothing locally; F holds a tagged post in its index. H queries F over
/// the channel (the sole carrier since slice 5), and the poller entry point
/// converts the served candidate into the local `ScoredCandidate` shape carrying
/// F's post.
#[tokio::test]
async fn feed_query_originates_over_the_channel() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;

    // Seed a matching post in F's index — the same write the poller makes when
    // ingesting a remote post.
    let author = [0xA1u8; 32];
    let post_id = [0xB2u8; 32];
    f_state
        .db
        .insert_post_index_entry(
            &post_id,
            &author,
            1_000,
            false,
            false,
            "local",
            &["rust".to_string()],
        )
        .await
        .unwrap();

    let req = RemoteQueryRequest {
        rules: vec![FilterRule::HasHashtag {
            tags: vec!["rust".into()],
        }],
        combination: "all".into(),
        authors: Some(vec![hex::encode(author)]),
        limit: Some(50),
        cursor: None,
    };

    // Low-level: the channel served the query.
    fauna_nest::federation_pool::originate_feed_query(
        &h_state.federation_pool,
        &h_state,
        &f_url,
        &req,
    )
    .await
    .expect("channel feed.query");

    // End-to-end via the poller entry point: the served candidate converts into a
    // `ScoredCandidate` carrying F's tagged post.
    let candidates = fauna_nest::peer_query::query_peer_channel_first(
        &h_state,
        &f_url,
        &[FilterRule::HasHashtag {
            tags: vec!["rust".into()],
        }],
        FilterCombination::All,
        &[ActorId(author)],
        50,
        None,
    )
    .await
    .expect("channel-first feed query");
    assert_eq!(
        candidates.len(),
        1,
        "the channel-served feed query returns F's matching post"
    );
    assert!(
        candidates[0].metadata.tags.iter().any(|t| t == "rust"),
        "the converted candidate carries the post's tag"
    );
}

/// **Client→home-nest social inbox send originates over the channel
/// (the bearer leg).** The authed
/// `fauna.inbox.send` kind with a cross-nest `recipient_nest_url` makes the home
/// nest H originate `fauna.federation.inbox.deliver` to F — the full Spec-Y2 path
/// (clients reach remote actors *through* their home nest, never by POSTing the
/// remote nest directly, the way the retiring HTTP twin allowed). The signed
/// `(CR, Post)` tuple lands in the recipient's inbox on **F**, not H — proving
/// both the locality decision (Some(nest_url) ⇒ federate) and that the send
/// handler drives `originate_inbox_deliver` end-to-end over the real channel.
#[tokio::test]
async fn inbox_send_cross_nest_originates_over_the_channel() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;

    // H's per-actor inbox surface (the bearer router the client calls).
    let router = {
        let mut b = fauna_nest::rpc_router::RpcRouter::builder();
        fauna_nest::inbox_handlers::register_inbox_handlers(&mut b);
        b.build()
    };

    let sender = ActorKeypair::from_secret([0x11u8; 32]);
    let caller = sender.actor_id().0; // sender-binding: caller == cr.sender
    let recipient = ActorId([0x77u8; 32]);
    f_state
        .db
        .create_user_with_handle(&recipient.0, "free", "recipient", None)
        .await
        .unwrap();
    // The recipient accepts strangers; this test is about the federation leg, not
    // the reach floor.
    f_state
        .db
        .set_inbox_mode(&recipient.0, "open")
        .await
        .unwrap();
    let payload = build_inbox_payload(&sender, &recipient, "note/v1");

    let req = InboxSendRequest {
        extra: Default::default(),
        recipient_actor_id: hex::encode(recipient.0),
        recipient_nest_url: Some(f_url.clone()),
        payload_bytes: payload,
    };
    common::seed_dispatch_actor(&h_state.db, &caller).await;
    let meta = router
        .kind_meta("fauna.inbox.send")
        .expect("send registered");
    let reply_bytes = (meta.handler)(
        h_state.clone(),
        caller,
        bytes::Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect("cross-nest send originates + delivers over the channel");
    let reply: InboxSendReply = decode_strict(&reply_bytes).unwrap();
    assert!(
        reply.inbox_id.is_some(),
        "the payload is delivered on F → the peer's inbox row id"
    );

    // The row landed on F (the recipient's home nest), NOT on H.
    let f_inbox = f_state.db.list_inbox_all(&recipient.0).await.unwrap();
    assert_eq!(f_inbox.len(), 1, "delivered to the recipient on F");
    let h_inbox = h_state.db.list_inbox_all(&recipient.0).await.unwrap();
    assert!(h_inbox.is_empty(), "nothing delivered locally on H");
}

/// **A cross-nest send to a stranger lands as a KNOCK on the peer — the
/// contacts-outcome-5 path.** The sibling test above proves the federation leg
/// carries a *delivered* inbox row; it seeds the recipient `"open"` precisely so
/// the reach floor stays out of the way. That leaves the combination contacts
/// actually takes — `InboxMode::AllowKnock` (the `#[default]`) plus
/// `ArrivalOrigin::Federation` — with no assertion anywhere, even though the
/// receiving nest runs the identical `deliver_inbox_payload_core` for both
/// origins (`federation_handlers::inbox_deliver_handler` →
/// `routes::deliver_inbox_payload_core` → `store_knock`).
///
/// Three things are pinned here that a delivered-row test cannot pin:
///
/// 1. the reply's `inbox_id` is `None` — and that null is **success**, the
///    single most invertible fact on this path (a client that reads it as a
///    failure shows the user an error for a knock that was stored fine);
/// 2. the knock is on **F**, from the right sender, with F's own knock queue as
///    the witness — not an inbox row on F, and nothing at all on H;
/// 3. the reach floor does not silently eat a *federated* stranger's knock,
///    which is what `ArrivalOrigin::Federation` exists to distinguish.
#[tokio::test]
async fn inbox_send_cross_nest_stores_a_knock_for_a_stranger() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;

    let router = {
        let mut b = fauna_nest::rpc_router::RpcRouter::builder();
        fauna_nest::inbox_handlers::register_inbox_handlers(&mut b);
        b.build()
    };

    let sender = ActorKeypair::from_secret([0x21u8; 32]);
    let caller = sender.actor_id().0; // sender-binding: caller == cr.sender
    let recipient = ActorId([0x87u8; 32]);
    f_state
        .db
        .create_user_with_handle(&recipient.0, "free", "recipient", None)
        .await
        .unwrap();

    // Deliberately NO `set_inbox_mode` — `allow_knock` is `InboxMode`'s
    // `#[default]`, and taking it by default is the point: this is the state a
    // real fresh account on F is in when a stranger on H adds them.
    assert_eq!(
        f_state.db.get_inbox_mode(&recipient.0).await.unwrap(),
        "allow_knock",
        "a fresh actor defaults to allow_knock — the mode this test is about"
    );

    let req = InboxSendRequest {
        extra: Default::default(),
        recipient_actor_id: hex::encode(recipient.0),
        recipient_nest_url: Some(f_url.clone()),
        payload_bytes: build_inbox_payload(&sender, &recipient, "note/v1"),
    };
    common::seed_dispatch_actor(&h_state.db, &caller).await;
    let meta = router
        .kind_meta("fauna.inbox.send")
        .expect("send registered");
    let reply_bytes = (meta.handler)(
        h_state.clone(),
        caller,
        bytes::Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect("a cross-nest knock to a stranger is accepted, not refused");
    let reply: InboxSendReply = decode_strict(&reply_bytes).unwrap();
    assert!(
        reply.inbox_id.is_none(),
        "a stored knock replies inbox_id: null — that null is success, not failure"
    );

    // The knock landed on F, from this sender.
    let knocks = f_state.db.poll_knocks(&recipient.0).await.unwrap();
    assert_eq!(
        knocks.len(),
        1,
        "exactly one pending knock on the recipient's own nest"
    );
    assert_eq!(
        knocks[0].sender_id, caller,
        "the knock is attributed to the cross-nest sender"
    );

    // ...and it is a KNOCK, not an inbox row — on either nest.
    assert!(
        f_state
            .db
            .list_inbox_all(&recipient.0)
            .await
            .unwrap()
            .is_empty(),
        "allow_knock stores a knock instead of delivering an inbox row"
    );
    assert!(
        h_state
            .db
            .list_inbox_all(&recipient.0)
            .await
            .unwrap()
            .is_empty(),
        "nothing is delivered locally on the sender's nest"
    );
    assert!(
        h_state
            .db
            .poll_knocks(&recipient.0)
            .await
            .unwrap()
            .is_empty(),
        "and no knock is stored locally on the sender's nest either"
    );
}

/// **The exchange originator plane (federation.md § the originator-gap note;
/// 2026-07-12 distributed-moderation plan, Phase 1).** One
/// `run_exchange_cycle` on H, with F seeded as a discovery contributor:
/// H **pushes** its local ≥k report aggregate to F,
/// **pulls** F's back through the same import path the serving handlers use,
/// records F as a prior exchange partner (and never records a
/// self-referential contributor row), and a second immediate cycle is
/// per-peer throttled. Nothing here issues a manual exchange RPC — the cycle
/// is the production code path the spawned worker drives.
#[tokio::test]
async fn exchange_originator_cycle_pushes_pulls_records_and_throttles() {
    use fauna_nest::db::reports::{ReportKey, capture_report};
    use fauna_nest::exchange_originator::run_exchange_cycle;

    let (h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;

    // Local ≥k report aggregates on BOTH nests (distinct hashes). No stored
    // item is needed: the exchange
    // rides `content_reports`, and `gated_report_score` reads the aggregate
    // directly.
    let hash_h = [0x51u8; 32];
    let hash_f = [0x52u8; 32];
    for (state, hash) in [(&h_state, hash_h), (&f_state, hash_f)] {
        let key = ReportKey {
            content_hash: hash,
            factor: "report:spam".into(),
            content_kind: "mail".into(),
        };
        for reporter in [[0x01u8; 32], [0x02; 32], [0x03; 32]] {
            state.db.set_share_reports(&reporter, true).await.unwrap();
            capture_report(&state.db, &reporter, &key, true)
                .await
                .unwrap();
        }
    }
    // H's peer set: F via a discovery contributor row, plus a
    // self-referential row that must be skipped, not self-dialed.
    h_state
        .db
        .upsert_contributor("feed-x", &f_url, None, "manual")
        .await
        .unwrap();
    h_state
        .db
        .upsert_contributor("feed-x", &h_url, None, "manual")
        .await
        .unwrap();

    let mut last_attempt = std::collections::HashMap::new();
    let stats = run_exchange_cycle(&h_state, &mut last_attempt).await;
    assert_eq!(stats.failed, 0, "both peers resolve (one is a self-skip)");

    // PULL: F's aggregates landed on H as the flat peer bucket (H has no
    // local reporters on hash_f → peer-only = exactly 100‰).
    assert_eq!(
        h_state
            .db
            .gated_report_score(&hash_f, "report:spam")
            .await
            .unwrap(),
        Some(100),
        "pulled peer aggregate = flat corroboration bucket"
    );

    // PUSH: H's aggregates landed on F symmetrically.
    assert_eq!(
        f_state
            .db
            .gated_report_score(&hash_h, "report:spam")
            .await
            .unwrap(),
        Some(100),
        "pushed aggregate = flat corroboration bucket on the peer"
    );

    // NO LAUNDERING round trip: H's own local aggregate stays purely local
    // (3 local reporters = 200‰, no peer bucket — F never re-exports it).
    assert_eq!(
        h_state
            .db
            .gated_report_score(&hash_h, "report:spam")
            .await
            .unwrap(),
        Some(200),
        "own local consensus untouched by the exchange"
    );

    // Partner memory: F recorded (the prior-partner peer source), the
    // self-referential URL not.
    assert_eq!(
        h_state.db.list_exchange_peer_urls().await.unwrap(),
        vec![f_url.clone()],
        "successful exchange recorded exactly the real peer"
    );

    // Per-peer origination throttle: an immediate second cycle exchanges with
    // nobody and reports when the earliest peer becomes due again.
    let stats2 = run_exchange_cycle(&h_state, &mut last_attempt).await;
    assert_eq!(stats2.exchanged, 0, "min-peer-interval throttle holds");
    assert!(
        stats2.throttled_until.is_some(),
        "a retry instant is surfaced"
    );
}

/// **Slice 4 — the import-triggered `post.get` fetch surfaces a peer-only
/// trending post** (`trending.md` § Import-triggered fetch). A (`f`) hosts a
/// PUBLIC post with 3 fresh local likers → its local trending row rises and
/// exports. B (`h`) has never seen the post, so the pulled trend entry alone
/// yields NO blind row; B's exchange cycle then pulls A's trends, imports the
/// presence bit, FETCHES the post via `fauna.federation.post.get`, verifies +
/// ingests it (content-addressed, signature-checked, public-only), and recomputes
/// — so B now carries a `trending` row scored by the single-peer ramp (100‰, B
/// has zero local engagement of its own). Because B has no local engagers for the
/// fetched post, B never RE-exports it — the fetch launders nothing.
#[tokio::test]
async fn trends_originator_fetches_and_surfaces_peer_only_post() {
    use fauna_nest::exchange_originator::exchange_with_peer;

    // b = the fetching nest; a = the origin hosting the hot post.
    let (_b_url, b_state) = start_nest().await;
    let (a_url, a_state) = start_nest().await;

    // A ingests one PUBLIC post (signed → `content_meta.gated_tier IS NULL`) via
    // the real `put_post` path, then 3 distinct fresh likes → local_pm 130
    // (v ≈ 3 → round(3000/23); decay over the test's wall-clock is negligible
    // against the 6 h half-life) — matching the Slice-3 export test.
    let kp = ActorKeypair::generate();
    let post = fauna_core::data::Post {
        author: kp.actor_id(),
        created_at: fauna_core::data::Timestamp(1_700_000_000),
        body: fauna_core::data::PostBody::Text {
            content: "hot peer post".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let bytes = fauna_core::encoding::sign_and_pack(&kp, &post).unwrap();
    // A native post rests under `blake3` of the exact wire bytes it was created
    // as (`routes::ingest_post_core`) — the id A exports and the id B must bind
    // the fetched bytes to. Keying this fixture by the envelope's inner CID
    // digest instead once hid a dead fetch leg: no native post carries that id.
    let digest: [u8; 32] = *blake3::hash(&bytes).as_bytes();
    a_state.db.put_post(&digest, &bytes, None).await.unwrap();
    let now_us = fauna_core::data::Timestamp::now().as_i64();
    for actor in [0x01u8, 0x02, 0x03] {
        let mut event_id = digest;
        event_id[0] = 0xE0;
        event_id[1] = actor;
        a_state
            .db
            .insert_engagement_event(&event_id, &digest, Some(&[actor; 32]), "like", None, now_us)
            .await
            .unwrap();
    }

    let trend_score = |db: Arc<CacheDb>, id: [u8; 32]| async move {
        db.get_content_scores(&id)
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.factor == "trending")
            .map(|e| e.score)
    };
    assert_eq!(
        trend_score(a_state.db.clone(), digest).await,
        Some(130),
        "A: 3 fresh local likes → local_pm"
    );
    // B has never seen the post: no body, no trending row.
    assert_eq!(
        trend_score(b_state.db.clone(), digest).await,
        None,
        "B has not seen the post before the exchange"
    );
    assert!(
        b_state.db.get_post(&digest).await.unwrap().is_none(),
        "B holds no body for the post before the exchange"
    );

    // Seed A as one of B's discovery contributors so the production cycle resolves
    // it, then run the real cross-nest exchange (push/pull/fetch) B → A.
    b_state
        .db
        .upsert_contributor("feed-hot", &a_url, None, "manual")
        .await
        .unwrap();
    exchange_with_peer(&b_state, &a_url).await.unwrap();

    // B pulled A's trend presence bit, FETCHED the post over `post.get`, verified
    // + ingested it, and recomputed → B's trending row = the single-peer ramp
    // (100‰). B has zero local engagement of its own, so local_pm = 0 and the
    // whole score is the flat one-peer bucket.
    assert_eq!(
        trend_score(b_state.db.clone(), digest).await,
        Some(100),
        "B: peer-only post fetched + ingested → single-peer ramp (no local velocity)"
    );
    // The fetch actually ingested the body (not just the presence bit).
    assert!(
        b_state.db.get_post(&digest).await.unwrap().is_some(),
        "the fetched post body is now resident on B"
    );

    // No laundering: B has no local engagers for the fetched post, so its own
    // export is empty — the peer-only post is never re-exported as B's.
    let b_export = b_state.db.export_trend_entries(now_us).await.unwrap();
    assert!(
        b_export.is_empty(),
        "B re-exports nothing — a fetched peer-only post has no local consensus"
    );
}

/// A row of another plane — an inbox message — resting in the shared `content`
/// table under the digest of a signed post's CID: the very id a signed wire of
/// that post names. (The retired group plane keyed its messages this way; the
/// shared table and its one-id-one-plane guards outlive it.)
struct SeededForeignPlaneRow {
    message_id: [u8; 32],
    payload: Vec<u8>,
    author: ActorKeypair,
    post: fauna_core::data::Post,
}

async fn seed_inbox_row_at_a_posts_id(state: &AppState, text: &str) -> SeededForeignPlaneRow {
    let author = ActorKeypair::generate();
    let post = fauna_core::data::Post {
        author: author.actor_id(),
        created_at: fauna_core::data::Timestamp::now(),
        body: fauna_core::data::PostBody::Text {
            content: text.into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let message_id = fauna_core::encoding::compute_post_id(&post)
        .unwrap()
        .digest();
    let payload = format!("members-only: {text}").into_bytes();
    {
        let conn = state.db.conn().await;
        fauna_nest::db::content::insert_content(
            &conn,
            &message_id,
            "inbox/message",
            &author.actor_id().0,
            fauna_core::data::Timestamp::now().as_i64(),
            &payload,
            None,
            "fauna",
            None,
        )
        .unwrap();
    }
    SeededForeignPlaneRow {
        message_id,
        payload,
        author,
        post,
    }
}

/// **Another plane's row is not a post: the peer post fetch refuses it.** The
/// `content` table is shared, so the post read once found a non-post row by id
/// and handed any peer nest its members-only bytes. The post read answers for
/// post rows only.
#[tokio::test]
async fn post_get_refuses_another_planes_row_over_channel() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    let seeded = seed_inbox_row_at_a_posts_id(&f_state, "words for members only").await;

    let req = FedPostGetRequest {
        post_id: hex::encode(seeded.message_id),
    };
    let err = conn
        .dispatcher
        .request_raw("fauna.federation.post.get", [4u8; 16], to_value(&req), None)
        .await
        .unwrap()
        .await_reply()
        .await
        .expect_err("an inbox message is not a post — the peer fetch must not serve it");
    assert_eq!(err.code, "fauna.federation.not_found");

    let db_conn = f_state.db.conn().await;
    assert_eq!(
        fauna_nest::db::content::get_content(&db_conn, &seeded.message_id)
            .unwrap()
            .map(|(p, _)| p),
        Some(seeded.payload.clone()),
        "beside-control: the refusal left the row as it was"
    );
}

/// **A hostile peer cannot alias another plane's stored row through the trend
/// fetch** (`trending.md` § Import-triggered public-post fetch). Such a row's id
/// can be the digest of a signed post's CID — the same inner CID a signed wire
/// of that post carries in its envelope. So a peer holding such a wire hosts it
/// as a post under that id, gives it enough engagement to export, and waits for
/// B to fetch it. B must refuse: the fetched bytes do not hash to the requested
/// id. B's row keeps its payload and schema, and gains no `content_meta` row
/// (the row a takedown matches, and the feeds and every off-box publisher gate
/// on) and no search entry.
#[tokio::test]
async fn trends_fetch_never_aliases_another_planes_row() {
    use fauna_nest::exchange_originator::exchange_with_peer;

    let (_b_url, b_state) = start_nest().await;
    let (a_url, a_state) = start_nest().await;

    let seeded = seed_inbox_row_at_a_posts_id(&b_state, "quietinboxbeacon for members only").await;

    // Hostile A: the row's id names a signed post, hosted as a post under that
    // id, with 3 fresh likers so it exports.
    let wire = fauna_core::encoding::sign_and_pack(&seeded.author, &seeded.post).unwrap();
    a_state
        .db
        .put_post(&seeded.message_id, &wire, None)
        .await
        .unwrap();
    let now_us = fauna_core::data::Timestamp::now().as_i64();
    for actor in [0x01u8, 0x02, 0x03] {
        let mut event_id = seeded.message_id;
        event_id[0] = 0xE1;
        event_id[1] = actor;
        a_state
            .db
            .insert_engagement_event(
                &event_id,
                &seeded.message_id,
                Some(&[actor; 32]),
                "like",
                None,
                now_us,
            )
            .await
            .unwrap();
    }
    assert!(
        a_state
            .db
            .export_trend_entries(now_us)
            .await
            .unwrap()
            .iter()
            .any(|(content_id, ..)| *content_id == seeded.message_id),
        "precondition: A exports the aliasing id"
    );

    b_state
        .db
        .upsert_contributor("feed-hot", &a_url, None, "manual")
        .await
        .unwrap();
    exchange_with_peer(&b_state, &a_url).await.unwrap();

    let conn = b_state.db.conn().await;
    let imported: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM peer_content_trends WHERE content_id = ?1",
            [seeded.message_id.as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        imported, 1,
        "precondition: B imported the presence bit, so the id was fetched as unseen"
    );
    let schema: String = conn
        .query_row(
            "SELECT schema FROM content WHERE id = ?1",
            [seeded.message_id.as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schema, "inbox/message", "the row is still an inbox message");
    assert_eq!(
        fauna_nest::db::content::get_content(&conn, &seeded.message_id)
            .unwrap()
            .map(|(p, _)| p),
        Some(seeded.payload.clone()),
        "the member-only payload is intact, not wiped by a post projection"
    );
    drop(conn);

    assert!(
        !b_state
            .db
            .content_meta_exists(&seeded.message_id)
            .await
            .unwrap(),
        "no content_meta row: nothing for a takedown, a feed or an off-box publisher to key on"
    );
    assert!(
        b_state
            .db
            .search_fts("quietinboxbeacon", None, None, None, 10, 0)
            .await
            .unwrap()
            .is_empty(),
        "the message text never reaches the search index"
    );
}

// ── Phase 2: cross-nest shared folders + channel append (federation.md
// § Cross-nest shared folders + channel append, ratified 2026-07-18) ────────
//
// The four net-new kinds share ONE structural gate — serve iff
// `foreign_member_home_nest(channel_id, requester) == origin_nest_id` (the row
// THIS nest wrote at Welcome-relay time; a nest signature is attribution, never
// authorization). These tests drive each kind over a real dialed channel between
// two in-process nests, seeding the home nest's state directly (the client-level
// two-nest choreography lives in `conformance_cross_nest_shared_folders.rs`).

use fauna_nest::federation_handlers::{
    FedChannelAppendReply, FedChannelAppendRequest, FedChannelLeaveReply, FedChannelLeaveRequest,
    FedFolderActorsFetchReply, FedFolderActorsFetchRequest, FedFolderChangesFetchReply,
    FedFolderChangesFetchRequest, FedFolderChangesRecordReply, FedFolderChangesRecordRequest,
    FedFolderContentKeyFetchReply, FedFolderContentKeyFetchRequest, FedFolderReadTokenMintReply,
    FedFolderReadTokenMintRequest, FedFolderWriteTokenMintReply, FedFolderWriteTokenMintRequest,
};

/// Seed the home nest F with a claimed, group-bound shared folder owned by
/// `owner` — born under the client-minted set nonce ([`common::SET_NONCE`])
/// every writer signs its records under — plus one recorded change and a
/// published content-key envelope. Returns the derived channel id.
async fn seed_claimed_shared_set(
    f_state: &Arc<AppState>,
    owner: &[u8; 32],
    name: &str,
) -> [u8; 32] {
    let group_id = vec![0x5du8; 24];
    let channel_id = fauna_mls::types::ChannelId::from_group_id(&group_id).0;
    let fs_id = f_state
        .db
        .create_folder_with_options(
            name,
            owner,
            fauna_nest::db::FolderOptions {
                set_nonce: Some(common::SET_NONCE.to_vec()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        f_state
            .db
            .set_folder_mls_group(name, owner, Some(&group_id))
            .await
            .unwrap()
    );
    assert!(matches!(
        f_state
            .db
            .claim_folder_channel(owner, &channel_id)
            .await
            .unwrap(),
        fauna_nest::db::channels::ChannelClaimOutcome::Allowed
    ));
    f_state
        .db
        .record_sync_change(
            owner,
            &[0x11u8; 32],
            Some(&[0x22u8; 32]),
            1234,
            "created",
            Some(fs_id),
            None,
            Some("docs/hello.txt"),
        )
        .await
        .unwrap();
    f_state
        .db
        .upsert_folder_content_key(&channel_id, 3, b"sealed-envelope-bytes", 1)
        .await
        .unwrap();
    channel_id
}

/// Sign a federated record as the foreign writer's engine does before its home
/// nest relays it: the writer's identity key over the record's
/// `SignedChange` statement (the same statement a same-nest
/// `fauna.sync.changes.record` carries — built here through that request
/// shape), under the set-home's stored nonce ([`common::SET_NONCE`]). A
/// direct signature needs no inline cert. An unsigned record is refused
/// `signature_required`.
fn signed_fed_record(
    mut req: FedFolderChangesRecordRequest,
    kp: &fauna_core::identity::ActorKeypair,
) -> FedFolderChangesRecordRequest {
    let mut statement_req = fauna_protocol::sync::SyncChangeRecordRequest {
        device_id: req.device_id.clone(),
        path: req.path.clone(),
        manifest_hash: req.manifest_hash.clone(),
        size_bytes: req.size_bytes,
        change_type: req.change_type.clone(),
        content_key_version: req.content_key_version,
        thumbnail_hash: req.thumbnail_hash.clone(),
        path_sealed: req.path_sealed.clone(),
        derived_through: req.derived_through,
        is_resolution: req.is_resolution,
        ..Default::default()
    };
    common::sign_record(&mut statement_req, kp, common::SET_NONCE);
    req.signature = statement_req.signature;
    req.signer_key = statement_req.signer_key;
    req
}

/// `fauna.federation.folder.changes.fetch` + `content_key.fetch`: a recorded
/// foreign member (bound to the requesting nest) reads the set's change log and
/// sealed content-key envelope over the channel; a requester with no foreign row
/// — or a row bound to a DIFFERENT home nest — is refused by the structural gate.
#[tokio::test]
async fn folder_read_plane_over_channel_serves_member_and_refuses_wrong_binding() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id_bytes = h_state.nest_identity.public_key_bytes();

    let owner = [0xa1u8; 32];
    let member = [0xb2u8; 32]; // lives on H
    let stranger = [0xc3u8; 32]; // no foreign row at all
    let channel_id = seed_claimed_shared_set(&f_state, &owner, "shared-docs").await;
    f_state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &member,
            &h_nest_id_bytes,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    // The member's home nest relays changes.fetch → the recorded change rows.
    let req = FedFolderChangesFetchRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(channel_id),
        after: 0,
        limit: 0,
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.changes.fetch",
            [0x51u8; 16],
            to_value(&req),
            None,
        )
        .await
        .unwrap();
    let reply: FedFolderChangesFetchReply = from_value(&call.await_reply().await.expect("ok"));
    assert_eq!(reply.changes.len(), 1, "the one recorded change is served");
    let c = &reply.changes[0];
    assert_eq!(c.path.as_deref(), Some("docs/hello.txt"));
    assert_eq!(c.size_bytes, 1234);
    assert_eq!(
        c.author_actor_id.as_deref(),
        Some(hex::encode(owner).as_str()),
        "nest-stamped attribution rides the federated read too"
    );

    // content_key.fetch → the sealed envelope + epoch, opaque to both nests.
    let ck_req = FedFolderContentKeyFetchRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(channel_id),
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.content_key.fetch",
            [0x52u8; 16],
            to_value(&ck_req),
            None,
        )
        .await
        .unwrap();
    let reply: FedFolderContentKeyFetchReply = from_value(&call.await_reply().await.expect("ok"));
    assert_eq!(reply.epoch, 3);
    assert_eq!(reply.sealed, hex::encode(b"sealed-envelope-bytes"));
    // The federated floor (`on-demand-files.md` § Shared sets on a capability
    // host → *One mechanism*, question 2): the home nest stamps the set's
    // owner-stamped `content_key_floor` — the same value `fauna.folders.list`
    // projects — so a cross-nest member's engine can hold its writes behind it
    // instead of learning of a rotation only from this nest's refusal.
    assert_eq!(
        reply.content_key_floor,
        Some(1),
        "the seed's floor (`current_version = 1`) rides the federated content-key read"
    );

    // A requester with NO foreign row is refused (structural gate).
    let bad = FedFolderChangesFetchRequest {
        requesting_actor_id: hex::encode(stranger),
        channel_id: hex::encode(channel_id),
        after: 0,
        limit: 0,
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.changes.fetch",
            [0x53u8; 16],
            to_value(&bad),
            None,
        )
        .await
        .unwrap();
    let err = call
        .await_reply()
        .await
        .expect_err("no foreign row → refused");
    assert_eq!(err.code, "fauna.federation.forbidden");

    // A member bound to a DIFFERENT home nest is refused for THIS origin —
    // a hostile signer cannot harvest a channel it does not host.
    let elsewhere = [0xd4u8; 32];
    f_state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &elsewhere,
            &[0x77u8; 32],
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
    let bad = FedFolderContentKeyFetchRequest {
        requesting_actor_id: hex::encode(elsewhere),
        channel_id: hex::encode(channel_id),
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.content_key.fetch",
            [0x54u8; 16],
            to_value(&bad),
            None,
        )
        .await
        .unwrap();
    let err = call
        .await_reply()
        .await
        .expect_err("wrong home-nest binding → refused");
    assert_eq!(err.code, "fauna.federation.forbidden");
}

/// Phase 3 write plane: `changes.record` + `write_token.mint`
/// take the structural foreign-member gate AND owner-granted `access ==
/// 'writer'`. A rostered **reader** (structural gate passes, writer gate fails)
/// and a **stranger** (no foreign row) are both refused at BOTH kinds; a
/// **writer** records (the change lands, nest-stamped to them) and mints a
/// write-only token; a re-relayed record is content-idempotent (same seq, no
/// second row).
#[tokio::test]
async fn folder_write_plane_gates_on_writer_and_is_content_idempotent() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id_bytes = h_state.nest_identity.public_key_bytes();

    let owner = [0xa1u8; 32];
    let reader = [0xb2u8; 32]; // foreign member, no writer grant
    // Foreign member WITH writer grant — a real keypair: its records are
    // signed by its identity key (an unsigned record is refused
    // `signature_required`).
    let writer_kp = common::signing_actor(0xe5);
    let writer = writer_kp.actor_id().0;
    let stranger = [0xc3u8; 32]; // no foreign row at all

    // The owner has a real account on F (the set-home nest); the cross-nest
    // members deliberately do NOT (owner-pays: a foreign writer has no account
    // on the storing nest — `file-sync.md` § Multi-writer shared sets).
    f_state
        .db
        .create_user(&owner, "free", "owner")
        .await
        .unwrap();
    let channel_id = seed_claimed_shared_set(&f_state, &owner, "shared-docs").await;
    f_state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &reader,
            &h_nest_id_bytes,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
    f_state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &writer,
            &h_nest_id_bytes,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
    // The owner's claimant-gated grant — the authorization the write gate checks.
    f_state
        .db
        .set_folder_member_access(&channel_id, &writer, "writer", Some(1_000_000))
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    // Helpers to build a record for a given actor + a mint for a given actor.
    let record_req = |actor: [u8; 32], path: &str| FedFolderChangesRecordRequest {
        requesting_actor_id: hex::encode(actor),
        channel_id: hex::encode(channel_id),
        device_id: hex::encode([0xd0u8; 32]),
        path: path.to_string(),
        manifest_hash: Some(hex::encode([0x9au8; 32])),
        size_bytes: 4096,
        change_type: "create".to_string(),
        // ≥ the seed's floor (current_version = 1) — fresh, so the version-floor
        // check admits it.
        content_key_version: Some(3),
        thumbnail_hash: None,
        path_sealed: Some(serde_bytes::ByteBuf::from(b"e2e-synthetic-seal".to_vec())),
        derived_through: None,
        is_resolution: None,
        ..Default::default()
    };
    let mint_req = |actor: [u8; 32]| FedFolderWriteTokenMintRequest {
        requesting_actor_id: hex::encode(actor),
        channel_id: hex::encode(channel_id),
    };

    // ── (b) A rostered READER is refused at record AND at mint. ──
    for (i, (kind, payload)) in [
        (
            "fauna.federation.folder.changes.record",
            to_value(&record_req(reader, "docs/reader.txt")),
        ),
        (
            "fauna.federation.folder.write_token.mint",
            to_value(&mint_req(reader)),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let call = conn
            .dispatcher
            .request_raw(kind, [0x60u8 + i as u8; 16], payload, None)
            .await
            .unwrap();
        let err = call
            .await_reply()
            .await
            .expect_err("a reader is refused at the write kinds");
        assert_eq!(
            err.code, "fauna.federation.forbidden",
            "reader refused at {kind} with the structural-writer gate"
        );
    }

    // ── A STRANGER (no foreign row) is refused at both too. ──
    for (i, (kind, payload)) in [
        (
            "fauna.federation.folder.changes.record",
            to_value(&record_req(stranger, "docs/stranger.txt")),
        ),
        (
            "fauna.federation.folder.write_token.mint",
            to_value(&mint_req(stranger)),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let call = conn
            .dispatcher
            .request_raw(kind, [0x70u8 + i as u8; 16], payload, None)
            .await
            .unwrap();
        let err = call
            .await_reply()
            .await
            .expect_err("a stranger is refused at the write kinds");
        assert_eq!(err.code, "fauna.federation.forbidden");
    }

    // ── (a-partial) The WRITER records: the change lands, nest-stamped to them. ──
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.changes.record",
            [0x80u8; 16],
            to_value(&signed_fed_record(
                record_req(writer, "docs/writer.txt"),
                &writer_kp,
            )),
            None,
        )
        .await
        .unwrap();
    let rec: FedFolderChangesRecordReply = from_value(&call.await_reply().await.expect("ok"));
    assert!(rec.seq > 0, "the writer's record was assigned a sequence");

    // Read it back over the fetch relay — the seed's row + the writer's, with
    // the writer's row nest-stamped to the writer (never sender-asserted).
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.changes.fetch",
            [0x81u8; 16],
            to_value(&FedFolderChangesFetchRequest {
                requesting_actor_id: hex::encode(writer),
                channel_id: hex::encode(channel_id),
                after: 0,
                limit: 0,
            }),
            None,
        )
        .await
        .unwrap();
    let listed: FedFolderChangesFetchReply = from_value(&call.await_reply().await.expect("ok"));
    // Addressed by `path_hash`, NOT the plaintext `path`: post-S9-flip the nest
    // stores a plaintext path only for a folder that RESTS plaintext paths
    // (public / web / reserved — `sync_handlers::record_change_core`'s
    // `rest_path`). This folder is an ordinary sealed shared set, so its rows
    // come back with `path: None` and a plaintext-path lookup can never match.
    let writer_path_hash = hex::encode(fauna_core::sync::path_hash("docs/writer.txt"));
    let wrote = listed
        .changes
        .iter()
        .find(|c| c.path_hash == writer_path_hash)
        .expect("the writer's change is in the log");
    assert_eq!(
        wrote.author_actor_id.as_deref(),
        Some(hex::encode(writer).as_str()),
        "the record is nest-stamped to the writer, not the owner or a sender claim"
    );
    let rows_after_first = listed.changes.len();

    // ── (P3-2) A re-relayed record with identical content is idempotent: the
    // SAME seq, and NO second row in the log (a reconnect / retry lands once). ──
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.changes.record",
            [0x82u8; 16],
            to_value(&signed_fed_record(
                record_req(writer, "docs/writer.txt"),
                &writer_kp,
            )),
            None,
        )
        .await
        .unwrap();
    let replay: FedFolderChangesRecordReply = from_value(&call.await_reply().await.expect("ok"));
    assert_eq!(
        replay.seq, rec.seq,
        "a re-relayed identical record returns the original seq"
    );
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.changes.fetch",
            [0x83u8; 16],
            to_value(&FedFolderChangesFetchRequest {
                requesting_actor_id: hex::encode(writer),
                channel_id: hex::encode(channel_id),
                after: 0,
                limit: 0,
            }),
            None,
        )
        .await
        .unwrap();
    let listed: FedFolderChangesFetchReply = from_value(&call.await_reply().await.expect("ok"));
    assert_eq!(
        listed.changes.len(),
        rows_after_first,
        "the replay appended no second row"
    );
    // Phase 4 refresh (`federation.md` § Cross-nest → Recipient-side access
    // discovery): the read reply stamps the requester's LIVE grant, resolved
    // from the same `folder_member_access` row the write gate reads. This is
    // what lets a client notice a promotion/demotion on its ordinary poll with
    // no push kind — and it must DISCRIMINATE, which is why the reader arm
    // below asserts the absent stamp rather than just "some value came back".
    assert_eq!(
        listed.caller_access.as_deref(),
        Some("writer"),
        "the home nest stamped the writer's live grant on the read reply"
    );
    // The rostered READER holds no role row, so the home nest asserts nothing.
    // `None` is "no claim", NOT "revoked" — the client keeps what it holds and
    // revocation is enforced fail-closed at the next mint/record.
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.changes.fetch",
            [0x8au8; 16],
            to_value(&FedFolderChangesFetchRequest {
                requesting_actor_id: hex::encode(reader),
                channel_id: hex::encode(channel_id),
                after: 0,
                limit: 0,
            }),
            None,
        )
        .await
        .unwrap();
    let reader_listed: FedFolderChangesFetchReply =
        from_value(&call.await_reply().await.expect("ok"));
    assert_eq!(
        reader_listed.caller_access, None,
        "no role row ⇒ the nest asserts no grant (the implicit reader default)"
    );
    // Same stamp on the content-key read — the federated read a foreign
    // member's client actually runs in production, so the refresh is live and
    // not merely latent behind an unbuilt engine read path.
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.content_key.fetch",
            [0x8bu8; 16],
            to_value(&FedFolderContentKeyFetchRequest {
                requesting_actor_id: hex::encode(writer),
                channel_id: hex::encode(channel_id),
            }),
            None,
        )
        .await
        .unwrap();
    let ck: FedFolderContentKeyFetchReply = from_value(&call.await_reply().await.expect("ok"));
    assert_eq!(
        ck.caller_access.as_deref(),
        Some("writer"),
        "every federated folder read reply carries the stamp, uniformly"
    );
    assert_eq!(
        ck.content_key_floor,
        Some(1),
        "the content-key read carries the set's floor beside the access stamp"
    );

    // ── (a-partial) The WRITER mints a write-only byte-plane token. ──
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.write_token.mint",
            [0x84u8; 16],
            to_value(&mint_req(writer)),
            None,
        )
        .await
        .unwrap();
    let mint: FedFolderWriteTokenMintReply = from_value(&call.await_reply().await.expect("ok"));
    assert!(!mint.token.is_empty(), "a write token was minted");
    assert!(mint.expires_at > 0, "the token carries an absolute expiry");
    // The minted token is a real Write-scoped bulk token bound to the writer —
    // exactly what `ChunkWriteAuth` accepts on the byte POST routes.
    let scope = f_state
        .auth
        .bulk_byte_tokens
        .validate(&mint.token)
        .await
        .expect("the minted token validates on the home nest");
    assert_eq!(
        scope.actor_id.0, writer,
        "token bound to the writer's actor"
    );
    assert_eq!(
        scope.access,
        fauna_protocol::wrapped_blob::BulkByteAccess::Write,
        "the token is write-only"
    );
    assert_eq!(
        scope.purpose,
        fauna_protocol::wrapped_blob::BulkByteMintPurpose::ForeignFolderWrite,
        "the token is audit-tagged as a federation-minted foreign-writer token"
    );
}

/// `fauna.federation.folder.read_token.mint` (`federation.md` § Cross-nest
/// shared folders + channel append → *Relay serving across nests*): the
/// read-scoped twin of `write_token.mint`, behind the structural member gate
/// ALONE. A rostered **reader** — the member `write_token.mint` refuses — is
/// minted a `Read` token of purpose `ForeignFolderRead` bound to its actor; a
/// **stranger** (no foreign row) and a member bound to a **different home
/// nest** are refused, as at every member-gated folder kind; and a channel no
/// folder claims mints nothing.
#[tokio::test]
async fn folder_read_token_mint_gates_on_membership_alone() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id_bytes = h_state.nest_identity.public_key_bytes();

    let owner = [0xa1u8; 32];
    let reader = [0xb2u8; 32]; // foreign member, no writer grant
    let stranger = [0xc3u8; 32]; // no foreign row at all
    let elsewhere = [0xd4u8; 32]; // a member, bound to another home nest
    let channel_id = seed_claimed_shared_set(&f_state, &owner, "shared-docs").await;
    let unclaimed_channel = [0x9eu8; 32];
    for (channel, actor, home) in [
        (channel_id, reader, h_nest_id_bytes),
        (channel_id, elsewhere, [0x77u8; 32]),
        (unclaimed_channel, reader, h_nest_id_bytes),
    ] {
        f_state
            .db
            .register_foreign_channel_member(&channel, &actor, &home, None, RebindPower::Standing)
            .await
            .unwrap();
    }

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let mint = |key: u8, channel: [u8; 32], actor: [u8; 32]| {
        let dispatcher = conn.dispatcher.clone();
        async move {
            dispatcher
                .request_raw(
                    "fauna.federation.folder.read_token.mint",
                    [key; 16],
                    to_value(&FedFolderReadTokenMintRequest {
                        requesting_actor_id: hex::encode(actor),
                        channel_id: hex::encode(channel),
                    }),
                    None,
                )
                .await
                .unwrap()
                .await_reply()
                .await
        }
    };

    // A reader mints — the grant the write twin demands is not read here.
    let reply: FedFolderReadTokenMintReply = from_value(
        &mint(0x91, channel_id, reader)
            .await
            .expect("a member mints"),
    );
    assert!(reply.expires_at > 0, "the token carries an absolute expiry");
    let scope = f_state
        .auth
        .bulk_byte_tokens
        .validate(&reply.token)
        .await
        .expect("the minted token validates on the home nest");
    assert_eq!(
        scope.actor_id.0, reader,
        "token bound to the member's actor"
    );
    assert_eq!(
        scope.access,
        fauna_protocol::wrapped_blob::BulkByteAccess::Read,
        "the token is read-only"
    );
    assert_eq!(
        scope.purpose,
        fauna_protocol::wrapped_blob::BulkByteMintPurpose::ForeignFolderRead,
        "the purpose the chunk route's relay arm admits"
    );

    for (key, actor, why) in [
        (0x92, stranger, "no foreign row"),
        (0x93, elsewhere, "bound to a different home nest"),
    ] {
        let err = mint(key, channel_id, actor)
            .await
            .expect_err("refused by the structural member gate");
        assert_eq!(err.code, "fauna.federation.forbidden", "{why}");
    }

    let err = mint(0x94, unclaimed_channel, reader)
        .await
        .expect_err("a member of a channel that is no folder's mints nothing");
    assert_eq!(err.code, "fauna.federation.not_found");
}

/// `fauna.federation.folder.actors.fetch` — the cross-nest writer roster read
/// (`federation.md` § Cross-nest…, *The cross-nest writer roster read*): a
/// foreign member bound to the requesting nest reads the set's actor roster —
/// the same projection the same-nest `members.list_actors` serves (the owner as
/// the `role == "owner"` row, each member with its grant) — with every `handle`
/// EMPTY across nests and the `caller_access` stamp; a requester with no
/// foreign row, or one bound to a DIFFERENT home nest, is refused `forbidden`;
/// an unclaimed conversation channel is `not_found` (S7).
#[tokio::test]
async fn folder_actors_fetch_serves_the_roster_ids_only_and_refuses_wrong_binding() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id_bytes = h_state.nest_identity.public_key_bytes();

    let owner = [0xa1u8; 32];
    let local = [0xa2u8; 32]; // a same-nest member on F, with a handle
    let writer = [0xe5u8; 32]; // foreign member on H, writer grant
    let reader = [0xb2u8; 32]; // foreign member on H, no grant
    let stranger = [0xc3u8; 32]; // no foreign row at all
    let channel_id = seed_claimed_shared_set(&f_state, &owner, "shared-docs").await;
    for (actor, handle) in [(owner, "alice"), (local, "carol")] {
        f_state.db.create_user(&actor, "free", "t").await.unwrap();
        f_state.db.set_handle(&actor, handle).await.unwrap();
        f_state
            .db
            .register_actor_channel(&actor, &channel_id)
            .await
            .unwrap();
    }
    for actor in [writer, reader] {
        f_state
            .db
            .register_foreign_channel_member(
                &channel_id,
                &actor,
                &h_nest_id_bytes,
                None,
                RebindPower::Standing,
            )
            .await
            .unwrap();
    }
    f_state
        .db
        .set_folder_member_access(&channel_id, &writer, "writer", Some(1_000_000))
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let fetch = |actor: [u8; 32], channel: [u8; 32], corr: u8| {
        let conn = &conn;
        async move {
            conn.dispatcher
                .request_raw(
                    "fauna.federation.folder.actors.fetch",
                    [corr; 16],
                    to_value(&FedFolderActorsFetchRequest {
                        requesting_actor_id: hex::encode(actor),
                        channel_id: hex::encode(channel),
                    }),
                    None,
                )
                .await
                .unwrap()
                .await_reply()
                .await
        }
    };

    // The writer reads the whole roster, ids-only.
    let reply: FedFolderActorsFetchReply =
        from_value(&fetch(writer, channel_id, 0x61).await.expect("member reads"));
    let row = |actor: [u8; 32]| {
        reply
            .members
            .iter()
            .find(|m| m.actor_id == hex::encode(actor))
            .unwrap_or_else(|| panic!("{} in the roster", hex::encode(actor)))
    };
    assert_eq!(
        reply.members.len(),
        4,
        "owner + local + two foreign members"
    );
    assert_eq!(
        row(owner).role,
        "owner",
        "the owner is the role == owner row"
    );
    assert_eq!(
        row(owner).access,
        None,
        "the owner holds no grant — they own"
    );
    assert_eq!(row(local).role, "member");
    assert_eq!(row(local).access.as_deref(), Some("reader"));
    assert_eq!(row(writer).access.as_deref(), Some("writer"));
    assert_eq!(row(writer).remote, Some(true));
    assert_eq!(row(reader).access.as_deref(), Some("reader"));
    assert!(
        reply.members.iter().all(|m| m.handle.is_empty()),
        "handle rides EMPTY across nests: {:?}",
        reply.members
    );
    assert_eq!(
        reply.caller_access.as_deref(),
        Some("writer"),
        "the caller_access stamp every federated folder read carries"
    );

    // A rostered reader reads it too (the gate is membership, not access).
    let reply: FedFolderActorsFetchReply =
        from_value(&fetch(reader, channel_id, 0x62).await.expect("reader reads"));
    assert_eq!(reply.members.len(), 4);
    assert_eq!(reply.caller_access, None, "no grant row ⇒ asserts nothing");

    // No foreign row → refused by the structural gate.
    let err = fetch(stranger, channel_id, 0x63)
        .await
        .expect_err("no foreign row → refused");
    assert_eq!(err.code, "fauna.federation.forbidden");

    // A member bound to a DIFFERENT home nest is refused for THIS origin.
    let elsewhere = [0xd4u8; 32];
    f_state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &elsewhere,
            &[0x77u8; 32],
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
    let err = fetch(elsewhere, channel_id, 0x64)
        .await
        .expect_err("wrong home-nest binding → refused");
    assert_eq!(err.code, "fauna.federation.forbidden");

    // S7: a conversation channel with a foreign member but no folder claim.
    let conversation = [0x98u8; 32];
    f_state
        .db
        .register_foreign_channel_member(
            &conversation,
            &writer,
            &h_nest_id_bytes,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
    let err = fetch(writer, conversation, 0x65)
        .await
        .expect_err("no folder claim on the channel → refused");
    assert_eq!(err.code, "fauna.federation.not_found");
}

/// The relayed door carries each writer's succession statements, because it
/// serves the one projection the same-nest read serves
/// (`mls-group-key-material.md` § M2 → *Writer-signed change records*, ruling
/// (8)(b) source (i); `federation.md` § Cross-nest…, *The cross-nest writer
/// roster read*): the owner row and a `writer` row each carry the landed chain
/// that ends at them — a home-nest succession and a peer-learned one alike —
/// a reader-access row carries none, and apart from the blanked handles the
/// relayed rows ARE the same-nest rows.
#[tokio::test]
async fn folder_actors_fetch_carries_the_same_succession_statements_as_the_same_nest_read() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id_bytes = h_state.nest_identity.public_key_bytes();

    let (owner0, owner1) = ([0xa1u8; 32], [0xa3u8; 32]);
    let (writer0, writer1) = ([0xe5u8; 32], [0xe6u8; 32]);
    let (reader0, reader1) = ([0xb2u8; 32], [0xb3u8; 32]);
    let channel_id = seed_claimed_shared_set(&f_state, &owner0, "shared-docs").await;
    f_state.db.create_user(&owner0, "free", "t").await.unwrap();
    f_state.db.set_handle(&owner0, "alice").await.unwrap();
    f_state
        .db
        .register_actor_channel(&owner0, &channel_id)
        .await
        .unwrap();
    for actor in [writer0, reader0] {
        f_state
            .db
            .register_foreign_channel_member(
                &channel_id,
                &actor,
                &h_nest_id_bytes,
                None,
                RebindPower::Standing,
            )
            .await
            .unwrap();
    }

    // The owner's succession on its home nest (the ceremony's transaction
    // moves the set and its claim), then the successor's own seat — the
    // roster row stays with the retired id until the successor registers.
    assert!(
        f_state
            .db
            .record_succession(&owner0, &owner1, b"owner-link", 1)
            .await
            .unwrap()
            .is_ok()
    );
    f_state
        .db
        .register_actor_channel(&owner1, &channel_id)
        .await
        .unwrap();
    // The two foreign members' successions, as federation delivers them: the
    // roster rows re-point, the grant does not — so the owner grants the
    // writer's successor.
    for (old, new, statement) in [
        (writer0, writer1, &b"writer-link"[..]),
        (reader0, reader1, &b"reader-link"[..]),
    ] {
        assert!(
            f_state
                .db
                .record_peer_succession(&old, &new, statement, 1)
                .await
                .unwrap()
                .is_ok()
        );
    }
    f_state
        .db
        .set_folder_member_access(&channel_id, &writer1, "writer", None)
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let reply: FedFolderActorsFetchReply = from_value(
        &conn
            .dispatcher
            .request_raw(
                "fauna.federation.folder.actors.fetch",
                [0x71; 16],
                to_value(&FedFolderActorsFetchRequest {
                    requesting_actor_id: hex::encode(reader1),
                    channel_id: hex::encode(channel_id),
                }),
                None,
            )
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("a rostered member reads"),
    );
    let carried = |actor: [u8; 32]| -> Vec<Vec<u8>> {
        reply
            .members
            .iter()
            .find(|m| m.actor_id == hex::encode(actor))
            .unwrap_or_else(|| panic!("{} in the roster", hex::encode(actor)))
            .succession_statements
            .iter()
            .map(|s| s.to_vec())
            .collect()
    };
    assert_eq!(carried(owner1), vec![b"owner-link".to_vec()]);
    assert_eq!(carried(writer1), vec![b"writer-link".to_vec()]);
    assert!(
        carried(reader1).is_empty(),
        "a reader-access row discloses no retired id"
    );
    assert!(
        carried(owner0).is_empty(),
        "the retired owner's stale seat is a reader-access row"
    );

    // One projection behind both doors: the same-nest read, handles blanked.
    let same_nest: fauna_protocol::folders::ActorMembersListReply = same_nest_call(
        &f_state,
        owner1,
        "fauna.folders.members.list_actors",
        &fauna_protocol::folders::ActorMembersListRequest {
            name: "shared-docs".into(),
            ..Default::default()
        },
    )
    .await;
    let blanked: Vec<_> = same_nest
        .members
        .into_iter()
        .map(|mut m| {
            m.handle = String::new();
            m
        })
        .collect();
    assert_eq!(reply.members, blanked);
}

/// Dispatch one same-nest folders/sync kind on a nest, as a local client of it
/// would — the set-home's own door, beside the federated one under test.
async fn same_nest_call<Req: Serialize, Rep: DeserializeOwned>(
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    req: &Req,
) -> Rep {
    let mut b = fauna_nest::rpc_router::RpcRouter::builder();
    fauna_nest::folder_handlers::register_folders_handlers(&mut b);
    fauna_nest::sync_handlers::register_sync_handlers(&mut b);
    let router = b.build();
    let meta = router.kind_meta(kind).expect("kind registered");
    let payload = bytes::Bytes::from(encode_canonical(req).unwrap().to_vec());
    let out = (meta.handler)(state.clone(), actor, payload)
        .await
        .unwrap_or_else(|e| panic!("{kind}: {e:?}"));
    decode_strict(&out).expect("reply decodes")
}

/// The moved cert is served across nests too (`mls-group-key-material.md`
/// § M2 → *Writer-signed change records*, ruling (8) preamble): after the
/// owner's succession, `fauna.federation.folder.changes.fetch` still carries
/// the delegated signer's cert — the one naming the PREDECESSOR — beside the
/// row now stamped with the successor. Presence, not table length.
#[tokio::test]
async fn folder_changes_fetch_still_serves_a_moved_rows_predecessor_cert() {
    use fauna_protocol::sync::{
        DeviceGrantRegisterReply, DeviceGrantRegisterRequest, SyncChangeRecordReply,
        SyncChangeRecordRequest, SyncRegisterReply, SyncRegisterRequest,
    };
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id_bytes = h_state.nest_identity.public_key_bytes();

    let owner0 = common::signing_actor(0xa1);
    let owner1 = [0xa3u8; 32];
    let member = [0xb2u8; 32]; // lives on H
    let owner0_id = owner0.actor_id().0;
    let channel_id = seed_claimed_shared_set(&f_state, &owner0_id, "shared-docs").await;
    f_state
        .db
        .create_user(&owner0_id, "free", "t")
        .await
        .unwrap();
    f_state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &member,
            &h_nest_id_bytes,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();

    // The owner's machine principal — a device key under a root-signed
    // `SyncWrite` grant — records one row through the real ingest.
    let device = common::signing_actor(0xd7);
    let device_hex = hex::encode(device.actor_id().0);
    let _: SyncRegisterReply = same_nest_call(
        &f_state,
        owner0_id,
        "fauna.sync.register",
        &SyncRegisterRequest {
            device_id: device_hex.clone(),
            label: "machine".into(),
            capabilities: "read,write".into(),
            ..Default::default()
        },
    )
    .await;
    let _: DeviceGrantRegisterReply = same_nest_call(
        &f_state,
        owner0_id,
        "fauna.sync.device_grant.register",
        &DeviceGrantRegisterRequest {
            device_id: device_hex.clone(),
            authorization: fauna_client_sync::build_principal_grant(&owner0, &device.actor_id().0)
                .expect("grant builds"),
            extra: Default::default(),
        },
    )
    .await;
    let mut record = SyncChangeRecordRequest {
        folder: "shared-docs".into(),
        device_id: device_hex,
        path: "docs/signed.txt".into(),
        manifest_hash: Some(hex::encode([0x7au8; 32])),
        size_bytes: 10,
        change_type: "create".into(),
        path_sealed: Some(fauna_protocol::ByteBuf::from(b"seal".to_vec())),
        derived_through: Some(0),
        ..Default::default()
    };
    let statement = fauna_protocol::sync_writer_sig::SignedChange::for_record(
        &record,
        owner0_id,
        common::SET_NONCE,
    )
    .unwrap();
    record.signature = Some(fauna_protocol::ByteBuf::from(
        statement.sign(device.signing_key()).to_vec(),
    ));
    record.signer_key = Some(fauna_protocol::ByteBuf::from(device.actor_id().0.to_vec()));
    let _: SyncChangeRecordReply =
        same_nest_call(&f_state, owner0_id, "fauna.sync.changes.record", &record).await;

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let fetch = |corr: u8| {
        let conn = &conn;
        async move {
            let reply: FedFolderChangesFetchReply = from_value(
                &conn
                    .dispatcher
                    .request_raw(
                        "fauna.federation.folder.changes.fetch",
                        [corr; 16],
                        to_value(&FedFolderChangesFetchRequest {
                            requesting_actor_id: hex::encode(member),
                            channel_id: hex::encode(channel_id),
                            after: 0,
                            limit: 0,
                        }),
                        None,
                    )
                    .await
                    .unwrap()
                    .await_reply()
                    .await
                    .expect("the member reads"),
            );
            reply
        }
    };
    let signed_hash = hex::encode(fauna_core::sync::path_hash("docs/signed.txt"));

    let before = fetch(0x81).await;
    assert_eq!(before.signer_certs.len(), 1, "the delegated signer's cert");
    let cert = before.signer_certs[0].clone();

    assert!(
        f_state
            .db
            .record_succession(&owner0_id, &owner1, b"owner-link", 1)
            .await
            .unwrap()
            .is_ok()
    );

    let after = fetch(0x82).await;
    let row = after
        .changes
        .iter()
        .find(|c| c.path_hash == signed_hash)
        .expect("the signed row is still served");
    assert_eq!(
        row.author_actor_id.as_deref(),
        Some(hex::encode(owner1).as_str()),
        "the row moved to the successor"
    );
    assert_eq!(
        row.signer_key.as_ref().map(|k| k.to_vec()),
        Some(device.actor_id().0.to_vec()),
        "its signer key did not"
    );
    assert!(
        after.signer_certs.contains(&cert),
        "the predecessor-named cert rides beside the moved row"
    );
}

/// S7: the folder read kinds serve only channels with a folder claim — a
/// plain conversation channel (foreign member present, no claim) is refused,
/// never resolved by peer-asserted typing.
#[tokio::test]
async fn folder_read_plane_refuses_an_unclaimed_conversation_channel() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id_bytes = h_state.nest_identity.public_key_bytes();

    let member = [0xb2u8; 32];
    let channel_id = [0x99u8; 32]; // a conversation channel — no claim, no set
    f_state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &member,
            &h_nest_id_bytes,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let req = FedFolderChangesFetchRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(channel_id),
        after: 0,
        limit: 0,
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.changes.fetch",
            [0x55u8; 16],
            to_value(&req),
            None,
        )
        .await
        .unwrap();
    let err = call
        .await_reply()
        .await
        .expect_err("no folder claim on the channel → refused");
    assert_eq!(err.code, "fauna.federation.not_found");
}

/// `fauna.federation.channel.leave` — self-scoped: the requester's own row is
/// deleted (killing subsequent federated fetches), a re-leave is an idempotent
/// success, and a request from a nest that is NOT the member's recorded home
/// nest is refused and deletes nothing.
#[tokio::test]
async fn channel_leave_over_channel_is_self_scoped_and_idempotent() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id_bytes = h_state.nest_identity.public_key_bytes();

    let owner = [0xa1u8; 32];
    let member = [0xb2u8; 32];
    let channel_id = seed_claimed_shared_set(&f_state, &owner, "shared-docs").await;
    f_state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &member,
            &h_nest_id_bytes,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    // Leave deletes the requester's own row…
    let req = FedChannelLeaveRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(channel_id),
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.channel.leave",
            [0x61u8; 16],
            to_value(&req),
            None,
        )
        .await
        .unwrap();
    let reply: FedChannelLeaveReply = from_value(&call.await_reply().await.expect("ok"));
    assert!(reply.removed);
    assert!(
        f_state
            .db
            .foreign_member_home_nest(&channel_id, &member)
            .await
            .unwrap()
            .is_none(),
        "the foreign row is gone"
    );

    // …after which every federated fetch is refused…
    let fetch = FedFolderChangesFetchRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(channel_id),
        after: 0,
        limit: 0,
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.folder.changes.fetch",
            [0x62u8; 16],
            to_value(&fetch),
            None,
        )
        .await
        .unwrap();
    let err = call
        .await_reply()
        .await
        .expect_err("post-leave fetch refused");
    assert_eq!(err.code, "fauna.federation.forbidden");

    // …and a re-leave is an idempotent success (absent row = success).
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.channel.leave",
            [0x63u8; 16],
            to_value(&req),
            None,
        )
        .await
        .unwrap();
    let reply: FedChannelLeaveReply = from_value(&call.await_reply().await.expect("ok"));
    assert!(
        !reply.removed,
        "idempotent re-leave reports removed = false"
    );

    // A leave for a member bound to a DIFFERENT nest is refused and deletes
    // nothing (self-scoped — only the member's own home nest may leave for them).
    let elsewhere = [0xd4u8; 32];
    f_state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &elsewhere,
            &[0x77u8; 32],
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
    let bad = FedChannelLeaveRequest {
        requesting_actor_id: hex::encode(elsewhere),
        channel_id: hex::encode(channel_id),
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.channel.leave",
            [0x64u8; 16],
            to_value(&bad),
            None,
        )
        .await
        .unwrap();
    let err = call
        .await_reply()
        .await
        .expect_err("wrong home-nest binding → refused");
    assert_eq!(err.code, "fauna.federation.forbidden");
    assert!(
        f_state
            .db
            .foreign_member_home_nest(&channel_id, &elsewhere)
            .await
            .unwrap()
            .is_some(),
        "the mismatched row survives"
    );
}

/// A CONFIRMED foreign binding on an unclaimed conversation channel has no
/// mover but the incumbent — and the incumbent's own `channel.leave` really is
/// the release: it deletes even a confirmed row (deliberately not pin-guarded),
/// and the delete re-opens the insert-if-absent arm so a re-invite can land the
/// member's NEW home nest. This is the cooperative re-homing path
/// `federation.md`'s TOFU bullet names (leave, then re-invite), and the power
/// `nest/common.md` § Client-state recoverability → *Per-object remedies* leans
/// on: were this delete ever pin-guarded
/// "for consistency" with the rebind arm, every cooperative re-home would
/// silently die with it, leaving the fresh channel as the only exit.
#[tokio::test]
async fn channel_leave_releases_a_confirmed_binding_and_reopens_the_grant() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id_bytes = h_state.nest_identity.public_key_bytes();

    let member = [0xb7u8; 32]; // foreign conversation member on H
    let conv_channel = [0x99u8; 32]; // unclaimed conversation channel on F
    f_state
        .db
        .register_foreign_channel_member(
            &conv_channel,
            &member,
            &h_nest_id_bytes,
            None,
            RebindPower::InsertOnly,
        )
        .await
        .unwrap();
    f_state
        .db
        .confirm_foreign_member(&conv_channel, &member, &h_nest_id_bytes)
        .await
        .unwrap();
    assert_eq!(
        f_state
            .db
            .foreign_member_binding(&conv_channel, &member)
            .await
            .unwrap(),
        Some((h_nest_id_bytes, true)),
        "the binding is confirmed before the leave"
    );

    // The incumbent's own leave deletes the CONFIRMED row…
    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let req = FedChannelLeaveRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(conv_channel),
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.channel.leave",
            [0x81u8; 16],
            to_value(&req),
            None,
        )
        .await
        .unwrap();
    let reply: FedChannelLeaveReply = from_value(&call.await_reply().await.expect("ok"));
    assert!(
        reply.removed,
        "a confirmed binding is still the incumbent's to release"
    );
    assert!(
        f_state
            .db
            .foreign_member_binding(&conv_channel, &member)
            .await
            .unwrap()
            .is_none(),
        "the confirmed row is gone"
    );

    // …and the release re-opens the first-grant arm: a re-invite lands the
    // member's NEW home nest with no standing at all (insert-if-absent), the
    // pin reset with the row — the incoming nest earns its own confirmation.
    let new_home = [0xcdu8; 32];
    f_state
        .db
        .register_foreign_channel_member(
            &conv_channel,
            &member,
            &new_home,
            None,
            RebindPower::InsertOnly,
        )
        .await
        .unwrap();
    assert_eq!(
        f_state
            .db
            .foreign_member_binding(&conv_channel, &member)
            .await
            .unwrap(),
        Some((new_home, false)),
        "the re-invite landed the new home nest, unconfirmed"
    );
}

/// `fauna.federation.channel.append` — a foreign conversation member's relayed
/// Application envelope lands on the home nest's log (served back by
/// `channel.fetch`); the structural gate refuses a requester with no/mismatched
/// foreign row; and folder-channel Commit admission is **roster-membership**
/// (re-ratified 2026-08-24, `federation.md` § Cross-nest shared folders +
/// channel append): a knock-only foreign member's Commit is refused by the
/// roster check while an accepted (rostered) member's takeover Commit lands —
/// and a conversation channel's member Commit lands as before.
#[tokio::test]
async fn channel_append_over_channel_lands_and_gates() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let h_nest_id_bytes = h_state.nest_identity.public_key_bytes();

    let member = [0xb2u8; 32]; // foreign conversation member on H
    let conv_channel = [0x98u8; 32]; // unclaimed conversation channel on F
    f_state
        .db
        .register_foreign_channel_member(
            &conv_channel,
            &member,
            &h_nest_id_bytes,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();

    let app_envelope = fauna_mls::types::ChannelEnvelope::Application(vec![0xEE; 32])
        .to_bytes()
        .unwrap();
    let commit_envelope = fauna_mls::types::ChannelEnvelope::Commit(vec![0xCC; 32])
        .to_bytes()
        .unwrap();

    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();

    // An Application envelope from the recorded member lands with a seq…
    let req = FedChannelAppendRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(conv_channel),
        envelope: app_envelope.clone(),
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.channel.append",
            [0x71u8; 16],
            to_value(&req),
            None,
        )
        .await
        .unwrap();
    let reply: FedChannelAppendReply = from_value(&call.await_reply().await.expect("ok"));
    assert_eq!(reply.seq, 1, "first record on the channel");

    // …and is served back over the fetch relay (the same envelope bytes).
    let fetch = FedChannelFetchRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(conv_channel),
        after: 0,
        limit: 0,
        requesting_handle: None,
        requesting_domain: None,
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.channel.fetch",
            [0x72u8; 16],
            to_value(&fetch),
            None,
        )
        .await
        .unwrap();
    let fetched: FedChannelFetchReply = from_value(&call.await_reply().await.expect("ok"));
    assert_eq!(fetched.messages.len(), 1);
    assert_eq!(fetched.messages[0].envelope, app_envelope);

    // A conversation member's Commit is allowed (any member commits a DM/group).
    let commit_req = FedChannelAppendRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(conv_channel),
        envelope: commit_envelope.clone(),
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.channel.append",
            [0x73u8; 16],
            to_value(&commit_req),
            None,
        )
        .await
        .unwrap();
    let reply: FedChannelAppendReply = from_value(&call.await_reply().await.expect("ok"));
    assert_eq!(reply.seq, 2, "conversation Commit lands");

    // The structural gate: a requester with no foreign row is refused.
    let stranger = [0xc3u8; 32];
    let bad = FedChannelAppendRequest {
        requesting_actor_id: hex::encode(stranger),
        channel_id: hex::encode(conv_channel),
        envelope: app_envelope.clone(),
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.channel.append",
            [0x74u8; 16],
            to_value(&bad),
            None,
        )
        .await
        .unwrap();
    let err = call
        .await_reply()
        .await
        .expect_err("no foreign row → refused");
    assert_eq!(err.code, "fauna.federation.forbidden");

    // Federated plane, folder-channel Commit admission (re-ratified 2026-08-24,
    // `federation.md` § Cross-nest shared folders + channel append — the
    // same-nest half is pinned by
    // `channel_send_admits_rostered_member_commit_on_claimed_folder_channel`):
    // a KNOCK-ONLY foreign member — recorded in `channel_foreign_members` but
    // never accepted onto the `actor_channels` roster (a cross-nest folder
    // share always knocks; accept is the sole cross-nest roster-add) — is
    // refused by the roster check, with zero side effects. An ACCEPTED
    // (rostered) foreign member's Commit — the device-owned-epoch takeover
    // shape — is admitted.
    let owner = [0xa1u8; 32];
    let fs_channel = seed_claimed_shared_set(&f_state, &owner, "shared-docs").await;
    f_state
        .db
        .register_foreign_channel_member(
            &fs_channel,
            &member,
            &h_nest_id_bytes,
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
    let knock_commit = FedChannelAppendRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(fs_channel),
        envelope: commit_envelope.clone(),
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.channel.append",
            [0x75u8; 16],
            to_value(&knock_commit),
            None,
        )
        .await
        .unwrap();
    let err = call
        .await_reply()
        .await
        .expect_err("a knock-only (off-roster) foreign member's Commit → refused");
    assert_eq!(err.code, "fauna.conversations.ingest_failed");
    // Refusal had zero side effects: no record landed on the folder channel.
    let fetch = FedChannelFetchRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(fs_channel),
        after: 0,
        limit: 0,
        requesting_handle: None,
        requesting_domain: None,
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.channel.fetch",
            [0x76u8; 16],
            to_value(&fetch),
            None,
        )
        .await
        .unwrap();
    let fetched: FedChannelFetchReply = from_value(&call.await_reply().await.expect("ok"));
    assert!(
        fetched.messages.is_empty(),
        "the refused Commit consumed no seq"
    );

    // The accepted member (on the actor_channels roster) IS admitted — the
    // member takeover the 2026-08-24 re-ratification exists to let through.
    f_state
        .db
        .register_actor_channel(&member, &fs_channel)
        .await
        .unwrap();
    let takeover = FedChannelAppendRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(fs_channel),
        envelope: commit_envelope,
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
    };
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.channel.append",
            [0x77u8; 16],
            to_value(&takeover),
            None,
        )
        .await
        .unwrap();
    let reply: FedChannelAppendReply = from_value(&call.await_reply().await.expect("ok"));
    assert_eq!(
        reply.seq, 1,
        "an accepted foreign member's takeover Commit lands on the folder channel"
    );
}

/// The `residency` stamp (`federation.md` § Cross-nest shared folders + channel
/// append → *Relay serving across nests*): every federated folder read reply —
/// `changes.fetch`, `content_key.fetch`, `actors.fetch` — carries the folder's
/// residency beside `caller_access`, read off the home nest's claimed row and
/// always STATED (`"full"` included), because absent on the wire means *not
/// stated*, never *full*. The metadata-only pair is seeded beneath the handlers
/// (the folder's column and the foreign-member row written on the nest DB):
/// the interim refusal (`file-sync.md` § Relay serving → *Until that leg is
/// built, the pair is refused*) stands, and an existing pair is left as it is.
#[tokio::test]
async fn every_federated_folder_read_reply_stamps_the_folders_residency() {
    let (_h_url, h_state) = start_nest().await;
    let (f_url, f_state) = start_nest().await;
    let f_nest_id = nest_id_hex(&f_state);
    let owner = [0xa1u8; 32];
    let member = [0xb2u8; 32]; // lives on H
    let channel_id = seed_claimed_shared_set(&f_state, &owner, "shared-docs").await;
    f_state
        .db
        .register_foreign_channel_member(
            &channel_id,
            &member,
            &h_state.nest_identity.public_key_bytes(),
            None,
            RebindPower::Standing,
        )
        .await
        .unwrap();
    let conn = dial(&h_state, &f_url, &f_nest_id).await.unwrap();
    let key = std::cell::Cell::new(0x60u8);
    let call = async |kind: &str, req: Value| -> Value {
        key.set(key.get() + 1);
        conn.dispatcher
            .request_raw(kind, [key.get(); 16], req, None)
            .await
            .unwrap()
            .await_reply()
            .await
            .expect("a member's read is served")
    };
    let changes_req = to_value(&FedFolderChangesFetchRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(channel_id),
        after: 0,
        limit: 0,
    });
    let ck_req = to_value(&FedFolderContentKeyFetchRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(channel_id),
    });
    let actors_req = to_value(&FedFolderActorsFetchRequest {
        requesting_actor_id: hex::encode(member),
        channel_id: hex::encode(channel_id),
    });

    for expected in ["full", "metadata_only"] {
        if expected == "metadata_only" {
            assert!(
                f_state
                    .db
                    .update_folder_for_user(
                        "shared-docs",
                        &owner,
                        fauna_nest::db::FolderUpdate {
                            residency: Some(Some("metadata_only")),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap(),
                "fixture: the folder is metadata-only beneath the handlers"
            );
        }
        let changes: FedFolderChangesFetchReply =
            from_value(&call("fauna.federation.folder.changes.fetch", changes_req.clone()).await);
        let ck: FedFolderContentKeyFetchReply =
            from_value(&call("fauna.federation.folder.content_key.fetch", ck_req.clone()).await);
        let actors: FedFolderActorsFetchReply =
            from_value(&call("fauna.federation.folder.actors.fetch", actors_req.clone()).await);
        for (reply, residency) in [
            ("changes.fetch", changes.residency),
            ("content_key.fetch", ck.residency),
            ("actors.fetch", actors.residency),
        ] {
            assert_eq!(
                residency.as_deref(),
                Some(expected),
                "{reply} stamps the folder's residency, stated"
            );
        }
    }
}
