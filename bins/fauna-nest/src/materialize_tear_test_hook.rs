//! Test-only HTTP endpoints that **tear the next segment-axis materialize** at
//! one of its two named interruption points, leaving the real half-state an
//! interrupted re-seed leaves.
//!
//! Gated on `test-hooks` **alone** (the `recovery_landing_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: the re-seed's flip
//! ([`crate::backup::materialize::materialize_segment_set`]) records an
//! in-flight marker before its first observable write, and a retry completes a
//! flip an interruption left standing — over whatever arrived in between
//! (`backup-destinations.md` § Third destination kind → *Re-seed* → *the
//! in-flight marker*). The Rust pins in `tests/nest_backup_coordinator.rs`
//! stage that half-state in process by un-committing a finished flip; an
//! api-tier test has no in-process access, so without this seam the
//! interrupted-then-retried flip is unreachable from the e2e tier. Nothing here
//! rewrites state: the armed point makes the REAL verb stop where a crash
//! would, so the marker, the containers and (after the adopt) both families
//! are exactly what the production path left, and the retry is the production
//! path too.
//!
//! The two points ([`TearPoint`]):
//!
//! * `between-containers-and-halves` — the marker stands and a journaled
//!   copy's containers (mailboxes, collections) are written; no segment half
//!   is on disk yet;
//! * `after-journal-adopt` — both families are on disk and adopted; the one
//!   transaction that makes them live never ran.
//!
//! **One-shot.** An armed point fires once, on the first materialize that
//! reaches it, and disarms; the torn call answers
//! `fauna.backup.custody_unreadable` carrying [`TORN_DETAIL`], which is what
//! an interrupted run's client sees as a failed call. `GET` reports what is
//! armed and how many tears have fired, so a test asserts the tear happened
//! instead of inferring it.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// The text a torn call's error carries — the one thing a test greps the
/// refusal for, so it can tell the seam's tear from a real integrity failure.
pub const TORN_DETAIL: &str = "torn by the test seam";

/// Where the armed tear stops the verb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TearPoint {
    /// After the marker and the containers, before the first segment half.
    BetweenContainersAndHalves,
    /// After the journal adopt, before the one transaction.
    AfterJournalAdopt,
}

impl TearPoint {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "between-containers-and-halves" => Some(Self::BetweenContainersAndHalves),
            "after-journal-adopt" => Some(Self::AfterJournalAdopt),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::BetweenContainersAndHalves => "between-containers-and-halves",
            Self::AfterJournalAdopt => "after-journal-adopt",
        }
    }
}

/// The armed point and the count of tears fired — one per nest process.
#[derive(Default)]
pub struct MaterializeTear {
    armed: Mutex<Option<TearPoint>>,
    torn: AtomicU64,
}

impl MaterializeTear {
    /// Whether the verb, now at `point`, must stop here. Consumes the arm on a
    /// match, so the tear fires exactly once.
    pub fn take(&self, point: TearPoint) -> bool {
        let mut armed = self.armed.lock().expect("materialize tear lock poisoned");
        if *armed == Some(point) {
            *armed = None;
            self.torn.fetch_add(1, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    pub fn arm(&self, point: Option<TearPoint>) {
        *self.armed.lock().expect("materialize tear lock poisoned") = point;
    }

    fn report(&self) -> serde_json::Value {
        let armed = *self.armed.lock().expect("materialize tear lock poisoned");
        json!({
            "armed": armed.map(TearPoint::as_str),
            "torn": self.torn.load(Ordering::SeqCst),
        })
    }
}

#[derive(Deserialize)]
struct ArmBody {
    /// `between-containers-and-halves`, `after-journal-adopt`, or null to
    /// disarm.
    at: Option<String>,
}

/// `POST /api/v1/test/backup/materialize_tear` — body `{"at": "<point>"}` arms
/// the point for the next materialize, `{"at": null}` disarms. Returns the
/// report `GET` returns.
async fn handle_arm(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ArmBody>,
) -> impl IntoResponse {
    let point = match body.at.as_deref() {
        None => None,
        Some(s) => match TearPoint::parse(s) {
            Some(p) => Some(p),
            None => {
                return ApiError::bad_request(format!(
                    "unknown tear point {s:?}: expected `between-containers-and-halves` \
                     or `after-journal-adopt`"
                ))
                .into_response();
            }
        },
    };
    state.materialize_tear.arm(point);
    Json(state.materialize_tear.report()).into_response()
}

/// `GET /api/v1/test/backup/materialize_tear` — `{"armed": <point|null>,
/// "torn": <n>}`.
async fn handle_report(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.materialize_tear.report())
}

/// Mount the `/api/v1/test/backup/materialize_tear` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/backup/materialize_tear",
        post(handle_arm).get(handle_report),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_armed_point_fires_once_and_only_at_its_own_point() {
        let tear = MaterializeTear::default();
        tear.arm(Some(TearPoint::AfterJournalAdopt));
        assert!(!tear.take(TearPoint::BetweenContainersAndHalves));
        assert!(tear.take(TearPoint::AfterJournalAdopt));
        assert!(!tear.take(TearPoint::AfterJournalAdopt), "one-shot");
        assert_eq!(tear.report()["torn"], 1);
        assert!(tear.report()["armed"].is_null());
    }

    #[test]
    fn the_wire_names_round_trip() {
        for p in [
            TearPoint::BetweenContainersAndHalves,
            TearPoint::AfterJournalAdopt,
        ] {
            assert_eq!(TearPoint::parse(p.as_str()), Some(p));
        }
        assert_eq!(TearPoint::parse("mid-flip"), None);
    }
}
