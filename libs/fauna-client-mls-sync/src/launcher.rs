//! The shared **tokio** trigger for the cross-device MLS state-sync plane —
//! the [`MlsSyncLauncher`] a tokio-runtime client injects into its
//! `ConversationsSession`, run once before the first poll by
//! `start_receive_loop` (`docs/goal/behavior/devices.md` § Cross-device MLS
//! group-state sync, slice 5).
//!
//! The restore/save *logic* is [`crate::orchestration`], which all legs drive;
//! this module is the **trigger** for every tokio-runtime consumer — the
//! native FFI factory (apple / windows / android, `libs/fauna-ffi`
//! `mls_sync_launch.rs`) and the tui (`apps/fauna-tui` `conv_backend.rs`) —
//! the twin of linux's `conv_backend.rs` glib debounce and web's
//! `future_to_promise` chokepoint (priority #2/#4: one orchestration, per-leg
//! triggers only where the runtime genuinely differs). It owns exactly three
//! things:
//!
//! * [`TokioMlsSyncLauncher::launch`] — on login, `restore_and_wire` (design
//!   §5 restore-before-first-poll: `load()` → restore the provider + each
//!   history slice → inject the device-owned-epoch gate + cursor), then the
//!   [`PostRestoreHook`], then attach the autosave.
//! * [`PostRestoreHook`] — the seam for launch-time work that needs the
//!   *wired* plane (today: the folder removal resume,
//!   `fauna_client_folders::orchestration::FolderRemovalResume`). It is a
//!   **required argument** of [`tokio_launcher`] so no consumer forgets it;
//!   `None` is the documented "no folder author on this build" shape (the
//!   Go-bridge `--no-default-features` build). The hook cannot live here:
//!   `fauna-client-folders` depends on `fauna-client-conversations`, which
//!   depends on this crate — the seam is what breaks that cycle.
//! * [`attach_replica_autosave`] — a tokio-timer generation-debounce over the
//!   manager's [`SnapshotObserver`] mutation stream: after [`REPLICA_DEBOUNCE`]
//!   of quiescence, snapshot the provider + every bound channel's history and
//!   hand the owned snapshot to an off-task seal + upload.

use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use fauna_conversations::backend::MlsSyncLauncher;
use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_conversations::{ConversationsManager, SnapshotObserver};
use fauna_core::identity::ActorKeypair;

use crate::orchestration::{self, RestoreRetryEnd};
use crate::store::MlsReplicaTransport;
use crate::sync::MlsStateSync;

/// Quiescence window before a mutation burst is sealed + uploaded — matched to
/// the linux `conv_backend.rs` glib debounce (a burst of engine/store mutations
/// coalesces into one replica upload).
const REPLICA_DEBOUNCE: Duration = Duration::from_millis(1500);

/// Launch-time work that needs the restored plane — run by
/// [`TokioMlsSyncLauncher::launch`] once per launch, after the restore attempt
/// settled (wired **or** permanently failed — the hook's own durable-bytes
/// guards make the failed-path run safe and are what heal a single-device
/// fallback; see `FolderRemovalResume`), and never when the session was torn
/// down mid-launch. The hook receives the still-live backend and MUST be
/// non-fatal: a hook failure logs and waits for the next launch, it never costs
/// the user their session.
#[async_trait]
pub trait PostRestoreHook: Send + Sync {
    /// Run the hook against the launch's backend (upgraded by the launcher —
    /// the session is still alive when this is called).
    async fn run(&self, backend: Arc<FaunaMlsBackend>);
}

/// Build the shared tokio cross-device MLS state-sync launcher over
/// `transport` + the session's `backend`/`manager`, ready to hand to
/// `ConversationsSession::set_mls_sync_launcher`. `None` when `self_secret` is
/// not a 32-byte seed — the plane stays disabled and the client runs
/// single-device (the linux `keypair_from_hex(...).map(...)` shape); every
/// consumer has already rejected a malformed secret, so this is a defensive
/// belt-and-suspenders.
///
/// `post_restore` is deliberately a **required argument** (see
/// [`PostRestoreHook`]); pass `None` only on a build with no folder author.
///
/// The [`MlsStateSync`] ctor is non-async (only `load()` touches the wire), so
/// the whole plane is assembled synchronously at session build; the async
/// restore runs later, inside `start_receive_loop`, before the first poll.
///
/// `aftermath` carries the post-succession `__mls` re-seal's two inputs; see
/// [`SuccessionReseal`]. `SuccessionReseal::default()` is the ordinary
/// never-succeeded case and costs nothing.
///
/// Its `predecessors` carries the retired identities' `BackupKey`s after a
/// succession, so the restore re-seals a replica left sealed to a predecessor
/// *before* it reads it ([`MlsStateSync::with_predecessors`] — a barrier, since
/// a lost race is permanent for the session). Pass an empty `Vec` when the app
/// has no account registry to resolve them from; that is the ordinary
/// never-succeeded path and costs nothing. Its `sink` is where that pass
/// reports its progress ([`MlsStateSync::with_reseal_sink`]) — the app's render
/// channel, or `None` on a build with no surface to tell.
///
/// The post-succession `__mls` re-seal's inputs, bundled because they are one
/// concern: the keys the pass tries, and where it reports what it did. Both
/// empty/absent for every identity that never succeeded — [`Default`] is that
/// case, and it is what an app without an account registry passes.
#[derive(Default)]
pub struct SuccessionReseal {
    /// Retired identities' `BackupKey`s, newest chain-hop first; empty when this
    /// identity never succeeded (the pass is then skipped without a round trip).
    pub predecessors: Vec<crate::BackupKey>,
    /// Where the pass reports its [`crate::ReplicaResealProgress`]; `None` on a
    /// build with no progress surface.
    pub sink: Option<crate::sync::ResealSink>,
}

pub fn tokio_launcher(
    transport: Box<dyn MlsReplicaTransport>,
    self_secret: &[u8],
    backend: Arc<FaunaMlsBackend>,
    manager: Arc<ConversationsManager>,
    post_restore: Option<Arc<dyn PostRestoreHook>>,
    aftermath: SuccessionReseal,
) -> Option<Arc<dyn MlsSyncLauncher>> {
    let seed: [u8; 32] = self_secret.try_into().ok()?;
    let keypair = ActorKeypair::from_secret(seed);
    let mut sync = MlsStateSync::new(transport, &keypair).with_predecessors(aftermath.predecessors);
    if let Some(sink) = aftermath.sink {
        sync = sync.with_reseal_sink(sink);
    }
    let sync = Arc::new(sync);
    // The plane exists from here, so no key-package mint may publish until the
    // launch restore wires its save-before-publish seam (or gives up).
    backend.expect_replica_restore();
    Some(Arc::new(TokioMlsSyncLauncher {
        sync,
        backend: Arc::downgrade(&backend),
        manager: Arc::downgrade(&manager),
        post_restore,
    }))
}

/// The shared tokio launcher — holds the plane's handles captured at session
/// build. Held by the session (via `set_mls_sync_launcher`), so it drops with
/// the session. `backend`/`manager` are `Weak` because [`Self::launch`] may
/// retry a transient launch-load failure for a long time: the in-flight launch
/// future (held by the task awaiting `start_receive_loop`) must not pin a
/// torn-down session's internals, and the failed upgrade is what ends the
/// retry on logout.
struct TokioMlsSyncLauncher {
    sync: Arc<MlsStateSync>,
    backend: Weak<FaunaMlsBackend>,
    manager: Weak<ConversationsManager>,
    post_restore: Option<Arc<dyn PostRestoreHook>>,
}

#[async_trait]
impl MlsSyncLauncher for TokioMlsSyncLauncher {
    async fn launch(&self) {
        // Restore the replica + inject the device-owned-epoch gate/cursor (design §5,
        // before the first poll), retrying a transient (nest-unreachable) load with
        // backoff so a launch blip converges instead of degrading the session to
        // single-device (devices.md § launch resilience). A permanent failure — e.g.
        // a seal/codec fault or any nest rejection — keeps today's single-device
        // fallback after one attempt; the autosave is skipped (a save would no-op
        // anyway — the un-lifted launch gate blocks every upload until a successful
        // `load()`).
        let wired = match orchestration::restore_and_wire_with_retry(
            Arc::clone(&self.sync),
            self.backend.clone(),
            self.manager.clone(),
        )
        .await
        {
            RestoreRetryEnd::Wired(restored) => {
                tracing::info!(
                    "mls-sync: cross-device plane wired ({restored} channel(s) restored from replica)"
                );
                true
            }
            RestoreRetryEnd::Failed(e) => {
                tracing::warn!("mls-sync: replica load failed ({e}); staying single-device");
                false
            }
            RestoreRetryEnd::SessionDropped => {
                tracing::debug!("mls-sync: session dropped during launch retry");
                return;
            }
        };

        // The post-restore hook runs HERE because this is the moment the restored
        // plane becomes live (for the folder removal resume: the removal gate is
        // the backend, and `restore_and_wire` is what injects its `CommitGate` —
        // resuming before the restore would see `NoGate` and defer every gated
        // sentinel to the next launch). It runs on the `Failed` path too — the
        // hook's contract requires that to be safe (see [`PostRestoreHook`] and
        // the durable-bytes guards documented on `FolderRemovalResume`).
        if let Some(hook) = &self.post_restore
            && let Some(backend) = self.backend.upgrade()
        {
            hook.run(backend).await;
        }

        if !wired {
            return;
        }
        let Some(backend) = self.backend.upgrade() else {
            return;
        };
        attach_replica_autosave(self.manager.clone(), backend, Arc::clone(&self.sync));
    }
}

/// A manager [`SnapshotObserver`] that pushes one tick per snapshot mutation onto
/// the autosave debounce channel — the tokio twin of the linux GTK observer.
struct ReplicaAutosaveObserver {
    tx: tokio::sync::mpsc::UnboundedSender<()>,
}

impl SnapshotObserver for ReplicaAutosaveObserver {
    fn on_changed(&self) {
        // Non-blocking + coalescing-safe: the receiver re-snapshots fresh each tick,
        // so a dropped/duplicate tick is harmless (matches the linux `try_send`).
        let _ = self.tx.send(());
    }
}

/// Attach the debounced replica autosave — the tokio twin of linux's glib
/// `attach_replica_autosave`. A [`ReplicaAutosaveObserver`] feeds every manager
/// mutation onto a channel; a detached task debounces a burst into one upload after
/// [`REPLICA_DEBOUNCE`] of quiescence, snapshots on the task, then seals + uploads
/// off-task.
///
/// **Retirement (no leak).** The task holds only a `Weak<ConversationsManager>`, so
/// it never keeps the session's manager alive: when the owner drops the session (its
/// receive loop exits on liveness → the last strong manager ref drops), the manager
/// drops, the observer's `Sender` drops with it, `rx.recv()` yields `None`, and the
/// task retires — the event-driven twin of linux's `clear_observers` teardown. It
/// holds strong `backend`/`sync` (neither references the manager, so they don't
/// pin it), upgrading the `Weak` only transiently per snapshot.
///
/// **Snapshot consistency.** `snapshot_replica` runs on the tokio task, concurrent
/// with engine mutations on the receive-loop/send tasks — but
/// `MlsEngine::export_provider_storage` clones the whole provider KV under a single
/// `RwLock` read guard, so the snapshot is an atomic point-in-time copy (the same
/// cross-thread pattern the proven linux leg uses: GTK-main snapshot vs tokio-worker
/// mutations).
///
/// **This debounce is the steady-state coalescer, NOT the durability guarantee.**
/// The `ThreadStore` is RAM-only and is *seeded from* the replica at launch, so
/// anything that misses durable storage before a quit is gone — and a sender cannot
/// MLS-decrypt its own application messages, so an own message is user-irrecoverable
/// (the union-merge in `save_history_cas` only heals an empty slice when a later save
/// in the **same session** carries the messages). What closes the quit-inside-the-
/// debounce window is `devices.md` § Durability rules **Rule 3 (durable-before-done)**:
/// `manager::send` (shared Rust, all legs) awaits a `history/<ch>` CAS-save after
/// appending the own message, and `bootstrap_group` persists the still-empty slice
/// before the first send's takeover — both through the `HistoryPersist` seam
/// `restore_and_wire` injects. So a quit at any moment after a send action returns
/// loses nothing, and this debounce only coalesces the remaining
/// (log-reconstructible or retryable) churn.
fn attach_replica_autosave(
    manager: Weak<ConversationsManager>,
    backend: Arc<FaunaMlsBackend>,
    sync: Arc<MlsStateSync>,
) {
    let Some(manager_strong) = manager.upgrade() else {
        return;
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    manager_strong.add_observer(Arc::new(ReplicaAutosaveObserver { tx }));
    drop(manager_strong);

    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // Debounce: coalesce a burst into one upload — reset the quiescence
            // window on each new mutation (the tokio twin of linux's glib
            // generation-counter debounce). A closed channel mid-window (manager
            // dropped) retires the task.
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(REPLICA_DEBOUNCE) => break,
                    ev = rx.recv() => {
                        if ev.is_none() {
                            return;
                        }
                    }
                }
            }
            let Some(manager) = manager.upgrade() else {
                break;
            };
            // Snapshot the provider + every bound channel's history on this task;
            // hand the owned snapshot to an off-task seal + upload so a transient
            // upload never blocks the next debounce.
            let snapshot = orchestration::snapshot_replica(&backend, &manager, &sync);
            drop(manager);
            let sync = Arc::clone(&sync);
            tokio::spawn(async move {
                if let Err(e) = orchestration::save_snapshot(&sync, &snapshot).await {
                    tracing::warn!("mls-sync: replica autosave failed: {e}");
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MlsTransportError, PutOutcome};
    use crate::test_conv::{ConvNest, FakeConvNest};
    use fauna_conversations::backend::ConversationsRpc;
    use fauna_mls::engine::MlsEngine;
    use fauna_protocol::mls_replica::ReplicaBase;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Minimal transport: `gets` served from a queue of scripted outcomes
    /// (`Ok(None)` = an empty replica ⇒ the restore wires an empty plane;
    /// `Err(permanent)` = a nest rejecting the restore ⇒ `Failed` after
    /// one attempt, no retry sleep). Puts always store-nothing successfully.
    struct ScriptedReplica {
        gets: Mutex<Vec<Result<Option<Vec<u8>>, MlsTransportError>>>,
    }

    #[async_trait]
    impl MlsReplicaTransport for ScriptedReplica {
        async fn get(&self, _path: String) -> Result<Option<Vec<u8>>, MlsTransportError> {
            self.gets.lock().unwrap().pop().unwrap_or(Ok(None))
        }
        async fn put(
            &self,
            _path: String,
            _blob: Vec<u8>,
            _base: ReplicaBase,
        ) -> Result<PutOutcome, MlsTransportError> {
            Ok(PutOutcome::Stored)
        }
    }

    /// Records each `run` call — proves the hook seam's call/skip contract.
    struct CountingHook {
        runs: AtomicUsize,
    }

    #[async_trait]
    impl PostRestoreHook for CountingHook {
        async fn run(&self, _backend: Arc<FaunaMlsBackend>) {
            self.runs.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Everything `graph` hands back: the backend, the manager, the RPC seam,
    /// the launcher under test, and the hook that counts its runs.
    type Graph = (
        Arc<FaunaMlsBackend>,
        Arc<ConversationsManager>,
        Arc<dyn ConversationsRpc>,
        Arc<dyn MlsSyncLauncher>,
        Arc<CountingHook>,
    );

    fn graph(transport: Box<dyn MlsReplicaTransport>) -> Graph {
        let engine =
            Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret([7; 32])).unwrap());
        let conv: Arc<dyn ConversationsRpc> = Arc::new(ConvNest(Arc::new(FakeConvNest::default())));
        let manager = ConversationsManager::new();
        let backend = Arc::new(FaunaMlsBackend::new(
            engine.clone(),
            Arc::clone(&conv),
            "alice",
            engine.identity_actor_id(),
        ));
        manager.register_backend(backend.clone());
        let hook = Arc::new(CountingHook {
            runs: AtomicUsize::new(0),
        });
        let launcher = tokio_launcher(
            transport,
            &[7u8; 32],
            Arc::clone(&backend),
            Arc::clone(&manager),
            Some(hook.clone() as Arc<dyn PostRestoreHook>),
            SuccessionReseal::default(),
        )
        .expect("32-byte seed");
        (backend, manager, conv, launcher, hook)
    }

    #[test]
    fn a_short_secret_disables_the_plane() {
        let transport = Box::new(ScriptedReplica {
            gets: Mutex::new(vec![]),
        });
        let engine =
            Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret([7; 32])).unwrap());
        let conv: Arc<dyn ConversationsRpc> = Arc::new(ConvNest(Arc::new(FakeConvNest::default())));
        let manager = ConversationsManager::new();
        let backend = Arc::new(FaunaMlsBackend::new(
            engine.clone(),
            Arc::clone(&conv),
            "alice",
            engine.identity_actor_id(),
        ));
        assert!(
            tokio_launcher(
                transport,
                &[7u8; 16],
                backend,
                manager,
                None,
                SuccessionReseal::default(),
            )
            .is_none()
        );
    }

    /// The hook runs after a WIRED restore (an empty replica wires an empty
    /// plane), with the backend still upgradeable.
    #[tokio::test]
    async fn hook_runs_after_a_wired_restore() {
        let transport = Box::new(ScriptedReplica {
            gets: Mutex::new(vec![]), // every get ⇒ Ok(None): empty replica, Wired(0)
        });
        let (_backend, _manager, _conv, launcher, hook) = graph(transport);
        launcher.launch().await;
        assert_eq!(hook.runs.load(Ordering::SeqCst), 1);
    }

    /// The hook runs on the PERMANENT-failure path too — the single-device
    /// fallback must still discharge what its durable-bytes guards
    /// allow.
    #[tokio::test]
    async fn hook_runs_after_a_permanently_failed_restore() {
        let transport = Box::new(ScriptedReplica {
            gets: Mutex::new(vec![Err(MlsTransportError::rejection(
                "no fauna.mls plane",
            ))]),
        });
        let (_backend, _manager, _conv, launcher, hook) = graph(transport);
        launcher.launch().await;
        assert_eq!(hook.runs.load(Ordering::SeqCst), 1);
    }

    /// Building the plane declares the restore, so a key-package mint refuses
    /// from session build on (`devices.md` Rule 3 — the pre-seam window); the
    /// launch lifts it either way it ends: wired (the persist seam is injected)
    /// or permanently failed (single-device, no swap is coming).
    #[tokio::test]
    async fn the_plane_holds_key_package_mints_until_the_launch_restore_ends() {
        let wired = Box::new(ScriptedReplica {
            gets: Mutex::new(vec![]),
        });
        let (backend, _manager, _conv, launcher, _hook) = graph(wired);
        assert!(
            backend.replica_restore_pending(),
            "a mint before the launch restore must be refused"
        );
        launcher.launch().await;
        assert!(
            !backend.replica_restore_pending(),
            "wired ⇒ the seam gates the mint"
        );

        let failed = Box::new(ScriptedReplica {
            gets: Mutex::new(vec![Err(MlsTransportError::rejection(
                "no fauna.mls plane",
            ))]),
        });
        let (backend, _manager, _conv, launcher, _hook) = graph(failed);
        launcher.launch().await;
        assert!(
            !backend.replica_restore_pending(),
            "a permanent failure falls back to single-device and the direct publish"
        );
    }

    /// A session torn down before launch (both Weak handles dead) skips the
    /// hook entirely — nothing to resume against.
    #[tokio::test]
    async fn hook_is_skipped_when_the_session_dropped() {
        let transport = Box::new(ScriptedReplica {
            gets: Mutex::new(vec![]),
        });
        let (backend, manager, _conv, launcher, hook) = graph(transport);
        drop(backend);
        drop(manager);
        launcher.launch().await;
        assert_eq!(hook.runs.load(Ordering::SeqCst), 0);
    }
}
