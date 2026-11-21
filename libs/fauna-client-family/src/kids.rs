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

use crate::supervision_snapshot::SupervisionSnapshot;

/// May this account use the kids app? `true` exactly when the
/// `fauna.family.status` reply names a guardian (`supervised_by`).
pub fn kids_app_eligible(status: &FamilyStatusReply) -> bool {
    status.supervised_by.is_some()
}

/// The three-way kids-app answer (`account-age-band.md` § The account age
/// band → *Kids-app eligibility while it is unknown*). *Unknown* shows neither
/// the kids shell nor the refusal: the not-supervised page's frame without its
/// message, sign-out live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KidsAppEligibility {
    /// The kids shell.
    Eligible,
    /// The not-supervised surface, with its message.
    Ineligible,
    /// Not decided yet — no successful read of this run and no snapshot that
    /// names a guardian.
    Unknown,
}

/// Decide the kids-app answer from the run's latest **successful**
/// `fauna.family.status` reply, if any, and the account's persisted
/// supervision snapshot, if any.
///
/// (a) A read decides, through [`kids_app_eligible`]. (b) With no read yet, a
/// snapshot naming a guardian answers eligible, so a supervised child offline
/// is never refused. (c) Otherwise unknown. Clause (d) — a failed read never
/// changes an answer already given — is the caller's: it passes the last
/// *successful* reply, never a failed one. Clause (e) — an account switch
/// starts over — is the caller's too: it passes the incoming account's inputs.
pub fn kids_app_eligibility(
    latest_read: Option<&FamilyStatusReply>,
    snapshot: Option<&SupervisionSnapshot>,
) -> KidsAppEligibility {
    match latest_read {
        Some(status) if kids_app_eligible(status) => KidsAppEligibility::Eligible,
        Some(_) => KidsAppEligibility::Ineligible,
        None if snapshot.is_some_and(SupervisionSnapshot::is_supervised) => {
            KidsAppEligibility::Eligible
        }
        None => KidsAppEligibility::Unknown,
    }
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

    fn supervised_snapshot() -> SupervisionSnapshot {
        SupervisionSnapshot::from_status(&FamilyStatusReply {
            supervised_by: Some(guardian()),
            ..Default::default()
        })
    }

    /// Clause (a): a successful read decides, either way.
    #[test]
    fn a_read_decides() {
        let supervised = FamilyStatusReply {
            supervised_by: Some(guardian()),
            ..Default::default()
        };
        assert_eq!(
            kids_app_eligibility(Some(&supervised), None),
            KidsAppEligibility::Eligible
        );
        assert_eq!(
            kids_app_eligibility(Some(&FamilyStatusReply::default()), None),
            KidsAppEligibility::Ineligible
        );
    }

    /// Clause (a) over (b): the graduation flip — a read that names no
    /// guardian beats a snapshot that still does.
    #[test]
    fn a_read_naming_no_guardian_beats_a_supervised_snapshot() {
        assert_eq!(
            kids_app_eligibility(
                Some(&FamilyStatusReply::default()),
                Some(&supervised_snapshot())
            ),
            KidsAppEligibility::Ineligible
        );
    }

    /// Clause (b): before any read, a supervised snapshot answers eligible.
    #[test]
    fn a_supervised_snapshot_without_a_read_is_eligible() {
        assert_eq!(
            kids_app_eligibility(None, Some(&supervised_snapshot())),
            KidsAppEligibility::Eligible
        );
    }

    /// Clause (c): no read and a snapshot naming no guardian is unknown —
    /// never a refusal on a guess.
    #[test]
    fn a_snapshot_naming_no_guardian_and_no_read_is_unknown() {
        assert_eq!(
            kids_app_eligibility(None, Some(&SupervisionSnapshot::default())),
            KidsAppEligibility::Unknown
        );
    }

    /// Clause (c): no read and no snapshot at all is unknown.
    #[test]
    fn no_read_and_no_snapshot_is_unknown() {
        assert_eq!(
            kids_app_eligibility(None, None),
            KidsAppEligibility::Unknown
        );
    }
}
