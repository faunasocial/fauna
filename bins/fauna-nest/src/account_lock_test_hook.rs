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
//! # Why the lock is ARMED, and written at the gate
//!
//! `POST /api/v1/test/account_lock/{actor}/at_next_verify?in_secs=N` arms a lock
//! that the actor's next `fauna.auth.verify` — the silent challenge's mint, the
//! sign-in every app launch makes — writes at the mint gate itself, just before
//! the gate reads it: `locked_until = now + N` on the nest's clock, through the
//! very setter the production lock uses. So the window starts at the moment the
//! app is provably at the gate, however long the relaunch that brought it there
//! took — latency-independent, `e2e-conventions.md` point 14.
//!
//! What it replaced: the test parked the app's verify (`rpc_hold_test_hook`),
//! wrote the lock, and released the park. That only worked while the park was
//! released inside the verify's own 5-second client deadline
//! (`fauna_protocol::kind::register_auth_kinds`), so its result depended on how
//! fast the app's relaunch handed control back to the test: on windows, where
//! the relaunch returns after the app has already sent its verify, the app's
//! launch timed out into its transient-offline surface before the lock existed.
//!
//! Only the verify gate consumes an arm: a sync agent's device-key handshake on
//! the same actor would otherwise start the window before the app's launch
//! reaches the nest. The arm is held under an async lock **across** the write, so
//! an app that sends two verifies at once (the launch machine's and a client's
//! bearer mint) cannot have the second read the lock before the first wrote it.
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

/// The hook's state on one nest: the locks armed for an actor's next verify,
/// and the cumulative `fauna.auth.account_locked` refusals per actor.
#[derive(Debug, Default)]
pub struct AccountLockHook {
    refused: Mutex<HashMap<[u8; 32], u64>>,
    /// `in_secs` per actor, written as `now + in_secs` by that actor's next
    /// verify. An async mutex because it is held across the database write
    /// (the module doc, *Why the lock is ARMED*).
    armed: tokio::sync::Mutex<HashMap<[u8; 32], u64>>,
}

impl AccountLockHook {
    /// Record one sign-in refused as locked.
    ///
    /// ⚠ Call this at the gate, synchronously, before the refusal is returned
    /// — a bump after the reply would be invisible to a reader the reply
    /// unblocked, and the absence assert would degrade into a race.
    pub fn note(&self, actor: &[u8; 32]) {
        *self.refused.lock().unwrap().entry(*actor).or_default() += 1;
    }

    /// Refusals of `actor` so far. Monotonic per actor, which is what lets a
    /// caller read it at one observable point and compare at a later one.
    pub fn count(&self, actor: &[u8; 32]) -> u64 {
        self.refused
            .lock()
            .unwrap()
            .get(actor)
            .copied()
            .unwrap_or(0)
    }

    /// Arm a lock of `in_secs` for `actor`'s next verify, replacing any arm
    /// still pending for it.
    pub async fn arm(&self, actor: [u8; 32], in_secs: u64) {
        self.armed.lock().await.insert(actor, in_secs);
    }

    /// The verify gate's call, made just before it reads `actor`'s lock: write
    /// the lock armed for `actor`, if one is, and disarm it. Holds the arm
    /// table across the write, so a concurrent verify of the same actor waits
    /// here and then reads the written lock. A failed write leaves no lock and
    /// is logged; the run's assertions report the rest.
    pub async fn write_armed_lock(&self, db: &crate::db::CacheDb, actor: &[u8; 32]) {
        let mut armed = self.armed.lock().await;
        let Some(in_secs) = armed.remove(actor) else {
            return;
        };
        let locked_until = (fauna_core::data::Timestamp::now_secs() as u64 + in_secs) as i64;
        if let Err(e) = db.set_locked_until(actor, Some(locked_until)).await {
            tracing::warn!("account_lock test hook: the armed lock was not written: {e:#}");
        }
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

/// `POST /api/v1/test/account_lock/{actor}/at_next_verify?in_secs=N` — arm a
/// lock of `actor` until `now + N`, `now` being the moment `actor`'s next
/// `fauna.auth.verify` reaches the mint gate. `N` must lie in
/// `1..=EMERGENCY_LOCKOUT_SECS`: this shortens a lock, it never lengthens one
/// past what the product can set. Returns `{"ok": true}`.
async fn handle_arm(
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
    state.account_lock_hook.arm(actor, q.in_secs).await;
    Json(json!({ "ok": true })).into_response()
}

/// `GET /api/v1/test/account_lock/{actor}/refusals` — `{"refused": <n>}`, the
/// sign-ins of `actor` this nest has refused as locked.
async fn handle_refusals(
    State(state): State<Arc<AppState>>,
    Path(actor): Path<String>,
) -> impl IntoResponse {
    match parse_actor(&actor) {
        Ok(a) => Json(json!({ "refused": state.account_lock_hook.count(&a) })).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Mount the `/api/v1/test/account_lock/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/account_lock/{actor}/at_next_verify",
            post(handle_arm),
        )
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
        let c = AccountLockHook::default();
        let (a, b) = ([1u8; 32], [2u8; 32]);
        assert_eq!(c.count(&a), 0);
        c.note(&a);
        c.note(&b);
        c.note(&b);
        assert_eq!(c.count(&a), 1);
        assert_eq!(c.count(&b), 2);
    }

    /// An armed lock is written by its own actor's next verify gate, once:
    /// another actor's gate leaves it armed, the actor's first gate writes
    /// `now + N`, and a later gate of the actor writes nothing again.
    #[tokio::test]
    async fn an_armed_lock_is_written_once_by_its_actors_next_verify() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let (a, b) = ([1u8; 32], [2u8; 32]);
        db.create_user(&a, "free", "a").await.unwrap();
        db.create_user(&b, "free", "b").await.unwrap();
        let hook = AccountLockHook::default();
        hook.arm(a, 45).await;

        hook.write_armed_lock(&db, &b).await;
        assert_eq!(
            db.get_locked_until(&a).await.unwrap(),
            None,
            "b's gate spent a's arm"
        );
        assert_eq!(
            db.get_locked_until(&b).await.unwrap(),
            None,
            "b was never armed"
        );

        let before = fauna_core::data::Timestamp::now_secs();
        hook.write_armed_lock(&db, &a).await;
        let after = fauna_core::data::Timestamp::now_secs();
        let locked = db
            .get_locked_until(&a)
            .await
            .unwrap()
            .expect("a's gate wrote the lock");
        assert!(
            (before + 45..=after + 45).contains(&locked),
            "locked_until {locked} is not now + 45 (now {before}..={after})"
        );

        db.set_locked_until(&a, None).await.unwrap();
        hook.write_armed_lock(&db, &a).await;
        assert_eq!(
            db.get_locked_until(&a).await.unwrap(),
            None,
            "the arm was spent once"
        );
    }
}
