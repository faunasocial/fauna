//! Cross-kind deterministic keyword-exclusion matching (content-moderation /
//! ranking frame, Q3).
//!
//! [`FilterRule::BodyExcludes`](crate::scoring::FilterRule::BodyExcludes) is a
//! user's transparent, exact, inspectable, revocable word-list — the *parallel
//! deterministic tier-1 path* of the frame
//! (`docs/goal/architecture/content-moderation-and-ranking.md` § Resolved design
//! decisions Q3), unified with the learned scorers only at the *apply* step.
//!
//! This module is the **single canonical definition** of "does a content body
//! match an exclude word-list", so every apply surface runs the *same*
//! deterministic match instead of forking its own (priority #2 — share the eval).
//!
//! **Who consumes it.** The genuine cross-kind gap it exists for is
//! **conversations**: MLS-sealed at rest, so the nest never holds plaintext and
//! the match must run **client-side, post-decrypt**, via this module's
//! `matches_muted_keywords` wrapper (`fauna-ffi`/`fauna-wasm`'s
//! `matches_muted_keywords`/`matchesMutedKeywords`), called per-message by each
//! app's conversations UI over the decrypted body — not inside
//! `ConversationsManager`, which runs the unrelated spam/phishing classify pass
//! (`observe_local_detection`) over the same field. The two *already-shipped*
//! keyword surfaces are
//! deliberately **not** reworked onto this matcher by the leg that introduced it
//! (see § Implementation status of the frame doc):
//! - **Feed** already evaluates `BodyExcludes` server-side as an FTS `MATCH`
//!   subquery over plaintext-mode posts (`bins/fauna-nest/src/db/feeds.rs`,
//!   `schema LIKE 'post/%'`) — a bulk SQL path, tokenized/stemmed, not a per-row
//!   Rust call. Its FTS semantics differ from this matcher's substring semantics;
//!   converging them is a deferred, feed-scoped reconciliation, not required for
//!   the conversation gap.
//! - **Mail** has its own richer keyword-exclusion —
//!   `EmailFilterRule::BodyContains` + `EmailFilterAction::{Discard,FileInto,…}`,
//!   evaluated at the MTA perimeter (`fauna_mail::filter`) — so mail is **not** a
//!   consumer of this matcher. Its `contains_ci` is the semantic precedent this
//!   matcher mirrors (priority #3 — one keyword-match concept).
//!
//! **Semantics.** Case-insensitive **substring** over Unicode-lowercased text
//! (mirroring `fauna_mail::filter::contains_ci`), **OR** across the terms — a
//! body is excluded if it contains *any* term (the same OR the feed `BodyExcludes`
//! SQL uses). This is the "exact / predictable / inspectable" match the frame
//! requires: a term matches iff the body *literally contains* it, with no stemming
//! or tokenization guesswork. Blank / whitespace-only terms are ignored (they
//! would otherwise match everything); an empty list never matches (no rule).

/// Does `body` match the exclude word-list `terms`?
///
/// Returns `true` — meaning the content is **excluded / hidden** — iff `body`
/// contains at least one non-blank term, compared case-insensitively as a
/// substring. An empty (or all-blank) `terms` list is *not a rule* and never
/// matches. See the module docs for the canonical semantics and why conversations
/// are the target consumer.
pub fn body_excludes_matches(terms: &[String], body: &str) -> bool {
    // Lower-case the body once, then test each term as a substring — mirrors
    // `fauna_mail::filter::contains_ci` (`haystack.to_lowercase().contains(needle)`)
    // so the mail perimeter and this matcher share one keyword-match concept.
    let body_lower = body.to_lowercase();
    terms
        .iter()
        .map(|term| term.trim())
        .filter(|term| !term.is_empty())
        .any(|term| body_lower.contains(&term.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_list_never_matches() {
        assert!(!body_excludes_matches(&[], "anything at all"));
    }

    #[test]
    fn all_blank_terms_never_match() {
        // A blank word-list would otherwise substring-match every body.
        assert!(!body_excludes_matches(
            &terms(&["", "   ", "\t"]),
            "any body"
        ));
    }

    #[test]
    fn present_term_matches() {
        assert!(body_excludes_matches(
            &terms(&["spam"]),
            "this is spam, ignore"
        ));
    }

    #[test]
    fn absent_term_does_not_match() {
        assert!(!body_excludes_matches(
            &terms(&["spam"]),
            "a perfectly fine post"
        ));
    }

    #[test]
    fn match_is_case_insensitive() {
        assert!(body_excludes_matches(
            &terms(&["SPAM"]),
            "lowercase spam here"
        ));
        assert!(body_excludes_matches(&terms(&["spam"]), "SHOUTING SPAM"));
    }

    #[test]
    fn match_is_substring_not_word_boundary() {
        // Canonical semantics are "literally contains" — muting "cat" hides
        // "category". This is documented, exact, and predictable.
        assert!(body_excludes_matches(
            &terms(&["cat"]),
            "a long category name"
        ));
    }

    #[test]
    fn or_across_terms_any_match_excludes() {
        let rule = terms(&["foo", "bar", "baz"]);
        assert!(body_excludes_matches(&rule, "mentions bar only"));
        assert!(!body_excludes_matches(&rule, "mentions none of them"));
    }

    #[test]
    fn blank_terms_are_ignored_but_real_terms_still_apply() {
        // A mix of a blank term and a real one matches on the real one only.
        assert!(body_excludes_matches(&terms(&["", "spam"]), "spam body"));
        assert!(!body_excludes_matches(&terms(&["", "spam"]), "clean body"));
    }

    #[test]
    fn leading_trailing_whitespace_in_term_is_trimmed() {
        assert!(body_excludes_matches(
            &terms(&["  spam  "]),
            "contains spam"
        ));
    }

    #[test]
    fn unicode_case_folding() {
        // to_lowercase folds non-ASCII, matching contains_ci's Unicode behaviour.
        assert!(body_excludes_matches(
            &terms(&["café"]),
            "a nice CAFÉ nearby"
        ));
    }
}
