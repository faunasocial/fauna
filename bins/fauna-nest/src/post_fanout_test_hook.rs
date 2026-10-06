//! Test-only counter + projection making the post **bridge fan-out initiation
//! point** observable, so a negative assert about a fan-out can anchor on state
//! instead of on elapsed time (`e2e-conventions.md` § convention 14).
//!
//! Gated on `test-hooks` **alone** (the `outbound_drain_test_hook` /
//! `rescore_drain_test_hook` shape), so it rides the standard
//! `--features test-hooks` e2e build that `build_node()` produces. Production
//! never compiles this module.
//!
//! # Why an initiation counter, and not a poke or a delivery observable
//!
//! D6c's poke-vs-observable question classifies this site as *observable*: the
//! fan-out has nothing to hurry — it is already nudged
//! (`state.activitypub.delivery_nudge`) the moment a job is enqueued, and the
//! 30 s poll is only a backstop. What the test lacked is a way to learn the
//! **decision** had been made.
//!
//! A *delivery*-side observable cannot answer it either, and the reason
//! generalizes. The property under test is an absence — "a re-delivered
//! forward must not fan out again" — and the fan-out is asynchronous
//! ([`crate::routes::spawn_post_bridge_fanout`] spawns; `create_push_inner`
//! then enqueues onto `ap_delivery_queue` and nudges the worker). So an
//! assertion that no second `Create` has *arrived* is exactly the settle-window
//! false-pass convention 14 forbids: a duplicate that is merely late reads
//! identical to a duplicate that never existed.
//!
//! # Why the initiation point is the right one, and why the RPC reply is the barrier
//!
//! The gate under test is **synchronous and inside the handler**:
//! `post_forward_handler` reads `newly_stored = !post_exists(post_id)` and
//! calls [`crate::routes::spawn_post_bridge_fanout`] only under it, both
//! strictly before it encodes its reply (`federation_handlers.rs`, the
//! `store_post` match arm → `encode_reply`). That is convention 14's own
//! corollary satisfied by the product already: *a would-be effect is initiated
//! synchronously inside a handler whose completion is observable.*
//!
//! So the counter is bumped where the spawn is *decided*, never where the
//! delivery lands — the same constraint `SESSION_GENERATION_KEY` is built on,
//! and for the same reason: a counter bumped at the landing site is invisible
//! to a barrier that ran first, and the negative assert silently reverts to the
//! race it replaced. Here the "trigger's completion observable" is the
//! `fauna.federation.post.forward` RPC reply itself, so a consumer needs **no
//! poll at all**: read the counter, make the call, read it again.
//!
//! Non-vacuity comes free and needs no second mechanism (contrast
//! `ALERT_SWEEP_PASSES_KEY`'s `{started, completed}` pair): the *first* forward
//! of a post bumps this counter, so a test asserting the *second* one did not
//! has already watched the counter prove itself live.
//!
//! # What it counts
//!
//! Every initiation, from either producer of a post — a local
//! `fauna.posts.create` and a relayed `fauna.federation.post.forward` both
//! funnel through [`crate::routes::spawn_post_bridge_fanout`], which is the
//! single place a post's bridge fan-out (ActivityPub push, Bluesky
//! write-through) begins. It counts *decisions to fan out*, never deliveries,
//! and it is content-free: no post id, author or inbox is ever reported.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_activitypub_federation.py::
//! TestActivityPubFollowerDelivery::test_forwarded_post_is_pushed_to_follower`.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use serde_json::json;

use crate::routes::AppState;

/// Cumulative post bridge fan-outs **initiated** on this nest.
///
/// Monotonic, which is what lets a caller read it once before a trigger and
/// compare afterwards.
#[derive(Debug, Default)]
pub struct PostFanoutInitiations {
    initiated: AtomicU64,
}

impl PostFanoutInitiations {
    /// Record one initiated fan-out.
    ///
    /// ⚠ Call this at the **initiation** point — synchronously, inside the
    /// handler that decided to fan out and before it replies — never from
    /// inside the spawned task. A test reads this counter, issues a trigger
    /// whose completion it can observe, and asserts the counter unchanged; a
    /// bump that happened asynchronously *after* the trigger completed would be
    /// invisible to that read, and the negative assert would silently degrade
    /// into the race it exists to replace.
    pub fn note_initiated(&self) {
        self.initiated.fetch_add(1, Ordering::Release);
    }

    /// Fan-outs initiated so far.
    pub fn snapshot(&self) -> u64 {
        self.initiated.load(Ordering::Acquire)
    }
}

/// `GET /api/v1/test/posts/fanouts` — snapshot the fan-out initiation counter.
/// Returns `{"initiated": <n>}`.
async fn handle_fanouts(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(json!({ "initiated": state.post_fanout_initiations.snapshot() })).into_response()
}

/// Mount the `/api/v1/test/posts/fanouts` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/posts/fanouts", get(handle_fanouts))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The counter is monotonic and counts every initiation, so a
    /// read-trigger-read consumer can subtract two snapshots.
    #[test]
    fn every_initiation_counts_itself() {
        let c = PostFanoutInitiations::default();
        assert_eq!(c.snapshot(), 0);

        c.note_initiated();
        assert_eq!(c.snapshot(), 1);

        // The shape a consumer relies on: a window in which nothing was
        // initiated leaves the reading unchanged, which is what makes
        // "unchanged" a sound absence claim rather than a timing guess.
        let before = c.snapshot();
        assert_eq!(c.snapshot() - before, 0);

        c.note_initiated();
        assert_eq!(c.snapshot() - before, 1);
    }
}
