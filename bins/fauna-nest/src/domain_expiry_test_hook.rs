//! Test-only HTTP endpoint seeding the domain-expiry watch's record.
//!
//! Gated on `test-hooks` alone (like `tlsa_test_hook`), so it is present in the
//! standard `cargo build -p fauna-nest --features test-hooks` e2e build.
//!
//! # Why a hook rather than driving the real watch
//!
//! The watch's input is a **public registry**, reached over the internet. A
//! tier_3 test cannot make `example.org` enter `redemptionPeriod`, and a test
//! that pointed the watch at a local fake would be testing the fake's URL
//! parsing. So the seam is cut at the *record*: everything downstream of the
//! fetch — the stored row, the read kind, the wire encoding, the client's
//! two-arm decision, the role-differentiated copy, and the banner — is exercised
//! for real against a real nest binary, which is precisely the cross-binary
//! drift tier_3 exists to catch. The fetch and parse halves upstream of it are
//! pinned by the unit tests in [`crate::domain_expiry`].
//!
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use fauna_protocol::domain_expiry::DomainExpiryRecord;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

#[derive(Deserialize)]
struct SeedBody {
    /// The primary domain the record is about.
    domain: String,
    /// Registration expiry, unix seconds. Omit for "the registry published
    /// none" — the case where only the status arm can decide.
    #[serde(default)]
    expires_at: Option<i64>,
    /// RDAP status strings, **as a registry would serve them** (either
    /// spelling — the normalization is the code under test).
    #[serde(default)]
    statuses: Vec<String>,
    /// One of `fauna_protocol::domain_expiry::outcomes`.
    outcome: String,
    /// A skip-reason token or a failure detail.
    #[serde(default)]
    detail: Option<String>,
}

/// `POST /api/v1/test/domain-expiry` — write the watch's record directly.
async fn handle_seed(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SeedBody>,
) -> impl IntoResponse {
    let record = DomainExpiryRecord {
        domain: body.domain,
        expires_at: body.expires_at,
        statuses: body.statuses,
        fetched_at: fauna_core::data::Timestamp::now_secs(),
        outcome: body.outcome,
        detail: body.detail,
        extra: Default::default(),
    };
    match state.db.put_domain_expiry(&record).await {
        Ok(()) => Json(json!({ "ok": true, "domain": record.domain })).into_response(),
        Err(e) => ApiError::internal(format!("seed domain expiry: {e}")).into_response(),
    }
}

/// Mount the `/api/v1/test/domain-expiry` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/domain-expiry", post(handle_seed))
}
