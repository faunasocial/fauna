//! The one answer every affordance that needs one of the author's OWN tiers
//! gives (`docs/goal/ui/README.md` § Offer only what is possible, rules 2, 4,
//! 5 and 6; the rulings are `docs/goal/ui/folders.md` and
//! `docs/goal/ui/profile.md` § Errors & edge cases).
//!
//! Three controls need a tier to exist: the folder paywall select (there is
//! nothing to paywall to), the claim-code mint (a code entitles a tier) and the
//! payment-provider form (a provider sells a tier — its tier map has nothing to
//! offer). Each used to read "no tier" off an empty list, and an empty list was
//! also what a failed or not-yet-answered read left behind, so the person was
//! told to create a tier they may already have. The read's state is kept apart
//! from what it returned here, once, for all three.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::{Possibility, ReadState};

/// What a tier is needed for — it picks the sentence the blocked answer
/// carries, never whether it is blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnTierUse {
    /// `folder-paywall-tier-select`.
    Paywall,
    /// `subscription-claim-mint-button`.
    ClaimCode,
    /// `subscription-provider-add-button`.
    PaymentProvider,
}

/// Blocked reason: no tier to paywall a folder to (the folder page's hint,
/// read by the cross-app paywall test).
pub const PAYWALL_NEEDS_TIER: &str = "devices.paywall_tier_needs_tier";
/// Blocked reason: no tier for a claim code to entitle.
pub const CLAIM_NEEDS_TIER: &str = "subscriptions.claim_needs_tier";
/// Blocked reason: no tier for a payment provider to sell.
pub const PROVIDER_NEEDS_TIER: &str = "subscriptions.provider_needs_tier";

impl OwnTierUse {
    /// The reason key for "the read answered, and the author owns no tier".
    pub fn needs_tier_key(self) -> &'static str {
        match self {
            OwnTierUse::Paywall => PAYWALL_NEEDS_TIER,
            OwnTierUse::ClaimCode => CLAIM_NEEDS_TIER,
            OwnTierUse::PaymentProvider => PROVIDER_NEEDS_TIER,
        }
    }
}

/// Whether an affordance needing one of the author's own tiers can succeed:
/// blocked "still loading" while the tier read is out, blocked "failed to
/// load" when it failed (rule 5: unknown is not possible, and never painted
/// as "create a tier"), blocked with `need`'s create-a-tier sentence when it
/// answered with none, possible otherwise.
pub fn own_tiers_possibility(
    read: ReadState,
    own_tier_count: usize,
    need: OwnTierUse,
) -> Possibility {
    match read {
        ReadState::Resolved if own_tier_count == 0 => {
            Possibility::blocked(LocalizedText::key(need.needs_tier_key()))
        }
        other => Possibility::from_read(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USES: [OwnTierUse; 3] = [
        OwnTierUse::Paywall,
        OwnTierUse::ClaimCode,
        OwnTierUse::PaymentProvider,
    ];

    fn key(p: &Possibility) -> Option<&str> {
        p.reason().map(|r| r.key.as_str())
    }

    #[test]
    fn a_pending_read_is_loading_never_no_tier() {
        for need in USES {
            let p = own_tiers_possibility(ReadState::Pending, 0, need);
            assert_eq!(key(&p), Some("common.still_loading"));
        }
    }

    #[test]
    fn a_failed_read_says_it_failed_never_no_tier() {
        for need in USES {
            // Even with tiers left over from an earlier read: the page does
            // not know them to be current.
            for count in [0, 2] {
                let p = own_tiers_possibility(ReadState::Failed, count, need);
                assert_eq!(key(&p), Some("common.load_failed"));
            }
        }
    }

    #[test]
    fn an_answered_read_with_no_tier_names_the_step_for_its_use() {
        let answer = |need| own_tiers_possibility(ReadState::Resolved, 0, need);
        assert_eq!(key(&answer(OwnTierUse::Paywall)), Some(PAYWALL_NEEDS_TIER));
        assert_eq!(key(&answer(OwnTierUse::ClaimCode)), Some(CLAIM_NEEDS_TIER));
        assert_eq!(
            key(&answer(OwnTierUse::PaymentProvider)),
            Some(PROVIDER_NEEDS_TIER)
        );
    }

    #[test]
    fn an_answered_read_with_a_tier_is_possible() {
        for need in USES {
            assert!(own_tiers_possibility(ReadState::Resolved, 1, need).is_possible());
        }
    }
}
