//! The loud surfaces' e2e seams for the four UniFFI apps (windows, macOS, iOS,
//! android) — the shared-Rust half of `fauna_e2e_agent::{RECONNECT_BACKOFF,
//! CONNECTION_REPORTS_KEY, PAINTED_ERRORS_KEY}`, so each app's leg is a thin
//! call where its indicator takes its state and where it publishes its e2e
//! state, never a Swift/Kotlin/C# re-implementation of the counting. The fourth
//! seam, `ALERT_SWEEP_WAKE`, lives beside the loop it wakes
//! (`crate::critical_alerts`). tui is the reference leg. The knock-toast log
//! (`fauna_e2e_agent::KNOCK_NOTICES_KEY`) rides here too: the same shape of
//! seam — a shared counter an app's firing arm feeds and its state publishes.
//! So does convention 17's `painted_text_area` record
//! (`fauna_e2e_agent::PAINTED_TEXT_AREA_KEY`): an app measures each laid-out
//! text element's room and the shared builder keeps the starved ones.
//!
//! `test-helpers` only (convention 15 rule (b)): the `*-ffi-test` flavors carry
//! these; the production flavors — the shipped artifacts — compile them out.

use std::sync::{Arc, Mutex};

use fauna_e2e_contract::{
    ConnectionReports, KnockNotices, LaidOutText, PaintedErrorTally, PaintedTextArea,
};

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
    // A new device holds no launch's reach decision either
    // (`fauna_ws_substrate::reach_hint`).
    fauna_ws_substrate::reach_hint::clear_all_for_test();
}

/// Hold this process's subscriptions author drain — the
/// `subscriptions_hold_author_drain` agent command's mechanism on the UniFFI
/// apps (tui's arm is the reference): every later `drain_auto_approvals` pass
/// reads the queue and commits nothing, so a payment-backed subscribe request
/// stands in the author's §2 while the test reads its paid badge
/// (`monetization.md` § Pillar 3). The flag is the shared process-wide one
/// (`fauna_client_subscriptions::orchestration`), which the in-process author
/// pump reads. Gated like its neighbours (convention 15): the shared fn it
/// calls is itself compiled out of a production flavor.
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn subscriptions_hold_author_drain_for_test() {
    fauna_client_subscriptions::orchestration::hold_author_drain_for_test();
}

/// Release [`subscriptions_hold_author_drain_for_test`]; the next pump tick
/// drains as usual. A no-op when nothing is held.
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn subscriptions_release_author_drain_for_test() {
    fauna_client_subscriptions::orchestration::release_author_drain_for_test();
}

/// Ring the app-only outbox drain now — the `app_outbox_drain_now` agent
/// command's mechanism on the UniFFI apps (`fauna_e2e_agent::APP_OUTBOX_DRAIN_NOW`
/// owns the contract; tui's arm is the reference). It starts the process
/// outbox's own drain through the lent conversations session
/// (`crate::offline_outbox::DEVICE`), the same loop the lend, the store's
/// ready edge and every offline→online word start, never a second one: a test
/// that has just deleted the e2e drain-hold file rings it, because the held
/// pass ended the loop. A no-op before the first connection word or with no
/// session (pre-auth). Fire-and-forget — the barrier is the effect.
#[cfg(feature = "test-helpers")]
#[cfg(feature = "feed-manager")]
#[uniffi::export]
pub fn app_outbox_drain_now_for_test() {
    crate::offline_outbox::DEVICE.drain();
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

/// The log behind `fauna_e2e_agent::KNOCK_NOTICES_KEY` — the knock toasts this
/// app process raised (`notifications` outcome 15). An app holds ONE, calls
/// [`Self::observe`] for every knock push its toast arm receives (before the
/// muted-word gate) and [`Self::record`] for every toast it handed the platform
/// (after it), and publishes [`Self::json`] under that key. The counting is the
/// shared `fauna_e2e_contract::KnockNotices`, tui's and linux's too.
#[derive(uniffi::Object)]
pub struct FfiKnockNoticesForTest {
    inner: Mutex<KnockNotices>,
}

#[uniffi::export]
impl FfiKnockNoticesForTest {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(KnockNotices::default()),
        })
    }

    /// A knock push reached the toast arm (call before the muted gate).
    pub fn observe(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .observe();
    }

    /// A knock toast was raised: the catalog `key` its text resolved from
    /// (`None` for a verbatim text), the painted `text`, and the platform
    /// notification id it replaces under.
    pub fn record(&self, key: Option<String>, text: String, replace_id: Option<u32>) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .record(key.as_deref(), &text, replace_id);
    }

    /// The `knock_notices` state value, as JSON text.
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

        let knocks = FfiKnockNoticesForTest::new();
        knocks.observe();
        knocks.observe();
        knocks.record(
            Some("notifications.row_knock".into()),
            "hello".into(),
            Some(fauna_client_notifications::KNOCK_TOAST_REPLACE_ID),
        );
        let json: serde_json::Value = serde_json::from_str(&knocks.json()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "seen": 2,
                "fired": [{ "key": "notifications.row_knock", "text": "hello", "replace_id": 1003 }],
            })
        );
    }
}

/// A layout box's size in layout units (DIP) — the `pane` of a [`FfiLaidOutText`].
#[derive(uniffi::Record)]
pub struct FfiLayoutSize {
    pub width: f64,
    pub height: f64,
}

/// One id-bearing text element as the app's layout placed it — the UniFFI face
/// of `fauna_e2e_contract::LaidOutText`, which owns the meaning of each field:
/// `width`/`height` the ROOM the layout gave it (capped by every clipping
/// ancestor, a scroll viewport excepted), `pane` its nearest ancestor that has
/// a box, `depth` the box-less ancestors between them.
#[derive(uniffi::Record)]
pub struct FfiLaidOutText {
    pub id: String,
    pub text: String,
    pub width: f64,
    pub height: f64,
    pub pane: Option<FfiLayoutSize>,
    pub depth: u32,
}

/// The record behind `fauna_e2e_agent::PAINTED_TEXT_AREA_KEY` — convention
/// 17's `painted-text-has-area` field. An app holds ONE, fed every laid-out
/// frame's text elements (every one — the shared `PaintedTextArea` keeps the
/// starved ones and caps them), and publishes [`Self::json`] under that key.
#[derive(uniffi::Object)]
pub struct FfiPaintedTextAreaForTest {
    inner: Mutex<PaintedTextArea>,
}

#[uniffi::export]
impl FfiPaintedTextAreaForTest {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(PaintedTextArea::default()),
        })
    }

    /// Replace the record with one laid-out frame.
    pub fn observe(&self, frame: Vec<FfiLaidOutText>) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .observe(frame.into_iter().map(|e| LaidOutText {
                id: e.id,
                text: e.text,
                width: e.width,
                height: e.height,
                pane: e.pane.map(|p| (p.width, p.height)),
                depth: e.depth,
            }));
    }

    /// The `painted_text_area` state value, as JSON text.
    pub fn json(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .json()
            .to_string()
    }
}
