//! The **kids-app eligibility verdict** (`family-safety.md` § The account age
//! band → the kids-app bullet, item (3)): the one shared-Rust answer the
//! `kids` build flavor of the android and iOS apps keys sign-in on.
//!
//! Eligibility keys on **supervised**, never on the age band — enforcement
//! never keys on the band (decision D2). An account that is not supervised is
//! refused past sign-in by the flavor's one explanatory surface; a graduated
//! account flips the verdict on its next status read and stays inside the
//! flavor's compiled-in floor and excision, never becoming an open-feed client.
//!
//! The UniFFI face is `fauna_ffi::kids_app_eligible`; the web twin is the
//! `kidsAppEligible` field wasm's `familyStatus` attaches to its reply — both
//! call this function, so no app re-derives the rule.

use fauna_protocol::family::FamilyStatusReply;

/// May this account use the kids app? `true` exactly when the
/// `fauna.family.status` reply names a guardian (`supervised_by`).
pub fn kids_app_eligible(status: &FamilyStatusReply) -> bool {
    status.supervised_by.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::ByteBuf;
    use fauna_protocol::age::AgeBand;
    use fauna_protocol::family::{FamilyAgeBandInfo, FamilyGuardianInfo, ReachPolicy};

    fn guardian() -> FamilyGuardianInfo {
        FamilyGuardianInfo {
            actor_id: ByteBuf::from(vec![0xab, 0xcd]),
            handle: "parent@example.org".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_supervised_account_is_eligible() {
        let status = FamilyStatusReply {
            supervised_by: Some(guardian()),
            ..Default::default()
        };
        assert!(kids_app_eligible(&status));
    }

    #[test]
    fn an_unsupervised_account_is_not_eligible() {
        assert!(!kids_app_eligible(&FamilyStatusReply::default()));
    }

    /// The graduation flip: a reply that still carries a policy document but
    /// names no guardian is a graduated account, and the verdict follows the
    /// guardian, not the leftover policy.
    #[test]
    fn a_graduated_account_with_a_leftover_policy_is_not_eligible() {
        let status = FamilyStatusReply {
            supervised_by: None,
            policy: Some(ReachPolicy::default()),
            ..Default::default()
        };
        assert!(!kids_app_eligible(&status));
    }

    /// Never the band (D2): an under-13 band without a guardian is not
    /// eligible, and a guardian makes an account eligible whatever its band.
    #[test]
    fn the_verdict_never_keys_on_the_age_band() {
        let unsupervised_child = FamilyStatusReply {
            age_band: Some(FamilyAgeBandInfo {
                band: AgeBand::U13.as_str().into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!kids_app_eligible(&unsupervised_child));
        let supervised_adult_band = FamilyStatusReply {
            supervised_by: Some(guardian()),
            age_band: Some(FamilyAgeBandInfo {
                band: AgeBand::Adult.as_str().into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(kids_app_eligible(&supervised_adult_band));
    }
}
