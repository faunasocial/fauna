//! The **shared** client-side account-state seams — one trait per account-plane
//! kind a feature machine reads or writes ([`SuccessionLedgerStore`],
//! [`DeploymentSeedStore`], [`CustodyCeremonyStore`], [`FollowsStore`],
//! [`PreferenceStore`], [`BackupStateStore`], [`MailStore`], [`KindManifestStore`]), each implemented
//! directly on the account-store handle, plus the one [`StoreError`] they share.
//!
//! They live here, in the crate every consumer already depends on, so nobody
//! depends upward on the account plane.

use async_trait::async_trait;
use fauna_core::backup_state::{BackupDestinationsRow, BackupState};
use fauna_core::custody_ceremony::CustodyConfig;
use fauna_core::data::{
    BackupConfig, DelegationConfig, DeploymentSeedEntry, DestinationUnattestedMark, FollowedFolder,
    FollowsConfig, MailConfig, MailCredential, ModerationConfig, MsekFingerprint,
    PersonalizationConfig,
};
use fauna_core::identity::ActorId;
use fauna_core::mail_rows::{MailRows, MailStateRow};
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_protocol::MaybeSendSync;

/// An account-state load/save failure, surfaced by the per-app UI via its
/// `error-message` element (the seam impl maps the store's detail into a
/// `Display` string). One shared type across every consumer crate — each
/// re-exports it and folds it into its own `DispatchError` via `#[from]`.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A read of the account store failed.
    #[error("account state load: {0}")]
    Load(String),
    /// A write to the account store failed.
    #[error("account state save: {0}")]
    Save(String),
}

impl StoreError {
    /// Whether this is [`LEDGER_NOT_READY`] — the host's account store has not
    /// assembled yet (a transient state right after sign-in), or a read it
    /// gates is not ready — or [`LEDGER_AWAITING_SIBLING`], as opposed to a
    /// store that answered and failed. A reader that can render a degraded view
    /// and refresh later branches on this; a writer never does.
    pub fn is_not_ready(&self) -> bool {
        match self {
            StoreError::Load(m) | StoreError::Save(m) => {
                m == LEDGER_NOT_READY || m == LEDGER_AWAITING_SIBLING
            }
        }
    }

    /// The user-voice reason a surface shows for a not-ready refusal
    /// ([`Self::is_not_ready`]) in place of the error's own text — the nest
    /// is what is missing (`common.needs_nest`), or another of the account's
    /// devices is (`common.needs_other_device`;
    /// `account-client-lifecycle.md` § The client-side lifecycle → *The
    /// first listing*, clauses (4) and (5)). `None` for any other failure.
    pub fn not_ready_reason(&self) -> Option<&'static str> {
        use fauna_i18n::strings::common::{NEEDS_NEST, NEEDS_OTHER_DEVICE};
        match self {
            StoreError::Load(m) | StoreError::Save(m) if m == LEDGER_NOT_READY => Some(NEEDS_NEST),
            StoreError::Load(m) | StoreError::Save(m) if m == LEDGER_AWAITING_SIBLING => {
                Some(NEEDS_OTHER_DEVICE)
            }
            _ => None,
        }
    }
}

/// The **succession-ledger** persistence seam — `fauna.state.succession-ledger`
/// (`config-dissolution.md` § Phases and gates → *Bounded rows* → *The
/// ledger*), declared here so every consumer of the ledger injects it and
/// nobody depends upward on the account plane.
///
/// Production implements it **directly on the account-store handle**
/// (`fauna_account_plane`'s `impl SuccessionLedgerStore for
/// AccountStoreHandle`), which carries the runtime's own identity and its
/// attested predecessors, so no host passes identity per call and no app
/// writes glue. A read is the handle's READ fold (the chain seeded with this
/// runtime's identity, only the events the folded chain signed kept); a write
/// is a per-row join through the fleet plane's writer door, so there is no
/// CAS base to carry: every row's arm is a join (events write-once, marks the
/// verdict-precedence minimum), and a replica that moves nothing puts
/// nothing.
///
/// Native boxes `Send` futures (`async_trait`); wasm's `Rc`-based transport
/// yields `!Send`, so the wasm arm is `async_trait(?Send)`. The
/// [`MaybeSendSync`] supertrait lets a wasm impl hold a `!Send` handle.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait SuccessionLedgerStore: MaybeSendSync {
    /// The account this seam reads and writes as — the identity the READ fold
    /// seeds the chain with, and the key a caller's per-account side state
    /// (the registry's parked ceremony) is filed under.
    fn self_actor(&self) -> Result<ActorId, StoreError>;
    /// The account's succession ledger as this runtime reads it — its own
    /// identity alone when no row rests yet.
    async fn load(&self) -> Result<SuccessionLedger, StoreError>;
    /// Join `replica` into the stored rows and answer the ledger as it now
    /// reads. A write surfaces the door's refusals — an event no attested
    /// identity signed, a forked chain, and the transient no-tip refusal
    /// (offline, no escrow target yet) — as [`StoreError::Save`]; the caller's
    /// leg stays owed and re-runs.
    async fn merge(&self, replica: SuccessionLedger) -> Result<SuccessionLedger, StoreError>;
    /// **The succession write** — re-point the ledger's chain from the
    /// ATTESTED `retired` identity (a predecessor whose key this runtime
    /// holds, never an id a row asserts) to this runtime's own. `Ok(false)`
    /// when the chain already says so. The post-store-ready pass runs it for
    /// every attested predecessor before any other ledger write: until it
    /// lands the READ fold links no predecessor, so their events are
    /// invisible and a mark write's chain forks from the stored one.
    async fn repoint(&self, retired: ActorId) -> Result<bool, StoreError>;
    /// **The grant-mark raise** — an `Open` mark keyed on
    /// `(grant, predecessor)` for every live grant whose latest event
    /// `predecessor` signed. Idempotent by key: a decided mark keeps its
    /// verdict. `Ok(false)` when every mark already rests.
    async fn raise_grant_marks(&self, predecessor: ActorId) -> Result<bool, StoreError>;
}

/// The **deployment-seed custody** persistence seam —
/// `fauna.state.deployment-seeds`, one row per custodied box
/// (`nest/box-recovery.md` § The plane-era recovery floor → *(c) The
/// writes*; the kind's rows and merge rule: `config-dissolution.md`'s kinds
/// table). Declared here beside
/// [`SuccessionLedgerStore`], and for the same reason: the custody leg and the
/// plane rotation drive (`crate::custody_leg`) inject it, and nobody depends
/// upward on the account plane.
///
/// Production implements it **directly on the account-store handle**
/// (`fauna_account_plane`'s `impl DeploymentSeedStore for AccountStoreHandle`).
/// A read is the handle's fold over the per-box rows; a write is a per-row
/// join through the fleet plane's writer door, so there is no CAS base and a
/// stale replica drops nothing.
///
/// Same `Send` split as [`SuccessionLedgerStore`].
/// **The succession cut's custody arm** as the post-store-ready aftermath pass
/// reaches it (`writer-signed-change-records.md` ruling (11)(a)): re-mint every
/// owned set whose live nonce the successor did not mint, over the account's
/// folder-key custody and never over the nest's listing. Declared here so the
/// recovery pass injects it without depending on the folders crate, which sits
/// above it; `fauna_client_folders::SetCustodyCut` is the one implementation,
/// over the custody store and the folders client every host already holds.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait FolderCustodyCut: MaybeSendSync {
    /// Cut as `identity` — the successor this pass serves. Answers how many
    /// sets were re-minted; an `Err` (custody unreadable, a push the nest
    /// refused) leaves the rest to the next pass and the launch reconcile.
    async fn cut(&self, identity: ActorId) -> Result<usize, StoreError>;
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait DeploymentSeedStore: MaybeSendSync {
    /// The account's custody map as it now reads — superseded entries
    /// included; empty when no row rests yet.
    async fn seeds(&self) -> Result<Vec<DeploymentSeedEntry>, StoreError>;
    /// Join `replica` into the stored rows and answer the map as it now
    /// reads. The door's refusals — an entry whose seed is not its id's
    /// preimage, and the transient no-tip refusal (no generation tip resolves
    /// yet) — surface as [`StoreError::Save`]; the caller's write stays owed.
    async fn merge_seeds(
        &self,
        replica: Vec<DeploymentSeedEntry>,
    ) -> Result<Vec<DeploymentSeedEntry>, StoreError>;
    /// Whether the row for `nest_actor_id` rests AND this runtime owes the
    /// bound nest no write of it — every write of that row this device made
    /// sits at or below its own published high-water. `false` for a row
    /// that does not rest. The rotation drive polls it between its custody
    /// merge and its dispatch (`box-recovery.md` § The ceremony: custody
    /// precedes dispatch, and the custody that counts is the one the box
    /// itself holds).
    async fn seed_published(&self, nest_actor_id: [u8; 32]) -> Result<bool, StoreError>;
}

/// The **custody-ceremony** persistence seam — `fauna.state.custody-ceremony`,
/// one row per ceremony side-record (`config-dissolution.md`, the kinds table; the
/// concept: `account-data-plane.md` § Replica posture → *The custody grant +
/// ceremony*). Declared here beside [`SuccessionLedgerStore`] for the same
/// reason: the ceremony machine (`fauna-client-capabilities`), its
/// conversations sink and the custody acts inject it, and nobody depends
/// upward on the account plane.
///
/// Production implements it **directly on the account-store handle**
/// (`fauna_account_plane`'s `impl CustodyCeremonyStore for AccountStoreHandle`);
/// a wasm chunk other than the core one reaches it through the account port
/// (`crate::custody_port`). A read is the handle's fold over the per-record
/// rows; a write is a per-record join through the fleet plane's writer door
/// (`CustodyConfig::merge`'s halves), so there is no CAS base, a stale replica
/// drops nothing, and nothing is ever removed.
///
/// Same `Send` split as [`SuccessionLedgerStore`].
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait CustodyCeremonyStore: MaybeSendSync {
    /// Every ceremony this account runs, as owner and as host, folded —
    /// empty when no row rests yet.
    async fn custody(&self) -> Result<CustodyConfig, StoreError>;
    /// Join `replica` into the stored rows and answer the state as it now
    /// reads. The door's refusals — the transient no-tip refusal (no
    /// generation tip resolves yet), the runtime gone — surface as
    /// [`StoreError::Save`]; the caller's write stays owed.
    async fn merge_custody(&self, replica: CustodyConfig) -> Result<CustodyConfig, StoreError>;
}

/// The **followed-public-folders** persistence seam — `fauna.state.follows`,
/// one row per followed folder (`config-dissolution.md`, the kinds table; the
/// concept: `folders.md`
/// § Publicly-synced follow). Declared here beside [`CustodyCeremonyStore`]
/// for the same reason: the follow recipes (`fauna-client-folders`), the
/// followed-folders source (`fauna-devices-machine`) and every app's face
/// inject it, and nobody depends upward on the account plane.
///
/// Production implements it **directly on the account-store handle**
/// (`fauna_account_plane`'s `impl FollowsStore for AccountStoreHandle`); a wasm
/// chunk other than the core one reaches it through the account port
/// (`crate::follows_port`). A read is the handle's fold over the per-folder
/// rows; a write is one row — a put (latest-wins on its own stamp) or a
/// stamped tombstone — so there is no CAS base and a concurrent follow of
/// another folder on another device is never touched.
///
/// Same `Send` split as [`SuccessionLedgerStore`].
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait FollowsStore: MaybeSendSync {
    /// Every followed folder, in canonical `(home_nest_url, folder_id)` order —
    /// empty when none is followed.
    async fn follows(&self) -> Result<FollowsConfig, StoreError>;
    /// Follow `follow`'s folder, or refresh the follow in place. Whether
    /// anything was written (`false` when the stored row already equals it).
    /// The door's refusals — the transient no-tip refusal, the runtime gone —
    /// surface as [`StoreError::Save`].
    async fn put_follow(&self, follow: FollowedFolder) -> Result<bool, StoreError>;
    /// Unfollow the folder at `(home_nest_url, folder_id)`. Whether anything
    /// was written (`false` when it is not followed, which is success).
    async fn unfollow(&self, home_nest_url: String, folder_id: i64) -> Result<bool, StoreError>;
}

/// The **preference cluster's read seam** — the four delegable preference
/// records (`fauna.state.moderation`, `.personalization`, `.delegation`;
/// `config-dissolution.md`, the E1 cluster), for a shared consumer that is not a preference page: the feed's
/// muted-words scorer and engagement opt-ins, the `index` lease loop's pins.
/// Declared here, beside [`FollowsStore`], so those consumers inject it and
/// nobody depends upward on the account plane.
///
/// Production implements it on the account-store handle and on the seat's
/// waiting source (`fauna_account_plane`'s `impl PreferenceStore for
/// AccountStoreHandle` / `SeatAccountStore`); a read crosses the store's
/// first-pass barrier. **Plane-only:** with no runtime the read fails — there is
/// no other copy to consult.
///
/// Same `Send` split as [`SuccessionLedgerStore`].
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait PreferenceStore: MaybeSendSync {
    /// The moderation record — the muted words and the hidden-content list.
    async fn moderation(&self) -> Result<ModerationConfig, StoreError>;
    /// The trained-topic registry.
    async fn personalization(&self) -> Result<PersonalizationConfig, StoreError>;
    /// The task-delegation pins.
    async fn delegation(&self) -> Result<DelegationConfig, StoreError>;
}

/// The shared handle every consumer of [`PreferenceStore`] holds.
pub type SharedPreferenceStore = std::sync::Arc<dyn PreferenceStore>;

/// The **backup-destination state** persistence seam — `fauna.state.backup`
/// (`config-dissolution.md`, the kinds
/// table's row and *Bounded rows* → *The backup state*). Declared here beside
/// [`SuccessionLedgerStore`] for the same reason: the shared backup sequences
/// (`crate::backup_store`, `crate::backup_enroll`), the trust facet and the
/// succession aftermath's backup legs inject it, and nobody depends upward on
/// the account plane.
///
/// **Per source box** (`backup-destinations.md` § State & data shape →
/// *Destination data model*): the list is read and written for ONE box, named
/// by the source nest's identity as the caller's connection proved it
/// (`fauna_client_pair::LinkedNestsMachine::bound_nest_id`), never another
/// box's; the marks are the account's.
///
/// Production implements it **directly on the account-store handle**
/// (`fauna_account_plane`'s `impl BackupStateStore for AccountStoreHandle`).
/// A write goes through the fleet plane's writer door — no CAS base; the door
/// stamps the list above the stored row and joins each mark into its row.
/// **Born plane-only (P5):** with no handle up a read answers
/// [`LEDGER_NOT_READY`] and a write is refused; there is no
/// fallback.
///
/// Same `Send` split as [`SuccessionLedgerStore`].
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait BackupStateStore: MaybeSendSync {
    /// `source_nest`'s state: that box's list pruned of every removed
    /// destination, and every mark of the account. The empty state (stamp 0)
    /// when the box keeps no list.
    async fn backup_state(&self, source_nest: [u8; 32]) -> Result<BackupState, StoreError>;
    /// Every box's list row the account holds — the all-boxes read.
    async fn backup_destination_lists(&self) -> Result<Vec<BackupDestinationsRow>, StoreError>;
    /// Replace `source_nest`'s list and answer that box's state as it now
    /// reads. A list over the row's bounds, the transient no-tip refusal and
    /// the runtime gone surface as [`StoreError::Save`].
    async fn write_backup_destinations(
        &self,
        source_nest: [u8; 32],
        backup: BackupConfig,
    ) -> Result<BackupState, StoreError>;
    /// Join `marks` into their rows and answer every mark of the account.
    async fn merge_destination_marks(
        &self,
        marks: Vec<DestinationUnattestedMark>,
    ) -> Result<Vec<DestinationUnattestedMark>, StoreError>;
}

/// The refusal a [`ResolvingLedgerStore`] answers while the host's account
/// store is not up — before its assembly completed, after a sign-out, or when
/// it failed (it is best-effort by design). A caller's leg stays owed.
pub const LEDGER_NOT_READY: &str = "the account store is not ready yet";

/// The not-ready refusal a read answers while the account store is held for
/// another of the account's devices: a listed replica holds rows under a
/// generation whose key only a sibling device can still hand it
/// (`account-client-lifecycle.md` § The client-side lifecycle → *The first
/// listing*, clause (5), the sibling source). Transient like
/// [`LEDGER_NOT_READY`], and recognised by [`StoreError::is_not_ready`]; a
/// surface renders it with its own reason, not the needs-nest one.
pub const LEDGER_AWAITING_SIBLING: &str =
    "the account store is waiting for another of the account's devices";

/// How long a [`ResolvingLedgerStore`] call waits for the host's account store
/// to come up before answering [`LEDGER_NOT_READY`].
///
/// **Why it waits at all.** Every host assembles its store asynchronously after
/// sign-in, and some ledger writes fire in exactly that window — onboarding's
/// one-tap "trust this box" mint runs at the `LoggedIn` handoff, before any
/// store exists. Refusing at once would silently mint nothing; the wait covers
/// the assembly (typically well under a second). A store that never comes
/// (the assembly is best-effort and can fail) costs a caller this bound once
/// per call and then the same refusal as before.
pub const LEDGER_READY_WAIT: core::time::Duration = core::time::Duration::from_secs(10);

/// The poll step of [`LEDGER_READY_WAIT`].
const LEDGER_READY_POLL: core::time::Duration = core::time::Duration::from_millis(100);

/// A [`SuccessionLedgerStore`] that resolves the host's account-store handle
/// **per call** — the seam a feature machine built before the store is up
/// holds (the Nests page's trust facet, the mail-settings and labeler
/// machines, the custody drive), so no machine is rebuilt when the store
/// arrives and no app writes per-call glue. `resolve` is the host's own
/// accessor (`account_runtime::handle` on fauna-ffi, linux and web; tui's
/// App-owned slot). While it answers `None` an async call waits up to
/// [`LEDGER_READY_WAIT`] for it, then answers [`LEDGER_NOT_READY`];
/// [`SuccessionLedgerStore::self_actor`], being synchronous, never waits.
pub struct ResolvingLedgerStore<F> {
    resolve: F,
}

impl<F> ResolvingLedgerStore<F> {
    /// Resolve the handle through `resolve` at every call.
    pub fn new(resolve: F) -> Self {
        Self { resolve }
    }
}

fn not_ready(save: bool) -> StoreError {
    if save {
        StoreError::Save(LEDGER_NOT_READY.into())
    } else {
        StoreError::Load(LEDGER_NOT_READY.into())
    }
}

impl<F, H> ResolvingLedgerStore<F>
where
    F: Fn() -> Option<H>,
{
    /// The handle, waiting out an assembly still in flight.
    async fn current(&self, save: bool) -> Result<H, StoreError> {
        let polls = LEDGER_READY_WAIT.as_millis() / LEDGER_READY_POLL.as_millis();
        for _ in 0..polls {
            if let Some(handle) = (self.resolve)() {
                return Ok(handle);
            }
            fauna_sleep::sleep(LEDGER_READY_POLL).await;
        }
        (self.resolve)().ok_or_else(|| not_ready(save))
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<F, H> SuccessionLedgerStore for ResolvingLedgerStore<F>
where
    F: Fn() -> Option<H> + MaybeSendSync,
    H: SuccessionLedgerStore,
{
    fn self_actor(&self) -> Result<ActorId, StoreError> {
        (self.resolve)()
            .ok_or_else(|| not_ready(false))?
            .self_actor()
    }

    async fn load(&self) -> Result<SuccessionLedger, StoreError> {
        self.current(false).await?.load().await
    }

    async fn merge(&self, replica: SuccessionLedger) -> Result<SuccessionLedger, StoreError> {
        self.current(true).await?.merge(replica).await
    }

    async fn repoint(&self, retired: ActorId) -> Result<bool, StoreError> {
        self.current(true).await?.repoint(retired).await
    }

    async fn raise_grant_marks(&self, predecessor: ActorId) -> Result<bool, StoreError> {
        self.current(true)
            .await?
            .raise_grant_marks(predecessor)
            .await
    }
}

/// The custody-ceremony seam, resolved per call exactly as the ledger seam
/// above — the custody drive, its conversations sink and the custody acts are
/// built before the store is up, like the ledger's consumers.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<F, H> CustodyCeremonyStore for ResolvingLedgerStore<F>
where
    F: Fn() -> Option<H> + MaybeSendSync,
    H: CustodyCeremonyStore,
{
    async fn custody(&self) -> Result<CustodyConfig, StoreError> {
        self.current(false).await?.custody().await
    }

    async fn merge_custody(&self, replica: CustodyConfig) -> Result<CustodyConfig, StoreError> {
        self.current(true).await?.merge_custody(replica).await
    }
}

/// The followed-folders seam, resolved per call exactly as the ledger seam
/// above — the followed-folders source and the follow faces are built before
/// the store is up.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<F, H> FollowsStore for ResolvingLedgerStore<F>
where
    F: Fn() -> Option<H> + MaybeSendSync,
    H: FollowsStore,
{
    async fn follows(&self) -> Result<FollowsConfig, StoreError> {
        self.current(false).await?.follows().await
    }

    async fn put_follow(&self, follow: FollowedFolder) -> Result<bool, StoreError> {
        self.current(true).await?.put_follow(follow).await
    }

    async fn unfollow(&self, home_nest_url: String, folder_id: i64) -> Result<bool, StoreError> {
        self.current(true)
            .await?
            .unfollow(home_nest_url, folder_id)
            .await
    }
}

/// The kind-manifest seam, resolved per call exactly as the ledger seam above
/// — the consent machines that admit a third-party app's kinds are built
/// before the store is up, like the ledger's consumers.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<F, H> KindManifestStore for ResolvingLedgerStore<F>
where
    F: Fn() -> Option<H> + MaybeSendSync,
    H: KindManifestStore,
{
    async fn admitted_kinds(
        &self,
    ) -> Result<fauna_protocol::merge_policy::AdmittedKinds, StoreError> {
        self.current(false).await?.admitted_kinds().await
    }

    async fn publish(
        &self,
        client_id: &str,
        manifest: &fauna_protocol::kind_manifest::VerifiedManifest,
        admitted_at_ms: i64,
    ) -> Result<(), StoreError> {
        self.current(true)
            .await?
            .publish(client_id, manifest, admitted_at_ms)
            .await
    }
}

/// The deployment-seed custody seam, resolved per call exactly as the ledger
/// seam above — a host that builds its machines before the store is up runs
/// the custody leg and the plane rotation drive through it
/// (`nest/box-recovery.md` § The plane-era recovery floor, *(c)*).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<F, H> DeploymentSeedStore for ResolvingLedgerStore<F>
where
    F: Fn() -> Option<H> + MaybeSendSync,
    H: DeploymentSeedStore,
{
    async fn seeds(&self) -> Result<Vec<DeploymentSeedEntry>, StoreError> {
        self.current(false).await?.seeds().await
    }

    async fn merge_seeds(
        &self,
        replica: Vec<DeploymentSeedEntry>,
    ) -> Result<Vec<DeploymentSeedEntry>, StoreError> {
        self.current(true).await?.merge_seeds(replica).await
    }

    async fn seed_published(&self, nest_actor_id: [u8; 32]) -> Result<bool, StoreError> {
        self.current(false)
            .await?
            .seed_published(nest_actor_id)
            .await
    }
}

/// The backup-state seam, resolved per call exactly as the ledger seam above
/// — the Backups page's sequences and the trust facet run on machines built
/// before the store is up.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<F, H> BackupStateStore for ResolvingLedgerStore<F>
where
    F: Fn() -> Option<H> + MaybeSendSync,
    H: BackupStateStore,
{
    async fn backup_state(&self, source_nest: [u8; 32]) -> Result<BackupState, StoreError> {
        self.current(false).await?.backup_state(source_nest).await
    }

    async fn backup_destination_lists(&self) -> Result<Vec<BackupDestinationsRow>, StoreError> {
        self.current(false).await?.backup_destination_lists().await
    }

    async fn write_backup_destinations(
        &self,
        source_nest: [u8; 32],
        backup: BackupConfig,
    ) -> Result<BackupState, StoreError> {
        self.current(true)
            .await?
            .write_backup_destinations(source_nest, backup)
            .await
    }

    async fn merge_destination_marks(
        &self,
        marks: Vec<DestinationUnattestedMark>,
    ) -> Result<Vec<DestinationUnattestedMark>, StoreError> {
        self.current(true)
            .await?
            .merge_destination_marks(marks)
            .await
    }
}

/// A [`SuccessionLedgerStore`] with **no account store behind it** — for a
/// machine built only for a capability that never touches the ledger (a
/// mail-settings machine serving as the MSEK key custody of the spam or export
/// page). Every call answers [`LEDGER_NOT_READY`], so a ledger write reached
/// through such a machine fails loudly instead of recording nowhere.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLedgerStore;

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl SuccessionLedgerStore for NoLedgerStore {
    fn self_actor(&self) -> Result<ActorId, StoreError> {
        Err(StoreError::Load(LEDGER_NOT_READY.into()))
    }

    async fn load(&self) -> Result<SuccessionLedger, StoreError> {
        Err(StoreError::Load(LEDGER_NOT_READY.into()))
    }

    async fn merge(&self, _replica: SuccessionLedger) -> Result<SuccessionLedger, StoreError> {
        Err(StoreError::Save(LEDGER_NOT_READY.into()))
    }

    async fn repoint(&self, _retired: ActorId) -> Result<bool, StoreError> {
        Err(StoreError::Save(LEDGER_NOT_READY.into()))
    }

    async fn raise_grant_marks(&self, _predecessor: ActorId) -> Result<bool, StoreError> {
        Err(StoreError::Save(LEDGER_NOT_READY.into()))
    }
}

/// No store, no ceremony state: a read or a write through it fails loudly
/// ([`LEDGER_NOT_READY`]), so a drive with no store behind it records nothing
/// and leaves every act owed for a later pass.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl CustodyCeremonyStore for NoLedgerStore {
    async fn custody(&self) -> Result<CustodyConfig, StoreError> {
        Err(StoreError::Load(LEDGER_NOT_READY.into()))
    }

    async fn merge_custody(&self, _replica: CustodyConfig) -> Result<CustodyConfig, StoreError> {
        Err(StoreError::Save(LEDGER_NOT_READY.into()))
    }
}

/// No store, no follows: a read or a write through it fails loudly
/// ([`LEDGER_NOT_READY`]) — a follow made with no store behind it is refused,
/// never recorded nowhere, and an unreadable list is never "no follows".
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl FollowsStore for NoLedgerStore {
    async fn follows(&self) -> Result<FollowsConfig, StoreError> {
        Err(StoreError::Load(LEDGER_NOT_READY.into()))
    }

    async fn put_follow(&self, _follow: FollowedFolder) -> Result<bool, StoreError> {
        Err(StoreError::Save(LEDGER_NOT_READY.into()))
    }

    async fn unfollow(&self, _home_nest_url: String, _folder_id: i64) -> Result<bool, StoreError> {
        Err(StoreError::Save(LEDGER_NOT_READY.into()))
    }
}

/// The **mail custody** persistence seam — `fauna.state.mail`
/// (`config-dissolution.md` § Phases and gates → *Bounded rows* → *The mail
/// plane*): the account's ONE mail-state row (`self`) and one row per
/// credential (`credential/<credential_id>`). Declared here beside
/// [`SuccessionLedgerStore`] for its reason: the consumers (the
/// mail-settings machine, grant minting, the DAV context, the mail keys) sit
/// below the account plane, so nobody depends upward on it.
///
/// Production implements it **directly on the account-store handle**
/// (`fauna_account_plane`'s `impl MailStore for AccountStoreHandle`) and on
/// the seat-sourced `AccountMailStore`, which waits a bounded while for a
/// runtime that has not come up yet. The kind is plane-only.
///
/// **Every write is a per-row read-join-put** at the door, stamped strictly
/// above the stored row, so a write never loses what the store gained since
/// the caller last read, and there is no deletion — which is why the state
/// row's `msek` can never be cleared (present-wins, the MSEK being
/// irrecoverable) and a revoke or a burn is a monotone marker. Each write
/// answers whether anything moved; the door's refusals (the transient no-tip
/// refusal included) surface as [`StoreError::Save`].
///
/// Same `Send` split as [`SuccessionLedgerStore`].
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait MailStore: MaybeSendSync {
    /// The READ fold — the composite `MailConfig`: the state row plus every
    /// credential that is not revoked (burned rows shown), oldest first; the
    /// default (mail never enabled) when no row rests.
    async fn load(&self) -> Result<MailConfig, StoreError>;
    /// The rows themselves, revoked credentials INCLUDED — what the spent-id
    /// rule ([`MailRows::spent_credential_ids`]) and the derived owed set
    /// ([`MailRows::owed_rewrap`]) read. [`MailRows::config`] is [`Self::load`].
    async fn load_rows(&self) -> Result<MailRows, StoreError>;
    /// Write the state row: `state`'s content (its `updated_at` ignored)
    /// joined into what is stored.
    async fn write_state(&self, state: MailStateRow) -> Result<bool, StoreError>;
    /// Put one credential at its own row, joined into what is stored — a
    /// marker the store holds is never undone.
    async fn put_credential(&self, credential: MailCredential) -> Result<bool, StoreError>;
    /// Record the MSEK generation `credential_id`'s nest-side blobs are
    /// wrapped under (a no-op on an absent, marked or already-current row).
    async fn mark_wrapped(
        &self,
        credential_id: String,
        fingerprint: MsekFingerprint,
    ) -> Result<bool, StoreError>;
    /// Soft-revoke `credential_id`: the marker, the generation cleared and the
    /// secret emptied together; the id stays spent.
    async fn revoke(&self, credential_id: String) -> Result<bool, StoreError>;
}

/// The **kind-manifest** seam — `fauna.state.kind-manifest`, one row per
/// consented third-party document, keyed by its `client_id`
/// (`third-party-kinds.md` § The kinds vocabulary → *The plane row*). The
/// consent-time mint (`fauna_client_capabilities::ext_consent`) reads the
/// account's overlay through it (is a foreign kind already admitted?) and
/// publishes the manifest it verified, before it wraps a key.
///
/// Production implements it **directly on the account-store handle**
/// (`fauna_account_plane`'s `impl KindManifestStore for AccountStoreHandle`):
/// the read is the handle's fold over the rows, each re-verified against its
/// own `client_id`'s host; the write is a whole-row latest-wins put through
/// the fleet plane's writer door, whose refusals (no generation tip yet, the
/// runtime gone) surface as [`StoreError::Save`].
///
/// Same `Send` split as [`SuccessionLedgerStore`].
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait KindManifestStore: MaybeSendSync {
    /// Every kind the account's verifying rows admit — the registry overlay,
    /// empty when no row rests.
    async fn admitted_kinds(
        &self,
    ) -> Result<fauna_protocol::merge_policy::AdmittedKinds, StoreError>;
    /// Publish `manifest` as the account's row for `client_id`, stamped now.
    /// `manifest` was verified against `client_id`'s own host; a mismatched
    /// pair is refused rather than published as a row that admits nothing.
    async fn publish(
        &self,
        client_id: &str,
        manifest: &fauna_protocol::kind_manifest::VerifiedManifest,
        admitted_at_ms: i64,
    ) -> Result<(), StoreError>;
}

/// No store, no backup state: a read or a write through it fails loudly
/// ([`LEDGER_NOT_READY`]) — born plane-only, so there is nothing to fall back
/// to.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl BackupStateStore for NoLedgerStore {
    async fn backup_state(&self, _source_nest: [u8; 32]) -> Result<BackupState, StoreError> {
        Err(StoreError::Load(LEDGER_NOT_READY.into()))
    }

    async fn backup_destination_lists(&self) -> Result<Vec<BackupDestinationsRow>, StoreError> {
        Err(StoreError::Load(LEDGER_NOT_READY.into()))
    }

    async fn write_backup_destinations(
        &self,
        _source_nest: [u8; 32],
        _backup: BackupConfig,
    ) -> Result<BackupState, StoreError> {
        Err(StoreError::Save(LEDGER_NOT_READY.into()))
    }

    async fn merge_destination_marks(
        &self,
        _marks: Vec<DestinationUnattestedMark>,
    ) -> Result<Vec<DestinationUnattestedMark>, StoreError> {
        Err(StoreError::Save(LEDGER_NOT_READY.into()))
    }
}
