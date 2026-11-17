//! The Family page's form-validity predicates (`docs/goal/ui/README.md`
//! § Offer only what is possible, rule 4's form case; the ruling is
//! `docs/goal/behavior/family-safety.md` § Errors & edge cases).
//!
//! Each answers [`Possibility`] for one form's submit from the draft typed so
//! far: `Possible`, or `Blocked` with the reason key. The app paints the submit
//! from it, the submit asks the same function and refuses with the same
//! reason, and a form closes only once its save has succeeded.

use fauna_core::identity::ActorId;
use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

/// Blocked reason: the pre-approve-contact box is empty.
pub const CONTACT_ADD_REQUIRED: &str = "family.contact_add_required";
/// Blocked reason: the pre-approve-contact box is not a hex actor id.
pub const CONTACT_ADD_INVALID: &str = "family.contact_add_invalid_actor_id";
/// Blocked reason: the transfer box is empty.
pub const TRANSFER_REQUIRED: &str = "family.transfer_required";
/// Blocked reason: the transfer box is not a hex actor id (its own key — it
/// used to reuse the contact-add string).
pub const TRANSFER_INVALID: &str = "family.transfer_invalid_actor_id";
/// Blocked reason: the screen-time entry cannot be stored; `{reason}` carries
/// the shared rule's own words.
pub const POLICY_SCREEN_TIME_INVALID: &str = "family.policy_screen_time_invalid";

fn actor_id_possibility(input: &str, required: &str, invalid: &str) -> Possibility {
    let input = input.trim();
    if input.is_empty() {
        return Possibility::blocked(LocalizedText::key(required));
    }
    // v1 takes a hex actor id, no handle resolution.
    match ActorId::from_hex(input) {
        Ok(_) => Possibility::Possible,
        Err(_) => Possibility::blocked(LocalizedText::key(invalid)),
    }
}

/// `family-contact-add-button`: a hex actor id to pre-approve.
pub fn contact_add_possibility(input: &str) -> Possibility {
    actor_id_possibility(input, CONTACT_ADD_REQUIRED, CONTACT_ADD_INVALID)
}

/// `family-transfer-button`: a hex actor id of the guardian to transfer to.
pub fn transfer_possibility(input: &str) -> Possibility {
    actor_id_possibility(input, TRANSFER_REQUIRED, TRANSFER_INVALID)
}

/// `family-policy-save-button`: the screen-time cells must parse and validate
/// through the very rules the nest refuses by (`fauna_core::screen_time`).
/// Empty cells are valid — empty means "unset".
pub fn policy_save_possibility(window_start: &str, window_end: &str, daily: &str) -> Possibility {
    match screen_time_policy(window_start, window_end, daily) {
        Ok(_) => Possibility::Possible,
        Err(reason) => Possibility::blocked(LocalizedText::key_arg(
            POLICY_SCREEN_TIME_INVALID,
            "reason",
            reason,
        )),
    }
}

/// The screen-time cells parsed and validated — the one place both the
/// predicate and the policy composition read them.
pub fn screen_time_policy(
    window_start: &str,
    window_end: &str,
    daily: &str,
) -> Result<fauna_core::screen_time::ScreenTimePolicy, &'static str> {
    use fauna_core::screen_time::{parse_daily_minutes, parse_time_of_day};
    let policy = fauna_core::screen_time::ScreenTimePolicy {
        window_start: parse_time_of_day(window_start)?,
        window_end: parse_time_of_day(window_end)?,
        daily_minutes: parse_daily_minutes(daily)?,
    };
    policy.validate()?;
    Ok(policy)
}

/// Whether the Family surface exists for the caller: supervised, guarding a
/// ward, or named on an incoming transfer. The one rule every app's gate
/// folds its status read through (`family-safety.md` § App surface).
pub fn has_family_surface(supervised: bool, wards: usize, incoming_transfers: usize) -> bool {
    supervised || wards > 0 || incoming_transfers > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(p: &Possibility) -> Option<String> {
        p.reason().map(|r| r.key.clone())
    }

    #[test]
    fn an_actor_id_box_needs_a_hex_id() {
        assert_eq!(
            key(&contact_add_possibility("  ")).as_deref(),
            Some(CONTACT_ADD_REQUIRED)
        );
        assert_eq!(
            key(&contact_add_possibility("zz")).as_deref(),
            Some(CONTACT_ADD_INVALID)
        );
        assert!(contact_add_possibility(&"ab".repeat(32)).is_possible());
    }

    #[test]
    fn transfer_has_its_own_reasons() {
        assert_eq!(
            key(&transfer_possibility("")).as_deref(),
            Some(TRANSFER_REQUIRED)
        );
        assert_eq!(
            key(&transfer_possibility("nothex")).as_deref(),
            Some(TRANSFER_INVALID)
        );
        assert!(transfer_possibility(&"cd".repeat(32)).is_possible());
    }

    #[test]
    fn screen_time_empty_is_valid_and_a_half_set_window_is_not() {
        assert!(policy_save_possibility("", "", "").is_possible());
        let p = policy_save_possibility("22:00", "", "");
        assert_eq!(key(&p).as_deref(), Some(POLICY_SCREEN_TIME_INVALID));
        assert!(p.reason().unwrap().args.contains_key("reason"));
        assert!(policy_save_possibility("22:00", "07:00", "90").is_possible());
    }

    #[test]
    fn the_family_surface_exists_for_any_leg() {
        assert!(!has_family_surface(false, 0, 0));
        assert!(has_family_surface(true, 0, 0));
        assert!(has_family_surface(false, 1, 0));
        assert!(has_family_surface(false, 0, 1));
    }
}
