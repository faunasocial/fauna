//! The subscription forms' draft-validity predicates (`docs/goal/ui/README.md`
//! § Offer only what is possible, rule 4's form case; the ruling is
//! `docs/goal/ui/profile.md` § Errors & edge cases).
//!
//! Each answers [`Possibility`] from what has been typed: the submit is
//! painted from it, asks it again and refuses with the same reason.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

/// Blocked reason: the tier form's name is blank.
pub const TIER_NAME_REQUIRED: &str = "subscriptions.tier_name_required";
/// Blocked reason: the claim-code box is blank.
pub const REDEEM_CODE_REQUIRED: &str = "subscriptions.redeem_code_required";

fn blocked_unless(ok: bool, key: &str) -> Possibility {
    if ok {
        Possibility::Possible
    } else {
        Possibility::blocked(LocalizedText::key(key))
    }
}

/// `subscription-tier-form-save`: a tier needs a name (it is the tier's key on
/// the nest). Every other cell is optional.
pub fn tier_form_possibility(name: &str) -> Possibility {
    blocked_unless(!name.trim().is_empty(), TIER_NAME_REQUIRED)
}

/// `subscription-claim-redeem-button`: a code to redeem. Folded with the
/// payments feature gate by the caller (`Possibility::and`).
pub fn redeem_code_possibility(code: &str) -> Possibility {
    blocked_unless(!code.trim().is_empty(), REDEEM_CODE_REQUIRED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tier_needs_a_name() {
        let p = tier_form_possibility("  ");
        assert_eq!(p.reason().unwrap().key, TIER_NAME_REQUIRED);
        assert!(tier_form_possibility("Gold").is_possible());
    }

    #[test]
    fn a_redeem_needs_a_code() {
        let p = redeem_code_possibility("");
        assert_eq!(p.reason().unwrap().key, REDEEM_CODE_REQUIRED);
        assert!(redeem_code_possibility("ABC-123").is_possible());
    }
}
