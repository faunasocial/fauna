use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{DnsRecordPlain, OnboardingMachine};
use std::sync::Arc;

#[test]
fn machine_starts_with_no_dns_records() {
    let m: Arc<OnboardingMachine> = OnboardingMachine::new(Arc::new(NullObserver));
    assert!(m.dns_records().is_empty());
}

#[test]
fn dns_records_can_be_seeded_for_test() {
    let m: Arc<OnboardingMachine> = OnboardingMachine::new(Arc::new(NullObserver));
    let recs = vec![DnsRecordPlain {
        record_type: "A".into(),
        name: "@".into(),
        value: "1.2.3.4".into(),
        ttl: 300,
        priority: None,
    }];
    m.set_dns_records_for_test(recs.clone());
    assert_eq!(m.dns_records(), recs);
}

#[tokio::test(flavor = "current_thread")]
async fn deferred_dns_path_populates_dns_records() {
    use fauna_onboarding_machine::{HandleCheckOutcome, OnboardingStep};

    let m: Arc<OnboardingMachine> = OnboardingMachine::new(Arc::new(NullObserver));
    m.set_step_for_test(OnboardingStep::VpsConfig);
    m.set_current_handle("alice@example.test".into());
    m.set_handle_check_outcome_for_test(HandleCheckOutcome::DomainAvailable {
        buyable_via_provider: false,
        price: None,
    });
    m.set_dns_state_for_test(|d| {
        d.set_up_later = true;
        d.selected_provider_id = Some("hetzner".into());
    });
    let recs = vec![DnsRecordPlain {
        record_type: "A".into(),
        name: "@".into(),
        value: "1.2.3.4".into(),
        ttl: 300,
        priority: None,
    }];
    m.set_dns_records_for_test(recs.clone());
    assert_eq!(m.dns_records(), recs);
}
