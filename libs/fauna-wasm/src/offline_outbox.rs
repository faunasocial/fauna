//! This tab's offline outbox — the one queue every manager that composes an
//! `OfflineQueued` kind puts a gesture in while the SPA has no nest
//! (`account-offline-mutation.md` § The offline-mutation contract): the feed's
//! interactions (`WasmFeedManager::setConnectionState`) and the conversations
//! manager's MLS sends (`WasmConversationsManager::setConnectionState`,
//! `devices.md` § Offline compose). The web twin of linux's process outbox
//! (`apps/fauna-linux/src/offline_outbox.rs`): the same shared queue type
//! (`fauna_account_plane::account_driver::offline_outbox::AccountOutbox`),
//! reading this tab's account runtime when it enqueues, so it needs no
//! store-ready edge to exist and outlives a runtime into the next.
//!
//! It is built by the first connection word either manager reports — until
//! then nothing is queued and every gesture is sent — and both managers feed it
//! the SPA's word from then on, so the gate and the composer read one fact.
//!
//! The MLS half drains only here, in the tab that hosts the account's
//! conversations engine ([`spawn_mls_drain`]): at the lend, at the account
//! store's ready edge (`crate::account_runtime`), on every offline→online
//! edge, and — web's own, its engine being in-memory — when the manager's MLS
//! restore settles, before which nothing is sent or parked. The first three
//! are the edges tui and linux drain at, through the same shared loop
//! (`AccountOutbox::drain_until_settled`) and the same sealing send
//! (`fauna_conversations::session::send_queued_mls_intent`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak as RcWeak};
use std::sync::{Arc, Weak};

use fauna_account_plane::account_driver::offline_outbox::AccountOutbox;
use fauna_conversations::ConversationsManager;
use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_conversations::session::{catch_up_bound_channel, resume_cursor, send_queued_mls_intent};
use fauna_core::offline_intent::{MlsIntentOutcome, MlsIntentSender, OfflineIntentQueue as _};
use fauna_mls::types::ChannelId;
use fauna_protocol::offline_class::is_online;
use wasm_bindgen::prelude::*;

thread_local! {
    static OUTBOX: RefCell<Option<Arc<AccountOutbox>>> = const { RefCell::new(None) };
    static SENDER: RefCell<Option<Rc<WebMlsIntentSender>>> = const { RefCell::new(None) };
}

/// This tab's outbox as the shared composer's queue, once a manager's first
/// connection word built it — lent to the family page's `OfflineQueued`
/// gestures (the approvals decide, the un-deny, the contact pre-approval),
/// which compose one call at a time. `None` before any word: every gesture is
/// sent, as before the outbox existed.
pub(crate) fn wired() -> Option<Arc<AccountOutbox>> {
    OUTBOX.with(|slot| slot.borrow().clone())
}

/// Feed the SPA's connection word to this tab's outbox, building it on the
/// first word, and return it for the caller to lend. Back online, queued MLS
/// sends drain (`devices.md` § Offline compose → *Drain*, on reconnect).
pub(crate) fn set_connection_state(word: &str) -> Arc<AccountOutbox> {
    let (outbox, came_online) = OUTBOX.with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.as_ref() {
            Some(outbox) => {
                let was_online = is_online(&outbox.connection_state());
                outbox.set_connection_state(word);
                (Arc::clone(outbox), !was_online && is_online(word))
            }
            None => {
                let outbox = outbox_over(word);
                // A retry or discard of an MLS intent drains the released
                // channel through this tab's engine now.
                outbox.set_drain_wake(Arc::new(spawn_mls_drain));
                *slot = Some(Arc::clone(&outbox));
                (outbox, false)
            }
        }
    });
    if came_online {
        spawn_mls_drain();
    }
    outbox
}

/// Lend the outbox to the conversations manager and register its sealing
/// send, after which whatever an earlier run left queued drains. Called on
/// every connection word the conversations manager reports; only the first
/// for a given manager lends (a later word's reconnect edge is
/// [`set_connection_state`]'s).
pub(crate) fn lend_to_conversations(outbox: &Arc<AccountOutbox>, sender: WebMlsIntentSender) {
    let already_lent = SENDER.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|lent| Weak::ptr_eq(&lent.manager, &sender.manager))
    });
    if already_lent {
        return;
    }
    if let Some(manager) = sender.manager.upgrade() {
        manager.set_offline_queue(Some(Arc::clone(outbox) as _));
    }
    SENDER.with(|slot| *slot.borrow_mut() = Some(Rc::new(sender)));
    spawn_mls_drain();
}

/// Drain this tab's queued MLS sends through the registered conversations
/// engine. A no-op with no outbox or no engine, when the engine's manager has
/// been freed, when the running account runtime serves another account than
/// the engine's, and — inside the shared loop — while the connection word reads
/// offline (the store-ready and reconnect edges drain again). The outbox
/// serializes drains, so two edges firing together send nothing twice.
pub(crate) fn spawn_mls_drain() {
    let Some(outbox) = OUTBOX.with(|slot| slot.borrow().clone()) else {
        return;
    };
    let Some(sender) = SENDER.with(|slot| slot.borrow().clone()) else {
        return;
    };
    if sender.manager.strong_count() == 0
        || crate::account_runtime::handle_for(&sender.actor_id_hex).is_none()
    {
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        outbox.drain_until_settled(Some(sender.as_ref())).await;
    });
}

/// e2e-only: the `app_outbox_drain_now` poke (`fauna_e2e_agent::APP_OUTBOX_DRAIN_NOW`)
/// — this tab's app drain loop rung now ([`spawn_mls_drain`], the loop the
/// lend, the store-ready edge and every reconnect run), for a test that has
/// just released the e2e drain hold, whose held pass ended the loop.
/// Fire-and-forget: the barrier is the effect. Keyed on `test-helpers` alone
/// (convention 15 rule (b)).
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = appOutboxDrainNowForTest)]
pub fn app_outbox_drain_now_for_test() {
    spawn_mls_drain();
}

/// e2e-only: the `outbox_drain_hold` command (`fauna_e2e_agent::OUTBOX_DRAIN_HOLD`)
/// — set or clear this tab's in-process outbox drain hold
/// (`fauna_account_plane::outbox::set_outbox_drain_hold`). A tab has no
/// environment or filesystem, so the hold file the desktop apps honour cannot
/// reach it. Keyed on `test-helpers` alone (convention 15 rule (b)).
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = setOutboxDrainHoldForTest)]
pub fn set_outbox_drain_hold_for_test(held: bool) {
    fauna_account_plane::outbox::set_outbox_drain_hold(held);
}

/// The outbox the unsent-changes doors act through: this tab's, once a
/// connection word built it (its MLS wake drains a released channel), else one
/// over the same runtime with no wake — the list and the doors work before the
/// first word too.
fn doors() -> Arc<AccountOutbox> {
    wired().unwrap_or_else(|| outbox_over("disconnected"))
}

/// An outbox over this tab's account runtime with the outbox carry seated
/// (`succession-aftermath.md` § Re-key scope, the device-local outbox row) —
/// both of this tab's builds go through here, as the UniFFI seat's and
/// linux's: every app drain loop first carries what a retired identity this
/// browser owes left undrained (its IndexedDB store opened at rest), and the
/// unsent-changes list shows what has not moved yet.
fn outbox_over(word: &str) -> Arc<AccountOutbox> {
    let outbox = AccountOutbox::over_source(Arc::new(crate::account_runtime::handle), word);
    match fauna_account_store::root::StoreRoot::platform() {
        Ok(root) => outbox.set_carry(Some(Arc::new(
            fauna_account_plane::outbox_carry::OutboxCarry::new(
                fauna_client_accounts::AccountRegistry::new(Arc::new(
                    fauna_client_accounts::WebIdentityStore,
                )),
                root,
            ),
        ))),
        Err(e) => tracing::warn!("outbox carry unseated: {e:#}"),
    }
    outbox
}

fn intent_id(hex_id: &str) -> Result<[u8; 16], JsValue> {
    fauna_core::offline_intent::parse_intent_id(hex_id)
        .ok_or_else(|| JsValue::from_str(&format!("not an intent id: {hex_id}")))
}

/// Every unsent change this tab's outbox holds — the unsent-changes list
/// (`account-offline-mutation.md` § The offline-mutation contract →
/// *The unsent-changes surface*, points 1 and 4): an array of
/// `{ intent_id, kind, scope, state: { state, reason_code? }, created_at,
/// attempts, description: { label, target } }`, scope order then compose
/// order. The UniFFI twin is `unsent_changes`.
#[wasm_bindgen(js_name = unsentChanges)]
pub fn unsent_changes() -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        let changes = doors()
            .unsent_changes()
            .await
            .map_err(|e| JsValue::from_str(&e))?;
        crate::rpc::to_js(&changes)
    })
}

/// Retry the not-sent change `intentId` (hex): back to waiting at its place in
/// its scope, its drain started. Resolves `false` for no such not-sent change.
#[wasm_bindgen(js_name = retryUnsentChange, unchecked_return_type = "Promise<boolean>")]
pub fn retry_unsent_change(intent_id_hex: String) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        let retried = doors()
            .retry_intent(intent_id(&intent_id_hex)?)
            .await
            .map_err(|e| JsValue::from_str(&e))?;
        Ok(JsValue::from_bool(retried))
    })
}

/// Discard the not-sent change `intentId` (hex), behind the surface's confirm.
/// Resolves `false` for no such not-sent change (a waiting one is never
/// deleted).
#[wasm_bindgen(js_name = discardUnsentChange, unchecked_return_type = "Promise<boolean>")]
pub fn discard_unsent_change(intent_id_hex: String) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        let discarded = doors()
            .discard_intent(intent_id(&intent_id_hex)?)
            .await
            .map_err(|e| JsValue::from_str(&e))?;
        Ok(JsValue::from_bool(discarded))
    })
}

/// Web's sealing send: the shared [`send_queued_mls_intent`] over the
/// conversations manager's own engine and its own channel cursors (the ones
/// `pollConversations` walks with, so the drain's catch-up and the receive
/// poll never fold one record twice). Holds everything weakly: a manager
/// dropped by an actor switch drains nothing.
///
/// Web's engine is in-memory: after a reload it holds no group until the
/// manager's `restoreMlsState` has settled, so until then the send answers
/// retry and holds no group — never the shared send's "no longer a member"
/// park, which would strand the message for good — and the restore's
/// settling drains again (`devices.md` § Offline compose → *Drain*).
pub(crate) struct WebMlsIntentSender {
    pub(crate) actor_id_hex: String,
    pub(crate) manager: Weak<ConversationsManager>,
    pub(crate) fauna_mls: Weak<FaunaMlsBackend>,
    pub(crate) cursors: RcWeak<RefCell<HashMap<ChannelId, i64>>>,
    pub(crate) restored: RcWeak<Cell<bool>>,
}

impl WebMlsIntentSender {
    /// Has the manager's MLS restore settled, so its engine's groups are the
    /// whole truth? False too once the manager is gone.
    fn restore_settled(&self) -> bool {
        self.restored.upgrade().is_some_and(|r| r.get())
    }
}

#[async_trait::async_trait(?Send)]
impl MlsIntentSender for WebMlsIntentSender {
    async fn send_mls_intent(&self, intent_id: [u8; 16], payload: &[u8]) -> MlsIntentOutcome {
        let (Some(manager), Some(fauna_mls), Some(cursors)) = (
            self.manager.upgrade(),
            self.fauna_mls.upgrade(),
            self.cursors.upgrade(),
        ) else {
            return MlsIntentOutcome::Retry("the conversations engine is gone".to_string());
        };
        if !self.restore_settled() {
            return MlsIntentOutcome::Retry("the conversation is not restored yet".to_string());
        }
        send_queued_mls_intent(&fauna_mls, &manager, intent_id, payload, |channel| {
            let (fauna_mls, manager) = (Arc::clone(&fauna_mls), Arc::clone(&manager));
            async move {
                // Read + copy the cursor (no borrow held across the await),
                // walk, write the advanced cursor back — `pollConversations`'
                // own discipline over the same map.
                let mut cur = *cursors
                    .borrow_mut()
                    .entry(channel)
                    .or_insert_with(|| resume_cursor(&fauna_mls, &channel));
                let polled = catch_up_bound_channel(&fauna_mls, &manager, &channel, &mut cur).await;
                if polled.is_ok() {
                    cursors.borrow_mut().insert(channel, cur);
                }
                polled
            }
        })
        .await
    }

    fn holds_group_of(&self, payload: &[u8]) -> bool {
        self.restore_settled()
            && self.fauna_mls.upgrade().is_some_and(|fauna_mls| {
                fauna_conversations::session::intent_group_held(&fauna_mls, payload)
            })
    }
}
