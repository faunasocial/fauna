//! Trait abstraction for the Media page's nest-side surface.
//!
//! The page's one cross-set read goes through [`MediaNestApi`]. Production code
//! uses the WS-RPC [`ws_rpc::WsRpcMediaNest`] (over `fauna-client-media`); tests
//! use [`FakeMediaNestApi`] (gated under `#[cfg(any(test, debug_assertions,
//! feature = "test-helpers"))]`). Mirrors `fauna_devices_machine::nest_api`.
//!
//! Transport: the read rides the authenticated WS-RPC connection — the page runs
//! inside an already-logged-in session, so the seam is constructed with the
//! session's connected requester (`Arc<NestClient>` native / `WsRpcClient` wasm)
//! and needs no per-call URL or token. There is no HTTP impl (the
//! `no-http-ws-rpc-everywhere` directive).

pub mod fake;
#[cfg(feature = "rpc-glue")]
pub mod ws_rpc;

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub use fake::{FakeMediaNestApi, MediaNestCall};
// Both builders exist on both targets since 2026-09-25 — the resolver is a
// `dyn FolderKeyResolver` on either side (a `NestFolderKeyResolver` over
// `Arc<NestClient>` natively, over `WsRpcClient` on wasm), and web's
// `media-item-detail-download-button` is the shared-set download gesture the
// wasm twin used to lack.
#[cfg(feature = "rpc-glue")]
pub use ws_rpc::{build_media_machine, build_media_machine_with_folder_keys};

use fauna_client_media::{MediaFolder, MediaSnapshot};
use fauna_client_share::share::{ShareCreateReply, ShareCreateRequest, ShareRecord};

use crate::snapshots::FileVersionSummary;

/// One version as the seam lists it: the renderable row, plus the one fact of
/// its verdict the machine must keep and an app never sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedVersion {
    pub summary: FileVersionSummary,
    /// The shared judge verified this version's row as signed under the
    /// seat's **current** identity (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*, ruling (8)(c)). `false` — signed as a
    /// predecessor, or by another writer — withholds the current owner root
    /// from every open of the version, and makes its restore open the bytes
    /// first rather than re-sign them unopened (ruling (8)(d)).
    pub signed_as_current: bool,
    /// The identity the judge verified the row as signed as (`None` for an
    /// exempt row) — what the per-signer bound narrows a predecessor's
    /// version to (ruling (8)(c)).
    pub signed_as: Option<[u8; 32]>,
}

fauna_core::declare_api_error!(
    @uniffi_flat_error
    /// Failure of a Media page nest interaction (the cross-set read, the
    /// upload/delete writes, and the thumbnail fetch+decrypt). `detail`
    /// carries the nest's error text. Mirrors
    /// `fauna_devices_machine::DevicesApiError`; the WS-RPC impl keys the
    /// variant off the `RpcError.code` suffix.
    ///
    /// `flat_error` so it can cross the FFI boundary as the `E` of
    /// `MediaMachine::fetch_thumbnail`'s `Result` — represented by its
    /// `Display` string at the boundary (the structured variants stay for
    /// in-Rust matching), the same shape
    /// `fauna_client_mail_settings::DispatchError` uses. `MediaApiError` is a
    /// globally-unique ident (UniFFI requires that of every exported error).
    MediaApiError {
        /// Invalid request (bad cursor / malformed payload).
        BadRequest,
        /// A readable set / resource was not found.
        NotFound,
        /// The nest refused a record for want of a sealed path
        /// (`fauna.sync.path_seal_required`): its view of the folder's
        /// audience is private while the write assumed public. Typed so the
        /// page can react to the code, which `detail` (the nest's prose) does
        /// not carry.
        SealRequired,
        /// Transport fault / 5xx / permission — retryable or session-level.
        Transient,
    }
);

/// Map the native HTTP content-API error onto [`MediaApiError`] — the shared
/// taxonomy `blob_fetcher::native::map_api_err` and
/// `blob_uploader::native::map_api_err` both used to hand-roll identically:
/// `404` → `NotFound`, `400` → `BadRequest`, every other status + transport
/// fault → `Transient`. `action` names the operation for the `Transient`
/// message only (e.g. `"blob fetch"`, `"blob upload"`) — the two call sites'
/// only real difference.
#[cfg(all(not(target_arch = "wasm32"), feature = "rpc-glue"))]
pub(crate) fn map_content_api_err(e: fauna_nest_http::ApiError, action: &str) -> MediaApiError {
    match e {
        fauna_nest_http::ApiError::Status { code: 404, message } => {
            MediaApiError::NotFound { detail: message }
        }
        fauna_nest_http::ApiError::Status { code: 400, message } => {
            MediaApiError::BadRequest { detail: message }
        }
        fauna_nest_http::ApiError::Status { code, message } => MediaApiError::Transient {
            detail: format!("{action} failed ({code}): {message}"),
        },
        fauna_nest_http::ApiError::Transport(detail) => MediaApiError::Transient { detail },
        // Session-level, which is exactly what `Transient` documents itself to
        // cover alongside retryable faults. The *blocking* route for this
        // verdict is not a page taxonomy's job — it rides the connection path
        // (the reconnect supervisor stops on it, and `fauna-ffi`'s `stringify`
        // raises `FfiError::NestIdentityChanged`), so this just reports the
        // failure it saw with the nest named in it.
        // The mid-session sign-in and succession refusals are the same shape:
        // their blocking route is the connection path too.
        e @ (fauna_nest_http::ApiError::NestIdentityChanged { .. }
        | fauna_nest_http::ApiError::SignInRefused
        | fauna_nest_http::ApiError::Superseded { .. }
        | fauna_nest_http::ApiError::NestOutdated
        | fauna_nest_http::ApiError::SubprotocolMismatch) => MediaApiError::Transient {
            detail: e.to_string(),
        },
    }
}

/// Map the wasm blob-transport error onto [`MediaApiError`] — the wasm twin of
/// [`map_content_api_err`] above (same 404/400/else taxonomy, same
/// `action`-parameterized `Transient` message), unifying
/// `blob_fetcher::wasm::map_status` and `blob_uploader::wasm::map_blob_http_err`,
/// which independently hand-rolled it: the fetcher only ever sees a bare
/// `(status, message)` pair (it wraps one into [`fauna_rpc_wasm::BlobHttpError::Status`]
/// to call this), while the uploader already receives the whole enum from
/// [`fauna_rpc_wasm::post_multipart_blob`].
#[cfg(all(target_arch = "wasm32", feature = "rpc-glue"))]
pub(crate) fn map_blob_http_err(err: fauna_rpc_wasm::BlobHttpError, action: &str) -> MediaApiError {
    match err {
        fauna_rpc_wasm::BlobHttpError::Status {
            status: 404,
            message,
        } => MediaApiError::NotFound { detail: message },
        fauna_rpc_wasm::BlobHttpError::Status {
            status: 400,
            message,
        } => MediaApiError::BadRequest { detail: message },
        fauna_rpc_wasm::BlobHttpError::Status { status, message } => MediaApiError::Transient {
            detail: format!("{action} failed ({status}): {message}"),
        },
        e @ fauna_rpc_wasm::BlobHttpError::Transport { .. } => MediaApiError::Transient {
            detail: e.to_string(),
        },
    }
}

// `MaybeSendSync` supertrait + dual `async_trait` arm so the one seam serves
// native (`Arc<NestClient>`, `Send + Sync`) and wasm (the single-threaded
// `Rc`-based `WsRpcClient`, `!Send`) — the identical pattern on
// `fauna_devices_machine::DevicesNestApi`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait MediaNestApi: fauna_core::MaybeSendSync + std::fmt::Debug {
    /// `fauna.media.list` (paged) — the full cross-set all-media aggregate the
    /// caller may read. Returns the raw [`MediaSnapshot`]; the machine applies the
    /// active sort / filter view state to derive the rendered page.
    ///
    /// Every item is one the shared judge admitted, stamped with whether its
    /// head row was signed as the seat's current identity
    /// (`MediaItem::signed_as_current`) — the machine keys every open of the
    /// item's bytes and label on it.
    async fn media_snapshot(&self) -> Result<MediaSnapshot, MediaApiError>;

    /// Hand the seam's reader the account's **attested** predecessor ids
    /// (`AccountRegistry::attested_predecessor_actor_ids`), so a row signed
    /// under a retired identity verifies as the account's own (ruling (8)(b),
    /// source (ii)). A seam with no reader ignores it; a reader handed none
    /// proves the link itself by the statement walk.
    fn set_reader_predecessors(&self, _predecessors: Vec<[u8; 32]>) {}

    /// Hand the seam the page's MLS query ([`MediaMachine::set_mls_query`]) —
    /// the host's [`FolderChannelOwners`], the anchor a **member** row's
    /// declassification is judged under: the channel's MLS-recorded owner,
    /// never the row's nest-filled `owner_actor_id` (`encryption-at-rest.md`
    /// § Readable classes → *The declassification is owner-ATTESTED*). A seam
    /// that judges no verdict ignores it; one never handed it seals every
    /// member row.
    ///
    /// [`MediaMachine::set_mls_query`]: crate::MediaMachine::set_mls_query
    /// [`FolderChannelOwners`]: fauna_client_media::member_rows::FolderChannelOwners
    fn set_mls_query(&self, _query: std::sync::Arc<dyn fauna_client_media::member_rows::MlsQuery>) {
    }

    /// `fauna.folders.list` with the member-visible projection — the caller's
    /// own folders and the sets shared with it, as the control plane reports
    /// them, including sets that hold no media yet. This is what makes a
    /// brand-new empty set choosable in Media (`media.md` § Layout & flow — the
    /// filter scopes over folders, and the upload targets "the selected
    /// folder"); deriving the options from the item aggregate alone left a
    /// fresh set permanently unreachable, so its first upload could never
    /// happen. A member row carries what the items do not: the set's row, its
    /// owner (the option label) and the caller's `writer` grant (the upload
    /// target) — `media.md` § Encryption at rest → *The filter and the upload
    /// target address the row*, (c).
    ///
    /// The member rows are only *rostered* nest-side — the nest cannot observe
    /// an MLS join — so the machine runs the client-side join-filter over this
    /// reply (`fauna_protocol::member_rows`, `MediaMachine::set_mls_query`)
    /// before any row is offered; unwired, every member row is dropped, so a
    /// stranger's un-accepted knock never appears in Media's filter. A
    /// member row's declassification verdict is judged under the channel's
    /// MLS-recorded owner ([`Self::set_mls_query`]), never the row's
    /// `owner_actor_id`.
    ///
    /// Mode is carried per row because Media's two option lists differ:
    /// Sync + Backup browse, Sync alone may be an upload target
    /// (`media.md` O-4 § Folder scope).
    async fn list_folders(&self) -> Result<Vec<MediaFolder>, MediaApiError>;

    /// `fauna.sync.changes.record` (`change_type = "create"`) — record a
    /// just-uploaded file as a manifest member of `folder` from the
    /// write-capable `device_id`. For an **owner-only** set `manifest_hash` is
    /// the content hash the blob POST returned, `size_bytes` the sealed blob
    /// length and `content_key_version` is `None` (no M2 content key). For a
    /// **content-keyed** set (served or shared — `media.md` § Encryption at
    /// rest → *Content-keyed sets*) it is the `ChunkManifest` hash the
    /// engine-pipeline seal produced, `size_bytes` the plaintext length the
    /// engine records, and `content_key_version` the generation the chunks
    /// were sealed under — stamped verbatim so every reader selects
    /// `keys_for(version)` (`mls-group-key-material.md` § M2).
    /// `thumbnail_hash` is the content hash (hex) of the companion thumbnail blob
    /// — the separate `POST /api/v1/blob` the `upload` gesture makes when
    /// `process_media` produced a thumbnail — which `fauna.media.list` surfaces as
    /// `MediaItem.thumbnail_hash` (the `?thumb=1` routing pointer); `None` when no
    /// thumbnail was produced. The `upload` gesture calls this after the blob
    /// POST(s) succeed.
    ///
    /// `path_sealed` is the canonical dag-cbor
    /// `fauna_core::path_crypto::SealedLabel` covering `path`, sealed under the
    /// **same** root the bytes sealed under — the owner root
    /// (`BackupKey::convergent_chunk_root()`) or the set's current content-key
    /// generation — so exactly the audience that can open the bytes can render
    /// the name (`docs/goal/behavior/file-sync.md` § Sealed names & paths). The
    /// upload gesture computes it where the key already is
    /// (`MediaMachine::do_upload`), because this seam is deliberately keyless.
    ///
    /// `folder_id` is the target set's row — `SyncChangeRecordRequest`'s
    /// additive `folder_id`, which the nest resolves under the record kind's
    /// writer gate and never by `folder`'s name forms when present
    /// (`set-addressing.md` § The row form); `None` for an item-only set this
    /// page knows by name alone.
    #[allow(clippy::too_many_arguments)]
    async fn record_member(
        &self,
        folder: &str,
        folder_id: Option<i64>,
        device_id: &str,
        path: &str,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        thumbnail_hash: Option<String>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError>;

    /// `fauna.sync.changes.record` (`change_type = "delete"`, `manifest_hash =
    /// None`) — tombstone a member of `folder` from the write-capable
    /// `device_id`. The `delete` gesture calls this.
    ///
    /// `path_sealed` is minted by the machine from its injected owner key +
    /// resolver ([`crate::MediaMachine::set_owner_backup_key`]; S8 D2 closed
    /// the keyless seam here) and carried verbatim — `None` still records the
    /// tombstone plaintext-only, best-effort, an S8 backfill row. A
    /// `sync_changes` row is append-only and its idempotence compare excludes
    /// `path_sealed`, so a tombstone that records unsealed can never be
    /// re-sealed later — sealing at the gesture is the only chance this row
    /// gets.
    ///
    /// `folder_id` is the row the tombstone lands in, sent as the record's
    /// `folder_id` — never a same-named set the name resolves to first
    /// (`set-addressing.md` § The row form); `None` sends the name forms alone.
    async fn delete_member(
        &self,
        folder: &str,
        folder_id: Option<i64>,
        device_id: &str,
        path: &str,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError>;

    /// `fauna.files.versions.list` — the version history of the file at `path`
    /// in `folder`, oldest→newest (file-sync.md § File Versions). The
    /// `path_hash` derivation (BLAKE3 of the normalized forward-slash relative
    /// path) happens inside the seam — shared Rust, never per-app. The
    /// `file-version-history` component reads this.
    /// `include_pruned` — `true` also returns soft-pruned rows (each carrying
    /// `pruned` + `purge_after`), the recovery browse of `file-versions.md`
    /// § Retention (3); `false` = the live-only listing.
    ///
    /// Every version is one the shared judge admitted, carried with whether
    /// it was signed as the seat's current identity ([`ListedVersion`]).
    ///
    /// `folder_id` is the row the list narrows to, sent as the request's
    /// `folder_id` — never a same-named set the name resolves to first
    /// (`set-addressing.md` § The row form); `None` sends the name forms alone.
    async fn file_versions(
        &self,
        folder: &str,
        folder_id: Option<i64>,
        path: &str,
        include_pruned: bool,
    ) -> Result<Vec<ListedVersion>, MediaApiError>;

    /// `fauna.files.versions.undelete` — restore a soft-pruned version of the
    /// file at `path` to the listable population (`file-versions.md`
    /// § Retention (3), the snapshot-undelete twin on the version plane).
    /// Owner-scoped; a version that is not currently soft-pruned answers the
    /// not-found rejection. `version_num` is the row's stable `seq`, as an
    /// `include_pruned` browse lists it.
    async fn undelete_version(&self, path: &str, version_num: i64) -> Result<(), MediaApiError>;

    /// `fauna.sync.changes.record` (`change_type = "modify"`) — restore a
    /// historical version by re-pointing `path` at its `manifest_hash`
    /// (file-sync.md § Restore): metadata-only (no byte re-upload — the chunks
    /// are already stored and GC-pinned), propagates to member devices as an
    /// ordinary remote change, and appends a NEW version (reversible).
    /// `content_key_version` is the historical version's sealed-set generation,
    /// carried **verbatim** so readers select `key_for(version)`. The
    /// `file-version-restore-confirm-button` gesture calls this.
    ///
    /// `path_sealed`: same contract as [`Self::delete_member`] — machine-minted,
    /// best-effort, carried verbatim.
    ///
    /// `folder_id` is the restored version's own row
    /// (`FileVersionSummary::folder_id`), sent as the record's `folder_id` so
    /// the re-point lands in the set the version belongs to — never in a
    /// same-named own set the name resolves to first (`set-addressing.md`
    /// § The row form); `None` sends the name forms alone.
    #[allow(clippy::too_many_arguments)]
    async fn restore_member(
        &self,
        folder: &str,
        folder_id: Option<i64>,
        device_id: &str,
        path: &str,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError>;

    /// `fauna.share.create` — register a minted share link
    /// (`fauna_client_share::mint_link`'s request, carried verbatim; the seam
    /// never mints — `share-links.md` § Flows → Create).
    async fn share_register(
        &self,
        request: ShareCreateRequest,
    ) -> Result<ShareCreateReply, MediaApiError>;

    /// `fauna.share.list` — the caller's registered links, newest first.
    async fn share_list(&self) -> Result<Vec<ShareRecord>, MediaApiError>;

    /// `fauna.share.revoke` — kill one of the caller's links.
    async fn share_revoke(&self, token_id: &str) -> Result<(), MediaApiError>;
}
