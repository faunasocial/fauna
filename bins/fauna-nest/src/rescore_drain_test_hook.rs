//! Test-only counter + projection making the re-score drain's **decision
//! point** observable, so a negative assert about the drain can anchor on
//! state instead of on elapsed time (`e2e-conventions.md` § convention 14).
//!
//! Gated on `test-hooks` **alone** (the `outbound_drain_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build that `build_node()`
//! produces. Production never compiles this module.
//!
//! # Why a counter and not a poke
//!
//! D6a's rule — look for the production nudge before building one — already
//! applies here and is already satisfied: the drain *has* a production poke.
//! `config_changed` reaches every approved bridge, `wsrpc` calls
//! `rescoreDrain.poke()`, and the loop runs `runOnce` at its next scheduling
//! opportunity (`bins/fauna-bridges/internal/mda/rescore_drain.go`, `start`/
//! `poke`). The e2e tests already fire it. What was missing is the other half
//! D6c named: a **completion observable**. Reaching for a second poke here
//! would have added a mechanism that still could not answer the question.
//!
//! # Why the worklist serve IS the decision point
//!
//! The drain owns no authority of its own — the nest is the decider at *both*
//! ends of a run, which is what makes one counter enough here (contrast
//! `ALERT_SWEEP_PASSES_KEY`, which needs a `{started, completed}` pair because
//! the app-side sweep is the decider and several loops can be in flight):
//!
//! 1. [`crate::bridge_blob_handlers`]'s `rescore_worklist_handler` intersects
//!    the holder's live `content.read{kind}` grants with the per-factor
//!    obligation gap, and returns work-units only for owners it may unseal. A
//!    revoked owner is simply absent from the answer.
//! 2. `submit_scores_handler` re-authorizes **every** row against a live
//!    `content.label-write` grant and is fail-closed on the batch — so even a
//!    run that fetched its worklist *before* a revoke cannot write back after
//!    it.
//!
//! Together those mean the interesting negative — "a revoked grant leaves the
//! obligation owed" — is not a race the test must out-wait; it is an invariant
//! the nest enforces. What the settle-sleep was really buying was
//! **non-vacuity**: without evidence that a drain ran at all after the revoke,
//! "the row did not advance" is trivially true and proves nothing.
//!
//! So the observable a test needs is exactly "the nest served this holder a
//! worklist *after* my plant landed" — the moment the nest made the decision
//! under test. [`RescoreWorklistServes::note_served`] is bumped **after** the
//! unit list is built, so an observed serve is a decision already made, never
//! one in progress. That ordering is the falsifiable half and is pinned by
//! `serves_are_counted_after_the_unit_list_is_decided`.
//!
//! Consumers: `tests/e2e-unified/tests/test_capability_labeler_drain.py`,
//! `test_capability_rescore_drain.py`, `test_spam_baseline_drain.py`, via
//! `helpers/waiting.py::await_rescore_worklist_serve_after`.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use serde_json::json;

use crate::routes::AppState;

/// Cumulative `fauna.capabilities.rescore_worklist` serves, and the cumulative
/// number of work-units handed out across them.
///
/// Both counters are monotonic, which is what lets a caller read them once
/// before planting a condition and compare afterwards. They are read together
/// in a single snapshot so `units` can be interpreted against the `serves` it
/// belongs to (site D6d-2 below needs exactly that pairing).
#[derive(Debug, Default)]
pub struct RescoreWorklistServes {
    serves: AtomicU64,
    units: AtomicU64,
}

impl RescoreWorklistServes {
    /// Record one served worklist and how many units it carried.
    ///
    /// ⚠ Call this **after** the unit list is decided, never before. A test
    /// waits for `serves` to advance and then asserts an absence; a bump that
    /// happened before the decision would release that wait early and hand the
    /// test the very false-pass the settle-sleep already had.
    pub fn note_served(&self, units: usize) {
        // `units` first, `serves` last: a reader that sees the serve is then
        // guaranteed to see this serve's units too, so the pair it snapshots
        // can never attribute zero units to a serve that carried some.
        self.units.fetch_add(units as u64, Ordering::Release);
        self.serves.fetch_add(1, Ordering::Release);
    }

    /// `(serves, units)` — read `serves` first to pair with [`Self::note_served`]'s
    /// write order.
    pub fn snapshot(&self) -> (u64, u64) {
        let serves = self.serves.load(Ordering::Acquire);
        let units = self.units.load(Ordering::Acquire);
        (serves, units)
    }
}

/// `GET /api/v1/test/capabilities/rescore_worklist` — snapshot the re-score
/// worklist serve counters. Returns `{"serves": <n>, "units": <n>}`.
///
/// Content-free by construction: it counts decisions, and never reports which
/// owners or content ids were in them.
async fn handle_worklist_serves(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let (serves, units) = state.rescore_worklist_serves.snapshot();
    Json(json!({ "serves": serves, "units": units })).into_response()
}

/// Mount the `/api/v1/test/capabilities/rescore_worklist` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/capabilities/rescore_worklist",
        get(handle_worklist_serves),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ordering the barrier rests on: a serve becomes visible only once its
    /// units have been accounted for.
    ///
    /// This is the falsifiable half of the mechanism. Per D6c's finding, a
    /// barrier guarding an *absence* cannot be graded by releasing it early —
    /// that makes the absence trivially true and the e2e passes vacuously — so
    /// what is pinned here is the ordering instead. Swap the two `fetch_add`s
    /// in `note_served` and a reader can observe `serves = 1, units = 0`, which
    /// is exactly the reading that would tell a waiting test "a decision was
    /// made and it was empty" about a serve that in fact carried work.
    #[test]
    fn serves_are_counted_after_the_unit_list_is_decided() {
        let c = RescoreWorklistServes::default();
        assert_eq!(c.snapshot(), (0, 0));

        c.note_served(3);
        assert_eq!(
            c.snapshot(),
            (1, 3),
            "a serve and its units must become visible together"
        );

        // An empty serve is the interesting case for a negative assert: it
        // still counts, because "the nest decided, and the answer was nothing"
        // is precisely the evidence the drain tests wait for.
        c.note_served(0);
        assert_eq!(c.snapshot(), (2, 3));
    }
}
