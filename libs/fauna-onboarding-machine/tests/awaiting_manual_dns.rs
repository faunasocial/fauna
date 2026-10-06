//! Lifecycle tests for the post-provisioning "Almost ready" surface.
//!
//! After the deferred-DNS provisioning path, the wizard exits to `Done`
//! with `wizard_outcome() == AwaitingManualDns`. The per-app glue saves
//! the slot and renders the "Almost ready" surface, which polls
//! `recheck_manual_dns()` until the freshly-provisioned nest comes online
//! and can be claimed. Every probe here is *pre-claim* HTTP over the
//! `NestApi` trait (`GET /api/v1/setup-status`, `POST /api/v1/claim-admin`)
//! — there is no WS-RPC, because an authenticated actor session only exists
//! after the claim succeeds.
//!
//! Per `docs/goal/behavior/onboarding.md` § "Wizard exit handling".
//!
//! These drive the machine with `FakeNestApi` (no wiremock): the whole
//! surface is reachable through the two `NestApi` endpoints, so a fake is
//! sufficient and far cheaper than standing up an HTTP server.

use std::sync::{Arc, Mutex};

use fauna_launch_machine::{AwaitingDnsRecord, PendingProvisionStore};
use fauna_onboarding_machine::nest_api::{
    ClaimAdminError, ClaimAdminResponse, ProbeError, SetupStatus, SilentChallengeOutcome,
};
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    AwaitingDnsState, DnsRecordPlain, FakeNestApi, OnboardingMachine, OnboardingObserver,
    OnboardingStep, WizardOutcome,
};

fn fixture_secret_hex() -> String {
    "01".repeat(32)
}

fn fixture_records() -> Vec<DnsRecordPlain> {
    vec![DnsRecordPlain {
        record_type: "A".into(),
        name: "@".into(),
        value: "1.2.3.4".into(),
        ttl: 300,
        priority: None,
    }]
}

/// Build a machine wired to a `FakeNestApi`, already seeded into the
/// AwaitingManualDns surface with a valid identity.
fn seeded(fake: Arc<FakeNestApi>) -> Arc<OnboardingMachine> {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::with_nest_api(observer, fake);
    m.seed_identity(fixture_secret_hex());
    m.seed_awaiting_manual_dns(
        "https://example.test".into(),
        "alice@example.test".into(),
        fixture_records(),
        "CLAIM-XYZ".into(),
    );
    m
}

#[tokio::test]
async fn seed_awaiting_manual_dns_hydrates_outcome_and_snapshot() {
    let fake = Arc::new(FakeNestApi::new());
    let m = seeded(fake);

    assert_eq!(m.step(), OnboardingStep::Done);
    assert_eq!(m.nest_url(), "https://example.test");
    assert_eq!(m.current_handle(), "alice@example.test");
    match m.wizard_outcome() {
        Some(WizardOutcome::AwaitingManualDns {
            nest_url,
            dns_records,
            claim_code,
        }) => {
            assert_eq!(nest_url, "https://example.test");
            assert_eq!(claim_code, "CLAIM-XYZ");
            assert_eq!(dns_records, fixture_records());
        }
        other => panic!("expected AwaitingManualDns, got {other:?}"),
    }
    let snap = m.awaiting_manual_dns_snapshot();
    assert_eq!(snap.state, AwaitingDnsState::Pending);
    assert_eq!(snap.dns_records, fixture_records());
}

#[tokio::test]
async fn recheck_is_noop_when_not_awaiting() {
    let fake = Arc::new(FakeNestApi::new());
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::with_nest_api(observer, fake.clone());
    // No seed: outcome is None, step is the fresh IdentityChoice.
    let step = m.recheck_manual_dns().await;
    assert_eq!(step, OnboardingStep::IdentityChoice);
    assert!(fake.calls().is_empty(), "must not probe when not awaiting");
}

#[tokio::test]
async fn recheck_stays_pending_when_nest_unreachable() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Err(ProbeError::Transient {
        reason: "connection refused".into(),
    }));
    let m = seeded(fake.clone());

    let step = m.recheck_manual_dns().await;
    // Still awaiting — the client keeps polling.
    assert_eq!(step, OnboardingStep::Done);
    assert!(matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::AwaitingManualDns { .. })
    ));
    assert_eq!(
        m.awaiting_manual_dns_snapshot().state,
        AwaitingDnsState::Pending
    );
    // Probed, but no claim attempted.
    assert_eq!(fake.calls(), vec!["probe_setup_status".to_string()]);
}

#[tokio::test]
async fn recheck_claims_when_nest_reachable_and_unclaimed() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: false,
        ..Default::default()
    }));
    fake.set_claim_admin_response(Ok(ClaimAdminResponse {
        ok: true,
        token: Some("tok".into()),
        expires_at: Some(0),
        domain: Some("example.test".into()),
        deployment_seed: None,
    }));
    let m = seeded(fake.clone());

    let step = m.recheck_manual_dns().await;
    // Mirrors wizard_submit_claim_code: claim → NatModeChoice, not straight to
    // LoggedIn. The NAT confirm is the terminal step.
    assert_eq!(step, OnboardingStep::NatModeChoice);
    assert_eq!(m.step(), OnboardingStep::NatModeChoice);
    // No longer awaiting; the outcome is cleared until the mode is committed.
    assert!(m.wizard_outcome().is_none());
    assert_eq!(
        m.awaiting_manual_dns_snapshot().state,
        AwaitingDnsState::Claimed
    );
    assert_eq!(
        fake.calls(),
        vec!["probe_setup_status".to_string(), "claim_admin".to_string()]
    );
}

/// box-recovery.md § The plane-era recovery floor, (c): the deferred-DNS claim
/// path does not consume the reply's seed either.
#[tokio::test]
async fn recheck_does_not_consume_the_deployment_seed() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: false,
        ..Default::default()
    }));
    let seed_hex = "cd".repeat(32);
    fake.set_claim_admin_response(Ok(ClaimAdminResponse {
        ok: true,
        token: Some("tok".into()),
        expires_at: Some(0),
        domain: Some("example.test".into()),
        deployment_seed: Some(seed_hex.clone().into()),
    }));
    let m = seeded(fake.clone());

    let step = m.recheck_manual_dns().await;
    assert_eq!(step, OnboardingStep::NatModeChoice);
}

#[tokio::test]
async fn recheck_terminal_error_on_invalid_claim_code() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: false,
        ..Default::default()
    }));
    fake.set_claim_admin_response(Err(ClaimAdminError::Invalid {
        reason: "claim code expired".into(),
    }));
    let m = seeded(fake.clone());

    let step = m.recheck_manual_dns().await;
    // Stays on the surface; outcome unchanged so a relaunch resumes here.
    assert_eq!(step, OnboardingStep::Done);
    assert!(matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::AwaitingManualDns { .. })
    ));
    match m.awaiting_manual_dns_snapshot().state {
        AwaitingDnsState::Error { cause } => assert!(cause.contains("claim code expired")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn recheck_transient_claim_failure_returns_to_pending() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: false,
        ..Default::default()
    }));
    fake.set_claim_admin_response(Err(ClaimAdminError::Transient {
        cause: "503".into(),
    }));
    let m = seeded(fake.clone());

    let step = m.recheck_manual_dns().await;
    // Transient → keep polling; not a terminal Error.
    assert_eq!(step, OnboardingStep::Done);
    assert!(matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::AwaitingManualDns { .. })
    ));
    assert_eq!(
        m.awaiting_manual_dns_snapshot().state,
        AwaitingDnsState::Pending
    );
}

/// A registered-probe fixture: the silent challenge reports this secret IS
/// registered on the box — i.e. the claim it holds is ours.
fn challenge_says_ours() -> SilentChallengeOutcome {
    SilentChallengeOutcome::Success(fauna_protocol::auth::VerifyReply {
        token: "bearer".into(),
        token_id: "0".repeat(16),
        handle: "alice".into(),
        domain: "example.test".into(),
        tier: "free".into(),
        expires_at: 0,
        ..Default::default()
    })
}

/// Recovery edge (`onboarding-provisioning.md` § 6 *Provisioning = build +
/// claim*): the probe reports the box is *already* claimed — we claimed it, then
/// the app restarted before the wizard exited. Conclude **without re-claiming**
/// and without the NAT page — but only after the **silent challenge** confirms
/// the box is ours, since `claimed: true` alone cannot tell our own lost-reply
/// claim from a stranger's nest (the arm below). This fixture never declares the
/// trust prompt, so the conclusion is `LoggedIn` directly; on an app that does,
/// the resume parks on § 3b-ter's offer first (`trust_prompt_navigation.rs`).
///
/// A claimed nest has no setup state left to branch on: the recheck logs in
/// through the challenge and never re-claims (the storage-mode compat commit
/// that a `mode: None` reply once drew here left with the compat-remnant
/// sweep, 2026-09-24).
#[tokio::test]
async fn recheck_already_claimed_logs_in_without_reclaiming() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        ..Default::default()
    }));
    fake.set_silent_challenge_response(challenge_says_ours());
    let m = seeded(fake.clone());

    let step = m.recheck_manual_dns().await;
    assert_eq!(step, OnboardingStep::Done);
    match m.wizard_outcome() {
        Some(WizardOutcome::LoggedIn { nest_url, handle }) => {
            assert_eq!(nest_url, "https://example.test");
            assert_eq!(handle, "alice@example.test");
        }
        other => panic!("expected LoggedIn, got {other:?}"),
    }
    // Already claimed → must NOT re-attempt a claim; ownership is settled by
    // the challenge instead.
    assert_eq!(
        fake.calls(),
        vec![
            "probe_setup_status".to_string(),
            "silent_challenge".to_string()
        ]
    );
}

/// The other side of the recovery edge, and the reason it is a challenge and not
/// an assumption: the box answers `claimed: true` but does **not** know this
/// identity — someone else's nest at this address. Signing the user in there
/// would hand them a nest they do not own, so this is terminal, not a login.
#[tokio::test]
async fn recheck_already_claimed_by_someone_else_does_not_log_in() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        ..Default::default()
    }));
    fake.set_silent_challenge_response(SilentChallengeOutcome::NotRegistered);
    let m = seeded(fake.clone());

    m.recheck_manual_dns().await;
    assert!(
        matches!(
            m.wizard_outcome(),
            Some(WizardOutcome::AwaitingManualDns { .. })
        ),
        "a stranger's box must never produce LoggedIn"
    );
    assert!(
        matches!(
            m.awaiting_manual_dns_snapshot().state,
            AwaitingDnsState::Error { .. }
        ),
        "terminal — there is nothing to keep polling for"
    );
    assert_eq!(
        fake.calls(),
        vec![
            "probe_setup_status".to_string(),
            "silent_challenge".to_string()
        ],
        "and no claim is attempted against it"
    );
}

/// A challenge that cannot answer (box unreachable mid-ceremony, degraded nest)
/// is neither proof of ownership nor proof against it: stay `Pending` so the next
/// poll asks again. Writing an error here would wedge the surface behind a
/// terminal screen on a dropped connection.
#[tokio::test]
async fn recheck_already_claimed_with_an_unanswerable_challenge_keeps_polling() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        ..Default::default()
    }));
    fake.set_silent_challenge_response(SilentChallengeOutcome::Transient {
        error: "connection reset".into(),
    });
    let m = seeded(fake.clone());

    m.recheck_manual_dns().await;
    assert!(matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::AwaitingManualDns { .. })
    ));
    assert_eq!(
        m.awaiting_manual_dns_snapshot().state,
        AwaitingDnsState::Pending
    );
}

/// The nest booted a degraded "needs-update" mode (`fauna.nest.outdated`,
/// `version-compatibility.md` Dim 4) — not retryable, so this must be
/// terminal rather than folded into the unanswerable-challenge `Pending` arm
/// above it: spinning a retry loop against a nest that has already said it
/// cannot serve would never resolve on its own.
#[tokio::test]
async fn recheck_already_claimed_but_nest_needs_update_is_terminal() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        ..Default::default()
    }));
    fake.set_silent_challenge_response(SilentChallengeOutcome::NeedsUpdate {
        message: "update required".into(),
    });
    let m = seeded(fake.clone());

    m.recheck_manual_dns().await;
    assert!(
        matches!(
            m.wizard_outcome(),
            Some(WizardOutcome::AwaitingManualDns { .. })
        ),
        "stays on the surface so a relaunch resumes here"
    );
    match m.awaiting_manual_dns_snapshot().state {
        AwaitingDnsState::Error { cause } => assert!(cause.contains("nest outdated")),
        other => panic!("expected terminal Error, got {other:?}"),
    }
}

/// The supplied secret isn't a valid 32-byte Ed25519 key — terminal, same
/// disposition `auth.rs`'s `SecretInvalid` doc-comment names.
#[tokio::test]
async fn recheck_already_claimed_with_invalid_secret_is_terminal() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        ..Default::default()
    }));
    fake.set_silent_challenge_response(SilentChallengeOutcome::SecretInvalid {
        error: "bad signature".into(),
    });
    let m = seeded(fake.clone());

    m.recheck_manual_dns().await;
    assert!(matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::AwaitingManualDns { .. })
    ));
    match m.awaiting_manual_dns_snapshot().state {
        AwaitingDnsState::Error { cause } => assert!(cause.contains("signature mismatch")),
        other => panic!("expected terminal Error, got {other:?}"),
    }
}

/// This identity was succeeded (`identity-succession.md` § Propagation →
/// *Own device fleet*) — terminal for a reason no retry touches: the old key
/// still produces valid signatures forever, so re-signing only re-earns the
/// refusal.
#[tokio::test]
async fn recheck_already_claimed_identity_superseded_is_terminal() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        ..Default::default()
    }));
    fake.set_silent_challenge_response(SilentChallengeOutcome::Superseded {
        new_actor_id_hex: "ab".repeat(32),
    });
    let m = seeded(fake.clone());

    m.recheck_manual_dns().await;
    assert!(matches!(
        m.wizard_outcome(),
        Some(WizardOutcome::AwaitingManualDns { .. })
    ));
    match m.awaiting_manual_dns_snapshot().state {
        AwaitingDnsState::Error { cause } => assert!(cause.contains("identity superseded")),
        other => panic!("expected terminal Error, got {other:?}"),
    }
}

/// The nest's pinned deployment identity changed (`security.md`
/// § Transport trust — the TOFU `known_hosts` model): auto-entry is BLOCKED
/// and the user must explicitly re-trust, never a silent re-pin or retry
/// loop. This is the security-relevant regression test — before this fix the
/// catch-all routed this signal into a silent, unbounded `NotYet` poll,
/// indistinguishable from "still booting" on the "Almost ready" surface.
#[tokio::test]
async fn recheck_already_claimed_identity_changed_is_terminal_not_a_retry_loop() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        ..Default::default()
    }));
    fake.set_silent_challenge_response(SilentChallengeOutcome::IdentityChanged {
        host: "example.test".into(),
        pinned_hex: "11".repeat(32),
        seen_hex: Some("22".repeat(32)),
        fork: false,
    });
    let m = seeded(fake.clone());

    m.recheck_manual_dns().await;
    assert!(
        matches!(
            m.wizard_outcome(),
            Some(WizardOutcome::AwaitingManualDns { .. })
        ),
        "must not silently keep polling a mismatched-identity nest"
    );
    match m.awaiting_manual_dns_snapshot().state {
        AwaitingDnsState::Error { cause } => assert!(cause.contains("nest identity changed")),
        other => panic!("expected terminal Error, got {other:?}"),
    }
}

/// The PROBE leg of the same law: a
/// first-contact trust graduation failure on the probe's fresh connection —
/// `WsNestApi::core` maps a held-root mismatch to
/// `ProbeError::IdentityMismatch` — must land the surface on the terminal
/// `Error`, NOT on the DNS resting message (`security.md` § Pre-claim
/// surfacing). Before the fix every probe error was flattened into
/// `NotYet`, so a detected MITM signal rendered as an endless, error-free
/// "waiting for DNS to propagate" spinner, indistinguishable from "box
/// still booting".
#[tokio::test]
async fn recheck_probe_identity_mismatch_is_terminal_not_the_dns_resting_message() {
    let fake = Arc::new(FakeNestApi::new());
    fake.set_probe_setup_status_response(Err(ProbeError::IdentityMismatch {
        reason: "nest trust: the pre-resolved identity root does not match the connected nest"
            .into(),
    }));
    let m = seeded(fake.clone());

    let step = m.recheck_manual_dns().await;
    // Still on the surface (a relaunch resumes here), but the snapshot must
    // tell the truth instead of resting on the DNS message.
    assert_eq!(step, OnboardingStep::Done);
    match m.awaiting_manual_dns_snapshot().state {
        AwaitingDnsState::Error { cause } => assert!(
            cause.contains("identity"),
            "the terminal error must carry the identity verdict; got {cause:?}"
        ),
        other => {
            panic!("expected terminal Error, got {other:?} — a mismatch must not keep polling")
        }
    }
    // The verdict is terminal at the first leg: no claim was attempted.
    assert_eq!(fake.calls(), vec!["probe_setup_status".to_string()]);
}

/// A factory reset must clear the wizard OUTCOME, not just the step.
///
/// The "Almost ready" surface is deliberately **not** an `OnboardingStep` — the
/// per-app glue renders it off `wizard_outcome()` (see this file's header and
/// `onboarding.md` § "Wizard exit handling"). So a `reset()` that returns `step`
/// to `IdentityChoice` while leaving `outcome` set produces a client that paints
/// "Almost ready" over a machine that believes it is at the start: the user
/// factory-resets and never reaches identity-choice again, because nothing
/// re-clears the outcome for the rest of that process's life.
///
/// This is the same class as the `renders_recovery_kit` bug — machine state that
/// lives OUTSIDE `State`, which `*guard = State::new()` therefore cannot reach.
/// `outcome` is `Mutex<Option<WizardOutcome>>` on the machine itself.
///
/// Found via the e2e reset probe: `test_awaiting_manual_dns_surface_renders`
/// poisoned every subsequent test in a batched `--app tui` run — 16+ consecutive
/// onboarding tests failing with `create-identity-button` at count=0, never
/// recovering, because the app kept rendering the seeded "Almost ready" surface.
#[tokio::test]
async fn reset_clears_the_wizard_outcome() {
    let fake = Arc::new(FakeNestApi::new());
    let m = seeded(fake);
    assert!(
        m.wizard_outcome().is_some(),
        "precondition: the seed sets an AwaitingManualDns outcome"
    );

    m.reset();

    assert_eq!(
        m.step(),
        OnboardingStep::IdentityChoice,
        "reset returns the machine to the start"
    );
    assert_eq!(
        m.wizard_outcome(),
        None,
        "a factory reset clears the user's PROGRESS — a surviving outcome keeps \
         every app rendering the exit surface it names, forever"
    );
}

// ── the exit ("Use a different nest") ───────────────────────────────────────
//
// `onboarding-provisioning.md` § "Almost ready" surface → *Exit*: the surface's
// one way out for a box that will never answer. It clears the durable slot of
// the identity being onboarded and lands the wizard at `HandleEntry`, holding
// that identity — the landing `launch_retry`'s fallthrough makes via
// `seed_identity`, minus the retired slot.

/// Records every slot the machine asked the store to clear, by the secret it
/// was addressed with — so a test can tell "cleared the onboarded identity's
/// slot" from "cleared some other account's".
#[derive(Default)]
struct ClearRecordingStore(Mutex<Vec<String>>);

impl ClearRecordingStore {
    fn cleared(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl PendingProvisionStore for ClearRecordingStore {
    fn save_awaiting_dns(
        &self,
        _secret_hex: String,
        record: AwaitingDnsRecord,
    ) -> Option<AwaitingDnsRecord> {
        Some(record)
    }

    fn clear_awaiting_dns(&self, secret_hex: String) {
        self.0.lock().unwrap().push(secret_hex);
    }
}

#[tokio::test]
async fn abandon_lands_at_handle_entry_holding_the_identity_and_clears_that_identitys_slot() {
    let store = Arc::new(ClearRecordingStore::default());
    let m = seeded(Arc::new(FakeNestApi::new()));
    m.set_pending_provision_store_for_test(store.clone());

    m.abandon_awaiting_manual_dns();

    assert_eq!(
        m.wizard_outcome(),
        None,
        "the outcome is what every app renders the surface off — it must be gone"
    );
    assert_eq!(m.step(), OnboardingStep::HandleEntry);
    assert_eq!(
        m.effective_secret(),
        Some(fixture_secret_hex()),
        "the identity stays: the user is choosing a different nest, not a different self"
    );
    assert_eq!(
        store.cleared(),
        vec![fixture_secret_hex()],
        "the slot cleared is the onboarded identity's, addressed by its secret — \
         the active account differs on an append run"
    );
}

#[tokio::test]
async fn abandon_is_a_noop_when_the_surface_is_not_showing() {
    let store = Arc::new(ClearRecordingStore::default());
    let m = OnboardingMachine::with_nest_api(
        Arc::new(NullObserver) as Arc<dyn OnboardingObserver>,
        Arc::new(FakeNestApi::new()),
    );
    m.seed_identity(fixture_secret_hex());
    m.set_pending_provision_store_for_test(store.clone());

    m.abandon_awaiting_manual_dns();

    assert_eq!(m.step(), OnboardingStep::HandleEntry);
    assert!(
        store.cleared().is_empty(),
        "with no awaiting outcome there is no slot of ours to clear — a stray call \
         must not retire a resumable box's slot"
    );
}
