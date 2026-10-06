//! What a **refused switch** paints, once, for every seat.
//!
//! [`crate::AccountRegistry::set_active`] is the point of no return for a
//! switch, so it is where a switch the device cannot complete is refused —
//! with the live session still running (`long-term-store.md` § Multi-account
//! evolution, "Activating refuses an account it cannot launch as"). The refusal
//! is only half the rule, though: a click that does nothing and says nothing is
//! indistinguishable from a broken button, and the user needs to know both that
//! they are still on the identity they were using and what would bring the
//! other one back. Before this module each seat either logged the error and
//! painted nothing (tui) or painted the raw error string (windows).
//!
//! The copy lives beside [`crate::erase_residue`]'s for the same reason that
//! copy is shared at all: seven seats phrasing one refusal seven ways is how a
//! user learns Fauna means different things on different devices.

use fauna_core::localized::LocalizedText;

use crate::AccountError;

/// The device holds no secret it could sign in with for the target account —
/// [`AccountError::NoStoredSecret`]. Names the remedy: bring the identity back
/// with its secret key or recovery kit.
const KEY_NO_SECRET: &str = "settings.switch_refused_no_secret";
/// Any other refusal. Still says the one thing the user must know: they are
/// where they were.
const KEY_REFUSED: &str = "settings.switch_refused";
/// The `{account}` argument both lines carry — the target's display label.
const ARG_ACCOUNT: &str = "account";

/// The line a seat paints when a switch to `account_label` was refused with
/// `err`. Every refusal names the target and says the user is still on the
/// identity they were using, because every refusal left the live session
/// running — that is the whole point of refusing at `set_active`.
///
/// [`AccountError::ConfirmationRequired`] should never reach a paint: a seat
/// runs its re-auth prompt first. If it does, it gets the plain refusal — the
/// honest statement of what happened — rather than a line naming a prompt the
/// user never saw.
pub fn switch_refused_copy(err: &AccountError, account_label: &str) -> LocalizedText {
    let key = match err {
        AccountError::NoStoredSecret(_) => KEY_NO_SECRET,
        _ => KEY_REFUSED,
    };
    LocalizedText::key_arg(key, ARG_ACCOUNT, account_label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_secret_names_the_account_and_the_way_back() {
        let line = switch_refused_copy(&AccountError::NoStoredSecret("aa11".into()), "alice");
        assert_eq!(line.key, KEY_NO_SECRET);
        assert_eq!(
            line.args.get(ARG_ACCOUNT).map(String::as_str),
            Some("alice")
        );
    }

    #[test]
    fn any_other_refusal_still_names_the_account() {
        for err in [
            AccountError::UnknownActor("aa11".into()),
            AccountError::ConfirmationRequired("aa11".into()),
        ] {
            let line = switch_refused_copy(&err, "bob");
            assert_eq!(line.key, KEY_REFUSED, "{err}");
            assert_eq!(line.args.get(ARG_ACCOUNT).map(String::as_str), Some("bob"));
        }
    }

    /// Every key this module can select resolves in the shipped catalog — a
    /// seat must never paint a raw key.
    #[test]
    fn every_key_has_shipped_copy() {
        for key in [KEY_NO_SECRET, KEY_REFUSED] {
            let copy = LocalizedText::key_arg(key, ARG_ACCOUNT, "alice")
                .resolve(fauna_i18n::strings::lookup);
            assert_ne!(copy, key, "{key} has no English copy");
            assert!(
                copy.contains("alice"),
                "{key} must name the account: {copy}"
            );
        }
    }
}
