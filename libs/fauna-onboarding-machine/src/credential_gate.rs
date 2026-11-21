//! The one answer a credential form's Verify, Connect or sign-in button gives
//! (`docs/goal/ui/README.md` § Offer only what is possible, rules 2 and 6).
//!
//! The wizard's credential forms — `dns_config`, `vps_config`, a provider's
//! dialog on the choose-providers page, and nest_retire's form — each gate one
//! press on the same three facts: a provider is picked, no check of it is
//! already on its way, and every required field it shows is filled. Each used
//! to answer a bare `bool`, so every app painted a disabled button with nothing
//! saying why. The predicates now answer [`Possibility`] through these helpers,
//! naming the act that revives the button; the old `bool` getters stay as
//! `is_possible()` wrappers until no app calls them.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

use crate::state::FieldMetaPlain;

/// A required field the form shows is still empty.
pub(crate) const FILL_REQUIRED: &str = "provisioning.fill_required_fields";
/// The form's own Verify is on its way.
pub(crate) const CHECKING: &str = "provisioning.checking_credentials";
/// The provider dialog's Connect is on its way.
pub(crate) const CONNECTING: &str = "provisioning.connecting";
/// A `hosted-auth` sign-in needs the provider's address typed first.
pub(crate) const BASE_URL_FIRST: &str = "provisioning.hosted_auth.base_url_first";
/// A `hosted-auth` sign-in is already waiting on the browser.
pub(crate) const FINISH_FIRST: &str = "provisioning.hosted_auth.finish_first";

/// The first check that blocks, with its reason key, else possible. Callers
/// list the checks in the order the person meets them on the page.
pub(crate) fn first_blocked(checks: &[(bool, &str)]) -> Possibility {
    checks
        .iter()
        .find(|(blocked, _)| *blocked)
        .map_or(Possibility::Possible, |(_, key)| {
            Possibility::blocked(LocalizedText::key(*key))
        })
}

/// Every required field in `fields` is filled, by `filled(field_id)`.
pub(crate) fn required_filled(fields: &[FieldMetaPlain], filled: impl Fn(&str) -> bool) -> bool {
    fields.iter().all(|f| !f.required || filled(&f.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every reason a credential gate names resolves in the shared string
    /// table — one that does not is painted as its raw key.
    #[test]
    fn every_reason_resolves() {
        for key in [
            FILL_REQUIRED,
            CHECKING,
            CONNECTING,
            BASE_URL_FIRST,
            FINISH_FIRST,
            "onboarding.dns_config.status_pick_provider",
            "onboarding.vps_config.status_pick_provider",
            "common.still_loading",
        ] {
            assert!(fauna_i18n::strings::lookup(key).is_some(), "{key}");
        }
    }

    #[test]
    fn the_first_blocking_check_wins() {
        assert_eq!(
            first_blocked(&[(false, "a"), (false, "b")]),
            Possibility::Possible
        );
        assert_eq!(
            first_blocked(&[(false, "a"), (true, "b"), (true, "c")])
                .reason()
                .map(|r| r.key.as_str()),
            Some("b")
        );
    }
}
