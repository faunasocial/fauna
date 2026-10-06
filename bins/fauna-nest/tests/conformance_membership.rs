//! Integration round-trips for the membership designation —
//! `fauna.admin.membership_tiers.{list,set,clear}` — Pillar 4 of
//! `docs/goal/behavior/monetization.md` (paid nest access), slice N1 step (2).
//!
//! What the designation *is* (monetization.md § Pillar 4 bullet 2): a membership
//! tier is an ordinary `subscription_tiers` row (the entitlement object the admin
//! owns as a payee) plus an Admin-class nest-state link
//! `(admin actor, subscription tier) → { admin_tier, lapse_tier }`, where
//! `admin_tier` is the quota tier (`tiers` — the tier *is* the quota,
//! `admin.md` § 2 Users) an admitted member is assigned and `lapse_tier` is the
//! quota tier a lapsed member degrades to. The two tier systems stay **distinct
//! concepts joined by this explicit link, never merged** — these tests pin that
//! separation: designating never mutates either tier table.
//!
//! Covered:
//! - `set` / `list` / `clear` round-trip, including the `lapse_tier` default (`free`).
//! - `set` is an idempotent upsert (re-designating re-points, never duplicates).
//! - Ownership: an admin may only designate a subscription tier they own; an
//!   unknown subscription tier is refused (the designation carries no FK to
//!   `subscription_tiers`, so existence + ownership are enforced here).
//! - Unknown quota tiers are refused (`admin_tier` / `lapse_tier` must name real
//!   `tiers` rows — admission assigns `users.tier` from the link).
//! - Multiple membership tiers compose (bronze/silver each linked to its own quota tier).
//! - The kinds are Admin-class on the bridge allowlist.
//! - Designating touches neither `tiers` nor `subscription_tiers` (the never-merged rule).
//!
//! Authority for the wire types: `libs/fauna-protocol/src/admin.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::admin_actor;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::Signer;

use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_nest::{
    account_core::{self, RegisterError},
    admin_ws_handlers,
    db::CacheDb,
    invite_core,
    payment_core::{self, PaymentApplied},
    routes::AppState,
    rpc_router::RpcRouter,
    subscription_handlers,
};
use fauna_payments::{Buyer, PaymentEntitlement};
use fauna_protocol::{
    RpcError,
    admin::{
        AdminMembershipTierClearRequest, AdminMembershipTierSetRequest,
        AdminMembershipTiersListReply, AdminMembershipTiersListRequest, AdminOkReply,
        AdminTiersListReply, AdminTiersListRequest,
    },
    decode_strict as decode,
    node_policy::RegistrationMode,
};

/// `AppState::for_test` + a router carrying the admin handlers (and the
/// subscription handlers, so a tier can be minted the way a payee really does).
async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    admin_ws_handlers::register_admin_handlers(&mut b);
    subscription_handlers::register_subscription_handlers(&mut b);
    (b.build(), state)
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

/// Mint a subscription tier owned by `owner` directly through the DB — the
/// payee-side mint (`fauna.subscriptions.tiers.create`) is Pillar-1 machinery
/// already covered by `conformance_subscription_tiers_list.rs`; here it is only
/// the precondition.
async fn mint_subscription_tier(state: &AppState, owner: &[u8; 32], name: &str, rank: i64) {
    state
        .db
        .create_subscription_tier(owner, name, rank, None, None, None, true, None, None, false)
        .await
        .unwrap();
}

async fn set_designation(
    router: &RpcRouter,
    state: &Arc<AppState>,
    admin: [u8; 32],
    tier_name: &str,
    admin_tier: &str,
    lapse_tier: Option<&str>,
) -> Result<AdminOkReply, RpcError> {
    let req = AdminMembershipTierSetRequest {
        tier_name: tier_name.to_string(),
        admin_tier: admin_tier.to_string(),
        lapse_tier: lapse_tier.map(str::to_string),
        extra: Default::default(),
    };
    let out = dispatch(
        router,
        state.clone(),
        admin,
        "fauna.admin.membership_tiers.set",
        encode(&req),
    )
    .await?;
    Ok(decode(&out).unwrap())
}

async fn list_designations(
    router: &RpcRouter,
    state: &Arc<AppState>,
    admin: [u8; 32],
) -> AdminMembershipTiersListReply {
    let out = dispatch(
        router,
        state.clone(),
        admin,
        "fauna.admin.membership_tiers.list",
        encode(&AdminMembershipTiersListRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect("list succeeds");
    decode(&out).unwrap()
}

// ═════════════════════════════════════════════════════════════════════════════
// Round-trip
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn designation_round_trips_with_the_default_lapse_tier() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;

    // `lapse_tier` omitted → defaults to `free` (monetization.md § Pillar 4).
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .expect("designation accepted");

    let reply = list_designations(&router, &state, admin).await;
    assert_eq!(reply.membership_tiers.len(), 1);
    let row = &reply.membership_tiers[0];
    assert_eq!(row.tier_name, "supporter");
    assert_eq!(row.admin_tier, "personal");
    assert_eq!(
        row.lapse_tier, "free",
        "an omitted lapse_tier must default to `free`"
    );
}

#[tokio::test]
async fn an_explicit_lapse_tier_is_preserved() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;

    set_designation(
        &router,
        &state,
        admin,
        "supporter",
        "community",
        Some("personal"),
    )
    .await
    .expect("designation accepted");

    let reply = list_designations(&router, &state, admin).await;
    assert_eq!(reply.membership_tiers[0].admin_tier, "community");
    assert_eq!(reply.membership_tiers[0].lapse_tier, "personal");
}

#[tokio::test]
async fn set_is_an_idempotent_upsert_that_re_points() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;

    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();
    set_designation(
        &router,
        &state,
        admin,
        "supporter",
        "community",
        Some("backup"),
    )
    .await
    .expect("re-designating the same tier is an upsert, not a conflict");

    let reply = list_designations(&router, &state, admin).await;
    assert_eq!(
        reply.membership_tiers.len(),
        1,
        "re-designating must re-point the existing link, never duplicate it"
    );
    assert_eq!(reply.membership_tiers[0].admin_tier, "community");
    assert_eq!(reply.membership_tiers[0].lapse_tier, "backup");
}

#[tokio::test]
async fn clear_removes_the_designation() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();

    let out = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.membership_tiers.clear",
        encode(&AdminMembershipTierClearRequest {
            tier_name: "supporter".to_string(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("clear succeeds");
    let _: AdminOkReply = decode(&out).unwrap();

    assert!(
        list_designations(&router, &state, admin)
            .await
            .membership_tiers
            .is_empty(),
        "clearing must remove the link"
    );
}

#[tokio::test]
async fn clear_of_an_undesignated_tier_is_not_found() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;

    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.membership_tiers.clear",
        encode(&AdminMembershipTierClearRequest {
            tier_name: "supporter".to_string(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("clearing a tier that carries no designation is not_found");
    assert_eq!(err.code, "fauna.admin.not_found");
}

#[tokio::test]
async fn multiple_membership_tiers_compose() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "bronze", 1).await;
    mint_subscription_tier(&state, &admin, "silver", 2).await;

    set_designation(&router, &state, admin, "bronze", "personal", None)
        .await
        .unwrap();
    set_designation(&router, &state, admin, "silver", "community", None)
        .await
        .unwrap();

    let reply = list_designations(&router, &state, admin).await;
    assert_eq!(reply.membership_tiers.len(), 2);
    let bronze = reply
        .membership_tiers
        .iter()
        .find(|m| m.tier_name == "bronze")
        .expect("bronze designated");
    let silver = reply
        .membership_tiers
        .iter()
        .find(|m| m.tier_name == "silver")
        .expect("silver designated");
    assert_eq!(bronze.admin_tier, "personal");
    assert_eq!(silver.admin_tier, "community");
}

// ═════════════════════════════════════════════════════════════════════════════
// Refusals
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn designating_an_unknown_subscription_tier_is_refused() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    // No subscription tier minted.

    let err = set_designation(&router, &state, admin, "ghost", "personal", None)
        .await
        .expect_err("a designation must name a real subscription tier");
    assert_eq!(err.code, "fauna.admin.not_found");
    assert!(
        list_designations(&router, &state, admin)
            .await
            .membership_tiers
            .is_empty()
    );
}

#[tokio::test]
async fn designating_another_actors_subscription_tier_is_refused() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    let other = [9u8; 32];
    // The tier exists, but belongs to someone else — the admin is a payee like
    // any other and may only designate their OWN tiers.
    mint_subscription_tier(&state, &other, "supporter", 1).await;

    let err = set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .expect_err("an admin may not designate a tier they do not own");
    assert_eq!(err.code, "fauna.admin.not_found");
    assert!(
        list_designations(&router, &state, admin)
            .await
            .membership_tiers
            .is_empty()
    );
}

#[tokio::test]
async fn designating_an_unknown_quota_tier_is_refused() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;

    let err = set_designation(&router, &state, admin, "supporter", "platinum", None)
        .await
        .expect_err(
            "admin_tier must name a real quota tier — admission assigns users.tier from it",
        );
    assert_eq!(err.code, "fauna.admin.invalid_params");

    let err = set_designation(
        &router,
        &state,
        admin,
        "supporter",
        "personal",
        Some("platinum"),
    )
    .await
    .expect_err("lapse_tier must name a real quota tier too");
    assert_eq!(err.code, "fauna.admin.invalid_params");

    assert!(
        list_designations(&router, &state, admin)
            .await
            .membership_tiers
            .is_empty(),
        "a refused designation must leave no row behind"
    );
}

#[tokio::test]
async fn a_non_admin_is_refused() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;

    // A REGISTERED non-admin user, not merely an unknown keypair: the family
    // refusal code (`class_refusal_namespace`) is derived from the KIND's own
    // listed class when the caller resolves to a real, wrong-class actor.
    // An unknown/unregistered actor takes a different path entirely — the
    // central `fauna.bridges.permission_denied` (`bridge_method_allowlist.rs`
    // § the handler layer's one refusal seam) — which is not what
    // this test means to pin.
    let stranger = [11u8; 32];
    state
        .db
        .create_user_with_handle(&stranger, "free", "stranger", None)
        .await
        .unwrap();
    let err = set_designation(&router, &state, stranger, "supporter", "personal", None)
        .await
        .expect_err("membership designation is Admin-class");
    assert_eq!(err.code, "fauna.admin.permission_denied");
}

// ═════════════════════════════════════════════════════════════════════════════
// The never-merged rule + the class gate
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn designating_does_not_mutate_either_tier_system() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;

    let quota_before: AdminTiersListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.tiers.list",
            encode(&AdminTiersListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let subs_before = state.db.list_subscription_tiers(&admin).await.unwrap();

    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();

    let quota_after: AdminTiersListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            admin,
            "fauna.admin.tiers.list",
            encode(&AdminTiersListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let subs_after = state.db.list_subscription_tiers(&admin).await.unwrap();

    assert_eq!(
        quota_before.tiers, quota_after.tiers,
        "the designation is a LINK — it must never mutate the quota-tier table"
    );
    assert_eq!(
        subs_before.len(),
        subs_after.len(),
        "the designation is a LINK — it must never mutate the subscription-tier table"
    );
}

// (The bridge-allowlist class gate + replay metadata for these three kinds ride
// the established `ADMIN_KINDS` sweep in `conformance_admin.rs` — the handler's
// own `require_permission` gate is pinned by `a_non_admin_is_refused` above,
// which is a distinct mechanism.)

// ═════════════════════════════════════════════════════════════════════════════
// Slice N1 step (3) — ADMISSION ARMS (monetization.md § Pillar 4 Rail C step 1)
//
// The designation from step (2) becomes load-bearing: an entitlement (a claim
// or a webhook event) to a MEMBERSHIP tier admits the buyer at the linked quota
// tier, instead of unlocking content keys. Two arms:
//   (a) register_core   — a paid claim code doubles as an invite code.
//   (b) grant_paid_entitlement — a payment-backed pending invite request
//       auto-approves; an existing user's quota tier is reassigned.
// The target axis lives on the tier's designation, so the SAME waist value routes
// to nest membership vs. content keys purely by whether a designation exists.
// ═════════════════════════════════════════════════════════════════════════════

fn now_ms() -> u64 {
    fauna_core::data::Timestamp::now_millis()
}

fn far_future() -> i64 {
    // Comfortably beyond any `is_subscriber` "now" check within a test run.
    (fauna_core::data::Timestamp::now_secs()) + 86_400
}

async fn set_mode(state: &Arc<AppState>, mode: RegistrationMode) {
    *state.registration_mode.write().await = (mode, None);
}

/// Apply a bound-buyer payment through the one engine entry point and return
/// `queued` (mirrors `conformance_payments.rs::grant`).
async fn apply_membership_payment(
    state: &Arc<AppState>,
    payee: &[u8; 32],
    buyer: &[u8; 32],
    tier: &str,
    valid_until: Option<i64>,
) -> bool {
    let e = PaymentEntitlement {
        provider: "fake".into(),
        payee: ActorId(*payee),
        buyer: Buyer::Actor(ActorId(*buyer)),
        tier: tier.into(),
        valid_until_secs: valid_until.map(|s| s as u64),
        external_ref: "mem_pay_1".into(),
    };
    match payment_core::apply_payment(state, &e)
        .await
        .expect("apply_payment ok")
    {
        PaymentApplied::Granted { queued } => queued,
        other => panic!("a bound membership buyer must grant, got {other:?}"),
    }
}

/// Register through the real `register_core` ceremony, signing over
/// `actor_id ‖ handle ‖ domain ‖ timestamp_be` exactly as a client does, and
/// passing `code` in the invite-code slot (a paid claim doubles as one).
async fn register_with_code(
    state: &Arc<AppState>,
    kp: &ActorKeypair,
    handle: &str,
    code: &str,
) -> Result<account_core::RegisterOutcome, RegisterError> {
    let domain = state.handle_domain();
    let ts = now_ms();
    let msg =
        fauna_protocol::account::register_signed_message(&kp.actor_id().0, handle, &domain, ts);
    let sig = kp.signing_key().sign(&msg);
    account_core::register_core(
        state,
        &kp.actor_id_hex(),
        handle,
        ts,
        &hex::encode(sig.to_bytes()),
        Some(code),
        // No declared band — this helper registers an ordinary adult seat
        // (`family-safety.md` § The account age band: absence is the
        // by-construction 18+/none case).
        None,
    )
    .await
}

// ── get_membership_tier (the per-pair lookup admission uses) ───────────────

#[tokio::test]
async fn get_membership_tier_reads_the_designated_pair_only() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    mint_subscription_tier(&state, &admin, "plain", 2).await;
    set_designation(
        &router,
        &state,
        admin,
        "supporter",
        "personal",
        Some("free"),
    )
    .await
    .unwrap();

    let hit = state
        .db
        .get_membership_tier(&admin, "supporter")
        .await
        .unwrap()
        .expect("the designated pair resolves");
    assert_eq!(hit.admin_tier, "personal");
    assert_eq!(hit.lapse_tier, "free");

    // An undesignated tier of the same admin → None (not a membership tier).
    assert!(
        state
            .db
            .get_membership_tier(&admin, "plain")
            .await
            .unwrap()
            .is_none()
    );
    // Caller-scoped: another actor's read of this admin's tier name → None.
    let other = [42u8; 32];
    assert!(
        state
            .db
            .get_membership_tier(&other, "supporter")
            .await
            .unwrap()
            .is_none()
    );
}

// ── Arm (b): payment-driven admission ──────────────────────────────────────

#[tokio::test]
async fn payment_backed_pending_invite_request_auto_approves() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();

    // A prospective member with no account submits an invite request.
    let prospect = [3u8; 32];
    state
        .db
        .create_invite_request(&prospect, "newbie", "let me in", None)
        .await
        .unwrap();
    assert!(!state.db.is_actor_registered(&prospect).await.unwrap());

    let until = far_future();
    let queued =
        apply_membership_payment(&state, &admin, &prospect, "supporter", Some(until)).await;
    assert!(
        !queued,
        "membership admission is active now, never client-queued"
    );

    // The account was created at the linked quota tier — no admin judgment.
    let user = state
        .db
        .get_user(&prospect)
        .await
        .unwrap()
        .expect("the pending request was auto-approved into an account");
    assert_eq!(user.tier, "personal", "admitted at the linked admin_tier");
    assert_eq!(
        state.db.resolve_handle("newbie").await.unwrap(),
        Some(prospect),
        "the requested handle now resolves to the admitted actor"
    );
    // The request is consumed.
    assert!(
        state
            .db
            .get_invite_request_by_actor(&prospect)
            .await
            .unwrap()
            .is_none(),
        "an auto-approved request is deleted like an admin-approved one"
    );
    // The membership subscription is recorded with its paid window.
    assert!(
        state
            .db
            .is_subscriber(&admin, &prospect, "supporter")
            .await
            .unwrap()
    );
    assert_eq!(
        state
            .db
            .get_subscriber_valid_until(&admin, &prospect, "supporter")
            .await
            .unwrap(),
        Some(until),
    );
}

#[tokio::test]
async fn existing_user_membership_payment_reassigns_the_quota_tier() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "community", None)
        .await
        .unwrap();

    // An already-registered free user with an admin-set label.
    let member = [4u8; 32];
    state
        .db
        .create_user_with_handle(&member, "free", "existing", None)
        .await
        .unwrap();
    state
        .db
        .update_user(&member, "free", "VIP note")
        .await
        .unwrap();

    apply_membership_payment(&state, &admin, &member, "supporter", Some(far_future())).await;

    let user = state.db.get_user(&member).await.unwrap().unwrap();
    assert_eq!(
        user.tier, "community",
        "an existing member is reassigned to admin_tier"
    );
    assert_eq!(
        user.label, "VIP note",
        "a payment must not clobber an admin-set label (set_user_tier, not update_user)"
    );
    assert!(
        state
            .db
            .is_subscriber(&admin, &member, "supporter")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_content_tier_payment_never_touches_nest_membership() {
    // The regression guard: a payment to a tier with NO designation stays on the
    // content-entitlement path — it must not reassign users.tier.
    let (_router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "plain", 1).await; // no designation

    let member = [5u8; 32];
    state
        .db
        .create_user_with_handle(&member, "free", "reader", None)
        .await
        .unwrap();

    // A content-tier grant enqueues rather than admits (client-minted tier).
    let queued =
        apply_membership_payment(&state, &admin, &member, "plain", Some(far_future())).await;
    assert!(
        queued,
        "a client-minted content tier enqueues for the author to mint"
    );

    let user = state.db.get_user(&member).await.unwrap().unwrap();
    assert_eq!(
        user.tier, "free",
        "a content-tier payment must never reassign the buyer's quota tier"
    );
}

// ── Arm (a): claim-code admission at registration ──────────────────────────

#[tokio::test]
async fn a_membership_claim_code_admits_at_the_linked_quota_tier() {
    let (router, state) = router_and_state().await;
    set_mode(&state, RegistrationMode::Open).await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();

    let until = far_future();
    let code = "MEMBERCLAIM01";
    state
        .db
        .insert_payment_claim(code, &admin, "supporter", "manual", "ext_1", Some(until))
        .await
        .unwrap();

    let kp = ActorKeypair::generate();
    let outcome = register_with_code(&state, &kp, "claimer", code)
        .await
        .expect("a valid membership claim admits at registration");
    assert_eq!(
        outcome.tier, "personal",
        "admitted at the linked admin_tier"
    );

    let user = state.db.get_user(&kp.actor_id().0).await.unwrap().unwrap();
    assert_eq!(user.tier, "personal");
    // The claim is stamped redeemed by the new account.
    let claim = state.db.get_payment_claim(code).await.unwrap().unwrap();
    assert_eq!(claim.redeemed_by.as_deref(), Some(&kp.actor_id().0[..]));
    // The membership subscription is recorded with its paid window.
    assert!(
        state
            .db
            .is_subscriber(&admin, &kp.actor_id().0, "supporter")
            .await
            .unwrap()
    );
    assert_eq!(
        state
            .db
            .get_subscriber_valid_until(&admin, &kp.actor_id().0, "supporter")
            .await
            .unwrap(),
        Some(until),
    );
}

// ── verify_invite_code_core's membership-claim peek fallback ───────────────
//
// `register_core` has always admitted a membership claim pasted into the
// invite-code slot (the test above). But the wizard's `Continue` button gates
// on the SEPARATE `fauna.account.invite_code.verify` pre-check, which — before
// this fix — only ever consulted the invite-code table: a prospective member
// pasting a valid membership claim code was refused before they could ever
// reach `register`. These pin the peek-only fallback
// (`invite_core::verify_invite_code_core` → `account_core::peek_membership_claim`).

#[tokio::test]
async fn verify_recognizes_a_membership_claim_code_without_consuming_it() {
    let (router, state) = router_and_state().await;
    set_mode(&state, RegistrationMode::Open).await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();
    let code = "MEMBERCLAIMVERIFY01";
    state
        .db
        .insert_payment_claim(code, &admin, "supporter", "manual", "ext_v1", None)
        .await
        .unwrap();

    let (invite_id, supervised_by, age_band) = invite_core::verify_invite_code_core(&state, code)
        .await
        .expect("a valid membership claim code must verify, not just admit at register");
    assert_eq!(invite_id, code, "round-trips the code itself");
    assert_eq!(
        supervised_by, None,
        "a membership claim carries no guardian"
    );
    assert_eq!(
        age_band, None,
        "a membership claim carries no age band either — the banded carry          rides the invite-code table, and this arm never touches it"
    );

    // Peek-only: the claim is still unredeemed after verify, so it still
    // admits at registration afterward — the prospective member's actual path.
    assert!(
        state
            .db
            .get_payment_claim(code)
            .await
            .unwrap()
            .unwrap()
            .redeemed_by
            .is_none(),
        "verify must not consume the one-shot claim"
    );
    let kp = ActorKeypair::generate();
    let outcome = register_with_code(&state, &kp, "claimer2", code)
        .await
        .expect("the same claim still admits at register after a prior verify");
    assert_eq!(outcome.tier, "personal");
}

#[tokio::test]
async fn verify_refuses_a_content_tier_claim_the_same_way_register_does() {
    // A claim for a tier with no membership designation must not verify as
    // valid either — the same fail-closed rule `resolve_membership_claim`
    // already enforces at registration.
    let (_router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "plain", 1).await; // NOT designated
    let code = "CONTENTCLAIMVERIFY01";
    state
        .db
        .insert_payment_claim(code, &admin, "plain", "manual", "ext_v2", None)
        .await
        .unwrap();

    let err = invite_core::verify_invite_code_core(&state, code)
        .await
        .expect_err("a content-tier claim is not an invite code, verify-side either");
    assert!(matches!(err, invite_core::InviteError::InviteCodeInvalid));
}

#[tokio::test]
async fn a_membership_claim_is_refused_when_registration_is_closed() {
    let (router, state) = router_and_state().await;
    set_mode(&state, RegistrationMode::Closed).await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();
    let code = "MEMBERCLAIM02";
    state
        .db
        .insert_payment_claim(code, &admin, "supporter", "manual", "ext_2", None)
        .await
        .unwrap();

    let kp = ActorKeypair::generate();
    let err = register_with_code(&state, &kp, "wouldbe", code)
        .await
        .expect_err("closed mode refuses self-service admission, paid or not");
    assert!(
        matches!(err, RegisterError::RegistrationClosed),
        "got {err:?}"
    );
    // No account, and the claim is untouched (still redeemable once mode opens).
    assert!(
        !state
            .db
            .is_actor_registered(&kp.actor_id().0)
            .await
            .unwrap()
    );
    assert!(
        state
            .db
            .get_payment_claim(code)
            .await
            .unwrap()
            .unwrap()
            .redeemed_by
            .is_none()
    );
}

#[tokio::test]
async fn a_content_tier_claim_does_not_admit_at_registration() {
    // A claim for a tier with NO membership designation is a content-tier claim —
    // redeemable only by an already-logged-in client, never an admission ticket.
    let (_router, state) = router_and_state().await;
    set_mode(&state, RegistrationMode::Open).await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "plain", 1).await; // NOT designated
    let code = "CONTENTCLAIM01";
    state
        .db
        .insert_payment_claim(code, &admin, "plain", "manual", "ext_3", None)
        .await
        .unwrap();

    let kp = ActorKeypair::generate();
    let err = register_with_code(&state, &kp, "sneaky", code)
        .await
        .expect_err("a content-tier claim is not an invite code");
    assert!(
        matches!(err, RegisterError::InvalidRequest(_)),
        "got {err:?}"
    );
    assert!(
        !state
            .db
            .is_actor_registered(&kp.actor_id().0)
            .await
            .unwrap()
    );
    // A non-membership claim is NOT consumed (it remains a valid content claim).
    assert!(
        state
            .db
            .get_payment_claim(code)
            .await
            .unwrap()
            .unwrap()
            .redeemed_by
            .is_none()
    );
}

#[tokio::test]
async fn an_already_redeemed_membership_claim_does_not_admit_twice() {
    let (router, state) = router_and_state().await;
    set_mode(&state, RegistrationMode::Open).await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();
    let code = "MEMBERCLAIM03";
    state
        .db
        .insert_payment_claim(code, &admin, "supporter", "manual", "ext_4", None)
        .await
        .unwrap();

    // First actor consumes the claim.
    let first = ActorKeypair::generate();
    register_with_code(&state, &first, "firstin", code)
        .await
        .expect("first redemption admits");

    // A second actor presenting the same code is refused — one paid claim, one
    // account (the redeem-first double-spend guard).
    let second = ActorKeypair::generate();
    let err = register_with_code(&state, &second, "secondin", code)
        .await
        .expect_err("an already-redeemed claim cannot admit a second account");
    assert!(
        matches!(err, RegisterError::InvalidRequest(_)),
        "got {err:?}"
    );
    assert!(
        !state
            .db
            .is_actor_registered(&second.actor_id().0)
            .await
            .unwrap()
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Slice N1 step (4) — LAPSE RECONCILE (monetization.md § Pillar 4 Rail C step 3)
//
// A membership window (`subscribers.valid_until`) expires silently. `reconcile_
// lapsed_memberships` degrades a member still at the link's admin_tier down to
// lapse_tier — a reversible quota downgrade, never suspension, never data loss.
// Two guards: `users.tier = admin_tier` (never clobbers a manual change or a
// higher membership) and "no other active membership" (the compose case).
// ═════════════════════════════════════════════════════════════════════════════

fn far_past() -> i64 {
    (fauna_core::data::Timestamp::now_secs()) - 86_400
}

/// Register a member at `start_tier`, subscribe them to `tier_name` under
/// `admin`, and stamp the window — the state the reconcile reads.
///
/// The admission pair is frozen from the designation **as it stands now**,
/// which is exactly what the production admission arms do at this moment; that
/// keeps the fixture honest while letting a test re-point or clear the link
/// afterwards to exercise the freeze.
async fn seed_member(
    state: &Arc<AppState>,
    admin: &[u8; 32],
    member: &[u8; 32],
    handle: &str,
    start_tier: &str,
    tier_name: &str,
    valid_until: Option<i64>,
) {
    state
        .db
        .create_user_with_handle(member, start_tier, handle, None)
        .await
        .unwrap();
    seed_membership_row(state, admin, member, tier_name, valid_until).await;
}

/// The membership half of [`seed_member`], for a member who already exists (the
/// compose case seeds two tiers onto one member).
async fn seed_membership_row(
    state: &Arc<AppState>,
    admin: &[u8; 32],
    member: &[u8; 32],
    tier_name: &str,
    valid_until: Option<i64>,
) {
    state
        .db
        .add_subscriber(admin, member, tier_name, None)
        .await
        .unwrap();
    state
        .db
        .set_subscriber_valid_until(admin, member, tier_name, valid_until)
        .await
        .unwrap();
    let d = state
        .db
        .get_membership_tier(admin, tier_name)
        .await
        .unwrap()
        .expect("seed_membership_row needs a designation to freeze");
    state
        .db
        .stamp_membership_admission(admin, member, tier_name, &d.admin_tier, &d.lapse_tier)
        .await
        .unwrap();
}

#[tokio::test]
async fn lapse_reconcile_downgrades_an_expired_member() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None) // lapse → free
        .await
        .unwrap();
    let member = [21u8; 32];
    seed_member(
        &state,
        &admin,
        &member,
        "lapser",
        "personal",
        "supporter",
        Some(far_past()),
    )
    .await;

    let n = state.db.reconcile_lapsed_memberships(None).await.unwrap();
    assert_eq!(n, 1, "one member lapsed");
    assert_eq!(
        state.db.get_user(&member).await.unwrap().unwrap().tier,
        "free",
        "an expired member degrades to the link's lapse_tier"
    );
}

#[tokio::test]
async fn lapse_reconcile_leaves_an_active_member_untouched() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();
    let member = [22u8; 32];
    seed_member(
        &state,
        &admin,
        &member,
        "active",
        "personal",
        "supporter",
        Some(far_future()),
    )
    .await;

    let n = state.db.reconcile_lapsed_memberships(None).await.unwrap();
    assert_eq!(n, 0);
    assert_eq!(
        state.db.get_user(&member).await.unwrap().unwrap().tier,
        "personal",
        "a still-active member keeps their quota tier"
    );
}

#[tokio::test]
async fn lapse_reconcile_never_clobbers_a_manual_tier_change() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();
    // The member's membership expired, but an admin has since moved them to
    // `community` by hand — the `users.tier = admin_tier` guard must skip them.
    let member = [23u8; 32];
    seed_member(
        &state,
        &admin,
        &member,
        "manual",
        "community",
        "supporter",
        Some(far_past()),
    )
    .await;

    let n = state.db.reconcile_lapsed_memberships(None).await.unwrap();
    assert_eq!(n, 0, "a member no longer at admin_tier is never downgraded");
    assert_eq!(
        state.db.get_user(&member).await.unwrap().unwrap().tier,
        "community",
        "a manual tier change survives the lapse sweep"
    );
}

#[tokio::test]
async fn lapse_reconcile_skips_a_member_holding_another_active_membership() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "bronze", 1).await;
    mint_subscription_tier(&state, &admin, "gold", 2).await;
    set_designation(&router, &state, admin, "bronze", "personal", None)
        .await
        .unwrap();
    set_designation(&router, &state, admin, "gold", "community", None)
        .await
        .unwrap();

    // Member sits at `community` (gold). Gold expired; bronze is still active.
    // The conservative guard must not strand them below their active bronze.
    let member = [24u8; 32];
    state
        .db
        .create_user_with_handle(&member, "community", "composer", None)
        .await
        .unwrap();
    seed_membership_row(&state, &admin, &member, "gold", Some(far_past())).await;
    seed_membership_row(&state, &admin, &member, "bronze", Some(far_future())).await;

    let n = state.db.reconcile_lapsed_memberships(None).await.unwrap();
    assert_eq!(n, 0, "a member with any active membership is left alone");
    assert_eq!(
        state.db.get_user(&member).await.unwrap().unwrap().tier,
        "community",
        "a still-active membership prevents the lapsed one from downgrading them"
    );
}

#[tokio::test]
async fn a_renewal_restores_a_lapsed_member() {
    // The other half of the lifecycle: a lapsed member (already at lapse_tier)
    // who pays again is restored to admin_tier by the step-3 grant_membership path.
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();
    let member = [25u8; 32];
    // Already lapsed: registered at `free`, with an expired supporter row.
    seed_member(
        &state,
        &admin,
        &member,
        "renewer",
        "free",
        "supporter",
        Some(far_past()),
    )
    .await;

    apply_membership_payment(&state, &admin, &member, "supporter", Some(far_future())).await;

    assert_eq!(
        state.db.get_user(&member).await.unwrap().unwrap().tier,
        "personal",
        "a renewal payment restores the lapsed member to the linked admin_tier"
    );
    // ...and the window is future again, so a follow-up sweep is a no-op.
    let n = state.db.reconcile_lapsed_memberships(None).await.unwrap();
    assert_eq!(n, 0);
    assert_eq!(
        state.db.get_user(&member).await.unwrap().unwrap().tier,
        "personal"
    );
}

#[tokio::test]
async fn lapse_reconcile_can_scope_to_one_buyer() {
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();
    let a = [26u8; 32];
    let b = [27u8; 32];
    seed_member(
        &state,
        &admin,
        &a,
        "expa",
        "personal",
        "supporter",
        Some(far_past()),
    )
    .await;
    seed_member(
        &state,
        &admin,
        &b,
        "expb",
        "personal",
        "supporter",
        Some(far_past()),
    )
    .await;

    // Scoped reconcile touches only `a` — the webhook-ingress-for-that-buyer path.
    let n = state
        .db
        .reconcile_lapsed_memberships(Some(&a))
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(state.db.get_user(&a).await.unwrap().unwrap().tier, "free");
    assert_eq!(
        state.db.get_user(&b).await.unwrap().unwrap().tier,
        "personal",
        "a buyer-scoped reconcile leaves other lapsed members for the sweep"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Lapse policy freezes at ADMISSION, not at sweep time.
//
// The reconcile used to JOIN the LIVE `membership_tiers` link, so the admin's
// own later edit silently stranded already-admitted members above `lapse_tier`
// forever — quota-accounting leak, unbounded, with no surface that shows it.
// Both arms below admit through the REAL payment path (`apply_payment` →
// `grant_membership`), so what they pin is the production stamp, not a fixture.
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn a_re_pointed_designation_still_lapses_the_member_admitted_under_the_old_pair() {
    // Arm 1 (the pre-analyzed case): admin re-points admin_tier
    // personal → community after admission. The member is still sitting at
    // `personal`, so a live-link derivation finds `u.tier != mt.admin_tier` and
    // skips them forever.
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();

    let member = [28u8; 32];
    state
        .db
        .create_user_with_handle(&member, "free", "repointed", None)
        .await
        .unwrap();
    // Admitted under (personal, free) through the production engine.
    apply_membership_payment(&state, &admin, &member, "supporter", Some(far_past())).await;
    assert_eq!(
        state.db.get_user(&member).await.unwrap().unwrap().tier,
        "personal",
        "precondition: admission assigned the then-current admin_tier"
    );

    // The admin now re-points the link to a different quota tier.
    set_designation(&router, &state, admin, "supporter", "community", None)
        .await
        .unwrap();

    let n = state.db.reconcile_lapsed_memberships(None).await.unwrap();
    assert_eq!(
        n, 1,
        "a re-pointed designation must not strand a member admitted under the old pair"
    );
    assert_eq!(
        state.db.get_user(&member).await.unwrap().unwrap().tier,
        "free",
        "lapse degrades to the ADMISSION-time lapse_tier, not the re-pointed link's"
    );
}

#[tokio::test]
async fn a_cleared_designation_still_lapses_its_already_admitted_members() {
    // Arm 2 (the larger arm, untouched by any re-point-only fix): the admin
    // clears the designation outright. The live-link JOIN then matches no row,
    // so EVERY expired member of that tier escapes lapse entirely.
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();

    let member = [29u8; 32];
    state
        .db
        .create_user_with_handle(&member, "free", "cleared", None)
        .await
        .unwrap();
    apply_membership_payment(&state, &admin, &member, "supporter", Some(far_past())).await;
    assert_eq!(
        state.db.get_user(&member).await.unwrap().unwrap().tier,
        "personal",
        "precondition: admission assigned the linked admin_tier"
    );

    // The admin retires the membership designation entirely.
    let out = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.membership_tiers.clear",
        encode(&AdminMembershipTierClearRequest {
            tier_name: "supporter".to_string(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("clear succeeds");
    let _: AdminOkReply = decode(&out).unwrap();

    let n = state.db.reconcile_lapsed_memberships(None).await.unwrap();
    assert_eq!(
        n, 1,
        "clearing the designation must not cancel the lapse of members already admitted under it"
    );
    assert_eq!(
        state.db.get_user(&member).await.unwrap().unwrap().tier,
        "free",
        "the admission-time lapse_tier still governs after the link is gone"
    );
}

#[tokio::test]
async fn a_claim_code_admitted_member_lapses_after_the_designation_is_cleared() {
    // The OTHER production admission arm (registration-time claim redemption,
    // `account_core::resolve_membership_claim`) must freeze the pair too —
    // otherwise a member admitted by claim code silently never lapses.
    let (router, state) = router_and_state().await;
    set_mode(&state, RegistrationMode::Open).await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();

    // A paid window that has already elapsed by the time the sweep runs.
    let code = "MEMBERCLAIM03";
    state
        .db
        .insert_payment_claim(
            code,
            &admin,
            "supporter",
            "manual",
            "ext_3",
            Some(far_past()),
        )
        .await
        .unwrap();
    let kp = ActorKeypair::generate();
    let outcome = register_with_code(&state, &kp, "claimlapser", code)
        .await
        .expect("a valid membership claim admits at registration");
    assert_eq!(
        outcome.tier, "personal",
        "precondition: admitted at admin_tier"
    );

    // Designation retired after admission.
    let out = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.membership_tiers.clear",
        encode(&AdminMembershipTierClearRequest {
            tier_name: "supporter".to_string(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("clear succeeds");
    let _: AdminOkReply = decode(&out).unwrap();

    let n = state.db.reconcile_lapsed_memberships(None).await.unwrap();
    assert_eq!(
        n, 1,
        "the registration arm must freeze the pair at admission too"
    );
    assert_eq!(
        state
            .db
            .get_user(&kp.actor_id().0)
            .await
            .unwrap()
            .unwrap()
            .tier,
        "free",
    );
}

#[tokio::test]
async fn a_frozen_lapse_tier_that_no_longer_exists_degrades_to_free() {
    // The stamp is a frozen historical value with no FK to `tiers` (an FK would
    // let a stale stamp block an admin from ever deleting a quota tier). So the
    // admin CAN delete the tier a stamp names. `users.tier` does have an FK, so
    // the reconcile must resolve the vanished tier rather than emit an UPDATE
    // that fails — which would abort the sweep for every other member too.
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    // Admitted with a lapse tier that is later removed from the quota system.
    set_designation(
        &router,
        &state,
        admin,
        "supporter",
        "community",
        Some("personal"),
    )
    .await
    .unwrap();

    let member = [31u8; 32];
    seed_member(
        &state,
        &admin,
        &member,
        "vanishing",
        "community",
        "supporter",
        Some(far_past()),
    )
    .await;
    // A second, ordinary lapser proves the sweep is not aborted for others.
    let bystander = [32u8; 32];
    seed_member(
        &state,
        &admin,
        &bystander,
        "bystander",
        "community",
        "supporter",
        Some(far_past()),
    )
    .await;

    // Retire the designation, then the quota tier the stamps name.
    let out = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.admin.membership_tiers.clear",
        encode(&AdminMembershipTierClearRequest {
            tier_name: "supporter".to_string(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("clear succeeds");
    let _: AdminOkReply = decode(&out).unwrap();
    // No `fauna.admin.tiers.delete` kind exists today (list/create/update only),
    // so this state is only reachable by construction — which is exactly why the
    // guard is worth pinning: a future session adding tier deletion would
    // otherwise silently break the sweep for every member, not just this one.
    {
        let conn = state.db.conn().await;
        conn.execute("DELETE FROM tiers WHERE name = 'personal'", [])
            .unwrap();
    }

    let n = state.db.reconcile_lapsed_memberships(None).await.unwrap();
    assert_eq!(n, 2, "a vanished lapse tier must not abort the sweep");
    assert_eq!(
        state.db.get_user(&member).await.unwrap().unwrap().tier,
        "free",
        "a frozen lapse tier that no longer exists degrades to the seeded floor"
    );
    assert_eq!(
        state.db.get_user(&bystander).await.unwrap().unwrap().tier,
        "free"
    );
}

#[tokio::test]
async fn a_content_subscription_is_never_treated_as_a_membership_by_the_sweep() {
    // The stamp is also what *identifies* a membership row now that the live-link
    // JOIN is gone. An ordinary content subscription carries no stamp and must
    // stay invisible to the sweep — including as the "other active membership"
    // that would wrongly shield a genuinely lapsed member.
    let (router, state) = router_and_state().await;
    let admin = admin_actor(&state).await;
    mint_subscription_tier(&state, &admin, "supporter", 1).await;
    mint_subscription_tier(&state, &admin, "plain", 2).await; // no designation
    set_designation(&router, &state, admin, "supporter", "personal", None)
        .await
        .unwrap();

    let member = [30u8; 32];
    seed_member(
        &state,
        &admin,
        &member,
        "mixed",
        "personal",
        "supporter",
        Some(far_past()),
    )
    .await;
    // An active *content* subscription alongside the lapsed membership.
    state
        .db
        .add_subscriber(&admin, &member, "plain", None)
        .await
        .unwrap();
    state
        .db
        .set_subscriber_valid_until(&admin, &member, "plain", Some(far_future()))
        .await
        .unwrap();

    let n = state.db.reconcile_lapsed_memberships(None).await.unwrap();
    assert_eq!(
        n, 1,
        "an active content subscription must not shield a lapsed membership"
    );
    assert_eq!(
        state.db.get_user(&member).await.unwrap().unwrap().tier,
        "free"
    );
}
