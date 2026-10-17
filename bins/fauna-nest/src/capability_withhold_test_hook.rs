//! Test-only HTTP endpoints making `fauna.nest.info` leave named tokens out of
//! its advertised capability set, so a test can stand a REAL running nest in
//! for one too old for a feature (`version-compatibility.md` § Dimension 3 —
//! Version & capability negotiation: the client hides or disables what the nest
//! does not advertise, instead of letting the user hit a raw error).
//!
//! # Why a hook
//!
//! Every token but `relay` is decided at compile time
//! ([`crate::discovery_core::capabilities_for`]) and the e2e build compiles
//! them all, so no honest path makes the test nest omit one. The seam is cut
//! at the advertise point ([`crate::discovery_core::nest_info_core`]) and
//! nowhere else: the handlers behind a withheld token keep serving, exactly
//! as an older nest would simply not have told the client about them. The
//! cfg-free `capabilities_for` and its pinned unit tests stay untouched.
//!
//! A client reads the set once per connection (`fauna_protocol::nest_info_memo`
//! — a real nest's capabilities change only across a restart, which is a
//! reconnect), so a withhold or clear is seen by the client's NEXT connection.
//!
//! Gated on `test-hooks` alone (like `bridge_status_test_hook`), so it is
//! present in the standard `cargo build -p fauna-nest --features test-hooks`
//! e2e build. The path avoids `/api/v1/test/capabilities/`, which
//! `rescore_drain_test_hook` owns.
//!
//! Consumer: `tests/e2e-unified/tests/test_feature_limits.py`
//! (`test_a_feature_the_nest_is_too_old_for_is_hidden_not_an_error`).
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::routes::AppState;

/// The advertised set minus every withheld token, order preserved — the one
/// filter [`crate::discovery_core::nest_info_core`] applies.
pub fn without_withheld(capabilities: Vec<String>, withheld: &HashSet<String>) -> Vec<String> {
    capabilities
        .into_iter()
        .filter(|token| !withheld.contains(token))
        .collect()
}

/// `POST /api/v1/test/capability-withhold/{token}` — stop advertising `token`
/// until cleared. Returns `{"ok": true, "token": <token>}`.
async fn withhold(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
) -> impl IntoResponse {
    state
        .capabilities_withheld
        .lock()
        .expect("capabilities_withheld mutex poisoned")
        .insert(token.clone());
    Json(json!({ "ok": true, "token": token })).into_response()
}

/// `POST /api/v1/test/capability-withhold/{token}/clear` — advertise `token`
/// again (test isolation). Returns `{"ok": true}`.
async fn clear(State(state): State<Arc<AppState>>, Path(token): Path<String>) -> impl IntoResponse {
    state
        .capabilities_withheld
        .lock()
        .expect("capabilities_withheld mutex poisoned")
        .remove(&token);
    Json(json!({ "ok": true })).into_response()
}

/// Mount the `/api/v1/test/capability-withhold/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/v1/test/capability-withhold/{token}", post(withhold))
        .route(
            "/api/v1/test/capability-withhold/{token}/clear",
            post(clear),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::discovery::capability;

    #[test]
    fn withheld_tokens_drop_out_and_the_rest_keep_their_order() {
        let caps = vec![
            capability::MAIL.to_string(),
            capability::SUBSCRIPTIONS.to_string(),
            capability::CALENDAR.to_string(),
        ];
        let withheld = HashSet::from([capability::SUBSCRIPTIONS.to_string()]);
        assert_eq!(
            without_withheld(caps.clone(), &withheld),
            vec![
                capability::MAIL.to_string(),
                capability::CALENDAR.to_string()
            ]
        );
        assert_eq!(without_withheld(caps.clone(), &HashSet::new()), caps);
    }

    /// End to end through the advertise point: a withheld token is absent from
    /// `nest_info_core`'s reply, and clearing it brings it back.
    #[tokio::test]
    async fn nest_info_omits_a_withheld_token_until_cleared() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = AppState::for_test(db);
        let advertised = |caps: &[String]| capability::supports(caps, capability::SUBSCRIPTIONS);

        let before = crate::discovery_core::nest_info_core(&state).await;
        assert_eq!(
            advertised(&before.capabilities),
            cfg!(feature = "payments"),
            "precondition: the build's own answer"
        );

        state
            .capabilities_withheld
            .lock()
            .unwrap()
            .insert(capability::SUBSCRIPTIONS.to_string());
        let withheld = crate::discovery_core::nest_info_core(&state).await;
        assert!(!advertised(&withheld.capabilities));
        assert!(
            capability::supports(&withheld.capabilities, capability::MAIL),
            "only the named token is withheld"
        );

        state.capabilities_withheld.lock().unwrap().clear();
        let cleared = crate::discovery_core::nest_info_core(&state).await;
        assert_eq!(
            advertised(&cleared.capabilities),
            cfg!(feature = "payments")
        );
    }
}
