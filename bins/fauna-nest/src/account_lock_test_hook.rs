//! Test-only endpoints that make an account lock **lapse inside a run** and make
//! the nest's own locked refusals **countable** — the two things the witness for
//! "a locked app stops retrying, and signs back in by itself once the lock ends"
//! (`docs/goal/behavior/devices.md` § The locked state) lacks.
//!
//! Gated on `test-hooks` **alone** (the `post_fanout_test_hook` shape), so it
//! rides the standard `--features test-hooks` e2e build and is compiled out of
//! every release artifact (`e2e-conventions.md` point 15).
//!
//! # Why a near `locked_until`, and why the nest sets it rather than the app
//!
//! The lock is a hard-coded 24 hours ([`crate::account_core::EMERGENCY_LOCKOUT_SECS`];
//! `devices.md` § Emergency lockout rules out a requested duration, so there is
//! no knob to turn on the wire or in config). The app schedules its one refresh
//! from the `locked_until` the refusal *carried*, on its own clock, and never
//! asks the nest again before then. So a hook that merely **ended** a lock early
//! would witness nothing: the app would keep waiting on its own 24-hour timer.
//! What a run needs is a refusal that carries a near unlock time, so the app's
//! own scheduled refresh comes due inside the run — no app-side hook, no app
//! clock moved, the same code path on all seven apps.
//!
//! `POST /api/v1/test/account_lock/{actor}?in_secs=N` writes
//! `locked_until = now + N` on the nest's clock, through the very setter the
//! production lock uses. A test sets it **while the app's sign-in is parked**
//! (`rpc_hold_test_hook` on `fauna.auth.verify`), so the window starts when the
//! app is provably at the gate rather than when a relaunch happened to finish —
//! latency-independent, `e2e-conventions.md` point 14.
//!
//! # Why a refusal counter
//!
//! "Stops retrying" is an absence, and an absence observed by waiting is the
//! settle-window false pass point 14 forbids. The counter is bumped
//! **synchronously inside the mint gate**, before the refusal is encoded, at each
//! of the three gates that answer `fauna.auth.account_locked` (the direct
//! handshake, the silent challenge's verify, the device-key handshake). So every
//! sign-in attempt the nest turns away as locked is counted by the time its reply
//! exists, and a test reads it at two points it can observe — the locked surface
//! painted, the app signed back in — and asserts nothing was refused between
//! them. Keyed per actor, so another test's locked identity on the shared nest
//! never moves this test's reading. Content-free beyond the actor key the caller
//! already named.
//!
//! Consumer: `tests/e2e-unified/tests/test_locked_surface.py::
//! test_a_locked_app_stops_retrying_and_signs_back_in_when_the_lock_ends`.

#![cfg(feature = "test-hooks")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// Cumulative `fauna.auth.account_locked` refusals per actor on this nest.
///
/// Monotonic per actor, which is what lets a caller read it at one observable
/// point and compare at a later one.
#[derive(Debug, Default)]
pub struct AccountLockRefusals {
    refused: Mutex<HashMap<[u8; 32], u64>>,
}

impl AccountLockRefusals {
    /// Record one sign-in refused as locked.
    ///
    /// ⚠ Call this at the gate, synchronously, before the refusal is returned
    /// — a bump after the reply would be invisible to a reader the reply
    /// unblocked, and the absence assert would degrade into a race.
    pub fn note(&self, actor: &[u8; 32]) {
        *self.refused.lock().unwrap().entry(*actor).or_default() += 1;
    }

    /// Refusals of `actor` so far.
    pub fn count(&self, actor: &[u8; 32]) -> u64 {
        self.refused
            .lock()
            .unwrap()
            .get(actor)
            .copied()
            .unwrap_or(0)
    }
}

fn parse_actor(hex_str: &str) -> Result<[u8; 32], ApiError> {
    fauna_core::hex32::decode(hex_str)
        .map_err(|_| ApiError::bad_request("actor must be 64 hex characters"))
}

#[derive(Deserialize)]
struct LockQuery {
    in_secs: u64,
}

/// `POST /api/v1/test/account_lock/{actor}?in_secs=N` — lock `actor` until
/// `now + N` on the nest's clock. `N` must lie in `1..=EMERGENCY_LOCKOUT_SECS`:
/// this shortens a lock, it never lengthens one past what the product can set.
/// Returns `{"ok": true, "locked_until": <unix secs>}`.
async fn handle_lock(
    State(state): State<Arc<AppState>>,
    Path(actor): Path<String>,
    Query(q): Query<LockQuery>,
) -> impl IntoResponse {
    let actor = match parse_actor(&actor) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    if q.in_secs == 0 || q.in_secs > crate::account_core::EMERGENCY_LOCKOUT_SECS {
        return ApiError::bad_request("in_secs must be 1..=EMERGENCY_LOCKOUT_SECS").into_response();
    }
    let locked_until = (fauna_core::data::Timestamp::now_secs() as u64 + q.in_secs) as i64;
    match state.db.set_locked_until(&actor, Some(locked_until)).await {
        Ok(()) => Json(json!({ "ok": true, "locked_until": locked_until })).into_response(),
        Err(e) => ApiError::internal(format!("account_lock set: {e:#}")).into_response(),
    }
}

/// `GET /api/v1/test/account_lock/{actor}/refusals` — `{"refused": <n>}`, the
/// sign-ins of `actor` this nest has refused as locked.
async fn handle_refusals(
    State(state): State<Arc<AppState>>,
    Path(actor): Path<String>,
) -> impl IntoResponse {
    match parse_actor(&actor) {
        Ok(a) => Json(json!({ "refused": state.account_lock_refusals.count(&a) })).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Mount the `/api/v1/test/account_lock/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/v1/test/account_lock/{actor}", post(handle_lock))
        .route(
            "/api/v1/test/account_lock/{actor}/refusals",
            get(handle_refusals),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per actor and monotonic: a read-observe-read consumer subtracts two
    /// snapshots, and another actor's refusals never move them.
    #[test]
    fn refusals_count_per_actor() {
        let c = AccountLockRefusals::default();
        let (a, b) = ([1u8; 32], [2u8; 32]);
        assert_eq!(c.count(&a), 0);
        c.note(&a);
        c.note(&b);
        c.note(&b);
        assert_eq!(c.count(&a), 1);
        assert_eq!(c.count(&b), 2);
    }
}
