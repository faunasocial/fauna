//! The Settings → Muted words sub-page — the tier-1 user keyword filter
//! (`docs/goal/architecture/content-moderation-and-ranking.md` § Resolved design
//! decisions Q3, the `muted-keywords` row owner; `docs/goal/behavior/moderation.md`
//! § Muted keywords; the rail slot — after Privacy — is `docs/goal/ui/settings.md`
//! § Navigation model).
//!
//! A person manages their single user-global muted-keyword list here: add a term
//! (`muted-word-input` + `muted-word-add-button`), see the terms (`muted-word-item`
//! rows: `muted-word-text` + `muted-word-remove-button`), empty state
//! (`muted-word-empty`). linux is the reference leg
//! (`apps/fauna-linux/src/settings/muted_words.rs`); tui is the seventh and last
//! client to lift it (priority #1).
//!
//! **No machine, no FFI hop.** Unlike the Mail/Devices/Nests sub-pages (each
//! fronting a per-feature `*Machine`), the muted-keywords seam needs no state
//! machine: `fauna_sync_engine::preference_surfaces::{load_muted_words,
//! save_muted_words}` own the whole round trip (the `fauna.state.moderation` plane read/write, and the
//! normalize-on-write — trim, drop blanks, case-insensitive dedupe) and hand
//! back the page record [`MutedWordsSnapshot`], so no machine abstraction earns
//! its keep. This page holds that record and dispatches those two calls — the
//! `crate::backups` shape (priority #2: no redundant layer for a seam this
//! thin), and linux's identical reasoning. The shared record is also what makes
//! the empty state honest on all 7 apps at once: it carries `loaded` beside the
//! terms, so no app re-derives the loading-vs-empty distinction
//! (`docs/goal/ui/README.md` § *List pages: loading is not empty*).
//!
//! **The list is the collapse's only read path.** `SettingsState::muted_words`
//! holds the authoritative in-memory copy, and `crate::conversations` reads it to
//! decide the `dm-message-muted` collapse. It is loaded once post-auth
//! ([`spawn_muted_words_refresh`], the `spawn_quota_refresh` shape), so a fresh
//! launch collapses correctly without ever visiting this page, and re-loaded on
//! every visit + after every mutation, so an edit shows up in the bubbles
//! immediately. (The FEED collapse does not read it — that one goes through the
//! shared `FeedManager::is_muted`, whose sealed scorers the manager reloads on its
//! own fetch. Two surfaces, two shared seams, no third client-side copy.)

use fauna_client_config::MutedWordsSnapshot;
use fauna_i18n::strings::muted_words as t;
use fauna_ui_ids as ids;

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture};

/// The Muted words sub-page's state — the persisted list plus the one local
/// input buffer.
#[derive(Debug, Clone, Default)]
pub(crate) struct MutedWordsState {
    /// The shared page record — the normalized persisted terms (the render
    /// source AND the list the conversation-bubble collapse matches against)
    /// **and** whether a read has resolved. Held whole rather than destructured
    /// so the two can never drift: `Default` is the unread state, and only a
    /// completed round trip through
    /// [`fauna_sync_engine::preference_surfaces::load_muted_words`]/[`fauna_sync_engine::preference_surfaces::save_muted_words`]
    /// replaces it (`docs/goal/ui/README.md` § *List pages: loading is not
    /// empty*).
    pub(crate) snapshot: MutedWordsSnapshot,
    /// The `muted-word-input` buffer. A local draft committed only on
    /// `muted-word-add-button` (the `new-handle` shape), cleared on success.
    pub(crate) input: String,
    /// Set while a load/mutation round trip is in flight — disables
    /// `muted-word-add-button` so a double-activation can't race two
    /// read-modify-write cycles against the same config (linux's
    /// `add_button.set_sensitive(false)`).
    pub(crate) busy: bool,
}

impl MutedWordsState {
    /// Reset the page-local draft on a fresh visit — the persisted snapshot
    /// survives (its terms back the bubble collapse app-wide, not just this
    /// page, and re-arming its `loaded` bit would repaint a loading state under
    /// rows the user can still see).
    pub(super) fn reset_form(&mut self) {
        self.input.clear();
        self.busy = false;
    }

    /// The persisted entries, terms and weights — what the conversation
    /// collapse matches against.
    pub(crate) fn keywords(&self) -> &[fauna_core::data::MutedKeyword] {
        &self.snapshot.keywords
    }
}

/// The Muted words sub-page's ordered element list.
///
/// The page's `error-message` is registered globally by
/// [`crate::ui::register_frame`] (the `tui-settings`/`logs`/`account`/`privacy`
/// precedent), so it is not painted here.
///
/// **Row scoping.** The shared action reads a row's term with a single-step
/// `scope="muted-word-item[i]"` (`actions/muted_words.py::words`), and the
/// registry resolves that step wherever it sits in a leaf's ancestor path —
/// hence `.within(ids::MUTED_WORD_ITEM, i)` on the two leaves and a FLAT
/// `muted-word-list`/`muted-word-item`. ⚠ The flatness is now *stylistic*, not
/// forced: before the 2026-08-14 descendant ruling (e2e-conventions.md
/// § convention 1) nesting the items under the list container would have put
/// `muted-word-list` first in every leaf's path and made each scoped read
/// resolve to nothing while the page painted perfectly. That trap is gone.
pub(super) fn muted_words_elements(state: &SettingsState) -> Vec<Element> {
    let mw = &state.muted_words;
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        // The page landmark the driver waits on, carrying the sealed-list promise
        // the copy makes ("this list stays on your devices").
        Element::label(ids::MUTED_WORDS, t::DESCRIPTION),
        Element::input(
            ids::MUTED_WORD_INPUT,
            mw.input.clone(),
            Field::Settings(SettingsField::MutedWordInput),
        )
        .labelled(t::INPUT_PLACEHOLDER),
        Element::gesture_button(
            ids::MUTED_WORD_ADD_BUTTON,
            t::ADD,
            !mw.busy,
            Gesture::Settings(Action::AddMutedWord),
        ),
        // The row container. Flat (see the doc comment) — it is the landmark the
        // rows belong to conceptually, not their registry ancestor.
        Element::label(ids::MUTED_WORD_LIST, String::new()),
    ];
    // Three states off one id: rows, a *loaded* empty list, and — painting
    // neither — a page whose read has not resolved. `keywords.is_empty()` alone
    // cannot tell the last two apart, so the empty state takes the shared
    // `loaded` bit as its second painting condition (`docs/goal/ui/README.md`
    // § *List pages: loading is not empty*; no `*-loading` id, by that rule).
    if mw.snapshot.shows_empty_state() {
        els.push(Element::label(ids::MUTED_WORD_EMPTY, t::EMPTY));
    }
    for (i, word) in mw.snapshot.terms().into_iter().enumerate() {
        els.push(Element::label(ids::MUTED_WORD_ITEM, word.clone()));
        els.push(
            Element::label(ids::MUTED_WORD_TEXT, word.clone()).within(ids::MUTED_WORD_ITEM, i),
        );
        els.push(
            Element::gesture_button(
                ids::MUTED_WORD_REMOVE_BUTTON,
                t::REMOVE,
                !mw.busy,
                Gesture::Settings(Action::RemoveMutedWord(word.clone())),
            )
            .within(ids::MUTED_WORD_ITEM, i),
        );
    }
    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );
    els
}

// ── The shared-call sequencing (the only logic here; everything else is shared) ──
//
// Mirrors `crate::backups`'s `config_client` shape: the async side takes only
// `Send` inputs (the `Arc<NestClient>` WS handle + the owner's hex secret), so no
// `&App` ever crosses the spawn boundary.

/// Load the persisted list. Runs post-auth and on every nav to the page —
/// delegates to [`fauna_sync_engine::preference_surfaces::load_muted_words`],
/// the shared surface linux's `settings/muted_words.rs` also
/// calls: the account store's own read, waited for when the page is opened
/// before the runtime is up.
pub(super) async fn load_words(
    store: fauna_sync_engine::account_runtime::SeatAccountStore,
) -> Result<MutedWordsSnapshot, String> {
    fauna_sync_engine::preference_surfaces::load_muted_words(&store)
        .await
        .map_err(fauna_sync_engine::preference_surfaces::plane_failure)
}

/// One page gesture as a **delta** — what the add/remove buttons actually mean,
/// as opposed to the whole-list replacement they used to be encoded as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MutedWordsMutation {
    /// `muted-word-add-button` — add the typed term.
    Add(String),
    /// `muted-word-remove-button` — remove one stored row's term.
    Remove(String),
}

/// Apply one add/remove gesture through the shared delta.
///
/// The delta form is the fix, not a convenience: the old shape sent the PAGE'S
/// list wholesale (`words() + push` / `filter`), so a term another device
/// stored since this page loaded was silently clobbered — a record update
/// cannot know a list replacement meant "add one term". The shared pair
/// applies the delta against the list as the update reads it, so the
/// concurrent term survives.
pub(super) async fn mutate_word(
    store: fauna_sync_engine::account_runtime::SeatAccountStore,
    mutation: MutedWordsMutation,
) -> Result<MutedWordsSnapshot, String> {
    use fauna_sync_engine::preference_surfaces;
    match mutation {
        MutedWordsMutation::Add(term) => preference_surfaces::add_muted_word(&store, &term).await,
        MutedWordsMutation::Remove(term) => {
            preference_surfaces::remove_muted_word(&store, &term).await
        }
    }
    .map_err(preference_surfaces::plane_failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SubPage;

    /// A page whose read has resolved and returned `words`.
    fn state_with(words: &[&str]) -> SettingsState {
        let mut state = state_still_loading();
        state.muted_words.snapshot = MutedWordsSnapshot {
            keywords: words.iter().map(|w| (*w).into()).collect(),
            loaded: true,
        };
        state
    }

    /// A page as it exists between navigation and the first reply — the state
    /// every app used to render as "you haven't muted any words yet".
    fn state_still_loading() -> SettingsState {
        SettingsState {
            sub: SubPage::MutedWords,
            ..Default::default()
        }
    }

    fn ids(els: &[Element]) -> Vec<String> {
        els.iter().map(|e| e.id.clone()).collect()
    }

    /// Every static ui.yaml id on the page renders with no data at all — the
    /// empty state included (`muted-word-empty` is the *read-and-found-nothing*
    /// arm; the still-loading arm is the test below).
    #[test]
    fn the_empty_page_paints_every_static_ui_yaml_id() {
        let els = muted_words_elements(&state_with(&[]));
        let ids = ids(&els);
        for id in [
            "muted-words",
            "muted-word-input",
            "muted-word-add-button",
            "muted-word-list",
            "muted-word-empty",
            "settings-nav-back",
        ] {
            assert!(
                ids.contains(&id.to_string()),
                "missing {id:?}; have {ids:?}"
            );
        }
        assert!(
            !ids.contains(&"muted-word-item".to_string()),
            "an empty list must paint no rows; have {ids:?}"
        );
    }

    /// The deterministic half of the loading-is-not-empty rule, pinned one tier
    /// below the e2e (the labeler/media precedent): a page whose read has not
    /// resolved paints **no** empty state — and no `*-loading` id either, so the
    /// absence beside zero rows is what identifies the third state.
    ///
    /// Removing the `loaded &&` gate turns this red; the e2e sibling
    /// (`test_muted_words_empty_state_marks_a_loaded_page_not_a_loading_one`)
    /// catches the opposite mistake, a gate whose field is never wired.
    #[test]
    fn the_empty_state_does_not_paint_before_the_read_resolves() {
        let state = state_still_loading();
        assert!(
            !state.muted_words.snapshot.loaded,
            "precondition: a freshly navigated page has not read yet"
        );

        let els = muted_words_elements(&state);
        let ids = ids(&els);
        assert!(
            !ids.contains(&"muted-word-empty".to_string()),
            "a page that has not read yet must not claim the list is empty; have {ids:?}"
        );
        assert!(
            !ids.contains(&"muted-word-item".to_string()),
            "and it has no rows either — that is the whole ambiguity"
        );
        assert!(
            ids.contains(&"muted-word-input".to_string()),
            "the page itself still renders while it loads; have {ids:?}"
        );
    }

    /// A populated list drops the empty state and paints one row per term.
    #[test]
    fn a_populated_list_paints_one_row_per_term_and_no_empty_state() {
        let els = muted_words_elements(&state_with(&["spam", "lottery"]));
        let ids = ids(&els);
        assert_eq!(
            ids.iter().filter(|id| *id == "muted-word-item").count(),
            2,
            "one row per term; have {ids:?}"
        );
        assert!(
            !ids.contains(&"muted-word-empty".to_string()),
            "the empty state must not paint alongside rows"
        );
        let texts: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "muted-word-text")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(texts, vec!["spam", "lottery"], "terms render in list order");
    }

    /// The load-bearing scoping contract: each row leaf's path **starts** with
    /// `muted-word-item[i]`, which is what makes the shared action's single-step
    /// `scope="muted-word-item[i]"` read resolve. A flat paint (or one nested
    /// under `muted-word-list`) would leave every scoped read empty while the
    /// page painted perfectly.
    #[test]
    fn row_leaves_are_scoped_within_their_own_indexed_row() {
        let els = muted_words_elements(&state_with(&["spam", "lottery"]));
        for (want_index, want_text) in [(0usize, "spam"), (1, "lottery")] {
            let text = els
                .iter()
                .find(|e| {
                    e.id == "muted-word-text"
                        && e.path == vec![("muted-word-item".to_string(), want_index)]
                })
                .unwrap_or_else(|| panic!("no muted-word-text at row {want_index}"));
            assert_eq!(text.text, want_text);
            assert!(
                els.iter().any(|e| {
                    e.id == "muted-word-remove-button"
                        && e.path == vec![("muted-word-item".to_string(), want_index)]
                }),
                "no muted-word-remove-button at row {want_index}"
            );
        }
        // The container itself is flat — nesting it would prefix every leaf path.
        assert!(
            els.iter()
                .filter(|e| e.id == "muted-word-item")
                .all(|e| e.path.is_empty()),
            "muted-word-item rows must paint flat, not under muted-word-list"
        );
    }

    /// A round trip in flight disables both mutating affordances, so a
    /// double-activation cannot race two read-modify-write cycles.
    #[test]
    fn an_in_flight_round_trip_disables_the_mutating_buttons() {
        let mut state = state_with(&["spam"]);
        state.muted_words.busy = true;
        let els = muted_words_elements(&state);
        for id in ["muted-word-add-button", "muted-word-remove-button"] {
            let el = els.iter().find(|e| e.id == id).unwrap();
            assert!(!el.enabled, "{id} must be disabled while busy");
        }
        // …and re-enabled once it settles.
        state.muted_words.busy = false;
        let els = muted_words_elements(&state);
        for id in ["muted-word-add-button", "muted-word-remove-button"] {
            assert!(els.iter().find(|e| e.id == id).unwrap().enabled);
        }
    }

    /// Removing targets the TERM, not a row index — a list that shifted under an
    /// in-flight mutation must never un-mute the wrong word.
    #[test]
    fn remove_targets_the_term_not_the_row_index() {
        let els = muted_words_elements(&state_with(&["spam", "lottery"]));
        let removes: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "muted-word-remove-button")
            .collect();
        assert!(matches!(
            removes[1].role,
            crate::element::Role::Button(Gesture::Settings(Action::RemoveMutedWord(ref w)))
                if w == "lottery"
        ));
    }

    /// A fresh visit clears the draft but keeps the persisted snapshot — the
    /// bubble collapse reads that list app-wide, so a nav must not blank it, and
    /// re-arming `loaded` would repaint a loading state under rows already on
    /// screen (the monotonicity half of the rule).
    #[test]
    fn reset_form_clears_the_draft_and_keeps_the_persisted_list() {
        let mut mw = MutedWordsState {
            snapshot: MutedWordsSnapshot {
                keywords: vec!["spam".into()],
                loaded: true,
            },
            input: "half-typed".into(),
            busy: true,
        };
        mw.reset_form();
        assert_eq!(mw.snapshot.terms(), vec!["spam".to_string()]);
        assert!(
            mw.snapshot.loaded,
            "a re-visit must not un-read a page that already has rows"
        );
        assert!(mw.input.is_empty());
        assert!(!mw.busy);
    }
}
