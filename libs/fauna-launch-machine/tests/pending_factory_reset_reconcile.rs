//! CR-2 — the pending-factory-reset slot is boot-reconciled against the box.
//!
//! `docs/goal/architecture/nest/common.md` § Client-state recoverability. The slot
//! is evaluated before every other launch row, so a slot left behind by a
//! permanently-failed reset dispatch would otherwise route the client to a
//! pre-filled claim page for a box that is still claimed and healthy — on every
//! launch, with no in-app exit. `start()` therefore probes `fauna.setup.status`
//! whenever a slot is present, and honors the slot only if the box actually
//! answers "unclaimed".
//!
//! The `Unreachable` arm is the load-bearing one: a probe failure must NOT be read
//! as "claimed", because clearing the slot on a network blip destroys the only copy
//! of the claim code for a box that really was wiped — which is CR-1 again.
//!
//! Run with `cargo test -p fauna-launch-machine --features test-helpers,test-observer`.
#![cfg(all(feature = "test-helpers", feature = "test-observer"))]

use std::sync::Arc;

use fauna_launch_machine::{
    ClaimProbe, InMemoryPersistence, LaunchMachine, LaunchPersistence, LaunchPhase,
    LaunchWizardEntry, MockAuthConnector, NullObserver, PendingFactoryResetRecord,
    SilentChallengeOutcome, persistence::FACTORY_RESET_CLAIM_GRACE_SECS,
};
use fauna_protocol::auth::VerifyReply;

fn test_secret() -> [u8; 32] {
    [0x42; 32]
}

fn sample_slot() -> PendingFactoryResetRecord {
    PendingFactoryResetRecord {
        nest_url: "https://nest.example".into(),
        handle: "alice".into(),
        claim_code: "abcd-efgh-ijkl-mnop-qrstuv".into(),
        // Minted twice the grace ago = "old": the `Claimed` arm takes the
        // stale-slot clear, which is what the CR-2 tests assert.
        minted_at_secs: now_secs() - 2 * FACTORY_RESET_CLAIM_GRACE_SECS,
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A slot minted moments ago — the reset-in-flight shape the grace window
/// exists for (`FACTORY_RESET_CLAIM_GRACE_SECS`).
fn fresh_slot() -> PendingFactoryResetRecord {
    PendingFactoryResetRecord {
        minted_at_secs: now_secs(),
        ..sample_slot()
    }
}

fn verify_reply() -> VerifyReply {
    VerifyReply {
        token: "actor.opaque".into(),
        token_id: "0".repeat(16),
        handle: "alice".into(),
        domain: "nest.example".into(),
        tier: "free".into(),
        expires_at: 1_700_003_600,
        expires_in: 3600,
        ..Default::default()
    }
}

/// Identity + saved nest + a pending-factory-reset slot — the post-crash shape.
fn persistence_with_slot() -> Arc<InMemoryPersistence> {
    Arc::new(
        InMemoryPersistence::new()
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://nest.example")
            .with_pending_factory_reset(sample_slot()),
    )
}

/// The CR-2 bug itself: the reset dispatch failed permanently, the box was never
/// wiped, and it still answers `claimed = true`. The slot must be cleared and the
/// launch must take the ordinary rows — the silent challenge just logs the admin
/// back in.
#[tokio::test]
async fn stale_slot_on_a_claimed_box_is_cleared_and_the_launch_proceeds() {
    let connector = MockAuthConnector::new()
        .with_claim_probe(ClaimProbe::Claimed)
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()));
    let p = persistence_with_slot();
    let m =
        LaunchMachine::new_with_connector(Arc::new(NullObserver), p.clone(), Arc::new(connector));

    m.start().await;

    assert_eq!(
        m.snapshot().phase,
        LaunchPhase::Online,
        "a stale slot must not hijack the launch of a healthy claimed box"
    );
    assert_eq!(
        p.load_pending_factory_reset(),
        None,
        "the stale slot must be cleared, or it hijacks every future launch too"
    );
}

/// The reset-in-flight window (measured on web, 2026-07-16): between the reset
/// dispatch and the box's self-exit the box still answers `Claimed`, and the
/// web flow enters the launch path in that window BY DESIGN (the admin page
/// navigates to onboarding the moment the reply lands). A freshly-minted slot
/// must therefore be HONORED on a `Claimed` probe, not cleared — clearing it
/// destroys the only copy of the claim code moments before the wipe executes:
/// CR-1 data loss delivered by CR-2's own reconcile.
#[tokio::test]
async fn a_fresh_slot_on_a_still_claimed_box_is_honored_not_cleared() {
    let slot = fresh_slot();
    let connector = MockAuthConnector::new().with_claim_probe(ClaimProbe::Claimed);
    let p = Arc::new(
        InMemoryPersistence::new()
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://nest.example")
            .with_pending_factory_reset(slot.clone()),
    );
    let m =
        LaunchMachine::new_with_connector(Arc::new(NullObserver), p.clone(), Arc::new(connector));

    m.start().await;

    assert_eq!(
        m.snapshot().phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::PendingFactoryReset
        },
        "a Claimed probe within the mint grace is a reset IN FLIGHT, not a stale slot"
    );
    assert_eq!(
        p.load_pending_factory_reset(),
        Some(slot),
        "the freshly-minted slot must survive — clearing it here is the measured CR-1 loss"
    );
}

/// The reset really landed: the box is fresh/unclaimed. Resume the pre-filled
/// claim with the code the client minted before dispatch (CR-1's whole point).
#[tokio::test]
async fn slot_on_an_unclaimed_box_resumes_the_prefilled_claim() {
    let connector = MockAuthConnector::new().with_claim_probe(ClaimProbe::Unclaimed);
    let p = persistence_with_slot();
    let m =
        LaunchMachine::new_with_connector(Arc::new(NullObserver), p.clone(), Arc::new(connector));

    m.start().await;

    assert_eq!(
        m.snapshot().phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::PendingFactoryReset
        },
    );
    assert_eq!(
        p.load_pending_factory_reset(),
        Some(sample_slot()),
        "the slot must survive until the claim actually succeeds"
    );
}

/// The CR-1 guard. An unreachable box cannot distinguish "the reset never happened"
/// from "the reset happened and the reply was lost" — so the slot MUST survive. If a
/// probe failure were read as `claimed`, a launch during a network blip would delete
/// the only copy of the claim code for a box that really was wiped.
#[tokio::test]
async fn an_unreachable_probe_keeps_the_slot_and_resumes_the_claim() {
    let connector = MockAuthConnector::new().with_claim_probe(ClaimProbe::Unreachable);
    let p = persistence_with_slot();
    let m =
        LaunchMachine::new_with_connector(Arc::new(NullObserver), p.clone(), Arc::new(connector));

    m.start().await;

    assert_eq!(
        m.snapshot().phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::PendingFactoryReset
        },
        "an unreachable box must still resume the claim — the code is in the slot"
    );
    assert_eq!(
        p.load_pending_factory_reset(),
        Some(sample_slot()),
        "clearing the slot on a probe failure would be CR-1 again: the claim code \
         for a genuinely-wiped box would be gone with no client able to learn it"
    );
}

/// The reconcile must not put a network round-trip on the common launch path: with
/// no slot present, `start()` never probes.
#[tokio::test]
async fn the_common_launch_path_never_probes() {
    let connector = Arc::new(
        MockAuthConnector::new()
            .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply())),
    );
    let p = Arc::new(
        InMemoryPersistence::new()
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://nest.example"),
    );
    let m = LaunchMachine::new_with_connector(Arc::new(NullObserver), p, connector.clone());

    m.start().await;

    assert_eq!(m.snapshot().phase, LaunchPhase::Online);
    assert_eq!(
        connector.claim_probe_calls(),
        0,
        "the setup.status probe is for the slot row only — it must never cost the \
         ordinary launch a round trip"
    );
}
