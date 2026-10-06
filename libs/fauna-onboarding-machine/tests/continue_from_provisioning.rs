use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{OnboardingMachine, OnboardingStep};
use fauna_provisioning::progress::{
    OverallStatus, ProvisionResultPlain, ProvisioningSnapshot, StepStatus,
};
use std::sync::Arc;

fn machine() -> Arc<OnboardingMachine> {
    OnboardingMachine::new(Arc::new(NullObserver))
}

fn succeeded_snapshot(domain: &str, claim: &str) -> ProvisioningSnapshot {
    let mut s = ProvisioningSnapshot::idle();
    s.overall = OverallStatus::Succeeded;
    for st in &mut s.steps {
        st.status = StepStatus::Succeeded;
    }
    s.result = Some(ProvisionResultPlain {
        server_id: "srv-1".into(),
        ipv4: "1.2.3.4".into(),
        domain: domain.into(),
        claim_code: claim.into(),
    });
    s
}

#[test]
fn continue_from_provisioning_refuses_when_not_succeeded() {
    let m = machine();
    m.set_step_for_test(OnboardingStep::NestProvisioning);
    let step = m.continue_from_provisioning();
    assert_eq!(step, OnboardingStep::NestProvisioning);
    assert!(m.wizard_outcome().is_none());
}

/// The standard path lands on `nat_mode_choice`, exactly as a claim-code submit
/// does — never straight to `Done`/`LoggedIn` (`onboarding.md` § 6 *Provisioning
/// = build + claim*, ratified 2026-08-29). The pre-ratification exit emitted
/// `LoggedIn` here, signing the user in to a box nobody had claimed.
#[test]
fn continue_from_provisioning_normal_path_routes_to_nat_mode_choice() {
    let m = machine();
    m.set_current_handle("alice@example.test".into());
    m.set_provisioning_snapshot_for_test(succeeded_snapshot("example.test", "AAAA"));
    m.set_claim_completed_for_test(true);
    let step = m.continue_from_provisioning();
    assert_eq!(step, OnboardingStep::NatModeChoice);
    assert!(
        m.wizard_outcome().is_none(),
        "the § 3b-bis tail owns the exit from here — this page emits no outcome"
    );
    assert_eq!(
        m.nest_url(),
        "https://example.test",
        "the identity URL is the domain, never the reach address the claim dialled"
    );
}

/// `Succeeded` alone is not enough on the standard path: the orchestrator marks
/// the run succeeded and notifies *before* returning, so an app that paints and
/// takes a click in the window before the claiming substep reopens the step would
/// otherwise walk past an unclaimed box.
#[test]
fn continue_from_provisioning_normal_path_refuses_until_the_box_is_claimed() {
    let m = machine();
    m.set_current_handle("alice@example.test".into());
    m.set_step_for_test(OnboardingStep::NestProvisioning);
    m.set_provisioning_snapshot_for_test(succeeded_snapshot("example.test", "AAAA"));
    let step = m.continue_from_provisioning();
    assert_eq!(step, OnboardingStep::NestProvisioning);
    assert!(m.wizard_outcome().is_none());
}

/// The deferred-DNS path is untouched by that gate — and must be: it never claims
/// here (its `Online` step is `Skipped`, no domain to poll yet; the "Almost ready"
/// surface runs the same claim core once the user's records resolve), so requiring
/// a claim would strand it.
#[test]
fn continue_from_provisioning_deferred_path_routes_to_dns_post_instructions() {
    let m = machine();
    m.set_current_handle("alice@example.test".into());
    m.set_dns_state_for_test(|d| d.set_up_later = true);
    m.set_provisioning_snapshot_for_test(succeeded_snapshot("example.test", "AAAA"));
    let step = m.continue_from_provisioning();
    assert_eq!(step, OnboardingStep::DnsPostInstructions);
    assert!(m.wizard_outcome().is_none());
}

#[test]
fn provisioning_button_affordances_track_overall_status() {
    let m = machine();
    let with_overall = |overall: OverallStatus| {
        let mut s = ProvisioningSnapshot::idle();
        s.overall = overall;
        s
    };

    // Idle: nothing actionable yet (the Start CTA is shown via is_idle()).
    m.set_provisioning_snapshot_for_test(with_overall(OverallStatus::Idle));
    assert!(!m.provisioning_in_progress());
    assert!(!m.can_retry_provisioning());
    assert!(!m.can_continue_provisioning());

    // Running: Cancel + ticker, nothing else.
    m.set_provisioning_snapshot_for_test(with_overall(OverallStatus::Running));
    assert!(m.provisioning_in_progress());
    assert!(!m.can_retry_provisioning());
    assert!(!m.can_continue_provisioning());

    // Failed and Cancelled both surface Retry (soft-cancel resumes).
    for overall in [OverallStatus::Failed, OverallStatus::Cancelled] {
        m.set_provisioning_snapshot_for_test(with_overall(overall));
        assert!(!m.provisioning_in_progress());
        assert!(m.can_retry_provisioning());
        assert!(!m.can_continue_provisioning());
    }

    // Succeeded: only Continue enables.
    m.set_provisioning_snapshot_for_test(with_overall(OverallStatus::Succeeded));
    assert!(!m.provisioning_in_progress());
    assert!(!m.can_retry_provisioning());
    assert!(m.can_continue_provisioning());
}
