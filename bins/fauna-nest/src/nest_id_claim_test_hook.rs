//! Test-only HTTP endpoint making `fauna.nest.info` CLAIM a nest id other than
//! the one this nest proves, so a test can stand a real running nest in for a
//! backup destination whose claim and proof disagree
//! (`backup-destinations.md` § Create / edit / remove protocol → Create step 1:
//! the enroll binds the id the connection PROVED, and refuses a destination
//! whose own `fauna.nest.info` claim names another nest).
//!
//! # Why a hook
//!
//! A nest's claim is read off its own deployment key
//! ([`crate::discovery_core::nest_info_core`]), the same key its
//! `fauna.auth.nest_handshake` possession proof signs with, so no honest nest
//! ever disagrees with itself — and a disagreeing one is exactly the hostile or
//! broken box the client-side check exists for. The seam is cut at the claim
//! and nowhere else: the handshake keeps proving the real key, so the client
//! sees the one mismatch the rule is about, never a failed proof.
//!
//! Gated on `test-hooks` alone (like `capability_withhold_test_hook`), so it is
//! present in the standard e2e build. Production never compiles this module.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_backup_identity_binding_reseed_fresh.py`
//! (the enroll arm of `docs/features/backup-destinations-and-restore.md`
//! outcome 32).

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::routes::AppState;

/// The id `fauna.nest.info` claims: the override when one is set, else the
/// nest's own — the one substitution [`crate::discovery_core::nest_info_core`]
/// applies.
pub fn claimed_nest_id(own: [u8; 32], claim_override: Option<[u8; 32]>) -> [u8; 32] {
    claim_override.unwrap_or(own)
}

#[derive(Deserialize)]
struct ClaimBody {
    /// 64 hex chars: the id to claim; `null` claims the nest's own again.
    nest_id: Option<String>,
}

/// `POST /api/v1/test/nest-info-claim` with `{"nest_id": "<64 hex>" | null}` —
/// claim that id in `fauna.nest.info` until cleared with `null`. Returns
/// `{"ok": true}`; a malformed id is a 400 and changes nothing.
async fn set_claim(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ClaimBody>,
) -> impl IntoResponse {
    let claim = match body.nest_id.as_deref().map(fauna_core::hex32::decode) {
        None => None,
        Some(Ok(id)) => Some(id),
        Some(Err(e)) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": format!("nest_id: {e}") })),
            )
                .into_response();
        }
    };
    *state
        .nest_id_claim_override
        .lock()
        .expect("nest_id_claim_override mutex poisoned") = claim;
    Json(json!({ "ok": true })).into_response()
}

/// Mount the `/api/v1/test/nest-info-claim` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/nest-info-claim", post(set_claim))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End to end through the claim point: an override replaces the claimed id
    /// and clearing it brings the nest's own back.
    #[tokio::test]
    async fn nest_info_claims_the_override_until_cleared() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = AppState::for_test(db);
        let own = state.nest_identity.public_key_bytes();

        let before = crate::discovery_core::nest_info_core(&state).await;
        assert_eq!(
            before.nest_id, own,
            "precondition: the nest claims its own id"
        );

        let foreign = [0x5a; 32];
        assert_ne!(foreign, own);
        *state.nest_id_claim_override.lock().unwrap() = Some(foreign);
        let claimed = crate::discovery_core::nest_info_core(&state).await;
        assert_eq!(claimed.nest_id, foreign);

        *state.nest_id_claim_override.lock().unwrap() = None;
        let cleared = crate::discovery_core::nest_info_core(&state).await;
        assert_eq!(cleared.nest_id, own);
    }
}
