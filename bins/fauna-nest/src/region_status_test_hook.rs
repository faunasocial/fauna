//! Test-only HTTP endpoints that make the admin's region staleness warning
//! reachable (`admin.md` § N. Nest — the region section warns when the nest has
//! stopped hearing from its region's authority).
//!
//! Gated on `test-hooks` **alone**, so it is present in the standard e2e nest
//! build and never in production (convention 15).
//!
//! Why it exists: the nest calls its authority channel stale only when **both**
//! hold (`region_tier::region_get_handler`) — the declared region is enrolled
//! in the registry, and the channel last answered more than `STALE_AFTER`
//! (three days) ago. The compiled-in registry enrols nobody, and the clock is
//! the wall clock, so neither half is reachable from a tier_3 run. This hook
//! supplies both through the production read path rather than around it:
//!
//! - it enrols a **synthetic** authority for the named region — the same
//!   synthetic registry `region_relay_test_hook` signs under, the shape
//!   `region-blocking.md` § The content plane → *What the build owes in tests*
//!   sanctions — in `AppState::region_status_registry_override`, which ONLY the
//!   status read consults (`region_tier::status_registry`). The refresh worker
//!   keeps the real registry, so no fetch to the real log is ever sent;
//! - it writes the refresh record through the production
//!   `record_region_refresh` — one *reached* refresh, moved back past
//!   `STALE_AFTER`, then one failed attempt now — so the staleness verdict is
//!   the production `refresh_staleness` reading `reached_at`, never the last
//!   attempt.
//!
//! The `clear` twin disarms the override and forgets the refresh record, so a
//! shared session nest is left as found.
//!
//! Consumer: `tests/e2e-unified/tests/test_admin_nest.py`.
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use fauna_core::region_authority::{PAYLOAD_KIND_FEATURE_POLICY, RegionCode};
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// How far past `STALE_AFTER` the last answer is moved — wide enough that no
/// clock skew between the hook and the next status read can land it inside.
const MARGIN_SECS: i64 = 60 * 60;

#[derive(Deserialize)]
struct SilentRequest {
    /// The region to enrol the synthetic authority for — the region the nest
    /// declares, or the status read never sees the enrollment.
    region: String,
}

/// `POST /api/v1/test/region/authority-silent` — enrol a synthetic authority for
/// `region` (status read only) and record that its channel last answered
/// `STALE_AFTER` + an hour ago. Returns `{"reached_at": <epoch secs>}`.
async fn arm_silent_authority(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SilentRequest>,
) -> impl IntoResponse {
    let Ok(region) = RegionCode::parse(req.region) else {
        return ApiError::bad_request("malformed region code").into_response();
    };
    *state
        .region_status_registry_override
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some(crate::region_relay_test_hook::synthetic_registry(&region));

    let age = crate::region_tier::STALE_AFTER.as_secs() as i64 + MARGIN_SECS;
    state
        .db
        .record_region_refresh(PAYLOAD_KIND_FEATURE_POLICY, None, true)
        .await;
    match state
        .db
        .test_age_region_refresh(PAYLOAD_KIND_FEATURE_POLICY, age)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return ApiError::internal("the refresh record was not written").into_response();
        }
        Err(e) => return ApiError::internal(format!("{e:#}")).into_response(),
    }
    // A failed attempt now: the channel keeps being tried and keeps not
    // answering, which must not refresh the reading the warning is made from.
    state
        .db
        .record_region_refresh(
            PAYLOAD_KIND_FEATURE_POLICY,
            Some("synthetic authority unreachable (test hook)"),
            false,
        )
        .await;
    match state
        .db
        .region_refresh_state(PAYLOAD_KIND_FEATURE_POLICY)
        .await
    {
        Ok(Some(refresh)) => Json(json!({"reached_at": refresh.reached_at})).into_response(),
        Ok(None) => ApiError::internal("the refresh record vanished").into_response(),
        Err(e) => ApiError::internal(format!("{e:#}")).into_response(),
    }
}

/// `POST /api/v1/test/region/authority-silent/clear` — disarm the synthetic
/// enrollment and forget the refresh record.
async fn clear_silent_authority(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    *state
        .region_status_registry_override
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    match state
        .db
        .test_forget_region_refresh(PAYLOAD_KIND_FEATURE_POLICY)
        .await
    {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => ApiError::internal(format!("{e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/region/authority-silent*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/region/authority-silent",
            post(arm_silent_authority),
        )
        .route(
            "/api/v1/test/region/authority-silent/clear",
            post(clear_silent_authority),
        )
}
