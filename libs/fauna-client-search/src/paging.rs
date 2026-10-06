//! The Search page's paging policy — the initial page size, what "load more"
//! does, and when the `search-load-more-button` is shown.
//!
//! Owner doc: `docs/goal/ui/search.md` § Layout & flow (the affordance's
//! behavior) + § Where logic lives (this crate as its home).
//!
//! **Why this is shared.** `fauna.search.query` has no cursor: "load more" is
//! implemented everywhere by re-firing the *same* query with a bigger `limit`
//! and replacing the result list. That is three coupled decisions — the first
//! limit, the growth step, and the predicate that decides whether another page
//! could exist — and before this module each of the six apps with a search page
//! had hand-written all three:
//!
//! | App | Site (pre-lift) |
//! |---|---|
//! | tui | `apps/fauna-tui/src/search.rs` — `INITIAL_LIMIT`/`LOAD_MORE_STEP`, `i64` |
//! | linux | `apps/fauna-linux/src/views/search.rs` — same, `u32` |
//! | android | `.../ui/viewmodel/SearchVM.kt` — same, `Int` |
//! | windows | `.../ViewModels/SearchViewModel.cs` — `InitialLimit`/`LoadMoreStep`, `int` |
//! | apple | `FaunaKit/.../ViewModels/SearchVM.swift` — `initialLimit`/`loadMoreStep`, `Int64` |
//! | web | `apps/fauna-web/src/routes/search/+page.svelte` — a bare literal `50`, three times |
//!
//! The constants all agreed at 50. The **predicate** is where they had already
//! drifted — linux carried a `count > 0 &&` guard nobody else had, and windows
//! computed its answer once at fetch time and stored it rather than deriving it
//! live. A constant that drifts is visible; a predicate that drifts is not.
//!
//! **The bug the lift exposed.** Every one of those six kept bumping the limit
//! by 50 with no ceiling, while the nest clamps `limit` into
//! `[MIN_LIMIT, MAX_LIMIT]` (`fauna_protocol::search`, applied by
//! `search_handlers.rs::query_handler`). So the third "load more" asked for
//! 150, got 100 back — the same 100 rows already on screen — and then hid its
//! own button, because `100 >= 150` is false. A click that changed nothing,
//! identically on all six apps. [`SearchPaging`] fixes it by construction: the
//! limit saturates at the ceiling, and [`SearchPaging::has_more`] reports
//! `false` once there is nothing left to ask for, so the affordance is gone
//! before it becomes dead rather than after.

use fauna_protocol::search;

/// Page size the first fire of a query asks for.
pub const INITIAL_LIMIT: i64 = 50;

/// How much each "load more" adds to the requested page size (saturating at
/// [`fauna_protocol::search::MAX_LIMIT`]).
pub const LOAD_MORE_STEP: i64 = 50;

/// The Search page's cursor-less paging state: the limit the next
/// `fauna.search.query` should carry.
///
/// Deliberately a state object rather than three loose constants — with the
/// constants exposed alone, each app still had to write `limit += STEP` and its
/// own `has_more`, which is exactly where the fleet had drifted. Owning the
/// transitions here means an app cannot pick its own growth policy or its own
/// end-of-results rule.
///
/// `Copy` and free of allocation, so an app can keep it in whatever state
/// container it already has (tui's `SearchState`, linux's `Rc<RefCell<..>>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchPaging {
    limit: i64,
}

impl Default for SearchPaging {
    fn default() -> Self {
        Self::initial()
    }
}

impl SearchPaging {
    /// The state a fresh query starts from — also what a submit, a type-filter
    /// change, or a cancel resets to. Every app re-fires from the first page on
    /// each of those, because the reply replaces the result list wholesale.
    pub fn initial() -> Self {
        Self {
            limit: INITIAL_LIMIT,
        }
    }

    /// Rebuild the state from a limit an app is already holding, clamped into
    /// the range this policy can actually produce
    /// (`[INITIAL_LIMIT, search::MAX_LIMIT]`).
    ///
    /// For the FFI/wasm faces (`fauna-ffi`, `fauna-wasm`), which are stateless
    /// by design: apple/android/windows/web each keep the limit in their own
    /// view-model, so they hand it back on every call rather than holding a
    /// shared object across the boundary. Exact rather than approximate —
    /// `SearchPaging` is a newtype over the limit — and clamping means a
    /// garbage value from the far side of the boundary cannot produce a limit
    /// the nest would refuse.
    pub fn at_limit(limit: i64) -> Self {
        Self {
            limit: limit.clamp(INITIAL_LIMIT, search::MAX_LIMIT),
        }
    }

    /// The `limit` to put on the next [`search::SearchQueryRequest`]. Always
    /// within the nest's clamp, so the page the nest serves is the page that
    /// was asked for.
    pub fn limit(&self) -> i64 {
        self.limit
    }

    /// Grow the requested page by [`LOAD_MORE_STEP`], saturating at
    /// [`search::MAX_LIMIT`].
    ///
    /// Saturating rather than clamping-at-send matters: the limit is also what
    /// [`has_more`](Self::has_more) compares the result count against, so a
    /// limit the nest would refuse to honour would make the predicate ask an
    /// unanswerable question ("did we get 150 rows?" — the nest never sends
    /// more than 100).
    pub fn load_more(&mut self) {
        self.limit = (self.limit + LOAD_MORE_STEP).min(search::MAX_LIMIT);
    }

    /// Reset to [`initial`](Self::initial) — for submit / filter change /
    /// cancel.
    pub fn reset(&mut self) {
        *self = Self::initial();
    }

    /// Whether `search-load-more-button` should be shown for a result list of
    /// `result_count` rows.
    ///
    /// Two conditions, both required:
    ///
    /// 1. **The page came back full.** With no cursor on the wire, a full page
    ///    is the only evidence more rows may exist; a partial page proves they
    ///    do not (`search.md` § Layout & flow).
    /// 2. **There is a bigger page to ask for.** At [`search::MAX_LIMIT`] there
    ///    is not — re-firing would return the same rows — so the affordance is
    ///    hidden rather than left to disappoint. This is the arm every app was
    ///    missing.
    ///
    /// Note `result_count >= limit` rather than `==`: the nest is not supposed
    /// to overshoot a limit, and this is a visibility rule, not a place to
    /// assert nest behavior.
    pub fn has_more(&self, result_count: usize) -> bool {
        result_count as i64 >= self.limit && self.limit < search::MAX_LIMIT
    }

    /// Whether the requested page is already at [`search::MAX_LIMIT`] — the
    /// point past which `fauna.search.query` cannot serve more for this query.
    /// Exposed so a client can explain the ceiling if it wants to; the paging
    /// rules themselves already account for it.
    pub fn at_cap(&self) -> bool {
        self.limit >= search::MAX_LIMIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_asks_for_the_first_page() {
        assert_eq!(SearchPaging::initial().limit(), INITIAL_LIMIT);
        assert_eq!(SearchPaging::default(), SearchPaging::initial());
    }

    #[test]
    fn load_more_grows_by_one_step() {
        let mut p = SearchPaging::initial();
        p.load_more();
        assert_eq!(p.limit(), INITIAL_LIMIT + LOAD_MORE_STEP);
    }

    /// The lift's reason for existing: growth stops at the nest's clamp, so a
    /// client never asks for a page the nest will silently truncate.
    #[test]
    fn load_more_saturates_at_the_nest_clamp() {
        let mut p = SearchPaging::initial();
        for _ in 0..10 {
            p.load_more();
        }
        assert_eq!(p.limit(), search::MAX_LIMIT);
        assert!(p.at_cap());
    }

    #[test]
    fn a_full_page_offers_more() {
        let p = SearchPaging::initial();
        assert!(p.has_more(INITIAL_LIMIT as usize));
    }

    #[test]
    fn a_partial_page_proves_there_is_no_more() {
        let p = SearchPaging::initial();
        assert!(!p.has_more(INITIAL_LIMIT as usize - 1));
        assert!(!p.has_more(0));
    }

    /// The dead-click case, stated as the fleet used to hit it: two load-mores
    /// puts the limit at the ceiling with a full page of rows. Every app showed
    /// the button here, and clicking it re-fetched the same 100 rows and then
    /// hid the button. The predicate now hides it one click earlier — while it
    /// still means something.
    #[test]
    fn a_full_page_at_the_cap_offers_nothing_further() {
        let mut p = SearchPaging::initial();
        p.load_more();
        assert_eq!(p.limit(), search::MAX_LIMIT);
        assert!(!p.has_more(search::MAX_LIMIT as usize));
        assert!(!p.has_more(search::MAX_LIMIT as usize + 5));
    }

    /// The FFI/wasm faces round-trip through [`SearchPaging::at_limit`] on
    /// every call, so it must reproduce the state exactly for every limit the
    /// policy can reach — and clamp anything else into range rather than trust
    /// a value from across the boundary.
    #[test]
    fn at_limit_round_trips_every_reachable_limit_and_clamps_the_rest() {
        let mut p = SearchPaging::initial();
        loop {
            assert_eq!(SearchPaging::at_limit(p.limit()), p);
            if p.at_cap() {
                break;
            }
            p.load_more();
        }
        // Out of range in both directions, and outright garbage.
        assert_eq!(SearchPaging::at_limit(0), SearchPaging::initial());
        assert_eq!(SearchPaging::at_limit(-7), SearchPaging::initial());
        assert_eq!(SearchPaging::at_limit(i64::MAX).limit(), search::MAX_LIMIT);
    }

    /// linux's extra `count > 0` guard was redundant, not a real divergence —
    /// pinned so the lift is provably behavior-preserving where it claims to
    /// be. An empty result set is a partial page for every reachable limit.
    #[test]
    fn the_empty_result_set_needs_no_special_guard() {
        let mut p = SearchPaging::initial();
        assert!(!p.has_more(0));
        p.load_more();
        assert!(!p.has_more(0));
    }
}
