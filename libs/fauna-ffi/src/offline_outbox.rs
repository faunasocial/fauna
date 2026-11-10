//! The device's offline outbox on the UniFFI apps — the one queue every manager
//! that composes an `OfflineQueued` kind puts a gesture in while there is no
//! nest (`account-offline-mutation.md` § The offline-mutation contract): the
//! feed's interactions (`FfiFeedManager`) and the conversations manager's MLS
//! sends (`devices.md` § Offline compose). linux's `offline_outbox.rs` is the
//! same seat over the crates it links natively.
//!
//! One per process: it reads the account runtime's handle when it enqueues
//! (`AccountOutbox::over_source`), so it needs no store-ready edge to exist and
//! outlives a runtime into the next. Unlike linux it is wired by the app's
//! FIRST connection word (`FfiFeedManager::set_connection_state`), not at
//! process start: an app that never feeds a word keeps sending every gesture
//! online, as before the outbox existed, rather than queueing behind a word
//! that never turns "connected".
//!
//! The MLS half drains here, in the process that hosts the account's
//! conversations engine, through the shared loop
//! (`AccountOutbox::drain_until_settled`) on the store's runtime
//! (`crate::account_runtime::store_runtime`): at the lend, at the account
//! store's ready edge, and on every offline→online word — the edges tui and
//! linux drain at. No app calls an export for any of them.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use fauna_conversations::ConversationsSession;
use fauna_core::offline_intent::OfflineIntentQueue as _;
use fauna_protocol::offline_class::is_online;
use fauna_sync_engine::account_runtime::AccountHandleSource;
use fauna_sync_engine::offline_intent_queue::AccountOutbox;

/// Starts one drain of `outbox` through `session` — the process's spawns it on
/// the store's runtime ([`spawn_on_store_runtime`]); a test counts it.
pub(crate) type StartDrain = fn(Arc<ConversationsSession>, Arc<AccountOutbox>);

/// The outbox and the conversations session it is lent to.
pub(crate) struct DeviceOutbox {
    /// Wired by the first connection word.
    outbox: OnceLock<Arc<AccountOutbox>>,
    /// The session this process's conversations engine runs in — held weakly:
    /// the factory's stash owns it, and a released session drains nothing.
    session: Mutex<Weak<ConversationsSession>>,
    start_drain: StartDrain,
    /// Rung by the outbox when a retry or discard released an MLS channel
    /// (`AccountOutbox::set_drain_wake`) — the process's own
    /// [`DeviceOutbox::drain`].
    drain_wake: fn(),
}

impl DeviceOutbox {
    pub(crate) const fn new(start_drain: StartDrain, drain_wake: fn()) -> Self {
        Self {
            outbox: OnceLock::new(),
            session: Mutex::new(Weak::new()),
            start_drain,
            drain_wake,
        }
    }

    /// The outbox, once the first word wired it.
    pub(crate) fn wired(&self) -> Option<Arc<AccountOutbox>> {
        self.outbox.get().cloned()
    }

    /// The app's connection word. The first wires the outbox over `source` and
    /// lends it to the session already built, if any; a word that turns the
    /// device online — the first one included, which may follow queued sends
    /// an earlier run left — starts a drain. Returns the outbox.
    pub(crate) fn feed_word(
        &self,
        source: impl FnOnce() -> AccountHandleSource,
        word: &str,
    ) -> Arc<AccountOutbox> {
        let mut first = false;
        let outbox = Arc::clone(self.outbox.get_or_init(|| {
            first = true;
            let outbox = AccountOutbox::over_source(source(), word);
            outbox.set_drain_wake(Arc::new(self.drain_wake));
            outbox
        }));
        let was_online = !first && is_online(&outbox.connection_state());
        outbox.set_connection_state(word);
        if first && let Some(session) = self.session() {
            session
                .manager()
                .set_offline_queue(Some(Arc::clone(&outbox) as _));
        }
        if !was_online && is_online(word) {
            self.drain();
        }
        outbox
    }

    /// The session factory's edge: `session` hosts the account's engine now.
    /// Lends the outbox to its manager when a word has wired one, and drains
    /// whatever an earlier run left queued.
    pub(crate) fn lend_to_session(&self, session: &Arc<ConversationsSession>) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(session);
        if let Some(outbox) = self.wired() {
            session.manager().set_offline_queue(Some(outbox as _));
            self.drain();
        }
    }

    fn session(&self) -> Option<Arc<ConversationsSession>> {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()
    }

    /// Start a drain through the lent session. A no-op before a word wired the
    /// outbox or with no live session — and, inside the shared loop, while the
    /// word reads offline. The outbox serializes drains, so two edges firing
    /// together send nothing twice.
    pub(crate) fn drain(&self) {
        if let (Some(outbox), Some(session)) = (self.wired(), self.session()) {
            (self.start_drain)(session, outbox);
        }
    }
}

/// The process's outbox.
pub(crate) static DEVICE: DeviceOutbox =
    DeviceOutbox::new(spawn_on_store_runtime, wake_device_drain);

/// The process outbox's MLS wake: drain through the lent session.
fn wake_device_drain() {
    DEVICE.drain();
}

/// The outbox the unsent-changes doors act through: the process's, once a
/// connection word wired it (its MLS wake drains a released channel), else
/// one over the same runtime with no wake — the list and the doors work
/// before the first word too.
fn doors() -> Arc<AccountOutbox> {
    DEVICE
        .wired()
        .unwrap_or_else(|| AccountOutbox::over_source(runtime_source(), "disconnected"))
}

fn intent_id(hex_id: &str) -> Result<[u8; 16], crate::FfiError> {
    fauna_core::offline_intent::parse_intent_id(hex_id)
        .ok_or_else(|| crate::general_err(format!("not an intent id: {hex_id}")))
}

/// Every unsent change this device's outbox holds — the unsent-changes list
/// (`account-offline-mutation.md` § The offline-mutation contract →
/// *The unsent-changes surface*, points 1 and 4): scope order then compose
/// order, each in its state (waiting / held / not sent with its code) and
/// described in user words. The wasm twin is `unsentChanges`.
#[fauna_uniffi_async::export]
pub async fn unsent_changes()
-> Result<Vec<fauna_core::offline_intent::UnsentChange>, crate::FfiError> {
    doors()
        .unsent_changes()
        .await
        .map_err(|e| crate::general_err(format!("read the unsent changes: {e}")))
}

/// Retry the not-sent change `intent_id` (hex): it returns to waiting at its
/// place in its scope and its drain starts. `false` = no such not-sent change.
#[fauna_uniffi_async::export]
pub async fn retry_unsent_change(intent_id_hex: String) -> Result<bool, crate::FfiError> {
    doors()
        .retry_intent(intent_id(&intent_id_hex)?)
        .await
        .map_err(|e| crate::general_err(format!("retry the change: {e}")))
}

/// Discard the not-sent change `intent_id` (hex), behind the surface's
/// confirm — the one user-initiated deletion of a pending write. `false` = no
/// such not-sent change (a waiting one is never deleted).
#[fauna_uniffi_async::export]
pub async fn discard_unsent_change(intent_id_hex: String) -> Result<bool, crate::FfiError> {
    doors()
        .discard_intent(intent_id(&intent_id_hex)?)
        .await
        .map_err(|e| crate::general_err(format!("discard the change: {e}")))
}

/// The handle source the process's outbox reads: this process's account
/// runtime.
pub(crate) fn runtime_source() -> AccountHandleSource {
    Arc::new(crate::account_runtime::handle)
}

/// Spawn the shared drain loop on the runtime the account store's install
/// recorded; none recorded yet → nothing, and the store-ready edge drains
/// again.
fn spawn_on_store_runtime(session: Arc<ConversationsSession>, outbox: Arc<AccountOutbox>) {
    let Some(rt) = crate::account_runtime::store_runtime() else {
        return;
    };
    rt.spawn(async move {
        outbox.drain_until_settled(Some(session.as_ref())).await;
    });
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn session() -> Arc<ConversationsSession> {
        let engine = Arc::new(
            fauna_mls::engine::MlsEngine::new_in_memory(
                fauna_core::identity::ActorKeypair::from_secret([0x5D; 32]),
            )
            .expect("in-memory engine"),
        );
        let self_actor = engine.identity_actor_id();
        ConversationsSession::from_parts(
            engine,
            Arc::new(fauna_conversations::backends::mock::InertConversationsRpc),
            "me@host.test".to_string(),
            self_actor,
            None,
        )
    }

    fn no_runtime() -> AccountHandleSource {
        Arc::new(|| None)
    }

    fn lent(session: &ConversationsSession) -> Option<String> {
        session
            .manager()
            .offline_queue()
            .map(|q| q.connection_state())
    }

    /// One outbox reaches the feed manager and the conversations manager:
    /// before any word nothing is lent (every send stays online); the first
    /// word lends the session built before it, and a session built after
    /// gets the same outbox at its build.
    #[test]
    fn one_lend_reaches_the_feed_and_the_conversations_manager() {
        static DRAINS: AtomicUsize = AtomicUsize::new(0);
        let device = DeviceOutbox::new(
            |_, _| {
                DRAINS.fetch_add(1, Ordering::SeqCst);
            },
            || {},
        );
        let early = session();
        device.lend_to_session(&early);
        assert!(lent(&early).is_none(), "no word yet: MLS sends stay online");

        let outbox = device.feed_word(no_runtime, "disconnected");
        let early_queue = early.manager().offline_queue().expect("the word lends it");
        assert!(Arc::ptr_eq(
            &(Arc::clone(&outbox) as Arc<dyn fauna_core::offline_intent::OfflineIntentQueue>),
            &early_queue
        ));

        let later = session();
        device.lend_to_session(&later);
        assert_eq!(lent(&later).as_deref(), Some("disconnected"));
        assert!(Arc::ptr_eq(
            &device.feed_word(no_runtime, "disconnected"),
            &outbox
        ));
        assert_eq!(
            DRAINS.load(Ordering::SeqCst),
            1,
            "only the later lend starts a drain (the shared loop rests while offline); \
             an offline word starts none"
        );
    }

    /// An offline→online word starts a drain through the lent session; a word
    /// that keeps the device online, or one with no session, starts none.
    #[test]
    fn an_offline_to_online_word_starts_a_drain() {
        static DRAINS: AtomicUsize = AtomicUsize::new(0);
        let device = DeviceOutbox::new(
            |_, _| {
                DRAINS.fetch_add(1, Ordering::SeqCst);
            },
            || {},
        );
        device.feed_word(no_runtime, "disconnected");
        device.feed_word(no_runtime, "connected");
        assert_eq!(DRAINS.load(Ordering::SeqCst), 0, "no session: no drain");

        let s = session();
        device.lend_to_session(&s);
        assert_eq!(DRAINS.load(Ordering::SeqCst), 1, "the lend drains");
        device.feed_word(no_runtime, "connected");
        assert_eq!(DRAINS.load(Ordering::SeqCst), 1, "still online: none");
        device.feed_word(no_runtime, "unreachable");
        device.feed_word(no_runtime, "connected");
        assert_eq!(DRAINS.load(Ordering::SeqCst), 2, "back online: a drain");

        drop(s);
        device.feed_word(no_runtime, "disconnected");
        device.feed_word(no_runtime, "connected");
        assert_eq!(
            DRAINS.load(Ordering::SeqCst),
            2,
            "a released session drains nothing"
        );
    }
}
