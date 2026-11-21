//! Whether the notifications page's two page-wide actions can succeed now
//! (`docs/goal/ui/README.md` § Offer only what is possible, rules 2, 5 and 6;
//! `docs/goal/behavior/notifications.md` § Errors & edge cases).
//!
//! Every app paints *Mark all read* and *Clear all* from these answers and
//! asks them again before sending, so the page and the action never disagree.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::{Possibility, ReadState};

/// *Mark all read*: blocked until the page's first list read has resolved
/// (rule 5: before it the page does not know whether anything is unread),
/// then blocked with `notifications.nothing_unread` while nothing is unread
/// (rule 2: it comes alive when a notification arrives). `unread` is the
/// unread count the app's badge renders (`notifications.md`, the app-global
/// unread count), never a count of the page's own rows.
pub fn mark_all_read_possibility(read: ReadState, unread: usize) -> Possibility {
    Possibility::from_read(read).and(if unread == 0 {
        Possibility::blocked(LocalizedText::key("notifications.nothing_unread"))
    } else {
        Possibility::Possible
    })
}

/// *Clear all*: blocked until the first list read has resolved (rule 5), then
/// blocked with `notifications.nothing_to_clear` while the page holds no row
/// (rule 2: it comes alive when a notification arrives). A clear with no row
/// shown has no `up_to` bound and would reach rows the person never saw
/// (`notifications.md` § Retention rule 2).
pub fn clear_all_possibility(read: ReadState, rows: usize) -> Possibility {
    Possibility::from_read(read).and(if rows == 0 {
        Possibility::blocked(LocalizedText::key("notifications.nothing_to_clear"))
    } else {
        Possibility::Possible
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(key: &str) -> Possibility {
        Possibility::blocked(LocalizedText::key(key))
    }

    #[test]
    fn before_the_first_read_neither_action_is_possible() {
        assert_eq!(
            mark_all_read_possibility(ReadState::Pending, 2),
            blocked("common.still_loading")
        );
        assert_eq!(
            clear_all_possibility(ReadState::Pending, 3),
            blocked("common.still_loading")
        );
        assert_eq!(
            clear_all_possibility(ReadState::Failed, 0),
            blocked("common.load_failed")
        );
    }

    #[test]
    fn once_read_clear_needs_a_row() {
        assert_eq!(
            clear_all_possibility(ReadState::Resolved, 0),
            blocked("notifications.nothing_to_clear")
        );
        assert_eq!(
            clear_all_possibility(ReadState::Resolved, 1),
            Possibility::Possible
        );
    }

    #[test]
    fn once_read_mark_read_needs_something_unread() {
        assert_eq!(
            mark_all_read_possibility(ReadState::Resolved, 0),
            blocked("notifications.nothing_unread")
        );
        assert_eq!(
            mark_all_read_possibility(ReadState::Resolved, 1),
            Possibility::Possible
        );
        // The load state answers first: before the read, zero unread is not
        // yet known to be zero.
        assert_eq!(
            mark_all_read_possibility(ReadState::Pending, 0),
            blocked("common.still_loading")
        );
    }

    #[test]
    fn every_reason_key_resolves() {
        for key in [
            "notifications.nothing_to_clear",
            "notifications.nothing_unread",
            "common.still_loading",
            "common.load_failed",
        ] {
            assert!(fauna_i18n::strings::lookup(key).is_some(), "{key}");
        }
    }
}
