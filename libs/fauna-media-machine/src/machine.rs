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
use fauna_core::crypto::BackupKey;
use fauna_core::file_download::{FileDownloadKeys, PredecessorSealKey, RecordSigner};
use fauna_core::followed_media::{FollowedFetchError, FollowedMediaScope, FollowedMediaSource};
use fauna_core::localized::LocalizedText;
use fauna_core::path_crypto;

use crate::blob_uploader::MediaBlobUploader;
use crate::nest_api::{MediaApiError, MediaNestApi};
use crate::observer::MediaObserver;
use crate::snapshots::{
    FileVersionSummary, FollowedScopeOption, MediaFolderOption, MediaItemSummary,
    MediaPageSnapshot, ShareCreateSnapshot, ShareLinkSummary, ShareLinksSnapshot,
};
pub use fauna_client_media::member_rows::{FolderChannelOwners, MlsQuery};

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
/// The `{message}` an upload reports through [`REFRESH_ERROR_KEY`] when it
/// found no target and the page's reads never completed (rows or own sets
/// still held back for want of custody, or the folder list unread): the
/// caller may well have a folder, so "create one first" would be false.
const FOLDERS_NOT_LOADED_DETAIL: &str =
    "your folders have not finished loading yet — try again in a moment";

/// How long a page that has never loaded keeps re-reading a read that is not
/// complete ([`MediaSnapshot::held`] non-zero, or the folder list lost) —
/// `ui/README.md` § List pages: loading is not empty, rule 2: only a complete
/// read licenses "nothing here". Right after sign-in the account runtime and
/// its custody arrive within about the bound a write already waits for them
/// (`fauna_account_seams::blessed_nests`, 120 × 250 ms); the re-reads back off
/// from `first`, doubling up to `cap`, until `budget` is spent, and then the
/// page loads with what it found — a row held for ever is absent, never a page
/// that spins for ever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlePolicy {
    pub first: std::time::Duration,
    pub cap: std::time::Duration,
    pub budget: std::time::Duration,
}

impl Default for SettlePolicy {
    fn default() -> Self {
        Self {
            first: std::time::Duration::from_millis(250),
            cap: std::time::Duration::from_secs(4),
            budget: std::time::Duration::from_secs(30),
        }
    }
}

/// Which settle budgets a read found spent when it landed (`commit_read`).
#[derive(Debug, Clone, Copy)]
struct SettleBudget {
    /// The page's first-load budget ([`State::first_load_spent`]).
    first_load_spent: bool,
    /// The running settle's own — what a forced settle on a loaded page spends.
    this_settle_spent: bool,
}
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
/// i18n key for a foreign browse scope whose home nest refused the member's
/// relayed read — the member was removed (`ui/media.md` § Cross-nest shared
/// sets, *Removal is loud*). The scope enters empty and the option stays
/// listed until the member's custody record goes. No `{message}` arg.
const FOREIGN_REMOVED_ERROR_KEY: &str = "media.error_foreign_removed";
/// The `{message}` a write into a set homed on another nest reports through
/// [`UPLOAD_ERROR_KEY`] / [`DELETE_ERROR_KEY`] on a platform that wired no
/// member write door ([`crate::folder_keys::ForeignBlobFetcherFactory::member_uploader_for`])
/// — refused loudly, never sent to the own nest, which holds none of the set's
/// bytes (`ui/media.md` § Cross-nest shared sets, *Upload*).
const FOREIGN_WRITE_UNSUPPORTED: &str =
    "writing to a folder homed on another nest is not supported on this platform";

/// One page read's custody view ([`fauna_core::folder_keys::read_scope`]):
/// opened at the first set the read's label render needs custody for, then
/// shared by every other set of that read and dropped with it — so a read
/// costs one roster + custody read, not one per set (`docs/goal/ui/README.md`
/// § List pages: loading is not empty). Reads only: a gesture's seal resolves
/// through the machine's own resolver, fresh.
#[derive(Default)]
struct ReadScope {
    opened: Option<Option<Arc<dyn crate::folder_keys::FolderKeyResolver>>>,
}

impl ReadScope {
    /// The scope over `base`, opened on the first call.
    async fn resolver(
        &mut self,
        base: Option<&Arc<dyn crate::folder_keys::FolderKeyResolver>>,
    ) -> Option<Arc<dyn crate::folder_keys::FolderKeyResolver>> {
        if self.opened.is_none() {
            let scope = match base {
                Some(base) => Some(fauna_core::folder_keys::read_scope(base).await),
                None => None,
            };
            self.opened = Some(scope);
        }
        self.opened.clone().flatten()
    }
}

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
    /// The page error [`MediaMachine::report_error`] put up — an answer to a
    /// press (no file chosen, no folder to upload into, a file the app could
    /// not read), not to a read. A read that succeeds clears `error` only when
    /// it is NOT this one: the page re-reads on its own (a sync push while the
    /// page shows, the first-load settle), and such a read landing a beat after
    /// the press would wipe the answer before anyone read it. The next gesture
    /// ([`MediaMachine::upload_selected`], [`MediaMachine::set_filter`]) drops
    /// the hold.
    held_error: Option<LocalizedText>,
    /// Whether a refresh has ever returned successfully — surfaced as
    /// [`MediaPageSnapshot::loaded`], which owns the full rationale. Set in the
    /// `Ok` arm of [`MediaMachine::refresh_with_owner`] and never cleared.
    ///
    /// A read that held rows or own sets back for want of custody
    /// ([`MediaSnapshot::held`]) or lost the folder list is **not** a
    /// complete read and does not set it — until the first-load settle
    /// ([`SettlePolicy`]) has spent its budget, after which the page loads
    /// with what the reads found rather than spin for ever.
    loaded: bool,
    /// Whether the last committed read was **complete** — nothing held back
    /// for want of custody, and the folder list read. `false` before any read.
    /// The upload gestures read it: no target on an incomplete read is "your
    /// folders have not loaded yet", never `media.error_no_set`.
    complete: bool,
    /// The generation of the newest read that has committed (0 = none) — the
    /// newest-landed rule `MediaMachine::refresh_gen` documents.
    committed_gen: u64,
    /// How much of the first-load settle budget ([`SettlePolicy::budget`]) the
    /// page has spent — counted per PAGE, never per refresh: a page that never
    /// loaded and keeps being asked to refresh while its rows stay held must
    /// still spend the budget and load (`ui/README.md` § List pages, rule 2).
    first_load_spent: std::time::Duration,
    /// Followed public folders as browse-scope options, refreshed with the
    /// aggregate from the injected [`FollowedMediaSource`]; empty while the
    /// source is unwired.
    followed_options: Vec<FollowedMediaScope>,
    /// The active followed browse scope, `None` in the ordinary own-set
    /// browse. While `Some`, the snapshot's `items` renders this listing and
    /// `filter` carries the scope's minted value
    /// (`media.md` § Followed public folders).
    followed_active: Option<FollowedActive>,
    /// The active **foreign** browse scope (`ui/media.md` § Cross-nest shared
    /// sets — the foreign browse scope), `None` outside it. While `Some`, the
    /// snapshot's `items` renders its judged heads and `filter` carries its
    /// `foreign:` ref. Never both this and [`Self::followed_active`].
    foreign_active: Option<ForeignActive>,
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
            Self::Ref(
                fauna_core::folder_keys::FolderRef::Foreign(_)
                | fauna_core::folder_keys::FolderRef::Group(_),
            ) => None,
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

    /// Who signed the row naming `manifest_hex` in `set`.
    fn signer(&self, set: SetKey, manifest_hex: &str) -> RecordSigner {
        let key = (set, manifest_hex.to_ascii_lowercase());
        self.manifests.get(&key).copied().unwrap_or_default()
    }
}

/// Parse a recorded hex manifest hash (a version's `manifest_hash`, an item's
/// `thumbnail_hash`) into its digest — a malformed one is a caller bug,
/// `BadRequest`, before any fetch. `what` names it in the detail.
fn manifest_digest(hex_hash: &str, what: &str) -> Result<[u8; 32], MediaApiError> {
    hex::decode(hex_hash)
        .map_err(|e| MediaApiError::BadRequest {
            detail: format!("{what} is not hex: {e}"),
        })?
        .try_into()
        .map_err(|v: Vec<u8>| MediaApiError::BadRequest {
            detail: format!("{what} must be 32 bytes, got {}", v.len()),
        })
}

/// The error a shared set's byte walk fails with. A cross-nest member the
/// set's home nest refused its read token — removed from the set
/// (`relay-serving.md` § Relay serving → *A member on another nest*, step (5))
/// — gets a `NotFound` naming the removal (`media.md` § Implementation status
/// today, the cross-nest download entry): it is permanent, and dressed as the
/// walk's other failures it would read as a blip to retry. Every other failure
/// stays the one transient detail string the page shows verbatim.
fn shared_read_error(what: &str, e: &anyhow::Error) -> MediaApiError {
    if fauna_core::file_download::is_membership_refused(e) {
        return MediaApiError::NotFound {
            detail: format!(
                "{what} refused: you are no longer a member of this shared folder, so its \
                 home nest no longer serves you its files"
            ),
        };
    }
    MediaApiError::Transient {
        detail: format!("{what} failed: {e:#}"),
    }
}

/// Seal a rendered thumbnail through the one whole-file pipeline under
/// `seal_root` — the root its file just sealed under — and post it chunks
/// first, manifest last (a manifest never rests pointing at chunks the nest
/// does not hold). Answers the manifest's hex hash, the `thumbnail_hash` the
/// record carries (`media.md` § Encryption at rest → *A thumbnail rests as
/// its file rests*).
async fn seal_and_post_thumbnail(
    uploader: &dyn MediaBlobUploader,
    thumbnail_bytes: &[u8],
    seal_root: Option<([u8; 32], Option<u64>)>,
) -> Result<String, MediaApiError> {
    let sealed = fauna_core::blob_seal::seal_blob(
        thumbnail_bytes,
        seal_root.map(|(k, v)| (k, v.map(fauna_core::blob_seal::SealStamp::Version))),
    )
    .map_err(|e| MediaApiError::Transient {
        detail: format!("sealing the thumbnail failed: {e:#}"),
    })?;
    for (store_key, body) in sealed.chunks {
        uploader.post_chunk(store_key.digest(), body).await?;
    }
    uploader
        .post_manifest(sealed.manifest_hash.digest(), sealed.manifest_bytes)
        .await?;
    Ok(hex::encode(sealed.manifest_hash.digest()))
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

/// Render one listed item's path under its set's `keys`, bounded to who signed
/// its row — `false` when the label does not render and the item omits (the
/// ratified degrade). The one per-item render the aggregate and the foreign
/// browse scope share, so a name renders alike in both.
///
/// Salt selection + the sealed-first policy both live in the shared seam
/// (`fauna_core::label_custody`), so this surface cannot drift from snapshot
/// browse/diff or the conflict list on either. Ruling (8)(c): a row signed
/// under a retired identity (or by another writer) is never offered this
/// account's CURRENT owner root on the label arm — else a predecessor's
/// signature could render, in this set, the sealed path of a file the
/// successor created in another.
fn render_item_path(keys: &FileDownloadKeys, signer: RecordSigner, item: &mut MediaItem) -> bool {
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
            true
        }
        // Already the plaintext we were handed — nothing to rewrite.
        path_crypto::SealedLabelRender::Plaintext(_) => true,
        path_crypto::SealedLabelRender::Omit => false,
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

/// The active foreign browse scope: the set as the option offered it, plus
/// its judged heads with their labels rendered under the scope's custody.
/// The set's identity lives HERE, never on an item (`ui/media.md` § Cross-nest
/// shared sets, *Identity lives on the scope*): every gesture on one of
/// `items` resolves its set as `SetKey::Ref(FolderRef::Foreign(channel))`.
struct ForeignActive {
    set: fauna_client_media::MediaForeignSet,
    items: Vec<MediaItem>,
    /// Heads the last read held for want of a roster — re-read on the page's
    /// settle ([`MediaMachine::reread_held_foreign_scope`]).
    held: usize,
}

impl ForeignActive {
    fn set_key(&self) -> SetKey {
        SetKey::Ref(self.set.folder_ref())
    }
}

impl State {
    /// A new gesture began: the held press answer (`held_error`) stops
    /// standing, and leaves the page if it is still the one shown.
    fn drop_held_error(&mut self) {
        if let Some(held) = self.held_error.take()
            && self.error.as_ref() == Some(&held)
        {
            self.error = None;
        }
    }

    fn new() -> Self {
        Self {
            raw: MediaSnapshot::default(),
            sort: MediaSortKey::default(),
            descending: false,
            filter: None,
            view_grid: false,
            error: None,
            held_error: None,
            loaded: false,
            complete: false,
            committed_gen: 0,
            first_load_spent: std::time::Duration::ZERO,
            followed_options: Vec::new(),
            followed_active: None,
            foreign_active: None,
            share_create: None,
            share_links: ShareLinksSnapshot::default(),
        }
    }
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MediaMachine {
    state: Mutex<State>,
    /// Refresh generation — claimed at the start of every aggregate re-read
    /// (`refresh_with_owner`, the tail of every gesture that re-reads) and
    /// checked under the state lock before the commit: a read lands only if no
    /// NEWER read has landed already (`State::committed_gen`) — newest-landed
    /// wins. Without it, overlapping re-reads race last-write-wins on the page:
    /// a slow nav-to-tab read issued before an upload landed can commit after
    /// the upload's own re-read and hide the new file, with nothing reading
    /// again. A read is never dropped merely because a newer one STARTED: the
    /// nest fires one `SyncChanged` per recorded file and the apps re-read the
    /// visible page on each, so on a large account reads can keep starting
    /// faster than one lands, and dropping on start meant the page never loaded.
    /// Since the read gate (`reads`) no two of the machine's reads overlap at
    /// all; the generation stays as the commit's backstop.
    refresh_gen: std::sync::atomic::AtomicU64,
    /// One aggregate read in flight, at most one trailing re-read
    /// ([`crate::read_gate`]).
    reads: crate::read_gate::ReadGate,
    /// The newest owner key a refresh was asked with — what the gate's runner
    /// reads under, so a read one caller runs for another's ask renders with
    /// the key the newest ask brought, never a keyless caller's `None`.
    read_owner: Mutex<Option<BackupKey>>,
    observer: Arc<dyn MediaObserver>,
    nest_api: Arc<dyn MediaNestApi>,
    /// The blob-upload seam for `upload()` (the bulk-binary `POST /api/v1/blob`
    /// leg). `None` on a platform whose blob uploader isn't wired yet (web's wasm
    /// coordinator — LEG B; native LEG A is a follow-on) → `upload()` returns a
    /// `Transient` "not supported" error. `delete()` never needs it.
    blob_uploader: Option<Arc<dyn MediaBlobUploader>>,
    /// The full-file download seam for `download_file()` and
    /// `fetch_thumbnail()` (a thumbnail rests as its file rests, a manifest) — the shared
    /// client-side walk's per-target fetch leg (native
    /// `fauna_client::NestPublicChunkFetcher`, wasm
    /// `fauna_core::file_download::WasmPublicChunkFetcher`, injected by
    /// `nest_api::ws_rpc::build_media_machine`; `/api/v1/manifests/{hash}` +
    /// `/api/v1/chunks/{hash}`). `None` on a platform with no wired leg →
    /// `download_file()` / `fetch_thumbnail()` return a `Transient` "not
    /// supported" error, like the upload seam above.
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
    /// The first-load settle schedule ([`SettlePolicy`]); a test shortens it.
    settle: Mutex<SettlePolicy>,
    /// Browse-scope generation — bumped by every scope change
    /// ([`Self::set_filter`], [`Self::select_followed_scope`],
    /// [`Self::select_foreign_scope`]) and checked before a foreign scope's
    /// fetch commits, so a selection the user has already moved past never
    /// lands over a newer one.
    scope_gen: std::sync::atomic::AtomicU64,
}

impl MediaMachine {
    /// Construct the page machine over an injected [`MediaNestApi`] seam + an
    /// optional [`MediaBlobUploader`]. State starts empty (all-media view, name
    /// sort ascending, list mode); the client calls `refresh()` to populate it.
    ///
    /// `blob_uploader` / `download_fetcher` are `Some` on a platform with a
    /// wired byte-upload / -download path and `None` otherwise (then `upload()`
    /// / `download_file()` / `fetch_thumbnail()` report unsupported; `delete()`
    /// and the reads work regardless).
    ///
    /// Not a `#[uniffi::constructor]` — the seams (`Arc<dyn …>`) have no FFI ABI.
    /// Clients construct via `nest_api::build_media_machine` (native `fauna-ffi` /
    /// linux, wasm web), which binds the session's connected requester; tests pass
    /// a `FakeMediaNestApi` (+ `FakeMediaBlobUploader` and a download fetcher).
    pub fn new(
        observer: Arc<dyn MediaObserver>,
        nest_api: Arc<dyn MediaNestApi>,
        blob_uploader: Option<Arc<dyn MediaBlobUploader>>,
        download_fetcher: Option<Arc<dyn fauna_core::file_download::BlobFetcher>>,
        folder_keys: Option<Arc<dyn crate::folder_keys::FolderKeyResolver>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::new()),
            refresh_gen: std::sync::atomic::AtomicU64::new(0),
            reads: Default::default(),
            read_owner: Mutex::new(None),
            observer,
            nest_api,
            blob_uploader,
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
            settle: Mutex::new(SettlePolicy::default()),
            scope_gen: std::sync::atomic::AtomicU64::new(0),
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
    /// The same query is the member-row declassification anchor, so it is
    /// handed to the nest seam too ([`MediaNestApi::set_mls_query`]).
    pub fn set_mls_query(&self, mls_query: Arc<dyn MlsQuery>) {
        self.nest_api.set_mls_query(mls_query.clone());
        *self.mls_query.lock().unwrap() = Some(mls_query);
    }

    /// Enter the foreign browse scope `value` names (`foreign:<channel hex>`,
    /// as `folder_options` offered it) and return once its listing has landed
    /// or failed — the awaitable form of what [`Self::set_filter`] runs on the
    /// background executor for the same value, for a caller that holds a
    /// runtime and wants the answer in order (tui's `Op`, a test). A value
    /// that is not a foreign ref reports and changes nothing.
    pub async fn select_foreign_scope(&self, value: String) {
        let generation = self.next_scope();
        match FolderFilter::parse(&value) {
            FolderFilter::Ref(fauna_core::folder_keys::FolderRef::Foreign(channel)) => {
                self.enter_foreign_scope(channel, generation).await;
            }
            _ => self.set_error(REFRESH_ERROR_KEY, "that is not a folder on another nest"),
        }
    }

    /// Shorten the first-load settle ([`SettlePolicy`]) — tests only, so a
    /// settle that never completes spends milliseconds, not its real budget.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn set_settle_policy(&self, policy: SettlePolicy) {
        *self.settle.lock().unwrap() = policy;
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

    /// The member's cross-nest sets as browse scopes (`ui/media.md`
    /// § Cross-nest shared sets → *Which sets are offered*): the live
    /// foreign-set records the custody resolver already opens, behind the same
    /// MLS join-filter the member rows pass (unwired, every one is dropped —
    /// fail-safe), dropping a record whose group a control-plane row already
    /// carries (a set is never in both planes). No resolver → none; a failed
    /// custody read keeps the options the last read offered, never reading as
    /// a removal.
    async fn read_foreign_sets(
        &self,
        known: &[fauna_client_media::MediaFolder],
    ) -> Vec<fauna_client_media::MediaForeignSet> {
        let Some(resolver) = self.folder_keys.as_ref() else {
            return Vec::new();
        };
        let records = match resolver.foreign_sets().await {
            Ok(records) => records,
            Err(e) => {
                tracing::warn!(
                    target: "fauna_media",
                    error = %e,
                    "reading the foreign shared sets failed; keeping the last read's options",
                );
                return self.state.lock().unwrap().raw.foreign_sets.clone();
            }
        };
        let query = self.mls_query.lock().unwrap().clone();
        records
            .into_iter()
            .filter_map(|record| {
                let group = hex::encode(&record.mls_group_id);
                let joined = fauna_client_media::member_rows::admits(
                    query.as_deref(),
                    true,
                    Some(group.as_str()),
                );
                let listed = known.iter().any(|f| {
                    f.mls_group_id
                        .as_deref()
                        .is_some_and(|g| g.eq_ignore_ascii_case(&group))
                });
                (joined && !listed).then(|| fauna_client_media::MediaForeignSet {
                    channel: record.channel_id,
                    name: record.set_name.clone().unwrap_or_default(),
                    owner: fauna_core::format::qualified_handle(
                        record.owner_handle.as_deref(),
                        record.owner_domain.as_deref(),
                    )
                    .unwrap_or_default(),
                    mls_group_id: group,
                    home_nest_url: record.home_nest_url.clone(),
                    writer: record.access.as_deref() == Some("writer"),
                    metadata_only: record.metadata_only_residency() == Some(true),
                })
            })
            .collect()
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
    ///
    /// Also returns how many **own** sets were omitted: an own set's name is
    /// opened by the reader's own custody, which right after sign-in may not
    /// have assembled yet — so its omission is a set held back, not a set that
    /// does not exist, and the page must not read it as "no folder"
    /// ([`MediaSnapshot::held`]). A member set's omission is not counted: it
    /// never makes the caller folderless (`default_upload_target` takes own
    /// sets only).
    async fn render_sealed_folder_names(
        &self,
        sets: Vec<fauna_client_media::MediaFolder>,
        owner: Option<BackupKey>,
        scope: &mut ReadScope,
    ) -> (Vec<fauna_client_media::MediaFolder>, usize) {
        let mut omitted_own = 0;
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
            let resolver = scope.resolver(self.folder_keys.as_ref()).await;
            let keys = self
                .folder_keys_through(resolver, &row, owner.clone())
                .await
                .0;
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
                path_crypto::SealedLabelRender::Omit => {
                    if !set.member {
                        omitted_own += 1;
                    }
                }
            }
        }
        (rendered, omitted_own)
    }

    async fn render_sealed_paths(
        &self,
        snap: &mut MediaSnapshot,
        owner: Option<BackupKey>,
        scope: &mut ReadScope,
    ) {
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
                let resolver = scope.resolver(self.folder_keys.as_ref()).await;
                let keys = self
                    .folder_keys_through(resolver, &set_key, owner.clone())
                    .await
                    .0;
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
            // hash, and an un-noted hash reads as the current identity's.

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

            if render_item_path(keys, signer, &mut item) {
                item.folder = set_name;
                rendered.push(item);
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
        // Inside a foreign browse scope an item's set is the scope's — by its
        // `foreign:` ref, never by name (`ui/media.md` § Cross-nest shared
        // sets: identity lives on the scope).
        if let Some(active) = s.foreign_active.as_ref()
            && active.items.iter().any(|it| at_path(&it))
        {
            return active.set_key();
        }
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
            // A foreign set has no row on this nest: it resolves to the set the
            // options offer (`ui/media.md` § Cross-nest shared sets), its name
            // the one the record carries — the channel form is its address.
            FolderFilter::Ref(r @ fauna_core::folder_keys::FolderRef::Foreign(channel)) => {
                let s = self.state.lock().unwrap();
                let set = s.raw.foreign_set(channel)?;
                Some((set.name.clone(), SetKey::Ref(*r)))
            }
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
        self.refresh_settling(owner, false).await;
    }

    /// One aggregate re-read and its commit — and, while the page has never
    /// loaded (or when `force_settle`, an upload that found no target), the
    /// **settle**: a read that was not complete ([`MediaSnapshot::held`]
    /// non-zero, or the folder list lost) is re-read on the [`SettlePolicy`]
    /// schedule until one is complete or the budget is spent. Each incomplete
    /// read still commits its rows (they are real) but leaves `loaded` unset,
    /// so no app paints `media-empty-state` over rows merely held back
    /// (`ui/README.md` § List pages: loading is not empty, rule 2).
    ///
    /// Through the read gate ([`crate::read_gate`]): one read in flight; a
    /// refresh asked meanwhile waits for the one trailing read every such ask
    /// shares, and returns once a read that started after it has committed
    /// (`force_settle`: once a settle covering it has ended).
    async fn refresh_settling(&self, owner: Option<BackupKey>, force_settle: bool) {
        use crate::read_gate::{RunGuard, Turn};
        if let Some(key) = &owner {
            *self.read_owner.lock().unwrap() = Some(key.clone());
        }
        let ticket = self.reads.ask(force_settle);
        loop {
            match self.reads.turn(ticket, force_settle).await {
                Turn::Done => return,
                Turn::Run => {
                    let _running = RunGuard(&self.reads);
                    self.run_reads(owner.clone()).await;
                }
            }
        }
    }

    /// The gate's runner: read, commit, and settle while the read is not
    /// complete — each re-read covering every ask made before it started. The
    /// back-off between re-reads is cut short by a new ask, so a gesture's
    /// re-read never waits out a sleep. The first-load budget is the page's
    /// ([`State::first_load_spent`]); a forced settle on a loaded page spends
    /// its own.
    async fn run_reads(&self, owner: Option<BackupKey>) {
        use std::sync::atomic::Ordering;
        let policy = *self.settle.lock().unwrap();
        let mut wait = policy.first;
        let mut waited = std::time::Duration::ZERO;
        loop {
            let (cover, force_settle) = self.reads.begin_read();
            let generation = self.refresh_gen.fetch_add(1, Ordering::SeqCst) + 1;
            let read_owner = self.read_owner.lock().unwrap().clone().or(owner.clone());
            let (result, complete) = self.read_page(read_owner).await;
            // A foreign scope whose last read held heads is re-read on the
            // same settle, and holds the page's completeness like a held row.
            let complete = self.reread_held_foreign_scope().await && complete;
            let budget = SettleBudget {
                first_load_spent: self.state.lock().unwrap().first_load_spent >= policy.budget,
                this_settle_spent: waited >= policy.budget,
            };
            match self
                .commit_read(generation, result, complete, force_settle, budget)
                .await
            {
                None | Some(false) => {
                    if waited > std::time::Duration::ZERO {
                        tracing::info!(
                            target: "fauna_media",
                            complete,
                            "media page settle ended after {waited:?}",
                        );
                    }
                    self.reads.end_settle(cover);
                    return;
                }
                Some(true) => self.reads.finish_read(cover),
            }
            if waited == std::time::Duration::ZERO {
                tracing::info!(
                    target: "fauna_media",
                    "media read held rows or folders back for want of custody; re-reading \
                     (up to {:?}) before the page may read as empty",
                    policy.budget,
                );
            }
            if !self.reads.asked_since(cover) {
                crate::read_gate::first_of(fauna_sleep::sleep(wait), self.reads.ask_after(cover))
                    .await;
            }
            // Counted at the nominal back-off even when an ask cut it short:
            // a push burst then spends the budget sooner, never later.
            waited += wait;
            {
                let mut s = self.state.lock().unwrap();
                if !s.loaded {
                    s.first_load_spent += wait;
                }
            }
            wait = (wait * 2).min(policy.cap);
        }
    }

    /// The page's one read: the judged aggregate, its sealed paths rendered,
    /// plus the control-plane folder list — with whether it is **complete**
    /// (nothing held back for want of custody, and the folder list read).
    async fn read_page(
        &self,
        owner: Option<BackupKey>,
    ) -> (Result<MediaSnapshot, MediaApiError>, bool) {
        match self.nest_api.media_snapshot().await {
            Ok(mut snap) => {
                // One custody view for every set this read renders, opened at
                // the first set that needs one (`ReadScope`).
                let mut scope = ReadScope::default();
                self.render_sealed_paths(&mut snap, owner.clone(), &mut scope)
                    .await;
                self.note_signed_as(&snap.items);
                // The control-plane set list (`fauna.folders.list`) is what
                // makes an EMPTY set choosable — the item aggregate can only
                // ever name sets that already hold media, so deriving the
                // options from it alone left a fresh set unreachable as an
                // upload target and it could never gain its first file
                // (`media.md` § Layout & flow).
                //
                // Deliberately NOT fatal to the page: media is the read that
                // matters, and a failed list (a transient failure) must still
                // render the browse rather than erroring the whole page. Losing
                // the list degrades to the pre-fix item-derived options — but
                // the read is then not complete: item-derived options cannot
                // name an empty own set, so "no folder" would be a guess.
                let folders_read = match self.list_joined_folders().await {
                    Ok(sets) => {
                        let (rendered, omitted_own) = self
                            .render_sealed_folder_names(sets, owner, &mut scope)
                            .await;
                        snap.known_folders = rendered;
                        snap.held += omitted_own;
                        true
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "fauna_media",
                            error = %e.detail(),
                            "listing folders failed; media options fall back to the sets that have media",
                        );
                        false
                    }
                };
                // The foreign browse scopes ride every read beside the
                // control-plane rows (`ui/media.md` § Cross-nest shared sets
                // → *Which sets are offered*).
                snap.foreign_sets = self.read_foreign_sets(&snap.known_folders).await;
                let complete = folders_read && snap.held == 0;
                (Ok(snap), complete)
            }
            Err(e) => (Err(e), false),
        }
    }

    /// Commit one read to the page and notify. `None` = superseded (dropped
    /// untouched); `Some(true)` = the settle re-reads; `Some(false)` = done.
    /// `budget` says which settle budgets are spent: once the one that applies
    /// is (the page's first-load budget before it loaded, the forced settle's
    /// own after), an incomplete read loads the page with what it found.
    async fn commit_read(
        &self,
        generation: u64,
        result: Result<MediaSnapshot, MediaApiError>,
        complete: bool,
        force_settle: bool,
        budget: SettleBudget,
    ) -> Option<bool> {
        use std::sync::atomic::Ordering;
        // Followed browse-scope options ride the same refresh (best-effort,
        // like the folders list; the source's staleness budget makes a
        // warm-cache pass cheap). Independent of the aggregate read's outcome:
        // the options render either way.
        let followed_options = match self.followed_source.get() {
            Some(source) => Some(source.followed_scopes().await),
            None => None,
        };
        let settle_more = {
            let mut s = self.state.lock().unwrap();
            // A newer read already LANDED → drop this stale result untouched;
            // the page already shows something fresher. A newer read merely in
            // flight drops nothing: this one lands, and the newer one commits
            // over it when it lands (newest-landed wins — `refresh_gen`'s doc).
            // Strict: a settle's own re-reads share its generation.
            if generation < s.committed_gen {
                return None;
            }
            s.committed_gen = generation;
            if let Some(options) = followed_options {
                s.followed_options = options;
            }
            match result {
                Ok(snap) => {
                    // The read RETURNED — and what it returned is the fact that
                    // separates "the page is empty because this actor has
                    // nothing" from "the page never got an answer". Counts only
                    // (no names, no paths).
                    tracing::debug!(
                        target: "fauna_media",
                        items = snap.items.len(),
                        folders = snap.known_folders.len(),
                        held = snap.held,
                        complete,
                        "media refresh returned",
                    );
                    let settling = (!s.loaded || force_settle) && !complete;
                    let exhausted = if s.loaded {
                        budget.this_settle_spent
                    } else {
                        budget.first_load_spent
                    };
                    s.raw = snap;
                    // A press's answer outlives a read that merely re-lists
                    // the page (`held_error`'s doc).
                    if s.error != s.held_error {
                        s.error = None;
                    }
                    s.complete = complete;
                    if settling && exhausted {
                        tracing::warn!(
                            target: "fauna_media",
                            "media reads still held rows or folders back when the settle budget \
                             ran out; the page loads with what they found",
                        );
                    }
                    // The one place the page becomes loaded. Only a COMPLETE
                    // read licenses `media-empty-state` — or the settle's last,
                    // once its budget is spent; an incomplete read on a page
                    // already loaded keeps it loaded (monotonic). The `Err` arm
                    // below deliberately leaves this alone (see the field's doc
                    // on `MediaPageSnapshot`).
                    if !settling || exhausted {
                        s.loaded = true;
                    }
                    settling && !exhausted
                }
                Err(e) => {
                    let err = error_text(REFRESH_ERROR_KEY, e.detail());
                    // Producer-side log for the reactive error banner — fire once
                    // here, not in the per-tick render (observability.md § Log on
                    // the *event*, not the *paint*). `log_line` is redaction-safe.
                    tracing::warn!(target: "fauna_media", "{}", err.log_line());
                    s.error = Some(err);
                    s.complete = false;
                    false
                }
            }
        };
        self.observer.on_changed();
        // Only the latest-started read goes on settling (the read gate keeps
        // reads from overlapping; this is the backstop).
        (self.refresh_gen.load(Ordering::SeqCst) == generation).then_some(settle_more)
    }

    /// An upload that resolved no target on a page whose reads never
    /// completed: settle first (re-read until complete or the budget is
    /// spent), so the target is resolved from a complete answer. Returns
    /// whether the page's last read is complete.
    async fn settle_before_refusing(&self, backup_key: &[u8]) -> bool {
        let complete = {
            let s = self.state.lock().unwrap();
            s.loaded && s.complete
        };
        if complete {
            return true;
        }
        let owner = <[u8; 32]>::try_from(backup_key)
            .ok()
            .map(BackupKey::from_bytes)
            .or_else(|| self.owner_key());
        self.refresh_settling(owner, true).await;
        self.state.lock().unwrap().complete
    }

    /// The refusal for an upload with no target: `media.error_no_set` only
    /// when a complete read found no own folder (`media.md` § Errors & edge
    /// cases); otherwise the folders have not loaded yet.
    fn refuse_no_target(&self, complete: bool) {
        if complete {
            self.set_error_no_upload_target();
        } else {
            self.set_error(REFRESH_ERROR_KEY, FOLDERS_NOT_LOADED_DETAIL);
        }
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
        self.folder_keys_through(self.folder_keys.clone(), set, owner)
            .await
    }

    /// [`Self::folder_keys_for_key`] through `resolver` — the machine's own, or
    /// one page read's [`ReadScope`] over it. The same assembly either way;
    /// only where the roster and custody come from differs.
    async fn folder_keys_through(
        &self,
        resolver: Option<Arc<dyn crate::folder_keys::FolderKeyResolver>>,
        set: &SetKey,
        owner: Option<BackupKey>,
    ) -> (
        FileDownloadKeys,
        Option<fauna_core::folder_keys::ForeignHome>,
    ) {
        let custody = fauna_core::label_custody::LabelCustody::new(resolver, owner)
            // Read candidates for a successor's predecessor-sealed media; the
            // seal-side twin below deliberately does NOT get them.
            .with_predecessor_keys(self.predecessor_keys());
        match set {
            SetKey::Ref(folder_ref) => custody.keys_for_ref(folder_ref).await,
            SetKey::Name(name_hash) => custody.keys_for_hash(name_hash).await,
        }
    }

    /// Open one recorded manifest of `set` through the single-file walk under
    /// that row's custody — the one byte read [`Self::download_file`] and
    /// [`Self::fetch_thumbnail`] share, so a file and its thumbnail can never
    /// open under different keys. `signer` is who signed the row naming the
    /// manifest (ruling (8)(c)); `stamp` the row's generation (ruling (10)(c));
    /// `what` prefixes the error detail.
    #[allow(clippy::too_many_arguments)]
    async fn open_manifest(
        &self,
        set: SetKey,
        folder: &str,
        digest: [u8; 32],
        content_key_version: Option<u64>,
        relative_path: &str,
        key: BackupKey,
        signer: RecordSigner,
        what: &str,
    ) -> Result<Vec<u8>, MediaApiError> {
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
        let (keys, foreign_home) = self.folder_keys_for_key(&set, Some(key)).await;
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
        let hint = fauna_core::file_download::FolderHint::new(folder).with_ref(set.folder_ref());
        if let Some(group_id) = keys.mls_group_id.as_deref() {
            // A FOREIGN (cross-nest) set's bytes live on its home nest — fetch
            // through the factory's MEMBER door bound to that base URL,
            // verified against the grant-delivered home identity, its folder
            // reads under the read token the member's own nest mints for this
            // set's channel and hinted by the set's `foreign:<channel>` ref:
            // the bearer and the row the home nest's store-miss arm resolves
            // against its cross-nest roster at every read, so a metadata-only
            // folder's chunk is relayed from a seat and a removal ends the
            // reads (`relay-serving.md` § Relay serving → *A member on another
            // nest*, step (5)). A same-nest set uses the machine's own
            // injected fetcher, hinted by the item's own ref.
            let (fetcher, hint): (Arc<dyn fauna_core::file_download::BlobFetcher>, _) =
                match foreign_home.as_ref() {
                    Some(home) => {
                        let channel = fauna_core::folder_keys::channel_id_for_group(group_id);
                        let fetcher = self
                            .foreign_fetchers
                            .get()
                            .ok_or_else(|| MediaApiError::Transient {
                                detail: "cross-nest file download is not yet supported on \
                                         this platform"
                                    .to_string(),
                            })?
                            .member_fetcher_for(home, channel);
                        let hint = hint
                            .with_ref(Some(fauna_core::folder_keys::FolderRef::Foreign(channel)));
                        (fetcher, hint)
                    }
                    None => (
                        self.download_fetcher
                            .clone()
                            .ok_or_else(|| MediaApiError::Transient {
                                detail: "media file download is not yet supported on this \
                                         platform"
                                    .to_string(),
                            })?,
                        hint,
                    ),
                };
            // The owner key rides in `keys` for a bound set too, but
            // `effective_backup_key` suppresses it on every chunk site
            // (FS-5DC) — a bound set's chunks stay content-keyed, exactly as
            // before this factor.
            return fauna_core::file_download::download_file_bytes_by_manifest(
                &fauna_core::file_download::FolderHinted::new(fetcher.as_ref(), hint),
                &keys,
                fauna_core::data::ContentHash::from_digest_raw(digest),
                content_key_version.map(fauna_core::blob_seal::SealStamp::Version),
                relative_path,
            )
            .await
            .map_err(|e| shared_read_error(what, &e));
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
            content_key_version.map(fauna_core::blob_seal::SealStamp::Version),
            relative_path,
        )
        .await
        // The walk's failures (fetch, open, hash mismatch) collapse to one
        // detail string: the page shows it verbatim and nothing branches on the
        // variant, and the walk's anyhow chain doesn't discriminate retryable
        // from permanent for us.
        .map_err(|e| MediaApiError::Transient {
            detail: format!("{what} failed: {e:#}"),
        })
    }

    /// Open a version another signature recorded and re-seal it under the
    /// **current** owner root — the only way such a version is restored
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (8)(d), the restore sentence). Answers the `(manifest hash, size,
    /// generation, thumbnail hash)` the restore records, or the detail it
    /// refuses with.
    ///
    /// The bytes open under the signer's own retired root and its
    /// predecessors' alone (`FileDownloadKeys::record_signer`) — exactly the roots ruling (8)(c) lets a
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
    ///
    /// The version's thumbnail moves with it (`media.md` § Encryption at rest
    /// → *A thumbnail rests as its file rests*, bound (2)): the manifest its
    /// row names is re-sealed exactly as the file — the same windowed re-seal,
    /// the same signer bound, the same root — and the restore records the new
    /// hash. A tile that does not open is dropped, never a reason to refuse the
    /// file: the download backfill re-renders it.
    async fn reseal_inherited_version(
        &self,
        set: SetKey,
        path: &str,
        version: &FileVersionSummary,
    ) -> Result<(String, i64, Option<u64>, Option<String>), String> {
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
        let thumbnail_hash = match version.thumbnail_hash.as_deref() {
            Some(thumb_hex) => match manifest_digest(thumb_hex, "the version's thumbnail hash") {
                Ok(thumb) => fauna_core::nest_reseal::reseal_nest_copy_windowed(
                    fetcher.as_ref(),
                    &record_keys,
                    fauna_core::data::ContentHash::from_digest_raw(thumb),
                    None,
                    path,
                    Some(seal_root),
                    &sink,
                )
                .await
                .map(|t| hex::encode(t.manifest_hash.digest()))
                .map_err(|e| format!("{e:#}")),
                Err(e) => Err(e.detail().to_string()),
            }
            .inspect_err(|e| {
                tracing::warn!(
                    target: "fauna_media",
                    error = %e,
                    "the restored version's thumbnail did not re-seal; recorded without one"
                );
            })
            .ok(),
            None => None,
        };
        Ok((
            hex::encode(resealed.manifest_hash.digest()),
            resealed.manifest.total_size as i64,
            None,
            thumbnail_hash,
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
            // A foreign browse scope, while active, IS the page's item list
            // too — its judged heads through the same view sort, each item's
            // `filter_value` the scope's own ref (`ui/media.md` § Cross-nest
            // shared sets: identity lives on the scope).
            None if s.foreign_active.is_some() => {
                let active = s.foreign_active.as_ref().expect("checked above");
                let value = active.set.option_value();
                let view = MediaSnapshot {
                    items: active.items.clone(),
                    ..MediaSnapshot::default()
                };
                let items = view
                    .view(None, s.sort, s.descending)
                    .into_iter()
                    .map(|item| MediaItemSummary {
                        filter_value: value.clone(),
                        ..MediaItemSummary::from(item)
                    })
                    .collect();
                (items, Some(value), Some(active.set.option_label()), None)
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
        let filter = FolderFilter::parse(&folder);
        let mut resolved = self.resolve_target(&filter);
        let mut complete = true;
        if resolved.is_none() {
            // No such set on a page whose reads never completed: settle and
            // look again before telling the user the folder is not there.
            complete = self.settle_before_refusing(&backup_key).await;
            resolved = self.resolve_target(&filter);
        }
        match resolved {
            Some((name, target)) => {
                self.upload_into(name, target, device_id, path, raw_bytes, backup_key)
                    .await
            }
            None => self.refuse_no_target(complete),
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
        let pick = |s: &State| {
            s.filter
                .as_ref()
                .map(FolderFilter::value)
                .or_else(|| s.raw.default_upload_target())
        };
        // Inside a foreign browse scope the filter holds the scope's
        // `foreign:` ref, so the upload targets the set the user is looking at;
        // a reader's or a metadata-only set's refusal is `upload_into`'s
        // (`ui/media.md` § Cross-nest shared sets, *Upload*).
        let (followed_active, mut target) = {
            let mut s = self.state.lock().unwrap();
            // A new press: the previous press's answer no longer stands.
            s.drop_held_error();
            (s.followed_active.is_some(), pick(&s))
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
        let mut complete = true;
        if target.is_none() {
            // No own folder on a page whose reads never completed — right
            // after sign-in every signed row and sealed own-set name is held
            // until custody reads. Settle first; "create a folder first" is
            // only true of a complete read (`media.md` § Errors & edge cases).
            complete = self.settle_before_refusing(&backup_key).await;
            target = pick(&self.state.lock().unwrap());
        }
        match target {
            Some(folder) => {
                self.upload(folder, device_id, path, raw_bytes, backup_key)
                    .await
            }
            None => self.refuse_no_target(complete),
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
        // A writer member's delete in a set homed on another nest is the same
        // relayed record door the upload uses — a tombstone (`ui/media.md`
        // § Cross-nest shared sets, *Upload*); the home nest's writer gate
        // decides it.
        if let SetKey::Ref(fauna_core::folder_keys::FolderRef::Foreign(channel)) = target {
            let home = self
                .state
                .lock()
                .unwrap()
                .raw
                .foreign_set(&channel)
                .map(|f| f.home_nest_url.clone());
            let Some(home) = home else {
                self.set_error(DELETE_ERROR_KEY, &format!("no listed set is {folder}"));
                return;
            };
            match self
                .nest_api
                .record_foreign(
                    channel,
                    &home,
                    &name,
                    &device_id,
                    &path,
                    None,
                    0,
                    "delete",
                    None,
                    None,
                    path_sealed,
                )
                .await
            {
                Ok(()) => {
                    self.refresh_with_owner(self.owner_key()).await;
                    self.reread_foreign_scope_of(target).await;
                }
                Err(e) => self.set_error(DELETE_ERROR_KEY, e.detail()),
            }
            return;
        }
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
        // Inside a foreign browse scope the history is the home nest's,
        // relayed through the member's own nest and judged under the scope's
        // custody (`ui/media.md` § Cross-nest shared sets): every version is
        // noted under the scope's `SetKey`, so the detail's download and a
        // writer's restore name exactly a manifest the judge admitted, never
        // one the own nest could answer for a namesake.
        if let FolderFilter::Ref(fauna_core::folder_keys::FolderRef::Foreign(channel)) =
            FolderFilter::parse(&folder)
        {
            return self.foreign_versions(channel, &path).await;
        }
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
        // A version of a set homed on another nest (`ui/media.md` § Cross-nest
        // shared sets): read under the scope's custody and verdicts — the key
        // `Self::foreign_versions` noted it under — and re-pointed through the
        // relayed record door the upload and delete use; a reader member is
        // refused here, as its upload is, before anything is sealed or sent.
        let foreign = match FolderFilter::parse(&folder) {
            FolderFilter::Ref(fauna_core::folder_keys::FolderRef::Foreign(channel)) => {
                let s = self.state.lock().unwrap();
                match s.raw.foreign_set(&channel) {
                    Some(f) if !f.writer => {
                        drop(s);
                        let err = LocalizedText::key(READ_ONLY_SHARED_FOLDER_ERROR_KEY);
                        tracing::warn!(target: "fauna_media", "{}", err.log_line());
                        self.state.lock().unwrap().error = Some(err);
                        self.observer.on_changed();
                        return;
                    }
                    Some(f) => Some((channel, f.home_nest_url.clone(), f.name.clone())),
                    None => {
                        drop(s);
                        self.set_error(RESTORE_ERROR_KEY, &format!("no listed set is {folder}"));
                        return;
                    }
                }
            }
            _ => None,
        };
        // The version is read under its own row's custody and verdicts, and
        // re-pointed into that row (`Self::version_set_key`): a writer
        // member's restore of a shared set's version lands there, never in a
        // same-named own set the name resolves to first.
        let set = match &foreign {
            Some((channel, _, _)) => {
                SetKey::Ref(fauna_core::folder_keys::FolderRef::Foreign(*channel))
            }
            None => self.version_set_key(&folder, &path, &version),
        };
        let folder_id = version.folder_id;
        // Same seal contract as `delete` — the re-point row is append-only
        // nest-side, so the gesture is this row's only chance to seal. A
        // version naming its row (or its foreign scope) seals under that
        // set's custody; one without goes by name, so it seals for the set the
        // name writes into.
        let path_sealed = match (folder_id, &foreign) {
            (_, Some(_)) | (Some(_), None) => self.seal_record_path(set, &path).await,
            (None, None) => self.seal_gesture_path(&folder, &path).await,
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
        let (manifest_hash, size_bytes, content_key_version, thumbnail_hash) =
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
                // so the listed decision never refuses. Its thumbnail rests
                // as its bytes rest, so it re-points with them verbatim.
                RestoreDecision::Verbatim | RestoreDecision::Refuse => (
                    version.manifest_hash,
                    version.size_bytes,
                    version.content_key_version,
                    version.thumbnail_hash,
                ),
            };
        if let Some((channel, home, name)) = foreign {
            // `SyncClient::restore_version`'s shape over the relayed door: a
            // `modify` naming the historical manifest, its generation verbatim.
            match self
                .nest_api
                .record_foreign(
                    channel,
                    &home,
                    &name,
                    &device_id,
                    &path,
                    Some(manifest_hash),
                    size_bytes,
                    "modify",
                    content_key_version,
                    thumbnail_hash,
                    path_sealed,
                )
                .await
            {
                Ok(()) => {
                    self.refresh_with_owner(self.owner_key()).await;
                    self.reread_foreign_scope_of(set).await;
                }
                Err(e) => self.set_error(RESTORE_ERROR_KEY, e.detail()),
            }
            return;
        }
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
                thumbnail_hash,
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

    /// Fetch + open the thumbnail at `thumbnail_hash` (a
    /// `MediaItemSummary.thumbnail_hash`) into paint-ready image bytes, so every
    /// app just paints decoded bytes (priority #2 — the whole render fetch is
    /// shared Rust, not hand-rolled per client).
    ///
    /// **A thumbnail rests as its file rests** (`media.md` § Encryption at rest
    /// → *A thumbnail rests as its file rests*): the hash names a chunk
    /// manifest sealed under the root the file's row names, so it opens exactly
    /// as the file does — the single-file walk
    /// (`fauna_core::file_download::download_file_bytes_by_manifest`) under the
    /// row's own `FileDownloadKeys`: the row's custody (a shared or served
    /// set's content keys, the owner root, or none for plaintext chunks), the
    /// row's stamp (a stamped row opens under its generation and never an owner
    /// root — ruling (10)(c), by construction), the row's signer bound (ruling
    /// (8)(c)) and the predecessor roots a successor inherited. The walk is the
    /// content-address check: the manifest is fetched by the hash asked for and
    /// every chunk by its store key, the reassembled bytes checked against the
    /// manifest's file hash — a nest cannot substitute another tile.
    ///
    /// The row is the listed item naming the hash; the gesture names nothing
    /// else. Every item naming it is tried, first open wins — two rows only
    /// share a thumbnail when they share its bytes and root. A hash no listed
    /// item names is refused (`NotFound`): with no row there is no custody to
    /// open it under, and none is guessed.
    ///
    /// `backup_key` is the client's 32-byte owner backup key (`backup_key_derive`)
    /// — injected per call, as `upload()` takes it (the machine holds no key).
    ///
    /// Unlike the page gestures, this is a **per-item query**: it returns the
    /// decoded bytes (or the error) to the caller and does **not** touch the page
    /// `error-message` banner — one unreadable thumbnail must not blank the page;
    /// the client falls back to the placeholder. On a platform with no download
    /// fetcher wired it returns a `Transient` "not supported" error.
    pub async fn fetch_thumbnail(
        &self,
        thumbnail_hash: String,
        backup_key: Vec<u8>,
    ) -> Result<Vec<u8>, MediaApiError> {
        // Fail fast if no download fetcher is wired on this platform — before
        // any work. A foreign set's thumbnail rides the member door, so the
        // foreign factory alone is a wired leg (`open_manifest` refuses a row
        // whose own leg is missing).
        if self.download_fetcher.is_none() && self.foreign_fetchers.get().is_none() {
            return Err(MediaApiError::Transient {
                detail: "media thumbnail fetch is not yet supported on this platform".to_string(),
            });
        }
        // A wrong key length / malformed hash is a caller bug → BadRequest
        // (no fetch attempted).
        let key_bytes: [u8; 32] =
            backup_key
                .try_into()
                .map_err(|v: Vec<u8>| MediaApiError::BadRequest {
                    detail: format!("backup key must be 32 bytes, got {}", v.len()),
                })?;
        let digest = manifest_digest(&thumbnail_hash, "thumbnail hash")?;
        let key = BackupKey::from_bytes(key_bytes);

        // The rows naming this thumbnail, each with what its walk needs: its
        // set (custody by the row — `media.md` § Encryption at rest → *A set
        // is addressed by its row*), its path, its stamp and its signer.
        let rows: Vec<(SetKey, String, String, Option<u64>, RecordSigner)> = {
            let st = self.state.lock().unwrap();
            // The active foreign scope's heads first — their set is the
            // scope's ref, and their thumbnails open under its custody
            // through the member door, exactly as their files do.
            let foreign = st.foreign_active.as_ref().map(|a| (a.set_key(), &a.items));
            foreign
                .into_iter()
                .flat_map(|(set, items)| items.iter().map(move |it| (Some(set), it)))
                .chain(st.raw.items.iter().map(|it| (None, it)))
                .filter(|(_, it)| {
                    it.thumbnail_hash
                        .as_deref()
                        .is_some_and(|h| h.eq_ignore_ascii_case(&thumbnail_hash))
                })
                .map(|(set, it)| {
                    (
                        set.unwrap_or_else(|| SetKey::of(it.folder_id, &it.folder)),
                        it.folder.clone(),
                        it.path.clone(),
                        it.content_key_version,
                        self.signer_of(it.signed_as_current, it.signed_as),
                    )
                })
                .collect()
        };
        let mut last = MediaApiError::NotFound {
            detail: "no listed item names this thumbnail".to_string(),
        };
        for (set, folder, path, stamp, signer) in rows {
            match self
                .open_manifest(
                    set,
                    &folder,
                    digest,
                    stamp,
                    &path,
                    key.clone(),
                    signer,
                    "thumbnail",
                )
                .await
            {
                Ok(bytes) => return Ok(bytes),
                Err(e) => last = e,
            }
        }
        Err(last)
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
        let digest = manifest_digest(&manifest_hash, "manifest hash")?;
        let key = BackupKey::from_bytes(key_bytes);
        // Custody, signer and hint all key by the downloaded item's own row
        // (`media.md` § Encryption at rest → *A set is addressed by its row*),
        // so a reader's own same-named set never answers for a shared one.
        let set = self.item_set_key(&folder, &relative_path, Some(&manifest_hash));
        // Ruling (8)(c): the row naming this manifest in this set decides
        // whether the current owner root may open it. A row signed under a
        // retired identity gets the retired roots alone, on every arm below.
        let signer = self.signer_for(set, &manifest_hash);
        self.open_manifest(
            set,
            &folder,
            digest,
            content_key_version,
            &relative_path,
            key,
            signer,
            "file download",
        )
        .await
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
    ///
    /// A **foreign** set's value (`foreign:<channel hex>`, an option
    /// `folder_options` offered) enters that set's browse scope with no face
    /// work (`ui/media.md` § Cross-nest shared sets): its listing is fetched on
    /// the machine's own background executor ([`fauna_sleep::spawn_background`]
    /// — the caller may hold no runtime, or one that dies on return) and lands
    /// through the observer. Nothing changes until it answers, so a transport
    /// fault keeps the prior browse ([`Self::select_foreign_scope`]).
    pub fn set_filter(self: Arc<Self>, folder: Option<String>) {
        let generation = self.next_scope();
        let filter = folder.as_deref().map(FolderFilter::parse);
        if let Some(FolderFilter::Ref(fauna_core::folder_keys::FolderRef::Foreign(channel))) =
            filter
        {
            fauna_sleep::spawn_background(async move {
                self.enter_foreign_scope(channel, generation).await;
            });
            return;
        }
        {
            let mut s = self.state.lock().unwrap();
            s.drop_held_error();
            s.filter = filter;
            s.followed_active = None;
            s.foreign_active = None;
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
        // A foreign scope's fetch still in flight must not land over this one.
        self.next_scope();
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
                    s.foreign_active = None;
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
                    s.foreign_active = None;
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
    /// Claim a new browse-scope generation ([`Self::scope_gen`]).
    fn next_scope(&self) -> u64 {
        self.scope_gen
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    /// Whether `generation` is still the newest scope change.
    fn scope_is_current(&self, generation: u64) -> bool {
        self.scope_gen.load(std::sync::atomic::Ordering::SeqCst) == generation
    }

    /// Enter the foreign browse scope of the set whose channel is `channel`
    /// (`ui/media.md` § Cross-nest shared sets — the foreign browse scope):
    /// one relayed listing, on demand, committed only while `generation` is
    /// still the newest scope change. The two failure families stay apart —
    /// the home nest's refusal of the membership enters the scope EMPTY with
    /// the removal on `error-message` (the option stays listed); a transport
    /// fault keeps the prior browse and reports itself. A channel no option
    /// offers (a set left since the options rendered) reports rather than
    /// silently no-opping (e2e convention 11).
    async fn enter_foreign_scope(&self, channel: [u8; 32], generation: u64) {
        let set = self
            .state
            .lock()
            .unwrap()
            .raw
            .foreign_set(&channel)
            .cloned();
        let Some(set) = set else {
            if self.scope_is_current(generation) {
                self.set_error(
                    REFRESH_ERROR_KEY,
                    "that shared folder is no longer in the list",
                );
            }
            return;
        };
        let read = self.read_foreign_scope(&set).await;
        if !self.scope_is_current(generation) {
            return;
        }
        match read {
            Ok((items, held)) => {
                let mut s = self.state.lock().unwrap();
                s.filter = Some(FolderFilter::Ref(set.folder_ref()));
                s.followed_active = None;
                s.foreign_active = Some(ForeignActive { set, items, held });
                s.error = None;
            }
            Err(MediaApiError::NotFound { detail }) => {
                let err = LocalizedText::key(FOREIGN_REMOVED_ERROR_KEY);
                tracing::warn!(target: "fauna_media", detail = %detail, "{}", err.log_line());
                let mut s = self.state.lock().unwrap();
                s.filter = Some(FolderFilter::Ref(set.folder_ref()));
                s.followed_active = None;
                s.foreign_active = Some(ForeignActive {
                    set,
                    items: Vec::new(),
                    held: 0,
                });
                s.error = Some(err);
            }
            Err(e) => {
                self.set_error(REFRESH_ERROR_KEY, e.detail());
                return;
            }
        }
        self.observer.on_changed();
    }

    /// One read of a foreign scope: the seam's judged heads, each label
    /// rendered under the scope's custody (`LabelCustody::keys_for_ref(Foreign)`
    /// — the custody its download opens under, so a name and its bytes never
    /// open under different keys), each signer noted for the gestures, each
    /// item stamped with the home nest's reachability. Answers the rendered
    /// items and how many heads the judge held.
    async fn read_foreign_scope(
        &self,
        set: &fauna_client_media::MediaForeignSet,
    ) -> Result<(Vec<MediaItem>, usize), MediaApiError> {
        let listing = self
            .nest_api
            .foreign_listing(set.channel, &set.home_nest_url)
            .await?;
        let source_online = listing.source_online();
        let set_key = SetKey::Ref(set.folder_ref());
        let owner = self
            .read_owner
            .lock()
            .unwrap()
            .clone()
            .or_else(|| self.owner_key());
        let (keys, _) = self.folder_keys_for_key(&set_key, owner).await;
        let name = if set.name.is_empty() {
            set.option_label()
        } else {
            set.name.clone()
        };
        let chain = self.predecessor_keys();
        let total = listing.items.len();
        let mut items = Vec::with_capacity(total);
        for mut item in listing.items {
            let signer = self.signer_of(item.signed_as_current, item.signed_as);
            if let Some(manifest) = item.manifest_hash.as_ref() {
                self.signed_as.lock().unwrap().note(
                    set_key,
                    &hex::encode(&manifest[..]),
                    signer,
                    &chain,
                );
            }
            if render_item_path(&keys, signer, &mut item) {
                item.folder = name.clone();
                item.source_online = source_online;
                items.push(item);
            }
        }
        if items.len() < total {
            tracing::warn!(
                target: "fauna_media",
                dropped = total - items.len(),
                total,
                "foreign scope rows omitted: their labels could not be rendered under the \
                 scope's custody",
            );
        }
        Ok((items, listing.held))
    }

    /// A foreign scope item's version history ([`Self::file_versions`]): the
    /// seam's relayed, judged versions, each signer noted under the scope's
    /// [`SetKey`] — the key the scope's listing notes its heads under and the
    /// restore reads ([`Self::restore_version`]). `NotFound` when the page
    /// lists no such foreign set, or when the home nest refused the membership.
    async fn foreign_versions(
        &self,
        channel: [u8; 32],
        path: &str,
    ) -> Result<Vec<FileVersionSummary>, MediaApiError> {
        let home = self
            .state
            .lock()
            .unwrap()
            .raw
            .foreign_set(&channel)
            .map(|f| f.home_nest_url.clone());
        let Some(home) = home else {
            return Err(MediaApiError::NotFound {
                detail: "no listed shared folder homed on another nest is this one".to_string(),
            });
        };
        let listed = self.nest_api.foreign_versions(channel, &home, path).await?;
        let set = SetKey::Ref(fauna_core::folder_keys::FolderRef::Foreign(channel));
        let chain = self.predecessor_keys();
        let signers: Vec<RecordSigner> = listed
            .iter()
            .map(|v| self.signer_of(v.signed_as_current, v.signed_as))
            .collect();
        let mut ledger = self.signed_as.lock().unwrap();
        Ok(listed
            .into_iter()
            .zip(signers)
            .map(|(version, signer)| {
                ledger.note(set, &version.summary.manifest_hash, signer, &chain);
                version.summary
            })
            .collect())
    }

    /// The page's settle re-reads a foreign scope whose last read HELD heads
    /// (custody not synced yet), exactly as it re-reads held aggregate rows
    /// (`ui/media.md` § Cross-nest shared sets, *Verification*). Answers
    /// whether the scope holds nothing back now — `true` with no scope, or one
    /// whose read held nothing. A failed re-read changes nothing.
    async fn reread_held_foreign_scope(&self) -> bool {
        self.reread_foreign_scope(true).await
    }

    /// Re-read the active foreign scope after a write into the set `target`
    /// names — the upload's new head, the delete's tombstone — so the browse
    /// shows what the home nest now lists. A no-op for any other set, or when
    /// the scope moved on.
    async fn reread_foreign_scope_of(&self, target: SetKey) {
        let SetKey::Ref(fauna_core::folder_keys::FolderRef::Foreign(channel)) = target else {
            return;
        };
        let active = self
            .state
            .lock()
            .unwrap()
            .foreign_active
            .as_ref()
            .is_some_and(|a| a.set.channel == channel);
        if active {
            self.reread_foreign_scope(false).await;
        }
    }

    /// One re-read of the active foreign scope — only one whose last read held
    /// heads when `held_only` (the settle's re-read). Answers whether the
    /// scope holds nothing back now — `true` with no scope. A failed re-read
    /// changes nothing.
    async fn reread_foreign_scope(&self, held_only: bool) -> bool {
        let (set, generation) = {
            let s = self.state.lock().unwrap();
            match s.foreign_active.as_ref() {
                Some(active) if !held_only || active.held > 0 => (
                    active.set.clone(),
                    self.scope_gen.load(std::sync::atomic::Ordering::SeqCst),
                ),
                _ => return true,
            }
        };
        let Ok((items, held)) = self.read_foreign_scope(&set).await else {
            return false;
        };
        if !self.scope_is_current(generation) {
            return true;
        }
        {
            let mut s = self.state.lock().unwrap();
            match s.foreign_active.as_mut() {
                Some(active) if active.set.channel == set.channel => {
                    active.items = items;
                    active.held = held;
                }
                _ => return true,
            }
        }
        self.observer.on_changed();
        held == 0
    }

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
            match target {
                // A foreign set refuses on the same two readings, off its
                // custody record, the metadata-only one first.
                SetKey::Ref(fauna_core::folder_keys::FolderRef::Foreign(channel)) => {
                    s.raw.foreign_set(&channel).and_then(|f| {
                        if f.metadata_only {
                            Some(METADATA_ONLY_FOLDER_ERROR_KEY)
                        } else if !f.writer {
                            Some(READ_ONLY_SHARED_FOLDER_ERROR_KEY)
                        } else {
                            None
                        }
                    })
                }
                _ => target.row_in(&s.raw.known_folders).and_then(|f| {
                    if f.metadata_only {
                        Some(METADATA_ONLY_FOLDER_ERROR_KEY)
                    } else if f.member && !f.writer {
                        Some(READ_ONLY_SHARED_FOLDER_ERROR_KEY)
                    } else {
                        None
                    }
                }),
            }
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
            // the one that seals on the way in. A foreign set's listing is
            // its scope's, so the scope re-reads too.
            Ok(()) => {
                self.refresh(Some(backup_key)).await;
                self.reread_foreign_scope_of(target).await;
            }
            Err(e) => {
                // A `path_seal_required` refusal means the nest's view of the
                // folder's audience differs from what this upload assumed:
                // re-read the snapshot so the page no longer shows the stale
                // verdict. The refresh clears the error, so set it after. Keyed
                // on the variant: the nest's detail is prose without the code.
                if matches!(e, MediaApiError::SealRequired { .. }) {
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
    /// | content-keyed (served / shared) | current content key, stamped | the same generation | a manifest under the same generation |
    /// | owner-only | `BackupKey::convergent_chunk_root`, unstamped | the owner root | a manifest under the same root |
    /// | `public` audience | none — plaintext chunks | none — names are URLs | a plaintext manifest |
    ///
    /// So every manifest reader opens what this page wrote with no second arm:
    /// the WebDAV MDA and a member (content-keyed), a synced desktop's hydrate
    /// and a private link's envelope (owner-only), the web serve's one door
    /// and a public link (public). The single sealed blob
    /// (`fauna_media::process_and_seal`) stays the attachment plane's and is
    /// never a folder file's shape. The thumbnail takes the file's custody
    /// (`media.md` § Encryption at rest → *A thumbnail rests as its file
    /// rests*): the same seal root, so whoever opens the file opens its tile.
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
        //    any sealing work. A foreign set's bytes go to its home nest
        //    through the member write door, chosen in step 3.
        let own_uploader = self.blob_uploader.as_ref();
        if own_uploader.is_none()
            && !matches!(
                target,
                SetKey::Ref(fauna_core::folder_keys::FolderRef::Foreign(_))
            )
        {
            return Err(MediaApiError::Transient {
                detail: "media upload is not yet supported on this platform".to_string(),
            });
        }

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
        // A cross-nest set (`ui/media.md` § Cross-nest shared sets, *Upload*):
        // addressed by its channel, its bytes POSTed DIRECT to its home nest
        // through the member write door, its record relayed through this
        // nest. Never the own nest's uploader, which would rest the bytes on
        // a nest that holds none of the set.
        let foreign = match (target, foreign_home) {
            (SetKey::Ref(fauna_core::folder_keys::FolderRef::Foreign(channel)), Some(home)) => {
                Some((channel, home))
            }
            (SetKey::Ref(fauna_core::folder_keys::FolderRef::Foreign(_)), None) => {
                return Err(MediaApiError::Transient {
                    detail: "this shared folder's home nest is not in this device's custody \
                             yet — try again once the folder's custody has synced"
                        .to_string(),
                });
            }
            (_, Some(_)) => {
                return Err(MediaApiError::Transient {
                    detail: FOREIGN_WRITE_UNSUPPORTED.to_string(),
                });
            }
            (_, None) => None,
        };
        let member_uploader = match &foreign {
            Some((channel, home)) => Some(
                self.foreign_fetchers
                    .get()
                    .and_then(|f| f.member_uploader_for(home, *channel))
                    .ok_or_else(|| MediaApiError::Transient {
                        detail: FOREIGN_WRITE_UNSUPPORTED.to_string(),
                    })?,
            ),
            None => None,
        };
        let uploader: &dyn crate::blob_uploader::MediaBlobUploader = match &member_uploader {
            Some(u) => u.as_ref(),
            None => own_uploader
                .expect("checked in step 1 for every non-foreign target")
                .as_ref(),
        };
        // A foreign set is content-keyed or nothing: its members open it
        // under its content keys alone, so an owner-keyed seal would rest
        // bytes no member could read.
        if foreign.is_some() && !keys.is_content_keyed() {
            return Err(MediaApiError::Transient {
                detail: "this shared folder's content keys are not on this device yet — try \
                         again once the folder's custody has synced"
                    .to_string(),
            });
        }
        let custody = if keys.is_content_keyed() {
            // A content-keyed set fails CLOSED, never owner-keyed: with the
            // set's content keys not in this device's custody yet (the
            // serve-enable or join custody write racing this device) the
            // gesture refuses and uploads nothing — the read-side twin of the
            // engine's `ServedKeysMissing`/`BoundKeysMissing` refusal.
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
        } else if self.declassified_folder_id_now(target).await.is_some() {
            // Media is a producer for a declassified folder exactly as the
            // sync engine is (`SyncEngine::with_public_audience`): its content,
            // names and paths rest in the clear, because they are URLs the
            // web serve reads. Unclassifiable folders never land here — the
            // fail-closed direction `judge_declassification` states.
            UploadCustody::Public
        } else {
            UploadCustody::OwnerOnly
        };

        // 4. `process_media` first: the metadata strip (what rests is the
        //    stripped body, whatever the custody) and the thumbnail.
        let processed = fauna_media::process::process_media(raw_bytes);

        // 5. Seal through the engine's own whole-file pipeline. Deterministic,
        //    so a retry re-puts the same store keys and the owner-only arm
        //    dedups per owner exactly as the engine's uploads do.
        let seal_root = match custody {
            UploadCustody::ContentKeyed { root, version } => Some((root, Some(version))),
            UploadCustody::OwnerOnly => Some((owner.convergent_chunk_root(), None)),
            UploadCustody::Public => None,
        };
        let sealed = fauna_core::blob_seal::seal_blob(
            &processed.stripped_bytes,
            seal_root.map(|(k, v)| (k, v.map(fauna_core::blob_seal::SealStamp::Version))),
        )
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

        // 7. The thumbnail, after the file: it rests as its file rests — the
        //    same pipeline under the same root and stamp, so every reader of
        //    the file opens its tile (`fetch_thumbnail`, the same walk). Its
        //    failure must NOT orphan the just-stored file: log it and record
        //    the member without one (the item renders the placeholder).
        let thumbnail_hash = match processed.thumbnail_bytes {
            Some(thumbnail_bytes) => {
                match seal_and_post_thumbnail(uploader, &thumbnail_bytes, seal_root).await {
                    Ok(hash) => Some(hash),
                    Err(e) => {
                        tracing::warn!(
                            target: "fauna_media",
                            "thumbnail upload failed, recording member without it: {e:?}"
                        );
                        None
                    }
                }
            }
            None => None,
        };

        // 8. Seal the path under the SAME root the chunks sealed under, so the
        //    audience that can open the bytes can render the name and nobody
        //    weaker (`file-sync.md` § Sealed names & paths). A public folder's
        //    names are URLs and seal no label — the engine's public arm
        //    (`label_seal_root` → `None`) agrees, and the serve reads the
        //    plaintext `path`. A seal failure must not orphan the just-stored
        //    bytes: record plaintext-only and let the backfill pass pick it up.
        let path_sealed = match custody {
            UploadCustody::Public => None,
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
        if let Some((channel, home)) = foreign {
            return self
                .nest_api
                .record_foreign(
                    channel,
                    &home.nest_url,
                    folder,
                    device_id,
                    path,
                    Some(hex::encode(sealed.manifest_hash.digest())),
                    sealed.manifest.total_size as i64,
                    "create",
                    sealed
                        .content_stamp
                        .and_then(fauna_core::blob_seal::SealStamp::version),
                    thumbnail_hash,
                    path_sealed,
                )
                .await;
        }
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
                sealed
                    .content_stamp
                    .and_then(fauna_core::blob_seal::SealStamp::version),
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
        self.report_error(LocalizedText::key(NO_SET_ERROR_KEY));
    }

    /// Put an app-glue page error into the snapshot — a guard the shared
    /// gestures cannot see (an Upload pressed with no file chosen, a picked
    /// file the app could not read). It must live HERE, not be painted beside
    /// the snapshot: a shell repaints `error-message` from
    /// [`MediaPageSnapshot::error`] on every wake, so an error written straight
    /// to the label is wiped by the next unrelated repaint (a thumbnail
    /// landing, a refresh) before anyone reads it. HELD, too: a read that
    /// succeeds does not clear it (the page re-reads on its own, a sync push
    /// a beat after the press included); the next gesture does
    /// (`State::held_error`).
    pub fn report_error(&self, err: LocalizedText) {
        tracing::warn!(target: "fauna_media", "{}", err.log_line());
        {
            let mut s = self.state.lock().unwrap();
            s.held_error = Some(err.clone());
            s.error = Some(err);
        }
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
/// root (the thumbnail's too) and the label root (`MediaMachine::do_upload`).
#[derive(Clone, Copy)]
enum UploadCustody {
    /// A WebDAV-served or shared set: its current content-key generation.
    ContentKeyed { root: [u8; 32], version: u64 },
    /// An owner-only set: the owner's convergent chunk root, unstamped.
    OwnerOnly,
    /// A `public`-audience folder: plaintext, no root.
    Public,
}
