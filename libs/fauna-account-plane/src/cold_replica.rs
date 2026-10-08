//! The **throwaway fleet-scope replica** — a process that hosts no account
//! store reads the account's fleet-only kinds through the plane's own code.
//!
//! Two readers need it, one per key source (`on-demand-files.md` § Shared
//! sets on a capability host, decision 1′, which owns the design; `nest/
//! box-recovery.md` § The plane-era recovery floor, *(b)*, the first reader):
//!
//! - [`ColdKeySource::Seed`] — the box-recovery **cold read**
//!   ([`crate::deployment_seed_recovery::cold_read_deployment_seeds`]): a
//!   device holding the identity seed and no store. Its generations key
//!   through one unfiltered `fauna.generation.escrow.get`, each wrap opened
//!   with the seed-derived escrow secret against the commitment of the walked
//!   mint it names ([`EscrowOpener`]).
//! - [`ColdKeySource::Device`] — a **capability host** (apple's File Provider
//!   extension, android's SAF provider): a process of an enrolled device,
//!   holding the machine principal's writer secret and no store. Its
//!   generations key as the device keys them — the inline member wrap on the
//!   mint or a top-up row addressed to this device id, opened with the KEM
//!   secret the writer secret derives, else the custody it holds
//!   ([`generation_key_for`]) — and every key it unwraps is recorded into that
//!   custody.
//!
//! The replica is an [`AccountStore`] on the in-memory backend under an
//! **ephemeral writer key that is never published**, with a **pull-only**
//! fleet plane over the caller's own authenticated connection: pull-only sends
//! no walker mark and has no publish path, so the nest records nothing for a
//! walk and the replica authors nothing on the plane — a value its walk
//! authors (a merge's output, when two writers' rows under one key join to a
//! value neither holds) is folded into its own store and never sealed
//! ([`AccountStatePlane::new_fold_only`]). ⚠ Two keys, kept apart:
//! the plane's writer key stays the ephemeral one — the plane refuses a
//! signing key that is not its store's writer, and a fresh replica refuses its
//! own writer's rows off the feed as the burnt-journal signature, so under the
//! machine's real writer key it would drop every row this machine's apps
//! wrote. The machine's writer key is only the device arm's UNWRAP key.
//!
//! Everything the replica applies runs the plane's own `apply` — the same
//! writer-signature check, trial open and join as every replica — so a row it
//! folds is a row every replica folds. Its trust is the identity line alone
//! (no `prior`, no escrow holder): trust gates only sealing, and the replica
//! seals nothing.
//!
//! [`ColdFleetReplica::walk`] is incremental: the plane's stored frontier lives
//! in the replica's store, so a held replica's later walks fetch only what
//! moved, and a full re-presentation (`reconcile`) runs only when a walk keys
//! a generation it could not key before.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use fauna_account_store::memory::MemoryBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::{StateEntry, WriterId};
use fauna_core::crypto::{AccountStateKeySchedule, BackupKey, GenerationKey};
use fauna_core::data::FoldersConfig;
use fauna_core::identity::ActorId;
use fauna_protocol::RpcRequester;
use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
use fauna_protocol::generation_escrow::{EscrowGetReply, EscrowGetRequest, KIND_ESCROW_GET};
use fauna_protocol::merge_policy::KIND_GENERATION_MINT;
use zeroize::Zeroizing;

use crate::account_state_plane::AccountStatePlane;
use crate::generation_escrow_recover::{EscrowOpener, minted_core};
use crate::generation_store::live_rows;
use crate::generation_tip::{GenerationTrust, RetainedKeyCustody, generation_key_for};

/// Where a [`ColdFleetReplica`] gets the generation keys the fleet scope's
/// sealed rows need (module docs).
pub enum ColdKeySource {
    /// The escrow arm: the account's identity seed.
    Seed(Zeroizing<[u8; 32]>),
    /// The device arm: the machine principal's writer key — the unwrap key of
    /// the inline member wrap and of every top-up row addressed to this device
    /// — and the custody that holds the generations this device obtained with
    /// no wrap on the plane. Every key the replica unwraps is recorded into it.
    Device {
        writer_key: Box<SigningKey>,
        custody: Arc<dyn RetainedKeyCustody>,
    },
}

/// The throwaway fleet-scope replica (module docs). One per (process,
/// account) for a capability host, held for the process's life; one per read
/// for the cold read.
pub struct ColdFleetReplica<R> {
    rpc: R,
    store: AccountStore<MemoryBackend>,
    /// The ephemeral writer key: the store's writer, never published.
    writer_key: SigningKey,
    schedule: AccountStateKeySchedule,
    trust: GenerationTrust,
    source: ColdKeySource,
    /// What the plane consults while it opens a row: the keys this replica
    /// obtained, then the device arm's custody.
    keys: LayeredCustody,
    /// Walks are serialized: the plane's frontier is the store's.
    walk_lock: tokio::sync::Mutex<()>,
}

impl<R: RpcRequester> ColdFleetReplica<R> {
    /// A replica for `actor` over `rpc` — which must be authenticated as the
    /// account: the account's own fleet scope, and on the seed arm its escrow
    /// wraps, are what the nest serves it. `backup_key` derives the class-2
    /// key schedule. Nothing is fetched until [`Self::walk`].
    ///
    /// # Errors
    /// Minting the ephemeral writer key, or opening the in-memory store.
    pub async fn open(
        rpc: R,
        actor: ActorId,
        backup_key: &BackupKey,
        source: ColdKeySource,
    ) -> Result<Self> {
        let writer_key = ephemeral_writer_key()?;
        let store = AccountStore::open(
            MemoryBackend::new(),
            &fauna_core::hex32::encode(&actor.0),
            WriterId(writer_key.verifying_key().to_bytes()),
        )
        .await
        .context("throwaway replica: open the in-memory store")?;
        let keys = LayeredCustody {
            obtained: Mutex::default(),
            outer: match &source {
                ColdKeySource::Seed(_) => None,
                ColdKeySource::Device { custody, .. } => Some(custody.clone()),
            },
        };
        Ok(Self {
            rpc,
            store,
            writer_key,
            schedule: AccountStateKeySchedule::derive(backup_key),
            trust: GenerationTrust {
                root: actor,
                prior: Vec::new(),
                trusted_holders: Default::default(),
            },
            source,
            keys,
            walk_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Walk the fleet scope from the stored frontier, key every walked mint
    /// through the key source, and — when that keyed a generation the replica
    /// did not hold before — re-present the rows the walk could not open.
    /// Returns how many generations this walk newly keyed.
    ///
    /// # Errors
    /// Any walk, escrow-door or reconcile failure. A generation the source
    /// cannot key is not an error: its rows stay unopened (fail closed), and a
    /// later walk re-tries it.
    pub async fn walk(&self) -> Result<usize> {
        let _serial = self.walk_lock.lock().await;
        let fleet = AccountStatePlane::new_fold_only(
            &self.store,
            &self.rpc,
            &self.schedule,
            &self.writer_key,
            &self.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )?
        .with_generation_custody(&self.keys);
        fleet
            .walk()
            .await
            .context("throwaway replica: walk the fleet scope")?;
        let keyed = match &self.source {
            ColdKeySource::Seed(seed) => self.key_from_escrow(seed).await?,
            ColdKeySource::Device { writer_key, .. } => self.key_as_device(writer_key).await?,
        };
        if keyed > 0 {
            fleet
                .reconcile()
                .await
                .context("throwaway replica: re-present the rows the new keys open")?;
        }
        Ok(keyed)
    }

    /// The escrow arm: one unfiltered escrow read, every wrap opened against
    /// the walked mint it names. No receipt is consulted and no holder is
    /// trusted — what a wrap is trusted for is its commitment check (`nest/
    /// box-recovery.md` § The plane-era recovery floor, *(b)*).
    async fn key_from_escrow(&self, seed: &[u8; 32]) -> Result<usize> {
        let reply: EscrowGetReply = self
            .rpc
            .request(KIND_ESCROW_GET, EscrowGetRequest::default())
            .await
            .map_err(|e| anyhow::anyhow!("throwaway replica: {KIND_ESCROW_GET}: {e}"))?;
        let opener = EscrowOpener::new(seed, &self.trust.root);
        let mut keyed = 0;
        for entry in live_rows(&self.store, KIND_GENERATION_MINT).await? {
            let Some((generation_id, core)) = minted_core(&entry) else {
                continue;
            };
            if self.keys.holds(&generation_id) {
                continue;
            }
            if let Some(key) = opener.open(&reply.wraps, &generation_id, &core) {
                self.keys.record_generation_key(&generation_id, &key);
                keyed += 1;
            }
        }
        Ok(keyed)
    }

    /// The device arm: every live mint keyed as this device keys it — custody
    /// first, then the inline wrap for this device id, then every merged
    /// top-up row ([`generation_key_for`], which records what it unwraps into
    /// the custody and drops an authored-shredded generation's key from it).
    async fn key_as_device(&self, writer_key: &SigningKey) -> Result<usize> {
        let view = crate::fleet_removal::fleet_view(&self.store, &self.trust).await?;
        let mut keyed = 0;
        for entry in live_rows(&self.store, KIND_GENERATION_MINT).await? {
            let Ok(generation_id) = fauna_core::hex32::decode(&entry.key) else {
                continue;
            };
            if self.keys.holds(&generation_id) {
                continue;
            }
            if let Some(key) = generation_key_for(
                &self.store,
                &generation_id,
                writer_key,
                Some(&self.keys),
                Some(&view),
            )
            .await?
            {
                self.keys.record_generation_key(&generation_id, &key);
                keyed += 1;
            }
        }
        Ok(keyed)
    }

    /// Every merged row of `kind`, tombstones included — the rows as the last
    /// walk left them, for a caller's own fold.
    ///
    /// # Errors
    /// A store read failure.
    pub async fn states_of_kind(&self, kind: &str) -> Result<Vec<StateEntry>> {
        self.store.states_of_kind(kind).await
    }

    /// The account's folder-key custody as the last walk left it, through the
    /// plane's own door ([`crate::folder_key_rows::read_folder_keys`]).
    ///
    /// # Errors
    /// A stored row the fold refuses.
    pub async fn read_folder_keys(&self) -> Result<FoldersConfig> {
        crate::folder_key_rows::read_folder_keys(&self.store).await
    }

    /// The replica's store, for an in-crate reader's own fold.
    pub(crate) fn store(&self) -> &AccountStore<MemoryBackend> {
        &self.store
    }
}

/// A writer key no row is ever signed under: the replica's own writer id is
/// required to open it, and nothing the replica does publishes a row.
fn ephemeral_writer_key() -> Result<SigningKey> {
    let mut secret = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut secret[..])
        .map_err(|e| anyhow::anyhow!("throwaway replica: mint the ephemeral writer key: {e}"))?;
    Ok(SigningKey::from_bytes(&secret))
}

/// The custody the replica's plane consults: the generation keys this replica
/// obtained, held in memory for its life, over the device arm's own custody.
/// A key is recorded into both; the memory layer is what keeps an open
/// possible when the outer custody could not persist a key (a slot at its item
/// cap).
struct LayeredCustody {
    obtained: Mutex<BTreeMap<[u8; 32], Zeroizing<[u8; 32]>>>,
    outer: Option<Arc<dyn RetainedKeyCustody>>,
}

impl LayeredCustody {
    fn obtained(&self) -> std::sync::MutexGuard<'_, BTreeMap<[u8; 32], Zeroizing<[u8; 32]>>> {
        self.obtained
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether this replica already keyed `generation` itself — a later walk
    /// skips it, so only a newly keyed generation costs a re-presentation.
    fn holds(&self, generation: &[u8; 32]) -> bool {
        self.obtained().contains_key(generation)
    }
}

impl RetainedKeyCustody for LayeredCustody {
    fn retained_generation_key(&self, generation: &[u8; 32]) -> Option<GenerationKey> {
        if let Some(key) = self.obtained().get(generation) {
            return Some(GenerationKey::from_bytes(**key));
        }
        self.outer
            .as_ref()
            .and_then(|c| c.retained_generation_key(generation))
    }
    fn record_generation_key(&self, generation: &[u8; 32], key: &GenerationKey) {
        self.obtained()
            .insert(*generation, Zeroizing::new(*key.as_bytes()));
        if let Some(outer) = &self.outer {
            outer.record_generation_key(generation, key);
        }
    }
    fn drop_generation_key(&self, generation: &[u8; 32]) {
        self.obtained().remove(generation);
        if let Some(outer) = &self.outer {
            outer.drop_generation_key(generation);
        }
    }
}

/// A [`RetainedKeyCustody`] held in memory only — the device arm's custody
/// where a process has no durable one to give it (tests, and a host the app
/// provisioned no custody for), and the shape a caller records a generation
/// it recovered some other way into by hand.
#[derive(Default)]
pub struct MemoryRetainedKeys(Mutex<BTreeMap<[u8; 32], Zeroizing<[u8; 32]>>>);

impl MemoryRetainedKeys {
    fn keys(&self) -> std::sync::MutexGuard<'_, BTreeMap<[u8; 32], Zeroizing<[u8; 32]>>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The generations held.
    pub fn generations(&self) -> BTreeSet<[u8; 32]> {
        self.keys().keys().copied().collect()
    }
}

impl RetainedKeyCustody for MemoryRetainedKeys {
    fn retained_generation_key(&self, generation: &[u8; 32]) -> Option<GenerationKey> {
        self.keys()
            .get(generation)
            .map(|k| GenerationKey::from_bytes(**k))
    }
    fn record_generation_key(&self, generation: &[u8; 32], key: &GenerationKey) {
        self.keys()
            .insert(*generation, Zeroizing::new(*key.as_bytes()));
    }
    fn drop_generation_key(&self, generation: &[u8; 32]) {
        self.keys().remove(generation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_fixture_test_support::{
        ESCROW_SEED, ROOT_SEED, THEM, US, device_key, machinery_row, member_of, root, target_key,
    };
    use fauna_core::generation::{EscrowTargetRecord, derive_escrow_xwing_keypair};
    use fauna_mls::wrapped_blob::generation_wraps::build_mint;

    /// A nest whose fleet feed is empty, recording every kind it is asked —
    /// the rows a walk would merge are placed in the replica's store directly.
    #[derive(Default)]
    struct EmptyFeed(Mutex<Vec<&'static str>>);

    impl RpcRequester for EmptyFeed {
        type Error = anyhow::Error;
        async fn request<Req, Reply>(&self, kind: &'static str, _: Req) -> Result<Reply>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.0.lock().unwrap().push(kind);
            anyhow::ensure!(kind == "fauna.sync.changes.list", "not served: {kind}");
            Ok(fauna_core::encoding::canonical_decode(
                &fauna_core::encoding::canonical_encode(
                    &fauna_protocol::sync::SyncChangesListReply::default(),
                )
                .unwrap(),
            )
            .unwrap())
        }
    }

    async fn device_replica(custody: Arc<MemoryRetainedKeys>) -> ColdFleetReplica<EmptyFeed> {
        ColdFleetReplica::open(
            EmptyFeed::default(),
            root().actor_id(),
            &BackupKey::derive(&ROOT_SEED),
            ColdKeySource::Device {
                writer_key: Box::new(device_key(US)),
                custody,
            },
        )
        .await
        .unwrap()
    }

    /// A mint over `members`, minted by [`THEM`], merged into `replica`'s
    /// store as a walk would leave it. Returns its id and key.
    async fn merged_mint(
        replica: &ColdFleetReplica<EmptyFeed>,
        members: &[[u8; 32]],
    ) -> ([u8; 32], GenerationKey) {
        let escrow = EscrowTargetRecord {
            xwing_escrow_pubkey: derive_escrow_xwing_keypair(&ESCROW_SEED)
                .public
                .to_bytes()
                .to_vec(),
        };
        let members: Vec<_> = members.iter().copied().map(member_of).collect();
        let built = build_mint(
            &members,
            &escrow,
            &target_key(),
            Vec::new(),
            &device_key(THEM),
            7_000,
        )
        .unwrap();
        replica
            .store()
            .put_state(machinery_row(
                KIND_GENERATION_MINT,
                fauna_core::hex32::encode(&built.generation_id),
                &built.record,
            ))
            .await
            .unwrap();
        (built.generation_id, built.gen_key)
    }

    /// The device arm keys a mint through the inline wrap addressed to its
    /// device id, records the key into the custody it was given, asks the
    /// nest nothing but the feed, and does not re-key what it holds.
    #[tokio::test]
    async fn the_device_arm_keys_a_mint_through_its_inline_wrap_and_records_it() {
        let custody = Arc::new(MemoryRetainedKeys::default());
        let replica = device_replica(custody.clone()).await;
        let (generation, key) = merged_mint(&replica, &[US, THEM]).await;

        assert_eq!(replica.walk().await.unwrap(), 1, "one generation keyed");
        assert_eq!(
            custody
                .retained_generation_key(&generation)
                .map(|k| *k.as_bytes()),
            Some(*key.as_bytes()),
            "the unwrapped key is recorded into the host's custody"
        );
        assert_eq!(replica.walk().await.unwrap(), 0, "held: nothing re-keyed");
        assert!(
            replica
                .rpc
                .0
                .lock()
                .unwrap()
                .iter()
                .all(|k| *k == "fauna.sync.changes.list"),
            "the device arm never asks the escrow door"
        );
    }

    /// A device the mint does not name keys nothing from the plane, and its
    /// custody stays empty — the removed-member shape fails closed.
    #[tokio::test]
    async fn the_device_arm_keys_nothing_for_a_mint_that_does_not_name_it() {
        let custody = Arc::new(MemoryRetainedKeys::default());
        let replica = device_replica(custody.clone()).await;
        merged_mint(&replica, &[THEM]).await;

        assert_eq!(replica.walk().await.unwrap(), 0);
        assert!(custody.generations().is_empty());
    }

    /// A generation held only in custody — recovered some other way, no wrap
    /// on the plane reaching this device — keys the replica all the same.
    #[tokio::test]
    async fn the_device_arm_keys_a_generation_held_only_in_custody() {
        let custody = Arc::new(MemoryRetainedKeys::default());
        let replica = device_replica(custody.clone()).await;
        let (generation, key) = merged_mint(&replica, &[THEM]).await;
        custody.record_generation_key(&generation, &key);

        assert_eq!(replica.walk().await.unwrap(), 1);
        assert!(replica.keys.holds(&generation));
    }

    /// A custody key failing the live mint's commitment is not served: the
    /// replica keys nothing rather than open rows under a key the mint never
    /// committed to.
    #[tokio::test]
    async fn a_custody_key_failing_the_mints_commitment_keys_nothing() {
        let custody = Arc::new(MemoryRetainedKeys::default());
        let replica = device_replica(custody.clone()).await;
        let (generation, _) = merged_mint(&replica, &[THEM]).await;
        custody.record_generation_key(&generation, &GenerationKey::from_bytes([0xEE; 32]));

        assert_eq!(replica.walk().await.unwrap(), 0);
    }
}
