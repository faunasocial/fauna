//! The **kind class** — the one vocabulary both search backends are read
//! through, and the single mapping the page's type filter applies to both arms
//! (`docs/goal/ui/search.md` § State & data shape: "one shared mapping applies
//! the page's filter to both arms (the nest `content_type` parameter and the
//! local kind set)").
//!
//! The two backends name the same things differently. Backend 1 (the nest's
//! `content_fts`) emits free-form `content_type` strings — `post`,
//! `post/<subtype>`, `profile`, and bridge source types (`imap`, `email`,
//! `calendar`). Backend 2 (the sealed per-user tantivy index) emits
//! `fauna_index::ContentKind` variants. A class is what a row *is* regardless of
//! which backend found it, so it is what the merge can dedup on
//! ([`crate::SearchManager`]) and what one filter token can select on both
//! sides at once.
//!
//! This crate stays wasm-clean, so the class is spelled here rather than
//! re-using `ContentKind` (whose crate carries tantivy and cannot compile to
//! wasm — `content-index.md` § Where queries run). The native adapter behind
//! [`crate::LocalSearchIndex`] maps between the two.

use fauna_core::localized::LocalizedText;

/// What a search row *is*, independent of which backend produced it.
///
/// The union of what either backend can emit: the eight
/// `fauna_index::ContentKind` variants (the sealed local index), plus
/// [`Profile`](Self::Profile) (nest-only — there is no local profile index),
/// plus [`Other`](Self::Other) for a type this build does not know.
///
/// **Additive by contract.** A newer nest may emit a `content_type` this build
/// has never heard of; it classifies as [`Other`](Self::Other) and still
/// renders, per the additive-everywhere rule
/// (`docs/goal/architecture/version-compatibility.md`). Never match on this
/// enum without an `Other`/`_` arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SearchKindClass {
    Post,
    /// Nest-only: profiles live in the floor-derived `content_fts` corpus and
    /// have no sealed-index counterpart.
    Profile,
    Mail,
    Calendar,
    Conversation,
    Contact,
    File,
    Draft,
    Media,
    /// A `content_type` this build does not recognise — rendered with its raw
    /// type as the badge, never dropped.
    Other,
}

impl SearchKindClass {
    /// Every class the **local** (sealed tantivy) index can hold — the
    /// `fauna_index::ContentKind` set. [`Profile`](Self::Profile) and
    /// [`Other`](Self::Other) are absent by construction: there is no local
    /// profile index, and an unknown class cannot be asked for.
    pub const LOCAL_ALL: &'static [SearchKindClass] = &[
        SearchKindClass::Mail,
        SearchKindClass::Calendar,
        SearchKindClass::Conversation,
        SearchKindClass::Post,
        SearchKindClass::File,
        SearchKindClass::Contact,
        SearchKindClass::Draft,
        SearchKindClass::Media,
    ];
}

/// Classify a `content_type` string from either backend.
///
/// Prefix-aware and nest-accurate, exactly as
/// [`content_type_badge`](crate::render::content_type_badge) has been since the
/// render lift — that function is now expressed *over this one*, so a row's
/// badge and its dedup identity can never disagree about what it is.
pub fn kind_class(content_type: &str) -> SearchKindClass {
    if content_type == "post" || content_type.starts_with("post/") {
        SearchKindClass::Post
    } else if content_type == "profile" {
        SearchKindClass::Profile
    } else if content_type == "mail"
        || content_type.starts_with("imap")
        || content_type.starts_with("email")
    {
        // `mail` is `ContentKind::Mail`'s own name (the local index); `imap` /
        // `email` are the nest's bridge source types for the same thing.
        SearchKindClass::Mail
    } else if content_type.starts_with("calendar") {
        SearchKindClass::Calendar
    } else if content_type == "conversation" {
        SearchKindClass::Conversation
    } else if content_type == "contact" {
        SearchKindClass::Contact
    } else if content_type == "file" {
        SearchKindClass::File
    } else if content_type == "draft" {
        SearchKindClass::Draft
    } else if content_type == "media" {
        SearchKindClass::Media
    } else {
        SearchKindClass::Other
    }
}

/// The badge label for a class, or `None` where the class has no label and the
/// caller should fall back to the raw type string.
///
/// Every known class carries a key: each one is a kind the Search page can
/// surface, and the result card's badge is how a row says what it is
/// (`ui/search.md` § State & data shape). Only [`Other`](SearchKindClass::Other)
/// passes through verbatim — an unknown type has no name to give, so its raw
/// type is the most honest label (the additive-everywhere rule).
pub(crate) fn class_badge_key(class: SearchKindClass) -> Option<&'static str> {
    match class {
        SearchKindClass::Post => Some("search_page.badge_post"),
        SearchKindClass::Profile => Some("search_page.badge_profile"),
        SearchKindClass::Mail => Some("search_page.badge_email"),
        SearchKindClass::Calendar => Some("search_page.badge_event"),
        SearchKindClass::Conversation => Some("search_page.badge_message"),
        SearchKindClass::Contact => Some("search_page.badge_contact"),
        SearchKindClass::File => Some("search_page.badge_file"),
        SearchKindClass::Draft => Some("search_page.badge_draft"),
        SearchKindClass::Media => Some("search_page.badge_media"),
        SearchKindClass::Other => None,
    }
}

/// The `search-type-filter` token meaning "no filter".
///
/// A client-side sentinel, never sent on the wire — every other token is a nest
/// `content_type` verbatim.
pub const TYPE_FILTER_ALL: &str = "all";

/// The page's type-filter tokens, in render order — the `search-type-filter`
/// option set every app offers.
///
/// Shared so the option list, the nest parameter, and the local kind set can
/// never drift apart; before the lift each app kept its own copy of this array.
pub const TYPE_FILTER_OPTIONS: &[&str] = &[TYPE_FILTER_ALL, "post", "imap", "profile"];

/// The filter's **display arm**: what a `search-type-filter` option is called.
///
/// Expressed over [`badge_for`], so an option is labelled with the very
/// `LocalizedText` the rows it selects carry as their badge — the option and its
/// own results can never disagree about what they are called, the same way
/// [`kind_class`] already keeps a row's badge and its merge identity in
/// agreement.
///
/// Additive like every other arm: a token this build has never heard of
/// surfaces its raw self (via `badge_for`'s unknown-type passthrough) rather
/// than being absorbed into the "All" label.
///
/// Shared because it had drifted. Each app used to hand-roll this map as a
/// *closed* match with an `_ => all` fallback — so a token added to
/// [`TYPE_FILTER_OPTIONS`] rendered a second option reading "All" — and
/// windows' copy had already diverged onto a different key set (`common/posts`
/// / `common/contacts` / `common/messages`), labelling `profile` "Contacts" and
/// `imap` "Messages" where the other five apps read "Profile" and "Email".
pub fn type_filter_label(token: &str) -> LocalizedText {
    if token == TYPE_FILTER_ALL {
        LocalizedText::key("search_page.all")
    } else {
        badge_for(token)
    }
}

/// The filter's **nest arm**: the `content_type` parameter to send with
/// `fauna.search.query`, or `None` for an unfiltered query.
pub fn nest_content_type(token: &str) -> Option<String> {
    if token == TYPE_FILTER_ALL {
        None
    } else {
        Some(token.to_string())
    }
}

/// The filter's **local arm**: which classes of the sealed index to query.
///
/// An empty slice means the local arm has nothing to answer for this filter and
/// is skipped entirely — the correct outcome for `profile`, which exists only
/// in the nest's floor-derived corpus. That is a *no rows* answer, never an
/// error (`content-index.md` § Where queries run).
pub fn local_kind_classes(token: &str) -> &'static [SearchKindClass] {
    if token == TYPE_FILTER_ALL {
        return SearchKindClass::LOCAL_ALL;
    }
    match kind_class(token) {
        SearchKindClass::Post => &[SearchKindClass::Post],
        SearchKindClass::Mail => &[SearchKindClass::Mail],
        SearchKindClass::Calendar => &[SearchKindClass::Calendar],
        SearchKindClass::Conversation => &[SearchKindClass::Conversation],
        SearchKindClass::Contact => &[SearchKindClass::Contact],
        SearchKindClass::File => &[SearchKindClass::File],
        SearchKindClass::Draft => &[SearchKindClass::Draft],
        SearchKindClass::Media => &[SearchKindClass::Media],
        // Nest-only or unrecognised: nothing local can match it.
        SearchKindClass::Profile | SearchKindClass::Other => &[],
    }
}

/// The badge for a `content_type`, resolved through the class.
///
/// Kept as a free function here (rather than only in [`crate::render`]) so the
/// snapshot builder has one call that goes type → class → label.
pub(crate) fn badge_for(content_type: &str) -> LocalizedText {
    match class_badge_key(kind_class(content_type)) {
        Some(key) => LocalizedText::key(key),
        // Unknown / not-yet-labelled: the raw type becomes the key, and
        // `LocalizedText::resolve` falls back to the key itself, so the type
        // still surfaces rather than the row rendering blank.
        None => LocalizedText::key(content_type),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nest_content_types_classify() {
        assert_eq!(kind_class("post"), SearchKindClass::Post);
        assert_eq!(kind_class("post/article"), SearchKindClass::Post);
        assert_eq!(kind_class("profile"), SearchKindClass::Profile);
        assert_eq!(kind_class("imap"), SearchKindClass::Mail);
        assert_eq!(kind_class("email/message"), SearchKindClass::Mail);
        assert_eq!(kind_class("calendar"), SearchKindClass::Calendar);
    }

    /// The local index's own `ContentKind::as_str()` names classify to the same
    /// classes as the nest's tokens — that identity is what lets one filter
    /// token drive both arms and one dedup key span both backends.
    #[test]
    fn local_kind_names_classify_to_the_same_classes() {
        assert_eq!(kind_class("mail"), SearchKindClass::Mail);
        assert_eq!(kind_class("conversation"), SearchKindClass::Conversation);
        assert_eq!(kind_class("contact"), SearchKindClass::Contact);
        assert_eq!(kind_class("file"), SearchKindClass::File);
        assert_eq!(kind_class("draft"), SearchKindClass::Draft);
        assert_eq!(kind_class("media"), SearchKindClass::Media);
    }

    /// A `content_type` from a newer nest classifies as `Other` and still
    /// renders — never dropped, never an error (additive-everywhere).
    #[test]
    fn an_unknown_content_type_is_other_and_keeps_its_raw_badge() {
        assert_eq!(kind_class("widget"), SearchKindClass::Other);
        assert_eq!(badge_for("widget").key, "widget");
    }

    #[test]
    fn all_selects_everything_local_and_nothing_on_the_wire() {
        assert_eq!(nest_content_type(TYPE_FILTER_ALL), None);
        assert_eq!(
            local_kind_classes(TYPE_FILTER_ALL),
            SearchKindClass::LOCAL_ALL
        );
    }

    #[test]
    fn a_type_token_narrows_both_arms_together() {
        assert_eq!(nest_content_type("post"), Some("post".to_string()));
        assert_eq!(local_kind_classes("post"), &[SearchKindClass::Post]);

        assert_eq!(nest_content_type("imap"), Some("imap".to_string()));
        assert_eq!(local_kind_classes("imap"), &[SearchKindClass::Mail]);
    }

    /// `profile` exists only in the nest's floor-derived corpus, so the local
    /// arm is skipped rather than asked a question with a guaranteed empty
    /// answer.
    #[test]
    fn profile_narrows_the_nest_arm_and_empties_the_local_one() {
        assert_eq!(nest_content_type("profile"), Some("profile".to_string()));
        assert!(local_kind_classes("profile").is_empty());
    }

    /// Every offered filter token must be one the mapping actually understands
    /// — an option the local arm silently can't read would be a dead filter.
    #[test]
    fn every_offered_filter_token_maps_on_both_arms() {
        for token in TYPE_FILTER_OPTIONS {
            if *token == TYPE_FILTER_ALL {
                continue;
            }
            assert!(
                nest_content_type(token).is_some(),
                "{token} must narrow the nest arm"
            );
            assert_ne!(
                kind_class(token),
                SearchKindClass::Other,
                "{token} must classify to a known class"
            );
        }
    }

    /// A filter **option** is labelled with the very `LocalizedText` the rows it
    /// selects carry as their **badge** — one map, so an option and its own
    /// results can never disagree about what they are called.
    ///
    /// Before the lift every app kept its own closed match here, and windows'
    /// had already drifted to a different key set entirely (`common/posts` /
    /// `common/contacts` / `common/messages`), so its `profile` option read
    /// "Contacts" and its `imap` option read "Messages" while the other five
    /// apps read "Profile" and "Email".
    #[test]
    fn a_filter_option_is_labelled_with_the_badge_its_rows_carry() {
        for token in TYPE_FILTER_OPTIONS {
            if *token == TYPE_FILTER_ALL {
                continue;
            }
            assert_eq!(
                type_filter_label(token).key,
                badge_for(token).key,
                "{token}'s filter label must be the badge its rows carry"
            );
        }
    }

    /// Only the sentinel reads "All". The per-app matches this replaces used
    /// `_ => all` as their fallback, so any token they had not been taught
    /// rendered a *second* option reading "All".
    #[test]
    fn the_all_sentinel_is_the_only_option_labelled_all() {
        assert_eq!(type_filter_label(TYPE_FILTER_ALL).key, "search_page.all");
        for token in TYPE_FILTER_OPTIONS {
            if *token == TYPE_FILTER_ALL {
                continue;
            }
            assert_ne!(
                type_filter_label(token).key,
                "search_page.all",
                "{token} must not render as a second All option"
            );
        }
    }

    /// The additive-everywhere arm: a token from a newer build surfaces its raw
    /// self, exactly as an unknown row badge does — never silently absorbed
    /// into "All" (`version-compatibility.md`).
    #[test]
    fn an_unknown_token_keeps_its_raw_label_instead_of_reading_all() {
        assert_eq!(type_filter_label("widget").key, "widget");
    }

    /// Every class a row can **be** says what it is in words — the result card's
    /// "type badge" (`ui/search.md` § State & data shape). Only `Other` keeps
    /// the raw passthrough, because only an unknown type has no name to give.
    ///
    /// The local-index classes used to pass through raw "until they have rows to
    /// label"; every local arm has shipped since, so a draft hit painted the
    /// wire token `draft` — a key that resolves nowhere — on every app. Checked
    /// against the real string table, not just for `Some`: a key the table
    /// lacks falls back to itself and paints `search_page.badge_…` instead.
    #[test]
    fn every_known_class_carries_a_badge_the_string_table_resolves() {
        let known = SearchKindClass::LOCAL_ALL
            .iter()
            .chain(std::iter::once(&SearchKindClass::Profile));
        for class in known {
            let key = class_badge_key(*class)
                .unwrap_or_else(|| panic!("{class:?} rows must carry a labelled badge"));
            assert!(
                fauna_i18n::strings::lookup(key).is_some(),
                "{class:?}'s badge key {key} must exist in i18n/strings/en.yaml"
            );
        }
        assert_eq!(class_badge_key(SearchKindClass::Other), None);
    }
}
