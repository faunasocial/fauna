//! Test-only HTTP endpoint that **ages** a task-delegation advisory lease, so a
//! staleness transition is a state a test can stand in rather than an interval
//! it waits out.
//!
//! ## The rule this exists to test
//!
//! `docs/goal/behavior/participants.md` § Coordination primitive makes
//! staleness the hand-over trigger: a lease with no heartbeat for
//! `fauna_core::delegation::LEASE_STALE_MS` (90 s) has no live runner, so the
//! Task-delegation row stops naming its holder and reads *Waiting*, and the
//! next eligible participant may take over. Two `[app]` outcomes on that page
//! are about exactly that moment — *"a job with no live device to run it says
//! it is waiting, instead of naming a device that is not working"* and *"a job
//! you assign to one device waits for that device rather than moving to
//! another"*.
//!
//! Neither is reachable without a seam. The nest computes `age_ms` from its own
//! monotonic clock (deliberately — no client clock is trusted), so a test can
//! only reach the stale side of the boundary by **waiting 90 s of wall clock**,
//! which `e2e-conventions.md` convention 14 calls defunct on sight: a
//! fixed-delay wait is slower than the state it waits for on every green run
//! and still wrong on a loaded one. The nest's other staleness escape —
//! [`LeaseRegistry::release`](crate::delegation_registry::LeaseRegistry::release)
//! on a revoked grant — frees a lease the *nest itself* holds and cannot touch
//! a device's, which is whose departure these two outcomes are about.
//!
//! ## What this does instead
//!
//! `POST /api/v1/test/delegation/age` back-dates the recorded heartbeat of one
//! actor's lease slots, so the very next `fauna.delegation.observe` reports
//! them that much older. The lease is otherwise untouched: same holder, same
//! class, same last-writer-wins discipline, and the next real heartbeat resets
//! it exactly as it would have.
//!
//! Two properties make it a convention-14 mechanism rather than another timing
//! trick:
//!
//! 1. **The move is a step, not a race.** The test names the age it wants; the
//!    boundary is crossed when the call returns, not at some point during a
//!    budget.
//! 2. **The effect is read back, never assumed.** The reply lists each aged
//!    kind with its resulting `age_ms`, so a test asserts it landed on the
//!    stale side before it asserts anything about the UI. A seam whose effect
//!    cannot be observed is still a race, just a quieter one — the same reason
//!    [`crate::rpc_hold_test_hook`]'s `GET` reports `holding`.
//!
//! ## Scoping
//!
//! The body names the **actor** whose leases to age, so a module driving one
//! account cannot perturb another's on the shared session nest — the lease map
//! is keyed `(actor, task_kind)` and this hook keeps that key. `task_kind` is
//! optional for the common "age everything this account holds" case.
//!
//! Gated on `test-hooks` **alone**, like its sibling hooks, so it is present in
//! the standard `cargo build -p fauna-nest --features test-hooks` e2e build and
//! in no production build whatsoever (`e2e-conventions.md` point 15 — the
//! feature is the boundary, not a runtime env gate). A test driving it is
//! therefore standalone-nest only.
//!
//! Consumer: `tests/e2e-unified/helpers/delegation_lease.py`, and through it
//! `tests/e2e-unified/tests/test_task_delegation.py`. Production never compiles
//! this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

#[derive(Deserialize)]
struct AgeBody {
    /// Hex-encoded 32-byte actor id whose leases to age — the same key the
    /// lease map uses, so this cannot reach another account's slots.
    actor: String,
    /// Restrict to one task kind; omitted ⇒ every kind this actor holds a slot
    /// for.
    #[serde(default)]
    task_kind: Option<String>,
    /// How far to back-date the recorded heartbeat, in milliseconds. A test
    /// asking to cross the staleness boundary passes something comfortably over
    /// `fauna_core::delegation::LEASE_STALE_MS`.
    age_ms: u64,
}

/// `POST /api/v1/test/delegation/age` — back-date an actor's advisory lease
/// heartbeats. Returns `{"aged": [{"task_kind": …, "age_ms": …}, …]}` with the
/// resulting age of each slot touched, so the caller asserts rather than
/// assumes. An actor with no lease slot is not an error — it answers with an
/// empty list, which is the honest reading of "nothing was holding anything".
async fn handle_age(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AgeBody>,
) -> impl IntoResponse {
    let Ok(actor) = fauna_core::hex32::decode(&body.actor) else {
        return ApiError::bad_request("`actor` must be 32 hex-encoded bytes").into_response();
    };
    let aged = state
        .delegation_leases
        .backdate(actor, body.task_kind.as_deref(), body.age_ms);
    let aged: Vec<_> = aged
        .into_iter()
        .map(|(task_kind, age_ms)| json!({ "task_kind": task_kind, "age_ms": age_ms }))
        .collect();
    Json(json!({ "aged": aged })).into_response()
}

/// Mount the `/api/v1/test/delegation/age` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/delegation/age", post(handle_age))
}
