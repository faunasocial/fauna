//! The device's offline outbox — the one queue every manager that composes an
//! `OfflineQueued` kind puts a gesture in while there is no nest
//! (`account-offline-mutation.md` § The offline-mutation contract): the feed's
//! interactions ([`lend_to_feed`]), the family page's decide and pre-approve
//! gestures ([`queue`]) and the conversations manager's MLS sends
//! ([`lend_to_conversations`], `devices.md` § Offline compose).
//!
//! One per process: it reads the account runtime's handle when it enqueues
//! (`AccountOutbox::over_source`), so it needs no store-ready edge to exist and
//! outlives a runtime into the next, and it starts from the word the offline
//! gate starts from.
//!
//! The MLS half drains only here, in the process that hosts the account's
//! conversations engine ([`spawn_mls_drain`]): at the lend, at the account
//! store's ready edge, and on every offline→online edge — the same three edges
//! tui drains at, through the same shared loop
//! (`AccountOutbox::drain_until_settled`).

use std::sync::{Arc, OnceLock};

use fauna_conversations::ConversationsManager;
use fauna_core::offline_intent::OfflineIntentQueue as _;
use fauna_protocol::offline_class::is_online;
use fauna_sync_engine::offline_intent_queue::AccountOutbox;

fn outbox() -> &'static Arc<AccountOutbox> {
    static OUTBOX: OnceLock<Arc<AccountOutbox>> = OnceLock::new();
    OUTBOX.get_or_init(|| {
        let outbox =
            AccountOutbox::over_source(Arc::new(crate::account_runtime::handle), "disconnected");
        // A retry or discard of an MLS intent (the unsent-changes doors,
        // reached through [`queue`]) drains the released channel now.
        outbox.set_drain_wake(Arc::new(spawn_mls_drain));
        outbox
    })
}

/// The connection word changed — the same word, from the same place, as the
/// offline gate's (`app.rs`'s `update_connection_indicator`), so a control the
/// gate keeps live is exactly a control whose gesture queues. Back online,
/// queued MLS sends drain (`devices.md` § Offline compose → *Drain*, on
/// reconnect).
pub fn set_connection_state(state: &'static str) {
    let was_online = is_online(&outbox().connection_state());
    outbox().set_connection_state(state);
    if !was_online && is_online(state) {
        spawn_mls_drain();
    }
}

/// The outbox as the shared composer's queue — lent to the family page's
/// `OfflineQueued` gestures (the approvals decide, the un-deny, the contact
/// pre-approval), which compose one call at a time rather than through a
/// long-lived manager.
pub fn queue() -> Arc<dyn fauna_core::offline_intent::OfflineIntentQueue> {
    Arc::clone(outbox()) as _
}

/// Lend the outbox to a freshly built feed manager.
pub fn lend_to_feed(m: &crate::feed::host::LinuxFeedManager) {
    m.set_offline_intent_queue(Arc::clone(outbox()) as _);
}

/// Lend the outbox to the conversations manager — called as the conversations
/// session starts, after which whatever an earlier run left queued drains.
pub fn lend_to_conversations(m: &ConversationsManager) {
    m.set_offline_queue(Some(Arc::clone(outbox()) as _));
    spawn_mls_drain();
}

/// Drain this device's queued MLS sends through the live conversations session
/// on the runtime it was started on. A no-op with no session, and — inside the
/// shared loop — while the connection word reads offline or the account
/// runtime is not running yet (the store-ready edge drains again). The outbox
/// serializes drains, so two edges firing together send nothing twice.
pub fn spawn_mls_drain() {
    let Some((session, runtime)) = crate::conversations::conv_backend::active_session_on_runtime()
    else {
        return;
    };
    let outbox = Arc::clone(outbox());
    runtime.spawn(async move {
        outbox.drain_until_settled(Some(session.as_ref())).await;
    });
}
