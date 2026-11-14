//! The **pre-succession flush** — the live replica autosave of an identity,
//! reachable by actor id so the succession ceremony can land everything this
//! device has folded BEFORE the submit retires the identity's bearer
//! (`docs/goal/behavior/succession-aftermath.md` § Re-key scope → *The
//! predecessor's replica is flushed before the submit*).
//!
//! # Why the debounce is not enough at a succession
//!
//! The autosave ([`crate::launcher`]) seals a burst only after
//! `REPLICA_DEBOUNCE` of quiescence, and its doc calls what it carries
//! "log-reconstructible or retryable churn": a peer's message folded into the
//! thread store re-decrypts from the nest log on the next launch, so missing
//! one tick costs nothing. A succession breaks both halves of that. The
//! submit revokes this identity's bearer, so the pending tick's upload fails
//! (`fauna.auth.superseded`) and is never retried under that identity; and
//! the successor cannot re-walk what it lost — it opens no community-room
//! generation minted before the ceremony (the predecessor's reception keys
//! are not re-sealed), so what it shows of a room's past is exactly the
//! `history/<ch>` slice the predecessor's device landed. A message read
//! within the debounce window of the ceremony was therefore lost to the
//! successor for good — measured on tui on 2026-11-12: the autosave fired
//! 55 ms after the submit and failed.
//!
//! # Why a registry keyed by actor
//!
//! Every app's ceremony submits through one shared function
//! (`fauna_client_recovery::succeed_with_held_kit`, under the native
//! `ceremony::succeed_stolen_identity` and web's `succeed_with_held_kit_web`),
//! which holds the identity but none of the session's conversations handles,
//! and whose callers differ in what they could pass. The autosave
//! already knows the identity it serves, so it registers here when it
//! attaches; the composition flushes by actor and needs no new parameter on
//! any app's path. A later registration for the same actor replaces the
//! earlier one (the session that attached last is the live one), and an entry
//! whose session is gone or whose identity has changed answers
//! [`LeaveFlush::NoLivePlane`].
//!
//! # Every leg registers, whatever drives its autosave
//!
//! The entry holds the plane itself — the manager, the backend and the
//! `MlsStateSync` — never the leg's debounce, so a leg whose autosave is its
//! own registers the same entry with [`register_live_plane`]: linux's glib
//! debounce (`conv_backend.rs`) and web's `saveMlsState` chokepoint (registered
//! when `restoreMlsState` wires the plane). The flush runs the shared
//! [`orchestration::snapshot_replica`] + [`orchestration::save_snapshot`] the
//! debounces run, on whatever thread awaits it: the snapshot reads the
//! manager's and the engine's own locks, exactly as the tokio autosave does
//! off any worker thread. Liveness is the manager's identity epoch
//! (`ConversationsManager::identity_epoch`, advanced by every identity change
//! on every target), read at registration and compared at the flush.
//!
//! # Not the drafts rails
//!
//! The drafts rails debounce the same 1.5 s, but nothing changes a draft on
//! this device except the user's own keystrokes in a composer, and the
//! ceremony's submit follows the user leaving that composer, entering the kit
//! phrase in Settings or on the locked surface, confirming, and the ceremony's
//! own anonymous connect — far past one debounce. A draft another device
//! changed was landed by that device. So the rails owe no pre-submit flush;
//! the sign-out door's `save_on_leave` that fails `fauna.auth.superseded`
//! after the submit has nothing pending to lose
//! (`succession-aftermath.md` § Re-key scope → *The predecessor's replica is
//! flushed before the submit*).

use std::collections::HashMap;
use std::sync::{Arc, Weak};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{LazyLock, Mutex};

use fauna_conversations::ConversationsManager;
use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_core::identity::ActorId;

use crate::orchestration;
use crate::sync::MlsStateSync;

/// What [`flush_before_retire`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaveFlush {
    /// No live, wired replica plane serves this identity on this process (a
    /// locked app, a session whose restore failed, an identity already
    /// switched away from). Nothing was owed here.
    NoLivePlane,
    /// The snapshot was sealed and every changed path landed. `provider_written`
    /// is whether the `provider` blob was a fresh write (false: it was
    /// already current).
    Flushed { provider_written: bool },
    /// A save failed. The ceremony goes on regardless (losing the account to
    /// a thief outranks this) — the reason is logged.
    Failed(String),
}

struct LiveFlusher {
    manager: Weak<ConversationsManager>,
    backend: Weak<FaunaMlsBackend>,
    sync: Arc<MlsStateSync>,
    /// The manager's identity epoch at registration; any other reading at the
    /// flush means the identity this entry served is gone.
    epoch: u64,
}

type Registry = HashMap<ActorId, Arc<LiveFlusher>>;

/// The registry, process-wide on native: the ceremony may await the flush on
/// any tokio worker, never the one that registered.
#[cfg(not(target_arch = "wasm32"))]
static LIVE: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// On wasm the page is one thread, and the manager is not `Sync` there, so the
/// registry is that thread's.
#[cfg(target_arch = "wasm32")]
thread_local! {
    static LIVE: std::cell::RefCell<Registry> = std::cell::RefCell::new(HashMap::new());
}

/// Run `f` over the registry; never held across an await.
fn with_live<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    #[cfg(not(target_arch = "wasm32"))]
    {
        f(&mut LIVE.lock().unwrap_or_else(|e| e.into_inner()))
    }
    #[cfg(target_arch = "wasm32")]
    {
        LIVE.with(|live| f(&mut live.borrow_mut()))
    }
}

/// Register the live replica plane of `actor` — called by every leg once the
/// plane is wired (the launch gate is lifted, so a save here is a real save):
/// the shared tokio autosave when it attaches, linux's when it attaches its
/// glib debounce, web's when `restoreMlsState` wires. The entry lives as long
/// as the handles and the identity it was registered under.
pub fn register_live_plane(
    actor: ActorId,
    manager: &Arc<ConversationsManager>,
    backend: &Arc<FaunaMlsBackend>,
    sync: Arc<MlsStateSync>,
) {
    let entry = Arc::new(LiveFlusher {
        manager: Arc::downgrade(manager),
        backend: Arc::downgrade(backend),
        sync,
        epoch: manager.identity_epoch(),
    });
    with_live(|live| live.insert(actor, entry));
}

/// Seal and land `actor`'s live replica NOW, awaiting the upload — the
/// durable-before-retire step the succession ceremony runs before its submit.
pub async fn flush_before_retire(actor: &ActorId) -> LeaveFlush {
    let entry = with_live(|live| live.get(actor).cloned());
    let Some(entry) = entry else {
        return LeaveFlush::NoLivePlane;
    };
    let (Some(manager), Some(backend)) = (entry.manager.upgrade(), entry.backend.upgrade()) else {
        forget(actor, &entry);
        return LeaveFlush::NoLivePlane;
    };
    if manager.identity_epoch() != entry.epoch {
        forget(actor, &entry);
        return LeaveFlush::NoLivePlane;
    }
    let snapshot = orchestration::snapshot_replica(&backend, &manager, &entry.sync);
    drop(manager);
    drop(backend);
    match orchestration::save_snapshot(&entry.sync, &snapshot).await {
        Ok(provider_written) => {
            tracing::info!(
                channels = snapshot.slices.len(),
                provider_written,
                "mls-sync: the replica was flushed before the succession submit"
            );
            LeaveFlush::Flushed { provider_written }
        }
        Err(e) => {
            tracing::warn!("mls-sync: the pre-succession replica flush failed: {e}");
            LeaveFlush::Failed(e.to_string())
        }
    }
}

/// Drop `actor`'s entry if it is still `entry` (a newer registration stays).
fn forget(actor: &ActorId, entry: &Arc<LiveFlusher>) {
    with_live(|live| {
        if live.get(actor).is_some_and(|e| Arc::ptr_eq(e, entry)) {
            live.remove(actor);
        }
    });
}
