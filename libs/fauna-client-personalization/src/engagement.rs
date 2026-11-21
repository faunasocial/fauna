//! The Personalization home's engagement-cue controls answer whether they can
//! succeed now (`docs/goal/ui/README.md` § Offer only what is possible, rules
//! 5–7; the ruling is `docs/goal/behavior/engagement-cues.md` § Layer B).
//!
//! Both controls act through the app's live feed manager: the opt-in toggle
//! writes the flipped value of the nest-confirmed `share`, and the erase deletes
//! the sealed rollup and resets the live engine. So neither is possible while
//! the manager is not up (a press would otherwise return with nothing shown),
//! and the toggle is not possible before the `signal_share.status` read
//! resolved — the flip of an unread `false` is a guessed `true`.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::{Possibility, ReadState};

/// The reason both controls give while the feed manager is not up.
pub const FEED_NOT_READY_KEY: &str = "errors.feed_not_ready";

fn manager_answer(manager_ready: bool) -> Possibility {
    if manager_ready {
        Possibility::Possible
    } else {
        Possibility::blocked(LocalizedText::key(FEED_NOT_READY_KEY))
    }
}

/// `personalization-share-signals-toggle`: the manager is up and the opt-in
/// was read (rule 5 — an unread opt-in is never flipped on a guess).
pub fn share_signals_possibility(manager_ready: bool, read: ReadState) -> Possibility {
    manager_answer(manager_ready).and(Possibility::from_read(read))
}

/// `personalization-clear-engagement-data-button`: the manager is up. The
/// erase needs no read — deleting a rollup that holds nothing is harmless.
pub fn clear_engagement_possibility(manager_ready: bool) -> Possibility {
    manager_answer(manager_ready)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(p: Possibility) -> Option<String> {
        p.reason().map(|r| r.key.clone())
    }

    #[test]
    fn the_toggle_waits_for_the_manager_then_the_read() {
        assert_eq!(
            key(share_signals_possibility(false, ReadState::Resolved)),
            Some(FEED_NOT_READY_KEY.into())
        );
        assert_eq!(
            key(share_signals_possibility(true, ReadState::Pending)),
            Some("common.still_loading".into())
        );
        assert_eq!(
            key(share_signals_possibility(true, ReadState::Failed)),
            Some("common.load_failed".into())
        );
        assert!(share_signals_possibility(true, ReadState::Resolved).is_possible());
    }

    #[test]
    fn the_erase_waits_for_the_manager_only() {
        assert_eq!(
            key(clear_engagement_possibility(false)),
            Some(FEED_NOT_READY_KEY.into())
        );
        assert!(clear_engagement_possibility(true).is_possible());
    }
}
