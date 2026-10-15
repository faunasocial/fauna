//! The ceremony device's record of the departed fleet
//! (`succession-aftermath.md` § Re-key scope → *Predecessor devices are
//! departed devices*): between a landed succession and its account switch,
//! the device that ran the ceremony records the predecessor's verified fleet
//! view — members and removed ids alike — on the predecessor's registry row
//! ([`AccountRegistry::record_departed_devices`]), for the successor's
//! runtime to attest.
//!
//! [`record_departed_fleet`] is the one call every ceremony host makes, the
//! signed-in surfaces and the locked launch surface alike. It reads the view
//! off the live account runtime when that runtime serves the predecessor
//! ([`AccountStoreHandle::fleet_device_ids`]), and otherwise off the
//! predecessor's closed store on disk ([`read_local_fleet_device_ids`]) — the
//! locked surface has no runtime up, but this device's merged replica of the
//! predecessor's account still rests under the store root.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::root::StoreRoot;
use fauna_account_store::types::StateEntry;
use fauna_client_accounts::AccountRegistry;
use fauna_core::crypto::ReplicaSealKey;
use fauna_core::generation::FleetView;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_protocol::merge_policy::KIND_DEVICE_SET;

use crate::account_driver::AccountStoreHandle;
use crate::deployment_seed_recovery::replica_seal_key;
use crate::fleet_removal::fleet_view_device_ids;
use crate::generation_store::row_ref;

/// Record the predecessor's departed fleet on its registry row — the
/// ceremony device's one call between a landed succession and its account
/// switch. `runtime` is the host's live account runtime, if any; it is read
/// only when it serves `predecessor`. Otherwise the predecessor's store under
/// `root` is read at rest, opened with the replica seal key its seed derives.
///
/// Never fatal: the succession has landed, and a miss costs only the
/// attestation (the residual the goal section states) — so it logs and
/// returns.
pub async fn record_departed_fleet(
    runtime: Option<&AccountStoreHandle>,
    registry: &AccountRegistry,
    root: Result<StoreRoot>,
    predecessor: &ActorKeypair,
) {
    let ids = read_departed_fleet(runtime, root, predecessor).await;
    record_departed_ids(registry, predecessor.actor_id(), ids);
}

/// The read half of [`record_departed_fleet`]: the predecessor's verified
/// fleet ids, off the live runtime when it serves `predecessor`, else off its
/// store at rest. Split out so a host whose registry writes must run inside a
/// synchronous critical section (web's cross-tab Web Lock) can do this async
/// read outside it and the write ([`record_departed_ids`]) inside.
///
/// `root` is the store root the at-rest read opens, or why it could not be
/// resolved (`StoreRoot::platform`'s no-home refusal): only the at-rest read
/// needs it, so a live runtime still answers, and the refusal is otherwise the
/// read's error, logged by the write half.
pub async fn read_departed_fleet(
    runtime: Option<&AccountStoreHandle>,
    root: Result<StoreRoot>,
    predecessor: &ActorKeypair,
) -> Result<Vec<[u8; 32]>> {
    let actor = predecessor.actor_id();
    match runtime {
        Some(handle) => match handle.fleet_device_ids(actor).await {
            Ok(ids) => Ok(ids),
            // A runtime serving another account, or one already gone: the
            // predecessor's store is still on disk.
            Err(e) => {
                tracing::debug!(
                    "succession: the live runtime gave no fleet ({e:#}); reading the store"
                );
                read_at_rest(&root?, predecessor).await
            }
        },
        None => read_at_rest(&root?, predecessor).await,
    }
}

/// The write half of [`record_departed_fleet`]: record `ids` (or log why they
/// could not be read) on `actor`'s registry row. Synchronous and never fatal.
pub fn record_departed_ids(registry: &AccountRegistry, actor: ActorId, ids: Result<Vec<[u8; 32]>>) {
    let recorded = ids.and_then(|ids| {
        registry
            .record_departed_devices(&actor.to_hex(), &ids)
            .map(|()| ids.len())
            .map_err(anyhow::Error::from)
    });
    match recorded {
        Ok(n) => tracing::info!("succession: recorded {n} predecessor device(s) to attest"),
        Err(e) => tracing::warn!(
            "succession: the predecessor's fleet was not recorded — its devices' inherited \
             rows will not be attested: {e:#}"
        ),
    }
}

async fn read_at_rest(root: &StoreRoot, predecessor: &ActorKeypair) -> Result<Vec<[u8; 32]>> {
    read_local_fleet_device_ids(
        root,
        predecessor.actor_id(),
        Some(replica_seal_key(predecessor.secret_bytes())),
    )
    .await
}

/// Every fleet id `actor`'s verified fleet view names, read from this
/// device's closed replica of `actor`'s account under `root` with no runtime
/// — the same answer [`crate::fleet_removal::fleet_device_ids`] gives a
/// runtime over that store (the view verifies against `actor` alone). Empty
/// when no store exists there, which it never creates. Safe beside a process
/// hosting the account's engine, as the deployment-seed local read is
/// ([`crate::deployment_seed_recovery::read_local_deployment_seeds`]): the
/// native store is SQLite in WAL, web's an IndexedDB database any connection
/// may read.
///
/// `seal` opens the sealed rows of a native store; `None` reads a store whose
/// rows rest opened and fails on a sealed one. Web's replica rests opened and
/// ignores it.
///
/// # Errors
///
/// A malformed actor id, or a store that fails to open or read.
pub async fn read_local_fleet_device_ids(
    root: &StoreRoot,
    actor: ActorId,
    seal: Option<ReplicaSealKey>,
) -> Result<Vec<[u8; 32]>> {
    let actor_hex = actor.to_hex();
    // Native: SQLite answers synchronously under its async signature and its
    // connection is not `Sync`, so the read runs to completion in a sync
    // helper and this future holds no connection across an await.
    #[cfg(not(target_arch = "wasm32"))]
    let rows = read_device_set_native(root, &actor_hex, seal)?;
    #[cfg(target_arch = "wasm32")]
    let rows = {
        let _ = seal;
        let name = root.store_name(&actor_hex)?;
        let backend = fauna_account_store::indexeddb::IndexedDbBackend::open_existing(&name)
            .await
            .with_context(|| format!("open the account store {name:?}"))?;
        match backend {
            Some(backend) => Some(backend.state_entries_of_kind(KIND_DEVICE_SET).await?),
            None => None,
        }
    };
    let Some(rows) = rows else {
        return Ok(Vec::new());
    };
    let view = FleetView::build(&actor, rows.iter().filter(|e| !e.tombstone).map(row_ref));
    Ok(fleet_view_device_ids(&view))
}

/// [`read_local_fleet_device_ids`]' native read: open, read, drop — all
/// synchronous. `None` when no store exists.
#[cfg(not(target_arch = "wasm32"))]
fn read_device_set_native(
    root: &StoreRoot,
    actor_hex: &str,
    seal: Option<ReplicaSealKey>,
) -> Result<Option<Vec<StateEntry>>> {
    use std::task::{Context as TaskContext, Poll, Waker};
    let dir = root.store_dir(actor_hex)?;
    let Some(backend) = fauna_account_store::sqlite::SqliteBackend::open_existing_keyed(&dir, seal)
        .with_context(|| format!("open the account store at {}", dir.display()))?
    else {
        return Ok(None);
    };
    let read = std::pin::pin!(backend.state_entries_of_kind(KIND_DEVICE_SET));
    match read.poll(&mut TaskContext::from_waker(Waker::noop())) {
        Poll::Ready(rows) => rows.map(Some),
        Poll::Pending => anyhow::bail!("the account store read did not complete synchronously"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_fixture_test_support::{THEM, US, device_id_of, enrollment_row, root};
    use fauna_account_store::sqlite::{ACCOUNT_STORE_DB_FILENAME, ReplicaPosture, SqliteBackend};
    use fauna_account_store::store::AccountStore;
    use fauna_account_store::types::WriterId;
    use fauna_client_accounts::InMemorySecretStore;
    use std::sync::Arc;

    /// The predecessor's replica as a device leaves it at rest: sealed under
    /// the seed's replica key, enrolling US and THEM, then closed.
    async fn closed_predecessor_store(store_root: &StoreRoot) {
        let pred = root();
        let dir = store_root.store_dir(&pred.actor_id_hex()).unwrap();
        let backend = SqliteBackend::open_keyed(
            &dir,
            replica_seal_key(pred.secret_bytes()),
            ReplicaPosture::Sealed,
        )
        .unwrap();
        let store = AccountStore::open(backend, &pred.actor_id_hex(), WriterId([0x11; 32]))
            .await
            .unwrap();
        for seed in [US, THEM] {
            store.put_state(enrollment_row(seed)).await.unwrap();
        }
    }

    fn registry_holding(pred: &ActorKeypair) -> AccountRegistry {
        let registry = AccountRegistry::new(Arc::new(InMemorySecretStore::new()));
        registry
            .add_account(&hex::encode(pred.secret_bytes()), None, None)
            .unwrap();
        registry
    }

    fn departed(registry: &AccountRegistry, pred: &ActorKeypair) -> Vec<String> {
        let mut got = registry
            .list()
            .into_iter()
            .find(|e| e.actor_id == pred.actor_id_hex())
            .expect("the predecessor's row")
            .departed_devices;
        got.sort();
        got
    }

    /// **The locked shape** (`succession-aftermath.md` § Re-key scope →
    /// *Predecessor devices are departed devices*): a ceremony with no
    /// account runtime records the predecessor's fleet from its closed store
    /// on disk.
    #[tokio::test]
    async fn a_ceremony_with_no_runtime_records_the_predecessors_fleet_from_its_store_at_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let store_root = StoreRoot::at(tmp.path());
        closed_predecessor_store(&store_root).await;
        let pred = root();
        let registry = registry_holding(&pred);

        record_departed_fleet(None, &registry, Ok(store_root.clone()), &pred).await;

        let mut want = vec![
            fauna_core::hex32::encode(&device_id_of(US)),
            fauna_core::hex32::encode(&device_id_of(THEM)),
        ];
        want.sort();
        assert_eq!(departed(&registry, &pred), want);
    }

    /// No store at rest: nothing read, nothing created, nothing recorded.
    #[tokio::test]
    async fn no_store_at_rest_records_nothing_and_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store_root = StoreRoot::at(tmp.path());
        let pred = root();
        let registry = registry_holding(&pred);

        record_departed_fleet(None, &registry, Ok(store_root.clone()), &pred).await;

        assert!(departed(&registry, &pred).is_empty());
        assert!(
            !store_root
                .store_dir(&pred.actor_id_hex())
                .unwrap()
                .join(ACCOUNT_STORE_DB_FILENAME)
                .exists()
        );
    }
}
