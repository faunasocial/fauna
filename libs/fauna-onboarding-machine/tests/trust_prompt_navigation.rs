//! The one-tap "trust this box" interstitial — navigation + the answer latch.
//!
//! Per `docs/goal/behavior/onboarding.md` § 3b-ter (IDs user-approved
//! 2026-08-13) and `tests/e2e-unified/ui.yaml` page `onboarding.trust_prompt`.
//! The screen **asks only**: no nest-side setup happens here (§ 3b-bis stays
//! the terminal admin-path *setup* step), and the mint itself runs at the
//! wizard's signed-in handoff — the one point the client holds an
//! authenticated session and can reach `fauna.capabilities.mint`. That is the
//! same deferral the recovery kit uses for registration + escrow, and these
//! tests pin the two halves it needs: the routing, and the consume-once latch
//! the per-app handoff glue reads.

use std::sync::Arc;

use fauna_onboarding_machine::nest_api::{
    FakeNestApi, InviteCodeVerification, InviteRequestError, SetupStatus,
};
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::state::LocalizedText;
use fauna_onboarding_machine::{
    HandleCheckOutcome, HandleCheckPhase, HandleCheckSnapshot, NestApi, OnboardingMachine,
    OnboardingObserver, OnboardingStep, WizardOutcome,
};
use fauna_protocol::auth::{SilentChallengeOutcome, VerifyReply};

fn machine_with_fake() -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    let m = OnboardingMachine::with_nest_api(observer, fake.clone() as Arc<dyn NestApi>);
    (m, fake)
}

/// A machine parked on the NAT-mode page with identity + handle + nest_url
/// arranged — i.e. mid claim-path, one step before the trust prompt's ratified
/// position.
fn machine_at_nat_mode() -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let (m, fake) = machine_with_fake();
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.seed_identity("01".repeat(32));
    m.set_step_for_test(OnboardingStep::NatModeChoice);
    (m, fake)
}

// ── The capability flag: six apps unchanged, tui routes through the prompt ──

/// An app that has not built the screen keeps the pre-existing flow: the NAT
/// step exits straight to `Done` with `LoggedIn`. The batched-trickle-down
/// parity gap, pinned — the same shape `set_renders_recovery_kit` uses.
#[tokio::test]
async fn without_the_capability_the_nat_step_still_exits_straight_to_done() {
    let (m, _fake) = machine_at_nat_mode();

    let next = m.submit_nat_mode_choice().await;

    assert_eq!(next, OnboardingStep::Done);
    assert!(matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::LoggedIn { .. })
    ));
    assert!(
        !m.take_trust_prompt_granted(),
        "an app that never showed the prompt must never latch a grant the user \
         was not asked for"
    );
}

/// With the capability declared, the NAT step routes through the interstitial
/// instead of finishing the wizard. The outcome stays unset: the app must not
/// route to the main UI while a wizard screen is still on the glass.
#[tokio::test]
async fn with_the_capability_the_nat_step_routes_through_the_trust_prompt() {
    let (m, _fake) = machine_at_nat_mode();
    m.set_renders_trust_prompt(true);

    let next = m.submit_nat_mode_choice().await;

    assert_eq!(next, OnboardingStep::TrustPrompt);
    assert_eq!(m.step(), OnboardingStep::TrustPrompt);
    assert!(
        m.wizard_outcome().is_none(),
        "the wizard is not done while the prompt is showing — a set outcome \
         here would race the app into the main UI"
    );
}

/// Deferring the NAT choice reaches the prompt too. Both exits from § 3b-bis
/// are claim completions, and the prompt is offered on the claim, not on which
/// button ended the NAT page.
#[tokio::test]
async fn deferring_the_nat_choice_reaches_the_prompt_as_well() {
    let (m, _fake) = machine_at_nat_mode();
    m.set_renders_trust_prompt(true);

    assert_eq!(m.defer_nat_mode_choice(), OnboardingStep::TrustPrompt);
}

// ── The two buttons ────────────────────────────────────────────────────────

/// `trust-box-grant-button`: the wizard finishes with `LoggedIn` **and** the
/// latch is armed, so the signed-in handoff knows to mint.
#[tokio::test]
async fn granting_finishes_the_wizard_and_arms_the_latch() {
    let (m, _fake) = machine_at_nat_mode();
    m.set_renders_trust_prompt(true);
    m.submit_nat_mode_choice().await;

    let next = m.grant_default_trust();

    assert_eq!(next, OnboardingStep::Done);
    assert!(
        matches!(m.wizard_outcome(), Some(WizardOutcome::LoggedIn { .. })),
        "the trust prompt is an interstitial, not a setup step — it must not \
         change what the wizard concludes"
    );
    assert!(m.take_trust_prompt_granted());
}

/// `trust-box-skip-button`: declining changes nothing at all. Same outcome,
/// same step, and no latch — "declining leaves everything as today"
/// (§ 3b-ter).
#[tokio::test]
async fn skipping_finishes_the_wizard_and_leaves_nothing_armed() {
    let (m, _fake) = machine_at_nat_mode();
    m.set_renders_trust_prompt(true);
    m.submit_nat_mode_choice().await;

    let next = m.skip_trust_prompt();

    assert_eq!(next, OnboardingStep::Done);
    assert!(matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::LoggedIn { .. })
    ));
    assert!(!m.take_trust_prompt_granted());
}

/// A factory reset clears the user's PROGRESS, never what the app is capable
/// of rendering — the same contract `renders_recovery_kit` carries, and the
/// same bug shape if it were dropped: the app declares this once when it builds
/// the machine, so a reset that cleared it would route onboarding around a
/// screen the app *does* render for the rest of that process's life.
#[tokio::test]
async fn a_factory_reset_preserves_the_apps_declared_capability() {
    let (m, _fake) = machine_at_nat_mode();
    m.set_renders_trust_prompt(true);
    m.grant_default_trust();

    m.reset();

    // Progress is gone…
    assert_eq!(m.step(), OnboardingStep::IdentityChoice);
    assert!(m.wizard_outcome().is_none());
    assert!(
        !m.take_trust_prompt_granted(),
        "the latched ANSWER is progress and must not survive a reset"
    );
    // …the capability is not: drive the claim path again and the prompt is
    // still in the flow.
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.seed_identity("01".repeat(32));
    m.set_step_for_test(OnboardingStep::NatModeChoice);
    assert_eq!(m.defer_nat_mode_choice(), OnboardingStep::TrustPrompt);
}

/// The latch is **consume-once**, like `take_pending_recovery_secret`. A second
/// read answers `false`, so a handoff that runs twice (a retried session, a
/// re-entered wizard) mints once — the belt to the shared mint's own
/// idempotence brace.
#[tokio::test]
async fn the_latch_is_consumed_by_the_first_read() {
    let (m, _fake) = machine_at_nat_mode();
    m.set_renders_trust_prompt(true);
    m.submit_nat_mode_choice().await;
    m.grant_default_trust();

    assert!(m.take_trust_prompt_granted());
    assert!(
        !m.take_trust_prompt_granted(),
        "the second read must not re-arm a second mint"
    );
}

// ── The joiner routes: § 3b-ter's SECOND offer position ────────────────────
//
// "Shown after a successful admin claim, **and offered at a joining user's
// first login**" (`onboarding.md` § 3b-ter). Until these landed only the first
// half existed: the machine parked on `TrustPrompt` from one place,
// `leave_nat_mode_choice`, and each joiner terminal set `LoggedIn` inline —
// four hand-written exits, three of them offer-free.
//
// The ask-gate answer these pin, decided from the owner docs rather than
// guessed: **a join is a redemption or an approved request**; an
// `AlreadyOnNest` sign-in is not. § 3b-ter promises the offer to a *joining*
// user, `docs/features/join-a-nest.md` outcome 8 says "your first sign-in to a
// nest you **joined**", and the machine has drawn that exact line since the
// serving-enablement fix — `claim_completed`'s doc comment distinguishes
// "`AlreadyOnNest` **sign-in**" from "invite redemption", and a returning
// sign-in deriving claim-time state was a live production bug.

fn verification(invite_id: &str) -> InviteCodeVerification {
    InviteCodeVerification {
        invite_id: invite_id.into(),
        supervised_by: None,
    }
}

fn challenge_says_registered_as(handle: &str, domain: &str) -> SilentChallengeOutcome {
    SilentChallengeOutcome::Success(VerifyReply {
        token: "bearer".into(),
        token_id: "0".repeat(16),
        handle: handle.into(),
        domain: domain.into(),
        tier: "free".into(),
        expires_at: 0,
        ..Default::default()
    })
}

fn challenge_says_registered() -> SilentChallengeOutcome {
    challenge_says_registered_as("bob", "example.com")
}

/// A machine parked on `invite_request` with a VALIDATED out-of-band code —
/// one `redeem_invite()` away from being registered on someone else's nest.
async fn machine_at_valid_oob_code() -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let (m, fake) = machine_with_fake();
    m.seed_identity("02".repeat(32));
    m.navigate_to_invite_request_for_known_nest(
        "https://example.com".into(),
        "bob@example.com".into(),
    );
    fake.set_verify_invite_code_response(Ok(verification("INV-1")));
    m.verify_oob_invite_code("CODE-1".into()).await;
    (m, fake)
}

/// A machine polling a submitted join request the admin has just approved: the
/// request row is gone (`NotFound`, the approval signal) and the registered
/// probe confirms the account now exists.
fn machine_at_approved_request() -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let (m, fake) = machine_with_fake();
    m.seed_identity("03".repeat(32));
    let status = serde_json::json!({
        "PendingReview": { "request_id": "REQ1", "last_checked_ms": 0 }
    });
    m.seed_pending_invite(
        "https://example.com".into(),
        "bob@example.com".into(),
        "REQ1".into(),
        status.to_string(),
    );
    fake.set_recheck_invite_request_response(Err(InviteRequestError::NotFound));
    fake.set_silent_challenge_response(challenge_says_registered());
    (m, fake)
}

/// A returning user whose handle check found them already registered on the
/// box — the third route to `LoggedIn`, and the one § 3b-ter does NOT promise.
fn machine_at_already_on_nest() -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let (m, fake) = machine_with_fake();
    m.seed_identity("04".repeat(32));
    m.set_current_handle("bob@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.set_handle_check_snapshot_for_test(HandleCheckSnapshot {
        phase: HandleCheckPhase::Complete,
        outcome: HandleCheckOutcome::AlreadyOnNest {
            handle_matches: true,
            current_handle: Some("bob@example.com".into()),
        },
        message: LocalizedText {
            key: "x".into(),
            args: Default::default(),
        },
        continue_enabled: true,
        control_checkbox_visible: false,
        control_checkbox_checked: false,
    });
    (m, fake)
}

/// An app relaunched onto the "Almost ready" surface after the deferred-DNS
/// provisioning path, whose next poll finds the box ALREADY claimed and the
/// silent challenge confirming that claim is ours — the manual-DNS resume,
/// the recovery edge of `onboarding-provisioning.md` § 6
/// *Provisioning = build + claim*: this admin claimed the box, then the app
///   died before the wizard exited. The fifth route to `LoggedIn`, and a
///   claim-path one.
fn machine_at_manual_dns_resume() -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let (m, fake) = machine_with_fake();
    m.seed_identity("05".repeat(32));
    m.seed_awaiting_manual_dns(
        "https://example.test".into(),
        "alice@example.test".into(),
        Vec::new(),
        "CLAIM-XYZ".into(),
    );
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        ..Default::default()
    }));
    fake.set_silent_challenge_response(challenge_says_registered_as("alice", "example.test"));
    (m, fake)
}

/// Redeeming an out-of-band invite code is a *join*, so it reaches the offer
/// instead of dropping the user straight into the app.
///
/// The outcome staying unset matters for the same reason it does on the claim
/// path: an app that routes off `wizard_outcome()` rather than `step` would
/// swap to the main UI with the interstitial still on the glass.
#[tokio::test]
async fn redeeming_an_invite_reaches_the_offer_instead_of_the_app() {
    let (m, _fake) = machine_at_valid_oob_code().await;
    m.set_renders_trust_prompt(true);

    let next = m.redeem_invite().await;

    assert_eq!(next, OnboardingStep::TrustPrompt);
    assert_eq!(m.step(), OnboardingStep::TrustPrompt);
    assert!(
        m.wizard_outcome().is_none(),
        "the wizard is not done while the offer is showing"
    );
}

/// An approved join request is the other join, and it arrives from a POLL
/// tick rather than a button. Same offer.
///
/// The poll cannot re-enter and race the interstitial: `recheck_invite_status`
/// proceeds only from `PendingReview`, which this transition has left.
#[tokio::test]
async fn an_approved_join_request_reaches_the_offer_too() {
    let (m, _fake) = machine_at_approved_request();
    m.set_renders_trust_prompt(true);

    let next = m.recheck_invite_status().await;

    assert_eq!(next, OnboardingStep::TrustPrompt);
    assert!(m.wizard_outcome().is_none());
}

/// An `AlreadyOnNest` sign-in is NOT a join — the user is already on this box,
/// and this is just another device or another launch. § 3b-ter's "first login"
/// has already happened, so the offer must not reappear.
///
/// Pinned as its own test because the cost of getting it wrong is asymmetric:
/// re-offering on every sign-in turns a one-time courtesy into a nag that
/// trains the user to dismiss a capability-grant prompt.
#[tokio::test]
async fn a_returning_sign_in_is_never_offered_the_box() {
    let (m, _fake) = machine_at_already_on_nest();
    m.set_renders_trust_prompt(true);

    let next = m.submit_handle_check_continue().await;

    assert_eq!(
        next,
        OnboardingStep::Done,
        "a returning sign-in must exit the wizard directly"
    );
    assert!(matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::LoggedIn { .. })
    ));
    assert!(
        !m.take_trust_prompt_granted(),
        "nothing may be latched on a route that never asked"
    );
}

/// The manual-DNS resume reaches the offer too (§ 3b-ter, ratified
/// 2026-09-21). The rule that decides every route is whether the wizard is
/// concluding, for the first time, a run that put the user on this box — and
/// the resume is exactly that: the admin's own claim, whose wizard never
/// concluded, so the admin was never asked. The NAT page it skips is a *setup*
/// step whose nest-held seed already holds without it; the offer is not a setup
/// step and has no seeded answer, so skipping the setup tail is no reason to
/// skip it. Both halves of the recovery edge survive: no second claim, and no
/// NAT page on the way.
#[tokio::test]
async fn the_manual_dns_resume_reaches_the_offer_too() {
    let (m, fake) = machine_at_manual_dns_resume();
    m.set_renders_trust_prompt(true);

    let next = m.recheck_manual_dns().await;

    assert_eq!(
        next,
        OnboardingStep::TrustPrompt,
        "the resume must park on the offer, not exit around it"
    );
    assert!(
        m.wizard_outcome().is_none(),
        "the awaiting outcome is cleared at the park, like the fresh-claim arm"
    );
    assert_eq!(
        fake.calls(),
        vec![
            "probe_setup_status".to_string(),
            "silent_challenge".to_string()
        ],
        "ownership is settled by the challenge — never a second claim"
    );
    // The app's poll timer keeps firing until it sees the step change; with
    // the outcome cleared a late tick is a no-op that cannot re-park the offer.
    assert_eq!(m.recheck_manual_dns().await, OnboardingStep::TrustPrompt);
    assert_eq!(fake.calls().len(), 2, "a late poll tick must not re-probe");

    // Answering concludes with the slot's own identity, like every other route.
    assert_eq!(m.skip_trust_prompt(), OnboardingStep::Done);
    match m.wizard_outcome() {
        Some(WizardOutcome::LoggedIn { nest_url, handle }) => {
            assert_eq!(nest_url, "https://example.test");
            assert_eq!(handle, "alice@example.test");
        }
        other => panic!("expected LoggedIn, got {other:?}"),
    }
    assert!(!m.take_trust_prompt_granted());
}

/// …and, like every route, exits straight to `Done` on an app that never
/// declared the screen (the parity gap `awaiting_manual_dns.rs` pins in full,
/// call sequence included).
#[tokio::test]
async fn without_the_capability_the_manual_dns_resume_still_exits_straight_to_done() {
    let (m, _fake) = machine_at_manual_dns_resume();

    let next = m.recheck_manual_dns().await;

    assert_eq!(next, OnboardingStep::Done);
    assert!(matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::LoggedIn { .. })
    ));
    assert!(!m.take_trust_prompt_granted());
}

/// The parity gap survives on the joiner routes exactly as it does on the
/// claim path: an app that never declared the screen keeps exiting straight to
/// `Done`. Without this the six apps that had not yet built the page would
/// have parked on a step they cannot render.
#[tokio::test]
async fn without_the_capability_a_redeem_still_exits_straight_to_done() {
    let (m, _fake) = machine_at_valid_oob_code().await;

    let next = m.redeem_invite().await;

    assert_eq!(next, OnboardingStep::Done);
    assert!(matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::LoggedIn { .. })
    ));
    assert!(!m.take_trust_prompt_granted());
}

/// Both answers conclude the joiner's wizard, and only the grant arms the
/// latch — the same contract the claim path's two buttons carry, now reached
/// from a route that never touches the NAT page.
#[tokio::test]
async fn the_joiners_two_answers_conclude_the_wizard_like_the_claimers() {
    let (grant, _f1) = machine_at_valid_oob_code().await;
    grant.set_renders_trust_prompt(true);
    grant.redeem_invite().await;
    assert_eq!(grant.grant_default_trust(), OnboardingStep::Done);
    assert!(matches!(
        grant.wizard_outcome(),
        Some(WizardOutcome::LoggedIn { .. })
    ));
    assert!(grant.take_trust_prompt_granted());

    let (skip, _f2) = machine_at_valid_oob_code().await;
    skip.set_renders_trust_prompt(true);
    skip.redeem_invite().await;
    assert_eq!(skip.skip_trust_prompt(), OnboardingStep::Done);
    assert!(matches!(
        skip.wizard_outcome(),
        Some(WizardOutcome::LoggedIn { .. })
    ));
    assert!(!skip.take_trust_prompt_granted());
}

/// The identity the wizard concludes with is CAPTURED WHEN THE ROUTE ARRIVES,
/// not re-derived when the offer is answered.
///
/// This is the whole reason the interstitial carries the pair across the park
/// rather than letting the finisher recompute it. The four exits do not agree
/// on how the pair is built — a redemption concludes with
/// `effective_nest_url()` (which a provider override redirects), the approved-
/// request poll deliberately uses the persisted `state.nest_url` instead
/// ("leaking [the override] here would put a test-cloud URL into production
/// identity-store records"), and `AlreadyOnNest` derives the URL from the
/// domain the user typed. A finisher that re-derived would quietly hand some
/// of those routes a different nest than the one they actually joined, and the
/// value it got wrong is the one the identity store persists forever.
///
/// Driven here by mutating the state the fallback derivation reads while the
/// offer is parked: capture-at-arrival ignores it, re-derivation would not.
#[tokio::test]
async fn the_offer_concludes_with_the_nest_the_route_actually_joined() {
    let (m, _fake) = machine_at_valid_oob_code().await;
    m.set_renders_trust_prompt(true);
    m.redeem_invite().await;

    // Whatever the machine's state says AFTER the join is irrelevant: the user
    // joined `example.com`, and that is what the wizard must conclude with.
    m.set_nest_url("https://someone-elses-box.example".into());
    m.set_current_handle("mallory@someone-elses-box.example".into());

    m.grant_default_trust();

    match m.wizard_outcome() {
        Some(WizardOutcome::LoggedIn { nest_url, handle }) => {
            assert_eq!(nest_url, "https://example.com");
            assert_eq!(handle, "bob@example.com");
        }
        other => panic!("expected LoggedIn, got {other:?}"),
    }
}
