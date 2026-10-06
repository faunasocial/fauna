//! UniFFI façade for the shared, stateful Search page
//! (`fauna_client_search::SearchManager`) — the native-client (Windows / Apple /
//! Android) seam onto the snapshot that tui and the Rust-native Linux app
//! drive as `SearchManager<Arc<NestClient>>` directly, and that web reaches via
//! `fauna-wasm` (`docs/goal/ui/search.md` § State & data shape, ratified
//! 2026-08-02).
//!
//! [`SearchManager<R>`] is **generic over the WS-RPC transport** and a generic
//! type can't be `#[uniffi::export]`ed, so — exactly like [`FfiFeedManager`] —
//! the export lives on this **concrete** façade wrapping
//! `SearchManager<Arc<NestClient>>`. Construct via
//! [`crate::nest_client::FfiNestClient::search_manager`].
//!
//! ## What adopting this façade buys a leg
//!
//! The manager owns the query / type-filter / paging / merge decisions and
//! publishes them as one [`SearchSnapshot`]; an adopting page becomes a paint
//! shell, exactly as tui did (`search.md` § Implementation status today). In
//! particular the leg **stops holding a page limit of its own**: `has_more` must
//! be derived from the *nest arm's* row count, which is no longer visible to the
//! app once local rows are merged into `results`, so an app deriving it from the
//! rows it can see would offer a page the wire cannot produce (`search.md`
//! § Where logic lives — *Who holds the limit*, settled 2026-08-03). An
//! un-migrated leg keeps its own `SearchPaging` and calls the free-function
//! faces in [`crate::search`]; a leg on this façade reads `has_more` and holds
//! no limit at all. The two surfaces coexist deliberately.
//!
//! ## Scope: both arms — but the local one is registered from the client
//!
//! There is intentionally **no exported `set_local_index` face here.** Backend 2
//! is registered by handing the manager an `Arc<dyn LocalSearchIndex>`, and that
//! trait is deliberately *not* UniFFI-exported: the implementation
//! (`fauna_client_index::MailLocalSearch`) is minted Rust-side by
//! `fauna_client_conversations::NestMailIndexLauncher::local_search_index`,
//! which owns the MSEK and hands app glue an opaque index — **never a key**
//! (`content-index.md` § Encryption posture). Exporting the seam as a foreign
//! trait would invert that and let app glue supply its own index.
//!
//! So the local arm is registered **through the client that owns the launcher**:
//! [`FfiNestClient::attach_local_search_index`] (since 2026-08-03), the FFI twin
//! of the two lines tui runs at its post-auth hook. The key never crosses the
//! boundary in either shape; what changed is only that the app now has a way to
//! ask for the arm at all.
//!
//! Until that call lands — and on an actor with no mail — a leg on this façade
//! runs **nest-only**, which `search.md` calls a normal state and not an error
//! state: the same rendered page as a device with no published index.
//!
//! [`FfiNestClient::attach_local_search_index`]: crate::nest_client::FfiNestClient::attach_local_search_index
//!
//! ## Why the whole module is gated `search-manager`
//!
//! [`SearchSnapshot`] (with its `SearchResultRow` / `SearchSource` / `SearchNav`
//! members) and the [`SearchSnapshotObserver`] foreign trait are
//! `fauna_client_search` types returned **directly** — no fauna-ffi-local
//! mirrors (priority #2). `uniffi-bindgen-go` emits an uncompilable bare
//! `fauna_client_search` cross-namespace import for them, so the feature is
//! **default-on for the native app FFI** and **off in the Go mail-bridge
//! `--no-default-features` build** — the bridge is a server with no Search UI.
//! Identical gating shape and rationale to `feed-manager`. The feature also
//! turns on `fauna-client-search/uniffi` so those types get their
//! `uniffi::Record`/`Enum` registration.
//!
//! Note this is a strictly *additive* sibling of [`crate::search_client`]'s
//! `FfiSearchClient` (the raw `fauna.search.query` transport) and
//! [`crate::search`]'s render/paging free functions — neither changes, because
//! the un-migrated legs still depend on them.
//!
//! [`SearchManager<R>`]: fauna_client_search::SearchManager
//! [`FfiFeedManager`]: crate::feed_manager::FfiFeedManager

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_search::{SearchManager, SearchSnapshot, SearchSnapshotObserver};

/// UniFFI handle wrapping the shared, stateful `SearchManager<Arc<NestClient>>`.
/// Methods are exposed to Swift as `async`/`func`, Kotlin as `suspend fun`/`fun`,
/// and C# as `Task`/sync. The snapshot read + observer wiring + cancel are
/// synchronous; the two user actions that drive a WS-RPC call are async.
#[derive(uniffi::Object)]
pub struct FfiSearchManager {
    inner: Arc<SearchManager<Arc<NestClient>>>,
}

impl FfiSearchManager {
    /// Build over an authed [`NestClient`]. Unlike the Feed manager this needs
    /// **no actor secret** — searching signs nothing; it reads.
    ///
    /// The snapshot starts [`SearchSnapshot::default`] (empty query ⇒ the
    /// pre-search rendering), and stays there until the page fires
    /// [`run_query`](Self::run_query).
    pub(crate) fn new(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(SearchManager::new(nest)),
        })
    }

    /// Register backend 2 (the sealed local index) on the wrapped manager.
    ///
    /// **Deliberately not `#[uniffi::export]`ed** — the only legitimate caller is
    /// [`FfiNestClient::attach_local_search_index`], which mints the index from
    /// the launcher holding the MSEK. Exposing it would mean exporting
    /// `LocalSearchIndex` as a foreign trait and letting app glue supply its own
    /// index, inverting the ownership `content-index.md` § Encryption posture
    /// requires (module docs § Scope).
    ///
    /// [`FfiNestClient::attach_local_search_index`]: crate::nest_client::FfiNestClient::attach_local_search_index
    #[cfg(feature = "conversations-session")]
    pub(crate) fn set_local_index(&self, local: Arc<dyn fauna_client_search::LocalSearchIndex>) {
        self.inner.set_local_index(local);
    }

    /// Register the source that mints the local arm later — what makes
    /// [`FfiNestClient::attach_local_search_index`] survive mail being enabled
    /// after login. Internal for the same reason `set_local_index` is: the arm
    /// and its resolver are minted Rust-side, and neither crosses the FFI
    /// boundary.
    ///
    /// [`FfiNestClient::attach_local_search_index`]: crate::nest_client::FfiNestClient::attach_local_search_index
    pub(crate) fn set_local_index_resolver(
        &self,
        resolver: Arc<dyn fauna_client_search::LocalIndexResolver>,
    ) {
        self.inner.set_local_index_resolver(resolver);
    }
}

// ── Synchronous surface ──────────────────────────────────────────────────────
// Snapshot read + observer reactivity + cancel — no WS-RPC, so no async.
#[uniffi::export]
impl FfiSearchManager {
    /// A cheap clone of the current [`SearchSnapshot`]. The client's
    /// [`SearchSnapshotObserver`] re-reads this on every `on_changed()`.
    ///
    /// Exactly one of the three page states renders at any moment, and all three
    /// are derived here rather than per app: not-searched-yet (`query` empty),
    /// still-loading (`in_flight`), and searched-and-found-nothing
    /// (`no_results`).
    pub fn snapshot(&self) -> SearchSnapshot {
        self.inner.snapshot()
    }

    /// Subscribe to state changes. The observer re-reads
    /// [`snapshot`](Self::snapshot) on each notification — the same reactivity
    /// contract the Feed and Conversations pages already use.
    pub fn add_observer(&self, observer: Arc<dyn SearchSnapshotObserver>) {
        self.inner.add_observer(observer);
    }

    /// Drop all registered observers — call at sign-out so stale receiver loops
    /// close (the `FeedManager`/`ConversationsManager` contract).
    pub fn clear_observers(&self) {
        self.inner.clear_observers();
    }

    /// `search-cancel-button` — reset the page to its pre-search state.
    ///
    /// Synchronous, but **not** merely a local clear: it bumps the manager's
    /// query generation, so a query still in flight lands on a cancelled page
    /// and is dropped rather than repopulating results the user just dismissed.
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// Whether backend 2 (the local sealed index) is registered — `false` until
    /// [`FfiNestClient::attach_local_search_index`] registers this login's arm,
    /// and permanently `false` for an actor with no mail.
    ///
    /// **Diagnostics only.** A client must not branch its rendering on it; a
    /// missing local arm is *no local rows*, never an error.
    ///
    /// [`FfiNestClient::attach_local_search_index`]: crate::nest_client::FfiNestClient::attach_local_search_index
    pub fn has_local_index(&self) -> bool {
        self.inner.has_local_index()
    }
}

// ── Asynchronous surface ─────────────────────────────────────────────────────
// The two user actions that drive a WS-RPC call. Neither returns a `Result`:
// a failing arm lands in the snapshot's `error` field while the other arm's
// rows stay on screen, so partial results are shown honestly rather than
// blanked (`search.md` § The local/nest merge) — the page reads the error from
// the snapshot exactly as tui and linux do.
#[fauna_uniffi_async::export]
impl FfiSearchManager {
    /// Fire a search: `search-submit-button`, and `search-type-filter` when the
    /// filter changes.
    ///
    /// `query` is the page's **live** query-field buffer — the field stays plain
    /// local state on every app and submit reads it explicitly (the page is
    /// submit-driven, no debounce), so a keystroke never implies network work.
    /// A blank query is a no-op that leaves the page untouched — not searched,
    /// not errored. `type_filter` is `TYPE_FILTER_ALL` or a nest `content_type`;
    /// the manager applies it to both arms through one shared mapping.
    pub async fn run_query(&self, query: String, type_filter: String) {
        self.inner.run_query(&query, &type_filter).await;
    }

    /// `search-load-more-button` — re-fire the **last fired** query (not the
    /// live buffer, which the user may have edited since) with a grown page.
    ///
    /// A no-op when nothing has been searched or the affordance isn't offered,
    /// so a stale click can't ask the nest for a page it already proved empty.
    /// The grown limit comes from the shared paging policy and saturates at the
    /// nest's ceiling — which is precisely the ceiling bug every un-migrated leg
    /// still carries.
    pub async fn load_more(&self) {
        self.inner.load_more().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nest_client::FfiNestClient;
    use fauna_client_search::TYPE_FILTER_ALL;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Build a façade over an unconnected client. Every assertion below is about
    /// state the manager owns locally, so no transport is needed — and nothing
    /// here dials the URL.
    fn manager() -> Arc<FfiSearchManager> {
        let nest = FfiNestClient::new("wss://127.0.0.1:0/ws".into(), vec![7u8; 32]).unwrap();
        nest.search_manager()
    }

    struct CountingObserver(AtomicUsize);
    impl SearchSnapshotObserver for CountingObserver {
        fn on_changed(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A fresh façade is in the pre-search state: the gate every app's
    /// `search-cancel-button` and results/no-results pair render behind.
    #[test]
    fn fresh_manager_is_in_the_pre_search_state() {
        let m = manager();
        let s = m.snapshot();

        assert_eq!(s.query, "", "no search has been fired");
        assert!(!s.searched());
        assert_eq!(
            s.type_filter, TYPE_FILTER_ALL,
            "the filter defaults to the shared ALL token, not a per-app literal"
        );
        assert!(s.results.is_empty());
        assert!(!s.in_flight);
        assert!(
            !s.no_results,
            "not-searched-yet is distinct from found-nothing"
        );
        assert!(!s.has_more);
        assert!(s.error.is_none());
    }

    /// A fresh façade has no local arm — `attach_local_search_index` has not run
    /// (and on an actor with no mail never will). Pins that a leg adopting the
    /// manager gets an honest `false` rather than a surface suggesting backend 2
    /// is live.
    #[test]
    fn local_index_is_absent_until_one_is_registered() {
        assert!(!manager().has_local_index());
    }

    /// The inverse, and the pin row 8's Success clause names: once the client
    /// registers this login's arm, the façade reports it.
    ///
    /// The registration path is `pub(crate)` on purpose (only
    /// `FfiNestClient::attach_local_search_index` may mint an index — module docs
    /// § Scope), so the test drives that same seam with a stand-in index rather
    /// than a foreign trait no app can supply.
    ///
    /// Gated exactly like the seam it drives — the Go `--no-default-features`
    /// build has no conversations session and therefore no way to mint an index.
    #[cfg(feature = "conversations-session")]
    #[test]
    fn a_registered_local_arm_is_visible_through_the_facade() {
        struct NoRows;
        #[async_trait::async_trait]
        impl fauna_client_search::LocalSearchIndex for NoRows {
            async fn query(
                &self,
                _query: &str,
                _kinds: &[fauna_client_search::SearchKindClass],
                _limit: usize,
            ) -> Result<Vec<fauna_client_search::LocalSearchHit>, String> {
                Ok(vec![])
            }
        }

        let m = manager();
        assert!(!m.has_local_index(), "precondition: nest-only to start");

        m.set_local_index(Arc::new(NoRows));

        assert!(
            m.has_local_index(),
            "the façade must report the arm the client registered — a leg that \
             cannot see its own registration cannot tell backend 2 apart from a \
             silently dropped one"
        );
    }

    /// The observer faces delegate to the real manager: `cancel` notifies
    /// through the façade, and `clear_observers` actually detaches. A façade
    /// that dropped the observer on the floor (or kept its own list) fails here.
    #[test]
    fn observer_faces_delegate_and_detach() {
        let m = manager();
        let obs = Arc::new(CountingObserver(AtomicUsize::new(0)));
        m.add_observer(obs.clone());

        m.cancel();
        assert_eq!(
            obs.0.load(Ordering::SeqCst),
            1,
            "cancel must notify through the façade"
        );

        m.clear_observers();
        m.cancel();
        assert_eq!(
            obs.0.load(Ordering::SeqCst),
            1,
            "a cleared observer must stop receiving — the sign-out contract"
        );
    }

    /// `snapshot()` reads **through** to the manager rather than returning a
    /// copy cached at construction. Uses the shared test-helper injection seam,
    /// so it pins the read path without a transport.
    #[test]
    fn snapshot_reads_through_to_the_manager() {
        let m = manager();
        // Struct-update form, so a later field added to `SearchSnapshot` merges
        // cleanly here instead of colliding on the grown axis.
        let injected = SearchSnapshot {
            query: "quarterly report".into(),
            no_results: true,
            ..Default::default()
        };

        m.inner.set_snapshot_for_test(injected.clone());

        let read = m.snapshot();
        assert_eq!(read, injected, "the façade must not cache a stale snapshot");
        assert!(read.searched());
    }

    /// A blank query is a no-op all the way through the façade — it must not
    /// mark the page searched, and must not notify. (Whitespace-only counts as
    /// blank; the manager trims.) This is also the one async face reachable
    /// without a transport, so it pins that the argument order survives the
    /// boundary: passing the filter as the query would leave the page searched.
    #[tokio::test]
    async fn blank_query_is_a_no_op_through_the_facade() {
        let m = manager();
        let obs = Arc::new(CountingObserver(AtomicUsize::new(0)));
        m.add_observer(obs.clone());

        m.run_query("   ".into(), TYPE_FILTER_ALL.into()).await;

        assert!(!m.snapshot().searched(), "a blank query never searches");
        assert!(!m.snapshot().in_flight);
        assert_eq!(
            obs.0.load(Ordering::SeqCst),
            0,
            "a blank query must not notify observers"
        );
    }

    /// `load_more` is a no-op when nothing has been searched — a stale click
    /// can't ask the nest for a page. Reachable without a transport precisely
    /// because the guard short-circuits before the call.
    #[tokio::test]
    async fn load_more_without_a_search_is_a_no_op() {
        let m = manager();
        let obs = Arc::new(CountingObserver(AtomicUsize::new(0)));
        m.add_observer(obs.clone());

        m.load_more().await;

        assert!(!m.snapshot().searched());
        assert_eq!(obs.0.load(Ordering::SeqCst), 0);
    }
}
