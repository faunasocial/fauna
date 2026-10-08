//! The global search page — a **paint shell** over the shared
//! [`fauna_client_search::SearchManager`] (`docs/goal/ui/search.md` § State &
//! data shape; ui.yaml `search`).
//!
//! Every decision this page used to make itself — which backends to ask, how to
//! map the type filter onto them, how to order and dedup the rows, when to offer
//! "load more", what the badge and snippet say — now belongs to the manager, and
//! tui is the first app to consume it (the lead-app rule). What is left here is
//! genuinely per-app: the query-field buffer, the collapsible bar, and turning a
//! [`SearchSnapshot`] into [`Element`]s.
//!
//! **Type filter is applied to BOTH arms** by the manager's one shared mapping
//! (`fauna_client_search::kind`) — server-side on the wire's `content_type`,
//! and as a kind set on the local sealed index. The old per-app note about
//! filtering server-side "matching linux/windows, NOT web/android's client-side
//! post-filter" is now moot for any app on the manager: there is one mapping,
//! and re-firing is what keeps "load more" correct.
//!
//! **Result-row navigation** is carried by the snapshot as a typed
//! [`SearchNav`](fauna_client_search::SearchNav) target, and [`open_result`]
//! acts on **every** variant — tui first (the lead-app rule). Two of them are
//! id-space *resolves* rather than wire-ups, because their row identity and
//! their destination's key are deliberately different spellings: `Contact`
//! (`uid_hash` → the Address Book's `card_id`) and `File` (`(folder_id,
//! path_hash)` → a Media-page item keyed by rendered fields). Both would fail
//! *silently* if matched instead of looked up, which is why each resolves
//! through shared Rust. `Mail` lands **both** halves of its contract — the
//! thread jump and the message selection — over the shared
//! `ConversationsManager::select_thread_and_message`; tui is the first app to
//! have a seam for the second half at all, and
//! [`focus_selected_message`] is what turns that selection into a visible
//! scroll here (the viewport follows the focus ring). (The old comment claiming
//! `content_id` "cannot deep-link" is stale: post-class `content_id` **is** the
//! post id, a ratified wire contract since 2026-08-02.)

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_search::snapshot::SearchSnapshot;
use fauna_client_search::{
    SearchManager, SearchSnapshotObserver, TYPE_FILTER_OPTIONS, type_filter_label,
};
use fauna_i18n::strings::{common as t, search_page};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, DataMessage, UiMessage};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;

/// The manager over tui's authed transport — the search twin of
/// [`crate::feed::CliFeedManager`].
pub type CliSearchManager = SearchManager<Arc<NestClient>>;

/// Page state: the live query buffer, the bar's collapse, and the manager that
/// owns everything else.
pub struct SearchState {
    /// `None` pre-login — every reader degrades to the default snapshot rather
    /// than panicking (the e2e state serializer runs pre-auth).
    pub manager: Option<Arc<CliSearchManager>>,
    /// `search-query-field`'s buffer (`SearchField::Query`) — deliberately NOT
    /// in the manager. The field is plain local state on every app and
    /// `search-submit-button` reads it explicitly, so a keystroke never implies
    /// network work (`search.md` § Where logic lives — submit-driven, no
    /// debounce). The manager's `snapshot().query` is the LAST FIRED query,
    /// which is a different thing and is what "load more" re-issues.
    pub query_buffer: String,
    /// Collapses/reveals the query field, submit/clear buttons, and type filter
    /// (`search-toggle-button`) — mirrors web's `barVisible` and linux's
    /// `wire_search_bar_controls`. The results panel is unaffected.
    pub bar_visible: bool,
}

/// **Hand-written, not derived** — `bar_visible` must default to `true`. The
/// search bar starts open on every app; a derived `Default` would give
/// `false` and ship a page whose query field is collapsed out of existence
/// until the user finds the toggle. `App::default()` and the sign-out reset both
/// go through here, so this is the value they get.
impl Default for SearchState {
    fn default() -> Self {
        SearchState {
            manager: None,
            query_buffer: String::new(),
            bar_visible: true,
        }
    }
}

impl SearchState {
    /// The page's read model. A default snapshot pre-login renders the empty
    /// page, exactly as `None` did before.
    pub fn snapshot(&self) -> SearchSnapshot {
        self.manager
            .as_ref()
            .map(|m| m.snapshot())
            .unwrap_or_default()
    }
}

/// Forward manager notifications into the render loop's `UiMessage` channel —
/// the `TuiFeedObserver` shape. `on_changed` fires synchronously on whatever
/// thread mutated, so it must not touch `App`; a fresh snapshot read per tick
/// makes coalescing safe.
struct TuiSearchObserver {
    tx: UnboundedSender<UiMessage>,
}

impl SearchSnapshotObserver for TuiSearchObserver {
    fn on_changed(&self) {
        // A closed channel means the app is shutting down — nothing to notify.
        let _ = self.tx.send(UiMessage::Data(DataMessage::SearchChanged));
    }
}

/// Build the page state at the post-auth hook. No initial fetch — there is no
/// query yet (unlike notifications/events, which prefetch a list).
pub fn init(nest: Arc<NestClient>, tx: &UnboundedSender<UiMessage>) -> SearchState {
    let manager = Arc::new(SearchManager::new(nest));
    manager.add_observer(Arc::new(TuiSearchObserver { tx: tx.clone() }));
    SearchState {
        manager: Some(manager),
        ..SearchState::default()
    }
}

/// Attach backend 2 — the sealed local index — as the manager's second arm.
///
/// Separate from [`init`] and deliberately later in the post-auth hook: the
/// launcher that can open the sealed slice is built by the conversations
/// session, and resolving a reader is async (derive the key off the account's
/// sealed state, read the `__index` rail). Registration therefore lands a
/// moment after login, and the page is correct throughout — until it lands
/// `has_local_index()` is `false` and the page renders nest rows only, which is
/// the same state a device with no published index stays in permanently
/// (`content-index.md` § Where queries run).
///
/// A no-op when mail is not enabled (nothing to derive the index key from) or
/// when this login has no conversations manager to resolve hits against.
pub fn attach_local_index(state: &SearchState, conv: &crate::conversations::ConversationsState) {
    let (Some(manager), Some(launcher), Some(conv_manager)) = (
        state.manager.clone(),
        conv.index_launcher.clone(),
        conv.manager.clone(),
    ) else {
        return;
    };
    // The conversations manager is the content lookup: the sealed index stores
    // postings only, so a local row's snippet and its `SearchNav` both resolve
    // out of the thread store (`ui/search.md` § The local/nest merge — "local
    // rows render theirs from locally-held content").
    //
    // A **resolver**, not an arm: minting the arm needs the MSEK, which does not
    // exist until mail is enabled, so resolving here once left a user who
    // enabled mail after login with a permanently nest-only page — even after
    // the builder started publishing that session's mail. Registering is
    // synchronous and always possible; the manager mints on the first query that
    // finds no arm (`content-index.md` § Ingest triggers, v1 → *An arm attaches
    // when its precondition arrives*).
    manager.set_local_index_resolver(fauna_client_conversations::LauncherLocalIndex::new(
        launcher,
        conv_manager,
    ));
}

/// The global search page's editable field (`crate::search`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SearchField {
    /// `search-query-field` — the live query buffer.
    Query,
}

pub fn field(state: &SearchState, field: &SearchField) -> String {
    match field {
        SearchField::Query => state.query_buffer.clone(),
    }
}

pub fn set_field(state: &mut SearchState, field: SearchField, value: String) {
    match field {
        SearchField::Query => state.query_buffer = value,
    }
}

/// The page's gestures (`search.md` § User actions).
#[derive(Debug, Clone)]
pub enum Action {
    Submit,
    SetTypeFilter(String),
    /// `search-clear-button` — clears the QUERY BUFFER only (linux's
    /// `clear_btn`: nothing else touched; the loaded results are left as-is
    /// until the next fire).
    Clear,
    LoadMore,
    /// `search-toggle-button` — flips [`SearchState::bar_visible`]. Local-only.
    ToggleBar,
    /// `search-cancel-button` — resets the page to pre-search and force-shows
    /// the bar. The manager owns the reset (it must also drop any reply still
    /// in flight); only the buffer and the bar are ours.
    Cancel,
}

impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`). Exhaustive with no fallback arm,
    /// so a new search gesture must answer the offline question.
    ///
    /// Nothing on this page desensitizes, and that is the classification
    /// talking, not an omission: the page's only wire call is
    /// `fauna.search.query`, which the shared table registers as a **`Read`**,
    /// and the gate declines to decide whether a read is answerable offline
    /// (that is W3 (account-data-plane.md § Workstreams)'s projection question). Declaring it anyway is the point —
    /// it records *which* kind runs here, so a later reclassification reaches
    /// this page for free rather than needing it re-derived.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            // Both fire the same query; `SetTypeFilter` re-fires with the live
            // buffer text, and `LoadMore` pages the same call.
            Action::Submit | Action::SetTypeFilter(_) | Action::LoadMore => {
                Some("fauna.search.query")
            }

            // Local. The query buffer, the bar's visibility, and the page reset
            // — `Cancel`'s `manager.cancel()` drops an in-flight reply, it does
            // not issue one.
            Action::Clear | Action::ToggleBar | Action::Cancel => None,
        }
    }
}

/// Local half of a gesture → its network half (the notifications/events split:
/// the agent awaits the op, the keyboard spawns it).
pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    match action {
        Action::Submit => {
            let manager = app.search.manager.clone()?;
            let query = app.search.query_buffer.trim().to_string();
            // The empty-query guard every app's search bar shares. Checked
            // here as well as in the manager so no op is even spawned.
            if query.is_empty() {
                return None;
            }
            let type_filter = manager.snapshot().type_filter;
            Some(Op::Run {
                manager,
                query,
                type_filter,
            })
        }
        Action::SetTypeFilter(token) => {
            let manager = app.search.manager.clone()?;
            // Re-fires using the LIVE buffer text (linux's `fire_search` always
            // reads the entry's current text), not the last-fired query.
            let query = app.search.query_buffer.trim().to_string();
            if query.is_empty() {
                return None;
            }
            Some(Op::Run {
                manager,
                query,
                type_filter: token,
            })
        }
        Action::Clear => {
            app.search.query_buffer.clear();
            None
        }
        Action::ToggleBar => {
            app.search.bar_visible = !app.search.bar_visible;
            None
        }
        Action::Cancel => {
            app.search.query_buffer.clear();
            app.search.bar_visible = true;
            if let Some(m) = app.search.manager.as_ref() {
                m.cancel();
            }
            sync_error(app);
            None
        }
        Action::LoadMore => {
            let manager = app.search.manager.clone()?;
            Some(Op::LoadMore { manager })
        }
    }
}

/// The network half. Both variants drive the manager, which owns the state —
/// so neither carries results back.
pub enum Op {
    Run {
        manager: Arc<CliSearchManager>,
        query: String,
        type_filter: String,
    },
    LoadMore {
        manager: Arc<CliSearchManager>,
    },
}

/// What an [`Op`] resolved to. The manager already committed its own state by
/// the time this lands, so the outcome is a bare "it settled" tick whose only
/// job is to move the manager's error onto tui's page-error surface.
#[derive(Debug)]
pub enum Outcome {
    Settled,
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::Run {
                manager,
                query,
                type_filter,
            } => manager.run_query(&query, &type_filter).await,
            Op::LoadMore { manager } => manager.load_more().await,
        }
        Outcome::Settled
    }
}

/// Fold an [`Outcome`] back into the page.
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::Settled => sync_error(app),
    }
}

/// Activate a `search-result-item[i]` — route its typed
/// [`fauna_client_search::SearchNav`] target into the destination page's own
/// gesture, exactly as a direct click there would:
///
/// - `Post` → `feed::Action::OpenPostDetail` (ids match: post-class
///   `content_id` **is** the post id, ratified).
/// - `Draft { thread_id: Some }` and `Mail` → `conversations::Action::
///   SelectThread`. `Mail` lands the thread jump only — no seam yet selects a
///   message *inside* a thread, so its contract's highlight half stays
///   unimplemented; opening the right thread is still strictly better than
///   the inert row it replaces.
/// - `Draft { thread_id: None }` → `StartNewConversation`, which is already
///   the "open/focus the single-slot new-thread composer, preserving any
///   in-progress draft" action (`ConversationsManager::start_new_conversation`
///   only seeds a fresh composer when none is stashed) — this looked like a
///   missing seam until read, and is not one.
///
/// - `Contact` → `contacts::Action::OpenCardByUid`, which resolves the row's
///   `uid_hash` to the Address Book's `card_id` over the wire
///   (`address_book::locate_card` → the shared
///   `CardDavClient::locate_card_by_uid_hash`). **Not** a cast of one id to the
///   other: the two spellings are the same width and both hex, so handing a
///   `uid_hash` to the `card_id` consumer would open nothing and raise no
///   error. The read is also what makes a card in a book the user never opened
///   reachable at all.
///
/// - `File` → `media::Action::OpenFile`, which looks the item up by the row's
///   **durable identity pair** (`MediaSnapshot::locate_file`) rather than
///   matching a rendered field: the page's items carry the set's *name* and a
///   display name, both of which a rename moves, while the pair does not. The
///   lookup runs over the raw drained aggregate, so it is independent of the
///   active filter and sort.
pub fn open_result(
    app: &mut App,
    nav: fauna_client_search::SearchNav,
) -> Option<crate::app::PageOp> {
    use crate::app::PageOp;
    use crate::pages::Page;
    use fauna_client_search::SearchNav;

    match nav {
        SearchNav::Post { post_id } => {
            app.page = Page::Feed;
            crate::feed::apply_local(app, crate::feed::Action::OpenPostDetail(post_id))
                .map(PageOp::Feed)
        }
        SearchNav::Draft {
            thread_id: Some(thread_id),
        } => {
            app.page = Page::Conversations;
            crate::conversations::apply_local(
                app,
                crate::conversations::Action::SelectThread(thread_id),
            )
            .map(PageOp::Conversations)
        }
        // The WHOLE of `Mail`'s contract — "open the thread **and** select this
        // message in it" (`ui/search.md` § State & data shape). A draft above
        // keeps the plain thread-open: an unsent draft has no message to select,
        // which is exactly why it got its own variant rather than riding `Mail`.
        SearchNav::Mail {
            thread_id,
            message_id,
        } => {
            app.page = Page::Conversations;
            let op = crate::conversations::apply_local(
                app,
                crate::conversations::Action::SelectThreadAndMessage {
                    thread_id,
                    message_id,
                },
            )
            .map(PageOp::Conversations);
            focus_selected_message(app);
            op
        }
        SearchNav::Draft { thread_id: None } => {
            app.page = Page::Conversations;
            crate::conversations::apply_local(
                app,
                crate::conversations::Action::StartNewConversation,
            )
            .map(PageOp::Conversations)
        }
        SearchNav::Contact { uid_hash } => {
            app.page = Page::Contacts;
            crate::contacts::apply_local(app, crate::contacts::Action::OpenCardByUid(uid_hash))
                .map(PageOp::Contacts)
        }
        SearchNav::File {
            folder_id,
            path_hash,
        } => {
            app.page = Page::Media;
            crate::media::apply_local(
                app,
                crate::media::Action::OpenFile {
                    folder_id,
                    path_hash,
                },
            )
            .map(PageOp::Media)
        }
    }
}

/// Land the focus ring on the message a `Mail` result selected, so the thread
/// opens *showing* it.
///
/// **In tui the viewport follows the focus ring** ([`crate::ui::scroll_offset`]),
/// so the focus IS the scroll — the same reason
/// `events::open_time_axis_at_working_start` exists. Without this the thread
/// opens at the top and a hit deep in a long conversation is exactly as hard to
/// find as it was before search pointed at it, which is the user-visible half of
/// what `SearchNav::Mail`'s `message_id` is for.
///
/// Anchors on the paint's own marker element rather than on a message ordinal:
/// the bubble loop emits a different child set per arm (a deleted, muted or
/// content-collapsed message paints no reply button), so "the Nth reply button"
/// is not the Nth message. Taking the first *focusable* element at or after the
/// marker gives the selected message's own first control on every arm.
///
/// Silent no-ops, both correct: the selection resolved to nothing (the message
/// is not in the fetched window — the manager already declined to mark it), or
/// the message has no focusable control at all (a deleted or legal-takedown
/// tombstone paints text only). The mark still paints; only the scroll is owed
/// to a control, and a page that cannot scroll to it is better than a ring
/// parked on an unrelated message.
fn focus_selected_message(app: &mut App) {
    let Some(marker_at) = app.page_elements().iter().position(|e| {
        e.attrs
            .iter()
            .any(|(k, v)| k == crate::conversations::SELECTED_MESSAGE_MARKER_ATTR && v == "true")
    }) else {
        return;
    };
    app.focus_page_element(marker_at);
}

/// Mirror the snapshot's error onto tui's page-error surface (`error-message`).
///
/// The manager reports the failure of **either** backend while keeping the
/// other's rows, so this can set an error on a page that is also showing
/// results — which is the point (`search.md` § The local/nest merge: partial
/// results shown honestly, never blanked).
pub fn sync_error(app: &mut App) {
    match app.search.snapshot().error {
        Some(text) => {
            let message = crate::wizard::localized(&text);
            app.errors.insert(Page::Search, message);
        }
        None => {
            app.errors.remove(&Page::Search);
        }
    }
}

/// The ordered ui.yaml element list (page `search` + the `search-bar` /
/// `search-results-panel` / `search-result-card` components).
pub fn elements(app: &App) -> Vec<Element> {
    let st = &app.search;
    let snap = st.snapshot();
    let mut out = vec![Element::label(ids::PAGE_HEADING, search_page::TITLE)];
    // Collapsible bar row (entry + submit + clear + type filter) — mirrors
    // web's `{#if barVisible}` and linux's `apply_bar_visible`.
    if st.bar_visible {
        // Labelled with the cross-app placeholder — an unlabelled input
        // prompts with its raw element id ("search-query-field: _"), which is
        // meaningless to a human (the walk invariant I3 pins the class).
        out.push(
            Element::input(
                ids::SEARCH_QUERY_FIELD,
                &st.query_buffer,
                Field::Search(SearchField::Query),
            )
            .labelled(search_page::PLACEHOLDER),
        );
        out.push(Element::gesture_button(
            ids::SEARCH_SUBMIT_BUTTON,
            t::SEARCH,
            true,
            Gesture::Search(Action::Submit),
        ));
        out.push(Element::gesture_button(
            ids::SEARCH_CLEAR_BUTTON,
            search_page::CLEAR,
            true,
            Gesture::Search(Action::Clear),
        ));
        // `text` stays the raw wire token — `get_text` returns it and `select`
        // takes it, and it is the `content_type` the query carries — so the
        // human form rides the paint-only `display` value instead. The label is
        // the shared one (`fauna_client_search::type_filter_label`), which is
        // the same `LocalizedText` a row of that type carries as its badge.
        out.push(
            Element::select(
                ids::SEARCH_TYPE_FILTER,
                snap.type_filter.clone(),
                SelectTarget::SearchType,
                TYPE_FILTER_OPTIONS.iter().map(|s| s.to_string()).collect(),
            )
            .display_value(
                type_filter_label(&snap.type_filter).resolve(fauna_i18n::strings::lookup),
            ),
        );
    }
    out.push(Element::gesture_button(
        ids::SEARCH_TOGGLE_BUTTON,
        if st.bar_visible {
            search_page::HIDE_SEARCH_BAR
        } else {
            search_page::SHOW_SEARCH_BAR
        },
        true,
        Gesture::Search(Action::ToggleBar),
    ));
    // Only shown once a search has actually fired (web's `{#if searched}`,
    // linux's build-time `cancel_btn.set_visible(false)`).
    if snap.searched() {
        out.push(Element::gesture_button(
            ids::SEARCH_CANCEL_BUTTON,
            t::CANCEL,
            true,
            Gesture::Search(Action::Cancel),
        ));
        // The three states are mutually exclusive by construction: `no_results`
        // is only ever set on a settled empty result set, so a first query still
        // in flight renders neither marker rather than flashing "No results".
        if snap.no_results {
            out.push(Element::label(
                ids::SEARCH_NO_RESULTS,
                search_page::NO_RESULTS_SHORT,
            ));
        } else if !snap.results.is_empty() {
            out.push(Element::label(
                ids::SEARCH_RESULTS_VIEW,
                format!("{} results", snap.results.len()),
            ));
            for r in &snap.results {
                out.push(result_element(r));
            }
            // The manager derives this from the NEST arm's page against the
            // shared paging policy — local rows never inflate it into offering
            // a page the wire cannot produce.
            if snap.has_more {
                out.push(Element::gesture_button(
                    ids::SEARCH_LOAD_MORE_BUTTON,
                    t::LOAD_MORE,
                    true,
                    Gesture::Search(Action::LoadMore),
                ));
            }
        }
    }
    out
}

fn result_element(r: &fauna_client_search::SearchResultRow) -> Element {
    let badge = crate::wizard::localized(&r.badge);
    // Epoch millis on the row (the manager normalises both backends);
    // `format_epoch_us` wants micros.
    let when = crate::format::format_epoch_us(r.timestamp.saturating_mul(1_000));
    let text = format!("{badge}  ·  {when}  ·  {}", r.snippet);
    // Only render a gesture for a target `open_result` can actually act on
    // (`Post` / `Draft` / `Mail`) — `Contact` and `File` carry a real typed
    // target from the manager but no destination seam yet, and a clickable
    // row that silently does nothing reads as a broken button, not an honest
    // "no navigation" state. Match `open_result`'s coverage explicitly rather
    // than a catch-all `Some(_)`, so a future `SearchNav` variant fails to
    // compile here instead of silently painting a dead button.
    match navigable(&r.navigation) {
        Some(nav) => Element::gesture_button(
            ids::SEARCH_RESULT_ITEM,
            text,
            true,
            Gesture::OpenSearchResult(nav),
        ),
        None => Element::label(ids::SEARCH_RESULT_ITEM, text),
    }
}

/// The `SearchNav` variants [`open_result`] wires today. Exhaustive on
/// purpose (no `_` arm): adding a variant to the shared enum without deciding
/// here whether tui can act on it yet would silently paint a dead button.
fn navigable(
    nav: &Option<fauna_client_search::SearchNav>,
) -> Option<fauna_client_search::SearchNav> {
    use fauna_client_search::SearchNav;
    match nav {
        None => None,
        Some(n @ SearchNav::Post { .. })
        | Some(n @ SearchNav::Draft { .. })
        | Some(n @ SearchNav::Mail { .. })
        | Some(n @ SearchNav::Contact { .. })
        | Some(n @ SearchNav::File { .. }) => Some(n.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_search::snapshot::{SearchResultRow, SearchSource};
    use fauna_core::localized::LocalizedText;

    /// A manager over an unreachable transport. Nothing here ever fires a
    /// query — these tests drive the page's *projection*, and the manager's own
    /// behaviour (both arms, merge, ordering, paging) is pinned by its tests in
    /// `fauna-client-search`, not re-driven from each of seven apps.
    fn offline_manager() -> Arc<CliSearchManager> {
        Arc::new(SearchManager::new(NestClient::new(
            "http://127.0.0.1:1".to_string(),
            fauna_core::identity::ActorKeypair::generate(),
        )))
    }

    /// Drive the page's rendering off a given snapshot, installed through the
    /// manager's own test seam.
    fn app_showing(snapshot: SearchSnapshot) -> App {
        let mut app = crate::app::tests::test_app();
        let manager = offline_manager();
        manager.set_snapshot_for_test(snapshot);
        app.search.manager = Some(manager);
        // `bar_visible` is deliberately NOT set here — these tests exercise the
        // real default, which must be `true` (see `impl Default for SearchState`).
        app
    }

    fn row(content_type: &str, snippet: &str) -> SearchResultRow {
        SearchResultRow {
            content_id: "deadbeef".into(),
            content_type: content_type.into(),
            badge: LocalizedText::key("search_page.badge_post"),
            snippet: snippet.into(),
            timestamp: 0,
            source: SearchSource::Nest,
            navigation: None,
        }
    }

    fn searched_with(results: Vec<SearchResultRow>) -> SearchSnapshot {
        SearchSnapshot {
            query: "hello".into(),
            results,
            ..SearchSnapshot::default()
        }
    }

    /// ui.yaml `search`: heading, query field, submit, clear, type filter,
    /// toggle button — no cancel button pre-search, and nothing results-shaped
    /// until a search has actually fired.
    #[test]
    fn the_empty_page_paints_the_bar_but_no_results_or_no_results_marker() {
        let app = app_showing(SearchSnapshot::default());
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert_eq!(
            ids,
            vec![
                "page-heading",
                "search-query-field",
                "search-submit-button",
                "search-clear-button",
                "search-type-filter",
                "search-toggle-button",
            ],
            "pre-search: neither search-cancel-button nor any results marker renders"
        );
    }

    #[test]
    fn a_settled_empty_result_set_paints_no_results_not_the_results_view() {
        let app = app_showing(SearchSnapshot {
            no_results: true,
            ..searched_with(vec![])
        });
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert!(ids.contains(&"search-no-results".to_string()));
        assert!(!ids.contains(&"search-results-view".to_string()));
    }

    /// The state the old page could not represent: a first query still in
    /// flight is neither "results" nor "no results", so it must flash neither.
    #[test]
    fn a_first_query_in_flight_paints_neither_marker() {
        let app = app_showing(SearchSnapshot {
            in_flight: true,
            ..searched_with(vec![])
        });
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert!(ids.contains(&"search-cancel-button".to_string()));
        assert!(
            !ids.contains(&"search-no-results".to_string()),
            "'No results' must not flash while an arm is still out"
        );
        assert!(!ids.contains(&"search-results-view".to_string()));
    }

    #[test]
    fn a_loaded_nonempty_result_set_paints_the_results_view_and_rows() {
        let app = app_showing(searched_with(vec![row("post", "hello world")]));
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert!(ids.contains(&"search-results-view".to_string()));
        assert!(!ids.contains(&"search-no-results".to_string()));
        assert_eq!(
            ids.iter().filter(|id| *id == "search-result-item").count(),
            1
        );
    }

    /// The row text is composed from the snapshot's already-projected badge and
    /// snippet — tui no longer cleans FTS markers or maps content types itself.
    #[test]
    fn result_row_text_reads_the_projected_badge_and_snippet() {
        let el = result_element(&row("post/article", "a hit here"));
        assert!(el.text.contains("a hit here"), "text: {}", el.text);
        assert!(!el.text.contains("<b>"), "text: {}", el.text);
    }

    /// Load-more visibility is the manager's derived `has_more` — the page no
    /// longer counts rows against a limit it holds.
    #[test]
    fn load_more_follows_the_snapshot_flag() {
        let app = app_showing(searched_with(vec![row("post", "x")]));
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert!(!ids.contains(&"search-load-more-button".to_string()));

        let app = app_showing(SearchSnapshot {
            has_more: true,
            ..searched_with(vec![row("post", "x")])
        });
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert!(ids.contains(&"search-load-more-button".to_string()));
    }

    /// The type filter renders the manager's committed value and offers the
    /// shared option set — no per-app copy of the token list.
    #[test]
    fn the_type_filter_paints_the_shared_options_and_the_committed_value() {
        let app = app_showing(SearchSnapshot {
            type_filter: "imap".into(),
            ..searched_with(vec![])
        });
        let el = elements(&app)
            .into_iter()
            .find(|e| e.id == "search-type-filter")
            .expect("the filter renders");
        assert_eq!(el.text, "imap");
        let crate::element::Role::Select { options, .. } = &el.role else {
            panic!("search-type-filter must be a select")
        };
        assert_eq!(options.len(), TYPE_FILTER_OPTIONS.len());
    }

    /// The committed value stays the raw wire token — `get_text` returns it and
    /// `select` takes it, so the round-trip the other apps' pickers rely on is
    /// unchanged — while the *human* reads the shared label through the
    /// paint-only display value. tui used to paint the bare token (`imap`).
    ///
    /// The label is the same `LocalizedText` a result row of that type carries
    /// as its badge (`fauna_client_search::type_filter_label`), so the filter
    /// and the rows it selects can never read differently.
    #[test]
    fn the_type_filter_paints_the_shared_label_over_its_wire_token() {
        for (token, want) in [("imap", "Email"), ("post", "Post"), ("all", "All")] {
            let app = app_showing(SearchSnapshot {
                type_filter: token.into(),
                ..searched_with(vec![])
            });
            let el = elements(&app)
                .into_iter()
                .find(|e| e.id == "search-type-filter")
                .expect("the filter renders");
            assert_eq!(el.text, token, "the wire token must round-trip unchanged");
            let crate::element::Role::Select { display, .. } = &el.role else {
                panic!("search-type-filter must be a select")
            };
            assert_eq!(
                display.as_deref(),
                Some(want),
                "{token} must paint its shared label"
            );
        }
    }

    /// Submit is a no-op on an empty/whitespace query (every app's guard):
    /// no `Op` is spawned at all.
    #[test]
    fn submit_on_empty_query_is_a_noop() {
        let mut app = crate::app::tests::test_app();
        app.search.manager = Some(offline_manager());
        app.search.query_buffer = "   ".to_string();
        assert!(apply_local(&mut app, Action::Submit).is_none());
        assert!(!app.search.snapshot().searched());
    }

    /// Submit carries the LIVE buffer and the committed filter into the op.
    #[test]
    fn submit_carries_the_live_buffer_and_the_committed_filter() {
        let mut app = crate::app::tests::test_app();
        app.search.manager = Some(offline_manager());
        app.search.query_buffer = "  hello  ".to_string();
        let op = apply_local(&mut app, Action::Submit).expect("non-empty query fires");
        let Op::Run {
            query, type_filter, ..
        } = op
        else {
            panic!("submit must produce a Run op")
        };
        assert_eq!(query, "hello", "the buffer is trimmed once, here");
        assert_eq!(type_filter, fauna_client_search::TYPE_FILTER_ALL);
    }

    /// Changing the filter re-fires with the live buffer under the NEW token —
    /// linux's `fire_search` semantics, preserved through the manager.
    #[test]
    fn setting_the_filter_refires_the_live_buffer_under_the_new_token() {
        let mut app = crate::app::tests::test_app();
        app.search.manager = Some(offline_manager());
        app.search.query_buffer = "hello".to_string();
        let op = apply_local(&mut app, Action::SetTypeFilter("post".into()))
            .expect("a non-empty buffer re-fires");
        let Op::Run {
            query, type_filter, ..
        } = op
        else {
            panic!("a filter change must produce a Run op")
        };
        assert_eq!(query, "hello");
        assert_eq!(type_filter, "post");
    }

    /// Clear touches only the query buffer — the loaded results are untouched
    /// (`search-clear-button`'s ui.yaml description is "Clear search query
    /// text", not "clear results").
    #[test]
    fn clear_only_touches_the_query_buffer() {
        let mut app = app_showing(searched_with(vec![row("post", "x")]));
        app.search.query_buffer = "hello".to_string();
        assert!(apply_local(&mut app, Action::Clear).is_none());
        assert_eq!(app.search.query_buffer, "");
        assert!(
            app.search.snapshot().searched(),
            "clear does not reset the search"
        );
        assert_eq!(app.search.snapshot().results.len(), 1);
    }

    /// `search-toggle-button`/`search-cancel-button` used to be inert 1px
    /// markers while web/windows/macos/ios/android all built a real collapsible
    /// search bar — this pins the real, wired behavior.
    #[test]
    fn toggle_hides_and_shows_the_bar_row() {
        let mut app = app_showing(SearchSnapshot::default());
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert!(ids.contains(&"search-query-field".to_string()));
        assert!(ids.contains(&"search-type-filter".to_string()));

        assert!(apply_local(&mut app, Action::ToggleBar).is_none());
        assert!(!app.search.bar_visible);
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert!(!ids.contains(&"search-query-field".to_string()));
        assert!(!ids.contains(&"search-submit-button".to_string()));
        assert!(!ids.contains(&"search-clear-button".to_string()));
        assert!(!ids.contains(&"search-type-filter".to_string()));
        assert!(
            ids.contains(&"search-toggle-button".to_string()),
            "the toggle button itself stays painted while hidden"
        );

        assert!(apply_local(&mut app, Action::ToggleBar).is_none());
        assert!(app.search.bar_visible, "a second toggle re-shows the bar");
    }

    /// The bug this shape guards against (linux's
    /// `cancel_after_a_hidden_toggle_does_not_desync_the_next_toggle_click`):
    /// hide the bar via toggle, then cancel (which force-shows it) — a THIRD
    /// click on toggle must hide it again immediately.
    #[test]
    fn cancel_after_a_hidden_toggle_does_not_desync_the_next_toggle_click() {
        let mut app = app_showing(searched_with(vec![]));
        apply_local(&mut app, Action::ToggleBar); // hide
        assert!(!app.search.bar_visible);

        apply_local(&mut app, Action::Cancel); // forces the bar back visible
        assert!(app.search.bar_visible, "cancel re-shows the bar");

        apply_local(&mut app, Action::ToggleBar);
        assert!(
            !app.search.bar_visible,
            "toggle must hide on the very next click, not resync first"
        );
    }

    /// A manager-reported failure lands on tui's page `error-message`, and
    /// clearing it removes the line.
    #[test]
    fn a_snapshot_error_lands_on_the_page_error_line() {
        let mut app = app_showing(SearchSnapshot {
            error: Some(LocalizedText::key_arg(
                "search_page.search_failed_reason",
                "reason",
                "nest unreachable",
            )),
            ..searched_with(vec![row("post", "x")])
        });
        sync_error(&mut app);
        let line = app
            .errors
            .get(&Page::Search)
            .cloned()
            .expect("an error line");
        assert!(line.contains("nest unreachable"), "line: {line}");

        app.search
            .manager
            .as_ref()
            .unwrap()
            .set_snapshot_for_test(searched_with(vec![row("post", "x")]));
        sync_error(&mut app);
        assert!(!app.errors.contains_key(&Page::Search));
    }

    /// `App::set_field` (not `search::set_field` in isolation) must route
    /// `Field::Search(SearchField::Query)` — the dispatcher-level test a unit
    /// test setting state directly cannot catch.
    #[test]
    fn app_set_field_routes_search_query_into_search_state() {
        let mut app = crate::app::tests::authed_app();
        assert!(
            app.set_field(Field::Search(SearchField::Query), "hello".to_string())
                .is_none()
        );
        assert_eq!(app.search.query_buffer, "hello");
        assert_eq!(app.field(Field::Search(SearchField::Query)), "hello");
    }

    // --- row 18: activating a search result navigates (`open_result`) ---

    fn row_with_nav(nav: fauna_client_search::SearchNav) -> SearchResultRow {
        SearchResultRow {
            navigation: Some(nav),
            ..row("post", "a hit")
        }
    }

    /// An authed app on the Search page with a real (offline) `FeedManager`
    /// and `ConversationsManager` attached — the destinations `open_result`
    /// routes into — mirroring `feed::tests::feed_app_with` /
    /// `conversations::tests::conv_app`'s no-nest-no-network shape.
    fn app_ready_to_navigate() -> App {
        let mut app = crate::app::tests::authed_app();
        app.feed.manager = Some(Arc::new(crate::feed::CliFeedManager::new(
            crate::app::tests::test_session().client,
            [7u8; 32],
        )));
        let conv_manager = fauna_conversations::ConversationsManager::new();
        conv_manager.install_mock_backends_for_test();
        app.conversations.manager = Some(conv_manager);
        app.page = Page::Search;
        app
    }

    /// `Post` ids match the wire contract directly — no lookup, straight into
    /// `feed::Action::OpenPostDetail`.
    #[test]
    fn activating_a_post_result_opens_its_post_detail_on_the_feed_page() {
        let mut app = app_ready_to_navigate();
        let op = open_result(
            &mut app,
            fauna_client_search::SearchNav::Post {
                post_id: "p1".into(),
            },
        );
        // The op is the detail-open door (resolve, then unseal if gated) — it
        // costs nothing on the wire for a post the timeline already holds, and
        // is what fetches one the feed never loaded, which is exactly what a
        // search hit can name.
        assert!(matches!(
            op,
            Some(crate::app::PageOp::Feed(
                crate::feed::Op::OpenPostDetail { .. }
            ))
        ));
        assert_eq!(app.page, Page::Feed);
        assert_eq!(
            app.feed.mode,
            crate::feed::Mode::PostDetail("p1".into()),
            "the feed page must land on the post the row named"
        );
    }

    /// A draft already tied to a thread opens that thread, exactly like
    /// clicking `conversation-item` would.
    #[test]
    fn activating_a_draft_with_a_thread_selects_that_thread_on_the_conversations_page() {
        let mut app = app_ready_to_navigate();
        let op = open_result(
            &mut app,
            fauna_client_search::SearchNav::Draft {
                thread_id: Some("t1".into()),
            },
        );
        // The only follow-up is re-deriving the opened thread's list-send view
        // (`refresh_list_send`), exactly as a `conversation-item` click runs it.
        assert!(matches!(
            op,
            Some(crate::app::PageOp::Conversations(
                crate::conversations::Op::RefreshListSend { .. }
            ))
        ));
        assert_eq!(app.page, Page::Conversations);
        assert_eq!(
            app.conversations.mode,
            crate::conversations::Mode::Detail(fauna_conversations::ThreadId("t1".into())),
        );
    }

    /// The thread-less draft is the single-slot new-thread composer —
    /// `StartNewConversation` opens/focuses it, preserving whatever is
    /// stashed there rather than resetting it.
    #[test]
    fn activating_a_threadless_draft_opens_the_new_thread_composer() {
        let mut app = app_ready_to_navigate();
        let op = open_result(
            &mut app,
            fauna_client_search::SearchNav::Draft { thread_id: None },
        );
        assert!(op.is_none());
        assert_eq!(app.page, Page::Conversations);
        assert_eq!(app.conversations.mode, crate::conversations::Mode::Compose);
    }

    /// `Mail` lands the thread jump — the half that always worked, kept as its
    /// own test so a regression in the jump is not masked by the marker.
    #[test]
    fn activating_a_mail_result_jumps_to_its_thread() {
        let mut app = app_ready_to_navigate();
        let op = open_result(
            &mut app,
            fauna_client_search::SearchNav::Mail {
                thread_id: "t2".into(),
                message_id: "m1".into(),
            },
        );
        assert!(op.is_none());
        assert_eq!(app.page, Page::Conversations);
        assert_eq!(
            app.conversations.mode,
            crate::conversations::Mode::Detail(fauna_conversations::ThreadId("t2".into())),
        );
    }

    /// A thread with `count` messages (`m-0`..), ready for a `Mail` navigation
    /// at it. Seeds through the page's own agent injector — the same door
    /// `conversations`' tests use — rather than hand-building wire structs.
    fn app_with_a_thread(count: usize) -> (App, fauna_conversations::ThreadId) {
        let app = app_ready_to_navigate();
        for i in 0..count {
            assert!(crate::conversations::inject_inbound(
                &app.conversations,
                &serde_json::json!({
                    "rail": "FaunaMls",
                    "sender": "carol@self-nest.test",
                    "body": format!("body {i}"),
                    "message_id": format!("m-{i}"),
                }),
            ));
        }
        let thread_id = app.conversations.snapshot().unwrap().threads[0]
            .thread_id
            .clone();
        (app, thread_id)
    }

    /// The user-visible half of the `message_id`: the thread does not merely
    /// open, it opens *showing* the hit. In tui the viewport follows the focus
    /// ring, so this asserts the ring landed inside the selected message —
    /// specifically on the first focusable element at or after its marker,
    /// which is that message's own reply button.
    ///
    /// Asserts the ring's TARGET, not its numeric index: an index assertion
    /// would pass for any bubble once the per-arm child counts shift.
    #[test]
    fn activating_a_mail_result_lands_the_focus_ring_on_the_selected_message() {
        let (mut app, thread_id) = app_with_a_thread(4);

        open_result(
            &mut app,
            fauna_client_search::SearchNav::Mail {
                thread_id: thread_id.0.clone(),
                message_id: "m-2".into(),
            },
        );

        let elements = app.page_elements();
        let focusable: Vec<_> = elements.iter().filter(|e| e.focusable()).collect();
        let landed = focusable.get(app.focus).expect("ring is on the page");
        assert_eq!(
            landed.id, "dm-reply-button",
            "the ring lands on the selected message's own first control"
        );

        // ...and it is the THIRD message's control, not the first's — the
        // property a "focus moved somewhere" assertion would miss entirely.
        let marker_at = elements
            .iter()
            .position(|e| {
                e.attrs.iter().any(|(k, v)| {
                    k == crate::conversations::SELECTED_MESSAGE_MARKER_ATTR && v == "true"
                })
            })
            .expect("the selected message paints its marker");
        assert_eq!(
            elements[..marker_at]
                .iter()
                .filter(|e| e.focusable())
                .count(),
            app.focus,
            "the ring sits at the marker, not at an earlier message"
        );
    }

    /// The ring is only moved for a real, placeable selection. A `Mail` hit on a
    /// message this thread does not hold still opens the thread (strictly better
    /// than nothing) and leaves the ring where it was, rather than parking it on
    /// an unrelated message.
    #[test]
    fn a_mail_result_naming_an_absent_message_opens_the_thread_and_moves_nothing() {
        let (mut app, thread_id) = app_with_a_thread(3);
        app.focus = 0;

        open_result(
            &mut app,
            fauna_client_search::SearchNav::Mail {
                thread_id: thread_id.0.clone(),
                message_id: "m-nowhere".into(),
            },
        );

        assert_eq!(
            app.conversations.mode,
            crate::conversations::Mode::Detail(thread_id),
            "the thread still opens",
        );
        assert_eq!(
            app.focus, 0,
            "the ring did not chase a selection that resolved to nothing"
        );
    }

    /// `Contact` opens the Address Book half of the Contacts page and hands
    /// back the **locate** op — the row carries a `uid_hash`, and the page opens
    /// cards by `card_id`, so the id-space join is a wire read rather than a
    /// cast (`address_book::locate_card`). The op is the whole point: without
    /// it the jump would silently render nothing whenever the card's book is
    /// not the one already loaded.
    #[test]
    fn activating_a_contact_result_opens_the_address_book_and_resolves_the_card() {
        let mut app = app_ready_to_navigate();
        // The locate is a network read, so the page needs its transport inputs
        // to build the op at all (the dummy-nest shape `contacts::tests` uses).
        app.contacts.nest = Some(fauna_client::NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        ));
        app.contacts.secret = Some([7u8; 32]);
        // The Address Book ops read the account's mail custody since
        // `fauna.state.mail` went plane-only; without a store they build no op.
        app.contacts.mail = Some(std::sync::Arc::new(
            fauna_client_config::test_helpers::FakeMailStore::empty(),
        ));
        let uid_hash = hex::encode([0xAB; 32]);
        let op = open_result(
            &mut app,
            fauna_client_search::SearchNav::Contact {
                uid_hash: uid_hash.clone(),
            },
        );
        match op {
            Some(crate::app::PageOp::Contacts(crate::contacts::Op::LocateCard {
                uid_hash: asked,
                ..
            })) => assert_eq!(
                asked, uid_hash,
                "the uid_hash must reach the locate verbatim — it is the index's own spelling"
            ),
            _ => panic!("expected the card-locate op on the contacts page"),
        }
        assert_eq!(app.page, Page::Contacts);
        assert_eq!(
            app.contacts.segment,
            crate::address_book::Segment::AddressBook,
            "the jump must land on the Address Book half, not the social roster"
        );
        assert!(
            app.contacts.open_card.is_none(),
            "nothing is open until the locate answers; a card_id is not knowable yet"
        );
    }

    /// `File` navigates to the Media page and asks it to open the item its
    /// durable pair names — a lookup, not a match against a rendered field.
    /// With no media machine attached (this app has none) the page still
    /// navigates and the lookup simply yields nothing, which is the honest
    /// pre-auth degrade every page here takes.
    #[test]
    fn activating_a_file_result_navigates_to_the_media_page() {
        let mut app = app_ready_to_navigate();
        let op = open_result(
            &mut app,
            fauna_client_search::SearchNav::File {
                folder_id: 1,
                path_hash: fauna_core::hex32::encode(&[0x11; 32]),
            },
        );
        assert!(
            op.is_none(),
            "no machine attached — nothing to load versions from"
        );
        assert_eq!(app.page, Page::Media);
    }

    /// The dispatcher-level twin of the above: `Gesture::OpenSearchResult`
    /// must actually reach `open_result` through the one gesture door
    /// (`app::gesture_work`), the same door `search-result-item`'s click
    /// paints through — a unit test calling `open_result` directly cannot
    /// catch a missing or misrouted match arm in `gesture_work`.
    #[test]
    fn the_open_search_result_gesture_reaches_open_result_through_the_gesture_door() {
        let mut app = app_ready_to_navigate();
        let work = crate::app::gesture_work(
            &mut app,
            Gesture::OpenSearchResult(fauna_client_search::SearchNav::Post {
                post_id: "p2".into(),
            }),
        );
        // Reaching `open_result` is what this asserts; the work it hands back is
        // the detail-open op (resolve, then unseal if gated).
        assert!(!matches!(work, crate::app::GestureWork::None));
        assert_eq!(app.page, Page::Feed);
        assert_eq!(app.feed.mode, crate::feed::Mode::PostDetail("p2".into()));
    }

    /// `result_element` must only paint a row as clickable when `open_result`
    /// can actually act on it — a target-less row stays `Role::Label` so a
    /// click never silently does nothing (an inert row must look inert, not
    /// merely behave that way). Every `SearchNav` variant is now navigable on
    /// tui, so `None` is the only inert case left.
    #[test]
    fn only_navigable_results_paint_as_gesture_buttons() {
        let app = app_showing(searched_with(vec![
            row_with_nav(fauna_client_search::SearchNav::Post {
                post_id: "p1".into(),
            }),
            row_with_nav(fauna_client_search::SearchNav::Contact {
                uid_hash: "deadbeef".into(),
            }),
            row_with_nav(fauna_client_search::SearchNav::File {
                folder_id: 1,
                path_hash: "ph1".into(),
            }),
            row("post", "no target"),
        ]));
        let roles: Vec<bool> = elements(&app)
            .iter()
            .filter(|e| e.id == "search-result-item")
            .map(|e| matches!(e.role, crate::element::Role::Button(_)))
            .collect();
        assert_eq!(
            roles,
            vec![true, true, true, false],
            "every targeted row is clickable; only the target-less row is inert"
        );
    }
}
