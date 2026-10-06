//! The shared **`SearchManager`** — the one object the Search page renders from
//! on every app (`docs/goal/ui/search.md` § State & data shape, ratified
//! 2026-08-02; built as rollout slice S3 piece 5).
//!
//! Before this, each of the seven apps composed its own result-card state from
//! the raw `fauna.search.query` reply plus the shared render helpers, and each
//! held its own copy of the query/filter/paging decisions. The manager owns all
//! of it — firing both backends, merging them into one ordered deduplicated
//! list, and deriving the load-more affordance — so an app is reduced to
//! painting [`SearchSnapshot`](crate::SearchSnapshot) and forwarding gestures
//! (`content-index.md` § What a client has to know: "No client composes
//! result-card text or makes ranking/filter decisions itself").
//!
//! # The two arms
//!
//! **Backend 1** is `fauna.search.query` over the nest's floor-derived
//! `content_fts` — every nest answers it, and it is the only arm the web SPA
//! has. **Backend 2** is this device's sealed per-user tantivy replica, reached
//! through the [`LocalSearchIndex`](crate::LocalSearchIndex) seam (absent on
//! wasm by construction — see that module). They fire **concurrently**, and
//! `in_flight` holds until both settle, so the page never reports "no results"
//! while an arm is still out.
//!
//! Neither arm can blank the other: a failure on either side sets `error` and
//! keeps the other's rows, because partial results shown honestly beat an empty
//! page (§ The local/nest merge).
//!
//! # A generic, like `FeedManager`
//!
//! `SearchManager<R>` is generic over the WS-RPC transport, so native call
//! sites pass `Arc<NestClient>` and the wasm SPA passes its `WsRpcClient`. A
//! generic type can't be `#[uniffi::export]`ed, so the FFI face for
//! windows/apple/android is a concrete façade in `fauna-ffi`, exactly as
//! `FfiFeedManager` façades `FeedManager`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use fauna_core::localized::LocalizedText;
use fauna_protocol::RpcRequester;
use fauna_protocol::search::{SearchQueryRequest, SearchResult};

use crate::SearchClient;
use crate::kind::{self, SearchKindClass};
use crate::local::{LocalIndexResolver, LocalSearchHit, LocalSearchIndex};
use crate::observer::SearchSnapshotObserver;
use crate::paging::SearchPaging;
use crate::render::clean_snippet;
use crate::snapshot::{SearchNav, SearchResultRow, SearchSnapshot, SearchSource};

/// The shared search manager. Construct over an authed transport; register a
/// [`LocalSearchIndex`] afterwards if this app has one.
pub struct SearchManager<R> {
    nest: R,
    /// Backend 2, if this app has it. `None` is a **normal** state, not a
    /// degraded one: the web SPA never has one, and a native app has none until
    /// its index replica opens (mail not enabled, a transient rail failure, a
    /// fresh device). A manager without it simply runs nest-only.
    local: RwLock<Option<Arc<dyn LocalSearchIndex>>>,
    /// Mints `local` once its precondition exists. Held beside the arm rather
    /// than replacing it because registration is what moved, not the arm: glue
    /// that already has an arm still uses `set_local_index`.
    resolver: RwLock<Option<Arc<dyn LocalIndexResolver>>>,
    state: RwLock<SearchSnapshot>,
    /// The shared cursor-less paging policy (`search.md` § Where logic lives).
    /// Held **here**, not in each app's view model, because `has_more` must be
    /// derived from the **nest arm's** row count — a number only the manager
    /// sees once local rows are merged in.
    paging: RwLock<SearchPaging>,
    /// Query generation — bumped when a query is fired. A reply commits only if
    /// its generation is still current; a superseded one is DROPPED. Without
    /// it, a slow first query landing after a faster second one clobbers it,
    /// leaving the committed `query`/`type_filter` describing rows that came
    /// from a different question (the `FeedManager::reload_gen` bug, whose
    /// apple red was committed-search-with-unfiltered-posts).
    query_gen: AtomicU64,
    observers: RwLock<Vec<Arc<dyn SearchSnapshotObserver>>>,
}

impl<R: RpcRequester + Clone> SearchManager<R> {
    pub fn new(nest: R) -> Self {
        Self {
            nest,
            local: RwLock::new(None),
            resolver: RwLock::new(None),
            state: RwLock::new(SearchSnapshot::default()),
            paging: RwLock::new(SearchPaging::initial()),
            query_gen: AtomicU64::new(0),
            observers: RwLock::new(Vec::new()),
        }
    }

    /// Register (or replace) backend 2.
    ///
    /// Separate from construction because opening the sealed replica is async
    /// and happens after login, while the page — and its manager — exist from
    /// the moment the user can navigate to Search.
    pub fn set_local_index(&self, local: Arc<dyn LocalSearchIndex>) {
        *self.local.write().unwrap() = Some(local);
    }

    /// Register the source that can mint backend 2 **later** — the shape app
    /// glue should prefer over [`Self::set_local_index`].
    ///
    /// A login cannot always produce the arm: opening the sealed replica needs
    /// an MSEK that does not exist until mail is enabled, so a user enabling
    /// mail after login left the one-shot registration with nothing to register
    /// and the page nest-only for the life of the process
    /// ([`LocalIndexResolver`] carries the full reasoning). Registering the
    /// resolver is synchronous and always possible, and the manager resolves on
    /// the first query that finds no arm.
    pub fn set_local_index_resolver(&self, resolver: Arc<dyn LocalIndexResolver>) {
        *self.resolver.write().unwrap() = Some(resolver);
    }

    /// Whether backend 2 is registered. Diagnostics only — the page renders the
    /// same either way (a missing local arm is *no local rows*, never an error).
    pub fn has_local_index(&self) -> bool {
        self.local.read().unwrap().is_some()
    }

    /// Ask the registered resolver for the arm, caching a `Some`.
    ///
    /// The lock is never held across the `await` — the resolver opens the sealed
    /// replica, which is real I/O, and this runs on the query path. A race
    /// between two first queries can therefore mint the arm twice; the loser's
    /// copy is simply dropped by the last writer, which is harmless because an
    /// arm is a read-only view over the published segments.
    async fn resolve_local_arm(&self) -> Option<Arc<dyn LocalSearchIndex>> {
        let resolver = { self.resolver.read().unwrap().clone() }?;
        let local = resolver.resolve().await?;
        *self.local.write().unwrap() = Some(Arc::clone(&local));
        Some(local)
    }

    // ── Reactivity ───────────────────────────────────────────────

    /// A cheap clone of the current state. The observer reads this on every
    /// `on_changed()`.
    pub fn snapshot(&self) -> SearchSnapshot {
        self.state.read().unwrap().clone()
    }

    pub fn add_observer(&self, obs: Arc<dyn SearchSnapshotObserver>) {
        self.observers.write().unwrap().push(obs);
    }

    /// Drop all registered observers — call at sign-out so stale receiver loops
    /// close (the `FeedManager`/`ConversationsManager` contract).
    pub fn clear_observers(&self) {
        self.observers.write().unwrap().clear();
    }

    fn notify(&self) {
        for o in self.observers.read().unwrap().iter() {
            o.on_changed();
        }
    }

    // ── User actions (`search.md` § User actions) ────────────────

    /// Fire a search: `search-submit-button`, and `search-type-filter` when the
    /// filter changes.
    ///
    /// `query` is the app's **live** query-field buffer — the field is plain
    /// local state on every app and submit reads it explicitly, so a
    /// keystroke never implies network work (§ Where logic lives — *Search
    /// debouncing*: submit-driven, no debounce). A blank query is a no-op on
    /// every app's search bar, and leaves the page untouched — not searched,
    /// not errored.
    pub async fn run_query(&self, query: &str, type_filter: &str) {
        let q = query.trim();
        if q.is_empty() {
            return;
        }
        let limit = {
            let mut p = self.paging.write().unwrap();
            p.reset();
            p.limit()
        };
        let generation = self.begin(q, type_filter);
        self.fetch_and_commit(q.to_string(), type_filter.to_string(), limit, generation)
            .await;
    }

    /// `search-load-more-button` — re-fire the **last fired** query (not the
    /// live buffer, which the user may have edited since) with a grown page.
    ///
    /// A no-op when nothing has been searched or the affordance isn't offered,
    /// so a stale click can't ask the nest for a page it already proved empty.
    pub async fn load_more(&self) {
        let (q, filter) = {
            let s = self.state.read().unwrap();
            if s.query.is_empty() || !s.has_more {
                return;
            }
            (s.query.clone(), s.type_filter.clone())
        };
        let limit = {
            let mut p = self.paging.write().unwrap();
            p.load_more();
            p.limit()
        };
        let generation = self.begin(&q, &filter);
        self.fetch_and_commit(q, filter, limit, generation).await;
    }

    /// `search-cancel-button` — reset the page to its pre-search state.
    ///
    /// Bumps the generation, so a query still in flight lands on a cancelled
    /// page and is dropped rather than repopulating results the user just
    /// dismissed.
    pub fn cancel(&self) {
        self.query_gen.fetch_add(1, Ordering::SeqCst);
        self.paging.write().unwrap().reset();
        *self.state.write().unwrap() = SearchSnapshot::default();
        self.notify();
    }

    // ── Internals ────────────────────────────────────────────────

    /// Open a query: commit the question, clear the last error, mark in-flight.
    /// Returns the generation the reply must still match to commit.
    ///
    /// Results are deliberately **left in place** while the new query runs —
    /// every app already renders the previous page until the next one lands,
    /// and blanking them would flash an empty list on every keystroke-free
    /// re-fire (a filter change, a load-more).
    fn begin(&self, query: &str, type_filter: &str) -> u64 {
        let generation = self.query_gen.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut s = self.state.write().unwrap();
            s.query = query.to_string();
            s.type_filter = type_filter.to_string();
            s.in_flight = true;
            s.no_results = false;
            s.error = None;
        }
        self.notify();
        generation
    }

    async fn fetch_and_commit(&self, query: String, filter: String, limit: i64, generation: u64) {
        let kinds = kind::local_kind_classes(&filter);
        let (nest, local) = futures_util::future::join(
            self.query_nest(&query, &filter, limit),
            self.query_local(&query, kinds, limit),
        )
        .await;

        // Superseded by a newer query (or by a cancel) while we were out —
        // drop this reply rather than committing an answer to a stale question.
        if self.query_gen.load(Ordering::SeqCst) != generation {
            return;
        }

        let mut failures: Vec<String> = Vec::new();
        let nest_rows = unwrap_arm(nest, &mut failures);
        let local_rows = unwrap_arm(local, &mut failures);

        // `has_more` speaks for the **nest** arm alone: the local arm is
        // unpaged (one shot against a local index), so counting its rows here
        // would offer a page the wire cannot produce.
        let has_more = self.paging.read().unwrap().has_more(nest_rows.len());
        let results = merge_rows(nest_rows, local_rows);

        {
            let mut s = self.state.write().unwrap();
            s.in_flight = false;
            s.has_more = has_more;
            s.no_results = results.is_empty();
            s.results = results;
            s.error = arm_failure_text(&failures);
        }
        self.notify();
    }

    async fn query_nest(
        &self,
        query: &str,
        filter: &str,
        limit: i64,
    ) -> Result<Vec<SearchResult>, String> {
        SearchClient::new(self.nest.clone())
            .query(SearchQueryRequest {
                query: query.to_string(),
                content_type: kind::nest_content_type(filter),
                before: None,
                after: None,
                limit: Some(limit),
                offset: None,
                extra: Default::default(),
            })
            .await
            .map(|reply| reply.results)
            .map_err(|e| e.to_string())
    }

    async fn query_local(
        &self,
        query: &str,
        kinds: &[SearchKindClass],
        limit: i64,
    ) -> Result<Vec<LocalSearchHit>, String> {
        // No registered index, or a filter that selects nothing local: no rows,
        // not an error. Skipping here is also what lets the seam promise its
        // implementations a non-empty `kinds`.
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let local = { self.local.read().unwrap().clone() };
        // Resolve on demand when this login registered a resolver but had no arm
        // to give at the time (mail enabled after login), or gave one still
        // awaiting that precondition (a master-only arm: no mail reader, no
        // `Contact` claim — `LocalSearchIndex::awaits_precondition`). Only
        // reached while the arm is absent or partial, so such a page pays one
        // resolver call per query and a complete one pays none; a resolve that
        // yields nothing keeps the partial arm it had.
        let local = match local {
            Some(local) if !local.awaits_precondition() => Some(local),
            cached => self.resolve_local_arm().await.or(cached),
        };
        let Some(local) = local else {
            return Ok(Vec::new());
        };
        local.query(query, kinds, limit.max(0) as usize).await
    }
}

// ── e2e / unit test-helper surface ───────────────────────────────────────
//
// The search twin of `FeedManager::set_feed_snapshot_for_test`, carrying the
// same two-gate rule: **visibility** is `any(test, debug_assertions, feature =
// "test-helpers")` so a plain debug build of an in-process consumer reaches it,
// while a release build strips it entirely — the automation surface is compiled
// out of release artifacts (cross-app e2e convention 15), never merely gated at
// runtime.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl<R: RpcRequester + Clone> SearchManager<R> {
    /// Install a snapshot directly, bypassing both arms.
    ///
    /// Lets a client's page tests assert their *projection* of a given state —
    /// which is all a page is, once the manager owns the behaviour — without
    /// standing up a transport. The manager's own behaviour is pinned by its
    /// tests here, not by re-driving it from each of seven apps.
    pub fn set_snapshot_for_test(&self, snapshot: SearchSnapshot) {
        *self.state.write().unwrap() = snapshot;
        self.notify();
    }
}

/// Fold one arm's outcome into rows, recording a failure instead of losing it.
fn unwrap_arm<T>(arm: Result<Vec<T>, String>, failures: &mut Vec<String>) -> Vec<T> {
    match arm {
        Ok(rows) => rows,
        Err(e) => {
            failures.push(e);
            Vec::new()
        }
    }
}

/// The page error for whichever arms failed — `None` when both succeeded.
///
/// Carries the reason so a failure diagnoses itself on `error-message` rather
/// than reading as an empty result set (cross-app e2e convention 6).
fn arm_failure_text(failures: &[String]) -> Option<LocalizedText> {
    if failures.is_empty() {
        return None;
    }
    Some(LocalizedText::key_arg(
        "search_page.search_failed_reason",
        "reason",
        failures.join("; "),
    ))
}

/// The merge (`search.md` § The local/nest merge).
///
/// **Dedup by identity, local wins.** The key is `(kind class, content_id)`:
/// for post-class rows the nest's `content_id` *is* the post id (the ratified
/// wire contract), so a locally-indexed twin collides with its nest row and the
/// local one is kept — it carries navigation and the private-corpus snippet.
/// Classing the key is what makes it safe across two id vocabularies; within a
/// class every writer mints ids under its own domain-separated scheme
/// (`search.md` § The page's wire surface), so an id can only collide with its
/// own twin.
///
/// **Ordering: BM25-family score descending**, tie-broken by recency then id so
/// the list is deterministic given both replies. The nest's `rank` is fixed-point
/// micro-units (the dag-cbor wire forbids floats), so `rank / 1e6` restores the
/// same statistic the local arm reports directly.
fn merge_rows(
    nest_rows: Vec<SearchResult>,
    local_rows: Vec<LocalSearchHit>,
) -> Vec<SearchResultRow> {
    let mut scored: Vec<(f64, SearchResultRow)> =
        Vec::with_capacity(nest_rows.len() + local_rows.len());
    let mut seen: std::collections::HashSet<(SearchKindClass, String)> =
        std::collections::HashSet::new();

    // Local first, so its rows win the dedup by construction.
    for hit in local_rows {
        let key = (kind::kind_class(&hit.content_type), hit.content_id.clone());
        if !seen.insert(key) {
            continue;
        }
        scored.push((hit.score as f64, local_row(hit)));
    }
    for row in nest_rows {
        // The nest emits lowercase hex (`db/fts.rs`), the spelling a local
        // arm's ids use, so the raw string is a sound dedup key.
        let key = (kind::kind_class(&row.content_type), row.content_id.clone());
        if !seen.insert(key) {
            continue;
        }
        scored.push((row.rank as f64 / 1e6, nest_row(row)));
    }

    scored.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then(b.1.timestamp.cmp(&a.1.timestamp))
            .then(a.1.content_id.cmp(&b.1.content_id))
    });
    scored.into_iter().map(|(_, row)| row).collect()
}

/// Project a backend-1 row. Navigation is minted only for the post class, whose
/// `content_id` is the real post id; profile and bridge rows hash their natural
/// id one-way and stay honestly inert until the additive wire field ships.
fn nest_row(r: SearchResult) -> SearchResultRow {
    let content_id = r.content_id;
    let navigation = match kind::kind_class(&r.content_type) {
        SearchKindClass::Post => Some(SearchNav::Post {
            post_id: content_id.clone(),
        }),
        _ => None,
    };
    SearchResultRow {
        badge: kind::badge_for(&r.content_type),
        snippet: clean_snippet(&r.snippet),
        // The wire carries epoch micros.
        timestamp: r.created_at / 1_000,
        source: SearchSource::Nest,
        navigation,
        content_id,
        content_type: r.content_type,
    }
}

/// Project a backend-2 hit. Snippet and navigation are already resolved behind
/// the seam (only the native side has the content stores to do it), so this is
/// a pure re-shape.
fn local_row(h: LocalSearchHit) -> SearchResultRow {
    SearchResultRow {
        badge: kind::badge_for(&h.content_type),
        snippet: h.snippet,
        timestamp: h.timestamp,
        source: SearchSource::Local,
        navigation: h.navigation,
        content_id: h.content_id,
        content_type: h.content_type,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nest_hit(
        content_type: &str,
        content_id: &str,
        rank: i64,
        created_at_us: i64,
    ) -> SearchResult {
        SearchResult {
            content_type: content_type.into(),
            content_id: content_id.into(),
            created_at: created_at_us,
            rank,
            snippet: "a <b>hit</b>".into(),
            extra: Default::default(),
        }
    }

    fn local_hit(content_type: &str, content_id: &str, score: f32, ts_ms: i64) -> LocalSearchHit {
        LocalSearchHit {
            content_id: content_id.into(),
            content_type: content_type.into(),
            snippet: "local body".into(),
            timestamp: ts_ms,
            score,
            navigation: Some(SearchNav::Mail {
                thread_id: "t-1".into(),
                message_id: content_id.into(),
            }),
        }
    }

    #[test]
    fn nest_rows_are_projected_with_the_shared_badge_and_snippet_cleanup() {
        let rows = merge_rows(
            vec![nest_hit("post/article", "abc", 2_000_000, 5_000_000)],
            vec![],
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].badge.key, "search_page.badge_post");
        assert_eq!(rows[0].snippet, "a hit", "FTS markers must be stripped");
        assert_eq!(
            rows[0].timestamp, 5_000,
            "micros on the wire → millis on the row"
        );
        assert_eq!(rows[0].source, SearchSource::Nest);
    }

    /// The ratified `content_id` contract: a post-class row deep-links by its
    /// own id; every other nest class stays inert because its id is a one-way
    /// hash (`search.md` § The page's wire surface).
    #[test]
    fn only_post_class_nest_rows_are_navigable() {
        let rows = merge_rows(
            vec![
                nest_hit("post", "post-id", 1_000_000, 0),
                nest_hit("profile", "hashed", 1_000_000, 0),
            ],
            vec![],
        );
        let post = rows.iter().find(|r| r.content_type == "post").unwrap();
        assert_eq!(
            post.navigation,
            Some(SearchNav::Post {
                post_id: "post-id".into()
            })
        );
        let profile = rows.iter().find(|r| r.content_type == "profile").unwrap();
        assert_eq!(
            profile.navigation, None,
            "a one-way hashed id cannot deep-link"
        );
    }

    /// Ordering is BM25-family score descending across BOTH arms — the nest's
    /// fixed-point `rank / 1e6` and the local arm's float are the same
    /// statistic, which is the whole basis for one merged list.
    #[test]
    fn rows_from_both_arms_interleave_by_score() {
        let rows = merge_rows(
            vec![
                nest_hit("post", "high-nest", 9_000_000, 0),
                nest_hit("post", "low-nest", 1_000_000, 0),
            ],
            vec![local_hit("mail", "mid-local", 5.0, 0)],
        );
        let ids: Vec<&str> = rows.iter().map(|r| r.content_id.as_str()).collect();
        assert_eq!(ids, vec!["high-nest", "mid-local", "low-nest"]);
    }

    /// Dedup is by `(class, id)` and the LOCAL row wins — it carries navigation
    /// and the private-corpus snippet.
    #[test]
    fn a_local_row_wins_the_dedup_against_its_nest_twin() {
        let rows = merge_rows(
            vec![nest_hit("post", "same-id", 9_000_000, 0)],
            vec![local_hit("post", "same-id", 0.1, 0)],
        );
        assert_eq!(rows.len(), 1, "the twin must collapse to one row");
        assert_eq!(rows[0].source, SearchSource::Local);
        assert_eq!(rows[0].snippet, "local body");
    }

    /// The key is CLASSED, so the same id in two different classes is two
    /// different things and must not collapse.
    #[test]
    fn the_same_id_in_two_classes_is_two_rows() {
        let rows = merge_rows(
            vec![nest_hit("post", "shared", 9_000_000, 0)],
            vec![local_hit("mail", "shared", 5.0, 0)],
        );
        assert_eq!(rows.len(), 2);
    }

    /// Determinism given both replies: equal scores tie-break by recency, then
    /// by id — never by arrival order.
    #[test]
    fn equal_scores_tie_break_by_recency_then_id() {
        let rows = merge_rows(
            vec![
                nest_hit("post", "bbb", 1_000_000, 1_000),
                nest_hit("post", "aaa", 1_000_000, 1_000),
                nest_hit("post", "ccc", 1_000_000, 9_000),
            ],
            vec![],
        );
        let ids: Vec<&str> = rows.iter().map(|r| r.content_id.as_str()).collect();
        assert_eq!(ids, vec!["ccc", "aaa", "bbb"]);
    }

    #[test]
    fn no_failures_means_no_error_and_a_failure_carries_its_reason() {
        assert!(arm_failure_text(&[]).is_none());
        let text = arm_failure_text(&["nest exploded".to_string()]).unwrap();
        assert_eq!(text.key, "search_page.search_failed_reason");
        assert_eq!(text.args.get("reason").unwrap(), "nest exploded");
    }

    // ── Flow tests: the manager driving both arms ────────────────────────
    //
    // The projection tests above are pure. These drive `run_query` end to end
    // over a fake transport and a fake local index, because the properties that
    // actually break in production — both arms in flight at once, a superseded
    // reply dropped, one arm's failure not blanking the other — live in the
    // orchestration, not in `merge_rows`.

    use crate::local::LocalSearchIndex;
    use fauna_protocol::search::SearchQueryReply;
    use std::sync::Mutex;
    use std::time::Duration;

    /// A budget, not a sleep: with the clock paused tokio auto-advances to the
    /// next timer, so a healthy test pays nothing and a deadlocked one reddens
    /// immediately instead of hanging (convention 14).
    const TEST_BUDGET: Duration = Duration::from_secs(600);

    /// Fake backend 1. Records what it was asked, answers what it was given.
    #[derive(Default)]
    struct FakeNest {
        results: Mutex<Vec<SearchResult>>,
        fail: Mutex<Option<String>>,
        /// Both arms rendezvous here when set — see `both_arms_are_in_flight_at_once`.
        barrier: Mutex<Option<Arc<tokio::sync::Barrier>>>,
        /// Held while the arm is "slow", to order two overlapping queries.
        gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
        calls: Mutex<Vec<(String, Option<String>, i64)>>,
    }

    // Implemented on the bare type; `Arc<FakeNest>` — what the manager holds,
    // since it needs `Clone` — picks it up through fauna-protocol's blanket
    // `impl<T: RpcRequester> RpcRequester for Arc<T>`.
    impl RpcRequester for FakeNest {
        type Error = String;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, String>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, "fauna.search.query");
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode");
            let req: SearchQueryRequest = fauna_protocol::decode_strict(&bytes).expect("decode");
            self.calls.lock().unwrap().push((
                req.query.clone(),
                req.content_type.clone(),
                req.limit.unwrap_or_default(),
            ));

            // Capture the answer NOW, before any waiting — a real nest computes
            // its reply and *then* the network is slow. Reading it after the
            // gate would make an overtaken query silently return the overtaking
            // one's rows, which is what made the first version of
            // `a_superseded_reply_is_dropped` survive its own mutation.
            let answer = if let Some(e) = self.fail.lock().unwrap().clone() {
                Err(e)
            } else {
                Ok(self.results.lock().unwrap().clone())
            };

            let barrier = self.barrier.lock().unwrap().clone();
            if let Some(b) = barrier {
                b.wait().await;
            }
            let gate = self.gate.lock().unwrap().clone();
            if let Some(g) = gate {
                g.notified().await;
            }

            let reply = fauna_protocol::encode_canonical(&SearchQueryReply {
                results: answer?,
                extra: Default::default(),
            })
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// Fake backend 2.
    #[derive(Default)]
    struct FakeLocal {
        hits: Mutex<Vec<LocalSearchHit>>,
        fail: Mutex<Option<String>>,
        barrier: Mutex<Option<Arc<tokio::sync::Barrier>>>,
        asked_kinds: Mutex<Vec<Vec<SearchKindClass>>>,
    }

    #[async_trait::async_trait]
    impl LocalSearchIndex for FakeLocal {
        async fn query(
            &self,
            _query: &str,
            kinds: &[SearchKindClass],
            _limit: usize,
        ) -> Result<Vec<LocalSearchHit>, String> {
            assert!(!kinds.is_empty(), "the manager must never ask for no kinds");
            self.asked_kinds.lock().unwrap().push(kinds.to_vec());
            let barrier = self.barrier.lock().unwrap().clone();
            if let Some(b) = barrier {
                b.wait().await;
            }
            if let Some(e) = self.fail.lock().unwrap().clone() {
                return Err(e);
            }
            Ok(self.hits.lock().unwrap().clone())
        }
    }

    fn manager_with(
        nest: Arc<FakeNest>,
        local: Option<Arc<FakeLocal>>,
    ) -> SearchManager<Arc<FakeNest>> {
        let m = SearchManager::new(nest);
        if let Some(l) = local {
            m.set_local_index(l);
        }
        m
    }

    /// **The concurrency contract, as a causal fact rather than a hope.** Both
    /// arms rendezvous on a 2-party barrier, which completes only if both are
    /// in flight simultaneously. Running them in sequence — in *either* order —
    /// leaves the first waiting for a party that has not been spawned, so the
    /// budget expires and the test reds. A fake that merely returned rows
    /// quickly would pass under a sequential mutation and pin nothing.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn both_arms_are_in_flight_at_once() {
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let nest = Arc::new(FakeNest {
            results: Mutex::new(vec![nest_hit("post", "n1", 1_000_000, 0)]),
            barrier: Mutex::new(Some(barrier.clone())),
            ..Default::default()
        });
        let local = Arc::new(FakeLocal {
            hits: Mutex::new(vec![local_hit("mail", "l1", 2.0, 0)]),
            barrier: Mutex::new(Some(barrier)),
            ..Default::default()
        });
        let m = manager_with(nest, Some(local));

        tokio::time::timeout(TEST_BUDGET, m.run_query("hello", "all"))
            .await
            .expect("both arms must be in flight together, or neither can pass the barrier");

        let snap = m.snapshot();
        assert_eq!(
            snap.results.len(),
            2,
            "both arms' rows land: {:?}",
            snap.results
        );
        assert!(!snap.in_flight);
    }

    /// `in_flight` opens synchronously (so the page shows the spinner without
    /// waiting for a round trip) and holds until BOTH arms settle.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn in_flight_holds_until_both_arms_settle() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let nest = Arc::new(FakeNest {
            gate: Mutex::new(Some(gate.clone())),
            ..Default::default()
        });
        let local = Arc::new(FakeLocal {
            hits: Mutex::new(vec![local_hit("mail", "l1", 1.0, 0)]),
            ..Default::default()
        });
        let m = Arc::new(manager_with(nest, Some(local)));

        let running = tokio::spawn({
            let m = Arc::clone(&m);
            async move { m.run_query("hello", "all").await }
        });

        // Let the query open and the local arm settle; the nest arm is gated.
        tokio::task::yield_now().await;
        let mid = m.snapshot();
        assert!(mid.in_flight, "the nest arm is still out");
        assert!(
            !mid.no_results,
            "never report 'no results' with an arm still out"
        );

        gate.notify_waiters();
        tokio::time::timeout(TEST_BUDGET, running)
            .await
            .expect("the gated arm should finish")
            .expect("task");
        assert!(!m.snapshot().in_flight);
    }

    /// A failed nest arm sets `error` **and keeps the local rows** — partial
    /// results shown honestly, never blanked (`search.md` § The local/nest merge).
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_failed_nest_arm_keeps_the_local_rows() {
        let nest = Arc::new(FakeNest {
            fail: Mutex::new(Some("nest unreachable".into())),
            ..Default::default()
        });
        let local = Arc::new(FakeLocal {
            hits: Mutex::new(vec![local_hit("mail", "l1", 1.0, 0)]),
            ..Default::default()
        });
        let m = manager_with(nest, Some(local));
        m.run_query("hello", "all").await;

        let snap = m.snapshot();
        assert_eq!(
            snap.results.len(),
            1,
            "the local rows survive the nest failure"
        );
        assert_eq!(snap.results[0].source, SearchSource::Local);
        let err = snap
            .error
            .expect("the failure must surface on error-message");
        assert!(err.args["reason"].contains("nest unreachable"), "{err:?}");
        assert!(
            !snap.no_results,
            "rows are on screen — this is not the empty state"
        );
    }

    /// **The query-side half of the one-shot gap.** A login that could not mint
    /// the local arm (mail not enabled yet) must not leave the page nest-only
    /// for the life of the process.
    ///
    /// The build-side fix alone left `test_search_local_index.py` red for
    /// exactly this reason: the mail *was* being indexed, and the page had no
    /// arm to ask. So the manager asks its resolver on any query made while it
    /// still has no arm, and the first search after mail is enabled finds one.
    ///
    /// Mutations this reddens under: never consulting the resolver; consulting
    /// it only at registration time; caching the first `None` as final.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_resolver_mints_the_local_arm_on_a_query_made_after_login() {
        struct LateResolver {
            ready: Mutex<bool>,
            local: Arc<FakeLocal>,
            asked: Mutex<usize>,
        }
        #[async_trait::async_trait]
        impl LocalIndexResolver for LateResolver {
            async fn resolve(&self) -> Option<Arc<dyn LocalSearchIndex>> {
                *self.asked.lock().unwrap() += 1;
                if !*self.ready.lock().unwrap() {
                    return None;
                }
                Some(Arc::clone(&self.local) as Arc<dyn LocalSearchIndex>)
            }
        }

        let nest = Arc::new(FakeNest {
            results: Mutex::new(vec![nest_hit("post", "n1", 1_000_000, 0)]),
            ..Default::default()
        });
        let resolver = Arc::new(LateResolver {
            ready: Mutex::new(false),
            local: Arc::new(FakeLocal {
                hits: Mutex::new(vec![local_hit("mail", "found-late", 5.0, 0)]),
                ..Default::default()
            }),
            asked: Mutex::new(0),
        });
        let m = manager_with(nest, None);
        m.set_local_index_resolver(Arc::clone(&resolver) as Arc<dyn LocalIndexResolver>);

        // Mail is not enabled yet: the page is nest-only, and that is a normal
        // rendered state rather than an error.
        m.run_query("hello", "all").await;
        assert!(
            !m.has_local_index(),
            "no arm can exist before mail is enabled"
        );
        assert!(
            m.snapshot().error.is_none(),
            "a `None` resolve is not an error"
        );
        assert!(
            m.snapshot()
                .results
                .iter()
                .all(|r| r.source == SearchSource::Nest),
            "nest-only until the arm exists"
        );

        // The user enables mail, then searches again.
        *resolver.ready.lock().unwrap() = true;
        m.run_query("hello", "all").await;

        assert!(
            m.has_local_index(),
            "the arm must be minted on the first query after its precondition arrives — \
             a resolver consulted only at registration time leaves the page nest-only \
             for the life of the process, which is the gap this closes"
        );
        assert!(
            m.snapshot()
                .results
                .iter()
                .any(|r| r.source == SearchSource::Local),
            "and its rows must reach the page"
        );
    }

    /// The resolved arm is **cached** — a page that has one must not pay a
    /// resolve per query (opening the sealed replica is real I/O on the query
    /// path).
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_resolved_arm_is_not_re_resolved_on_every_query() {
        struct CountingResolver {
            local: Arc<FakeLocal>,
            asked: Mutex<usize>,
        }
        #[async_trait::async_trait]
        impl LocalIndexResolver for CountingResolver {
            async fn resolve(&self) -> Option<Arc<dyn LocalSearchIndex>> {
                *self.asked.lock().unwrap() += 1;
                Some(Arc::clone(&self.local) as Arc<dyn LocalSearchIndex>)
            }
        }

        let nest = Arc::new(FakeNest::default());
        let resolver = Arc::new(CountingResolver {
            local: Arc::new(FakeLocal::default()),
            asked: Mutex::new(0),
        });
        let m = manager_with(nest, None);
        m.set_local_index_resolver(Arc::clone(&resolver) as Arc<dyn LocalIndexResolver>);

        m.run_query("a", "all").await;
        m.run_query("b", "all").await;
        m.run_query("c", "all").await;

        assert_eq!(
            *resolver.asked.lock().unwrap(),
            1,
            "the arm is minted once and cached; re-resolving per query would put \
             a replica open on every search"
        );
    }

    /// **An arm minted before its precondition is completed when the
    /// precondition arrives.** A login mints a master-only arm at once (the
    /// identity seed is all the master reader needs), and that arm is a real
    /// `Some` — so under "cache the first `Some`" a user who enabled mail
    /// afterwards never got the mail reader or the `Contact` claim, both of
    /// which ride the MSEK, until the process restarted. Found by the phone
    /// twin of the Contact search leg: mail is enabled through the app after
    /// login, and whether the card could ever be found depended on whether the
    /// arm happened to be minted before or after that.
    ///
    /// Mutations this reddens under: caching a partial arm as final; never
    /// re-resolving while the cached arm awaits its precondition; re-resolving
    /// a complete arm.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn an_arm_minted_before_its_precondition_is_completed_once_it_arrives() {
        struct PartialLocal;
        #[async_trait::async_trait]
        impl LocalSearchIndex for PartialLocal {
            async fn query(
                &self,
                _query: &str,
                _kinds: &[SearchKindClass],
                _limit: usize,
            ) -> Result<Vec<LocalSearchHit>, String> {
                Ok(Vec::new())
            }
            fn awaits_precondition(&self) -> bool {
                true
            }
        }
        struct MailLateResolver {
            mail_enabled: Mutex<bool>,
            full: Arc<FakeLocal>,
            asked: Mutex<usize>,
        }
        #[async_trait::async_trait]
        impl LocalIndexResolver for MailLateResolver {
            async fn resolve(&self) -> Option<Arc<dyn LocalSearchIndex>> {
                *self.asked.lock().unwrap() += 1;
                if *self.mail_enabled.lock().unwrap() {
                    Some(Arc::clone(&self.full) as Arc<dyn LocalSearchIndex>)
                } else {
                    Some(Arc::new(PartialLocal) as Arc<dyn LocalSearchIndex>)
                }
            }
        }

        let resolver = Arc::new(MailLateResolver {
            mail_enabled: Mutex::new(false),
            full: Arc::new(FakeLocal {
                hits: Mutex::new(vec![local_hit("contact", "found-after-mail", 5.0, 0)]),
                ..Default::default()
            }),
            asked: Mutex::new(0),
        });
        let m = manager_with(Arc::new(FakeNest::default()), None);
        m.set_local_index_resolver(Arc::clone(&resolver) as Arc<dyn LocalIndexResolver>);

        m.run_query("hello", "all").await;
        assert!(m.has_local_index(), "the master-only arm is a real arm");
        assert!(
            m.snapshot().results.is_empty(),
            "nothing local yet: the partial arm holds no contact class"
        );

        // The user enables mail, then searches again.
        *resolver.mail_enabled.lock().unwrap() = true;
        m.run_query("hello", "all").await;
        assert!(
            m.snapshot()
                .results
                .iter()
                .any(|r| r.source == SearchSource::Local),
            "the first query after mail is enabled must reach the completed arm — a \
             partial arm cached as final hides contact and mail hits until restart"
        );

        let asked = *resolver.asked.lock().unwrap();
        m.run_query("again", "all").await;
        assert_eq!(
            *resolver.asked.lock().unwrap(),
            asked,
            "a complete arm is cached; only an arm awaiting its precondition re-resolves"
        );
    }

    /// The mirror: a failed local arm keeps the nest rows. Neither backend can
    /// blank the other.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_failed_local_arm_keeps_the_nest_rows() {
        let nest = Arc::new(FakeNest {
            results: Mutex::new(vec![nest_hit("post", "n1", 1_000_000, 0)]),
            ..Default::default()
        });
        let local = Arc::new(FakeLocal {
            fail: Mutex::new(Some("replica unreadable".into())),
            ..Default::default()
        });
        let m = manager_with(nest, Some(local));
        m.run_query("hello", "all").await;

        let snap = m.snapshot();
        assert_eq!(snap.results.len(), 1);
        assert_eq!(snap.results[0].source, SearchSource::Nest);
        assert!(snap.error.is_some());
    }

    /// No registered local index is a NORMAL state (the web SPA always, a
    /// native app until its replica opens): nest-only rows, and **no error**.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_manager_without_a_local_index_runs_nest_only_without_erroring() {
        let nest = Arc::new(FakeNest {
            results: Mutex::new(vec![nest_hit("post", "n1", 1_000_000, 0)]),
            ..Default::default()
        });
        let m = manager_with(nest, None);
        assert!(!m.has_local_index());
        m.run_query("hello", "all").await;

        let snap = m.snapshot();
        assert_eq!(snap.results.len(), 1);
        assert!(snap.error.is_none(), "a missing local arm is not a failure");
    }

    /// **The stale-reply guard.** A slow first query that lands after a faster
    /// second one must be DROPPED, not committed — otherwise the page shows the
    /// first query's rows under the second query's committed question.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_superseded_reply_is_dropped() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let nest = Arc::new(FakeNest {
            results: Mutex::new(vec![nest_hit("post", "stale", 1_000_000, 0)]),
            gate: Mutex::new(Some(gate.clone())),
            ..Default::default()
        });
        let m = Arc::new(manager_with(nest.clone(), None));

        let slow = tokio::spawn({
            let m = Arc::clone(&m);
            async move { m.run_query("first", "all").await }
        });
        tokio::task::yield_now().await;

        // The second query overtakes: ungated, different rows.
        *nest.gate.lock().unwrap() = None;
        *nest.results.lock().unwrap() = vec![nest_hit("post", "fresh", 1_000_000, 0)];
        m.run_query("second", "all").await;
        assert_eq!(m.snapshot().results[0].content_id, "fresh");

        // Now let the first reply land. It must not clobber the second.
        gate.notify_waiters();
        tokio::time::timeout(TEST_BUDGET, slow)
            .await
            .expect("the gated query should finish")
            .expect("task");

        let snap = m.snapshot();
        assert_eq!(snap.query, "second");
        assert_eq!(
            snap.results[0].content_id, "fresh",
            "the superseded reply must not repopulate the page"
        );
    }

    /// `has_more` is derived from the **nest arm's** count alone. Local rows
    /// padding the merged list must never offer a page the wire cannot produce.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn has_more_ignores_local_rows() {
        let limit = crate::paging::INITIAL_LIMIT as usize;
        // A partial nest page (one row short) — the wire has no more.
        let nest_rows: Vec<SearchResult> = (0..limit - 1)
            .map(|i| nest_hit("post", &format!("n{i}"), 1_000_000, 0))
            .collect();
        // …but enough local rows to push the merged list well past the limit.
        let local_rows: Vec<LocalSearchHit> = (0..10)
            .map(|i| local_hit("mail", &format!("l{i}"), 1.0, 0))
            .collect();

        let nest = Arc::new(FakeNest {
            results: Mutex::new(nest_rows),
            ..Default::default()
        });
        let local = Arc::new(FakeLocal {
            hits: Mutex::new(local_rows),
            ..Default::default()
        });
        let m = manager_with(nest, Some(local));
        m.run_query("hello", "all").await;

        let snap = m.snapshot();
        assert!(
            snap.results.len() > limit,
            "the merged list exceeds the page limit"
        );
        assert!(
            !snap.has_more,
            "a partial NEST page means no more rows exist on the wire"
        );
    }

    /// The one shared mapping reaches both arms: the token narrows the wire
    /// parameter and the local kind set together.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn the_type_filter_narrows_both_arms() {
        let nest = Arc::new(FakeNest::default());
        let local = Arc::new(FakeLocal::default());
        let m = manager_with(nest.clone(), Some(local.clone()));

        m.run_query("hello", "imap").await;

        let (_, content_type, _) = nest.calls.lock().unwrap()[0].clone();
        assert_eq!(content_type, Some("imap".to_string()));
        assert_eq!(
            local.asked_kinds.lock().unwrap()[0],
            vec![SearchKindClass::Mail]
        );
        assert_eq!(m.snapshot().type_filter, "imap");
    }

    /// A filter with no local counterpart skips the arm entirely rather than
    /// asking it a question with a guaranteed empty answer.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_nest_only_filter_skips_the_local_arm() {
        let nest = Arc::new(FakeNest::default());
        let local = Arc::new(FakeLocal::default());
        let m = manager_with(nest, Some(local.clone()));

        m.run_query("hello", "profile").await;
        assert!(
            local.asked_kinds.lock().unwrap().is_empty(),
            "the local arm must not be asked about profiles"
        );
        assert!(m.snapshot().error.is_none());
    }

    /// A blank query is every app's no-op guard: nothing fires, and the page
    /// stays un-searched rather than flipping to the empty state.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_blank_query_is_a_noop() {
        let nest = Arc::new(FakeNest::default());
        let m = manager_with(nest.clone(), None);

        m.run_query("   ", "all").await;

        assert!(
            nest.calls.lock().unwrap().is_empty(),
            "nothing may be fired"
        );
        let snap = m.snapshot();
        assert!(!snap.searched());
        assert!(!snap.no_results);
        assert!(!snap.in_flight);
    }

    /// Searched-and-found-nothing is a distinct state from not-searched-yet.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn an_empty_result_set_is_the_no_results_state_not_the_pre_search_one() {
        let m = manager_with(Arc::new(FakeNest::default()), None);
        assert!(!m.snapshot().searched(), "pre-search");

        m.run_query("nothing matches", "all").await;

        let snap = m.snapshot();
        assert!(snap.searched());
        assert!(snap.no_results);
        assert!(snap.results.is_empty());
    }

    /// Load-more re-fires the LAST FIRED query with a grown page — not whatever
    /// the user has since typed into the field.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn load_more_regrows_the_last_fired_query() {
        let limit = crate::paging::INITIAL_LIMIT;
        let rows: Vec<SearchResult> = (0..limit)
            .map(|i| nest_hit("post", &format!("n{i}"), 1_000_000, 0))
            .collect();
        let nest = Arc::new(FakeNest {
            results: Mutex::new(rows),
            ..Default::default()
        });
        let m = manager_with(nest.clone(), None);

        m.run_query("hello", "all").await;
        assert!(m.snapshot().has_more, "a full page offers more");

        m.load_more().await;

        let calls = nest.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].0, "hello", "the last fired query, re-issued");
        assert_eq!(calls[1].2, limit + crate::paging::LOAD_MORE_STEP);
    }

    /// Load-more is inert when the affordance isn't offered — a stale click
    /// can't ask the nest for a page it already proved empty.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn load_more_without_a_full_page_fires_nothing() {
        let nest = Arc::new(FakeNest {
            results: Mutex::new(vec![nest_hit("post", "n1", 1_000_000, 0)]),
            ..Default::default()
        });
        let m = manager_with(nest.clone(), None);

        m.run_query("hello", "all").await;
        assert!(!m.snapshot().has_more);
        m.load_more().await;

        assert_eq!(nest.calls.lock().unwrap().len(), 1, "no second fetch");
    }

    /// Cancel resets the page to pre-search, and a query still in flight lands
    /// on the cancelled page and is dropped rather than repopulating it.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn cancel_resets_the_page_and_drops_an_in_flight_reply() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let nest = Arc::new(FakeNest {
            results: Mutex::new(vec![nest_hit("post", "late", 1_000_000, 0)]),
            gate: Mutex::new(Some(gate.clone())),
            ..Default::default()
        });
        let m = Arc::new(manager_with(nest, None));

        let running = tokio::spawn({
            let m = Arc::clone(&m);
            async move { m.run_query("hello", "post").await }
        });
        tokio::task::yield_now().await;

        m.cancel();
        let snap = m.snapshot();
        assert!(!snap.searched());
        assert_eq!(snap.type_filter, crate::kind::TYPE_FILTER_ALL);
        assert!(!snap.in_flight);

        gate.notify_waiters();
        tokio::time::timeout(TEST_BUDGET, running)
            .await
            .expect("the gated query should finish")
            .expect("task");

        assert!(
            m.snapshot().results.is_empty(),
            "a cancelled page must not be repopulated by the reply it dismissed"
        );
    }

    /// Every mutation notifies, so a client that only ever re-reads on
    /// `on_changed` sees the opening in-flight state AND the settled one.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn observers_are_notified_on_open_and_on_settle() {
        #[derive(Default)]
        struct Counter(std::sync::atomic::AtomicU64);
        impl SearchSnapshotObserver for Counter {
            fn on_changed(&self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let obs = Arc::new(Counter::default());
        let m = manager_with(Arc::new(FakeNest::default()), None);
        m.add_observer(obs.clone());

        m.run_query("hello", "all").await;
        assert_eq!(
            obs.0.load(Ordering::SeqCst),
            2,
            "one notification opening the query, one committing it"
        );
    }
}
