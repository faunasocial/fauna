//! Linux leg of draft-persistence v2 (`docs/goal/behavior/file-sync.md`
//! § Drafts Sync; `docs/goal/ui/conversations.md` § Persistence).
//!
//! Pure trigger glue over the shared core: [`fauna_client_drafts::DraftsSync`]
//! owns the seal, the WS-RPC `fauna.drafts.{get,put}` calls, the launch gate,
//! and the last-saved baseline; the conversations-rail `DraftStore` lives inside
//! the shared [`ConversationsManager`]. This module only wires the two
//! platform-specific triggers:
//!
//! * **Load on launch** — spawn `DraftsSync::load()` on the tokio runtime, then
//!   await `manager.restore_drafts_at(epoch, bytes)` so the composer reflects
//!   drafts the user left on this or another of their devices (async since the
//!   restore probes the recipient it restored — `conversations.md`
//!   § Persistence; the epoch is read before the load — [`restore_when_loaded`]).
//! * **Debounced autosave** — a `ConversationsManager` observer ticks on every
//!   compose edit (`set_compose_body` / `set_compose_subject` / `set_new_thread_body`
//!   all `notify()`); the tick is marshalled onto the GTK main loop (widget-thread
//!   rule, mirroring [`crate::conversations::observer`]) where a generation-counter
//!   debounce waits for ~1.5 s of quiescence, then spawns
//!   `DraftsSync::save_if_changed(manager.drafts_snapshot_bytes())` on the tokio
//!   runtime (the save is async and `NestClient` is tokio-based; the GTK main
//!   thread is not a tokio context).
//!
//! Linux has no interim in-memory draft store to retire — it consumes the shared
//! manager's `DraftStore` directly (leg-step 4 of the cross-app drafts
//! rollout is N/A here). Lifecycle: the observer is dropped by the same
//! `manager.clear_observers()` (`main.rs`) that retires the UI observer on every
//! session rebuild, so this autosave loop self-terminates on logout/re-login.
//!
//! **Leave-flush** (`reserved-folders.md` § The leave-flush promise, row 481):
//! [`flush_now_blocking`] forces an immediate save (`DraftsSync::save_on_leave`,
//! which first reads once when the launch load has not opened the gate) of the
//! manager's current snapshot, bypassing the debounce timer entirely — wired
//! into `main.rs`'s `connect_close_request` handler alongside
//! `feed::host::flush_cues_on_close`, on the paths where the process is about
//! to end (sign-out, no-tray quit). A no-op before the first [`start`] (no
//! sync in the slot yet), matching every other rail's non-fatal-if-absent
//! posture.
//!
//! The debounced autosave itself is [`crate::drafts_autosave::attach_autosave`],
//! shared with [`crate::feed::drafts`] — see that module's doc for why it
//! holds only a `Weak` even though this rail's own singleton is eternal.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use fauna_client::NestClient;
use fauna_client_drafts::{DraftsSync, LaunchRestore};
use fauna_conversations::ConversationsManager;
use fauna_core::identity::ActorKeypair;

use crate::drafts_autosave::DraftsHost;

impl DraftsHost for ConversationsManager {
    // Delegates to the inherent method of the same name — Rust's method
    // resolution always prefers an inherent impl over a trait impl for `.`
    // call syntax, so this is the real snapshot, not infinite recursion.
    fn drafts_snapshot_bytes(&self) -> Vec<u8> {
        self.drafts_snapshot_bytes()
    }
}

/// The conversations compose rail's `__drafts` key (one combined blob per
/// `file-sync.md` § Drafts Sync). Feed / Events reuse the plane later under
/// `"posts"` / `"events"`.
const RAIL: &str = fauna_protocol::drafts::RAIL_CONVERSATIONS;

type ConvDraftsSync = DraftsSync<Arc<NestClient>>;

/// The current session's conversations `DraftsSync`, so the leave-door
/// close handler (which has no other route to it) can force a flush.
/// `conversations::host::manager()` is an eternal singleton (its own doc
/// comment), so overwriting this slot on every `start()` — one per
/// login/re-login — never resurrects a torn-down session's sync.
fn slot() -> &'static Mutex<Option<Arc<ConvDraftsSync>>> {
    static SYNC: OnceLock<Mutex<Option<Arc<ConvDraftsSync>>>> = OnceLock::new();
    SYNC.get_or_init(|| Mutex::new(None))
}

/// Force an immediate, bounded-blocking save of the manager's current
/// snapshot — the leave-door flush. No-op before the first [`start`] (the
/// slot is empty pre-auth). `bounded=true` for the paths where the process is
/// about to exit; see `blocking_flush::run_bounded`'s doc for the split.
pub fn flush_now_blocking(bounded: bool) {
    let Some(sync) = slot().lock().unwrap().clone() else {
        return;
    };
    let manager = crate::conversations::host::manager();
    // Both read NOW, at the door: a teardown that clears the manager next
    // moves its epoch, so the fill below then fills nothing.
    let epoch = manager.identity_epoch();
    let captured = manager.drafts_snapshot_bytes();
    crate::blocking_flush::run_bounded(
        async move {
            if let Err(e) = leave_save(&sync, &manager, epoch, captured).await {
                tracing::warn!("drafts: leave-flush failed: {e}");
            }
        },
        fauna_client_drafts::LEAVE_FLUSH_BUDGET,
        fauna_client_drafts::LEAVE_FLUSH_BUDGET + Duration::from_millis(500),
        bounded,
    );
}

/// The leave door's save of this rail (`reserved-folders.md` § The
/// leave-flush promise): the shared `DraftsSync::save_on_leave` with this
/// rail's FILL rule, `ConversationsManager::fill_drafts_for_leave` — so a door
/// taken before the launch load has lifted the save gate still saves, by one
/// read of its own. `captured` and `epoch` are read at the door. Generic over
/// the transport so its tests run over an in-memory nest.
async fn leave_save<R: fauna_protocol::RpcRequester>(
    sync: &DraftsSync<R>,
    manager: &ConversationsManager,
    epoch: u64,
    captured: Vec<u8>,
) -> Result<bool, fauna_client_drafts::DraftsClientError<R::Error>> {
    sync.save_on_leave(captured, |resting| {
        manager.fill_drafts_for_leave(epoch, &resting)
    })
    .await
}

/// Wire draft persistence for the just-authenticated actor: build the shared
/// [`DraftsSync`], restore drafts on launch, and attach the debounced autosave.
/// Called from the `SessionEstablished` handler alongside `start_conversations_session`.
/// A malformed secret is logged and skipped (drafts simply don't persist) rather
/// than aborting login.
///
/// `succession_predecessors` is the retired identities' `BackupKey`s
/// (`FaunaClient::predecessor_backup_keys()`, resolved ONCE by the caller and
/// shared with the `__mls` plane and the other two drafts rails — never a
/// second registry walk), offered to [`DraftsSync::with_predecessors`] so a
/// successor's launch load opens a rail its predecessor sealed instead of
/// hard-erroring on it (`docs/goal/behavior/succession-aftermath.md` § Re-key
/// scope).
pub fn start(
    manager: Arc<ConversationsManager>,
    nest: Arc<NestClient>,
    secret_hex: &str,
    runtime: &tokio::runtime::Handle,
    succession_predecessors: &[fauna_client_drafts::BackupKey],
) {
    let keypair = match keypair_from_hex(secret_hex) {
        Some(k) => k,
        None => {
            tracing::warn!("drafts: malformed identity secret; draft persistence disabled");
            return;
        }
    };
    let sync = Arc::new(build_sync(nest, &keypair, succession_predecessors));
    *slot().lock().unwrap() = Some(Arc::clone(&sync));
    restore_on_launch(Arc::clone(&manager), Arc::clone(&sync), runtime);
    // A fresh observer channel of the same kind the UI render loop uses; the
    // receiver lives on the GTK main loop so the shared debounce is legal.
    let rx = crate::conversations::observer::attach(&manager);
    crate::drafts_autosave::attach_autosave(manager, rx, sync, runtime.clone(), "drafts");
}

/// Assemble this rail's [`DraftsSync`] — factored out of [`start`] so the
/// call-site pin test below can call it without building a whole session
/// (tui's `conversations/drafts.rs::build_sync`, the same factoring
/// `settings::devices::label_custody` uses for the label plane's twin).
///
/// ⚠ The retired keys are a READ fallback, never a seal root
/// ([`DraftsSync::with_predecessors`]). A successor's launch load beats the
/// post-auth re-seal pass more often than not, and without them it
/// hard-errors, the load gate never lifts, and the user's half-written reply
/// stays invisible for the whole session.
fn build_sync(
    nest: Arc<NestClient>,
    keypair: &ActorKeypair,
    succession_predecessors: &[fauna_client_drafts::BackupKey],
) -> ConvDraftsSync {
    DraftsSync::new(nest, keypair, RAIL).with_predecessors(succession_predecessors.to_vec())
}

/// Decode a 32-byte hex identity seed into an [`ActorKeypair`] (mirrors
/// `conv_backend::keypair_from_hex`; both delegate to the canonical
/// `ActorKeypair::from_secret_hex`).
fn keypair_from_hex(secret_hex: &str) -> Option<ActorKeypair> {
    ActorKeypair::from_secret_hex(secret_hex).ok()
}

/// Spawn the launch load: fetch + unseal this actor's conversation drafts and
/// hand them to the manager. `None` is first run (keep the empty store); a
/// transport/seal error is logged and left non-fatal.
fn restore_on_launch(
    manager: Arc<ConversationsManager>,
    sync: Arc<ConvDraftsSync>,
    runtime: &tokio::runtime::Handle,
) {
    runtime.spawn(restore_when_loaded(
        manager,
        async move { sync.load().await },
    ));
}

/// The body of [`restore_on_launch`], over any load: the shared
/// [`fauna_client_drafts::restore_when_loaded`]. The manager is the eternal
/// singleton, kept across every switch and sign-out, and `main.rs` wipes it
/// before the outgoing client shuts down, so the outgoing account's fetch can
/// answer after the wipe; the shared body reads the identity epoch
/// synchronously before the load is polled and the restore refuses the reply
/// once it has moved (`account-scoping.md` § The scoping taxonomy).
fn restore_when_loaded<E: std::fmt::Display + Send>(
    manager: Arc<ConversationsManager>,
    load: impl std::future::Future<Output = Result<Option<Vec<u8>>, E>> + Send + 'static,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let reader = Arc::clone(&manager);
    let restore = fauna_client_drafts::restore_when_loaded(
        move || reader.identity_epoch(),
        load,
        move |epoch, bytes| async move { manager.restore_drafts_at(epoch, bytes).await },
    );
    async move {
        match restore.await {
            LaunchRestore::Restored(len) => {
                tracing::info!("drafts: restored {len} bytes of conversation drafts")
            }
            LaunchRestore::FirstRun => tracing::debug!("drafts: none persisted yet (first run)"),
            LaunchRestore::Failed(e) => tracing::warn!("drafts: load failed: {e}"),
        }
    }
}

#[cfg(test)]
mod leave_door_tests {
    use super::*;
    use fauna_client_drafts::testing::{MemoryDraftsNest, SharedDraftsNest};

    fn sync(nest: &Arc<MemoryDraftsNest>) -> DraftsSync<SharedDraftsNest> {
        DraftsSync::new(
            SharedDraftsNest(Arc::clone(nest)),
            &ActorKeypair::from_secret([7u8; 32]),
            RAIL,
        )
    }

    fn with_new_thread(body: &str) -> Arc<ConversationsManager> {
        let manager = ConversationsManager::new();
        manager.start_new_conversation();
        manager.set_new_thread_body(body.into());
        manager
    }

    async fn rest(nest: &Arc<MemoryDraftsNest>, bytes: &[u8]) {
        let other = sync(nest);
        other.load().await.unwrap();
        assert!(other.save_if_changed(bytes).await.unwrap());
    }

    /// **A leave door taken before the launch load lifted the gate still
    /// saves the typed draft** (`reserved-folders.md` § The leave-flush
    /// promise). Mutation: put `save_if_changed(&captured)` back in
    /// [`leave_save`] and this reds — the gate is shut, nothing is written.
    #[tokio::test]
    async fn a_door_before_the_launch_load_saves_the_typed_draft() {
        let nest = Arc::new(MemoryDraftsNest::default());
        rest(&nest, &with_new_thread("resting").drafts_snapshot_bytes()).await;

        let typed = with_new_thread("typed before the gate opened");
        let captured = typed.drafts_snapshot_bytes();
        let wrote = leave_save(
            &sync(&nest),
            &typed,
            typed.identity_epoch(),
            captured.clone(),
        )
        .await
        .unwrap();

        assert!(wrote);
        assert_eq!(sync(&nest).load().await.unwrap(), Some(captured));
    }

    /// An untouched composer takes the resting draft: the door writes nothing
    /// over it. Mutation: save `captured` unfilled and this reds.
    #[tokio::test]
    async fn a_door_with_nothing_typed_never_overwrites_the_resting_draft() {
        let nest = Arc::new(MemoryDraftsNest::default());
        let resting = with_new_thread("resting").drafts_snapshot_bytes();
        rest(&nest, &resting).await;
        let before = *nest.puts.lock().unwrap();

        let untouched = ConversationsManager::new();
        let captured = untouched.drafts_snapshot_bytes();
        let wrote = leave_save(
            &sync(&nest),
            &untouched,
            untouched.identity_epoch(),
            captured,
        )
        .await
        .unwrap();

        assert!(!wrote);
        assert_eq!(*nest.puts.lock().unwrap(), before);
        assert_eq!(sync(&nest).load().await.unwrap(), Some(resting));
    }
}

#[cfg(test)]
mod identity_change_tests {
    use super::*;

    /// **A launch load held open across an identity change fills nothing into
    /// the singleton manager the incoming account uses** — the linux leg of
    /// `account-scoping.md` § The scoping taxonomy's rule that the writers of
    /// account-scoped state retire with the drop. Mutation: read the epoch
    /// inside the future after the load returns, and this reds.
    #[tokio::test]
    async fn a_load_the_outgoing_account_started_fills_nothing_after_the_switch() {
        let outgoing = ConversationsManager::new();
        outgoing.start_new_conversation();
        outgoing.set_new_thread_body("the outgoing account's draft".into());
        let blob = outgoing.drafts_snapshot_bytes();

        let manager = ConversationsManager::new();
        let (reply, load) = tokio::sync::oneshot::channel::<Vec<u8>>();
        let task = tokio::spawn(restore_when_loaded(Arc::clone(&manager), async move {
            Ok::<_, String>(Some(load.await.expect("the reply")))
        }));

        // `main.rs`'s switch: wipe the singleton, then the reply lands.
        manager.clear_for_identity_change();
        reply.send(blob.clone()).expect("the restore is waiting");
        task.await.expect("no panic");

        manager.start_new_conversation();
        assert_eq!(
            manager
                .snapshot()
                .new_thread_compose
                .expect("the composer")
                .body_draft,
            "",
            "the outgoing account's reply filled the manager the incoming one uses"
        );

        // A load started after the switch still fills.
        manager.cancel_new_conversation();
        restore_when_loaded(
            Arc::clone(&manager),
            async move { Ok::<_, String>(Some(blob)) },
        )
        .await;
        manager.start_new_conversation();
        assert_eq!(
            manager
                .snapshot()
                .new_thread_compose
                .expect("the composer")
                .body_draft,
            "the outgoing account's draft"
        );
    }
}

#[cfg(test)]
mod succession_fallback_tests {
    use super::*;

    /// **The call-site pin for the `__drafts` read fallback on the conversations
    /// rail.** `fauna-client-drafts`' own tests prove the fallback *works*;
    /// none of them can see THIS app stop passing the walk — the exact vacuity
    /// that let a threading ship with zero production writers while every unit
    /// test stayed green.
    ///
    /// Mutation: drop the `.with_predecessors(..)` in [`build_sync`] and this reds.
    #[test]
    fn the_conversations_rail_offers_the_accounts_retired_roots() {
        let retired = vec![fauna_core::crypto::BackupKey::from_bytes([0x11u8; 32])];
        let sync = build_sync(
            NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate()),
            &ActorKeypair::generate(),
            &retired,
        );

        assert_eq!(
            sync.predecessor_count(),
            1,
            "after a succession this rail is still sealed under a predecessor's \
             root; without the offered key the launch load hard-errors and the \
             user's half-written reply stays invisible for the whole session"
        );
    }

    /// The overwhelmingly common path — an identity that never succeeded — is
    /// unchanged, so a red above is about the threading and not about
    /// construction in general.
    #[test]
    fn an_identity_that_never_succeeded_offers_nothing() {
        let sync = build_sync(
            NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate()),
            &ActorKeypair::generate(),
            &[],
        );
        assert_eq!(sync.predecessor_count(), 0);
    }
}
