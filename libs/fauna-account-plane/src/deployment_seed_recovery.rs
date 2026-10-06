//! The deployment-seed custody's **pre-login readers** — the local read, the
//! cold read and the resolver that joins them (`nest/box-recovery.md` § The
//! plane-era recovery floor, *(b) The reads*, which owns why there are three
//! sources and why none of them may wait for an account runtime).
//!
//! Every recovery read runs where no account runtime exists: the onboarding
//! wizard's box list and self-hosted command, the re-provision drive, the
//! launch-retry entry. The signed-in handle read
//! (`AccountStoreHandle::deployment_seeds`) is the fourth reader and lives with
//! the handle; the fold every reader answers with is
//! [`crate::deployment_seed_rows`]'s.
//!
//! - [`read_local_deployment_seeds`] — this device's own account store, no
//!   runtime and no nest. Rows rest opened in the local store
//!   (`account-replica-posture.md` § Local at-rest posture), so it needs the
//!   actor and the store root only. It answers empty for a store that does not
//!   exist and never creates one.
//! - [`cold_read_deployment_seeds`] — a reachable nest and the identity seed,
//!   for a device holding no store for the account. It **writes nothing**: no
//!   enrollment, no device-set row, no escrow target, no mint, no put, no walk
//!   mark — a read that joined the fleet would leave a phantom device behind
//!   every abandoned recovery.
//! - [`resolve_deployment_seeds`] — the map a pre-login surface reads: the
//!   local read joined with the cold read through the kind's own lattice
//!   ([`DeploymentSeedEntry::merge_seed_map`]). Never either-or on whether a
//!   nest URL is stored: in the case recovery exists for, a surviving device's
//!   stored nest is the dead box.

use std::future::Future;

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_core::crypto::BackupKey;
use fauna_core::data::DeploymentSeedEntry;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_DEPLOYMENT_SEEDS;
use zeroize::Zeroizing;

use crate::cold_replica::{ColdFleetReplica, ColdKeySource};
use crate::deployment_seed_rows::{deployment_seeds_of, read_deployment_seeds};

/// The store location [`read_local_deployment_seeds`] and
/// [`resolve_deployment_seeds`] take — re-exported so a pre-login caller needs
/// no second dependency to name it.
pub use fauna_account_store::root::StoreRoot;

// ── The local read ───────────────────────────────────────────────────────────

/// The custody map folded from this device's own account store for
/// `actor_id_hex` under `root` — empty when no store exists there, which it
/// never creates. Safe beside a process that hosts the account's engine: the
/// native store is SQLite in WAL (a reader beside the one writer), web's is an
/// IndexedDB database any connection may read.
///
/// # Errors
///
/// A malformed actor id, a store that fails to open (a newer-breaking format
/// among them), or a stored row that does not decode strictly
/// ([`deployment_seeds_of`] — never a silently skipped box).
pub async fn read_local_deployment_seeds(
    root: &StoreRoot,
    actor_id_hex: &str,
) -> Result<Vec<DeploymentSeedEntry>> {
    // Native: the SQLite read is synchronous under its async signature, and the
    // connection is not `Sync`, so it runs to completion in a sync helper and
    // this future holds no connection across an await — the resolver's future
    // stays `Send` for a native caller's `Send` seam (the re-provision reader).
    #[cfg(not(target_arch = "wasm32"))]
    return read_local_deployment_seeds_native(root, actor_id_hex);
    #[cfg(target_arch = "wasm32")]
    {
        let name = root.store_name(actor_id_hex)?;
        let backend = fauna_account_store::indexeddb::IndexedDbBackend::open_existing(&name)
            .await
            .with_context(|| format!("open the account store {name:?}"))?;
        let Some(backend) = backend else {
            return Ok(Vec::new());
        };
        deployment_seeds_of(&backend.state_entries_of_kind(KIND_DEPLOYMENT_SEEDS).await?)
    }
}

/// [`read_local_deployment_seeds`]' native body: open, read, drop — all
/// synchronous. The backend's read is `async` by signature only (SQLite answers
/// on the calling thread), so one poll completes it; a backend that ever
/// suspended would answer an error here rather than block.
#[cfg(not(target_arch = "wasm32"))]
fn read_local_deployment_seeds_native(
    root: &StoreRoot,
    actor_id_hex: &str,
) -> Result<Vec<DeploymentSeedEntry>> {
    use std::task::{Context as TaskContext, Poll, Waker};
    let dir = root.store_dir(actor_id_hex)?;
    let Some(backend) = fauna_account_store::sqlite::SqliteBackend::open_existing(&dir)
        .with_context(|| format!("open the account store at {}", dir.display()))?
    else {
        return Ok(Vec::new());
    };
    let read = std::pin::pin!(backend.state_entries_of_kind(KIND_DEPLOYMENT_SEEDS));
    match read.poll(&mut TaskContext::from_waker(Waker::noop())) {
        Poll::Ready(rows) => deployment_seeds_of(&rows?),
        Poll::Pending => anyhow::bail!("the account store read did not complete synchronously"),
    }
}

// ── The cold read ────────────────────────────────────────────────────────────

/// The custody map as the nest behind `rpc` holds it for the account whose
/// identity seed is `identity_seed` — read into a throwaway in-memory replica
/// and dropped with it. `rpc` must be authenticated as the owner: the
/// account's own scope and escrow wraps are what the nest serves it.
///
/// The replica is [`ColdFleetReplica`] on its seed arm ([`ColdKeySource::Seed`]):
///
/// 1. a **pull-only** fleet plane over an in-memory store under an ephemeral
///    writer key that is never published — pull-only sends no walker mark, so
///    the nest records nothing for the walk, and the plane has no publish path
///    at all;
/// 2. `walk` — every live fleet row, the generation-sealed ones unopened;
/// 3. **one unfiltered `fauna.generation.escrow.get`**, and every returned wrap
///    opened with the seed-derived escrow secret against the key commitment
///    of the walked `Minted` row it names (`EscrowOpener`, the check the
///    pump's recovery pass shares), the keys held in memory;
/// 4. `reconcile` — the rows step 2 could not open, re-presented now that
///    their generations key;
/// 5. the fold.
///
/// **No escrow receipt is consulted and no holder is trusted** (`nest/
/// box-recovery.md` § The plane-era recovery floor, *(b)*; `account-data-
/// taxonomy.md` § The generation machinery → *Escrow recovery*): a receipt
/// bounds what a recurring pass may ask for, and this is one reader asking
/// once — so the read works at a rebuilt box, whose receipts describe wraps it
/// no longer holds, and at a rotated one, whose receipts name a holder this
/// device does not trust. What a wrap is trusted for is its commitment check.
///
/// Reading without the fleet's admissibility checks is safe for this kind:
/// a row names its box and carries a seed that derives to that box's id (the
/// strict decode), so nobody lacking a box's seed can forge its row. The full
/// account runtime is the wrong tool — it enrolls the device and publishes an
/// escrow target inside its readiness barrier. The plane's trust is empty
/// (no `prior`, no holder): it gates only sealing, and this read seals
/// nothing.
///
/// # Errors
///
/// Any walk, escrow-door or reconcile failure. The caller's resolver keeps the
/// local read when this fails.
pub async fn cold_read_deployment_seeds<R: RpcRequester>(
    rpc: &R,
    identity_seed: &[u8; 32],
) -> Result<Vec<DeploymentSeedEntry>> {
    let actor = ActorKeypair::from_secret(*identity_seed).actor_id();
    let replica = ColdFleetReplica::open(
        rpc,
        actor,
        &BackupKey::derive(identity_seed),
        ColdKeySource::Seed(Zeroizing::new(*identity_seed)),
    )
    .await
    .context("cold read")?;
    replica.walk().await.context("cold read")?;
    read_deployment_seeds(replica.store()).await
}

// ── The resolver ─────────────────────────────────────────────────────────────

/// What [`resolve_deployment_seeds`] answers: the joined map, and each source
/// that failed. A failure of either source leaves the other's answer in
/// `seeds`; the failures are kept so a caller can tell "no box custodied"
/// from "the nest could not be asked".
#[derive(Debug, Default)]
pub struct ResolvedDeploymentSeeds {
    /// The local read joined with the cold read, in `nest_actor_id` order.
    pub seeds: Vec<DeploymentSeedEntry>,
    /// Why the local read failed, when it did.
    pub local_failure: Option<anyhow::Error>,
    /// Whether a cold read was asked at all.
    pub cold_asked: bool,
    /// Why the cold read failed, when one was asked and it did.
    pub cold_failure: Option<anyhow::Error>,
}

impl ResolvedDeploymentSeeds {
    /// The map, or an error when **every** source asked failed — one that
    /// answered, even empty, is an answer.
    ///
    /// # Errors
    ///
    /// Both failures, when both sources were asked and neither answered; the
    /// local failure when no cold read was asked.
    pub fn into_result(self) -> Result<Vec<DeploymentSeedEntry>> {
        match (self.local_failure, self.cold_failure) {
            (Some(local), Some(cold)) => Err(anyhow::anyhow!(
                "no custody source answered — local read: {local:#}; cold read: {cold:#}"
            )),
            (Some(local), None) if !self.cold_asked => Err(local),
            _ => Ok(self.seeds),
        }
    }
}

/// The pre-login custody map: the local read always, joined with `cold` when
/// one is given (the caller composes it — connect, authenticate, then
/// [`cold_read_deployment_seeds`] — because the transport is per target).
/// A caller with a nest URL gives the cold read whether or not the local read
/// has anything: a surviving device's stored nest may be the dead box, and a
/// fresh device has no local store at all.
pub async fn resolve_deployment_seeds<F>(
    root: &StoreRoot,
    actor_id_hex: &str,
    cold: Option<F>,
) -> ResolvedDeploymentSeeds
where
    F: Future<Output = Result<Vec<DeploymentSeedEntry>>>,
{
    let local = read_local_deployment_seeds(root, actor_id_hex).await;
    let cold = match cold {
        Some(read) => Some(read.await),
        None => None,
    };
    join_deployment_seed_sources(local, cold)
}

/// [`resolve_deployment_seeds`] composed for a caller holding (or not) an
/// owner-authenticated connection: the local read always, and the cold read
/// over `cold` when the caller reached a nest. The one composition every host's
/// pre-login getter calls — the box list, the self-hosted command, the
/// re-provision reader and the launch-retry entry — so none of them decides
/// either-or on its own.
pub async fn resolve_deployment_seeds_over<R: RpcRequester>(
    root: &StoreRoot,
    identity_seed: &[u8; 32],
    cold: Option<&R>,
) -> ResolvedDeploymentSeeds {
    let actor_hex = ActorKeypair::from_secret(*identity_seed).actor_id_hex();
    resolve_deployment_seeds(
        root,
        &actor_hex,
        cold.map(|rpc| cold_read_deployment_seeds(rpc, identity_seed)),
    )
    .await
}

/// [`resolve_deployment_seeds`]'s join, over answers already in hand: each
/// answer that arrived is joined through the kind's lattice, each failure is
/// kept beside it.
pub fn join_deployment_seed_sources(
    local: Result<Vec<DeploymentSeedEntry>>,
    cold: Option<Result<Vec<DeploymentSeedEntry>>>,
) -> ResolvedDeploymentSeeds {
    let mut resolved = ResolvedDeploymentSeeds {
        cold_asked: cold.is_some(),
        ..Default::default()
    };
    match local {
        Ok(seeds) => resolved.seeds = seeds,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "deployment seeds: the local read failed");
            resolved.local_failure = Some(e);
        }
    }
    match cold {
        None => {}
        Some(Ok(seeds)) => {
            resolved.seeds = DeploymentSeedEntry::merge_seed_map(&resolved.seeds, &seeds);
        }
        Some(Err(e)) => {
            tracing::warn!(error = %format!("{e:#}"), "deployment seeds: the cold read failed");
            resolved.cold_failure = Some(e);
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_state_plane::AccountStatePlane;
    use crate::deployment_seed_rows::merge_deployment_seeds;
    use crate::generation_fixture_test_support::{
        Bundle, ESCROW_SEED, NoNest, ROOT_SEED, US, device_key, enrollment_row, machinery_row,
        member_of, root, target_key,
    };
    use crate::generation_tip::GenerationTrust;
    use ed25519_dalek::SigningKey;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::store::AccountStore;
    use fauna_account_store::types::WriterId;
    use fauna_core::crypto::AccountStateKeySchedule;
    use fauna_core::generation::{
        EscrowTargetRecord, derive_escrow_xwing_keypair, escrow_receipt_cell_key,
        sign_escrow_receipt,
    };
    use fauna_mls::wrapped_blob::generation_wraps::build_mint;
    use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
    use fauna_protocol::merge_policy::{
        KIND_ESCROW_RECEIPT, KIND_ESCROW_TARGET, KIND_GENERATION_MINT,
    };

    fn holder_key() -> SigningKey {
        SigningKey::from_bytes(&[0x66u8; 32])
    }

    /// A custody entry for the box whose deployment seed is `[byte; 32]`.
    fn custodied(byte: u8, domain: &str) -> DeploymentSeedEntry {
        let seed = [byte; 32];
        DeploymentSeedEntry {
            nest_actor_id: fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed(seed),
            seed: seed.into(),
            domain: Some(domain.into()),
            ..Default::default()
        }
    }

    fn actor_hex() -> String {
        root().actor_id_hex()
    }

    /// Write `entries` into the store under `root_dir` through the production
    /// door (`merge_deployment_seeds`) — a device enrolled, a generation it
    /// keys, acked by the holder its trust names, so the `GenerationTip` kind
    /// seals. Returns the open store, which a test keeps open as the engine
    /// would.
    async fn door_written_store(
        root_dir: &StoreRoot,
        entries: &[DeploymentSeedEntry],
    ) -> AccountStore<SqliteBackend> {
        let writer_key = device_key(US);
        let store = AccountStore::open(
            SqliteBackend::open(root_dir.store_dir(&actor_hex()).unwrap()).unwrap(),
            &actor_hex(),
            WriterId(writer_key.verifying_key().to_bytes()),
        )
        .await
        .unwrap();
        let escrow = EscrowTargetRecord {
            xwing_escrow_pubkey: derive_escrow_xwing_keypair(&ESCROW_SEED)
                .public
                .to_bytes()
                .to_vec(),
        };
        let built = build_mint(
            &[member_of(US)],
            &escrow,
            &target_key(),
            Vec::new(),
            &writer_key,
            7_000,
        )
        .unwrap();
        let receipt = sign_escrow_receipt(
            &holder_key(),
            built.generation_id,
            blake3::hash(&built.escrow_wrap).into(),
            &target_key(),
            7_000,
        );
        for row in [
            enrollment_row(US),
            machinery_row(KIND_ESCROW_TARGET, target_key(), &escrow),
            machinery_row(
                KIND_GENERATION_MINT,
                fauna_core::hex32::encode(&built.generation_id),
                &built.record,
            ),
            machinery_row(
                KIND_ESCROW_RECEIPT,
                escrow_receipt_cell_key(
                    &built.generation_id,
                    &receipt.holder_id,
                    &root().actor_id(),
                ),
                &receipt,
            ),
        ] {
            store.put_state(row).await.unwrap();
        }
        let schedule = AccountStateKeySchedule::derive(&BackupKey::derive(&ROOT_SEED));
        let trust = GenerationTrust {
            root: root().actor_id(),
            prior: Vec::new(),
            trusted_holders: vec![holder_key().verifying_key().to_bytes()].into(),
        };
        let bundle = Bundle::default();
        {
            let plane = AccountStatePlane::new_pull_only(
                &store,
                &NoNest,
                &schedule,
                &writer_key,
                &trust,
                ACCOUNT_STATE_FLEET_SCOPE,
            )
            .unwrap()
            .with_generation_custody(&bundle);
            let (_folded, moved) = merge_deployment_seeds(&store, &plane, entries)
                .await
                .unwrap();
            assert!(moved, "the door wrote the custody rows");
        }
        store
    }

    // ── The local read ───────────────────────────────────────────────────────

    /// A store the production door wrote is read back with no runtime, no
    /// nest and no key — while the engine's lock and a live writer connection
    /// are held beside it, as a running account runtime holds them.
    #[tokio::test]
    async fn the_local_read_folds_a_door_written_store_beside_a_live_engine() {
        let tmp = tempfile::tempdir().unwrap();
        let root_dir = StoreRoot::at(tmp.path());
        let entries = [
            custodied(0x5D, "box.example"),
            custodied(0x5E, "other.example"),
        ];
        let engine_store = door_written_store(&root_dir, &entries).await;
        let dir = root_dir.store_dir(&actor_hex()).unwrap();
        let _engine = match fauna_account_store::locks::EngineLock::try_acquire(&dir) {
            fauna_account_store::locks::EngineLockOutcome::Held(lock) => lock,
            other => panic!("the test's engine must hold the lock: {other:?}"),
        };

        let read = read_local_deployment_seeds(&root_dir, &actor_hex())
            .await
            .unwrap();

        assert_eq!(
            read,
            DeploymentSeedEntry::merge_seed_map(&entries, &[]),
            "every custodied box reads back through the fold"
        );
        engine_store
            .put_state(machinery_row(
                KIND_ESCROW_TARGET,
                "after-the-read".into(),
                &1u8,
            ))
            .await
            .expect("the engine's replica stays writable beside the read");
    }

    /// No store at the location: the read answers empty and leaves the
    /// filesystem as it found it — not even the actor's directory.
    #[tokio::test]
    async fn the_local_read_answers_empty_and_creates_nothing_for_a_missing_store() {
        let tmp = tempfile::tempdir().unwrap();
        let root_dir = StoreRoot::at(tmp.path());

        let read = read_local_deployment_seeds(&root_dir, &actor_hex())
            .await
            .unwrap();

        assert!(read.is_empty());
        assert_eq!(
            std::fs::read_dir(tmp.path()).unwrap().count(),
            0,
            "the read minted no store location under the root"
        );
    }

    /// A store directory with no database in it (a half-erased store) is a
    /// missing store: empty, and no database is created there.
    #[tokio::test]
    async fn the_local_read_creates_no_database_in_an_empty_store_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root_dir = StoreRoot::at(tmp.path());
        let dir = root_dir.store_dir(&actor_hex()).unwrap();
        std::fs::create_dir_all(&dir).unwrap();

        let read = read_local_deployment_seeds(&root_dir, &actor_hex())
            .await
            .unwrap();

        assert!(read.is_empty());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    // ── The resolver ─────────────────────────────────────────────────────────

    type Cold = std::future::Ready<Result<Vec<DeploymentSeedEntry>>>;

    fn cold(answer: Result<Vec<DeploymentSeedEntry>>) -> Option<Cold> {
        Some(std::future::ready(answer))
    }

    #[tokio::test]
    async fn the_resolver_answers_the_local_read_when_no_nest_is_asked() {
        let tmp = tempfile::tempdir().unwrap();
        let root_dir = StoreRoot::at(tmp.path());
        let local = [custodied(0x5D, "box.example")];
        let _store = door_written_store(&root_dir, &local).await;

        let resolved = resolve_deployment_seeds::<Cold>(&root_dir, &actor_hex(), None).await;

        assert!(!resolved.cold_asked);
        assert!(resolved.local_failure.is_none());
        assert_eq!(resolved.into_result().unwrap(), local.to_vec());
    }

    #[tokio::test]
    async fn the_resolver_answers_the_cold_read_on_a_device_with_no_store() {
        let tmp = tempfile::tempdir().unwrap();
        let root_dir = StoreRoot::at(tmp.path());
        let remote = vec![custodied(0x5D, "box.example")];

        let resolved =
            resolve_deployment_seeds(&root_dir, &actor_hex(), cold(Ok(remote.clone()))).await;

        assert!(resolved.local_failure.is_none() && resolved.cold_failure.is_none());
        assert_eq!(resolved.into_result().unwrap(), remote);
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    /// Both sources answer: the union by box, and a supersession mark that
    /// only one side carries survives the join (present-wins) — a rotated box
    /// never resurfaces from the side that has not seen the rotation.
    #[tokio::test]
    async fn the_resolver_joins_both_sources_and_a_one_sided_mark_survives() {
        let tmp = tempfile::tempdir().unwrap();
        let root_dir = StoreRoot::at(tmp.path());
        let rotated = custodied(0x5D, "box.example");
        let successor = custodied(0x6D, "box.example");
        let only_local = custodied(0x7D, "local.example");
        let marked = DeploymentSeedEntry {
            superseded_by: Some(successor.nest_actor_id),
            ..rotated.clone()
        };
        let _store = door_written_store(&root_dir, &[marked.clone(), only_local.clone()]).await;

        let seeds = resolve_deployment_seeds(
            &root_dir,
            &actor_hex(),
            cold(Ok(vec![rotated.clone(), successor.clone()])),
        )
        .await
        .into_result()
        .unwrap();

        assert_eq!(
            seeds,
            DeploymentSeedEntry::merge_seed_map(
                &[marked, only_local],
                std::slice::from_ref(&successor)
            ),
        );
        let joined = seeds
            .iter()
            .find(|e| e.nest_actor_id == rotated.nest_actor_id)
            .unwrap();
        assert_eq!(joined.superseded_by, Some(successor.nest_actor_id));
        assert_eq!(
            DeploymentSeedEntry::seed_for(&seeds, &rotated.nest_actor_id),
            None,
            "the resolution-point read refuses the rotated box"
        );
    }

    #[tokio::test]
    async fn the_resolver_keeps_the_local_read_when_the_cold_read_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let root_dir = StoreRoot::at(tmp.path());
        let local = [custodied(0x5D, "box.example")];
        let _store = door_written_store(&root_dir, &local).await;

        let resolved = resolve_deployment_seeds(
            &root_dir,
            &actor_hex(),
            cold(Err(anyhow::anyhow!("the nest did not answer"))),
        )
        .await;

        assert!(resolved.cold_failure.is_some());
        assert_eq!(resolved.into_result().unwrap(), local.to_vec());
    }

    #[tokio::test]
    async fn the_resolver_keeps_the_cold_read_when_the_local_read_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let root_dir = StoreRoot::at(tmp.path());
        let remote = vec![custodied(0x5D, "box.example")];
        // A store location holding a file that is not a database.
        let dir = root_dir.store_dir(&actor_hex()).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(fauna_account_store::sqlite::ACCOUNT_STORE_DB_FILENAME),
            [0xFFu8; 4096],
        )
        .unwrap();

        let resolved =
            resolve_deployment_seeds(&root_dir, &actor_hex(), cold(Ok(remote.clone()))).await;

        assert!(resolved.local_failure.is_some(), "{resolved:?}");
        assert_eq!(resolved.into_result().unwrap(), remote);
    }

    /// Neither source answers: an error naming both, never an empty list a
    /// surface would render as "nothing custodied".
    #[test]
    fn the_resolver_errs_only_when_every_source_asked_failed() {
        let both = join_deployment_seed_sources(
            Err(anyhow::anyhow!("local broke")),
            Some(Err(anyhow::anyhow!("nest broke"))),
        );
        let e = both.into_result().unwrap_err().to_string();
        assert!(e.contains("local broke") && e.contains("nest broke"), "{e}");

        let local_only = join_deployment_seed_sources(Err(anyhow::anyhow!("local broke")), None);
        assert!(local_only.into_result().is_err());
    }
}
