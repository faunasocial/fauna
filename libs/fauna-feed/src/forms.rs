//! The Feed page's form-validity predicates (`docs/goal/ui/README.md`
//! § Offer only what is possible, rule 4's form case; the page's ruling is
//! `docs/goal/ui/feed.md` § Errors & edge cases).
//!
//! Each answers [`Possibility`] for one form's submit from the draft the
//! person has typed: `Possible`, or `Blocked` with the field's reason key.
//! Every app paints its submit from the answer (disabled, the reason within
//! eyeshot), the submit asks the same function first and refuses with the same
//! reason, and the form closes only once its save has succeeded — a failure
//! keeps it open with what was typed (rule 7).

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

fn blocked_unless(ok: bool, key: &str) -> Possibility {
    if ok {
        Possibility::Possible
    } else {
        Possibility::blocked(LocalizedText::key(key))
    }
}

/// `post-submit-button`: a post needs text. The manager's
/// [`submit_post`](crate::manager::FeedManager::submit_post) refuses with the
/// same key as the backstop.
pub fn post_submit_possibility(text: &str) -> Possibility {
    blocked_unless(!text.trim().is_empty(), "feed.compose_empty")
}

/// `feed-reply-submit-button`: a reply needs text.
pub fn reply_submit_possibility(text: &str) -> Possibility {
    blocked_unless(!text.trim().is_empty(), "feed.reply_empty")
}

/// `create-feed`: a custom feed needs a name (its rules are optional — an
/// empty rule set is the "everything" feed).
pub fn create_feed_possibility(name: &str) -> Possibility {
    blocked_unless(!name.trim().is_empty(), "feed.create.name_required")
}

/// `bridge-form-subscribe-button`: a bridge feed needs its bridge chosen and
/// its feed URI; the display name is optional (it defaults to the URI).
pub fn bridge_subscribe_possibility(kind: &str, uri: &str) -> Possibility {
    if kind.trim().is_empty() {
        return Possibility::blocked(LocalizedText::key("feed.bridge_form.kind_required"));
    }
    blocked_unless(!uri.trim().is_empty(), "feed.bridge_form.uri_required")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(p: Possibility) -> Option<String> {
        p.reason().map(|r| r.key.clone())
    }

    #[test]
    fn a_post_and_a_reply_need_text() {
        assert_eq!(
            key(post_submit_possibility(" \n")).as_deref(),
            Some("feed.compose_empty")
        );
        assert!(post_submit_possibility("hi").is_possible());
        assert_eq!(
            key(reply_submit_possibility("")).as_deref(),
            Some("feed.reply_empty")
        );
        assert!(reply_submit_possibility("yes").is_possible());
    }

    #[test]
    fn a_feed_needs_a_name() {
        assert_eq!(
            key(create_feed_possibility("  ")).as_deref(),
            Some("feed.create.name_required")
        );
        assert!(create_feed_possibility("Mine").is_possible());
    }

    #[test]
    fn a_bridge_feed_needs_its_bridge_then_its_uri() {
        assert_eq!(
            key(bridge_subscribe_possibility("", "")).as_deref(),
            Some("feed.bridge_form.kind_required")
        );
        assert_eq!(
            key(bridge_subscribe_possibility("rss", " ")).as_deref(),
            Some("feed.bridge_form.uri_required")
        );
        assert!(bridge_subscribe_possibility("rss", "https://x.example/feed").is_possible());
    }
}
