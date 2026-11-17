//! The Folders page's form-validity predicates (`docs/goal/ui/README.md`
//! § Offer only what is possible, rule 4's form case; the ruling is
//! `docs/goal/ui/folders.md` § Errors & edge cases).
//!
//! Each answers [`Possibility`] for one form's submit from the draft typed so
//! far and from whether the app holds what the op needs (a signed-in machine,
//! the identity secret, the live conversations session). The submit is painted
//! from it, asks it again and refuses with the same reason, and the share form
//! closes only once the share has been issued — a missing session never
//! swallows what was typed.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

/// Blocked reason: the follow form's handle box is blank.
pub const FOLLOW_HANDLE_REQUIRED: &str = "folders.follow_handle_required";
/// Blocked reason: the follow form's folder-name box is blank.
pub const FOLLOW_NAME_REQUIRED: &str = "folders.follow_name_required";
/// Blocked reason: the follow cannot run — no signed-in nest or identity yet.
pub const FOLLOW_NOT_READY: &str = "folders.follow_not_ready";
/// Blocked reason: the share form's recipient box is blank.
pub const SHARE_RECIPIENT_REQUIRED: &str = "folders.share_recipient_required";
/// Blocked reason: the share cannot run — no signed-in nest, identity or live
/// conversations session yet.
pub const SHARE_NOT_READY: &str = "folders.share_not_ready";

fn blocked(key: &str) -> Possibility {
    Possibility::blocked(LocalizedText::key(key))
}

/// `folder-follow-confirm`: a handle and a folder name typed, and the app
/// signed in (`ready` = machine, nest and identity secret all held).
pub fn follow_possibility(handle: &str, name: &str, ready: bool) -> Possibility {
    if handle.trim().is_empty() {
        return blocked(FOLLOW_HANDLE_REQUIRED);
    }
    if name.trim().is_empty() {
        return blocked(FOLLOW_NAME_REQUIRED);
    }
    if !ready {
        return blocked(FOLLOW_NOT_READY);
    }
    Possibility::Possible
}

/// `folder-share-confirm`: a recipient typed, and the app able to admit them
/// (`ready` = machine, nest, identity secret and the live conversations
/// session all held).
pub fn share_possibility(recipient: &str, ready: bool) -> Possibility {
    if recipient.trim().is_empty() {
        return blocked(SHARE_RECIPIENT_REQUIRED);
    }
    if !ready {
        return blocked(SHARE_NOT_READY);
    }
    Possibility::Possible
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(p: &Possibility) -> Option<&str> {
        p.reason().map(|r| r.key.as_str())
    }

    #[test]
    fn follow_needs_both_boxes_then_a_signed_in_app() {
        assert_eq!(
            key(&follow_possibility(" ", "x", true)),
            Some(FOLLOW_HANDLE_REQUIRED)
        );
        assert_eq!(
            key(&follow_possibility("a", "", true)),
            Some(FOLLOW_NAME_REQUIRED)
        );
        assert_eq!(
            key(&follow_possibility("a", "x", false)),
            Some(FOLLOW_NOT_READY)
        );
        assert!(follow_possibility("a", "x", true).is_possible());
    }

    #[test]
    fn share_needs_a_recipient_then_a_session() {
        assert_eq!(
            key(&share_possibility("", true)),
            Some(SHARE_RECIPIENT_REQUIRED)
        );
        assert_eq!(key(&share_possibility("bob", false)), Some(SHARE_NOT_READY));
        assert!(share_possibility("bob", true).is_possible());
    }
}
