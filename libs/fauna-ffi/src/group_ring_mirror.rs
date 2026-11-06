//! The app's side of the **group-ring mirror** (`group-set.md` § Build
//! design — the group set, decision 12; the file and its reader:
//! [`fauna_sync_engine::group_ring_mirror`]).
//!
//! The shell's one duty is to **seat the hosts' state dir** for the account
//! ([`install_group_ring_mirror`]) — the same directory it builds every
//! capability host of the account with (android's `syncStateDir()`, apple's
//! container-domain root). Everything else is the runtime's: once a store is
//! mounted, [`run`] keeps the mirror current from the store itself — a pass at
//! the mount, a pass after every store change and a pass when the seat lands
//! — so no app call at an assembly or a walk can be forgotten. A process with
//! no seat (a desktop app, whose hosts mount the store) writes nothing.

use std::path::PathBuf;

use fauna_core::crypto::GroupRingMirrorKey;
use fauna_core::identity::ActorId;
use fauna_sync_engine::account_runtime::AccountStoreHandle;
use fauna_sync_engine::group_ring_mirror::{GroupRingMirror, held_rings, write_if_changed};

use crate::FfiError;
use crate::crypto::bytes32;

/// The seated state dir per account, and the wake every seat sends the
/// running task.
struct Seats {
    dirs: std::collections::HashMap<[u8; 32], PathBuf>,
    wake: tokio::sync::watch::Sender<u64>,
}

static SEATS: std::sync::LazyLock<std::sync::Mutex<Seats>> = std::sync::LazyLock::new(|| {
    std::sync::Mutex::new(Seats {
        dirs: std::collections::HashMap::new(),
        wake: tokio::sync::watch::Sender::new(0),
    })
});

/// Name the sync state dir this process's capability hosts of `actor_id`
/// are built with (`state_dir` on `FfiFileProviderHost::app_dead` /
/// `app_dead_owned_tree`: `<base>/<actor-id-hex>/` of the host's domain). The
/// runtime mirrors the account's held group rings into it from then on
/// (`group-set.md` § decision 12); seating again replaces the dir and triggers
/// a pass. Idempotent; a seat before the store is mounted is honoured at the
/// mount.
#[uniffi::export]
pub fn install_group_ring_mirror(actor_id: Vec<u8>, state_dir: String) -> Result<(), FfiError> {
    let actor_id = bytes32(&actor_id, "actor_id")?;
    let mut seats = SEATS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    seats.dirs.insert(actor_id, PathBuf::from(state_dir));
    seats.wake.send_modify(|n| *n += 1);
    Ok(())
}

/// The seated dir of `actor_id`, if any.
fn seated(actor_id: &[u8; 32]) -> Option<PathBuf> {
    SEATS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .dirs
        .get(actor_id)
        .cloned()
}

/// Keep `actor`'s mirror current until its runtime is gone: a pass now, a
/// pass after every store change (coalesced behind a short settle), a pass
/// whenever a seat lands. A pass with no seat writes nothing; a pass that
/// fails logs and leaves the last mirror standing (a read that fails is
/// never written as empty).
pub(crate) async fn run(store: AccountStoreHandle, actor: ActorId, key: GroupRingMirrorKey) {
    let mut watch =
        fauna_client_account_runtime::store_change::StoreChangeWatch::new(store.clone()).await;
    let mut seat_moved = SEATS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .wake
        .subscribe();
    let actor_hex = actor.to_hex();
    let mut last: Option<[u8; 32]> = None;
    let mut last_dir: Option<PathBuf> = None;
    loop {
        if let Some(dir) = seated(&actor.0) {
            // A new seat is a new file: forget the digest so the first
            // pass there writes.
            if last_dir.as_ref() != Some(&dir) {
                last = None;
                last_dir = Some(dir.clone());
            }
            match held_rings(&store, &actor).await {
                Ok(rings) => {
                    let mirror = GroupRingMirror::in_state_dir(&dir);
                    match write_if_changed(&mirror, &key, &actor_hex, &rings, &mut last) {
                        Ok(true) => tracing::info!(
                            scopes = rings.len(),
                            path = %mirror.path().display(),
                            "group-ring mirror written"
                        ),
                        Ok(false) => {}
                        Err(e) => tracing::warn!("group-ring mirror not written: {e:#}"),
                    }
                }
                Err(e) => {
                    tracing::warn!("group-ring mirror pass skipped — the rings did not read: {e:#}")
                }
            }
        }
        tokio::select! {
            changed = watch.changed() => {
                if !changed {
                    break;
                }
                // Coalesce a burst of writes (a walk landing a page of rows)
                // into one pass.
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            moved = seat_moved.changed() => {
                if moved.is_err() {
                    break;
                }
            }
        }
    }
}

/// Spawn [`run`] for the store just mounted — the store-installed edge of
/// the FFI account runtime. The `BackupKey`-derived mirror key is the one
/// the hosts derive from the key they are built with.
pub(crate) fn spawn(store: AccountStoreHandle, actor: ActorId, key: GroupRingMirrorKey) {
    tokio::spawn(run(store, actor, key));
}

/// A test seam: the seated dir, if any.
#[cfg(test)]
pub(crate) fn seated_for_test(actor_id: &[u8; 32]) -> Option<PathBuf> {
    seated(actor_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seat lands per account, replaces an earlier one, and wakes a waiter.
    #[tokio::test]
    async fn a_seat_is_per_account_and_wakes_the_task() {
        let actor = [0xA7; 32];
        let mut rx = SEATS.lock().unwrap().wake.subscribe();
        let before = *rx.borrow_and_update();
        install_group_ring_mirror(actor.to_vec(), "/tmp/one".into()).unwrap();
        assert_eq!(seated_for_test(&actor), Some(PathBuf::from("/tmp/one")));
        assert!(seated_for_test(&[0xA8; 32]).is_none());
        install_group_ring_mirror(actor.to_vec(), "/tmp/two".into()).unwrap();
        assert_eq!(seated_for_test(&actor), Some(PathBuf::from("/tmp/two")));
        rx.changed().await.unwrap();
        assert!(*rx.borrow() > before);
        assert!(install_group_ring_mirror(vec![1, 2, 3], "/tmp".into()).is_err());
    }
}
