//! The Media page — the cross-set, Explorer-style browser over the media inside
//! the user's folders (`ui/media.md` § Layout & flow).
//!
//! A paint shell over the shared, observer-driven
//! [`MediaMachine`](fauna_media_machine::MediaMachine), consumed **directly
//! in-process** (no FFI hop) exactly as linux consumes it — tui is the 2nd
//! direct-Rust client. Cross-set aggregation, sort, filter and the "which set
//! does an upload target" policy all already ran in shared Rust
//! (`media.md` § Where logic lives / rule 2); this module renders the resulting
//! [`MediaPageSnapshot`](fauna_media_machine::MediaPageSnapshot) and forwards
//! gestures back to the machine. It holds no view-model state the machine owns.
//!
//! Media is the **content plane only** (`media.md` rule 4): it reads folders
//! and never configures them — no place-flag / retention / roster
//! affordance belongs here.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_i18n::strings::media as t;
use fauna_i18n::strings::share_link as sl;
use fauna_media_machine::{
    FILTER_ALL_VALUE, FileVersionSummary, MediaItemSummary, MediaMachine, MediaObserver,
    MediaPageSnapshot,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, DataMessage, UiMessage};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::image_cache::{ImageCache, ImageState};

// `FILTER_ALL_VALUE` (the `media-folder-filter` all-media sentinel) is
// `fauna_media_machine::FILTER_ALL_VALUE`, imported above — this page
// hand-copied it until this lift.

/// The sort keys `media-sort-select` offers — the stable option **values** the
/// cross-app suites drive it with (`"name"`/`"size"`/`"date"`), not the
/// localized labels. A raw-value picker like `SelectTarget::ReminderOffset`;
/// the human reads the localized label off [`Element::label`].
const SORT_VALUES: [&str; 3] = ["name", "size", "date"];

/// The localized label for a sort value — paint-only (`Element::label`), via
/// the shared `fauna_core::format::media_sort_label` decision (priority #2).
fn sort_label(value: &str) -> String {
    crate::format::media_sort_label(value)
}

/// The directions `media-sort-direction` offers — stable option **values**, same
/// raw-value contract as [`SORT_VALUES`]. Ascending is the default, so it leads.
const SORT_DIRECTION_VALUES: [&str; 2] = ["ascending", "descending"];

/// The value `media-sort-direction` reports for a `descending` flag.
fn sort_direction_value(descending: bool) -> &'static str {
    if descending {
        "descending"
    } else {
        "ascending"
    }
}

/// The localized label for a direction value — paint-only (`Element::label`),
/// via the shared `fauna_core::format::media_sort_direction_label` decision.
fn sort_direction_label(value: &str) -> String {
    crate::format::media_sort_direction_label(value == "descending")
}

// ── Gestures ────────────────────────────────────────────────────────────────

/// A Media-page gesture.
#[derive(Debug, Clone)]
pub enum Action {
    /// `media-view-toggle` — flip list ↔ thumbnail grid.
    ToggleView,
    /// `media-sort-select` — re-sort by the picked key.
    SetSort(String),
    /// `media-sort-direction` — re-sort ascending/descending on the active key.
    SetSortDirection(String),
    /// `media-folder-filter` — scope to one set, or the all-media default.
    SetFilter(String),
    /// `upload-button` — seal + POST + record the picked file into the selected
    /// set via the shared `upload_selected` gesture.
    Upload,
    /// `media-item` tap/open — open the `media-item-detail` surface for the item
    /// at this index of the rendered list.
    OpenDetail(usize),
    /// Open `media-item-detail` for the file a **durable identity pair** names —
    /// the `SearchNav::File` deep link's destination (`ui/search.md` § User
    /// actions).
    ///
    /// Its own action rather than a conversion into [`Self::OpenDetail`]'s index
    /// because the two address items differently and only one of them survives
    /// the user's browse state: an index is a position in the *filtered, sorted*
    /// rendered list, while this pair is the file's identity regardless of
    /// filter, sort, or a set rename. Resolving it to an index at the call site
    /// would reintroduce exactly the coupling the pair exists to avoid.
    OpenFile { folder_id: i64, path_hash: String },
    /// `media-item-detail-close-button`.
    CloseDetail,
    /// `file-version-show-pruned-toggle` — the recovery browse switch
    /// (`file-versions.md` § Retention (3)): ON re-lists with
    /// `include_pruned`, so soft-pruned rows appear with their badge +
    /// undelete button; OFF returns to the live-only listing.
    TogglePruned,
    /// `file-version-undelete-button` on the (pruned) version row at this
    /// index — recovers it into the live listable population.
    Undelete(usize),
    /// `file-version-restore-button` on the version row at this index — arms the
    /// lightweight confirm rather than restoring outright.
    ArmRestore(usize),
    /// `file-version-restore-confirm-button`.
    ConfirmRestore,
    /// `file-version-restore-cancel-button`.
    CancelRestore,
    /// `media-delete-button` — arms the single delete confirm rather than
    /// deleting outright (`media.md` § Element IDs — a media delete records a
    /// tombstone and leaves the version rows, so it is a light single confirm,
    /// deliberately NOT the backups typed-id immediate-delete ceremony).
    ArmDelete,
    /// `media-delete-confirm-button` — runs the shared `MediaMachine::delete`.
    ConfirmDelete,
    /// `media-delete-cancel-button` — a pure no-op decline: no mutation, no
    /// error, the detail surface stays open.
    CancelDelete,
    /// `media-external-open-button` — hand this audio/video item to the OS
    /// default handler, honoring the `tui-settings-external-media` mode: `ask`
    /// arms the inline confirm, `always` hands off directly (`apps/tui.md`
    /// § External media handoff; under `never` the trigger is never painted).
    ExternalOpen,
    /// `media-external-open-confirm-button` — the armed confirm's launch.
    ConfirmExternalOpen,
    /// `media-external-open-cancel-button` — a pure no-op decline: nothing
    /// downloads, nothing decrypts, nothing launches; the detail stays open.
    CancelExternalOpen,
    /// `media-item-detail-download-button` — save the opened file's plaintext
    /// into the downloads dir under its own name (`media.md` § Element IDs).
    /// For any item, unlike the AV-only handoff above; no confirm — a save is
    /// not a launch.
    Download,
    /// `share-link-button` — open the create surface on the detail's file
    /// (`share-links.md` § Flows → Create).
    OpenShareCreate,
    /// `share-link-expiry-select`.
    SetShareExpiry(String),
    /// `share-link-create-button` — the shared mint → seal → register.
    CreateShareLink,
    /// `share-link-cancel-button` — close the create surface.
    CloseShareCreate,
    /// `share-link-copy-button` — copy the revealed URL (OSC 52).
    CopyShareUrl,
    /// `share-link-list-button` — open + load the list.
    OpenShareLinks,
    /// `share-link-list-close-button`.
    CloseShareLinks,
    /// `share-link-item-copy-button` on the row at this index — copy its
    /// verified re-derived URL (OSC 52).
    CopyShareLinkRow(usize),
    /// `share-link-revoke-button` on the row at this index — arm the confirm.
    ArmShareRevoke(usize),
    /// `share-link-revoke-confirm-button`.
    ConfirmShareRevoke,
    /// `share-link-revoke-cancel-button`.
    CancelShareRevoke,
}

impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`). Exhaustive with no fallback arm,
    /// so a new media gesture must answer the offline question.
    ///
    /// **The file writes do not desensitize, and that is the classification
    /// talking rather than an omission.** The media content plane consumes
    /// `fauna.media.list` + `fauna.files.versions.list` to read, and
    /// `fauna.sync.changes.record` to write — every file mutation here is a
    /// sync-change row, which the shared table classifies **`OfflineSafe`**,
    /// because that is exactly the
    /// content-addressed, replayable write class 1 describes. Upload, delete
    /// and restore therefore stay live with no nest, and they *should*: they are
    /// the writes the W4 (account-data-plane.md § Workstreams) outbox exists to carry.
    ///
    /// **The bytes move over a seam the table does not describe.** The upload's
    /// blob POST and the thumbnail/handoff blob GETs ride the bulk-binary
    /// `/api/v1/blob` carve-out, not WS-RPC, so they have no kind to classify —
    /// which is why the external-handoff gestures answer `None` for a different
    /// reason than the local ones do, and why `Upload` names the sync row it
    /// records rather than the POST it also performs.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            // The three writes. Each records one sync-change row — create for an
            // upload, a `delete` tombstone, and restore's re-point `modify`
            // (`file-sync.md` § Restore) — through the same shared
            // `SyncClient` call, so they cannot drift apart.
            Action::Upload | Action::ConfirmDelete | Action::ConfirmRestore => {
                Some("fauna.sync.changes.record")
            }

            // Opening the detail loads that file's version rows. A `Read`, which
            // the gate declines to decide on; declared anyway so a later
            // reclassification reaches this page without re-deriving it.
            Action::OpenDetail(_) | Action::OpenFile { .. } | Action::TogglePruned => {
                Some("fauna.files.versions.list")
            }

            // The recovery verb: flips a nest-side row state (soft-pruned →
            // live). NOT a sync-change record — nothing content-addressed is
            // written — so it takes its own kind and the gate's default
            // treatment for it, rather than riding the OfflineSafe class.
            Action::Undelete(_) => Some("fauna.files.versions.undelete"),

            // Share links (`share-links.md` § Flows): create and revoke are
            // `OnlineOnly` (the URL is revealed only after registration
            // succeeds), so the gate desensitizes them offline; the list is a
            // `Read`.
            Action::CreateShareLink => Some("fauna.share.create"),
            Action::ConfirmShareRevoke => Some("fauna.share.revoke"),
            Action::OpenShareLinks => Some("fauna.share.list"),

            // The external handoff. Not local — it downloads and decrypts real
            // bytes — but the fetch is the bulk-binary blob carve-out, which has
            // no wire kind, so there is nothing here for the table to classify.
            // (`ExternalOpen` additionally may only *arm* the confirm, depending
            // on the `external_media` preference; both of its outcomes are
            // undeclarable for this same reason.)
            // The download rides the same blob carve-out, for the same reason.
            Action::ExternalOpen | Action::ConfirmExternalOpen | Action::Download => None,

            // Local. The view/sort/filter controls re-project the snapshot the
            // page already holds, closing the detail drops client state, and the
            // three arm/cancel pairs only toggle a confirm — the mutation is the
            // confirm's own gesture, declared above.
            Action::ToggleView
            | Action::SetSort(_)
            | Action::SetSortDirection(_)
            | Action::SetFilter(_)
            | Action::CloseDetail
            | Action::ArmRestore(_)
            | Action::CancelRestore
            | Action::ArmDelete
            | Action::CancelDelete
            | Action::CancelExternalOpen
            | Action::OpenShareCreate
            | Action::SetShareExpiry(_)
            | Action::CloseShareCreate
            | Action::CopyShareUrl
            | Action::CloseShareLinks
            | Action::CopyShareLinkRow(_)
            | Action::ArmShareRevoke(_)
            | Action::CancelShareRevoke => None,
        }
    }
}

// ── State ───────────────────────────────────────────────────────────────────

/// The open `media-item-detail` surface: which item, its loaded version rows,
/// and which version (if any) has its restore confirm armed.
#[derive(Debug, Clone, Default)]
pub struct DetailState {
    /// The opened item's set + path (its identity for the version queries) and
    /// display name (`media-item-detail-name`).
    pub folder: String,
    pub path: String,
    pub name: String,
    /// Version rows oldest→newest, `None` until the async load resolves.
    pub versions: Option<Vec<FileVersionSummary>>,
    /// The recovery browse is on (`file-version-show-pruned-toggle`): version
    /// loads ask for `include_pruned`, and soft-pruned rows render with their
    /// badge + undelete button. Resets with the detail surface.
    pub show_pruned: bool,
    /// Index into `versions` whose restore confirm is armed
    /// (`file-version-restore-confirm-modal` is painted iff this is `Some`).
    pub arming: Option<usize>,
    /// The external-open confirm is armed (`media-external-open-confirm-modal`
    /// is painted iff true — the `ask` mode's inline prompt).
    pub external_arming: bool,
    /// The delete confirm is armed (`media-delete-confirm-modal` is painted iff
    /// true). A separate flag from `arming`/`external_arming` rather than one
    /// tri-state: the three confirms gate unrelated gestures, and collapsing
    /// them would let a future edit arm one and fire another.
    pub delete_arming: bool,
    /// `Some(scope value)` when the detail was opened from a **followed browse
    /// scope** (`media.md` § Followed public folders): the surface is
    /// read-only — no version history, no restore, no delete — and its one
    /// action, the external handoff, routes through the keyless
    /// `download_followed`, which resolves the head manifest by path (a
    /// followed item has no version rows to read one from; the public plane
    /// is head-only).
    pub followed_scope_value: Option<String>,
}

/// The Media page's state.
///
/// The `MediaMachine` is built at the post-auth hook and dropped at sign-out, so
/// its lifetime is the session's — the same session-scoped shape `feed` and
/// `conversations` use (and deliberately not a process-wide singleton).
#[derive(Default)]
pub struct MediaState {
    /// `None` before the first sign-in — the e2e state serializer runs pre-auth,
    /// so every reader degrades gracefully rather than panicking.
    pub machine: Option<Arc<MediaMachine>>,
    /// The owner 32-byte `BackupKey`, derived once at login (per-actor,
    /// immutable): the key an upload seals under and a thumbnail decrypts under.
    pub backup_key: Vec<u8>,
    /// This device's stable sync device id (hex) — the recording identity every
    /// `fauna.sync.changes.record` carries. `None` when the device-id store
    /// could not be read; the upload gesture then surfaces that on
    /// `error-message` rather than uploading.
    pub device_id: Option<String>,
    /// The `file-upload` picker's staged path. A **path**, not an OS file picker
    /// (`tui.md` § Declared platform absences 4); read at submit.
    pub file_input: String,
    /// The open `media-item-detail`, if any.
    pub detail: Option<DetailState>,
    /// Rasterized `media-thumbnail` art, keyed by the item's `thumbnail_hash`.
    ///
    /// **Why tui caches when linux does not.** Linux re-runs the whole fetch on
    /// every render (`apps/fauna-linux/src/views/media/item.rs`) because GTK
    /// rebuilds the FlowBox each tick — a handful of widget rebuilds. Ratatui is
    /// immediate-mode: this page's element list is rebuilt on **every frame**,
    /// so a fetch driven from the element builder would re-issue every
    /// thumbnail's GET on every keystroke. The cache is what makes the same
    /// behavior affordable here; keying by content hash makes it correct — a
    /// hash is the bytes, so an entry can never go stale, and the same blob
    /// referenced from two folders is fetched once.
    ///
    /// Bounded by the item count of what the user has actually browsed this
    /// session, and dropped with the session (it hangs off `MediaState`).
    pub thumbnails: crate::image_cache::ImageCache,
}

impl MediaState {
    pub fn snapshot(&self) -> Option<MediaPageSnapshot> {
        self.machine.as_ref().map(|m| m.snapshot())
    }
}

/// Forward machine notifications into the render loop's `UiMessage` channel.
///
/// `on_changed` fires **synchronously on whatever thread mutated**, so it must
/// not touch `App`; the tick carries no payload because the observer contract is
/// "read a fresh snapshot off the machine" — coalescing duplicates loses nothing.
struct TuiMediaObserver {
    tx: UnboundedSender<UiMessage>,
}

impl MediaObserver for TuiMediaObserver {
    fn on_changed(&self) {
        // A closed channel means the app is shutting down — nothing to notify.
        let _ = self.tx.send(UiMessage::Data(DataMessage::MediaChanged));
    }
}

/// Build the media machine over the authed transport, attach the observer, and
/// derive the per-actor upload/decrypt key + this device's recording id.
///
/// Called from the one post-auth hook (`session::establish`), so every path that
/// produces a session gets a Media page without each remembering to wire one.
/// The first `fauna.media.list` pull is **not** kicked here: entering the tab is
/// the refresh trigger ([`nav_enter_op`]), matching linux's `connect_map`.
#[allow(clippy::too_many_arguments)]
pub fn init(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    folder_keys: std::sync::Arc<dyn fauna_client_folders::FolderKeyStore>,
    tx: &UnboundedSender<UiMessage>,
    succession_predecessors: &[fauna_core::crypto::BackupKey],
    attested_predecessors: &[fauna_core::identity::ActorId],
    predecessor_chain: &[(fauna_core::identity::ActorId, fauna_core::crypto::BackupKey)],
    follows: Arc<dyn fauna_client_config::FollowsStore>,
) -> MediaState {
    let observer: Arc<dyn MediaObserver> = Arc::new(TuiMediaObserver { tx: tx.clone() });
    // The shared-folder content-key resolver (Phase 0 — the read leg): lets a
    // `download_file` of a shared set open under content keys from custody rather
    // than the owner `BackupKey`, so a member (or the owner) can decrypt shared
    // content via the Media external-open gesture. Built over the account's
    // folder-key custody; the same shared `NestFolderKeyResolver` the FFI +
    // linux Media glue construct.
    let resolver: Arc<dyn fauna_media_machine::FolderKeyResolver> = Arc::new(
        fauna_client_folders::NestFolderKeyResolver::new(Arc::clone(&nest), folder_keys),
    );
    let machine = fauna_media_machine::build_media_machine_with_folder_keys(
        Arc::clone(&nest),
        observer,
        Some(resolver),
    );
    // The followed browse-scope source (`media.md` § Followed public folders):
    // the same account-store-backed source type the Folders page reads follows
    // through (`follows` — `settings::follows_door`), here feeding `media-folder-filter`'s followed options and the
    // on-demand listing fetch. Its availability verdicts ride the shared
    // staleness budget, and a browse fetch feeds its cache — so wiring it puts
    // no per-follow round trip on every refresh.
    machine.set_followed_media_source(Arc::new(
        fauna_devices_machine::StoreFollowedFoldersSource::new(Arc::clone(&nest), follows),
    ));
    // Write-side label custody for the delete/restore gestures (S8 D2): the
    // same per-actor owner key the upload gesture seals with, injected once so
    // those records seal instead of resting plaintext-only.
    machine.set_owner_backup_key(
        fauna_core::crypto::BackupKey::derive(&secret)
            .to_bytes()
            .to_vec(),
    );
    // READ-side custody for a successor: the media corpus a succession
    // re-pointed is still sealed under the identities it succeeded from
    // (`succession-aftermath.md` § Re-key scope — *media*, folders, backups).
    // Deliberately a second injection rather than an extra key on the line
    // above: that one is the delete/restore **seal** root, and a retired key
    // must never reach it.
    if !succession_predecessors.is_empty() {
        machine.set_predecessor_backup_keys(
            succession_predecessors
                .iter()
                .map(|k| k.to_bytes().to_vec())
                .collect(),
        );
    }
    // …and the READER's half of the same walk: the attested predecessor ids
    // (`AccountRegistry::attested_predecessor_actor_ids`), so the listing's
    // judge reads a row a retired identity signed as this account's own
    // (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    // ruling (8)(b)). Without the keys above such a row would list and not
    // open; without these ids it would open and not list.
    if !attested_predecessors.is_empty() {
        machine.set_predecessor_actor_ids(
            attested_predecessors
                .iter()
                .map(|id| id.0.to_vec())
                .collect(),
        );
    }
    // …and the keys PAIRED with those identities: a row signed as a
    // predecessor opens only under that identity's root and its
    // predecessors' (ruling (8)(c)), so the paired form replaces the bare
    // keys above wherever the registry could name them.
    if !predecessor_chain.is_empty() {
        let (ids, keys) = predecessor_chain
            .iter()
            .map(|(id, key)| (id.0.to_vec(), key.to_bytes().to_vec()))
            .unzip();
        machine.set_predecessor_chain(ids, keys);
    }
    // The share-link author (`share-links.md` § Where logic lives): the
    // session's identity signs the token and seals its filename, and the links
    // point at this session's nest. After the predecessors, which it reads.
    machine.set_share_author(secret.to_vec(), nest.nest_url());

    MediaState {
        machine: Some(machine),
        backup_key: fauna_core::crypto::BackupKey::derive(&secret)
            .to_bytes()
            .to_vec(),
        device_id: device_id_hex_for_secret(secret),
        file_input: String::new(),
        detail: None,
        thumbnails: ImageCache::new(),
    }
}

/// The sync device id `actor_id_hex` registers under on this install, as hex,
/// or `None` when the store is unreadable or the actor id is not 32-byte hex.
///
/// **Per account, never per machine** (`sync-agent-credentials.md`
/// § Credential model, the 2026-09-20 ruling): read from (or derived into) the
/// dedicated `device.db` under the actor's own scope
/// ([`crate::account_scope::account_state_dir`]) via the shared
/// `fauna_sync_engine::engine_lifecycle::load_device_id_for_actor`. A
/// persisted id wins. An empty store derives
/// `derive_device_id(install_secret, actor_id)` from the install device secret
/// under [`crate::account_scope::install_sync_dir_under`]. Two accounts on one
/// install therefore register under unlinkable ids, while the sign-in after a
/// sign-out (which erases the scope) re-derives the same id and comes back to
/// its own `sync_devices` row. Every reader of this device's sync identity
/// goes through here with the actor its session serves, so the id the sync
/// agent registers under is the id the Media upload records, the Devices page
/// marks, and the account runtime enrolls against — the linux
/// `sync::device_id` shape.
///
/// A failure is not fatal: each caller degrades on `None` (the upload gesture
/// surfaces it on `error-message` rather than uploading; the page's reads keep
/// working).
pub(crate) fn device_id_hex(actor_id_hex: &str) -> Option<String> {
    device_id_hex_under(&crate::session::config_dir()?, actor_id_hex)
}

/// [`device_id_hex`] for the account whose identity secret the caller holds —
/// the same session actor, derived from the secret rather than carried beside
/// it, for the inits and op futures that were handed the secret alone.
pub(crate) fn device_id_hex_for_secret(secret: [u8; 32]) -> Option<String> {
    device_id_hex(&fauna_core::identity::ActorKeypair::from_secret(secret).actor_id_hex())
}

/// [`device_id_hex`] minus the config-dir resolution — the flat-base seam the
/// in-crate tests drive with a temp path instead of mutating the
/// process-global `XDG_CONFIG_HOME` (`account_scope`'s sign-out tests read
/// through it too, hence `pub(crate)`).
pub(crate) fn device_id_hex_under(flat: &std::path::Path, actor_id_hex: &str) -> Option<String> {
    let actor_id = fauna_core::hex32::decode(actor_id_hex)
        .inspect_err(|e| tracing::warn!("media: the session actor id is not 32-byte hex: {e}"))
        .ok()?;
    let scope = crate::account_scope::account_state_dir_under(flat, actor_id_hex);
    fauna_sync_engine::engine_lifecycle::load_device_id_for_actor(
        &crate::account_scope::install_sync_dir_under(flat),
        &scope,
        &actor_id,
    )
    .inspect_err(|e| tracing::warn!("media: could not read the device id: {e}"))
    .ok()
    .map(|d| fauna_core::hex32::encode(&d))
}

/// Adopt `hex` as the sync device id `actor_id_hex` registers under, writing it
/// to the same actor-scoped `device.db` [`device_id_hex`] reads. Returns whether
/// it was stored.
///
/// **The e2e session door's precondition seam, and nothing else.** `devices.md`
/// § New Platform Implementation Checklist steps 4–5 make one value do two jobs
/// on every conformant client: the id the app **registers** with
/// (`fauna.sync.register`) is the id the this-device marker **compares** against
/// (§ This-device marker — "the same 32-byte-hex value it registered with"). A
/// real client satisfies that by generating once and reusing; the e2e session
/// patch hands the app a *well-known* id instead, so the harness can name the
/// row that ought to carry the badge. Before this existed the patch's id reached
/// only the accounts registry (`fauna_client_accounts`, a per-actor credential
/// slot), while the marker kept reading a randomly-minted `device.db` — two
/// stores with no writer between them, which is exactly why
/// `test_device_card_marks_this_device`'s documented premise was false.
///
/// Call this **before** [`crate::session::establish`], which starts the sync
/// agent: the agent reads the id once at init (`sync_agent::init`) and registers
/// under it, so a later write would not move the device the nest already knows.
///
/// Not gated on `debug_assertions`/`e2e-agent` itself, matching its only caller
/// [`crate::session::apply_session_patch`] — the whole seam is the automation
/// door, reachable solely from the compile-gated agent (convention 15).
///
/// `actor_id_hex` is the account the patch just signed in: the forced id lands
/// in THAT account's scope, and no other account's reads see it.
pub(crate) fn adopt_device_id_hex(actor_id_hex: &str, hex: &str) -> bool {
    let Some(flat) = crate::session::config_dir() else {
        return false;
    };
    adopt_device_id_under(&flat, actor_id_hex, hex)
}

/// [`adopt_device_id_hex`] minus the config-dir resolution — the flat-base
/// seam the in-crate tests drive with a temp path instead of mutating the
/// process-global `XDG_CONFIG_HOME` (the `conv_backend::start_with_db` shape).
/// Shares linux's `sync::adopt_device_id_hex` implementation via
/// `fauna_sync_engine::engine_lifecycle::store_device_id_hex`.
///
/// A malformed actor id is refused before anything is written: the scope
/// resolver would otherwise fall back to the flat base itself, and a forced id
/// written there would be a machine-flat store again.
fn adopt_device_id_under(flat: &std::path::Path, actor_id_hex: &str, hex: &str) -> bool {
    if let Err(e) = fauna_core::hex32::decode(actor_id_hex) {
        tracing::warn!("media: refusing to adopt a device id for a malformed actor id: {e}");
        return false;
    }
    let scope = crate::account_scope::account_state_dir_under(flat, actor_id_hex);
    fauna_sync_engine::engine_lifecycle::store_device_id_hex(&scope, hex)
        .inspect_err(|e| tracing::warn!("media: {e}"))
        .is_ok()
}

// ── Field access ────────────────────────────────────────────────────────────

/// A Media-page editable field.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MediaField {
    /// `file-upload` — the staged path (`tui.md` § Declared platform absences 4:
    /// path entry, not an OS picker).
    FileUpload,
}

pub fn field(state: &MediaState, field: &MediaField) -> String {
    match field {
        MediaField::FileUpload => state.file_input.clone(),
    }
}

pub fn set_field(state: &mut MediaState, field: MediaField, value: String) {
    match field {
        MediaField::FileUpload => state.file_input = value,
    }
}

// ── Local half ──────────────────────────────────────────────────────────────

/// Apply a gesture's synchronous local half; return the network half to run.
///
/// The view-state setters (`set_view_grid` / `set_sort` / `set_filter`) are
/// **machine** calls, not local buffers: the machine owns the view state and
/// re-sorts/filters in shared Rust, notifying the observer, which repaints. They
/// are synchronous and infallible, so they land here and return `None`.
/// Open `media-item-detail` on one item and ask for its versions — the one
/// definition of "the detail surface is now showing this file", shared by the
/// index-addressed open (`media-item` tap) and the identity-addressed one (a
/// `SearchNav::File` deep link) so the two can never disagree about what
/// opening means.
///
/// Opens with no version rows yet; the returned op is the network half the
/// caller runs, and its outcome fills them in.
fn open_detail_for(
    app: &mut App,
    machine: &Arc<MediaMachine>,
    item: MediaItemSummary,
) -> Option<Op> {
    let followed_scope_value = machine.snapshot().followed_scope.map(|s| s.value);
    app.media.detail = Some(DetailState {
        folder: item.folder.clone(),
        path: item.path.clone(),
        name: item.name,
        versions: None,
        show_pruned: false,
        arming: None,
        external_arming: false,
        delete_arming: false,
        followed_scope_value: followed_scope_value.clone(),
    });
    // A followed item has no version history to load — the public plane is
    // head-only — so its detail opens complete, with no network half (and the
    // render suppresses the whole versions section for it).
    if followed_scope_value.is_some() {
        return None;
    }
    Some(Op::LoadVersions {
        machine: Arc::clone(machine),
        folder: item.folder,
        path: item.path,
        include_pruned: false,
    })
}

pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    let machine = app.media.machine.as_ref().map(Arc::clone);
    match action {
        Action::ToggleView => {
            let m = machine?;
            let grid = m.snapshot().view_grid;
            m.set_view_grid(!grid);
            None
        }
        Action::SetSort(value) => {
            machine?.set_sort(value);
            None
        }
        Action::SetSortDirection(value) => {
            machine?.set_descending(value == "descending");
            None
        }
        Action::SetFilter(value) => {
            let m = machine?;
            // A followed scope's minted value routes to the async on-demand
            // entry fetch (`media.md` § Followed public folders); a set name
            // (or the all-media sentinel) stays the sync view-state setter.
            // The machine's option list, not this app, is the authority on
            // which values are followed.
            if m.snapshot().followed.iter().any(|f| f.value == value) {
                return Some(Op::SelectFollowedScope { machine: m, value });
            }
            let filter = (value != FILTER_ALL_VALUE).then_some(value);
            m.set_filter(filter);
            None
        }
        Action::Upload => {
            let m = machine?;
            let picked = app.media.file_input.trim().to_string();
            if picked.is_empty() {
                // Say so. This was a silent `return None` on the theory that the
                // prompt already guides, which predates field evidence: a live
                // user pressed Upload with an empty box and read the silence as
                // a broken button (field report). A
                // pressed button must always answer — the app's own
                // `error-message` is the answer (e2e convention 2).
                app.errors
                    .insert(crate::pages::Page::Media, t::FILE_PATH_REQUIRED.to_string());
                return None;
            }
            // File IO + the device-id read are the only client glue (the shared
            // gesture takes bytes, not a path). Both are done here, before the
            // spawn, so a read / device-id failure surfaces at once on
            // `error-message`; a successful upload's refresh then clears it.
            let raw_bytes = match std::fs::read(&picked) {
                Ok(b) => b,
                Err(e) => {
                    app.errors
                        .insert(crate::pages::Page::Media, t::error_upload(&e.to_string()));
                    return None;
                }
            };
            let Some(device_id) = app.media.device_id.clone() else {
                app.errors.insert(
                    crate::pages::Page::Media,
                    t::error_upload("no sync device id"),
                );
                return None;
            };
            // The member path within the set is the picked file's basename —
            // linux's shape, and what the version-history suite relies on when
            // it re-uploads the same basename to record a second version.
            let member_path = std::path::Path::new(&picked)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(picked);
            Some(Op::Upload {
                machine: m,
                device_id,
                path: member_path,
                raw_bytes,
                backup_key: app.media.backup_key.clone(),
            })
        }
        Action::OpenDetail(index) => {
            let m = machine?;
            let item = m.snapshot().items.get(index).cloned()?;
            open_detail_for(app, &m, item)
        }
        Action::OpenFile {
            folder_id,
            path_hash,
        } => {
            let m = machine?;
            let Some(item) = m.locate_file(folder_id, path_hash) else {
                // Deleted, renamed, or in a set this seat cannot read — the
                // DROPPED rule with a user waiting on it, so it is stated
                // rather than swallowed (an unchanged page reads as a dead row).
                app.errors
                    .insert(crate::pages::Page::Media, t::FILE_NOT_FOUND.to_string());
                return None;
            };
            // Point the browse at the item's own set. The deep link says nothing
            // about the filter the user last left here, and closing the detail
            // onto a list that excludes the very file they just opened is the
            // one outcome that would read as a bug — this also answers "where
            // does this file live", which after a search jump is the question.
            m.set_filter(Some(item.folder.clone()));
            open_detail_for(app, &m, item)
        }
        Action::CloseDetail => {
            app.media.detail = None;
            // The create surface lives inside the detail.
            if let Some(m) = machine {
                m.close_share_create();
            }
            None
        }
        Action::ArmRestore(index) => {
            // Arms the lightweight confirm; the restore itself is the confirm's
            // gesture (`media.md` § Element IDs — restore is reversible, so this
            // is a light confirm, not the irreversible-action ceremony).
            app.media.detail.as_mut()?.arming = Some(index);
            None
        }
        Action::CancelRestore => {
            app.media.detail.as_mut()?.arming = None;
            None
        }
        Action::ConfirmRestore => {
            let m = machine?;
            let device_id = app.media.device_id.clone()?;
            let detail = app.media.detail.as_mut()?;
            let index = detail.arming.take()?;
            let version = detail.versions.as_ref()?.get(index).cloned()?;
            let (folder, path) = (detail.folder.clone(), detail.path.clone());
            Some(Op::RestoreVersion {
                machine: m,
                folder,
                device_id,
                path,
                version,
            })
        }
        Action::TogglePruned => {
            let m = machine?;
            let detail = app.media.detail.as_mut()?;
            detail.show_pruned = !detail.show_pruned;
            // Any armed row index is void once the row set changes shape.
            detail.arming = None;
            let include_pruned = detail.show_pruned;
            // Back to the loading state so the switchover never renders the
            // old population under the new toggle.
            detail.versions = None;
            Some(Op::LoadVersions {
                machine: m,
                folder: detail.folder.clone(),
                path: detail.path.clone(),
                include_pruned,
            })
        }
        Action::Undelete(index) => {
            let m = machine?;
            let detail = app.media.detail.as_ref()?;
            let version = detail.versions.as_ref()?.get(index)?;
            // The button is painted only on pruned rows; a stale driver click
            // on a live row is a no-op rather than a spurious wire call.
            if !version.pruned {
                return None;
            }
            Some(Op::UndeleteVersion {
                machine: m,
                folder: detail.folder.clone(),
                path: detail.path.clone(),
                version_num: version.version_num,
                include_pruned: detail.show_pruned,
            })
        }
        Action::ArmDelete => {
            // Arms the single confirm; the delete itself is the confirm's
            // gesture. The delete propagates to every device, so it is
            // confirmed — but lightly: the tombstone leaves the historical
            // version rows and backup destinations do not forward deletes
            // at all (`media.md` § Element IDs, `file-sync.md` § File Versions /
            // § Destination modes), so the heavier typed-id ceremony would be
            // miscalibrated to the risk.
            app.media.detail.as_mut()?.delete_arming = true;
            None
        }
        Action::CancelDelete => {
            // The pure no-op decline (`media.md` § Element IDs): nothing is
            // recorded, no error surfaces, the detail surface stays open.
            app.media.detail.as_mut()?.delete_arming = false;
            None
        }
        Action::ConfirmDelete => {
            let m = machine?;
            // The device-id derivation is the only client glue here (the shared
            // gesture takes an id, not a device). A missing id surfaces on
            // `error-message` rather than silently dropping the gesture — the
            // upload path's idiom, and `testing.md` point 11's contract.
            let Some(device_id) = app.media.device_id.clone() else {
                app.errors.insert(
                    crate::pages::Page::Media,
                    t::error_delete("no sync device id"),
                );
                return None;
            };
            let detail = app.media.detail.as_mut()?;
            // Only an armed confirm may delete (a stale driver click is a no-op).
            if !detail.delete_arming {
                return None;
            }
            detail.delete_arming = false;
            Some(Op::Delete {
                machine: m,
                folder: detail.folder.clone(),
                device_id,
                path: detail.path.clone(),
            })
        }
        Action::OpenShareCreate => {
            let detail = app.media.detail.as_ref()?;
            machine?.open_share_create(detail.folder.clone(), detail.path.clone());
            None
        }
        Action::SetShareExpiry(value) => {
            machine?.set_share_expiry(value);
            None
        }
        Action::CreateShareLink => Some(Op::CreateShareLink { machine: machine? }),
        Action::CloseShareCreate => {
            machine?.close_share_create();
            None
        }
        Action::CopyShareUrl => {
            let url = machine?.snapshot().share_create?.url?;
            crate::wizard::copy_to_clipboard(&url);
            None
        }
        Action::OpenShareLinks => Some(Op::OpenShareLinks { machine: machine? }),
        Action::CloseShareLinks => {
            machine?.close_share_links();
            None
        }
        Action::CopyShareLinkRow(index) => {
            let url = machine?
                .snapshot()
                .share_links
                .rows
                .get(index)?
                .url
                .clone()?;
            crate::wizard::copy_to_clipboard(&url);
            None
        }
        Action::ArmShareRevoke(index) => {
            let m = machine?;
            let token_id = m.snapshot().share_links.rows.get(index)?.token_id.clone();
            m.arm_share_revoke(token_id);
            None
        }
        Action::ConfirmShareRevoke => Some(Op::ConfirmShareRevoke { machine: machine? }),
        Action::CancelShareRevoke => {
            machine?.cancel_share_revoke();
            None
        }
        Action::ExternalOpen => {
            match crate::settings::external_media_outcome(&app.settings.prefs.external_media) {
                // `ask` (the default): arm the inline confirm; nothing runs yet.
                crate::settings::HandoffOutcome::Prompt => {
                    app.media.detail.as_mut()?.external_arming = true;
                    None
                }
                // `always`: straight to the download → materialize → spawn.
                crate::settings::HandoffOutcome::Launched => external_open_op(app),
                // `never` paints no trigger at all (the ui.yaml contract), so
                // this only fires from a stale driver click — a quiet no-op is
                // honest: the mode says nothing may launch.
                crate::settings::HandoffOutcome::Suppressed => None,
            }
        }
        Action::ConfirmExternalOpen => {
            {
                let detail = app.media.detail.as_mut()?;
                // Only an armed confirm may launch (a stale click is a no-op).
                if !detail.external_arming {
                    return None;
                }
                detail.external_arming = false;
            }
            external_open_op(app)
        }
        Action::CancelExternalOpen => {
            // The pure no-op decline (ui.yaml: nothing downloads, nothing
            // decrypts, nothing launches; the detail surface stays open).
            app.media.detail.as_mut()?.external_arming = false;
            None
        }
        Action::Download => {
            let (machine, source) = fetch_source(app)?;
            Some(Op::Download {
                machine,
                source,
                name: app.media.detail.as_ref()?.name.clone(),
            })
        }
    }
}

/// Which shared query reads the open detail item's current bytes — the one
/// resolution both reads of this surface (the download and the external
/// handoff) share, so they cannot open different files.
pub enum FetchSource {
    /// An own or shared set: the shared `download_file` walk keyed by the
    /// **latest** version row (rows are oldest→newest, so the last row is the
    /// current file).
    Own {
        /// Hex, as `FileVersionSummary` carries it.
        manifest_hash: String,
        content_key_version: Option<u64>,
        /// The set name — routes to the content-key resolver so a shared set opens
        /// under its content keys, an owner-only set under the owner `BackupKey`.
        folder: String,
        relative_path: String,
        backup_key: Vec<u8>,
    },
    /// The active followed scope: the keyless `download_followed`, which
    /// resolves the head manifest by path from the listing the machine retained
    /// at scope entry, so no version row (and no key) is consulted — `media.md`
    /// § Followed public folders + architectural rule 6.
    Followed {
        value: String,
        relative_path: String,
    },
}

impl FetchSource {
    /// Run the shared query — every fetch/decrypt/verify step is shared Rust;
    /// the error is the machine's own detail, for the caller's page wording.
    async fn fetch(self, machine: &MediaMachine) -> Result<Vec<u8>, String> {
        let result = match self {
            FetchSource::Own {
                manifest_hash,
                content_key_version,
                folder,
                relative_path,
                backup_key,
            } => {
                machine
                    .download_file(
                        manifest_hash,
                        content_key_version,
                        folder,
                        relative_path,
                        backup_key,
                    )
                    .await
            }
            FetchSource::Followed {
                value,
                relative_path,
            } => machine.download_followed(value, relative_path).await,
        };
        result.map_err(|e| e.detail().to_string())
    }
}

/// The open detail item's [`FetchSource`]. `None` while the machine / detail /
/// version rows aren't there; both read affordances paint only when they are,
/// so a painted button always produces its Op.
fn fetch_source(app: &App) -> Option<(Arc<MediaMachine>, FetchSource)> {
    let machine = Arc::clone(app.media.machine.as_ref()?);
    let detail = app.media.detail.as_ref()?;
    if let Some(value) = detail.followed_scope_value.clone() {
        return Some((
            machine,
            FetchSource::Followed {
                value,
                relative_path: detail.path.clone(),
            },
        ));
    }
    let latest = detail.versions.as_ref()?.last()?.clone();
    Some((
        machine,
        FetchSource::Own {
            manifest_hash: latest.manifest_hash,
            content_key_version: latest.content_key_version,
            folder: detail.folder.clone(),
            relative_path: detail.path.clone(),
            backup_key: app.media.backup_key.clone(),
        },
    ))
}

/// The external handoff's network half for the open detail item, keyed by its
/// **latest** version's `manifest_hash` — the current file, which is why the
/// trigger lives on `media-item-detail` and not on the `media-item` row
/// (`apps/tui.md` § External media handoff). Resolved by [`fetch_source`].
fn external_open_op(app: &App) -> Option<Op> {
    let (machine, source) = fetch_source(app)?;
    let name = app.media.detail.as_ref()?.name.clone();
    Some(match source {
        FetchSource::Followed {
            value,
            relative_path,
        } => Op::FollowedExternalOpen {
            machine,
            value,
            relative_path,
            name,
        },
        FetchSource::Own {
            manifest_hash,
            content_key_version,
            folder,
            relative_path,
            backup_key,
        } => Op::ExternalOpen {
            machine,
            manifest_hash,
            content_key_version,
            folder,
            relative_path,
            backup_key,
            name,
        },
    })
}

/// The download's save step — the only platform glue in it. A terminal has no
/// save dialog (`tui.md` § Declared platform absences), so the file lands in
/// the dir every other file this app hands back lands in
/// (`backups::download_dir`), under the item's last path component so a name
/// carrying a separator can never steer the write outside it. A second press
/// replaces the earlier copy, as the snapshot download does.
fn save_download(
    dir: &std::path::Path,
    name: &str,
    bytes: &[u8],
) -> std::io::Result<std::path::PathBuf> {
    let file_name = std::path::Path::new(name)
        .file_name()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| std::ffi::OsStr::new("download"));
    std::fs::create_dir_all(dir)?;
    let path = dir.join(file_name);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// The nav-edge refresh entering the Media tab implies — the `fauna.media.list`
/// pull that populates the cross-set aggregate (linux fires the same on
/// `connect_map`). Awaited by the agent's nav handler, so a driver's very next
/// read never races the fetch.
pub fn nav_enter_op(state: &MediaState) -> Option<Op> {
    Some(Op::Refresh {
        machine: Arc::clone(state.machine.as_ref()?),
        backup_key: state.backup_key.clone(),
    })
}

// ── Network half ────────────────────────────────────────────────────────────

/// The Media page's network half. Owns only `Arc`s + owned data, so it crosses a
/// `tokio::spawn`.
pub enum Op {
    Refresh {
        machine: Arc<MediaMachine>,
        backup_key: Vec<u8>,
    },
    Upload {
        machine: Arc<MediaMachine>,
        device_id: String,
        path: String,
        raw_bytes: Vec<u8>,
        backup_key: Vec<u8>,
    },
    LoadVersions {
        machine: Arc<MediaMachine>,
        folder: String,
        path: String,
        /// The recovery browse's switch state — `true` also lists soft-pruned
        /// rows (`file-versions.md` § Retention (3)).
        include_pruned: bool,
    },
    /// Recover the soft-pruned version (`fauna.files.versions.undelete`), then
    /// re-read the rows under the browse's current toggle so the recovered row
    /// repaints live without a close/reopen.
    UndeleteVersion {
        machine: Arc<MediaMachine>,
        folder: String,
        path: String,
        version_num: i64,
        include_pruned: bool,
    },
    RestoreVersion {
        machine: Arc<MediaMachine>,
        folder: String,
        device_id: String,
        path: String,
        version: FileVersionSummary,
    },
    /// Delete the opened file: the shared gesture records the tombstone
    /// (`fauna.sync.changes.record`, `change_type = "delete"`) and refreshes, so
    /// the tombstone-excluding `fauna.media.list` stops listing the row.
    Delete {
        machine: Arc<MediaMachine>,
        folder: String,
        device_id: String,
        path: String,
    },
    /// Fetch + decrypt + rasterize the thumbnails for `hashes`
    /// ([`kick_thumbnail_fetches`]). The rasterizing happens here, off the render
    /// thread, so the fold is a pure cache write.
    FetchThumbnails {
        machine: Arc<MediaMachine>,
        backup_key: Vec<u8>,
        hashes: Vec<String>,
    },
    /// The external handoff: download + decrypt the item's bytes via the shared
    /// walk (`MediaMachine::download_file`), materialize them per the ratified
    /// temp-file shape (`crate::media_handoff`), and spawn the OS default
    /// handler. The client glue is exactly the materialize + spawn — every
    /// fetch/decrypt/verify step stays shared Rust.
    ExternalOpen {
        machine: Arc<MediaMachine>,
        /// Hex, as `FileVersionSummary` carries it (the latest version).
        manifest_hash: String,
        content_key_version: Option<u64>,
        /// The set name — routes to the content-key resolver so a shared set opens
        /// under its content keys, an owner-only set under the owner `BackupKey`.
        folder: String,
        relative_path: String,
        backup_key: Vec<u8>,
        /// The item's display name — the materialized file takes its extension
        /// (what the OS resolves the handler by).
        name: String,
    },
    /// Enter a followed browse scope — the machine's on-demand listing fetch
    /// (`media.md` § Followed public folders). The machine notifies on settle
    /// (the listing, or the loud unavailable / transport error), so there is
    /// nothing to fold.
    SelectFollowedScope {
        machine: Arc<MediaMachine>,
        /// The machine-minted scope value the select handed back.
        value: String,
    },
    /// The external handoff for an item of the **active followed scope** — the
    /// keyless follower read: `MediaMachine::download_followed` resolves the
    /// head manifest by path from the listing it retained, so no version row,
    /// no key, and no name-keyed custody is consulted (`media.md`
    /// architectural rule 6).
    FollowedExternalOpen {
        machine: Arc<MediaMachine>,
        /// The scope's minted value — a race with a scope switch fails loudly.
        value: String,
        relative_path: String,
        /// The item's display name — the materialized file takes its extension.
        name: String,
    },
    /// `media-item-detail-download-button`: the shared read for the open item
    /// ([`FetchSource`]), then the plaintext saved into the downloads dir under
    /// the item's name ([`save_download`]).
    Download {
        machine: Arc<MediaMachine>,
        source: FetchSource,
        /// The item's display name — the saved file's name.
        name: String,
    },
    /// `share-link-create-button` — the machine mints, seals, registers and
    /// reveals (or sets the page error); it notifies on settle.
    CreateShareLink { machine: Arc<MediaMachine> },
    /// `share-link-list-button` — open + load the list.
    OpenShareLinks { machine: Arc<MediaMachine> },
    /// `share-link-revoke-confirm-button` — revoke the armed row, re-list.
    ConfirmShareRevoke { machine: Arc<MediaMachine> },
}

#[derive(Debug)]
pub enum Outcome {
    /// The machine applied the effect and notified its observer; the repaint
    /// rides that tick, so there is nothing left to fold.
    Done,
    /// The version rows for the open detail surface loaded (oldest→newest).
    Versions(Vec<FileVersionSummary>),
    /// A per-item query failed. Unlike the page gestures, `file_versions` never
    /// touches the machine's page-level `error-message`, so the fold surfaces it.
    Failed(String),
    /// The delete gesture settled. `succeeded` is read off the machine's own
    /// snapshot rather than a return value, because `MediaMachine::delete`
    /// swallows its error into the snapshot instead of returning it — the
    /// success arm's `refresh()` clears `error`, the failure arm sets
    /// `media.error_delete`. It decides whether the detail surface closes
    /// (linux's `open_delete_confirm` makes the same read).
    Deleted { succeeded: bool },
    /// Thumbnail loads settled — `(hash, thumbnail)`, with `None` for any that
    /// failed to fetch, decrypt or decode. **Never** an `Outcome::Failed`: a
    /// thumbnail that will not load is a placeholder, not a page error
    /// (`ui/media.md` § Thumbnails; the machine's `fetch_thumbnail` contract says
    /// the same).
    Thumbnails(Vec<(String, Option<crate::thumbnail::Thumbnail>)>),
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::Refresh {
                machine,
                backup_key,
            } => {
                machine.refresh(Some(backup_key)).await;
                Outcome::Done
            }
            Op::CreateShareLink { machine } => {
                machine.create_share_link().await;
                Outcome::Done
            }
            Op::OpenShareLinks { machine } => {
                machine.open_share_links().await;
                Outcome::Done
            }
            Op::ConfirmShareRevoke { machine } => {
                machine.confirm_share_revoke().await;
                Outcome::Done
            }
            Op::Upload {
                machine,
                device_id,
                path,
                raw_bytes,
                backup_key,
            } => {
                // The shared gesture resolves the target set (or sets the no-set
                // page error), seals, POSTs, records, and refreshes — the "which
                // set" policy is identical on every app because it lives there.
                machine
                    .upload_selected(device_id, path, raw_bytes, backup_key)
                    .await;
                Outcome::Done
            }
            Op::LoadVersions {
                machine,
                folder,
                path,
                include_pruned,
            } => match machine.file_versions(folder, path, include_pruned).await {
                Ok(versions) => Outcome::Versions(versions),
                Err(e) => Outcome::Failed(e.detail().to_string()),
            },
            Op::UndeleteVersion {
                machine,
                folder,
                path,
                version_num,
                include_pruned,
            } => {
                // A user gesture → must say so on `error-message` (point 11);
                // wrapped here because `undelete_version` is a per-item query
                // that never touches the machine's own banner.
                if let Err(e) = machine.undelete_version(path.clone(), version_num).await {
                    return Outcome::Failed(t::error_undelete(e.detail()));
                }
                match machine.file_versions(folder, path, include_pruned).await {
                    Ok(versions) => Outcome::Versions(versions),
                    Err(e) => Outcome::Failed(e.detail().to_string()),
                }
            }
            Op::RestoreVersion {
                machine,
                folder,
                device_id,
                path,
                version,
            } => {
                machine
                    .restore_version(folder.clone(), device_id, path.clone(), version)
                    .await;
                // The restore appended a version; re-read the rows so the open
                // detail surface shows it without a close/reopen.
                match machine.file_versions(folder, path, false).await {
                    Ok(versions) => Outcome::Versions(versions),
                    Err(e) => Outcome::Failed(e.detail().to_string()),
                }
            }
            Op::Delete {
                machine,
                folder,
                device_id,
                path,
            } => {
                // The shared gesture records the tombstone and refreshes; the
                // "which device may record" and wire shape both stay there.
                machine.delete(folder, device_id, path).await;
                Outcome::Deleted {
                    succeeded: machine.snapshot().error.is_none(),
                }
            }
            Op::FetchThumbnails {
                machine,
                backup_key,
                hashes,
            } => {
                // Concurrently, not in sequence: these are N independent GETs of
                // a few KiB each, and awaiting them one by one would make the
                // last thumbnail of a set wait for every thumbnail before it.
                let arts = futures_util::future::join_all(hashes.into_iter().map(|hash| {
                    let machine = Arc::clone(&machine);
                    let backup_key = backup_key.clone();
                    async move {
                        // Every step up to here is shared Rust (fetch by hash,
                        // content-address verify, owner-key decrypt); the client
                        // decodes and rasterizes the bytes it hands back, and
                        // nothing more (`ui/media.md` § Where logic lives).
                        let art = match machine.fetch_thumbnail(hash.clone(), backup_key).await {
                            Ok(bytes) => crate::thumbnail::rasterize(
                                &bytes,
                                crate::thumbnail::THUMBNAIL_COLS,
                            ),
                            // Deliberately dropped, not surfaced: an offline
                            // source folder makes its items unfetchable
                            // (`media.md` § Errors & edge cases), which is a
                            // placeholder, not a banner.
                            Err(_) => None,
                        };
                        (hash, art)
                    }
                }))
                .await;
                Outcome::Thumbnails(arts)
            }
            Op::ExternalOpen {
                machine,
                manifest_hash,
                content_key_version,
                folder,
                relative_path,
                backup_key,
                name,
            } => {
                // The shared walk: manifest + chunks by content address →
                // owner-key decrypt → whole-file verify (`fauna_core::
                // file_download` via the machine query). A failure surfaces on
                // the page banner — the user asked for this action.
                let bytes = match machine
                    .download_file(
                        manifest_hash,
                        content_key_version,
                        folder,
                        relative_path,
                        backup_key,
                    )
                    .await
                {
                    Ok(bytes) => bytes,
                    Err(e) => return Outcome::Failed(t::error_external_open(e.detail())),
                };
                // Client glue: the hardened temp file + the OS spawn (the
                // ratified materialization; `apps/tui.md` § External media
                // handoff). The spawn itself is fire-and-forget by design.
                match crate::media_handoff::materialize(&name, &bytes) {
                    Ok(path) => {
                        crate::os_open::open(&path.to_string_lossy());
                        Outcome::Done
                    }
                    Err(e) => Outcome::Failed(t::error_external_open(&e.to_string())),
                }
            }
            Op::SelectFollowedScope { machine, value } => {
                // The machine owns the whole outcome: the listing on success,
                // the folded unavailable wording or the transport error on the
                // page banner otherwise — each notifies its own repaint.
                machine.select_followed_scope(value).await;
                Outcome::Done
            }
            Op::FollowedExternalOpen {
                machine,
                value,
                relative_path,
                name,
            } => {
                // The keyless follower read; then the same materialize + spawn
                // client glue as the ordinary handoff above.
                let bytes = match machine.download_followed(value, relative_path).await {
                    Ok(bytes) => bytes,
                    Err(e) => return Outcome::Failed(t::error_external_open(e.detail())),
                };
                match crate::media_handoff::materialize(&name, &bytes) {
                    Ok(path) => {
                        crate::os_open::open(&path.to_string_lossy());
                        Outcome::Done
                    }
                    Err(e) => Outcome::Failed(t::error_external_open(&e.to_string())),
                }
            }
            Op::Download {
                machine,
                source,
                name,
            } => {
                // A failure surfaces on the page banner — the user asked for
                // this action.
                let bytes = match source.fetch(&machine).await {
                    Ok(bytes) => bytes,
                    Err(e) => return Outcome::Failed(t::error_download(&e)),
                };
                let Some(dir) = crate::backups::download_dir() else {
                    return Outcome::Failed(t::error_download(
                        "no downloads directory to save into",
                    ));
                };
                let saved =
                    tokio::task::spawn_blocking(move || save_download(&dir, &name, &bytes)).await;
                match saved {
                    Ok(Ok(_)) => Outcome::Done,
                    Ok(Err(e)) => Outcome::Failed(t::error_download(&e.to_string())),
                    Err(e) => Outcome::Failed(t::error_download(&e.to_string())),
                }
            }
        }
    }
}

/// The thumbnail loads the current snapshot implies but the cache does not yet
/// hold — marking each `Loading` so a later tick does not re-issue it.
///
/// **Called from the observer tick**, not the nav edge: the machine notifies on
/// *every* mutation (the nav-enter refresh, a filter change, and — the case a
/// nav-edge kick would miss — the refresh that follows an **upload**, whose new
/// item is exactly what `test_media_upload_into_selected_set` asserts paints).
/// One idempotent trigger covers them all, because the cache, not the call site,
/// is what makes a repeat fetch impossible.
///
/// Returns `None` when there is nothing new to load, so the common tick spawns
/// nothing at all.
pub fn kick_thumbnail_fetches(app: &mut App) -> Option<Op> {
    let machine = Arc::clone(app.media.machine.as_ref()?);
    let snapshot = machine.snapshot();
    let mut hashes: Vec<String> = snapshot
        .items
        .iter()
        .filter_map(|item| item.thumbnail_hash.clone())
        .collect();
    hashes.retain(|hash| app.media.thumbnails.begin(hash));
    if hashes.is_empty() {
        return None;
    }
    Some(Op::FetchThumbnails {
        machine,
        backup_key: app.media.backup_key.clone(),
        hashes,
    })
}

/// Mirror the machine's page-level error onto the page's `error-message`.
///
/// **The machine owns the page error**, not this page: `media.md` § Errors &
/// edge cases puts refresh failure, upload failure and the "select a folder
/// first" policy on the *machine*, which surfaces them on its snapshot. tui's
/// `error-message` is registered globally off `App::errors`
/// (`ui::register_frame`), so the two must be bridged — without this, a gesture
/// that fails *inside* shared Rust (the no-set upload being the one every app
/// tests) silently does nothing: no error, no effect, which reads exactly like a
/// dropped command (`../testing.md` point 10).
///
/// Called wherever the machine may have just changed its error: the fold of any
/// machine-driven `Outcome`, and the observer tick. A *glue* error (a file the
/// client itself could not read) is written to `App::errors` directly and is
/// overwritten by the next tick — the same precedence linux's `render_page`
/// gives `snap.error` over its own `show_upload_glue_error`.
pub fn sync_page_error(app: &mut App) {
    let error = app
        .media
        .snapshot()
        .and_then(|s| s.error)
        .map(|text| text.resolve(fauna_i18n::strings::lookup));
    match error {
        Some(error) => {
            app.errors.insert(crate::pages::Page::Media, error);
        }
        None => {
            app.errors.remove(&crate::pages::Page::Media);
        }
    }
}

pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        // The machine's own observer tick drives the repaint; the page error is
        // whatever the machine now says it is (a success clears it; a failed
        // upload/refresh — including the no-set policy — surfaces it).
        Outcome::Done => sync_page_error(app),
        Outcome::Versions(versions) => {
            if let Some(detail) = app.media.detail.as_mut() {
                detail.versions = Some(versions);
            }
            sync_page_error(app);
        }
        Outcome::Deleted { succeeded } => {
            // Deleted: close the surface — its subject is gone, so its version
            // history would be a dangling view. (The restore path instead
            // reloads: its file still exists.) On FAILURE keep it open — the
            // file is still there, and closing would yank away the context the
            // user acted in. Either way the banner is whatever the machine now
            // says (a success clears it, a failure carries `media.error_delete`).
            if succeeded {
                app.media.detail = None;
            }
            sync_page_error(app);
        }
        // A per-item query failure is NOT the machine's page error
        // (`file_versions` deliberately never touches the banner), so it is
        // surfaced here rather than read back off the snapshot.
        Outcome::Failed(detail) => {
            app.errors.insert(crate::pages::Page::Media, detail);
        }
        // A pure cache write — the art arrived rasterized (the Op did that work
        // off the render thread). No `sync_page_error`: a thumbnail never
        // touches the banner in either direction, so folding one must not clear
        // an unrelated upload error either.
        Outcome::Thumbnails(arts) => {
            for (hash, art) in arts {
                app.media.thumbnails.set(hash, art);
            }
        }
    }
}

// ── Paint ───────────────────────────────────────────────────────────────────

/// The page's element list — the one list paint, the automation registry and the
/// focus ring all consume.
///
/// **The element list is the registry; the viewport clips paint only**
/// (`crate::element`): every item the snapshot implies is listed, however few fit
/// the terminal, so `count("media-item")` answers off the aggregate rather than
/// off terminal height.
pub fn elements(app: &App) -> Vec<Element> {
    let Some(snapshot) = app.media.snapshot() else {
        // Pre-auth / no machine: the page still paints its heading, so the tab
        // is never a blank pane.
        return vec![Element::label(ids::PAGE_HEADING, t::TITLE)];
    };

    // The detail surface is a sub-page: while open it owns the pane, so the
    // explorer's own chrome is not painted behind it (the `create_feed` /
    // `conversation_detail` sub-page shape).
    if let Some(detail) = &app.media.detail {
        return detail_elements(app, &snapshot, detail);
    }
    // The share-link list is a page-level sub-surface: while open it owns the
    // pane, the same way the detail does.
    if snapshot.share_links.open {
        return share_list_elements(&snapshot);
    }

    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        // The toggle's label reflects the ACTIVE mode, which is what makes the
        // flip observable to a test (and to a human).
        Element::gesture_button(
            ids::MEDIA_VIEW_TOGGLE,
            if snapshot.view_grid {
                t::VIEW_GRID
            } else {
                t::VIEW_LIST
            },
            true,
            Gesture::Media(Action::ToggleView),
        ),
        Element::select(
            ids::MEDIA_SORT_SELECT,
            snapshot.sort.clone(),
            SelectTarget::MediaSort,
            SORT_VALUES.iter().map(|s| s.to_string()).collect(),
        )
        .display_value(sort_label(&snapshot.sort)),
        Element::select(
            ids::MEDIA_SORT_DIRECTION,
            sort_direction_value(snapshot.descending).to_string(),
            SelectTarget::MediaSortDirection,
            SORT_DIRECTION_VALUES
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
        .display_value(sort_direction_label(sort_direction_value(
            snapshot.descending,
        ))),
    ];

    // Filter options: the all-media sentinel + every browsable set the client
    // knows (empty ones included — `media.md` § Layout & flow)
    // (the machine hands the set list independent of the active filter), then
    // the followed browse scopes (`media.md` § Followed public folders). A
    // followed option's string is the machine-minted opaque VALUE — never
    // parsed here, and never shown: the select paints only the current
    // selection, whose label comes from `followed_scope` below.
    let mut filter_options = vec![FILTER_ALL_VALUE.to_string()];
    filter_options.extend(snapshot.folders.iter().cloned());
    filter_options.extend(snapshot.followed.iter().map(|f| f.value.clone()));
    let active_filter = snapshot
        .filter
        .clone()
        .unwrap_or_else(|| FILTER_ALL_VALUE.to_string());
    let filter_label = if let Some(scope) = &snapshot.followed_scope {
        scope.label.clone()
    } else if snapshot.filter.is_none() {
        t::FILTER_ALL.to_string()
    } else {
        active_filter.clone()
    };
    els.push(
        Element::select(
            ids::MEDIA_FOLDER_FILTER,
            active_filter,
            SelectTarget::MediaFolderFilter,
            filter_options,
        )
        .display_value(filter_label),
    );

    // Upload affordance: a staged path + the submit that runs the shared
    // gesture. Absent entirely while a followed browse scope is active — the
    // scope is structurally read-only (`media.md` § Followed public folders),
    // and the machine refuses too, so a stale driver click still answers.
    if snapshot.followed_scope.is_none() {
        els.push(
            Element::input(
                ids::FILE_UPLOAD,
                app.media.file_input.clone(),
                Field::Media(MediaField::FileUpload),
            )
            // Typed-path language, NOT the shared `CHOOSE_FILE` picker prompt: tui
            // cannot open an OS picker (declared platform absence 4), so "Choose
            // file…" told a live user to do something no key in this app performs
            // — they reported no way to upload at all. `ui/media.md`'s matrix
            // already records tui's `file-upload` as a typed PATH; this is the
            // label catching up to the ratified reality.
            .labelled(t::TYPE_FILE_PATH),
        );
        els.push(Element::gesture_button(
            ids::UPLOAD_BUTTON,
            t::UPLOAD,
            true,
            Gesture::Media(Action::Upload),
        ));
    }

    // "Shared links" — the page-level entry to the caller's share links
    // (`share-links.md` § Flows → List), in the ui.yaml page order.
    els.push(Element::gesture_button(
        ids::SHARE_LINK_LIST_BUTTON,
        sl::LIST_BUTTON,
        true,
        Gesture::Media(Action::OpenShareLinks),
    ));

    // The empty state is `media-empty-state` as of 2026-08-05 (user-approved,
    // ui.yaml `media` page) — it used to paint as untagged chrome because ui.yaml
    // gave it no ID. `loaded` is the load-bearing half of the condition: a page
    // whose first read is still in flight also has no items, and painting "No
    // media yet" over media that is about to arrive is a lie to the user AND the
    // ambiguity that let a zero-items defect read as a legitimate empty set
    // (`media.md` § Default view: cross-set all-media).
    if snapshot.loaded && snapshot.items.is_empty() {
        els.push(Element::label(ids::MEDIA_EMPTY_STATE, t::NO_MEDIA_YET));
    }
    for (index, item) in snapshot.items.iter().enumerate() {
        els.extend(media_item(item, index, &app.media.thumbnails));
    }
    els
}

/// One indexed `media-item` row and its children.
///
/// The children are a real positional **scope** (`Element::within`), which is
/// what lets a test read `media-item-name` scoped to `media-item[2]` instead of
/// counting globally and slicing.
///
/// `thumbnails` is the page's rasterized-art cache ([`MediaState::thumbnails`]);
/// an item whose art is absent for any reason paints the placeholder.
fn media_item(item: &MediaItemSummary, index: usize, thumbnails: &ImageCache) -> Vec<Element> {
    vec![
        // The row root carries the open gesture (`media.md` § User actions —
        // media-item tap/open opens the detail surface).
        Element::gesture_button(
            ids::MEDIA_ITEM,
            item.name.clone(),
            true,
            Gesture::Media(Action::OpenDetail(index)),
        ),
        Element::label(ids::MEDIA_ITEM_NAME, item.name.clone()).within(ids::MEDIA_ITEM, index),
        Element::label(
            ids::MEDIA_ITEM_SIZE,
            crate::format::byte_size(item.size_bytes),
        )
        .within(ids::MEDIA_ITEM, index),
        // `updated_at` is unix **seconds**; the formatter takes micros.
        Element::label(
            ids::MEDIA_ITEM_DATE,
            crate::format::format_epoch_us(item.updated_at.saturating_mul(1_000_000)),
        )
        .within(ids::MEDIA_ITEM, index),
        // The item's picture, painted as half-block art (`apps/tui.md`
        // § Rendering — the images half). Registered **unconditionally**, like
        // linux's placeholder icon (`views/media/item.rs`): an item with no
        // `thumbnail_hash`, a fetch/decrypt/decode that failed, or art still in
        // flight all paint the placeholder glyph, so the row never blanks and
        // the element is always addressable.
        match item
            .thumbnail_hash
            .as_ref()
            .and_then(|hash| thumbnails.get(hash))
        {
            Some(ImageState::Ready(art)) => Element::thumbnail(ids::MEDIA_THUMBNAIL, art.clone()),
            Some(ImageState::Loading) | Some(ImageState::Failed) | None => {
                Element::label(ids::MEDIA_THUMBNAIL, crate::thumbnail::PLACEHOLDER)
            }
        }
        .within(ids::MEDIA_ITEM, index),
        // The source folder's device liveness — distinct from the file's own
        // sync-state badge below (`ui.yaml` media-item: "media-source-status is
        // the SOURCE FOLDER liveness — distinct from sync-state-badge, which
        // is THIS FILE's presence").
        Element::label(
            ids::MEDIA_SOURCE_STATUS,
            if item.source_online {
                t::SOURCE_ONLINE
            } else {
                t::SOURCE_OFFLINE
            },
        )
        .within(ids::MEDIA_ITEM, index),
        // This file's own presence — `file-sync.md` § Per-file sync-status
        // display. tui is a control-plane client (no local sync engine behind
        // the media page; `fauna.sync.files` carries no per-file status field),
        // so it renders only the `Synced` state — the class split the goal doc
        // sanctions, the same leg linux renders (`views/media/item.rs`). The
        // label comes from the shared `sync_display_state_label`, never a
        // hand-written string.
        Element::label(
            ids::SYNC_STATE_BADGE,
            fauna_core::format::sync_display_state_label(
                fauna_core::format::SyncDisplayState::Synced,
            )
            .resolve(fauna_i18n::strings::lookup),
        )
        .within(ids::MEDIA_ITEM, index),
    ]
}

/// The `media-item-detail` surface: the item's name, the tui-only external-open
/// trigger for an audio/video item, its `file-version-history` rows, and the
/// confirms when armed.
fn detail_elements(app: &App, snapshot: &MediaPageSnapshot, detail: &DetailState) -> Vec<Element> {
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::label(ids::MEDIA_ITEM_DETAIL, detail.name.clone()),
        Element::label(ids::MEDIA_ITEM_DETAIL_NAME, detail.name.clone()),
        Element::gesture_button(
            ids::MEDIA_ITEM_DETAIL_CLOSE_BUTTON,
            t::DETAIL_CLOSE,
            true,
            Gesture::Media(Action::CloseDetail),
        ),
    ];

    // A followed item's detail is READ-ONLY (`media.md` § Followed public
    // folders): no delete, no restore, no version history — the follower holds
    // no seat on the folder and the public plane is head-only. Absent
    // entirely, never painted-but-inert (the `testing.md` point-10 shape).
    let followed = detail.followed_scope_value.is_some();

    // Delete the opened file (`media.md` § Element IDs, user-approved
    // 2026-07-16). Painted on every OWN-item detail — the ui.yaml component
    // order puts it right after the close button — and always armed-gated, so
    // pressing it can never delete anything by itself.
    if !followed {
        els.push(Element::gesture_button(
            ids::MEDIA_DELETE_BUTTON,
            t::file_detail::DELETE_FILE,
            true,
            Gesture::Media(Action::ArmDelete),
        ));
    }

    // "Share a link" — present ONLY on an eligible file (the shared
    // `share_link_eligible` verdict the machine stamped on the item), absent
    // elsewhere, never painted-but-inert (`share-links.md` § Which files can be
    // linked). Its create surface, once open, paints right below.
    let eligible = !followed
        && snapshot
            .items
            .iter()
            .any(|i| i.folder == detail.folder && i.path == detail.path && i.share_link_eligible);
    if eligible {
        els.push(Element::gesture_button(
            ids::SHARE_LINK_BUTTON,
            sl::BUTTON,
            true,
            Gesture::Media(Action::OpenShareCreate),
        ));
        if let Some(create) = &snapshot.share_create {
            els.extend(share_create_elements(snapshot, create));
        }
    }

    // The external handoff trigger (`media.md` § Element IDs, tui-only;
    // behavior owner `apps/tui.md` § External media handoff). Painted ONLY
    // for an audio/video item (the shared single-source predicate), ONLY under
    // ask|always — under `never` it is absent entirely, metadata only, never
    // painted-but-inert (the `testing.md` point-10 shape) — and only once the
    // version rows carry a manifest to download, so a painted trigger is
    // always actionable.
    let handoff_mode = crate::settings::external_media_outcome(&app.settings.prefs.external_media);
    // A followed item's manifest is resolved machine-side by path (the scope
    // retained the folded listing), so its trigger is always actionable; an
    // own item's waits for the version rows to carry one.
    let has_manifest = followed || detail.versions.as_ref().is_some_and(|v| !v.is_empty());

    // The cross-app download (`media.md` § Element IDs) — for any item, under
    // the same manifest gate, so it is painted once actionable and never inert.
    if has_manifest {
        els.push(Element::gesture_button(
            ids::MEDIA_ITEM_DETAIL_DOWNLOAD_BUTTON,
            t::DOWNLOAD,
            true,
            Gesture::Media(Action::Download),
        ));
    }

    if fauna_core::share::is_audio_video_filename(&detail.name)
        && handoff_mode != crate::settings::HandoffOutcome::Suppressed
        && has_manifest
    {
        els.push(Element::gesture_button(
            ids::MEDIA_EXTERNAL_OPEN_BUTTON,
            t::EXTERNAL_OPEN,
            true,
            Gesture::Media(Action::ExternalOpen),
        ));
    }

    // The whole versions surface is an own-item affordance (`followed` above);
    // the armed-confirm blocks below stay outside this gate because the
    // external-open confirm must still paint for a followed item.
    let versions: &[FileVersionSummary] = if followed {
        &[]
    } else {
        els.push(Element::label(ids::FILE_VERSION_LIST, t::VERSIONS_TITLE));
        // The recovery browse switch (`file-versions.md` § Retention (3)) — ON
        // re-lists with `include_pruned`, so soft-pruned rows appear below with
        // their badge + undelete button.
        els.push(
            Element::checkbox_gesture(
                ids::FILE_VERSION_SHOW_PRUNED_TOGGLE,
                t::VERSIONS_SHOW_PRUNED,
                detail.show_pruned,
                Gesture::Media(Action::TogglePruned),
            )
            .attr("state", if detail.show_pruned { "on" } else { "off" }),
        );
        // While the async load is out, linux and web say so under the title; a
        // silent empty list here read as "no versions" (copy-audit, 2026-08-04 —
        // state legibility, `ui/README.md` § Copy comprehensibility rule 6).
        if detail.versions.is_none() {
            els.push(Element::chrome(t::VERSIONS_LOADING));
        }
        detail.versions.as_deref().unwrap_or(&[])
    };
    for (index, version) in versions.iter().enumerate() {
        // The row ROOT carries no scope of its own — it *is* the scope its
        // children hang under (the `media-item` shape). Nesting it inside
        // itself would make `count("file-version-item")` and every scoped child
        // read resolve to nothing.
        els.push(Element::label(
            ids::FILE_VERSION_ITEM,
            format!("v{}", version.version_num),
        ));
        els.push(
            // Version rows render the same size formatting as items — the
            // version-history suite compares the two directly.
            Element::label(
                ids::FILE_VERSION_SIZE,
                crate::format::byte_size(version.size_bytes),
            )
            .within(ids::FILE_VERSION_ITEM, index),
        );
        els.push(
            // `created_at` is epoch **millis**; the formatter takes micros.
            Element::label(
                ids::FILE_VERSION_TIMESTAMP,
                crate::format::format_epoch_us(version.created_at.saturating_mul(1_000)),
            )
            .within(ids::FILE_VERSION_ITEM, index),
        );
        els.push(
            Element::label(
                ids::FILE_VERSION_AUTHOR,
                t::version_author(&version.author_display),
            )
            .within(ids::FILE_VERSION_ITEM, index),
        );
        // A soft-pruned row (only an include_pruned listing carries one)
        // says so and offers its recovery verb — badge + undelete, present
        // ONLY on pruned rows (the snapshot-undelete-button shape).
        if version.pruned {
            els.push(
                Element::label(ids::FILE_VERSION_PRUNED_BADGE, t::VERSION_PRUNED_BADGE)
                    .within(ids::FILE_VERSION_ITEM, index),
            );
            els.push(
                Element::gesture_button(
                    ids::FILE_VERSION_UNDELETE_BUTTON,
                    t::VERSION_UNDELETE,
                    true,
                    Gesture::Media(Action::Undelete(index)),
                )
                .within(ids::FILE_VERSION_ITEM, index),
            );
        }
        els.push(
            Element::gesture_button(
                ids::FILE_VERSION_RESTORE_BUTTON,
                t::VERSION_RESTORE,
                true,
                Gesture::Media(Action::ArmRestore(index)),
            )
            .within(ids::FILE_VERSION_ITEM, index),
        );
    }

    // The lightweight confirm — painted only while armed.
    if detail.arming.is_some() {
        els.push(Element::label(
            ids::FILE_VERSION_RESTORE_CONFIRM_MODAL,
            t::RESTORE_CONFIRM_TITLE,
        ));
        els.push(Element::gesture_button(
            ids::FILE_VERSION_RESTORE_CONFIRM_BUTTON,
            t::RESTORE_CONFIRM,
            true,
            Gesture::Media(Action::ConfirmRestore),
        ));
        els.push(Element::gesture_button(
            ids::FILE_VERSION_RESTORE_CANCEL_BUTTON,
            t::RESTORE_CANCEL,
            true,
            Gesture::Media(Action::CancelRestore),
        ));
    }

    // The delete confirm — painted only while armed. A SINGLE confirm (no typed
    // id): the body names the file and says it cannot be undone, which is
    // honest — a deleted file has no reachable restore path (`media.md`
    // § Element IDs, the known follow-on). Mirrors its sibling confirms above.
    if detail.delete_arming {
        els.push(Element::label(
            ids::MEDIA_DELETE_CONFIRM_MODAL,
            t::file_detail::DELETE_CONFIRM_TITLE,
        ));
        els.push(Element::chrome(t::file_detail::delete_confirm(
            &detail.name,
        )));
        els.push(Element::gesture_button(
            ids::MEDIA_DELETE_CONFIRM_BUTTON,
            t::file_detail::DELETE_CONFIRM_BUTTON,
            true,
            Gesture::Media(Action::ConfirmDelete),
        ));
        els.push(Element::gesture_button(
            ids::MEDIA_DELETE_CANCEL_BUTTON,
            fauna_i18n::strings::common::CANCEL,
            true,
            Gesture::Media(Action::CancelDelete),
        ));
    }

    // The external-open inline confirm — painted only while armed (`ask`, the
    // default). Names the item and states what happens (the ui.yaml contract:
    // decrypted to a local temp file, handed to an external program); mirrors
    // its sibling restore confirm above.
    if detail.external_arming {
        els.push(Element::label(
            ids::MEDIA_EXTERNAL_OPEN_CONFIRM_MODAL,
            t::external_open_confirm_title(&detail.name),
        ));
        els.push(Element::chrome(t::EXTERNAL_OPEN_CONFIRM_BODY));
        els.push(Element::gesture_button(
            ids::MEDIA_EXTERNAL_OPEN_CONFIRM_BUTTON,
            t::EXTERNAL_OPEN_CONFIRM,
            true,
            Gesture::Media(Action::ConfirmExternalOpen),
        ));
        els.push(Element::gesture_button(
            ids::MEDIA_EXTERNAL_OPEN_CANCEL_BUTTON,
            t::EXTERNAL_OPEN_CANCEL,
            true,
            Gesture::Media(Action::CancelExternalOpen),
        ));
    }
    els
}

/// The share-link create surface (`share-link-create-modal`): the file's name,
/// the expiry, Create / Cancel — and, ONLY after registration succeeded, the
/// URL and its Copy (`share-links.md` § Flows → Create, step 4).
fn share_create_elements(
    snapshot: &MediaPageSnapshot,
    create: &fauna_media_machine::ShareCreateSnapshot,
) -> Vec<Element> {
    let mut els = vec![
        Element::label(ids::SHARE_LINK_CREATE_MODAL, sl::create_title(&create.name)),
        Element::chrome(sl::CREATE_BODY),
    ];
    // A sealed file's link carries its key: the author is told the two honest
    // limits before and after the create (`share-links.md` § The private-file
    // extension), off the shared snapshot's bit — absent on a public file.
    if create.key_in_fragment {
        els.push(Element::label(ids::SHARE_LINK_KEY_NOTICE, sl::KEY_NOTICE));
    }
    match &create.url {
        None => {
            els.push(
                Element::select(
                    ids::SHARE_LINK_EXPIRY_SELECT,
                    create.expiry.clone(),
                    SelectTarget::ShareLinkExpiry,
                    snapshot.share_expiry_options.clone(),
                )
                .labelled(sl::EXPIRY_LABEL)
                .display_value(expiry_label(&create.expiry)),
            );
            els.push(Element::gesture_button(
                ids::SHARE_LINK_CREATE_BUTTON,
                if create.busy {
                    sl::CREATING
                } else {
                    sl::CREATE
                },
                !create.busy,
                Gesture::Media(Action::CreateShareLink),
            ));
            els.push(Element::gesture_button(
                ids::SHARE_LINK_CANCEL_BUTTON,
                sl::CANCEL,
                true,
                Gesture::Media(Action::CloseShareCreate),
            ));
        }
        Some(url) => {
            els.push(Element::label(ids::SHARE_LINK_URL, url.clone()));
            els.push(Element::gesture_button(
                ids::SHARE_LINK_COPY_BUTTON,
                sl::COPY,
                true,
                Gesture::Media(Action::CopyShareUrl),
            ));
            els.push(Element::gesture_button(
                ids::SHARE_LINK_CANCEL_BUTTON,
                sl::CLOSE,
                true,
                Gesture::Media(Action::CloseShareCreate),
            ));
        }
    }
    els
}

/// The localized label of an expiry value — paint-only, off the shared
/// `fauna_core::format::share_link_expiry_label` map (raw for an unknown value).
fn expiry_label(value: &str) -> String {
    fauna_core::format::share_link_expiry_label(value).map_or_else(
        || value.to_string(),
        |t| t.resolve(fauna_i18n::strings::lookup),
    )
}

/// The localized label of a list row's state — paint-only, off the shared
/// `fauna_core::format::share_link_state_label` map; the element's `state`
/// attribute carries the stable value an e2e test asserts on.
fn link_state_label(state: &str) -> String {
    fauna_core::format::share_link_state_label(state).map_or_else(
        || state.to_string(),
        |t| t.resolve(fauna_i18n::strings::lookup),
    )
}

/// The share-link list surface (`share-link-list`): three states off one
/// `loaded` bit — rows, `share-link-empty-state`, or neither (loading).
fn share_list_elements(snapshot: &MediaPageSnapshot) -> Vec<Element> {
    let list = &snapshot.share_links;
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::label(ids::SHARE_LINK_LIST, sl::LIST_TITLE),
        Element::gesture_button(
            ids::SHARE_LINK_LIST_CLOSE_BUTTON,
            sl::CLOSE,
            true,
            Gesture::Media(Action::CloseShareLinks),
        ),
    ];
    if !list.loaded {
        els.push(Element::chrome(sl::LIST_LOADING));
    } else if list.rows.is_empty() {
        els.push(Element::label(ids::SHARE_LINK_EMPTY_STATE, sl::EMPTY));
    }
    for (index, row) in list.rows.iter().enumerate() {
        els.push(Element::label(ids::SHARE_LINK_ITEM, row.name.clone()));
        els.push(
            Element::label(ids::SHARE_LINK_ITEM_NAME, row.name.clone())
                .within(ids::SHARE_LINK_ITEM, index),
        );
        els.push(
            Element::label(
                ids::SHARE_LINK_ITEM_EXPIRES,
                sl::expires(&crate::format::epoch_secs_date(
                    u64::try_from(row.expires_at).unwrap_or(0),
                )),
            )
            .within(ids::SHARE_LINK_ITEM, index),
        );
        els.push(
            Element::label(ids::SHARE_LINK_ITEM_STATE, link_state_label(&row.state))
                .attr("state", row.state.as_str())
                .within(ids::SHARE_LINK_ITEM, index),
        );
        // Copy only where the shared re-derivation verified the URL; absent
        // otherwise, never a wrong link.
        if row.url.is_some() {
            els.push(
                Element::gesture_button(
                    ids::SHARE_LINK_ITEM_COPY_BUTTON,
                    sl::COPY,
                    true,
                    Gesture::Media(Action::CopyShareLinkRow(index)),
                )
                .within(ids::SHARE_LINK_ITEM, index),
            );
        }
        if row.state == "active" {
            els.push(
                Element::gesture_button(
                    ids::SHARE_LINK_REVOKE_BUTTON,
                    sl::REVOKE,
                    true,
                    Gesture::Media(Action::ArmShareRevoke(index)),
                )
                .within(ids::SHARE_LINK_ITEM, index),
            );
        }
    }
    // The single revoke confirm — painted only while armed.
    if let Some(armed) = &list.revoke_confirm {
        let name = list
            .rows
            .iter()
            .find(|r| &r.token_id == armed)
            .map(|r| r.name.as_str())
            .unwrap_or_default();
        els.push(Element::label(
            ids::SHARE_LINK_REVOKE_CONFIRM_MODAL,
            sl::REVOKE_CONFIRM_TITLE,
        ));
        els.push(Element::chrome(sl::revoke_confirm_body(name)));
        els.push(Element::gesture_button(
            ids::SHARE_LINK_REVOKE_CONFIRM_BUTTON,
            sl::REVOKE_CONFIRM,
            true,
            Gesture::Media(Action::ConfirmShareRevoke),
        ));
        els.push(Element::gesture_button(
            ids::SHARE_LINK_REVOKE_CANCEL_BUTTON,
            sl::CANCEL,
            true,
            Gesture::Media(Action::CancelShareRevoke),
        ));
    }
    els
}

// ── E2E state ───────────────────────────────────────────────────────────────

/// The page's declared ui.yaml `state_fields`.
pub fn state_json(state: &MediaState) -> serde_json::Value {
    let Some(snapshot) = state.snapshot() else {
        return serde_json::json!({ "items": [] });
    };
    serde_json::json!({
        "items": snapshot
            .items
            .iter()
            .map(|i| serde_json::json!({
                "folder": i.folder,
                "path": i.path,
                "name": i.name,
                "size_bytes": i.size_bytes,
                "updated_at": i.updated_at,
                "thumbnail_hash": i.thumbnail_hash,
                "source_online": i.source_online,
            }))
            .collect::<Vec<_>>(),
        "folders": snapshot.folders,
        "sort": snapshot.sort,
        "filter": snapshot.filter,
        "view_grid": snapshot.view_grid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::art;
    use fauna_media_machine::observer::NullObserver;
    use fauna_media_machine::{FakeMediaNestApi, MediaItem, MediaSnapshot};

    fn item(folder: &str, path: &str, size: i64) -> MediaItem {
        MediaItem {
            folder: folder.into(),
            path: path.into(),
            size_bytes: size,
            updated_at: 1_600_000_000,
            source_online: true,
            ..Default::default()
        }
    }

    fn version(version_num: i64, size_bytes: i64) -> FileVersionSummary {
        FileVersionSummary {
            version_num,
            manifest_hash: "ab".repeat(32),
            size_bytes,
            created_at: 1_600_000_000_000,
            content_key_version: None,
            author_display: "bob".into(),
            pruned: false,
            purge_after: None,
        }
    }

    /// An app whose Media page is backed by a fake nest carrying `items`,
    /// already refreshed — the shape every element assertion below reads.
    async fn app_with(items: Vec<MediaItem>) -> App {
        let mut app = crate::app::tests::test_app();
        let api = Arc::new(FakeMediaNestApi::new());
        api.set_snapshot(MediaSnapshot {
            items,
            ..Default::default()
        });
        let machine = MediaMachine::new(Arc::new(NullObserver), api, None, None, None, None);
        // A fixed placeholder owner key, mirroring production's post-login
        // `refresh(Some(backup_key))`; these fixtures carry no `path_sealed`, so
        // `render_sealed_paths` short-circuits before ever touching it.
        machine.refresh(Some(vec![0u8; 32])).await;
        app.media.machine = Some(machine);
        app.media.device_id = Some("de".repeat(32));
        app
    }

    fn ids(app: &App) -> Vec<String> {
        elements(app).iter().map(|e| e.id.clone()).collect()
    }

    /// An item carrying a `thumbnail_hash` — the shape `fauna.media.list`
    /// returns once the producer has recorded a companion thumbnail blob.
    fn item_with_thumb(folder: &str, path: &str, hash: &str) -> MediaItem {
        MediaItem {
            thumbnail_hash: Some(hash.to_string()),
            ..item(folder, path, 10)
        }
    }

    /// One `media-thumbnail` element, by row index.
    fn thumb_of(app: &App, index: usize) -> Element {
        elements(app)
            .into_iter()
            .find(|e| {
                e.id == "media-thumbnail" && e.path == vec![("media-item".to_string(), index)]
            })
            .expect("every item registers a media-thumbnail")
    }

    /// An item with no thumbnail, one whose fetch failed, and one still loading
    /// all paint the placeholder — never a blank row and never a page error.
    ///
    /// `media.md` § Thumbnails' per-item degrade: the source folder may simply
    /// be offline, which is a picture-less row, not a failure the user must read
    /// about. The machine's own `fetch_thumbnail` contract says the same of its
    /// errors ("does not touch the page `error-message` banner").
    #[tokio::test]
    async fn an_item_without_loaded_art_paints_the_placeholder_and_no_error() {
        let mut app = app_with(vec![
            item("photos", "photos/clip.mp4", 10),
            item_with_thumb("photos", "photos/broken.jpg", "bad"),
            item_with_thumb("photos", "photos/slow.jpg", "pending"),
        ])
        .await;
        app.media.thumbnails.set("bad".into(), None);
        app.media.thumbnails.mark_loading("pending".into());

        for index in 0..3 {
            let thumb = thumb_of(&app, index);
            assert_eq!(thumb.text, crate::thumbnail::PLACEHOLDER);
            assert!(thumb.art.is_none(), "no art painted for row {index}");
        }
        assert!(
            !app.errors.contains_key(&crate::pages::Page::Media),
            "an unloadable thumbnail must never raise the page banner"
        );
    }

    /// Cached art paints as the item's `media-thumbnail`, and its `text` is the
    /// art's plaintext — the string `get_text` answers with, and the e2e's proof
    /// that the paint happened (`actions/media.py::painted_thumbnail_count`).
    #[tokio::test]
    async fn cached_art_paints_as_the_items_thumbnail() {
        let mut app = app_with(vec![item_with_thumb("photos", "photos/a.jpg", "h1")]).await;
        app.media.thumbnails.set("h1".into(), Some(art()));

        let thumb = thumb_of(&app, 0);
        assert_eq!(
            thumb.art.as_ref(),
            Some(&art().art),
            "the art rides the element"
        );
        assert_eq!(thumb.text, art().art.to_plaintext());
        assert!(thumb.text.contains(crate::thumbnail::HALF_BLOCK));
        assert!(
            thumb.pixels.is_some(),
            "and so do the protocol arms' pixels — the post-paint pass reads them \
             off the element and has no other source (`crate::graphics`)"
        );

        // …and it paints as a RECTANGLE. `art` is the third paint arm, separate
        // from the text arm, and a picture whose rows land on one line is the
        // same defect the QR hit — invisible to every assertion above, since
        // `art`, `text` and `pixels` all still read correct.
        let painted = crate::ui::painted_line_texts(&[thumb]);
        assert_eq!(
            painted.len(),
            art().art.rows.len(),
            "each art row must paint on its own line: {painted:#?}"
        );
        let widths: std::collections::BTreeSet<usize> =
            painted.iter().map(|l| l.chars().count()).collect();
        assert_eq!(
            widths.len(),
            1,
            "every art row must paint the same width or the picture is skewed: {widths:?}"
        );
    }

    /// The kick asks for exactly the uncached hashes, marks them `Loading`, and
    /// then has nothing left to ask for.
    ///
    /// This is what keeps the observer tick affordable: it fires on every machine
    /// mutation, and the element list is rebuilt every frame, so without the
    /// cache gate each tick would re-issue every thumbnail's GET.
    #[tokio::test]
    async fn the_kick_requests_each_uncached_hash_exactly_once() {
        let mut app = app_with(vec![
            item_with_thumb("photos", "photos/a.jpg", "h1"),
            item_with_thumb("photos", "photos/b.jpg", "h2"),
            // No hash: nothing to fetch.
            item("photos", "photos/c.mp4", 10),
        ])
        .await;

        let op = kick_thumbnail_fetches(&mut app).expect("two uncached hashes to load");
        match op {
            Op::FetchThumbnails { mut hashes, .. } => {
                hashes.sort();
                assert_eq!(hashes, vec!["h1".to_string(), "h2".to_string()]);
            }
            _ => panic!("the kick must produce a thumbnail fetch"),
        }
        assert!(
            matches!(app.media.thumbnails.get("h1"), Some(ImageState::Loading)),
            "requested hashes are marked in flight"
        );

        // A second tick with the same snapshot has nothing to do — the `Loading`
        // marks are what make the repeat impossible.
        assert!(
            kick_thumbnail_fetches(&mut app).is_none(),
            "a repeat tick must not re-issue an in-flight fetch"
        );

        // Nor once they settle.
        apply_outcome(
            &mut app,
            Outcome::Thumbnails(vec![("h1".into(), Some(art())), ("h2".into(), None)]),
        );
        assert!(
            kick_thumbnail_fetches(&mut app).is_none(),
            "a settled fetch — art or failure — is never re-issued"
        );
        assert!(matches!(
            app.media.thumbnails.get("h2"),
            Some(ImageState::Failed)
        ));
    }

    /// Folding thumbnails leaves the page banner alone in BOTH directions: a
    /// failed thumbnail raises no error, and a successful one does not clear an
    /// unrelated error the machine is still reporting.
    #[tokio::test]
    async fn folding_thumbnails_never_touches_the_page_error() {
        let mut app = app_with(vec![item_with_thumb("photos", "photos/a.jpg", "h1")]).await;
        app.errors
            .insert(crate::pages::Page::Media, "upload failed".into());

        apply_outcome(
            &mut app,
            Outcome::Thumbnails(vec![("h1".into(), Some(art()))]),
        );
        assert_eq!(
            app.errors
                .get(&crate::pages::Page::Media)
                .map(String::as_str),
            Some("upload failed"),
            "a thumbnail load must not clear an unrelated page error"
        );

        app.errors.clear();
        apply_outcome(&mut app, Outcome::Thumbnails(vec![("h1".into(), None)]));
        assert!(
            !app.errors.contains_key(&crate::pages::Page::Media),
            "a failed thumbnail must not raise the page banner"
        );
    }

    /// The empty explorer paints exactly ui.yaml's `media` page elements — the
    /// chrome + the upload affordance — and nothing item-shaped.
    ///
    /// `page-heading` / `error-message` aside, this is the page's whole static
    /// ID surface. `media-empty-state` closes it: a LOADED page holding nothing
    /// says so under its own ID (user-approved 2026-08-05), where it used to
    /// paint as untagged chrome.
    #[tokio::test]
    async fn the_empty_explorer_paints_exactly_the_ui_yaml_chrome() {
        let app = app_with(vec![]).await;
        assert_eq!(
            ids(&app),
            vec![
                "page-heading",
                "media-view-toggle",
                "media-sort-select",
                "media-sort-direction",
                "media-folder-filter",
                "file-upload",
                "upload-button",
                "share-link-list-button",
                "media-empty-state",
            ]
        );
    }

    /// An app whose Media machine exists but has **never refreshed** — the state
    /// every app is in for the whole first read, and the one that used to be
    /// indistinguishable from "empty".
    fn app_unrefreshed() -> App {
        let mut app = crate::app::tests::test_app();
        let api = Arc::new(FakeMediaNestApi::new());
        api.set_snapshot(MediaSnapshot {
            items: vec![item("photos", "photos/a.jpg", 10)],
            ..Default::default()
        });
        app.media.machine = Some(MediaMachine::new(
            Arc::new(NullObserver),
            api,
            None,
            None,
            None,
            None,
        ));
        app.media.device_id = Some("de".repeat(32));
        app
    }

    /// A page still loading must NOT claim to be empty.
    ///
    /// This is the whole point of `MediaPageSnapshot::loaded`: before this, a
    /// first read in flight painted "No media yet" over media that was about to
    /// arrive, and the multiseat harness read the same ambiguity as a settled
    /// empty set (`helpers/multiseat_config.py::settle_listing`). The fake here
    /// is deliberately loaded with an item the page has not fetched yet — the
    /// listing is NOT genuinely empty, only unread.
    #[tokio::test]
    async fn a_page_that_has_not_finished_loading_paints_no_empty_state() {
        let app = app_unrefreshed();
        assert!(
            !ids(&app).contains(&"media-empty-state".to_string()),
            "media-empty-state must be absent while the first read is in flight — \
             its absence beside zero rows is what identifies the loading state"
        );
        assert!(
            !elements(&app).iter().any(|e| e.id == "media-item"),
            "and no rows have arrived yet either"
        );
    }

    /// The positive twin: once a read has RETURNED and found nothing, the page
    /// is genuinely empty and says so under its ID.
    #[tokio::test]
    async fn a_loaded_page_with_no_media_paints_the_empty_state() {
        let app = app_with(vec![]).await;
        let els = elements(&app);
        let empty = els
            .iter()
            .find(|e| e.id == "media-empty-state")
            .expect("a loaded, media-less page paints media-empty-state");
        assert!(
            !empty.text.is_empty(),
            "it is real visible UI carrying the 'No media yet' line, not an \
             automation shim (convention 1: no invisible shims)"
        );
    }

    /// And a page holding media never paints it.
    #[tokio::test]
    async fn a_page_with_media_paints_no_empty_state() {
        let app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        assert!(!ids(&app).contains(&"media-empty-state".to_string()));
    }

    /// Each item paints the indexed `media-item` component: the row root plus
    /// its five children, each SCOPED to its own row — so a test reads
    /// `media-item-name` under `media-item[1]` rather than counting globally.
    ///
    /// The child set (and its order) is `ui/media.md` § Element IDs' component
    /// definition; `media-thumbnail` is registered for every item, painting the
    /// placeholder until (or unless) its art loads.
    #[tokio::test]
    async fn items_paint_the_indexed_component_with_scoped_children() {
        let app = app_with(vec![
            item("photos", "photos/a.jpg", 10),
            item("docs", "docs/notes.txt", 2048),
        ])
        .await;
        let els = elements(&app);

        let rows: Vec<_> = els.iter().filter(|e| e.id == "media-item").collect();
        assert_eq!(rows.len(), 2, "one row root per item");

        // The children of row 1 carry that row's scope path, and nothing else does.
        let scoped: Vec<_> = els
            .iter()
            .filter(|e| e.path == vec![("media-item".to_string(), 1)])
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(
            scoped,
            vec![
                "media-item-name",
                "media-item-size",
                "media-item-date",
                "media-thumbnail",
                "media-source-status",
                // The file's own presence: the control-plane `Synced` leg
                // (`file-sync.md` § Per-file sync-status display), the shared
                // label — ui.yaml's media-item child order, badge last.
                "sync-state-badge"
            ]
        );

        // The name is the shared-Rust basename projection, not the full path.
        let name = els
            .iter()
            .find(|e| e.id == "media-item-name" && e.path == vec![("media-item".to_string(), 1)])
            .expect("row 1 name");
        assert_eq!(name.text, "notes.txt");

        // Sizes render through the shared byte scale.
        let size = els
            .iter()
            .find(|e| e.id == "media-item-size" && e.path == vec![("media-item".to_string(), 1)])
            .expect("row 1 size");
        assert_eq!(size.text, crate::format::byte_size(2048));
    }

    /// The view toggle's LABEL reflects the active mode, and the gesture flips
    /// the machine's view state — which is what makes the flip observable.
    #[tokio::test]
    async fn the_view_toggle_reflects_and_flips_the_machines_view_state() {
        let mut app = app_with(vec![]).await;
        let label = |app: &App| {
            elements(app)
                .into_iter()
                .find(|e| e.id == "media-view-toggle")
                .expect("toggle")
                .text
        };
        assert_eq!(label(&app), t::VIEW_LIST, "list is the default view");

        assert!(apply_local(&mut app, Action::ToggleView).is_none());
        assert!(app.media.snapshot().unwrap().view_grid, "flipped to grid");
        assert_eq!(label(&app), t::VIEW_GRID, "the label follows the mode");

        apply_local(&mut app, Action::ToggleView);
        assert_eq!(label(&app), t::VIEW_LIST, "and back");
    }

    /// The filter select offers the all-media sentinel FIRST, then every
    /// browsable set the client knows — Sync and Backup sets, empty ones
    /// included (`MediaSnapshot::folders`, the empty-set upload-target
    /// fix); selecting the sentinel means "no filter".
    #[tokio::test]
    async fn the_filter_offers_the_all_media_sentinel_then_the_sets() {
        let mut app = app_with(vec![
            item("photos", "photos/a.jpg", 10),
            item("docs", "docs/notes.txt", 20),
        ])
        .await;
        let filter = elements(&app)
            .into_iter()
            .find(|e| e.id == "media-folder-filter")
            .expect("filter");
        let crate::element::Role::Select { options, .. } = &filter.role else {
            panic!("the filter must be a select");
        };
        assert_eq!(
            options[0], FILTER_ALL_VALUE,
            "all-media is the first option"
        );
        assert!(options.contains(&"photos".to_string()));
        assert!(options.contains(&"docs".to_string()));
        assert_eq!(filter.text, FILTER_ALL_VALUE, "and is the default");

        // Selecting a set narrows the machine; the sentinel clears it.
        apply_local(&mut app, Action::SetFilter("photos".into()));
        assert_eq!(
            app.media.snapshot().unwrap().filter.as_deref(),
            Some("photos")
        );
        apply_local(&mut app, Action::SetFilter(FILTER_ALL_VALUE.into()));
        assert_eq!(
            app.media.snapshot().unwrap().filter,
            None,
            "the sentinel is the all-media view, never a set named __all__"
        );
    }

    /// **The earlier false-green, pinned.** The e2e that was written to catch
    /// "an empty folder can never be an upload target" PASSED against the
    /// un-fixed code, because `select` handed the set name straight to
    /// `Action::SetFilter` without asking whether this frame had painted it as
    /// an option. So the test drove a state no user could reach, and the
    /// missing-option half of the bug was invisible to it.
    ///
    /// The agent must refuse instead: 409 (the element was *found* — this is not
    /// a scroll-retry case), a message naming what the frame did paint, and the
    /// filter left exactly as it was. Membership IS reachability here: the
    /// keyboard path cycles this same `options` list.
    #[tokio::test]
    async fn the_filter_refuses_a_set_this_frame_never_painted() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        let mut registry = crate::automation::Registry::default();
        for e in elements(&app) {
            registry.element(e);
        }
        let select = |value: &str| fauna_e2e_agent::ElementReq {
            kind: fauna_e2e_agent::ElementKind::Select,
            id: "media-folder-filter".to_string(),
            index: 0,
            scope: Vec::new(),
            arg: value.to_string(),
        };

        // An offered option actuates, exactly as before.
        let reply = crate::automation::perform(&mut app, &registry, &select("photos")).await;
        assert_eq!(reply, serde_json::json!({ "ok": true }));
        assert_eq!(
            app.media.snapshot().unwrap().filter.as_deref(),
            Some("photos")
        );

        // A set this frame never painted is refused, and changes nothing.
        let reply = crate::automation::perform(&mut app, &registry, &select("never-painted")).await;
        assert_eq!(
            reply["status"], 409,
            "the element was found — a 404 would send the driver scroll-retrying \
             for a picker that is already on screen"
        );
        let error = reply["error"].as_str().expect("a refusal names itself");
        assert!(
            error.contains("never-painted") && error.contains("media-folder-filter"),
            "the refusal must name both the value and the picker: {error}"
        );
        assert!(
            error.contains("photos"),
            "and must list what the frame DID paint, so the failure diagnoses \
             itself (convention 6): {error}"
        );
        assert_eq!(
            app.media.snapshot().unwrap().filter.as_deref(),
            Some("photos"),
            "a refused select must not have moved the machine"
        );
    }

    /// The sort select round-trips the RAW key (what every app's suite drives
    /// it with), carrying the localized label as paint-only.
    #[tokio::test]
    async fn the_sort_select_round_trips_the_raw_key() {
        let mut app = app_with(vec![]).await;
        apply_local(&mut app, Action::SetSort("size".into()));
        let sort = elements(&app)
            .into_iter()
            .find(|e| e.id == "media-sort-select")
            .expect("sort");
        assert_eq!(sort.text, "size", "get_text returns the raw key");
        match &sort.role {
            crate::element::Role::Select { display, .. } => assert_eq!(
                display.as_deref(),
                Some(t::SORT_SIZE),
                "the human form of the current value is the paint-only display"
            ),
            other => panic!("sort should be a select, got {other:?}"),
        }
    }

    /// The direction select round-trips its RAW value and actually re-orders the
    /// rendered list — the property the announce read depends on.
    ///
    /// Ordering, not tolerance, is what makes a lazy-list client's read sound: an
    /// off-screen row registers neither its cell nor its name, so "read the whole
    /// list and hope" cannot be enforced from the harness, whereas "put the rows
    /// that matter at the HEAD" is one UI action. Asserting the reversal here (not
    /// just the round-trip) is what stops a client wiring the select to nothing.
    #[tokio::test]
    async fn the_sort_direction_select_round_trips_and_reverses_the_order() {
        let mut app = app_with(vec![
            item("docs", "docs/alpha.txt", 1),
            item("docs", "docs/bravo.txt", 2),
            item("docs", "docs/charlie.txt", 3),
        ])
        .await;

        // The rendered order, read off the per-row `media-item-name` children in
        // paint order — the same thing the e2e reader walks by index.
        fn rendered_item_names(app: &App) -> Vec<String> {
            elements(app)
                .into_iter()
                .filter(|e| e.id == "media-item-name")
                .map(|e| e.text.clone())
                .collect()
        }

        assert_eq!(
            rendered_item_names(&app),
            vec!["alpha.txt", "bravo.txt", "charlie.txt"],
            "ascending is the default order"
        );

        apply_local(&mut app, Action::SetSortDirection("descending".into()));

        let direction = elements(&app)
            .into_iter()
            .find(|e| e.id == "media-sort-direction")
            .expect("direction select");
        assert_eq!(
            direction.text, "descending",
            "get_text returns the raw direction value"
        );
        match &direction.role {
            crate::element::Role::Select { display, .. } => assert_eq!(
                display.as_deref(),
                Some(t::SORT_DESCENDING),
                "the human form of the current value is the paint-only display"
            ),
            other => panic!("direction should be a select, got {other:?}"),
        }
        assert_eq!(
            rendered_item_names(&app),
            vec!["charlie.txt", "bravo.txt", "alpha.txt"],
            "descending reverses the rendered order, so the newest-by-key rows \
             sit at the HEAD where a lazy client registers them"
        );
    }

    /// An upload with a file staged but NO folder to target surfaces the
    /// machine's `media.error_no_set` policy on the page's `error-message`.
    ///
    /// Regression, caught by e2e before this test existed: the machine owns the
    /// page error and reports it on its *snapshot*, but tui's `error-message` is
    /// registered off `App::errors` — with no bridge the gesture ran, set the
    /// machine's error, and the page showed **nothing**. No error and no effect
    /// is indistinguishable from a dropped command (`testing.md` point 10), and
    /// it is the exact shape `test_media_upload_no_set_error` asserts on every
    /// app. Costs milliseconds here; cost a 9.5-minute e2e round trip to find.
    #[tokio::test]
    async fn an_upload_with_no_target_set_surfaces_the_machines_no_set_error() {
        let dir = tempfile::tempdir().unwrap();
        let picked = dir.path().join("snapshot.jpg");
        std::fs::write(&picked, b"bytes").unwrap();

        // No items => no readable set to default the upload target to.
        let mut app = app_with(vec![]).await;
        app.media.file_input = picked.to_string_lossy().into_owned();

        let op = apply_local(&mut app, Action::Upload).expect("an upload op");
        let outcome = op.run().await;
        apply_outcome(&mut app, outcome);

        assert_eq!(
            app.errors
                .get(&crate::pages::Page::Media)
                .map(String::as_str),
            Some(t::ERROR_NO_SET),
            "the machine's no-set policy must reach error-message"
        );
    }

    /// Upload with an empty path box ANSWERS rather than no-opping.
    ///
    /// The same "no error and no effect is indistinguishable from a dropped
    /// command" rule as the test above, reached from field evidence instead of
    /// e2e: a live user pressed Upload with nothing typed and read the silence
    /// as a broken button (field report). The old
    /// `return None` was justified by "the prompt guides" — which is exactly
    /// the reasoning the same user disproved, since the prompt then said
    /// "Choose file…" on an app with no picker.
    ///
    /// **This deliberately supersedes `upload_with_no_file_staged_is_a_silent_no_op`**,
    /// which asserted the silence as correct ("the user has not asked for
    /// anything yet, so the page must not shout at them"). That reading was
    /// reasonable before a human tried it; the field drive is the evidence that
    /// settles it. Deleted rather than left red — a superseded contract's test
    /// is not a regression to route around.
    #[tokio::test]
    async fn an_upload_with_an_empty_path_says_so_instead_of_no_opping() {
        let mut app = app_with(vec![]).await;
        app.media.file_input = "   ".to_string();

        assert!(
            apply_local(&mut app, Action::Upload).is_none(),
            "an empty path must not start an upload op"
        );
        assert_eq!(
            app.errors
                .get(&crate::pages::Page::Media)
                .map(String::as_str),
            Some(t::FILE_PATH_REQUIRED),
            "a pressed button must always answer — on `error-message`, never with silence"
        );
    }

    /// A later success clears the machine's error off the banner — the bridge
    /// syncs both directions, so a stale error can't outlive its cause.
    #[tokio::test]
    async fn a_machine_success_clears_the_page_error() {
        let mut app = app_with(vec![]).await;
        app.errors
            .insert(crate::pages::Page::Media, "stale".to_string());
        let op = nav_enter_op(&app.media).expect("a refresh op");
        let outcome = op.run().await;
        apply_outcome(&mut app, outcome);
        assert!(!app.errors.contains_key(&crate::pages::Page::Media));
    }

    /// An unreadable staged path fails on the page's `error-message` and never
    /// reaches the machine — the glue error surfaces at once, wrapped in the
    /// shared `media.error_upload` string (linux's `show_upload_glue_error`).
    #[tokio::test]
    async fn upload_of_an_unreadable_path_errors_without_reaching_the_machine() {
        let mut app = app_with(vec![]).await;
        app.media.file_input = "/definitely/not/a/real/file.png".into();
        assert!(apply_local(&mut app, Action::Upload).is_none());
        let err = app
            .errors
            .get(&crate::pages::Page::Media)
            .expect("page error");
        assert!(
            err.starts_with("Failed to upload:"),
            "expected the shared error_upload wrapper, got {err:?}"
        );
    }

    /// A staged file yields the upload Op carrying the picked file's BASENAME as
    /// the member path (not the full local path) — the shape the version-history
    /// suite depends on when it re-uploads the same basename as a new version.
    #[tokio::test]
    async fn upload_stages_the_basename_as_the_member_path() {
        let dir = tempfile::tempdir().unwrap();
        let picked = dir.path().join("snapshot.jpg");
        std::fs::write(&picked, b"bytes").unwrap();

        let mut app = app_with(vec![]).await;
        app.media.file_input = picked.to_string_lossy().into_owned();
        let op = apply_local(&mut app, Action::Upload).expect("an upload op");
        match op {
            Op::Upload {
                path, raw_bytes, ..
            } => {
                assert_eq!(path, "snapshot.jpg", "the member path is the basename");
                assert_eq!(raw_bytes, b"bytes");
            }
            _ => panic!("expected an Upload op"),
        }
    }

    /// Opening an item both opens the detail surface and returns the version
    /// load — the detail can't paint rows it never asked for.
    #[tokio::test]
    async fn opening_an_item_opens_the_detail_and_asks_for_its_versions() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        let op = apply_local(&mut app, Action::OpenDetail(0)).expect("a versions op");
        match op {
            Op::LoadVersions { folder, path, .. } => {
                assert_eq!(folder, "photos");
                assert_eq!(path, "photos/a.jpg");
            }
            _ => panic!("expected a LoadVersions op"),
        }
        let detail = app.media.detail.as_ref().expect("the detail surface");
        assert_eq!(detail.name, "a.jpg");
        assert!(detail.versions.is_none(), "rows arrive with the outcome");

        // While open, the detail owns the pane — the explorer chrome is not
        // painted behind it.
        let ids = ids(&app);
        assert!(ids.contains(&"media-item-detail".to_string()));
        assert!(!ids.contains(&"media-view-toggle".to_string()));
    }

    /// An item carrying the wire `path_hash` — the durable half of the identity
    /// a `SearchNav::File` row names.
    fn item_with_hash(folder: &str, path: &str, path_hash: [u8; 32]) -> MediaItem {
        MediaItem {
            path_hash: Some(fauna_protocol::ByteBuf::from(path_hash.to_vec())),
            ..item(folder, path, 10)
        }
    }

    /// `app_with`, plus the control-plane set rows — which a durable
    /// `folder_id` can only be resolved through.
    async fn app_with_sets(
        items: Vec<MediaItem>,
        sets: Vec<fauna_media_machine::MediaFolder>,
    ) -> App {
        let mut app = crate::app::tests::test_app();
        let api = Arc::new(FakeMediaNestApi::new());
        api.set_snapshot(MediaSnapshot {
            items,
            ..Default::default()
        });
        api.set_folders(sets);
        let machine = MediaMachine::new(Arc::new(NullObserver), api, None, None, None, None);
        machine.refresh(Some(vec![0u8; 32])).await;
        app.media.machine = Some(machine);
        app.media.device_id = Some("de".repeat(32));
        app
    }

    fn set_row(id: i64, name: &str) -> fauna_media_machine::MediaFolder {
        fauna_media_machine::MediaFolder {
            id,
            name: name.into(),
            ..Default::default()
        }
    }

    /// A `SearchNav::File` deep link opens the detail for the file its identity
    /// pair names — **regardless of the browse state**, which is the property
    /// that separates it from `OpenDetail`'s index. Here the active filter
    /// excludes the target's set and the file is not even in the rendered list;
    /// it must still open, and the filter must follow it rather than leaving
    /// the user to close the detail onto a list without it.
    #[tokio::test]
    async fn a_file_deep_link_opens_its_detail_across_the_active_filter() {
        let mut app = app_with_sets(
            vec![
                item_with_hash("photos", "photos/a.jpg", [0x11; 32]),
                item_with_hash("docs", "docs/b.pdf", [0x22; 32]),
            ],
            vec![set_row(7, "photos"), set_row(9, "docs")],
        )
        .await;
        // The user is browsing only "photos"; the target lives in "docs".
        apply_local(&mut app, Action::SetFilter("photos".into()));
        assert!(
            !app.media
                .snapshot()
                .expect("a snapshot")
                .items
                .iter()
                .any(|i| i.path == "docs/b.pdf"),
            "precondition: the target is not in the rendered list"
        );

        let op = apply_local(
            &mut app,
            Action::OpenFile {
                folder_id: 9,
                path_hash: fauna_core::hex32::encode(&[0x22; 32]),
            },
        )
        .expect("a versions op");
        match op {
            Op::LoadVersions { folder, path, .. } => {
                assert_eq!(folder, "docs");
                assert_eq!(path, "docs/b.pdf");
            }
            _ => panic!("expected a LoadVersions op"),
        }
        let detail = app.media.detail.as_ref().expect("the detail surface");
        assert_eq!(detail.name, "b.pdf");
        assert_eq!(
            app.media.snapshot().expect("a snapshot").filter.as_deref(),
            Some("docs"),
            "the browse follows the file the user just opened"
        );
        assert!(!app.errors.contains_key(&crate::pages::Page::Media));
    }

    /// The file was deleted or renamed between being indexed and being clicked
    /// (a rename is structurally delete + create). Say so — a silently
    /// unchanged page reads as a dead row.
    #[tokio::test]
    async fn a_file_deep_link_to_a_vanished_file_reports_it_instead_of_going_quiet() {
        let mut app = app_with_sets(
            vec![item_with_hash("photos", "photos/a.jpg", [0x11; 32])],
            vec![set_row(7, "photos")],
        )
        .await;
        let op = apply_local(
            &mut app,
            Action::OpenFile {
                folder_id: 7,
                path_hash: fauna_core::hex32::encode(&[0xEE; 32]),
            },
        );
        assert!(op.is_none());
        assert!(app.media.detail.is_none(), "nothing opens");
        assert_eq!(
            app.errors
                .get(&crate::pages::Page::Media)
                .map(String::as_str),
            Some(t::FILE_NOT_FOUND),
        );
    }

    /// Version rows are the indexed `file-version-history` component: an
    /// UNSCOPED row root per version, with its children scoped under it.
    ///
    /// Regression: registering the root `.within(ids::FILE_VERSION_ITEM, i)` — i.e.
    /// inside itself — leaves `count("file-version-item")` at 0 and every scoped
    /// child read resolving to nothing, so the whole version suite reads as "the
    /// rows never loaded" rather than as a registration bug.
    #[tokio::test]
    async fn version_rows_are_unscoped_roots_with_scoped_children() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        apply_outcome(
            &mut app,
            Outcome::Versions(vec![version(1, 10), version(2, 48_000)]),
        );
        let els = elements(&app);

        let roots: Vec<_> = els.iter().filter(|e| e.id == "file-version-item").collect();
        assert_eq!(roots.len(), 2, "one row root per version");
        assert!(
            roots.iter().all(|e| e.path.is_empty()),
            "a row root must not be scoped inside itself"
        );

        let scoped: Vec<_> = els
            .iter()
            .filter(|e| e.path == vec![("file-version-item".to_string(), 1)])
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(
            scoped,
            vec![
                "file-version-size",
                "file-version-timestamp",
                "file-version-author",
                "file-version-restore-button"
            ]
        );

        // Version sizes render exactly as item sizes do — the suite compares the
        // two strings directly.
        let size = els
            .iter()
            .find(|e| {
                e.id == "file-version-size" && e.path == vec![("file-version-item".to_string(), 1)]
            })
            .expect("row 1 size");
        assert_eq!(size.text, crate::format::byte_size(48_000));
    }

    /// A soft-pruned version row — only an `include_pruned` listing carries
    /// one. It says so with `file-version-pruned-badge` and offers
    /// `file-version-undelete-button`; a live row renders neither, ever
    /// (the `snapshot-undelete-button` present-only-on-recoverable shape).
    fn pruned(version_num: i64, size_bytes: i64) -> FileVersionSummary {
        FileVersionSummary {
            pruned: true,
            purge_after: Some(1_800_000_000),
            ..version(version_num, size_bytes)
        }
    }

    #[tokio::test]
    async fn pruned_rows_render_badge_and_undelete_and_live_rows_never_do() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        // The recovery browse is on — the outcome below is an include_pruned
        // listing, so it may carry the pruned row.
        let _ = apply_local(&mut app, Action::TogglePruned);
        apply_outcome(
            &mut app,
            Outcome::Versions(vec![version(1, 10), pruned(2, 48_000)]),
        );
        let els = elements(&app);

        let scoped_ids = |row: usize| -> Vec<String> {
            els.iter()
                .filter(|e| e.path == vec![("file-version-item".to_string(), row)])
                .map(|e| e.id.clone())
                .collect()
        };
        assert_eq!(
            scoped_ids(1),
            vec![
                "file-version-size",
                "file-version-timestamp",
                "file-version-author",
                "file-version-pruned-badge",
                "file-version-undelete-button",
                "file-version-restore-button"
            ],
            "the pruned row carries its badge + recovery verb"
        );
        assert!(
            !scoped_ids(0).iter().any(|id| id == "file-version-pruned-badge"
                || id == "file-version-undelete-button"),
            "a live row never renders the recovery affordance"
        );
        // The switch itself renders, stateful.
        let toggle = els
            .iter()
            .find(|e| e.id == "file-version-show-pruned-toggle")
            .expect("the recovery browse switch");
        assert_eq!(
            toggle
                .attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.as_str()),
            Some("on")
        );
    }

    /// The switch re-issues the load under the new toggle — and drops back to
    /// the loading state, so the old population never renders under the new
    /// switch position.
    #[tokio::test]
    async fn the_show_pruned_toggle_reissues_the_load_with_include_pruned() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        apply_outcome(&mut app, Outcome::Versions(vec![version(1, 10)]));

        match apply_local(&mut app, Action::TogglePruned) {
            Some(Op::LoadVersions {
                folder,
                path,
                include_pruned,
                ..
            }) => {
                assert_eq!(folder, "photos");
                assert_eq!(path, "photos/a.jpg");
                assert!(include_pruned, "ON asks for the include_pruned listing");
            }
            other => panic!("expected a LoadVersions op, got {:?}", other.is_some()),
        }
        let detail = app.media.detail.as_ref().expect("the detail surface");
        assert!(detail.show_pruned);
        assert!(
            detail.versions.is_none(),
            "back to the loading state across the switchover"
        );

        apply_outcome(&mut app, Outcome::Versions(vec![version(1, 10)]));
        match apply_local(&mut app, Action::TogglePruned) {
            Some(Op::LoadVersions { include_pruned, .. }) => {
                assert!(!include_pruned, "OFF returns to the live-only listing");
            }
            other => panic!("expected a LoadVersions op, got {:?}", other.is_some()),
        }
    }

    /// The undelete gesture: a pruned row yields the wire op (carrying the
    /// row's stable seq + the browse's current toggle for the re-list); a
    /// stale click on a live row is a no-op, never a spurious wire call.
    #[tokio::test]
    async fn undelete_fires_on_a_pruned_row_and_noops_on_a_live_one() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        let _ = apply_local(&mut app, Action::TogglePruned);
        apply_outcome(
            &mut app,
            Outcome::Versions(vec![version(1, 10), pruned(7, 48_000)]),
        );

        assert!(
            apply_local(&mut app, Action::Undelete(0)).is_none(),
            "a live row's undelete is a no-op"
        );
        match apply_local(&mut app, Action::Undelete(1)) {
            Some(Op::UndeleteVersion {
                version_num,
                include_pruned,
                ..
            }) => {
                assert_eq!(version_num, 7, "the row's stable seq, not its index");
                assert!(include_pruned, "the re-list keeps the browse's toggle");
            }
            other => panic!("expected an UndeleteVersion op, got {:?}", other.is_some()),
        }
    }

    /// `file-version-author` renders "Edited by ‹handle›" on every row, each
    /// with its own recorder.
    #[tokio::test]
    async fn version_author_renders_on_every_row() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        apply_outcome(
            &mut app,
            Outcome::Versions(vec![
                FileVersionSummary {
                    author_display: "alice".to_string(),
                    ..version(1, 10)
                },
                version(2, 20),
            ]),
        );
        let els = elements(&app);
        let author_of = |row: usize| {
            els.iter()
                .find(|e| {
                    e.id == "file-version-author"
                        && e.path == vec![("file-version-item".to_string(), row)]
                })
                .unwrap_or_else(|| panic!("row {row} renders file-version-author"))
                .text
                .clone()
        };
        assert_eq!(author_of(0), t::version_author("alice"));
        assert_eq!(author_of(1), t::version_author("bob"));
    }

    /// Restore is a TWO-step gesture: the per-row button only ARMS the confirm,
    /// and only the confirm returns the restore Op. A single click must never
    /// mutate the file.
    #[tokio::test]
    async fn restore_arms_a_confirm_before_it_mutates_anything() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        apply_outcome(&mut app, Outcome::Versions(vec![version(1, 10)]));

        // The confirm modal is not painted until the row button arms it.
        assert!(!ids(&app).contains(&"file-version-restore-confirm-modal".to_string()));
        assert!(
            apply_local(&mut app, Action::ArmRestore(0)).is_none(),
            "arming runs nothing"
        );
        assert!(ids(&app).contains(&"file-version-restore-confirm-modal".to_string()));

        // Only the confirm produces the restore.
        let op = apply_local(&mut app, Action::ConfirmRestore).expect("a restore op");
        assert!(matches!(op, Op::RestoreVersion { .. }));
    }

    /// Cancelling the confirm disarms it and mutates nothing.
    #[tokio::test]
    async fn cancelling_the_restore_confirm_disarms_it() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        apply_outcome(&mut app, Outcome::Versions(vec![version(1, 10)]));
        apply_local(&mut app, Action::ArmRestore(0));
        assert!(apply_local(&mut app, Action::CancelRestore).is_none());
        assert!(!ids(&app).contains(&"file-version-restore-confirm-modal".to_string()));
        assert!(app.media.detail.as_ref().unwrap().arming.is_none());
    }

    // ── Delete the opened file (media-delete-*) ─────────────────────────────

    /// The detail surface always offers the delete trigger, and the confirm trio
    /// is painted ONLY while armed — a painted confirm the user never asked for
    /// would be a delete one keypress from firing.
    #[tokio::test]
    async fn the_detail_paints_delete_and_arms_its_confirm_only_on_demand() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        apply_outcome(&mut app, Outcome::Versions(vec![version(1, 10)]));

        let before = ids(&app);
        assert!(
            before.contains(&"media-delete-button".to_string()),
            "the opened detail surface must offer media-delete-button"
        );
        for id in [
            "media-delete-confirm-modal",
            "media-delete-confirm-button",
            "media-delete-cancel-button",
        ] {
            assert!(
                !before.contains(&id.to_string()),
                "{id} must not be painted before the trigger arms it"
            );
        }

        assert!(
            apply_local(&mut app, Action::ArmDelete).is_none(),
            "arming runs nothing"
        );
        let armed = ids(&app);
        for id in [
            "media-delete-confirm-modal",
            "media-delete-confirm-button",
            "media-delete-cancel-button",
        ] {
            assert!(armed.contains(&id.to_string()), "{id} paints once armed");
        }
    }

    /// Delete is a TWO-step gesture: the trigger only ARMS, and only the confirm
    /// produces the Op. A single click must never record a tombstone.
    #[tokio::test]
    async fn delete_arms_a_confirm_before_it_mutates_anything() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));

        assert!(apply_local(&mut app, Action::ArmDelete).is_none());
        let op = apply_local(&mut app, Action::ConfirmDelete).expect("a delete op");
        match op {
            Op::Delete {
                folder,
                path,
                device_id,
                ..
            } => {
                // The Op carries the OPENED item's identity, not the list's
                // selection — the detail surface is what the user acted on.
                assert_eq!(folder, "photos");
                assert_eq!(path, "photos/a.jpg");
                assert_eq!(device_id, "de".repeat(32));
            }
            _ => panic!("expected a Delete op"),
        }
    }

    /// Cancelling the delete confirm is a PURE no-op: no Op, no error, the
    /// surface stays open. Pinning it stops a future refactor from wiring cancel
    /// to the delete path — a silent data-loss bug the happy path cannot see.
    #[tokio::test]
    async fn cancelling_the_delete_confirm_is_a_pure_no_op() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        apply_local(&mut app, Action::ArmDelete);

        assert!(apply_local(&mut app, Action::CancelDelete).is_none());
        assert!(!ids(&app).contains(&"media-delete-confirm-modal".to_string()));
        assert!(!app.media.detail.as_ref().unwrap().delete_arming);
        assert!(
            !app.errors.contains_key(&crate::pages::Page::Media),
            "cancelling must surface no error"
        );
        // Still open on the same item — cancel does not close the surface.
        assert_eq!(app.media.detail.as_ref().unwrap().path, "photos/a.jpg");
    }

    /// A confirm that was never armed is a no-op — a stale driver click on a
    /// disarmed surface must not delete.
    #[tokio::test]
    async fn an_unarmed_delete_confirm_does_nothing() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        assert!(apply_local(&mut app, Action::ConfirmDelete).is_none());
    }

    /// The delete's only client glue is the device-id read; when it is missing
    /// the gesture surfaces `media.error_delete` on `error-message` rather than
    /// silently dropping (`testing.md` point 11).
    #[tokio::test]
    async fn a_missing_device_id_surfaces_the_delete_error() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        apply_local(&mut app, Action::ArmDelete);
        app.media.device_id = None;

        assert!(apply_local(&mut app, Action::ConfirmDelete).is_none());
        assert_eq!(
            app.errors.get(&crate::pages::Page::Media),
            Some(&t::error_delete("no sync device id"))
        );
    }

    /// A successful delete closes the detail surface — its subject is gone, so
    /// its version history would be a dangling view.
    #[tokio::test]
    async fn a_successful_delete_closes_the_detail_surface() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        apply_outcome(&mut app, Outcome::Deleted { succeeded: true });

        assert!(app.media.detail.is_none());
        assert!(!app.errors.contains_key(&crate::pages::Page::Media));
        // Back on the explorer, whose chrome the detail surface had replaced.
        assert!(ids(&app).contains(&"media-view-toggle".to_string()));
    }

    /// A FAILED delete keeps the surface open — the file is still there, and
    /// closing would yank away the context the user acted in.
    #[tokio::test]
    async fn a_failed_delete_keeps_the_detail_surface_open() {
        let mut app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        apply_outcome(&mut app, Outcome::Deleted { succeeded: false });

        assert!(app.media.detail.is_some());
        assert!(ids(&app).contains(&"media-delete-button".to_string()));
    }

    /// `data.media.items[]` is a contract: the harness resolves the aggregate
    /// from state and nowhere else, so every wire field must survive.
    #[tokio::test]
    async fn state_json_carries_the_snapshot_contract() {
        let app = app_with(vec![item("photos", "photos/a.jpg", 10)]).await;
        let state = state_json(&app.media);
        assert_eq!(state["items"][0]["folder"], "photos");
        assert_eq!(state["items"][0]["path"], "photos/a.jpg");
        assert_eq!(state["items"][0]["name"], "a.jpg");
        assert_eq!(state["items"][0]["size_bytes"], 10);
        assert_eq!(state["items"][0]["source_online"], true);
        assert_eq!(state["filter"], serde_json::Value::Null);
        assert_eq!(state["view_grid"], false);
    }

    /// Pre-auth (no machine) the page still paints its heading rather than
    /// panicking — the e2e state serializer runs before sign-in.
    #[test]
    fn the_page_degrades_to_its_heading_with_no_machine() {
        let app = crate::app::tests::test_app();
        assert_eq!(ids(&app), vec!["page-heading"]);
        assert_eq!(state_json(&app.media), serde_json::json!({ "items": [] }));
    }

    // ── The external-media open (media-external-open-*) ─────────────────────

    use crate::settings::ExternalMediaMode;

    /// An app with one AV item's detail open and its version rows loaded — the
    /// state every external-open assertion below starts from.
    async fn app_with_av_detail_open() -> App {
        let mut app = app_with(vec![item("photos", "photos/clip.mp4", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        apply_outcome(
            &mut app,
            Outcome::Versions(vec![
                version(1, 10),
                FileVersionSummary {
                    manifest_hash: "cd".repeat(32),
                    ..version(2, 20)
                },
            ]),
        );
        app
    }

    /// The trigger paints ONLY when all three gates hold: an audio/video item
    /// (the shared `is_audio_video_filename` predicate), mode ask|always, and
    /// loaded version rows (a painted trigger is always actionable). Under
    /// `never` it is ABSENT entirely — metadata only, never painted-but-inert
    /// (the ui.yaml registry contract; `testing.md` point 10).
    #[tokio::test]
    async fn external_open_trigger_gates_on_av_mode_and_versions() {
        // AV + ask (default) + versions loaded → painted.
        let app = app_with_av_detail_open().await;
        assert!(ids(&app).contains(&"media-external-open-button".to_string()));

        // `always` also paints it.
        let mut app = app_with_av_detail_open().await;
        app.settings.prefs.external_media = ExternalMediaMode::Always;
        assert!(ids(&app).contains(&"media-external-open-button".to_string()));

        // `never` → absent entirely.
        app.settings.prefs.external_media = ExternalMediaMode::Never;
        assert!(!ids(&app).contains(&"media-external-open-button".to_string()));

        // A non-AV item never paints it, whatever the mode.
        let mut app = app_with(vec![item("docs", "docs/notes.txt", 5)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        apply_outcome(&mut app, Outcome::Versions(vec![version(1, 5)]));
        assert!(!ids(&app).contains(&"media-external-open-button".to_string()));

        // Version rows not loaded yet → not painted (nothing to download).
        let mut app = app_with(vec![item("photos", "photos/clip.mp4", 10)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        assert!(!ids(&app).contains(&"media-external-open-button".to_string()));
    }

    /// Under `ask` (the default) the trigger only ARMS the confirm — nothing
    /// runs — and the confirm modal paints with both buttons.
    #[tokio::test]
    async fn external_open_under_ask_arms_the_confirm_without_running() {
        let mut app = app_with_av_detail_open().await;
        assert!(!ids(&app).contains(&"media-external-open-confirm-modal".to_string()));

        assert!(
            apply_local(&mut app, Action::ExternalOpen).is_none(),
            "ask arms; it must not produce the download Op"
        );
        let ids = ids(&app);
        for id in [
            "media-external-open-confirm-modal",
            "media-external-open-confirm-button",
            "media-external-open-cancel-button",
        ] {
            assert!(ids.contains(&id.to_string()), "{id} paints while armed");
        }
    }

    /// The armed confirm's launch downloads by the LATEST version's
    /// `manifest_hash` (rows are oldest→newest; the last row is the current
    /// file — the ui.yaml registry contract) and disarms the modal.
    #[tokio::test]
    async fn confirm_runs_the_download_keyed_by_the_latest_version() {
        let mut app = app_with_av_detail_open().await;
        apply_local(&mut app, Action::ExternalOpen);

        let op = apply_local(&mut app, Action::ConfirmExternalOpen).expect("the download op");
        match op {
            Op::ExternalOpen {
                manifest_hash,
                relative_path,
                name,
                ..
            } => {
                assert_eq!(manifest_hash, "cd".repeat(32), "the LATEST version's hash");
                assert_eq!(relative_path, "photos/clip.mp4");
                assert_eq!(name, "clip.mp4");
            }
            _ => panic!("expected an ExternalOpen op"),
        }
        assert!(
            !ids(&app).contains(&"media-external-open-confirm-modal".to_string()),
            "the confirm disarms once actioned"
        );

        // A confirm click with nothing armed (a stale driver click) is a no-op.
        assert!(apply_local(&mut app, Action::ConfirmExternalOpen).is_none());
    }

    /// Cancel is a PURE no-op: nothing downloads, nothing launches, the detail
    /// surface stays open (the ui.yaml registry contract).
    #[tokio::test]
    async fn cancel_disarms_and_runs_nothing() {
        let mut app = app_with_av_detail_open().await;
        apply_local(&mut app, Action::ExternalOpen);
        assert!(apply_local(&mut app, Action::CancelExternalOpen).is_none());
        let ids = ids(&app);
        assert!(!ids.contains(&"media-external-open-confirm-modal".to_string()));
        assert!(
            ids.contains(&"media-item-detail".to_string()),
            "the detail stays open"
        );
        assert!(!app.errors.contains_key(&crate::pages::Page::Media));
    }

    /// Under `always` the trigger goes straight to the download — no confirm.
    #[tokio::test]
    async fn external_open_under_always_skips_the_confirm() {
        let mut app = app_with_av_detail_open().await;
        app.settings.prefs.external_media = ExternalMediaMode::Always;
        let op = apply_local(&mut app, Action::ExternalOpen).expect("the download op");
        assert!(matches!(op, Op::ExternalOpen { .. }));
        assert!(!ids(&app).contains(&"media-external-open-confirm-modal".to_string()));
    }

    /// Under `never` even a stale ExternalOpen action (the trigger is not
    /// painted) is a quiet no-op — the mode says nothing may launch.
    #[tokio::test]
    async fn external_open_under_never_is_a_no_op() {
        let mut app = app_with_av_detail_open().await;
        app.settings.prefs.external_media = ExternalMediaMode::Never;
        assert!(apply_local(&mut app, Action::ExternalOpen).is_none());
        assert!(!app.media.detail.as_ref().unwrap().external_arming);
    }

    // ── The download (media-item-detail-download-button) ────────────────────

    /// The download paints for ANY item — a non-AV file included, the case the
    /// external handoff never covers — once the version rows carry a manifest,
    /// and never before (painted once a manifest is known, never inert —
    /// `media.md` § Element IDs). The external-open mode does not gate it: a
    /// save-to-disk is not a hand-to-the-player.
    #[tokio::test]
    async fn the_download_paints_for_any_item_once_a_manifest_is_known() {
        let mut app = app_with(vec![item("docs", "docs/report.pdf", 5)]).await;
        apply_local(&mut app, Action::OpenDetail(0));
        assert!(
            !ids(&app).contains(&"media-item-detail-download-button".to_string()),
            "no version rows yet → nothing to download"
        );
        apply_outcome(&mut app, Outcome::Versions(vec![version(1, 5)]));
        assert!(ids(&app).contains(&"media-item-detail-download-button".to_string()));

        let mut app = app_with_av_detail_open().await;
        app.settings.prefs.external_media = ExternalMediaMode::Never;
        assert!(
            ids(&app).contains(&"media-item-detail-download-button".to_string()),
            "the handoff mode gates the handoff, not the download"
        );
    }

    /// The press runs the shared walk keyed by the LATEST version row and saves
    /// under the item's own name — no confirm, nothing launched.
    #[tokio::test]
    async fn the_download_runs_keyed_by_the_latest_version() {
        let mut app = app_with_av_detail_open().await;
        let op = apply_local(&mut app, Action::Download).expect("the download op");
        match op {
            Op::Download {
                source:
                    FetchSource::Own {
                        manifest_hash,
                        relative_path,
                        ..
                    },
                name,
                ..
            } => {
                assert_eq!(manifest_hash, "cd".repeat(32), "the LATEST version's hash");
                assert_eq!(relative_path, "photos/clip.mp4");
                assert_eq!(name, "clip.mp4");
            }
            _ => panic!("expected an own-item Download op"),
        }
        assert!(
            !ids(&app).contains(&"media-external-open-confirm-modal".to_string()),
            "the download arms no confirm"
        );
    }

    /// The save step writes the plaintext into the directory under the item's
    /// last path component, so a name carrying a separator can never steer the
    /// write outside it (linux's `save_file_name`).
    #[test]
    fn the_save_lands_in_the_dir_under_the_items_own_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_download(dir.path(), "../../escape/report.pdf", b"plain").unwrap();
        assert_eq!(path, dir.path().join("report.pdf"));
        assert_eq!(std::fs::read(&path).unwrap(), b"plain");

        // A second press of the same name replaces the earlier copy.
        save_download(dir.path(), "report.pdf", b"newer").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"newer");
    }

    // ── share links (`share-links.md`) ──────────────────────────────────────

    async fn app_with_public_set() -> (App, Arc<FakeMediaNestApi>) {
        let mut app = crate::app::tests::test_app();
        let api = Arc::new(FakeMediaNestApi::new());
        api.set_snapshot(MediaSnapshot {
            items: vec![
                item("site", "site/index.jpg", 10),
                item("docs", "docs/notes.txt", 10),
            ],
            ..Default::default()
        });
        api.set_folders(vec![
            fauna_media_machine::MediaFolder {
                rests_unsealed: true,
                ..set_row(1, "site")
            },
            set_row(2, "docs"),
        ]);
        api.set_versions(vec![version(1, 10)]);
        let machine =
            MediaMachine::new(Arc::new(NullObserver), api.clone(), None, None, None, None);
        machine.set_share_author(vec![5u8; 32], "http://127.0.0.1:1".into());
        machine.refresh(Some(vec![0u8; 32])).await;
        app.media.machine = Some(machine);
        app.media.device_id = Some("de".repeat(32));
        (app, api)
    }

    fn open_detail_of(app: &mut App, path: &str) {
        let index = app
            .media
            .snapshot()
            .unwrap()
            .items
            .iter()
            .position(|i| i.path == path)
            .unwrap();
        apply_local(app, Action::OpenDetail(index));
    }

    #[tokio::test]
    async fn share_link_button_paints_only_on_an_eligible_file() {
        let (mut app, _api) = app_with_public_set().await;
        open_detail_of(&mut app, "site/index.jpg");
        assert!(ids(&app).contains(&"share-link-button".to_string()));
        apply_local(&mut app, Action::CloseDetail);
        open_detail_of(&mut app, "docs/notes.txt");
        assert!(!ids(&app).contains(&"share-link-button".to_string()));
    }

    #[tokio::test]
    async fn a_sealed_files_create_surface_paints_the_key_notice() {
        let mut app = crate::app::tests::test_app();
        let api = Arc::new(FakeMediaNestApi::new());
        api.set_snapshot(MediaSnapshot {
            items: vec![item("vault", "vault/plan.txt", 10)],
            ..Default::default()
        });
        api.set_folders(vec![fauna_media_machine::MediaFolder {
            owner_only: true,
            ..set_row(3, "vault")
        }]);
        let machine =
            MediaMachine::new(Arc::new(NullObserver), api.clone(), None, None, None, None);
        machine.set_share_author(vec![5u8; 32], "http://127.0.0.1:1".into());
        machine.refresh(Some(vec![0u8; 32])).await;
        app.media.machine = Some(machine);
        open_detail_of(&mut app, "vault/plan.txt");
        assert!(ids(&app).contains(&"share-link-button".to_string()));
        apply_local(&mut app, Action::OpenShareCreate);
        let notice = elements(&app)
            .into_iter()
            .find(|e| e.id == "share-link-key-notice")
            .expect("the key notice paints on a sealed file");
        assert_eq!(notice.text, sl::KEY_NOTICE);
    }

    #[tokio::test]
    async fn the_url_paints_only_after_the_create_returns() {
        let (mut app, api) = app_with_public_set().await;
        open_detail_of(&mut app, "site/index.jpg");
        apply_local(&mut app, Action::OpenShareCreate);
        let before = ids(&app);
        for id in [
            "share-link-create-modal",
            "share-link-expiry-select",
            "share-link-create-button",
            "share-link-cancel-button",
        ] {
            assert!(before.contains(&id.to_string()), "{id} painted");
        }
        assert!(!before.contains(&"share-link-url".to_string()));
        assert!(!before.contains(&"share-link-copy-button".to_string()));
        assert!(
            !before.contains(&"share-link-key-notice".to_string()),
            "a public file's link carries no key"
        );

        let op = apply_local(&mut app, Action::CreateShareLink).expect("a create op");
        apply_outcome(&mut app, op.run().await);
        let after = ids(&app);
        assert!(after.contains(&"share-link-url".to_string()));
        assert!(after.contains(&"share-link-copy-button".to_string()));
        assert!(!after.contains(&"share-link-create-button".to_string()));
        assert_eq!(api.shares().len(), 1);
    }

    #[tokio::test]
    async fn the_list_paints_three_states_and_revoke_through_its_confirm() {
        let (mut app, _api) = app_with_public_set().await;
        // Empty once loaded.
        let op = apply_local(&mut app, Action::OpenShareLinks).expect("a list op");
        apply_outcome(&mut app, op.run().await);
        let ids_empty = ids(&app);
        assert!(ids_empty.contains(&"share-link-list".to_string()));
        assert!(ids_empty.contains(&"share-link-empty-state".to_string()));
        assert!(!ids_empty.contains(&"media-view-toggle".to_string()));
        apply_local(&mut app, Action::CloseShareLinks);

        // Make a link, then list it.
        open_detail_of(&mut app, "site/index.jpg");
        apply_local(&mut app, Action::OpenShareCreate);
        let op = apply_local(&mut app, Action::CreateShareLink).unwrap();
        apply_outcome(&mut app, op.run().await);
        apply_local(&mut app, Action::CloseDetail);
        let op = apply_local(&mut app, Action::OpenShareLinks).unwrap();
        apply_outcome(&mut app, op.run().await);
        let els = elements(&app);
        let name = els
            .iter()
            .find(|e| e.id == "share-link-item-name")
            .expect("row name");
        assert_eq!(name.text, "index.jpg");
        let state = els
            .iter()
            .find(|e| e.id == "share-link-item-state")
            .expect("row state");
        assert_eq!(
            state
                .attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.as_str()),
            Some("active")
        );
        assert!(els.iter().any(|e| e.id == "share-link-item-copy-button"));
        assert!(!els.iter().any(|e| e.id == "share-link-empty-state"));

        apply_local(&mut app, Action::ArmShareRevoke(0));
        assert!(ids(&app).contains(&"share-link-revoke-confirm-modal".to_string()));
        let op = apply_local(&mut app, Action::ConfirmShareRevoke).unwrap();
        apply_outcome(&mut app, op.run().await);
        let els = elements(&app);
        let state = els
            .iter()
            .find(|e| e.id == "share-link-item-state")
            .unwrap();
        assert_eq!(
            state
                .attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.as_str()),
            Some("revoked")
        );
        assert!(!els.iter().any(|e| e.id == "share-link-revoke-button"));
        assert!(!els.iter().any(|e| e.id == "share-link-item-copy-button"));
    }

    // ── The offline gate's media declarations (W4 phase 4, row 43) ──────────

    /// One instance of **every** [`Action`] variant — the admin leg's corpus
    /// shape, and here for the same reason: walk invariants I6/I7 check only
    /// what a page *paints*, and this page paints from a `MediaMachine`
    /// snapshot that never loads offline, so the walks reach almost none of
    /// these. `MEDIA_ACTION_COUNT` is the ratchet that keeps a new variant from
    /// slipping past the registry check below.
    fn every_media_action() -> Vec<Action> {
        vec![
            Action::ToggleView,
            Action::SetSort("name".to_string()),
            Action::SetSortDirection("asc".to_string()),
            Action::SetFilter("set".to_string()),
            Action::Upload,
            Action::OpenDetail(0),
            Action::OpenFile {
                folder_id: 1,
                path_hash: "abc".to_string(),
            },
            Action::CloseDetail,
            Action::ArmRestore(0),
            Action::ConfirmRestore,
            Action::CancelRestore,
            Action::ArmDelete,
            Action::ConfirmDelete,
            Action::CancelDelete,
            Action::ExternalOpen,
            Action::ConfirmExternalOpen,
            Action::CancelExternalOpen,
            Action::Download,
            Action::TogglePruned,
            Action::Undelete(0),
            Action::OpenShareCreate,
            Action::SetShareExpiry("7d".to_string()),
            Action::CreateShareLink,
            Action::CloseShareCreate,
            Action::CopyShareUrl,
            Action::OpenShareLinks,
            Action::CloseShareLinks,
            Action::CopyShareLinkRow(0),
            Action::ArmShareRevoke(0),
            Action::ConfirmShareRevoke,
            Action::CancelShareRevoke,
        ]
    }

    const MEDIA_ACTION_COUNT: usize = 31;

    #[test]
    fn every_media_action_is_in_the_corpus() {
        assert_eq!(
            every_media_action().len(),
            MEDIA_ACTION_COUNT,
            "a new `Action` variant must be added to `every_media_action` — \
             otherwise its wire-kind declaration is never checked"
        );
    }

    /// I7 at the type level: an unregistered kind reads as `Available` by
    /// design, so a typo here would silently ungate the affordance forever.
    #[test]
    fn every_declared_media_kind_is_registered() {
        crate::test_support::assert_every_wire_kind_is_registered(every_media_action(), |a| {
            a.wire_kind()
        });
    }

    /// The finding this leg records: media's three writes are `OfflineSafe`, so
    /// **nothing on this page greys out**. Upload, delete and restore each
    /// record one sync-change row — the content-addressed, replayable write
    /// class 1 describes, and precisely what the W4 outbox exists to carry.
    /// Gating them would be the over-claim rulings 1–3 forbid.
    #[test]
    fn media_writes_are_offline_safe_and_stay_live() {
        use fauna_protocol::offline_class::{OfflineClass, affordance, offline_class};
        for action in [
            Action::Upload,
            Action::ConfirmDelete,
            Action::ConfirmRestore,
        ] {
            assert_eq!(
                action.wire_kind(),
                Some("fauna.sync.changes.record"),
                "{action:?} records a sync-change row"
            );
            assert_eq!(
                offline_class("fauna.sync.changes.record"),
                Some(OfflineClass::OfflineSafe),
            );
            assert!(
                affordance("fauna.sync.changes.record", "disconnected").is_available(),
                "{action:?} must stay actuable with no nest"
            );
        }
    }

    /// Share-link create and revoke are `OnlineOnly` (`share-links.md` § Flows
    /// → Create: the URL is revealed only after registration succeeds), so the
    /// gate greys them out with no nest; the list is a read.
    #[test]
    fn share_link_create_and_revoke_are_online_only() {
        use fauna_protocol::offline_class::affordance;
        for (action, kind) in [
            (Action::CreateShareLink, "fauna.share.create"),
            (Action::ConfirmShareRevoke, "fauna.share.revoke"),
        ] {
            assert_eq!(action.wire_kind(), Some(kind));
            assert!(
                !affordance(kind, "disconnected").is_available(),
                "{action:?} must desensitize offline"
            );
        }
        assert_eq!(Action::OpenShareLinks.wire_kind(), Some("fauna.share.list"));
    }

    /// Opening the detail reads that file's version rows. `Read`, so the gate
    /// declines to decide — but the kind is recorded, which is the whole point
    /// of declaring a read (the search-page precedent).
    #[test]
    fn opening_the_detail_declares_the_version_read() {
        for action in [
            Action::OpenDetail(3),
            Action::OpenFile {
                folder_id: 7,
                path_hash: "deadbeef".to_string(),
            },
        ] {
            assert_eq!(action.wire_kind(), Some("fauna.files.versions.list"));
        }
    }

    /// The external handoff moves real bytes, but over the bulk-binary blob
    /// carve-out rather than WS-RPC — so there is no kind for the table to
    /// classify. Declaring one would be inventing a claim; this pins the
    /// `None` so a later reader does not "fix" it into a guess.
    #[test]
    fn the_external_handoff_has_no_wire_kind_to_declare() {
        assert_eq!(Action::ExternalOpen.wire_kind(), None);
        assert_eq!(Action::ConfirmExternalOpen.wire_kind(), None);
        assert_eq!(Action::Download.wire_kind(), None);
    }

    /// View state and confirm-arming issue nothing; the mutation is the
    /// confirm's own gesture.
    #[test]
    fn media_view_state_and_arming_declare_nothing() {
        for action in [
            Action::ToggleView,
            Action::SetSort("size".to_string()),
            Action::SetFilter("s".to_string()),
            Action::ArmDelete,
            Action::ArmRestore(0),
            Action::CancelExternalOpen,
        ] {
            assert_eq!(action.wire_kind(), None, "{action:?} issues nothing");
        }
    }

    // ── The device-identity seam (`devices.md` § This-device marker) ─────────
    //
    // One value must do two jobs on a conformant client: the id the app
    // REGISTERS with is the id the marker COMPARES against. These pin the
    // adopt→read round trip that makes that true when the e2e session door
    // hands the app a well-known id. Before it existed the patch's id reached
    // only the accounts registry while `device.db` kept a random one, and
    // `test_device_card_marks_this_device`'s documented premise was false.
    //
    // And the id is PER ACCOUNT (`sync-agent-credentials.md` § Credential
    // model, the 2026-09-20 ruling): every test drives the flat-base seam with
    // an actor, never the process env.

    const ACTOR_A: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
    const ACTOR_B: &str = "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22";

    /// The whole point: what the session door adopts is what every later reader
    /// of this account's sync identity sees — read back through
    /// [`device_id_hex_under`], the seam the sync agent registers with and the
    /// marker compares against.
    #[test]
    fn an_adopted_device_id_is_what_the_store_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let flat = dir.path().join("fauna-tui");
        let well_known = "0123456789abcdef".repeat(4);

        assert!(
            adopt_device_id_under(&flat, ACTOR_A, &well_known),
            "adopting a valid 32-byte hex id should succeed"
        );

        assert_eq!(
            device_id_hex_under(&flat, ACTOR_A).as_deref(),
            Some(well_known.as_str()),
            "the adopted id must be the one a later read returns — otherwise the \
             app registers as one device and marks another"
        );
    }

    /// Adoption REPLACES a previously-resolved id rather than losing to it. The
    /// ordering this protects is real: `apply_session_patch` adopts before
    /// `establish` starts the sync agent, but anything that touched the store
    /// first (a Media read, a relaunch) has already derived one, and the
    /// get-or-create would happily keep it.
    #[test]
    fn adopting_replaces_an_already_resolved_device_id() {
        let dir = tempfile::tempdir().unwrap();
        let flat = dir.path().join("fauna-tui");

        let derived = device_id_hex_under(&flat, ACTOR_A).expect("the get-or-create resolves");
        let well_known = "0123456789abcdef".repeat(4);
        assert_ne!(
            derived, well_known,
            "the derivation must not collide by luck"
        );

        assert!(adopt_device_id_under(&flat, ACTOR_A, &well_known));

        assert_eq!(
            device_id_hex_under(&flat, ACTOR_A).as_deref(),
            Some(well_known.as_str()),
            "adoption must win over an earlier derivation"
        );
    }

    /// A malformed id — or a malformed ACTOR, which would otherwise resolve to
    /// the flat base and write a machine-flat store again — is REFUSED, not
    /// silently stored or half-written. The caller warns and carries on: a bad
    /// patch value must leave the app's real identity intact.
    #[test]
    fn a_malformed_device_id_or_actor_is_refused_and_leaves_the_store_alone() {
        let dir = tempfile::tempdir().unwrap();
        let flat = dir.path().join("fauna-tui");
        let resolved = device_id_hex_under(&flat, ACTOR_A).expect("the get-or-create resolves");

        for bad in ["", "not-hex", "abcd", &"ab".repeat(33)] {
            assert!(
                !adopt_device_id_under(&flat, ACTOR_A, bad),
                "{bad:?} is not a 32-byte hex id and must be refused"
            );
        }
        let well_known = "0123456789abcdef".repeat(4);
        assert!(
            !adopt_device_id_under(&flat, "not-an-actor", &well_known),
            "a malformed actor id must be refused, never written to the flat base"
        );
        assert!(
            !flat.join("device.db").exists(),
            "nothing may land in the flat base itself"
        );

        assert_eq!(
            device_id_hex_under(&flat, ACTOR_A),
            Some(resolved),
            "a refused adoption must not disturb the store"
        );
    }

    /// A missing config dir does not lose the adopt — the real first-login
    /// shape, where nothing has opened the flat base or the actor's scope yet
    /// (the scope resolver creates the scope dir).
    #[test]
    fn adopting_creates_the_store_dir_when_it_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let flat = dir.path().join("never-created").join("fauna-tui");
        assert!(!flat.exists(), "precondition: the dir must be absent");

        let well_known = "0123456789abcdef".repeat(4);
        assert!(
            adopt_device_id_under(&flat, ACTOR_A, &well_known),
            "a first login must not lose the adopt to a missing dir"
        );
        assert_eq!(
            device_id_hex_under(&flat, ACTOR_A).as_deref(),
            Some(well_known.as_str())
        );
    }

    /// Property (2) of the ruling: two accounts signed in on one install
    /// register under DIFFERENT ids, each stable across reads — so a nest
    /// serving both learns no machine linkage between them. The pre-rule tui
    /// read one machine-flat `sync/device.db` for every account and handed
    /// both the same id.
    #[test]
    fn two_accounts_on_one_install_register_under_different_device_ids() {
        let dir = tempfile::tempdir().unwrap();
        let flat = dir.path().join("fauna-tui");

        let a = device_id_hex_under(&flat, ACTOR_A).expect("a resolves");
        let b = device_id_hex_under(&flat, ACTOR_B).expect("b resolves");
        assert_ne!(
            a, b,
            "two accounts on one install must never share a device id"
        );

        assert_eq!(device_id_hex_under(&flat, ACTOR_A), Some(a), "a is stable");
        assert_eq!(device_id_hex_under(&flat, ACTOR_B), Some(b), "b is stable");
        assert!(
            !flat.join("sync").join("device.db").exists(),
            "no machine-flat id store is minted any more"
        );
    }

    /// The session door's forced id lands in ITS account's scope only: a
    /// second account signing in afterwards must not inherit it.
    #[test]
    fn a_forced_device_id_stays_in_its_own_accounts_scope() {
        let dir = tempfile::tempdir().unwrap();
        let flat = dir.path().join("fauna-tui");
        let well_known = "0123456789abcdef".repeat(4);

        assert!(adopt_device_id_under(&flat, ACTOR_A, &well_known));

        assert_ne!(
            device_id_hex_under(&flat, ACTOR_B).as_deref(),
            Some(well_known.as_str()),
            "another account must not read the id forced for the first"
        );
    }

    /// A machine-flat `sync/device.db` is never read: its first-adopter adoption
    /// was retired by the compat-remnant sweep (`version-compatibility.md`
    /// § Dimension 2), so no account — the first to sign in included — takes
    /// its id, and the flat file is left untouched.
    #[test]
    fn a_machine_flat_device_id_is_never_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let flat = dir.path().join("fauna-tui");
        let legacy_dir = flat.join("sync");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy = {
            let db = fauna_sync_engine::db::SyncDb::open(legacy_dir.join("device.db")).unwrap();
            fauna_core::hex32::encode(&db.get_or_create_device_id().unwrap())
        };

        for actor in [ACTOR_A, ACTOR_B] {
            let id = device_id_hex_under(&flat, actor);
            assert!(id.is_some(), "every account still resolves an id");
            assert_ne!(
                id.as_deref(),
                Some(legacy.as_str()),
                "the flat id is never adopted"
            );
        }
        assert!(
            legacy_dir.join("device.db").exists(),
            "the flat file is not the app's to touch"
        );
    }

    // ── The followed browse scope (`media.md` § Followed public folders) ────

    struct FakeFollowedSource {
        scopes: Vec<fauna_media_machine::FollowedMediaScope>,
        entries: Vec<fauna_media_machine::FollowedFileEntry>,
    }

    #[async_trait::async_trait]
    impl fauna_media_machine::FollowedMediaSource for FakeFollowedSource {
        async fn followed_scopes(&self) -> Vec<fauna_media_machine::FollowedMediaScope> {
            self.scopes.clone()
        }
        async fn fetch_listing(
            &self,
            _folder_id: i64,
            _home_nest_url: &str,
        ) -> Result<
            Vec<fauna_media_machine::FollowedFileEntry>,
            fauna_media_machine::FollowedFetchError,
        > {
            Ok(self.entries.clone())
        }
    }

    /// `app_with` plus a wired followed source holding one scope + one entry,
    /// refreshed so the option is offered.
    async fn app_with_followed(entry_path: &str) -> (App, String) {
        let app = app_with(vec![item("docs", "docs/notes.txt", 30)]).await;
        let machine = app.media.machine.as_ref().unwrap();
        machine.set_followed_media_source(Arc::new(FakeFollowedSource {
            scopes: vec![fauna_media_machine::FollowedMediaScope {
                folder_id: 7,
                home_nest_url: "https://home.example".into(),
                owner_actor_id: "aabbccdd00112233".into(),
                display_name: "their-photos".into(),
                available: true,
                ..Default::default()
            }],
            entries: vec![fauna_media_machine::FollowedFileEntry {
                path: entry_path.into(),
                path_hash: format!("hash-{entry_path}"),
                manifest_hash: "ab".repeat(32),
                size_bytes: 42,
                updated_at: 1_700_000_000,
                thumbnail_hash: None,
                seq: 1,
            }],
        }));
        machine.refresh(Some(vec![0u8; 32])).await;
        let value = machine.snapshot().followed[0].value.clone();
        (app, value)
    }

    /// The followed option rides the existing `media-folder-filter` select —
    /// its opaque value among the options, its label painted only once active —
    /// and entering the scope swaps the browse to the on-demand listing while
    /// the upload affordance (a read-only scope's non-action) leaves entirely.
    #[tokio::test]
    async fn a_followed_scope_rides_the_filter_and_browses_read_only() {
        let (mut app, value) = app_with_followed("a.jpg").await;

        let select = elements(&app)
            .into_iter()
            .find(|e| e.id == "media-folder-filter")
            .expect("the filter select renders");
        let crate::element::Role::Select { options, .. } = &select.role else {
            panic!("media-folder-filter is a select");
        };
        assert!(options.contains(&value), "the followed value is an option");

        // Selecting the followed value routes to the async scope entry, not the
        // sync name setter.
        let op = apply_local(&mut app, Action::SetFilter(value.clone()))
            .expect("a followed value produces the network half");
        assert!(matches!(op, Op::SelectFollowedScope { .. }));
        op.run().await;

        let ids = ids(&app);
        assert!(
            !ids.contains(&"file-upload".to_string())
                && !ids.contains(&"upload-button".to_string()),
            "a read-only scope offers no upload affordance"
        );
        let snap = app.media.machine.as_ref().unwrap().snapshot();
        assert_eq!(snap.items.len(), 1, "the followed listing renders");
        assert_eq!(snap.items[0].path, "a.jpg");
        let select = elements(&app)
            .into_iter()
            .find(|e| e.id == "media-folder-filter")
            .unwrap();
        assert_eq!(select.text, value, "the select carries the scope value");
    }

    /// A followed item's detail is read-only: no delete, no version history —
    /// and its one action, the external handoff, resolves through the keyless
    /// scope download rather than a version row's manifest.
    #[tokio::test]
    async fn a_followed_detail_is_read_only_with_a_keyless_handoff() {
        let (mut app, value) = app_with_followed("clip.mp4").await;
        apply_local(&mut app, Action::SetFilter(value.clone()))
            .unwrap()
            .run()
            .await;

        let op = apply_local(&mut app, Action::OpenDetail(0));
        assert!(op.is_none(), "a followed detail has no versions to load");
        let detail = app.media.detail.as_ref().expect("the detail opened");
        assert_eq!(detail.followed_scope_value.as_deref(), Some(value.as_str()));

        let ids = ids(&app);
        assert!(
            !ids.contains(&"media-delete-button".to_string()),
            "no delete on a folder the user holds no seat on"
        );
        assert!(
            !ids.contains(&"file-version-list".to_string()),
            "the public plane is head-only — no version history"
        );
        assert!(
            ids.contains(&"media-external-open-button".to_string()),
            "the handoff trigger is the read affordance, and it needs no version rows"
        );

        let op = external_open_op(&app).expect("the handoff produces its op");
        assert!(matches!(op, Op::FollowedExternalOpen { .. }));

        // The download is the followed detail's other read (`media.md` §
        // Followed public folders: "offers download alone"), keyless too.
        assert!(ids.contains(&"media-item-detail-download-button".to_string()));
        let op = apply_local(&mut app, Action::Download).expect("the download op");
        assert!(matches!(
            op,
            Op::Download {
                source: FetchSource::Followed { .. },
                ..
            }
        ));
    }
}
