use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{DnsRecordPlain, OnboardingMachine, OnboardingStep, WizardOutcome};
use fauna_provisioning::progress::{
    OverallStatus, ProvisionResultPlain, ProvisioningSnapshot, StepStatus,
};
use std::sync::Arc;

#[test]
fn emits_awaiting_manual_dns_outcome() {
    let m: Arc<OnboardingMachine> = OnboardingMachine::new(Arc::new(NullObserver));
    m.set_current_handle("alice@example.test".into());
    m.set_nest_url("https://example.test".into());
    let recs = vec![DnsRecordPlain {
        record_type: "A".into(),
        name: "@".into(),
        value: "1.2.3.4".into(),
        ttl: 300,
        priority: None,
    }];
    m.set_dns_records_for_test(recs.clone());
    let mut snap = ProvisioningSnapshot::idle();
    snap.overall = OverallStatus::Succeeded;
    for st in &mut snap.steps {
        st.status = StepStatus::Succeeded;
    }
    snap.result = Some(ProvisionResultPlain {
        server_id: "srv-1".into(),
        ipv4: "1.2.3.4".into(),
        domain: "example.test".into(),
        claim_code: "CLAIM-XYZ".into(),
    });
    m.set_provisioning_snapshot_for_test(snap);

    let step = m.continue_from_dns_post_instructions();
    assert_eq!(step, OnboardingStep::Done);
    match m.wizard_outcome() {
        Some(WizardOutcome::AwaitingManualDns {
            nest_url,
            dns_records,
            claim_code,
        }) => {
            assert_eq!(nest_url, "https://example.test");
            assert_eq!(claim_code, "CLAIM-XYZ");
            assert_eq!(dns_records, recs);
        }
        other => panic!("expected AwaitingManualDns, got {:?}", other),
    }
}

/// `awaiting_dns_records_json()` is what every non-Rust client writes into the
/// awaiting-DNS slot's `dns_records_json`, and `seed_awaiting_manual_dns` is what
/// parses it back on relaunch. Pin the round-trip, and pin the *field names* —
/// serde emits `record_type`, while the UniFFI/WASM bindings expose the same
/// field as `recordType`. A client that serialized the bound type instead would
/// produce JSON that silently deserializes to an empty list, losing the records
/// the user still has to add at their registrar. That failure is invisible until
/// a relaunch, which is exactly why it is pinned here.
#[test]
fn awaiting_dns_records_json_round_trips_through_the_seeder() {
    let m: Arc<OnboardingMachine> = OnboardingMachine::new(Arc::new(NullObserver));
    m.set_current_handle("alice@example.test".into());
    m.set_nest_url("https://example.test".into());
    let recs = vec![
        DnsRecordPlain {
            record_type: "A".into(),
            name: "@".into(),
            value: "1.2.3.4".into(),
            ttl: 300,
            priority: None,
        },
        DnsRecordPlain {
            record_type: "MX".into(),
            name: "@".into(),
            value: "mail.example.test".into(),
            ttl: 3600,
            priority: Some(10),
        },
    ];
    m.set_dns_records_for_test(recs.clone());
    let mut snap = ProvisioningSnapshot::idle();
    snap.overall = OverallStatus::Succeeded;
    for st in &mut snap.steps {
        st.status = StepStatus::Succeeded;
    }
    snap.result = Some(ProvisionResultPlain {
        server_id: "srv-1".into(),
        ipv4: "1.2.3.4".into(),
        domain: "example.test".into(),
        claim_code: "CLAIM-XYZ".into(),
    });
    m.set_provisioning_snapshot_for_test(snap);
    m.continue_from_dns_post_instructions();

    let json = m.awaiting_dns_records_json();
    assert!(
        json.contains("\"record_type\""),
        "the slot's JSON must use serde's field names, not the bindings' \
         camelCase — got {json}"
    );
    assert!(!json.contains("recordType"), "got camelCase in {json}");

    // The relaunch path: a fresh machine seeded from the slot must come back
    // with byte-identical records.
    let m2: Arc<OnboardingMachine> = OnboardingMachine::new(Arc::new(NullObserver));
    let parsed: Vec<DnsRecordPlain> = serde_json::from_str(&json).expect("slot JSON must parse");
    m2.seed_awaiting_manual_dns(
        "https://example.test".into(),
        "alice@example.test".into(),
        parsed,
        "CLAIM-XYZ".into(),
    );
    assert_eq!(m2.awaiting_manual_dns_snapshot().dns_records, recs);
}
