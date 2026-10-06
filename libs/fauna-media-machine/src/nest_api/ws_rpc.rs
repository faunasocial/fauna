//! WS-RPC production impl of the Media page seam, over
//! `fauna_client_media::MediaClient` (the shared `fauna.media.list` typed-call +
//! pager). This is the directive-correct (`no-http-ws-rpc-everywhere`) consumer —
//! no HTTP.
//!
//! Mirrors `fauna_devices_machine::nest_api::ws_rpc`: a generic
//! [`WsRpcMediaNest<R>`] holds the call + error mapping once (priority #2); the
//! per-target concrete trait impls (native `Arc<NestClient>`, wasm `WsRpcClient`)
//! and the `build_media_machine` constructors live in the `cfg`-gated submodules
//! below and just delegate.

use std::sync::Arc;

use fauna_client_media::{MediaClient, MediaFolder, MediaSnapshot};
use fauna_client_sync::SyncClient;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use super::{ListedVersion, MediaApiError, MediaNestApi};
use crate::machine::MediaMachine;
use crate::observer::MediaObserver;
use crate::snapshots::FileVersionSummary;

/// Transcribes the wire `fauna.files.versions.list` row into the crate-local FFI
/// record — byte hashes become hex strings, the wire `extra` map is dropped.
/// Confined to this `rpc-glue`-gated module (mirrors
/// `fauna_devices_machine::nest_api::ws_rpc`, where `fauna-protocol` stays out
/// of the trait/fake/snapshot surface entirely) so `fauna_protocol` never needs
/// to be an always-on dependency of this crate.
impl From<fauna_protocol::files::FileVersionInfo> for FileVersionSummary {
    fn from(v: fauna_protocol::files::FileVersionInfo) -> Self {
        Self {
            version_num: v.version_num,
            manifest_hash: hex::encode(v.manifest_hash.as_ref()),
            size_bytes: v.size_bytes,
            created_at: v.created_at,
            content_key_version: v.content_key_version,
            // Folded ONCE at this transcribe (the  `owner_display`
            // precedent) so no client re-derives the handle-else-id chooser.
            author_display: fauna_core::format::account_display_label(
                v.author_handle.as_deref(),
                &v.author_actor_id,
            ),
            pruned: v.pruned.unwrap_or(false),
            purge_after: v.purge_after,
        }
    }
}

/// Generic WS-RPC seam over any [`RpcRequester`]. Native binds
/// `R = Arc<NestClient>`, wasm `R = WsRpcClient`; the per-target trait impls
/// below delegate to these inherent methods so the logic is written once.
///
/// Holds two shared typed-call clients over the same requester: `media`
/// (`fauna.media.list`, the cross-set read) + `sync` (`fauna.sync.changes.record`,
/// the upload/delete manifest writes) — both lifted, not reimplemented
/// (priority #2). The blob POST is a separate seam
/// ([`MediaBlobUploader`](crate::blob_uploader::MediaBlobUploader)), not here.
pub struct WsRpcMediaNest<R: RpcRequester> {
    media: MediaClient<R>,
    sync: SyncClient<R>,
    /// The raw requester, for `fauna.folders.list` (the control-plane option
    /// list behind `media-folder-filter` / the upload target). Called through
    /// `fauna-protocol`'s typed wire structs rather than `FoldersClient`
    /// **deliberately**: `fauna-client-folders`' `mls` feature depends on
    /// `fauna-media-machine`, so taking that crate as a dependency here would
    /// close a dependency cycle. The call is one request; a sealed set's name
    /// is carried sealed and rendered by the machine, which holds the label
    /// custody (`MediaMachine::render_sealed_folder_names`).
    nest: R,
    /// This seat's own actor id — the trusted owner every folder in the
    /// owner-scoped list is verified against (`encryption-at-rest.md`
    /// § Readable classes → *The declassification is owner-ATTESTED*). `None`
    /// when the builder could not read one: every folder then seals.
    own: Option<fauna_core::identity::ActorId>,
    /// What this seat remembers about each folder's attestations, by folder
    /// id — the replay floor. **Process-lifetime only**: Media holds no
    /// device-local store, which is the replay residual the goal doc names for
    /// it (§ Implementation status today); a persistent memory is a later
    /// wiring, not a different rule.
    memories: std::sync::Mutex<
        std::collections::HashMap<i64, fauna_protocol::folders::AttestationMemory>,
    >,
    /// Who reads, for the READER half — every media item and version this
    /// seam lists is judged first (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*, ruling (3)). Without a nonce source no
    /// signed row verifies, so none is listed.
    reader: fauna_client_sync::row_judge::ReaderSeat,
    /// The account's attested predecessor ids, handed in after construction
    /// ([`MediaNestApi::set_reader_predecessors`]) — the reader judges with
    /// them from the next listing on.
    reader_predecessors: std::sync::Mutex<Vec<[u8; 32]>>,
}

// `MediaNestApi` requires `Debug`, but the client isn't `Debug`; the requester
// carries no renderable state, so a name-only impl satisfies the bound.
impl<R: RpcRequester> std::fmt::Debug for WsRpcMediaNest<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WsRpcMediaNest")
    }
}

impl<R> WsRpcMediaNest<R>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    /// `own` is the connection's actor id — the declassification anchor for
    /// the owner-scoped folder list; `None` seals every folder.
    pub fn new(nest: R, own: Option<fauna_core::identity::ActorId>) -> Self {
        Self {
            media: MediaClient::new(nest.clone()),
            sync: SyncClient::new(nest.clone()),
            nest,
            own,
            memories: std::sync::Mutex::new(std::collections::HashMap::new()),
            reader: fauna_client_sync::row_judge::ReaderSeat {
                own: own.map(|a| a.0),
                ..Default::default()
            },
            reader_predecessors: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The reader seat for one listing: the seam's own, with the predecessor
    /// ids it currently holds (the seat's learned-link memory is shared by
    /// every clone).
    fn seat(&self) -> fauna_client_sync::row_judge::ReaderSeat {
        fauna_client_sync::row_judge::ReaderSeat {
            predecessors: self
                .reader_predecessors
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            ..self.reader.clone()
        }
    }

    fn do_set_reader_predecessors(&self, predecessors: Vec<[u8; 32]>) {
        *self
            .reader_predecessors
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = predecessors;
    }

    /// Verify the rows this seam lists under `nonces` — the same custody the
    /// seat signs its own records through.
    #[must_use]
    pub fn with_row_nonces(mut self, nonces: fauna_client_sync::SetNonceSource) -> Self {
        self.reader.nonces = Some(nonces);
        self
    }

    /// Sign every change record this seam writes (a media upload or delete)
    /// — writer-signed change records (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*). Without it the records go out
    /// unsigned, which a nest enforcing writer signatures refuses.
    #[must_use]
    pub fn with_record_signing(mut self, signing: fauna_client_sync::RecordSigning) -> Self {
        self.sync = self.sync.with_record_signing(signing);
        self
    }

    async fn do_media_snapshot(&self) -> Result<MediaSnapshot, MediaApiError> {
        let (items, _) = self.judged_media().await?;
        Ok(MediaSnapshot {
            items,
            ..Default::default()
        })
    }

    /// Every page of `fauna.media.list`, each item judged through its head
    /// row (`MediaItem::as_change_row`) by the one shared judge; a refused or
    /// held item is absent — never listed, so never thumbnailed, downloaded or
    /// shared.
    async fn judged_media(
        &self,
    ) -> Result<
        (
            Vec<fauna_client_media::media::MediaItem>,
            fauna_client_sync::row_judge::ProjectionTally,
        ),
        MediaApiError,
    > {
        let listing = self.media.list_all().await.map_err(map_err)?;
        let (listing, tally) = fauna_client_sync::row_judge::judge_media_listing(
            &self.nest,
            &self.seat(),
            listing,
            "fauna.media.list",
        )
        .await
        .map_err(map_err)?;
        Ok((listing.items, tally))
    }

    /// `fauna.folders.list`, owner-scoped (`include_shared_with_me` unset —
    /// see the seam's `list_folders` doc for why the member projection is
    /// deliberately not used here). Transcribes to the mode-carrying
    /// [`MediaFolder`]; the browse/upload scope rules live in that type, not
    /// here, so all seven apps apply them identically.
    async fn do_list_folders(&self) -> Result<Vec<MediaFolder>, MediaApiError> {
        let reply: fauna_protocol::folders::FoldersListReply = self
            .nest
            .request(
                fauna_protocol::folders::KIND_FOLDERS_LIST,
                fauna_protocol::folders::FoldersListRequest {
                    include_shared_with_me: None,
                    extra: Default::default(),
                },
            )
            .await
            .map_err(map_err)?;
        // The verdict is asked HERE, at the one seam that holds a whole
        // `FolderSummary`, and carried on the row — the declassification rule
        // stays the one verifier's (`FolderSummary::judge_declassification`:
        // the OWNER's attestation over this row, verified under this seat's own
        // actor id — the trusted owner of every row in an owner-scoped list —
        // above the seat's floor; WebDAV fail-safe included), and `MediaFolder`
        // never re-derives it from an audience string it would have to
        // re-type. A folder this seat remembered that the
        // list no longer carries is burned like any withdrawn verdict.
        let mut memories = self.memories.lock().unwrap_or_else(|p| p.into_inner());
        let listed: std::collections::HashSet<i64> = reply.folders.iter().map(|fs| fs.id).collect();
        memories.retain(|id, memory| {
            if listed.contains(id) {
                return true;
            }
            *memory = memory.observe_sealed();
            *memory != fauna_protocol::folders::AttestationMemory::default()
        });
        Ok(reply
            .folders
            .into_iter()
            .map(|fs| {
                let memory = memories.get(&fs.id).copied().unwrap_or_default();
                let (rests_unsealed, memory) =
                    fs.judge_declassification(&fs.name, self.own.as_ref(), memory);
                memories.insert(fs.id, memory);
                MediaFolder {
                    id: fs.id,
                    rests_unsealed,
                    metadata_only: fs.is_metadata_only(),
                    owner_only: fs.mls_group_id.is_none() && !fs.webdav_enabled,
                    // Rendered by the machine, which holds the label custody.
                    name_sealed: fs.name_sealed.map(|b| b.into_vec()),
                    name_hash: fs.name_hash.map(|b| b.into_vec()),
                    name: fs.name,
                }
            })
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    async fn do_record_member(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        thumbnail_hash: Option<String>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError> {
        // (seal computed by the caller — see the `MediaNestApi` trait doc)
        // `content_key_version` is `None` for an owner-only set (mirrors the
        // engine's create-record for an unbound set, `engine.rs` `handle_*`)
        // and the sealed generation for a content-keyed one — carried verbatim.
        self.record_self_healing(
            folder,
            device_id,
            path,
            Some(manifest_hash),
            size_bytes,
            "create",
            content_key_version,
            thumbnail_hash,
            path_sealed,
        )
        .await
    }

    async fn do_delete_member(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError> {
        // A delete tombstone carries no manifest, no bytes, no content key, no
        // thumbnail (`engine.rs::handle_delete`: `None, 0, "delete", None`).
        // `path_sealed` is machine-minted (S8 D2) and carried verbatim — the
        // trait doc on `delete_member` owns the contract.
        self.record_self_healing(
            folder,
            device_id,
            path,
            None,
            0,
            "delete",
            None,
            None,
            path_sealed,
        )
        .await
    }

    async fn do_file_versions(
        &self,
        folder: &str,
        path: &str,
        include_pruned: bool,
    ) -> Result<Vec<ListedVersion>, MediaApiError> {
        // `MediaItemSummary.path` is already normalized; one shared owner does
        // the hashing (file-sync.md § Path hashing).
        let (versions, _) = self.judged_versions(folder, path, include_pruned).await?;
        Ok(versions
            .into_iter()
            .map(|v| ListedVersion {
                signed_as_current: v.signed_as_current,
                signed_as: v.signed_as,
                summary: FileVersionSummary::from(v),
            })
            .collect())
    }

    /// The version history, each version judged through its row
    /// (`FileVersionInfo::as_change_row`) by the one shared judge; a refused
    /// or held version is absent — never listed, so never restored or shared.
    async fn judged_versions(
        &self,
        folder: &str,
        path: &str,
        include_pruned: bool,
    ) -> Result<
        (
            Vec<fauna_protocol::files::FileVersionInfo>,
            fauna_client_sync::row_judge::ProjectionTally,
        ),
        MediaApiError,
    > {
        let judged = self
            .sync
            .versions_list_judged(
                fauna_core::sync::path_hash(path),
                folder,
                include_pruned,
                &self.seat(),
            )
            .await
            .map_err(map_err)?;
        Ok(fauna_client_sync::row_judge::retain_judged(
            judged,
            "fauna.files.versions.list",
        ))
    }

    async fn do_share_register(
        &self,
        request: fauna_client_share::share::ShareCreateRequest,
    ) -> Result<fauna_client_share::share::ShareCreateReply, MediaApiError> {
        fauna_client_share::ShareClient::new(self.nest.clone())
            .register(request)
            .await
            .map_err(map_err)
    }

    async fn do_share_list(
        &self,
    ) -> Result<Vec<fauna_client_share::share::ShareRecord>, MediaApiError> {
        fauna_client_share::ShareClient::new(self.nest.clone())
            .list()
            .await
            .map_err(map_err)
    }

    async fn do_share_revoke(&self, token_id: &str) -> Result<(), MediaApiError> {
        fauna_client_share::ShareClient::new(self.nest.clone())
            .revoke_link(token_id)
            .await
            .map_err(map_err)
    }

    async fn do_undelete_version(&self, path: &str, version_num: i64) -> Result<(), MediaApiError> {
        let path_hash = fauna_core::sync::path_hash(path);
        self.sync
            .versions_undelete(path_hash, version_num)
            .await
            .map(|_| ())
            .map_err(map_err)
    }

    #[allow(clippy::too_many_arguments)]
    async fn do_restore_member(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError> {
        // Restore = re-point (file-sync.md § Restore): an ordinary `modify`
        // carrying the historical manifest + its sealed-set generation verbatim.
        // The record shape is shared (`SyncClient::restore_version`) so Media and
        // the Windows shell-extension verb can never drift apart. Self-healing like
        // the other Media writes — restore works from a folder-less control-plane
        // client (web) too. `path_sealed` is machine-minted (S8 D2), verbatim.
        //
        // Media has no local copy of the file (it is a nest-side library fetched by
        // hash), so the § Restore "recording device re-points its own local copy"
        // step does not apply here.
        self.sync
            .restore_version(
                folder,
                device_id,
                path,
                manifest_hash,
                size_bytes,
                content_key_version,
                path_sealed,
            )
            .await
            .map(|_| ())
            .map_err(map_err)
    }

    /// `fauna.sync.changes.record`, self-healing a folder-less client's
    /// unregistered sync device. A Media write (upload create / delete tombstone
    /// / restore modify) requires the recording device to be one of the actor's *registered,
    /// write-capable* devices — but a client registers its sync device only when
    /// it maps a sync folder (`apps/fauna-linux/src/sync.rs` `engine_lifecycle`),
    /// so a logged-in user who uploads media without ever mapping a folder is
    /// rejected `fauna.sync.device_unregistered`. On exactly that rejection,
    /// register the device write-capable (the idempotent `fauna.sync.register`)
    /// and retry once.
    ///
    /// This fires **only** for a never-registered device: an already-registered
    /// one (e.g. by location-map, carrying its real label) records on the first
    /// try, so the placeholder label never clobbers a real one — and a
    /// later location-map `register` (`INSERT OR REPLACE`) still sets the
    /// authoritative label. Model: `docs/goal/behavior/file-sync.md`
    /// § Device Registration.
    ///
    /// The mechanism itself now lives one layer down, baked into
    /// `fauna_client_sync::SyncClient::changes_record` itself (self-healing is no
    /// longer an opt-in sibling — the 2026-07-17 engine bug), so Media, the
    /// Windows shell-extension restore verb, and any future writer share **one**
    /// record path (priorities #1/#2). This wrapper only maps the transport error
    /// onto [`MediaApiError`].
    #[allow(clippy::too_many_arguments)]
    async fn record_self_healing(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        manifest_hash: Option<String>,
        size_bytes: i64,
        change_type: &str,
        content_key_version: Option<u64>,
        thumbnail_hash: Option<String>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), MediaApiError> {
        self.sync
            .changes_record(
                folder,
                device_id,
                path,
                manifest_hash,
                size_bytes,
                change_type,
                content_key_version,
                thumbnail_hash,
                path_sealed,
                // Media records fresh one-shot uploads with no catch-up
                // anchor — causality honestly unknown (readers treat the row
                // as anchor-less; the merge licence never fires on media
                // binaries anyway).
                None,
                None,
            )
            .await
            .map(|_| ())
            .map_err(map_err)
    }
}

// The self-heal label + the `fauna.sync.device_unregistered` predicate moved down
// to `fauna_client_sync`, baked into `changes_record` itself, so every writer
// (Media, the Windows shell-extension restore verb, …) shares one definition.

fauna_core::map_rpc_error! {
    /// Map a transport `R::Error` onto [`MediaApiError`], keyed on the WS-RPC
    /// `RpcError.code` suffix (`fauna.media.{invalid_cursor,…}`). A transport fault
    /// (the request never reached a server rejection) is `Transient`. Mirrors
    /// `fauna_devices_machine::nest_api::ws_rpc::map_err`.
    fn map_err(e) -> MediaApiError {
        "not_found" => NotFound,
        "invalid_request" | "malformed" | "invalid_cursor" => BadRequest,
    }
}

// ── Native (`Arc<NestClient>`) ──────────────────────────────────────────────
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use fauna_client::{NestClient, NestPublicChunkFetcher};

    #[async_trait::async_trait]
    impl MediaNestApi for WsRpcMediaNest<Arc<NestClient>> {
        async fn media_snapshot(&self) -> Result<MediaSnapshot, MediaApiError> {
            self.do_media_snapshot().await
        }

        async fn list_folders(&self) -> Result<Vec<MediaFolder>, MediaApiError> {
            self.do_list_folders().await
        }
        async fn record_member(
            &self,
            folder: &str,
            device_id: &str,
            path: &str,
            manifest_hash: String,
            size_bytes: i64,
            content_key_version: Option<u64>,
            thumbnail_hash: Option<String>,
            path_sealed: Option<Vec<u8>>,
        ) -> Result<(), MediaApiError> {
            self.do_record_member(
                folder,
                device_id,
                path,
                manifest_hash,
                size_bytes,
                content_key_version,
                thumbnail_hash,
                path_sealed,
            )
            .await
        }
        async fn delete_member(
            &self,
            folder: &str,
            device_id: &str,
            path: &str,
            path_sealed: Option<Vec<u8>>,
        ) -> Result<(), MediaApiError> {
            self.do_delete_member(folder, device_id, path, path_sealed)
                .await
        }
        async fn file_versions(
            &self,
            folder: &str,
            path: &str,
            include_pruned: bool,
        ) -> Result<Vec<ListedVersion>, MediaApiError> {
            self.do_file_versions(folder, path, include_pruned).await
        }

        fn set_reader_predecessors(&self, predecessors: Vec<[u8; 32]>) {
            self.do_set_reader_predecessors(predecessors);
        }

        async fn undelete_version(
            &self,
            path: &str,
            version_num: i64,
        ) -> Result<(), MediaApiError> {
            self.do_undelete_version(path, version_num).await
        }
        async fn share_register(
            &self,
            request: fauna_client_share::share::ShareCreateRequest,
        ) -> Result<fauna_client_share::share::ShareCreateReply, MediaApiError> {
            self.do_share_register(request).await
        }
        async fn share_list(
            &self,
        ) -> Result<Vec<fauna_client_share::share::ShareRecord>, MediaApiError> {
            self.do_share_list().await
        }
        async fn share_revoke(&self, token_id: &str) -> Result<(), MediaApiError> {
            self.do_share_revoke(token_id).await
        }
        async fn restore_member(
            &self,
            folder: &str,
            device_id: &str,
            path: &str,
            manifest_hash: String,
            size_bytes: i64,
            content_key_version: Option<u64>,
            path_sealed: Option<Vec<u8>>,
        ) -> Result<(), MediaApiError> {
            self.do_restore_member(
                folder,
                device_id,
                path,
                manifest_hash,
                size_bytes,
                content_key_version,
                path_sealed,
            )
            .await
        }
    }

    /// Build a [`MediaMachine`] over `nest`'s authenticated session. The native
    /// entry the linux app + `fauna-ffi` call.
    ///
    /// Injects the native [`NativeBlobUploader`](crate::blob_uploader::NativeBlobUploader)
    /// (the `upload` POST leg) + the native
    /// [`NativeBlobFetcher`](crate::blob_fetcher::NativeBlobFetcher) (the
    /// `fetch_thumbnail` GET-by-hash leg) — both built from `nest`'s `AuthClient`
    /// (pinned http + shared bearer) — so `upload` POSTs + `fetch_thumbnail`
    /// downloads over the bulk-binary `/api/v1/blob` carve-out, and `delete` rides
    /// WS-RPC. The whole content plane works on all four native apps.
    pub fn build_media_machine(
        nest: Arc<NestClient>,
        observer: Arc<dyn MediaObserver>,
    ) -> Arc<MediaMachine> {
        // Owner-only Media (no folder custody resolver) — the default for a
        // client whose Media page has no shared-set download gesture yet.
        build_media_machine_with_folder_keys(nest, observer, None)
    }

    /// [`build_media_machine`] plus the shared-folder content-key resolver
    /// (Phase 0 — the read leg): a client that ingests folder custody passes a
    /// [`crate::folder_keys::FolderKeyResolver`] so a `download_file` of a
    /// **shared** set opens under its content keys, not the owner `BackupKey`. The
    /// resolver is `fauna_client_folders::NestFolderKeyResolver`, built by the
    /// client's session glue (it holds the folders client + config CAS path).
    pub fn build_media_machine_with_folder_keys(
        nest: Arc<NestClient>,
        observer: Arc<dyn MediaObserver>,
        folder_keys: Option<Arc<dyn crate::folder_keys::FolderKeyResolver>>,
    ) -> Arc<MediaMachine> {
        // Build the three blob seams from the shared session before `nest` is
        // moved into the WS-RPC api.
        let uploader: Arc<dyn crate::blob_uploader::MediaBlobUploader> =
            Arc::new(crate::blob_uploader::NativeBlobUploader::new(&nest));
        let fetcher: Arc<dyn crate::blob_fetcher::MediaBlobFetcher> =
            Arc::new(crate::blob_fetcher::NativeBlobFetcher::new(&nest));
        // The full-file download leg: the neutral-home `fauna-client` binding
        // (priority #2 dedup — it used to be a media-local copy,
        // `NativeDownloadFetcher`, byte-for-byte identical to this).
        let download: Arc<dyn fauna_core::file_download::BlobFetcher> =
            Arc::new(NestPublicChunkFetcher::new(&nest));
        // The record signer: the connection's own identity, each record's
        // nonce through the same custody resolver the downloads use. No
        // resolver or no identity → the records go out unsigned.
        let signing = folder_keys
            .clone()
            .zip(nest.auth().keypair())
            .map(|(resolver, kp)| fauna_client_sync::RecordSigning {
                signer: Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(kp)),
                set_nonce: fauna_client_sync::SetNonceSource::Resolver(resolver),
            });
        // The seat's own actor id, the declassification anchor for every
        // (owner-scoped) folder the seam lists; unreadable ⇒ `None` ⇒ sealed.
        let own = fauna_core::identity::ActorId::from_hex(&nest.actor_id_hex()).ok();
        let mut seam = WsRpcMediaNest::new(nest, own);
        if let Some(signing) = signing {
            seam = seam.with_record_signing(signing);
        }
        // The reader half: every listed row verifies under the nonce the
        // same custody resolver answers.
        if let Some(resolver) = folder_keys.clone() {
            seam = seam.with_row_nonces(fauna_client_sync::SetNonceSource::Resolver(resolver));
        }
        let api: Arc<dyn MediaNestApi> = Arc::new(seam);
        let machine = MediaMachine::new(
            observer,
            api,
            Some(uploader),
            Some(fetcher),
            Some(download),
            folder_keys,
        );
        // Cross-nest downloads (a foreign shared set, a followed public folder
        // homed elsewhere): the bytes come straight off the home nest's public
        // routes, integrity by content address, the TLS verified against the
        // record-delivered home identity (`ForeignPublicChunkFetcher` doc).
        machine.set_foreign_fetchers(Arc::new(NativeForeignFetchers));
        machine
    }

    /// Native [`crate::folder_keys::ForeignBlobFetcherFactory`] — builds a
    /// [`fauna_client::ForeignPublicChunkFetcher`] per foreign home, pinned on
    /// the identity the record delivered.
    struct NativeForeignFetchers;

    impl crate::folder_keys::ForeignBlobFetcherFactory for NativeForeignFetchers {
        fn fetcher_for(
            &self,
            base_url: &str,
            home_nest_actor_id: Option<&str>,
        ) -> Arc<dyn fauna_core::file_download::BlobFetcher> {
            Arc::new(fauna_client::ForeignPublicChunkFetcher::for_home(
                base_url,
                home_nest_actor_id,
            ))
        }
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::{build_media_machine, build_media_machine_with_folder_keys};

// ── Wasm (`WsRpcClient`) ────────────────────────────────────────────────────
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use fauna_rpc_wasm::WsRpcClient;

    #[async_trait::async_trait(?Send)]
    impl MediaNestApi for WsRpcMediaNest<WsRpcClient> {
        async fn media_snapshot(&self) -> Result<MediaSnapshot, MediaApiError> {
            self.do_media_snapshot().await
        }

        async fn list_folders(&self) -> Result<Vec<MediaFolder>, MediaApiError> {
            self.do_list_folders().await
        }
        async fn record_member(
            &self,
            folder: &str,
            device_id: &str,
            path: &str,
            manifest_hash: String,
            size_bytes: i64,
            content_key_version: Option<u64>,
            thumbnail_hash: Option<String>,
            path_sealed: Option<Vec<u8>>,
        ) -> Result<(), MediaApiError> {
            self.do_record_member(
                folder,
                device_id,
                path,
                manifest_hash,
                size_bytes,
                content_key_version,
                thumbnail_hash,
                path_sealed,
            )
            .await
        }
        async fn delete_member(
            &self,
            folder: &str,
            device_id: &str,
            path: &str,
            path_sealed: Option<Vec<u8>>,
        ) -> Result<(), MediaApiError> {
            self.do_delete_member(folder, device_id, path, path_sealed)
                .await
        }
        async fn file_versions(
            &self,
            folder: &str,
            path: &str,
            include_pruned: bool,
        ) -> Result<Vec<ListedVersion>, MediaApiError> {
            self.do_file_versions(folder, path, include_pruned).await
        }

        fn set_reader_predecessors(&self, predecessors: Vec<[u8; 32]>) {
            self.do_set_reader_predecessors(predecessors);
        }

        async fn undelete_version(
            &self,
            path: &str,
            version_num: i64,
        ) -> Result<(), MediaApiError> {
            self.do_undelete_version(path, version_num).await
        }
        async fn share_register(
            &self,
            request: fauna_client_share::share::ShareCreateRequest,
        ) -> Result<fauna_client_share::share::ShareCreateReply, MediaApiError> {
            self.do_share_register(request).await
        }
        async fn share_list(
            &self,
        ) -> Result<Vec<fauna_client_share::share::ShareRecord>, MediaApiError> {
            self.do_share_list().await
        }
        async fn share_revoke(&self, token_id: &str) -> Result<(), MediaApiError> {
            self.do_share_revoke(token_id).await
        }
        async fn restore_member(
            &self,
            folder: &str,
            device_id: &str,
            path: &str,
            manifest_hash: String,
            size_bytes: i64,
            content_key_version: Option<u64>,
            path_sealed: Option<Vec<u8>>,
        ) -> Result<(), MediaApiError> {
            self.do_restore_member(
                folder,
                device_id,
                path,
                manifest_hash,
                size_bytes,
                content_key_version,
                path_sealed,
            )
            .await
        }
    }

    /// Build a [`MediaMachine`] over the SPA's browser `WsRpcClient`.
    ///
    /// Injects both wasm blob seams — the [`WasmBlobUploader`](crate::blob_uploader::WasmBlobUploader)
    /// (LEG B — the `upload` POST leg, a `gloo-net` fetch + `web-sys` FormData
    /// multipart carrying the JS-provided bearer) and the
    /// [`WasmBlobFetcher`](crate::blob_fetcher::WasmBlobFetcher) (LEG B's render
    /// twin — the `fetch_thumbnail` GET-by-hash leg over the public by-hash route,
    /// no bearer) — both built from the SPA's session, mirroring native. So
    /// `upload` POSTs + `fetch_thumbnail` downloads over the bulk-binary
    /// `/api/v1/blob` carve-out, and `delete` rides WS-RPC. The whole content plane
    /// now works on web too.
    ///
    /// Owner-only Media (no folder custody resolver) — the same default as the
    /// native `build_media_machine`, for a client whose Media page reads no
    /// shared set. The SPA's own machine (`fauna-wasm-media`) does not take
    /// this door: it passes its resolver to
    /// [`build_media_machine_with_folder_keys`].
    pub fn build_media_machine(
        nest: WsRpcClient,
        observer: Arc<dyn MediaObserver>,
    ) -> Arc<MediaMachine> {
        build_media_machine_with_folder_keys(nest, None, observer, None)
    }

    /// [`build_media_machine`] plus the shared-folder content-key resolver —
    /// the wasm twin of the native `build_media_machine_with_folder_keys`,
    /// the same signature and the same `MediaMachine::new` slot: a client that
    /// ingests folder custody passes a
    /// [`crate::folder_keys::FolderKeyResolver`] so a `download_file` of a
    /// **shared** set opens under its content keys (and its sealed names
    /// render — `folder_keys_for` feeds both), not the owner `BackupKey`. The
    /// resolver is `fauna_client_folders::NestFolderKeyResolver` over the
    /// same `WsRpcClient`, built by the SPA's session glue (`fauna-wasm-media`'s
    /// constructor) exactly as tui/linux/the FFI apps build theirs. Until
    /// 2026-09-25 the wasm builder hardcoded `None` here ("web has no
    /// shared-set download gesture yet"); the granted
    /// `media-item-detail-download-button` is that gesture.
    ///
    /// `identity` signs the change records the machine writes (the browser
    /// connection carries no keypair of its own), each record's nonce resolved
    /// through `folder_keys`; either absent → the records go out unsigned.
    pub fn build_media_machine_with_folder_keys(
        nest: WsRpcClient,
        identity: Option<&fauna_core::identity::ActorKeypair>,
        observer: Arc<dyn MediaObserver>,
        folder_keys: Option<Arc<dyn crate::folder_keys::FolderKeyResolver>>,
    ) -> Arc<MediaMachine> {
        // Build the three blob seams from the shared session before `nest` is
        // moved into the WS-RPC api; `WsRpcClient` is a cheap `Rc` handle (clone).
        let uploader: Arc<dyn crate::blob_uploader::MediaBlobUploader> =
            Arc::new(crate::blob_uploader::WasmBlobUploader::new(nest.clone()));
        let fetcher: Arc<dyn crate::blob_fetcher::MediaBlobFetcher> =
            Arc::new(crate::blob_fetcher::WasmBlobFetcher::new(nest.clone()));
        // The full-file download leg: the neutral-home `fauna-core` binding
        // (priority #2 dedup — it used to be a media-local copy,
        // `WasmDownloadFetcher`, byte-for-byte identical to this).
        let download: Arc<dyn fauna_core::file_download::BlobFetcher> = Arc::new(
            fauna_core::file_download::WasmPublicChunkFetcher::new(nest.nest_url()),
        );
        let signing = folder_keys.clone().zip(identity).map(|(resolver, kp)| {
            fauna_client_sync::RecordSigning {
                signer: Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(kp)),
                set_nonce: fauna_client_sync::SetNonceSource::Resolver(resolver),
            }
        });
        // The seat's own actor id, the declassification anchor for every
        // (owner-scoped) folder the seam lists; unreadable ⇒ `None` ⇒ sealed.
        let own = fauna_core::identity::ActorId::from_hex(nest.actor_id_hex()).ok();
        let mut seam = WsRpcMediaNest::new(nest, own);
        if let Some(signing) = signing {
            seam = seam.with_record_signing(signing);
        }
        // The reader half: every listed row verifies under the nonce the
        // same custody resolver answers.
        if let Some(resolver) = folder_keys.clone() {
            seam = seam.with_row_nonces(fauna_client_sync::SetNonceSource::Resolver(resolver));
        }
        let api: Arc<dyn MediaNestApi> = Arc::new(seam);
        let machine = MediaMachine::new(
            observer,
            api,
            Some(uploader),
            Some(fetcher),
            Some(download),
            folder_keys,
        );
        // Cross-nest shared-set downloads (Phase 2): browsers reach a foreign
        // nest's public byte routes directly — the S6 route-scoped permissive
        // CORS on exactly those two GETs is what makes this fetch legal; the
        // resolver above is what routes a foreign set's `download_file` here.
        machine.set_foreign_fetchers(Arc::new(WasmForeignFetchers));
        machine
    }

    /// Wasm [`crate::folder_keys::ForeignBlobFetcherFactory`] — a
    /// [`fauna_core::file_download::WasmPublicChunkFetcher`] over the foreign
    /// base (same type as the home fetcher; the browser owns TLS, so the
    /// record-delivered home identity cannot be pinned here — a self-signed
    /// home is unreachable from a browser, WebPKI only, exactly as
    /// `security.md` § Transport trust records for web).
    struct WasmForeignFetchers;

    impl crate::folder_keys::ForeignBlobFetcherFactory for WasmForeignFetchers {
        fn fetcher_for(
            &self,
            base_url: &str,
            _home_nest_actor_id: Option<&str>,
        ) -> Arc<dyn fauna_core::file_download::BlobFetcher> {
            Arc::new(fauna_core::file_download::WasmPublicChunkFetcher::new(
                base_url.trim_end_matches('/'),
            ))
        }
    }
}
#[cfg(target_arch = "wasm32")]
pub use wasm::{build_media_machine, build_media_machine_with_folder_keys};

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::ClassifiedError;
    use fauna_protocol::media::{MediaListReply, MediaListRequest};
    use fauna_protocol::sync::{
        SyncChangeRecordReply, SyncChangeRecordRequest, SyncRegisterReply, SyncRegisterRequest,
    };
    use fauna_protocol::{RpcError, Value};
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use std::sync::Mutex;

    /// Stub requester: records each request kind and returns either a fixtured
    /// `fauna.media.list` reply (`err == None`, to exercise the pager) or the
    /// configured rejection (to exercise error mapping).
    #[derive(Clone)]
    struct StubRequester {
        kinds: Arc<Mutex<Vec<&'static str>>>,
        err: Option<RpcError>,
    }

    impl RpcRequester for StubRequester {
        type Error = ClassifiedError;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, ClassifiedError>
        where
            Req: Serialize,
            Reply: DeserializeOwned,
        {
            self.kinds.lock().unwrap().push(kind);
            if let Some(e) = &self.err {
                return Err(ClassifiedError::Rejected(e.clone()));
            }
            // Decode the request so a payload-shape break would surface, then
            // return an empty single-page reply (next_cursor None ⇒ pager stops).
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode");
            let _req: MediaListRequest = fauna_protocol::decode_strict(&bytes).expect("decode");
            let reply = fauna_protocol::encode_canonical(&MediaListReply {
                items: vec![],
                next_cursor: None,
                ..Default::default()
            })
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    fn seam(
        err: Option<RpcError>,
    ) -> (WsRpcMediaNest<StubRequester>, Arc<Mutex<Vec<&'static str>>>) {
        let kinds = Arc::new(Mutex::new(Vec::new()));
        let stub = StubRequester {
            kinds: Arc::clone(&kinds),
            err,
        };
        (WsRpcMediaNest::new(stub, None), kinds)
    }

    fn rejection(code: &str, detail: &str) -> RpcError {
        let mut e = RpcError::new(code.to_string(), "error.x".to_string());
        e.details = Some(Box::new(Value::String(detail.to_string())));
        e
    }

    /// A nest that answers `fauna.folders.list` with whatever rows the test
    /// put in — the seam's one control-plane read, fixtured so the verdict
    /// each row is carried with can be pinned against what the OWNER signed.
    #[derive(Clone)]
    struct FoldersRequester {
        folders: Arc<Mutex<Vec<fauna_protocol::folders::FolderSummary>>>,
    }

    impl RpcRequester for FoldersRequester {
        type Error = ClassifiedError;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, ClassifiedError>
        where
            Req: Serialize,
            Reply: DeserializeOwned,
        {
            assert_eq!(kind, fauna_protocol::folders::KIND_FOLDERS_LIST);
            let reply =
                fauna_protocol::encode_canonical(&fauna_protocol::folders::FoldersListReply {
                    folders: self.folders.lock().unwrap().clone(),
                    ..Default::default()
                })
                .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    fn owner() -> fauna_core::identity::ActorKeypair {
        fauna_core::identity::ActorKeypair::from_secret([7u8; 32])
    }

    /// A row the nest reports `public`, attested (or not) as the test says.
    fn public_row(
        id: i64,
        attest_by: Option<&fauna_core::identity::ActorKeypair>,
        counter: u64,
    ) -> fauna_protocol::folders::FolderSummary {
        let name = format!("set-{id}");
        fauna_protocol::folders::FolderSummary {
            id,
            audience: fauna_protocol::folders::AUDIENCE_PUBLIC.into(),
            audience_attestation: attest_by.map(|kp| {
                fauna_protocol::folders::AudienceAttestation::mint(kp, id, &name, counter, None)
            }),
            name,
            ..Default::default()
        }
    }

    async fn unsealed_ids(seam: &WsRpcMediaNest<FoldersRequester>) -> Vec<i64> {
        seam.do_list_folders()
            .await
            .expect("list")
            .into_iter()
            .filter(|f| f.rests_unsealed)
            .map(|f| f.id)
            .collect()
    }

    /// Media's seam carries the VERIFIER's verdict on every row, never the
    /// nest's claim: a bare `public`, a stranger's
    /// signature and a seam with no own identity all read sealed; only the
    /// owner's genuine attestation arms a row — and, once the owner flips
    /// back, the nest re-serving that attestation never re-arms this process
    /// (the process-lifetime replay floor), while an honest re-flip does.
    #[tokio::test]
    async fn list_folders_carries_the_verifiers_verdict_not_the_nests_claim() {
        let stranger = fauna_core::identity::ActorKeypair::from_secret([9u8; 32]);
        let folders = Arc::new(Mutex::new(vec![
            public_row(1, None, 1_000),
            public_row(2, Some(&stranger), 1_000),
            public_row(3, Some(&owner()), 1_000),
        ]));
        let nest = FoldersRequester {
            folders: Arc::clone(&folders),
        };

        let seam = WsRpcMediaNest::new(nest.clone(), Some(owner().actor_id()));
        assert_eq!(
            unsealed_ids(&seam).await,
            [3],
            "only the owner's attestation arms"
        );

        let anonymous = WsRpcMediaNest::new(nest.clone(), None);
        assert!(
            unsealed_ids(&anonymous).await.is_empty(),
            "a seam with no own identity has no anchor and seals everything"
        );

        // The owner flips set 3 back (the nest keeps serving the stale
        // attestation, inert on a non-public row): sealed, and burned.
        {
            let mut rows = folders.lock().unwrap();
            rows[2].audience = fauna_protocol::folders::AUDIENCE_PRIVATE.into();
        }
        assert!(unsealed_ids(&seam).await.is_empty());

        // Replay: the nest re-serves the pre-flip row verbatim.
        *folders.lock().unwrap() = vec![public_row(3, Some(&owner()), 1_000)];
        assert!(
            unsealed_ids(&seam).await.is_empty(),
            "the withdrawn attestation never re-arms this process"
        );

        // The honest re-flip mints above what the nest last served.
        *folders.lock().unwrap() = vec![public_row(3, Some(&owner()), 5_000)];
        assert_eq!(unsealed_ids(&seam).await, [3]);
    }

    #[tokio::test]
    async fn snapshot_targets_media_list_kind() {
        let (seam, kinds) = seam(None);
        let snap = seam.do_media_snapshot().await.expect("ok");
        assert!(snap.items.is_empty());
        assert_eq!(kinds.lock().unwrap().as_slice(), ["fauna.media.list"]);
    }

    /// Writer-signed change records, ruling (3), at the Media reader: the grid
    /// and the version history list only what the one shared judge admits. An
    /// item — and a version — signed under ANOTHER set's nonce is absent; the
    /// unsigned item is refused and counted.
    #[tokio::test]
    async fn media_and_versions_list_only_rows_the_shared_judge_admits() {
        use fauna_client_media::media::MediaItem;
        use fauna_protocol::ByteBuf;
        use fauna_protocol::files::{FileVersionInfo, FilesVersionsListReply};
        use fauna_protocol::sync_writer_sig::ChangeSigner;

        const NONCE: [u8; 32] = [0x11; 32];
        const OTHER_NONCE: [u8; 32] = [0x22; 32];
        let owner = owner();
        let signer = ChangeSigner::direct(&owner);
        let item = |path: &str, nonce: Option<[u8; 32]>| {
            let mut it = MediaItem {
                folder: "photos".into(),
                path: path.into(),
                size_bytes: 10,
                updated_at: 1_000,
                path_hash: Some(ByteBuf::from(fauna_core::sync::path_hash(path).to_vec())),
                manifest_hash: Some(ByteBuf::from(vec![3; 32])),
                device_id: Some(ByteBuf::from(vec![4; 32])),
                author_actor_id: Some(ByteBuf::from(owner.actor_id().0.to_vec())),
                change_type: Some("create".into()),
                ..Default::default()
            };
            if let Some(nonce) = nonce {
                let mut row = it.as_change_row().expect("statement");
                signer.sign_row(&mut row, nonce).expect("signs");
                it.signature = row.signature;
                it.signer_key = row.signer_key;
            }
            it
        };
        let version = |version_num: i64, nonce: [u8; 32]| {
            let mut v = FileVersionInfo {
                path_hash: ByteBuf::from(fauna_core::sync::path_hash("good.jpg").to_vec()),
                version_num,
                manifest_hash: ByteBuf::from(vec![version_num as u8; 32]),
                size_bytes: 10,
                created_at: 1_000,
                author_actor_id: owner.actor_id().to_hex(),
                device_id: Some(ByteBuf::from(vec![4; 32])),
                change_type: Some("modify".into()),
                ..Default::default()
            };
            let mut row = v.as_change_row().expect("statement");
            signer.sign_row(&mut row, nonce).expect("signs");
            v.signature = row.signature;
            v.signer_key = row.signer_key;
            v
        };
        let nest = Arc::new(
            fauna_client_testkit::RejectingRequester::new()
                .reply(
                    "fauna.media.list",
                    &MediaListReply {
                        items: vec![
                            item("good.jpg", Some(NONCE)),
                            item("foreign.jpg", Some(OTHER_NONCE)),
                            item("unsigned.jpg", None),
                        ],
                        cursor_version: fauna_protocol::media::MEDIA_LIST_CURSOR_V2,
                        ..Default::default()
                    },
                )
                .reply(
                    fauna_protocol::folders::KIND_FOLDERS_LIST,
                    &fauna_protocol::folders::FoldersListReply {
                        folders: vec![fauna_protocol::folders::FolderSummary {
                            name: "photos".into(),
                            role: Some("owner".into()),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                )
                .reply(
                    "fauna.files.versions.list",
                    &FilesVersionsListReply {
                        versions: vec![version(1, NONCE), version(2, OTHER_NONCE)],
                        ..Default::default()
                    },
                ),
        );
        let seam = WsRpcMediaNest::new(Arc::clone(&nest), Some(owner.actor_id())).with_row_nonces(
            fauna_client_sync::SetNonceSource::by_folder(
                [("photos".to_string(), NONCE)].into_iter().collect(),
            ),
        );

        let (items, tally) = seam.judged_media().await.expect("listed");
        let paths: Vec<&str> = items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(
            paths,
            ["good.jpg"],
            "a foreign-nonce and an unsigned row are absent"
        );
        assert_eq!(
            tally,
            fauna_client_sync::row_judge::ProjectionTally {
                verified: 1,
                refused: 2,
                ..Default::default()
            }
        );
        let snap = seam.do_media_snapshot().await.expect("snapshot");
        assert_eq!(snap.items.len(), 1, "the grid renders the judged listing");

        let (versions, tally) = seam
            .judged_versions("photos", "good.jpg", false)
            .await
            .expect("versions");
        assert_eq!(
            versions.iter().map(|v| v.version_num).collect::<Vec<_>>(),
            [1]
        );
        assert_eq!((tally.verified, tally.refused), (1, 1));
        let summaries = seam
            .do_file_versions("photos", "good.jpg", false)
            .await
            .expect("summaries");
        assert_eq!(summaries.len(), 1, "a refused version is never restorable");
    }

    /// `writer-signed-change-records.md` ruling (11)(f), the Media door: a
    /// HISTORY version — a predecessor's row under a retired nonce of the set
    /// — is listed under the identity it was signed as and never as the
    /// current one's, which is what sends the Media restore down the opening
    /// branch (`MediaMachine::restore_version`: not current-vouched and
    /// unstamped → `NeedsReseal` → `reseal_inherited_version` under the
    /// signer's roots), the branch the sync agent's restore shares.
    #[tokio::test]
    async fn a_history_version_is_listed_under_its_signer_for_the_opening_restore() {
        use fauna_protocol::ByteBuf;
        use fauna_protocol::files::{FileVersionInfo, FilesVersionsListReply};
        use fauna_protocol::sync_writer_sig::ChangeSigner;
        const NONCE: [u8; 32] = [0x5a; 32];
        const RETIRED: [u8; 32] = [0x6c; 32];
        let owner = fauna_core::identity::ActorKeypair::from_secret([0x41; 32]);
        let predecessor = fauna_core::identity::ActorKeypair::from_secret([0x42; 32]);
        let mut v = FileVersionInfo {
            path_hash: ByteBuf::from(fauna_core::sync::path_hash("old.jpg").to_vec()),
            version_num: 7,
            manifest_hash: ByteBuf::from(vec![7; 32]),
            size_bytes: 10,
            created_at: 1_000,
            author_actor_id: predecessor.actor_id().to_hex(),
            device_id: Some(ByteBuf::from(vec![4; 32])),
            change_type: Some("modify".into()),
            ..Default::default()
        };
        let mut row = v.as_change_row().expect("statement");
        ChangeSigner::direct(&predecessor)
            .sign_row(&mut row, RETIRED)
            .expect("signs");
        v.signature = row.signature;
        v.signer_key = row.signer_key;
        v.author_actor_id = owner.actor_id().to_hex();
        let nest = Arc::new(
            fauna_client_testkit::RejectingRequester::new()
                .reply(
                    fauna_protocol::folders::KIND_FOLDERS_LIST,
                    &fauna_protocol::folders::FoldersListReply {
                        folders: vec![fauna_protocol::folders::FolderSummary {
                            name: "photos".into(),
                            role: Some("owner".into()),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                )
                .reply(
                    "fauna.files.versions.list",
                    &FilesVersionsListReply {
                        versions: vec![v],
                        ..Default::default()
                    },
                ),
        );
        let seam = WsRpcMediaNest::new(Arc::clone(&nest), Some(owner.actor_id())).with_row_nonces(
            fauna_client_sync::SetNonceSource::ByFolder(Arc::new(
                [(
                    "photos".to_string(),
                    fauna_core::folder_keys::SetNonceLineage {
                        live: Some(NONCE),
                        retired: vec![fauna_core::folder_keys::RetiredSetNonce {
                            nonce: RETIRED,
                            minted_by: None,
                        }],
                        ..Default::default()
                    },
                )]
                .into_iter()
                .collect(),
            )),
        );
        seam.do_set_reader_predecessors(vec![predecessor.actor_id().0]);

        let listed = seam
            .do_file_versions("photos", "old.jpg", false)
            .await
            .expect("versions");
        assert_eq!(listed.len(), 1, "a history row is listed as a version");
        assert!(
            !listed[0].signed_as_current,
            "never as the current identity's"
        );
        assert_eq!(listed[0].signed_as, Some(predecessor.actor_id().0));
        assert_eq!(listed[0].summary.content_key_version, None);
    }

    #[tokio::test]
    async fn rejection_codes_map_to_variants() {
        let bad = seam(Some(rejection("fauna.media.invalid_cursor", "bad cursor")))
            .0
            .do_media_snapshot()
            .await;
        assert!(matches!(bad, Err(MediaApiError::BadRequest { detail }) if detail == "bad cursor"));

        let nf = seam(Some(rejection("fauna.media.not_found", "no set")))
            .0
            .do_media_snapshot()
            .await;
        assert!(matches!(nf, Err(MediaApiError::NotFound { detail }) if detail == "no set"));

        let other = seam(Some(rejection("fauna.media.internal", "boom")))
            .0
            .do_media_snapshot()
            .await;
        assert!(matches!(other, Err(MediaApiError::Transient { .. })));
    }

    #[tokio::test]
    async fn transport_fault_maps_to_transient() {
        // A non-rejection error (no RpcError) maps to Transient.
        let transient = map_err(ClassifiedError::Transport("test transport error".into()));
        assert!(matches!(transient, MediaApiError::Transient { .. }));
    }

    /// Recorded `(kind, canonical-CBOR payload)` requests.
    type Captured = Arc<Mutex<Vec<(&'static str, Vec<u8>)>>>;

    /// Captures each request's `(kind, canonical-CBOR payload)` and always returns
    /// a transport fault — so `Reply` is never constructed; the test decodes the
    /// captured payload to assert the write gestures' wire shape. (Reuses the
    /// canonical encode the real transport applies, so a payload-shape break would
    /// surface here too.)
    #[derive(Clone)]
    struct RecordingRequester {
        captured: Captured,
    }

    impl RpcRequester for RecordingRequester {
        type Error = ClassifiedError;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, ClassifiedError>
        where
            Req: Serialize,
            Reply: DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode");
            self.captured.lock().unwrap().push((kind, bytes.to_vec()));
            Err(ClassifiedError::Transport("test transport error".into()))
        }
    }

    #[tokio::test]
    async fn record_and_delete_target_changes_record() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let seam = WsRpcMediaNest::new(
            RecordingRequester {
                captured: Arc::clone(&captured),
            },
            None,
        );

        let _ = seam
            .do_record_member(
                "photos",
                "dev-1",
                "photos/c.jpg",
                "abc123".into(),
                42,
                None,
                Some("deadbeef".into()),
                None,
            )
            .await;
        let _ = seam
            .do_delete_member("photos", "dev-1", "photos/b.png", Some(vec![7u8; 40]))
            .await;

        let calls = captured.lock().unwrap();
        assert_eq!(
            calls.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            vec!["fauna.sync.changes.record", "fauna.sync.changes.record"]
        );

        // Upload record: create, with the POST's hash + sealed length, no version.
        let rec: SyncChangeRecordRequest =
            fauna_protocol::decode_strict(&calls[0].1).expect("decode record");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &rec, "photos"
        ));
        assert_eq!(rec.device_id, "dev-1");
        assert_eq!(rec.path, "photos/c.jpg");
        assert_eq!(rec.change_type, "create");
        assert_eq!(rec.manifest_hash.as_deref(), Some("abc123"));
        assert_eq!(rec.size_bytes, 42);
        assert_eq!(rec.content_key_version, None);
        // The producer's companion-thumbnail hash rides the record (media.md
        // § State & data shape → MediaItem.thumbnail_hash).
        assert_eq!(rec.thumbnail_hash.as_deref(), Some("deadbeef"));

        // Delete tombstone: no manifest, zero size, no version — and the
        // machine-minted seal rides the wire VERBATIM (S8 D2: the tombstone
        // row is append-only nest-side, so this record is its only seal).
        let del: SyncChangeRecordRequest =
            fauna_protocol::decode_strict(&calls[1].1).expect("decode delete");
        assert_eq!(del.path, "photos/b.png");
        assert_eq!(del.change_type, "delete");
        assert_eq!(del.manifest_hash, None);
        assert_eq!(del.size_bytes, 0);
        assert_eq!(del.content_key_version, None);
        assert_eq!(
            del.path_sealed.as_ref().map(|b| &b[..]),
            Some(&[7u8; 40][..])
        );
    }

    /// Simulates the folder-less-upload gap: the **first**
    /// `fauna.sync.changes.record` is rejected `fauna.sync.device_unregistered`
    /// (the client never mapped a folder, so its sync device isn't registered);
    /// `fauna.sync.register` succeeds, and the **retried** record succeeds — so
    /// the seam's register-and-retry self-heal shows up as the kind sequence.
    #[derive(Clone)]
    struct SelfHealRequester {
        kinds: Arc<Mutex<Vec<&'static str>>>,
        record_calls: Arc<Mutex<usize>>,
    }

    impl RpcRequester for SelfHealRequester {
        type Error = ClassifiedError;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, ClassifiedError>
        where
            Req: Serialize,
            Reply: DeserializeOwned,
        {
            self.kinds.lock().unwrap().push(kind);
            // Round-trip the payload so a wire-shape break still surfaces here.
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode");
            let reply: Vec<u8> = match kind {
                "fauna.sync.changes.record" => {
                    let _req: SyncChangeRecordRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode record req");
                    let n = {
                        let mut c = self.record_calls.lock().unwrap();
                        *c += 1;
                        *c
                    };
                    if n == 1 {
                        // First write: the device isn't registered yet.
                        return Err(ClassifiedError::Rejected(rejection(
                            "fauna.sync.device_unregistered",
                            "device not registered",
                        )));
                    }
                    fauna_protocol::encode_canonical(&SyncChangeRecordReply {
                        seq: 1,
                        extra: Default::default(),
                    })
                    .expect("encode record reply")
                    .to_vec()
                }
                "fauna.sync.register" => {
                    let req: SyncRegisterRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode register req");
                    fauna_protocol::encode_canonical(&SyncRegisterReply {
                        device_id: req.device_id,
                        extra: Default::default(),
                    })
                    .expect("encode register reply")
                    .to_vec()
                }
                other => panic!("unexpected kind {other}"),
            };
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    #[tokio::test]
    async fn record_self_registers_unregistered_device() {
        let kinds = Arc::new(Mutex::new(Vec::new()));
        let seam = WsRpcMediaNest::new(
            SelfHealRequester {
                kinds: Arc::clone(&kinds),
                record_calls: Arc::new(Mutex::new(0)),
            },
            None,
        );

        // A folder-less client's first upload-record is rejected
        // `device_unregistered`; the seam registers the device write-capable and
        // retries, so the gesture ultimately succeeds.
        seam.do_record_member(
            "photos",
            "dev-1",
            "photos/c.jpg",
            "abc123".into(),
            42,
            None,
            None,
            None,
        )
        .await
        .expect("self-heal: register + retry succeeds");

        assert_eq!(
            kinds.lock().unwrap().as_slice(),
            [
                "fauna.sync.changes.record", // rejected device_unregistered
                "fauna.sync.register",       // self-heal: register write-capable
                "fauna.sync.changes.record", // retry succeeds
            ]
        );
    }

    #[tokio::test]
    async fn restore_records_modify_with_historical_generation() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let seam = WsRpcMediaNest::new(
            RecordingRequester {
                captured: Arc::clone(&captured),
            },
            None,
        );

        let _ = seam
            .do_restore_member(
                "photos",
                "dev-1",
                "photos/c.jpg",
                "abc123".into(),
                42,
                Some(5),
                Some(vec![9u8; 40]),
            )
            .await;

        let calls = captured.lock().unwrap();
        assert_eq!(calls[0].0, "fauna.sync.changes.record");
        // Restore = an ordinary modify re-pointing the historical manifest,
        // carrying its sealed-set generation VERBATIM (file-sync.md § Restore) —
        // and the machine-minted seal, verbatim too (S8 D2).
        let rec: SyncChangeRecordRequest =
            fauna_protocol::decode_strict(&calls[0].1).expect("decode restore");
        assert_eq!(rec.change_type, "modify");
        assert_eq!(rec.manifest_hash.as_deref(), Some("abc123"));
        assert_eq!(rec.size_bytes, 42);
        assert_eq!(rec.content_key_version, Some(5));
        assert_eq!(rec.thumbnail_hash, None);
        assert_eq!(
            rec.path_sealed.as_ref().map(|b| &b[..]),
            Some(&[9u8; 40][..])
        );
    }

    #[tokio::test]
    async fn file_versions_derives_path_hash_and_scopes_to_set() {
        use fauna_protocol::files::FilesVersionsListRequest;
        let captured = Arc::new(Mutex::new(Vec::new()));
        let seam = WsRpcMediaNest::new(
            RecordingRequester {
                captured: Arc::clone(&captured),
            },
            None,
        );

        let _ = seam.do_file_versions("photos", "photos/c.jpg", false).await;

        let calls = captured.lock().unwrap();
        assert_eq!(calls[0].0, "fauna.files.versions.list");
        let req: FilesVersionsListRequest =
            fauna_protocol::decode_strict(&calls[0].1).expect("decode list");
        // path_hash = BLAKE3 of the normalized relative path — derived in shared
        // Rust here, matching the nest's record-side hash.
        assert_eq!(
            req.path_hash.as_ref(),
            blake3::hash("photos/c.jpg".as_bytes()).as_bytes()
        );
        // Scoped to the item's set (a bare path_hash can collide across sets).
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "photos"
        ));
    }

    #[tokio::test]
    async fn record_does_not_self_register_on_other_errors() {
        // A non-`device_unregistered` rejection (quota, here) must NOT trigger a
        // register — the self-heal is scoped to the never-registered case, so an
        // already-registered device's real failures surface unchanged.
        let (seam, kinds) = seam(Some(rejection(
            "fauna.sync.storage_quota_exceeded",
            "over quota",
        )));
        let err = seam
            .do_record_member(
                "photos",
                "dev-1",
                "photos/c.jpg",
                "abc123".into(),
                42,
                None,
                None,
                None,
            )
            .await
            .expect_err("quota error propagates");
        assert!(matches!(err, MediaApiError::Transient { .. }));
        // Exactly one record attempt, no register.
        assert_eq!(
            kinds.lock().unwrap().as_slice(),
            ["fauna.sync.changes.record"]
        );
    }
}
