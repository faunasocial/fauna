//! Verifies LaunchMachine::start() routes to the correct phase per
//! docs/goal/behavior/onboarding.md § App-launch routing (lines 230–243).
//!
//! Run with `cargo test -p fauna-launch-machine --features test-helpers,test-observer`.
#![cfg(all(feature = "test-helpers", feature = "test-observer"))]

use std::sync::Arc;

use fauna_launch_machine::{
    AwaitingDnsRecord, InMemoryPersistence, LaunchMachine, LaunchPhase, LaunchWizardEntry,
    NullObserver, PendingInviteRecord,
};

fn sample_invite() -> PendingInviteRecord {
    PendingInviteRecord {
        nest_url: "https://nest.example".into(),
        handle: "alice".into(),
        request_id: "req-1".into(),
        status_json: r#"{"PendingReview":{"request_id":"req-1","last_checked_ms":0}}"#.into(),
    }
}

fn sample_awaiting_dns() -> AwaitingDnsRecord {
    AwaitingDnsRecord {
        nest_url: "https://nest.example".into(),
        handle: "alice".into(),
        dns_records_json:
            r#"[{"record_type":"A","name":"@","value":"1.2.3.4","ttl":300,"priority":null}]"#.into(),
        claim_code: "claim-abc".into(),
        reach_ipv4: None,
        nest_actor_id: None,
    }
}

#[tokio::test]
async fn no_identity_routes_to_wizard_at_identity_choice() {
    let persistence = Arc::new(InMemoryPersistence::new());
    let machine = LaunchMachine::new(Arc::new(NullObserver), persistence);
    machine.start().await;
    assert_eq!(
        machine.snapshot().phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::IdentityChoice
        }
    );
}

#[tokio::test]
async fn identity_only_routes_to_wizard_at_handle_entry() {
    let persistence = Arc::new(InMemoryPersistence::new().with_identity(vec![0xaa; 32]));
    let machine = LaunchMachine::new(Arc::new(NullObserver), persistence);
    machine.start().await;
    assert_eq!(
        machine.snapshot().phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::HandleEntry
        }
    );
}

#[tokio::test]
async fn identity_plus_pending_invite_routes_to_wizard_at_invite_request() {
    let persistence = Arc::new(
        InMemoryPersistence::new()
            .with_identity(vec![0xaa; 32])
            .with_pending_invite(sample_invite()),
    );
    let machine = LaunchMachine::new(Arc::new(NullObserver), persistence);
    machine.start().await;
    assert_eq!(
        machine.snapshot().phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::InviteRequest
        }
    );
}

#[tokio::test]
async fn identity_plus_awaiting_dns_routes_to_wizard_at_awaiting_manual_dns() {
    let persistence = Arc::new(
        InMemoryPersistence::new()
            .with_identity(vec![0xaa; 32])
            .with_awaiting_dns(sample_awaiting_dns()),
    );
    let machine = LaunchMachine::new(Arc::new(NullObserver), persistence);
    machine.start().await;
    assert_eq!(
        machine.snapshot().phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::AwaitingManualDns
        }
    );
}

/// The ordering claim in `docs/goal/behavior/onboarding.md` § App-launch routing:
/// the awaiting-manual-dns row is checked **before** the silent-challenge row,
/// "while DNS is still pending the nest is unreachable, so a silent challenge
/// would just fail through to the `launch_retry` surface".
///
/// A saved `nest_url` must therefore NOT divert the launch onto a challenge the
/// not-yet-reachable nest cannot answer. No HTTP mock is wired here on purpose:
/// if the branch order regresses, the machine tries to reach `nest.example` and
/// lands in `Offline`/`SilentChallenge` rather than the wizard entry — which is
/// exactly the failure this pins.
#[tokio::test]
async fn awaiting_dns_is_checked_before_the_silent_challenge_row() {
    let persistence = Arc::new(
        InMemoryPersistence::new()
            .with_identity(vec![0xaa; 32])
            .with_nest_url("https://nest.example")
            .with_awaiting_dns(sample_awaiting_dns()),
    );
    let machine = LaunchMachine::new(Arc::new(NullObserver), persistence);
    machine.start().await;
    assert_eq!(
        machine.snapshot().phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::AwaitingManualDns
        },
        "a saved nest_url must not outrank the awaiting-manual-dns slot"
    );
}

/// The slot outranks a pending invite too — a client that deferred DNS and also
/// has a stale invite row resumes the nest it is mid-provisioning.
#[tokio::test]
async fn awaiting_dns_outranks_a_pending_invite() {
    let persistence = Arc::new(
        InMemoryPersistence::new()
            .with_identity(vec![0xaa; 32])
            .with_pending_invite(sample_invite())
            .with_awaiting_dns(sample_awaiting_dns()),
    );
    let machine = LaunchMachine::new(Arc::new(NullObserver), persistence);
    machine.start().await;
    assert_eq!(
        machine.snapshot().phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::AwaitingManualDns
        }
    );
}

// Case 1 (identity + nest_url) now runs the silent challenge inline,
// so its routing test belongs in silent_challenge.rs against a
// wiremock nest. This file covers Cases 2–4 (no-HTTP routing only).

#[tokio::test]
async fn snapshot_before_start_is_boot() {
    let persistence = Arc::new(InMemoryPersistence::new());
    let machine = LaunchMachine::new(Arc::new(NullObserver), persistence);
    assert_eq!(machine.snapshot().phase, LaunchPhase::Boot);
}

#[tokio::test]
async fn observer_notified_on_phase_transitions() {
    use fauna_launch_machine::CountingObserver;
    let persistence = Arc::new(InMemoryPersistence::new());
    let observer = CountingObserver::new();
    let machine = LaunchMachine::new(observer.clone(), persistence);
    assert_eq!(observer.count(), 0);
    machine.start().await;
    // Boot → Hydrating → WizardAt: at least 2 transitions.
    assert!(
        observer.count() >= 2,
        "observer count was {}, expected ≥2",
        observer.count()
    );
}
