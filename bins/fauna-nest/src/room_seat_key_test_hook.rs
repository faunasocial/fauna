//! Test-only HTTP endpoint that poisons one member's stored reception key on a
//! community room's floor.
//!
//! Gated on `test-hooks` **alone** (the `keypackage_age_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production (`e2e-automation-surface-gating.md`).
//!
//! Why it exists: a seat *loses its key* only by a key the nest stored before
//! every writer validated it (`community-rooms.md` § Implementation status
//! today, *A reception key is FIPS-203-valid before it is ever stored*) — no
//! door can write such a key any more, so no user gesture reaches the state.
//! The hook writes the same bytes the conformance case
//! `a_poisoned_seat_is_skipped_not_frozen_and_heals_on_its_own_rekey` seeds —
//! an all-`0xFF` key of the right length, which fails FIPS 203 validation —
//! through `CacheDb::set_room_member_reception_key`, the one writer with no
//! validation, and does nothing else. What a test then observes is production:
//! the roster projecting the seat's key as absent, and the member's own tend
//! pass re-supplying its key through `room.set_reception_key`.
//!
//! Consumer: `tests/e2e-unified/tests/test_conversation_room_community_seat_lost_key.py`.

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
struct PoisonRequest {
    /// The community room, 64 hex chars.
    room_id: String,
    /// The live user member whose seat loses its key, 64 hex chars.
    principal_id: String,
}

/// `POST /api/v1/test/rooms/poison-seat-key` — overwrite one live user seat's
/// reception key with FIPS-invalid bytes. Returns `{"poisoned": true}`, or
/// `{"poisoned": false}` when the principal holds no live user seat there.
async fn handle_poison(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PoisonRequest>,
) -> impl IntoResponse {
    let Ok(room_id) = fauna_core::hex32::decode(&req.room_id) else {
        return ApiError::bad_request("room_id must be 64 hex chars").into_response();
    };
    let Ok(principal_id) = fauna_core::hex32::decode(&req.principal_id) else {
        return ApiError::bad_request("principal_id must be 64 hex chars").into_response();
    };
    let poisoned = vec![0xFFu8; fauna_mls::wrapped_blob::XWING_ENCAPS_KEY_LEN];
    match state
        .db
        .set_room_member_reception_key(&room_id, &principal_id, &poisoned)
        .await
    {
        Ok(bound) => Json(json!({ "poisoned": bound.is_some() })).into_response(),
        Err(e) => ApiError::internal(format!("poison seat key: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/rooms/poison-seat-key` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/rooms/poison-seat-key", post(handle_poison))
}
