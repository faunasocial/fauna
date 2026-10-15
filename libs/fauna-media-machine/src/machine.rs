//! The page-level Media state machine.
//!
//! Mirrors `fauna_devices_machine::DevicesMachine`: each app holds an
//! `Arc<MediaMachine>`, observes via a registered `MediaObserver`, drives
//! gestures, and renders the whole Media page off `snapshot()`. It owns the
//! *content plane* (`media.md` rule 4 — browse + upload/delete media, never
//! *configure* folders): the cross-set all-media list, the client-held view
//! state (`media-sort-select` / `media-folder-filter` / `media-view-toggle`),
//! and the `upload()` / `delete()` content gestures.
//!
//! Uses `std::sync::Mutex` (not tokio's) so getters and the sync view-state
//! setters work from any thread context — including UI threads and
//! `#[tokio::test]`. The async `refresh()` clones the seam handle, drops the
//! lock, does IO, then re-acquires; the lock is never held across an `await`.

use std::sync::{Arc, Mutex};

use fauna_client_media::media::MediaItem;
use fauna_client_media::{FolderFilter, MediaSnapshot, MediaSortKey};
use fauna_core::crypto::{BackupKey, decrypt_backup_chunk};
use fauna_core::file_download::{FileDownloadKeys, PredecessorSealKey, RecordSigner};
use fauna_core::followed_media::{FollowedFetchError, FollowedMediaScope, FollowedMediaSource};
use fauna_core::localized::LocalizedText;
use fauna_core::path_crypto;
use fauna_media::audience::Audience;

use crate::blob_fetcher::MediaBlobFetcher;
use crate::blob_uploader::MediaBlobUploader;
use crate::nest_api::{MediaApiError, MediaNestApi};
use crate::observer::MediaObserver;
use crate::snapshots::{
    FileVersionSummary, FollowedScopeOption, MediaFolderOption, MediaItemSummary,
    MediaPageSnapshot, ShareCreateSnapshot, ShareLinkSummary, ShareLinksSnapshot,
};
pub use fauna_client_media::member_rows::MlsQuery;

/// i18n key for a cross-set read (`refresh()`) failure ("Failed to load media: {message}").
const REFRESH_ERROR_KEY: &str = "media.error_refresh";
/// i18n key for an `upload()` failure ("Failed to upload: {message}").
const UPLOAD_ERROR_KEY: &str = "media.error_upload";
/// i18n key for a `delete()` failure ("Failed to delete: {message}").
const DELETE_ERROR_KEY: &str = "media.error_delete";
/// i18n key for a failed share-link create ("Couldn't create the link:
/// {message}") — the surface stays open for a retry and no URL is shown
/// (`share-links.md` § Flows → Create, step 4).
const SHARE_CREATE_ERROR_KEY: &str = "share_link.error_create";
/// i18n key for a failed share-link list read ("Couldn't load your shared
/// links: {message}").
const SHARE_LIST_ERROR_KEY: &str = "share_link.error_list";
/// i18n key for a failed revoke ("Couldn't revoke the link: {message}").
const SHARE_REVOKE_ERROR_KEY: &str = "share_link.error_revoke";
/// i18n key for an `upload_selected()` with no set to target because the
/// caller has **no folders at all** — the create-one-first copy. No
/// `{message}` arg — a pure client-side condition. An *empty* set is a valid
/// target and never lands here — every own folder is an upload target
/// (`media.md` § Layout & flow).
const NO_SET_ERROR_KEY: &str = "media.error_no_set";
/// i18n key for a `restore_version()` failure ("Failed to restore version:
/// {message}").
const RESTORE_ERROR_KEY: &str = "media.error_restore";
/// The detail a restore reports when the version was recorded under a
/// previous identity of this account and its bytes do not open under any key
/// that identity could have sealed them with (ruling (8)(d)) — shared with the
/// sync engine's restore re-seal, which refuses with the same words.
pub use fauna_core::nest_reseal::RESTORE_INHERITED_UNOPENABLE;
/// i18n key for an `upload_selected()` fired while a **followed browse scope**
/// is active. The scope is structurally read-only (`media.md` § Followed
/// public folders); without this guard the upload would silently target the
/// set the filter held *before* the scope was entered — a wrong-set landing
/// the user never asked for. No `{message}` arg.
const FOLLOWED_READ_ONLY_ERROR_KEY: &str = "media.error_followed_read_only";
/// i18n key for an upload into a **metadata-only** folder — its content stays
/// on the user's devices and this door keeps no copy, so it refuses before
/// anything is sealed or sent (`file-sync.md` § Relay serving). No `{message}`
/// arg.
const METADATA_ONLY_FOLDER_ERROR_KEY: &str = "media.error_metadata_only_folder";
/// i18n key for an upload into a shared set the caller may only **read** — the
/// owner granted reading, not writing, so it refuses before anything is sealed
/// or sent; the set stays an option (`media.md` § Encryption at rest → *The
/// filter and the upload target address the row*, (d)). No `{message}` arg.
const READ_ONLY_SHARED_FOLDER_ERROR_KEY: &str = "media.error_read_only_shared_folder";
/// i18n key for a followed-scope fetch the home nest refused — the plane's
/// folded `not_found`, ONE message for every cause (flipped back / deleted /
/// never there), deliberately: a per-case message would rebuild the existence
/// oracle the fold prevents. No `{message}` arg.
const FOLLOWED_UNAVAILABLE_ERROR_KEY: &str = "media.error_followed_unavailable";
/// i18n key for a followed-scope failure that is NOT the plane's refusal
/// ("Couldn't read that followed folder: {message}") — kept apart from the
/// unavailable wording because a dropped connection must never render as the
/// revoke (`folders.md` § Publicly-synced follow).
const FOLLOWED_FETCH_ERROR_KEY: &str = "media.error_followed_fetch";

/// Internal page state. In-memory only; clients read snapshots via the getter.
struct State {
    /// The full cross-set aggregate from the last successful `refresh()` (all
    /// readable sets' media, pre-view). The rendered list is derived from this by
    /// applying the view state below; kept on a read failure (prior data stands).
    raw: MediaSnapshot,
    /// `media-sort-select` key.
    sort: MediaSortKey,
    /// Sort direction (desktop column-header toggle).
    descending: bool,
    /// `media-folder-filter`: the parsed option value — an own set's name, a
    /// shared set's ref, an item-only set's name — or `None` for the
    /// all-media default (`media.md` § Encryption at rest → *The filter and the
    /// upload target address the row*). The name variant keeps the plaintext,
    /// which the snapshot echoes and the item-only degrade matches on.
    filter: Option<FolderFilter>,
    /// `media-view-toggle`: `true` = thumbnail grid, `false` = list.
    view_grid: bool,
    /// Last page-level error (the `error-message` element); `None` when clear.
    error: Option<LocalizedText>,
    /// Whether a refresh has ever returned successfully — surfaced as
    /// [`MediaPageSnapshot::loaded`], which owns the full rationale. Set in the
    /// `Ok` arm of [`MediaMachine::refresh_with_owner`] and never cleared.
    loaded: bool,
    /// Followed public folders as browse-scope options, refreshed with the
    /// aggregate from the injected [`FollowedMediaSource`]; empty while the
    /// source is unwired.
    followed_options: Vec<FollowedMediaScope>,
    /// The active followed browse scope, `None` in the ordinary own-set
    /// browse. While `Some`, the snapshot's `items` renders this listing and
    /// `filter` carries the scope's minted value
    /// (`media.md` § Followed public folders).
    followed_active: Option<FollowedActive>,
    /// The open share-link create surface (`share-link-create-modal`).
    share_create: Option<ShareCreate>,
    /// The share-link list surface (`share-link-list`).
    share_links: ShareLinksSnapshot,
}

/// The create surface's state (`share-links.md` § Flows → Create). The file
/// is kept by `(folder, path)` — the gesture reads its CURRENT version at
/// create time, never a version frozen at open.
struct ShareCreate {
    folder: String,
    path: String,
    name: String,
    expiry: String,
    busy: bool,
    url: Option<String>,
    /// The file rests sealed, so the link is the fragment-keyed one
    /// (`fauna_client_share::mint_private_link`): decided once, at open, off
    /// the same facts the eligibility verdict read.
    key_in_fragment: bool,
}

/// Which set a custody read on this page addresses (`media.md` § Encryption at
/// rest → *A set is addressed by its row, named by its label*): the set's
/// **row** wherever the item or gesture can name one, its name only as the
/// declared degrade. Two sets a reader sees may share a name — a member who
/// owns `photos` also reads a collaborator's shared `photos` — so content
/// keys, the signer ledger, the declassification verdict and the relay hint
/// all key by this, never by a name.
///
/// Built from a record ([`Self::of`]: an item's or version's `folder_id`),
/// from a gesture's name through the page's own rows
/// ([`MediaMachine::item_set_key`]), or for a write by name through the
/// owner-first target rule ([`MediaMachine::target_set_key`]) — and anything
/// that already holds a set's identity hands its `Ref` straight in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SetKey {
    /// The set's row: `FolderRef::Local(folders.id)`, the `folder_id` every
    /// `fauna.media.list` item and `fauna.folders.list` row carries.
    Ref(fauna_core::folder_keys::FolderRef),
    /// The degrade for a producer that stamps no `folder_id`: the set's name
    /// hash (`path_crypto::set_name_hash`, the row's `folder_hash`), resolved
    /// through the by-name forms exactly as before the row was addressable.
    Name([u8; 32]),
}

impl SetKey {
    /// The key of a record that carries `folder_id` (else its set's `name`).
    fn of(folder_id: Option<i64>, name: &str) -> Self {
        Self::of_hash(folder_id, path_crypto::set_name_hash(name))
    }

    /// [`Self::of`] with the name already hashed — a listed row whose
    /// plaintext name may be scrubbed carries its `folder_hash`.
    fn of_hash(folder_id: Option<i64>, name_hash: [u8; 32]) -> Self {
        match folder_id {
            Some(id) => Self::Ref(fauna_core::folder_keys::FolderRef::Local(id)),
            None => Self::Name(name_hash),
        }
    }

    /// The row form, for the relay hint — `None` on the name degrade.
    fn folder_ref(&self) -> Option<fauna_core::folder_keys::FolderRef> {
        match self {
            Self::Ref(r) => Some(*r),
            Self::Name(_) => None,
        }
    }

    /// The same-nest row a request names in its `folder_id` — `None` on the
    /// name degrade and for a foreign set, which keeps its channel form.
    fn local_row(&self) -> Option<i64> {
        match self.folder_ref() {
            Some(fauna_core::folder_keys::FolderRef::Local(id)) => Some(id),
            _ => None,
        }
    }

    /// The control-plane row this key names in `rows`: the row with that id,
    /// or on the degrade the first row of that name. A foreign set has no row
    /// here. `None` is every caller's fail-closed answer.
    fn row_in<'a>(
        &self,
        rows: &'a [fauna_client_media::MediaFolder],
    ) -> Option<&'a fauna_client_media::MediaFolder> {
        match self {
            Self::Ref(fauna_core::folder_keys::FolderRef::Local(id)) => {
                rows.iter().find(|f| f.id == *id)
            }
            Self::Ref(fauna_core::folder_keys::FolderRef::Foreign(_)) => None,
            Self::Name(hash) => rows
                .iter()
                .find(|f| path_crypto::set_name_hash(&f.name) == *hash),
        }
    }
}

/// Who the shared judge found each listed row **signed as**
/// (`mls-group-key-material.md` § M2 → *Writer-signed change records*, ruling
/// (8)(c)). The machine's gestures take a hash, not a row, so this is where a
/// row's verdict waits for the open that names its hash: a row signed as a
/// retired identity of the account is offered only that identity's root and
/// its predecessors', another writer's row none of the owner family
/// ([`RecordSigner`]).
///
/// **Sticky for the machine's lifetime, and the widest signature wins**
/// ([`RecordSigner::wider`]): once a row signed as the current identity names
/// a hash, it opens as the current identity's whatever another row says.
/// Nothing clears an entry — a later listing that omits the row (a nest that
/// stops serving it between the listing and the tap) must not re-arm a root.
/// A hash no listed row named reads as the current identity's, the ordinary
/// offer.
#[derive(Default)]
struct SignedAsLedger {
    /// `(set, manifest hash)` → who signed the row naming it. The set is the
    /// row's [`SetKey`], so a same-named pair's rows never share an entry.
    manifests: std::collections::HashMap<(SetKey, String), RecordSigner>,
    /// Thumbnail hashes, the same way. Keyed by hash alone: the thumbnail
    /// gesture names nothing else, and a thumbnail is only ever painted for
    /// the account that holds its key.
    thumbnails: std::collections::HashMap<String, RecordSigner>,
}

impl SignedAsLedger {
    fn note(
        &mut self,
        set: SetKey,
        manifest_hex: &str,
        signer: RecordSigner,
        chain: &[PredecessorSealKey],
    ) {
        let key = (set, manifest_hex.to_ascii_lowercase());
        let kept = self
            .manifests
            .get(&key)
            .map_or(signer, |held| held.wider(signer, chain));
        self.manifests.insert(key, kept);
    }

    fn note_thumbnail(
        &mut self,
        thumbnail_hex: &str,
        signer: RecordSigner,
        chain: &[PredecessorSealKey],
    ) {
        let key = thumbnail_hex.to_ascii_lowercase();
        let kept = self
            .thumbnails
            .get(&key)
            .map_or(signer, |held| held.wider(signer, chain));
        self.thumbnails.insert(key, kept);
    }

    /// Who signed the row naming `manifest_hex` in `set`.
    fn signer(&self, set: SetKey, manifest_hex: &str) -> RecordSigner {
        let key = (set, manifest_hex.to_ascii_lowercase());
        self.manifests.get(&key).copied().unwrap_or_default()
    }

    fn thumbnail_signer(&self, thumbnail_hex: &str) -> RecordSigner {
        self.thumbnails
            .get(&thumbnail_hex.to_ascii_lowercase())
            .copied()
            .unwrap_or_default()
    }
}

/// The facts [`fauna_client_share::share_link_eligible`] decides over for the
/// item's `set` (its row — [`SetKey`]): its VERIFIED public audience
/// (`MediaFolder::rests_unsealed`, the owner-attested verdict), whether it is
/// owner-only (`MediaFolder::owner_only`), and whether this seat holds the
/// owner root its chunks seal under — `author_held`, the share author wired by
/// [`MediaMachine::set_share_author`], which carries the owner root and its
/// predecessors. Fail-closed for a set the control plane did not report.
fn share_facts(
    raw: &MediaSnapshot,
    set: SetKey,
    author_held: bool,
) -> fauna_client_share::ShareFolderFacts {
    let known = set.row_in(&raw.known_folders);
    let owner_only = known.is_some_and(|f| f.owner_only);
    fauna_client_share::ShareFolderFacts {
        public_audience: known.is_some_and(|f| f.rests_unsealed),
        unbound: owner_only,
        owner_root_held: owner_only && author_held,
    }
}

/// An own-set item's summary with its share-link verdict filled in — the one
/// place `share_link_eligible` is set, so an item the browse lists and an item a
/// deep link locates ([`MediaMachine::locate_file`] / [`MediaMachine::locate_path`])
/// open the same detail surface (`share-links.md` § Which files can be linked).
fn own_summary(raw: &MediaSnapshot, item: MediaItem, author_held: bool) -> MediaItemSummary {
    let set = SetKey::of(item.folder_id, &item.folder);
    let filter_value = raw.filter_value(&item);
    let mut summary = MediaItemSummary::from(item);
    summary.filter_value = filter_value;
    summary.share_link_eligible =
        fauna_client_share::share_link_eligible(&share_facts(raw, set, author_held));
    summary
}

/// The active followed browse scope: the follow's identity plus the folded
/// listing its entry fetch produced. The entries are retained whole — not
/// just their rendered projection — because they carry each head's
/// `manifest_hash`, which [`MediaMachine::download_followed`] resolves by
/// path so the pointer never has to cross to an app (the ordinary browse gets
/// its manifest from the version rows, which a followed item can never have —
/// the public plane is head-only).
struct FollowedActive {
    scope: FollowedMediaScope,
    entries: Vec<fauna_core::followed_media::FollowedFileEntry>,
}

impl FollowedActive {
    /// The listing as the snapshot renders it — ordinary [`MediaItem`] rows,
    /// so the snapshot applies the active sort exactly as for the aggregate.
    fn items(&self) -> Vec<MediaItem> {
        self.entries
            .iter()
            .map(|e| followed_item(&self.scope, e))
            .collect()
    }
}

impl State {
    fn new() -> Self {
        Self {
            raw: MediaSnapshot::default(),
            sort: MediaSortKey::default(),
            descending: false,
            filter: None,
            view_grid: false,
            error: None,
            loaded: false,
            followed_options: Vec::new(),
            followed_active: None,
            share_create: None,
            share_links: ShareLinksSnapshot::default(),
        }
    }
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MediaMachine {
    state: Mutex<State>,
    observer: Arc<dyn MediaObserver>,
    nest_api: Arc<dyn MediaNestApi>,
    /// The blob-upload seam for `upload()` (the bulk-binary `POST /api/v1/blob`
    /// leg). `None` on a platform whose blob uploader isn't wired yet (web's wasm
    /// coordinator — LEG B; native LEG A is a follow-on) → `upload()` returns a
    /// `Transient` "not supported" error. `delete()` never needs it.
    blob_uploader: Option<Arc<dyn MediaBlobUploader>>,
    /// The blob-download seam for `fetch_thumbnail()` (the bulk-binary `GET
    /// /api/v1/blob/<hash>` direct-by-hash leg). `None` on a platform whose blob
    /// fetcher isn't wired yet (web, until its wasm fetch coordinator) →
    /// `fetch_thumbnail()` returns a `Transient` "not supported" error. The
    /// download twin of `blob_uploader`.
    blob_fetcher: Option<Arc<dyn MediaBlobFetcher>>,
    /// The full-file download seam for `download_file()` — the shared
    /// client-side walk's per-target fetch leg (native
    /// `fauna_client::NestPublicChunkFetcher`, wasm
    /// `fauna_core::file_download::WasmPublicChunkFetcher`, injected by
    /// `nest_api::ws_rpc::build_media_machine`; `/api/v1/manifests/{hash}` +
    /// `/api/v1/chunks/{hash}`). `None` on a platform with no wired leg →
    /// `download_file()` returns a `Transient` "not supported" error, like the
    /// two seams above.
    download_fetcher: Option<Arc<dyn fauna_core::file_download::BlobFetcher>>,
    /// The shared-folder content-key resolver for `download_file()` (Phase 0 —
    /// the read leg). `Some` on a platform that ingests folder custody (native
    /// apps that build the conversations session) → a download of a **shared**
    /// set opens under its content keys; `None` (web today, tests) → every set is
    /// treated as owner-only (opens under the owner `BackupKey`). See
    /// [`crate::folder_keys::FolderKeyResolver`].
    folder_keys: Option<Arc<dyn crate::folder_keys::FolderKeyResolver>>,
    /// The **foreign-nest** fetcher factory for `download_file()` of a
    /// cross-nest shared set (Phase 2 client read-side): when the resolver
    /// returns a `home_nest_url`, the bytes live on that nest and the machine
    /// fetches through a factory-built fetcher instead of
    /// [`Self::download_fetcher`] (which points home). Set once post-
    /// construction by the platform builder ([`Self::set_foreign_fetchers`],
    /// the `set_folder_custody_sink` pattern — `new()` keeps its arity for
    /// the many test constructors). Unset → a cross-nest download refuses
    /// loudly (`Transient` "not supported"), same-nest sets unaffected.
    foreign_fetchers: std::sync::OnceLock<Arc<dyn crate::folder_keys::ForeignBlobFetcherFactory>>,
    /// The followed browse-scope source (`media.md` § Followed public
    /// folders): the follow records plus their on-demand listings, with the
    /// availability verdicts cached source-side. Set once post-construction by
    /// the platform builder ([`Self::set_followed_media_source`], the
    /// [`Self::set_foreign_fetchers`] pattern); unset → no followed options,
    /// the correct render for an app that has not built the surface. The
    /// production impl is the same `StoreFollowedFoldersSource` instance the
    /// Devices machine holds, so a Media browse fetch feeds the very cache the
    /// Folders page's probe reads.
    followed_source: std::sync::OnceLock<Arc<dyn FollowedMediaSource>>,
    /// The client's MLS join-filter over the member-visible folder rows
    /// (`fauna_protocol::member_rows`, the one filter the Folders page runs —
    /// `media.md` § Layout & flow → *Where the offerable sets come from*).
    /// Wired post-construction ([`Self::set_mls_query`]); `None` until then ⇒
    /// **fail-safe**: every member row is dropped, so a stranger's rostered
    /// knock never surfaces as an option or a target.
    mls_query: Mutex<Option<Arc<dyn MlsQuery>>>,
    /// The owner `BackupKey` for the **write-side label seals** the delete /
    /// restore gestures mint (S8 D2 — those tombstone/re-point records are
    /// append-only nest-side and can never be re-sealed later). Injected once
    /// post-construction by the platform builder
    /// ([`Self::set_owner_backup_key`], the [`Self::set_foreign_fetchers`]
    /// pattern — `new()` keeps its arity), NOT per gesture like `upload`'s key:
    /// delete/restore are keyed by *which row*, not *which bytes*, so threading
    /// a key through every app's confirm dialog would fan the same parameter
    /// across 7 apps for no per-call reason. `None` (an app not yet wired, the
    /// trickle-down gap) degrades to plaintext-only records — S8 backfill rows,
    /// exactly the pre-D2 behavior. Assembled with [`Self::folder_keys`] into
    /// a [`fauna_core::label_custody::LabelCustody`] at the gesture, the same
    /// pairing `refresh()` builds from its per-call key.
    owner_backup_key: Mutex<Option<fauna_core::crypto::BackupKey>>,
    /// Retired owner `BackupKey`s of the identities this account **succeeded
    /// from** — **read** candidates for the media corpus a succession re-pointed
    /// but did not re-seal (`succession-aftermath.md` § Re-key scope, the
    /// `BackupKey` corpus row: *media*, folders and backups).
    ///
    /// Injected post-construction like [`Self::owner_backup_key`] and for the
    /// same reason. Empty for every identity that never succeeded, so the common
    /// fleet is untouched; an app that has not wired it renders and downloads
    /// exactly as before — fail-closed, since a media item sealed under a
    /// predecessor simply stays unopenable rather than opening under a wrong
    /// root.
    ///
    /// ⚠ **Read side only.** These reach `FileDownloadKeys::predecessor_backup_keys`
    /// via [`LabelCustody::with_predecessors`], never `label_seal_root`, so the
    /// delete/restore gestures above keep sealing under
    /// [`Self::owner_backup_key`] alone.
    ///
    /// Each key paired with the identity it belongs to where the app named it
    /// ([`Self::set_predecessor_chain`]) — only a paired key opens a row signed
    /// as a predecessor (ruling (8)(c), [`RecordSigner`]).
    predecessor_backup_keys: Mutex<Vec<PredecessorSealKey>>,
    /// The account's attested predecessor ids
    /// ([`Self::set_predecessor_actor_ids`], [`Self::set_predecessor_chain`]):
    /// a row signed as one of them is the account's own, bounded to that
    /// identity's roots; one signed as anyone else is another writer's.
    predecessor_ids: Mutex<Vec<[u8; 32]>>,
    /// What the judged listings said about who signed each row — consulted by
    /// every open ([`SignedAsLedger`]).
    signed_as: Mutex<SignedAsLedger>,
    /// The identity share links are minted, signed and sealed under, and the
    /// nest they point at ([`Self::set_share_author`]). `None` until the
    /// platform wires it: the share gestures then report an error rather
    /// than mint under nothing.
    share_author: Mutex<Option<Arc<fauna_client_share::ShareAuthor>>>,
}

impl MediaMachine {
    /// Construct the page machine over an injected [`MediaNestApi`] seam + an
    /// optional [`MediaBlobUploader`]. State starts empty (all-media view, name
    /// sort ascending, list mode); the client calls `refresh()` to populate it.
    ///
    /// `blob_uploader` / `blob_fetcher` are `Some` on a platform with a wired
    /// blob-upload / -download path and `None` otherwise (then `upload()` /
    /// `fetch_thumbnail()` report unsupported; `delete()` and the reads work
    /// regardless).
    ///
    /// Not a `#[uniffi::constructor]` — the seams (`Arc<dyn …>`) have no FFI ABI.
    /// Clients construct via `nest_api::build_media_machine` (native `fauna-ffi` /
    /// linux, wasm web), which binds the session's connected requester; tests pass
    /// a `FakeMediaNestApi` (+ `FakeMediaBlobUploader` / `FakeMediaBlobFetcher`).
    pub fn new(
        observer: Arc<dyn MediaObserver>,
        nest_api: Arc<dyn MediaNestApi>,
        blob_uploader: Option<Arc<dyn MediaBlobUploader>>,
        blob_fetcher: Option<Arc<dyn MediaBlobFetcher>>,
        download_fetcher: Option<Arc<dyn fauna_core::file_download::BlobFetcher>>,
        folder_keys: Option<Arc<dyn crate::folder_keys::FolderKeyResolver>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::new()),
            observer,
            nest_api,
            blob_uploader,
            blob_fetcher,
            download_fetcher,
            folder_keys,
            foreign_fetchers: std::sync::OnceLock::new(),
            followed_source: std::sync::OnceLock::new(),
            mls_query: Mutex::new(None),
            owner_backup_key: Mutex::new(None),
            predecessor_backup_keys: Mutex::new(Vec::new()),
            predecessor_ids: Mutex::new(Vec::new()),
            signed_as: Mutex::new(SignedAsLedger::default()),
            share_author: Mutex::new(None),
        })
    }

    /// Wire the foreign-nest fetcher factory (cross-nest shared-set downloads —
    /// see the field doc). Set once by the platform builder after `new()`;
    /// later calls are no-ops.
    pub fn set_foreign_fetchers(
        &self,
        factory: Arc<dyn crate::folder_keys::ForeignBlobFetcherFactory>,
    ) {
        let _ = self.foreign_fetchers.set(factory);
    }

    /// Wire the followed browse-scope source (see the field doc). Set once by
    /// the platform builder after `new()`; later calls are no-ops.
    pub fn set_followed_media_source(&self, source: Arc<dyn FollowedMediaSource>) {
        let _ = self.followed_source.set(source);
    }

    /// Wire the client's MLS join-filter (see the field doc) — the
    /// `DevicesMachine::set_mls_query` twin, called by the platform glue after
    /// construction and never over UniFFI (a trait object has no FFI ABI).
    /// Until wired, every shared row is dropped (fail-safe): shared sets still
    /// reach the page through their items, as item-only options by name.
    pub fn set_mls_query(&self, mls_query: Arc<dyn MlsQuery>) {
        *self.mls_query.lock().unwrap() = Some(mls_query);
    }

    /// `list_folders` behind the join-filter: an own row always passes, a
    /// member row only once this client has joined its group.
    async fn list_joined_folders(
        &self,
    ) -> Result<Vec<fauna_client_media::MediaFolder>, MediaApiError> {
        let rows = self.nest_api.list_folders().await?;
        let query = self.mls_query.lock().unwrap().clone();
        Ok(rows
            .into_iter()
            .filter(|f| {
                fauna_client_media::member_rows::admits(
                    query.as_deref(),
                    f.member,
                    f.mls_group_id.as_deref(),
                )
            })
            .collect())
    }

    /// Whether a builder handed this machine the folder custody resolver — the
    /// seam a platform builder's call-site pin asserts, since a machine built
    /// without one seals a bound set's gesture paths under the owner root
    /// (`path-sealing.md` § S8 D2). Kept out of the UniFFI-exported blocks.
    pub fn has_folder_keys(&self) -> bool {
        self.folder_keys.is_some()
    }
}

/// What a set's name rendered to for the reader holding this page — cached once
/// per distinct set inside [`MediaMachine::render_sealed_paths`].
///
/// A two-armed [`path_crypto::SealedLabelRender`], collapsed: the render's
/// `Sealed`/`Plaintext` distinction says *where the name came from*, which this
/// caller does not care about, while `Omit` is a decision it must act on. Kept a
/// named type rather than an `Option<String>` so the omit arm reads as the
/// ratified degrade at the match site instead of as a missing value.
enum SetNameRender {
    Named(String),
    /// The reader can open neither the seal nor a plaintext — every item in this
    /// set omits from the listing (*omit from the listing, re-enter on
    /// re-record*).
    Omit,
}

/// Private helpers. Deliberately in their own `impl` block: the surface
/// block below is `uniffi::export`ed wholesale, and an exported method may
/// only take/return FFI-liftable types — which `FileDownloadKeys` and
/// `MediaSnapshot` are not, and should not become just to sit next to
/// their callers.
impl MediaMachine {
    /// Render every item's path **sealed-first**, in place, right where the page
    /// data arrives — not on each `snapshot()` paint (`observability.md`'s
    /// *event, not paint* rule applies to crypto for the same reason it applies
    /// to logs: a photo-library-sized list would otherwise re-open every label
    /// on every tick).
    ///
    /// Key custody is the reader's ordinary **byte-download** custody
    /// ([`fauna_core::file_download::FileDownloadKeys`]) — by the sealing
    /// ruling's own logic the roots that open a set's chunks open its names, so
    /// there is deliberately no second resolver here to drift out of sync with
    /// [`MediaMachine::download_file`]'s.
    ///
    /// An item whose label this reader cannot render **and** which carries no
    /// plaintext is dropped from the snapshot entirely — the ratified degrade
    /// (*omit from the listing, re-enter on re-record*), never an item with an
    /// empty name and never a failed page.
    /// Render each control-plane set's name — the upload targets and the
    /// `media-folder-filter` options — through the same custody seam the item
    /// rows' set names render through ([`Self::render_sealed_paths`]). A set
    /// created sealed is listed with a blank plaintext name
    /// (`path-sealing.md` § the set-name plane); offered unrendered it was a
    /// nameless upload target whose record carried no address. A set no
    /// custody opens is omitted, never offered nameless.
    async fn render_sealed_folder_names(
        &self,
        sets: Vec<fauna_client_media::MediaFolder>,
        owner: Option<BackupKey>,
    ) -> Vec<fauna_client_media::MediaFolder> {
        let mut rendered = Vec::with_capacity(sets.len());
        for mut set in sets {
            if set.name_sealed.is_none() {
                rendered.push(set);
                continue;
            }
            let wire_hash = set.name_hash.as_deref();
            // The row's own custody, by its identity — never by the name hash
            // a same-named set shares.
            let row = SetKey::Ref(fauna_core::folder_keys::FolderRef::Local(set.id));
            let keys = self.folder_keys_for_key(&row, owner.clone()).await.0;
            match fauna_core::label_custody::render_set_name(
                &keys,
                set.name_sealed.as_deref(),
                &set.name,
                wire_hash,
            ) {
                path_crypto::SealedLabelRender::Sealed(name)
                | path_crypto::SealedLabelRender::Plaintext(name) => {
                    set.name = name;
                    rendered.push(set);
                }
                path_crypto::SealedLabelRender::Omit => {}
            }
        }
        rendered
    }

    async fn render_sealed_paths(&self, snap: &mut MediaSnapshot, owner: Option<BackupKey>) {
        // Nothing sealed on this page: a plaintext-resting plane's rows and the
        // keyless-writer rows. Skips the per-set key resolution entirely, so a
        // `None`-key caller costs exactly what it did before this slice.
        //
        // ⚠ The guard is *"nothing on this page is sealed"*, deliberately — never
        // *"this reader holds no key"*. Skipping the render for a keyless reader
        // shows a sealed-only row's blank plaintext as its name; that is the bug
        // `LabelCustody::is_keyless` was deleted for rather than fixed (S3).
        // Both label axes count: a page may carry set-name seals and no path
        // seals (a bound set whose files a keyless writer recorded as plaintext-only paths).
        if snap
            .items
            .iter()
            .all(|it| it.path_sealed.is_none() && it.folder_sealed.is_none())
        {
            return;
        }

        // Resolve custody once per distinct set, not once per item — `resolve`
        // is a roster + config read. The set's *name* renders once per set for
        // the same reason, and is cached beside its keys. Keyed by the item's
        // row ([`SetKey`]) — two same-named sets share a `name_hash` and must
        // each render under their own custody — and, for a row that stamps no
        // id, by the set's `name_hash` (the row's `folder_hash`, else the hash
        // of its plaintext), never by `folder`: once the nest scrubs sealed
        // names every set's plaintext is the same empty sentinel.
        let mut per_set: std::collections::HashMap<SetKey, (FileDownloadKeys, SetNameRender)> =
            std::collections::HashMap::new();
        let total = snap.items.len();
        let mut rendered = Vec::with_capacity(total);
        let chain = self.predecessor_keys();
        for mut item in std::mem::take(&mut snap.items) {
            let set_key = SetKey::of_hash(
                item.folder_id,
                fauna_core::label_custody::set_name_label_salt(
                    item.folder_hash.as_ref().map(|b| &b[..]),
                    &item.folder,
                ),
            );
            if let std::collections::hash_map::Entry::Vacant(slot) = per_set.entry(set_key) {
                let keys = self.folder_keys_for_key(&set_key, owner.clone()).await.0;
                // The set-name twin of the path render below, through the same
                // shared seam so the two cannot disagree on root or salt. The
                // nest ships `folder_sealed`/`folder_hash` only to this
                // set's label audience, so a non-audience reader sees `None`
                // here and keeps the plaintext it was sent.
                let name = match fauna_core::label_custody::render_set_name(
                    &keys,
                    item.folder_sealed.as_ref().map(|b| &b[..]),
                    &item.folder,
                    item.folder_hash.as_ref().map(|b| &b[..]),
                ) {
                    path_crypto::SealedLabelRender::Sealed(name) => SetNameRender::Named(name),
                    path_crypto::SealedLabelRender::Plaintext(_) => {
                        SetNameRender::Named(item.folder.clone())
                    }
                    path_crypto::SealedLabelRender::Omit => SetNameRender::Omit,
                };
                slot.insert((keys, name));
            }
            let (keys, set_name) = &per_set[&set_key];
            let signer = self.signer_of(item.signed_as_current, item.signed_as);
            // Noted WHATEVER the labels below do (ruling (8)(c)): a row whose
            // name does not render is still one a gesture can name by its
            // hash, and an un-noted hash reads as the current identity's. A
            // thumbnail is keyed by hash alone, so it is noted even before
            // the set's name renders.
            if let Some(thumbnail) = item.thumbnail_hash.as_deref() {
                self.signed_as
                    .lock()
                    .unwrap()
                    .note_thumbnail(thumbnail, signer, &chain);
            }

            // A set whose *name* this reader cannot render is a set whose files
            // it cannot render either (same custody, same roots), so the item
            // omits rather than surfacing under an empty set — the ratified
            // degrade, and the reason no caller ever sees a blank `folder`.
            let SetNameRender::Named(set_name) = set_name else {
                continue;
            };
            let set_name = set_name.clone();
            if let Some(manifest) = item.manifest_hash.as_ref() {
                // Under the rendered name — the one a gesture hands back — on
                // the degrade, exactly as `note_signed_as` and every reader
                // derive it.
                self.signed_as.lock().unwrap().note(
                    SetKey::of(item.folder_id, &set_name),
                    &hex::encode(&manifest[..]),
                    signer,
                    &chain,
                );
            }

            // Salt selection + the sealed-first policy both live in the shared
            // seam (`fauna_core::label_custody`), so this surface cannot drift
            // from snapshot browse/diff or the conflict list on either.
            // Ruling (8)(c): a row signed under a retired identity (or by
            // another writer) is never offered this account's CURRENT owner
            // root on the label arm — else a predecessor's signature could
            // render, in this set, the sealed path of a file the successor
            // created in another.
            let label_keys;
            let keys = if signer.offers_current() {
                keys
            } else {
                label_keys = FileDownloadKeys {
                    record_signer: signer,
                    ..keys.clone()
                };
                &label_keys
            };
            match fauna_core::label_custody::render_path(
                keys,
                item.path_sealed.as_ref().map(|b| &b[..]),
                &item.path,
                item.path_hash.as_ref().map(|b| &b[..]),
                path_crypto::LabelField::SyncChangePath,
            ) {
                path_crypto::SealedLabelRender::Sealed(path) => {
                    item.path = path;
                    item.folder = set_name;
                    rendered.push(item);
                }
                // Already the plaintext we were handed — nothing to rewrite.
                path_crypto::SealedLabelRender::Plaintext(_) => {
                    item.folder = set_name;
                    rendered.push(item);
                }
                path_crypto::SealedLabelRender::Omit => {}
            }
        }
        // An omitted row is a *correct* degrade for a genuine non-audience reader
        // (see the two `Omit` arms above) — but it is indistinguishable, from the
        // page's side, from the caller simply never passing its key: both render
        // fewer items with no error, and dropping ALL of them empties the page in
        // silence. That silence is what made the apple leg's `refresh(nil)` cost
        // several sessions to find (it presented as an empty `fauna.media.list`
        // reply, which it never was). Log on the event, once per refresh
        // (`observability.md` § Log on the *event*, not the *paint*), so the next
        // keyless caller is one grep away instead of one bisect.
        if rendered.len() < total {
            tracing::warn!(
                target: "fauna_media",
                dropped = total - rendered.len(),
                total,
                keyed = owner.is_some(),
                "media rows omitted: their labels could not be rendered under this \
                 reader's custody (a keyless caller drops its own sealed rows)",
            );
        }
        snap.items = rendered;
    }

    /// Keep what the judged listing said about who signed each item, under
    /// the names the page renders (and so the names a gesture hands back).
    fn note_signed_as(&self, items: &[MediaItem]) {
        let chain = self.predecessor_keys();
        let mut ledger = self.signed_as.lock().unwrap();
        for item in items {
            let signer = self.signer_of(item.signed_as_current, item.signed_as);
            if let Some(manifest) = item.manifest_hash.as_ref() {
                ledger.note(
                    SetKey::of(item.folder_id, &item.folder),
                    &hex::encode(&manifest[..]),
                    signer,
                    &chain,
                );
            }
            if let Some(thumbnail) = item.thumbnail_hash.as_deref() {
                // Ruling (10)(c), the stamp binds the root: a thumbnail is a
                // bare-key blob only an unstamped row records (a content-keyed
                // upload records none), so a STAMPED row naming one earns it
                // no owner root — whoever signed it. Another row's right to
                // it still stands (the widest wins).
                let signer = if item.content_key_version.is_some() {
                    RecordSigner::Other
                } else {
                    signer
                };
                ledger.note_thumbnail(thumbnail, signer, &chain);
            }
        }
    }

    /// The per-signer bound for a judged row ([`RecordSigner`]): the current
    /// identity; a retired identity of this account (an attested id, or one a
    /// paired key names); or another writer. An item no judge verified
    /// (`signed_as: None` and not current — an exempt row) is another
    /// writer's for this purpose: offered none of the owner family, as the
    /// coarse rule it replaces withheld the current root from it.
    fn signer_of(&self, signed_as_current: bool, signed_as: Option<[u8; 32]>) -> RecordSigner {
        if signed_as_current {
            return RecordSigner::Current;
        }
        let Some(signed_as) = signed_as else {
            return RecordSigner::Other;
        };
        let attested = self.predecessor_ids.lock().unwrap().contains(&signed_as);
        let paired = self
            .predecessor_backup_keys
            .lock()
            .unwrap()
            .iter()
            .any(|k| k.actor_id.is_some_and(|a| a.0 == signed_as));
        if attested || paired {
            RecordSigner::Predecessor(fauna_core::identity::ActorId(signed_as))
        } else {
            RecordSigner::Other
        }
    }

    /// Who signed the row naming `manifest_hex` in `set`
    /// ([`SignedAsLedger::signer`]).
    fn signer_for(&self, set: SetKey, manifest_hex: &str) -> RecordSigner {
        self.signed_as.lock().unwrap().signer(set, manifest_hex)
    }

    /// The [`SetKey`] of the item a gesture names by `(folder, path)` — and by
    /// its manifest when the gesture carries one — read off that item in the
    /// page's own rows, never by name through `known_folders`, where a
    /// reader's own same-named set would answer for a shared one (`media.md`
    /// § Encryption at rest → *A set is addressed by its row*; `file-sync.md`
    /// § Relay serving, step (4)). The path matches the rendered path or its
    /// `path_hash`. A manifest no listed row names (a historical version)
    /// falls back to the item at that path; no matching row, or a row the
    /// nest stamped no id on, is the name degrade.
    ///
    /// Every reader of a record's custody and every writer of its ledger entry
    /// derives the key here or from the record's own `folder_id`, so the two
    /// cannot disagree.
    fn item_set_key(&self, folder: &str, path: &str, manifest_hex: Option<&str>) -> SetKey {
        let path_hash = fauna_core::sync::path_hash(path);
        let s = self.state.lock().unwrap();
        let at_path = |it: &&MediaItem| {
            it.folder == folder
                && (it.path == path
                    || it
                        .path_hash
                        .as_deref()
                        .is_some_and(|h| h[..] == path_hash[..]))
        };
        let names_manifest = |it: &&MediaItem| {
            manifest_hex.is_some_and(|m| {
                it.manifest_hash
                    .as_deref()
                    .is_some_and(|h| hex::encode(h).eq_ignore_ascii_case(m))
            })
        };
        let items = &s.raw.items;
        let found = items
            .iter()
            .filter(at_path)
            .find(names_manifest)
            .or_else(|| items.iter().find(at_path));
        SetKey::of(found.and_then(|it| it.folder_id), folder)
    }

    /// The [`SetKey`] of a listed version: its own row, which the nest stamps
    /// on every version (`FileVersionSummary::folder_id`) — a by-name listing
    /// interleaves a same-named pair's versions of one path, so the item at
    /// that path cannot answer for them (`media.md` § Encryption at rest → *A
    /// set is addressed by its row*). A version carrying no row (a nest that
    /// stamps none) resolves through [`Self::item_set_key`].
    ///
    /// The one derivation for both the ledger's note ([`Self::file_versions`])
    /// and the restore's read ([`Self::restore_version`]), so they agree.
    fn version_set_key(&self, folder: &str, path: &str, version: &FileVersionSummary) -> SetKey {
        match version.folder_id {
            Some(_) => SetKey::of(version.folder_id, folder),
            None => self.item_set_key(folder, path, Some(&version.manifest_hash)),
        }
    }

    /// The set a gesture made BY NAME **writes** into — the upload target and
    /// the delete / restore re-point's set: the caller's own set of that name
    /// (`MediaSnapshot::own_row` — never a shared row of that name) when they own
    /// one, else the name degrade. The nest resolves the same by-name write
    /// owner-first (`folder_authz::resolve_writable_folder`), so the seal root
    /// and the row the nest writes agree.
    ///
    /// The by-name write is the stopgap until the filter and the upload target
    /// address the row (`media.md` § Encryption at rest → *The filter and the
    /// upload target address the row*, (e)): a caller that already holds the
    /// target's identity hands its [`SetKey::Ref`] to the write directly
    /// ([`Self::upload_into`]) and never comes here.
    fn target_set_key(&self, folder: &str) -> SetKey {
        let s = self.state.lock().unwrap();
        SetKey::of(s.raw.own_row(folder).map(|f| f.id), folder)
    }

    /// The set an upload's target **value** writes into, with the name its
    /// record carries (`media.md` § Encryption at rest → *The filter and the
    /// upload target address the row*, (e)): a ref resolves to its listed row
    /// (`None` when the list carries no such row — nothing to name, nothing to
    /// write); a name to the caller's own set of that name, else the name
    /// degrade ([`Self::target_set_key`]).
    fn resolve_target(&self, filter: &FolderFilter) -> Option<(String, SetKey)> {
        match filter {
            FolderFilter::Ref(r) => {
                let s = self.state.lock().unwrap();
                let row = s.raw.row_for(filter)?;
                Some((row.name.clone(), SetKey::Ref(*r)))
            }
            FolderFilter::Name(name) => Some((name.clone(), self.target_set_key(name))),
        }
    }

    /// The owner key injected by [`Self::set_owner_backup_key`], cloned out from
    /// under the lock.
    ///
    /// Its own function so the guard can never be held across an `.await`: both
    /// callers immediately await, and `lock().unwrap().clone()` written inline
    /// keeps the temporary guard alive to the end of the enclosing statement —
    /// which would include that await.
    fn owner_key(&self) -> Option<BackupKey> {
        self.owner_backup_key.lock().unwrap().clone()
    }

    /// The retired owner keys injected by
    /// [`Self::set_predecessor_backup_keys`], cloned out from under the lock.
    ///
    /// Its own function for exactly [`Self::owner_key`]'s reason, and this one
    /// learned it the hard way: written inline as
    /// `self.predecessor_backup_keys.lock().unwrap().clone()` inside
    /// [`Self::folder_keys_for_key`], the temporary `MutexGuard` lives to the end
    /// of the enclosing statement — which spans the `.await` — and the whole
    /// `#[uniffi::export]` block stops compiling with "future is not `Send`".
    fn predecessor_keys(&self) -> Vec<PredecessorSealKey> {
        self.predecessor_backup_keys.lock().unwrap().clone()
    }

    /// The id of `set`'s row when its bytes rest **unsealed**, else `None` —
    /// the verdict read off the row with that id, never the first row of the
    /// name.
    ///
    /// The page-snapshot reading — for the READ and refuse-only paths, where a
    /// stale answer costs a failed open or a refusal, never a plaintext write.
    /// The producer arm uses [`Self::declassified_folder_id_now`].
    fn declassified_folder_id(&self, set: SetKey) -> Option<i64> {
        let s = self.state.lock().unwrap();
        set.row_in(&s.raw.known_folders)
            .filter(|f| f.rests_unsealed)
            .map(|f| f.id)
    }

    /// The id of `set`'s row when its bytes rest **unsealed** *right now*, else `None`.
    ///
    /// Reads the verdict the control plane gives **now** (a fresh
    /// `list_folders`, not the page snapshot; `MediaFolder::rests_unsealed`, set from
    /// `FolderSummary::judge_declassification` at the ws-rpc seam) — this page never
    /// re-derives the declassification rule, and an unknown folder answers
    /// `None`, which seals. That direction is deliberate on BOTH sides of the
    /// gesture: a producer that guesses "public" leaks bytes irrecoverably,
    /// while one that guesses "sealed" costs only a re-record, and a reader
    /// that guesses wrong merely fails to open (`encryption-at-rest.md`
    /// § Readable classes -> *Owner-flipped public-audience folders*).
    async fn declassified_folder_id_now(&self, set: SetKey) -> Option<i64> {
        // Read at upload time, never from the page's last-refresh snapshot: the
        // owner may have flipped the folder back to private on another device
        // since, and a stale "public" would POST plaintext the nest then refuses
        // (`path_seal_required`) and orphans until the blob GC. A failed read, or
        // a folder the reply does not name as public, seals. The read goes
        // through the same `list_folders` seam (the one `judge_declassification`
        // mapping); nothing is re-derived here.
        match self.list_joined_folders().await {
            Ok(folders) => set
                .row_in(&folders)
                .filter(|f| f.rests_unsealed)
                .map(|f| f.id),
            Err(e) => {
                tracing::warn!(
                    target: "fauna_media",
                    error = %e.detail(),
                    "folder audience read failed at upload time; sealing",
                );
                None
            }
        }
    }

    /// Open a raw-AEAD **Library**-audience blob under every owner root this
    /// account may hold for it — the gesture's `current` key first, then each
    /// retired predecessor in registry order.
    ///
    /// The crate's one *bare*-`BackupKey` open site's funnel
    /// ([`Self::fetch_thumbnail`]: a folder file rests as a chunk manifest, its
    /// thumbnail as its own blob). It is the plane
    /// `FileDownloadKeys::predecessor_backup_keys` does **not** reach: that
    /// field feeds `owner_open_roots`, which offers *convergent chunk roots*,
    /// while these blobs seal under the bare key. So a successor whose chunk
    /// corpus already reads (leg 5's read half) still had its whole inherited
    /// thumbnail grid read as dark — the same break, one plane over
    /// (`succession-aftermath.md` § Re-key scope, the `BackupKey` corpus row).
    ///
    /// ⚠ **Callers MUST have verified the content address first**, and that
    /// ordering is what makes the extra candidates safe rather than a widening.
    /// The backup-chunk frame carries no AAD, so a tag proves only
    /// *sealed-under-some-root-we-hold*, never *these bytes* — but
    /// because the address check has already pinned the bytes to the hash the
    /// caller asked for, an extra key can only widen *which root may open
    /// already-proven bytes*, never *which bytes may be served*. Reversing the
    /// order would hand a malicious nest one substitution attempt per ancestor.
    ///
    /// **Read-only by construction, exactly as the chunk plane is**: the retired
    /// roots are reachable only from here, and every Library *seal* site
    /// ([`Self::do_upload`], `seal_gesture_path`) takes the caller's current key
    /// directly — so no gesture can land new bytes under a root the aftermath
    /// exists to retire.
    ///
    /// Cost in the steady state is nil: an account that never succeeded holds no
    /// predecessors, and the current key is always tried first.
    ///
    /// `signer` is who signed the row naming the blob (ruling (8)(c), the
    /// bare-key twin of `FileDownloadKeys::record_signer`, through the same
    /// [`RecordSigner::retired_keys`]): a row signed as a retired identity is
    /// offered only that identity's root and its predecessors', so a
    /// predecessor's signature never opens a blob the successor — or a later
    /// predecessor — sealed; another writer's row, none of the owner family.
    fn open_library_blob(
        &self,
        current: &BackupKey,
        sealed: &[u8],
        what: &str,
        signer: RecordSigner,
    ) -> Result<Vec<u8>, MediaApiError> {
        // The current key's failure is the one reported: for the overwhelmingly
        // common no-succession account it is the only attempt, so the message
        // stays exactly what it was before the candidates existed.
        let current_err = if signer.offers_current() {
            match decrypt_backup_chunk(current, sealed) {
                Ok(plaintext) => return Ok(plaintext),
                Err(e) => e.to_string(),
            }
        } else {
            "its record was signed under another identity, whose keys do not open it".to_string()
        };
        let chain = self.predecessor_keys();
        for retired in signer.retired_keys(&chain) {
            let Some(retired) = retired.client_key() else {
                continue;
            };
            if let Ok(plaintext) = decrypt_backup_chunk(retired, sealed) {
                return Ok(plaintext);
            }
        }
        Err(MediaApiError::BadRequest {
            // Non-retryable: no root this account holds opens these bytes, and
            // that verdict cannot change on retry.
            detail: format!("{what} decrypt failed: {current_err}"),
        })
    }

    /// [`MediaMachine::refresh`] over an already-typed owner key — the form the
    /// **gesture** refreshes take, so a write can re-render under the very
    /// custody it just sealed with ([`Self::owner_key`]) instead of having to
    /// re-serialize it to bytes and back.
    ///
    /// Split out because the two callers differ in where the key comes from,
    /// never in what it means: the public entry point receives it per call from
    /// the app, a gesture reads the one injected by
    /// [`MediaMachine::set_owner_backup_key`]. Both then render identically —
    /// which is the property that keeps a delete from emptying its own set.
    ///
    /// ⚠ Lives in **this** block, not beside `refresh` in the exported one:
    /// `BackupKey` is not FFI-liftable, so `#[uniffi::export]` over a method
    /// taking `Option<BackupKey>` fails to compile — and only under
    /// `--features uniffi`, which a default-feature `cargo test`/`clippy` run
    /// never exercises.
    async fn refresh_with_owner(&self, owner: Option<BackupKey>) {
        let result = match self.nest_api.media_snapshot().await {
            Ok(mut snap) => {
                self.render_sealed_paths(&mut snap, owner.clone()).await;
                self.note_signed_as(&snap.items);
                // The control-plane set list (`fauna.folders.list`) is what
                // makes an EMPTY set choosable — the item aggregate can only
                // ever name sets that already hold media, so deriving the
                // options from it alone left a fresh set unreachable as an
                // upload target and it could never gain its first file
                // (`media.md` § Layout & flow).
                //
                // Deliberately NOT fatal to the page: media is the read that
                // matters, and a failed read (a
                // transient failure) must still render the browse rather than
                // erroring the whole page. Losing the list degrades exactly to
                // the pre-fix item-derived options.
                match self.list_joined_folders().await {
                    Ok(sets) => {
                        snap.known_folders = self.render_sealed_folder_names(sets, owner).await
                    }
                    Err(e) => tracing::warn!(
                        target: "fauna_media",
                        error = %e.detail(),
                        "listing folders failed; media options fall back to the sets that have media",
                    ),
                }
                Ok(snap)
            }
            Err(e) => Err(e),
        };
        // Followed browse-scope options ride the same refresh (best-effort,
        // like the folders list above; the source's staleness budget makes a
        // warm-cache pass cheap). Independent of the aggregate read's outcome:
        // the options render either way.
        let followed_options = match self.followed_source.get() {
            Some(source) => Some(source.followed_scopes().await),
            None => None,
        };
        {
            let mut s = self.state.lock().unwrap();
            if let Some(options) = followed_options {
                s.followed_options = options;
            }
            match result {
                Ok(snap) => {
                    // The read RETURNED — and what it returned is the fact that
                    // separates "the page is empty because this actor has
                    // nothing" from "the page never got an answer". Only the
                    // `Err` arm below is visible today, so a successful-but-empty
                    // read leaves no trace at all and the page simply parks
                    // loaded-and-empty. Counts only (no names, no paths).
                    tracing::debug!(
                        target: "fauna_media",
                        items = snap.items.len(),
                        folders = snap.known_folders.len(),
                        "media refresh returned",
                    );
                    s.raw = snap;
                    s.error = None;
                    // The one place the page becomes loaded. Only a read that
                    // RETURNED may license `media-empty-state`; the `Err` arm
                    // below deliberately leaves this alone (see the field's doc
                    // on `MediaPageSnapshot`).
                    s.loaded = true;
                }
                Err(e) => {
                    let err = error_text(REFRESH_ERROR_KEY, e.detail());
                    // Producer-side log for the reactive error banner — fire once
                    // here, not in the per-tick render (observability.md § Log on
                    // the *event*, not the *paint*). `log_line` is redaction-safe.
                    tracing::warn!(target: "fauna_media", "{}", err.log_line());
                    s.error = Some(err);
                }
            }
        }
        self.observer.on_changed();
    }

    /// The reader's key custody for one folder, plus the set's **home-nest
    /// base URL** when it is a foreign (cross-nest) set.
    ///
    /// One assembly, two consumers — the sealed-label render above and
    /// [`MediaMachine::download_file`]'s byte walk — precisely so the two can
    /// never end up opening a set's names and its bytes under different
    /// custody. `owner` is the per-gesture owner backup key, `None` for a
    /// keyless caller.
    ///
    /// Note the owner key is carried through even for a **bound** set: the
    /// chunk path suppresses it in one place (`effective_backup_key`, FS-5DC),
    /// while the label path deliberately keeps it so a set's owner can still
    /// render the names it sealed under the owner root *before* the set was
    /// bound (`FileDownloadKeys::label_open_roots`).
    ///
    /// The assembly itself lives in [`fauna_core::label_custody::LabelCustody`]
    /// — shared with snapshot browse/diff and the conflict list, which resolve
    /// the identical custody and must not be free to resolve it differently.
    ///
    /// `set` is the set's [`SetKey`]: its row resolves through the resolver's
    /// identity entry point (`LabelCustody::keys_for_ref`), so a reader's own
    /// same-named set never answers for it; the name degrade through the
    /// by-name form (`LabelCustody::keys_for_hash`, the row's `folder_hash`).
    async fn folder_keys_for_key(
        &self,
        set: &SetKey,
        owner: Option<BackupKey>,
    ) -> (
        FileDownloadKeys,
        Option<fauna_core::folder_keys::ForeignHome>,
    ) {
        let custody = fauna_core::label_custody::LabelCustody::new(self.folder_keys.clone(), owner)
            // Read candidates for a successor's predecessor-sealed media; the
            // seal-side twin below deliberately does NOT get them.
            .with_predecessor_keys(self.predecessor_keys());
        match set {
            SetKey::Ref(folder_ref) => custody.keys_for_ref(folder_ref).await,
            SetKey::Name(name_hash) => custody.keys_for_hash(name_hash).await,
        }
    }

    /// Open a version another signature recorded and re-seal it under the
    /// **current** owner root — the only way such a version is restored
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (8)(d), the restore sentence). Answers the `(manifest hash, size,
    /// generation)` the restore records, or the detail it refuses with.
    ///
    /// The bytes open under the signer's own retired root and its
    /// predecessors' alone (`FileDownloadKeys::record_signer`,
    /// [`Self::open_library_blob`]) — exactly the roots ruling (8)(c) lets a
    /// predecessor's signature reach. A version that names bytes the successor
    /// (or a later predecessor) sealed — the planted row this rule exists for —
    /// therefore does not open,
    /// and is refused: nothing is recorded. One that opens is re-sealed whole
    /// as a chunk manifest under the current root — the shape the owner-only
    /// download walk reads — so the head the restore records opens under the
    /// current root *because it was re-sealed*, never because a signature was
    /// swapped.
    ///
    /// Refused outright, as not this owner-root move: an unstamped version of
    /// a content-keyed set (pre-bind content, which only the current
    /// identity's own signature licenses — ruling (5)), and a version of a
    /// folder whose bytes rest unsealed (a re-pointed public head naming
    /// sealed bytes would hand them to the declassification, and re-sealing
    /// into a public folder would hide what it publishes).
    async fn reseal_inherited_version(
        &self,
        set: SetKey,
        path: &str,
        version: &FileVersionSummary,
    ) -> Result<(String, i64, Option<u64>), String> {
        let refused = || RESTORE_INHERITED_UNOPENABLE.to_string();
        // The roots the version's signer may reach — its own and its
        // predecessors' (ruling (8)(c)), never the current one.
        let signer = self.signer_for(set, &version.manifest_hash);
        let owner = self.owner_key().ok_or_else(refused)?;
        let uploader = self.blob_uploader.as_ref().ok_or_else(refused)?;
        let digest: [u8; 32] = hex::decode(&version.manifest_hash)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| "the version has a malformed manifest hash".to_string())?;
        let (keys, _) = self.folder_keys_for_key(&set, Some(owner.clone())).await;
        if keys.is_content_keyed() || self.declassified_folder_id(set).is_some() {
            return Err(refused());
        }
        let seal_root = (owner.convergent_chunk_root(), None);
        let sink = crate::blob_uploader::UploaderSink(uploader.as_ref());

        // A folder file rests as chunks behind a manifest (`media.md`
        // § Encryption at rest → *One at-rest shape*).
        let fetcher = self.download_fetcher.as_ref().ok_or_else(refused)?;
        let record_keys = FileDownloadKeys {
            record_signer: signer,
            ..keys
        };
        let resealed = fauna_core::nest_reseal::reseal_nest_copy_windowed(
            fetcher.as_ref(),
            &record_keys,
            fauna_core::data::ContentHash::from_digest_raw(digest),
            None,
            path,
            Some(seal_root),
            &sink,
        )
        .await
        .map_err(|e| {
            tracing::warn!(
                target: "fauna_media",
                error = %format!("{e:#}"),
                "an inherited version did not open under a retired root; not restored"
            );
            refused()
        })?;
        Ok((
            hex::encode(resealed.manifest_hash.digest()),
            resealed.manifest.total_size as i64,
            None,
        ))
    }

    /// Seal a delete-tombstone / restore-re-point path (S8 D2) — the write-side
    /// twin of the render custody above, assembled from the **injected** owner
    /// key ([`MediaMachine::set_owner_backup_key`]) instead of a per-gesture
    /// one, through the same [`Self::folder_keys_for_key`] so the seal root can
    /// never diverge from the root the page renders under.
    ///
    /// Best-effort like every label seal on a user gesture: `None` records
    /// plaintext-only (an S8 backfill row). A **bound** set whose content keys
    /// the resolver cannot produce fails *closed* to `None` rather than sealing
    /// under an owner root the roster could not open — the mistake class,
    /// and the reason this does NOT copy `upload()`'s inline owner-root shape.
    /// The refusal's mechanism: a bound-but-unresolvable
    /// resolve keeps its `mls_group_id`, so `label_seal_root()`'s bail arm
    /// fires here — before that fix the resolver collapsed the case to
    /// "unbound" and this site stamped under the owner root despite this very
    /// comment; a resolve *failure* likewise yields no keys, never the owner
    /// fallback.
    ///
    /// The seal is the TARGET set's ([`Self::target_set_key`]): the record goes
    /// to the nest by name, which writes it into the caller's own set of that
    /// name first, so this seals under that row's custody.
    async fn seal_gesture_path(&self, folder: &str, path: &str) -> Option<Vec<u8>> {
        self.seal_record_path(self.target_set_key(folder), path)
            .await
    }

    /// [`Self::seal_gesture_path`] for a record whose set is already known —
    /// a restore re-pointing a version into that version's own row.
    async fn seal_record_path(&self, target: SetKey, path: &str) -> Option<Vec<u8>> {
        let (keys, _) = self.folder_keys_for_key(&target, self.owner_key()).await;
        match fauna_core::label_custody::seal_path_from_keys(&keys, path) {
            Ok(bytes) => Some(bytes),
            Err(fauna_core::label_custody::SealPathFromKeysError::NoRoot) => None,
            Err(fauna_core::label_custody::SealPathFromKeysError::RootUnresolved(e)) => {
                tracing::warn!(
                    target: "fauna_media",
                    error = %e,
                    "no seal root for the delete/restore record (bound set, keys \
                     unresolved); recording plaintext-only"
                );
                None
            }
            Err(fauna_core::label_custody::SealPathFromKeysError::SealFailed(e)) => {
                tracing::warn!(
                    target: "fauna_media",
                    error = %e,
                    "sealing the gesture path failed; recording plaintext-only"
                );
                None
            }
        }
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl MediaMachine {
    // ── Read surface ────────────────────────────────────────────────────

    /// The whole renderable Media page in one record: the cross-set items for the
    /// active filter sorted by the active key, the filter options, the view state,
    /// and any error. Filter + sort run in shared Rust
    /// (`MediaSnapshot::view`); `media.md` § Where logic lives.
    pub fn snapshot(&self) -> MediaPageSnapshot {
        let author_held = self.share_author_held();
        let s = self.state.lock().unwrap();
        // A followed browse scope, while active, IS the page's item list —
        // rendered through the same view sort as the aggregate, with `filter`
        // carrying the scope's minted value so the select paints its selection
        // with zero new app logic (`media.md` § Followed public folders).
        let (items, filter, filter_label, followed_scope) = match &s.followed_active {
            Some(active) => {
                let view = MediaSnapshot {
                    items: active.items(),
                    ..MediaSnapshot::default()
                };
                // The option list's verdict is fresher than the one frozen at
                // scope entry — prefer it for the active-scope rendering.
                let available = s
                    .followed_options
                    .iter()
                    .find(|sc| {
                        sc.folder_id == active.scope.folder_id
                            && sc.home_nest_url == active.scope.home_nest_url
                    })
                    .map(|sc| sc.available)
                    .unwrap_or(active.scope.available);
                let mut option = followed_option_of(&active.scope);
                option.available = available;
                let items: Vec<MediaItemSummary> = view
                    .view(None, s.sort, s.descending)
                    .into_iter()
                    .map(|item| MediaItemSummary {
                        filter_value: option.value.clone(),
                        ..MediaItemSummary::from(item)
                    })
                    .collect();
                (
                    items,
                    Some(option.value.clone()),
                    Some(option.label.clone()),
                    Some(option),
                )
            }
            None => {
                let value = s.filter.as_ref().map(FolderFilter::value);
                (
                    s.raw
                        .view(value.as_deref(), s.sort, s.descending)
                        .into_iter()
                        .map(|item| own_summary(&s.raw, item, author_held))
                        .collect(),
                    value.clone(),
                    value.map(|v| s.raw.filter_label(&v)),
                    None,
                )
            }
        };
        MediaPageSnapshot {
            items,
            folders: s.raw.folders(),
            folder_options: s
                .raw
                .folder_options()
                .into_iter()
                .map(|option| MediaFolderOption {
                    name: option.name,
                    folder_id: option.folder_ref.map(|r| r.to_wire()),
                    value: option.value,
                    label: option.label,
                })
                .collect(),
            filter_label,
            followed: s.followed_options.iter().map(followed_option_of).collect(),
            followed_scope,
            sort: s.sort.as_select_value().to_string(),
            descending: s.descending,
            filter,
            view_grid: s.view_grid,
            error: s.error.clone(),
            loaded: s.loaded,
            share_create: s.share_create.as_ref().map(|c| ShareCreateSnapshot {
                name: c.name.clone(),
                expiry: c.expiry.clone(),
                busy: c.busy,
                url: c.url.clone(),
                key_in_fragment: c.key_in_fragment,
            }),
            share_expiry_options: fauna_client_share::EXPIRY_OPTIONS
                .iter()
                .map(|o| o.value.to_string())
                .collect(),
            share_links: s.share_links.clone(),
        }
    }

    /// The item a **durable identity pair** names — the set's stable
    /// `FolderSummary.id` plus the file's `path_hash` (hex-lowercase) — for a
    /// reference minted off this page, today `SearchNav::File`
    /// (`ui/search.md` § Implementation status today).
    ///
    /// Runs over the **raw** aggregate rather than [`Self::snapshot`]'s view, and
    /// that is the point: a deep link must not depend on the active
    /// `media-folder-filter` or sort, which are the user's browse state and say
    /// nothing about what they just asked to open. `fauna.media.list` is drained
    /// to exhaustion (`MediaClient::media_snapshot`), so the raw aggregate really
    /// is every readable item — this lookup has no "not paged in yet" case.
    ///
    /// The join itself is `MediaSnapshot::locate_file`, which owns why it is a
    /// lookup rather than a spelling match. `None` = deleted, renamed, or in a
    /// set this caller cannot see.
    pub fn locate_file(&self, folder_id: i64, path_hash: String) -> Option<MediaItemSummary> {
        let author_held = self.share_author_held();
        let s = self.state.lock().unwrap();
        s.raw
            .locate_file(folder_id, &path_hash)
            .cloned()
            .map(|item| own_summary(&s.raw, item, author_held))
    }

    /// The item at folder-relative `path` of the set whose durable id is
    /// `folder_id` — [`Self::locate_file`]'s sibling for a reference that
    /// carries the plaintext path, today the Windows Explorer Share leaf's
    /// `fauna://share-link` route (`docs/goal/architecture/apps/windows.md`
    /// § Shell Extension → *The Share hand-off*). Over the raw aggregate for
    /// the same reason as `locate_file`; `MediaSnapshot::locate_path` owns the
    /// join. `None` = deleted, renamed, or in a set this caller cannot see.
    pub fn locate_path(&self, folder_id: i64, path: String) -> Option<MediaItemSummary> {
        let author_held = self.share_author_held();
        let s = self.state.lock().unwrap();
        s.raw
            .locate_path(folder_id, &path)
            .cloned()
            .map(|item| own_summary(&s.raw, item, author_held))
    }

    // ── Gestures ────────────────────────────────────────────────────────

    /// Re-read the cross-set all-media aggregate (`fauna.media.list`, paged). On a
    /// read failure the prior data is kept and the page error is set; on success
    /// the error clears. The view state (sort / filter / toggle) is untouched.
    /// Notifies once.
    ///
    /// `backup_key` is the client's 32-byte owner backup key — the same
    /// per-gesture secret injection [`MediaMachine::download_file`] takes, for
    /// the same reason: the machine outlives the gesture and must not hold the
    /// key. Passing it enables the **sealed-first render** of each item's path
    /// (`docs/goal/behavior/file-sync.md` § Sealed names & paths), which is the
    /// only render that keeps working once the plaintext column is scrubbed at
    /// the flip.
    ///
    /// `None` is the plaintext-only path and is *exactly* today's behavior —
    /// which is why every app still compiles and behaves identically after this
    /// slice. Swapping each app's call to pass its key is S3.
    pub async fn refresh(&self, backup_key: Option<Vec<u8>>) {
        let owner = backup_key
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .map(BackupKey::from_bytes);
        self.refresh_with_owner(owner).await;
    }

    /// Upload `raw_bytes` (the picked file) into `folder` as `path` from the
    /// write-capable `device_id`, sealed under the custody root the folder
    /// names — the owner's `backup_key` for an owner-only set (`media.md`
    /// § Encryption at rest → *One at-rest shape*). The whole gesture is shared
    /// Rust: strip + seal (`process_media`, `seal_blob`), POST the chunks and
    /// manifest (and the thumbnail blob) over the bulk-binary seam, record
    /// the manifest member (`fauna.sync.changes.record`), then `refresh()` so the
    /// new item appears. On any failure the page error is set (the
    /// `error-message` element) and prior data is kept; notifies once.
    ///
    /// `backup_key` is the client's 32-byte owner backup key
    /// (`backup_key_derive`). The realistic target is a **Sync**-mode set (a
    /// backup-type set records to `backup_custody`, not yet visible in Media). On
    /// a platform with no blob uploader wired `upload` reports a `Transient`
    /// "not supported" error rather than uploading. A **metadata-only** folder
    /// (its content stays on the user's devices) is refused first, before
    /// anything is sealed or sent: this door keeps no copy, so the bytes would
    /// rest on the nest against the folder's promise
    /// ([`METADATA_ONLY_FOLDER_ERROR_KEY`]).
    ///
    /// `folder` is a filter value: an own set's name, or a shared set's ref
    /// (`media.md` § Encryption at rest → *The filter and the upload target
    /// address the row*, (e)); a shared set the caller may only read is
    /// refused before anything is sealed ([`READ_ONLY_SHARED_FOLDER_ERROR_KEY`]).
    pub async fn upload(
        &self,
        folder: String,
        device_id: String,
        path: String,
        raw_bytes: Vec<u8>,
        backup_key: Vec<u8>,
    ) {
        match self.resolve_target(&FolderFilter::parse(&folder)) {
            Some((name, target)) => {
                self.upload_into(name, target, device_id, path, raw_bytes, backup_key)
                    .await
            }
            None => self.set_error_no_upload_target(),
        }
    }

    /// Upload `raw_bytes` (the picked file's contents) as `path` into the
    /// **currently selected** set, then refresh — the cross-app "upload into
    /// the selected folder" gesture (`media.md` § Layout & flow / § User
    /// actions). The target is the `media-folder-filter` set — by its value, so
    /// a selected shared set is that row, never the caller's same-named own
    /// set — or, in the all-media view (no single set selected), the **first
    /// own set** ([`MediaSnapshot::default_upload_target`] — empty or not,
    /// never a shared one: `media.md` § Encryption at rest → *The filter and
    /// the upload target address the row*, (d)) — the goal doc's "the client
    /// … defaults the target set; the upload always lands in exactly one set."
    /// When the caller has no own folder at all the page error is set and
    /// nothing is uploaded.
    ///
    /// The default used to be "the first set that *has media*", which made a
    /// brand-new empty set impossible to upload into: it could not be defaulted
    /// to (no media) and could not be selected (the filter listed only sets with
    /// media), so the one action that would have given it media was the one
    /// action it blocked.
    /// Otherwise delegates to [`MediaMachine::upload`] (seal → POST → record →
    /// refresh). Resolving the target here — rather than in each app's UI —
    /// keeps the "which set" policy identical on every app (priority #1).
    pub async fn upload_selected(
        &self,
        device_id: String,
        path: String,
        raw_bytes: Vec<u8>,
        backup_key: Vec<u8>,
    ) {
        let (followed_active, target) = {
            let s = self.state.lock().unwrap();
            (
                s.followed_active.is_some(),
                s.filter
                    .as_ref()
                    .map(FolderFilter::value)
                    .or_else(|| s.raw.default_upload_target()),
            )
        };
        // A followed browse scope is structurally read-only, and while one is
        // active `filter` still holds whatever set the user browsed BEFORE
        // entering it — resolving a target from it would land the file in a
        // set the user is not even looking at.
        if followed_active {
            let err = LocalizedText::key(FOLLOWED_READ_ONLY_ERROR_KEY);
            tracing::warn!(target: "fauna_media", "{}", err.log_line());
            self.state.lock().unwrap().error = Some(err);
            self.observer.on_changed();
            return;
        }
        match target {
            Some(folder) => {
                self.upload(folder, device_id, path, raw_bytes, backup_key)
                    .await
            }
            None => self.set_error_no_upload_target(),
        }
    }

    /// Wire the owner `BackupKey` the delete / restore gestures seal their
    /// records with (S8 D2 — see the `owner_backup_key` field doc). Called once
    /// by the platform glue with the same 32 raw bytes `upload()` takes per
    /// call. Best-effort like the gestures themselves: a wrong-length key is
    /// ignored with a warning and delete/restore then record plaintext-only
    /// (S8 backfill rows, the unwired-app behavior) — never a failed page.
    pub fn set_owner_backup_key(&self, key: Vec<u8>) {
        let Ok(bytes) = <[u8; 32]>::try_from(key.as_slice()) else {
            tracing::warn!(
                target: "fauna_media",
                "owner backup key is not 32 bytes; delete/restore will record plaintext-only"
            );
            return;
        };
        *self.owner_backup_key.lock().unwrap() =
            Some(fauna_core::crypto::BackupKey::from_bytes(bytes));
    }

    /// Wire the account's retired owner `BackupKey`s so a successor can still
    /// render and download the media it inherited — see the
    /// `predecessor_backup_keys` field doc for the read-only contract.
    ///
    /// Resolve them from `AccountRegistry::predecessor_backup_keys` (the one
    /// shared walk) rather than deriving per app. A wrong-length key is skipped
    /// with a warning rather than failing the page: the honest degrade for one
    /// unusable ancestor is "that ancestor's rows stay dark", which is what an
    /// unwired app already sees.
    pub fn set_predecessor_backup_keys(&self, keys: Vec<Vec<u8>>) {
        let parsed: Vec<fauna_core::crypto::BackupKey> = keys
            .into_iter()
            .filter_map(|k| match <[u8; 32]>::try_from(k.as_slice()) {
                Ok(bytes) => Some(fauna_core::crypto::BackupKey::from_bytes(bytes)),
                Err(_) => {
                    tracing::warn!(
                        target: "fauna_media",
                        "predecessor backup key is not 32 bytes; that ancestor's                          media stays unopenable"
                    );
                    None
                }
            })
            .collect();
        *self.predecessor_backup_keys.lock().unwrap() =
            parsed.into_iter().map(PredecessorSealKey::from).collect();
    }

    /// Wire the account's retired owner `BackupKey`s **paired with the
    /// identities they belong to**, nearest hop first — `actor_ids[i]` is the
    /// identity `keys[i]` belongs to, exactly as
    /// `AccountRegistry::predecessor_backup_keys_by_actor` yields them. The
    /// form every app with a registry hands over: only a paired key opens a
    /// row signed as a predecessor, and only that identity's root and its
    /// predecessors' are offered to it (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*, ruling (8)(c)); an unpaired key
    /// ([`Self::set_predecessor_backup_keys`]) still opens what the current
    /// identity signed. Replaces either earlier call's keys, and adds the ids
    /// to the attested set ([`Self::set_predecessor_actor_ids`]). A pair with
    /// a wrong-length half is skipped — that ancestor's rows stay dark; lists
    /// of different lengths pair nothing, since a positional slip would hand
    /// one identity another's root.
    pub fn set_predecessor_chain(&self, actor_ids: Vec<Vec<u8>>, keys: Vec<Vec<u8>>) {
        let Some(chain) = PredecessorSealKey::chain_from_wire(&actor_ids, &keys) else {
            tracing::warn!(
                target: "fauna_media",
                ids = actor_ids.len(),
                keys = keys.len(),
                "predecessor chain: ids and keys differ in length; pairing nothing"
            );
            return;
        };
        let ids: Vec<[u8; 32]> = chain
            .iter()
            .filter_map(|k| k.actor_id.map(|a| a.0))
            .collect();
        *self.predecessor_backup_keys.lock().unwrap() = chain;
        let all = {
            let mut held = self.predecessor_ids.lock().unwrap();
            for id in ids {
                if !held.contains(&id) {
                    held.push(id);
                }
            }
            held.clone()
        };
        self.nest_api.set_reader_predecessors(all);
    }

    /// Wire the account's **attested** predecessor actor ids —
    /// `AccountRegistry::attested_predecessor_actor_ids`, the same walk
    /// [`Self::set_predecessor_backup_keys`] takes its keys from — so the
    /// listing's judge reads a row signed under a retired identity as this
    /// account's own (`mls-group-key-material.md` § M2 → *Writer-signed change
    /// records*, ruling (8)(b)). Without it the inherited corpus still lists:
    /// the judge proves the link itself from the landed succession statements,
    /// at the cost of one lookup. A wrong-length id is skipped.
    pub fn set_predecessor_actor_ids(&self, ids: Vec<Vec<u8>>) {
        let parsed: Vec<[u8; 32]> = ids
            .into_iter()
            .filter_map(|id| <[u8; 32]>::try_from(id.as_slice()).ok())
            .collect();
        *self.predecessor_ids.lock().unwrap() = parsed.clone();
        self.nest_api.set_reader_predecessors(parsed);
    }

    // ── Share links (`share-links.md` § Flows) ────────────────────────────

    /// Wire the identity share links are made under: the account's 32-byte
    /// identity `secret` (signs the token, derives the filename-seal root) and
    /// the nest base URL the links point at. The retired owner keys already
    /// injected ([`Self::set_predecessor_backup_keys`]) open old list names.
    /// A wrong-length secret is ignored with a warning (the share gestures
    /// then report their error — never a mint under a wrong key).
    pub fn set_share_author(&self, secret: Vec<u8>, nest_url: String) {
        let Ok(bytes) = <[u8; 32]>::try_from(secret.as_slice()) else {
            tracing::warn!(target: "fauna_media", "share author secret is not 32 bytes");
            return;
        };
        let author = fauna_client_share::ShareAuthor::new(bytes, nest_url).with_predecessors(
            self.predecessor_keys()
                .into_iter()
                .filter_map(|k| k.key.client_key().cloned())
                .collect(),
        );
        *self.share_author.lock().unwrap() = Some(Arc::new(author));
        // An owner-only file's link is offered only once this seat holds the
        // root (`share_facts`), so the verdict the page shows moves here.
        self.observer.on_changed();
    }

    /// Whether a share author is wired — the seat holds the owner root a
    /// private link derives its chunk keys from. Read BEFORE taking the state
    /// lock (the two mutexes are never held together).
    fn share_author_held(&self) -> bool {
        self.share_author.lock().unwrap().is_some()
    }

    /// Open the create surface on the file at `path` of `folder` — a no-op
    /// unless the item is in the browse and eligible (the control is absent
    /// elsewhere; `share-links.md` § Which files can be linked). The expiry
    /// starts at the default.
    pub fn open_share_create(&self, folder: String, path: String) {
        let author_held = self.share_author_held();
        {
            let mut s = self.state.lock().unwrap();
            let Some(item) = s
                .raw
                .items
                .iter()
                .find(|i| i.folder == folder && i.path == path)
            else {
                return;
            };
            let facts = share_facts(
                &s.raw,
                SetKey::of(item.folder_id, &item.folder),
                author_held,
            );
            if !fauna_client_share::share_link_eligible(&facts) {
                return;
            }
            let name = fauna_client_media::display_name(item).to_string();
            s.share_create = Some(ShareCreate {
                folder,
                path,
                name,
                expiry: fauna_client_share::DEFAULT_EXPIRY.to_string(),
                busy: false,
                url: None,
                // A public folder's bytes rest in the clear and take the
                // public arm; every other eligible file rests sealed.
                key_in_fragment: !facts.public_audience,
            });
        }
        self.observer.on_changed();
    }

    /// `share-link-expiry-select` — a value outside the option list is
    /// refused (no silent default), as is a change after the URL is shown.
    pub fn set_share_expiry(&self, value: String) {
        if fauna_client_share::expiry_secs(&value).is_none() {
            return;
        }
        {
            let mut s = self.state.lock().unwrap();
            match s.share_create.as_mut() {
                Some(c) if c.url.is_none() => c.expiry = value,
                _ => return,
            }
        }
        self.observer.on_changed();
    }

    /// `share-link-cancel-button` — close the create surface (also the close
    /// after success).
    pub fn close_share_create(&self) {
        self.state.lock().unwrap().share_create = None;
        self.observer.on_changed();
    }

    /// `share-link-create-button` — read the file's current version, mint +
    /// sign the token, seal the filename, register (`fauna.share.create`), and
    /// reveal the URL **only** once registration succeeded. On failure the
    /// minted token is dropped here (never shown, never logged), the page
    /// error says why, and the surface stays open for a retry.
    pub async fn create_share_link(&self) {
        let Some((folder, path, name, expiry, key_in_fragment)) = ({
            let mut s = self.state.lock().unwrap();
            match s.share_create.as_mut() {
                Some(c) if !c.busy && c.url.is_none() => {
                    c.busy = true;
                    Some((
                        c.folder.clone(),
                        c.path.clone(),
                        c.name.clone(),
                        c.expiry.clone(),
                        c.key_in_fragment,
                    ))
                }
                _ => None,
            }
        }) else {
            return;
        };
        self.observer.on_changed();
        let outcome = self
            .mint_and_register(&folder, &path, &name, &expiry, key_in_fragment)
            .await;
        let still_open = {
            let mut s = self.state.lock().unwrap();
            match s.share_create.as_mut() {
                Some(c) if c.folder == folder && c.path == path => {
                    c.busy = false;
                    if let Ok(url) = &outcome {
                        c.url = Some(url.clone());
                        s.error = None;
                    }
                    true
                }
                _ => false,
            }
        };
        match outcome {
            Ok(_) => {
                self.observer.on_changed();
                // A new link belongs in an open list straight away.
                if self.state.lock().unwrap().share_links.open {
                    self.load_share_links().await;
                }
            }
            Err(detail) => {
                if still_open {
                    self.set_error(SHARE_CREATE_ERROR_KEY, &detail);
                } else {
                    self.observer.on_changed();
                }
            }
        }
    }

    /// `share-link-list-button` — open the list and load it
    /// (`fauna.share.list`). The list starts unloaded, so it paints neither
    /// rows nor its empty state until the read returns.
    pub async fn open_share_links(&self) {
        {
            let mut s = self.state.lock().unwrap();
            s.share_links = ShareLinksSnapshot {
                open: true,
                ..ShareLinksSnapshot::default()
            };
        }
        self.observer.on_changed();
        self.load_share_links().await;
    }

    /// `share-link-list-close-button`.
    pub fn close_share_links(&self) {
        self.state.lock().unwrap().share_links = ShareLinksSnapshot::default();
        self.observer.on_changed();
    }

    /// `share-link-revoke-button` — arm the single confirm for an Active row.
    pub fn arm_share_revoke(&self, token_id: String) {
        {
            let mut s = self.state.lock().unwrap();
            let active = s
                .share_links
                .rows
                .iter()
                .any(|r| r.token_id == token_id && r.state == "active");
            if !active {
                return;
            }
            s.share_links.revoke_confirm = Some(token_id);
        }
        self.observer.on_changed();
    }

    /// `share-link-revoke-cancel-button`.
    pub fn cancel_share_revoke(&self) {
        self.state.lock().unwrap().share_links.revoke_confirm = None;
        self.observer.on_changed();
    }

    /// `share-link-revoke-confirm-button` — `fauna.share.revoke` the armed
    /// row, then re-list (the row stays, as Revoked). No un-revoke.
    pub async fn confirm_share_revoke(&self) {
        let Some(token_id) = self.state.lock().unwrap().share_links.revoke_confirm.take() else {
            return;
        };
        self.observer.on_changed();
        match self.nest_api.share_revoke(&token_id).await {
            Ok(()) => self.load_share_links().await,
            Err(e) => self.set_error(SHARE_REVOKE_ERROR_KEY, e.detail()),
        }
    }

    /// Delete the member at `path` of `folder` (tombstone via
    /// `fauna.sync.changes.record`, `change_type = "delete"`) from the
    /// write-capable `device_id`, then `refresh()`. On failure the page error is
    /// set and prior data is kept; notifies once. The tombstone's seal contract
    /// is [`Self::set_owner_backup_key`]'s doc.
    ///
    /// `folder` is the item's set as a filter value — the item's
    /// `filter_value` (a shared set's ref) or a set's name — resolved like the
    /// upload target ([`Self::resolve_target`]): the tombstone names that row
    /// and seals under its custody (`set-addressing.md` § The row form →
    /// *Whoever holds the row sends it*); a name is the caller's own set of
    /// that name, else the name degrade. A ref the page no longer lists names
    /// nothing, so nothing is written.
    pub async fn delete(&self, folder: String, device_id: String, path: String) {
        let Some((name, target)) = self.resolve_target(&FolderFilter::parse(&folder)) else {
            self.set_error(DELETE_ERROR_KEY, &format!("no listed set is {folder}"));
            return;
        };
        let path_sealed = self.seal_record_path(target, &path).await;
        let folder_id = target.local_row();
        match self
            .nest_api
            .delete_member(&name, folder_id, &device_id, &path, path_sealed)
            .await
        {
            // Re-render under the custody this gesture just SEALED with, not
            // keyless: a keyless re-render omits every row the injected owner
            // key was opening, and — via `SetNameRender::Omit` — every row of
            // an owner-named set, so deleting one of two files empties the
            // whole set from the library. `refresh(None)` here was that bug.
            Ok(()) => self.refresh_with_owner(self.owner_key()).await,
            Err(e) => self.set_error(DELETE_ERROR_KEY, e.detail()),
        }
    }

    /// The version history of the file at `path` in `folder`, oldest→newest —
    /// the `file-version-history` rows for the `media-item-detail` surface
    /// (media.md § Element IDs; semantics `file-sync.md` § File Versions).
    /// History is a projection over the recorded change history, so every change
    /// ever recorded is a version; the `path_hash` derivation happens in shared
    /// Rust inside the seam. Like [`MediaMachine::fetch_thumbnail`], this is a
    /// **per-item query**: it returns the versions (or the error) to the caller
    /// and never touches the page `error-message` banner.
    /// `include_pruned` — `true` also returns soft-pruned rows (each carrying
    /// `pruned` + `purge_after`), the `file-version-show-pruned-toggle`
    /// recovery browse (`file-versions.md` § Retention (3)); `false` = the
    /// live-only listing.
    ///
    /// `folder` is the item's set as a filter value, resolved like
    /// [`Self::delete`]'s: a ref lists that row's history, a name the
    /// caller's own set of that name, else the name degrade
    /// (`set-addressing.md` § The row form → *Whoever holds the row sends
    /// it*). A ref the page no longer lists answers `NotFound`.
    pub async fn file_versions(
        &self,
        folder: String,
        path: String,
        include_pruned: bool,
    ) -> Result<Vec<FileVersionSummary>, MediaApiError> {
        let Some((folder, target)) = self.resolve_target(&FolderFilter::parse(&folder)) else {
            return Err(MediaApiError::NotFound {
                detail: format!("no listed set is {folder}"),
            });
        };
        let listed = self
            .nest_api
            .file_versions(&folder, target.local_row(), &path, include_pruned)
            .await?;
        // Who signed each version waits here for the download or restore that
        // names its manifest (ruling (8)(c)/(d)).
        // Each under its own row — the key the restore of that version derives
        // too (`Self::version_set_key`).
        let chain = self.predecessor_keys();
        let noted: Vec<(SetKey, RecordSigner)> = listed
            .iter()
            .map(|v| {
                (
                    self.version_set_key(&folder, &path, &v.summary),
                    self.signer_of(v.signed_as_current, v.signed_as),
                )
            })
            .collect();
        let mut ledger = self.signed_as.lock().unwrap();
        Ok(listed
            .into_iter()
            .zip(noted)
            .map(|(version, (set, signer))| {
                ledger.note(set, &version.summary.manifest_hash, signer, &chain);
                version.summary
            })
            .collect())
    }

    /// Restore a soft-pruned version of the file at `path` to the listable
    /// population — the `file-version-undelete-button` gesture
    /// (`file-versions.md` § Retention (3), the snapshot-undelete twin on the
    /// version plane). Like [`MediaMachine::file_versions`] this is a
    /// **per-item query**: it returns the outcome to the caller — who re-lists
    /// on success and surfaces the error on the page's own `error-message` —
    /// and never touches the machine's page-level banner itself.
    pub async fn undelete_version(
        &self,
        path: String,
        version_num: i64,
    ) -> Result<(), MediaApiError> {
        self.nest_api.undelete_version(&path, version_num).await
    }

    /// Restore `version` of the file at `path` in `folder` — the
    /// `file-version-restore-confirm-button` gesture. Records an ordinary
    /// `modify` re-pointing the file at the historical manifest (metadata-only,
    /// no byte re-upload; propagates to every member device; appends a NEW
    /// version, so it is reversible — `file-sync.md` § Restore), carrying the
    /// version's `content_key_version` verbatim, then `refresh()`. On failure
    /// the page error is set and prior data is kept; notifies once.
    pub async fn restore_version(
        &self,
        folder: String,
        device_id: String,
        path: String,
        version: FileVersionSummary,
    ) {
        // The version is read under its own row's custody and verdicts, and
        // re-pointed into that row (`Self::version_set_key`): a writer
        // member's restore of a shared set's version lands there, never in a
        // same-named own set the name resolves to first.
        let set = self.version_set_key(&folder, &path, &version);
        let folder_id = version.folder_id;
        // Same seal contract as `delete` — the re-point row is append-only
        // nest-side, so the gesture is this row's only chance to seal. A
        // version naming its row seals under that row's custody; one without
        // goes by name, so it seals for the set the name writes into.
        let path_sealed = match folder_id {
            Some(_) => self.seal_record_path(set, &path).await,
            None => self.seal_gesture_path(&folder, &path).await,
        };
        // Ruling (8)(d): a restore re-signs the historical manifest under the
        // CURRENT identity, and a head so signed is offered the current owner
        // root ever after. So a version some other signature recorded — a
        // predecessor's — whose bytes rest under an owner root is restored
        // only once they have opened under a root that signature may reach,
        // re-sealed under the current one. (A stamped version opens under its
        // content-key generation whoever re-signs it, never under an owner
        // root — every reader binds the root to the stamp, ruling (10)(c) —
        // so it re-points verbatim.)
        //
        // The branch is the one decision every restore door shares
        // (`fauna_core::restore_branch`, ruling (10)(b)); this
        // machine's part is the verdict memory it reads — the `(set,
        // manifest)` ledger, the current identity winning — and the byte seam
        // that performs the re-seal the other doors can only refuse.
        use fauna_core::restore_branch::{RestoreDecision, listed_restore_decision};
        let current_vouches = self
            .signer_for(set, &version.manifest_hash)
            .offers_current();
        let (manifest_hash, size_bytes, content_key_version) =
            match listed_restore_decision(version.content_key_version, current_vouches) {
                RestoreDecision::NeedsReseal => {
                    match self.reseal_inherited_version(set, &path, &version).await {
                        Ok(resealed) => resealed,
                        Err(detail) => {
                            self.set_error(RESTORE_ERROR_KEY, &detail);
                            return;
                        }
                    }
                }
                // A listed version is a version (the judged listing kept it),
                // so the listed decision never refuses.
                RestoreDecision::Verbatim | RestoreDecision::Refuse => (
                    version.manifest_hash,
                    version.size_bytes,
                    version.content_key_version,
                ),
            };
        match self
            .nest_api
            .restore_member(
                &folder,
                folder_id,
                &device_id,
                &path,
                manifest_hash,
                size_bytes,
                content_key_version,
                path_sealed,
            )
            .await
        {
            // Same custody contract as `delete` above — a keyless re-render
            // drops the row the restore just re-pointed.
            Ok(()) => self.refresh_with_owner(self.owner_key()).await,
            Err(e) => self.set_error(RESTORE_ERROR_KEY, e.detail()),
        }
    }

    /// Fetch + decrypt the thumbnail blob at `thumbnail_hash` (a
    /// `MediaItemSummary.thumbnail_hash`) into paint-ready image bytes, so every
    /// app just paints decoded bytes (priority #2 — the whole render fetch is
    /// shared Rust, not hand-rolled per client). The flow:
    ///
    /// 1. `GET /api/v1/blob/<thumbnail_hash>` **direct-by-hash** over the
    ///    bulk-binary seam — **not** `?thumb=1`: a Media-library / device-synced
    ///    thumbnail has no `/api/v1/blob` primary with `blob_metadata` for
    ///    `?thumb=1` to resolve, and the client already holds the thumbnail's own
    ///    hash (`media.md` § Implementation status (a)).
    /// 2. Verify the content address — `blake3(fetched sealed bytes)` must equal
    ///    the requested `thumbnail_hash`, else reject. This is what rejects a
    ///    substituted blob: the backup-chunk AEAD frame carries no AAD, so the tag
    ///    proves only *sealed-under-the-owner-key*, not *this* blob (a malicious
    ///    nest could otherwise serve any other owner-sealed blob and it would
    ///    decrypt cleanly).
    /// 3. Decrypt under the owner's `backup_key` — the audience its producer
    ///    sealed under, for every thumbnail EXCEPT one belonging to a folder the
    ///    owner declassified: those rest plaintext (`media.md` § Encryption at
    ///    rest → *Public-audience folders*) and are returned as fetched, since
    ///    there is no seal to open and attempting one would paint the
    ///    placeholder instead of the image. The item is identified by the very
    ///    hash step 2 verified, so the branch cannot widen what is rendered.
    ///
    /// `backup_key` is the client's 32-byte owner backup key (`backup_key_derive`)
    /// — injected per call, as `upload()` takes it (the machine holds no key).
    ///
    /// Unlike the page gestures, this is a **per-item query**: it returns the
    /// decoded bytes (or the error) to the caller and does **not** touch the page
    /// `error-message` banner — one unreadable thumbnail must not blank the page;
    /// the client falls back to the placeholder. On a platform with no blob
    /// fetcher wired (web, until its wasm coordinator) it returns a `Transient`
    /// "not supported" error.
    pub async fn fetch_thumbnail(
        &self,
        thumbnail_hash: String,
        backup_key: Vec<u8>,
    ) -> Result<Vec<u8>, MediaApiError> {
        // Fail fast if no blob fetcher is wired on this platform — before any work.
        let fetcher = self
            .blob_fetcher
            .as_ref()
            .ok_or_else(|| MediaApiError::Transient {
                detail: "media thumbnail fetch is not yet supported on this platform".to_string(),
            })?;

        // A wrong key length is a caller bug → BadRequest (no fetch attempted).
        let key_bytes: [u8; 32] =
            backup_key
                .try_into()
                .map_err(|v: Vec<u8>| MediaApiError::BadRequest {
                    detail: format!("backup key must be 32 bytes, got {}", v.len()),
                })?;

        // Fetch the stored (sealed) bytes by hash.
        let sealed = fetcher.fetch_blob(thumbnail_hash.clone()).await?;

        // Content-address check — the one thing standing between a malicious /
        // buggy nest and a wrong-image render. The nest stores an AEAD-sealed blob
        // verbatim, so `blake3(sealed)` MUST equal the hash we asked for. The
        // backup-chunk frame carries **no AAD** (`fauna_core::crypto`), so its tag
        // proves only *sealed-under-this-owner-key*, NOT *this specific blob* —
        // without this check the nest could serve any OTHER blob sealed under the
        // owner's own `backup_key` (a different thumbnail, or a full backup-file
        // chunk) and it would decrypt cleanly, painting the wrong owner content.
        // `blake3::Hash::to_hex()` is canonical lowercase hex, matching the nest's
        // `hex::encode(blake3(..))`; compare case-insensitively for robustness.
        //
        if !blake3::hash(&sealed)
            .to_hex()
            .as_str()
            .eq_ignore_ascii_case(&thumbnail_hash)
        {
            return Err(MediaApiError::BadRequest {
                detail: "thumbnail content-hash mismatch: the nest served a different blob"
                    .to_string(),
            });
        }

        // Decrypt under the owner key — or, for a successor still holding a
        // thumbnail its predecessor sealed, under a retired root. The address
        // check above has already pinned these bytes, so the candidates widen
        // only the key, never the content ([`Self::open_library_blob`]).
        // A thumbnail seals under the folder's audience (`do_upload`, and the
        // engine's twin), so one that belongs to a declassified folder is
        // already the plaintext JPEG — there is nothing to open, and trying
        // would fail the AEAD and paint the placeholder instead of the image.
        // The item is found by the hash this call already verified above, so the
        // lookup cannot widen what is rendered: it only says how to read bytes
        // whose address is settled.
        // The verdict is the item's own row's, so a same-named public set
        // never passes a shared set's sealed thumbnail off as public bytes.
        let set = {
            let st = self.state.lock().unwrap();
            st.raw
                .items
                .iter()
                .find(|it| {
                    it.thumbnail_hash
                        .as_deref()
                        .is_some_and(|h| h.eq_ignore_ascii_case(&thumbnail_hash))
                })
                .map(|it| SetKey::of(it.folder_id, &it.folder))
        };
        if set.is_some_and(|set| self.declassified_folder_id(set).is_some()) {
            return Ok(sealed);
        }

        let key = BackupKey::from_bytes(key_bytes);
        // A thumbnail only a predecessor's signature names is never offered
        // the current root (ruling (8)(c)).
        let signer = self
            .signed_as
            .lock()
            .unwrap()
            .thumbnail_signer(&thumbnail_hash);
        self.open_library_blob(&key, &sealed, "thumbnail", signer)
    }

    /// Download + decrypt one file's **full plaintext bytes** by a version's
    /// recorded `manifest_hash` — the per-item *download* `media.md` § User
    /// actions puts on the detail surface.
    ///
    /// The recorded hash names a **chunk-store manifest** — every producer
    /// records a folder file as sealed chunks behind one (`media.md`
    /// § Encryption at rest → *One at-rest shape*) — resolved via the one
    /// shared single-file walk
    /// (`fauna_core::file_download::download_file_bytes_by_manifest` over the
    /// injected `download_fetcher` field; `ui/backups.md`
    /// § Where logic lives → *Single-file byte download*).
    ///
    /// `manifest_hash` is **hex**, exactly as [`FileVersionSummary`] carries it
    /// (the latest version is the current file). `content_key_version` is that
    /// version's stamp — which generation to open under, on every read: a
    /// stamped version opens under that generation or not at all, never under
    /// the owner key, on an owner-only set too (ruling (10)(c)). `folder` is the
    /// set name ([`MediaItemSummary::folder`]): it routes to the content-key
    /// resolver so a **shared** set opens under content keys from custody, while an
    /// owner-only set opens under the owner `BackupKey` (Phase 0 — the read leg,
    /// `folders.md` § Sharing). `relative_path` is the member path (error context
    /// and path guard). `backup_key` is the client's 32-byte owner backup key,
    /// injected per call like every keyed gesture here (used only by the owner-key
    /// path; a shared set's chunks are content-keyed).
    ///
    /// Like `fetch_thumbnail`, this is a **per-item query**: it returns the
    /// bytes (or the error) to the caller and never touches the page
    /// `error-message` banner. The whole file lands **in memory** (the walk's
    /// documented contract) — fine for typical media; a streaming
    /// composition is the follow-on for very large files.
    pub async fn download_file(
        &self,
        manifest_hash: String,
        content_key_version: Option<u64>,
        folder: String,
        relative_path: String,
        backup_key: Vec<u8>,
    ) -> Result<Vec<u8>, MediaApiError> {
        // A wrong key length / non-hex hash is a caller bug → BadRequest, no fetch.
        let key_bytes: [u8; 32] =
            backup_key
                .try_into()
                .map_err(|v: Vec<u8>| MediaApiError::BadRequest {
                    detail: format!("backup key must be 32 bytes, got {}", v.len()),
                })?;
        let digest: [u8; 32] = hex::decode(&manifest_hash)
            .map_err(|e| MediaApiError::BadRequest {
                detail: format!("manifest hash is not hex: {e}"),
            })?
            .try_into()
            .map_err(|v: Vec<u8>| MediaApiError::BadRequest {
                detail: format!("manifest hash must be 32 bytes, got {}", v.len()),
            })?;
        let key = BackupKey::from_bytes(key_bytes);

        // Shared-set path (Phase 0 — the read leg). A bound shared folder's
        // chunks are content-keyed, never owner-keyed, so *any* reader (the owner
        // reading their own shared set, or a member) opens under the content keys
        // in their `fauna.state.folder-keys` custody — resolved here by the item's row — and the
        // set's readable content lives in the chunk store (the walk).
        // `content_open_roots` fails closed on
        // a generation this holder lacks (a removed member, or an unsynced
        // rotation): no plaintext, no owner-key fall-through (FS-BIND-5). An
        // owner-only (unbound) set takes the owner-key path below — guaranteed
        // by the resolver's three-valued contract since a fix: `Ok(None)` is
        // a POSITIVE unbound answer (an owned roster row named this never falls
        // through to the foreign-record lookup, so a same-named foreign set can
        // no longer route this set's fetches to a stranger's nest), and a
        // resolve *failure* yields no keys at all rather than masquerading as
        // unbound.
        //
        // Custody, signer and hint all key by the downloaded item's own row
        // (`media.md` § Encryption at rest → *A set is addressed by its row*),
        // so a reader's own same-named set never answers for a shared one.
        let set = self.item_set_key(&folder, &relative_path, Some(&manifest_hash));
        let (keys, foreign_home) = self.folder_keys_for_key(&set, Some(key.clone())).await;
        // Ruling (8)(c): the row naming this manifest in this set decides
        // whether the current owner root may open it. A row signed under a
        // retired identity gets the retired roots alone, on every arm below.
        let signer = self.signer_for(set, &manifest_hash);
        let keys = FileDownloadKeys {
            record_signer: signer,
            ..keys
        };
        // A same-nest read is a read of `folder`: the walk carries its relay
        // hint, so a file whose bytes rest only on a holder's seat (a
        // metadata-only folder) is relayed instead of missing (`file-sync.md`
        // § Relay serving, step (4)). The hint travels with this download, not
        // with the machine's one fetcher, which serves every folder. The ref
        // form comes from the item itself, so a reader's own same-named set
        // never shadows the shared set the item belongs to.
        let hint =
            fauna_core::file_download::FolderHint::new(folder.as_str()).with_ref(set.folder_ref());
        if keys.mls_group_id.is_some() {
            // A FOREIGN (cross-nest) set's bytes live on its home nest — fetch
            // through the factory-built fetcher bound to that base URL (Phase 2
            // client read-side; the byte routes are public + CORS-open and
            // integrity is by content address), verified against the
            // grant-delivered home identity. A same-nest set uses the
            // machine's own injected fetcher, hinted. The foreign read carries
            // no hint: this session holds no bearer the home nest can name
            // (its reader's door is § Relay serving → *A member on another
            // nest*'s byte-plane token).
            let fetcher: Arc<dyn fauna_core::file_download::BlobFetcher> = match foreign_home
                .as_ref()
            {
                Some(home) => self
                    .foreign_fetchers
                    .get()
                    .ok_or_else(|| MediaApiError::Transient {
                        detail: "cross-nest file download is not yet supported on this \
                                     platform"
                            .to_string(),
                    })?
                    .fetcher_for(&home.nest_url, home.nest_actor_id.as_deref()),
                None => self
                    .download_fetcher
                    .clone()
                    .ok_or_else(|| MediaApiError::Transient {
                        detail: "media file download is not yet supported on this platform"
                            .to_string(),
                    })?,
            };
            let hinted = fauna_core::file_download::FolderHinted::new(fetcher.as_ref(), hint);
            let walk: &dyn fauna_core::file_download::BlobFetcher = if foreign_home.is_some() {
                fetcher.as_ref()
            } else {
                &hinted
            };
            // The owner key rides in `keys` for a bound set too, but
            // `effective_backup_key` suppresses it on every chunk site
            // (FS-5DC) — a bound set's chunks stay content-keyed, exactly as
            // before this factor.
            return fauna_core::file_download::download_file_bytes_by_manifest(
                walk,
                &keys,
                fauna_core::data::ContentHash::from_digest_raw(digest),
                content_key_version,
                &relative_path,
            )
            .await
            .map_err(|e| MediaApiError::Transient {
                detail: format!("file download failed: {e:#}"),
            });
        }

        // Owner-only path — the chunk-store walk under the owner key.
        let fetcher = self
            .download_fetcher
            .as_ref()
            .ok_or_else(|| MediaApiError::Transient {
                detail: "media file download is not yet supported on this platform".to_string(),
            })?;
        // `keys` is `FileDownloadKeys::owner(key)` here when the resolver
        // positively answered unbound. On a resolve *failure* it is keyless instead (no keys, per the
        // three-valued contract) and the walk refuses to open — transient and
        // retryable, preferred over guessing an audience.
        fauna_core::file_download::download_file_bytes_by_manifest(
            &fauna_core::file_download::FolderHinted::new(fetcher.as_ref(), hint),
            &keys,
            fauna_core::data::ContentHash::from_digest_raw(digest),
            content_key_version,
            &relative_path,
        )
        .await
        // The walk's failures (fetch, open, hash mismatch) collapse to one
        // detail string: the page shows it verbatim and nothing branches on the
        // variant, and the walk's anyhow chain doesn't discriminate retryable
        // from permanent for us.
        .map_err(|e| MediaApiError::Transient {
            detail: format!("file download failed: {e:#}"),
        })
    }

    /// Set the `media-sort-select` key from its UI value (`"name"` / `"size"` /
    /// `"date"`). An unrecognized value is ignored (the select only emits valid
    /// values). Re-sorts the rendered list on the next snapshot; notifies.
    pub fn set_sort(&self, value: String) {
        if let Some(key) = MediaSortKey::from_select_value(&value) {
            self.state.lock().unwrap().sort = key;
            self.observer.on_changed();
        }
    }

    /// Set the sort direction (the desktop column-header asc/desc toggle).
    /// Notifies.
    pub fn set_descending(&self, descending: bool) {
        self.state.lock().unwrap().descending = descending;
        self.observer.on_changed();
    }

    /// Set the `media-folder-filter` scope: `Some(value)` for one set — an
    /// option's value as the snapshot offered it, which parses as a ref
    /// (`local:<id>`, a shared set) or else names a set (the caller's own set
    /// of that name, then an item-only set; `media.md` § Encryption at rest →
    /// *The filter and the upload target address the row*, (a)) — and `None`
    /// for the all-media default. Notifies.
    ///
    /// Always leaves any active followed browse scope — a set value (or the
    /// all-media `None`) addresses the own-set browse; entering a followed
    /// scope is [`Self::select_followed_scope`]'s job.
    pub fn set_filter(&self, folder: Option<String>) {
        {
            let mut s = self.state.lock().unwrap();
            s.filter = folder.as_deref().map(FolderFilter::parse);
            s.followed_active = None;
        }
        self.observer.on_changed();
    }

    /// Enter a followed public folder's browse scope
    /// (`media.md` § Followed public folders): `value` is a
    /// [`FollowedScopeOption::value`] from the snapshot, handed back verbatim.
    /// Fetches the folder's current listing from its home nest **on demand** —
    /// a followed folder is never aggregated — then notifies.
    ///
    /// The two failure families must never render alike (a dropped connection
    /// is not a revoke): the plane's own refusal enters the scope **empty**
    /// with the folded unavailable wording on `error-message`, while a
    /// transport fault keeps the prior browse and reports itself. An unknown
    /// `value` (a follow removed since the options rendered) also reports
    /// rather than silently no-opping (e2e convention 11).
    pub async fn select_followed_scope(&self, value: String) {
        let Some(source) = self.followed_source.get().map(Arc::clone) else {
            self.set_error(
                FOLLOWED_FETCH_ERROR_KEY,
                "followed folders are not wired on this app",
            );
            return;
        };
        let scope = {
            let s = self.state.lock().unwrap();
            s.followed_options
                .iter()
                .find(|sc| followed_value(sc) == value)
                .cloned()
        };
        let Some(scope) = scope else {
            self.set_error(
                FOLLOWED_FETCH_ERROR_KEY,
                "that followed folder is no longer in the list",
            );
            return;
        };
        match source
            .fetch_listing(scope.folder_id, &scope.home_nest_url)
            .await
        {
            Ok(entries) => {
                {
                    let mut s = self.state.lock().unwrap();
                    s.followed_active = Some(FollowedActive { scope, entries });
                    s.error = None;
                }
                self.observer.on_changed();
            }
            Err(FollowedFetchError::Unavailable) => {
                // The revoke: enter the scope empty and loudly, and flip the
                // option's verdict now — the source fed its cache from this
                // same fetch, so the next refresh agrees.
                let err = LocalizedText::key(FOLLOWED_UNAVAILABLE_ERROR_KEY);
                tracing::warn!(target: "fauna_media", "{}", err.log_line());
                {
                    let mut s = self.state.lock().unwrap();
                    if let Some(opt) = s.followed_options.iter_mut().find(|sc| {
                        sc.folder_id == scope.folder_id && sc.home_nest_url == scope.home_nest_url
                    }) {
                        opt.available = false;
                    }
                    let mut scope = scope;
                    scope.available = false;
                    s.followed_active = Some(FollowedActive {
                        scope,
                        entries: Vec::new(),
                    });
                    s.error = Some(err);
                }
                self.observer.on_changed();
            }
            Err(FollowedFetchError::Transport(detail)) => {
                self.set_error(FOLLOWED_FETCH_ERROR_KEY, &detail);
            }
        }
    }

    /// Download one file of the **active** followed browse scope — the keyless
    /// follower read, routed through
    /// [`fauna_core::file_download::download_followed_file`] and a fetcher
    /// bound to the scope's home nest. Never [`Self::download_file`], never the
    /// name-keyed custody resolver (`media.md` architectural rule 6) — a
    /// follower holds no key, structurally.
    ///
    /// `value` names the scope (the snapshot's `followed_scope.value`) so a
    /// race with a scope switch fails loudly instead of downloading from the
    /// wrong nest, and `relative_path` names the item — the machine resolves
    /// the head `manifest_hash` from the entries it retained at scope entry,
    /// so the pointer never crosses to an app (a followed item has no version
    /// rows to read one from — the public plane is head-only). Like
    /// `download_file`, a per-item query: returns the bytes or the error to
    /// the caller, never touches the page banner.
    pub async fn download_followed(
        &self,
        value: String,
        relative_path: String,
    ) -> Result<Vec<u8>, MediaApiError> {
        let resolved = {
            let s = self.state.lock().unwrap();
            s.followed_active
                .as_ref()
                .filter(|a| followed_value(&a.scope) == value)
                .map(|a| {
                    (
                        a.scope.clone(),
                        a.entries
                            .iter()
                            .find(|e| e.path == relative_path)
                            .map(|e| e.manifest_hash.clone()),
                    )
                })
        };
        let Some((scope, manifest_hash)) = resolved else {
            return Err(MediaApiError::BadRequest {
                detail: "that followed folder is not the active browse scope".to_string(),
            });
        };
        let Some(manifest_hash) = manifest_hash else {
            return Err(MediaApiError::BadRequest {
                detail: "no such file in the followed folder's current listing".to_string(),
            });
        };
        let digest: [u8; 32] = hex::decode(&manifest_hash)
            .map_err(|e| MediaApiError::BadRequest {
                detail: format!("manifest hash is not hex: {e}"),
            })?
            .try_into()
            .map_err(|v: Vec<u8>| MediaApiError::BadRequest {
                detail: format!("manifest hash must be 32 bytes, got {}", v.len()),
            })?;
        // The byte plane: the scope's home nest (the already-injected foreign
        // fetcher factory — the same seam cross-nest shared sets ride, verified
        // against the identity the public-fetch reply stamped), or the
        // machine's own fetcher for a same-nest follow — mirroring
        // `download_file`'s two arms.
        let fetcher: Arc<dyn fauna_core::file_download::BlobFetcher> =
            if scope.home_nest_url.is_empty() {
                self.download_fetcher
                    .clone()
                    .ok_or_else(|| MediaApiError::Transient {
                        detail: "media file download is not yet supported on this platform"
                            .to_string(),
                    })?
            } else {
                self.foreign_fetchers
                    .get()
                    .ok_or_else(|| MediaApiError::Transient {
                        detail: "cross-nest file download is not yet supported on this platform"
                            .to_string(),
                    })?
                    .fetcher_for(&scope.home_nest_url, scope.home_nest_actor_id.as_deref())
            };
        fauna_core::file_download::download_followed_file(
            fetcher.as_ref(),
            fauna_core::data::ContentHash::from_digest_raw(digest),
            &relative_path,
        )
        .await
        .map_err(|e| MediaApiError::Transient {
            detail: format!("followed file download failed: {e:#}"),
        })
    }

    /// Set the `media-view-toggle`: `true` = thumbnail grid, `false` = list.
    /// Notifies (the toggle is pure render state — the item set is unchanged).
    pub fn set_view_grid(&self, grid: bool) {
        self.state.lock().unwrap().view_grid = grid;
        self.observer.on_changed();
    }
}

// Private helpers (not UniFFI-exported — kept out of the `#[uniffi::export]`
// impl above).
impl MediaMachine {
    /// [`MediaMachine::upload`] into the set `target` names — the row the
    /// upload's custody, declassification verdict and seal key by. `folder` is
    /// the set's name, which the record request still carries. A caller that
    /// holds the target's identity (the filter value, `media.md` § Encryption
    /// at rest → *The filter and the upload target address the row*, (e))
    /// hands its [`SetKey::Ref`] here; `upload` derives it by name.
    ///
    /// A **metadata-only** folder, or a shared set the caller may only read, is
    /// refused first, before anything is sealed or sent.
    async fn upload_into(
        &self,
        folder: String,
        target: SetKey,
        device_id: String,
        path: String,
        raw_bytes: Vec<u8>,
        backup_key: Vec<u8>,
    ) {
        let refusal = {
            let s = self.state.lock().unwrap();
            target.row_in(&s.raw.known_folders).and_then(|f| {
                if f.metadata_only {
                    Some(METADATA_ONLY_FOLDER_ERROR_KEY)
                } else if f.member && !f.writer {
                    Some(READ_ONLY_SHARED_FOLDER_ERROR_KEY)
                } else {
                    None
                }
            })
        };
        if let Some(key) = refusal {
            let err = LocalizedText::key(key);
            tracing::warn!(target: "fauna_media", "{}", err.log_line());
            self.state.lock().unwrap().error = Some(err);
            self.observer.on_changed();
            return;
        }
        match self
            .do_upload(
                &folder,
                target,
                &device_id,
                &path,
                &raw_bytes,
                backup_key.clone(),
            )
            .await
        {
            // Success: refresh re-reads the aggregate (clearing the error) and
            // notifies once — the new item then appears via `fauna.media.list`.
            // The gesture's own key renders the sealed labels: an upload is the
            // one Media gesture that already holds one, which is why it is also
            // the one that seals on the way in.
            Ok(()) => self.refresh(Some(backup_key)).await,
            Err(e) => {
                // A `path_seal_required` refusal means the nest's view of the
                // folder's audience differs from what this upload assumed:
                // re-read the snapshot so the page no longer shows the stale
                // verdict. The refresh clears the error, so set it after.
                if e.detail().contains("path_seal_required") {
                    self.refresh(Some(backup_key)).await;
                }
                self.set_error(UPLOAD_ERROR_KEY, e.detail())
            }
        }
    }

    /// The seal → POST → record core of `upload()`, factored out so the public
    /// gesture stays a thin success/error dispatcher (mirrors `refresh`'s split).
    ///
    /// **One at-rest shape** (`media.md` § Encryption at rest → *One at-rest
    /// shape*): every file this page records into a folder is the sync
    /// engine's own — FastCDC chunks behind a canonical `ChunkManifest`, sealed
    /// through `fauna_core::blob_seal::seal_blob` under the root the folder's
    /// custody names, uploaded chunk by chunk over the byte routes. Only the
    /// root differs by custody:
    ///
    /// | custody | chunk root | label root | thumbnail |
    /// |---|---|---|---|
    /// | content-keyed (served / shared) | current content key, stamped | the same generation | none |
    /// | owner-only | `BackupKey::convergent_chunk_root`, unstamped | the owner root | sealed `Library` blob |
    /// | `public` audience | none — plaintext chunks | none — names are URLs | plaintext blob |
    ///
    /// So every manifest reader opens what this page wrote with no second arm:
    /// the WebDAV MDA and a member (content-keyed), a synced desktop's hydrate
    /// and a private link's envelope (owner-only), the web serve's one door
    /// and a public link (public). The single sealed blob
    /// (`fauna_media::process_and_seal`) stays the attachment plane's and is
    /// never a folder file's shape.
    async fn do_upload(
        &self,
        folder: &str,
        target: SetKey,
        device_id: &str,
        path: &str,
        raw_bytes: &[u8],
        backup_key: Vec<u8>,
    ) -> Result<(), MediaApiError> {
        // 1. Fail fast if no blob uploader is wired on this platform — before
        //    any sealing work.
        let uploader = self
            .blob_uploader
            .as_ref()
            .ok_or_else(|| MediaApiError::Transient {
                detail: "media upload is not yet supported on this platform".to_string(),
            })?;

        // 2. A wrong key length is a caller bug → BadRequest (no seal
        //    attempted); the key is parsed either way because the owner-only
        //    custody's chunk and label roots both derive from it.
        let key_bytes: [u8; 32] =
            backup_key
                .try_into()
                .map_err(|v: Vec<u8>| MediaApiError::BadRequest {
                    detail: format!("backup key must be 32 bytes, got {}", v.len()),
                })?;
        let owner = BackupKey::from_bytes(key_bytes);

        // 3. The folder's custody — the same one the page renders and
        //    downloads under (`folder_keys_for_key` → the one shared resolver),
        //    so the seal root can never diverge from the root the readers
        //    hold — by the TARGET row, the one the record lands in.
        let (keys, foreign_home) = self.folder_keys_for_key(&target, Some(owner.clone())).await;
        let custody = if keys.is_content_keyed() {
            // A content-keyed set fails CLOSED, never owner-keyed: with the
            // set's content keys not in this device's custody yet (the
            // serve-enable or join custody write racing this device) the
            // gesture refuses and uploads nothing — the read-side twin of the
            // engine's `ServedKeysMissing`/`BoundKeysMissing` refusal. A
            // foreign (cross-nest) set's bytes live on its home nest, which
            // this session holds no write token for; refused likewise.
            if foreign_home.is_some() {
                return Err(MediaApiError::Transient {
                    detail: "uploading into a folder homed on another nest is not supported yet"
                        .to_string(),
                });
            }
            let Some(content_keys) = keys.content_keys.as_ref() else {
                return Err(MediaApiError::Transient {
                    detail: "this folder's content keys are not on this device yet (a served \
                             or shared folder seals under them) — try again once the folder's \
                             custody has synced"
                        .to_string(),
                });
            };
            UploadCustody::ContentKeyed {
                root: *content_keys.current_key(),
                version: content_keys.current_version(),
            }
        } else if let Some(folder_id) = self.declassified_folder_id_now(target).await {
            // Media is a producer for a declassified folder exactly as the
            // sync engine is (`SyncEngine::with_public_audience`): its content,
            // names and paths rest in the clear, because they are URLs the
            // web serve reads. Unclassifiable folders never land here — the
            // fail-closed direction `judge_declassification` states.
            UploadCustody::Public { folder_id }
        } else {
            UploadCustody::OwnerOnly
        };

        // 4. `process_media` first: the metadata strip (what rests is the
        //    stripped body, whatever the custody) and the thumbnail. A
        //    content-keyed set records no thumbnail — the engine records none
        //    either, and a blob sealed under a per-set key would need an
        //    `Audience` the sidecar wire does not carry.
        let processed = fauna_media::process::process_media(raw_bytes);
        let thumbnail_audience = match custody {
            UploadCustody::ContentKeyed { .. } => None,
            UploadCustody::OwnerOnly => Some(Audience::Library {
                backup_key: owner.clone(),
            }),
            UploadCustody::Public { folder_id } => Some(Audience::PublicFolder { folder_id }),
        };

        // 5. Seal through the engine's own whole-file pipeline. Deterministic,
        //    so a retry re-puts the same store keys and the owner-only arm
        //    dedups per owner exactly as the engine's uploads do.
        let seal_root = match custody {
            UploadCustody::ContentKeyed { root, version } => Some((root, Some(version))),
            UploadCustody::OwnerOnly => Some((owner.convergent_chunk_root(), None)),
            UploadCustody::Public { .. } => None,
        };
        let sealed = fauna_core::blob_seal::seal_blob(&processed.stripped_bytes, seal_root)
            .map_err(|e| MediaApiError::Transient {
                detail: format!("sealing the file failed: {e:#}"),
            })?;

        // 6. The chunks first, the manifest last: a manifest must never rest
        //    on the nest pointing at chunks it does not hold (the engine's
        //    order).
        for (store_key, body) in sealed.chunks {
            uploader.post_chunk(store_key.digest(), body).await?;
        }
        uploader
            .post_manifest(sealed.manifest_hash.digest(), sealed.manifest_bytes)
            .await?;

        // 7. The thumbnail, after the file: it stays a blob-store blob sealed
        //    under the folder's audience (a derived view only this page reads,
        //    fetched by its own hash — `fetch_thumbnail`), so its failure must
        //    NOT orphan the just-stored file: log it and record the member
        //    without one (the item renders the placeholder).
        let thumbnail_hash = match (thumbnail_audience, processed.thumbnail_bytes) {
            (Some(audience), Some(thumbnail_bytes)) => {
                let thumb =
                    fauna_media::pipeline::seal_rendered_thumbnail(&thumbnail_bytes, &audience);
                match uploader
                    .post_blob(thumb.sidecar.to_dag_cbor(), thumb.bytes)
                    .await
                {
                    Ok(hash) => Some(hash),
                    Err(e) => {
                        tracing::warn!(
                            target: "fauna_media",
                            "thumbnail blob upload failed, recording member without it: {e:?}"
                        );
                        None
                    }
                }
            }
            _ => None,
        };

        // 8. Seal the path under the SAME root the chunks sealed under, so the
        //    audience that can open the bytes can render the name and nobody
        //    weaker (`file-sync.md` § Sealed names & paths). A public folder's
        //    names are URLs and seal no label — the engine's public arm
        //    (`label_seal_root` → `None`) agrees, and the serve reads the
        //    plaintext `path`. A seal failure must not orphan the just-stored
        //    bytes: record plaintext-only and let the backfill pass pick it up.
        let path_sealed = match custody {
            UploadCustody::Public { .. } => None,
            UploadCustody::ContentKeyed { .. } => {
                fauna_core::label_custody::seal_path_from_keys(&keys, path)
                    .inspect_err(|e| {
                        tracing::warn!(
                            target: "fauna_media",
                            "sealing the media path under the content key failed, recording \
                             it plaintext-only: {e:?}"
                        )
                    })
                    .ok()
            }
            UploadCustody::OwnerOnly => fauna_core::label_custody::seal_path(
                &path_crypto::LabelRoot::owner_of(&owner),
                path,
            )
            .inspect_err(|e| {
                tracing::warn!(
                    target: "fauna_media",
                    "sealing the media path failed, recording it plaintext-only: {e:?}"
                )
            })
            .ok(),
        };

        // 9. Record the manifest member (the WS-RPC control plane): the
        //    manifest's hash, the plaintext size the engine records, the
        //    generation stamp of a content-keyed seal, the thumbnail's hash —
        //    and the target's ROW wherever the key names one, which the nest
        //    resolves by id and never by the name forms (`media.md`
        //    § Encryption at rest → *The filter and the upload target address
        //    the row*, (e)); an item-only name records by name alone.
        let folder_id = match target {
            SetKey::Ref(fauna_core::folder_keys::FolderRef::Local(id)) => Some(id),
            _ => None,
        };
        self.nest_api
            .record_member(
                folder,
                folder_id,
                device_id,
                path,
                hex::encode(sealed.manifest_hash.digest()),
                sealed.manifest.total_size as i64,
                sealed.content_key_version,
                thumbnail_hash,
                path_sealed,
            )
            .await
    }

    /// Set the page error from a failed gesture: build the localized banner, log
    /// it once (producer-side, observability.md § Log on the *event*), store it,
    /// and notify. Mirrors the `refresh()` error arm.
    /// The create flow's fallible middle: the author, the file's current
    /// version, the mint, the registration, the reveal. `Err` carries the
    /// page error's detail — and never the token, nor a private link's key.
    async fn mint_and_register(
        &self,
        folder: &str,
        path: &str,
        name: &str,
        expiry: &str,
        key_in_fragment: bool,
    ) -> Result<String, String> {
        let author = self
            .share_author
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "no signing identity is wired".to_string())?;
        let lifetime = fauna_client_share::expiry_secs(expiry)
            .ok_or_else(|| format!("unknown expiry {expiry}"))?;
        // A link names the CURRENT version's bytes (`share-links.md` § What a
        // link is): the newest live version row (the list is oldest→newest).
        // The link's set is the caller's own set of that name, as every
        // by-name write resolves it ([`Self::target_set_key`]).
        let row = self.target_set_key(folder).local_row();
        let versions = self
            .nest_api
            .file_versions(folder, row, path, false)
            .await
            .map_err(|e| e.detail().to_string())?;
        let current = versions
            .iter()
            .map(|v| &v.summary)
            .rev()
            .find(|v| !v.pruned)
            .ok_or_else(|| "the file has no current version".to_string())?;
        let manifest_hash: [u8; 32] = hex::decode(&current.manifest_hash)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| "the file's version has a malformed manifest hash".to_string())?;
        let now = u64::try_from(fauna_core::data::Timestamp::now_secs()).unwrap_or(0);
        let file = fauna_client_share::LinkFile {
            manifest_hash,
            filename: name.to_string(),
        };
        let minted = if key_in_fragment {
            // The sealed arm (`share-links.md` § The private-file extension):
            // the version's manifest as the nest holds it, which the mint
            // unseals under the owner root (or a predecessor's) to derive the
            // link's chunk keys — refusing one no held root opens.
            let manifest = self.fetch_link_manifest(manifest_hash).await?;
            fauna_client_share::mint_private_link(&author, &file, manifest, lifetime, now)
        } else {
            // No owner root is at stake whoever signed the version: the public
            // arm is offered only on a folder whose bytes rest unsealed and
            // carries no key — it names a manifest.
            fauna_client_share::mint_link(&author, &file, lifetime, now)
        }
        .map_err(|e| e.to_string())?;
        let reply = self
            .nest_api
            .share_register(minted.request().clone())
            .await
            .map_err(|e| e.detail().to_string())?;
        minted
            .reveal(&reply)
            .ok_or_else(|| "the nest registered a different link".to_string())
    }

    /// The sealed manifest a private link names, fetched over the byte routes
    /// and checked against its address before anything reads it (the root of
    /// the integrity chain the viewer walks).
    async fn fetch_link_manifest(
        &self,
        manifest_hash: [u8; 32],
    ) -> Result<fauna_core::chunk::ChunkManifest, String> {
        let fetcher = self
            .download_fetcher
            .as_ref()
            .ok_or_else(|| "file download is not supported on this platform".to_string())?;
        let address = fauna_core::data::ContentHash::from_digest_raw(manifest_hash);
        let bytes = fetcher
            .fetch_manifest(&address)
            .await
            .map_err(|e| format!("the file's manifest could not be fetched: {e:#}"))?;
        if fauna_core::data::ContentHash::of_raw(&bytes) != address {
            return Err("the nest served a different manifest".to_string());
        }
        let manifest: fauna_core::chunk::ChunkManifest =
            fauna_core::encoding::canonical_decode(&bytes)
                .map_err(|e| format!("the file's manifest is malformed: {e:#}"))?;
        manifest
            .check_hash_shape()
            .map_err(|e| format!("the file's manifest is malformed: {e:#}"))?;
        Ok(manifest)
    }

    /// Read + render the list into state (loaded on success; on failure the
    /// page error, and the list stays unloaded).
    async fn load_share_links(&self) {
        let Some(author) = self.share_author.lock().unwrap().clone() else {
            self.set_error(SHARE_LIST_ERROR_KEY, "no signing identity is wired");
            return;
        };
        match self.nest_api.share_list().await {
            Ok(records) => {
                let now = fauna_core::data::Timestamp::now_secs();
                let rows = fauna_client_share::link_rows(&author, &records, now)
                    .into_iter()
                    .map(|r| ShareLinkSummary {
                        token_id: r.token_id,
                        name: r.filename,
                        expires_at: r.expires_at,
                        state: r.state.as_str().to_string(),
                        url: r.url,
                    })
                    .collect();
                {
                    let mut s = self.state.lock().unwrap();
                    if !s.share_links.open {
                        return;
                    }
                    s.share_links.rows = rows;
                    s.share_links.loaded = true;
                }
                self.observer.on_changed();
            }
            Err(e) => self.set_error(SHARE_LIST_ERROR_KEY, e.detail()),
        }
    }

    fn set_error(&self, key: &str, detail: &str) {
        let err = error_text(key, detail);
        tracing::warn!(target: "fauna_media", "{}", err.log_line());
        self.state.lock().unwrap().error = Some(err);
        self.observer.on_changed();
    }

    /// Surface the "no set to upload into" page error (`upload_selected`;
    /// `media.md` § Layout & flow): the caller has **no folders at all** →
    /// [`NO_SET_ERROR_KEY`] ("create one first"). Every own folder is an
    /// upload target, so this is the one condition; an *empty* folder is a
    /// valid target and never reaches it. A pure client-side condition → a
    /// no-arg localized banner (no `{message}`).
    fn set_error_no_upload_target(&self) {
        let err = LocalizedText::key(NO_SET_ERROR_KEY);
        tracing::warn!(target: "fauna_media", "{}", err.log_line());
        self.state.lock().unwrap().error = Some(err);
        self.observer.on_changed();
    }
}

fn error_text(key: &str, detail: &str) -> LocalizedText {
    LocalizedText::key_arg(key, "message", detail.to_string())
}

/// The minted `media-folder-filter` value for a followed scope — opaque to
/// apps, stable per follow (`(home_nest_url, folder_id)` is the follow's full
/// identity). Disjoint from every set name in practice; the gestures resolve a
/// handed-back value against the followed table **only**, so even a set
/// pathologically named in this form can never be routed as a follow.
fn followed_value(scope: &FollowedMediaScope) -> String {
    format!("followed:{}@{}", scope.folder_id, scope.home_nest_url)
}

/// The display label: the follow's name plus its owner — a follow named like
/// one of the user's own sets stays tellable apart at the label level, because
/// the routing never keys on names (`media.md` § Followed public folders).
///
/// The owner half is the source's `owner_display`, the very string the Folders
/// page's followed row shows (the handle while it still names the owner, else
/// the actor id's short form); a source that fills none gets the same short
/// form here rather than a second truncation rule.
fn followed_label(scope: &FollowedMediaScope) -> String {
    let owner = if scope.owner_display.is_empty() {
        fauna_core::format::short_id(&scope.owner_actor_id)
    } else {
        scope.owner_display.clone()
    };
    format!("{} ({owner})", scope.display_name)
}

/// A scope as the snapshot offers it.
fn followed_option_of(scope: &FollowedMediaScope) -> FollowedScopeOption {
    FollowedScopeOption {
        value: followed_value(scope),
        label: followed_label(scope),
        available: scope.available,
    }
}

/// A followed listing row as the snapshot renders it — an ordinary
/// [`MediaItem`], so the per-app `media-item` render is byte-identical to the
/// aggregate browse. `folder` carries the follow's display name for rendering
/// only (nothing routes on it — `media.md` architectural rule 6);
/// `source_online` carries the availability verdict.
///
/// `thumbnail_hash` is deliberately dropped for now: whether a public folder's
/// thumbnail blobs rest plaintext (i.e. whether a keyless follower could open
/// one) is NOT yet verified (`ui/media.md` § Followed public folders), and the
/// pointer would send every app's thumbnail fetch to its *own* nest for a blob
/// homed elsewhere. Restore it together with a followed-aware thumbnail fetch
/// once that question is answered.
fn followed_item(
    scope: &FollowedMediaScope,
    e: &fauna_core::followed_media::FollowedFileEntry,
) -> MediaItem {
    MediaItem {
        folder: scope.display_name.clone(),
        path: e.path.clone(),
        size_bytes: e.size_bytes,
        updated_at: e.updated_at,
        thumbnail_hash: None,
        source_online: scope.available,
        ..MediaItem::default()
    }
}

/// The custody a folder upload seals under — the one fact that picks the chunk
/// root, the label root and the thumbnail's audience (`MediaMachine::do_upload`).
#[derive(Clone, Copy)]
enum UploadCustody {
    /// A WebDAV-served or shared set: its current content-key generation.
    ContentKeyed { root: [u8; 32], version: u64 },
    /// An owner-only set: the owner's convergent chunk root, unstamped.
    OwnerOnly,
    /// A `public`-audience folder: plaintext, no root.
    Public { folder_id: i64 },
}
