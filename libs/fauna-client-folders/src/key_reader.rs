//! [`FolderKeyReader`] and [`FolderKeyStore`] — the folder-key custody seam.
//!
//! Shared-set custody rests only on the account plane
//! (`fauna.state.folder-keys`, `config-dissolution.md`'s kinds table), so
//! every custody read and write goes through this seam rather than a
//! blob load. The read half has two implementations, each in the crate
//! that owns its source: the account's own store handle, for a process that
//! hosts the account runtime, and a capability host's throwaway fleet replica
//! (`fauna_sync_engine::cold_folder_keys::ColdReplicaFolderKeys`), for a
//! process that hosts no account store (`on-demand-files.md` § Shared sets on
//! a capability host, decision 1′).
//!
//! The reader is read-only on purpose. A capability host never writes custody —
//! its content keys are the account's, written by the apps that hold the
//! account runtime — so it implements [`FolderKeyReader`] and nothing else: the
//! write half is the separate supertrait [`FolderKeyStore`] that only a
//! store-hosting process implements, making "a host writes no custody" a
//! type-level fact rather than a runtime refusal.

use async_trait::async_trait;
use fauna_core::data::{FolderPendingRemoval, FoldersConfig};

/// The account's folder-key custody as its source holds it now.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait FolderKeyReader: fauna_core::MaybeSendSync {
    /// The custody fold, fresh from the source — a reader backed by a nest
    /// walks it first. An `Err` is "custody unreadable", never an empty
    /// custody: a caller builds a bound set keyless on it, it never falls back
    /// to plaintext.
    async fn load(&self) -> anyhow::Result<FoldersConfig>;

    /// This device's **adoption markers** (`writer-signed-change-records.md`
    /// ruling (11)(d)): the nonces this device's re-mints replaced, each a
    /// standing licence for this device's engine to adopt the nest's history
    /// heads of that set once. Device-local — the store's replica-local meta,
    /// never the shared plane — so a source that holds none (a capability
    /// host's cold replica) answers empty, which licenses nothing.
    async fn adoption_markers(&self) -> anyhow::Result<Vec<[u8; 32]>> {
        Ok(Vec::new())
    }

    /// When the host of this device's sync engine last asked for the set at
    /// `channel_id` to have its content-key envelope re-fetched
    /// ([`Self::request_refetch`]), in microseconds — `None` when it never
    /// did, or when this source keeps no device-local state.
    async fn refetch_requested_at(&self, _channel_id: &[u8; 32]) -> anyhow::Result<Option<u64>> {
        Ok(None)
    }

    /// Ask the process that holds this account's MLS engine to re-fetch the
    /// content-key envelope of the set at `channel_id`
    /// (`writer-signed-change-records.md` ruling (11)(b)): a member's engine
    /// refused a row of the set `signature_invalid`, so the nonce it was built
    /// with is not the one the set's writers sign under. The engine cannot
    /// fetch it itself — opening the envelope takes the MLS group — and on a
    /// desktop it runs in the sync agent, a process apart from the app that
    /// can; the request is the one signal that crosses, read by the member's
    /// custody-ingest sink ([`Self::refetch_requested_at`]).
    ///
    /// **Not a custody write**, which is why it sits on the read half: one
    /// row of the store's replica-local meta, device-local and never synced
    /// like the adoption markers, owned by the engine's host (its only
    /// writer; one engine per set, so no two writers of one key). A source
    /// that keeps no device-local state — a capability host's cold replica —
    /// drops the request: its app's next launch re-fetches every set anyway.
    async fn request_refetch(
        &self,
        _channel_id: &[u8; 32],
        _now_micros: u64,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// The account's mail-custody MSEKs, current first, then the retained
    /// prior generations (`fauna.state.mail`) — what a set's engine derives
    /// the owner's recipient key from to open the folder's parked third-party
    /// deposits (`file-sync.md` § Third-party deposit ingress, adoption). Not
    /// folder-key custody, but the same source answers it: the account store
    /// a runtime-hosting process mounts. A source that holds no mail custody —
    /// a capability host's cold replica — answers empty, and its engines
    /// adopt nothing: another seat of the owner's does.
    async fn recipient_mseks(&self) -> anyhow::Result<Vec<fauna_core::secret::SecretArray32>> {
        Ok(Vec::new())
    }
}

/// The replica-local meta key the engine's host stamps a set's re-fetch
/// request under ([`FolderKeyReader::request_refetch`]) — one row per set, so
/// each engine writes a key no other engine does and the write needs no read.
pub fn refetch_request_meta_key(channel_id: &[u8; 32]) -> String {
    format!("folder_keys/refetch_request/{}", hex::encode(channel_id))
}

/// The stored form of a re-fetch request: when it was made.
pub fn encode_refetch_request(now_micros: u64) -> Vec<u8> {
    now_micros.to_be_bytes().to_vec()
}

/// [`encode_refetch_request`]'s inverse; an absent or malformed row is no
/// request.
pub fn decode_refetch_request(bytes: Option<&[u8]>) -> Option<u64> {
    Some(u64::from_be_bytes(bytes?.try_into().ok()?))
}

/// A level of "custody may have changed" notices
/// ([`FolderKeyStore::change_notices`]): sources coalesce, so a burst reads as
/// one, and the consumer re-reads custody and acts only on what differs.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait CustodyNotices: fauna_core::MaybeSendSync {
    /// Wait until custody may have changed: `true` then, `false` once the
    /// source has ended (the runtime is gone), after which the consumer ends.
    async fn changed(&mut self) -> bool;
}

/// The WRITE half of the custody seam — implemented only by a process that
/// hosts the account's store (the seat's `AccountStoreHandle`, served by
/// `fauna_account_seams::folder_keys::PlaneFolderKeys`, and on web the account
/// port's forwarder, [`crate::port::PortFolderKeys`]).
///
/// The store is **merge-only** (`mls-group-key-material.md` § M2 → *Custody
/// shape of the set nonce*, ruling (l)): a write joins the caller's replica
/// into what rests and answers the joined custody, so a concurrent device's
/// write is never lost and there is no compare-and-swap to retry — the join is
/// the conflict resolution. The custody API's one removal, a staged removal's
/// clear, is [`FolderKeyStore::settle_removal`].
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait FolderKeyStore: FolderKeyReader {
    /// [`FolderKeyReader::load`] read as the first half of a write — a
    /// read-join-put (a set keyed, a received key ingested, a removal staged).
    /// A store whose writes wait out a runtime still assembling
    /// (`fauna_account_seams::folder_keys`) waits here too, so a write made
    /// right after sign-in lands instead of failing on its own read; the
    /// answer is still the custody or an error, never an empty custody.
    /// Defaults to `load`, for a store that is always up.
    async fn load_for_write(&self) -> anyhow::Result<FoldersConfig> {
        self.load().await
    }

    /// A source of "custody may have changed" notices for this store — the
    /// wake the served-blob follower re-reads custody on
    /// ([`crate::ServedBlobFollower`], `writer-signed-change-records.md`
    /// ruling (7)(b)(ii) rule (3): the blob reconcile re-runs on the custody
    /// nudge). The store over a seat's account runtime answers its
    /// store-change notice, which fires once a walk has LANDED a sibling's
    /// custody write — whichever process walked — so the re-read never races
    /// the walk the nudge started. Waits out a runtime still assembling, as a
    /// write does. `None` (the default): this store has no notice, and the
    /// launch pass is its only re-run.
    async fn change_notices(&self) -> Option<Box<dyn CustodyNotices>> {
        None
    }

    /// Join `replica` into the stored custody; answers the custody as it now
    /// reads. A write adds and advances, never drops.
    async fn merge(&self, replica: FoldersConfig) -> anyhow::Result<FoldersConfig>;

    /// Settle one staged removal whose rotation the caller has committed: its
    /// fresh generation lands in the set it rotates and the staging leaves the
    /// fold on every replica, never to return from a stale one. Idempotent.
    async fn settle_removal(&self, removal: FolderPendingRemoval) -> anyhow::Result<FoldersConfig>;

    /// Record a device-local adoption marker naming `replaced`, the nonce a
    /// re-mint is about to replace (ruling (11)(d)) — written BEFORE the
    /// custody write the re-mint makes, durable when this answers. A store
    /// that keeps no device-local state refuses, and the re-mint does not run.
    async fn record_adoption_marker(&self, _replaced: [u8; 32]) -> anyhow::Result<()> {
        anyhow::bail!("this folder-key custody store keeps no device-local adoption marker")
    }

    /// The identity this custody serves — the one whose device mints a
    /// set's nonce here, which the create helper records as the entry's
    /// `minted_by` (ruling (11)(a)). `None` when the store cannot say: the
    /// entry is then written without one, and the owner's reconcile re-mints
    /// it under the identity it runs as.
    async fn minting_identity(&self) -> anyhow::Result<Option<fauna_core::identity::ActorId>> {
        Ok(None)
    }
}

/// The replica-local meta key a store-hosting process keeps this device's
/// adoption markers under (`writer-signed-change-records.md` ruling (11)(d)):
/// one row, the canonical encoding of the marker list
/// ([`encode_adoption_markers`]). Device-local and never synced — the store's
/// meta table is per replica — and shared by every process that mounts this
/// device's store (the identity app that writes it, the sync agent that reads
/// it).
pub const ADOPTION_MARKERS_META_KEY: &str = "folder_keys/adoption_markers";

/// The stored form of the marker list.
pub fn encode_adoption_markers(markers: &[[u8; 32]]) -> anyhow::Result<Vec<u8>> {
    let wire: Vec<fauna_protocol::ByteBuf> = markers
        .iter()
        .map(|m| fauna_protocol::ByteBuf::from(m.to_vec()))
        .collect();
    Ok(fauna_core::encoding::canonical_encode(&wire)?)
}

/// [`encode_adoption_markers`]'s inverse; an absent row is no marker.
pub fn decode_adoption_markers(bytes: Option<&[u8]>) -> anyhow::Result<Vec<[u8; 32]>> {
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    fauna_core::encoding::canonical_decode::<Vec<fauna_protocol::ByteBuf>>(bytes)?
        .into_iter()
        .map(|m| {
            <[u8; 32]>::try_from(m.as_slice())
                .map_err(|_| anyhow::anyhow!("an adoption marker is not 32 bytes"))
        })
        .collect()
}

/// Load → mutate → merge: the one read-modify-write every custody writer runs.
/// Answers the joined custody and what `edit` returned. No CAS and no retry —
/// the store's join is the conflict resolution. The read is the write's own
/// ([`FolderKeyStore::load_for_write`]): over a runtime still assembling it
/// waits as the join does.
pub async fn update<T>(
    store: &dyn FolderKeyStore,
    edit: impl FnOnce(&mut FoldersConfig) -> T,
) -> anyhow::Result<(FoldersConfig, T)> {
    let mut custody = store.load_for_write().await?;
    let out = edit(&mut custody);
    Ok((store.merge(custody).await?, out))
}

/// A [`FolderKeyReader`] whose every load answers why custody cannot be read —
/// what a host holds when its custody source failed to start. Never an empty
/// custody: a bound set builds keyless and fails closed, never plaintext.
pub struct UnreadableFolderKeys(pub String);

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl FolderKeyReader for UnreadableFolderKeys {
    async fn load(&self) -> anyhow::Result<FoldersConfig> {
        Err(anyhow::anyhow!("folder-key custody unreadable: {}", self.0))
    }
}

/// An in-memory [`FolderKeyStore`] with the plane door's semantics — a write
/// joins through [`FoldersConfig::merge`], a settle commits the staging's
/// generation and keeps it out of the fold for good. The store a test (or a
/// host's test) hands the custody writers in place of a seat's handle.
#[derive(Default)]
pub struct MemoryFolderKeyStore {
    inner: std::sync::Mutex<MemoryCustody>,
}

#[derive(Default)]
struct MemoryCustody {
    custody: FoldersConfig,
    /// The settled stagings — the plane's `settled` marker, matched by the
    /// custody API's staging identity (channel, member, fresh key).
    settled: Vec<FolderPendingRemoval>,
    /// The device-local adoption markers ([`FolderKeyStore::record_adoption_marker`]).
    markers: Vec<[u8; 32]>,
    /// [`FolderKeyStore::minting_identity`].
    identity: Option<fauna_core::identity::ActorId>,
    /// The device-local re-fetch requests ([`FolderKeyReader::request_refetch`]).
    refetch_requests: std::collections::HashMap<[u8; 32], u64>,
}

impl MemoryCustody {
    fn strip_settled(&mut self) {
        let settled = &self.settled;
        self.custody.pending_removals.retain(|r| {
            !settled.iter().any(|s| {
                s.channel_id == r.channel_id
                    && s.removed_member == r.removed_member
                    && s.new_generation.key == r.new_generation.key
            })
        });
    }
}

impl MemoryFolderKeyStore {
    /// A store already holding `custody`.
    pub fn with(custody: FoldersConfig) -> Self {
        Self {
            inner: std::sync::Mutex::new(MemoryCustody {
                custody,
                ..Default::default()
            }),
        }
    }

    /// This store serving `identity` ([`FolderKeyStore::minting_identity`]).
    #[must_use]
    pub fn serving(self, identity: fauna_core::identity::ActorId) -> Self {
        self.inner.lock().expect("custody lock").identity = Some(identity);
        self
    }

    /// The custody as it rests, synchronously — for a test's assertions.
    pub fn snapshot(&self) -> FoldersConfig {
        self.inner.lock().expect("custody lock").custody.clone()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl FolderKeyReader for MemoryFolderKeyStore {
    async fn load(&self) -> anyhow::Result<FoldersConfig> {
        Ok(self.snapshot())
    }

    async fn adoption_markers(&self) -> anyhow::Result<Vec<[u8; 32]>> {
        Ok(self.inner.lock().expect("custody lock").markers.clone())
    }

    async fn refetch_requested_at(&self, channel_id: &[u8; 32]) -> anyhow::Result<Option<u64>> {
        let inner = self.inner.lock().expect("custody lock");
        Ok(inner.refetch_requests.get(channel_id).copied())
    }

    async fn request_refetch(&self, channel_id: &[u8; 32], now_micros: u64) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().expect("custody lock");
        inner.refetch_requests.insert(*channel_id, now_micros);
        Ok(())
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl FolderKeyStore for MemoryFolderKeyStore {
    async fn merge(&self, replica: FoldersConfig) -> anyhow::Result<FoldersConfig> {
        let mut inner = self.inner.lock().expect("custody lock");
        inner.custody = inner.custody.merge(&replica);
        inner.strip_settled();
        Ok(inner.custody.clone())
    }

    async fn settle_removal(&self, removal: FolderPendingRemoval) -> anyhow::Result<FoldersConfig> {
        let mut inner = self.inner.lock().expect("custody lock");
        // The plane writes the generation into the set it rotates whether or
        // not the caller's commit reached this replica; a set it holds no
        // keys for has nothing to rotate here.
        let _ = crate::custody::commit_generation(
            &mut inner.custody,
            &removal.channel_id,
            &removal.new_generation,
        );
        inner.settled.push(removal);
        inner.strip_settled();
        Ok(inner.custody.clone())
    }

    async fn record_adoption_marker(&self, replaced: [u8; 32]) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().expect("custody lock");
        if !inner.markers.contains(&replaced) {
            inner.markers.push(replaced);
        }
        Ok(())
    }

    async fn minting_identity(&self) -> anyhow::Result<Option<fauna_core::identity::ActorId>> {
        Ok(self.inner.lock().expect("custody lock").identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_core::folder_keys::ContentKeyGeneration;
    use fauna_core::identity::ActorId;

    const CH: [u8; 32] = [7; 32];

    fn staging(key: u8) -> FolderPendingRemoval {
        FolderPendingRemoval {
            channel_id: CH,
            name: "docs".into(),
            removed_member: ActorId([2; 32]),
            new_generation: ContentKeyGeneration {
                version: 2,
                key: [key; 32].into(),
                rotated_at: 2_000,
            },
            commit: None,
            gated_attempted: false,
        }
    }

    #[test]
    fn a_merge_never_drops_what_another_writer_put() {
        let store = MemoryFolderKeyStore::default();
        let mut ours = FoldersConfig::default();
        crate::custody::record_new_set(&mut ours, CH, [0x42; 32], 1_000);
        block_on(store.merge(ours)).unwrap();
        // A stale replica — read before the set existed — joins, never replaces.
        let joined = block_on(store.merge(FoldersConfig::default())).unwrap();
        assert!(crate::custody::content_keys(&joined, &CH).is_some());
    }

    #[test]
    fn a_settled_staging_leaves_the_fold_and_a_stale_replica_cannot_return_it() {
        let store = MemoryFolderKeyStore::default();
        block_on(update(&store, |c| {
            crate::custody::record_new_set(c, CH, [0x42; 32], 1_000);
            crate::custody::stage_pending_removal(c, staging(0x43));
        }))
        .unwrap();
        let stale = store.snapshot();
        let settled = block_on(store.settle_removal(staging(0x43))).unwrap();
        assert!(settled.pending_removals.is_empty());
        assert_eq!(
            crate::custody::current_generation(&settled, &CH)
                .unwrap()
                .version,
            2,
            "the settle lands the fresh generation"
        );
        let after = block_on(store.merge(stale)).unwrap();
        assert!(after.pending_removals.is_empty(), "no resurrection");
    }

    /// A custody store over a runtime still assembling: a plain read answers
    /// "not running" at once, while a write — and the read a write starts
    /// from — waits the assembly out (`fauna_account_seams::folder_keys`),
    /// modelled here as succeeding.
    struct AssemblingRuntime(MemoryFolderKeyStore);

    #[async_trait]
    impl FolderKeyReader for AssemblingRuntime {
        async fn load(&self) -> anyhow::Result<FoldersConfig> {
            Err(anyhow::anyhow!("the account runtime is not running"))
        }
    }

    #[async_trait]
    impl FolderKeyStore for AssemblingRuntime {
        async fn load_for_write(&self) -> anyhow::Result<FoldersConfig> {
            self.0.load().await
        }
        async fn merge(&self, replica: FoldersConfig) -> anyhow::Result<FoldersConfig> {
            self.0.merge(replica).await
        }
        async fn settle_removal(
            &self,
            removal: FolderPendingRemoval,
        ) -> anyhow::Result<FoldersConfig> {
            self.0.settle_removal(removal).await
        }
    }

    /// A custody write made before the account runtime has assembled (web's
    /// tab, right after sign-in) waits for the runtime as its join does,
    /// instead of failing on the read it starts from.
    #[test]
    fn an_update_before_the_runtime_assembles_waits_rather_than_failing() {
        let store = AssemblingRuntime(MemoryFolderKeyStore::default());
        block_on(update(&store, |c| {
            crate::custody::record_new_set(c, CH, [0x42; 32], 1_000);
        }))
        .expect("the write lands once the runtime is up");
        assert!(crate::custody::content_keys(&store.0.snapshot(), &CH).is_some());
        assert!(
            block_on(store.load()).is_err(),
            "a plain read still refuses"
        );
    }
}
