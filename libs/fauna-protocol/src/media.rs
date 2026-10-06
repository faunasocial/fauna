//! Cross-set media-listing WS-RPC payload types — `fauna.media.list`.
//!
//! The Media page is the **media-optimized view over the user's folders**
//! (`docs/goal/ui/media.md`): folders are the substrate, Media browses them.
//! Its default **all-media** view aggregates the media items across **every file
//! set the caller may read** (their own sets + any group-bound shared set they're
//! a roster member of). No per-set surface (`fauna.sync.files`) or set-list
//! surface (`fauna.folders.list` / `fauna.sync.backup_status`) returns that
//! union, so `fauna.media.list` is the net-new aggregating RPC `media.md`
//! § State & data shape flagged (spec O-4, settled here at nest implementation).
//!
//! Wire-shape decisions (O-4):
//! - **Item field set** `{folder, path, size_bytes, updated_at, thumbnail_hash,
//!   source_online}`. `folder` is the set **name** (what `media-folder-filter`
//!   scopes by); `size_bytes` / `updated_at` mirror `fauna.sync.files`'
//!   `SyncFile` (uniform across the namespace). `source_online` realizes the
//!   doc's `source_status` dot as the boolean `SyncStatusReply.source_online`
//!   already uses (the folder's content reachability, owned by `file-sync.md`
//!   § Content reachability — *not* a file's sync-state badge). `thumbnail_hash` is the hex `?thumb=1` routing pointer
//!   when the uploader recorded one (`UploadSidecar.thumbnail_hash`); it rides
//!   the existing thumbnail wire, no new surface.
//! - **Pagination** is **keyset** over the stable `(folder, path)` order: an
//!   opaque forward `cursor` + `next_cursor`, plus a `limit` cap. Keyset (not
//!   offset) survives concurrent inserts/deletes between pages and scales to a
//!   photo-library-sized aggregate; the `since`-cursor precedent is
//!   `fauna.sync.changes.list`. The client pulls pages into the shared
//!   `media_snapshot()` and re-sorts/filters by the active `media-sort-select` /
//!   `media-folder-filter` in shared Rust (`media.md` § Where logic lives).
//! - No floats anywhere; hashes ride as hex `String`. Kind registry:
//!   `kind.rs::register_media_kinds`.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use crate::Value;

/// Server default cap on items per `fauna.media.list` page when the request
/// `limit` is `0`.
pub const MEDIA_LIST_DEFAULT_LIMIT: u32 = 1000;

/// Hard ceiling on items per page regardless of the requested `limit` — bounds a
/// single reply's size for a photo-library-scale aggregate.
pub const MEDIA_LIST_MAX_LIMIT: u32 = 5000;

/// The hash-ordered pagination: `(folder_id, path_hash)` — the order that
/// survives the plaintext scrub (`docs/goal/ui/media.md` O-4, sealed-names
/// amendment), and the only one a nest serves. (Version `1`, the original
/// plaintext `(folder, path)` order, left the wire with the compat-remnant
/// sweep, `version-compatibility.md` § Dimension 2; the number stays spent.)
pub const MEDIA_LIST_CURSOR_V2: u32 = 2;

/// `fauna.media.list` request — one keyset page of the caller's cross-set media.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MediaListRequest {
    /// Opaque forward cursor from a prior reply's `next_cursor`; `None` (absent)
    /// starts at the beginning of the requested order.
    ///
    /// **Opaque is literal: the nest seals its cursors** (path-sealing S5e —
    /// their contents include a `path_hash` the reply may withhold from this
    /// reader). Replay one verbatim; never construct, parse or edit one. A cursor
    /// this nest did not mint — a foreign nest's, a tampered one, or one from
    /// before its deployment key rotated — is refused `invalid_cursor`, which
    /// means "restart the listing", not "retry".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Max items to return; `0` ⇒ `MEDIA_LIST_DEFAULT_LIMIT`, and any value is
    /// capped at `MEDIA_LIST_MAX_LIMIT`.
    #[serde(default)]
    pub limit: u32,
    /// Which pagination order to page in — required; today only
    /// [`MEDIA_LIST_CURSOR_V2`] (the `(folder_id, path_hash)` order). The field
    /// is how a future order is added; an order this nest does not serve is
    /// refused `invalid_cursor`, never silently downgraded. Display sorting is
    /// client-side over pulled pages, so the order is only the pagination key
    /// (`media.md` O-4).
    ///
    /// A cursor must be presented under the same version that produced it;
    /// mixing them is `invalid_cursor`, never a silently mis-paged listing.
    pub cursor_version: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One media item in the cross-set all-media view (`media-item` component).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MediaItem {
    /// Name of the readable folder this item belongs to (the
    /// `media-folder-filter` key).
    pub folder: String,
    /// Folder-relative path, forward-slash normalized.
    pub path: String,
    /// Stored ciphertext size in bytes (mirrors `SyncFile.size_bytes`).
    pub size_bytes: i64,
    /// Last-change timestamp (unix seconds) — the file's mtime proxy.
    pub updated_at: i64,
    /// Hex thumbnail-blob hash (the `?thumb=1` routing pointer), when the
    /// uploader recorded one. `None` for folder-sourced items until the
    /// uploader↔manifest thumbnail association lands; the field is in the
    /// contract so clients render thumbnails as it populates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_hash: Option<String>,
    /// Whether the backing folder's content is reachable right now (the
    /// `media-source-status` dot; meaning owned by `file-sync.md` § Content
    /// reachability). Distinct from a file's sync-state badge.
    pub source_online: bool,
    /// The item's `path`, sealed — see [`crate::sync::SyncChange::path_sealed`].
    /// Carried so a sealed-first renderer has the label without the plaintext
    /// column.
    ///
    /// **Projected per reader, exactly like [`Self::folder_sealed`]** — see
    /// [`Self::path_hash`] for why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<ByteBuf>,
    /// The item's `path_hash` (`fauna_core::sync::path_hash`) — the stable
    /// equality-only path key that stays on the floor.
    ///
    /// **Load-bearing for the render, not just for ordering:** `path_sealed` is
    /// a *convergent* seal salted by this hash, so a reader with no plaintext
    /// `path` has no salt to derive and cannot open the label without it. It is
    /// therefore the field that makes the sealed-first render survive the
    /// plaintext scrub — the reason it rides the media plane and not only the
    /// snapshot/conflict planes.
    ///
    /// **Both halves are projected per reader (path-sealing S5d), the identical
    /// gate [`Self::folder_hash`] uses.** They are sent to a set's *label
    /// audience* only; a non-audience reader (a nest admin under the Q5
    /// discovery grant) receives neither, because the salt is an unkeyed
    /// digest of the path and would hand back the name offline
    /// (`encryption-at-rest.md` § Carve-outs). `None` therefore means "you are
    /// not this label's audience" — and a reader needs neither.
    ///
    /// ⚠ **Residual, not closed by this projection:** `fauna.media.list`'s v2
    /// pagination cursor still embeds a page boundary item's raw `path_hash`
    /// (`MEDIA_LIST_CURSOR_V2`'s `CursorKey.path_hash` — a nest-side sort/skip
    /// position key, computed from the row unconditionally). A non-audience
    /// reader who pages past the first page can extract that hash from their
    /// own `next_cursor`, which this field's suppression does not prevent.
    /// Flagged for the security review; not fixed here (closing it needs a
    /// persistent nest secret to key the cursor, which `bins/fauna-nest` does
    /// not have — the same gap S5c-1 already declined to solve for `name_hash`
    /// for the identical reason).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hash: Option<ByteBuf>,
    /// The item's `folder` name, sealed — see
    /// [`crate::folders::FolderSummary::name_sealed`]. Opaque to the nest,
    /// which copies it verbatim out of the `folders.name_sealed` column;
    /// rendered client-side by `fauna_core::label_custody::render_set_name`.
    ///
    /// **Projected per reader** — see [`Self::folder_hash`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_sealed: Option<ByteBuf>,
    /// The item's set-name salt (`fauna_core::path_crypto::set_name_hash`) — what
    /// [`Self::folder_sealed`] opens under, exactly as [`Self::path_hash`] is
    /// what `path_sealed` opens under. The two ship as a **pair** or not at all:
    /// a seal whose salt is missing is unrenderable the moment the plaintext
    /// `folder` scrubs (the hole found twice, on this very plane and on
    /// `WebdavFile`).
    ///
    /// **Both halves are projected per reader, and this field is why.** They are
    /// sent to a set's *label audience* — its owner and its roster members, who
    /// hold a key that opens the seal. They are **withheld from a nest admin
    /// reading under the Q5 discovery-metadata grant**, who holds no such key:
    /// the salt is an unkeyed digest of a user-chosen, dictionary-shaped string,
    /// so shipping it to a reader who cannot open the seal would hand them the
    /// name back offline and undo the seal for exactly the reader
    /// *paths-are-content* names as the adversary (`file-sync.md` § Sealed names
    /// & paths; the nest's own split is `folder_authz::FolderReadGrant`).
    /// `None` therefore means "you are not this label's audience", or a set
    /// no keyed writer has stamped a name on yet — a reader needs neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_hash: Option<ByteBuf>,
    // ── The writer-signed statement of the head row this item projects
    // (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    // ruling (2): web's Media reader has no other row source, so the item
    // carries every field the statement covers beyond the ones above). All
    // wire-additive, and projected to the set's LABEL AUDIENCE only — the
    // same gate as `path_hash`, without which the statement cannot be rebuilt
    // anyway. A reader verifies through [`Self::as_change_row`]. ──
    /// The head row's manifest hash (32 bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<ByteBuf>,
    /// The recording sync device id (32 bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<ByteBuf>,
    /// The nest-stamped recorder — the statement's signed actor (32 bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_actor_id: Option<ByteBuf>,
    /// The head row's `change_type` (`create` / `modify`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_type: Option<String>,
    /// The M2 generation the head's chunks were sealed under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_key_version: Option<u64>,
    /// The head row's causal watermark ([`crate::sync::SyncChange::derived_through`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_through: Option<i64>,
    /// The head row's resolution marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_resolution: Option<bool>,
    /// The writer's signature over the head row's `SignedChange` (64 bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<ByteBuf>,
    /// The key [`Self::signature`] verifies under (32 bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_key: Option<ByteBuf>,
    /// **Reader-stamped, never on the wire** (`serde(skip)`: a nest cannot set
    /// it, and it decodes `false`). `true` only once the shared judge verified
    /// the head row as signed under the reading account's **current** identity
    /// — the one fact that licenses the current owner root for this item's
    /// bytes and label (`mls-group-key-material.md` § M2 → *Writer-signed
    /// change records*, ruling (8)(c)). `false` — a row signed as a
    /// predecessor, another writer's row, an exempt row, or an item no judge
    /// has read — withholds that root: fail-closed.
    #[serde(skip)]
    pub signed_as_current: bool,
    /// **Reader-stamped, never on the wire**, beside
    /// [`Self::signed_as_current`]: the identity the shared judge verified the
    /// head row as **signed as** — `None` for an exempt row or one no judge
    /// has read. What the per-signer bound reads (ruling (8)(c)): a row signed
    /// as a retired identity of the reading account is offered only that
    /// identity's root and its predecessors'.
    #[serde(skip)]
    pub signed_as: Option<[u8; 32]>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl MediaItem {
    /// The head change row this item projects, rebuilt for the one reader-side
    /// verifier ([`crate::sync_writer_sig::verify_row`]) — so the Media reader
    /// verifies exactly as every `changes.list` reader does, never through a
    /// second statement construction. `None` when the item carries no
    /// statement (a reader outside the label audience).
    pub fn as_change_row(&self) -> Option<crate::sync::SyncChange> {
        let hex = |b: &ByteBuf| hex::encode(&b[..]);
        Some(crate::sync::SyncChange {
            path_hash: hex(self.path_hash.as_ref()?),
            manifest_hash: Some(hex(self.manifest_hash.as_ref()?)),
            size_bytes: self.size_bytes,
            change_type: self.change_type.clone()?,
            created_at: self.updated_at,
            path: (!self.path.is_empty()).then(|| self.path.clone()),
            device_id: Some(hex(self.device_id.as_ref()?)),
            content_key_version: self.content_key_version,
            thumbnail_hash: self.thumbnail_hash.clone(),
            author_actor_id: Some(hex(self.author_actor_id.as_ref()?)),
            path_sealed: self.path_sealed.clone(),
            derived_through: self.derived_through,
            is_resolution: self.is_resolution,
            signature: self.signature.clone(),
            signer_key: self.signer_key.clone(),
            ..Default::default()
        })
    }
}

/// `fauna.media.list` reply — one keyset page plus the continuation cursor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MediaListReply {
    /// Media items across the caller's readable folders, in the stable order
    /// named by [`Self::cursor_version`].
    pub items: Vec<MediaItem>,
    /// Forward cursor for the next page, or `None` when this is the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// The pagination order this reply applied — the echo of
    /// [`MediaListRequest::cursor_version`].
    pub cursor_version: u32,
    /// The page's `signer_certs` side table — one embed-as-bytes
    /// `DeviceAuthorization` per distinct delegated signer among the page's
    /// items, exactly as [`crate::sync::SyncChangesListReply::signer_certs`].
    /// Wire-additive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signer_certs: Vec<fauna_core::encoding::EmbedAsBytes>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}
