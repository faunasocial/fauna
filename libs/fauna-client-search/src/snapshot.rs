//! The shared search snapshot — the single read model the Search page renders
//! from on every app (`docs/goal/ui/search.md` § State & data shape, ratified
//! 2026-08-02). No client composes result-card state itself (priorities
//! #1/#2/#3); the manager owns it and hands down this cheap-to-clone projection.
//!
//! UniFFI/WASM-exposed, so **no serde-`flatten` maps**: [`SearchResultRow`] is a
//! clean projection of the wire `fauna_protocol::search::SearchResult` (which
//! carries an `extra: BTreeMap`), exactly as `FeedSummaryView` projects its wire
//! type in `fauna-feed`.

use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

/// Which backend produced a row (`search.md` § State & data shape — the row's
/// **source**).
///
/// Rendered as provenance, not as a section split: the ratified UX is **one
/// merged list** ordered by relevance, never a "local results" header above a
/// "nest results" header (§ The local/nest merge).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SearchSource {
    /// Backend 1 — the nest's floor-derived `content_fts`, over
    /// `fauna.search.query`.
    #[default]
    Nest,
    /// Backend 2 — this device's sealed per-user tantivy replica.
    Local,
}

/// Where a result row navigates when the user opens it — a **typed** target,
/// never a raw id the client parses itself (`search.md` § Where logic lives —
/// *Result navigation*).
///
/// `None` on the row means the row is honestly non-navigable and renders inert,
/// which is today's behaviour on all 7 apps. The variants grow per class as
/// each becomes navigable; the local-index kinds beyond mail arrive with
/// rollout slice S4.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SearchNav {
    /// A post, by its real 64-hex id.
    ///
    /// Minted from the nest row's `content_id` under the **ratified wire
    /// contract** that post-class `content_id` *is* the post id — not a hash
    /// (`search.md` § The page's wire surface, corrected + ratified
    /// 2026-08-02). Profile and bridge rows hash their natural id one-way and
    /// stay non-navigable until the additive `SearchResult` id field ships.
    Post { post_id: String },
    /// A mail message, addressed by the thread that holds it plus the message
    /// itself — what a conversations UI needs to open the thread and select the
    /// row inside it.
    Mail {
        thread_id: String,
        message_id: String,
    },
    /// An unsent draft — the target is the **composer holding it**, not a
    /// message.
    ///
    /// Its own variant rather than a `Mail` with a synthetic message id, because
    /// a draft has no message to select: `Mail`'s contract is "open the thread
    /// *and* highlight this message in it", and there is nothing there to
    /// highlight. `thread_id` is `None` for the single-slot new-thread compose,
    /// which belongs to no thread yet — opening it means opening the new-message
    /// composer (`fauna_conversations::index_sink::NEW_THREAD_DRAFT_ID`).
    Draft { thread_id: Option<String> },
    /// An address-book card, by its `uid_hash` (hex-lowercase) — the target is
    /// the card's detail view in the Address Book.
    ///
    /// The `uid_hash` rather than the server-assigned `card_id` because it is
    /// the identity that survives an in-place vCard edit and is stable across
    /// devices — the same key the index dedups on and the CardDAV wire's own
    /// dedup key (`content-index.md` § Ingest triggers, v1 — the contacts
    /// ruling's *doc identity* sub-bullet).
    Contact { uid_hash: String },
    /// A file in one of the user's Sync- or backup-type folders — the target
    /// is the Media page's `media-item-detail`, the one surface all 7 apps ship
    /// (`content-index.md` § Ingest triggers, v1 → *The files/media arms are
    /// SCOPED*; `ui/media.md`).
    ///
    /// **The identity is the durable pair, not the rendered row.** `folder_id`
    /// is the set's stable `FolderSummary.id` rather than its *name*, which the
    /// user may rename at any time, and `path_hash` is
    /// `blake3(normalized relative path)` (`fauna_core::sync::path_hash`), hex-
    /// lowercase. Carrying the name instead would move every file identity in a
    /// set the moment it was renamed; carrying the path plaintext would put a
    /// sealed label on a wire that no longer rests one.
    ///
    /// ⚠ The Media page keys its item state on **rendered row fields**, not on
    /// this pair, so an app acting on this variant owes an explicit
    /// identity→item lookup — it must not assume the two spellings meet. That is
    /// the `Contact { uid_hash }` → `card_id` mismatch, pre-stated here rather
    /// than discovered a second time (`content-index.md`, the ruling's
    /// *resolver and the target* sub-bullet).
    File { folder_id: i64, path_hash: String },
}

/// One row of the merged, ordered, deduplicated result list — the manager's
/// projection of either a wire `SearchResult` (backend 1) or a
/// [`LocalSearchHit`](crate::LocalSearchHit) (backend 2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SearchResultRow {
    /// The row's identity within its class — half of the dedup key. Never
    /// parsed by a client for meaning (`search.md` § The page's wire surface:
    /// "Apps never parse `content_id` themselves in any class").
    pub content_id: String,
    /// The raw type string the producing backend used, kept for keying and
    /// diagnostics. Clients render [`badge`](Self::badge), never this.
    pub content_type: String,
    /// The localized type badge, from the shared class map — each app
    /// resolves the key rather than hard-coding English.
    pub badge: LocalizedText,
    /// Display-ready snippet: the nest's FTS snippet with its `<b>` markers and
    /// entities cleaned, or — for a local row — the snippet the local arm
    /// rendered from locally-held content (the sealed index stores postings
    /// only, never source text; `content-index.md` § Don't do these).
    pub snippet: String,
    /// Epoch **millis** at the client boundary. The wire carries micros
    /// (`SearchResult.created_at`) and the local index nanos
    /// (`QueryHit.timestamp_ns`); both are normalised here so no client divides.
    pub timestamp: i64,
    pub source: SearchSource,
    /// The typed navigation target, or `None` for an inert row.
    pub navigation: Option<SearchNav>,
}

/// The Search page's entire observable state — a cheap clone of the manager's
/// current state, re-read on every observer notification.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SearchSnapshot {
    /// The **last fired** query, not the live input buffer — the query field is
    /// a plain local field on every app and submit reads it explicitly
    /// (`search.md` § User actions). Empty ⇒ no search has been run, which is
    /// what gates the pre-search rendering (`search-cancel-button` and the
    /// results/no-results pair all appear only once this is set).
    pub query: String,
    /// The active `search-type-filter` token (`crate::kind::TYPE_FILTER_ALL` or
    /// a nest `content_type`), applied to **both** arms through the one shared
    /// mapping (`crate::kind`).
    pub type_filter: String,
    /// The merged, deduplicated, relevance-ordered rows.
    pub results: Vec<SearchResultRow>,
    /// A query is running. Holds until **both** arms settle, so the page never
    /// reports "no results" while the nest arm is still out (`search.md` § The
    /// local/nest merge).
    pub in_flight: bool,
    /// The searched-and-found-nothing state — `search-no-results`. Distinct
    /// from "not searched yet" (`query` empty) and from "still loading"
    /// (`in_flight`); exactly one of the three renders at any moment.
    pub no_results: bool,
    /// Whether `search-load-more-button` should show. Derived from the **nest
    /// arm's** row count against the shared paging policy — the local arm is
    /// unpaged (it answers from a local index in one shot), so local rows must
    /// never inflate this into offering a page that cannot exist.
    pub has_more: bool,
    /// Page-level error → `error-message`. Set when **either** arm fails, while
    /// the other arm's rows stay on screen: partial results are shown honestly,
    /// never blanked (`search.md` § The local/nest merge).
    pub error: Option<LocalizedText>,
}

impl Default for SearchSnapshot {
    fn default() -> Self {
        SearchSnapshot {
            query: String::new(),
            type_filter: crate::kind::TYPE_FILTER_ALL.to_string(),
            results: Vec::new(),
            in_flight: false,
            no_results: false,
            has_more: false,
            error: None,
        }
    }
}

impl SearchSnapshot {
    /// Whether a search has ever been fired — the gate every app's
    /// `search-cancel-button` and results/no-results pair render behind.
    ///
    /// Derived rather than stored: [`cancel`](crate::SearchManager::cancel)
    /// clears the query, and firing sets it, so the two can never disagree.
    pub fn searched(&self) -> bool {
        !self.query.is_empty()
    }
}
