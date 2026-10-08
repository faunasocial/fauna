//! Integration round-trips for the five `fauna.bridges.put_<substruct>_policy`
//! admin kinds (A3 Bucket B). Each drives the
//! full Admin → `put_<x>_policy` → `fetch_config` projection so the wire shape
//! + DB upsert + handler overlay + allowlist gate all participate.
//!
//! The bridge already *reads* every projected sub-struct field
//! (`SpamPolicyThresholds` / `AuthPolicy` / `SubmissionPolicyThresholds` /
//! `ImapPolicy` / `OutboundPolicy`); these kinds add the admin *write* path so
//! an override is overlaid onto the catalog default before `fetch_config`
//! encodes. `None` / unset ⇒ catalog default. Was the single 4-field
//! `put_mail_policy` (DNS-perimeter slice), now
//! `put_spam_policy` covering the whole
//! `SpamPolicyThresholds` sub-struct.

mod common;
use common::approve_bridge;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{
    AliasPolicy, FetchConfigReply, FetchConfigRequest, GetAliasPolicyRequest,
    PutAliasPolicyRequest, PutAuthPolicyRequest, PutImapPolicyRequest, PutOutboundPolicyRequest,
    PutPolicyReply, PutSpamPolicyRequest, PutSubmissionPolicyRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

async fn fixture_state() -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let rpc_router = Arc::new({
        let mut b = RpcRouter::builder();
        register_bridge_routing_handlers(&mut b);
        b.build()
    });
    Arc::new(AppState {
        rpc_router,
        ..AppState::for_test(db)
    })
}

async fn dispatch(
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = state
        .rpc_router
        .kind_meta(kind)
        .expect("kind registered with router");
    (meta.handler)(state.clone(), actor, payload).await
}

async fn fetch_config_as_mta(state: Arc<AppState>, mta: [u8; 32]) -> FetchConfigReply {
    let req = FetchConfigRequest {
        scope: "all".into(),
    };
    let reply_bytes = dispatch(state, mta, "fauna.bridges.fetch_config", encode(&req))
        .await
        .expect("fetch_config ok");
    decode::<FetchConfigReply>(&reply_bytes).expect("decode FetchConfigReply")
}

/// Approve an MTA (to issue `fetch_config`) + an admin actor (the only
/// caller class permitted on the `put_<x>_policy` kinds). Returns both.
async fn mta_and_admin(state: &Arc<AppState>) -> ([u8; 32], [u8; 32]) {
    let mta = [1u8; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x99u8; 32]).await;
    let admin = [42u8; 32];
    state
        .db
        .add_admin_actor(&admin[..])
        .await
        .expect("add admin actor");
    (mta, admin)
}

async fn put_as_admin<T: serde::Serialize>(
    state: &Arc<AppState>,
    admin: [u8; 32],
    kind: &str,
    req: &T,
) {
    let reply_bytes = dispatch(state.clone(), admin, kind, encode(req))
        .await
        .unwrap_or_else(|e| panic!("{kind} ok, got {e:?}"));
    let reply: PutPolicyReply = decode(&reply_bytes).expect("decode PutPolicyReply");
    assert!(reply.ok);
}

// ── spam ────────────────────────────────────────────────────────────

#[tokio::test]
async fn fetch_config_returns_catalog_defaults_before_any_override() {
    let state = fixture_state().await;
    let mta = [1u8; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x99u8; 32]).await;

    let reply = fetch_config_as_mta(state, mta).await;
    // Catalog defaults across every sub-struct this slice newly writes.
    assert_eq!(
        reply.spam.dnsbl_servers,
        vec!["zen.spamhaus.org".to_string()]
    );
    // Catalog default is 0 = disabled (never 550-reject on content score) per
    // mail-policy-config.md § Policy catalog + SpamPolicyThresholds::default().
    assert_eq!(reply.spam.max_score_before_reject, 0);
    assert_eq!(reply.spam.max_message_bytes, 50_000_000);
    assert!(reply.auth.enforce_dmarc);
    assert!(!reply.auth.enforce_dkim);
    // Per-IP concurrent-connection cap on the authenticated surfaces
    // (mail.auth.per_ip_max_concurrent_conn); catalog default 256.
    assert_eq!(reply.auth.max_conn_per_ip, 256);
    assert_eq!(reply.submission.max_per_day, 1000);
    assert_eq!(reply.imap.idle_timeout_secs, 1740);
    assert_eq!(reply.imap.delete_nonempty, "forbidden");
    assert_eq!(reply.outbound.permanent_failure_timeout_hours, 120);
    assert!(reply.outbound.ipv6_enabled);
}

#[tokio::test]
async fn admin_put_spam_clears_dnsbl_and_sets_threshold_reflected_in_fetch_config() {
    let state = fixture_state().await;
    let (mta, admin) = mta_and_admin(&state).await;

    let put = PutSpamPolicyRequest {
        dnsbl_servers: Some(Vec::new()),
        greylist_enabled: Some(false),
        greylist_delay_secs: Some(0),
        fcrdns_mode: Some("off".into()),
        max_score_before_reject: Some(20),
        max_message_bytes: Some(100_000_000),
        ..Default::default()
    };
    put_as_admin(&state, admin, "fauna.bridges.put_spam_policy", &put).await;

    let reply = fetch_config_as_mta(state, mta).await;
    assert!(reply.spam.dnsbl_servers.is_empty());
    assert!(!reply.spam.greylist_enabled);
    assert_eq!(reply.spam.greylist_delay_secs, 0);
    assert_eq!(reply.spam.fcrdns_mode, "off");
    assert_eq!(reply.spam.max_score_before_reject, 20);
    assert_eq!(reply.spam.max_message_bytes, 100_000_000);
    // Unmentioned spam fields keep the catalog default.
    assert_eq!(reply.spam.max_score_before_spam_folder, 5);
    assert_eq!(reply.spam.max_conn_per_min, 10);
}

/// `max_conn_per_min` override round-trip. The catalog default (10) is
/// asserted above; this guards the override-*set* path, which the e2e MTA
/// harness depends on: `tests/e2e-unified/conftest.py` `mail_bridge_mta`
/// sets `max_conn_per_min` high because every test connects from 127.0.0.1
/// and shares one per-IP connection budget (the Go MTA's
/// `RateLimiter`, fed by `snap.Spam.MaxConnPerMin`). If this projection
/// regressed, the session bridge would 421 a downstream test with
/// `Connection rate limit exceeded` — a silent suite-stranding flake the
/// default-only assertion above would not catch.
#[tokio::test]
async fn admin_put_spam_max_conn_per_min_override_reflected_in_fetch_config() {
    let state = fixture_state().await;
    let (mta, admin) = mta_and_admin(&state).await;

    let put = PutSpamPolicyRequest {
        max_conn_per_min: Some(1000),
        ..Default::default()
    };
    put_as_admin(&state, admin, "fauna.bridges.put_spam_policy", &put).await;

    let reply = fetch_config_as_mta(state, mta).await;
    assert_eq!(reply.spam.max_conn_per_min, 1000);
}

#[tokio::test]
async fn admin_put_spam_rejects_threshold_ordering_violation() {
    let state = fixture_state().await;
    let (_mta, admin) = mta_and_admin(&state).await;

    // Effective {folder=5, reject=3} violates folder < reject — must be
    // rejected (the bridge would
    // otherwise debug-assert in decide_spam_disposition).
    let put = PutSpamPolicyRequest {
        max_score_before_reject: Some(3),
        ..Default::default()
    };
    let err = dispatch(
        state.clone(),
        admin,
        "fauna.bridges.put_spam_policy",
        encode(&put),
    )
    .await
    .expect_err("ordering violation must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // A fully-reordered, consistent set is accepted.
    let ok = PutSpamPolicyRequest {
        max_score_before_spam_folder: Some(3),
        max_score_before_reject: Some(12),
        ..Default::default()
    };
    put_as_admin(&state, admin, "fauna.bridges.put_spam_policy", &ok).await;
}

/// The product ceiling has an upper bound (`mail-message-size.md` § Message size
/// limits, ruled 2026-08-26): `max_message_bytes` above
/// `MAX_MESSAGE_BYTES_CEILING` is refused with `fauna.protocol.malformed`.
///
/// This is a *security* bound, not a taste one. The ClamAV gate's size cap is
/// derived from this very knob (`mail-content-scanning.md` § Oversize messages)
/// and the scan sidecar's shipped stream/scan/file limits are sized for the
/// bound — so accepting a larger ceiling would name a perimeter the scanner
/// cannot cover. The round-trip test above deliberately keeps writing
/// 100,000,000 (a legitimate, under-bound admin choice); this one guards the far
/// side of the same door.
#[tokio::test]
async fn admin_put_spam_rejects_max_message_bytes_above_the_product_ceiling() {
    let state = fixture_state().await;
    let (mta, admin) = mta_and_admin(&state).await;

    let ceiling = fauna_mail::transport_limits::MAX_MESSAGE_BYTES_CEILING;

    let put = PutSpamPolicyRequest {
        max_message_bytes: Some(ceiling + 1),
        ..Default::default()
    };
    let err = dispatch(
        state.clone(),
        admin,
        "fauna.bridges.put_spam_policy",
        encode(&put),
    )
    .await
    .expect_err("a ceiling above MAX_MESSAGE_BYTES_CEILING must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");

    // The refusal is a *bound*, not a ban: the ceiling itself is accepted, and
    // the rejected write left nothing behind.
    let ok = PutSpamPolicyRequest {
        max_message_bytes: Some(ceiling),
        ..Default::default()
    };
    put_as_admin(&state, admin, "fauna.bridges.put_spam_policy", &ok).await;

    let reply = fetch_config_as_mta(state, mta).await;
    assert_eq!(reply.spam.max_message_bytes, ceiling);
}

// ── auth / submission / imap / outbound ──────────────────────────────

#[tokio::test]
async fn admin_put_auth_policy_reflected_in_fetch_config() {
    let state = fixture_state().await;
    let (mta, admin) = mta_and_admin(&state).await;

    let put = PutAuthPolicyRequest {
        enforce_dmarc: Some(false),
        enforce_dkim: Some(true),
        log_only: Some(true),
        max_auth_failures_per_minute: Some(15),
        max_conn_per_ip: Some(64),
        ..Default::default()
    };
    put_as_admin(&state, admin, "fauna.bridges.put_auth_policy", &put).await;

    let reply = fetch_config_as_mta(state, mta).await;
    assert!(!reply.auth.enforce_dmarc);
    assert!(reply.auth.enforce_dkim);
    assert!(reply.auth.log_only);
    assert_eq!(reply.auth.max_auth_failures_per_minute, 15);
    // The per-IP concurrent-connection cap override projects to the bridge,
    // so an admin tightening it (or, in the e2e harness, loosening it for the
    // shared 127.0.0.1 source) reaches the Go connlimit per-IP limiter.
    assert_eq!(reply.auth.max_conn_per_ip, 64);
    // Unmentioned auth fields keep the catalog default.
    assert!(reply.auth.enforce_spf_hardfail);
}

#[tokio::test]
async fn admin_put_submission_policy_reflected_in_fetch_config() {
    let state = fixture_state().await;
    let (mta, admin) = mta_and_admin(&state).await;

    let put = PutSubmissionPolicyRequest {
        max_per_day: Some(500),
        max_recipients_per_message: Some(50),
        extra: Default::default(),
    };
    put_as_admin(&state, admin, "fauna.bridges.put_submission_policy", &put).await;

    let reply = fetch_config_as_mta(state, mta).await;
    assert_eq!(reply.submission.max_per_day, 500);
    assert_eq!(reply.submission.max_recipients_per_message, 50);
}

#[tokio::test]
async fn admin_put_imap_policy_reflected_in_fetch_config() {
    let state = fixture_state().await;
    let (mta, admin) = mta_and_admin(&state).await;

    let put = PutImapPolicyRequest {
        idle_timeout_secs: Some(900),
        delete_nonempty: Some("allowed".into()),
        storage_bytes_default: Some(2 << 30),
        message_count_default: Some(100_000),
        ..Default::default()
    };
    put_as_admin(&state, admin, "fauna.bridges.put_imap_policy", &put).await;

    let reply = fetch_config_as_mta(state, mta).await;
    assert_eq!(reply.imap.idle_timeout_secs, 900);
    assert_eq!(reply.imap.delete_nonempty, "allowed");
    assert_eq!(reply.imap.storage_bytes_default, 2 << 30);
    assert_eq!(reply.imap.message_count_default, 100_000);
    // Unmentioned imap fields keep the catalog default.
    assert_eq!(reply.imap.tombstone_retention_days, 30);
}

#[tokio::test]
async fn admin_put_outbound_policy_reflected_in_fetch_config() {
    let state = fixture_state().await;
    let (mta, admin) = mta_and_admin(&state).await;

    let put = PutOutboundPolicyRequest {
        retry_schedule_seconds: Some(vec![0, 600, 3600]),
        permanent_failure_timeout_hours: Some(72),
        ipv6_enabled: Some(false),
        treat_5xx_as_transient: Some(vec!["5.7.1".into()]),
        ..Default::default()
    };
    put_as_admin(&state, admin, "fauna.bridges.put_outbound_policy", &put).await;

    let reply = fetch_config_as_mta(state, mta).await;
    assert_eq!(reply.outbound.retry_schedule_seconds, vec![0, 600, 3600]);
    assert_eq!(reply.outbound.permanent_failure_timeout_hours, 72);
    assert!(!reply.outbound.ipv6_enabled);
    assert_eq!(
        reply.outbound.treat_5xx_as_transient,
        vec!["5.7.1".to_string()]
    );
    // Unmentioned outbound fields keep the catalog default.
    assert!(reply.outbound.tlsrpt_send_reports);
}

// ── non-admin callers are rejected by the allowlist gate ────────────

#[tokio::test]
async fn put_spam_policy_rejects_non_admin_callers() {
    let state = fixture_state().await;
    let mta = [1u8; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x99u8; 32]).await;
    // No bridge enrollment + no admin row ⇒ this actor resolves as User.
    let user = [7u8; 32];

    for actor in [mta, user] {
        let err = dispatch(
            state.clone(),
            actor,
            "fauna.bridges.put_spam_policy",
            encode(&PutSpamPolicyRequest::default()),
        )
        .await
        .expect_err("non-admin must be denied on put_spam_policy");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }
}

// ── alias policy (the admin read twin of put_alias_policy) ───────────
//
// Unlike the five projected `put_<substruct>_policy` kinds (which round-trip
// through `fetch_config`), the four nest-side alias knobs are not in
// `FetchConfigReply`, so they read back through their own `get_alias_policy`
// admin kind. Drives Admin → (put_alias_policy →) get_alias_policy so the wire
// shape + DB read + `.effective()` overlay + allowlist gate all participate.

async fn get_alias_policy_as_admin(state: Arc<AppState>, admin: [u8; 32]) -> AliasPolicy {
    let reply_bytes = dispatch(
        state,
        admin,
        "fauna.bridges.get_alias_policy",
        encode(&GetAliasPolicyRequest::default()),
    )
    .await
    .expect("get_alias_policy ok");
    decode::<AliasPolicy>(&reply_bytes).expect("decode AliasPolicy")
}

#[tokio::test]
async fn get_alias_policy_returns_catalog_defaults_before_any_override() {
    let state = fixture_state().await;
    let (_mta, admin) = mta_and_admin(&state).await;

    // Fresh nest: the effective alias policy is the `fauna_mail::aliases`
    // catalog default verbatim.
    let reply = get_alias_policy_as_admin(state, admin).await;
    assert_eq!(reply, AliasPolicy::default());
    assert_eq!(reply.exact_aliases_max, 20);
    assert_eq!(reply.reserved_local_parts.len(), 6);
    assert!(reply.subaddressing_enabled);
    assert!(reply.wildcard_prefix_enabled);
}

#[tokio::test]
async fn admin_put_alias_policy_reflected_in_get_alias_policy() {
    let state = fixture_state().await;
    let (_mta, admin) = mta_and_admin(&state).await;

    let put = PutAliasPolicyRequest {
        exact_aliases_max: Some(3),
        subaddressing_enabled: Some(false),
        reserved_local_parts: Some(vec!["postmaster".into(), "sales".into()]),
        ..Default::default()
    };
    put_as_admin(&state, admin, "fauna.bridges.put_alias_policy", &put).await;

    let reply = get_alias_policy_as_admin(state, admin).await;
    assert_eq!(reply.exact_aliases_max, 3);
    assert!(!reply.subaddressing_enabled);
    assert_eq!(
        reply.reserved_local_parts,
        vec!["postmaster".to_string(), "sales".to_string()]
    );
    // The unmentioned override keeps the catalog default.
    assert!(reply.wildcard_prefix_enabled);
}

#[tokio::test]
async fn get_alias_policy_rejects_non_admin_callers() {
    let state = fixture_state().await;
    let mta = [1u8; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x99u8; 32]).await;
    // No bridge enrollment + no admin row ⇒ this actor resolves as User.
    let user = [7u8; 32];

    for actor in [mta, user] {
        let err = dispatch(
            state.clone(),
            actor,
            "fauna.bridges.get_alias_policy",
            encode(&GetAliasPolicyRequest::default()),
        )
        .await
        .expect_err("non-admin must be denied on get_alias_policy");
        assert_eq!(err.code, "fauna.bridges.permission_denied");
    }
}
