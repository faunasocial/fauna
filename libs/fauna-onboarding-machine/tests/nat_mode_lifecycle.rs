//! Lifecycle tests for the nat_mode_choice stage.
//!
//! Per docs/goal/behavior/onboarding.md § 3b-bis: the wizard reaches
//! `NatModeChoice` **directly on claim completion** — it is the single,
//! terminal admin-path setup step. `selected_mode` defaults
//! to the nest's resolved seed, with the one-directional private-ward
//! refinement (a `public` seed + a private-network handle target pre-selects
//! Private with the `private_hint` message; a `private` seed is never
//! overridden the other way; a *public* bare IP must NOT trigger it).

use std::sync::Arc;

use fauna_onboarding_machine::nest_api::{FakeNestApi, NatModeError};
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    NatModeSnapshot, NatModeState, NestApi, NodeMode, OnboardingMachine, OnboardingObserver,
    OnboardingStep, WizardOutcome,
};

fn machine_with_fake() -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    let m = OnboardingMachine::with_nest_api(observer, fake.clone() as Arc<dyn NestApi>);
    (m, fake)
}

/// A machine parked on `ClaimCode` with identity + handle + nest_url arranged.
/// The claim is now the **only** entry to `NatModeChoice` — the storage-mode
/// step that used to sit between them is deleted (Phase-4, no-modes).
fn machine_at_claim_code(handle: &str) -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let (m, fake) = machine_with_fake();
    m.set_current_handle(handle.into());
    m.set_nest_url("https://example.com".into());
    m.seed_identity("01".repeat(32));
    m.set_step_for_test(OnboardingStep::ClaimCode);
    (m, fake)
}

/// Drive the claim. Entry to `nat_mode_choice` is what computes the
/// pre-selection (`reset_nat_mode_snapshot`), so the defaulting tests below
/// drive the real transition rather than parking on the page.
async fn claim(m: &Arc<OnboardingMachine>) -> OnboardingStep {
    m.wizard_submit_claim_code("CLAIMCODE".into()).await
}

// ── (a) the re-anchor: a successful claim lands NatModeChoice ──────────────

#[tokio::test]
async fn claim_success_lands_nat_mode_choice_not_done() {
    let (m, _fake) = machine_at_claim_code("alice@example.com");

    let next = claim(&m).await;
    assert_eq!(next, OnboardingStep::NatModeChoice);
    assert_eq!(m.step(), OnboardingStep::NatModeChoice);
    // The outcome is NOT set yet — LoggedIn moves to the NAT submit/defer.
    assert!(m.wizard_outcome().is_none());

    // Entry resets the NAT snapshot to idle (public handle domain, no seed).
    let snap = m.nat_mode_snapshot();
    assert_eq!(snap.state, NatModeState::Choosing);
    assert_eq!(snap.selected_mode, NodeMode::Public);
    assert_eq!(snap.message.key, "onboarding.nat_mode.choosing");
    assert!(snap.submit_enabled);
}

// ── (b–e) seed + private-ward defaulting ────────────────────────────────────

#[tokio::test]
async fn private_seed_preselects_private() {
    let (m, _fake) = machine_at_claim_code("alice@example.com");
    m.set_node_mode_seed_for_test(Some(NodeMode::Private));

    claim(&m).await;
    let snap = m.nat_mode_snapshot();
    assert_eq!(snap.selected_mode, NodeMode::Private);
    // A private seed is the installer's deliberate choice, not the ward
    // inference — no private_hint.
    assert_eq!(snap.message.key, "onboarding.nat_mode.choosing");
}

#[tokio::test]
async fn public_seed_with_mdns_local_handle_triggers_private_ward() {
    let (m, _fake) = machine_at_claim_code("alice@pi.local");
    m.set_node_mode_seed_for_test(Some(NodeMode::Public));

    claim(&m).await;
    let snap = m.nat_mode_snapshot();
    assert_eq!(snap.selected_mode, NodeMode::Private);
    assert_eq!(snap.message.key, "onboarding.nat_mode.private_hint");
    assert!(snap.submit_enabled);
}

#[tokio::test]
async fn public_seed_with_private_ip_handle_triggers_private_ward() {
    // A LAN IP with an explicit port — the port must be stripped before
    // classification (a raw `192.168.1.50:8443` parses as no IP at all).
    let (m, _fake) = machine_at_claim_code("alice@192.168.1.50:8443");
    m.set_node_mode_seed_for_test(Some(NodeMode::Public));

    claim(&m).await;
    let snap = m.nat_mode_snapshot();
    assert_eq!(snap.selected_mode, NodeMode::Private);
    assert_eq!(snap.message.key, "onboarding.nat_mode.private_hint");
}

#[tokio::test]
async fn public_seed_with_public_domain_stays_public() {
    let (m, _fake) = machine_at_claim_code("alice@example.com");
    m.set_node_mode_seed_for_test(Some(NodeMode::Public));

    claim(&m).await;
    let snap = m.nat_mode_snapshot();
    assert_eq!(snap.selected_mode, NodeMode::Public);
    assert_eq!(snap.message.key, "onboarding.nat_mode.choosing");
}

#[tokio::test]
async fn public_bare_ip_does_not_trigger_private_ward() {
    // A *public* bare IP is not a private-network target — the refinement is
    // reachability-flavored (`is_private_network_target`), not the
    // `is_public_dns_name` locality classifier.
    let (m, _fake) = machine_at_claim_code("alice@8.8.8.8");
    m.set_node_mode_seed_for_test(Some(NodeMode::Public));

    claim(&m).await;
    let snap = m.nat_mode_snapshot();
    assert_eq!(snap.selected_mode, NodeMode::Public);
    assert_eq!(snap.message.key, "onboarding.nat_mode.choosing");
}

// ── seed capture plumbing ───────────────────────────────────────────────────

#[tokio::test]
async fn node_mode_seed_test_helpers_round_trip() {
    // The production capture (probe_setup_status → State.node_mode_seed) rides
    // the NestApi trait plumbing; here we verify the helpers + the SetupStatus
    // field.
    use fauna_onboarding_machine::nest_api::SetupStatus;
    let (m, _fake) = machine_with_fake();
    m.set_node_mode_seed_for_test(Some(NodeMode::Private));
    assert_eq!(m.node_mode_seed_for_test(), Some(NodeMode::Private));
    m.set_node_mode_seed_for_test(None);
    assert_eq!(m.node_mode_seed_for_test(), None);

    let setup = SetupStatus {
        claimed: true,
        node_mode: Some(NodeMode::Private),
    };
    assert_eq!(setup.node_mode, Some(NodeMode::Private));
}

// ── (f) select ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn select_nat_mode_flips_selected_mode() {
    let (m, _fake) = machine_with_fake();
    m.select_nat_mode(NodeMode::Private);
    let snap = m.nat_mode_snapshot();
    assert_eq!(snap.selected_mode, NodeMode::Private);
    assert_eq!(snap.state, NatModeState::Choosing);
    assert!(snap.submit_enabled);

    m.select_nat_mode(NodeMode::Public);
    assert_eq!(m.nat_mode_snapshot().selected_mode, NodeMode::Public);
}

#[tokio::test]
async fn select_nat_mode_recovers_from_error() {
    let (m, _fake) = machine_with_fake();
    let mut snap = NatModeSnapshot::idle();
    snap.state = NatModeState::Error {
        transient: true,
        cause: "boom".into(),
    };
    m.set_nat_mode_snapshot_for_test(snap);

    m.select_nat_mode(NodeMode::Private);
    let snap = m.nat_mode_snapshot();
    assert_eq!(snap.state, NatModeState::Choosing);
    assert_eq!(snap.selected_mode, NodeMode::Private);
}

// ── (g) submit success ──────────────────────────────────────────────────────

#[tokio::test]
async fn submit_nat_mode_success_yields_logged_in_done() {
    let (m, fake) = machine_with_fake();
    fake.set_submit_nat_mode_response(Ok(()));
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.seed_identity("01".repeat(32));
    m.set_step_for_test(OnboardingStep::NatModeChoice);
    m.select_nat_mode(NodeMode::Private);

    let next = m.submit_nat_mode_choice().await;
    assert_eq!(next, OnboardingStep::Done);
    assert_eq!(m.nat_mode_snapshot().state, NatModeState::Done);
    assert_eq!(
        m.nat_mode_snapshot().message.key,
        "onboarding.nat_mode.done"
    );

    match m.wizard_outcome().expect("outcome must be set") {
        WizardOutcome::LoggedIn { nest_url, handle } => {
            assert_eq!(nest_url, "https://example.com");
            assert_eq!(handle, "alice@example.com");
        }
        other => panic!("expected LoggedIn, got {other:?}"),
    }

    // The fake recorded exactly one signed commit whose body carries the
    // selected mode and a signature that actually verifies over the canonical
    // bytes `mode_wire_str || "\n" || actor_id_hex || "\n" || timestamp_decimal`.
    assert_eq!(
        fake.calls()
            .iter()
            .filter(|c| c.as_str() == "submit_nat_mode")
            .count(),
        1
    );
    let bodies = fake.submit_nat_mode_bodies();
    assert_eq!(bodies.len(), 1);
    let body = &bodies[0];
    assert_eq!(body.mode, NodeMode::Private);
    assert_eq!(body.actor_id.len(), 64);
    assert_eq!(body.signature.len(), 128);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!(
        (now_ms - body.timestamp).abs() < 5 * 60 * 1000,
        "timestamp must be current wall-clock millis, got {}",
        body.timestamp
    );
    // The actor id must be the identity the wizard holds.
    let sk = ed25519_dalek::SigningKey::from_bytes(&[0x01; 32]);
    assert_eq!(body.actor_id, hex::encode(sk.verifying_key().to_bytes()));
    // The body names the nest the fake stands in for.
    assert_eq!(
        body.nest_id,
        fauna_onboarding_machine::nest_api::FAKE_NEST_ID
    );
    // Verify the Ed25519 signature over the canonical bytes — built through the
    // shared helper, which domain-separates under `sig_domain::SETUP_NAT_MODE_V2`.
    // Re-spelling the layout here is what let this assertion drift into
    // verifying a message the client never signs (red on `origin/main`).
    let signed = fauna_protocol::nat_mode::nat_mode_signed_message(
        "private",
        &body.actor_id,
        body.timestamp,
        &body.nest_id,
    );
    let sig_bytes: [u8; 64] = hex::decode(&body.signature).unwrap().try_into().unwrap();
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    use ed25519_dalek::Verifier;
    sk.verifying_key()
        .verify(&signed, &signature)
        .expect("signature must verify over the canonical bytes");
}

// ── (h, i) submit failure buckets ───────────────────────────────────────────

#[tokio::test]
async fn submit_invalid_stays_on_nat_mode_choice_with_terminal_error() {
    let (m, fake) = machine_with_fake();
    fake.set_submit_nat_mode_response(Err(NatModeError::Invalid {
        reason: "signature mismatch".into(),
    }));
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.seed_identity("01".repeat(32));
    m.set_step_for_test(OnboardingStep::NatModeChoice);

    let next = m.submit_nat_mode_choice().await;
    assert_eq!(next, OnboardingStep::NatModeChoice);
    let snap = m.nat_mode_snapshot();
    assert!(matches!(
        snap.state,
        NatModeState::Error {
            transient: false,
            ..
        }
    ));
    assert_eq!(snap.message.key, "onboarding.nat_mode.error.terminal");
    // Resubmit is allowed from Error.
    assert!(snap.submit_enabled);
    assert!(m.wizard_outcome().is_none());
}

#[tokio::test]
async fn submit_transient_stays_on_nat_mode_choice_with_transient_error() {
    let (m, fake) = machine_with_fake();
    fake.set_submit_nat_mode_response(Err(NatModeError::Transient {
        cause: "boom".into(),
    }));
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.seed_identity("01".repeat(32));
    m.set_step_for_test(OnboardingStep::NatModeChoice);

    let next = m.submit_nat_mode_choice().await;
    assert_eq!(next, OnboardingStep::NatModeChoice);
    let snap = m.nat_mode_snapshot();
    assert!(matches!(
        snap.state,
        NatModeState::Error {
            transient: true,
            ..
        }
    ));
    assert_eq!(snap.message.key, "onboarding.nat_mode.error.transient");
    assert!(snap.submit_enabled);
}

// ── (j) defer ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn defer_yields_logged_in_done_and_sends_nothing() {
    let (m, fake) = machine_with_fake();
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.seed_identity("01".repeat(32));
    m.set_step_for_test(OnboardingStep::NatModeChoice);

    let next = m.defer_nat_mode_choice();
    assert_eq!(next, OnboardingStep::Done);
    match m.wizard_outcome().expect("outcome must be set") {
        WizardOutcome::LoggedIn { nest_url, handle } => {
            assert_eq!(nest_url, "https://example.com");
            assert_eq!(handle, "alice@example.com");
        }
        other => panic!("expected LoggedIn, got {other:?}"),
    }
    // Deferring keeps the server-side seed: nothing is sent.
    assert!(!fake.calls().iter().any(|c| c == "submit_nat_mode"));
}

// ── snapshot serde + E2E bridge dispatchers ─────────────────────────────────

#[tokio::test]
async fn nat_mode_snapshot_round_trips_via_json() {
    use fauna_onboarding_machine::state::LocalizedText;
    let s = NatModeSnapshot {
        state: NatModeState::Error {
            transient: true,
            cause: "boom".into(),
        },
        selected_mode: NodeMode::Private,
        message: LocalizedText {
            key: "k".into(),
            args: Default::default(),
        },
        submit_enabled: true,
    };
    let raw = serde_json::to_string(&s).unwrap();
    let s2: NatModeSnapshot = serde_json::from_str(&raw).unwrap();
    assert_eq!(s, s2);
}

#[tokio::test]
async fn call_machine_method_dispatches_set_nat_mode_snapshot() {
    let (m, _fake) = machine_with_fake();
    let payload = r#"{"state":{"Error":{"transient":true,"cause":"x"}},"selected_mode":"private","message":{"key":"k","args":{}},"submit_enabled":true}"#;
    m.call_machine_method("set_nat_mode_snapshot_for_test".into(), payload.into());
    let snap = m.nat_mode_snapshot();
    assert!(matches!(
        snap.state,
        NatModeState::Error {
            transient: true,
            ..
        }
    ));
    assert_eq!(snap.selected_mode, NodeMode::Private);
}

#[tokio::test]
async fn call_machine_method_dispatches_select_and_defer_nat_mode() {
    let (m, _fake) = machine_with_fake();
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.seed_identity("01".repeat(32));
    m.set_step_for_test(OnboardingStep::NatModeChoice);

    // `select_nat_mode` takes the NodeMode serde repr (snake_case wire form).
    m.call_machine_method("select_nat_mode".into(), "\"private\"".into());
    assert_eq!(m.nat_mode_snapshot().selected_mode, NodeMode::Private);

    m.call_machine_method("defer_nat_mode_choice".into(), String::new());
    assert_eq!(m.step(), OnboardingStep::Done);
    assert!(matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::LoggedIn { .. })
    ));
}

#[tokio::test]
async fn call_machine_method_async_runs_submit_nat_mode_choice() {
    let (m, fake) = machine_with_fake();
    fake.set_submit_nat_mode_response(Ok(()));
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    m.seed_identity("01".repeat(32));
    m.set_step_for_test(OnboardingStep::NatModeChoice);

    let result = m
        .call_machine_method_async("submit_nat_mode_choice".into(), String::new())
        .await;
    assert_eq!(result.as_deref(), Some("\"Done\""));
    assert_eq!(m.step(), OnboardingStep::Done);
}
