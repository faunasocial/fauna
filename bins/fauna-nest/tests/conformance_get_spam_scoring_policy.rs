//! Conformance for `fauna.bridges.get_spam_scoring_policy` — the **User-class**
//! read of the admin-effective spam-scoring policy the on-device Fauna-app
//! scorer needs so its INBOX→Junk placement is byte-identical to the MDA/nest
//! at every scoring position (`docs/goal/behavior/mail-spam.md` § Architectural
//! rules "Scoring placement = search placement", § Combined-score formula). The
//! Admin-only `fetch_config` / `get_mail_config` twin is unreachable to a
//! client; this getter projects the four scoring knobs a `User` may read from
//! the effective `SpamPolicyThresholds` (catalog defaults + the admin
//! `put_spam_policy` override applied).
//!
//! **tier_3** — real router dispatch (not a WS mock) through the actual
//! `register_bridge_imap_handlers` registration, against a real in-memory DB.
//! No stubs.
//!
//! Authority: `docs/goal/behavior/mail-spam.md` § Scoring placement.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::bridge_imap_handlers::register_bridge_imap_handlers;
use fauna_nest::bridge_method_allowlist::{CallerClass, is_permitted};
use fauna_nest::db::CacheDb;
use fauna_nest::db::mail_policy::SpamPolicyOverrides;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::wrapped_blob::{GetSpamScoringPolicyReply, GetSpamScoringPolicyRequest};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_bridge_imap_handlers(&mut b);
    (b.build(), state)
}

async fn get_policy(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
) -> Result<GetSpamScoringPolicyReply, RpcError> {
    common::seed_dispatch_actor(&state.db, &caller).await;
    let payload = Bytes::from(
        encode_canonical(&GetSpamScoringPolicyRequest::default())
            .expect("encode req")
            .to_vec(),
    );
    let meta = router
        .kind_meta("fauna.bridges.get_spam_scoring_policy")
        .expect("kind registered");
    let reply_bytes = (meta.handler)(state.clone(), caller, payload).await?;
    Ok(decode(&reply_bytes).expect("decode reply"))
}

// ── (a) a User caller reads the catalog-default policy ────────────

#[tokio::test]
async fn user_reads_default_policy() {
    let (router, state) = router_and_state().await;
    // A bare, non-bridge actor resolves to `CallerClass::User`.
    let user: [u8; 32] = [0x42; 32];

    let reply = get_policy(&router, &state, user)
        .await
        .expect("a User may read the deployment scoring policy");

    // The `SpamPolicyThresholds::default()` catalog values.
    assert_eq!(reply.spam_folder_threshold, 5);
    assert_eq!(reply.bayesian_weight_milli, 700);
    assert_eq!(reply.bayesian_min_samples, 50);
    assert_eq!(reply.bayesian_full_confidence_samples, 200);
}

// ── (b) the getter reflects an admin override ─────────────────────

#[tokio::test]
async fn user_reads_overridden_policy() {
    let (router, state) = router_and_state().await;
    let user: [u8; 32] = [0x42; 32];

    // The admin override (written the same way `put_spam_policy_handler` writes
    // it, via the DB layer). `effective()` overlays each `Some` onto the
    // catalog default, so the getter reflects the overridden values.
    state
        .db
        .put_spam_policy(SpamPolicyOverrides {
            max_score_before_spam_folder: Some(8),
            bayesian_weight_milli: Some(900),
            bayesian_min_samples: Some(20),
            bayesian_full_confidence_samples: Some(400),
            ..Default::default()
        })
        .await
        .expect("store spam policy override");

    let reply = get_policy(&router, &state, user)
        .await
        .expect("a User reads the overridden policy");

    assert_eq!(reply.spam_folder_threshold, 8);
    assert_eq!(reply.bayesian_weight_milli, 900);
    assert_eq!(reply.bayesian_min_samples, 20);
    assert_eq!(reply.bayesian_full_confidence_samples, 400);
}

// ── (c) allowlist: User/Admin/BridgeMda admitted, BridgeMta denied ─

#[test]
fn allowlist_admits_scorer_legs_denies_mta() {
    for class in [
        CallerClass::User,
        CallerClass::Admin,
        CallerClass::BridgeMda,
    ] {
        assert!(
            is_permitted(class, "fauna.bridges.get_spam_scoring_policy"),
            "get_spam_scoring_policy should be permitted for {class:?}"
        );
    }
    // The MTA has no scoring-policy read leg (same class set as
    // `fetch_spam_model`, which excludes BridgeMta).
    assert!(
        !is_permitted(
            CallerClass::BridgeMta,
            "fauna.bridges.get_spam_scoring_policy"
        ),
        "get_spam_scoring_policy should be denied for BridgeMta"
    );
}
