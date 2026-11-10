//! Test-only HTTP endpoint for driving the custody-hosting pump on demand.
//!
//! Gated on the `test-hooks` Cargo feature so this module — and the route it
//! registers — never compile into the production binary (e2e conventions,
//! `testing.md` point 15: the automation surface is compiled out of release
//! artifacts).
//!
//! **Why this exists.** [`CustodyHostingWorker`] ticks every
//! [`crate::custody_hosting_worker::PULL_INTERVAL`] — 15 minutes — with the
//! first tick at boot. A hosting row deposited *after* boot therefore gets no
//! pull for up to a quarter hour, so any test asserting that the custodian
//! nest pulled (or that a receipt reached the owner) would otherwise sleep on
//! a wall clock — exactly the brittleness `testing.md` convention 14 forbids.
//! This poke is the sanctioned alternative: run one sweep synchronously, then
//! assert *state*, never timing.
//!
//! The poke alone does not stop the periodic loop: its first tick fires at
//! boot and the next every 15 minutes, so on a nest that lives across tests
//! (a module-scoped fixture) a scheduled pass can land mid-journey and make a
//! "nothing confirmed yet" precondition a race. `POST .../hold` suspends the
//! periodic loop's passes on this nest until released; `run-now` still runs.
//!
//! `POST .../clock` shifts the pump's pass clock: the expiry skip and the
//! expired-store GC (a month past a witness's expiry) read it, so a test
//! reaches both without waiting — the register door refuses an already-expired
//! witness, so no row can be planted past them.
//!
//! `GET .../handshakes` reads the custodied-fleet handshake tally — how often
//! an owner's device asked this custodian nest for a session, and how often it
//! was admitted — so a journey asserts *whether* this nest was asked at all
//! (the custodian-NEST serve door's ruling 6) from the nest's own count.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use serde_json::json;

use crate::api_error::ApiError;
use crate::custody_hosting_worker::CustodyHostingWorker;
use crate::routes::AppState;

/// `POST /api/v1/test/custody_hosting/run-now` — run exactly one
/// custody-hosting sweep and return its pass report.
///
/// Returns `{"ok": true, "rows": N, "pulled": N, "recorded": N,
/// "adopted_segments": N, "stopped": N, "refused_url": N, "refused_fan_out": N,
/// "expired": N, "stores_reclaimed": N, "failed": N, "owner_refused": N,
/// "receipts_deposited": N,
/// "receipts_failed": N, "recovery_pulled": N, "escrows_custodied": N,
/// "braked": N, "metering_failed": N}`.
/// `rows == 0` is a meaningful negative signal for a test, not an error.
///
/// The sweep runs on the blocking pool driven by this runtime's handle,
/// exactly as [`CustodyHostingWorker::spawn`] does and for the same reasons
/// (blocking-shaped, `!Send` store futures).
async fn handle_run_now(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sweep = tokio::task::spawn_blocking(move || {
        let worker = CustodyHostingWorker::new(state, std::time::Duration::MAX);
        tokio::runtime::Handle::current().block_on(worker.run_once())
    })
    .await;

    match sweep {
        Ok(Ok(report)) => Json(json!({
            "ok": true,
            "rows": report.rows,
            "stopped": report.stopped,
            "refused_url": report.refused_url,
            "refused_fan_out": report.refused_fan_out,
            "expired": report.expired,
            "stores_reclaimed": report.stores_reclaimed,
            "pulled": report.pulled,
            "recorded": report.recorded,
            "adopted_segments": report.adopted_segments,
            "failed": report.failed,
            "owner_refused": report.owner_refused,
            "receipts_deposited": report.receipts_deposited,
            "receipts_failed": report.receipts_failed,
            "recovery_pulled": report.recovery_pulled,
            "escrows_custodied": report.escrows_custodied,
            "braked": report.braked,
            "metering_failed": report.metering_failed,
        }))
        .into_response(),
        Ok(Err(e)) => ApiError::internal(format!("custody hosting run_now: {e:#}")).into_response(),
        Err(e) => ApiError::internal(format!("custody hosting run_now join: {e}")).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct HoldBody {
    held: bool,
}

/// `POST /api/v1/test/custody_hosting/hold` `{"held": bool}` — hold (or
/// release) the periodic loop: while held every scheduled tick skips its
/// pass. Returns `{"ok": true, "held": bool}`.
async fn handle_hold(
    State(state): State<Arc<AppState>>,
    Json(body): Json<HoldBody>,
) -> impl IntoResponse {
    state
        .custody_hosting_periodic_held
        .store(body.held, std::sync::atomic::Ordering::SeqCst);
    Json(json!({ "ok": true, "held": body.held }))
}

#[derive(serde::Deserialize)]
struct ClockBody {
    offset_secs: i64,
}

/// `POST /api/v1/test/custody_hosting/clock` `{"offset_secs": i64}` — shift
/// the pump's pass clock by `offset_secs` from the wall clock (`0` restores
/// it). Only the pump's own `now` moves: the register door and the owner nest's
/// handshake keep the real clock. Returns `{"ok": true, "offset_secs": i64}`.
async fn handle_clock(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ClockBody>,
) -> impl IntoResponse {
    state
        .custody_hosting_clock_offset_secs
        .store(body.offset_secs, std::sync::atomic::Ordering::SeqCst);
    Json(json!({ "ok": true, "offset_secs": body.offset_secs }))
}

/// The custodied-fleet handshake tally ([`AppState::custodied_fleet_handshakes`]):
/// monotone counters, relaxed — a reader compares two readings, never orders
/// them against anything else.
#[derive(Debug, Default)]
pub struct HandshakeTally {
    asked: std::sync::atomic::AtomicU64,
    admitted: std::sync::atomic::AtomicU64,
}

impl HandshakeTally {
    /// A handshake request reached this nest — counted before any check, so a
    /// refused or malformed one is still "asked".
    pub(crate) fn asked(&self) {
        self.asked
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    /// A handshake passed every check and minted a session.
    pub(crate) fn admitted(&self) {
        self.admitted
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn read(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (self.asked.load(Relaxed), self.admitted.load(Relaxed))
    }
}

/// `GET /api/v1/test/custody_hosting/handshakes` — the custodied-fleet
/// handshake tally since this nest booted: `{"ok": true, "asked": N,
/// "admitted": N}`.
async fn handle_handshakes(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let (asked, admitted) = state.custodied_fleet_handshakes.read();
    Json(json!({ "ok": true, "asked": asked, "admitted": admitted }))
}

/// Mount the `/api/v1/test/custody_hosting/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/v1/test/custody_hosting/run-now", post(handle_run_now))
        .route("/api/v1/test/custody_hosting/hold", post(handle_hold))
        .route("/api/v1/test/custody_hosting/clock", post(handle_clock))
        .route(
            "/api/v1/test/custody_hosting/handshakes",
            get(handle_handshakes),
        )
}

#[cfg(test)]
mod tests {
    use super::HandshakeTally;

    #[test]
    fn the_tally_counts_asked_and_admitted_apart() {
        let tally = HandshakeTally::default();
        assert_eq!(tally.read(), (0, 0));
        tally.asked();
        tally.asked();
        tally.admitted();
        assert_eq!(tally.read(), (2, 1));
    }
}
