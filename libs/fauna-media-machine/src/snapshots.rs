//! The page-level renderable `MediaPageSnapshot` + its item record. Clients read
//! a fresh copy on every observer tick and render the whole Media page off it;
//! they never see the internal state. Mirrors `fauna_devices_machine::snapshots`.
//!
//! `MediaItemSummary` transcribes the cross-set `fauna.media.list` item
//! (`fauna_protocol::media::MediaItem`, re-exported by `fauna-client-media`) into
//! a clean `uniffi::Record` — dropping the wire type's `extra` flatten map so it
//! crosses the FFI boundary — and adds the shared displayed `name`
//! ([`fauna_client_media::display_name`]) so every app shows the same name and
//! the `media-sort-select` name sort agrees with what's rendered.

use serde::{Deserialize, Serialize};

use fauna_client_media::{display_name, media::MediaItem};
use fauna_core::localized::LocalizedText;

/// One item in the cross-set all-media browse — the `media-item` component. A
/// transcription of `fauna_protocol::media::MediaItem` (drops the wire `extra`
/// map; adds the derived `name`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MediaItemSummary {
    /// Name of the readable folder this item belongs to (the
    /// `media-folder-filter` key).
    pub folder: String,
    /// Folder-relative path, forward-slash normalized.
    pub path: String,
    /// The displayed file name (`media-item-name`) — the path's basename, derived
    /// once in shared Rust so every app renders the same name.
    pub name: String,
    /// Stored ciphertext size in bytes (`media-item-size`).
    pub size_bytes: i64,
    /// Last-change timestamp, unix seconds (`media-item-date`).
    pub updated_at: i64,
    /// Hex thumbnail-blob hash (`media-thumbnail` `?thumb=1` routing pointer),
    /// when the uploader recorded one. `None` for folder files until the
    /// uploader↔manifest thumbnail association lands (`media.md` § Impl status).
    pub thumbnail_hash: Option<String>,
    /// Whether the backing folder's source device is reachable
    /// (`media-source-status` online/offline dot). Distinct from a file's
    /// sync-state badge.
    pub source_online: bool,
    /// Whether "Share a link" (`share-link-button`) is offered on this item's
    /// detail surface — `fauna_client_share::share_link_eligible` over the
    /// item's folder, decided in shared Rust so the control's presence is
    /// state, never per-app logic (`share-links.md` § Which files can be
    /// linked). `false` for a followed-scope item (not the caller's file).
    pub share_link_eligible: bool,
}

impl From<MediaItem> for MediaItemSummary {
    fn from(it: MediaItem) -> Self {
        let name = display_name(&it).to_string();
        Self {
            folder: it.folder,
            path: it.path,
            name,
            size_bytes: it.size_bytes,
            updated_at: it.updated_at,
            thumbnail_hash: it.thumbnail_hash,
            source_online: it.source_online,
            // Stamped by the machine, which holds the folder facts.
            share_link_eligible: false,
        }
    }
}

/// The open share-link create surface (`share-link-create-modal`,
/// `share-links.md` § Flows → Create).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ShareCreateSnapshot {
    /// The file's displayed name.
    pub name: String,
    /// The chosen `share-link-expiry-select` value (one of
    /// [`MediaPageSnapshot::share_expiry_options`]).
    pub expiry: String,
    /// A create is in flight — the create control is inert until it returns.
    pub busy: bool,
    /// The link's URL (`share-link-url` / `share-link-copy-button`) — `Some`
    /// ONLY after the registration succeeded (the reveal-after-registration
    /// rule); once set, the surface shows the URL and no create control.
    pub url: Option<String>,
    /// The file rests sealed, so the link is fragment-keyed: its URL carries
    /// the key after `#`. Paints `share-link-key-notice`, the honest limits
    /// the author is told at create time (`share-links.md` § The private-file
    /// extension) — absent on a public-folder file.
    pub key_in_fragment: bool,
}

/// One row of the share-link list (`share-link-item`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ShareLinkSummary {
    /// The registry id (hex) — the revoke key.
    pub token_id: String,
    /// The file's name, opened seal-first (`share-link-item-name`).
    pub name: String,
    /// Expiry, Unix seconds (`share-link-item-expires`).
    pub expires_at: i64,
    /// `"active"` / `"expired"` / `"revoked"` (`share-link-item-state`).
    pub state: String,
    /// The verified re-derived URL — the `share-link-item-copy-button`'s
    /// presence and payload; `None` hides the control. Active rows only.
    pub url: Option<String>,
}

/// The share-link list surface (`share-link-list`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ShareLinksSnapshot {
    /// The list surface is open.
    pub open: bool,
    /// The list has loaded since it opened — the three-state rule
    /// (`ui/README.md` § List pages: loading is not empty): `share-link-empty-state`
    /// paints only when `loaded` and `rows` is empty.
    pub loaded: bool,
    /// Newest first.
    pub rows: Vec<ShareLinkSummary>,
    /// The token id whose revoke confirm (`share-link-revoke-confirm-modal`)
    /// is armed, if any.
    pub revoke_confirm: Option<String>,
}

/// One followed public folder, as the `media-folder-filter` offers it — a
/// browse **scope**, not a set in the aggregate
/// (`docs/goal/ui/media.md` § Followed public folders).
///
/// Apps append these after the own-set name options and hand the chosen
/// `value` back to `select_followed_scope`; the value↔identity mapping lives
/// in the machine, so no app composes or parses an address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FollowedScopeOption {
    /// The machine-minted opaque select value. Stable per follow, disjoint
    /// from every set name by construction; hand it back verbatim, never
    /// parse it.
    pub value: String,
    /// The display label: the follow's name plus a short owner
    /// disambiguator, so a follow named like one of the user's own sets
    /// ("photos" following "photos") stays tellable apart at the label level
    /// — the routing itself never keys on names.
    pub label: String,
    /// Whether the home nest still served this folder at the last verdict.
    /// `false` is the loud *no longer available* rendering; the option stays
    /// offered (it is the revoke, and a re-flip resumes it).
    pub available: bool,
}

/// One version of a synced file — a `file-version-item` row in the
/// `file-version-history` component (`media-item-detail` surface, media.md
/// § Element IDs; semantics `file-sync.md` § File Versions). A transcription of
/// `fauna_protocol::files::FileVersionInfo` into a clean FFI record: byte
/// hashes become hex strings, the wire `extra` map is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FileVersionSummary {
    /// The version's stable identity — the recording `sync_changes` row's `seq`
    /// (never renumbered). Display ordinals come from list position.
    pub version_num: i64,
    /// Hex manifest hash; a restore re-points the file at this
    /// (`file-sync.md` § Restore).
    pub manifest_hash: String,
    /// Logical size in bytes (`file-version-size`).
    pub size_bytes: i64,
    /// Recorded-at, epoch millis (`file-version-timestamp`).
    pub created_at: i64,
    /// The M2 content-key generation the version's chunks were sealed under —
    /// carried verbatim on restore so readers select `key_for(version)`.
    /// `None` for owner-only sets.
    pub content_key_version: Option<u64>,
    /// Pre-computed "who wrote this" label (`file-version-author`) — the
    /// nest-stamped recorder's handle, else the `short_id` of their actor id
    /// (`fauna_core::format::account_display_label`; the one derivation site,
    /// multi-writer Phase 1 attribution). Every version has one.
    pub author_display: String,
    /// This version is **soft-pruned** (`file-versions.md` § Retention (3)):
    /// out of the live listing, recoverable via `undelete_version` until
    /// [`Self::purge_after`]. Only an `include_pruned` listing carries pruned
    /// rows, so a live-only browse never renders the badge. Wire `Some(true)`
    /// folds to `true` at the transcribe.
    pub pruned: bool,
    /// Epoch seconds after which the purge may run — the recovery deadline the
    /// `file-version-pruned-badge` renders. `None` on a live row.
    pub purge_after: Option<i64>,
}

/// The `media-folder-filter` picker's raw value standing for the all-media
/// default (as opposed to a real set name) — [`MediaPageSnapshot::filter`]
/// itself models "all" as `None`, but a raw-value picker widget (the
/// `SelectTarget::ReminderOffset` shape) needs a *string* sentinel for that
/// option. `__`-prefixed so it can't collide with a real set name (reserved
/// `__*` sets are excluded from `fauna.media.list`, `media.md` § O-4).
/// tui's and linux's own `FILTER_ALL_VALUE` each hand-copied this exact
/// literal — collapsed here (web stays its own TypeScript copy; the e2e
/// action layer's `MEDIA_FILTER_ALL` mirrors it in Python for the same
/// cross-language reason).
pub const FILTER_ALL_VALUE: &str = "__all__";

/// One `media-folder-filter` option with the set's identity beside its label
/// (`fauna_client_media::MediaFolderOption`, rendered for the apps).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MediaFolderOption {
    /// The set name — the filter key.
    pub name: String,
    /// The set's `FolderRef` wire string (`local:<id>`), the key every
    /// per-set local read takes (`FfiSyncEngineHost::file_states`,
    /// `sync_file_states`); `None` for a set known only from its items (a
    /// shared-with-me set the caller's own list does not carry), which this
    /// device hosts nowhere.
    pub folder_id: Option<String>,
}

/// The whole renderable Media page in one record — the content-plane analogue of
/// `DevicesSnapshot`. A single observer tick fully describes the page: the
/// filtered + sorted item list plus the client-held view state. See
/// `docs/goal/ui/media.md` § State & data shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MediaPageSnapshot {
    /// The cross-set media for the active filter, sorted by the active key — the
    /// `media-item` rows/tiles to render. Already filtered + sorted in shared Rust
    /// (`fauna_client_media::MediaSnapshot::view`).
    pub items: Vec<MediaItemSummary>,
    /// The `media-folder-filter` options (alongside the all-media default) —
    /// the caller's browsable folders, **including ones holding no media yet**
    /// (`fauna_client_media::MediaSnapshot::folders`). Independent of the
    /// active filter.
    pub folders: Vec<String>,
    /// [`folders`](Self::folders) with each set's identity beside its label —
    /// same entries, same order ([`MediaFolderOption`]). What an app keys a
    /// **local-state read** on: the per-file sync badges read the set's
    /// `fsid-<ref>.db` by this id, never by the name.
    pub folder_options: Vec<MediaFolderOption>,
    /// Followed public folders offered as browse **scopes** in
    /// `media-folder-filter`, after the own-set options
    /// (`media.md` § Followed public folders). Empty until the platform wires
    /// [`crate::machine::MediaMachine::set_followed_media_source`] — the
    /// correct render for an app that has not built the surface.
    pub followed: Vec<FollowedScopeOption>,
    /// `Some` while a followed browse scope is active: `items` then carries
    /// that folder's listing (fetched on demand from its home nest) and
    /// `filter` carries the option's `value`. Apps route an item tap to
    /// `download_followed` while this is set — and never offer upload, delete,
    /// restore, or version history (the scope is read-only; public plane is
    /// head-only).
    pub followed_scope: Option<FollowedScopeOption>,
    /// The active `media-sort-select` value (`"name"` / `"size"` / `"date"`).
    pub sort: String,
    /// Whether the active sort is descending (the desktop column-header toggle).
    pub descending: bool,
    /// The active `media-folder-filter`: a set name, or `None` for the all-media
    /// default view.
    pub filter: Option<String>,
    /// The active `media-view-toggle`: `true` = thumbnail grid, `false` = list.
    pub view_grid: bool,
    /// The page-level `error-message` (last refresh failure), localized
    /// client-side; `None` when clear.
    pub error: Option<LocalizedText>,
    /// Whether a `refresh()` has ever **returned successfully** — the page's
    /// loaded-vs-still-loading bit, and the second painting condition of the
    /// `media-empty-state` element (`media.md` § Default view: cross-set
    /// all-media).
    ///
    /// `items` alone cannot answer "is there no media?": it is empty both before
    /// the first read returns and after one that found nothing, so every app
    /// painted "No media yet" over a page that was merely still loading, and the
    /// multiseat harness could not tell an empty set from an unloaded one
    /// (`tests/e2e-unified/helpers/multiseat_config.py::settle_listing`). The
    /// three renderable states are therefore:
    ///
    /// | `loaded` | `items` | render |
    /// |---|---|---|
    /// | `false` | (any) | still loading — no rows, and **no** `media-empty-state` |
    /// | `true` | empty | the genuine empty state — paint `media-empty-state` |
    /// | `true` | non-empty | the `media-item` rows |
    ///
    /// **Monotonic**: set on the first successful refresh and never cleared. A
    /// *failed* refresh leaves it as it was — a first-read failure keeps the page
    /// unloaded (`error-message` does the talking; claiming "No media yet" beside
    /// an error would be a lie), and a later failure keeps prior data on screen,
    /// so re-arming the loading state under visible rows would be one too.
    pub loaded: bool,
    /// The share-link create surface, while open.
    pub share_create: Option<ShareCreateSnapshot>,
    /// The `share-link-expiry-select` values, shortest first
    /// (`fauna_client_share::EXPIRY_OPTIONS`); apps label each by value.
    pub share_expiry_options: Vec<String>,
    /// The share-link list surface.
    pub share_links: ShareLinksSnapshot,
}
