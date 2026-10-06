//! The controversial-class feature gate's **enforcement floor**, end to end
//! through a real gate surface — W2 (account-data-plane.md § Workstreams) slice 2 of
//! `docs/goal/architecture/dynamic-features.md`.
//!
//! § Evaluation points states what these tests exist to prove: *"Nest-side
//! enforcement is the floor … the gate holds against non-conforming and
//! version-skewed clients (an old client that predates gating calls the old
//! kinds and receives the typed refusal — no bypass)."* Every test below drives
//! `fauna.payments.claims.redeem` — the shipped kind, unchanged on the wire —
//! and never calls a `fauna.features.*` read first, because a client that never
//! asks is exactly the client the floor is for.
//!
//! **Why this file exists beside `db::feature_gate`'s unit tests.** Those pin
//! the bucket arithmetic with a delta the test hands them. Here the delta is
//! resolved by the *handler* against the feature's own mutable records, which is
//! the half that can silently go wrong: the anti-cycling property is about what happens when
//! those records change under a counter that must not follow them.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::feature_gate::{
    Availability, FeaturePolicy, GatedFeature, QuotaDimension, RuleTier, Window, WindowedBounds,
    test_support::bound_at,
};
use fauna_core::identity::ActorKeypair;
use fauna_nest::{
    db::CacheDb,
    feature_gate::{
        CODE_FEATURE_DENIED, CODE_FEATURE_OVER_QUOTA, KIND_POLICY_GET, KIND_SELF_LIMITS_GET,
    },
    payment_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
    subscription_handlers,
};
use fauna_protocol::{
    RpcError, Value, decode_strict as decode, encode_canonical,
    features::{
        AuthoredPolicyItem, FeaturePolicyReadReply, FeaturePolicyReadRequest, FeaturesStatusReply,
        FeaturesStatusRequest,
    },
    payments::{ClaimMintReply, ClaimMintRequest, ClaimRedeemReply, ClaimRedeemRequest},
};

mod common;

fn build_router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    subscription_handlers::register_subscription_handlers(&mut b);
    payment_handlers::register_payment_handlers(&mut b);
    fauna_nest::feature_gate::register_features_handlers(&mut b);
    fauna_nest::family_handlers::register_family_handlers(&mut b);
    b.build()
}

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    (build_router(), state)
}

/// An author with a "gold" tier. A redemption enqueues a payment-entitled
/// request for the author's client to approve; the counterparty appears in the
/// `subscribers` records this file's newness delta reads only once that client
/// acts — a second actor's move, which is not what these tests are measuring,
/// so the tests that need a standing counterparty stand in for it
/// ([`author_client_approves`]).
async fn seed_author(state: &Arc<AppState>) -> [u8; 32] {
    let author = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user(&author, "free", "author")
        .await
        .unwrap();
    state
        .db
        .create_subscription_tier(
            &author,
            "gold",
            1,
            None,
            Some("$5/mo"),
            None,
            false,
            None,
            None,
            false,
        )
        .await
        .unwrap();
    author
}

/// The author client's approval of the redeemer's queued, payment-entitled
/// request — the one act that writes the `subscribers` row a standing
/// counterparty is read from. Written directly: the approval's KeyBlob
/// verification is `requests.approve`'s, not the gate's.
async fn author_client_approves(state: &Arc<AppState>, author: [u8; 32], redeemer: [u8; 32]) {
    state
        .db
        .add_subscriber(&author, &redeemer, "gold", None)
        .await
        .unwrap();
    state
        .db
        .delete_pending_subscribe_request(&author, &redeemer, "gold")
        .await
        .unwrap();
}

async fn mint(router: &RpcRouter, state: Arc<AppState>, author: [u8; 32]) -> String {
    let req = encode_canonical(&ClaimMintRequest {
        tier: "gold".into(),
        valid_until: None,
        extra: Default::default(),
    })
    .unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.payments.claims.mint",
        author,
        Bytes::from(req.to_vec()),
    )
    .await
    .expect("mint ok");
    decode::<ClaimMintReply>(&reply).unwrap().code
}

async fn redeem(
    router: &RpcRouter,
    state: Arc<AppState>,
    redeemer: [u8; 32],
    code: &str,
) -> Result<ClaimRedeemReply, RpcError> {
    let req = encode_canonical(&ClaimRedeemRequest {
        code: code.into(),
        extra: Default::default(),
    })
    .unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.payments.claims.redeem",
        redeemer,
        Bytes::from(req.to_vec()),
    )
    .await?;
    Ok(decode(&reply).expect("decode claims.redeem reply"))
}

fn detail(error: &RpcError, key: &str) -> Value {
    let Some(boxed) = &error.details else {
        panic!("refusal carries no details: {}", error.code);
    };
    let Value::Map(map) = boxed.as_ref() else {
        panic!("refusal details are not a map");
    };
    map.get(key)
        .unwrap_or_else(|| panic!("refusal details carry no {key}"))
        .clone()
}

// ── The floor ──────────────────────────────────────────────────

/// The slice's own definition of done: a gated operation is refused with a typed
/// Dim-4 error **naming the binding tier** once the account is over an admin-set
/// quota — and the client is never asked to have asked first.
#[tokio::test]
async fn an_over_quota_operation_is_refused_with_a_typed_error_naming_the_tier() {
    let (router, state) = router_with_db().await;
    let author = seed_author(&state).await;
    let redeemer = ActorKeypair::generate().actor_id().0;

    // ⚠ Mint the fixture codes BEFORE authoring the policy. Every tier except
    // `self` is stored nest-wide (`db::feature_gate`'s `NEST_WIDE_SUBJECT`), so
    // an admin bound binds the *author's* `payments.claim.mint` too — correctly,
    // and it would otherwise refuse this test's own setup. The operation under
    // test is the redemption; the mints are fixture.
    let first = mint(&router, state.clone(), author).await;
    let second = mint(&router, state.clone(), author).await;

    state
        .db
        .put_feature_policy(
            RuleTier::Admin,
            &redeemer,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Operations, Window::Day, 1),
        )
        .await
        .unwrap();

    redeem(&router, state.clone(), redeemer, &first)
        .await
        .expect("the first redemption fits under a bound of 1");

    let error = redeem(&router, state.clone(), redeemer, &second)
        .await
        .expect_err("the second must be refused");

    assert_eq!(error.code, CODE_FEATURE_OVER_QUOTA);
    assert_eq!(detail(&error, "feature"), Value::String("payments".into()));
    assert_eq!(
        detail(&error, "surface"),
        Value::String("payments.claim.redeem".into())
    );
    assert_eq!(
        detail(&error, "tier"),
        Value::String("admin".into()),
        "boundary 4 — the person it binds must see WHICH tier bound them"
    );
    assert_eq!(
        detail(&error, "dimension"),
        Value::String("operations".into())
    );
    assert_eq!(detail(&error, "window"), Value::String("day".into()));
    assert_eq!(detail(&error, "limit"), Value::Integer(1));
}

/// An availability `deny` at any tier refuses the operation outright, and the
/// code is distinct from the quota refusal so a client can tell "not available"
/// from "not right now".
#[tokio::test]
async fn a_tier_deny_refuses_the_operation_outright() {
    let (router, state) = router_with_db().await;
    let author = seed_author(&state).await;
    let redeemer = ActorKeypair::generate().actor_id().0;

    // Fixture first — a region deny is nest-wide and would refuse the mint too
    // (see the note in the test above).
    let code = mint(&router, state.clone(), author).await;

    state
        .db
        .put_feature_policy(
            RuleTier::Region,
            &redeemer,
            GatedFeature::Payments,
            &FeaturePolicy {
                availability: Availability::Deny,
                ..FeaturePolicy::NO_OPINION
            },
        )
        .await
        .unwrap();

    let error = redeem(&router, state.clone(), redeemer, &code)
        .await
        .expect_err("a denied plane refuses");
    assert_eq!(error.code, CODE_FEATURE_DENIED);
    assert_eq!(detail(&error, "tier"), Value::String("region".into()));
}

/// § Fail posture: a deployment no region claims runs at **tier-1 defaults, ON**
/// — never off-by-default. With no policy row anywhere, the operation proceeds.
#[tokio::test]
async fn with_no_policy_anywhere_the_feature_is_on_at_tier_one() {
    let (router, state) = router_with_db().await;
    let author = seed_author(&state).await;
    let redeemer = ActorKeypair::generate().actor_id().0;

    assert!(
        state
            .db
            .feature_policies_for(&redeemer, GatedFeature::Payments)
            .await
            .unwrap()
            .is_empty(),
        "no tier authored anything"
    );

    let code = mint(&router, state.clone(), author).await;
    redeem(&router, state.clone(), redeemer, &code)
        .await
        .expect("gated features are ON at tier-1 defaults where nothing else speaks");
}

/// A refusal must cost nothing: a client hammering a bound it cannot meet would
/// otherwise deepen its own hole.
#[tokio::test]
async fn a_refused_operation_does_not_deepen_the_hole() {
    let (router, state) = router_with_db().await;
    let author = seed_author(&state).await;
    let redeemer = ActorKeypair::generate().actor_id().0;

    // Fixture first — the nest-wide admin bound would refuse the mints (see
    // `an_over_quota_operation_is_refused_with_a_typed_error_naming_the_tier`).
    let first = mint(&router, state.clone(), author).await;
    let mut codes = Vec::new();
    for _ in 0..5 {
        codes.push(mint(&router, state.clone(), author).await);
    }

    state
        .db
        .put_feature_policy(
            RuleTier::Admin,
            &redeemer,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Operations, Window::Day, 1),
        )
        .await
        .unwrap();

    redeem(&router, state.clone(), redeemer, &first).await.ok();
    for code in &codes {
        redeem(&router, state.clone(), redeemer, code)
            .await
            .expect_err("still refused");
    }

    let today =
        fauna_core::day_bucket::local_day_bucket(fauna_core::data::Timestamp::now_secs(), 0);
    let counters = state
        .db
        .feature_usage_counters(&redeemer, GatedFeature::Payments, today)
        .await
        .unwrap();
    assert_eq!(
        counters.operations.day, 1,
        "only the operation that was ADMITTED spent quota"
    );
}

/// A code that does not exist is not an operation — a typo must not spend the
/// caller's quota.
#[tokio::test]
async fn an_unknown_code_spends_nothing() {
    let (router, state) = router_with_db().await;
    let redeemer = ActorKeypair::generate().actor_id().0;

    let error = redeem(&router, state.clone(), redeemer, "not-a-code")
        .await
        .expect_err("unknown code");
    assert_eq!(error.code, "fauna.payments.claim_not_found");

    let today =
        fauna_core::day_bucket::local_day_bucket(fauna_core::data::Timestamp::now_secs(), 0);
    let counters = state
        .db
        .feature_usage_counters(&redeemer, GatedFeature::Payments, today)
        .await
        .unwrap();
    assert_eq!(counters.operations.day, 0);
}

// ── The anti-cycling pin ──────

/// **The Pirate-Bay pin.** A single-counterparty `add → remove → re-add` loop,
/// repeated, trips `OverQuota(Counterparties)` at exactly the bound.
///
/// The loop mutates the very records the newness delta is resolved from — the
/// redeemer's own subscriptions — so if the *count* were ever re-derived from
/// them instead of held in the day bucket, this loop would run forever and the
/// bound would be void (`dynamic-features.md` § The quota grammar's third
/// refinement: *"removal must never refund counterparty quota"*).
#[tokio::test]
async fn a_removal_never_refunds_counterparty_quota_end_to_end() {
    let (router, state) = router_with_db().await;
    let author = seed_author(&state).await;
    let redeemer = ActorKeypair::generate().actor_id().0;

    const BOUND: u64 = 3;
    state
        .db
        .put_feature_policy(
            RuleTier::Admin,
            &redeemer,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Counterparties, Window::Week, BOUND),
        )
        .await
        .unwrap();

    for cycle in 0..BOUND {
        let code = mint(&router, state.clone(), author).await;
        redeem(&router, state.clone(), redeemer, &code)
            .await
            .unwrap_or_else(|e| panic!("cycle {cycle} should fit under a bound of {BOUND}: {e:?}"));
        author_client_approves(&state, author, redeemer).await;

        // The removal an ordinary user can perform at any time — the affordance
        // "user always controls their data" makes mandatory, and precisely why
        // the records cannot be the count's source.
        assert!(
            state
                .db
                .remove_subscriber(&author, &redeemer, "gold")
                .await
                .unwrap(),
            "the subscriber row this cycle created is removed again"
        );
    }

    let code = mint(&router, state.clone(), author).await;
    let error = redeem(&router, state.clone(), redeemer, &code)
        .await
        .expect_err("the cycle after the bound must be refused, not run forever");
    assert_eq!(error.code, CODE_FEATURE_OVER_QUOTA);
    assert_eq!(
        detail(&error, "dimension"),
        Value::String("counterparties".into())
    );
    assert_eq!(detail(&error, "limit"), Value::Integer(BOUND as i128));
    assert_eq!(
        detail(&error, "observed"),
        Value::Integer(BOUND as i128),
        "every cycle counted; none was refunded"
    );
}

/// **The spouses control, green beside the pin above.** Repeat operations
/// against a *standing* counterparty spend **0** counterparty quota after the
/// first, so the charter's own sizing sentence — "the spouses … must never feel
/// a gate" — stays true. Without this control the pin above would also be
/// satisfied by a counter that simply over-counts everything.
#[tokio::test]
async fn repeat_operations_against_a_standing_counterparty_never_feel_a_gate() {
    let (router, state) = router_with_db().await;
    let author = seed_author(&state).await;
    let redeemer = ActorKeypair::generate().actor_id().0;

    state
        .db
        .put_feature_policy(
            RuleTier::Admin,
            &redeemer,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Counterparties, Window::Week, 1),
        )
        .await
        .unwrap();

    // The subscription stands throughout — nothing is removed.
    for round in 0..8 {
        let code = mint(&router, state.clone(), author).await;
        redeem(&router, state.clone(), redeemer, &code)
            .await
            .unwrap_or_else(|e| panic!("round {round} must not feel a gate: {e:?}"));
        author_client_approves(&state, author, redeemer).await;
    }

    let today =
        fauna_core::day_bucket::local_day_bucket(fauna_core::data::Timestamp::now_secs(), 0);
    let counters = state
        .db
        .feature_usage_counters(&redeemer, GatedFeature::Payments, today)
        .await
        .unwrap();
    assert_eq!(
        counters.counterparties.week, 1,
        "one counterparty, counted once, however many operations they take part in"
    );
    assert_eq!(counters.operations.week, 8, "the operations still counted");
}

// ── The subset edge ───

/// A `payments` **deny** must refuse a `zaps` operation too — the runtime twin
/// of the compile-time proof that `zaps` cannot be built without
/// `payments`.
///
/// The nest's resolver is the only caller of `effective_policy`, and it always
/// loads the superset's authored documents, so this holds for a `zaps` gate
/// surface that has not been written yet as much as for one that has. Driven
/// through `resolve_effective_policy` rather than a handler for exactly that
/// reason.
#[tokio::test]
async fn zaps_inherit_a_payments_deny_but_never_a_payments_bound() {
    let (_router, state) = router_with_db().await;
    let actor = ActorKeypair::generate().actor_id().0;

    state
        .db
        .put_feature_policy(
            RuleTier::Admin,
            &actor,
            GatedFeature::Payments,
            &FeaturePolicy {
                availability: Availability::Deny,
                ..FeaturePolicy::NO_OPINION
            },
        )
        .await
        .unwrap();

    let zaps = fauna_nest::feature_gate::resolve_effective_policy(
        &state,
        &actor,
        GatedFeature::Zaps,
        fauna_nest::feature_gate::TierScope::All,
    )
    .await
    .unwrap();
    assert_eq!(
        zaps.availability(),
        Availability::Deny,
        "a payments deny reaches zaps along the subset edge"
    );

    // …and the edge carries availability ONLY: a payments *quota* must not
    // consume zaps' budget (§ Charter members' deliberate asymmetry).
    state
        .db
        .clear_feature_policy(RuleTier::Admin, &actor, GatedFeature::Payments)
        .await
        .unwrap();
    state
        .db
        .put_feature_policy(
            RuleTier::Admin,
            &actor,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Operations, Window::Day, 1),
        )
        .await
        .unwrap();

    let zaps = fauna_nest::feature_gate::resolve_effective_policy(
        &state,
        &actor,
        GatedFeature::Zaps,
        fauna_nest::feature_gate::TierScope::All,
    )
    .await
    .unwrap();
    assert_ne!(zaps.availability(), Availability::Deny);
    let day_bound = zaps
        .bounds(QuotaDimension::Operations)
        .get(Window::Day)
        .expect("zaps keeps its own tier-1 day bound");
    assert_eq!(
        day_bound.tier,
        RuleTier::Structural,
        "the binding tier is zaps' own tier-1 constant, not the payments admin bound"
    );
    assert!(
        day_bound.limit > 1,
        "payments' 1/day did not leak onto zaps: {day_bound:?}"
    );
}

// ── The transparency read (slice 3) ────────────────────────────

async fn status(router: &RpcRouter, state: Arc<AppState>, actor: [u8; 32]) -> FeaturesStatusReply {
    let req = encode_canonical(&FeaturesStatusRequest::default()).unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.features.status",
        actor,
        Bytes::from(req.to_vec()),
    )
    .await
    .expect("status ok");
    decode(&reply).expect("decode features.status reply")
}

fn payments_of(reply: &FeaturesStatusReply) -> &fauna_protocol::features::FeatureStatusItem {
    reply
        .features
        .iter()
        .find(|f| f.feature == GatedFeature::Payments)
        .expect("payments reported")
}

/// Boundary 4 — *"there is no restriction you cannot see"*. The read covers
/// **every** registry member, in canonical order, including the ones the caller
/// is not limited on: "unrestricted" is an answer, and a client that had to
/// infer it from an absence could not tell it from a nest that never heard of
/// the feature.
#[tokio::test]
async fn status_reports_every_registry_member_with_its_tier_one_bounds() {
    let (router, state) = router_with_db().await;
    let actor = ActorKeypair::generate().actor_id().0;

    let reply = status(&router, state.clone(), actor).await;

    let reported: Vec<GatedFeature> = reply.features.iter().map(|f| f.feature).collect();
    let expected: Vec<GatedFeature> = fauna_core::feature_gate::registry()
        .iter()
        .map(|e| e.feature)
        .collect();
    assert_eq!(reported, expected, "every member, in registry order");

    // With nothing authored anywhere, every bound is tier 1's — and it says so.
    for item in &reply.features {
        let entry = fauna_core::feature_gate::entry(item.feature);
        for window in [Window::Day, Window::Week, Window::Month] {
            if let Some(limit) = entry.tier1.operations.get(window) {
                let cell = item
                    .policy
                    .bounds(QuotaDimension::Operations)
                    .get(window)
                    .expect("a tier-1 bound is always present in the meet");
                assert_eq!(cell.limit, limit);
                assert_eq!(cell.tier, RuleTier::Structural);
            }
        }
    }
}

/// The read must name **which tier binds** — the half boundary 4 is actually
/// about. A bare number would leave the person unable to tell a limit they set
/// themselves from one their government set.
#[tokio::test]
async fn status_attributes_each_surviving_bound_to_the_tier_that_set_it() {
    let (router, state) = router_with_db().await;
    let actor = ActorKeypair::generate().actor_id().0;

    state
        .db
        .put_feature_policy(
            RuleTier::Admin,
            &actor,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Operations, Window::Day, 5),
        )
        .await
        .unwrap();
    state
        .db
        .put_feature_policy(
            RuleTier::SelfImposed,
            &actor,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Operations, Window::Month, 3),
        )
        .await
        .unwrap();

    let reply = status(&router, state.clone(), actor).await;
    let payments = payments_of(&reply);

    let day = payments
        .policy
        .bounds(QuotaDimension::Operations)
        .get(Window::Day)
        .unwrap();
    assert_eq!((day.limit, day.tier), (5, RuleTier::Admin));

    let month = payments
        .policy
        .bounds(QuotaDimension::Operations)
        .get(Window::Month)
        .unwrap();
    assert_eq!(
        (month.limit, month.tier),
        (3, RuleTier::SelfImposed),
        "the tighter self-limit wins its own cell and keeps its own attribution"
    );
}

/// § Transparency asks for the **remaining** quota, which is `limit − observed`
/// — so the observed half must move as the account spends, over the same
/// trailing windows the gate evaluated.
#[tokio::test]
async fn status_observed_usage_tracks_what_the_gate_spent() {
    let (router, state) = router_with_db().await;
    let author = seed_author(&state).await;
    let redeemer = ActorKeypair::generate().actor_id().0;

    let before = status(&router, state.clone(), redeemer).await;
    assert_eq!(payments_of(&before).usage.operations.day, 0);
    assert_eq!(payments_of(&before).usage.counterparties.day, 0);

    let code = mint(&router, state.clone(), author).await;
    redeem(&router, state.clone(), redeemer, &code)
        .await
        .expect("redemption fits at tier 1");

    let after = status(&router, state.clone(), redeemer).await;
    let payments = payments_of(&after);
    assert_eq!(payments.usage.operations.day, 1);
    assert_eq!(
        payments.usage.counterparties.day, 1,
        "the author is a newly-introduced counterparty"
    );
    // Remaining, the number a client renders, is derivable from what was sent.
    let limit = payments
        .policy
        .bounds(QuotaDimension::Operations)
        .get(Window::Day)
        .unwrap()
        .limit;
    assert_eq!(limit - payments.usage.operations.day, limit - 1);
}

/// ⚠ **The honesty pin.** A `payments` deny must show `zaps` as denied in the
/// transparency read, not merely refuse it at the gate. This is why the handler
/// reads through `resolve_effective_policy`: a status read that composed its own
/// meet would omit the subset edge and tell the user a plane is available that
/// the nest will refuse — the exact *"silent gate"* boundary 4 forbids, wearing
/// the opposite costume.
#[tokio::test]
async fn status_shows_zaps_denied_when_payments_is_denied() {
    let (router, state) = router_with_db().await;
    let actor = ActorKeypair::generate().actor_id().0;

    state
        .db
        .put_feature_policy(
            RuleTier::Admin,
            &actor,
            GatedFeature::Payments,
            &FeaturePolicy {
                availability: Availability::Deny,
                ..FeaturePolicy::NO_OPINION
            },
        )
        .await
        .unwrap();

    let reply = status(&router, state.clone(), actor).await;
    let zaps = reply
        .features
        .iter()
        .find(|f| f.feature == GatedFeature::Zaps)
        .expect("zaps reported");
    assert_eq!(
        zaps.policy.availability(),
        Availability::Deny,
        "the subset edge must be visible in the read, not only enforced at the gate"
    );
    assert_eq!(
        zaps.policy.denied_by,
        Some(RuleTier::Admin),
        "and it must name the tier that actually denied"
    );

    // `p2p-share` has no superset, so it is untouched — the edge is not a
    // blanket.
    let p2p = reply
        .features
        .iter()
        .find(|f| f.feature == GatedFeature::P2pShare)
        .unwrap();
    assert_ne!(p2p.policy.availability(), Availability::Deny);
}

/// **A restriction that became unreadable is still a restriction you can see**
/// (§ Fail posture — the undecodable-document clause; § Transparency, boundary
/// 4).
///
/// The gate denies on an unreadable guardian document. This pins the *read* half
/// end to end, because a deny nobody can see is the same violation as an
/// unexplained refusal: the ward asks `fauna.features.status`, gets `Deny`, and
/// is told the **guardian** tier is what binds them — which is what points them
/// at the person who can clear it by re-authoring.
#[tokio::test]
async fn status_shows_an_unreadable_guardian_document_as_a_guardian_deny() {
    let (router, state) = router_with_db().await;
    let guardian = ActorKeypair::generate().actor_id().0;
    let ward = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user_with_handle(&guardian, "personal", "parent", None)
        .await
        .unwrap();
    state
        .db
        .create_user_with_handle(&ward, "personal", "kid", Some(&guardian[..]))
        .await
        .unwrap();

    // Well-formed canonical dag-cbor, wrong shape — the decoder-tightening /
    // corruption case, stored past the handler (which validates and re-encodes).
    let undecodable = encode_canonical(&"not a policy document".to_string())
        .unwrap()
        .to_vec();
    state
        .db
        .update_guardian_policy(
            &ward[..],
            false,
            "allow",
            true,
            "allow",
            None,
            None,
            None,
            None,
            Some(&undecodable),
        )
        .await
        .unwrap();

    let reply = status(&router, state.clone(), ward).await;
    for item in &reply.features {
        assert_eq!(
            item.policy.availability(),
            Availability::Deny,
            "{:?} must read as denied while the guardian's document is unreadable",
            item.feature
        );
        assert_eq!(
            item.policy.denied_by,
            Some(RuleTier::Guardian),
            "and the read must name the guardian tier, not leave the ward guessing"
        );
    }

    // The beside-control that keeps this from being satisfied by a nest that
    // denies everyone: an account with no guardian document reads unrestricted.
    let adult = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user_with_handle(&adult, "personal", "grown", None)
        .await
        .unwrap();
    for item in &status(&router, state.clone(), adult).await.features {
        assert_ne!(item.policy.availability(), Availability::Deny);
    }
}

/// The read is about the bearer and nobody else — there is no parameter naming
/// another account, and one account's self-limit must not appear in another's
/// answer.
#[tokio::test]
async fn status_answers_about_the_bearer_only() {
    let (router, state) = router_with_db().await;
    let (a, b) = (
        ActorKeypair::generate().actor_id().0,
        ActorKeypair::generate().actor_id().0,
    );
    state
        .db
        .put_feature_policy(
            RuleTier::SelfImposed,
            &a,
            GatedFeature::Payments,
            &bound_at(QuotaDimension::Operations, Window::Day, 2),
        )
        .await
        .unwrap();

    let for_a = status(&router, state.clone(), a).await;
    let a_day = payments_of(&for_a)
        .policy
        .bounds(QuotaDimension::Operations)
        .get(Window::Day)
        .unwrap();
    assert_eq!((a_day.limit, a_day.tier), (2, RuleTier::SelfImposed));

    let for_b = status(&router, state.clone(), b).await;
    let b_day = payments_of(&for_b)
        .policy
        .bounds(QuotaDimension::Operations)
        .get(Window::Day)
        .unwrap();
    assert_eq!(
        b_day.tier,
        RuleTier::Structural,
        "another account's self-limit is not this account's"
    );
}

// ── W2 slice 3, the write half ─────────────────────────────────────────
//
// § Wire & data shape mints two kinds here — `fauna.features.policy.update`
// (admin tier) and `fauna.features.self_limits.update` (self tier) — and
// deliberately mints none for the guardian tier, which rides
// `fauna.family.policy.update` instead.
//
// These drive the handlers, not the store: `db::feature_gate`'s unit tests
// already pin what a written document does to the meet. What can only go wrong
// *here* is the tier a write lands at and who is allowed to perform it.

/// Author a policy through one of the two update kinds.
async fn update(
    router: &RpcRouter,
    state: Arc<AppState>,
    kind: &str,
    bearer: [u8; 32],
    feature: GatedFeature,
    policy: Option<FeaturePolicy>,
) -> Result<Bytes, RpcError> {
    let req = encode_canonical(&fauna_protocol::features::FeaturePolicyUpdateRequest {
        feature,
        policy,
        extra: Default::default(),
    })
    .unwrap();
    common::call_raw(router, state, kind, bearer, req).await
}

/// An admin, seeded the way `caller_class_for_actor` actually resolves one: a
/// row in `admin_actor_ids`, which is what `is_admin` counts.
///
/// ⚠ `set_admin_role` alone does **not** make an admin — it is an `UPDATE`, so
/// on an actor with no row it silently updates nothing and the caller still
/// resolves as `User`. Add first, then (if a role matters) set it.
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

/// **The admin tier is admin-only, and this is the test that says so.**
///
/// The tier a write lands at is baked into the handler at registration rather
/// than sent on the wire, so the *only* thing standing between an ordinary user
/// and the nest-wide admin document is the class check. A `User` reaching this
/// kind would let any account set a policy binding **every** account on the nest.
#[tokio::test]
async fn an_ordinary_user_cannot_write_the_admin_tier() {
    let (router, state) = router_with_db().await;
    let user = seed_user(&state, "ordinary").await;

    let err = update(
        &router,
        state.clone(),
        "fauna.features.policy.update",
        user,
        GatedFeature::Payments,
        Some(FeaturePolicy::DENIED),
    )
    .await
    .expect_err("an ordinary user must not write the admin tier");
    assert_eq!(err.code, "fauna.features.permission_denied", "got {err:?}");

    // And nothing landed — the refusal is before the write, not after it.
    let authored = state
        .db
        .feature_policies_for(&user, GatedFeature::Payments)
        .await
        .unwrap();
    assert!(
        !authored.iter().any(|(t, _)| *t == RuleTier::Admin),
        "a refused write must leave no admin document behind"
    );
}

/// The admin document is **nest-wide**: it binds an account that never wrote it
/// and never asked. That is the whole difference between this tier and the self
/// tier, and it is what makes the previous test's class check load-bearing.
#[tokio::test]
async fn an_admin_write_binds_an_account_that_never_asked() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let bystander = seed_user(&state, "bystander").await;

    update(
        &router,
        state.clone(),
        "fauna.features.policy.update",
        admin,
        GatedFeature::Payments,
        Some(FeaturePolicy::DENIED),
    )
    .await
    .expect("an admin may write the admin tier");

    let for_bystander = status(&router, state.clone(), bystander).await;
    assert_eq!(
        payments_of(&for_bystander).policy.denied_by,
        Some(RuleTier::Admin),
        "the admin document must bind an account that never wrote it"
    );
}

/// A self-limit lands at the **self** tier and binds only its author — the same
/// property the store-level test pins, but reached through the kind, so a
/// handler wired to the wrong tier constant is caught here.
#[tokio::test]
async fn a_self_limit_lands_at_the_self_tier_and_binds_only_its_author() {
    let (router, state) = router_with_db().await;
    let a = seed_user(&state, "self-limiter").await;
    let b = seed_user(&state, "neighbour").await;

    update(
        &router,
        state.clone(),
        "fauna.features.self_limits.update",
        a,
        GatedFeature::Payments,
        Some(FeaturePolicy::DENIED),
    )
    .await
    .expect("a user may set their own limits");

    assert_eq!(
        payments_of(&status(&router, state.clone(), a).await)
            .policy
            .denied_by,
        Some(RuleTier::SelfImposed)
    );
    assert_eq!(
        payments_of(&status(&router, state.clone(), b).await)
            .policy
            .denied_by,
        None,
        "one account's self-limit is not another's"
    );
}

/// A newer app's write naming a feature this nest does not know, or an
/// availability it cannot read, decodes through the open arms
/// (`transport.md` § Rule 3 in full) and is then refused typed — for that one
/// request, storing nothing.
#[tokio::test]
async fn a_write_this_nest_cannot_evaluate_is_refused_and_stores_nothing() {
    use fauna_protocol::Value;
    let (router, state) = router_with_db().await;
    let user = seed_user(&state, "newer").await;

    let request = |feature: &str, availability: Option<&str>| {
        let mut map = std::collections::BTreeMap::new();
        map.insert("feature".to_string(), Value::String(feature.into()));
        if let Some(a) = availability {
            let mut policy = std::collections::BTreeMap::new();
            policy.insert("availability".to_string(), Value::String(a.into()));
            map.insert("policy".to_string(), Value::Map(policy));
        }
        encode_canonical(&Value::Map(map)).unwrap()
    };

    for (feature, availability) in [
        ("dowsing", Some("deny")),
        ("dowsing", None),
        ("payments", Some("suspend")),
    ] {
        let err = common::call_raw(
            &router,
            state.clone(),
            "fauna.features.self_limits.update",
            user,
            request(feature, availability),
        )
        .await
        .expect_err("a write this nest cannot evaluate is refused");
        assert!(err.code.ends_with("invalid_params"), "got {err:?}");
    }
    assert!(
        state
            .db
            .feature_policies_for(&user, GatedFeature::Payments)
            .await
            .unwrap()
            .iter()
            .all(|(t, _)| *t != RuleTier::SelfImposed),
        "a refused write leaves no document behind"
    );
}

/// **Absent `policy` clears the tier, and clearing is idempotent.**
///
/// "No opinion at this tier" is not the same as an authored allow — the tier
/// drops out of the meet entirely — and a second clear is a no-op rather than a
/// `not_found`, because the caller's intent ("this tier says nothing") holds
/// either way.
#[tokio::test]
async fn an_absent_policy_clears_the_tier_and_repeats_harmlessly() {
    let (router, state) = router_with_db().await;
    let user = seed_user(&state, "clearer").await;

    update(
        &router,
        state.clone(),
        "fauna.features.self_limits.update",
        user,
        GatedFeature::Payments,
        Some(FeaturePolicy::DENIED),
    )
    .await
    .expect("author");
    assert_eq!(
        payments_of(&status(&router, state.clone(), user).await)
            .policy
            .denied_by,
        Some(RuleTier::SelfImposed)
    );

    for attempt in 0..2 {
        update(
            &router,
            state.clone(),
            "fauna.features.self_limits.update",
            user,
            GatedFeature::Payments,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("clear attempt {attempt} must succeed, got {e:?}"));
    }

    let authored = state
        .db
        .feature_policies_for(&user, GatedFeature::Payments)
        .await
        .unwrap();
    assert!(
        !authored.iter().any(|(t, _)| *t == RuleTier::SelfImposed),
        "a cleared tier must leave no document, not an allow-shaped one"
    );
}

/// A bound on a dimension the registry does not declare for the feature is
/// **unrepresentable** (§ The quota grammar), so it is refused at the door
/// rather than stored and silently ignored — a stored-but-ignored bound is a
/// rule-setter believing they set a limit that binds nothing.
///
/// The undeclared dimension is *found* rather than hard-coded, so the test
/// asserts the rule and not a guess about today's registry contents.
#[tokio::test]
async fn a_bound_on_an_undeclared_dimension_is_refused() {
    let (router, state) = router_with_db().await;
    let user = seed_user(&state, "over-reacher").await;

    let entry = fauna_core::feature_gate::entry(GatedFeature::Payments);
    let undeclared = [
        QuotaDimension::Operations,
        QuotaDimension::Counterparties,
        QuotaDimension::Volume,
    ]
    .into_iter()
    .find(|d| !entry.declares(*d));

    let Some(undeclared) = undeclared else {
        // Every dimension is declared for this feature — the rule still holds,
        // there is simply nothing to refuse here. Say so rather than pass mutely.
        eprintln!("payments declares every dimension; nothing to refuse");
        return;
    };

    let mut policy = FeaturePolicy::NO_OPINION;
    policy.availability = Availability::Limit;
    let bounds = WindowedBounds::UNSET.tightened_with(Window::Day, 1);
    match undeclared {
        QuotaDimension::Operations => policy.operations = bounds,
        QuotaDimension::Counterparties => policy.counterparties = bounds,
        QuotaDimension::Volume => policy.volume = bounds,
    }

    let err = update(
        &router,
        state.clone(),
        "fauna.features.self_limits.update",
        user,
        GatedFeature::Payments,
        Some(policy),
    )
    .await
    .expect_err("a bound on an undeclared dimension must be refused");
    assert_eq!(err.code, "fauna.features.invalid_params", "got {err:?}");
}

// ── The authored-document reads (§ Wire & data shape) ─────────────────
//
// `fauna.features.policy.get` (admin tier) and `fauna.features.self_limits.get`
// (self tier) answer a different question from `fauna.features.status`: not
// "what binds me" (the effective meet) but "what did this tier author", plus
// the **ceiling** — the meet of the tiers outside the one read. The authoring
// editors seed from these, because a save is a whole-document replace
// (§ Authoring surfaces): an editor seeded from the effective meet would drop a
// bound that lost the MIN to a tighter tier on the next save.

async fn read_authored(
    router: &RpcRouter,
    state: Arc<AppState>,
    kind: &str,
    bearer: [u8; 32],
) -> Result<FeaturePolicyReadReply, RpcError> {
    let req = encode_canonical(&FeaturePolicyReadRequest::default()).unwrap();
    let reply = common::call_raw(router, state, kind, bearer, Bytes::from(req.to_vec())).await?;
    Ok(decode(&reply).expect("decode authored-document read reply"))
}

fn item_of(reply: &FeaturePolicyReadReply, feature: GatedFeature) -> &AuthoredPolicyItem {
    reply
        .features
        .iter()
        .find(|f| f.feature == feature)
        .unwrap_or_else(|| panic!("{feature:?} reported"))
}

/// Both reads answer for **every** registry member, in canonical order — the
/// same completeness rule as the transparency read: *no opinion* is an answer.
#[tokio::test]
async fn both_authored_reads_report_every_registry_member() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    for kind in [KIND_POLICY_GET, KIND_SELF_LIMITS_GET] {
        let reply = read_authored(&router, state.clone(), kind, admin)
            .await
            .unwrap_or_else(|e| panic!("{kind} for an admin: {e:?}"));
        let got: Vec<_> = reply.features.iter().map(|f| f.feature).collect();
        let want: Vec<_> = fauna_core::feature_gate::registry()
            .iter()
            .map(|e| e.feature)
            .collect();
        assert_eq!(got, want, "{kind} must cover the registry in order");
        for item in &reply.features {
            assert_eq!(item.policy, None, "{kind}: nothing authored yet");
            assert!(!item.unreadable, "{kind}: absent is not unreadable");
        }
    }
}

/// **Pin (a) — the reason the reads exist.** A bound the tier authored that
/// LOST the MIN to a tighter tier is invisible in `fauna.features.status`; the
/// authored read must return it verbatim, and the ceiling must name the tier
/// that actually binds.
#[tokio::test]
async fn an_authored_bound_that_lost_the_meet_is_still_read_back() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "self-limiter").await;

    let mine = bound_at(QuotaDimension::Operations, Window::Day, 5);
    update(
        &router,
        state.clone(),
        "fauna.features.self_limits.update",
        user,
        GatedFeature::Payments,
        Some(mine.clone()),
    )
    .await
    .expect("self-limit");
    update(
        &router,
        state.clone(),
        "fauna.features.policy.update",
        admin,
        GatedFeature::Payments,
        Some(bound_at(QuotaDimension::Operations, Window::Day, 2)),
    )
    .await
    .expect("tighter admin bound");

    // The meet: the self bound lost, so status cannot show it.
    let effective = payments_of(&status(&router, state.clone(), user).await)
        .policy
        .bounds(QuotaDimension::Operations)
        .get(Window::Day)
        .unwrap();
    assert_eq!((effective.limit, effective.tier), (2, RuleTier::Admin));

    let reply = read_authored(&router, state.clone(), KIND_SELF_LIMITS_GET, user)
        .await
        .expect("self read");
    let payments = item_of(&reply, GatedFeature::Payments);
    assert_eq!(
        payments.policy,
        Some(mine),
        "the self tier's own authored document, not the meet"
    );
    assert!(!payments.unreadable);
    let ceiling = payments
        .ceiling
        .bounds(QuotaDimension::Operations)
        .get(Window::Day)
        .unwrap();
    assert_eq!(
        (ceiling.limit, ceiling.tier),
        (2, RuleTier::Admin),
        "the ceiling carries the outer tier's binding bound"
    );
}

/// **Pin (b) — unreadable is not absent** (`nest/common.md` § Unreadable
/// stored values). An authored document this nest cannot decode is *enforced as
/// a deny*; a read that folded it into "no opinion" would hand the editor an
/// empty row saying *No limit set* over a feature that is actually off.
#[tokio::test]
async fn an_undecodable_authored_document_reads_back_unreadable_not_absent() {
    let data_dir = tempfile::tempdir().unwrap();
    let db_path = data_dir.path().join("nest.db");
    let state = Arc::new(AppState::for_test(Arc::new(
        CacheDb::open(&db_path).unwrap(),
    )));
    let router = build_router();
    let admin = seed_admin(&state).await;

    update(
        &router,
        state.clone(),
        "fauna.features.policy.update",
        admin,
        GatedFeature::Payments,
        Some(bound_at(QuotaDimension::Operations, Window::Day, 3)),
    )
    .await
    .expect("author");

    // Well-formed dag-cbor, wrong shape — stored past the handler (which
    // validates and re-encodes) through a second connection.
    let undecodable = encode_canonical(&"not a policy document".to_string())
        .unwrap()
        .to_vec();
    let changed = rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute(
            "UPDATE feature_policies SET document = ?1 WHERE feature = 'payments'",
            rusqlite::params![undecodable],
        )
        .unwrap();
    assert_eq!(changed, 1, "exactly the admin payments row");

    let reply = read_authored(&router, state.clone(), KIND_POLICY_GET, admin)
        .await
        .expect("admin read");
    let payments = item_of(&reply, GatedFeature::Payments);
    assert!(
        payments.unreadable,
        "an undecodable document reads unreadable"
    );
    assert_eq!(payments.policy, None, "and carries no invented document");

    // Beside-control: a member with no row at all is absent, not unreadable.
    let p2p = item_of(&reply, GatedFeature::P2pShare);
    assert!(!p2p.unreadable);
    assert_eq!(p2p.policy, None);

    // Removing is the in-app recovery: the next read is clean.
    update(
        &router,
        state.clone(),
        "fauna.features.policy.update",
        admin,
        GatedFeature::Payments,
        None,
    )
    .await
    .expect("remove clears the corrupt row");
    let reply = read_authored(&router, state.clone(), KIND_POLICY_GET, admin)
        .await
        .expect("admin read after clear");
    assert!(!item_of(&reply, GatedFeature::Payments).unreadable);
}

/// **Pin (c) — the ceiling rides the one resolver, so it carries the subset
/// edge.** A `payments` deny at an outer tier must show as a denied `zaps`
/// ceiling; a read that composed its own meet would report `zaps` as open.
#[tokio::test]
async fn the_ceiling_carries_the_subset_edge() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;
    let user = seed_user(&state, "zapper").await;
    update(
        &router,
        state.clone(),
        "fauna.features.policy.update",
        admin,
        GatedFeature::Payments,
        Some(FeaturePolicy::DENIED),
    )
    .await
    .expect("admin denies payments");

    let reply = read_authored(&router, state.clone(), KIND_SELF_LIMITS_GET, user)
        .await
        .expect("self read");
    let zaps = item_of(&reply, GatedFeature::Zaps);
    assert_eq!(zaps.policy, None, "nobody authored zaps");
    assert_eq!(
        zaps.ceiling.availability(),
        Availability::Deny,
        "the payments deny reaches the zaps ceiling along the subset edge"
    );
    assert_eq!(zaps.ceiling.denied_by, Some(RuleTier::Admin));
}

/// **Pin (d) — the admin read's ceiling is tiers 1–2 only.** It must exclude
/// the admin tier itself AND the calling admin's own guardian/self tiers: the
/// admin document is nest-wide, so the calling account's personal limits say
/// nothing about how far it reaches.
#[tokio::test]
async fn the_admin_ceiling_excludes_the_admin_tier_and_the_callers_own_limits() {
    let (router, state) = router_with_db().await;
    let admin = seed_admin(&state).await;

    // The admin's own self-limit, and the admin tier's own document.
    update(
        &router,
        state.clone(),
        "fauna.features.self_limits.update",
        admin,
        GatedFeature::Payments,
        Some(FeaturePolicy::DENIED),
    )
    .await
    .expect("admin self-limit");
    update(
        &router,
        state.clone(),
        "fauna.features.policy.update",
        admin,
        GatedFeature::P2pShare,
        Some(FeaturePolicy::DENIED),
    )
    .await
    .expect("admin-tier document");

    let reply = read_authored(&router, state.clone(), KIND_POLICY_GET, admin)
        .await
        .expect("admin read");
    let payments = item_of(&reply, GatedFeature::Payments);
    assert_eq!(payments.policy, None, "no ADMIN-tier payments document");
    assert_eq!(
        payments.ceiling.denied_by, None,
        "the caller's self-limit is not part of the admin ceiling"
    );
    let p2p = item_of(&reply, GatedFeature::P2pShare);
    assert_eq!(p2p.policy, Some(FeaturePolicy::DENIED));
    assert_eq!(
        p2p.ceiling.denied_by, None,
        "the tier being read is not its own ceiling"
    );
    for item in &reply.features {
        // Every member, `zaps` included: the superset's documents are cut to
        // the same scope, so the caller's own payments self-deny must not
        // reach the zaps ceiling along the subset edge either.
        assert_eq!(
            item.ceiling.denied_by, None,
            "{:?}: nothing in tiers 1–2 denies",
            item.feature
        );
        for dimension in [
            QuotaDimension::Operations,
            QuotaDimension::Counterparties,
            QuotaDimension::Volume,
        ] {
            for window in [Window::Day, Window::Week, Window::Month] {
                if let Some(cell) = item.ceiling.bounds(dimension).get(window) {
                    assert!(
                        cell.tier <= RuleTier::Region,
                        "{:?} ceiling cell from {:?} — outside tiers 1–2",
                        item.feature,
                        cell.tier
                    );
                }
            }
        }
    }

    // …while the SELF read, for the same account, does include tier 3.
    let reply = read_authored(&router, state.clone(), KIND_SELF_LIMITS_GET, admin)
        .await
        .expect("self read");
    assert_eq!(
        item_of(&reply, GatedFeature::P2pShare).ceiling.denied_by,
        Some(RuleTier::Admin)
    );
    assert_eq!(
        item_of(&reply, GatedFeature::Payments).policy,
        Some(FeaturePolicy::DENIED),
        "the self read returns the caller's own self document"
    );
}

/// **Pin (e) — the class split.** The admin read is Admin-only: a User reaching
/// it would read the nest-wide document the admin editor owns. The self read is
/// User-class and answers for the bearer alone.
#[tokio::test]
async fn an_ordinary_user_cannot_read_the_admin_tier() {
    let (router, state) = router_with_db().await;
    let user = seed_user(&state, "ordinary").await;
    let err = read_authored(&router, state.clone(), KIND_POLICY_GET, user)
        .await
        .expect_err("an ordinary user must not read the admin tier");
    assert_eq!(err.code, "fauna.features.permission_denied", "got {err:?}");

    read_authored(&router, state.clone(), KIND_SELF_LIMITS_GET, user)
        .await
        .expect("a user may read their own self tier");
}

// ── The guardian's editor seed, on the ward's status entry ───────────────────
//
// The guardian tier has no authored-document read of its own: its document
// already rides `fauna.family.status`' ward entry, which carries the two halves
// an editor needs beside it — the unreadable flag and the tiers-1–3 ceiling
// (`family-safety.md` § Wire & data shape, the guardian's feature-limits editor
// seed).

/// A guardian and one ward, linked the way admission links them.
async fn seed_guardian_and_ward(state: &Arc<AppState>) -> ([u8; 32], [u8; 32]) {
    let guardian = ActorKeypair::generate().actor_id().0;
    let ward = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user_with_handle(&guardian, "personal", "parent", None)
        .await
        .unwrap();
    state
        .db
        .create_user_with_handle(&ward, "personal", "kid", Some(&guardian[..]))
        .await
        .unwrap();
    (guardian, ward)
}

/// Store a guardian feature sub-document past the handler, reach knobs at
/// their defaults.
async fn store_guardian_features(state: &Arc<AppState>, ward: [u8; 32], document: &[u8]) {
    state
        .db
        .update_guardian_policy(
            &ward[..],
            false,
            "allow",
            true,
            "allow",
            None,
            None,
            None,
            None,
            Some(document),
        )
        .await
        .unwrap();
}

/// The guardian's one ward entry, read through the real status handler.
async fn ward_entry(
    router: &RpcRouter,
    state: Arc<AppState>,
    guardian: [u8; 32],
) -> fauna_protocol::family::FamilyWardInfo {
    let req = encode_canonical(&fauna_protocol::family::FamilyStatusRequest::default()).unwrap();
    let raw = common::call_raw(router, state, "fauna.family.status", guardian, req)
        .await
        .expect("family status");
    let reply: fauna_protocol::family::FamilyStatusReply = decode(&raw).expect("decode status");
    assert_eq!(reply.wards.len(), 1, "one ward seeded");
    reply.wards.into_iter().next().unwrap()
}

/// A link row whose ward id is not an actor id names no ward. The status read
/// skips it — it never answers an entry with no handle and no ceiling, which
/// every host would have to special-case.
#[tokio::test]
async fn a_malformed_link_row_is_skipped_never_answered_as_a_ceilingless_entry() {
    let (router, state) = router_with_db().await;
    let (guardian, ward) = seed_guardian_and_ward(&state).await;
    state
        .db
        .execute_batch(&format!(
            "INSERT INTO guardianships (supervised_actor_id, guardian_actor_id, created_at)
             VALUES (x'0102', x'{}', 1)",
            hex::encode(guardian)
        ))
        .await
        .unwrap();

    // `ward_entry` asserts the reply carries exactly one ward.
    let entry = ward_entry(&router, state.clone(), guardian).await;
    assert_eq!(entry.actor_id.as_slice(), &ward[..]);
    assert_eq!(
        entry.features_ceiling.len(),
        fauna_core::feature_gate::registry().len()
    );
}

/// Absent and unreadable must not collapse on the read an editor seeds from:
/// an undecodable stored document reports the flag beside the deny it is
/// enforced as, and a decodable one reports the document verbatim with no flag.
#[tokio::test]
async fn a_ward_entry_flags_an_undecodable_feature_document_beside_its_enforced_deny() {
    let (router, state) = router_with_db().await;
    let (guardian, ward) = seed_guardian_and_ward(&state).await;

    // No document at all: no flag, no `features`, and the ceiling is still there.
    let entry = ward_entry(&router, state.clone(), guardian).await;
    assert!(!entry.features_unreadable);
    assert_eq!(entry.policy.features, None);
    let members: Vec<_> = entry.features_ceiling.iter().map(|i| i.feature).collect();
    let registry: Vec<_> = fauna_core::feature_gate::registry()
        .iter()
        .map(|e| e.feature)
        .collect();
    assert_eq!(members, registry, "one ceiling entry per member, in order");

    // A decodable document reads back verbatim.
    let mut authored = fauna_protocol::features::GuardianFeaturePolicies::new();
    authored.insert(
        GatedFeature::P2pShare.as_str().to_string(),
        FeaturePolicy {
            availability: Availability::Limit,
            operations: WindowedBounds::at(Window::Day, 2),
            ..FeaturePolicy::NO_OPINION
        },
    );
    store_guardian_features(&state, ward, &encode_canonical(&authored).unwrap()).await;
    let entry = ward_entry(&router, state.clone(), guardian).await;
    assert!(!entry.features_unreadable);
    assert_eq!(entry.policy.features, Some(authored), "verbatim");

    // An undecodable one: well-formed canonical dag-cbor, wrong shape.
    let undecodable = encode_canonical(&"not a policy document".to_string()).unwrap();
    store_guardian_features(&state, ward, &undecodable).await;
    let entry = ward_entry(&router, state.clone(), guardian).await;
    assert!(
        entry.features_unreadable,
        "an editor must be told not to seed from the enforced deny"
    );
    let enforced = entry.policy.features.expect("the enforced view");
    for member in fauna_core::feature_gate::registry() {
        assert_eq!(
            enforced.get(member.feature.as_str()),
            Some(&FeaturePolicy::DENIED),
            "{:?} reads as the deny it is enforced as",
            member.feature
        );
    }
}

/// The ceiling on a ward entry is tiers 1–3 **for the ward**: it carries an
/// admin document, and neither the guardian's own document nor the ward's
/// self-limit.
#[tokio::test]
async fn a_ward_entrys_ceiling_carries_the_admin_tier_and_not_the_guardians_own() {
    let (router, state) = router_with_db().await;
    let (guardian, ward) = seed_guardian_and_ward(&state).await;
    let admin = seed_admin(&state).await;

    update(
        &router,
        state.clone(),
        "fauna.features.policy.update",
        admin,
        GatedFeature::P2pShare,
        Some(FeaturePolicy::DENIED),
    )
    .await
    .expect("admin-tier document");
    update(
        &router,
        state.clone(),
        "fauna.features.self_limits.update",
        ward,
        GatedFeature::Zaps,
        Some(FeaturePolicy::DENIED),
    )
    .await
    .expect("the ward's own self-limit");
    let mut authored = fauna_protocol::features::GuardianFeaturePolicies::new();
    authored.insert(
        GatedFeature::Payments.as_str().to_string(),
        FeaturePolicy::DENIED,
    );
    store_guardian_features(&state, ward, &encode_canonical(&authored).unwrap()).await;

    let entry = ward_entry(&router, state.clone(), guardian).await;
    let ceiling = |feature: GatedFeature| {
        &entry
            .features_ceiling
            .iter()
            .find(|i| i.feature == feature)
            .unwrap_or_else(|| panic!("{feature:?} missing from the ceiling"))
            .ceiling
    };
    assert_eq!(
        ceiling(GatedFeature::P2pShare).denied_by,
        Some(RuleTier::Admin),
        "the admin tier is outside the guardian's"
    );
    assert_eq!(
        ceiling(GatedFeature::Payments).denied_by,
        None,
        "the tier being edited is not its own ceiling"
    );
    assert_eq!(
        ceiling(GatedFeature::Zaps).denied_by,
        None,
        "neither the ward's self-limit nor the guardian's payments deny along \
         the subset edge is outside the guardian tier"
    );
}
