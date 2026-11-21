//! The trained-topic forms answer whether their submit can succeed now
//! (`docs/goal/ui/README.md` § Offer only what is possible, rules 2, 4 and 7;
//! the ruling is `docs/goal/ui/settings.md` § Errors & edge cases).
//!
//! Each answer paints its button and is asked again by the press, so a blank
//! name, an empty prune or a missing feed manager is a disabled control with
//! its reason, never a press that does nothing.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;
use fauna_core::scoring::MAX_LABELER_LIST_NAME_LEN;

/// A blank create or rename (`TrainedTopicsError::BlankName`'s sentence).
pub const FACTOR_NAME_BLANK: &str = "personalization.trained_factor_blank_name";
/// A blank public name on the publish sheet.
pub const PUBLISH_NAME_BLANK: &str = "personalization.publish_name_blank";
/// A public name over [`MAX_LABELER_LIST_NAME_LEN`] characters.
pub const PUBLISH_NAME_TOO_LONG: &str = "personalization.publish_name_too_long";
/// Every review item unticked, or nothing scored.
pub const PUBLISH_NOTHING_KEPT: &str = "personalization.publish_nothing_kept";

fn in_flight(busy: bool) -> Possibility {
    if busy {
        Possibility::blocked(LocalizedText::key("common.in_flight"))
    } else {
        Possibility::Possible
    }
}

/// `personalization-trained-factor-create-button` (create and save-name): a
/// name, and no change in flight.
pub fn factor_name_possibility(name: &str, busy: bool) -> Possibility {
    if name.trim().is_empty() {
        return Possibility::blocked(LocalizedText::key(FACTOR_NAME_BLANK));
    }
    in_flight(busy)
}

/// `personalization-trained-factor-publish-button` on one row: absent for a
/// row with no addressable model (a registry id that names no `topic:<hex>`
/// key can never be scored, rule 2); blocked while the live feed manager —
/// whose loaded window is the corpus — is not up, and while a change is in
/// flight.
pub fn publish_open_possibility(
    has_factor_key: bool,
    manager_ready: bool,
    busy: bool,
) -> Possibility {
    if !has_factor_key {
        return Possibility::Absent;
    }
    crate::engagement::clear_engagement_possibility(manager_ready).and(in_flight(busy))
}

/// `personalization-trained-factor-publish-submit-button`: a public name
/// within the bound, at least one kept item, and no publish in flight.
pub fn publish_submit_possibility(name: &str, any_kept: bool, busy: bool) -> Possibility {
    let name = name.trim();
    if name.is_empty() {
        return Possibility::blocked(LocalizedText::key(PUBLISH_NAME_BLANK));
    }
    if name.chars().count() > MAX_LABELER_LIST_NAME_LEN {
        return Possibility::blocked(LocalizedText::key_arg(
            PUBLISH_NAME_TOO_LONG,
            "max",
            MAX_LABELER_LIST_NAME_LEN.to_string(),
        ));
    }
    if !any_kept {
        return Possibility::blocked(LocalizedText::key(PUBLISH_NOTHING_KEPT));
    }
    in_flight(busy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(p: Possibility) -> Option<String> {
        p.reason().map(|r| r.key.clone())
    }

    #[test]
    fn a_factor_needs_a_name() {
        assert_eq!(
            key(factor_name_possibility(" ", false)),
            Some(FACTOR_NAME_BLANK.into())
        );
        assert_eq!(
            key(factor_name_possibility("cats", true)),
            Some("common.in_flight".into())
        );
        assert!(factor_name_possibility("cats", false).is_possible());
    }

    #[test]
    fn publish_opens_only_onto_a_model_and_a_live_window() {
        assert_eq!(
            publish_open_possibility(false, true, false),
            Possibility::Absent
        );
        assert_eq!(
            key(publish_open_possibility(true, false, false)),
            Some(crate::engagement::FEED_NOT_READY_KEY.into())
        );
        assert!(publish_open_possibility(true, true, false).is_possible());
    }

    #[test]
    fn publish_needs_a_bounded_name_and_something_kept() {
        assert_eq!(
            key(publish_submit_possibility("", true, false)),
            Some(PUBLISH_NAME_BLANK.into())
        );
        let long = "x".repeat(MAX_LABELER_LIST_NAME_LEN + 1);
        assert_eq!(
            key(publish_submit_possibility(&long, true, false)),
            Some(PUBLISH_NAME_TOO_LONG.into())
        );
        assert_eq!(
            key(publish_submit_possibility("Cats", false, false)),
            Some(PUBLISH_NOTHING_KEPT.into())
        );
        assert!(publish_submit_possibility("Cats", true, false).is_possible());
    }
}
