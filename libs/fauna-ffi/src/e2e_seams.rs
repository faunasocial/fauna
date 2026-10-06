//! The loud surfaces' e2e seams for the four UniFFI apps (windows, macOS, iOS,
//! android) — the shared-Rust half of `fauna_e2e_agent::{RECONNECT_BACKOFF,
//! CONNECTION_REPORTS_KEY, PAINTED_ERRORS_KEY}`, so each app's leg is a thin
//! call where its indicator takes its state and where it publishes its e2e
//! state, never a Swift/Kotlin/C# re-implementation of the counting. The fourth
//! seam, `ALERT_SWEEP_WAKE`, lives beside the loop it wakes
//! (`crate::critical_alerts`). tui is the reference leg.
//!
//! `test-helpers` only (convention 15 rule (b)): the `*-ffi-test` flavors carry
//! these; the production flavors — the shipped artifacts — compile them out.

use std::sync::{Arc, Mutex};

use fauna_e2e_contract::{ConnectionReports, PaintedErrorTally};

use crate::nest_client::FfiConnectionState;
use crate::{FfiError, FfiNestClient};

#[cfg(feature = "test-helpers")]
#[uniffi::export]
impl FfiNestClient {
    /// Pace this client's reconnect retries — `{"initial_ms": N, "max_ms": M}` —
    /// or restore the production bounds with `{}`: the e2e seam behind
    /// `fauna_e2e_agent::RECONNECT_BACKOFF`, parsed by the natives' shared
    /// parser so every app refuses the same malformed payloads. The pace only,
    /// never the `Unreachable` threshold; it reaches the running supervisor on
    /// its next backed-off failure.
    ///
    /// # Errors
    ///
    /// - [`FfiError::General`] on a malformed payload (convention 11: a pace
    ///   that did not land would leave the production one in force).
    pub fn set_reconnect_backoff_for_test(&self, payload_json: String) -> Result<(), FfiError> {
        let payload: serde_json::Value =
            serde_json::from_str(&payload_json).map_err(|e| FfiError::General {
                msg: format!("reconnect_backoff payload: {e}"),
            })?;
        let bounds = fauna_e2e_contract::reconnect_backoff_bounds(&payload).map_err(|msg| {
            FfiError::General {
                msg: format!("reconnect_backoff: {msg}"),
            }
        })?;
        self.nest_arc().set_reconnect_backoff_for_test(bounds);
        Ok(())
    }

    /// The `launch_token` state value, as JSON text — this client's held
    /// session bearer's schedule on the app's own clock
    /// (`fauna_e2e_agent::LAUNCH_TOKEN_KEY` owns the shape and names whose
    /// bearer each app reports; `fauna_e2e_contract::launch_token_json` builds
    /// it). The UniFFI apps' bearer is this client's `WsChallengeBearer`, not
    /// the launch machine's, so this is the read the wrong-clock refresh
    /// witness (case M) makes on them. Read without minting and without
    /// waiting, so a synchronous state builder can call it; `"null"` ("cannot
    /// answer yet") while a mint is in flight.
    pub fn launch_token_json_for_test(&self) -> String {
        match self.nest_arc().auth().held_bearer_for_test() {
            Some(held) => fauna_e2e_contract::launch_token_json(
                held.expires_at_secs,
                fauna_protocol::client_clock::now_secs_or_zero(),
                &held.own_session_ids,
            )
            .to_string(),
            None => "null".to_string(),
        }
    }
}

#[cfg(feature = "test-helpers")]
#[fauna_uniffi_async::export]
impl FfiNestClient {
    /// Force this client's held session bearer to refresh NOW — drop the
    /// cached token and re-mint over the silent challenge, the path its TTL
    /// and 401 refreshes take — the `launch_refresh_token` agent command's
    /// mechanism on the UniFFI apps (`fauna_e2e_agent::LAUNCH_REFRESH_TOKEN`).
    /// Awaited, so the command's ack lands after the outcome.
    ///
    /// # Errors
    ///
    /// - [`FfiError::General`] when the re-mint fails (convention 11: a
    ///   refresh that did not land must not ack as one).
    pub async fn refresh_held_bearer_for_test(&self) -> Result<(), FfiError> {
        let nest = self.nest_arc();
        nest.auth().clear_token().await;
        nest.auth()
            .ensure_auth()
            .await
            .map(|_| ())
            .map_err(|e| FfiError::General {
                msg: format!("launch_refresh_token: {e}"),
            })
    }
}

/// Give this process a fresh dial budget — every app's per-test `reset` arm
/// calls this, because a native driver factory-resets one long-lived process
/// between tests and a spent burst must not carry into the next test
/// (`fauna_ws_substrate::dial_budget::clear_for_test`).
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn dial_budget_clear_for_test() {
    fauna_ws_substrate::dial_budget::clear_for_test();
}

/// The counter behind `fauna_e2e_agent::CONNECTION_REPORTS_KEY`. An app holds
/// ONE, fed every connection-state value its indicator receives (each
/// `FfiConnectionStateSubscription::next`, repeats included), and publishes
/// [`Self::json`] under that key.
#[derive(uniffi::Object)]
pub struct FfiConnectionReportsForTest {
    inner: Mutex<ConnectionReports>,
}

#[uniffi::export]
impl FfiConnectionReportsForTest {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(ConnectionReports::default()),
        })
    }

    /// Record one report the indicator received.
    pub fn observe(&self, state: FfiConnectionState) {
        let word = fauna_client::ConnectionState::from(state).as_wire_word();
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .observe(word);
    }

    /// The `connection_reports` state value, as JSON text.
    pub fn json(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .json()
            .to_string()
    }
}

/// One element a painted frame showed — its test id and visible text.
#[derive(uniffi::Record)]
pub struct FfiPaintedElement {
    pub id: String,
    pub text: String,
}

/// The tally behind `fauna_e2e_agent::PAINTED_ERRORS_KEY`. An app holds ONE,
/// fed each painted frame's elements (every element, or only the error-shaped
/// ones — the tally filters itself), and publishes [`Self::json`] under that
/// key.
#[derive(uniffi::Object)]
pub struct FfiPaintedErrorTallyForTest {
    inner: Mutex<PaintedErrorTally>,
}

#[uniffi::export]
impl FfiPaintedErrorTallyForTest {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(PaintedErrorTally::default()),
        })
    }

    /// Record one painted frame.
    pub fn observe(&self, frame: Vec<FfiPaintedElement>) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .observe(frame.iter().map(|e| (e.id.as_str(), e.text.as_str())));
    }

    /// The `painted_errors` state value, as JSON text.
    pub fn json(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .json()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The FFI wrappers feed the shared counters faithfully: a repeated word is a
    /// report and never a transition, and a standing error counts once.
    #[test]
    fn the_ffi_counters_count_as_the_shared_contract_does() {
        let reports = FfiConnectionReportsForTest::new();
        for state in [
            FfiConnectionState::Connecting,
            FfiConnectionState::Unreachable,
            FfiConnectionState::Unreachable,
        ] {
            reports.observe(state);
        }
        let json: serde_json::Value = serde_json::from_str(&reports.json()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "reports": 3, "transitions": 1, "word": "unreachable" })
        );

        let tally = FfiPaintedErrorTallyForTest::new();
        let frame = || {
            vec![FfiPaintedElement {
                id: "error-message".into(),
                text: "boom".into(),
            }]
        };
        tally.observe(frame());
        tally.observe(frame());
        let json: serde_json::Value = serde_json::from_str(&tally.json()).unwrap();
        assert_eq!(json["count"], 1, "a standing error counts once");
    }
}
