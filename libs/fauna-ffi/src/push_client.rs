//! UniFFI façade for the push-subscription-management WS-RPC kinds —
//! `fauna.push.{vapid_key,subscribe,unsubscribe}`. The per-device push
//! subscription surface a client drives: fetch the server's VAPID key (web-push
//! `applicationServerKey`), then register / remove a device's subscription.
//!
//! [`FfiPushClient`] wraps `fauna_client_push::PushClient` (which wraps the
//! shared `NestClient`); it is the native-client twin of the wire calls the
//! Rust-native Linux app would make through `PushClient` directly — letting
//! Apple / Windows / Android reach `fauna.push.*` over WS-RPC instead of the
//! deleted `/api/v1/push/{vapid-key,subscribe}` HTTP twins (priority #2). No
//! push logic client-side — the nest owns subscription storage + delivery; this
//! just composes the requests. The browser/OS push registration (APNs token,
//! `PushManager.subscribe`) stays in the client shell.
//!
//! Only built-in types and this file's own `uniffi::Record`s cross the FFI
//! boundary, so the Go mail-bridge `--no-default-features` build emits it
//! cleanly (same as `fauna-client-search` / `-inbox`).
//!
//! The second half of the file vends the shared registration state machine
//! ([`FfiPushRegistration`]) — the intent bit, the leave-drops and the presence
//! announce every FFI app drives its Settings toggle through.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_push::PushClient;
use fauna_client_push::push::SubscribeRequest;
use fauna_client_push::registration::{
    FileIntentStore, IntentStore, PushIntent, PushRegistration, RegistrationError,
    WS_DEVICE_TRANSPORT, clear_opt_in, ws_device_subscription,
};

use crate::{FfiError, stringify};

/// UniFFI handle for the `fauna.push.{vapid_key,subscribe,unsubscribe}` kinds.
/// Construct via [`crate::nest_client::FfiNestClient::push`]; methods are
/// exposed to Swift as `async throws` and Kotlin as `suspend fun`. The
/// connection actor is the implicit subscriber (no `actor_id` param). Thin
/// wrapper over the shared `fauna_client_push::PushClient`.
#[derive(uniffi::Object)]
pub struct FfiPushClient {
    nest: Arc<NestClient>,
}

impl FfiPushClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> PushClient<Arc<NestClient>> {
        PushClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiPushClient {
    /// `fauna.push.vapid_key` — the server's base64url VAPID public key (the
    /// `applicationServerKey` for web-push `pushManager.subscribe`). APNs
    /// clients don't need it; vended for surface parity with the shared client.
    pub async fn vapid_key(&self) -> Result<String, FfiError> {
        let reply = self.client().vapid_key().await.map_err(stringify)?;
        Ok(reply.public_key)
    }

    /// `fauna.push.subscribe` — register or update this device's push
    /// subscription (idempotent upsert on `device_id`). `transport` is
    /// `"web-push"` (default if `None`) or `"apns"`; for web-push,
    /// `key_p256dh`/`key_auth` are required (the nest validates).
    pub async fn subscribe(
        &self,
        device_id: String,
        endpoint: String,
        key_p256dh: Option<String>,
        key_auth: Option<String>,
        transport: Option<String>,
    ) -> Result<(), FfiError> {
        self.client()
            .subscribe(SubscribeRequest {
                device_id,
                endpoint,
                key_p256dh,
                key_auth,
                transport,
                extra: Default::default(),
            })
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// `fauna.push.unsubscribe` — remove this device's push subscription.
    /// Idempotent (an unknown `device_id` is a no-op nest-side).
    pub async fn unsubscribe(&self, device_id: String) -> Result<(), FfiError> {
        self.client()
            .unsubscribe(device_id)
            .await
            .map_err(stringify)?;
        Ok(())
    }
}

// ── The shared registration machine over the FFI ────────────────────────────
//
// `fauna_client_push::registration` (the install intent bit, the which-actor
// record, the leave-shape drops — `common.md` § Push Notifications →
// *Registration*) vended to the FFI apps. Transport-general: the app hands the
// subscription it holds (`apns` on the Apple targets, whose device token only
// exists inside the OS's async callback) to `enable` / `rearm`; everything else
// — what the Settings toggle renders, when a re-arm may subscribe, what a leave
// gesture drops — is the shared machine's, identical to tui's.

/// FFI mirror of [`fauna_client_push::registration::PushIntent`]: the
/// install-scoped push record.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiPushIntent {
    /// This install opted in and has not since disabled — what the Settings
    /// toggle renders, never the OS permission.
    pub opted_in: bool,
    /// The actor whose nest row is live for this install, if any.
    pub actor: Option<String>,
}

impl From<PushIntent> for FfiPushIntent {
    fn from(i: PushIntent) -> Self {
        Self {
            opted_in: i.opted_in,
            actor: i.actor,
        }
    }
}

/// The transport half of a subscription — everything but the device id, which
/// is the registration's own (one id for the row and the announce).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiPushEndpoint {
    /// `"apns"`, `"web-push"` or `"ws-device"`.
    pub transport: String,
    /// The transport's endpoint: the hex APNs device token, a web-push relay
    /// URL; ignored for `ws-device` (the device id is its endpoint).
    pub endpoint: String,
    pub key_p256dh: Option<String>,
    pub key_auth: Option<String>,
}

fn subscribe_request(device_id: &str, s: FfiPushEndpoint) -> SubscribeRequest {
    if s.transport == WS_DEVICE_TRANSPORT {
        return ws_device_subscription(device_id);
    }
    SubscribeRequest {
        device_id: device_id.to_string(),
        endpoint: s.endpoint,
        key_p256dh: s.key_p256dh,
        key_auth: s.key_auth,
        transport: Some(s.transport),
        extra: Default::default(),
    }
}

/// The stored record at `intent_path` — what the Settings toggle renders. Reads
/// no connection, so the control paints before (and without) one; an absent or
/// unreadable file is "not opted in".
#[uniffi::export]
pub fn push_intent(intent_path: String) -> FfiPushIntent {
    FileIntentStore::new(intent_path).load().into()
}

/// Set the bit at `intent_path` with no connection, keeping the actor record —
/// what a successful `enable` leaves stored, for a test double standing in for
/// [`FfiPushRegistration`]. The test flavors only (convention 15): a production
/// build opts an install in through `enable` alone.
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn push_seed_opt_in(intent_path: String) -> Result<(), FfiError> {
    let store = FileIntentStore::new(intent_path);
    let mut intent = store.load();
    intent.opted_in = true;
    store.save(&intent).map_err(crate::general_err)
}

/// The user's Disable with no connection in hand: clear the bit, keep the actor
/// record so a later leave-drop still removes the row. *Off* stays off even
/// when the app cannot reach its nest to build a registration.
#[uniffi::export]
pub fn push_clear_opt_in(intent_path: String) -> Result<(), FfiError> {
    clear_opt_in(&FileIntentStore::new(intent_path))
        .map(|_| ())
        .map_err(crate::general_err)
}

/// FFI mirror of [`fauna_client_push::registration::StandingFailure`]: why a
/// desktop's push banner cannot reach this machine right now.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiPushStandingFailure {
    /// The sync agent, which posts the banner while the app is closed, did not
    /// answer.
    AgentUnreachable,
    /// The agent answered and has no notification sink here.
    NoSink,
}

/// The push control's standing inline line on a desktop — the shared rule
/// tui renders (`fauna_client_push::registration::standing_failure`): `None`
/// for an opted-out install, the unreachable agent first, then a sink the
/// agent reports absent. `notification_sink` is the agent status's field
/// (`FfiAgentStatus::notification_sink`); `agent_running` whether the agent
/// answered at all.
#[uniffi::export]
pub fn push_standing_failure(
    opted_in: bool,
    agent_running: bool,
    notification_sink: Option<bool>,
) -> Option<FfiPushStandingFailure> {
    use fauna_client_push::registration::{StandingFailure, standing_failure};
    standing_failure(opted_in, agent_running, notification_sink).map(|failure| match failure {
        StandingFailure::AgentUnreachable => FfiPushStandingFailure::AgentUnreachable,
        StandingFailure::NoSink => FfiPushStandingFailure::NoSink,
    })
}

/// UniFFI handle for one install's push registration under the signed-in
/// actor. Construct via
/// [`crate::nest_client::FfiNestClient::push_registration`].
#[derive(uniffi::Object)]
pub struct FfiPushRegistration {
    nest: Arc<NestClient>,
    intent_path: String,
    actor: String,
    device_id: String,
}

impl FfiPushRegistration {
    pub(crate) fn from_nest(
        nest: Arc<NestClient>,
        intent_path: String,
        actor: String,
        device_id: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            nest,
            intent_path,
            actor,
            device_id,
        })
    }

    fn machine(&self) -> PushRegistration<Arc<NestClient>, FileIntentStore> {
        PushRegistration::new(
            Arc::clone(&self.nest),
            FileIntentStore::new(&self.intent_path),
            self.actor.clone(),
            self.device_id.clone(),
        )
    }
}

fn registration_err(e: RegistrationError<fauna_client::NestClientError>) -> FfiError {
    match e {
        RegistrationError::Nest(e) => stringify(e),
        store @ RegistrationError::Store(_) => crate::general_err(store),
    }
}

#[fauna_uniffi_async::export]
impl FfiPushRegistration {
    /// The user's Enable: register `subscription` under this install's device
    /// id, then set the bit. A failure leaves the bit unset — the toggle
    /// settles back off.
    pub async fn enable(&self, subscription: FfiPushEndpoint) -> Result<(), FfiError> {
        self.machine()
            .enable(subscribe_request(&self.device_id, subscription))
            .await
            .map_err(registration_err)
    }

    /// Launch and identity settle: re-register while the bit is set, otherwise
    /// do nothing — never opt a device in. Returns whether a subscribe was
    /// issued.
    pub async fn rearm(&self, subscription: FfiPushEndpoint) -> Result<bool, FfiError> {
        self.machine()
            .rearm(subscribe_request(&self.device_id, subscription))
            .await
            .map_err(registration_err)
    }

    /// The user's Disable: the bit clears first (off stays off even when the
    /// nest cannot be reached), then the row is removed.
    pub async fn disable(&self) -> Result<(), FfiError> {
        self.machine().disable().await.map_err(registration_err)
    }

    /// A leave gesture (switch, sign-out): drop this actor's row. Never touches
    /// the bit; issues nothing on an install with no push history. Best-effort
    /// — the caller proceeds whatever this returns.
    pub async fn drop_actor_row(&self) -> Result<(), FfiError> {
        self.machine()
            .drop_actor_row()
            .await
            .map_err(registration_err)
    }

    /// Announce this install's device on the connection, now and at every
    /// (re)connect (`NestClient::set_push_presence`). The id announced is the
    /// one every row of this registration is keyed under — one field feeds
    /// both — which is the equality `common.md` § Registration → *Every
    /// connection announces* requires before an FFI app may announce.
    pub async fn announce_presence(&self) {
        self.nest.set_push_presence(self.device_id.clone()).await;
    }
}

#[uniffi::export]
impl FfiPushRegistration {
    /// The stored record (what the Settings toggle renders).
    pub fn intent(&self) -> FfiPushIntent {
        push_intent(self.intent_path.clone())
    }

    /// The device id this registration's rows are keyed under and its
    /// connection announces.
    pub fn device_id(&self) -> String {
        self.device_id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apns() -> FfiPushEndpoint {
        FfiPushEndpoint {
            transport: "apns".into(),
            endpoint: "a1b2".into(),
            key_p256dh: Some("pk".into()),
            key_auth: Some("auth".into()),
        }
    }

    #[test]
    fn an_apns_row_is_keyed_under_the_device_id_the_connection_announces() {
        // `announce_presence` sends `self.device_id`; the row must carry the
        // same value, or the nest reads the app's own row as absent and dials
        // APNs while the app is open (`common.md` § Dispatch Logic).
        let nest = NestClient::new(
            "wss://nest.invalid".to_string(),
            crate::keypair_from_bytes(&[7u8; 32]).unwrap(),
        );
        let reg = FfiPushRegistration::from_nest(
            nest,
            "/nonexistent/push-intent.cbor".into(),
            "actor".into(),
            "derived-device-id".into(),
        );
        let req = subscribe_request(&reg.device_id, apns());
        assert_eq!(req.device_id, reg.device_id());
        assert_eq!(req.endpoint, "a1b2");
        assert_eq!(req.transport.as_deref(), Some("apns"));
        assert_eq!(reg.machine().device_id(), "derived-device-id");
    }

    #[test]
    fn a_ws_device_row_names_the_device_as_its_own_endpoint() {
        let req = subscribe_request(
            "dev-1",
            FfiPushEndpoint {
                transport: "ws-device".into(),
                endpoint: "ignored".into(),
                key_p256dh: None,
                key_auth: None,
            },
        );
        assert_eq!(req.endpoint, "dev-1");
        assert!(req.key_p256dh.is_none());
    }
}
