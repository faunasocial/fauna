use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum WizardOutcome {
    LoggedIn {
        nest_url: String,
        handle: String,
    },
    // ⚠ There is deliberately no `InviteSubmitted` variant (retired 2026-08-12,
    // `onboarding.md` § Wizard exit handling). The pending-review journey never
    // exits the wizard: the app stays on `invite_request` and polls until
    // `recheck_invite_status` resolves an approval into `LoggedIn` by itself.
    //
    // The exit had grown five divergent per-app behaviors — quit the app, blank
    // page, fall back to identity-choice, a sessionless shell, a dead-end text
    // screen — and deleting it is what makes the surface un-divergable: the
    // state simply renders the page that already exists. The resume slot is now
    // written at the submit return from
    // `OnboardingMachine::pending_invite_slot()`, which is the only write
    // moment (§ 3 Persistence callouts).
    AwaitingManualDns {
        nest_url: String,
        dns_records: Vec<crate::state::DnsRecordPlain>,
        claim_code: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logged_in_serializes_round_trip() {
        let o = WizardOutcome::LoggedIn {
            nest_url: "https://a.example".into(),
            handle: "alice@a.example".into(),
        };
        let s = serde_json::to_string(&o).unwrap();
        let o2: WizardOutcome = serde_json::from_str(&s).unwrap();
        assert_eq!(o, o2);
    }

    /// The retirement, pinned as a property rather than a comment: the
    /// pending-review journey must not reintroduce a wizard *exit*. Reviving
    /// `InviteSubmitted` (or any sibling that means "submitted, now leave the
    /// page") re-opens the five divergent per-app behaviors this deletion
    /// closed — persist at the submit return via `pending_invite_slot()`
    /// instead.
    #[test]
    fn no_outcome_means_invite_submitted() {
        // A wizard sitting in PendingReview produces NO outcome at all; the
        // page stays put and polls. If a future variant re-encodes that state
        // as an exit, this decode starts succeeding and the test reds.
        let revived = r#"{"InviteSubmitted":{"nest_url":"u","handle":"h","request_id":"r"}}"#;
        assert!(
            serde_json::from_str::<WizardOutcome>(revived).is_err(),
            "`InviteSubmitted` is retired — the pending-review journey never \
             exits the wizard (onboarding.md § Wizard exit handling)"
        );
    }

    #[test]
    fn awaiting_manual_dns_serializes_round_trip() {
        use crate::state::DnsRecordPlain;
        let o = WizardOutcome::AwaitingManualDns {
            nest_url: "u".into(),
            dns_records: vec![DnsRecordPlain {
                record_type: "A".into(),
                name: "@".into(),
                value: "1.2.3.4".into(),
                ttl: 300,
                priority: None,
            }],
            claim_code: "code".into(),
        };
        let s = serde_json::to_string(&o).unwrap();
        let o2: WizardOutcome = serde_json::from_str(&s).unwrap();
        assert_eq!(o, o2);
    }
}
