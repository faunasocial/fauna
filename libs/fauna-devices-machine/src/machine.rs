//! The page-level Devices state machine.
//!
//! Mirrors `fauna_folders_machine::FolderWizardMachine`: each app holds an
//! `Arc<DevicesMachine>`, observes via a registered `DevicesObserver`, drives
//! gestures, and renders the whole Devices page off `snapshot()`. Where the
//! wizard machine owns the *creation* flow, this owns the *page*: the device /
//! folder / conflict reads and the page-level write gestures, plus the
//! embedded wizard (`Option<Arc<FolderWizardMachine>>`, surfaced as
//! `DevicesSnapshot.wizard`).
//!
//! Uses `std::sync::Mutex` (not tokio's) so getters and sync gestures work from
//! any thread context — including UI threads and `#[tokio::test]`. The async
//! gestures snapshot/clone under the lock, drop it, do IO, then re-acquire; the
//! lock is never held across an `await`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fauna_core::file_download::FileDownloadKeys;
pub use fauna_core::fleet_removal::{
    FleetMembersView, FleetRemovalRefusal, NestDeletion, UnaccountedMember,
};
use fauna_core::label_custody::LabelCustody;
use fauna_core::localized::LocalizedText;
use fauna_core::path_crypto::{LabelField, SealedLabelRender};
use fauna_folders_machine::{DeviceOption, FolderWizardMachine, FolderWizardObserver};
use fauna_protocol::folders::FolderSummary as WireFolderSummary;
use fauna_protocol::folders::SyncConflict;
use fauna_protocol::sync::SyncDevice;

use crate::nest_api::{CandidateVerdict, DevicesNestApi, WizardFactory};
use crate::observer::DevicesObserver;
use crate::p2p_participation::{OwnRowAnswer, is_own_row, p2p_participation_paint};
use crate::snapshots::{
    ConflictSummary, DeviceSummary, DevicesSnapshot, FleetMemberSummary, FolderSummary,
    FollowedFolderSummary, name_skipping_devices,
};

/// i18n key for a page read (`refresh()`) failure ("Failed to load devices: {message}").
const REFRESH_ERROR_KEY: &str = "devices.error_refresh";
/// i18n key for a `remove_device` failure.
const REMOVE_DEVICE_ERROR_KEY: &str = "devices.error_remove_device";
/// i18n key for a `set_p2p_participation` failure (`{message}`).
const SET_P2P_PARTICIPATION_ERROR_KEY: &str = "devices.error_set_p2p_participation";
/// i18n key for the one refusal the page makes itself: turning ANOTHER
/// device's peer transfers on — enabling is local consent on that device
/// (`behavior/p2p.md` § Per-device participation), so nothing is sent.
const P2P_REMOTE_ENABLE_KEY: &str = "devices.error_p2p_remote_enable";
/// i18n key for `remove_device`'s fleet-scope leg failing (the nest deletion
/// already succeeded — see [`DevicesMachine::remove_device`]).
const REMOVE_FLEET_DEVICE_ERROR_KEY: &str = "devices.error_remove_fleet_device";
/// i18n key for a removal refused because its target resolves to this device
/// itself ([`FleetRemovalRefusal::OwnDevice`]) — nothing was removed.
const REMOVE_OWN_DEVICE_ERROR_KEY: &str = "devices.error_remove_own_device";
/// i18n key for a removal refused because the row's fleet member could not be
/// verified from client-held truth ([`FleetRemovalRefusal::NotAMember`]) —
/// nothing was removed; a retry can clear it once this replica has synced the
/// member.
const REMOVE_UNVERIFIED_DEVICE_ERROR_KEY: &str = "devices.error_remove_unverified_device";
/// i18n key for a removal refused because the row's member states a different
/// row ([`FleetRemovalRefusal::RowMismatch`]) — nothing was removed, and no
/// retry clears it, so its copy never says "try again".
const REMOVE_ROW_MISMATCH_ERROR_KEY: &str = "devices.error_remove_row_mismatch";
/// i18n key for a `delete_folder` failure.
const DELETE_FOLDER_ERROR_KEY: &str = "devices.error_delete_folder";
/// i18n key for a `resolve_conflict` failure.
const RESOLVE_CONFLICT_ERROR_KEY: &str = "devices.error_resolve_conflict";
/// i18n key for a `set_folder_paths` failure.
const SAVE_PATHS_ERROR_KEY: &str = "devices.error_save_paths";
/// i18n key for a `set_folder_conflict_policy` failure.
const SET_CONFLICT_POLICY_ERROR_KEY: &str = "devices.error_set_conflict_policy";
const SET_NEST_PLACE_ERROR_KEY: &str = "devices.error_set_nest_place";
/// i18n key for a `set_folder_audience` failure (`folder-audience-select`).
const SET_AUDIENCE_ERROR_KEY: &str = "devices.error_set_audience";
/// i18n key for a `set_folder_website_enabled` failure (`folder-website-toggle`)
/// — the key tui already surfaces for the same failure, reused rather than
/// twinned (priority #1).
const SET_WEBSITE_ERROR_KEY: &str = "devices.error_serve_website";
/// i18n key for a `set_folder_residency` failure (`folder-nest-residency-select`
/// / `folder-residency-confirm`) — the key tui already surfaces for the same
/// failure, reused rather than twinned (priority #1).
const SET_RESIDENCY_ERROR_KEY: &str = "devices.error_set_residency";
/// i18n key for a `set_folder_place` failure (the place editor's checkboxes).
const SET_PLACE_ERROR_KEY: &str = "devices.error_set_place";
/// i18n key for a `use_other_version` (review-list re-point) failure.
const USE_OTHER_VERSION_ERROR_KEY: &str = "devices.error_use_other_version";
/// i18n key for a `use_other_version` the judged history does not vouch for:
/// no admitted version of the file carries the candidate's manifest.
const OTHER_VERSION_UNVERIFIED_KEY: &str = "devices.error_other_version_unverified";
/// i18n key for a `use_other_version` whose version must be opened and
/// re-sealed — the file's version history performs that restore.
const OTHER_VERSION_NEEDS_HISTORY_KEY: &str = "devices.error_other_version_needs_history";

/// A minimal, client-provided query into the local MLS engine: whether this client
/// has **actually joined** the MLS group of a shared folder (addressed by the raw
/// `mls_group_id`, hex). The Devices page uses it as the load-bearing **join-filter**
/// for B3 member-visible rows: the nest returns *rostered* members (it cannot observe
/// a client-side MLS join), so a `role == "member"` row must be shown **only if** the
/// client has joined the group — else a stranger's un-accepted knock would surface in
/// the folders list unbidden (`docs/goal/ui/folders.md` § Sharing, lines 128/139).
///
/// `fauna-devices-machine` has no `fauna-mls` dependency, so the derivation
/// `ChannelId::from_group_id(mls_group_id)` + `MlsEngine::has_group` lives in the
/// client's impl (which holds the conversations-rail engine). Centralizing the filter
/// **here** — instead of in each of the 6 client renders — is the anti-abuse
/// control: a client that forgot the filter is
/// exactly how the list-surface exposure arose (the list twin of the web
/// gate-bypass).
pub trait MlsQuery: Send + Sync {
    /// Whether the local MLS engine holds the group for the shared set with this raw
    /// hex `mls_group_id` — i.e. the client has joined it.
    fn is_joined_shared_set(&self, mls_group_id_hex: &str) -> bool;
}

/// One **foreign** (cross-nest) shared set from the member's own `fauna.state.folder-keys`
/// foreign-set row (`fauna_core::data::ForeignFolder`, written at accept time) — the
/// machine's source row for sets whose home is ANOTHER nest, which the own
/// nest's `fauna.folders.list` therefore cannot return (Phase 2 client
/// read-side; `docs/goal/ui/folders.md` § Sharing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignSetRow {
    /// The home-nest-resolved display name; `None` when the seal was
    /// unopenable (the row renders the client's unknown-set fallback).
    pub set_name: Option<String>,
    /// Hex-encoded raw MLS group id — the same identity axis as
    /// [`FolderSummary::mls_group_id`] (join-filter + leave both key on it).
    pub mls_group_id_hex: String,
    /// The set's home-nest base URL — every read/leave for this set relays
    /// there (surfaced as [`FolderSummary::home_nest_url`]).
    pub home_nest_url: String,
    /// The member's access grant on the set (`"reader"`/`"writer"`), as the
    /// **home** nest resolved it — seeded on the Welcome relay, refreshed by the
    /// `caller_access` stamp on federated read replies
    /// (`docs/goal/architecture/federation.md` § Cross-nest → *Recipient-side
    /// access discovery*). Surfaced as [`FolderSummary::access`], which is what
    /// splits a client's row into the writer binding UI vs. a read-only row.
    ///
    /// **Advisory-for-UI only, never an authorization input** — every write is
    /// gated on the home nest's own role row, and a bind is verified by an eager
    /// `write_token.get`. `None` = unknown ⇒ reader (fail-safe).
    pub access: Option<String>,
    /// The folder's residency as the **home** nest last stamped it on the
    /// member's custody record (`ForeignFolder::metadata_only_residency`):
    /// `Some(true)` metadata-only, `Some(false)` full, `None` unknown.
    /// Surfaced as [`FolderSummary::residency`] only when stated.
    pub metadata_only_residency: Option<bool>,
}

/// Reads the member's foreign-set records for [`DevicesMachine::refresh`]'s
/// list union. Injected post-construction like [`MlsQuery`]
/// ([`DevicesMachine::set_foreign_sets_source`]) because the concrete reader
/// holds the account-plane handle the builder doesn't.
/// Unwired ⇒ no foreign rows (web today; the join-filter still applies to any
/// row that does arrive). Failures return an empty list — the page shows the
/// same-nest rows rather than erroring (foreign rows reappear next refresh).
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait ForeignSetsSource: fauna_core::MaybeSendSync {
    /// The current foreign-set records, best-effort.
    async fn foreign_sets(&self) -> Vec<ForeignSetRow>;
}

/// Reads the user's followed public folders for [`DevicesMachine::refresh`]
/// (`docs/goal/ui/folders.md` § Following a public folder). Injected
/// post-construction exactly like [`ForeignSetsSource`], and for the same
/// reason: the concrete reader holds the account-plane handle *and* the public-fetch
/// connection, neither of which the builder has.
///
/// The **availability probe belongs to the implementor**, not to this machine:
/// deciding "still served" means running
/// `fauna_client_folders::public_follow::fetch_followed_changes` and
/// distinguishing the plane's refusal from a transport fault — a transport
/// concern, and this machine is deliberately transport-free.
///
/// Unwired ⇒ no followed rows. Failures return an empty list, matching
/// [`ForeignSetsSource`]: the page keeps rendering its own folders rather than
/// erroring, and the follows reappear next refresh.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait FollowedFoldersSource: fauna_core::MaybeSendSync {
    /// The user's current followed folders, best-effort, each already carrying
    /// its availability verdict.
    async fn followed_folders(&self) -> Vec<FollowedFolderSummary>;
}

/// The account-data plane's fleet-scope removal door
/// (`AccountStoreHandle::settle_fleet_removal`, whose `Gone` outcome writes
/// the `Removed` rows through `fleet_removal::write_removed`) — the devices page's
/// remove-device action's *second* leg, beside `fauna.sync.devices.delete`
/// (`docs/goal/behavior/devices.md` § Removing a Device;
/// `docs/goal/architecture/account-data-taxonomy.md` § The generation
/// machinery → *Fleet-scope reclamation*, clause (4)). Injected
/// post-construction like [`MlsQuery`] and for the same reason:
/// `fauna-devices-machine` stays wasm-clean and does not depend on the
/// account plane, so each native app's build glue wires the one shared impl
/// (`fauna_account_seams::fleet_removal`, re-exported at
/// `fauna_client_account_runtime::fleet_removal`) over its own
/// `AccountStoreHandle`, and web wires [`crate::port::PortFleetRemoval`],
/// whose calls the core chunk answers through that same impl
/// (`account-client-lifecycle.md` § The client-side lifecycle → *The account
/// port*).
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait FleetRemoval: fauna_core::MaybeSendSync {
    /// Which fleet members removing nest row `row_device_id` must exclude —
    /// resolved from **client-held truth** (the members' own sealed row
    /// statements and the verified fleet view), with `claimed_principal` — the
    /// principal the nest put on the row — only ever a claim to check
    /// (`fauna_core::fleet_removal` owns the rule). Called *before* the nest
    /// deletion: a refusal leaves everything in place and the user is told the
    /// device was not removed. An empty `Ok` means the row names no fleet
    /// member and the nest deletion proceeds alone.
    async fn resolve_removal(
        &self,
        row_device_id: &str,
        claimed_principal: Option<[u8; 32]>,
    ) -> Result<Vec<[u8; 32]>, FleetRemovalRefusal>;

    /// Stage the **durable intent** to exclude `targets` — the ids
    /// [`Self::resolve_removal`] answered, never a principal read off the
    /// nest's row — with nest row `row_device_id`. Called *before* the nest
    /// deletion, and only with a non-empty `targets`. An error means nothing
    /// persisted: the page deletes nothing, since a removal it cannot promise
    /// to finish is the leak this seam exists to close
    /// (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
    /// reclamation*, clause (4), *The completion rule*).
    async fn stage_removal(
        &self,
        row_device_id: &str,
        targets: Vec<[u8; 32]>,
    ) -> Result<(), String>;

    /// Settle the staged removal on what the nest deletion came to: `Gone`
    /// journals the `Removed` rows and clears the intent, `Kept` clears it
    /// unwritten, `Unknown` leaves it for the runtime's reconcile. An error is
    /// surfaced on `error-message` (e2e convention 11) but loses nothing — the
    /// intent is still staged and the runtime finishes it with no gesture.
    async fn settle_removal(
        &self,
        row_device_id: &str,
        targets: Vec<[u8; 32]>,
        outcome: NestDeletion,
    ) -> Result<(), String>;

    /// The **member-addressed door's read** (clause (4), *A disagreement is
    /// the user's to settle*): this device's own fleet id, and every verified
    /// member other than it that no roster row accounts for — a row accounts
    /// for a member only when removing that row resolves to that member
    /// alone (`fauna_core::fleet_removal::unaccounted_members`). `roster` is
    /// every row the page lists, as `(row id, claimed principal)`. Read on
    /// every [`DevicesMachine::refresh`]; an error keeps the previously listed
    /// members rather than blanking them (the runtime not being up is the
    /// usual cause, and it clears on its own).
    async fn fleet_members(
        &self,
        roster: Vec<(String, Option<[u8; 32]>)>,
    ) -> Result<FleetMembersView, String>;

    /// The member-addressed door's **one leg**: write the `Removed` row for
    /// `member`, the fleet id the user picked by its card — no nest input, no
    /// row statement read (`fauna_core::fleet_removal::resolve_member_removal`
    /// owns the rule: this device → `OwnDevice`, unverified → `NotAMember`,
    /// already removed → `Ok` and nothing written). Nothing to stage and no
    /// nest deletion to bracket; a failed write answers `Unavailable` and the
    /// member stays listed to retry from.
    async fn remove_member(&self, member: [u8; 32]) -> Result<(), FleetRemovalRefusal>;
}

/// THIS device's own peer participation — the device-local authority behind
/// `device-p2p-participation-toggle` on this device's own roster row
/// (`docs/goal/behavior/p2p.md` § Per-device participation). Injected
/// post-construction exactly like [`FleetRemoval`], for the same reason:
/// the row lives on the account runtime's store, which this wasm-clean
/// crate cannot name, so each native app's build glue wires the one shared
/// impl (`fauna_client_account_runtime::p2p_participation`) over its own
/// `AccountStoreHandle`; web leaves it unwired (no runtime, no listener,
/// no own row — its toggle is the request-off arm only).
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait P2pParticipation: fauna_core::MaybeSendSync {
    /// This device's own switch, as rested on its store. Read on every
    /// [`DevicesMachine::refresh`].
    async fn local(&self) -> Result<bool, String>;

    /// Flip this device's own switch. The listeners react through the two
    /// shared bind doors, never through this crate.
    async fn set_local(&self, on: bool) -> Result<(), String>;

    /// The roster row this device enrolled on (hex device id), the same
    /// read `device-this-mark-badge` prefers — which is how the machine
    /// tells this device's own row from a sibling's. `None` = not enrolled
    /// yet; the fleet-id fallback then decides.
    async fn own_row(&self) -> Result<Option<String>, String>;
}

/// Internal page state. In-memory only; clients read snapshots via the getter.
struct State {
    devices: Vec<DeviceSummary>,
    folders: Vec<FolderSummary>,
    /// Followed public folders — their own row kind, their own list.
    followed: Vec<FollowedFolderSummary>,
    conflicts: Vec<ConflictSummary>,
    /// The actor's web-address opt-in, `None` = unknown — see
    /// `DevicesSnapshot::website_address_enabled`.
    website_address_enabled: Option<bool>,
    /// The open folder wizard, if any. Surfaced as `DevicesSnapshot.wizard`.
    wizard: Option<Arc<FolderWizardMachine>>,
    /// Last page-level error (the `error-message` element); `None` when clear.
    error: Option<LocalizedText>,
    /// The member door's read — `DevicesSnapshot::{members, own_fleet_id,
    /// own_fingerprint}`; empty/`None` until a wired door answers.
    members: Vec<FleetMemberSummary>,
    own_fleet_id: Option<String>,
    own_fingerprint: Option<String>,
    /// `DevicesSnapshot::own_p2p_participation` — the door's read.
    own_p2p_participation: Option<bool>,
    /// The participation door's enrolled-row read at the last refresh —
    /// the first step of the own-row rule the per-row paint is drawn with.
    p2p_own_row: OwnRowAnswer,
}

impl State {
    fn new() -> Self {
        Self {
            devices: Vec::new(),
            folders: Vec::new(),
            followed: Vec::new(),
            conflicts: Vec::new(),
            website_address_enabled: None,
            wizard: None,
            error: None,
            members: Vec::new(),
            own_fleet_id: None,
            own_fingerprint: None,
            own_p2p_participation: None,
            p2p_own_row: OwnRowAnswer::NoDoor,
        }
    }
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct DevicesMachine {
    state: Mutex<State>,
    observer: Arc<dyn DevicesObserver>,
    nest_api: Arc<dyn DevicesNestApi>,
    wizard_factory: Arc<dyn WizardFactory>,
    /// The client's local MLS join-filter for B3 member-visible rows (see
    /// [`MlsQuery`]). Wired post-construction by the client's build glue via
    /// [`Self::set_mls_query`]; `None` until then ⇒ **fail-safe** (every
    /// `role == "member"` row is dropped, so a rostered-but-un-joined knock never
    /// surfaces in the list).
    mls_query: Mutex<Option<Arc<dyn MlsQuery>>>,
    /// The foreign-set (cross-nest) list source ([`ForeignSetsSource`]), wired
    /// post-construction like [`Self::mls_query`]; `None` until then ⇒ no
    /// foreign rows in the list (web today).
    foreign_sets: Mutex<Option<Arc<dyn ForeignSetsSource>>>,
    /// The followed-public-folders reader ([`FollowedFoldersSource`]), wired
    /// post-construction like [`Self::foreign_sets`]; `None` until then ⇒ no
    /// followed rows, which is every app today (no app has built the follow
    /// surface — its element IDs are still ungranted).
    followed_source: Mutex<Option<Arc<dyn FollowedFoldersSource>>>,
    /// Label-opening custody for the conflict list's **sealed-first** path
    /// render (`docs/goal/behavior/file-sync.md` § Sealed names & paths). Wired
    /// post-construction by the client's build glue
    /// ([`Self::set_label_custody`]), the same reason `mls_query` is: the
    /// concrete resolver holds the account-plane handle the builder doesn't.
    ///
    /// Empty until wired ⇒ the plaintext render (keyless custody, public folders), byte-identical to this
    /// page's behaviour before sealing. That default is what lets the seven apps
    /// adopt it in a batched sweep instead of all at once — but it is also why
    /// an un-wired app renders nothing for a conflict once the plaintext column
    /// is scrubbed at the flip.
    label_custody: Mutex<LabelCustody>,
    /// The owner's identity key, so [`Self::set_folder_audience`]'s `→public`
    /// flip carries the owner's signed attestation
    /// (`encryption-at-rest.md` § Readable classes → *The declassification is
    /// owner-ATTESTED*). Wired post-construction by the client's build glue
    /// ([`Self::set_audience_attestor`]), the [`Self::set_label_custody`]
    /// delivery: the key lives with the session, not the builder.
    ///
    /// `None` until wired ⇒ the flip still lands on the nest, but **no
    /// verifying seat unseals the folder** — it reads as public and rests
    /// sealed. An app offering `folder-audience-select` must wire this.
    audience_attestor: Mutex<Option<Arc<fauna_core::identity::ActorKeypair>>>,
    /// The account-data plane's fleet-scope removal door ([`FleetRemoval`]),
    /// wired post-construction like [`Self::mls_query`]; `None` until then ⇒
    /// `remove_device` performs only the nest deletion. Every app wires it
    /// at build (web through the account port).
    fleet_removal: Mutex<Option<Arc<dyn FleetRemoval>>>,
    /// This device's own peer participation door ([`P2pParticipation`]),
    /// wired post-construction like [`Self::fleet_removal`]; `None` ⇒ no
    /// row is this device's own (web): every row paints and acts on the
    /// request-off arm.
    p2p_participation: Mutex<Option<Arc<dyn P2pParticipation>>>,
    /// The app's own word on which roster row is this device's
    /// ([`Self::set_this_device_row`]) — the last fallback the participation
    /// gesture consults, after the door's enrolled row and the fleet id.
    this_device_row: Mutex<Option<String>>,
    /// The refresh barrier's triple (`fauna_e2e_agent::DEVICES_REFRESHES_KEY`,
    /// which owns the contract; [`devices_refreshes_json`] the one JSON shape):
    /// refreshes begun — the generation each claims at its first statement —
    /// refreshes that committed their verdict to the snapshot (Ok and Err arms
    /// alike), and the newest committed generation, the release condition.
    refresh_started: AtomicU64,
    refresh_completed: AtomicU64,
    refresh_committed_gen: AtomicU64,
}

impl DevicesMachine {
    /// Construct the page machine over an injected [`DevicesNestApi`] seam +
    /// [`WizardFactory`]. State starts empty; the client calls `refresh()` to
    /// populate it.
    ///
    /// Not a `#[uniffi::constructor]` — the seams (`Arc<dyn …>`) have no FFI ABI.
    /// Clients construct via `nest_api::build_devices_machine` (native
    /// `fauna-ffi` / linux, wasm web), which binds the session's connected
    /// requester; tests pass a `FakeDevicesNestApi` + `FakeWizardFactory`.
    pub fn new(
        observer: Arc<dyn DevicesObserver>,
        nest_api: Arc<dyn DevicesNestApi>,
        wizard_factory: Arc<dyn WizardFactory>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::new()),
            observer,
            nest_api,
            wizard_factory,
            mls_query: Mutex::new(None),
            foreign_sets: Mutex::new(None),
            followed_source: Mutex::new(None),
            label_custody: Mutex::new(LabelCustody::default()),
            audience_attestor: Mutex::new(None),
            fleet_removal: Mutex::new(None),
            p2p_participation: Mutex::new(None),
            this_device_row: Mutex::new(None),
            refresh_started: AtomicU64::new(0),
            refresh_completed: AtomicU64::new(0),
            refresh_committed_gen: AtomicU64::new(0),
        })
    }

    /// The `{started, completed, committed_gen}` refresh triple — plain atomic
    /// reads, so legal on an automation state path. E2e plumbing only
    /// (`fauna_e2e_agent::DEVICES_REFRESHES_KEY`); read `started` before the
    /// trigger and wait for `committed_gen` to pass it.
    pub fn refresh_counts(&self) -> (u64, u64, u64) {
        (
            self.refresh_started.load(Ordering::SeqCst),
            self.refresh_completed.load(Ordering::SeqCst),
            self.refresh_committed_gen.load(Ordering::SeqCst),
        )
    }

    /// Wire the client's local MLS join-filter ([`MlsQuery`]) — the load-bearing B3
    /// member-row filter. Called by the
    /// client's build glue after construction (the per-app `build_devices_machine`
    /// wiring), **never over UniFFI** — `Arc<dyn MlsQuery>` has no FFI ABI, exactly
    /// like the other injected seams (see [`Self::new`]). Until wired, the page is
    /// fail-safe: every `role == "member"` row is dropped (a joined shared set is
    /// invisible until the client wires this, but a stranger's knock never surfaces).
    pub fn set_mls_query(&self, mls_query: Arc<dyn MlsQuery>) {
        *self.mls_query.lock().unwrap() = Some(mls_query);
    }

    /// Wire the account-data plane's fleet-scope removal door ([`FleetRemoval`]).
    /// Called by the client's build glue after construction, the `set_mls_query`
    /// pattern — never over UniFFI (`Arc<dyn FleetRemoval>` has no FFI ABI); a
    /// UniFFI app instead passes a boundary-native adapter built over its own
    /// `AccountStoreHandle` at the same call site; web passes the account
    /// port's forwarder ([`crate::port::PortFleetRemoval`]). Until wired,
    /// `remove_device` performs only the nest deletion.
    pub fn set_fleet_removal(&self, fleet_removal: Arc<dyn FleetRemoval>) {
        *self.fleet_removal.lock().unwrap() = Some(fleet_removal);
    }

    /// Wire this device's own peer participation door ([`P2pParticipation`]).
    pub fn set_p2p_participation_door(&self, door: Arc<dyn P2pParticipation>) {
        *self.p2p_participation.lock().unwrap() = Some(door);
    }

    /// Read back the wired door — test/inspection only, the
    /// [`Self::label_custody`]/[`Self::has_audience_attestor`] read-back's
    /// sibling: a seat's build-time wiring pin asserts against the door the
    /// machine actually holds rather than a re-derived replica, so it reds
    /// when the seat's own wiring call is dropped.
    pub fn fleet_removal(&self) -> Option<Arc<dyn FleetRemoval>> {
        self.fleet_removal.lock().unwrap().clone()
    }

    /// Wire the foreign-set (cross-nest) list source — same post-construction
    /// glue pattern as [`Self::set_mls_query`], because the concrete reader
    /// holds the account-plane handle the builder
    /// doesn't. Until wired, the list simply carries no foreign rows.
    pub fn set_foreign_sets_source(&self, source: Arc<dyn ForeignSetsSource>) {
        *self.foreign_sets.lock().unwrap() = Some(source);
    }

    /// Wire the followed-public-folders reader (see [`FollowedFoldersSource`]).
    /// Unwired ⇒ the page shows no followed rows, which is the correct render
    /// for an app that has not built the follow surface yet.
    pub fn set_followed_folders_source(&self, source: Arc<dyn FollowedFoldersSource>) {
        *self.followed_source.lock().unwrap() = Some(source);
    }

    /// Wire the reader's label custody so the conflict list renders paths
    /// **sealed-first** (`docs/goal/behavior/file-sync.md` § Sealed names &
    /// paths).
    ///
    /// Post-construction injection rather than a `refresh()` parameter on
    /// purpose: `refresh()` has a dozen call sites across the seven apps and is
    /// exported over UniFFI, so a key parameter would be a public-API sweep for
    /// every app before any of them could benefit. This is the same trade the
    /// Media page made the other way (`MediaMachine::refresh` *does* take the
    /// key, because its gestures already did) — the custody *object* is shared
    /// either way ([`LabelCustody`]); only its delivery differs.
    pub fn set_label_custody(&self, custody: LabelCustody) {
        *self.label_custody.lock().unwrap() = custody;
    }

    /// Read back the wired custody — test/inspection only (mirrors
    /// `SnapshotsClient::label_custody` / `FoldersClient::label_custody`),
    /// so a builder's custody-shape pin can assert against the façade it
    /// actually hands callers rather than a re-derived replica.
    pub fn label_custody(&self) -> LabelCustody {
        self.label_custody.lock().unwrap().clone()
    }

    /// Wire the owner's identity key so the `→public` audience flip carries
    /// the owner's signed attestation (the field's doc has the consequence of
    /// leaving it unwired). Called by the client's build glue after
    /// construction — `fauna-ffi`'s `build_devices_machine` for apple /
    /// android / windows, the wasm `setAudienceAttestor` for web — never over
    /// UniFFI: a keypair has no FFI ABI, exactly like the other injected seams.
    pub fn set_audience_attestor(&self, keypair: Arc<fauna_core::identity::ActorKeypair>) {
        *self.audience_attestor.lock().unwrap() = Some(keypair);
    }

    /// Whether an attestor is wired — test/inspection only, the
    /// [`Self::label_custody`] read-back's sibling.
    pub fn has_audience_attestor(&self) -> bool {
        self.audience_attestor.lock().unwrap().is_some()
    }

    fn audience_attestor(&self) -> Option<Arc<fauna_core::identity::ActorKeypair>> {
        self.audience_attestor.lock().unwrap().clone()
    }

    /// Render each conflict's path sealed-first, then transcribe to the page
    /// summary.
    ///
    /// **Order is load-bearing.** `ConflictSummary::file_info` is the
    /// display-ready `conflict-file-info` line, precomputed once at transcribe
    /// from `path` (the `owner_display` pattern). Rendering after the transcribe
    /// would leave that line built from the *unrendered* path — an empty string
    /// once the plaintext column is scrubbed, and a name this reader may not be
    /// entitled to before then. So the wire row's `path` is rewritten first and
    /// the transcribe is fed the rendered value.
    ///
    /// A row this reader can open neither half of is **dropped** — the ratified
    /// degrade (*omit from the listing, re-enter on re-record*), never an empty
    /// name and never a failed page. The same degrade applies to the **set
    /// name**: this surface has no non-audience arm to project against (unlike
    /// `media.list`'s Q5-admin case — path-sealing S5c-2), so an `Omit` on
    /// [`SyncConflict::folder_sealed`] drops the whole row rather than
    /// showing a conflict under a blank set name.
    async fn render_conflicts(&self, rows: Vec<SyncConflict>) -> Vec<ConflictSummary> {
        let custody = self.label_custody.lock().unwrap().clone();
        // The ONLY safe skip: nothing on this page is sealed (a plaintext-resting
        // plane, or a row from one of the keyless writer seams). Deliberately NOT
        // "custody is empty" — a keyless reader meeting a sealed-only row must
        // reach `Omit`, and skipping would render its blank plaintext as the
        // name. See `LabelCustody::keys_for`.
        if rows.iter().all(|c| {
            c.path_sealed.is_none() && c.folder_sealed.is_none() && c.details_sealed.is_none()
        }) {
            return rows.into_iter().map(Into::into).collect();
        }
        // Resolve custody once per distinct set, not once per row — `resolve` is
        // a roster + config read (the `MediaMachine::render_sealed_paths` rule).
        // Keyed and resolved by the set's `name_hash` (`folder_hash`), never the
        // plaintext `folder`, which is the empty sentinel for every sealed set
        // once the nest scrubs it (`LabelCustody::keys_for_row`).
        let mut per_set: std::collections::HashMap<[u8; 32], FileDownloadKeys> =
            std::collections::HashMap::new();
        let mut out = Vec::with_capacity(rows.len());
        for mut row in rows {
            let set_key = fauna_core::label_custody::set_name_label_salt(
                row.folder_hash.as_ref().map(|b| &b[..]),
                &row.folder,
            );
            if let std::collections::hash_map::Entry::Vacant(slot) = per_set.entry(set_key) {
                let (keys, _) = custody.keys_for_hash(&set_key).await;
                slot.insert(keys);
            }
            let keys = &per_set[&set_key];

            // The set-name render, through the same shared seam as the path
            // render below so the two cannot disagree on root or salt
            // (`MediaMachine::render_sealed_paths`'s pattern, S5c-1). Order is
            // load-bearing: a row this reader cannot name is dropped before
            // the path render even runs, exactly as it would for a
            // custody-less path.
            let set_name = match fauna_core::label_custody::render_set_name(
                keys,
                row.folder_sealed.as_ref().map(|b| &b[..]),
                &row.folder,
                row.folder_hash.as_ref().map(|b| &b[..]),
            ) {
                SealedLabelRender::Sealed(name) => name,
                SealedLabelRender::Plaintext(name) => name,
                SealedLabelRender::Omit => continue,
            };
            row.folder = set_name;

            // Both the path and the free-text `details` open under this one
            // salt — the row's `path_hash` (path-sealing S6-a). Computing it
            // once here rather than letting each seam re-derive is not just
            // tidiness: `render_conflict_details` takes it explicitly precisely
            // because deriving it from *its* plaintext would hash the details
            // text instead of the path and silently omit every row.
            let salt =
                fauna_core::label_custody::path_label_salt(Some(&row.path_hash[..]), &row.path);
            let rendered = fauna_core::label_custody::render_path(
                keys,
                row.path_sealed.as_ref().map(|b| &b[..]),
                &row.path,
                Some(&row.path_hash[..]),
                LabelField::SyncChangePath,
            );
            match rendered {
                SealedLabelRender::Sealed(path) => row.path = path,
                SealedLabelRender::Plaintext(_) => {}
                // A skipped catch-up change is the one row kept name-less: the
                // user acts on every other conflict by path, but here the
                // unreadable name IS the finding (`conflicts.md` § Skipped
                // catch-up changes reach the review list).
                SealedLabelRender::Omit
                    if row.conflict_type
                        == fauna_protocol::folders::CONFLICT_TYPE_CATCHUP_FAILED =>
                {
                    row.path = fauna_i18n::strings::devices::conflicts::UNREADABLE_PATH.to_string();
                }
                SealedLabelRender::Omit => continue,
            }
            // `details` is prose *about* the conflict, not an identifier: an
            // unopenable one degrades to `None` on its own rather than dropping
            // the row, because a conflict the reader can name is still
            // actionable (choose-a-winner needs the path, never the blurb).
            // That is deliberately weaker than the path/set-name degrade above.
            if row.details_sealed.is_some() {
                row.details = fauna_core::label_custody::render_conflict_details(
                    keys,
                    row.details_sealed.as_ref().map(|b| &b[..]),
                    row.details.as_deref().unwrap_or(""),
                    salt.as_ref(),
                )
                .text()
                .map(str::to_string);
            }
            out.push(row.into());
        }
        out
    }

    /// Seal a selective-sync path list for the row named `name`, if this reader
    /// is that set's **owner** and holds a key.
    ///
    /// ⚠ **Owner rows only** (path-sealing S6-c). The seal
    /// is under the owner's own root, salted by the row id, and a member neither
    /// owns the config nor is the audience for the owner's filesystem layout —
    /// the nest withholds even the plaintext from a `role == "member"` row. A
    /// member editing paths is already refused nest-side (`update` is
    /// owner-scoped); minting a seal for one would be meaningless bytes at best.
    ///
    /// `None` = nothing to seal: an unknown or non-owner row, a keyless reader
    /// (no [`LabelCustody`] wired), or a caller who sent no list. The save then
    /// proceeds unsealed and the nest *clears* the column — the pair moves
    /// together, so a keyless writer leaves an S8 backfill row rather than a
    /// stale seal (`fauna_protocol::folders::FolderUpdateRequest`).
    ///
    /// Lives in this **non-exported** impl block on purpose: it takes borrowed
    /// slices and returns a tuple, neither of which UniFFI can lift, and it is
    /// internal plumbing no app calls.
    fn seal_paths_for(
        &self,
        name: &str,
        include_paths: Option<&[String]>,
        exclude_paths: Option<&[String]>,
    ) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
        let Some(id) = self.state.lock().unwrap().folders.iter().find_map(|fs| {
            (fs.name == name && fs.role.as_deref() != Some("member")).then_some(fs.id)
        }) else {
            return (None, None);
        };
        let Some(owner) = self.label_custody.lock().unwrap().owner_key() else {
            return (None, None);
        };
        // Best-effort, exactly like the engine's device-label seal: a derivation
        // failure must not fail the user's save. The nest clears the column, so
        // the row lands plaintext-only for S8 rather than stale-sealed.
        let include_sealed = include_paths.and_then(|p| {
            fauna_core::label_custody::seal_include_paths(&owner, id, p)
                .inspect_err(|e| {
                    tracing::warn!(
                        target: "fauna_devices",
                        error = %e,
                        "sealing include_paths failed; saving plaintext-only"
                    );
                })
                .ok()
        });
        let exclude_sealed = exclude_paths.and_then(|p| {
            fauna_core::label_custody::seal_exclude_paths(&owner, id, p)
                .inspect_err(|e| {
                    tracing::warn!(
                        target: "fauna_devices",
                        error = %e,
                        "sealing exclude_paths failed; saving plaintext-only"
                    );
                })
                .ok()
        });
        (include_sealed, exclude_sealed)
    }

    /// Seal a re-pointed path for the review list's "use the other version"
    /// record — the path-plane twin of [`Self::seal_paths_for`], but through
    /// [`LabelCustody::keys_for`] rather than the bare owner key: a conflict can
    /// live on a *bound* set, whose paths seal under the M2 content-key
    /// generation every roster member holds, never the owner root
    /// (`file-sync.md` § Sealed names & paths — the one-root-per-set rule).
    ///
    /// Best-effort like every label seal on a user gesture: `None` records the
    /// re-point plaintext-only (an S8 backfill row), and a bound set whose keys
    /// this custody cannot resolve fails *closed* to `None` rather than sealing
    /// under a root the roster could not open (the mistake class).
    async fn seal_repoint_path(&self, folder: &str, path: &str) -> Option<Vec<u8>> {
        let custody = self.label_custody.lock().unwrap().clone();
        let (keys, _) = custody.keys_for(folder).await;
        match fauna_core::label_custody::seal_path_from_keys(&keys, path) {
            Ok(bytes) => Some(bytes),
            Err(fauna_core::label_custody::SealPathFromKeysError::NoRoot) => None,
            Err(fauna_core::label_custody::SealPathFromKeysError::RootUnresolved(e)) => {
                tracing::warn!(
                    target: "fauna_devices",
                    error = %e,
                    "no seal root for the re-point (bound set, keys unresolved); \
                     recording plaintext-only"
                );
                None
            }
            Err(fauna_core::label_custody::SealPathFromKeysError::SealFailed(e)) => {
                tracing::warn!(
                    target: "fauna_devices",
                    error = %e,
                    "sealing the re-pointed path failed; recording plaintext-only"
                );
                None
            }
        }
    }

    /// Render the set name of every place each device holds, sealed-first — the
    /// place twin of [`Self::render_conflicts`]' set-name render, through the same
    /// seam (`render_set_name`) and the same per-set custody cache keyed by the
    /// row's `name_hash`, never the plaintext `name`, which is the empty sentinel
    /// once the nest scrubs a sealed set (`path-sealing.md` § the set-name plane).
    ///
    /// A place whose set name this reader cannot open is **dropped**, never shown
    /// as a nameless chip — the list render's degrade. The device row itself
    /// stays (its label render below keeps every row for the reasons given there).
    async fn render_device_places(&self, mut rows: Vec<SyncDevice>) -> Vec<SyncDevice> {
        // The ONLY safe skip, as in `render_conflicts`: nothing here is sealed.
        if rows
            .iter()
            .all(|d| d.folders.iter().all(|p| p.name_sealed.is_none()))
        {
            return rows;
        }
        let custody = self.label_custody.lock().unwrap().clone();
        let mut per_set: std::collections::HashMap<[u8; 32], FileDownloadKeys> =
            std::collections::HashMap::new();
        for row in &mut rows {
            let mut kept = Vec::with_capacity(row.folders.len());
            for mut place in std::mem::take(&mut row.folders) {
                let wire_hash = place.name_hash.as_ref().map(|b| &b[..]);
                let set_key =
                    fauna_core::label_custody::set_name_label_salt(wire_hash, &place.name);
                if let std::collections::hash_map::Entry::Vacant(slot) = per_set.entry(set_key) {
                    let (keys, _) = custody.keys_for_hash(&set_key).await;
                    slot.insert(keys);
                }
                match fauna_core::label_custody::render_set_name(
                    &per_set[&set_key],
                    place.name_sealed.as_ref().map(|b| &b[..]),
                    &place.name,
                    wire_hash,
                ) {
                    SealedLabelRender::Sealed(name) | SealedLabelRender::Plaintext(name) => {
                        place.name = name;
                        kept.push(place);
                    }
                    SealedLabelRender::Omit => {}
                }
            }
            row.folders = kept;
        }
        rows
    }

    /// Render each device's sealed label at ingest, then transcribe to the
    /// page's [`DeviceSummary`] rows — the device-plane twin of
    /// [`Self::render_conflicts`], and for the same reason: the summary is what
    /// every app renders, so a label the reader cannot open must never reach
    /// it as blank plaintext.
    ///
    /// **Custody is owner-only**, not [`LabelCustody::keys_for`]: a device label
    /// seals under the registering *owner's* root, so there is no folder to
    /// resolve custody for (`file-sync.md` § Sealed names & paths). This reply
    /// is `WHERE actor_id = ?1` nest-side, so this reader **is** that owner.
    ///
    /// A row whose label this reader can open neither half of **keeps its row**
    /// with an empty label — deliberately weaker than the conflict/path degrade,
    /// which drops the row. A device is an actionable object in its own right
    /// (revoke it, see it online, read its folder roles) and every one of those
    /// keys off `device_id`, not the name; dropping it would hide a device the
    /// user may need to revoke — the one outcome worse than an unnamed row. Same
    /// reasoning as conflict `details`, which degrades to `None` rather than
    /// dropping the conflict.
    async fn render_devices(&self, rows: Vec<SyncDevice>) -> Vec<DeviceSummary> {
        let rows = self.render_device_places(rows).await;
        // The ONLY safe skip, exactly as in `render_conflicts`: nothing on this
        // page is sealed. Deliberately NOT "custody is empty" — a keyless reader
        // meeting a sealed-only row must reach `Omit` rather than render its
        // blank plaintext as the name.
        if rows.iter().all(|d| d.label_sealed.is_none()) {
            return rows.into_iter().map(Into::into).collect();
        }
        // Read custody, so a successor still opens rows sealed under the
        // identity it succeeded from (`succession-aftermath.md` § Re-key scope).
        // `owner_key()` — the seal-side accessor — would silently degrade every
        // predecessor-sealed row on this page to its no-list state.
        let keys = self.label_custody.lock().unwrap().owner_plane_read_keys();
        rows.into_iter()
            .map(|mut row| {
                // The salt is the device id itself — no hash companion needed on
                // this plane, unlike paths and set names.
                if let Ok(salt) = fauna_core::hex32::decode(&row.device_id) {
                    row.label = match fauna_core::label_custody::render_device_label(
                        &keys,
                        row.label_sealed.as_ref().map(|b| &b[..]),
                        &row.label,
                        &salt,
                    ) {
                        SealedLabelRender::Sealed(label) => label,
                        SealedLabelRender::Plaintext(label) => label,
                        SealedLabelRender::Omit => String::new(),
                    };
                }
                row.into()
            })
            .collect()
    }

    /// Map the member's foreign-set records into member-visible list rows and
    /// apply the SAME join-filter as [`Self::filter_member_rows`] (fail-safe: a
    /// record whose group this client has not MLS-joined — or with no
    /// [`MlsQuery`] wired — never surfaces). A foreign set has no nest row, so
    /// the synthetic summary carries `id == -1`, empty caps/paths, and the
    /// record's `home_nest_url`; `owner_display` stays empty until the
    /// cross-nest sharer stamp lands (clients render their unknown-sharer
    /// label). Skips any group id the same-nest list already carries (belt and
    /// braces — a foreign set should never be in the own-nest reply).
    fn foreign_rows(
        &self,
        rows: Vec<ForeignSetRow>,
        same_nest: &[FolderSummary],
    ) -> Vec<FolderSummary> {
        let mls_query = self.mls_query.lock().unwrap().clone();
        rows.into_iter()
            .filter(|r| {
                mls_query
                    .as_ref()
                    .is_some_and(|q| q.is_joined_shared_set(&r.mls_group_id_hex))
            })
            .filter(|r| {
                !same_nest
                    .iter()
                    .any(|fs| fs.mls_group_id.as_deref() == Some(r.mls_group_id_hex.as_str()))
            })
            .map(|r| FolderSummary {
                id: -1,
                name: r.set_name.unwrap_or_default(),
                retention_policy: None,
                cached_snapshot_count: 0,
                cached_total_bytes: 0,
                cached_last_snapshot_at: None,
                include_paths: None,
                exclude_paths: None,
                mls_group_id: Some(r.mls_group_id_hex),
                role: Some("member".to_string()),
                // The grant the set's HOME nest resolved, carried in the member's
                // own `fauna.state.folder-keys` foreign-set row (seeded on the Welcome relay,
                // refreshed by the `caller_access` stamp federated read replies
                // carry). Same axis as a same-nest member row, whose `access` the
                // own nest projects onto the summary — so a client's row split
                // (writer ⇒ folder-binding UI, reader ⇒ read-only) is ONE rule
                // across both planes.
                //
                // Advisory-for-UI only, never an authz input: enforcement is the
                // home nest's `require_foreign_writer` gate on the write kinds,
                // and the bind gesture verifies with an eager `write_token.get`.
                // `None` (a record carrying no access stamp) ⇒ reader ⇒ unbindable, the fail-safe direction.
                access: r.access,
                owner_handle: None,
                owner_display: String::new(),
                webdav_enabled: false,
                conflict_policy: None,
                web_paywall_tier: None,
                home_nest_url: Some(r.home_nest_url),
                // A foreign row is synthesized from the member's own foreign-set
                // row, which carries no policy — and the nest place is the
                // OWNER's to set anyway, on the set's home nest. Unset, not
                // false.
                nest_snapshots: None,
                // Same statement for the version-retention pair: the owner's
                // policy, on the set's home nest — a foreign row rests (0, 0).
                version_retention_max_versions: 0,
                version_retention_max_age_days: 0,
                nest_snapshot_quiet_secs: None,
                // A foreign row is a bound membership; the foreign-set row
                // carries no audience (a cross-nest public-folder projection is
                // the home nest's, unreached here), so the
                // bound ⇒ shared derivation is the honest, fail-sealed
                // value. The website toggle is the owner's, on the home nest.
                audience: "shared".to_string(),
                website_enabled: false,
                // The residency the HOME nest stamped on the member's custody
                // record, projected only when it states metadata-only — the
                // same projection a same-nest row carries (empty = full). An
                // unknown reading stays empty, which
                // `FolderSummary::is_metadata_only` reads as full: nothing
                // unparseable stops bytes resting (`file-sync.md` § Relay
                // serving → *A member on another nest*, step (1)).
                residency: if r.metadata_only_residency == Some(true) {
                    fauna_protocol::folders::RESIDENCY_METADATA_ONLY.to_string()
                } else {
                    String::new()
                },
                // Exclusive editing and its lease are the HOME nest's facts,
                // not carried on the foreign-set row: un-governed, the
                // fail-open reading (`file-sync.md` § Exclusive editing).
                exclusive_editing: false,
                lease: None,
                // The attestation is a home-nest projection field, not carried
                // on the foreign-set row — and the row claims `shared`, so
                // nothing would read it.
                audience_attestation: None,
            })
            .collect()
    }

    /// Drop B3 member-visible rows (`role == "member"`) the client has not actually
    /// MLS-joined — the load-bearing join-filter for the `list_owned_and_shared`
    /// projection. Owner rows (`role != "member"`) always pass.
    /// **Fail-safe:** with no [`MlsQuery`] wired, or a member row missing its
    /// `mls_group_id`, the row is dropped — a rostered-but-un-joined knock never
    /// reaches the snapshot the client renders.
    /// Render each row's **selective-sync path pair** sealed-first, then
    /// transcribe to the snapshot type — the folder twin of
    /// [`Self::render_devices`] and [`Self::render_conflicts`], and the reason
    /// [`DevicesNestApi::list_folders`] hands over wire rows.
    ///
    /// **Why this exists.** `include_paths`/`exclude_paths` seal under the
    /// owner's root (S6-c), and the S9 flip NULLs both plaintext columns wherever
    /// a seal is present (`SCRUB_PLANES`, `bins/fauna-nest/src/db/migrations.rs`).
    /// The `From<WireFolderSummary>` transcribe drops the `*_sealed` columns, so
    /// ingesting through it left this page reading both lists as **empty for a
    /// reader that demonstrably holds the key** — the same consumer-wiring shape
    /// as the media-refresh and headless-restore bugs before it, while the
    /// since-removed headless daemon's own reader had been rendering
    /// sealed-first all along.
    ///
    /// That made this more than a blank field: [`Self::set_folder_paths`] is
    /// the **one durable writer** of these seals, so a user who opened the
    /// (blank) selective-sync editor and saved would overwrite their real lists
    /// with nothing, and no engine pass can re-derive them once the plaintext is
    /// scrubbed.
    ///
    /// **Custody is owner-only**, never [`LabelCustody::keys_for`]: these seal
    /// under the *owner's* root, so a member's resolver-derived custody cannot
    /// open them and must not be handed them (`file-sync.md` § Sealed names &
    /// paths — the one tightening-set member whose audience is owner-only rather
    /// than label-audience). A member row carries no seal to begin with: the nest
    /// projects the pair on the owner arm only (`member_summary`).
    ///
    /// An unopenable pair degrades to `None` — "this reader has no list to show"
    /// — which is what the wire's `Option` has always meant, and is the
    /// conflict-`details` degrade rather than the row-dropping one: a set is an
    /// actionable object in its own right.
    ///
    /// **The set name renders first, by the row's hash** ([`Self::render_set_names`]):
    /// since schema 114 a sealed set's row rests no plaintext name, so this is
    /// where a folder the reader can open gets its name and one it cannot drops.
    async fn render_folders(&self, rows: Vec<WireFolderSummary>) -> Vec<FolderSummary> {
        let mut rows = self.render_set_names(rows).await;
        // The served state is the owner's custody's word, asked AFTER the names
        // render — an unshared set's window rests at the pseudo-channel of its
        // plaintext name (ruling (7)(b)(ii) rule (2)). It replaces the nest's
        // flag before the transcribe, so the snapshot never carries the flag.
        let served = self.nest_api.webdav_served(&rows).await;
        for (row, served) in rows.iter_mut().zip(served) {
            row.webdav_enabled = served;
        }
        // The ONLY safe skip, exactly as in `render_devices`/`render_conflicts`:
        // nothing on this page is sealed. Deliberately NOT "custody is empty" — a
        // keyless reader meeting a sealed-only row must reach `Omit` rather than
        // render its scrubbed-empty plaintext as the list.
        if rows
            .iter()
            .all(|fs| fs.include_paths_sealed.is_none() && fs.exclude_paths_sealed.is_none())
        {
            return rows.into_iter().map(Into::into).collect();
        }
        // Read custody, so a successor still opens rows sealed under the
        // identity it succeeded from (`succession-aftermath.md` § Re-key scope).
        // `owner_key()` — the seal-side accessor — would silently degrade every
        // predecessor-sealed row on this page to its no-list state.
        let keys = self.label_custody.lock().unwrap().owner_plane_read_keys();
        rows.into_iter()
            .map(|mut row| {
                // The salt is the row id — already on this same struct, so this
                // plane needs no hash companion (unlike set names and paths).
                let id = row.id;
                row.include_paths = fauna_core::label_custody::render_include_paths(
                    &keys,
                    row.include_paths_sealed.as_deref().map(|b| &b[..]),
                    row.include_paths.as_deref(),
                    id,
                );
                row.exclude_paths = fauna_core::label_custody::render_exclude_paths(
                    &keys,
                    row.exclude_paths_sealed.as_deref().map(|b| &b[..]),
                    row.exclude_paths.as_deref(),
                    id,
                );
                row.into()
            })
            .collect()
    }

    /// Render each folder row's **set name and retention policy** sealed-first,
    /// by the row's `name_hash` — the list twin of [`Self::render_device_places`],
    /// through the same seam (`render_set_name`) and the same per-set custody
    /// cache keyed by the hash, never the plaintext `name`, which is the empty
    /// sentinel once a sealed set rests NULL (`path-sealing.md` § the set-name
    /// plane). [`DevicesNestApi::list_folders`] hands over the rows unrendered
    /// for exactly this reason: the adapter holds no custody, this machine does.
    ///
    /// A row whose name this reader cannot open is **dropped**, never shown as a
    /// nameless folder (the list's ratified degrade). A reserved `__` or
    /// `public` set carries no seal and keeps its plaintext.
    async fn render_set_names(&self, rows: Vec<WireFolderSummary>) -> Vec<WireFolderSummary> {
        // The ONLY safe skip, as everywhere on this page: nothing here is sealed.
        if rows
            .iter()
            .all(|fs| fs.name_sealed.is_none() && fs.retention_policy_sealed.is_none())
        {
            return rows;
        }
        let custody = self.label_custody.lock().unwrap().clone();
        let mut per_set: std::collections::HashMap<[u8; 32], FileDownloadKeys> =
            std::collections::HashMap::new();
        let mut kept = Vec::with_capacity(rows.len());
        for mut row in rows {
            let wire_hash = row.name_hash.as_ref().map(|b| &b[..]);
            let set_key = fauna_core::label_custody::set_name_label_salt(wire_hash, &row.name);
            if let std::collections::hash_map::Entry::Vacant(slot) = per_set.entry(set_key) {
                let (keys, _) = custody.keys_for_hash(&set_key).await;
                slot.insert(keys);
            }
            let keys = &per_set[&set_key];
            let name = match fauna_core::label_custody::render_set_name(
                keys,
                row.name_sealed.as_ref().map(|b| &b[..]),
                &row.name,
                wire_hash,
            ) {
                SealedLabelRender::Sealed(name) | SealedLabelRender::Plaintext(name) => name,
                SealedLabelRender::Omit => continue,
            };
            // The policy's salt falls back to the *wire* plaintext, never the
            // rendered name — the wire hash wins whenever it is present anyway.
            row.retention_policy = fauna_core::label_custody::render_retention_policy(
                keys,
                row.retention_policy_sealed.as_ref().map(|b| &b[..]),
                row.retention_policy.as_deref(),
                &row.name,
                wire_hash,
            );
            row.name = name;
            kept.push(row);
        }
        kept
    }

    fn filter_member_rows(&self, folders: Vec<FolderSummary>) -> Vec<FolderSummary> {
        let mls_query = self.mls_query.lock().unwrap().clone();
        folders
            .into_iter()
            .filter(|fs| {
                if fs.role.as_deref() != Some("member") {
                    return true; // owner (or unspecified) rows always show
                }
                match (&mls_query, &fs.mls_group_id) {
                    (Some(q), Some(gid)) => q.is_joined_shared_set(gid),
                    // Fail-safe: no query wired, or a member row with no group id ⇒
                    // drop it (never surface a rostered-but-un-joined knock).
                    _ => false,
                }
            })
            .collect()
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl DevicesMachine {
    // ── App-held identity ───────────────────────────────────────────────

    /// The app's own device id (hex) — the row its `device-this-mark-badge`
    /// paints on — handed in so the participation gesture agrees with the
    /// badge on a seat whose door cannot name the row: a seat with no account
    /// runtime (iOS today) answers neither the door's enrolled row nor the
    /// fleet id, and without this its own row's toggle would take the
    /// sibling arm and ask the nest to turn this device off. Consulted LAST:
    /// the door's enrolled row, then the fleet id, outrank it. `None` or an
    /// empty id clears it.
    pub fn set_this_device_row(&self, row: Option<String>) {
        *self.this_device_row.lock().unwrap() = row.filter(|r| !r.is_empty());
    }

    // ── Read surface ────────────────────────────────────────────────────

    /// The whole renderable Devices page in one record.
    pub fn snapshot(&self) -> DevicesSnapshot {
        let s = self.state.lock().unwrap();
        // Every row's `device-p2p-participation-toggle` paint, own-ness by
        // the SAME rule `set_p2p_participation` decides its arm with.
        let this_device_row = self.this_device_row.lock().unwrap().clone();
        let devices = s
            .devices
            .iter()
            .map(|d| {
                let own = is_own_row(
                    &s.p2p_own_row,
                    s.own_fleet_id.as_deref(),
                    this_device_row.as_deref(),
                    &d.device_id,
                    d.principal.as_deref(),
                );
                let mut d = d.clone();
                d.p2p_participation_paint = Some(p2p_participation_paint(
                    own,
                    s.own_p2p_participation,
                    d.p2p_participation,
                    d.p2p_off_requested,
                ));
                d
            })
            .collect();
        DevicesSnapshot {
            devices,
            folders: s.folders.clone(),
            followed: s.followed.clone(),
            conflicts: s.conflicts.clone(),
            website_address_enabled: s.website_address_enabled,
            wizard: s.wizard.as_ref().map(|w| w.snapshot()),
            error: s.error.clone(),
            members: s.members.clone(),
            own_fleet_id: s.own_fleet_id.clone(),
            own_fingerprint: s.own_fingerprint.clone(),
            own_p2p_participation: s.own_p2p_participation,
        }
    }

    /// The open folder wizard machine, if any. Clients forward wizard gestures
    /// (`set_name`, `next`, `submit`, …) to it directly; its observer ticks the
    /// page so the embedded `DevicesSnapshot.wizard` re-renders. `None` when no
    /// wizard is open.
    pub fn wizard(&self) -> Option<Arc<FolderWizardMachine>> {
        self.state.lock().unwrap().wizard.clone()
    }

    // ── Gestures ────────────────────────────────────────────────────────

    /// Re-read the device / folder / conflict lists. On a read failure the
    /// prior data is kept and the page error is set; on success the error
    /// clears. Notifies once.
    pub async fn refresh(&self) {
        // The barrier generation, claimed synchronously at the first statement
        // (convention 14's "initiated synchronously" corollary): a refresh that
        // began after a reader's baseline always carries a larger one.
        let generation = self.refresh_started.fetch_add(1, Ordering::SeqCst) + 1;
        // Stage trace for the page's three reads. An empty Folders list is
        // otherwise INDISTINGUISHABLE, from any client's log, between "the nest
        // returned no rows", "the join-filter dropped them all", and "a read
        // never came back so this fn never reached the assignment below" — the
        // last of which leaves the snapshot at its initial empty value with
        // `error: None`, i.e. looking exactly like a legitimately empty account.
        // That ambiguity cost two operator-coordinated tri-machine rounds
        // (`20260730-01`/`-02`) and three refuted candidates. Each stage logs on
        // the *event*, per observability.md § Log on the event, not the paint.
        tracing::debug!(target: "fauna_devices", "refresh: reading devices");
        // Sealed-first at ingest, same as the conflict rows below.
        let devices = match self.nest_api.list_devices().await {
            Ok(rows) => Ok(self.render_devices(rows).await),
            Err(e) => Err(e),
        };
        tracing::debug!(
            target: "fauna_devices",
            devices = devices.as_ref().map(|d| d.len()).unwrap_or(0),
            ok = devices.is_ok(),
            "refresh: devices read returned; reading folders"
        );
        // The member door's read — signed-in devices no roster row accounts
        // for — over the roster just read (`ui/devices.md` § Members without
        // a matching entry). A local store read behind the door, so it rides
        // the same refresh; no door (web) or a roster that failed to read
        // leaves the previous answer standing (`None` below = keep).
        let door = self.fleet_removal.lock().unwrap().clone();
        let members = match (&devices, door) {
            (Ok(rows), Some(door)) => {
                let roster: Vec<(String, Option<[u8; 32]>)> = rows
                    .iter()
                    .map(|d| {
                        (
                            d.device_id.clone(),
                            d.principal
                                .as_deref()
                                .and_then(|hex| fauna_core::hex32::decode(hex).ok()),
                        )
                    })
                    .collect();
                match door.fleet_members(roster).await {
                    Ok(view) => Some(view),
                    Err(message) => {
                        tracing::debug!(
                            target: "fauna_devices",
                            %message,
                            "refresh: the member door did not answer; keeping the listed members"
                        );
                        None
                    }
                }
            }
            _ => None,
        };
        // This device's own peer participation — the device-local row behind
        // the door (`behavior/p2p.md` § Per-device participation). No door
        // (web) or no runtime yet reads `None`, and the own row then paints
        // its reported state like any other.
        let participation_door = self.p2p_participation.lock().unwrap().clone();
        // Which row is this device's — the door's enrolled row, the first
        // step of the own-row rule the per-row paint is drawn with.
        let p2p_own_row = match &participation_door {
            Some(door) => OwnRowAnswer::from_door(Some(door.own_row().await)),
            None => OwnRowAnswer::NoDoor,
        };
        let own_p2p_participation = match participation_door {
            Some(door) => match door.local().await {
                Ok(on) => Some(on),
                Err(message) => {
                    tracing::debug!(
                        target: "fauna_devices",
                        %message,
                        "refresh: the participation door did not answer"
                    );
                    None
                }
            },
            None => None,
        };
        // Apply the load-bearing B3 join-filter before the rows
        // ever reach the snapshot: a `role == "member"` row the client hasn't
        // MLS-joined is dropped, centrally, so no client render can surface a
        // stranger's un-accepted knock (`folders.md` § Sharing lines 128/139).
        let listed = self.nest_api.list_folders().await;
        let folders = match listed {
            Ok(f) => {
                // Raw vs. filtered is the load-bearing distinction: equal counts
                // exonerate the join-filter, a drop to zero indicts it.
                let raw = f.len();
                // Sealed-first at ingest, same as the devices above and the
                // conflicts below — this is what turns the wire rows into snapshot
                // rows, so the set names and selective-sync lists are opened while
                // custody is still in reach.
                let kept = self.filter_member_rows(self.render_folders(f).await);
                tracing::debug!(
                    target: "fauna_devices",
                    raw,
                    kept = kept.len(),
                    mls_query_wired = self.mls_query.lock().unwrap().is_some(),
                    "refresh: folders read + join-filtered"
                );
                Ok(kept)
            }
            Err(e) => Err(e),
        };
        // Union in the member's FOREIGN (cross-nest) sets — their home is
        // another nest, so the own-nest list above cannot carry them (Phase 2
        // client read-side). Same join-filter, fail-safe, appended after the
        // same-nest rows. Skipped entirely when the same-nest list errored
        // (the page error stands; foreign rows return with the next refresh).
        let folders = match folders {
            Ok(mut rows) => {
                let source = self.foreign_sets.lock().unwrap().clone();
                if let Some(source) = source {
                    // Bracketed because this read is a SECOND network round trip
                    // inside the same refresh, on a source only some clients wire
                    // (apple does, tui does not) — a stall here strands the whole
                    // refresh before any assignment happens.
                    tracing::debug!(target: "fauna_devices", "refresh: reading foreign (cross-nest) sets");
                    let foreign = self.foreign_rows(source.foreign_sets().await, &rows);
                    tracing::debug!(
                        target: "fauna_devices",
                        foreign = foreign.len(),
                        "refresh: foreign sets read returned"
                    );
                    rows.extend(foreign);
                }
                Ok(rows)
            }
            Err(e) => Err(e),
        };
        // Sealed-first at ingest, before the transcribe that precomputes
        // `file_info` from the path (see `render_conflicts`).
        //
        // Bracketed like the foreign-set read above, and for a sharper reason:
        // `render_conflicts` is the LAST await before the assignment block, and
        // on a sealed account it does a custody `keys_for` — a roster + config
        // network read — per distinct set. A stall there strands the whole
        // refresh with the folder rows already fetched but never assigned, so
        // the page renders an EMPTY list with NO error, indistinguishable from
        // an account that genuinely has no sets.
        // The followed public folders — a THIRD source, read like the foreign
        // sets above: optional, best-effort, and never able to fail the page.
        // A follow lives entirely in the account's own store, so an empty
        // answer here means "none followed" or "the reader had a bad moment",
        // never "the nest said no".
        let followed = {
            let source = self.followed_source.lock().unwrap().clone();
            match source {
                Some(source) => {
                    tracing::debug!(target: "fauna_devices", "refresh: reading followed folders");
                    let rows = source.followed_folders().await;
                    tracing::debug!(
                        target: "fauna_devices",
                        followed = rows.len(),
                        unavailable = rows.iter().filter(|r| !r.available).count(),
                        "refresh: followed folders read returned"
                    );
                    rows
                }
                None => Vec::new(),
            }
        };

        // The actor's web-address opt-in — the website hint's second half
        // (`website_serve_hint`). Best-effort BY SIGNATURE (`Option`, not
        // `Result`): unknown degrades the hint's wording, never the page.
        let website_address_enabled = self.nest_api.web_subdomain_enabled().await;

        tracing::debug!(target: "fauna_devices", "refresh: reading conflicts");
        let listed_conflicts = self.nest_api.list_conflicts().await;
        tracing::debug!(
            target: "fauna_devices",
            conflicts = listed_conflicts.as_ref().map(|r| r.len()).unwrap_or(0),
            ok = listed_conflicts.is_ok(),
            "refresh: conflicts read returned; rendering (may unseal per set)"
        );
        let conflicts = match listed_conflicts {
            Ok(rows) => Ok(self.render_conflicts(rows).await),
            Err(e) => Err(e),
        };
        tracing::debug!(target: "fauna_devices", "refresh: conflicts rendered");

        {
            let mut s = self.state.lock().unwrap();
            let mut error: Option<LocalizedText> = None;
            match devices {
                Ok(d) => s.devices = d,
                Err(e) => error = Some(error_text(REFRESH_ERROR_KEY, e.detail())),
            }
            if let Some(view) = members {
                s.members = render_members(&view);
                s.own_fingerprint = Some(fauna_core::format::fleet_fingerprint(&view.me));
                s.own_fleet_id = Some(fauna_core::hex32::encode(&view.me));
            }
            // `None` IS the failure encoding here too (no door / no runtime),
            // so assign as-read: the own row then paints its reported state.
            s.own_p2p_participation = own_p2p_participation;
            s.p2p_own_row = p2p_own_row;
            match folders {
                Ok(f) => s.folders = f,
                Err(e) => {
                    error.get_or_insert_with(|| error_text(REFRESH_ERROR_KEY, e.detail()));
                }
            }
            // Assigned unconditionally: this source cannot report failure (an
            // unwired or unhappy reader answers with an empty list), so there
            // is no error arm to take and nothing to preserve across it.
            s.followed = followed;
            // Same posture: `None` IS the failure encoding, so assign as-read.
            s.website_address_enabled = website_address_enabled;
            match conflicts {
                Ok(mut c) => {
                    name_skipping_devices(&mut c, &s.devices);
                    s.conflicts = c;
                }
                Err(e) => {
                    error.get_or_insert_with(|| error_text(REFRESH_ERROR_KEY, e.detail()));
                }
            }
            // Producer-side log for the reactive error banner (the same rule as
            // `set_error`): fire once here, where the refresh failure is
            // recorded, not in the per-tick render. observability.md § Log on
            // the *event*, not the *paint*.
            if let Some(err) = &error {
                tracing::warn!(target: "fauna_devices", "{}", err.log_line());
            }
            s.error = error;
            // The completion line the stage trace above is bracketed by: its
            // ABSENCE (with an entry line present) is the positive signal that a
            // read never came back, which no snapshot read can ever show. INFO,
            // unlike the stages: one line per page refresh is cheap, and it must
            // be in the log a session already has (default `RUST_LOG=info`)
            // rather than one they'd have to know to re-run with.
            tracing::info!(
                target: "fauna_devices",
                devices = s.devices.len(),
                folders = s.folders.len(),
                conflicts = s.conflicts.len(),
                error = s.error.is_some(),
                "refresh: snapshot committed"
            );
        }
        // Counted BEFORE the observer fires, so a state republish on
        // `on_changed` carries the generation together with the snapshot it
        // committed. `fetch_max`, not a store: two overlapping refreshes may
        // commit out of order, and the newest generation must never regress.
        self.refresh_completed.fetch_add(1, Ordering::SeqCst);
        self.refresh_committed_gen
            .fetch_max(generation, Ordering::SeqCst);
        self.observer.on_changed();
    }

    /// [`Self::refresh_counts`] as the shared JSON string
    /// ([`devices_refreshes_json`]) — the one read the wasm and FFI faces make
    /// for `fauna_e2e_agent::DEVICES_REFRESHES_KEY`, so no app re-derives it.
    pub fn refreshes_json(&self) -> String {
        devices_refreshes_json(Some(self.refresh_counts())).to_string()
    }

    /// Open a fresh folder creation wizard, seeded with the current device
    /// list. Stored in `wizard` (surfaced as `DevicesSnapshot.wizard`). No-op'd
    /// re-open replaces any prior wizard.
    pub fn open_wizard(&self) {
        let available_devices: Vec<DeviceOption> = {
            let s = self.state.lock().unwrap();
            s.devices
                .iter()
                .map(|d| DeviceOption {
                    device_id: d.device_id.clone(),
                    label: d.label.clone(),
                })
                .collect()
        };
        // The wizard's own observer forwards to the page observer so driving the
        // wizard re-renders the embedded snapshot.
        let wiz_observer: Arc<dyn FolderWizardObserver> =
            Arc::new(WizardObserverBridge(Arc::clone(&self.observer)));
        let wizard = self
            .wizard_factory
            .build_wizard(wiz_observer, available_devices);
        self.state.lock().unwrap().wizard = Some(wizard);
        self.observer.on_changed();
    }

    /// Close the wizard (drop it). The client calls this on cancel, or after a
    /// successful create (`wizard().step() == Done`) before `refresh()`.
    pub fn close_wizard(&self) {
        self.state.lock().unwrap().wizard = None;
        self.observer.on_changed();
    }

    /// Remove (unregister) device at `index` into the current device list, then
    /// refresh. Out-of-range indices are ignored.
    ///
    /// Two legs, `devices.md` § Removing a Device: `fauna.sync.devices.delete`,
    /// and the plane's fleet-scope `Removed` row through the wired
    /// [`FleetRemoval`] door. **The fleet target is resolved first, from
    /// client-held truth, and before anything is deleted**
    /// ([`Self::resolve_fleet_targets`]): the row's `principal` is the nest's
    /// to write, and a `Removed` row is absorbing, so trusting it let the nest
    /// pick which device a removal excludes and which it spares. A refusal
    /// deletes nothing and says so on `error-message` — the row is still there
    /// to retry from.
    ///
    /// **Crash-safe across the two legs** (clause (4), *The completion rule*):
    /// the durable intent is staged through the door before the nest deletion
    /// — the transition's single decision point — and settled on its outcome.
    /// `Gone` (deleted, or the nest no longer holds the row) journals the
    /// `Removed` rows; `Kept` (a definitive refusal) clears the intent
    /// unwritten, since `Removed` is absorbing and the user was told the
    /// device stays; anything unclear stays staged and the runtime's reconcile
    /// decides from the roster. A settle failure is surfaced on
    /// `error-message` rather than dropped silently (e2e convention 11) — set
    /// *after* `refresh()` so it survives refresh's success-clears-error
    /// assignment — but the staged intent means the runtime still finishes it.
    pub async fn remove_device(&self, index: u32) {
        let (device_id, principal) = {
            let s = self.state.lock().unwrap();
            match s.devices.get(index as usize) {
                Some(d) => (d.device_id.clone(), d.principal.clone()),
                None => return,
            }
        };
        let door = self.fleet_removal.lock().unwrap().clone();
        tracing::info!(
            target: "fauna_devices",
            %device_id,
            door = door.is_some(),
            "remove_device: resolving the fleet target"
        );
        let targets =
            match Self::resolve_fleet_targets(door.as_deref(), &device_id, principal).await {
                Ok(targets) => targets,
                Err(refusal) => {
                    self.set_error(refusal_text(&refusal));
                    return;
                }
            };
        // A row that names no fleet member has no second leg to make safe.
        let door = door.filter(|_| !targets.is_empty());
        if let Some(door) = &door
            && let Err(message) = door.stage_removal(&device_id, targets.clone()).await
        {
            self.set_error(error_text(REMOVE_DEVICE_ERROR_KEY, &message));
            return;
        }
        tracing::info!(
            target: "fauna_devices",
            %device_id,
            targets = targets.len(),
            "remove_device: deleting the nest row"
        );
        let deletion = self.nest_api.remove_device(&device_id).await;
        tracing::info!(
            target: "fauna_devices",
            %device_id,
            ok = deletion.is_ok(),
            "remove_device: nest deletion answered"
        );
        let outcome = nest_deletion_outcome(&deletion);
        let settled = match &door {
            Some(door) => door.settle_removal(&device_id, targets, outcome).await,
            None => Ok(()),
        };
        match deletion {
            Ok(()) => {
                self.refresh().await;
                if let Err(message) = settled {
                    self.set_error(error_text(REMOVE_FLEET_DEVICE_ERROR_KEY, &message));
                }
            }
            Err(e) => self.set_error(error_text(REMOVE_DEVICE_ERROR_KEY, e.detail())),
        }
    }

    /// `device-p2p-participation-toggle[index]` — whether the `index`-th
    /// listed device takes part in peer-to-peer transfers
    /// (`docs/goal/behavior/p2p.md` § Per-device participation). Two arms,
    /// decided here so no app re-derives them: on THIS device's own row
    /// (the door's enrolled row, else the row whose principal is this
    /// device's own fleet id, else the app's [`Self::set_this_device_row`] —
    /// a runtimeless seat's own row then answers the door's refusal on
    /// `error-message`) the switch is the device's own, both
    /// directions, written through the [`P2pParticipation`] door; on any
    /// other row only `false` is sendable — the owner arm of
    /// `fauna.sync.devices.p2p_participation.set`, a request that device
    /// folds at its next pass — and `true` is refused on `error-message`
    /// without a call, because enabling is local consent on that device.
    /// Success re-reads the roster; a failure paints the page error and
    /// changes nothing. Out-of-range indices are ignored.
    pub async fn set_p2p_participation(&self, index: u32, on: bool) {
        let (device_id, principal) = {
            let s = self.state.lock().unwrap();
            match s.devices.get(index as usize) {
                Some(d) => (d.device_id.clone(), d.principal.clone()),
                None => return,
            }
        };
        let door = self.p2p_participation.lock().unwrap().clone();
        // Read afresh at the gesture, decided by the one rule the snapshot's
        // paint is drawn with (`p2p_participation::is_own_row`).
        let answer = match door.as_ref() {
            Some(door) => OwnRowAnswer::from_door(Some(door.own_row().await)),
            None => OwnRowAnswer::NoDoor,
        };
        let own = {
            let s = self.state.lock().unwrap();
            is_own_row(
                &answer,
                s.own_fleet_id.as_deref(),
                self.this_device_row.lock().unwrap().as_deref(),
                &device_id,
                principal.as_deref(),
            )
        };
        tracing::info!(
            target: "fauna_devices",
            %device_id,
            on,
            own,
            "set_p2p_participation"
        );
        if own {
            // `own` is only ever true with a door.
            let Some(door) = door else { return };
            match door.set_local(on).await {
                Ok(()) => self.refresh().await,
                Err(message) => {
                    self.set_error(error_text(SET_P2P_PARTICIPATION_ERROR_KEY, &message))
                }
            }
        } else if on {
            self.set_error(LocalizedText::key(P2P_REMOTE_ENABLE_KEY));
        } else {
            match self.nest_api.request_p2p_off(&device_id).await {
                Ok(()) => self.refresh().await,
                Err(e) => self.set_error(error_text(SET_P2P_PARTICIPATION_ERROR_KEY, e.detail())),
            }
        }
    }

    /// `device-member-remove-confirm-button` — remove the signed-in device
    /// without a matching entry (`DevicesSnapshot::members`) whose card
    /// carried `device_id` (`FleetMemberSummary::device_id`, the hex fleet
    /// id) **by its key**, through the door's member-addressed leg
    /// (`FleetRemoval::remove_member`; `ui/devices.md` § Members without a
    /// matching entry). The key is the armed card's own, so a refresh between
    /// arm and confirm that reshapes the list cannot retarget the removal.
    /// One leg: no nest row is chosen, so nothing is deleted at the nest and
    /// nothing is staged. A refusal or a failed write says so on
    /// `error-message` and the card stays to retry from; success refreshes,
    /// which re-reads the door and drops the card. An id no longer listed —
    /// never a fall-through to a position — and an unwired door are ignored.
    pub async fn remove_member_by_id(&self, device_id: String) {
        let listed = {
            let s = self.state.lock().unwrap();
            s.members.iter().any(|m| m.device_id == device_id)
        };
        if !listed {
            return;
        }
        let Some(door) = self.fleet_removal.lock().unwrap().clone() else {
            return;
        };
        let Ok(member) = fauna_core::hex32::decode(&device_id) else {
            // The snapshot's ids are this crate's own encoding; a decode
            // failure is a bug, not a user-facing state.
            tracing::warn!(target: "fauna_devices", %device_id, "remove_member: undecodable id");
            return;
        };
        tracing::info!(
            target: "fauna_devices",
            %device_id,
            "remove_member: writing the fleet removal by key"
        );
        match door.remove_member(member).await {
            Ok(()) => self.refresh().await,
            Err(refusal) => self.set_error(refusal_text(&refusal)),
        }
    }

    /// Delete folder `name`, then refresh.
    pub async fn delete_folder(&self, name: String) {
        match self.nest_api.delete_folder(&name).await {
            Ok(()) => self.refresh().await,
            Err(e) => self.set_error(error_text(DELETE_FOLDER_ERROR_KEY, e.detail())),
        }
    }

    /// Resolve conflict `id` by keeping the candidate `winning_manifest_hash`
    /// (or `None` for the candidate-free (mark-only) resolve), then refresh.
    pub async fn resolve_conflict(&self, id: i64, winning_manifest_hash: Option<String>) {
        // A choose-winner is signed by this device, so the seam vouches for
        // the pick against the judged version history, looked up under the
        // conflict's RENDERED path (`writer-signed-change-records.md` ruling
        // (10)(f)). A conflict this page does not hold, or a pick that is not
        // one of its candidates, has no path to look up under: refused here,
        // nothing sent. A mark-only resolve signs nothing and is untouched.
        let winner = match winning_manifest_hash {
            None => None,
            Some(manifest_hash) => {
                let path = {
                    let s = self.state.lock().unwrap();
                    s.conflicts
                        .iter()
                        .find(|c| c.id == id)
                        .filter(|c| {
                            c.candidates
                                .iter()
                                .any(|k| k.manifest_hash == manifest_hash)
                        })
                        .map(|c| c.path.clone())
                };
                let Some(path) = path else {
                    self.set_error(error_text(
                        RESOLVE_CONFLICT_ERROR_KEY,
                        "that version is not one of this conflict's candidates",
                    ));
                    return;
                };
                Some(crate::nest_api::ChosenWinner {
                    manifest_hash,
                    path,
                })
            }
        };
        match self.nest_api.resolve_conflict(id, winner).await {
            Ok(()) => self.refresh().await,
            Err(e) => self.set_error(error_text(RESOLVE_CONFLICT_ERROR_KEY, e.detail())),
        }
    }

    /// Save selective-sync paths for folder `name` (only the path fields are
    /// sent; retention is left unchanged), then refresh.
    ///
    /// The lists ride **sealed** beside their plaintext when this reader owns the
    /// set and holds a key ([`Self::seal_paths_for`]) — this gesture is the one
    /// durable writer of `folders.include_paths_sealed`, because the salt is
    /// the row id the nest mints at create (so the create wizard cannot seal) and
    /// the plaintext lives only on the nest row (so no engine catch-up pass can
    /// re-derive it once the flip scrubs it).
    pub async fn set_folder_paths(
        &self,
        name: String,
        include_paths: Option<Vec<String>>,
        exclude_paths: Option<Vec<String>>,
    ) {
        let (include_sealed, exclude_sealed) =
            self.seal_paths_for(&name, include_paths.as_deref(), exclude_paths.as_deref());
        match self
            .nest_api
            .set_folder_paths(
                &name,
                include_paths,
                exclude_paths,
                include_sealed,
                exclude_sealed,
            )
            .await
        {
            Ok(()) => self.refresh().await,
            Err(e) => self.set_error(error_text(SAVE_PATHS_ERROR_KEY, e.detail())),
        }
    }

    /// Set folder `name`'s conflict policy (`"auto"` | `"latest_wins_always"`)
    /// on the nest row in place, then refresh — the per-set
    /// `folder-conflict-policy-select` edit. The nest row is the single
    /// authoritative source the resolving device reads (file-sync.md §
    /// Conflicts, policy). Every other row field is left unchanged.
    pub async fn set_folder_conflict_policy(&self, name: String, conflict_policy: String) {
        match self
            .nest_api
            .set_folder_conflict_policy(&name, &conflict_policy)
            .await
        {
            Ok(()) => self.refresh().await,
            Err(e) => self.set_error(error_text(SET_CONFLICT_POLICY_ERROR_KEY, e.detail())),
        }
    }

    /// Set folder `name`'s audience (`folder-audience-select`), then refresh —
    /// the phase-4 write behind the picker (`ui/folders.md` § Audience and
    /// website serving).
    ///
    /// **Keyless**: no MLS engine and no content key, because the back-catalogue
    /// is moved by each device's own engine at its next catch-up off the
    /// projected audience, not by this caller.
    ///
    /// Every direction the picker offers rides here — `"public"`, `"private"`,
    /// and `"shared"` on a bound folder exiting its public window (the
    /// flip-back, selectable exactly there per `audience_options`); each
    /// device's engine converges off the projection, members' included, with no
    /// custody sentinel staged for any of them.
    ///
    /// ⚠ `→public` is confirm-gated in the UI (`folder-audience-public-confirm`)
    /// because a public folder rests **unsealed**, names and paths included. The
    /// app arms the confirm and calls this only once it is answered — the
    /// machine writes when told, so the gate is the app's to hold. That
    /// answered confirm is also where the owner's **attestation** is minted:
    /// the seam signs the flip under the key [`Self::set_audience_attestor`]
    /// wired (`encryption-at-rest.md` § Readable classes → *The
    /// declassification is owner-ATTESTED*), and a seat unseals only on that
    /// signature, never on the nest's report.
    pub async fn set_folder_audience(&self, name: String, audience: String) {
        let attestor = self.audience_attestor();
        match self
            .nest_api
            .set_folder_audience(&name, &audience, attestor)
            .await
        {
            Ok(()) => self.refresh().await,
            Err(e) => self.set_error(error_text(SET_AUDIENCE_ERROR_KEY, e.detail())),
        }
    }

    /// Flip folder `name`'s website serving (`folder-website-toggle`), then
    /// refresh — the only door to a website folder since phase 2 slice e retired
    /// the wizard's mode step.
    ///
    /// Orthogonal to the audience, which decides who may *read* what is
    /// published: the toggle stays enabled on a folder that is neither `public`
    /// nor paywalled — the setting is real, merely inert, and the app hints
    /// (`website_serve_hint`) rather than disabling, so a user can prepare a
    /// site before publishing it.
    pub async fn set_folder_website_enabled(&self, name: String, enabled: bool) {
        match self
            .nest_api
            .set_folder_website_enabled(&name, enabled)
            .await
        {
            Ok(()) => self.refresh().await,
            Err(e) => self.set_error(error_text(SET_WEBSITE_ERROR_KEY, e.detail())),
        }
    }

    /// Set folder `name`'s content residency (`folder-nest-residency-select` /
    /// `folder-residency-confirm`), then refresh — folders re-model phase 5;
    /// `file-sync.md` § Content residency owns the model.
    ///
    /// **Keyless**, like [`Self::set_folder_audience`]. Its own
    /// `folders.update` field, deliberately never folded into the batched
    /// `folder-nest-*` policy [`Self::set_folder_nest_place`] sends — an older
    /// writer's policy edit must never silently clear it.
    ///
    /// ⚠ **The flip to `metadata_only` is confirm-gated, and the gate is the
    /// app's to hold**: the nest deletes its chunk bytes for the folder on that
    /// write, so the app arms `folder-residency-confirm` naming exactly that
    /// and calls this only once answered — the machine writes when told. While
    /// armed the select keeps painting the folder's CURRENT residency, never
    /// the pending one (the `folder-audience-public-confirm` shape).
    pub async fn set_folder_residency(&self, name: String, residency: String) {
        match self.nest_api.set_folder_residency(&name, &residency).await {
            Ok(()) => self.refresh().await,
            Err(e) => self.set_error(error_text(SET_RESIDENCY_ERROR_KEY, e.detail())),
        }
    }

    /// Set one device place's flags on folder `name` (the expanded owner row's
    /// place editor — `folder-place-row` + its three checkboxes), then refresh.
    ///
    /// ⚠ **The point applies whole**: pass the seat's full triple, never just
    /// the box that moved, or the two left alone are silently cleared. Every
    /// flag point is writable since phase 2 slice f, so a combination the four
    /// legacy roles cannot name goes out unrounded — a leg paints three
    /// checkboxes and **no refusal**.
    ///
    /// The refresh is what repaints the row from nest truth rather than from an
    /// optimistic local flip; an app rendering the seat's boxes re-reads the
    /// roster (`fauna.folders.members.list`) after this returns, since the
    /// per-seat rows are not on this snapshot.
    pub async fn set_folder_place(
        &self,
        name: String,
        device_id: String,
        originates: bool,
        accepts: bool,
        applies_deletes: bool,
    ) {
        match self
            .nest_api
            .set_folder_place(&name, &device_id, originates, accepts, applies_deletes)
            .await
        {
            Ok(()) => self.refresh().await,
            Err(e) => self.set_error(error_text(SET_PLACE_ERROR_KEY, e.detail())),
        }
    }

    /// Set folder `name`'s **nest-place snapshot policy** — the three knobs
    /// behind `folder-nest-snapshots-select` / `-quiet-input` /
    /// `-retention-snapshots` / `-retention-days`, saved together by
    /// `folder-nest-save-button` — then refresh. Behavior owner:
    /// `docs/goal/behavior/backup-restore.md` § 8b.
    ///
    /// ⚠ **Whole-value, not a delta.** `snapshots` / `quiet_secs` passed as
    /// `None` are written as *unset* (§ 8b: "sent whole and applied whole"), so
    /// callers pass the full policy they want to rest — the editor reads all
    /// four controls and sends them together, which is also what lets a user
    /// return a knob to "use the default". Passing `None` for a knob the user
    /// did not touch would clear it silently.
    ///
    /// ⚠⚠ **`retention` does NOT follow that rule** — its wire field's `None`
    /// means *leave unchanged*, so clearing it means passing the canonical
    /// binds-nothing policy instead. [`DevicesNestApi::set_folder_nest_place`]
    /// states the full reason; a caller that passes `None` for a retention the
    /// user just emptied leaves the old bounds in force, silently.
    ///
    /// `version_retention` is the fourth per-place knob, its `Option` at THIS
    /// call: `None` = leave unchanged (an app without the knobs), `Some` = the
    /// whole version policy from `version_retention_write` (binds-nothing
    /// clears).
    pub async fn set_folder_nest_place(
        &self,
        name: String,
        snapshots: Option<bool>,
        quiet_secs: Option<i64>,
        retention: Option<String>,
        version_retention: Option<fauna_folders_machine::VersionRetentionWrite>,
    ) {
        match self
            .nest_api
            .set_folder_nest_place(&name, snapshots, quiet_secs, retention, version_retention)
            .await
        {
            Ok(()) => self.refresh().await,
            Err(e) => self.set_error(error_text(SET_NEST_PLACE_ERROR_KEY, e.detail())),
        }
    }

    /// The review list's one-tap **"use the other version"** on conflict
    /// `conflict_id` (an auto-resolved row): re-point the file at the latest
    /// retained candidate that is NOT the winning head — an ordinary restore
    /// record (`SyncClient::restore_version`; file-sync.md § Restore), so it is
    /// itself reversible and appears in version history. `device_id` is the
    /// caller's recording device (the same id the client passes Media's
    /// restore). On a `"latest_wins"` row the target is the unique retained
    /// loser; on a `"merged"` row (winner = the merged result, both parents
    /// retained) it is the more recent parent — finer control stays on the File
    /// Versions surface, which lists every retained version.
    ///
    /// No-ops with a page error on an unresolved row (nothing to re-point —
    /// resolution happens on the detecting device) or when no non-winning
    /// candidate is retained (candidate-free (mark-only) rows).
    pub async fn use_other_version(&self, conflict_id: i64, device_id: String) {
        // Snapshot the conflict under the lock, then drop it before IO.
        let conflict = {
            let s = self.state.lock().unwrap();
            s.conflicts.iter().find(|c| c.id == conflict_id).cloned()
        };
        let Some(conflict) = conflict else {
            self.set_error(error_text(USE_OTHER_VERSION_ERROR_KEY, "unknown conflict"));
            return;
        };
        let Some(winning) = conflict.winning_manifest_hash.clone() else {
            self.set_error(error_text(
                USE_OTHER_VERSION_ERROR_KEY,
                "conflict not yet resolved",
            ));
            return;
        };
        // Latest non-winning candidate: created_at, manifest-hash tiebreak —
        // deterministic across devices rendering the same row.
        let other = conflict
            .candidates
            .iter()
            .filter(|c| c.manifest_hash != winning)
            .max_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.manifest_hash.cmp(&b.manifest_hash))
            })
            .cloned();
        let Some(other) = other else {
            self.set_error(error_text(
                USE_OTHER_VERSION_ERROR_KEY,
                "no retained other version",
            ));
            return;
        };
        // The conflict row and its candidates are the nest's word — no field
        // of them signed — and the restore puts this device's signature over
        // the manifest. So the row only NAMES the version; the judged version
        // history vouches for it (`writer-signed-change-records.md` ruling
        // (10)(a)): looked up under the hash of the very path the record will
        // carry, and the VERSION's signed size and stamp are what is recorded.
        // Then the one shared restore decision (ruling (10)(b)).
        let (size_bytes, content_key_version) = match self
            .nest_api
            .judge_candidate(&conflict.folder, &conflict.path, &other.manifest_hash)
            .await
        {
            Ok(CandidateVerdict::Verbatim {
                size_bytes,
                content_key_version,
            }) => (size_bytes, content_key_version),
            Ok(CandidateVerdict::NeedsReseal) => {
                self.set_error(LocalizedText::key(OTHER_VERSION_NEEDS_HISTORY_KEY));
                return;
            }
            Ok(CandidateVerdict::NotAVersion) => {
                self.set_error(LocalizedText::key(OTHER_VERSION_UNVERIFIED_KEY));
                return;
            }
            Err(e) => {
                self.set_error(error_text(USE_OTHER_VERSION_ERROR_KEY, e.detail()));
                return;
            }
        };
        let path_sealed = self
            .seal_repoint_path(&conflict.folder, &conflict.path)
            .await;
        match self
            .nest_api
            .restore_file_version(
                &conflict.folder,
                &device_id,
                &conflict.path,
                other.manifest_hash,
                size_bytes,
                content_key_version,
                path_sealed,
            )
            .await
        {
            Ok(()) => self.refresh().await,
            Err(e) => self.set_error(error_text(USE_OTHER_VERSION_ERROR_KEY, e.detail())),
        }
    }
}

// ── Internal helpers (not FFI-exported) ──────────────────────────────────
impl DevicesMachine {
    /// The resolution half of [`Self::remove_device`]: which fleet members
    /// removing nest row `row_device_id` must exclude. No door wired (web — no
    /// account runtime) resolves nothing and the nest deletion proceeds alone.
    /// A principal that does not decode is nest-supplied garbage, not "no
    /// principal" — refused, since reading it as a row that names no fleet
    /// member would spare whichever device the row really is.
    ///
    /// It lives in THIS block, not beside its caller: `#[uniffi::export]`
    /// exports an impl block's non-`pub` methods too, so an exported
    /// non-FFI `Result` would compile here and then panic `uniffi_bindgen`
    /// with `unknown throw type` on every bindings-generating gate at once, on
    /// every dev platform — which the FFI throw-type lint exists to catch at
    /// merge. Making it private is not enough: it has to leave the block.
    async fn resolve_fleet_targets(
        door: Option<&dyn FleetRemoval>,
        row_device_id: &str,
        principal_hex: Option<String>,
    ) -> Result<Vec<[u8; 32]>, FleetRemovalRefusal> {
        let Some(door) = door else {
            return Ok(Vec::new());
        };
        let claimed = match principal_hex {
            None => None,
            Some(hex) => match fauna_core::hex32::decode(&hex) {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::warn!(
                        target: "fauna_devices",
                        error = %e,
                        "remove_device: row's principal did not decode to 32 bytes; refusing"
                    );
                    return Err(FleetRemovalRefusal::NotAMember);
                }
            },
        };
        door.resolve_removal(row_device_id, claimed).await
    }

    /// Set the page error and notify (the failure-branch counterpart to a
    /// successful gesture's `refresh()`, which clears the error itself). Logs
    /// the error once here at the producer — the per-app views paint
    /// `snapshot().error` reactively on every observer tick, so logging there
    /// would re-fire on every repaint (observability.md § Log on the *event*,
    /// not the *paint*). `log_line` is redaction-safe (i18n key + error
    /// metadata, never plaintext bodies). Shared Rust → all seven apps.
    fn set_error(&self, err: LocalizedText) {
        tracing::warn!(target: "fauna_devices", "{}", err.log_line());
        self.state.lock().unwrap().error = Some(err);
        self.observer.on_changed();
    }
}

/// Forwards the embedded wizard's `on_changed` to the page observer, so opening
/// / driving the wizard re-renders the Devices page (the wizard snapshot is part
/// of `DevicesSnapshot`).
struct WizardObserverBridge(Arc<dyn DevicesObserver>);

impl FolderWizardObserver for WizardObserverBridge {
    fn on_changed(&self) {
        self.0.on_changed();
    }
}

/// The one JSON shape of `fauna_e2e_agent::DEVICES_REFRESHES_KEY` —
/// `{"started", "completed", "committed_gen"}` from
/// [`DevicesMachine::refresh_counts`], the `fauna_feed::feed_reloads_json` twin.
/// `None` (no machine built yet — pre-auth) is the legitimate zero triple; an app
/// without the leg publishes no key at all, which a reader refuses loudly.
pub fn devices_refreshes_json(counts: Option<(u64, u64, u64)>) -> serde_json::Value {
    let (started, completed, committed_gen) = counts.unwrap_or((0, 0, 0));
    serde_json::json!({
        "started": started,
        "completed": completed,
        "committed_gen": committed_gen,
    })
}

/// What `fauna.sync.devices.delete`'s answer means for the staged intent.
/// `NotFound` is the deletion's own postcondition — the row is gone, however
/// that came about — so the fleet leg must finish. `Conflict` / `BadRequest`
/// are the nest definitively keeping the row — `Conflict` including its
/// `guardian_marked` refusal (the seam's error mapping).
/// Everything else is `Unknown`, deliberately including a refusal that
/// mapping does not name: the runtime's reconcile then reads the roster, and
/// a row still there is waited out for the in-flight bound before the intent
/// is dropped unwritten — the same end state, later, with no guess made here.
fn nest_deletion_outcome(deletion: &Result<(), crate::nest_api::DevicesApiError>) -> NestDeletion {
    use crate::nest_api::DevicesApiError as E;
    match deletion {
        Ok(()) | Err(E::NotFound { .. }) => NestDeletion::Gone,
        Err(E::Conflict { .. } | E::BadRequest { .. }) => NestDeletion::Kept,
        Err(E::Transient { .. }) => NestDeletion::Unknown,
    }
}

/// The member cards from the door's read — the fingerprint rendered ONCE,
/// here, through the same formatter the own row uses.
fn render_members(view: &FleetMembersView) -> Vec<FleetMemberSummary> {
    view.unaccounted
        .iter()
        .map(|m| FleetMemberSummary {
            device_id: fauna_core::hex32::encode(&m.device_id),
            fingerprint: fauna_core::format::fleet_fingerprint(&m.device_id),
            enrolled_at_ms: m.enrolled_at_ms,
        })
        .collect()
}

/// The page error for a removal refused before anything was deleted.
fn refusal_text(refusal: &FleetRemovalRefusal) -> LocalizedText {
    match refusal {
        FleetRemovalRefusal::OwnDevice => LocalizedText::key(REMOVE_OWN_DEVICE_ERROR_KEY),
        FleetRemovalRefusal::NotAMember => LocalizedText::key(REMOVE_UNVERIFIED_DEVICE_ERROR_KEY),
        FleetRemovalRefusal::RowMismatch => LocalizedText::key(REMOVE_ROW_MISMATCH_ERROR_KEY),
        FleetRemovalRefusal::Unavailable(message) => error_text(REMOVE_DEVICE_ERROR_KEY, message),
    }
}

fn error_text(key: &str, detail: &str) -> LocalizedText {
    LocalizedText::key_arg(key, "message", detail.to_string())
}
