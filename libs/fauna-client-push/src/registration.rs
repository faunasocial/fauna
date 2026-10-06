//! The push-registration state machine every app shares — the client half of
//! `docs/goal/architecture/apps/common.md` § Push Notifications → *Registration*.
//!
//! Lifted from web's `push-actor.ts` and apple's `PushManager`, which each
//! carried a copy of the same contract (priority #2): one shape, three rules.
//!
//! 1. **A re-arm never opts a device in.** The install-scoped intent bit
//!    ([`PushIntent::opted_in`]) is set only by the user's Enable and cleared
//!    the moment a Disable is issued; [`PushRegistration::rearm`] re-registers
//!    only while it is set. The OS notification permission is never consulted
//!    here — it answers whether a banner may be posted, not whether the user
//!    wants their nest to push to this install.
//! 2. **The subscription follows the signed-in identity.** At most one live row
//!    per install: every leave-shape (switch, sign-out) calls
//!    [`PushRegistration::drop_actor_row`] at the last point the leaving
//!    session's authority is in hand. The drop never touches the intent bit, so
//!    the next identity in front re-arms with no Settings visit; it is
//!    best-effort and never a gate (a leave gesture must complete offline).
//! 3. **Which actor's row is live** ([`PushIntent::actor`]) is recorded beside
//!    the bit, so a drop issues nothing on an install that never subscribed and
//!    a successful drop forgets the row it removed.
//!
//! Both records are install-scoped, never account-scoped (`account-scoping.md`
//! § The scoping taxonomy, class 2): they describe this install's push
//! transport, so no sign-out or account-removal ERASE touches them. Where they
//! persist is the app's ([`IntentStore`]); [`FileIntentStore`] is the native
//! desktop one.
//!
//! The transport is the caller's: the subscription a registration upserts is a
//! plain [`SubscribeRequest`] handed to [`PushRegistration::enable`] /
//! [`PushRegistration::rearm`] — [`ws_device_subscription`] for the desktops'
//! `ws-device` row, [`apns_subscription`] for the Apple apps (whose device
//! token only exists inside the OS's async callback, which is why the
//! subscription is an argument and not part of the registration), a browser
//! subscription elsewhere.
//!
//! **One device id.** The registration is built over the install's derived
//! device id for the signed-in actor and keys every row it writes or removes
//! under it, whatever the request it is handed says — the same id the
//! connection announces as presence (`common.md` § Registration → *Every
//! connection announces*), so the row and the announce cannot drift apart.

use fauna_protocol::RpcRequester;
use fauna_protocol::push::SubscribeRequest;
use serde::{Deserialize, Serialize};

use crate::PushClient;

/// The `transport` string of a `ws-device` row (`common.md` § Transports).
pub const WS_DEVICE_TRANSPORT: &str = "ws-device";

/// The subscription a desktop app registers: its own device id as the
/// endpoint, no keys — delivered over the device's own authenticated
/// connection as `fauna.push.notification` (`common.md` § Transports, the
/// `ws-device` ruling). `device_id` is the install's derived device id for the
/// signed-in actor, the one every connection announces as presence.
pub fn ws_device_subscription(device_id: impl Into<String>) -> SubscribeRequest {
    let device_id = device_id.into();
    SubscribeRequest {
        endpoint: device_id.clone(),
        device_id,
        key_p256dh: None,
        key_auth: None,
        transport: Some(WS_DEVICE_TRANSPORT.to_string()),
        extra: Default::default(),
    }
}

/// The `transport` string of an `apns` row.
pub const APNS_TRANSPORT: &str = "apns";

/// The subscription an Apple app registers: the hex APNs device token as the
/// endpoint, plus the install's push P-256 public key and auth secret
/// (base64url) the nest encrypts the payload to (`common.md` § Registration).
pub fn apns_subscription(
    device_id: impl Into<String>,
    device_token_hex: impl Into<String>,
    key_p256dh: impl Into<String>,
    key_auth: impl Into<String>,
) -> SubscribeRequest {
    SubscribeRequest {
        endpoint: device_token_hex.into(),
        device_id: device_id.into(),
        key_p256dh: Some(key_p256dh.into()),
        key_auth: Some(key_auth.into()),
        transport: Some(APNS_TRANSPORT.to_string()),
        extra: Default::default(),
    }
}

/// The install-scoped push record: the user's own opt-in, and whose row it
/// currently registers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushIntent {
    /// This install completed an Enable and the user has not since disabled
    /// it. What the Settings toggle renders — never the OS permission.
    pub opted_in: bool,
    /// The actor whose nest row is live for this install, or `None` once a
    /// drop removed it (or before any subscribe landed).
    pub actor: Option<String>,
}

impl PushIntent {
    /// Might this install hold a row worth dropping on a leave gesture? Wider
    /// than `opted_in`: a row registered before a Disable that failed to reach
    /// the nest is still worth one more unsubscribe.
    pub fn may_hold_row(&self) -> bool {
        self.opted_in || self.actor.is_some()
    }
}

/// Where an app keeps its [`PushIntent`]. Install-scoped storage only.
pub trait IntentStore: Send + Sync {
    /// The stored record; an absent or unreadable store reads as the default
    /// (not opted in) — the safe direction, since a re-arm never opts in.
    fn load(&self) -> PushIntent;
    /// Persist `intent`.
    fn save(&self, intent: &PushIntent) -> Result<(), String>;
}

/// The first half of a Disable, on its own: clear the bit and keep the actor
/// record (the row may still exist — a later drop retries it). Returns the
/// record as stored. For an app that cannot even reach a connection to build a
/// [`PushRegistration`] over: *off* must stay off regardless.
pub fn clear_opt_in(store: &impl IntentStore) -> Result<PushIntent, String> {
    let mut intent = store.load();
    intent.opted_in = false;
    store.save(&intent)?;
    Ok(intent)
}

/// Why a registration step failed.
#[derive(Debug)]
pub enum RegistrationError<E> {
    /// The nest call failed (unreachable or refused).
    Nest(E),
    /// The intent record could not be written.
    Store(String),
}

impl<E: core::fmt::Display> core::fmt::Display for RegistrationError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Nest(e) => write!(f, "{e}"),
            Self::Store(e) => write!(f, "could not save the push setting: {e}"),
        }
    }
}

/// One install's push registration for the signed-in `actor`.
pub struct PushRegistration<R: RpcRequester, S: IntentStore> {
    push: PushClient<R>,
    store: S,
    actor: String,
    device_id: String,
}

impl<R: RpcRequester, S: IntentStore> PushRegistration<R, S> {
    /// `nest` is the signed-in `actor`'s connection; `device_id` the install's
    /// derived device id for that actor — the key of the one row this install
    /// registers under it, and the id its connections announce.
    pub fn new(nest: R, store: S, actor: impl Into<String>, device_id: impl Into<String>) -> Self {
        Self {
            push: PushClient::new(nest),
            store,
            actor: actor.into(),
            device_id: device_id.into(),
        }
    }

    /// The device id every row of this registration is keyed under.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// `subscription`, keyed under this registration's device id.
    fn keyed(&self, mut subscription: SubscribeRequest) -> SubscribeRequest {
        subscription.device_id = self.device_id.clone();
        subscription
    }

    /// The stored record (what the Settings toggle renders).
    pub fn intent(&self) -> PushIntent {
        self.store.load()
    }

    /// The user's Enable: register the row, then set the bit. A failed
    /// subscribe leaves the bit unset, so the toggle settles back off.
    pub async fn enable(
        &self,
        subscription: SubscribeRequest,
    ) -> Result<(), RegistrationError<R::Error>> {
        self.push
            .subscribe(self.keyed(subscription))
            .await
            .map_err(RegistrationError::Nest)?;
        self.store
            .save(&PushIntent {
                opted_in: true,
                actor: Some(self.actor.clone()),
            })
            .map_err(RegistrationError::Store)
    }

    /// The user's Disable: clear the bit FIRST — the moment the unsubscribe is
    /// issued, not when it lands, so *off* stays off even when the nest cannot
    /// be reached — then remove the row. A failed unsubscribe keeps the actor
    /// record so a later drop retries it; the bit stays clear either way.
    pub async fn disable(&self) -> Result<(), RegistrationError<R::Error>> {
        let mut intent = clear_opt_in(&self.store).map_err(RegistrationError::Store)?;
        self.push
            .unsubscribe(self.device_id.clone())
            .await
            .map_err(RegistrationError::Nest)?;
        intent.actor = None;
        self.store.save(&intent).map_err(RegistrationError::Store)
    }

    /// Launch and identity settle: re-register under the signed-in actor while
    /// the bit is set; otherwise do nothing — never opt a device in. Returns
    /// whether a subscribe was issued. Re-registering on every launch is an
    /// idempotent upsert of the same `(actor, device)` key, so it also restores
    /// a row the nest lost.
    pub async fn rearm(
        &self,
        subscription: SubscribeRequest,
    ) -> Result<bool, RegistrationError<R::Error>> {
        let intent = self.store.load();
        if !intent.opted_in {
            return Ok(false);
        }
        self.push
            .subscribe(self.keyed(subscription))
            .await
            .map_err(RegistrationError::Nest)?;
        self.store
            .save(&PushIntent {
                opted_in: true,
                actor: Some(self.actor.clone()),
            })
            .map_err(RegistrationError::Store)?;
        Ok(true)
    }

    /// A leave gesture (switch, sign-out): drop this actor's row, issued by the
    /// leaving session itself. Never touches the bit. Best-effort: the caller
    /// proceeds whatever this returns. Issues nothing on an install with no
    /// push history.
    pub async fn drop_actor_row(&self) -> Result<(), RegistrationError<R::Error>> {
        let intent = self.store.load();
        if !intent.may_hold_row() {
            return Ok(());
        }
        self.push
            .unsubscribe(self.device_id.clone())
            .await
            .map_err(RegistrationError::Nest)?;
        // Forget the row only if the record names it: a drop may be removing a
        // stale row while the record names another, still-live actor.
        if intent.actor.as_deref() == Some(self.actor.as_str()) {
            self.store
                .save(&PushIntent {
                    actor: None,
                    ..intent
                })
                .map_err(RegistrationError::Store)?;
        }
        Ok(())
    }
}

/// The native desktop [`IntentStore`]: one small file under the app's
/// install-scoped directory, written atomically (temp file + rename).
#[cfg(not(target_arch = "wasm32"))]
pub struct FileIntentStore {
    path: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileIntentStore {
    /// The store at `path`; its parent directory is created on first save.
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl IntentStore for FileIntentStore {
    fn load(&self) -> PushIntent {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| fauna_protocol::decode_strict(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, intent: &PushIntent) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = fauna_protocol::encode_canonical(intent).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{FailingRequester, RecordingRequester, block_on};
    use fauna_protocol::push::{SubscribeReply, UnsubscribeReply, UnsubscribeRequest};
    use std::sync::{Arc, Mutex};

    #[derive(Default, Clone)]
    struct MemStore(Arc<Mutex<PushIntent>>);
    impl IntentStore for MemStore {
        fn load(&self) -> PushIntent {
            self.0.lock().unwrap().clone()
        }
        fn save(&self, intent: &PushIntent) -> Result<(), String> {
            *self.0.lock().unwrap() = intent.clone();
            Ok(())
        }
    }

    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.push.subscribe" => fauna_protocol::encode_canonical(&SubscribeReply {
                ok: true,
                extra: Default::default(),
            }),
            "fauna.push.unsubscribe" => fauna_protocol::encode_canonical(&UnsubscribeReply {
                ok: true,
                extra: Default::default(),
            }),
            other => panic!("unhandled kind {other}"),
        }
        .expect("encode")
        .to_vec()
    }

    fn reg(
        store: MemStore,
        actor: &str,
    ) -> (
        Arc<RecordingRequester>,
        PushRegistration<Arc<RecordingRequester>, MemStore>,
    ) {
        let rec = Arc::new(RecordingRequester::new(reply));
        let reg = PushRegistration::new(Arc::clone(&rec), store, actor, "dev-1");
        (rec, reg)
    }

    #[test]
    fn the_ws_device_row_names_itself_as_its_endpoint_with_no_keys() {
        let s = ws_device_subscription("dev-1");
        assert_eq!(s.endpoint, "dev-1");
        assert_eq!(s.device_id, "dev-1");
        assert_eq!(s.transport.as_deref(), Some("ws-device"));
        assert!(s.key_p256dh.is_none() && s.key_auth.is_none());
    }

    #[test]
    fn enable_subscribes_then_sets_the_bit_for_this_actor() {
        let store = MemStore::default();
        let (rec, reg) = reg(store.clone(), "alice");
        block_on(reg.enable(ws_device_subscription("dev-1"))).unwrap();
        assert_eq!(rec.kinds(), vec!["fauna.push.subscribe"]);
        let req: SubscribeRequest = fauna_protocol::decode_strict(&rec.recorded().1).unwrap();
        assert_eq!(req.transport.as_deref(), Some("ws-device"));
        assert_eq!(
            store.load(),
            PushIntent {
                opted_in: true,
                actor: Some("alice".into())
            }
        );
    }

    #[test]
    fn every_row_is_keyed_under_the_registrations_own_device_id() {
        // The id the connection announces is the registration's; a request
        // naming another must not put the row where no announce finds it.
        let (rec, reg) = reg(MemStore::default(), "alice");
        block_on(reg.enable(apns_subscription("some-other-id", "abcd", "pk", "auth"))).unwrap();
        let req: SubscribeRequest = fauna_protocol::decode_strict(&rec.recorded().1).unwrap();
        assert_eq!(req.device_id, reg.device_id());
        assert_eq!(req.transport.as_deref(), Some("apns"));
        assert_eq!(req.endpoint, "abcd");
        assert_eq!(req.key_p256dh.as_deref(), Some("pk"));
        assert_eq!(req.key_auth.as_deref(), Some("auth"));
    }

    #[test]
    fn a_failed_enable_leaves_the_toggle_off() {
        let store = MemStore::default();
        let reg = PushRegistration::new(
            FailingRequester::new("nest down"),
            store.clone(),
            "alice",
            "dev-1",
        );
        assert!(block_on(reg.enable(ws_device_subscription("dev-1"))).is_err());
        assert!(!store.load().opted_in);
    }

    #[test]
    fn disable_clears_the_bit_even_when_the_nest_is_unreachable() {
        let store = MemStore(Arc::new(Mutex::new(PushIntent {
            opted_in: true,
            actor: Some("alice".into()),
        })));
        let reg = PushRegistration::new(
            FailingRequester::new("nest down"),
            store.clone(),
            "alice",
            "dev-1",
        );
        assert!(block_on(reg.disable()).is_err());
        let after = store.load();
        assert!(!after.opted_in, "off stays off");
        assert_eq!(
            after.actor.as_deref(),
            Some("alice"),
            "the row may still exist — a later drop retries it"
        );
    }

    #[test]
    fn disable_unsubscribes_this_device_and_forgets_the_row() {
        let store = MemStore(Arc::new(Mutex::new(PushIntent {
            opted_in: true,
            actor: Some("alice".into()),
        })));
        let (rec, reg) = reg(store.clone(), "alice");
        block_on(reg.disable()).unwrap();
        let req: UnsubscribeRequest = fauna_protocol::decode_strict(&rec.recorded().1).unwrap();
        assert_eq!(req.device_id, "dev-1");
        assert_eq!(store.load(), PushIntent::default());
    }

    #[test]
    fn a_rearm_never_opts_a_device_in() {
        let store = MemStore::default();
        let (rec, reg) = reg(store.clone(), "alice");
        assert!(!block_on(reg.rearm(ws_device_subscription("dev-1"))).unwrap());
        assert!(rec.kinds().is_empty(), "nothing subscribed");
        assert_eq!(store.load(), PushIntent::default());
    }

    #[test]
    fn a_rearm_registers_the_incoming_actor_on_an_opted_in_install() {
        let store = MemStore(Arc::new(Mutex::new(PushIntent {
            opted_in: true,
            actor: None,
        })));
        let (rec, reg) = reg(store.clone(), "bob");
        assert!(block_on(reg.rearm(ws_device_subscription("dev-1"))).unwrap());
        assert_eq!(rec.kinds(), vec!["fauna.push.subscribe"]);
        assert_eq!(store.load().actor.as_deref(), Some("bob"));
    }

    #[test]
    fn a_leave_drop_removes_the_row_and_keeps_the_bit() {
        let store = MemStore(Arc::new(Mutex::new(PushIntent {
            opted_in: true,
            actor: Some("alice".into()),
        })));
        let (rec, reg) = reg(store.clone(), "alice");
        block_on(reg.drop_actor_row()).unwrap();
        assert_eq!(rec.kinds(), vec!["fauna.push.unsubscribe"]);
        assert_eq!(
            store.load(),
            PushIntent {
                opted_in: true,
                actor: None
            }
        );
    }

    #[test]
    fn a_leave_drop_on_an_install_with_no_push_history_sends_nothing() {
        let (rec, reg) = reg(MemStore::default(), "alice");
        block_on(reg.drop_actor_row()).unwrap();
        assert!(rec.kinds().is_empty());
    }

    #[test]
    fn a_drop_of_a_stale_row_keeps_the_record_of_the_live_one() {
        let store = MemStore(Arc::new(Mutex::new(PushIntent {
            opted_in: true,
            actor: Some("bob".into()),
        })));
        let (_rec, reg) = reg(store.clone(), "alice");
        block_on(reg.drop_actor_row()).unwrap();
        assert_eq!(store.load().actor.as_deref(), Some("bob"));
    }

    #[test]
    fn the_file_store_round_trips_and_reads_absent_as_not_opted_in() {
        let dir = std::env::temp_dir().join(format!(
            "fauna-push-intent-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let store = FileIntentStore::new(dir.join("push-intent.cbor"));
        assert_eq!(store.load(), PushIntent::default());
        let intent = PushIntent {
            opted_in: true,
            actor: Some("alice".into()),
        };
        store.save(&intent).unwrap();
        assert_eq!(store.load(), intent);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
