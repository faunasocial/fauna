//! Lifecycle tests for the claim-code stage of the onboarding wizard.
//!
//! Per `docs/goal/behavior/onboarding.md` §3a, the wizard reaches `claim_code`
//! when handle-check returns `UnregisteredUnclaimedNest` and the user clicks
//! Continue. Submit calls `NestApi::claim_admin` (the `fauna.auth.claim_admin`
//! pre-identity WS-RPC kind); on success the wizard advances to
//! `NatModeChoice` — the single, terminal admin-path setup step (the claim is
//! not itself the `LoggedIn` point; the NAT submit/defer is).
//!
//! These drive the machine through `FakeNestApi` — the transport-agnostic seam —
//! so they assert the page logic without standing up a server. The WS-RPC wire
//! mapping (kind → wire type, `RpcError.code` → `ClaimAdminError`) is proven
//! against a real nest in `bins/fauna-nest/tests/onboarding_ws_rpc_roundtrip.rs`.
//!
//! Coverage:
//! - `submit_handle_check_continue` routes UnregisteredUnclaimedNest → ClaimCode.
//! - claim success → NatModeChoice + snapshot Claimed.
//! - claim threads the bare handle local part + the domain as `mail_domain`.
//! - `ClaimAdminError::Invalid` → Invalid { reason }, stays on ClaimCode.
//! - `ClaimAdminError::Transient` → Error { transient: true }, stays on ClaimCode.
//! - `back()` from ClaimCode returns to HandleEntry.

use std::sync::Arc;

use fauna_onboarding_machine::nest_api::{ClaimAdminError, ClaimAdminResponse, FakeNestApi};
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::state::LocalizedText;
use fauna_onboarding_machine::{
    ClaimCodeSnapshot, ClaimCodeState, HandleCheckOutcome, HandleCheckPhase, HandleCheckSnapshot,
    NestApi, OnboardingMachine, OnboardingObserver, OnboardingStep,
};

/// Build a machine over a `FakeNestApi` — the transport-agnostic seam. The
/// claim-code page logic exercised here (2xx → NatModeChoice, 4xx →
/// Invalid, 5xx → Transient, handle threading) is independent of how the call
/// reaches nest; the WS-RPC wire mapping itself is proven against a real nest in
/// `bins/fauna-nest/tests/onboarding_ws_rpc_roundtrip.rs`.
fn machine_with_fake() -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    let m = OnboardingMachine::with_nest_api(observer, fake.clone() as Arc<dyn NestApi>);
    (m, fake)
}

fn fixture_secret_hex() -> String {
    "01".repeat(32)
}

#[tokio::test]
async fn submit_handle_check_continue_routes_unclaimed_nest_to_claim_code() {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::new(observer);
    m.seed_identity(fixture_secret_hex());
    m.set_step_for_test(OnboardingStep::HandleEntry);
    m.set_current_handle("alice@example.com".into());
    m.set_handle_check_snapshot_for_test(HandleCheckSnapshot {
        phase: HandleCheckPhase::Complete,
        outcome: HandleCheckOutcome::UnregisteredUnclaimedNest,
        message: LocalizedText::default(),
        continue_enabled: true,
        control_checkbox_visible: false,
        control_checkbox_checked: false,
    });

    let next = m.submit_handle_check_continue().await;
    assert_eq!(next, OnboardingStep::ClaimCode);
    assert_eq!(m.step(), OnboardingStep::ClaimCode);
    // The continuation must derive nest_url from the handle's domain so
    // wizard_submit_claim_code knows where to POST.
    assert_eq!(m.nest_url(), "https://example.com");
}

#[tokio::test]
async fn back_from_claim_code_returns_to_handle_entry() {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::new(observer);
    m.seed_identity(fixture_secret_hex());
    m.set_step_for_test(OnboardingStep::ClaimCode);
    m.back();
    assert_eq!(m.step(), OnboardingStep::HandleEntry);
}

#[tokio::test]
async fn wizard_submit_claim_code_2xx_yields_nat_mode_choice() {
    let (m, fake) = machine_with_fake();
    fake.set_claim_admin_response(Ok(ClaimAdminResponse {
        ok: true,
        token: Some("tok".into()),
        expires_at: Some(1234),
        domain: Some("example.com".into()),
        deployment_seed: None,
    }));
    m.seed_identity(fixture_secret_hex());
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.set_step_for_test(OnboardingStep::ClaimCode);

    let next = m.wizard_submit_claim_code("ABC123".into()).await;
    // The claim does not terminate the wizard. Once the admin is claimed
    // server-side the wizard advances to NatModeChoice; LoggedIn is set only by
    // that page's submit/defer.
    assert_eq!(next, OnboardingStep::NatModeChoice);
    assert_eq!(m.step(), OnboardingStep::NatModeChoice);

    let snap = m.claim_code_snapshot();
    assert_eq!(snap.state, ClaimCodeState::Claimed);

    // wizard_outcome must NOT be set yet — encryption-mode submit is the
    // commit point.
    assert!(m.wizard_outcome().is_none());
}

/// box-recovery.md § The plane-era recovery floor, (c): the claim reply still
/// carries the nest's deployment seed on the wire, but the machine no longer
/// consumes it — the launched client's custody leg fetches it from the box. The
/// claim succeeds and nothing is handed off.
#[tokio::test]
async fn wizard_submit_claim_code_does_not_consume_the_deployment_seed() {
    let (m, fake) = machine_with_fake();
    let seed_hex = "ab".repeat(32); // 64-char hex Ed25519 seed
    fake.set_claim_admin_response(Ok(ClaimAdminResponse {
        ok: true,
        token: Some("tok".into()),
        expires_at: Some(1234),
        domain: Some("example.com".into()),
        deployment_seed: Some(seed_hex.clone().into()),
    }));
    m.seed_identity(fixture_secret_hex());
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.set_step_for_test(OnboardingStep::ClaimCode);

    let next = m.wizard_submit_claim_code("ABC123".into()).await;
    assert_eq!(next, OnboardingStep::NatModeChoice);
}

/// A keyless nest (no signing key) omits the seed (`None`); the channel stays
/// empty and the claim still succeeds.
#[tokio::test]
async fn wizard_submit_claim_code_no_seed_leaves_channel_empty() {
    let (m, fake) = machine_with_fake();
    fake.set_claim_admin_response(Ok(ClaimAdminResponse {
        ok: true,
        token: Some("tok".into()),
        expires_at: Some(1234),
        domain: Some("example.com".into()),
        deployment_seed: None,
    }));
    m.seed_identity(fixture_secret_hex());
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.set_step_for_test(OnboardingStep::ClaimCode);

    let next = m.wizard_submit_claim_code("ABC123".into()).await;
    assert_eq!(next, OnboardingStep::NatModeChoice);
}

#[tokio::test]
async fn wizard_submit_claim_code_threads_handle_local_part() {
    // The claim must carry the wizard's chosen handle split into its bare local
    // part (the nest's handle validation rejects '@') plus the domain as
    // `mail_domain` — so claiming registers `alice` as the admin's handle AND
    // auto-registers `example.com` as its mail domain (canonical-alias-on-enable
    // + the send from-handle check depend on it; see the
    // onboarding-auto-adds-handle-domain behavior). We assert on the captured
    // args rather than a wire body so the test is transport-agnostic.
    let (m, fake) = machine_with_fake();
    m.seed_identity(fixture_secret_hex());
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.set_step_for_test(OnboardingStep::ClaimCode);

    let next = m.wizard_submit_claim_code("ABC123".into()).await;
    assert_eq!(next, OnboardingStep::NatModeChoice);
    assert_eq!(
        fake.claim_admin_args(),
        vec![("alice".to_string(), Some("example.com".to_string()))],
        "claim must thread the bare local part as `handle` and the domain as `mail_domain`",
    );
}

#[tokio::test]
async fn wizard_submit_claim_code_rejects_empty_handle_without_round_trip() {
    // A handle is REQUIRED to claim — a handle-less admin is unrepresentable
    // (mail/AUTH login resolves nobody). The machine guards an empty handle
    // client-side, surfacing the same `Invalid` state the nest would return
    // WITHOUT a pointless round-trip. This is the shared backstop for all six
    // apps (e.g. a factory-reset re-onboard that failed to thread the admin's
    // authoritative handle).
    let (m, fake) = machine_with_fake();
    m.seed_identity(fixture_secret_hex());
    m.set_current_handle("".into()); // empty → no handle to claim with
    m.set_nest_url("https://example.com".into());
    m.set_step_for_test(OnboardingStep::ClaimCode);

    let next = m.wizard_submit_claim_code("ABC123".into()).await;

    // Stays on ClaimCode with an Invalid (retry-able) snapshot — NOT advanced to
    // NatModeChoice.
    assert_eq!(next, OnboardingStep::ClaimCode);
    assert_eq!(m.step(), OnboardingStep::ClaimCode);
    assert!(matches!(
        m.claim_code_snapshot().state,
        ClaimCodeState::Invalid { .. }
    ));
    // No claim was attempted — the guard fires before any nest round-trip.
    assert!(
        !fake.calls().contains(&"claim_admin".to_string()),
        "empty handle must not round-trip a claim the nest would reject"
    );
    assert!(m.wizard_outcome().is_none());
}

#[tokio::test]
async fn wizard_submit_claim_code_4xx_keeps_on_claim_code_with_invalid_state() {
    // A server rejection (the WS path maps every claim 4xx-class `RpcError` to
    // `ClaimAdminError::Invalid`, carrying the nest's reason text).
    let (m, fake) = machine_with_fake();
    fake.set_claim_admin_response(Err(ClaimAdminError::Invalid {
        reason: "invalid claim code".into(),
    }));
    m.seed_identity(fixture_secret_hex());
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.set_step_for_test(OnboardingStep::ClaimCode);

    let next = m.wizard_submit_claim_code("WRONG".into()).await;
    // Stays on ClaimCode.
    assert_eq!(next, OnboardingStep::ClaimCode);
    assert_eq!(m.step(), OnboardingStep::ClaimCode);

    let snap = m.claim_code_snapshot();
    match snap.state {
        ClaimCodeState::Invalid { reason } => {
            assert!(reason.contains("invalid claim code"), "got {reason:?}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    // Submit re-enabled so user can retry.
    assert!(snap.submit_enabled);
    // No outcome — wizard hasn't terminated.
    assert!(m.wizard_outcome().is_none());
}

#[tokio::test]
async fn wizard_submit_claim_code_already_claimed_shows_dedicated_message() {
    // The nest already has an admin (e.g. the within-grace pending-factory-
    // reset honor arm pre-fills this page against a box whose reset never
    // actually landed — common.md § Client-state recoverability). The code
    // itself was fine, so this must NOT render through the generic
    // `onboarding.claim_code.invalid` "{reason}" substitution (which would
    // leak a raw wire code); it gets its own dedicated, argument-free message.
    let (m, fake) = machine_with_fake();
    fake.set_claim_admin_response(Err(ClaimAdminError::AlreadyClaimed));
    m.seed_identity(fixture_secret_hex());
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.set_step_for_test(OnboardingStep::ClaimCode);

    let next = m.wizard_submit_claim_code("SOMECODE".into()).await;
    // Stays on ClaimCode — not stuck, not silently advanced.
    assert_eq!(next, OnboardingStep::ClaimCode);
    assert_eq!(m.step(), OnboardingStep::ClaimCode);

    let snap = m.claim_code_snapshot();
    assert!(
        matches!(snap.state, ClaimCodeState::Invalid { .. }),
        "still structurally Invalid (retry-able), got {:?}",
        snap.state
    );
    assert_eq!(
        snap.message,
        LocalizedText {
            key: "onboarding.claim_code.error.already_claimed".into(),
            args: Default::default(),
        },
        "must use the dedicated already-claimed message, not the generic \
         invalid-code {{reason}} substitution"
    );
    // Submit re-enabled — the page must not get stuck; `claim-code-back-button`
    // is the real exit (ui.yaml claim_code § transitions).
    assert!(snap.submit_enabled);
    assert!(m.wizard_outcome().is_none());
}

#[tokio::test]
async fn wizard_submit_claim_code_5xx_keeps_on_claim_code_with_transient_error() {
    // A transport/internal fault (`fauna.protocol.internal` and any transport
    // error both map to `ClaimAdminError::Transient` on the WS path).
    let (m, fake) = machine_with_fake();
    fake.set_claim_admin_response(Err(ClaimAdminError::Transient {
        cause: "status 503".into(),
    }));
    m.seed_identity(fixture_secret_hex());
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.set_step_for_test(OnboardingStep::ClaimCode);

    let next = m.wizard_submit_claim_code("ABC123".into()).await;
    assert_eq!(next, OnboardingStep::ClaimCode);
    let snap = m.claim_code_snapshot();
    match snap.state {
        ClaimCodeState::Error { transient, .. } => {
            assert!(transient, "5xx must be transient");
        }
        other => panic!("expected Error, got {other:?}"),
    }
    assert!(snap.submit_enabled);
}

#[tokio::test]
async fn wizard_submit_claim_code_with_no_identity_yields_terminal_error() {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::new(observer);
    // No seed_identity — the wizard has no secret to sign with.
    m.set_step_for_test(OnboardingStep::ClaimCode);
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());

    let next = m.wizard_submit_claim_code("ABC123".into()).await;
    assert_eq!(next, OnboardingStep::ClaimCode);
    let snap = m.claim_code_snapshot();
    assert!(matches!(
        snap.state,
        ClaimCodeState::Error {
            transient: false,
            ..
        }
    ));
}

#[tokio::test]
async fn claim_code_snapshot_default_is_idle_submittable() {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::new(observer);
    let snap = m.claim_code_snapshot();
    assert_eq!(snap.state, ClaimCodeState::Idle);
    assert!(snap.submit_enabled);
}

/// Re-entering the claim-code page through the ORDINARY route must hand the
/// user a submittable page, even when this process already claimed a nest.
///
/// A native app holds ONE long-lived machine for the life of the process
/// (`Wizard::new` on tui, `machine_glue` on linux, the view-model's `machine` on
/// apple), so a terminal `Claimed` snapshot — whose `submit_enabled` is
/// deliberately `false` — outlives the wizard run that produced it. Both
/// `navigate_to_claim_code_for_known_nest*` helpers already re-seed the snapshot
/// on entry for exactly this reason; the handle-check route into `ClaimCode`
/// (`UnregisteredUnclaimedNest`) is the third door into the same page and must
/// behave identically, or the second claim of a process renders a permanently
/// disabled Submit button with no way forward but `claim-code-back-button`.
///
/// This is the `provisioning`-snapshot bug one page over (see
/// `OnboardingMachine::reset`'s doc comment): a prior run's TERMINAL snapshot
/// persisting into a fresh run. Caught by
/// `tests/e2e-unified/tests/test_mail_enable_at_admin_claim.py`, whose second
/// test claimed a second nest in one app process and got `disabled` from the
/// click.
#[tokio::test]
async fn re_entering_claim_code_after_a_successful_claim_reseeds_the_snapshot() {
    let (m, fake) = machine_with_fake();
    fake.set_claim_admin_response(Ok(ClaimAdminResponse {
        ok: true,
        token: Some("tok".into()),
        expires_at: Some(1234),
        domain: Some("example.com".into()),
        deployment_seed: None,
    }));
    m.seed_identity(fixture_secret_hex());
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.set_step_for_test(OnboardingStep::ClaimCode);

    // First claim succeeds and leaves the page in its terminal state.
    assert_eq!(
        m.wizard_submit_claim_code("ABC123".into()).await,
        OnboardingStep::NatModeChoice
    );
    let after_claim = m.claim_code_snapshot();
    assert_eq!(after_claim.state, ClaimCodeState::Claimed);
    assert!(
        !after_claim.submit_enabled,
        "precondition: a completed claim disables Submit"
    );

    // Same process, second nest: the user runs the wizard again and handle-check
    // reports another unclaimed nest.
    m.set_step_for_test(OnboardingStep::HandleEntry);
    m.set_current_handle("bob@second.example".into());
    m.set_handle_check_snapshot_for_test(HandleCheckSnapshot {
        phase: HandleCheckPhase::Complete,
        outcome: HandleCheckOutcome::UnregisteredUnclaimedNest,
        message: LocalizedText::default(),
        continue_enabled: true,
        control_checkbox_visible: false,
        control_checkbox_checked: false,
    });
    assert_eq!(
        m.submit_handle_check_continue().await,
        OnboardingStep::ClaimCode
    );

    let snap = m.claim_code_snapshot();
    assert_eq!(
        snap.state,
        ClaimCodeState::Idle,
        "a fresh claim-code page must start Idle, not carry the last claim's state"
    );
    assert!(
        snap.submit_enabled,
        "Submit must be live on a freshly-entered claim-code page — a stale \
         `Claimed` snapshot strands the admin on a dead button"
    );
}

/// `reset()` is what a "start over" and the e2e per-test reset call, so it must
/// leave no terminal page state behind. It already clears `provisioning` for
/// this reason; `claim_code` is the same shape (terminal states with
/// `submit_enabled == false`) and its route in does not re-seed unconditionally.
#[tokio::test]
async fn reset_clears_a_terminal_claim_code_snapshot() {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::new(observer);
    m.set_claim_code_snapshot_for_test(ClaimCodeSnapshot {
        state: ClaimCodeState::Claimed,
        message: LocalizedText {
            key: "onboarding.claim_code.claimed".into(),
            args: Default::default(),
        },
        submit_enabled: false,
    });

    m.reset();

    let snap = m.claim_code_snapshot();
    assert_eq!(snap.state, ClaimCodeState::Idle);
    assert!(snap.submit_enabled);
}

#[tokio::test]
async fn set_claim_code_snapshot_for_test_overwrites_snapshot() {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::new(observer);
    m.set_claim_code_snapshot_for_test(ClaimCodeSnapshot {
        state: ClaimCodeState::Invalid {
            reason: "bad".into(),
        },
        message: LocalizedText {
            key: "onboarding.claim_code.invalid".into(),
            args: Default::default(),
        },
        submit_enabled: true,
    });
    let snap = m.claim_code_snapshot();
    assert!(matches!(snap.state, ClaimCodeState::Invalid { .. }));
}

#[tokio::test]
async fn call_machine_method_dispatches_set_claim_code_snapshot() {
    // Pin the JSON dispatch path used by the cross-app E2E bridge:
    // tests/e2e-unified/drivers/machine_test_setter.py:set_claim_code_snapshot
    // sends `set_claim_code_snapshot_for_test` as the method name and a
    // JSON-serialized ClaimCodeSnapshot as the arg.
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::new(observer);
    let payload = r#"{"state":{"Invalid":{"reason":"x"}},"message":{"key":"k","args":{}},"submit_enabled":true}"#;
    m.call_machine_method("set_claim_code_snapshot_for_test".into(), payload.into());
    let snap = m.claim_code_snapshot();
    assert!(matches!(snap.state, ClaimCodeState::Invalid { .. }));
}

#[tokio::test]
async fn call_machine_method_dispatches_set_step_to_claim_code() {
    // The Python helper calls set_step_for_test("ClaimCode") before
    // injecting the snapshot. Pin that path.
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::new(observer);
    m.call_machine_method("set_step_for_test".into(), "\"ClaimCode\"".into());
    assert_eq!(m.step(), OnboardingStep::ClaimCode);
}
