//! UniFFI surface for the macOS/iOS **File Provider** on-demand host — the
//! callback→engine primitives an `NSFileProviderReplicatedExtension` calls.
//!
//! `docs/goal/behavior/file-sync.md` § On-Demand Files → Apple File Provider
//! binding (ratified 2026-07-18) owns the callback→engine table + milestone chain.
//! This is **M1**: the FFI surface that vends the shared, tier-1-tested
//! [`fauna_sync_engine::provider_face`] serving cores. No Swift/appex changes in
//! M1 — the appex wires these in M2 (read: enumerate/fetch/evict) / M3
//! (write: ingest/delete/rename).
//!
//! ## Why a dedicated host (not [`crate::FfiSyncEngineHost`])
//!
//! The FP extension is a **separate, app-dead process**: `fileproviderd` launches
//! it with the app not running, and iOS has no daemons (`file-sync.md` § Apple File
//! Provider binding, *the extension hosts the engines*). So it cannot reach the
//! app's [`FfiSyncEngineHost`] (which is built from the app's live WS-RPC
//! connection). It holds **one** `folder`-scoped engine of its own — one FP
//! domain ↔ one folder ↔ one engine — and answers the OS's callbacks. This is the
//! out-of-app hydration host of `file-sync.md` § *Who runs the hydration host*.
//!
//! ## Control inversion (why there is no watcher)
//!
//! The FP host builds its engine ([`build_engine`]) but runs **no** watcher-driven
//! loop (those are the `fauna-sync-agent`'s): the OS owns the
//! on-disk tree and *calls* the extension, so the host serves discrete
//! request/response primitives. It keeps the engine resident on a dedicated OS
//! thread with a current-thread runtime — the `SyncEngine`-is-`!Sync` reason spelled
//! out on [`FfiSyncEngineHost`].
//!
//! ## M1 → M2 construction note
//!
//! M1 constructs this over the same **seed-based** [`crate::sync_engine_host::HostContext`]
//! the byte-sync host uses (the only build path that exists today). **M2 replaces the
//! constructor** with the app-dead shape from `file-sync.md` § *Who runs the
//! hydration host*: the extension is provisioned a `BackupKey` + renewable bearer via
//! the shared app-group Keychain (never the identity seed), and per-set M2 content
//! keys load from the `BackupKey`-sealed `fauna.state.folder-keys` custody. M2 also adds the remote-pull
//! tick that drives `NSFileProviderManager.signalEnumerator`. The **primitive
//! surface below is stable across that change** — only the build inputs move.
//!
//! ## The owned-tree surface (android)
//!
//! A host built by [`FfiFileProviderHost::app_dead_owned_tree`] also owns its
//! replica — the kept and cache roots of
//! [`fauna_sync_engine::provider_face::owned_tree`], for a provider no OS keeps
//! a tree for (android's SAF `DocumentsProvider`, `on-demand-files.md`
//! § Android SAF DocumentsProvider binding). It serves the same primitives plus
//! the owned-tree operations (`open_for_read`, `open_for_write`,
//! `closed_write`, `create_document`, `delete_document`, `rename_document`,
//! `sweep_kept_root`, `observe_evictions`, and the peer-transfer seam's
//! `lookup_body` / `land_body` / `record_placeholder`), and runs the start
//! sweep itself once the set is populated. A host built by [`Self::app_dead`]
//! (apple's) answers those with an error — its surface is unchanged.
//!
//! ## A reader's host
//!
//! A set shared with the account that it may only read builds **read-only**
//! (`on-demand-files.md` § Shared sets on a capability host, decision 3): the
//! host learns it from the set's own row at every build and edge — never from
//! its caller — says so through [`FfiFileProviderHost::is_read_only`], and
//! every write it is handed is refused by the shared cores with
//! `provider_face::ReadOnlyHost` before anything is sealed or moved. It runs
//! no seal edge, no kept-root sweep and no re-seal drive: nothing it does
//! seals. A reader granted `writer` rebuilds two-way at its next edge, and a
//! writer demoted to reader rebuilds read-only.
//!
//! ## The share plane's door, and the host registry
//!
//! An owned-tree host is also where a phone's **share plane** lands what it
//! pulls from peers (`p2p-shared-set-build.md` § *Phone peers — design*,
//! decision 1): `share_ingest` is the on-demand twin of the sync agent's
//! `ShareIngest` command arm, run on this host's own worker — the set's one
//! writer. The plane finds the hosts through the process-wide registry below
//! ([`on_demand_host`]), keyed by account and set: one host per set, one
//! writer per set, reached by the platform's provider and by the plane alike.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use fauna_core::folder_keys::FolderRef;
use fauna_nest_http::{ApiError, BearerSource};
use fauna_sync_engine::engine_lifecycle::{BuiltEngine, EdgeVerdict, build_engine};
use fauna_sync_engine::provider_face;
use fauna_sync_engine::provider_face::owned_tree::{BodyRoot, OwnedTree};

use crate::FfiError;
use crate::crypto::bytes32;
use crate::sync_engine_host::{HostContext, file_provider_host_context, parse_folder_id};

/// A source of the extension's current nest **bearer**, implemented in Swift — it
/// reads the shared app-group Keychain the app writes at first domain creation (and
/// rotates on refresh), so the **app-dead** extension always presents a
/// currently-provisioned token without ever holding the identity seed
/// (`file-sync.md` § Apple File Provider binding, *the extension hosts the
/// engines*). Sync — a Keychain read is a fast local call.
#[uniffi::export(with_foreign)]
pub trait FfiBearerProvider: Send + Sync {
    /// The bearer the app most recently wrote to the shared app-group Keychain.
    /// Empty ⇒ none provisioned yet (the host then fails closed until one appears).
    fn current_bearer(&self) -> String;
}

/// Adapts a Swift [`FfiBearerProvider`] to the client [`BearerSource`]. Reads fresh
/// on every `bearer()` — no cache — so a bearer the app rotated into the Keychain
/// is picked up without rebuilding the host. `notify_401` keeps the default no-op:
/// the next `bearer()` already re-reads the Keychain.
struct ForeignBearer(Arc<dyn FfiBearerProvider>);

#[async_trait]
impl BearerSource for ForeignBearer {
    async fn bearer(&self) -> Result<String, ApiError> {
        let token = self.0.current_bearer();
        if token.is_empty() {
            return Err(ApiError::Transport(
                "file provider host: no bearer provisioned in the app-group Keychain".into(),
            ));
        }
        Ok(token)
    }
}

/// The machine principal's **change-record signer**, in the form the app
/// provisions a capability host with (`mls-group-key-material.md` § M2 →
/// *Writer-signed change records* (1), *The capability host*): the store
/// device principal's writer secret and the canonical `EmbedAsBytes` bytes of
/// the root-signed `DeviceAuthorization` certifying it with `SyncWrite`. A
/// device key — neither the identity seed nor any MLS state.
#[derive(uniffi::Record, Clone)]
pub struct FfiChangeSignerCarriage {
    /// The principal writer key's 32-byte Ed25519 secret.
    pub writer_secret: Vec<u8>,
    /// The canonical `EmbedAsBytes` encoding of its `DeviceAuthorization`.
    pub device_authorization: Vec<u8>,
}

impl From<fauna_sync_engine::principal_bundle::ChangeSignerCarriage> for FfiChangeSignerCarriage {
    fn from(c: fauna_sync_engine::principal_bundle::ChangeSignerCarriage) -> Self {
        Self {
            writer_secret: c.writer_secret.to_vec(),
            device_authorization: c.device_authorization,
        }
    }
}

/// A source of a capability host's current [`FfiChangeSignerCarriage`], read
/// fresh at every write so the host never holds a key the app has since
/// re-minted. Two implementations, one per process shape: apple's
/// **out-of-process** extension gets a Swift one over the same app-group
/// Keychain as [`FfiBearerProvider`] — the app copies the carriage in beside
/// the bearer at provisioning and again on every principal-slot write
/// ([`machine_change_signer_carriage`], [`principal_slot_writes_after`]) —
/// and an **in-process** host (android's SAF `DocumentsProvider`) gets the
/// Rust [`SlotChangeSigner`], which reads the principal slot itself
/// ([`machine_change_signer`]). Sync — a secure-store read.
#[uniffi::export(with_foreign)]
pub trait FfiChangeSignerProvider: Send + Sync {
    /// The signer the app most recently wrote; `None` ⇒ none provisioned (this
    /// machine is not enrolled with `SyncWrite` yet) — the host then records
    /// nothing until one appears.
    fn current_signer(&self) -> Option<FfiChangeSignerCarriage>;
}

/// The **in-process** host's [`FfiChangeSignerProvider`] — android's SAF
/// provider runs in the app's own process (`on-demand-files.md` § Android SAF
/// DocumentsProvider binding, *Control inversion, in the app's own process*),
/// so its host reads the machine principal straight out of the app's principal
/// slot — the T10 slot the app's account runtime enrolls at every seed-holding
/// sign-in ([`fauna_sync_engine::principal_bundle`]; on the phones the app's own
/// secure store lent over the foreign seam, `apps/common.md` § Credential
/// storage → *The shared Rust credential slots on the phones*). Apple's
/// extension needs that carriage copied across a process boundary and re-copied
/// whenever the slot moves; here there is no boundary, so there is no copy and
/// no watch: a host built before the first enrollment completes finds the
/// signer at its next write or kept-root sweep by construction
/// (`mls-group-key-material.md` § M2 → *Writer-signed change records* (1),
/// *The capability host* — the android arm). The writer secret is read into
/// Rust and zeroized after each signing; it never crosses into the app's
/// managed memory.
pub struct SlotChangeSigner {
    credentials: fauna_credential_store::CredentialStore,
    actor_id: [u8; 32],
}

impl SlotChangeSigner {
    /// The slot reader for `actor_id` over `credentials` — the production
    /// export [`machine_change_signer`] hands it the app's production store;
    /// a headless proof hands it a file-backed one the ceremony wrote.
    pub fn over(
        credentials: fauna_credential_store::CredentialStore,
        actor_id: [u8; 32],
    ) -> Arc<Self> {
        Arc::new(Self {
            credentials,
            actor_id,
        })
    }
}

impl FfiChangeSignerProvider for SlotChangeSigner {
    fn current_signer(&self) -> Option<FfiChangeSignerCarriage> {
        fauna_sync_engine::principal_bundle::load_change_signer_carriage(
            &self.credentials,
            &self.actor_id,
        )
        .map(Into::into)
    }
}

/// This machine's change-record signer for `actor_id`, read from the app's
/// principal slot at every write — the [`FfiChangeSignerProvider`] an
/// in-process host is built with ([`FfiFileProviderHost::app_dead_owned_tree`]).
/// It carries `None` (the host holds every write in the kept root) until the
/// app's account runtime has enrolled this machine with `SyncWrite`, and needs
/// no re-provisioning when it does.
#[uniffi::export]
pub fn machine_change_signer(
    actor_id: Vec<u8>,
) -> Result<Arc<dyn FfiChangeSignerProvider>, FfiError> {
    let actor_id = bytes32(&actor_id, "actor_id")?;
    Ok(SlotChangeSigner::over(
        fauna_sync_engine::account_runtime::production_credential_store(),
        actor_id,
    ))
}

/// Adapts an [`FfiChangeSignerProvider`] to the engine's
/// [`ChangeSigner`](fauna_protocol::sync_writer_sig::ChangeSigner): read fresh
/// on every call — no cache — and checked through the shared delegation chain
/// before anything is signed with it.
struct ForeignSigner {
    provider: Arc<dyn FfiChangeSignerProvider>,
    actor_id: [u8; 32],
}

impl ForeignSigner {
    /// The signer every record this host writes now carries, or why there is
    /// none. There is no unsigned fallback: the nest refuses an unsigned record
    /// once it enforces, so a host with no signer holds the write here, loudly,
    /// rather than acknowledging a record that will not stand.
    fn current(&self) -> Result<Arc<fauna_protocol::sync_writer_sig::ChangeSigner>, String> {
        let carriage = self.provider.current_signer().ok_or_else(|| {
            "no change signer is provisioned (this machine's principal is not enrolled \
             with SyncWrite yet — the app's account runtime enrolls it at sign-in, and an \
             out-of-process host is handed it once it is): the write is held, never \
             recorded unsigned"
                .to_string()
        })?;
        let secret: [u8; 32] = carriage.writer_secret.as_slice().try_into().map_err(|_| {
            "the provisioned change signer's writer secret is not 32 bytes: the write is held"
                .to_string()
        })?;
        let secret = zeroize::Zeroizing::new(secret);
        fauna_protocol::sync_writer_sig::ChangeSigner::from_delegated_carriage(
            self.actor_id,
            &secret,
            &carriage.device_authorization,
        )
        .map(Arc::new)
        .map_err(|e| format!("the provisioned change signer is unusable ({e}): the write is held"))
    }
}

/// The app's side of the **out-of-process** provisioning: this machine's
/// change-record signer for `actor_id`, read from the principal slot the app's
/// own account runtime keeps
/// ([`fauna_sync_engine::principal_bundle::load_change_signer_carriage`]) —
/// `None` until the machine is enrolled with a `SyncWrite` grant. The apple app
/// writes it into the app-group Keychain beside the extension's bearer; an
/// in-process host reads the same slot itself ([`machine_change_signer`]).
#[uniffi::export]
pub fn machine_change_signer_carriage(
    actor_id: Vec<u8>,
) -> Result<Option<FfiChangeSignerCarriage>, FfiError> {
    let actor_id = bytes32(&actor_id, "actor_id")?;
    let credentials = fauna_sync_engine::account_runtime::production_credential_store();
    Ok(
        fauna_sync_engine::principal_bundle::load_change_signer_carriage(&credentials, &actor_id)
            .map(Into::into),
    )
}

/// Wait until this process has written a principal slot's writer key or grant
/// since the caller last saw `seen` ([`fauna_sync_engine::principal_bundle::slot_writes`]),
/// and return the new count — the app's cue to re-provision
/// [`machine_change_signer_carriage`], so a first enrollment, a `SyncWrite`
/// re-certification or a re-minted principal reaches the extension without
/// waiting for the next domain reconcile. Returns at once when the count
/// already differs from `seen` (pass `0` first). An in-process host needs no
/// such cue: its [`SlotChangeSigner`] reads the slot at every write.
#[fauna_uniffi_async::export]
pub async fn principal_slot_writes_after(seen: u64) -> u64 {
    let mut writes = fauna_sync_engine::principal_bundle::slot_writes();
    loop {
        let now = *writes.borrow_and_update();
        if now != seen {
            return now;
        }
        if writes.changed().await.is_err() {
            return now;
        }
    }
}

// ── Plane custody: the throwaway fleet replica (decision 1′) ────────────────
//
// `on-demand-files.md` § Shared sets on a capability host, decision 1′: once
// `fauna.state.folder-keys` is plane-only, a capability host reads it through a
// throwaway fleet-scope replica keyed as the enrolled device it is a process
// of — the machine principal's wraps, else the custody below. One replica per
// (process, account), shared by every host of that account in the process.

/// A capability host's **retained generation-key custody** — the generations
/// this device holds with no wrap on the plane reaching it (recovered from
/// escrow at a seed-holding sign-in; a top-up the reclaim pass retired), and
/// every generation the host unwraps itself. Two implementations, one per
/// process shape, like [`FfiChangeSignerProvider`]: apple's out-of-process
/// extension gets a Swift one over the app-group credential store, which the
/// app fills from the machine slot on every slot write
/// ([`machine_retained_generation_keys`], [`principal_slot_writes_after`]);
/// android's in-process provider gets the Rust slot reader
/// ([`machine_retained_key_custody`]). Installed per account with
/// [`install_capability_host_custody`]. Sync — a secure-store call.
///
/// Every generation id and key is 32 bytes. A key is recorded only after its
/// caller checked it against its mint's commitment, and a held key is checked
/// again at every use.
#[uniffi::export(with_foreign)]
pub trait FfiRetainedKeyCustody: Send + Sync {
    /// The key held for `generation`, or `None`.
    fn get(&self, generation: Vec<u8>) -> Option<Vec<u8>>;
    /// Hold `key` for `generation`. A generation already held keeps its key.
    fn put(&self, generation: Vec<u8>, key: Vec<u8>);
    /// Forget `generation` — its mint was shredded (the crypto-shred's
    /// device-side half).
    fn remove(&self, generation: Vec<u8>);
}

/// Adapts an [`FfiRetainedKeyCustody`] to the plane's custody seam. A value of
/// the wrong length answers as absent.
struct ForeignRetainedKeys(Arc<dyn FfiRetainedKeyCustody>);

impl fauna_sync_engine::generation_tip::RetainedKeyCustody for ForeignRetainedKeys {
    fn retained_generation_key(
        &self,
        generation: &[u8; 32],
    ) -> Option<fauna_core::crypto::GenerationKey> {
        let key = zeroize::Zeroizing::new(self.0.get(generation.to_vec())?);
        let key: [u8; 32] = key.as_slice().try_into().ok()?;
        Some(fauna_core::crypto::GenerationKey::from_bytes(key))
    }
    fn record_generation_key(
        &self,
        generation: &[u8; 32],
        key: &fauna_core::crypto::GenerationKey,
    ) {
        self.0.put(generation.to_vec(), key.as_bytes().to_vec());
    }
    fn drop_generation_key(&self, generation: &[u8; 32]) {
        self.0.remove(generation.to_vec());
    }
}

/// The **in-process** host's [`FfiRetainedKeyCustody`]: the machine slot's own
/// retained-key carriage, read fresh at every consult and written under the
/// account store's section, exactly as the app's runtime writes it
/// ([`fauna_sync_engine::principal_bundle::SlotRetainedKeys`]).
pub struct SlotRetainedKeyCustody(fauna_sync_engine::principal_bundle::SlotRetainedKeys);

impl FfiRetainedKeyCustody for SlotRetainedKeyCustody {
    fn get(&self, generation: Vec<u8>) -> Option<Vec<u8>> {
        use fauna_sync_engine::generation_tip::RetainedKeyCustody;
        let generation: [u8; 32] = generation.as_slice().try_into().ok()?;
        self.0
            .retained_generation_key(&generation)
            .map(|k| k.as_bytes().to_vec())
    }
    fn put(&self, generation: Vec<u8>, key: Vec<u8>) {
        use fauna_sync_engine::generation_tip::RetainedKeyCustody;
        let key = zeroize::Zeroizing::new(key);
        let (Ok(generation), Ok(key)) = (
            <[u8; 32]>::try_from(generation.as_slice()),
            <[u8; 32]>::try_from(key.as_slice()),
        ) else {
            return;
        };
        self.0.record_generation_key(
            &generation,
            &fauna_core::crypto::GenerationKey::from_bytes(key),
        );
    }
    fn remove(&self, generation: Vec<u8>) {
        use fauna_sync_engine::generation_tip::RetainedKeyCustody;
        if let Ok(generation) = <[u8; 32]>::try_from(generation.as_slice()) {
            self.0.drop_generation_key(&generation);
        }
    }
}

/// This machine's retained-key custody for `actor_id`, over the app's own
/// principal slot — the [`FfiRetainedKeyCustody`] an in-process host installs.
/// `store_container_dir` is the account-store container the app's runtime is
/// started with: its per-account store dir is the section a record serializes
/// under, beside the runtime's own.
#[uniffi::export]
pub fn machine_retained_key_custody(
    actor_id: Vec<u8>,
    store_container_dir: String,
) -> Result<Arc<dyn FfiRetainedKeyCustody>, FfiError> {
    let actor_id = bytes32(&actor_id, "actor_id")?;
    let actor_hex = fauna_core::hex32::encode(&actor_id);
    let store_dir = fauna_sync_engine::root::StoreRoot::at(PathBuf::from(store_container_dir))
        .store_dir(&actor_hex)
        .map_err(|e| FfiError::General {
            msg: format!("the account store dir: {e:#}"),
        })?;
    Ok(Arc::new(SlotRetainedKeyCustody(
        fauna_sync_engine::principal_bundle::SlotRetainedKeys::over(
            Arc::new(fauna_sync_engine::account_runtime::production_credential_store()),
            actor_hex,
            store_dir,
        ),
    )))
}

/// One generation key of the machine slot's retained-key carriage.
#[derive(uniffi::Record)]
pub struct FfiRetainedGenerationKey {
    /// The generation id.
    pub generation: Vec<u8>,
    /// Its 32-byte key.
    pub key: Vec<u8>,
}

/// The app's side of the **out-of-process** custody: every generation key the
/// machine slot carries for `actor_id`, read fresh — what the apple app puts
/// into the extension's app-group custody at provisioning and again whenever
/// [`principal_slot_writes_after`] moves (a carriage write moves it too). The
/// extension records its own unwraps beside these; a generation the slot drops
/// on a shred is dropped by the extension itself when it reads the shredded
/// mint.
#[uniffi::export]
pub fn machine_retained_generation_keys(
    actor_id: Vec<u8>,
) -> Result<Vec<FfiRetainedGenerationKey>, FfiError> {
    let actor_id = bytes32(&actor_id, "actor_id")?;
    let credentials = fauna_sync_engine::account_runtime::production_credential_store();
    Ok(
        fauna_sync_engine::principal_bundle::retained_generation_keys(
            &credentials,
            &fauna_core::hex32::encode(&actor_id),
        )
        .into_iter()
        .map(|(generation, key)| FfiRetainedGenerationKey {
            generation: generation.to_vec(),
            key: key.to_vec(),
        })
        .collect(),
    )
}

/// A per-account registry of this process, keyed by actor id.
type PerAccount<T> = std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<[u8; 32], T>>>;

/// The custody each account's capability hosts in this process read with.
static HOST_CUSTODY: PerAccount<Arc<dyn FfiRetainedKeyCustody>> =
    std::sync::LazyLock::new(Default::default);

/// Install the retained-key custody this process's capability hosts of
/// `actor_id` read with: apple's extension its Swift app-group custody,
/// android's provider [`machine_retained_key_custody`]. Replaces any earlier
/// one; a replica already built keeps the custody it was built with until the
/// machine principal changes.
#[uniffi::export]
pub fn install_capability_host_custody(
    actor_id: Vec<u8>,
    custody: Arc<dyn FfiRetainedKeyCustody>,
) -> Result<(), FfiError> {
    let actor_id = bytes32(&actor_id, "actor_id")?;
    HOST_CUSTODY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(actor_id, custody);
    Ok(())
}

/// A capability host's [`fauna_client_folders::FolderKeyReader`]: every load
/// asks [`capability_host_folder_keys`] for the account's replica (built at the
/// first ask, rebuilt on a principal re-mint) and loads through it. A host
/// started before its machine principal was provisioned reads custody once the
/// app enrolls it; until then — and on any other failure — a load answers the
/// reason: custody unreadable, so a bound set builds keyless, never plaintext.
pub(crate) struct CapabilityHostFolderKeys {
    pub(crate) auth: Arc<fauna_client::AuthClient>,
    pub(crate) actor_id: [u8; 32],
    pub(crate) backup_key: fauna_core::crypto::BackupKey,
    pub(crate) signer: Arc<dyn FfiChangeSignerProvider>,
}

#[async_trait::async_trait]
impl fauna_client_folders::FolderKeyReader for CapabilityHostFolderKeys {
    async fn load(&self) -> anyhow::Result<fauna_core::data::FoldersConfig> {
        let reader =
            capability_host_folder_keys(&self.auth, self.actor_id, &self.backup_key, &*self.signer)
                .map_err(anyhow::Error::msg)?;
        reader.load().await
    }
}

/// What a replica was built from beside its account and nest: the machine
/// principal it keys by and the `BackupKey` its schedule derives from. A host
/// asking with either changed gets a fresh replica, never one another's
/// inputs keyed.
struct ReplicaInputs {
    writer_pub: [u8; 32],
    backup_key: fauna_core::crypto::BackupKey,
}

/// Each (account, nest) replica in this process, with the inputs it was built
/// from.
static HOST_REPLICAS: std::sync::LazyLock<std::sync::Mutex<ReplicaRegistry>> =
    std::sync::LazyLock::new(Default::default);

/// (account, nest URL) → the replica and what it was built from.
type ReplicaRegistry = std::collections::HashMap<
    ([u8; 32], String),
    (
        ReplicaInputs,
        fauna_sync_engine::cold_folder_keys::ColdReplicaFolderKeys,
    ),
>;

/// The folder-key reader every capability host of `actor_id` on one nest in
/// this process shares — its throwaway fleet replica, built at the first ask
/// over its own connection from `auth` (the host's bearer source, whose nest
/// URL is half the registry key), the account's `backup_key`, the machine
/// principal `signer` carries (the device arm's unwrap key) and the custody
/// [`install_capability_host_custody`] installed (none installed: an in-memory
/// one, so the host keys through the plane's wraps alone). Rebuilt when the
/// machine principal is re-minted or the `BackupKey` differs; otherwise held
/// for the process's life.
///
/// # Errors
/// No machine principal is provisioned yet (the device arm has no key), or the
/// replica's worker could not start.
pub fn capability_host_folder_keys(
    auth: &Arc<fauna_client::AuthClient>,
    actor_id: [u8; 32],
    backup_key: &fauna_core::crypto::BackupKey,
    signer: &dyn FfiChangeSignerProvider,
) -> Result<fauna_sync_engine::cold_folder_keys::ColdReplicaFolderKeys, String> {
    let carriage = signer.current_signer().ok_or_else(|| {
        "no machine principal is provisioned: the host cannot read the account's plane \
         custody as this device until the app enrolls it"
            .to_string()
    })?;
    let secret: [u8; 32] = carriage.writer_secret.as_slice().try_into().map_err(|_| {
        "the provisioned machine principal's writer secret is not 32 bytes".to_string()
    })?;
    let writer_key = ed25519_dalek::SigningKey::from_bytes(&zeroize::Zeroizing::new(secret));
    let writer_pub = writer_key.verifying_key().to_bytes();
    let registry_key = (actor_id, auth.nest_url());
    let mut replicas = HOST_REPLICAS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((built_from, reader)) = replicas.get(&registry_key)
        && built_from.writer_pub == writer_pub
        && built_from.backup_key.to_bytes() == backup_key.to_bytes()
    {
        return Ok(reader.clone());
    }
    let custody: Arc<dyn fauna_sync_engine::generation_tip::RetainedKeyCustody> = match HOST_CUSTODY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&actor_id)
    {
        Some(custody) => Arc::new(ForeignRetainedKeys(custody.clone())),
        None => {
            tracing::warn!(
                "no retained-key custody is installed for this account's capability hosts — \
                 the replica keys through the plane's wraps alone"
            );
            Arc::new(fauna_sync_engine::cold_replica::MemoryRetainedKeys::default())
        }
    };
    let reader = fauna_sync_engine::cold_folder_keys::ColdReplicaFolderKeys::spawn(
        fauna_client::SelfConnecting(fauna_client::NestClient::with_auth(Arc::clone(auth))),
        fauna_core::identity::ActorId(actor_id),
        backup_key.clone(),
        fauna_sync_engine::cold_replica::ColdKeySource::Device {
            writer_key: Box::new(writer_key),
            custody,
        },
    )
    .map_err(|e| format!("{e:#}"))?;
    replicas.insert(
        registry_key,
        (
            ReplicaInputs {
                writer_pub,
                backup_key: backup_key.clone(),
            },
            reader.clone(),
        ),
    );
    Ok(reader)
}

/// Every set the account holds, as the shared presence plan takes it
/// ([`crate::on_demand_presence_plan`]'s `sets`) — the account's own folders
/// and the folders shared with it, on this nest or another
/// (`on-demand-files.md` § Shared sets on a capability host, decision 3). The
/// one mapping every set-level on-demand platform calls instead of building
/// `PresenceSet`s itself; the rules are
/// [`fauna_folders_machine::on_demand_presence::held_sets`]'s.
///
/// It reads what the capability hosts read: the member-visible folder list and
/// each OWN folder's device roster through `folders`, and the account's
/// folder-key custody through the capability host's own reader
/// ([`capability_host_folder_keys`] — the replica the hosts of this account
/// share, so the plan lists exactly the sets a host can key). `nest_url`,
/// `backup_key`, `bearer` and `signer` are the values the hosts are built
/// with.
///
/// # Errors
/// Any read failed — the list, a roster, or custody (no machine principal is
/// enrolled yet, or the plane could not be walked). An error is never an
/// empty answer: the caller skips the reconcile rather than tearing down
/// presences on a state it could not see.
#[cfg(feature = "folders")]
#[fauna_uniffi_async::export]
pub async fn capability_host_presence_sets(
    folders: Arc<crate::FfiFoldersClient>,
    nest_url: String,
    actor_id: Vec<u8>,
    device_id: Vec<u8>,
    backup_key: Vec<u8>,
    bearer: Arc<dyn FfiBearerProvider>,
    signer: Arc<dyn FfiChangeSignerProvider>,
) -> Result<Vec<fauna_folders_machine::on_demand_presence::PresenceSet>, FfiError> {
    let actor_id = bytes32(&actor_id, "actor_id")?;
    let device_id_hex = hex::encode(bytes32(&device_id, "device_id")?);
    let backup_key = fauna_core::crypto::BackupKey::from_bytes(bytes32(&backup_key, "backup_key")?);

    let http = fauna_client::pinned_http_client(&nest_url);
    let auth = Arc::new(fauna_client::AuthClient::bearer_only(
        nest_url,
        actor_id,
        Arc::new(ForeignBearer(bearer)),
        http,
    ));
    let custody = capability_host_folder_keys(&auth, actor_id, &backup_key, &*signer)
        .map_err(|msg| FfiError::General { msg })?;
    crate::folders_client::presence_sets_over(&folders.client(), &device_id_hex, &custody).await
}

/// One enumerated File Provider item — the projection of a tracked row (or a
/// synthesized directory) the appex maps onto an `NSFileProviderItem`. `rel` is the
/// item identifier; `content_version` is the `NSFileProviderItemVersion.contentVersion`
/// bytes.
#[derive(uniffi::Record)]
pub struct FfiFileProviderItem {
    /// Full forward-slash, folder-relative path (the item identifier).
    pub rel: String,
    /// Final path component (the `filename`).
    pub name: String,
    pub size_bytes: i64,
    /// Unix seconds.
    pub mtime: i64,
    pub is_dir: bool,
    /// FP `contentVersion` bytes (empty for a directory).
    pub content_version: Vec<u8>,
}

impl From<provider_face::ProviderItem> for FfiFileProviderItem {
    fn from(i: provider_face::ProviderItem) -> Self {
        FfiFileProviderItem {
            rel: i.rel,
            name: i.name,
            size_bytes: i.size as i64,
            mtime: i.mtime,
            is_dir: i.is_dir,
            content_version: i.content_version,
        }
    }
}

/// The result of [`FfiFileProviderHost::fetch`] — the materialized bytes plus the
/// item's new `contentVersion` (the served content's hash). The appex writes the
/// bytes to the temp URL the OS asked for and hands back an item carrying this
/// version.
#[derive(uniffi::Record)]
pub struct FfiFileProviderContent {
    pub bytes: Vec<u8>,
    pub content_version: Vec<u8>,
}

/// Whether a local write reached the nest **and** stamped the row
/// (`UploadOutcome::recorded`). The appex acks the OS **only** when `acked`, so the
/// OS never discards an un-acked local copy (`file-sync.md` § Apple File Provider
/// binding, *recorded-head gate*).
#[derive(uniffi::Record)]
pub struct FfiFileProviderAck {
    pub acked: bool,
    /// The row moved to a head the OS's on-disk bytes are NOT (a conflicted
    /// ingest resolved to a merge / incoming winner). The appex passes this as
    /// `modifyItem`'s `shouldFetchContent`, so the OS re-fetches the winner
    /// instead of associating its loser bytes with the winning version.
    pub content_changed: bool,
    /// The rel is ignored (dotfile component, built-in default ignore, or
    /// `.faunaignore` pattern) and was deliberately NOT ingested — no row, no
    /// upload, no change record. The appex maps this to
    /// `NSFileProviderError.excludedFromSync` so the OS keeps the local file
    /// but stops syncing it (`file-sync.md` § Built-in default ignores).
    pub excluded: bool,
}

impl From<provider_face::WriteAck> for FfiFileProviderAck {
    fn from(a: provider_face::WriteAck) -> Self {
        FfiFileProviderAck {
            acked: a.acked,
            content_changed: a.content_changed,
            excluded: a.excluded,
        }
    }
}

/// What [`FfiFileProviderHost::open_for_write`] hands back: the kept-root path
/// to write, and the `contentVersion` the open saw — passed back to
/// [`FfiFileProviderHost::closed_write`] so a head that moved meanwhile takes
/// the shared conflict auto-resolve.
#[derive(uniffi::Record)]
pub struct FfiOwnedWriteOpen {
    pub path: String,
    pub base_content_version: Vec<u8>,
}

/// What one [`FfiFileProviderHost::sweep_kept_root`] did.
#[derive(uniffi::Record)]
pub struct FfiOwnedSweepReport {
    /// Kept-root bodies whose change the nest recorded.
    pub recorded: Vec<String>,
    /// Kept-root bodies still waiting for the next sweep.
    pub pending: Vec<String>,
    /// Hydrated rows whose body was gone, now placeholders again.
    pub evicted: Vec<String>,
}

/// Which of an owned tree's two roots a landed body goes to
/// ([`FfiFileProviderHost::land_body`]).
#[derive(uniffi::Enum)]
pub enum FfiBodyRoot {
    /// A body the nest may not hold yet — the next sweep ingests it.
    Kept,
    /// A body the nest holds — the OS may reclaim it.
    Cache,
}

impl From<FfiBodyRoot> for BodyRoot {
    fn from(r: FfiBodyRoot) -> Self {
        match r {
            FfiBodyRoot::Kept => BodyRoot::Kept,
            FfiBodyRoot::Cache => BodyRoot::Cache,
        }
    }
}

/// One request to the host's engine worker.
enum Request {
    Enumerate {
        parent_rel: String,
        reply: oneshot::Sender<Result<Vec<FfiFileProviderItem>, String>>,
    },
    Item {
        rel: String,
        reply: oneshot::Sender<Result<Option<FfiFileProviderItem>, String>>,
    },
    Fetch {
        rel: String,
        reply: oneshot::Sender<Result<FfiFileProviderContent, String>>,
    },
    FetchToPath {
        rel: String,
        dest_path: String,
        reply: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    Ingest {
        rel: String,
        reply: oneshot::Sender<Result<FfiFileProviderAck, String>>,
    },
    IngestWithBase {
        rel: String,
        base_content_version: Vec<u8>,
        reply: oneshot::Sender<Result<FfiFileProviderAck, String>>,
    },
    Delete {
        rel: String,
        reply: oneshot::Sender<Result<FfiFileProviderAck, String>>,
    },
    Rename {
        from_rel: String,
        to_rel: String,
        reply: oneshot::Sender<Result<FfiFileProviderAck, String>>,
    },
    IsIgnored {
        rel: String,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    Evict {
        rel: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    HydratedRels {
        reply: oneshot::Sender<Result<Vec<String>, String>>,
    },
    Refresh {
        reply: oneshot::Sender<Result<bool, String>>,
    },
    ReadOnly {
        reply: oneshot::Sender<Result<bool, String>>,
    },
    // ---- the owned-tree operations ----
    OpenForRead {
        rel: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    OpenForWrite {
        rel: String,
        reply: oneshot::Sender<Result<FfiOwnedWriteOpen, String>>,
    },
    ClosedWrite {
        rel: String,
        base_content_version: Vec<u8>,
        reply: oneshot::Sender<Result<FfiFileProviderAck, String>>,
    },
    CreateDocument {
        rel: String,
        reply: oneshot::Sender<Result<FfiFileProviderAck, String>>,
    },
    DeleteDocument {
        rel: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RenameDocument {
        from_rel: String,
        to_rel: String,
        reply: oneshot::Sender<Result<FfiFileProviderAck, String>>,
    },
    SweepKeptRoot {
        reply: oneshot::Sender<Result<FfiOwnedSweepReport, String>>,
    },
    ObserveEvictions {
        reply: oneshot::Sender<Result<Vec<String>, String>>,
    },
    LookupBody {
        rel: String,
        reply: oneshot::Sender<Result<Option<String>, String>>,
    },
    LandBody {
        rel: String,
        src_path: String,
        root: FfiBodyRoot,
        reply: oneshot::Sender<Result<String, String>>,
    },
    RecordPlaceholder {
        rel: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    // ---- the share plane's two operations ----
    #[cfg(feature = "p2p-share")]
    ShareReplica {
        reply:
            oneshot::Sender<Result<Option<fauna_sync_engine::share_glue::OnDemandReplica>, String>>,
    },
    #[cfg(feature = "p2p-share")]
    ShareIngest {
        proven_actor_hex: String,
        rows: Vec<Vec<u8>>,
        spool_dir: String,
        reply: oneshot::Sender<Result<FfiShareIngestOutcome, String>>,
    },
}

/// What one [`FfiFileProviderHost::share_ingest`] did — the engine's own
/// ingest report plus the authoritative pull cursor.
#[cfg(feature = "p2p-share")]
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiShareIngestOutcome {
    pub refused: u32,
    pub overlaid: u32,
    pub materialized: u32,
    pub already_current: u32,
    /// The per-peer pull cursor after the page — the pump's next `since`.
    pub cursor: i64,
    /// A wanted body was not landed because that would take the device's
    /// free space under the storage floor.
    pub storage_limited: bool,
}

/// The process's on-demand hosts that own their tree, by account and set
/// (`FolderRef` wire form) — the registry the platform's provider and the
/// share plane both reach (`p2p-shared-set-build.md` § *Phone peers —
/// design*, decision 1: one host per set, one writer per set).
///
/// Held `Weak`: a host lives exactly as long as whoever built it holds it
/// (the provider drops its hosts at sign-out, switch and toggle-off), and the
/// registry never keeps a worker alive past that. The most recently built
/// host for a set is the registered one.
type OnDemandHostKey = ([u8; 32], String);
static ON_DEMAND_HOSTS: std::sync::LazyLock<
    std::sync::Mutex<
        std::collections::HashMap<OnDemandHostKey, std::sync::Weak<FfiFileProviderHost>>,
    >,
> = std::sync::LazyLock::new(Default::default);

fn register_on_demand_host(actor_id: [u8; 32], folder_id: String, host: &Arc<FfiFileProviderHost>) {
    let mut hosts = ON_DEMAND_HOSTS.lock().unwrap_or_else(|e| e.into_inner());
    hosts.retain(|_, held| held.strong_count() > 0);
    hosts.insert((actor_id, folder_id), Arc::downgrade(host));
}

/// Every live registered host of `actor_id`, in set order.
#[cfg(feature = "p2p-share")]
fn on_demand_hosts_of(actor_id: [u8; 32]) -> Vec<Arc<FfiFileProviderHost>> {
    let hosts = ON_DEMAND_HOSTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut live: Vec<(&String, Arc<FfiFileProviderHost>)> = hosts
        .iter()
        .filter(|((actor, _), _)| *actor == actor_id)
        .filter_map(|((_, folder_id), held)| Some((folder_id, held.upgrade()?)))
        .collect();
    live.sort_by(|a, b| a.0.cmp(b.0));
    live.into_iter().map(|(_, host)| host).collect()
}

/// The live on-demand host this process holds for one set of one account, if
/// any — so a provider asks the registry before it builds a second host over
/// a set that already has its one writer. `folder_id` is the set's
/// `FolderRef` wire string.
#[uniffi::export]
pub fn on_demand_host(
    actor_id: Vec<u8>,
    folder_id: String,
) -> Result<Option<Arc<FfiFileProviderHost>>, FfiError> {
    let actor_id = bytes32(&actor_id, "actor_id")?;
    let folder_id = parse_folder_id(&folder_id)?.to_wire();
    Ok(ON_DEMAND_HOSTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(actor_id, folder_id))
        .and_then(std::sync::Weak::upgrade))
}

/// The per-domain File Provider host handed to the extension.
///
/// Drop semantics: dropping the handle closes the request channel, so the worker
/// loop exits and its runtime — and the resident engine's `SyncDb` connection,
/// opened on that thread — shuts down there (what rusqlite wants).
#[derive(uniffi::Object)]
pub struct FfiFileProviderHost {
    requests: mpsc::Sender<Request>,
}

impl FfiFileProviderHost {
    /// Rust-side constructor (the uniffi constructors call this). Spawns the
    /// worker that builds the set-scoped engine over `root_dir` and holds it
    /// resident; with a `cache_root` the host also owns its tree, `root_dir`
    /// being the kept root.
    fn start(
        ctx: HostContext,
        signer: ForeignSigner,
        folder_ref: FolderRef,
        root_dir: PathBuf,
        cache_root: Option<PathBuf>,
    ) -> Arc<Self> {
        let requests = crate::worker_thread::spawn_worker_thread(
            "fauna-fp-host",
            "file-provider",
            32,
            move |requests| engine_worker(ctx, signer, folder_ref, root_dir, cache_root, requests),
        );
        Arc::new(Self { requests })
    }

    /// Send a request to the worker and await its reply, mapping a dead worker onto
    /// an [`FfiError`].
    async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, String>>) -> Request,
    ) -> Result<T, FfiError> {
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(make(tx))
            .await
            .map_err(|_| FfiError::General {
                msg: "file provider host is not running".into(),
            })?;
        rx.await
            .map_err(|_| FfiError::General {
                msg: "file provider host stopped before replying".into(),
            })?
            .map_err(|msg| FfiError::General { msg })
    }
}

#[uniffi::export]
impl FfiFileProviderHost {
    /// Build the **app-dead** File Provider host for one folder — the
    /// per-domain, out-of-app hydration host an `NSFileProviderReplicatedExtension`
    /// runs (`file-sync.md` § Apple File Provider binding, *the extension hosts the
    /// engines*). It is provisioned a pre-derived owner `BackupKey` + a Swift
    /// `bearer` provider (which reads the shared app-group Keychain) and **never the
    /// identity seed**; it builds its own bearer-authenticated nest connection and
    /// holds one set-scoped engine over `root_dir` (the domain's on-disk
    /// location under `~/Library/CloudStorage/`), vending the callback→engine
    /// primitives (enumerate / item / fetch / evict for the M2 read path; ingest /
    /// delete / rename land in M3).
    ///
    /// `folder_id` is the set's `FolderRef` wire string
    /// ([`crate::sync_engine_host::parse_folder_id`]) — the domain's identity,
    /// which the appex takes from the domain it was constructed for. A bare set
    /// name is refused: the set's state DB and its row are keyed by the ref
    /// (`on-demand-files.md` § Hosting multiple on-demand folders). A
    /// `foreign:<channel>` ref — a set shared with the account from another nest
    /// — builds from the account's custody record alone, its byte plane at the
    /// set's home nest (`on-demand-files.md` § Shared sets on a capability host →
    /// *One mechanism*, question 2); whether a presence plan hands one over is
    /// the shared-with-me arm's call (decision 3), not this host's.
    ///
    /// `signer` is the machine principal's change-record signer, re-read at every
    /// write (the pre-seal edge): every record the host writes is signed with it
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records* (1),
    /// *The capability host*), and a write that finds none is held with the
    /// reason — never recorded unsigned. Reads never need it.
    ///
    /// Fail-closed by construction: 32-byte length checks on `actor_id` /
    /// `device_id` / `backup_key`; an owner-only set decrypts under the `BackupKey`,
    /// and a set's nonce and a bound set's content keys load from this
    /// account's plane custody, read through the throwaway fleet replica
    /// [`capability_host_folder_keys`] keys by the machine principal `signer`
    /// carries (`on-demand-files.md` § Shared sets on a capability host,
    /// decision 1′). A host with no principal yet reads no custody — a
    /// nonce'd set then lists nothing — and rebuilds at the first refresh or
    /// seal that finds one. Custody without the generation a body or the
    /// floor names fails that operation closed; it never falls back to
    /// plaintext or an older generation.
    ///
    /// `predecessor_chain` is the account's attested predecessors paired with
    /// their retired owner keys (`FfiAccountRegistry::predecessor_chain`, as
    /// the identity-holding app provisioned it beside `backup_key`): a row a
    /// retired identity signed then opens under that identity's own root
    /// (`writer-signed-change-records.md` ruling (8)(c)). `None` — a host
    /// provisioned none — still verifies such a row as this account's, by the
    /// statement walk ending at `actor_id` (ruling (8)(b), source (ii)), and
    /// opens it under no retired root.
    #[uniffi::constructor(default(predecessor_chain = None))]
    #[allow(clippy::too_many_arguments)]
    pub fn app_dead(
        nest_url: String,
        actor_id: Vec<u8>,
        device_id: Vec<u8>,
        device_label: String,
        backup_key: Vec<u8>,
        bearer: Arc<dyn FfiBearerProvider>,
        signer: Arc<dyn FfiChangeSignerProvider>,
        state_dir: String,
        folder_id: String,
        root_dir: String,
        predecessor_chain: Option<crate::accounts_registry::FfiPredecessorChain>,
    ) -> Result<Arc<Self>, FfiError> {
        let folder_ref = parse_folder_id(&folder_id)?;
        let actor_id = bytes32(&actor_id, "actor_id")?;
        let device_id = bytes32(&device_id, "device_id")?;
        let backup_key = bytes32(&backup_key, "backup_key")?;
        let ctx = file_provider_host_context(
            nest_url,
            actor_id,
            device_id,
            device_label,
            backup_key,
            Arc::new(ForeignBearer(bearer)),
            Arc::clone(&signer),
            state_dir,
            predecessor_chain
                .map(|chain| chain.seal_keys())
                .transpose()?
                .unwrap_or_default(),
        );
        let signer = ForeignSigner {
            provider: signer,
            actor_id,
        };
        Ok(Self::start(
            ctx,
            signer,
            folder_ref,
            PathBuf::from(root_dir),
            None,
        ))
    }

    /// Build the host for one set whose provider **owns its tree** — android's
    /// SAF `DocumentsProvider` (`on-demand-files.md` § Android SAF
    /// DocumentsProvider binding). The same capability construction as
    /// [`Self::app_dead`] (the owner `BackupKey` + a bearer source + a change
    /// signer source, never the identity seed — an in-process provider passes
    /// [`machine_change_signer`], the slot reader), but the roots are derived
    /// here, not passed: the kept
    /// root `<files_dir>/on-demand/<actor-id-hex>/<ref>/` (the engine's
    /// `root_dir`) and the cache root under `cache_dir` in the same grammar
    /// ([`OwnedTree::for_set`]). Once the set is populated the host runs the
    /// start sweep: every kept-root body a dead process left is ingested.
    /// `predecessor_chain` as on [`Self::app_dead`].
    #[uniffi::constructor(default(predecessor_chain = None))]
    #[allow(clippy::too_many_arguments)]
    pub fn app_dead_owned_tree(
        nest_url: String,
        actor_id: Vec<u8>,
        device_id: Vec<u8>,
        device_label: String,
        backup_key: Vec<u8>,
        bearer: Arc<dyn FfiBearerProvider>,
        signer: Arc<dyn FfiChangeSignerProvider>,
        state_dir: String,
        folder_id: String,
        files_dir: String,
        cache_dir: String,
        predecessor_chain: Option<crate::accounts_registry::FfiPredecessorChain>,
    ) -> Result<Arc<Self>, FfiError> {
        let folder_ref = parse_folder_id(&folder_id)?;
        let actor_id = bytes32(&actor_id, "actor_id")?;
        let device_id = bytes32(&device_id, "device_id")?;
        let backup_key = bytes32(&backup_key, "backup_key")?;
        let tree = OwnedTree::for_set(
            std::path::Path::new(&files_dir),
            std::path::Path::new(&cache_dir),
            &folder_ref.scoped_to(actor_id),
        );
        for root in [tree.kept_root(), tree.cache_root()] {
            std::fs::create_dir_all(root).map_err(|e| FfiError::General {
                msg: format!("creating the owned tree's root {}: {e}", root.display()),
            })?;
        }
        let ctx = file_provider_host_context(
            nest_url,
            actor_id,
            device_id,
            device_label,
            backup_key,
            Arc::new(ForeignBearer(bearer)),
            Arc::clone(&signer),
            state_dir,
            predecessor_chain
                .map(|chain| chain.seal_keys())
                .transpose()?
                .unwrap_or_default(),
        );
        let signer = ForeignSigner {
            provider: signer,
            actor_id,
        };
        let host = Self::start(
            ctx,
            signer,
            folder_ref,
            tree.kept_root().to_path_buf(),
            Some(tree.cache_root().to_path_buf()),
        );
        register_on_demand_host(actor_id, folder_ref.to_wire(), &host);
        Ok(host)
    }
}

#[cfg(feature = "p2p-share")]
impl FfiFileProviderHost {
    async fn share_ingest_page(
        &self,
        proven_actor_hex: String,
        rows: Vec<Vec<u8>>,
        spool_dir: String,
    ) -> Result<FfiShareIngestOutcome, FfiError> {
        self.call(|reply| Request::ShareIngest {
            proven_actor_hex,
            rows,
            spool_dir,
            reply,
        })
        .await
    }
}

#[cfg(feature = "p2p-share")]
#[fauna_uniffi_async::export]
impl FfiFileProviderHost {
    /// The share plane's ingest door on this replica — the on-demand twin of
    /// the sync agent's `ShareIngest` (`p2p-shared-set-build.md` § *Phone
    /// peers — design*, decision 1). `rows` are one page of accepted peer
    /// rows (canonical dag-cbor `PeerShareChange`s), `spool_dir` holds the
    /// bodies the pump pre-fetched (`manifests/<hex>` + `chunks/<hex>`), and
    /// `proven_actor_hex` is the peer the channel proved. Every accepted row
    /// is recorded; a body lands only when the landing policy wants it, in
    /// the kept root. Provenance is re-judged here: the host never trusts
    /// that the pump screened. Refused on a host that owns no tree.
    ///
    /// android reaches this through the plane in-process; iOS's app calls it
    /// across to its File Provider extension, the set's one writer.
    pub async fn share_ingest(
        &self,
        proven_actor_hex: String,
        rows: Vec<Vec<u8>>,
        spool_dir: String,
    ) -> Result<FfiShareIngestOutcome, FfiError> {
        self.share_ingest_page(proven_actor_hex, rows, spool_dir)
            .await
    }
}

/// The plane's view of this host: the replica it serves, and its door.
#[cfg(feature = "p2p-share")]
#[async_trait]
impl fauna_sync_engine::share_glue::OnDemandReplicaHost for FfiFileProviderHost {
    async fn replica(&self) -> Option<fauna_sync_engine::share_glue::OnDemandReplica> {
        self.call(|reply| Request::ShareReplica { reply })
            .await
            .ok()
            .flatten()
    }

    async fn share_ingest(
        &self,
        proven_actor_hex: &str,
        rows: Vec<Vec<u8>>,
        spool_dir: &std::path::Path,
    ) -> anyhow::Result<fauna_sync_engine::share_pump::ShareIngestSummary> {
        let outcome = self
            .share_ingest_page(
                proven_actor_hex.to_string(),
                rows,
                spool_dir.to_string_lossy().into_owned(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(fauna_sync_engine::share_pump::ShareIngestSummary {
            refused: outcome.refused,
            overlaid: outcome.overlaid,
            materialized: outcome.materialized,
            already_current: outcome.already_current,
            cursor: outcome.cursor,
            storage_limited: outcome.storage_limited,
        })
    }
}

/// One account's registered hosts, read afresh at every pass of the plane.
#[cfg(feature = "p2p-share")]
struct ActorOnDemandHosts([u8; 32]);

#[cfg(feature = "p2p-share")]
impl fauna_sync_engine::share_glue::OnDemandHosts for ActorOnDemandHosts {
    fn hosts(&self) -> Vec<Arc<dyn fauna_sync_engine::share_glue::OnDemandReplicaHost>> {
        on_demand_hosts_of(self.0)
            .into_iter()
            .map(|host| host as Arc<dyn fauna_sync_engine::share_glue::OnDemandReplicaHost>)
            .collect()
    }
}

/// The share plane's replica access over `actor_id`'s registered on-demand
/// hosts — the on-demand construction, beside the agent's.
#[cfg(feature = "p2p-share")]
pub(crate) fn on_demand_share_access_for(
    actor_id: [u8; 32],
) -> fauna_sync_engine::share_glue::ReplicaAccess {
    fauna_sync_engine::share_glue::on_demand_share_access(Arc::new(ActorOnDemandHosts(actor_id)))
}

#[fauna_uniffi_async::export]
impl FfiFileProviderHost {
    /// enumerate container → the immediate children of `parent_rel` (`""` = root).
    pub async fn enumerate(
        &self,
        parent_rel: String,
    ) -> Result<Vec<FfiFileProviderItem>, FfiError> {
        self.call(|reply| Request::Enumerate { parent_rel, reply })
            .await
    }

    /// `item(for:)` → the metadata for one identifier (file, directory, or `None`).
    pub async fn item(&self, rel: String) -> Result<Option<FfiFileProviderItem>, FfiError> {
        self.call(|reply| Request::Item { rel, reply }).await
    }

    /// `fetchContents` → the materialized bytes + the new `contentVersion`
    /// (`download_file_bytes` + `mark_hydrated`). Prefer [`Self::fetch_to_path`]
    /// — this whole-buffer variant remains for small reads (`stagedOrFetchedBytes`)
    /// but a large file must not cross UniFFI as one `Vec<u8>`.
    pub async fn fetch(&self, rel: String) -> Result<FfiFileProviderContent, FfiError> {
        self.call(|reply| Request::Fetch { rel, reply }).await
    }

    /// `fetchContents` → the file's plaintext written to `dest_path` with
    /// bounded memory (windowed fetch → decrypt → append; the appex hands the
    /// OS-provided temp URL). Returns the new `contentVersion` (the verified
    /// whole-file hash). The preferred serving core — an iOS appex runs under a
    /// hard memory cap, and even on macOS a large file should not cross UniFFI
    /// as one buffer. On failure the partial file is removed.
    pub async fn fetch_to_path(&self, rel: String, dest_path: String) -> Result<Vec<u8>, FfiError> {
        self.call(|reply| Request::FetchToPath {
            rel,
            dest_path,
            reply,
        })
        .await
    }

    /// `createItem` / `modifyItem` → seal + upload, acking only on recorded.
    ///
    /// Every write (this, [`Self::ingest_with_base`], [`Self::delete`],
    /// [`Self::rename`]) first re-reads the set's row: a floor ahead of the
    /// generation this host holds re-reads custody, and if the generation is
    /// still missing the write errors — held, never sealed under the older
    /// generation, never acked.
    pub async fn ingest(&self, rel: String) -> Result<FfiFileProviderAck, FfiError> {
        self.call(|reply| Request::Ingest { rel, reply }).await
    }

    /// `modifyItem` carrying the OS's `baseVersion` → seal + upload, but compare the
    /// base `contentVersion` the OS believed it was editing against the row's
    /// current head first. Equal (or an empty base) is a plain fast-forward ingest;
    /// a mismatch means a concurrent writer advanced the head, so the staged write
    /// is routed through the engine's shared conflict auto-resolve instead of
    /// clobbering it (`file-sync.md` § Conflicts — retention-first, non-destructive).
    /// Acks only on recorded, exactly like [`Self::ingest`].
    pub async fn ingest_with_base(
        &self,
        rel: String,
        base_content_version: Vec<u8>,
    ) -> Result<FfiFileProviderAck, FfiError> {
        self.call(|reply| Request::IngestWithBase {
            rel,
            base_content_version,
            reply,
        })
        .await
    }

    /// `deleteItem` → record-first tombstone; acks only on recorded, exactly
    /// like [`Self::ingest`] — a not-recorded delete stays pending OS-side.
    pub async fn delete(&self, rel: String) -> Result<FfiFileProviderAck, FfiError> {
        self.call(|reply| Request::Delete { rel, reply }).await
    }

    /// rename / move → tombstone the old path + ingest the new (delete+create
    /// pair; ack off the ingest).
    pub async fn rename(
        &self,
        from_rel: String,
        to_rel: String,
    ) -> Result<FfiFileProviderAck, FfiError> {
        self.call(|reply| Request::Rename {
            from_rel,
            to_rel,
            reply,
        })
        .await
    }

    /// Is this rel excluded from sync — a dotfile component, a built-in default
    /// ignore, or a `.faunaignore` pattern (`file-sync.md` § Built-in default
    /// ignores)? The appex asks up front in `createItem` — before staging
    /// bytes, and for folders, which never reach a write core: an ignored
    /// folder is excluded whole, so the OS never sends `createItem` for its
    /// children — and maps `true` to `NSFileProviderError.excludedFromSync`.
    pub async fn is_ignored(&self, rel: String) -> Result<bool, FfiError> {
        self.call(|reply| Request::IsIgnored { rel, reply }).await
    }

    /// observed eviction → demote the materialized file back to a placeholder row.
    pub async fn evict(&self, rel: String) -> Result<(), FfiError> {
        self.call(|reply| Request::Evict { rel, reply }).await
    }

    /// eviction-observation tick → every rel the host believes is materialized
    /// on the OS's disk (`Synced` rows). The appex diffs these against
    /// `NSFileProviderManager.enumeratorForMaterializedItems` (the pure diff is
    /// `FileProviderEviction.evictionCandidates`, headlessly tested) and calls
    /// [`Self::evict`] for each rel the OS dropped, so the dehydration
    /// bookkeeping stays honest.
    pub async fn hydrated_rels(&self) -> Result<Vec<String>, FfiError> {
        self.call(|reply| Request::HydratedRels { reply }).await
    }

    /// live-refresh tick → re-pull the set from the nest in-session and converge
    /// the rows (new/moved placeholders + stale-hydrated re-points). Returns whether
    /// anything changed, so the appex signals `signalEnumerator(for: .rootContainer)`
    /// only when there is something new to enumerate (`file-sync.md` § Apple File
    /// Provider binding — *a pulled remote change signals the enumerator*).
    ///
    /// Before the pull it re-reads the set's row, and when the binding, the
    /// caller's access or the content-key floor moved (or the floor is still
    /// ahead of the generation held) it re-reads custody and rebuilds the engine
    /// over the same state DB — how a rotation reaches an app-dead host
    /// (`on-demand-files.md` § Shared sets on a capability host, decision 2).
    pub async fn refresh(&self) -> Result<bool, FfiError> {
        self.call(|reply| Request::Refresh { reply }).await
    }

    /// Whether this host serves its set read-only — the account holds it as a
    /// reader (`on-demand-files.md` § Shared sets on a capability host,
    /// decision 3). The platform advertises no write, create, delete or rename
    /// capability on such a set's items; a write that reaches the host anyway
    /// is refused before anything is sealed or moved. Read from the engine the
    /// host holds now, so it follows a grant change at the next refresh. An
    /// error when the set's binding refused (no engine — fail closed).
    pub async fn is_read_only(&self) -> Result<bool, FfiError> {
        self.call(|reply| Request::ReadOnly { reply }).await
    }

    // ---- the owned-tree operations (a host built by `app_dead_owned_tree`) ----

    /// `openDocument` for read → the body's path, hydrated into the cache root
    /// first when the document is a placeholder. A document whose body cannot
    /// be fetched is an error — never a stand-in body.
    pub async fn open_for_read(&self, rel: String) -> Result<String, FfiError> {
        self.call(|reply| Request::OpenForRead { rel, reply }).await
    }

    /// `openDocument` for write → the body promoted into the kept root, and the
    /// `contentVersion` the open saw. Match every open with one
    /// [`Self::closed_write`].
    pub async fn open_for_write(&self, rel: String) -> Result<FfiOwnedWriteOpen, FfiError> {
        self.call(|reply| Request::OpenForWrite { rel, reply })
            .await
    }

    /// The written descriptor closed → ingest against the open's base; on a
    /// recorded ack the body leaves the kept root. An un-acked change stays
    /// kept and the next sweep re-drives it.
    pub async fn closed_write(
        &self,
        rel: String,
        base_content_version: Vec<u8>,
    ) -> Result<FfiFileProviderAck, FfiError> {
        self.call(|reply| Request::ClosedWrite {
            rel,
            base_content_version,
            reply,
        })
        .await
    }

    /// `createDocument` → an empty document in the kept root, ingested. An
    /// existing or ignored name is refused.
    pub async fn create_document(&self, rel: String) -> Result<FfiFileProviderAck, FfiError> {
        self.call(|reply| Request::CreateDocument { rel, reply })
            .await
    }

    /// `deleteDocument` → record-first; a delete that cannot be recorded is an
    /// error and nothing changes. A directory is deleted file by file, stopping
    /// at the first file that cannot be recorded (those before it stay deleted;
    /// a repeat finishes the rest).
    pub async fn delete_document(&self, rel: String) -> Result<(), FfiError> {
        self.call(|reply| Request::DeleteDocument { rel, reply })
            .await
    }

    /// `renameDocument` / `moveDocument` → record-first; a rename whose old
    /// path's tombstone cannot be recorded is an error and nothing changes. A
    /// directory is renamed file by file under the same stop-at-first-failure
    /// rule as [`Self::delete_document`].
    pub async fn rename_document(
        &self,
        from_rel: String,
        to_rel: String,
    ) -> Result<FfiFileProviderAck, FfiError> {
        self.call(|reply| Request::RenameDocument {
            from_rel,
            to_rel,
            reply,
        })
        .await
    }

    /// Re-drive the start sweep (the host runs it itself at start): ingest
    /// every kept-root body, demote what records, observe evictions.
    pub async fn sweep_kept_root(&self) -> Result<FfiOwnedSweepReport, FfiError> {
        self.call(|reply| Request::SweepKeptRoot { reply }).await
    }

    /// The refresh tick's eviction observation: every hydrated row whose body
    /// the OS reclaimed goes back to a placeholder — never a delete.
    pub async fn observe_evictions(&self) -> Result<Vec<String>, FfiError> {
        self.call(|reply| Request::ObserveEvictions { reply }).await
    }

    /// The peer-transfer seam: `rel`'s body (kept root, then cache root), or
    /// `None` for a placeholder.
    pub async fn lookup_body(&self, rel: String) -> Result<Option<String>, FfiError> {
        self.call(|reply| Request::LookupBody { rel, reply }).await
    }

    /// The peer-transfer seam: move the body at `src_path` into `root` as
    /// `rel`'s body; returns where it landed.
    pub async fn land_body(
        &self,
        rel: String,
        src_path: String,
        root: FfiBodyRoot,
    ) -> Result<String, FfiError> {
        self.call(|reply| Request::LandBody {
            rel,
            src_path,
            root,
            reply,
        })
        .await
    }

    /// The peer-transfer seam: record `rel`'s row as a placeholder.
    pub async fn record_placeholder(&self, rel: String) -> Result<(), FfiError> {
        self.call(|reply| Request::RecordPlaceholder { rel, reply })
            .await
    }
}

/// The host's one resident engine, and what rebuilding it takes.
///
/// A resident engine's keys are final at build, and an app-dead host has no app
/// to rebuild it, so the host re-resolves on its own evidence at three edges
/// (`on-demand-files.md` § Shared sets on a capability host, decision 2): at
/// **build** (the custody load itself, `build_engine`); at every **refresh**,
/// when the set's row says the binding, the caller's access or the content-key
/// floor moved, or the floor is still ahead of the generation held; and
/// **before a seal**, when the floor is ahead — a write there is held, never
/// sealed under the older generation and never acknowledged. Re-resolving is a
/// custody re-read and a rebuild over the same state DB; the owner's sets are
/// held to the floor exactly as a member's.
struct Resident {
    ctx: HostContext,
    /// The provisioned change signer, re-read at every build and every seal.
    signer: ForeignSigner,
    /// Whether the resident engine was built with a signer — and so read the
    /// custody its set nonce lives in. One built without is rebuilt at the
    /// first seal that finds a signer; one built with has its signer swapped
    /// in place.
    built_signed: bool,
    folder_ref: FolderRef,
    root_dir: PathBuf,
    /// `None` = the binding refused (fail closed): every request reports it.
    built: Option<BuiltEngine>,
    /// Set by every (re)build, taken by the worker loop — a new engine is a new
    /// binding, which re-arms the pre-bind re-seal pass.
    rebuilt: bool,
}

impl Resident {
    /// (Re)build the engine over the same state DB. The old engine — and its
    /// `SyncDb` connection — is dropped before the new one opens the DB.
    async fn build(&mut self) {
        self.built = None;
        let mut params = self
            .ctx
            .params(self.root_dir.clone(), self.folder_ref, None);
        // A missing signer is no reason to refuse the build — reads need none;
        // the pre-seal edge holds every write until one is provisioned.
        params.change_signer = match self.signer.current() {
            Ok(signer) => Some(signer),
            Err(why) => {
                tracing::info!("file-provider host: built without a change signer: {why}");
                None
            }
        };
        self.built_signed = params.change_signer.is_some();
        self.built = build_engine(params).await;
        self.rebuilt = true;
    }

    /// Put the currently provisioned signer on the resident engine — in place,
    /// or by a rebuild when the engine was built without one. `Err` holds the
    /// write: no signer is provisioned, or the one provisioned is unusable.
    async fn arm_signer(&mut self) -> Result<(), String> {
        let signer = self.signer.current()?;
        match self.built.as_ref() {
            None => {}
            Some(_) if !self.built_signed => self.build().await,
            // A `false` swap = built signed but no nonce resolved for the set;
            // the engine says so itself (once), and its unsigned record is
            // refused `signature_required` at the nest.
            Some(built) => {
                built.engine.replace_change_signer(signer);
            }
        }
        Ok(())
    }

    /// Whether the resident engine is a reader's: it seals nothing, so the
    /// host runs no seal edge, sweep or re-seal drive for it.
    fn read_only(&self) -> bool {
        self.built.as_ref().is_some_and(|b| b.read_only)
    }

    /// Whether the engine was (re)built since the last call.
    fn take_rebuilt(&mut self) -> bool {
        std::mem::take(&mut self.rebuilt)
    }

    /// Read the set's row — a cross-nest set's custody record, which is its row
    /// — and rebuild when the engine no longer answers it. `Err(())` = the read
    /// failed: nothing is known about the row.
    async fn re_resolve(&mut self) -> Result<(), ()> {
        let row = self.ctx.binding_basis(self.folder_ref).await.ok_or(())?;
        let rebuild = match &self.built {
            // An engine built before the machine had a principal read no
            // custody (a host reads it as the enrolled device it is), so it
            // holds no set nonce and no content keys: once a signer is
            // provisioned, the next edge — a refresh as much as a seal —
            // rebuilds it over the custody it can now read.
            Some(_) if !self.built_signed && self.signer.current().is_ok() => true,
            Some(b) => b.edge_verdict(row.as_ref()) == EdgeVerdict::Rebuild,
            // A refused build is retried once the set has a row again.
            None => row.is_some(),
        };
        if rebuild {
            self.build().await;
        }
        Ok(())
    }

    /// The refresh edge. An unavailable list keeps the engine as it is — the
    /// refresh's own pull reports the outage.
    async fn refresh_edge(&mut self) {
        let _ = self.re_resolve().await;
    }

    /// The pre-seal edge: `Err` holds the write (un-sealed, un-acked). Every
    /// write records, so every write passes here signed or not at all.
    async fn seal_edge(&mut self, folder: &str) -> Result<(), String> {
        self.re_resolve().await.map_err(|()| {
            format!(
                "folder {folder}: the set's row could not be read (the folder list, or a \
                 cross-nest set's custody) — its content-key floor cannot be checked, so the \
                 write is held (fail closed)"
            )
        })?;
        self.arm_signer()
            .await
            .map_err(|why| format!("folder {folder}: {why}"))?;
        let Some(built) = self.built.as_ref() else {
            // `serve` reports the refused binding.
            return Ok(());
        };
        // The engine's own hold — the one implementation the desktop agent's
        // resident engines run too; checked here as well so the write is
        // refused before any I/O. The PUBLICATION hold, not the seal hold
        // alone: this host acknowledges only what is recorded, so a write it
        // could seal but not send stays un-acked (decision 2′).
        match built.publish_hold() {
            None => Ok(()),
            Some(hold) => Err(format!("folder {folder}: {hold}")),
        }
    }

    /// Populate the placeholder rows from the nest (see [`engine_worker`]).
    /// Hands back what the fold's share reconcile retired, for the owned tree
    /// to settle its peer-landed bodies by.
    async fn populate(&self, folder: &str) -> fauna_sync_engine::engine::OverlayReconcile {
        let Some(b) = self.built.as_ref() else {
            return Default::default();
        };
        match b.engine.populate_placeholders_from_nest().await {
            Ok(fold) => {
                tracing::info!(
                    "file-provider host ({folder}): populated {} placeholder row(s)",
                    fold.recorded
                );
                fold.overlay
            }
            Err(e) => {
                tracing::error!(
                    "file-provider host ({folder}): initial placeholder population failed \
                     (the OS re-invokes the extension to retry): {e}"
                );
                Default::default()
            }
        }
    }

    /// This host's replica as the share plane uses it: `None` while the
    /// engine is not built (a refused binding) or the host owns no tree.
    #[cfg(feature = "p2p-share")]
    fn share_replica(
        &self,
        owned: Option<&OwnedTree>,
    ) -> Option<fauna_sync_engine::share_glue::OnDemandReplica> {
        let (tree, built) = (owned?, self.built.as_ref()?);
        let folder_id = self.folder_ref.to_wire();
        Some(fauna_sync_engine::share_glue::OnDemandReplica {
            folder: built
                .engine
                .folder()
                .map_or_else(|| folder_id.clone(), str::to_owned),
            folder_id,
            db_path: self.folder_ref.state_db_path(self.ctx.state_dir()),
            kept_root: tree.kept_root().to_path_buf(),
            cache_root: tree.cache_root().to_path_buf(),
        })
    }
}

/// Settle the owned tree's peer-landed bodies by one fold's share reconcile:
/// a confirmed body is demoted to the cache root, a superseded one dropped —
/// before any sweep could read either as a write intent.
fn settle_peer_bodies(
    owned: Option<&OwnedTree>,
    engine: &fauna_sync_engine::engine::SyncEngine,
    reconcile: &fauna_sync_engine::engine::OverlayReconcile,
    folder: &str,
) {
    if let Some(tree) = owned
        && let Err(e) = tree.settle_peer_bodies(engine, reconcile)
    {
        tracing::warn!("file-provider host ({folder}): settling peer-landed bodies failed: {e:#}");
    }
}

/// The worker: builds the resident engine on the caller's current-thread
/// runtime, serves provider-face primitives against it, and re-resolves it at
/// the refresh and pre-seal edges ([`Resident`]). A failed content-key binding
/// (fail-closed) leaves no engine, so every request reports it rather than
/// touching plaintext.
async fn engine_worker(
    ctx: HostContext,
    signer: ForeignSigner,
    folder_ref: FolderRef,
    root_dir: PathBuf,
    cache_root: Option<PathBuf>,
    mut requests: mpsc::Receiver<Request>,
) {
    let owned = cache_root.map(|cache| OwnedTree::new(root_dir.clone(), cache));
    // The set's identity, as every log line names it.
    let folder = folder_ref.to_wire();
    // The FP host owns its (bearer) nest connection and must connect it before
    // serving — the seed-based `FfiSyncEngineHost` is instead handed the app's
    // already-connected client. A connect failure is logged and left to
    // `build_engine`'s list-fetch retry / the per-request fail-closed report,
    // and the OS re-invokes the extension to reconstruct.
    if let Err(e) = ctx.nest_rpc().connect().await {
        tracing::error!("file-provider host ({folder}): nest connect failed: {e}");
    }
    let mut resident = Resident {
        ctx,
        signer,
        built_signed: false,
        folder_ref,
        root_dir,
        built: None,
        rebuilt: false,
    };
    resident.build().await;

    // Populate the SyncDb placeholder rows from the nest before serving the
    // OS's first `enumerate`. The FP host is control-inverted — it runs no
    // watcher/loop (unlike the agent's resident engines), so nothing else folds the set's
    // `changes.list` into the local db, and `serve_enumerate` reads only local
    // rows (`provider_rows`); without this an app-dead extension whose db is
    // empty would enumerate nothing. This is the same on-demand primitive the
    // Windows sync service runs at startup (`populate_placeholders_from_nest`:
    // fetch changes → record placeholders, NO byte download — hydration stays
    // lazy on `fetchContents`). `fileproviderd` reconstructs the app-dead
    // extension, so each reconstruction re-pulls; the appex ALSO drives an
    // in-session [`FfiFileProviderHost::refresh`] tick on the constant
    // reconcile cadence (`DEFAULT_RESCAN_INTERVAL` — phase 5's de-knob) +
    // `NSFileProviderManager.signalEnumerator` so a remote change appears
    // without a reconstruction (`file-sync.md` § Apple File Provider
    // binding).
    let reconciled = resident.populate(&folder).await;
    if let Some(b) = resident.built.as_ref() {
        settle_peer_bodies(owned.as_ref(), &b.engine, &reconciled, &folder);
    }

    // An owned tree's start sweep (`on-demand-files.md` § Android SAF
    // DocumentsProvider binding): a body a dead process left in the kept root
    // IS its write intent, so it is ingested before the first request — after
    // the populate, so a kept body over a row the nest's head moved under takes
    // the conflict arm. It seals, so it passes the same pre-seal edge a write
    // does; a held or failed sweep leaves the bodies for the next one.
    if let Some(tree) = owned.as_ref() {
        // A reader's sweep ingests nothing (it only observes evictions), so it
        // passes no seal edge.
        let edge = if resident.read_only() {
            Ok(())
        } else {
            resident.seal_edge(&folder).await
        };
        match edge {
            Err(msg) => tracing::info!("file-provider host: start sweep held: {msg}"),
            Ok(()) => {
                if let Some(b) = resident.built.as_ref() {
                    match tree.start_sweep(&b.engine).await {
                        Ok(r) => tracing::info!(
                            "file-provider host ({folder}): start sweep recorded {}, pending {}, \
                             evicted {}",
                            r.recorded.len(),
                            r.pending.len(),
                            r.evicted.len()
                        ),
                        Err(e) => {
                            tracing::warn!(
                                "file-provider host ({folder}): start sweep failed: {e:#}"
                            )
                        }
                    }
                }
            }
        }
    }

    // Part (D) (`mls-group-key-material.md` § M2 → *Pre-bind re-seal
    // migration*): on the owner's host the pre-bind re-seal runs HERE, the
    // only engine an on-demand owner has. It is driven one file at a time and
    // only while the OS has nothing waiting, so a whole-corpus walk never
    // stands between `fileproviderd` and an `enumerate`; the worst an OS
    // request waits is the one file in flight. Armed at every build (the
    // binding the host observed) and re-armed at every refresh edge; a pass
    // that cannot run here (unbound, or a bound set whose keys the host does
    // not hold) disarms after one attempt, failing closed. It reaches this
    // host's placeholders only once `build_engine` declares the owner role,
    // which waits on part (D)'s provenance ruling (until then it walks the
    // hydrated rows, as every engine's pass does). A re-seal is a seal, so each
    // step passes the same pre-seal edge a write does ([`Resident::seal_edge`]):
    // it is never sealed under a generation the owner has rotated past.
    let mut prebind = PrebindDrive::armed();
    loop {
        if resident.take_rebuilt() {
            prebind = PrebindDrive::armed();
        }
        // A re-seal is a seal: a reader's host never drives the pass.
        let request = if resident.built.is_some() && prebind.armed && !resident.read_only() {
            match requests.try_recv() {
                Ok(request) => request,
                Err(mpsc::error::TryRecvError::Empty) => {
                    prebind.step_held_to_the_floor(&mut resident, &folder).await;
                    continue;
                }
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            }
        } else {
            match requests.recv().await {
                Some(request) => request,
                None => break,
            }
        };
        // The plane asks which replica this host serves — answered from the
        // resident's own state, built or not.
        #[cfg(feature = "p2p-share")]
        let request = match request {
            Request::ShareReplica { reply } => {
                let _ = reply.send(Ok(resident.share_replica(owned.as_ref())));
                continue;
            }
            other => other,
        };
        let refresh = matches!(request, Request::Refresh { .. });
        // A write handed to a reader's host: re-read the row once — a `writer`
        // grant may have arrived since the last refresh — and if the host is
        // still a reader, skip the seal edge; the cores refuse the write typed.
        if request.seals() && resident.read_only() {
            resident.refresh_edge().await;
        }
        if request.seals() && !resident.read_only() {
            if let Err(msg) = resident.seal_edge(&folder).await {
                tracing::warn!("file-provider host: {msg}");
                // A held close still ends its writer: the body stays in the
                // kept root, and only a released writer lets the next sweep
                // re-drive it.
                if let (Request::ClosedWrite { rel, .. }, Some(tree)) = (&request, owned.as_ref()) {
                    tree.release_writer(rel);
                }
                request.fail(msg);
                continue;
            }
        } else if refresh {
            resident.refresh_edge().await;
        }
        serve(resident.built.as_ref(), owned.as_ref(), &folder, request).await;
        if refresh {
            prebind = PrebindDrive::armed();
        }
    }
}

/// The capability host's idle-time drive of the pre-bind re-seal pass
/// ([`fauna_sync_engine::engine::SyncEngine::reseal_next_pending_under_current`]).
struct PrebindDrive {
    armed: bool,
    attempted: std::collections::HashSet<String>,
    moved: usize,
}

impl PrebindDrive {
    fn armed() -> Self {
        Self {
            armed: true,
            attempted: Default::default(),
            moved: 0,
        }
    }

    async fn step(&mut self, engine: &fauna_sync_engine::engine::SyncEngine, folder: &str) {
        match engine
            .reseal_next_pending_under_current(&mut self.attempted)
            .await
        {
            Ok(true) => self.moved += 1,
            Ok(false) => {
                if self.moved > 0 {
                    tracing::info!(
                        "file-provider host ({folder}): pre-bind re-seal pass reached {} file(s)",
                        self.moved
                    );
                }
                self.armed = false;
            }
            Err(e) => {
                // Resumable by the per-row stamp: the next build or refresh
                // re-arms it and picks up where this stopped.
                tracing::warn!(
                    "file-provider host ({folder}): pre-bind re-seal pass stopped: {e:#}"
                );
                self.armed = false;
            }
        }
    }

    /// One step, behind the pre-seal edge: a floor this host cannot meet (or a
    /// row it cannot read) disarms the pass until the next build or refresh
    /// re-arms it, rather than re-sealing under an older generation.
    async fn step_held_to_the_floor(&mut self, resident: &mut Resident, folder: &str) {
        if let Err(msg) = resident.seal_edge(folder).await {
            tracing::info!("file-provider host: pre-bind re-seal pass held: {msg}");
            self.armed = false;
            return;
        }
        match resident.built.as_ref() {
            Some(b) => self.step(&b.engine, folder).await,
            None => self.armed = false,
        }
    }
}

/// Dispatch one request to the shared serving cores, or report the fail-closed
/// binding. `built` is checked once up front so a `None` (indeterminate content-key
/// binding) reports through the request's reply channel rather than serving.
async fn serve(
    built: Option<&BuiltEngine>,
    owned: Option<&OwnedTree>,
    folder: &str,
    request: Request,
) {
    let engine = match built {
        Some(b) => &b.engine,
        None => {
            request.fail(format!(
                "folder {folder}: content-key binding indeterminate — \
                 refusing to serve (fail closed)"
            ));
            return;
        }
    };

    match request {
        Request::Enumerate { parent_rel, reply } => {
            let out = provider_face::serve_enumerate(engine, &parent_rel)
                .await
                .map(|items| items.into_iter().map(FfiFileProviderItem::from).collect())
                .map_err(|e| e.to_string());
            let _ = reply.send(out);
        }
        Request::Item { rel, reply } => {
            let out = provider_face::serve_item(engine, &rel)
                .await
                .map(|opt| opt.map(FfiFileProviderItem::from))
                .map_err(|e| e.to_string());
            let _ = reply.send(out);
        }
        Request::Fetch { rel, reply } => {
            let out = provider_face::serve_fetch(engine, &rel)
                .await
                .map(|f| FfiFileProviderContent {
                    bytes: f.bytes,
                    content_version: f.content_version,
                })
                .map_err(|e| e.to_string());
            let _ = reply.send(out);
        }
        Request::FetchToPath {
            rel,
            dest_path,
            reply,
        } => {
            let out =
                provider_face::serve_fetch_to_path(engine, &rel, std::path::Path::new(&dest_path))
                    .await
                    .map_err(|e| e.to_string());
            let _ = reply.send(out);
        }
        Request::Ingest { rel, reply } => {
            let out = provider_face::serve_ingest(engine, &rel)
                .await
                .map(FfiFileProviderAck::from)
                .map_err(|e| e.to_string());
            let _ = reply.send(out);
        }
        Request::IngestWithBase {
            rel,
            base_content_version,
            reply,
        } => {
            let out = provider_face::serve_ingest_with_base(engine, &rel, &base_content_version)
                .await
                .map(FfiFileProviderAck::from)
                .map_err(|e| e.to_string());
            let _ = reply.send(out);
        }
        Request::Delete { rel, reply } => {
            let out = provider_face::serve_delete(engine, &rel)
                .await
                .map(FfiFileProviderAck::from)
                .map_err(|e| e.to_string());
            let _ = reply.send(out);
        }
        Request::Rename {
            from_rel,
            to_rel,
            reply,
        } => {
            let out = provider_face::serve_rename(engine, &from_rel, &to_rel)
                .await
                .map(FfiFileProviderAck::from)
                .map_err(|e| e.to_string());
            let _ = reply.send(out);
        }
        Request::IsIgnored { rel, reply } => {
            let _ = reply.send(Ok(provider_face::serve_is_ignored(engine, &rel)));
        }
        Request::Evict { rel, reply } => {
            let out = provider_face::serve_evict(engine, &rel)
                .await
                .map_err(|e| e.to_string());
            let _ = reply.send(out);
        }
        Request::HydratedRels { reply } => {
            let out = provider_face::serve_hydrated_rels(engine)
                .await
                .map_err(|e| e.to_string());
            let _ = reply.send(out);
        }
        Request::ReadOnly { reply } => {
            let _ = reply.send(Ok(engine.is_read_only()));
        }
        Request::Refresh { reply } => {
            let out = engine
                .refresh_from_nest_reconciling()
                .await
                .map(|(changed, reconciled)| {
                    settle_peer_bodies(owned, engine, &reconciled, folder);
                    changed
                })
                .map_err(|e| e.to_string());
            let _ = reply.send(out);
        }
        owned_request => serve_owned(engine, owned, folder, owned_request).await,
    }
}

/// Dispatch one owned-tree request, or refuse it on a host that owns no tree.
async fn serve_owned(
    engine: &fauna_sync_engine::engine::SyncEngine,
    owned: Option<&OwnedTree>,
    folder: &str,
    request: Request,
) {
    let Some(tree) = owned else {
        request.fail(format!(
            "folder {folder}: this host owns no tree (built by app_dead, not \
             app_dead_owned_tree)"
        ));
        return;
    };
    let path = |p: PathBuf| p.to_string_lossy().into_owned();
    match request {
        Request::OpenForRead { rel, reply } => {
            let out = tree.open_for_read(engine, &rel).await.map(path);
            let _ = reply.send(out.map_err(|e| format!("{e:#}")));
        }
        Request::OpenForWrite { rel, reply } => {
            let out = tree
                .open_for_write(engine, &rel)
                .await
                .map(|o| FfiOwnedWriteOpen {
                    path: path(o.path),
                    base_content_version: o.base_version,
                });
            let _ = reply.send(out.map_err(|e| format!("{e:#}")));
        }
        Request::ClosedWrite {
            rel,
            base_content_version,
            reply,
        } => {
            let out = tree
                .closed_write(engine, &rel, &base_content_version)
                .await
                .map(FfiFileProviderAck::from);
            let _ = reply.send(out.map_err(|e| format!("{e:#}")));
        }
        Request::CreateDocument { rel, reply } => {
            let out = tree
                .create(engine, &rel)
                .await
                .map(FfiFileProviderAck::from);
            let _ = reply.send(out.map_err(|e| format!("{e:#}")));
        }
        Request::DeleteDocument { rel, reply } => {
            let out = tree.delete(engine, &rel).await;
            let _ = reply.send(out.map_err(|e| format!("{e:#}")));
        }
        Request::RenameDocument {
            from_rel,
            to_rel,
            reply,
        } => {
            let out = tree
                .rename(engine, &from_rel, &to_rel)
                .await
                .map(FfiFileProviderAck::from);
            let _ = reply.send(out.map_err(|e| format!("{e:#}")));
        }
        Request::SweepKeptRoot { reply } => {
            let out = tree.start_sweep(engine).await.map(|r| FfiOwnedSweepReport {
                recorded: r.recorded,
                pending: r.pending,
                evicted: r.evicted,
            });
            let _ = reply.send(out.map_err(|e| format!("{e:#}")));
        }
        Request::ObserveEvictions { reply } => {
            let out = tree.observe_evictions(engine).await;
            let _ = reply.send(out.map_err(|e| format!("{e:#}")));
        }
        Request::LookupBody { rel, reply } => {
            let out = tree.lookup_body(engine, &rel).map(|p| p.map(path));
            let _ = reply.send(out.map_err(|e| format!("{e:#}")));
        }
        Request::LandBody {
            rel,
            src_path,
            root,
            reply,
        } => {
            let out = tree
                .land_body(engine, &rel, std::path::Path::new(&src_path), root.into())
                .map(path);
            let _ = reply.send(out.map_err(|e| format!("{e:#}")));
        }
        Request::RecordPlaceholder { rel, reply } => {
            let out = tree.record_placeholder(engine, &rel);
            let _ = reply.send(out.map_err(|e| format!("{e:#}")));
        }
        #[cfg(feature = "p2p-share")]
        Request::ShareIngest {
            proven_actor_hex,
            rows,
            spool_dir,
            reply,
        } => {
            let out = tree
                .share_ingest(engine, &proven_actor_hex, &rows, PathBuf::from(spool_dir))
                .await
                .map(|(report, cursor)| {
                    for (path, reason) in &report.skipped {
                        tracing::debug!(
                            path = %fauna_core::log_redact::log_path(path),
                            %reason,
                            "share ingest: no body landed for this row"
                        );
                    }
                    FfiShareIngestOutcome {
                        refused: report.refused as u32,
                        overlaid: report.overlaid as u32,
                        materialized: report.materialized as u32,
                        already_current: report.already_current as u32,
                        cursor,
                        storage_limited: report.storage_limited,
                    }
                });
            let _ = reply.send(out.map_err(|e| format!("{e:#}")));
        }
        other => other.fail(format!(
            "folder {folder}: internal — not an owned-tree request"
        )),
    }
}

impl Request {
    /// Does serving this request seal under the set's keys — content, or a
    /// recorded path (a tombstone's path seals under the same root)?
    fn seals(&self) -> bool {
        matches!(
            self,
            Request::Ingest { .. }
                | Request::IngestWithBase { .. }
                | Request::Delete { .. }
                | Request::Rename { .. }
                | Request::ClosedWrite { .. }
                | Request::CreateDocument { .. }
                | Request::DeleteDocument { .. }
                | Request::RenameDocument { .. }
                | Request::SweepKeptRoot { .. }
        )
    }

    /// Send `msg` to whichever reply channel this request carries (the fail-closed
    /// path).
    fn fail(self, msg: String) {
        match self {
            Request::Enumerate { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::Item { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::Fetch { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::FetchToPath { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::Ingest { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::IngestWithBase { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::Delete { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::Rename { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::IsIgnored { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::Evict { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::HydratedRels { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::Refresh { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::ReadOnly { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::OpenForRead { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::OpenForWrite { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::ClosedWrite { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::CreateDocument { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::DeleteDocument { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::RenameDocument { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::SweepKeptRoot { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::ObserveEvictions { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::LookupBody { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::LandBody { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            Request::RecordPlaceholder { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
            #[cfg(feature = "p2p-share")]
            Request::ShareReplica { reply } => {
                let _ = reply.send(Err(msg));
            }
            #[cfg(feature = "p2p-share")]
            Request::ShareIngest { reply, .. } => {
                let _ = reply.send(Err(msg));
            }
        }
    }
}

// The per-actor state-dir derivation the sync processes resolve — app,
// extension and agent alike (`file-sync.md` § Apple File Provider binding,
// Multi-account × File Provider consequence 3) — is NOT duplicated here: it is
// the same `<base>/<actor-id-hex>/` rule every account-scoped store takes, so
// there is one exported name for it, `account_state_dir`
// (`crate::account_state`), applied to the sync base. One derivation, one
// export, no way for two processes to resolve differently.

/// The rels in one folder's state DB whose local content lacks a proven
/// record — the app-side gate behind the iOS File Provider domain-removal
/// refusal (`file-sync.md` § Multi-account × File Provider, consequence 2: iOS
/// has no preserve mode, so removal refuses while any such row exists and the
/// OS's pending-change retry drains it). A cross-process WAL read of the
/// extension's DB, same sanctioned pattern as the Media-badge `file_states`
/// reads. A missing DB reports empty; an unreadable one errors and the caller
/// refuses the removal (fail closed). `folder_id` is the set's `FolderRef`
/// wire string ([`crate::sync_engine_host::parse_folder_id`]) — the same key
/// the extension's host writes the DB under.
#[uniffi::export]
pub fn sync_set_unrecorded_rels(
    state_dir: String,
    folder_id: String,
) -> Result<Vec<String>, FfiError> {
    let folder_ref = parse_folder_id(&folder_id)?;
    fauna_sync_engine::db::set_unrecorded_rels(&PathBuf::from(state_dir), folder_ref).map_err(|e| {
        FfiError::General {
            msg: format!("un-recorded rows check failed: {e}"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_sync_engine::generation_tip::RetainedKeyCustody;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    struct StaticBearer;
    impl FfiBearerProvider for StaticBearer {
        fn current_bearer(&self) -> String {
            "bearer".into()
        }
    }

    struct Principal(Mutex<[u8; 32]>);
    impl FfiChangeSignerProvider for Principal {
        fn current_signer(&self) -> Option<FfiChangeSignerCarriage> {
            Some(FfiChangeSignerCarriage {
                writer_secret: self.0.lock().unwrap().to_vec(),
                device_authorization: Vec::new(),
            })
        }
    }

    fn auth(actor_id: [u8; 32]) -> Arc<fauna_client::AuthClient> {
        auth_on(actor_id, "https://nest.invalid")
    }

    fn auth_on(actor_id: [u8; 32], url: &str) -> Arc<fauna_client::AuthClient> {
        let url = url.to_string();
        Arc::new(fauna_client::AuthClient::bearer_only(
            url.clone(),
            actor_id,
            Arc::new(ForeignBearer(Arc::new(StaticBearer))),
            fauna_client::pinned_http_client(&url),
        ))
    }

    /// Every capability host of one account in a process reads through one
    /// replica; a re-minted machine principal gets a fresh one (the old
    /// replica was keyed by the old device); another account gets its own.
    #[tokio::test]
    async fn the_hosts_of_one_account_share_one_replica_until_the_principal_changes() {
        let actor = [0xD1; 32];
        let backup = fauna_core::crypto::BackupKey::from_bytes([0x02; 32]);
        let principal = Principal(Mutex::new([0x03; 32]));
        let first = capability_host_folder_keys(&auth(actor), actor, &backup, &principal).unwrap();
        let second = capability_host_folder_keys(&auth(actor), actor, &backup, &principal).unwrap();
        assert!(first.shares_replica_with(&second));

        *principal.0.lock().unwrap() = [0x04; 32];
        let reminted =
            capability_host_folder_keys(&auth(actor), actor, &backup, &principal).unwrap();
        assert!(!reminted.shares_replica_with(&first));

        let other = [0xD2; 32];
        let theirs = capability_host_folder_keys(&auth(other), other, &backup, &principal).unwrap();
        assert!(!theirs.shares_replica_with(&reminted));
    }

    /// A replica is bound to the nest it walks and the `BackupKey` it derives
    /// from: the same account and principal on another nest, or handed another
    /// key, gets its own — a host never reads through a replica keyed by
    /// inputs it was not given (a wrong key must fail closed, not borrow a
    /// sibling's).
    #[tokio::test]
    async fn a_replica_is_never_shared_across_nests_or_backup_keys() {
        let actor = [0xD4; 32];
        let backup = fauna_core::crypto::BackupKey::from_bytes([0x02; 32]);
        let principal = Principal(Mutex::new([0x05; 32]));
        let here = capability_host_folder_keys(
            &auth_on(actor, "https://one.invalid"),
            actor,
            &backup,
            &principal,
        )
        .unwrap();
        let there = capability_host_folder_keys(
            &auth_on(actor, "https://two.invalid"),
            actor,
            &backup,
            &principal,
        )
        .unwrap();
        assert!(!there.shares_replica_with(&here));

        let wrong_key = capability_host_folder_keys(
            &auth_on(actor, "https://one.invalid"),
            actor,
            &fauna_core::crypto::BackupKey::from_bytes([0x09; 32]),
            &principal,
        )
        .unwrap();
        assert!(!wrong_key.shares_replica_with(&here));
    }

    /// No machine principal provisioned: no replica, and the reason says so.
    #[test]
    fn no_principal_no_replica() {
        struct None_;
        impl FfiChangeSignerProvider for None_ {
            fn current_signer(&self) -> Option<FfiChangeSignerCarriage> {
                None
            }
        }
        let actor = [0xD3; 32];
        let why = capability_host_folder_keys(
            &auth(actor),
            actor,
            &fauna_core::crypto::BackupKey::from_bytes([0x02; 32]),
            &None_,
        )
        .err()
        .unwrap();
        assert!(why.contains("no machine principal"), "{why}");
    }

    #[derive(Default)]
    struct SwiftStore(Mutex<BTreeMap<Vec<u8>, Vec<u8>>>);
    impl FfiRetainedKeyCustody for SwiftStore {
        fn get(&self, generation: Vec<u8>) -> Option<Vec<u8>> {
            self.0.lock().unwrap().get(&generation).cloned()
        }
        fn put(&self, generation: Vec<u8>, key: Vec<u8>) {
            self.0.lock().unwrap().entry(generation).or_insert(key);
        }
        fn remove(&self, generation: Vec<u8>) {
            self.0.lock().unwrap().remove(&generation);
        }
    }

    /// The foreign custody round-trips a generation key through the plane's
    /// seam, answers absent for a value that is not a key, and forgets a
    /// dropped generation.
    #[test]
    fn a_foreign_custody_serves_the_planes_custody_seam() {
        let store = Arc::new(SwiftStore::default());
        let custody = ForeignRetainedKeys(store.clone());
        let key = fauna_core::crypto::GenerationKey::from_bytes([0x09; 32]);
        custody.record_generation_key(&[0x01; 32], &key);
        assert_eq!(
            custody
                .retained_generation_key(&[0x01; 32])
                .map(|k| *k.as_bytes()),
            Some([0x09; 32])
        );
        store.put(vec![0x02; 32], vec![0x09; 31]);
        assert!(custody.retained_generation_key(&[0x02; 32]).is_none());
        custody.drop_generation_key(&[0x01; 32]);
        assert!(custody.retained_generation_key(&[0x01; 32]).is_none());
    }
}
