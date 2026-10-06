//! Identity-recovery onboarding steps — navigation + the pending-root latch.
//!
//! Per `docs/goal/behavior/onboarding.md` § 1 Identity (screens ratified
//! 2026-08-01) and `tests/e2e-unified/ui.yaml` pages `recovery_kit` /
//! `recovery_entry`. The kit screen mints and displays only — registration +
//! escrow run at the wizard's signed-in handoff
//! (`identity-succession.md` § The RecoveryKey → *Creation UX*), which is what
//! the take-latch tests pin. The ceremony drive is a separate slice; these are
//! the shared transitions every app's screens ride.

use std::sync::Arc;

use fauna_core::hex32::is_hex64;
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{OnboardingMachine, OnboardingStep};

fn machine() -> Arc<OnboardingMachine> {
    OnboardingMachine::new(Arc::new(NullObserver))
}

/// Create an identity and confirm it, driving the machine to wherever
/// `confirm_generated_identity` routes for the current capability declaration.
fn create_and_confirm(m: &OnboardingMachine) {
    m.begin_create_identity();
    m.confirm_generated_identity()
        .expect("generated secret exists after begin_create_identity");
}

// ── The capability flag: six apps unchanged, tui routes through the kit ─────

/// An app that never declares the capability keeps the pre-existing flow:
/// confirm goes straight to `handle_entry`, and no recovery root is ever
/// minted — the batched-trickle-down parity gap, pinned.
#[test]
fn without_the_capability_confirm_goes_straight_to_handle_entry() {
    let m = machine();
    create_and_confirm(&m);

    assert_eq!(m.step(), OnboardingStep::HandleEntry);
    assert_eq!(
        m.recovery_kit_secret_hex(),
        None,
        "no capability, no mint — the six lagging apps must be byte-for-byte unchanged"
    );
    assert!(m.take_pending_recovery_secret().is_none());
}

/// With the capability declared, confirm routes through the kit screen and a
/// 64-hex root is minted for display.
#[test]
fn with_the_capability_confirm_routes_to_recovery_kit_and_mints() {
    let m = machine();
    m.set_renders_recovery_kit(true);
    create_and_confirm(&m);

    assert_eq!(m.step(), OnboardingStep::RecoveryKit);
    let hex = m
        .recovery_kit_secret_hex()
        .expect("entering recovery_kit mints the root");
    assert!(is_hex64(&hex), "the root is the 64-hex idiom: {hex}");
}

// ── Confirm keeps the root for the handoff; skip drops it ───────────────────

/// `recovery-kit-confirm-button` → `handle_entry`, and the root survives to be
/// taken exactly once at the signed-in handoff.
#[test]
fn confirm_keeps_the_root_and_the_handoff_takes_it_once() {
    let m = machine();
    m.set_renders_recovery_kit(true);
    create_and_confirm(&m);
    let shown = m.recovery_kit_secret_hex().expect("minted");

    m.confirm_recovery_kit();
    assert_eq!(m.step(), OnboardingStep::HandleEntry);

    let taken = m
        .take_pending_recovery_secret()
        .expect("confirm keeps the root for the handoff");
    assert_eq!(
        taken.as_str(),
        shown,
        "the handoff registers the phrase the user saved"
    );
    assert!(
        m.take_pending_recovery_secret().is_none(),
        "the latch is consume-once — a second handoff pass must not re-register"
    );
}

/// `recovery-kit-skip-button` → `handle_entry` with the root dropped: nothing
/// registers at handoff, so Settings' never-created warning tells the truth.
#[test]
fn skip_drops_the_root_so_nothing_registers_at_handoff() {
    let m = machine();
    m.set_renders_recovery_kit(true);
    create_and_confirm(&m);
    assert!(m.recovery_kit_secret_hex().is_some());

    m.skip_recovery_kit();
    assert_eq!(m.step(), OnboardingStep::HandleEntry);
    assert!(m.take_pending_recovery_secret().is_none());
}

// ── Re-entry semantics: a written-down phrase is never silently invalidated ──

/// Confirm the kit, back out of `handle_entry` to `identity_created`, continue
/// again: the SAME root must be re-shown — a re-mint would silently invalidate
/// a phrase the user already wrote down.
#[test]
fn re_entry_after_back_navigation_re_shows_the_same_root() {
    let m = machine();
    m.set_renders_recovery_kit(true);
    create_and_confirm(&m);
    let first = m.recovery_kit_secret_hex().expect("minted");
    m.confirm_recovery_kit();

    m.back(); // handle_entry → identity_created (came-from-create)
    assert_eq!(m.step(), OnboardingStep::IdentityCreated);
    m.confirm_generated_identity()
        .expect("secret still present");

    assert_eq!(m.step(), OnboardingStep::RecoveryKit);
    assert_eq!(
        m.recovery_kit_secret_hex().as_deref(),
        Some(first.as_str()),
        "re-entry reuses the pending root"
    );
}

/// Skip, then come back and continue again: skip dropped the root, so a FRESH
/// one is minted — the discarded phrase stays discarded.
#[test]
fn re_entry_after_a_skip_mints_fresh() {
    let m = machine();
    m.set_renders_recovery_kit(true);
    create_and_confirm(&m);
    let first = m.recovery_kit_secret_hex().expect("minted");
    m.skip_recovery_kit();

    m.back();
    m.confirm_generated_identity()
        .expect("secret still present");

    let second = m.recovery_kit_secret_hex().expect("re-minted after skip");
    assert_ne!(first, second, "a skipped root must not come back");
}

// ── recovery_entry navigation ───────────────────────────────────────────────

/// `restore-from-recovery-kit-button` on `identity_choice` routes to the
/// phrase-entry screen; its back button returns to `identity_choice`.
#[test]
fn recovery_entry_routes_from_identity_choice_and_backs_out() {
    let m = machine();
    assert_eq!(m.step(), OnboardingStep::IdentityChoice);

    m.begin_recovery_entry();
    assert_eq!(m.step(), OnboardingStep::RecoveryEntry);

    m.back();
    assert_eq!(m.step(), OnboardingStep::IdentityChoice);
}

/// The kit screen has no Back by design (ui.yaml `recovery_kit`: confirm and
/// skip are its only exits) — back() stays put.
#[test]
fn recovery_kit_has_no_back() {
    let m = machine();
    m.set_renders_recovery_kit(true);
    create_and_confirm(&m);

    m.back();
    assert_eq!(m.step(), OnboardingStep::RecoveryKit);
}

// ── The capability survives a factory reset ─────────────────────────────────

/// `set_renders_recovery_kit` is a **static per-app declaration**, not run
/// state: the app calls it once when it builds the machine (tui does so in
/// `Wizard::new`). A factory reset clears the user's onboarding progress — it
/// must NOT un-declare what the app is capable of rendering, because nothing
/// ever re-declares it: the app builds its `Wizard` once per process, so a
/// capability dropped here is dropped for the rest of that process's life.
///
/// The e2e `app` fixture calls `reset()` before EVERY test against one
/// session-scoped app process, so a reset that strips the capability makes
/// tui's onboarding route through the kit screen when a test runs alone and
/// skip it once the same test runs in a batch — the batch-vs-solo divergence
/// class `testing.md` § point 10 exists to keep out of the suite.
#[test]
fn a_factory_reset_preserves_the_apps_declared_capability() {
    let m = machine();
    m.set_renders_recovery_kit(true);

    m.reset();

    // The declaration survives, so the post-reset re-onboard still routes
    // through the kit screen exactly as the first pass did.
    create_and_confirm(&m);
    assert_eq!(
        m.step(),
        OnboardingStep::RecoveryKit,
        "a factory reset must clear onboarding PROGRESS, never the app's \
         declared rendering capability — nothing re-declares it"
    );
}
