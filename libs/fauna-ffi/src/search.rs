//! UniFFI façade for the shared search-result card formatters
//! ([`fauna_client_search::render::content_type_badge`] +
//! [`fauna_client_search::render::clean_snippet`]).
//!
//! The Rust-native Linux app calls `fauna_client_search::render::*`
//! directly; these exports give Apple / Windows / Android the identical,
//! **prefix-aware + nest-accurate** badge map and FTS snippet cleanup so no
//! client re-derives them per platform (priority #1/#2 — the search analog of
//! [`crate::handle::validate_handle`] / `spam::spam_threshold_band`). The badge
//! returns a [`LocalizedText`] (already a registered `uniffi::Record` via the
//! embedded onboarding/feed surfaces); clients resolve its `key` through their
//! own i18n pipeline. See `docs/goal/ui/search.md` § Implementation status today.

use fauna_core::localized::LocalizedText;

/// UniFFI face of [`fauna_client_search::render::content_type_badge`] — maps a
/// nest `content_type` (incl. `post/<subtype>` variants, bridge `source` types)
/// to its localized badge label. Resolve the returned `LocalizedText.key` through
/// the client's i18n pipeline; an unknown type passes through as its own key.
//
// NB: a `///` doc on a UniFFI-exported item must not contain a slash-star
// comment-open (it used to read "post slash-star"). UniFFI renders the doc into a
// Kotlin KDoc, and Kotlin block comments NEST, so a bare comment-open with no
// matching close swallows the rest of the generated file ("Unclosed comment" at
// EOF, breaking every android binding compile). Phrase wildcards as `<subtype>`.
#[uniffi::export]
pub fn search_content_type_badge(content_type: String) -> LocalizedText {
    fauna_client_search::render::content_type_badge(&content_type)
}

/// UniFFI face of [`fauna_client_search::render::clean_snippet`] — strips the
/// FTS `<b>` match markers and decodes the common HTML entities, yielding
/// display plaintext.
#[uniffi::export]
pub fn search_clean_snippet(raw: String) -> String {
    fauna_client_search::render::clean_snippet(&raw)
}

// ── Paging policy ──────────────────────────────────────────────────────
//
// UniFFI faces of `fauna_client_search::paging::SearchPaging`. Exported as
// three free functions over the limit itself rather than as an object, because
// every app already stores the limit in its own view-model
// (`SearchViewModel._limit`, `SearchVM.currentLimit`, …) — these let it keep
// that field while the *policy* over it comes from one place. The shared unit
// tests live with the implementation (`paging.rs`); these faces only pin
// delegation.
//
// See `docs/goal/ui/search.md` § Where logic lives.

/// UniFFI face of `SearchPaging::initial().limit()` — the page size a fresh
/// query asks for, and what a submit / type-filter change / cancel resets to.
#[uniffi::export]
pub fn search_paging_initial_limit() -> i64 {
    fauna_client_search::paging::SearchPaging::initial().limit()
}

/// UniFFI face of `SearchPaging::load_more` — the page size to request after a
/// "load more" click, given the current one. Saturates at the nest's page
/// ceiling, so a client can never ask for a page the nest silently truncates.
#[uniffi::export]
pub fn search_paging_load_more(current_limit: i64) -> i64 {
    let mut p = fauna_client_search::paging::SearchPaging::at_limit(current_limit);
    p.load_more();
    p.limit()
}

/// UniFFI face of `SearchPaging::has_more` — whether `search-load-more-button`
/// should be shown, given the current page limit and how many rows came back.
/// False for a partial page (there are no more rows) AND at the nest's ceiling
/// (re-firing would return the same rows).
#[uniffi::export]
pub fn search_paging_has_more(current_limit: i64, result_count: u32) -> bool {
    fauna_client_search::paging::SearchPaging::at_limit(current_limit)
        .has_more(result_count as usize)
}

/// UniFFI face of `fauna_client_search::kind::TYPE_FILTER_OPTIONS` — the
/// `search-type-filter` option tokens, in render order. `"all"`
/// (`TYPE_FILTER_ALL`) is the client-side sentinel meaning "no filter"; every
/// other token is a nest `content_type` verbatim. Shared so a native picker
/// can never drift from the mapping `SearchManager` applies to both search
/// backends. The wasm twin is `search_type_filter_options` in `fauna-wasm`.
///
/// Plain `Vec<String>`, so — unlike [`crate::search_manager::FfiSearchManager`]
/// — this needs no `search-manager` feature gate: it carries no
/// `fauna_client_search`-owned uniffi type across the boundary.
#[uniffi::export]
pub fn search_type_filter_options() -> Vec<String> {
    fauna_client_search::TYPE_FILTER_OPTIONS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// UniFFI face of [`fauna_client_search::type_filter_label`] — what a
/// `search-type-filter` option is *called*, as a [`LocalizedText`] the client
/// resolves through its own i18n pipeline.
///
/// The companion to [`search_type_filter_options`]: that face says which tokens
/// exist, this one says what each is named. An option is labelled with the very
/// badge its rows carry, so a picker and its own results agree by construction.
/// The wasm twin is `searchTypeFilterLabel` in `fauna-wasm`.
#[uniffi::export]
pub fn search_type_filter_label(token: String) -> LocalizedText {
    fauna_client_search::type_filter_label(&token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_delegates_and_returns_localized_key() {
        // post/* subtype -> the canonical "Post" key (the bug the exact-match
        // per-app maps had); unknown -> raw passthrough as the key.
        assert_eq!(
            search_content_type_badge("post/article".into()).key,
            "search_page.badge_post"
        );
        assert_eq!(search_content_type_badge("widget".into()).key, "widget");
    }

    #[test]
    fn snippet_delegates_and_cleans() {
        assert_eq!(
            search_clean_snippet("a <b>hit</b> &amp; more".into()),
            "a hit & more"
        );
    }

    /// Pins that every offered option's label comes back as the badge its rows
    /// carry, through the face — the native pickers that used to hand-roll this
    /// map (windows' had drifted to `common/*` keys, reading "Contacts" for
    /// `profile`) now get one answer.
    #[test]
    fn type_filter_labels_delegate_and_match_the_row_badge() {
        assert_eq!(
            search_type_filter_label("all".into()).key,
            "search_page.all"
        );
        for token in fauna_client_search::TYPE_FILTER_OPTIONS {
            if *token == fauna_client_search::TYPE_FILTER_ALL {
                continue;
            }
            assert_eq!(
                search_type_filter_label(token.to_string()).key,
                search_content_type_badge(token.to_string()).key,
                "{token}'s option label must be its rows' badge"
            );
        }
        // Additive: an unknown token surfaces raw, never as a second "All".
        assert_eq!(search_type_filter_label("widget".into()).key, "widget");
    }

    /// Pins that the face delegates to the shared token list rather than a
    /// per-app copy — a native picker built off this can never drift from
    /// what `SearchManager` maps onto both backends.
    #[test]
    fn type_filter_options_delegate_to_the_shared_list() {
        assert_eq!(
            search_type_filter_options(),
            fauna_client_search::TYPE_FILTER_OPTIONS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }

    /// Pins that the faces delegate rather than re-deriving the policy — an
    /// apple/android/windows leg reading these gets the same answers the tui
    /// and linux legs compute in-process.
    #[test]
    fn paging_faces_delegate_to_the_shared_policy() {
        use fauna_client_search::paging::SearchPaging;

        let initial = search_paging_initial_limit();
        assert_eq!(initial, SearchPaging::initial().limit());

        let grown = search_paging_load_more(initial);
        assert!(grown > initial);
        assert!(search_paging_has_more(initial, initial as u32));
        assert!(!search_paging_has_more(initial, initial as u32 - 1));
    }

    /// The dead-click case as the four remaining app legs will meet it: at the
    /// nest's ceiling, "load more" stops growing and stops being offered.
    #[test]
    fn paging_faces_stop_at_the_nest_ceiling() {
        let max = fauna_client_search::search::MAX_LIMIT;
        let at_cap = search_paging_load_more(search_paging_initial_limit());
        assert_eq!(at_cap, max);
        assert_eq!(search_paging_load_more(at_cap), max, "growth saturates");
        assert!(
            !search_paging_has_more(at_cap, max as u32),
            "a full page at the ceiling offers nothing further"
        );
    }
}
