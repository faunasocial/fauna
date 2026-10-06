//! Typed-call wrapper + shared snapshot/sort/filter for the `fauna.media.*`
//! WS-RPC kinds — the cross-set all-media surface powering the Media page
//! (`docs/goal/ui/media.md`).
//!
//! Media is the media-optimized **view** over the user's folders: its default
//! all-media view aggregates the media items across **every folder the caller
//! may read** (their own sets + any group-bound shared set they're a roster
//! member of). The nest `fauna.media.list` RPC returns that union one keyset page
//! at a time (`libs/fauna-protocol/src/media.rs`); this crate is the **single
//! shared-Rust client surface** all 7 apps consume so none hand-rolls the call
//! (priority #1/#2/#4).
//!
//! Two layers, both shared Rust per `media.md` § Where logic lives:
//! - **The typed call** — [`MediaClient`], a thin `RpcRequester` wrapper (one
//!   async method per kind, no state machine, wasm-clean), mirroring
//!   `fauna-client-sync` / `fauna-client-snapshots`. [`MediaClient::list`] is
//!   one page; [`MediaClient::media_snapshot`] drains every keyset page into a
//!   full [`MediaSnapshot`].
//! - **Browse logic over the snapshot** — [`MediaSnapshot`]'s [`view`] /
//!   [`folders`] + [`MediaSortKey`] realize `media.md` § User actions'
//!   "sort/filter logic … shared Rust": the `media-folder-filter` scope and the
//!   `media-sort-select` key are applied here, not per client. The active
//!   selection itself is client-held view state (`media.md` § State & data
//!   shape) — passed in, not stored.
//!
//! [`view`]: MediaSnapshot::view
//! [`folders`]: MediaSnapshot::folders

use fauna_core::folder_keys::FolderRef;
use fauna_protocol::RpcRequester;
use fauna_protocol::media::{
    MEDIA_LIST_CURSOR_V2, MEDIA_LIST_MAX_LIMIT, MediaItem, MediaListReply, MediaListRequest,
};

// Re-export so consumers reach `MediaItem` etc. through this crate (the surface
// they consume) without depending on `fauna-protocol` directly. Mirrors
// `fauna-client-sync`'s `pub use fauna_protocol::sync`.
pub use fauna_protocol::media;

/// Typed `fauna.media.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the wasm SPA
/// passes its `WsRpcClient`. Errors propagate as the transport's `R::Error`.
pub struct MediaClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> MediaClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.media.list` — one keyset page of the caller's cross-set media, in
    /// the stable hash order ([`MEDIA_LIST_CURSOR_V2`], the `(folder_id,
    /// path_hash)` order that outlives the plaintext scrub — the only one a nest
    /// serves). `cursor = None` starts at the
    /// beginning; pass the prior reply's `next_cursor` to continue. `limit = 0` ⇒
    /// the server default (1000), capped at `MEDIA_LIST_MAX_LIMIT` (5000).
    /// Replay-safe pure read. Most callers want
    /// [`media_snapshot`](Self::media_snapshot) instead.
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<MediaListReply, R::Error> {
        self.nest
            .request(
                "fauna.media.list",
                MediaListRequest {
                    cursor,
                    limit,
                    cursor_version: MEDIA_LIST_CURSOR_V2,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// Page through **all** of the caller's cross-set media into a full
    /// [`MediaSnapshot`] — the Media page's default all-media view. Drains every
    /// keyset page (requesting the max page size for the fewest round-trips) until
    /// the nest reports no `next_cursor`. The keyset cursor strictly advances past
    /// the last item's sort key each page, so this always terminates. The
    /// client then re-sorts / filters the snapshot via [`MediaSnapshot::view`].
    ///
    /// Display sorting is client-side over the pulled pages (`media.md` O-4),
    /// so the server's hash order is only the pagination key.
    pub async fn media_snapshot(&self) -> Result<MediaSnapshot, R::Error> {
        Ok(MediaSnapshot {
            items: self.list_all().await?.items,
            ..Default::default()
        })
    }

    /// Every keyset page as one reply: all the items, and every page's
    /// `signer_certs` side table — what a reader verifying the items
    /// (`MediaItem::as_change_row`, writer-signed change records) needs beside
    /// them. `next_cursor` is always `None`.
    pub async fn list_all(&self) -> Result<MediaListReply, R::Error> {
        let mut all = MediaListReply {
            cursor_version: MEDIA_LIST_CURSOR_V2,
            ..Default::default()
        };
        let mut cursor: Option<String> = None;
        loop {
            let reply = self.list(cursor, MEDIA_LIST_MAX_LIMIT).await?;
            all.items.extend(reply.items);
            all.signer_certs.extend(reply.signer_certs);
            match reply.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(all)
    }
}

/// One folder the client knows about from the **control plane**
/// (`fauna.folders.list`), independent of whether it currently holds any media.
///
/// This is what makes an **empty** set reachable: `media.md` § Layout & flow
/// scopes `media-folder-filter` over *folders*, not over sets-that-have-media,
/// and § Layout & flow's Upload bullet targets "the selected folder" — so a set
/// created in Settings → Folders must be offerable before its first upload,
/// which is precisely the chicken-and-egg [`MediaSnapshot::folders`] used to
/// have when it derived its options from items alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaFolder {
    /// The set's stable `FolderSummary.id` — the identity a **rename cannot
    /// move**, and therefore the one a durable reference carries
    /// (`SearchNav::File { folder_id, .. }`).
    ///
    /// The page itself keys everything on [`name`](Self::name), because that is
    /// what the wire's media rows carry and what the filter reads; this field is
    /// what lets a reference minted elsewhere be *joined* to those rows instead
    /// of matched against a spelling that may have changed since.
    pub id: i64,
    /// The set name — the `media-folder-filter` key, same axis as
    /// [`MediaItem::folder`].
    pub name: String,

    /// Whether an upload into this set must rest **unsealed** — the verdict
    /// `fauna_protocol::folders::FolderSummary::judge_declassification` gave at
    /// the control-plane seam (the OWNER's attestation over the row, verified
    /// under this seat's own identity and above its replay floor — never the
    /// nest's bare `audience` claim), *carried* rather than re-derived: that
    /// function is the one verifier every seat asks, WebDAV fail-safe
    /// included, and a reader re-deriving it is exactly what it exists to
    /// prevent.
    ///
    /// Media is a **producer** for these folders exactly as the sync engine is
    /// (`SyncEngine::with_public_audience`): a folder whose owner declassified
    /// it rests unsealed — content, names and paths — so a byte this page
    /// uploads into one must be plaintext, or the folder's own website serves
    /// ciphertext no reader can open (`encryption-at-rest.md` § Readable
    /// classes → *Owner-flipped public-audience folders*, the authority;
    /// `media.md` § Encryption at rest → *Public-audience folders*).
    ///
    /// **Fail-closed:** `false` unless the nest said otherwise, so an
    /// unparseable audience, or a construction path that
    /// never heard of audiences keeps sealing. Over-sealing a public folder is
    /// an availability lag (it serves after the next re-record); under-sealing
    /// a private one is an unrecoverable disclosure — the same asymmetry the
    /// engine's own flag documents.
    pub rests_unsealed: bool,

    /// Whether this set's content stays on the user's devices
    /// (`FolderSummary::is_metadata_only` — residency `metadata_only`), read
    /// from the same list as [`rests_unsealed`](Self::rests_unsealed). The
    /// Media upload door keeps no copy, so it refuses such a set
    /// (`file-sync.md` § Relay serving → *A write door that keeps no body
    /// refuses a metadata-only folder*); the set stays an upload target so the
    /// user can pick it and read why not.
    ///
    /// **Fail-closed in the wire's direction:** `false` unless the nest
    /// explicitly said `metadata_only`; an absent or unrecognised residency
    /// reads as full and uploads as always.
    pub metadata_only: bool,

    /// Whether this set is **owner-only**: bound to no sharing group
    /// (`FolderSummary::mls_group_id` absent) and not served over WebDAV
    /// (`webdav_enabled` false), so its chunks seal under the owner's own
    /// root — the one case a private (fragment-keyed) share link can be made
    /// for (`share-links.md` § The private-file extension → *Eligibility*).
    /// A bound set's chunks seal under the group's content key and a served
    /// set's under the serve generation, neither of which a link may hand out.
    ///
    /// **Fail-closed:** `false` unless the control-plane row said both; the
    /// mint is the second gate (it unseals the manifest under the owner root
    /// and refuses one that root does not open).
    pub owner_only: bool,

    /// The set's name, **sealed** (`FolderSummary::name_sealed`), as the
    /// control-plane row carried it. A set created sealed rests no plaintext
    /// name, so [`name`](Self::name) arrives blank and is rendered from this
    /// through `fauna_core::label_custody::render_set_name` before the row is
    /// offered (`path-sealing.md` § the set-name plane); `None` for a set
    /// listed by its plaintext name.
    pub name_sealed: Option<Vec<u8>>,

    /// The set's `name_hash` (`FolderSummary::name_hash`) — the salt
    /// [`name_sealed`](Self::name_sealed) opens under and the key its custody
    /// resolves by once the plaintext is blank.
    pub name_hash: Option<Vec<u8>>,
}

impl MediaFolder {
    /// Whether this set may appear as a `media-folder-filter` option — Media's
    /// browse scope: every user folder (website-published folders appear in
    /// Media like any other — folders re-model open call #4, ratified
    /// 2026-08-13; the former web-type exclusion is retired with the mode);
    /// reserved `__*` sets are excluded on both planes.
    ///
    /// It is also the **upload-target** rule: every own folder records to the
    /// one `sync_changes` head plane Media reads, so Media uploads into every
    /// folder it browses (`media.md` § Layout & flow — the former sync-type-only
    /// upload rule retired with the mode).
    pub fn is_browsable(&self) -> bool {
        !fauna_core::sync::is_reserved_folder_name(&self.name)
    }
}

/// The full cross-set all-media snapshot — every readable folder's media items
/// paged in from `fauna.media.list`, in the nest's stable `(folder, path)`
/// order (`media.md` § State & data shape) — plus the control-plane set list the
/// filter and upload target are chosen from. The client applies the active
/// `media-folder-filter` / `media-sort-select` view state over it via
/// [`view`](Self::view); that selection is client-held and is **not** stored in
/// the snapshot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaSnapshot {
    /// Cross-set media items in stable `(folder, path)` order.
    pub items: Vec<MediaItem>,
    /// The caller's folders as the **control plane** reports them
    /// (`fauna.folders.list`), in nest order — including sets with no media
    /// yet. Populated by the page machine, which owns the seam; a consumer that
    /// only pages `fauna.media.list` leaves it empty and degrades to the
    /// item-derived options (exactly the pre-fix behavior, never worse).
    pub known_folders: Vec<MediaFolder>,
}

/// One `media-folder-filter` option: the set's label plus its identity, when
/// the control plane reported one ([`MediaSnapshot::folder_options`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFolderOption {
    /// The set name — the filter key, same axis as [`MediaItem::folder`].
    pub name: String,
    /// The set's identity (`FolderRef::Local` over the row's id), or `None`
    /// for a set known only from its items.
    pub folder_ref: Option<FolderRef>,
}

/// The `media-sort-select` key (`media.md` § Layout & flow): sort the browse by
/// item name, size, or last-change date. A single select, uniform across form
/// factors (desktop *may* render it as clickable column headers; same key
/// either way).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaSortKey {
    /// By the displayed file name ([`display_name`]) — the Explorer default.
    #[default]
    Name,
    /// By stored ciphertext size.
    Size,
    /// By last-change timestamp (`updated_at`).
    Date,
}

impl MediaSortKey {
    /// Map a `media-sort-select` UI value (`"name"` / `"size"` / `"date"`, the
    /// ui.yaml option values) to the sort key, so each app doesn't re-derive
    /// the mapping (priority #2). Unknown values ⇒ `None`.
    pub fn from_select_value(value: &str) -> Option<Self> {
        match value {
            "name" => Some(Self::Name),
            "size" => Some(Self::Size),
            "date" => Some(Self::Date),
            _ => None,
        }
    }

    /// The `media-sort-select` UI value (`"name"` / `"size"` / `"date"`) for this
    /// key — the inverse of [`from_select_value`](Self::from_select_value). Lets a
    /// consumer (e.g. a page state machine) surface the selected key back to the
    /// UI as the same string the select emits, without re-deriving the mapping.
    pub fn as_select_value(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Size => "size",
            Self::Date => "date",
        }
    }
}

impl MediaSnapshot {
    /// The Media browse view: scope to `folder` (or the all-media default when
    /// `None` — the `media-folder-filter`), then sort by `sort` (the
    /// `media-sort-select`), ascending or `descending`. Returns owned items so it
    /// crosses the FFI/wasm boundary cleanly. This is the one entry point a client
    /// calls to render the list; `media.md` § User actions runs both the filter
    /// and the sort in shared Rust.
    pub fn view(
        &self,
        folder: Option<&str>,
        sort: MediaSortKey,
        descending: bool,
    ) -> Vec<MediaItem> {
        let mut out: Vec<MediaItem> = self
            .items
            .iter()
            .filter(|it| folder.is_none_or(|f| it.folder == f))
            .cloned()
            .collect();
        sort_items(&mut out, sort);
        if descending {
            out.reverse();
        }
        out
    }

    /// The sets the client offers in `media-folder-filter` (alongside the
    /// all-media default) — the caller's **browsable** sets from the control
    /// plane (Sync + Backup, no Web, no reserved `__*`), unioned with any set
    /// seen only in the items.
    ///
    /// Deriving this from the items alone — which it used to do, "so the filter
    /// only ever lists sets that actually have media" — is what made a
    /// brand-new empty set unreachable: absent from the filter AND skipped by
    /// the upload default, so it could never acquire the media that would have
    /// listed it (`media.md` § Layout & flow → *Where the offerable sets come
    /// from*).
    pub fn folders(&self) -> Vec<String> {
        self.folder_options()
            .into_iter()
            .map(|option| option.name)
            .collect()
    }

    /// [`folders`](Self::folders) with each set's **identity** beside its
    /// label — same entries, same order. A control-plane row carries its
    /// `FolderRef::Local(id)`; a set seen only in the items (a shared-with-me
    /// set the caller's own list does not carry) has none.
    ///
    /// The identity is what a **local-state read** keys on: the per-file sync
    /// badges read the set's `fsid-<ref>.db`, the file the hosting engine
    /// writes, and a name is a label two sets can share
    /// (`on-demand-files.md` § Hosting multiple on-demand folders).
    pub fn folder_options(&self) -> Vec<MediaFolderOption> {
        self.offer(MediaFolder::is_browsable)
    }

    /// The sets an upload may **target** — the `upload_selected` default and the
    /// answer to "which set does this file land in": the browse options' names,
    /// since every own folder is an upload target ([`MediaFolder::is_browsable`]).
    pub fn upload_targets(&self) -> Vec<String> {
        self.offer(MediaFolder::is_browsable)
            .into_iter()
            .map(|option| option.name)
            .collect()
    }

    /// The shared shape of both option lists: the control-plane sets passing
    /// `in_scope`, in nest order, then any set seen only in `items` (a
    /// shared-with-me set the caller's own list does not carry) not already
    /// named. With no control-plane list at all, the item-derived names are the
    /// whole answer — the pre-fix behavior, so an unwired consumer degrades
    /// rather than losing its options.
    fn offer(&self, in_scope: fn(&MediaFolder) -> bool) -> Vec<MediaFolderOption> {
        let mut out: Vec<MediaFolderOption> = self
            .known_folders
            .iter()
            .filter(|fs| in_scope(fs))
            .map(|fs| MediaFolderOption {
                name: fs.name.clone(),
                folder_ref: Some(FolderRef::Local(fs.id)),
            })
            .collect();
        let known_any = !self.known_folders.is_empty();
        for name in self.item_folders() {
            // An item-only set is out of scope by construction when the control
            // plane knows it (it was filtered above); when the plane is silent
            // we cannot classify it, and keeping it is the safe degrade.
            let classified = known_any && self.known_folders.iter().any(|fs| fs.name == name);
            if !classified && !out.iter().any(|option| option.name == name) {
                out.push(MediaFolderOption {
                    name,
                    folder_ref: None,
                });
            }
        }
        out
    }

    /// The distinct folder names actually present in `items`, in the snapshot's
    /// stable `(folder, path)` order (already grouped, so adjacent de-dup).
    fn item_folders(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for it in &self.items {
            if out.last().map(String::as_str) != Some(it.folder.as_str()) {
                out.push(it.folder.clone());
            }
        }
        out
    }

    /// Find the item a **durable identity pair** names — the set's stable
    /// `FolderSummary.id` plus the file's `path_hash` (hex-lowercase) — for a
    /// reference minted somewhere other than this page (`SearchNav::File`;
    /// `ui/search.md` § Implementation status today).
    ///
    /// **An explicit identity→item lookup, not a spelling match.** The page's
    /// items are keyed by *rendered* fields: `MediaItem::folder` is the set's
    /// **name**, which the user may rename at any time, and the displayed name
    /// is derived from `path`. A durable reference therefore cannot be compared
    /// to a row directly — it has to be joined, which is what this does: the id
    /// resolves to the set's current name through the control-plane list, and
    /// the `path_hash` matches the row's own.
    ///
    /// **`path_hash` comes off the wire row rather than being recomputed from
    /// `path`, deliberately** — it is the same field the index's walk derives a
    /// file's identity from, so both halves mint the identical spelling. A row
    /// that carries none (a reader outside the set's label
    /// audience) cannot be addressed by identity at all, and answers
    /// no rather than a guess.
    ///
    /// `None` means the file is gone, renamed (structurally a delete + create),
    /// or in a set this caller cannot see — the same **DROPPED** outcome a
    /// query-time resolve gives, arriving one click later.
    pub fn locate_file(&self, folder_id: i64, path_hash_hex: &str) -> Option<&MediaItem> {
        // Decoded once, and compared as bytes rather than re-encoding each row: a
        // `path_hash` that is not 32-byte hex names no file this build can
        // address, so it answers `None` here instead of never matching later.
        let want = fauna_core::hex32::decode(path_hash_hex).ok()?;
        let set_name = &self
            .known_folders
            .iter()
            .find(|fs| fs.id == folder_id)?
            .name;
        self.items.iter().find(|it| {
            it.folder == *set_name && it.path_hash.as_ref().is_some_and(|h| h.as_ref() == want)
        })
    }

    /// The item at folder-relative `path` of the set whose durable id is
    /// `folder_id` — [`Self::locate_file`]'s sibling for a producer that holds
    /// the plaintext path rather than its hash: the Windows Explorer Share leaf,
    /// whose sync agent resolves an Explorer path to (set id, relative path)
    /// (`docs/goal/architecture/apps/windows.md` § Shell Extension → *The Share
    /// hand-off*). The set is resolved by id first, so two sets sharing a label
    /// can never answer for each other; `None` = deleted, renamed, or in a set
    /// this caller cannot see.
    pub fn locate_path(&self, folder_id: i64, path: &str) -> Option<&MediaItem> {
        let set_name = &self
            .known_folders
            .iter()
            .find(|fs| fs.id == folder_id)?
            .name;
        self.items
            .iter()
            .find(|it| it.folder == *set_name && it.path == path)
    }

    /// Total item count across all readable sets (the empty-state check:
    /// `media.md` "No media yet" shows when this is 0).
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether no readable set has any media (drives the empty state).
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// The displayed file name for a media item (`media-item-name`) — the last
/// forward-slash-separated path segment. Shared so every app renders the same
/// name and the `MediaSortKey::Name` sort agrees with what's shown. Paths are
/// forward-slash normalized by the upload pipeline; a trailing slash or an empty
/// path falls back to the whole path.
pub fn display_name(item: &MediaItem) -> &str {
    match item.path.rsplit_once('/') {
        Some((_, name)) if !name.is_empty() => name,
        _ => item.path.as_str(),
    }
}

/// Sort `items` in place by `sort` ascending. The `(folder, path)` order is the
/// stable tiebreak for every key (the snapshot already arrives in that order), so
/// a `descending` reverse in [`MediaSnapshot::view`] yields a total, deterministic
/// order with no equal-element churn.
fn sort_items(items: &mut [MediaItem], sort: MediaSortKey) {
    match sort {
        MediaSortKey::Name => items.sort_by(|a, b| {
            display_name(a)
                .cmp(display_name(b))
                .then_with(|| a.folder.cmp(&b.folder))
                .then_with(|| a.path.cmp(&b.path))
        }),
        MediaSortKey::Size => items.sort_by(|a, b| {
            a.size_bytes
                .cmp(&b.size_bytes)
                .then_with(|| a.folder.cmp(&b.folder))
                .then_with(|| a.path.cmp(&b.path))
        }),
        MediaSortKey::Date => items.sort_by(|a, b| {
            a.updated_at
                .cmp(&b.updated_at)
                .then_with(|| a.folder.cmp(&b.folder))
                .then_with(|| a.path.cmp(&b.path))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};
    use fauna_protocol::media::MediaListReply;

    fn item(folder: &str, path: &str, size: i64, updated: i64) -> MediaItem {
        MediaItem {
            folder: folder.into(),
            path: path.into(),
            size_bytes: size,
            updated_at: updated,
            source_online: true,
            ..Default::default()
        }
    }

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = MediaClient::new(MockRequester);
    }

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.media.list" => fauna_protocol::encode_canonical(&MediaListReply {
                items: vec![],
                next_cursor: None,
                ..Default::default()
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn list_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = MediaClient::new(rec.clone());
        block_on(client.list(Some("cur".into()), 250)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.media.list");
        let req: MediaListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.cursor.as_deref(), Some("cur"));
        assert_eq!(req.limit, 250);
        assert_eq!(
            req.cursor_version,
            fauna_protocol::media::MEDIA_LIST_CURSOR_V2
        );
    }

    // ── pager: media_snapshot drains every keyset page until next_cursor None ─
    struct PagingRequester {
        // Pages keyed by the incoming cursor (None ⇒ "" key).
        pages: std::collections::HashMap<String, (Vec<MediaItem>, Option<String>)>,
        calls: std::sync::Mutex<u32>,
    }
    impl RpcRequester for PagingRequester {
        type Error = std::convert::Infallible;
        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            *self.calls.lock().unwrap() += 1;
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode");
            let req: MediaListRequest = fauna_protocol::decode_strict(&bytes).expect("decode");
            let key = req.cursor.unwrap_or_default();
            let (items, next) = self
                .pages
                .get(&key)
                .cloned()
                .expect("page exists for cursor");
            let reply = fauna_protocol::encode_canonical(&MediaListReply {
                items,
                next_cursor: next,
                ..Default::default()
            })
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    #[test]
    fn media_snapshot_pages_until_exhausted() {
        let mut pages = std::collections::HashMap::new();
        // page 1 (cursor "") -> 2 items + cursor "p2"
        pages.insert(
            String::new(),
            (
                vec![item("a", "a/1.jpg", 1, 1), item("a", "a/2.jpg", 2, 2)],
                Some("p2".to_string()),
            ),
        );
        // page 2 (cursor "p2") -> 1 item, no next (last page)
        pages.insert("p2".to_string(), (vec![item("b", "b/3.jpg", 3, 3)], None));
        let req = std::sync::Arc::new(PagingRequester {
            pages,
            calls: std::sync::Mutex::new(0),
        });
        let snap = block_on(MediaClient::new(req.clone()).media_snapshot()).expect("infallible");
        assert_eq!(snap.len(), 3);
        assert_eq!(*req.calls.lock().unwrap(), 2, "exactly two pages fetched");
        assert_eq!(snap.items[0].path, "a/1.jpg");
        assert_eq!(snap.items[2].path, "b/3.jpg");
    }

    // ── filter (media-folder-filter) ───────────────────────────────────────
    #[test]
    fn view_filters_to_one_set_or_all() {
        let snap = MediaSnapshot {
            items: vec![
                item("photos", "photos/x.jpg", 10, 100),
                item("docs", "docs/y.pdf", 20, 50),
                item("photos", "photos/z.png", 30, 200),
            ],
            ..Default::default()
        };
        let all = snap.view(None, MediaSortKey::Name, false);
        assert_eq!(all.len(), 3);
        let only = snap.view(Some("photos"), MediaSortKey::Name, false);
        assert_eq!(only.len(), 2);
        assert!(only.iter().all(|i| i.folder == "photos"));
    }

    // ── sort (media-sort-select) ────────────────────────────────────────────
    #[test]
    fn view_sorts_by_name_size_date_and_direction() {
        let snap = MediaSnapshot {
            items: vec![
                item("s", "s/banana.jpg", 30, 100),
                item("s", "s/apple.jpg", 10, 300),
                item("s", "s/cherry.jpg", 20, 200),
            ],
            ..Default::default()
        };
        let by_name: Vec<_> = snap
            .view(None, MediaSortKey::Name, false)
            .iter()
            .map(|i| display_name(i).to_string())
            .collect();
        assert_eq!(by_name, vec!["apple.jpg", "banana.jpg", "cherry.jpg"]);

        let by_size_desc: Vec<_> = snap
            .view(None, MediaSortKey::Size, true)
            .iter()
            .map(|i| i.size_bytes)
            .collect();
        assert_eq!(by_size_desc, vec![30, 20, 10]);

        let by_date: Vec<_> = snap
            .view(None, MediaSortKey::Date, false)
            .iter()
            .map(|i| i.updated_at)
            .collect();
        assert_eq!(by_date, vec![100, 200, 300]);
    }

    // ── locate_file (the SearchNav::File identity→item lookup) ──────────────

    fn hashed(folder: &str, path: &str, path_hash: [u8; 32]) -> MediaItem {
        MediaItem {
            path_hash: Some(fauna_protocol::ByteBuf::from(path_hash.to_vec())),
            ..item(folder, path, 10, 100)
        }
    }

    /// A durable pair finds its item, and the identity is the PAIR: the same
    /// `path_hash` in another set is a different file, and the wrong set id
    /// resolves to nothing at all.
    #[test]
    fn locate_file_matches_the_identity_pair_not_either_half() {
        let snap = MediaSnapshot {
            items: vec![
                hashed("photos", "photos/x.jpg", [0x11; 32]),
                hashed("docs", "docs/x.jpg", [0x11; 32]),
                hashed("docs", "docs/y.pdf", [0x22; 32]),
            ],
            known_folders: vec![known_with_id(7, "photos"), known_with_id(9, "docs")],
        };
        let hex11 = fauna_core::hex32::encode(&[0x11; 32]);

        assert_eq!(
            snap.locate_file(7, &hex11).map(|i| i.path.as_str()),
            Some("photos/x.jpg"),
        );
        assert_eq!(
            snap.locate_file(9, &hex11).map(|i| i.path.as_str()),
            Some("docs/x.jpg"),
            "the same path_hash in another set is a different file"
        );
        assert!(
            snap.locate_file(404, &hex11).is_none(),
            "a set id this caller does not hold resolves to nothing"
        );
        assert!(
            snap.locate_file(7, &fauna_core::hex32::encode(&[0x22; 32]))
                .is_none(),
            "the right set with the wrong file is still a miss"
        );
    }

    /// `locate_path` keys on (set id, path): the same path in another set is a
    /// different file, and a set id this caller does not hold finds nothing —
    /// the Explorer Share hand-off's lookup (`windows.md` § Shell Extension).
    #[test]
    fn locate_path_matches_the_set_id_and_the_path() {
        let snap = MediaSnapshot {
            items: vec![
                item("photos", "a/x.jpg", 10, 100),
                item("docs", "a/x.jpg", 20, 200),
            ],
            known_folders: vec![known_with_id(7, "photos"), known_with_id(9, "docs")],
        };
        assert_eq!(
            snap.locate_path(7, "a/x.jpg").map(|i| i.size_bytes),
            Some(10)
        );
        assert_eq!(
            snap.locate_path(9, "a/x.jpg").map(|i| i.size_bytes),
            Some(20)
        );
        assert!(snap.locate_path(7, "a/y.jpg").is_none());
        assert!(snap.locate_path(404, "a/x.jpg").is_none());
    }

    /// **The whole reason the reference carries an id and not a name.** Rename
    /// the set — the control-plane row and every item row now say
    /// "family-photos" — and a reference minted before the rename still lands.
    /// A name-carrying reference would have gone dead at the rename.
    #[test]
    fn locate_file_survives_a_folder_rename() {
        let before = MediaSnapshot {
            items: vec![hashed("photos", "photos/x.jpg", [0x11; 32])],
            known_folders: vec![known_with_id(7, "photos")],
        };
        let after = MediaSnapshot {
            items: vec![hashed("family-photos", "photos/x.jpg", [0x11; 32])],
            known_folders: vec![known_with_id(7, "family-photos")],
        };
        let hex11 = fauna_core::hex32::encode(&[0x11; 32]);
        assert!(before.locate_file(7, &hex11).is_some());
        assert_eq!(
            after.locate_file(7, &hex11).map(|i| i.path.as_str()),
            Some("photos/x.jpg"),
            "the id join is what makes the reference survive the rename"
        );
    }

    /// A row carrying no `path_hash` — a reader outside the set's label
    /// audience — cannot be addressed by
    /// identity, and answers no rather than falling back to a path guess (the
    /// index's walk skips such a file for the same reason, so there is no hit
    /// to resolve in the first place).
    #[test]
    fn locate_file_will_not_guess_for_a_row_without_a_path_hash() {
        let snap = MediaSnapshot {
            items: vec![item("photos", "photos/x.jpg", 10, 100)],
            known_folders: vec![known_with_id(7, "photos")],
        };
        assert!(
            snap.locate_file(7, &fauna_core::hex32::encode(&[0x11; 32]))
                .is_none()
        );
    }

    /// A malformed identity names no file this build can address — `None` at
    /// the lookup rather than a silent never-match downstream.
    #[test]
    fn locate_file_rejects_a_path_hash_that_is_not_32_byte_hex() {
        let snap = MediaSnapshot {
            items: vec![hashed("photos", "photos/x.jpg", [0x11; 32])],
            known_folders: vec![known_with_id(7, "photos")],
        };
        assert!(snap.locate_file(7, "not-hex").is_none());
        assert!(
            snap.locate_file(7, "1122").is_none(),
            "right hex, wrong width"
        );
    }

    fn known(name: &str) -> MediaFolder {
        // Ids are per-set and stable; the option-list tests below care only
        // about names, so a name-derived id keeps them distinct without noise.
        known_with_id(name.len() as i64, name)
    }

    fn known_with_id(id: i64, name: &str) -> MediaFolder {
        MediaFolder {
            id,
            name: name.into(),
            ..Default::default()
        }
    }

    // ── the control-plane option list (media.md § Layout & flow) ────────────
    //
    // The property the whole fix exists for: a set the client KNOWS about is
    // offerable, whether or not it already holds media. Deriving the options
    // from items alone made a brand-new empty set permanently unreachable as an
    // upload target — you could not put the first file into it, which is the
    // only way it would ever have gained one.
    #[test]
    fn every_known_folder_is_offerable_even_with_no_media_in_it() {
        let snap = MediaSnapshot {
            items: vec![item("photos", "photos/x.jpg", 10, 100)],
            known_folders: vec![known("photos"), known("fresh")],
        };
        assert!(
            snap.folders().contains(&"fresh".to_string()),
            "an empty set must still be a media-folder-filter option: {:?}",
            snap.folders()
        );
        assert!(
            snap.upload_targets().contains(&"fresh".to_string()),
            "an empty set must still be an upload target: {:?}",
            snap.upload_targets()
        );
    }

    #[test]
    fn reserved_sets_are_never_offerable_and_website_sets_browse_and_upload() {
        // Phase 4 (folders re-model, executing ratified open call #4): a
        // website-published folder BROWSES like any other — the former web-type
        // exclusion is retired, and so is the mode itself, so a website
        // folder is an ordinary folder with its toggle on (a flag Media
        // never sees): browsable AND an upload target. Reserved `__*` sets stay
        // excluded on both planes.
        let snap = MediaSnapshot {
            items: vec![],
            known_folders: vec![known("site"), known("__config"), known("photos")],
        };
        assert_eq!(
            snap.folders(),
            vec!["site".to_string(), "photos".to_string()]
        );
        assert_eq!(
            snap.upload_targets(),
            vec!["site".to_string(), "photos".to_string()]
        );
    }

    #[test]
    fn every_own_folder_is_an_upload_target() {
        // A folder has no type (`media.md` § Layout & flow): every own folder
        // records to the one head plane Media reads, so the Photo Library
        // browses AND takes uploads like any other folder.
        let snap = MediaSnapshot {
            items: vec![],
            known_folders: vec![known("photo-library"), known("docs")],
        };
        assert_eq!(
            snap.folders(),
            vec!["photo-library".to_string(), "docs".to_string()]
        );
        assert_eq!(snap.upload_targets(), snap.folders());
    }

    #[test]
    fn a_set_seen_only_in_items_still_browses() {
        // Shared-with-me sets reach the snapshot through `fauna.media.list`
        // (the nest's readable-set scope) without appearing in the caller's
        // own owner-scoped control-plane list. They must not disappear.
        let snap = MediaSnapshot {
            items: vec![item("shared-with-me", "s/1.jpg", 1, 1)],
            known_folders: vec![known("mine")],
        };
        assert_eq!(
            snap.folders(),
            vec!["mine".to_string(), "shared-with-me".to_string()],
            "known sets first in nest order, then item-only sets"
        );
    }

    #[test]
    fn with_no_control_plane_list_the_options_degrade_to_the_item_derived_ones() {
        // An unwired consumer (or a nest read that failed) must behave exactly
        // as before this fix — never worse, never no-upload-at-all.
        let snap = MediaSnapshot {
            items: vec![item("a", "a/1", 1, 1), item("b", "b/1", 1, 1)],
            known_folders: vec![],
        };
        assert_eq!(snap.folders(), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            snap.upload_targets(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn folders_lists_distinct_sets_in_order() {
        let snap = MediaSnapshot {
            items: vec![
                item("a", "a/1", 1, 1),
                item("a", "a/2", 1, 1),
                item("b", "b/1", 1, 1),
            ],
            ..Default::default()
        };
        assert_eq!(snap.folders(), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn display_name_takes_basename() {
        assert_eq!(
            display_name(&item("s", "deep/path/photo.jpg", 0, 0)),
            "photo.jpg"
        );
        assert_eq!(display_name(&item("s", "flat.png", 0, 0)), "flat.png");
        assert_eq!(display_name(&item("s", "trailing/", 0, 0)), "trailing/");
    }

    #[test]
    fn sort_key_from_select_value() {
        assert_eq!(
            MediaSortKey::from_select_value("name"),
            Some(MediaSortKey::Name)
        );
        assert_eq!(
            MediaSortKey::from_select_value("size"),
            Some(MediaSortKey::Size)
        );
        assert_eq!(
            MediaSortKey::from_select_value("date"),
            Some(MediaSortKey::Date)
        );
        assert_eq!(MediaSortKey::from_select_value("bogus"), None);
    }

    #[test]
    fn sort_key_select_value_round_trips() {
        for key in [MediaSortKey::Name, MediaSortKey::Size, MediaSortKey::Date] {
            assert_eq!(
                MediaSortKey::from_select_value(key.as_select_value()),
                Some(key)
            );
        }
    }
}
