//! What a **refused account add** paints, for every seat.
//!
//! The account index is bounded against one credential item
//! (`long-term-store.md` § Multi-account evolution → *The index is bounded*):
//! an add that would push it past [`crate::MAX_INDEX_VALUE_BYTES`] is refused
//! with [`AccountError::IndexFull`] and writes nothing. That refusal is the one
//! add failure the user can act on — remove an account this device no longer
//! uses — so it gets its own translated line instead of the error's English
//! `Display` inside a generic "couldn't save your account" wrapper. Every other
//! add failure is a fault the user cannot fix from the add, and keeps the seat's
//! wrapper.
//!
//! Shared for the reason [`crate::switch_refused_copy`] is: seven seats
//! phrasing one refusal seven ways is how a user learns Fauna means different
//! things on different devices.

use fauna_core::localized::LocalizedText;

use crate::AccountError;

/// The device's account list is full. Names the remedy: remove an account.
const KEY_LIST_FULL: &str = "settings.account_list_full";

/// The line a seat paints when adding an account was refused with `err`, or
/// `None` when the refusal is not one the user can act on — the seat then
/// keeps its own wrapper around the error.
pub fn add_refused_copy(err: &AccountError) -> Option<LocalizedText> {
    match err {
        AccountError::IndexFull { .. } => Some(LocalizedText::key(KEY_LIST_FULL)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_index_gets_the_list_full_line() {
        let line = add_refused_copy(&AccountError::IndexFull {
            needed: 2600,
            limit: 2560,
        })
        .expect("a full list is a refusal the user can act on");
        assert_eq!(line.key, KEY_LIST_FULL);
    }

    #[test]
    fn any_other_refusal_keeps_the_seats_wrapper() {
        for err in [
            AccountError::UnknownActor("aa11".into()),
            AccountError::NoStoredSecret("aa11".into()),
        ] {
            assert!(add_refused_copy(&err).is_none(), "{err}");
        }
    }

    /// The key resolves in the shipped catalog — a seat must never paint a
    /// raw key.
    #[test]
    fn the_line_has_shipped_copy() {
        let copy = LocalizedText::key(KEY_LIST_FULL).resolve(fauna_i18n::strings::lookup);
        assert_ne!(copy, KEY_LIST_FULL, "{KEY_LIST_FULL} has no English copy");
    }
}
