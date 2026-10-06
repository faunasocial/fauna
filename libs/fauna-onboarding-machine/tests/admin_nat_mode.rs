//! Lifecycle tests for the admin-panel NAT-mode control
//! (`AdminNatModeMachine`) — `docs/goal/behavior/admin.md` § Nest → NAT-mode
//! control. The machine shares the wizard's seam and commit ceremony; these
//! tests pin the admin-specific semantics: hydrate-pre-selects-current,
//! save-re-enables-after-success (mutable upsert; the page persists), and the
//! `admin.nest_page.nat_mode_*` message keys.

use std::sync::Arc;

use fauna_onboarding_machine::nest_api::{FakeNestApi, NatModeError, ProbeError, SetupStatus};
use fauna_onboarding_machine::{AdminNatModeMachine, NatModeState, NestApi, NodeMode};

fn machine_with_fake(secret_hex: &str) -> (Arc<AdminNatModeMachine>, Arc<FakeNestApi>) {
    let fake = Arc::new(FakeNestApi::new());
    let m = AdminNatModeMachine::with_nest_api(
        fake.clone() as Arc<dyn NestApi>,
        "https://example.com".into(),
        secret_hex.into(),
    );
    (m, fake)
}

const SECRET: &str = "0101010101010101010101010101010101010101010101010101010101010101";

// ── hydrate ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn hydrate_preselects_current_mode_from_setup_status() {
    let (m, fake) = machine_with_fake(SECRET);
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        node_mode: Some(NodeMode::Private),
    }));

    m.hydrate().await;
    let snap = m.snapshot();
    assert_eq!(snap.selected_mode, NodeMode::Private);
    assert_eq!(snap.state, NatModeState::Choosing);
    assert_eq!(snap.message.key, "admin.nest_page.nat_mode_choosing");
    assert!(snap.submit_enabled);
}

#[tokio::test]
async fn hydrate_unknown_node_mode_defaults_public() {
    // A `node_mode` spelling this build does not know reads as no seed, and
    // the pre-selection falls to the nest's absent-row default, Public.
    let (m, fake) = machine_with_fake(SECRET);
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        node_mode: None,
    }));

    m.hydrate().await;
    assert_eq!(m.snapshot().selected_mode, NodeMode::Public);
}

#[tokio::test]
async fn hydrate_failure_surfaces_error_but_keeps_save_enabled() {
    // The set is a mutable upsert — safe to submit without a successful read,
    // so a load failure must not brick the save button.
    let (m, fake) = machine_with_fake(SECRET);
    fake.set_probe_setup_status_response(Err(ProbeError::Transient {
        reason: "boom".into(),
    }));

    m.hydrate().await;
    let snap = m.snapshot();
    assert!(matches!(
        snap.state,
        NatModeState::Error {
            transient: true,
            ..
        }
    ));
    assert_eq!(snap.message.key, "admin.nest_page.nat_mode_error_load");
    assert_eq!(
        snap.message.args.get("cause").map(String::as_str),
        Some("boom")
    );
    assert!(snap.submit_enabled);
}

// ── select ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn select_flips_mode_and_recovers_from_error() {
    let (m, fake) = machine_with_fake(SECRET);
    fake.set_probe_setup_status_response(Err(ProbeError::Transient {
        reason: "boom".into(),
    }));
    m.hydrate().await;

    m.select(NodeMode::Private);
    let snap = m.snapshot();
    assert_eq!(snap.selected_mode, NodeMode::Private);
    assert_eq!(snap.state, NatModeState::Choosing);
    assert!(snap.submit_enabled);
}

// ── submit ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn submit_success_sends_signed_body_and_keeps_save_enabled() {
    let (m, fake) = machine_with_fake(SECRET);
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        node_mode: Some(NodeMode::Public),
    }));
    fake.set_submit_nat_mode_response(Ok(()));

    m.hydrate().await;
    m.select(NodeMode::Private);
    m.submit().await;

    let snap = m.snapshot();
    assert_eq!(snap.state, NatModeState::Done);
    assert_eq!(snap.message.key, "admin.nest_page.nat_mode_saved");
    // Unlike the wizard (which exits on Done), the admin page persists and the
    // set is mutable — save stays enabled for an immediate re-flip.
    assert!(snap.submit_enabled);

    // The commit ceremony is the wizard's exactly: one signed body whose
    // signature verifies over `mode_wire_str \n actor_id_hex \n timestamp`.
    let bodies = fake.submit_nat_mode_bodies();
    assert_eq!(bodies.len(), 1);
    let body = &bodies[0];
    assert_eq!(body.mode, NodeMode::Private);
    let sk = ed25519_dalek::SigningKey::from_bytes(&[0x01; 32]);
    assert_eq!(body.actor_id, hex::encode(sk.verifying_key().to_bytes()));
    // The body names the nest the fake stands in for.
    assert_eq!(
        body.nest_id,
        fauna_onboarding_machine::nest_api::FAKE_NEST_ID
    );
    // Built through the shared helper rather than hand-rolled here: the
    // canonical bytes are domain-separated (`sig_domain::SETUP_NAT_MODE_V2`),
    // and a test that re-spells the layout silently stops verifying the real
    // ceremony the moment the tag or the framing moves — which is exactly what
    // happened: this assertion was left asserting the pre-separation bytes and
    // had been red on `origin/main`.
    let signed = fauna_protocol::nat_mode::nat_mode_signed_message(
        "private",
        &body.actor_id,
        body.timestamp,
        &body.nest_id,
    );
    let sig_bytes: [u8; 64] = hex::decode(&body.signature).unwrap().try_into().unwrap();
    use ed25519_dalek::Verifier;
    sk.verifying_key()
        .verify(&signed, &ed25519_dalek::Signature::from_bytes(&sig_bytes))
        .expect("signature must verify over the canonical bytes");
}

#[tokio::test]
async fn submit_invalid_lands_terminal_error_with_save_enabled() {
    let (m, fake) = machine_with_fake(SECRET);
    fake.set_submit_nat_mode_response(Err(NatModeError::Invalid {
        reason: "signature mismatch".into(),
    }));

    m.select(NodeMode::Private);
    m.submit().await;

    let snap = m.snapshot();
    assert!(matches!(
        snap.state,
        NatModeState::Error {
            transient: false,
            ..
        }
    ));
    assert_eq!(snap.message.key, "admin.nest_page.nat_mode_error_terminal");
    assert!(snap.submit_enabled);
}

#[tokio::test]
async fn submit_transient_lands_retryable_error() {
    let (m, fake) = machine_with_fake(SECRET);
    fake.set_submit_nat_mode_response(Err(NatModeError::Transient {
        cause: "boom".into(),
    }));

    m.submit().await;

    let snap = m.snapshot();
    assert!(matches!(
        snap.state,
        NatModeState::Error {
            transient: true,
            ..
        }
    ));
    assert_eq!(snap.message.key, "admin.nest_page.nat_mode_error_transient");
    assert!(snap.submit_enabled);
}

#[tokio::test]
async fn submit_with_invalid_secret_is_terminal_and_sends_nothing() {
    let (m, fake) = machine_with_fake("not-hex");
    fake.set_submit_nat_mode_response(Ok(()));

    m.submit().await;

    let snap = m.snapshot();
    assert!(matches!(
        snap.state,
        NatModeState::Error {
            transient: false,
            ..
        }
    ));
    assert!(fake.submit_nat_mode_bodies().is_empty());
}
