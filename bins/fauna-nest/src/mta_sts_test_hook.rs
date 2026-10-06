//! Test-only HTTP endpoint scripting the MTA-STS lookup seam.
//!
//! Gated on `test-hooks` **alone** (like `outbound_clock_test_hook`) so it is
//! present in the standard `cargo build -p fauna-nest --features test-hooks`
//! e2e build that `build_node()` produces. It lets a tier_3 test script the
//! result of `fauna.bridges.fetch_mta_sts_policy` for a test domain —
//! installing a `Found` policy (or a `not_published` / `fetch_error` /
//! `invalid` outcome) into [`crate::routes::AppState::mta_sts_override`]
//! without any real `_mta-sts.<domain>` TXT lookup or `.well-known/
//! mta-sts.txt` GET. `fetch_mta_sts_policy_handler` consults the override
//! ahead of the production `mta_sts_fetcher`.
//!
//! Consumer: the mail-bridge MTA-STS enforcement e2e (T2.x). Production
//! never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;
use fauna_mail::outbound::mta_sts::{
    FetchedPolicy, MtaStsLookup, MtaStsMode, MtaStsOutcome, MtaStsPolicy,
};

#[derive(Deserialize)]
struct MtaStsBody {
    /// Domain to script the lookup for (the key the handler reads).
    domain: String,
    /// One `fauna_mail::outbound::mta_sts::MtaStsOutcome` wire token — that
    /// type owns the vocabulary; do not restate it here.
    outcome: String,
    /// For the policy-carrying outcome: one `MtaStsMode` token. Ignored otherwise.
    #[serde(default)]
    mode: Option<String>,
    /// For `"found"`: the `mx:` patterns. Ignored otherwise.
    #[serde(default)]
    mx: Option<Vec<String>>,
    /// For `"found"`: `max_age` seconds (defaults to 86400). Ignored otherwise.
    #[serde(default)]
    max_age_secs: Option<u32>,
    /// For `"found"`: the policy `id=` (defaults to empty). Ignored otherwise.
    #[serde(default)]
    id: Option<String>,
}

/// `POST /api/v1/test/outbound/mta-sts` — install a scripted MTA-STS lookup
/// for `domain`. Returns `{"ok": true, "domain": <domain>}`.
async fn handle_mta_sts(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MtaStsBody>,
) -> impl IntoResponse {
    // Both vocabularies come from their owner in `fauna-mail`, so a scripted
    // lookup here cannot drift from the one the production handler emits — this
    // hook exists to stand in for that handler, and a third hand-written table
    // would have made it able to script an outcome nest never produces.
    let parsed = match MtaStsOutcome::from_wire(&body.outcome) {
        Some(o) => o,
        None => {
            return ApiError::bad_request(format!("invalid outcome: {}", body.outcome))
                .into_response();
        }
    };
    let lookup = match MtaStsLookup::from_outcome(parsed) {
        Some(payload_free) => payload_free,
        // The one outcome that carries a policy body.
        None => {
            let mode = match body.mode.as_deref() {
                None => MtaStsMode::Enforce,
                Some(token) => match token.parse::<MtaStsMode>() {
                    Ok(m) => m,
                    Err(_) => {
                        return ApiError::bad_request(format!("invalid mode: {token}"))
                            .into_response();
                    }
                },
            };
            MtaStsLookup::Found(FetchedPolicy {
                id: body.id.unwrap_or_default(),
                policy: MtaStsPolicy {
                    version: "STSv1".into(),
                    mode,
                    mx: body.mx.unwrap_or_default(),
                    max_age_secs: body.max_age_secs.unwrap_or(86400),
                },
            })
        }
    };
    state
        .mta_sts_override
        .lock()
        .expect("mta_sts_override mutex poisoned")
        .insert(body.domain.clone(), lookup);
    Json(json!({ "ok": true, "domain": body.domain })).into_response()
}

/// Mount the `/api/v1/test/outbound/mta-sts` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/outbound/mta-sts", post(handle_mta_sts))
}
