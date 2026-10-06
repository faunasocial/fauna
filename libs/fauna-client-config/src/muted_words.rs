//! The **muted-words page read** — the one shared place the `muted-words`
//! Settings sub-page's rows are assigned, and therefore the one place that can
//! honestly say whether they have been read yet.
//!
//! Authority: `docs/goal/ui/README.md` § *List pages: loading is not empty*
//! (the three-state rule + why the bit is shared, not per-app) and
//! `docs/goal/architecture/content-moderation-and-ranking.md` § Resolved design
//! decisions Q3 (the `muted-keywords` row owner, which records this shape).
//!
//! # Why this module exists at all
//!
//! The list itself has never needed a home: it is the `moderation` record's
//! `muted_keywords`, read and written on the account plane by
//! `fauna_account_plane::preference_surfaces`.
//!
//! What did not fit it is the *loaded* bit. `keywords` is empty both **before** the
//! first read returns and **after** one that found nothing, so an app rendering
//! off the list alone announces "You haven't muted any words yet" over a list
//! nobody has read — which all 7 apps did. The bit that separates those two
//! states is page state a pure projection structurally cannot hold, and giving
//! each app its own `loaded` flag is seven copies of one rule (priority #1) —
//! the divergence the ratified rule names in as many words.
//!
//! So the page read hands back a [`MutedWordsSnapshot`] that carries the two
//! facts *together*; the bit cannot drift from the rows it describes because
//! the same expression assigns both.

use fauna_core::data::MutedKeyword;
use serde::{Deserialize, Serialize};

/// The `muted-words` sub-page in one record — the rows plus the *have they been
/// read* bit that is their second painting condition.
///
/// [`Default`] is the pre-read state (`loaded: false`), which is what makes the
/// rule hold by construction: an app that holds this record from page
/// construction paints no empty state until a read replaces it, and a **failed**
/// first read replaces nothing (the load returns `Err`), so the page stays
/// unloaded and its `error-message` does the talking instead of a false "nothing
/// here" beside it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MutedWordsSnapshot {
    /// The normalized, persisted entries — term and weight. Each term is a
    /// `muted-word-item` row (the page renders the term); the whole list is
    /// what the conversation-bubble collapse matches against
    /// (`matches_muted_keywords`, which reads the weights).
    pub keywords: Vec<MutedKeyword>,
    /// Whether a read has **returned successfully**, i.e. whether `keywords` is an
    /// answer rather than an absence. The second painting condition of
    /// `muted-word-empty`: paint it only on `loaded && keywords.is_empty()`
    /// (`docs/goal/ui/README.md` § *List pages: loading is not empty*).
    ///
    /// **Monotonic by construction**: every value here is minted by a call that
    /// already succeeded, so there is no code path that can clear it — a later
    /// failure leaves the previous snapshot (and its rows) on screen rather than
    /// re-arming a loading state under content the user can still see.
    pub loaded: bool,
}

impl MutedWordsSnapshot {
    /// May `muted-word-empty` paint? Both conditions, in one owner: the read has
    /// resolved **and** it found nothing.
    ///
    /// The two Rust apps call this; the five that reach the list across a
    /// UniFFI/wasm boundary re-state it in their own language (a `uniffi::Record`
    /// carries data, not methods), which is why the field doc above names the
    /// condition rather than leaving it to each caller to invent.
    pub fn shows_empty_state(&self) -> bool {
        self.loaded && self.keywords.is_empty()
    }

    /// The terms, in stored order — what the page's rows render. The weights
    /// stay on [`Self::keywords`] for the collapse matcher.
    pub fn terms(&self) -> Vec<String> {
        self.keywords.iter().map(|k| k.keyword.clone()).collect()
    }
}
