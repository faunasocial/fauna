//! Whether `save-spam-prefs` can do anything now — the shared predicate
//! every app paints the button from and the save asks before it sends
//! (`docs/goal/ui/README.md` § Offer only what is possible, rules 4 and 6;
//! `docs/goal/ui/settings.md`, the spam-moderation row).
//!
//! The two threshold inputs are prefilled from the nest's own preferences, so
//! a Save pressed before either is edited would send back exactly what the
//! nest holds: the echo folds identical values in and nothing on the page
//! moves — a press indistinguishable from a dropped one (rule 7). So Save is
//! offered only while a draft differs from the saved value, and blocked with
//! [`NOTHING_TO_SAVE`] otherwise. The comparison is on the wire's per-mille,
//! the one value the nest stores, so `"0.70"` and `"0.7"` are the same draft.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::{Possibility, ReadState};
use fauna_protocol::spam::{SpamPreferences, probability_to_per_mille};

/// The reason a Save with nothing changed is blocked.
pub const NOTHING_TO_SAVE: &str = "common.nothing_to_save";

/// A threshold draft as the wire's per-mille, or `None` when it does not
/// parse as a probability (the save then reports the parse error itself).
pub fn draft_per_mille(draft: &str) -> Option<u16> {
    draft
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|p| p.is_finite())
        .map(probability_to_per_mille)
}

/// The answer for `save-spam-prefs`: the read that prefilled the inputs first
/// (rule 5 — never a save over values not yet known), then blocked
/// [`NOTHING_TO_SAVE`] while both drafts equal the `saved` preferences.
///
/// A draft that does not parse stays *possible*: the press then names the bad
/// threshold on the page's `error-message`, which a disabled button could not.
pub fn save_preferences_possibility(
    read: ReadState,
    saved: Option<&SpamPreferences>,
    spam_draft: &str,
    phishing_draft: &str,
) -> Possibility {
    Possibility::from_read(read).and(match saved {
        Some(saved)
            if draft_per_mille(spam_draft) == Some(saved.spam_threshold)
                && draft_per_mille(phishing_draft) == Some(saved.phishing_threshold) =>
        {
            Possibility::blocked(LocalizedText::key(NOTHING_TO_SAVE))
        }
        _ => Possibility::Possible,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(spam: u16, phishing: u16) -> SpamPreferences {
        SpamPreferences {
            spam_threshold: spam,
            phishing_threshold: phishing,
            extra: Default::default(),
        }
    }

    fn key(p: &Possibility) -> Option<String> {
        p.reason().map(|r| r.key.clone())
    }

    #[test]
    fn unchanged_drafts_have_nothing_to_save() {
        let saved = prefs(700, 400);
        let p = save_preferences_possibility(ReadState::Resolved, Some(&saved), "0.7", "0.4");
        assert_eq!(key(&p).as_deref(), Some(NOTHING_TO_SAVE));
        // The same value spelled differently is still unchanged.
        let p = save_preferences_possibility(ReadState::Resolved, Some(&saved), " 0.70", "0.400");
        assert_eq!(key(&p).as_deref(), Some(NOTHING_TO_SAVE));
    }

    #[test]
    fn an_edited_threshold_can_be_saved() {
        let saved = prefs(700, 400);
        for (spam, phishing) in [("0.8", "0.4"), ("0.7", "0.3")] {
            let p = save_preferences_possibility(ReadState::Resolved, Some(&saved), spam, phishing);
            assert!(
                p.is_possible(),
                "{spam}/{phishing} differs from the saved pair"
            );
        }
    }

    #[test]
    fn an_unparseable_draft_stays_possible_so_the_save_names_it() {
        let saved = prefs(700, 400);
        let p = save_preferences_possibility(ReadState::Resolved, Some(&saved), "abc", "0.4");
        assert!(p.is_possible());
        assert_eq!(draft_per_mille("abc"), None);
        assert_eq!(draft_per_mille("NaN"), None);
    }

    #[test]
    fn the_read_decides_first() {
        let p = save_preferences_possibility(ReadState::Pending, None, "0.8", "0.4");
        assert_eq!(key(&p).as_deref(), Some("common.still_loading"));
        let p = save_preferences_possibility(ReadState::Failed, None, "0.8", "0.4");
        assert_eq!(key(&p).as_deref(), Some("common.load_failed"));
    }
}
