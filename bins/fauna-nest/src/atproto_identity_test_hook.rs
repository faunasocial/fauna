//! Test-only HTTP endpoint making `fauna.bridges.atproto.get_integration_status`
//! stop NAMING one actor's hosted ATProto identity.
//!
//! Gated on `test-hooks` alone (like `domain_expiry_test_hook`), so it is
//! present in the standard `cargo build -p fauna-nest --features test-hooks`
//! e2e build.
//!
//! # Why a hook, and why it withholds rather than retires
//!
//! The client's custody audit set is the nest's answer **unioned with a floor
//! the client froze itself** (`atproto-identity-custody.md` § The audit floor and
//! the departed-DID alarm): a box that minted an identity and then answers
//! "nothing" about it must still be audited, and its silence must never clear an
//! alarm standing against it. That answer — the identity row, DID and public log
//! all still there, only the status reply leaving it out — is exactly the lie the
//! floor defends against, and no honest nest path produces it. The one real path
//! that makes the nest stop naming an identity is a RETIREMENT, which leaves a
//! tombstone in the public log: that is the attributable, silent arm of the rule,
//! a different case from the one under test. So the seam is cut at the reply,
//! and everything else — the row, the bridge, the directory, the client's floor
//! and the banner — is real.
//!
//! Consumer: `tests/e2e-unified/tests/test_audit_floor_nest_silence.py`.
//! Production never compiles this module.

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
struct WithholdBody {
    /// The actor whose identity the status reply stops (or resumes) naming,
    /// as 64 lowercase hex characters.
    actor_id: String,
    /// `true` to withhold, `false` to name it again.
    withhold: bool,
}

/// `POST /api/v1/test/atproto/withhold-identity` — see [`WithholdBody`].
/// Returns `{"ok": true, "withheld": <bool>}`.
async fn handle_withhold(
    State(state): State<Arc<AppState>>,
    Json(body): Json<WithholdBody>,
) -> impl IntoResponse {
    let actor: [u8; 32] = match hex::decode(body.actor_id.trim())
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    {
        Some(actor) => actor,
        None => {
            return ApiError::bad_request("actor_id must be 64 hex characters").into_response();
        }
    };
    let mut withheld = state
        .atproto_identity_withheld
        .lock()
        .expect("atproto_identity_withheld mutex poisoned");
    if body.withhold {
        withheld.insert(actor);
    } else {
        withheld.remove(&actor);
    }
    Json(json!({ "ok": true, "withheld": body.withhold })).into_response()
}

/// Mount the `/api/v1/test/atproto/withhold-identity` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/atproto/withhold-identity",
        post(handle_withhold),
    )
}
