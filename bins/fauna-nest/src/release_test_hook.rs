//! Test-only HTTP endpoint seeding the newest release this nest reports on
//! `fauna.nest.info` (`installers/README.md` § Knowing a newer version is out
//! → *The nest's newest-release field*).
//!
//! Gated on `test-hooks` **alone**, so it is present in the standard e2e nest
//! build and never in production. In production the field comes only from the
//! update check's watch channel, which asks the real release host — so without
//! this hook no tier_3 journey could put a known newer release in front of an
//! app, and every run would depend on what the public release host says.
//!
//! Once seeded, the seed REPLACES the update check's answer for this process
//! (`discovery_core::newest_release`), `null` included: a walk that needs "the
//! nest says nothing" seeds `null` and is never at the mercy of a real release.
//! The app's read still goes through the real `fauna.nest.info` door.
//!
//! `GET` answers how many `fauna.nest.info` replies have carried the seed
//! since it was set — a walk's causal anchor (convention 14) that a sign-in's
//! reads have happened before it asserts that nothing was painted.
//!
//! Consumer: `tests/e2e-unified/tests/test_app_version_and_updates.py`.
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::routes::AppState;

/// `None`: never seeded — the update check answers. `Some(seed)`: the seed
/// answers, `Some(None)` being "this nest knows of no release".
static SEED: Mutex<Option<Option<String>>> = Mutex::new(None);

/// Replies that have carried the current seed since it was set.
static READS: AtomicU64 = AtomicU64::new(0);

/// The seeded answer, when the harness has seeded one — counted as a read.
pub(crate) fn seeded() -> Option<Option<String>> {
    let seed = SEED.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if seed.is_some() {
        READS.fetch_add(1, Ordering::SeqCst);
    }
    seed
}

async fn reads_since_seed() -> impl IntoResponse {
    Json(json!({ "reads": READS.load(Ordering::SeqCst) }))
}

#[derive(Deserialize)]
struct SeedRequest {
    /// The release version to report (`"9.9.9"`), or `null` for none.
    version: Option<String>,
}

async fn seed_newest_release(Json(req): Json<SeedRequest>) -> impl IntoResponse {
    let mut seed = SEED.lock().unwrap_or_else(|p| p.into_inner());
    *seed = Some(req.version.clone());
    READS.store(0, Ordering::SeqCst);
    Json(json!({ "newest_release": req.version }))
}

pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/release/newest",
        post(seed_newest_release).get(reads_since_seed),
    )
}
