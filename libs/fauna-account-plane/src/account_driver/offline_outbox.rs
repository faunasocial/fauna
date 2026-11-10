//! `AccountOutbox` — the [`OfflineIntentQueue`] every app shares: the account
//! runtime's durable outbox door plus the app's connection word (the
//! offline-mutation composer's app side).
//!
//! Charter: `account-offline-mutation.md` § The offline-mutation contract. A
//! shared manager holds this behind `Arc<dyn OfflineIntentQueue>` and routes an
//! `OfflineQueued` gesture through `fauna_protocol::offline_intent::send_or_queue`;
//! with no nest the intent goes to [`AccountStoreHandle::enqueue_intent`] with
//! the generic RPC drainer, and the runtime's full pump pass drains it once the
//! nest is back (`crate::outbox::drain_outbox`). The app feeds the connection
//! word it already computes for its offline gate
//! ([`AccountOutbox::set_connection_state`]), so the gate and the composer read
//! one fact.
//!
//! One type for every app: it sits over the driver's [`AccountStoreHandle`],
//! which both hosts hand out — the native one (`fauna_sync_engine`'s account
//! runtime, which re-exports this module as
//! `fauna_sync_engine::offline_intent_queue`) and the web one (`fauna_wasm`'s
//! account runtime, a `spawn_local` task) — so tui, linux, the UniFFI apps and
//! web all build the same queue (priority #2).
//!
//! Three doors: [`OfflineIntentQueue::enqueue`] takes the generic RPC
//! drainer; [`OfflineIntentQueue::enqueue_mls`] an MLS plaintext intent, which
//! only the conversations engine's sealing send drains (`devices.md` §
//! Offline compose); and [`OfflineIntentQueue::enqueue_identity`] the inputs
//! of an identity-bound request, which only the app's identity composer
//! drains, signing as the then-current identity (`account-offline-mutation.md`
//! § The offline-mutation contract → *Identity-bound intents compose at
//! drain*). The two app-only drains run in one shared loop,
//! [`AccountOutbox::drain_until_settled`], at startup and on every reconnect:
//! the MLS leg when the app hands it its conversations engine, the identity
//! leg when the app has seated its composer
//! ([`AccountOutbox::set_identity_sender`]). Nothing else runs them, so the
//! sync agent — which wires no conversations engine and holds no seed —
//! never drains either.
//!
//! And the unsent-changes doors (`account-offline-mutation.md` § The
//! offline-mutation contract → *The unsent-changes surface*, point 4): the
//! list ([`OfflineIntentQueue::unsent_changes`]), and the user's retry and
//! discard of a parked intent, each of which starts the drain that owns the
//! intent's scope — a full pump pass for an RPC intent, the host's app drain
//! ([`AccountOutbox::set_drain_wake`]) for an MLS or identity one.

use std::sync::{Arc, RwLock};

use fauna_account_store::types::IntentDrainer;
use fauna_core::offline_intent::{
    IdentityIntentSender, MLS_INTENT_KIND, MlsIntentSender, OfflineIntentQueue, UnsentChange,
};

use super::{AccountHandleSource, AccountStoreHandle, RUNTIME_ABSENT};
use crate::outbox::DrainReport;

/// How many drain passes one online edge runs before leaving a still-retrying
/// app-only intent to the next reconnect ([`AccountOutbox::drain_until_settled`]).
pub const APP_DRAIN_PASSES: usize = 12;
/// The wait before the second pass; it doubles each pass after.
pub const APP_DRAIN_FIRST_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);
/// The longest wait between two passes.
pub const APP_DRAIN_MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(60);

/// The account runtime's outbox, as the composer's queue.
pub struct AccountOutbox {
    /// Where the runtime's handle is read at each enqueue — a fixed handle
    /// ([`new`](Self::new)) or the seat's live source
    /// ([`over_source`](Self::over_source)).
    source: AccountHandleSource,
    connection: RwLock<String>,
    /// One app-only drain pass at a time: a reconnect edge and a startup
    /// edge may both ask, and two passes over one scope would hand the engine
    /// or the composer the same intent twice (the nest's idempotency table
    /// would absorb the second send, but it would still be sealed or signed
    /// twice).
    app_drain: tokio::sync::Mutex<()>,
    /// How the host that runs this queue's app-only drains is asked to start
    /// one ([`Self::set_drain_wake`]); a retry or discard of an MLS or
    /// identity intent rings it so the released scope drains now, not at the
    /// next reconnect.
    drain_wake: RwLock<Option<AppDrainWake>>,
    /// The app's identity composer, seated by the app that holds the seed
    /// ([`Self::set_identity_sender`]); unseated, the identity leg of
    /// [`Self::drain_until_settled`] sends nothing and the intents wait.
    identity_sender: RwLock<Option<Arc<dyn IdentityIntentSender>>>,
}

/// Starts one app-only drain on the host's own runtime, fire-and-forget —
/// each app's `spawn_mls_drain`.
pub type AppDrainWake = Arc<dyn Fn() + Send + Sync>;

impl AccountOutbox {
    /// Wrap `handle`, starting from the app's current connection word — for
    /// a host that builds the queue at its store-ready edge (tui).
    pub fn new(handle: AccountStoreHandle, connection_state: &str) -> Arc<Self> {
        Self::over_source(Arc::new(move || Some(handle.clone())), connection_state)
    }

    /// Read the runtime's handle from `source` at each enqueue — for a host
    /// whose manager may be built before the store is ready, or outlive one
    /// runtime into the next (the UniFFI apps through `fauna-ffi`, linux,
    /// web): the queue is wired once, at the manager's build, with no
    /// store-ready edge to chase. An enqueue with no runtime running fails
    /// with [`RUNTIME_ABSENT`], and the gesture fails as it would have
    /// without a queue.
    pub fn over_source(source: AccountHandleSource, connection_state: &str) -> Arc<Self> {
        Arc::new(Self {
            source,
            connection: RwLock::new(connection_state.to_string()),
            app_drain: tokio::sync::Mutex::new(()),
            drain_wake: RwLock::new(None),
            identity_sender: RwLock::new(None),
        })
    }

    /// Register how this host starts an app-only drain — where it lends the
    /// queue. Unset, a retried or discarded MLS or identity intent drains at
    /// the next online edge instead.
    pub fn set_drain_wake(&self, wake: AppDrainWake) {
        *self.drain_wake.write().unwrap_or_else(|e| e.into_inner()) = Some(wake);
    }

    /// Seat (or, with `None`, unseat) the app's identity composer — the one
    /// [`IdentityIntentSender`] the identity leg of
    /// [`Self::drain_until_settled`] sends through. Only a process holding
    /// the identity seed seats one.
    pub fn set_identity_sender(&self, sender: Option<Arc<dyn IdentityIntentSender>>) {
        *self
            .identity_sender
            .write()
            .unwrap_or_else(|e| e.into_inner()) = sender;
    }

    /// Settle one parked intent by the user's word, then start the drain that
    /// owns its scope: a full pump pass for an RPC intent, the host's app
    /// drain for an MLS or identity one (the scope's remainder shares its
    /// drainer).
    async fn settle(&self, intent_id: [u8; 16], retry: bool) -> Result<bool, String> {
        let handle = self.handle()?;
        let drainer = handle
            .outbox_rows()
            .await
            .map_err(|e| format!("{e:#}"))?
            .into_iter()
            .find(|r| r.intent_id == intent_id)
            .map(|r| r.drainer);
        let settled = if retry {
            handle.outbox_retry(intent_id).await
        } else {
            handle.outbox_discard(intent_id).await
        }
        .map_err(|e| format!("{e:#}"))?;
        if settled {
            match drainer {
                Some(IntentDrainer::Mls | IntentDrainer::Identity) => {
                    let wake = self
                        .drain_wake
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    if let Some(wake) = wake {
                        wake();
                    }
                }
                _ => handle.wake_outbox_drain(),
            }
        }
        Ok(settled)
    }

    /// The runtime's handle now, or [`RUNTIME_ABSENT`].
    fn handle(&self) -> Result<AccountStoreHandle, String> {
        (self.source)().ok_or_else(|| RUNTIME_ABSENT.to_string())
    }

    /// One pass of the app-only drains: the MLS leg through `mls` when the
    /// app hands it its conversations engine, then the identity leg through
    /// the seated composer when one is seated — each owning its own scopes'
    /// order and parks (`AccountStoreHandle::drain_mls_outbox`,
    /// `AccountStoreHandle::drain_identity_outbox`). With no runtime running
    /// nothing drains, and the report says why.
    pub async fn drain_app_once(&self, mls: Option<&dyn MlsIntentSender>) -> DrainReport {
        let _one_at_a_time = self.app_drain.lock().await;
        let handle = match self.handle() {
            Ok(handle) => handle,
            Err(why) => {
                return DrainReport {
                    errors: vec![why],
                    ..DrainReport::default()
                };
            }
        };
        let mut report = match mls {
            Some(sender) => handle.drain_mls_outbox(sender).await,
            None => DrainReport::default(),
        };
        let identity = self
            .identity_sender
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(sender) = identity {
            let leg = handle.drain_identity_outbox(sender.as_ref()).await;
            report.drained += leg.drained;
            report.newly_parked += leg.newly_parked;
            report.held_behind_park += leg.held_behind_park;
            report.retried += leg.retried;
            report.errors.extend(leg.errors);
        }
        report
    }

    /// Drain on one online edge until nothing is left to retry — the one
    /// loop every app runs at the lend (whatever a previous run queued) and
    /// on every reconnect (`devices.md` § Offline compose → *Drain*;
    /// `account-offline-mutation.md` § The offline-mutation contract →
    /// *Identity-bound intents compose at drain*), over both app-only drains
    /// ([`Self::drain_app_once`]). A retryable intent (the channel not
    /// restored yet, a stalled walk, a transport fault) is tried again on a
    /// backoff of [`APP_DRAIN_FIRST_BACKOFF`] doubling to
    /// [`APP_DRAIN_MAX_BACKOFF`], at most [`APP_DRAIN_PASSES`] passes (about
    /// 8 minutes), and the loop ends as soon as the connection word reads
    /// offline: the next reconnect edge starts afresh. A no-op pass while
    /// offline. Returns the last pass's report; the caller spawns it on its
    /// own runtime.
    pub async fn drain_until_settled(&self, mls: Option<&dyn MlsIntentSender>) -> DrainReport {
        let mut last = DrainReport::default();
        let mut backoff = APP_DRAIN_FIRST_BACKOFF;
        for pass in 0..APP_DRAIN_PASSES {
            if !fauna_protocol::offline_class::is_online(&self.connection_state()) {
                return last;
            }
            if pass > 0 {
                fauna_sleep::sleep(backoff).await;
                backoff = (backoff * 2).min(APP_DRAIN_MAX_BACKOFF);
                if !fauna_protocol::offline_class::is_online(&self.connection_state()) {
                    return last;
                }
            }
            last = self.drain_app_once(mls).await;
            if last != DrainReport::default() {
                tracing::info!(
                    drained = last.drained,
                    parked = last.newly_parked,
                    retried = last.retried,
                    held = last.held_behind_park,
                    errors = ?last.errors,
                    "app outbox drain"
                );
            }
            if last.retried == 0 {
                return last;
            }
        }
        last
    }

    /// The app's connection word changed — call wherever the app updates the
    /// word its offline gate reads.
    pub fn set_connection_state(&self, connection_state: &str) {
        let mut word = self.connection.write().unwrap_or_else(|e| e.into_inner());
        if *word != connection_state {
            *word = connection_state.to_string();
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl OfflineIntentQueue for AccountOutbox {
    fn connection_state(&self) -> String {
        self.connection
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    async fn enqueue(
        &self,
        kind: &'static str,
        scope: String,
        payload: Vec<u8>,
    ) -> Result<[u8; 16], String> {
        self.handle()?
            .enqueue_intent(kind, scope, payload, IntentDrainer::Rpc)
            .await
            .map_err(|e| format!("{e:#}"))
    }

    async fn enqueue_mls(&self, scope: String, payload: Vec<u8>) -> Result<[u8; 16], String> {
        self.handle()?
            .enqueue_intent(MLS_INTENT_KIND, scope, payload, IntentDrainer::Mls)
            .await
            .map_err(|e| format!("{e:#}"))
    }

    async fn enqueue_identity(
        &self,
        kind: &'static str,
        scope: String,
        payload: Vec<u8>,
    ) -> Result<[u8; 16], String> {
        self.handle()?
            .enqueue_intent(kind, scope, payload, IntentDrainer::Identity)
            .await
            .map_err(|e| format!("{e:#}"))
    }

    async fn unsent_changes(&self) -> Result<Vec<UnsentChange>, String> {
        self.handle()?
            .unsent_changes()
            .await
            .map_err(|e| format!("{e:#}"))
    }

    async fn retry_intent(&self, intent_id: [u8; 16]) -> Result<bool, String> {
        self.settle(intent_id, true).await
    }

    async fn discard_intent(&self, intent_id: [u8; 16]) -> Result<bool, String> {
        self.settle(intent_id, false).await
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// A queue wired before any runtime runs (a manager built ahead of the
    /// store) reports the app's word and refuses an enqueue as the runtime
    /// being absent — the gesture then fails as it did with no queue, never
    /// lands anywhere else.
    #[tokio::test]
    async fn a_queue_with_no_runtime_refuses_as_absent_and_tracks_the_word() {
        let outbox = AccountOutbox::over_source(Arc::new(|| None), "disconnected");
        assert_eq!(outbox.connection_state(), "disconnected");
        outbox.set_connection_state("connected");
        assert_eq!(outbox.connection_state(), "connected");
        let refused = outbox
            .enqueue("fauna.posts.interact", "posts.interact:aa".into(), vec![])
            .await;
        assert_eq!(refused, Err(RUNTIME_ABSENT.to_string()));
    }
}
