//! Derivation tests for the four claim-time serving-enablement intents
//! (`email_enable_requested()` + siblings).
//!
//! Per docs/goal/behavior/onboarding.md § 3b (ratified 2026-07-13): the intents
//! are machine-derived from **two** axes — handle locality AND the NAT axis:
//! ON iff the handle targets a real registerable domain AND the box's effective
//! NAT mode is not `Private`. The NAT conjunct is what lets the two-box
//! home-relay deployment (deployment-home-with-public-relay.md) express "this
//! box does not run mail" with no checkbox: both boxes share the same
//! real-domain handle, but the home box is exactly the box on the private axis,
//! and a claim-time enable there would mint a fresh MSEK diverging from the
//! fleet MSEK that `LinkBoth`/`ProvisionRelayMailbox` later re-seals onto it.
//!
//! "Effective NAT mode" = the committed choice when `submit_nat_mode_choice`
//! succeeded, else the nest's seed (`fauna.setup.status`.`node_mode`) — a defer
//! sends nothing and leaves the seed authoritative.

use std::sync::Arc;

use fauna_onboarding_machine::nest_api::FakeNestApi;
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    NestApi, NodeMode, OnboardingMachine, OnboardingObserver, OnboardingStep,
};

fn machine_with_fake() -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    let m = OnboardingMachine::with_nest_api(observer, fake.clone() as Arc<dyn NestApi>);
    (m, fake)
}

/// A machine parked on `ClaimCode` with identity + handle + nest_url arranged,
/// mirroring `nat_mode_lifecycle.rs` — the claim is the only entry to
/// `NatModeChoice`, and entry is what resets the NAT snapshot.
fn machine_at_claim_code(handle: &str) -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let (m, fake) = machine_with_fake();
    m.set_current_handle(handle.into());
    m.set_nest_url("https://example.com".into());
    m.seed_identity("01".repeat(32));
    m.set_step_for_test(OnboardingStep::ClaimCode);
    (m, fake)
}

fn all_four(m: &OnboardingMachine) -> [bool; 4] {
    [
        m.email_enable_requested(),
        m.caldav_enable_requested(),
        m.carddav_enable_requested(),
        m.webdav_enable_requested(),
    ]
}

// ── handle-locality axis (unchanged by the NAT conjunct) ────────────────────

#[tokio::test]
async fn loopback_handle_derives_all_four_off_even_on_public_axis() {
    let (m, fake) = machine_at_claim_code("alice@127.0.0.1:8443");
    m.set_node_mode_seed_for_test(Some(NodeMode::Public));
    fake.set_submit_nat_mode_response(Ok(()));

    m.wizard_submit_claim_code("CLAIMCODE".into()).await;
    m.select_nat_mode(NodeMode::Public);
    m.submit_nat_mode_choice().await;

    assert_eq!(all_four(&m), [false; 4]);
}

// ── NAT axis: committed choice ──────────────────────────────────────────────

#[tokio::test]
async fn committed_public_with_real_domain_derives_all_four_on() {
    let (m, fake) = machine_at_claim_code("alice@example.com");
    m.set_node_mode_seed_for_test(Some(NodeMode::Public));
    fake.set_submit_nat_mode_response(Ok(()));

    m.wizard_submit_claim_code("CLAIMCODE".into()).await;
    m.submit_nat_mode_choice().await;

    assert_eq!(all_four(&m), [true; 4]);
}

#[tokio::test]
async fn committed_private_with_real_domain_derives_all_four_off() {
    // The home-relay home box: real-domain handle (same as the relay's), but
    // the admin marks it Private — no claim-time enable, no fresh MSEK mint;
    // the link action provisions its mailbox from the fleet MSEK.
    let (m, fake) = machine_at_claim_code("alice@example.com");
    m.set_node_mode_seed_for_test(Some(NodeMode::Public));
    fake.set_submit_nat_mode_response(Ok(()));

    m.wizard_submit_claim_code("CLAIMCODE".into()).await;
    m.select_nat_mode(NodeMode::Private);
    m.submit_nat_mode_choice().await;

    assert_eq!(all_four(&m), [false; 4]);
}

// ── NAT axis: defer leaves the seed authoritative ───────────────────────────

#[tokio::test]
async fn deferred_private_seed_derives_all_four_off() {
    // The home-relay installer seeds FAUNA_MODE=private; a defer sends nothing
    // and the box stays private — the derivation must follow the seed.
    let (m, fake) = machine_at_claim_code("alice@example.com");
    m.set_node_mode_seed_for_test(Some(NodeMode::Private));

    m.wizard_submit_claim_code("CLAIMCODE".into()).await;
    m.defer_nat_mode_choice();

    assert_eq!(all_four(&m), [false; 4]);
    assert!(!fake.calls().iter().any(|c| c == "submit_nat_mode"));
}

#[tokio::test]
async fn deferred_public_seed_derives_all_four_on() {
    // Defer on a public-seeded box: the box genuinely stays public (the
    // private-ward pre-*selection* is only a suggestion the confirm would
    // commit; a defer commits nothing), so the enables follow the seed.
    let (m, _fake) = machine_at_claim_code("alice@example.com");
    m.set_node_mode_seed_for_test(Some(NodeMode::Public));

    m.wizard_submit_claim_code("CLAIMCODE".into()).await;
    m.defer_nat_mode_choice();

    assert_eq!(all_four(&m), [true; 4]);
}

#[tokio::test]
async fn deferred_missing_seed_defaults_public_and_derives_on() {
    // No seed resolved (probe raced): the nest's absent-row
    // default is Public, so the derivation falls back the same way.
    let (m, _fake) = machine_at_claim_code("alice@example.com");
    m.set_node_mode_seed_for_test(None);

    m.wizard_submit_claim_code("CLAIMCODE".into()).await;
    m.defer_nat_mode_choice();

    assert_eq!(all_four(&m), [true; 4]);
}

// ── a committed choice beats the seed ───────────────────────────────────────

#[tokio::test]
async fn committed_public_overrides_private_seed() {
    // The admin can flip a private-seeded box to Public on the wizard page;
    // the commit, not the seed, is then the box's mode.
    let (m, fake) = machine_at_claim_code("alice@example.com");
    m.set_node_mode_seed_for_test(Some(NodeMode::Private));
    fake.set_submit_nat_mode_response(Ok(()));

    m.wizard_submit_claim_code("CLAIMCODE".into()).await;
    m.select_nat_mode(NodeMode::Public);
    m.submit_nat_mode_choice().await;

    assert_eq!(all_four(&m), [true; 4]);
}

// ── a failed submit commits nothing ─────────────────────────────────────────

#[tokio::test]
async fn failed_submit_leaves_seed_authoritative() {
    use fauna_onboarding_machine::nest_api::NatModeError;
    let (m, fake) = machine_at_claim_code("alice@example.com");
    m.set_node_mode_seed_for_test(Some(NodeMode::Public));
    fake.set_submit_nat_mode_response(Err(NatModeError::Transient {
        cause: "boom".into(),
    }));

    m.wizard_submit_claim_code("CLAIMCODE".into()).await;
    m.select_nat_mode(NodeMode::Private);
    m.submit_nat_mode_choice().await;

    // The Private selection never landed; the box is still public-seeded.
    assert_eq!(all_four(&m), [true; 4]);
}

// ── the claim axis: a plain SIGN-IN is not a claim ──────────────────────────
//
// § 3b is "Serving enablement **at claim**", and § 3a (onboarding.md line 136)
// says the launched app applies "the claim-time serving enablement" once the
// claim succeeds. The two published axes (handle locality, NAT) are the
// *defaulting* rule **within** that scope — they were never meant to be
// evaluated on a wizard run that claimed nothing.
//
// `submit_handle_check_continue`'s `AlreadyOnNest` arm routes an
// already-registered identity straight to `Done` / `WizardOutcome::LoggedIn` —
// the same outcome the claim path ends on, and the same one every app's launch
// glue reads these four getters at. So a returning admin merely SIGNING IN on a
// real-domain, public-axis handle derived all four ON, and the glue fired four
// Admin-class deployment writes (`provision_mail_at_first_setup`,
// `set_caldav_enabled(true)`, `set_carddav_enabled(true)`,
// `set_webdav_enabled(true)`) against a box the admin had already configured.
//
// Found 2026-08-16 against the live example.com box:
// the writes were observed being issued on a sign-in, and failed only because
// that run's WS-RPC channel was down for an unrelated reason.

fn machine_signed_in_as_returning_admin(handle: &str) -> Arc<OnboardingMachine> {
    use fauna_onboarding_machine::state::LocalizedText;
    use fauna_onboarding_machine::{HandleCheckOutcome, HandleCheckPhase, HandleCheckSnapshot};

    let (m, _fake) = machine_with_fake();
    m.seed_identity("01".repeat(32));
    m.set_current_handle(handle.into());
    m.set_handle_check_snapshot_for_test(HandleCheckSnapshot {
        phase: HandleCheckPhase::Complete,
        outcome: HandleCheckOutcome::AlreadyOnNest {
            handle_matches: true,
            current_handle: Some(handle.into()),
        },
        message: LocalizedText {
            key: "x".into(),
            args: Default::default(),
        },
        continue_enabled: true,
        control_checkbox_visible: false,
        control_checkbox_checked: false,
    });
    m
}

/// The **manual-DNS** claim (`recheck_manual_dns` → `complete_manual_dns_claim`)
/// is the second of the two claim paths, and the reason the claim axis is an
/// explicit state fact rather than a read of the claim-code snapshot: this path
/// never moves that snapshot to `Claimed`. Without this pin, the claim conjunct
/// would silently derive all four OFF for every deferred-DNS claim — turning a
/// production-safety fix into a "works out-of-the-box" regression on the exact
/// path a VPS-provisioned box takes.
#[tokio::test]
async fn a_manual_dns_claim_still_derives_all_four_on() {
    use fauna_onboarding_machine::DnsRecordPlain;
    use fauna_onboarding_machine::nest_api::{ClaimAdminResponse, SetupStatus};

    let (m, fake) = machine_with_fake();
    m.seed_identity("01".repeat(32));
    m.seed_awaiting_manual_dns(
        "https://example.com".into(),
        "alice@example.com".into(),
        vec![DnsRecordPlain {
            record_type: "A".into(),
            name: "@".into(),
            value: "1.2.3.4".into(),
            ttl: 300,
            priority: None,
        }],
        "CLAIM-XYZ".into(),
    );
    m.set_node_mode_seed_for_test(Some(NodeMode::Public));
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: false,
        ..Default::default()
    }));
    fake.set_claim_admin_response(Ok(ClaimAdminResponse {
        ok: true,
        token: Some("tok".into()),
        expires_at: Some(0),
        domain: Some("example.com".into()),
        deployment_seed: None,
    }));

    let step = m.recheck_manual_dns().await;
    assert_eq!(step, OnboardingStep::NatModeChoice, "the claim succeeded");

    assert_eq!(
        all_four(&m),
        [true; 4],
        "a deferred-DNS claim is still a claim — it must keep its § 3b defaults"
    );
}

#[tokio::test]
async fn plain_sign_in_on_a_real_domain_public_box_derives_all_four_off() {
    let m = machine_signed_in_as_returning_admin("alice@example.com");
    m.set_node_mode_seed_for_test(Some(NodeMode::Public));

    // The sign-in route: no claim is ever submitted.
    let step = m.submit_handle_check_continue().await;
    assert_eq!(step, OnboardingStep::Done, "sign-in lands on Done/LoggedIn");

    assert_eq!(
        all_four(&m),
        [false; 4],
        "a sign-in claimed nothing, so it must request no claim-time enablement"
    );
}
