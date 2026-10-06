//! Card-rendering helpers for search results — the snippet cleanup and the
//! `content_type → badge` map — lifted into one canonical shared implementation.
//!
//! These two formatters were duplicated and *divergent* across every app's
//! search page (linux `content_type_badge`/`clean_snippet`, windows
//! `ContentTypeBadge`/`CleanSnippet`, apple `contentTypeBadge`, web
//! `contentTypeLabel`). Per `docs/goal/ui/search.md` § Implementation status
//! today, the canonical badge map is **prefix-aware + nest-accurate** (web's
//! variant was the only nest-accurate one) and returns [`LocalizedText`] so each
//! app localizes the label rather than hard-coding English. Lifting it here —
//! beside the `fauna.search.query` wrapper, the home the goal doc names — lets
//! all seven apps converge and makes the exact-match `post/*` bug disappear,
//! "rather than each app patching its own interim map."
//!
//! Pure + portable (no I/O, no platform APIs, deterministic) so this stays on
//! every target including wasm. When the typed `SearchSnapshot` (Plan 6) lands,
//! the snapshot builder calls these same functions instead of each app.

use fauna_core::localized::LocalizedText;

/// Strip the FTS `<b>` match markers and decode the common HTML entities that
/// the nest's SQLite FTS5 `snippet()`/highlight helper emits, yielding display
/// plaintext. Lifted verbatim from the identical linux `clean_snippet` /
/// windows `CleanSnippet`.
pub fn clean_snippet(raw: &str) -> String {
    raw.replace("<b>", "")
        .replace("</b>", "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
}

/// Map a nest `content_type` to its human-readable badge label.
///
/// Prefix-aware + nest-accurate per `docs/goal/ui/search.md`: the nest's FTS
/// emits `content_type` ∈ {`profile`, `post`, **`post/<subtype>` variants**
/// (`bins/fauna-nest/src/db/fts.rs`, `f.schema LIKE 'post/%'`), and bridge
/// `source` types (`imap`, `email`, `calendar`)}. An unknown type passes
/// through verbatim — its raw string becomes the [`LocalizedText`] key, and
/// [`LocalizedText::resolve`] falls back to the key itself when it is not an
/// i18n key, so the raw type still surfaces.
///
/// Expressed over [`crate::kind::kind_class`] so a row's badge and its merge
/// identity are decided by **one** classification — two spellings of "what is
/// this row" is exactly how the per-app `post/*` bug survived as long as it did.
pub fn content_type_badge(content_type: &str) -> LocalizedText {
    crate::kind::badge_for(content_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_snippet_strips_b_markers_and_decodes_entities() {
        assert_eq!(
            clean_snippet("a <b>hit</b> &amp; more &lt;x&gt; said &quot;hi&quot;"),
            "a hit & more <x> said \"hi\""
        );
    }

    #[test]
    fn clean_snippet_passes_plain_text_through() {
        assert_eq!(
            clean_snippet("plain text, no markup"),
            "plain text, no markup"
        );
    }

    #[test]
    fn badge_post_exact_and_subtype() {
        assert_eq!(content_type_badge("post").key, "search_page.badge_post");
        // The documented latent bug the exact-match clients had: a `post/*`
        // subtype must map to "Post", not render the raw `post/article` string.
        assert_eq!(
            content_type_badge("post/article").key,
            "search_page.badge_post"
        );
    }

    #[test]
    fn badge_profile() {
        assert_eq!(
            content_type_badge("profile").key,
            "search_page.badge_profile"
        );
    }

    #[test]
    fn badge_email_prefixes() {
        // Nest-accurate: bridge `source` mail types are prefix-matched.
        assert_eq!(content_type_badge("imap").key, "search_page.badge_email");
        assert_eq!(content_type_badge("email").key, "search_page.badge_email");
        assert_eq!(
            content_type_badge("email/message").key,
            "search_page.badge_email"
        );
    }

    #[test]
    fn badge_event_calendar() {
        assert_eq!(
            content_type_badge("calendar").key,
            "search_page.badge_event"
        );
    }

    #[test]
    fn badge_unknown_passes_type_through_as_key() {
        // An unknown type becomes its own key; `resolve` then falls back to the
        // raw string (verified below) — preserving the old passthrough behavior.
        assert_eq!(content_type_badge("widget").key, "widget");
    }

    #[test]
    fn badge_resolves_through_a_lookup() {
        let lt = content_type_badge("post");
        let label = lt.resolve(|k| {
            if k == "search_page.badge_post" {
                Some("Post")
            } else {
                None
            }
        });
        assert_eq!(label, "Post");

        // Unknown type resolves to its raw string (no translation entry).
        let raw = content_type_badge("widget").resolve(|_| None::<&str>);
        assert_eq!(raw, "widget");
    }
}
