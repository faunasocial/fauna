//! `build_archive_import_machine` — the native WS-RPC implementation of
//! [`crate::ArchiveNest`], delegating each seam method to the `fauna-client-*`
//! crates (`archive-import.md` § The wizard and its machine).
//!
//! Native-only by construction (`#[cfg(all(feature = "rpc-glue", not(target_arch
//! = "wasm32")))]` on the `mod` line): everything here rides `Arc<NestClient>`,
//! the HTTP chunk store and a tokio runtime, none of which the web SPA has —
//! the SPA implements the same seam over its own `WsRpcClient`.
//!
//! The three shapes it provides:
//!
//! - [`RpcArchiveNest`] — the fourteen [`ArchiveNest`] methods. Folder writes go
//!   through the sink-agnostic seal (`fauna_sync_engine::seal::seal_blob` under
//!   the owner's `BackupKey` chunk root) → the chunk store (`check_chunks`,
//!   `POST /api/v1/chunks`, `POST /api/v1/manifests`) → one
//!   `fauna.sync.changes.record` row per path, and reads run the shared walk
//!   (`fauna_core::file_download`) back over the same store
//!   (`archive-import.md` § Storage).
//! - [`FolderArchiveSource`] — the byte-range bridge that lets the zip *stay* in
//!   the folder: an [`ArchiveSource`] (a sync, seeking reader — what
//!   `zip::ZipArchive` consumes) over async range reads, with a one-block
//!   read-ahead cache so a central-directory walk does not fetch a chunk per
//!   `read`.
//! - [`FileOpener`] — the platform [`ArchiveOpener`] on native: a path on disk.

use std::collections::HashSet;
use std::io;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use fauna_archive::ArchiveSource;
use fauna_archive::source::FileSource;
use fauna_client::media_upload::{
    PeriodMediaSeal, UploadedBlob, upload_gated_post_blob, upload_period_sealed_media,
    upload_public_post_blob_bytes,
};
use fauna_client::{NestClient, NestClientError, SetupStatusReply, SetupStatusRequest};
use fauna_client_account::AccountClient;
use fauna_client_caldav::bridge_routing::ListCalendarsRequest;
use fauna_client_caldav::{CalDavClient, EventFields, FaunaEventExt, PutEventError, uid_hash};
use fauna_client_config::dav_store_context;
use fauna_client_folders::FoldersClient;
use fauna_client_folders::folders::FolderCreateRequest;
use fauna_client_posts::PostsClient;
use fauna_client_subscriptions::SubscriptionsClient;
use fauna_client_subscriptions::orchestration::{
    AuthorError, SubscriptionsAuthor, TierGateMaterial,
};
use fauna_client_sync::SyncClient as SyncRpc;
use fauna_client_sync::licensed_read::{LicensedManifestReader, SetAddress};
use fauna_client_sync::row_judge::ReaderSeat;
use fauna_client_sync::sync::SyncFile;
use fauna_core::apply_failure::{PermanentApplyFailure, permanent_reason};
use fauna_core::crypto::BackupKey;
use fauna_core::data::{ContentHash, Timestamp};
use fauna_core::file_download::{
    BlobFetcher, FileDownloadKeys, download_file_bytes_by_manifest,
    download_file_range_by_manifest, file_len_by_manifest,
};
use fauna_core::identity::ActorKeypair;
use fauna_protocol::discovery::capability::HIDDEN_TIERS;
use fauna_protocol::{RpcErrorClass, RpcRequester};
use fauna_sync_engine::nest_client::SyncClient as ChunkStore;

use crate::machine::ArchiveImportMachine;
use crate::nest::{
    ArchiveFolderRef, ArchiveNest, ArchiveNestError, ArchiveOpener, ImportedEvent, MediaSeal,
    SharedSource, TierGate, UploadedMedia,
};
use crate::state::{ArchiveMarker, MARKER_PATH};

/// The one refusal a keyless (bearer-only) connection earns: every folder write
/// seals under the owner's `BackupKey`, which is derived from the identity seed
/// this connection does not hold.
const BEARER_ONLY: &str = "bearer-only connection";

/// The read-ahead block a [`FolderArchiveSource`] fetches per cache miss. One
/// MiB is a few chunks' worth: big enough that a zip central-directory walk
/// (many small `read`s) costs one fetch, small enough that seeking to one
/// member of a multi-gigabyte export never reads the file (§ Storage — "reads
/// one zip member at a time, never the gigabytes of zip").
const READ_AHEAD_BLOCK: u64 = 1 << 20;

// ── error mapping ──────────────────────────────────────────────────────────

/// The code an archive-folder read no judged change row licenses is refused
/// with (`backup-restore.md` § Security Properties → Integrity): permanent —
/// re-reading reproduces it — so a `Rejected`, never a `Transport`, and loud
/// wherever a read's error propagates.
pub const ENTRY_UNLICENSED_CODE: &str = "fauna.archive_import.entry_unlicensed";

/// The one transport→seam mapping. A wire rejection keeps its **code** — the
/// run's folder-name loop matches on `fauna.folders.conflict` verbatim
/// (`run.rs::create_named_folder`) — while every transport fault (disconnect,
/// timeout, auth, framing) becomes `Transport`.
fn nest_err<E: RpcErrorClass + core::fmt::Display>(e: E) -> ArchiveNestError {
    match e.as_rpc_error() {
        Some(rpc) => ArchiveNestError::Rejected {
            code: rpc.code.clone(),
            detail: rpc.localized().to_string(),
        },
        None => ArchiveNestError::Transport(e.to_string()),
    }
}

/// An `anyhow` failure from the shared download walk / seal. These carry no
/// wire code (they are chunk-store HTTP, decode, or integrity failures), so
/// they are all `Transport`; `{e:#}` keeps the whole context chain, which is
/// where the walk records *which* chunk failed.
fn walk_err(e: anyhow::Error) -> ArchiveNestError {
    ArchiveNestError::Transport(format!("{e:#}"))
}

/// A licence gate's failure ([`LicensedManifestReader::require_licence`]):
/// the typed `ENTRY_UNLICENSED` refusal becomes [`ENTRY_UNLICENSED_CODE`],
/// and a history the nest withheld stays `Transport` — a re-run retries it.
fn licence_err(e: anyhow::Error) -> ArchiveNestError {
    if permanent_reason(&e) == Some(PermanentApplyFailure::ENTRY_UNLICENSED.reason) {
        ArchiveNestError::Rejected {
            code: ENTRY_UNLICENSED_CODE.to_string(),
            detail: format!("{e:#}"),
        }
    } else {
        walk_err(e)
    }
}

/// A `SubscriptionsAuthor` failure. The two arms that wrap a transport error
/// keep their wire code through [`nest_err`]. Three refusals are **permanent
/// for this account** — the followers key is lost (`NoPeriodKey`), the nest
/// offers the reserved tier it should hide (`HiddenTierNotHonored`), the live
/// followers blob wraps another device's key (`LiveBlobMismatch`) — and no
/// retry of the record changes them, so they carry a `Rejected` code of their
/// own: the run's `classify` then skip-logs the one record with that reason
/// and goes on to the public posts and the events, exactly as
/// `archive-import.md` § Audience mapping promises for a `Friends` post that
/// cannot be gated (a `Transport` would stop the whole run, and every resume
/// would stop at the same record). The rest (mint, custody, self-delegation,
/// retries exhausted) are transient local failures with no code to carry.
fn author_err(e: AuthorError<NestClientError>) -> ArchiveNestError {
    match e {
        AuthorError::Transport(e) => nest_err(e),
        permanent @ (AuthorError::NoPeriodKey(_)
        | AuthorError::HiddenTierNotHonored
        | AuthorError::LiveBlobMismatch { .. }) => ArchiveNestError::Rejected {
            code: match permanent {
                AuthorError::NoPeriodKey(_) => "fauna.subscriptions.no_period_key",
                AuthorError::HiddenTierNotHonored => "fauna.subscriptions.hidden_tier_not_honored",
                _ => "fauna.subscriptions.live_blob_mismatch",
            }
            .to_string(),
            detail: permanent.to_string(),
        },
        other => ArchiveNestError::Transport(other.to_string()),
    }
}

fn put_event_err(e: PutEventError<NestClientError>) -> ArchiveNestError {
    match e {
        PutEventError::Transport(e) => nest_err(e),
        PutEventError::Seal(e) => ArchiveNestError::Transport(format!("seal event: {e}")),
    }
}

fn gate_of(material: TierGateMaterial) -> TierGate {
    TierGate {
        tier: material.tier,
        rank: material.rank,
        period_key: material.period_key,
        period_version: material.period_version,
        key_blob_ref: material.key_blob_ref,
    }
}

/// The nest's hex blob hash → the `MediaItem` input the machine carries.
fn uploaded_media(blob: UploadedBlob) -> Result<UploadedMedia, ArchiveNestError> {
    let blob_hash = fauna_core::hex32::decode(&blob.blob_hash)
        .map_err(|e| ArchiveNestError::Transport(format!("blob hash: {e}")))?;
    Ok(UploadedMedia {
        blob_hash,
        media_type: blob.media_type,
        size_bytes: blob.size,
    })
}

/// A `Timestamp` (micros) as the RFC 3339 UTC string [`EventFields`] wants.
/// Integer-only (the `epoch_secs_to_ical_utc` primitives), so no date crate and
/// the same string on every target.
fn rfc3339_utc(ts: Timestamp) -> String {
    let secs = (ts.0 / 1_000_000) as i64;
    let (days, hh, mm, ss) = fauna_core::caltime::epoch_secs_to_days_and_time(secs);
    let (y, m, d) = fauna_core::caltime::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

// ── the byte-range bridge ──────────────────────────────────────────────────

/// One byte-range read against a folder-resident file. The production
/// implementation walks the manifest ([`download_file_range_by_manifest`]);
/// tests slice a `Vec`.
#[async_trait::async_trait]
pub trait RangeReader: Send + Sync {
    async fn read_range(&self, offset: u64, len: u64) -> Result<Vec<u8>, ArchiveNestError>;
}

/// An [`ArchiveSource`] over a folder-resident zip, read by byte range.
///
/// ⚠ **Requires a multi-thread tokio runtime.** [`ArchiveSource::read_at`] is
/// synchronous (it is what `zip::ZipArchive` calls through `Read + Seek`) while
/// the range fetch is async, so the bridge is
/// `tokio::task::block_in_place(|| handle.block_on(..))` over a [`Handle`]
/// captured at construction. `block_in_place` moves the current worker off the
/// pool for the duration; on a **current-thread** runtime there is no pool and
/// it would panic, so that flavour is refused with an `io::Error` instead. Every
/// native consumer qualifies: tui's `#[tokio::main]`, linux's
/// `Builder::new_multi_thread`, and the uniffi faces' tokio runtime are all
/// multi-thread.
///
/// [`Handle`]: tokio::runtime::Handle
pub struct FolderArchiveSource {
    reader: Arc<dyn RangeReader>,
    len: u64,
    handle: tokio::runtime::Handle,
    /// The single read-ahead slot: `(block start, bytes)`. One slot, not a map:
    /// a zip walk is overwhelmingly sequential, and an unbounded cache over a
    /// multi-gigabyte archive would be the memory bug this whole path exists to
    /// avoid.
    block: Mutex<Option<(u64, Vec<u8>)>>,
}

impl FolderArchiveSource {
    /// Capture the current runtime handle. Called from an async context (the
    /// seam's `open_folder_archive`), so a handle always exists.
    pub fn new(reader: Arc<dyn RangeReader>, len: u64) -> Self {
        Self {
            reader,
            len,
            handle: tokio::runtime::Handle::current(),
            block: Mutex::new(None),
        }
    }

    fn fetch(&self, offset: u64, len: u64) -> io::Result<Vec<u8>> {
        if matches!(
            self.handle.runtime_flavor(),
            tokio::runtime::RuntimeFlavor::CurrentThread
        ) {
            return Err(io::Error::other(
                "reading an archive out of its folder needs a multi-thread tokio runtime",
            ));
        }
        let reader = Arc::clone(&self.reader);
        tokio::task::block_in_place(|| self.handle.block_on(reader.read_range(offset, len)))
            .map_err(io::Error::other)
    }
}

impl ArchiveSource for FolderArchiveSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len().min((self.len - offset) as usize);
        let mut done = 0usize;
        // Block by block: a read inside one block costs at most one fetch, a
        // read straddling two costs at most two, and a re-read of the block
        // still in the slot costs none.
        while done < want {
            let pos = offset + done as u64;
            let start = pos - pos % READ_AHEAD_BLOCK;
            let mut slot = self.block.lock().expect("read-ahead slot");
            if !matches!(slot.as_ref(), Some((cached, _)) if *cached == start) {
                let bytes = self.fetch(start, READ_AHEAD_BLOCK.min(self.len - start))?;
                *slot = Some((start, bytes));
            }
            let (_, data) = slot.as_ref().expect("the slot was just filled");
            let within = (pos - start) as usize;
            if within >= data.len() {
                // A short block: the file is shorter than `len` claimed, or the
                // range read was clamped. Report the short read rather than
                // spinning.
                break;
            }
            let n = (data.len() - within).min(want - done);
            buf[done..done + n].copy_from_slice(&data[within..within + n]);
            done += n;
        }
        Ok(done)
    }
}

/// The production [`RangeReader`]: the shared byte-range walk over one file's
/// manifest. Each opened chunk is verified against the manifest's plaintext
/// hash by the walk itself (§ Storage — a range has no whole-file address).
struct ManifestRangeReader {
    fetcher: ChunkStoreFetcher,
    file: LicensedFile,
    path: String,
}

#[async_trait::async_trait]
impl RangeReader for ManifestRangeReader {
    async fn read_range(&self, offset: u64, len: u64) -> Result<Vec<u8>, ArchiveNestError> {
        download_file_range_by_manifest(
            &self.fetcher,
            &self.file.keys,
            self.file.manifest,
            self.file.content_key_version,
            &self.path,
            offset,
            len,
        )
        .await
        .map_err(walk_err)
    }
}

/// The two content-address GETs the shared walk needs, over the authenticated
/// HTTP chunk store.
struct ChunkStoreFetcher {
    store: Arc<ChunkStore>,
}

#[async_trait::async_trait]
impl BlobFetcher for ChunkStoreFetcher {
    async fn fetch_manifest(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
        self.store.download_manifest(hash).await
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        // Sequential — reqwest pools the connection, and the engine's own
        // pooled/parallel transfer path is private to it. Follow-up: lift that
        // pool behind a public seam and consume it here; nothing shared moves
        // when it happens, because the batch arrives whole exactly so the
        // implementer owns the concurrency choice.
        let mut out = Vec::with_capacity(store_keys.len());
        for key in store_keys {
            let bytes = self
                .store
                .download_chunk(key)
                .await
                .with_context(|| format!("chunk of {relative_path}"))?;
            out.push(bytes);
        }
        Ok(out)
    }
}

// ── the seam ───────────────────────────────────────────────────────────────

/// [`ArchiveNest`] over a live `NestClient`: WS-RPC for the control plane
/// (folders, change records, posts, tiers, calendar) and the authenticated
/// bulk-HTTP plane for chunks, manifests and blobs.
pub struct RpcArchiveNest {
    nest: Arc<NestClient>,
    /// Hex of the sync device id every change record is stamped with.
    device_id_hex: String,
    sync_http: Arc<ChunkStore>,
    /// `<handle>@<domain>` — the VEVENT `ORGANIZER` on every imported event.
    /// Resolved once (two RPCs) and shared by every `put_event` in a run.
    self_email: tokio::sync::OnceCell<String>,
    /// Each archive folder's set nonce — the binding every change record the
    /// run writes into it is signed under, and every read of it is judged
    /// under — resolved once per folder (two RPCs) rather than per file. A
    /// folder whose nonce did not resolve is not cached: the next write or
    /// read asks again, and until then refuses.
    set_nonces: tokio::sync::Mutex<std::collections::HashMap<String, [u8; 32]>>,
    /// The owner's period-key custody (`fauna.state.subscriptions`) the
    /// reserved tiers' gate material is read from and recorded into.
    period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    /// The account's folder-key custody (`fauna.state.folder-keys`): the
    /// archive folder's nonce is minted into it before the nest sees the set,
    /// and every record into the folder is signed under the nonce it holds.
    folder_keys: Arc<dyn fauna_client_folders::FolderKeyStore>,
    /// The account's mail custody — the MSEK an imported event's encrypted
    /// CalDAV write seals under.
    mail: Arc<dyn fauna_client_config::MailStore>,
}

impl RpcArchiveNest {
    /// The connection's identity keypair, or the keyless refusal.
    ///
    /// Derived per call rather than cached: the connection's keypair is not
    /// fixed for the life of this struct, and a stale owner root would fail
    /// *silently* (wrong key ⇒ unopenable chunks, not an error) — the
    /// `RpcMailImportNest::owner_custody` precedent.
    fn keypair(&self) -> Result<&ActorKeypair, ArchiveNestError> {
        self.nest
            .auth()
            .keypair()
            .ok_or_else(|| ArchiveNestError::Unsupported(BEARER_ONLY.to_string()))
    }

    /// A detached copy of the identity, for the client surfaces that take one by
    /// value (the subscriptions author). `ActorKeypair` is deliberately not `Clone` (it
    /// holds zeroizing secret material), so this is the fleet-wide idiom.
    fn identity(&self) -> Result<ActorKeypair, ArchiveNestError> {
        Ok(ActorKeypair::from_secret(*self.keypair()?.secret_bytes()))
    }

    /// `folder`'s set nonce, resolved once per folder from the roster plus
    /// the owner's custody.
    ///
    /// An unresolved nonce is a typed TRANSIENT refusal, never an unsigned
    /// record: the archive folder's nonce is minted into custody before the
    /// nest sees the set ([`ArchiveNest::create_folder`]), so its absence is
    /// a fault (a custody read that failed, a roster that has not caught up)
    /// — and an unsigned record could never be read back, since every read
    /// of the folder is licensed by a judged, writer-signed row
    /// ([`licensed_file`]). The run stops and a resume retries.
    async fn set_nonce(&self, folder: &str) -> Result<[u8; 32], ArchiveNestError> {
        let mut cache = self.set_nonces.lock().await;
        if let Some(nonce) = cache.get(folder) {
            return Ok(*nonce);
        }
        let files = FoldersClient::new(Arc::clone(&self.nest));
        let nonce = fauna_client_folders::record_nonce(&files, &*self.folder_keys, folder)
            .await
            .map_err(|e| {
                ArchiveNestError::Transport(format!(
                    "archive folder's set nonce unresolved, refusing an unsigned record: {e}"
                ))
            })?
            .ok_or_else(|| {
                ArchiveNestError::Transport(
                    "archive folder's set nonce is not in custody, refusing an unsigned record"
                        .to_string(),
                )
            })?;
        cache.insert(folder.to_string(), nonce);
        Ok(nonce)
    }

    /// The record client for `folder`: signed with the connection's identity
    /// under the folder's nonce ([`Self::set_nonce`]) — never unsigned.
    async fn record_client(
        &self,
        folder: &str,
    ) -> Result<SyncRpc<Arc<NestClient>>, ArchiveNestError> {
        let identity = self.identity()?;
        let nonce = self.set_nonce(folder).await?;
        Ok(SyncRpc::new(Arc::clone(&self.nest)).with_record_signing(
            fauna_client_sync::RecordSigning {
                signer: Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
                    &identity,
                )),
                set_nonce: fauna_client_sync::SetNonceSource::Fixed(nonce),
            },
        ))
    }

    /// The seat every read of `folder` is judged from: this identity, under
    /// the folder's nonce ([`Self::set_nonce`]) — the binding the run's own
    /// records were signed under.
    async fn reader_seat(&self, folder: &str) -> Result<ReaderSeat, ArchiveNestError> {
        Ok(ReaderSeat {
            own: Some(self.keypair()?.actor_id().0),
            nonces: Some(fauna_client_sync::SetNonceSource::Fixed(
                self.set_nonce(folder).await?,
            )),
            ..Default::default()
        })
    }

    /// The owner-audience reader for the archive folder's files —
    /// [`FileDownloadKeys::sealed_only`]: every file in the folder is written
    /// through `seal_blob` under the owner's root (§ Storage), so the folder
    /// is never plaintext and a plaintext manifest is refused before any
    /// chunk is fetched.
    fn owner_keys(&self) -> Result<FileDownloadKeys, ArchiveNestError> {
        Ok(
            FileDownloadKeys::owner(BackupKey::derive(self.keypair()?.secret_bytes()))
                .sealed_only(),
        )
    }

    /// The convergent chunk root every file in the folder seals under.
    fn seal_root(&self) -> Result<[u8; 32], ArchiveNestError> {
        Ok(BackupKey::derive(self.keypair()?.secret_bytes()).convergent_chunk_root())
    }

    fn fetcher(&self) -> ChunkStoreFetcher {
        ChunkStoreFetcher {
            store: Arc::clone(&self.sync_http),
        }
    }

    fn author(&self) -> Result<SubscriptionsAuthor<Arc<NestClient>>, ArchiveNestError> {
        Ok(SubscriptionsAuthor::new(
            SubscriptionsClient::new(Arc::clone(&self.nest)),
            self.identity()?,
            Arc::clone(&self.period_keys),
        ))
    }

    /// The licensed head of `path` in `folder` ([`licensed_head`]), or `None`
    /// when no live record names it.
    async fn head_file(
        &self,
        folder: &str,
        path: &str,
    ) -> Result<Option<LicensedFile>, ArchiveNestError> {
        licensed_head(
            &SyncRpc::new(Arc::clone(&self.nest)),
            &self.owner_keys()?,
            folder,
            path,
            async || self.reader_seat(folder).await,
        )
        .await
    }

    /// The whole file at `path`, or `None` when no live record names it. The
    /// inherent twin of the seam method, so the seam's own callers (the marker
    /// scan in `list_archive_folders`) do not recurse through the trait object.
    async fn read_path(
        &self,
        folder: &str,
        path: &str,
    ) -> Result<Option<Vec<u8>>, ArchiveNestError> {
        let Some(file) = self.head_file(folder, path).await? else {
            return Ok(None);
        };
        read_licensed(&self.fetcher(), &file, path).await.map(Some)
    }

    /// The actor's own email (`<handle>@<domain>`) — the CAL-ADDRESS an imported
    /// event's roster carries the owner's answer against, and its `ORGANIZER`
    /// when the owner hosted it. `handle` from `fauna.account.get`, `domain` from
    /// `fauna.setup.status`; a resolution failure yields `""` (roster and
    /// organizer omitted), which is the same safe degrade
    /// `FfiCalDavClient::self_email` takes.
    async fn self_email(&self) -> &str {
        self.self_email
            .get_or_init(|| async {
                let handle = AccountClient::new(Arc::clone(&self.nest))
                    .get()
                    .await
                    .ok()
                    .and_then(|r| r.handle)
                    .unwrap_or_default();
                let domain = self
                    .nest
                    .request::<SetupStatusRequest, SetupStatusReply>(
                        "fauna.setup.status",
                        SetupStatusRequest::default(),
                    )
                    .await
                    .map(|r| r.domain)
                    .unwrap_or_default();
                if handle.is_empty() && domain.is_empty() {
                    String::new()
                } else {
                    format!("{handle}@{domain}")
                }
            })
            .await
    }

    /// `(actor_id, msek)` for an encrypted CalDAV write, or `None` when
    /// mail/CalDAV was never enabled (no `msek` minted).
    ///
    /// Re-derived per call (a mail-custody read), exactly as
    /// `FfiCalDavClient::caldav_context` does — so the `msek` is never held past
    /// the write that needs it. Follow-up if a big events import proves this too
    /// chatty: cache it for the run's lifetime, which is a deliberate
    /// key-material-lifetime decision rather than a performance tweak.
    async fn dav_context(&self) -> Result<Option<([u8; 32], [u8; 32])>, ArchiveNestError> {
        let actor_id = self.keypair()?.actor_id().0;
        // Write-only: an import seals to the current generation alone, so the
        // custody's prior generations (the read ring's) are not carried.
        Ok(dav_store_context(self.mail.as_ref(), actor_id)
            .await
            .map(|ctx| (ctx.actor_id, ctx.msek)))
    }

    /// The actor's first calendar, in `created_at` order — the one an import
    /// writes into.
    async fn first_calendar(
        &self,
        actor_id: [u8; 32],
    ) -> Result<Option<[u8; 32]>, ArchiveNestError> {
        let listing = CalDavClient::new(Arc::clone(&self.nest))
            .list_calendars(ListCalendarsRequest {
                actor_id: actor_id.to_vec(),
            })
            .await
            .map_err(nest_err)?;
        Ok(listing
            .calendars
            .first()
            .and_then(|c| <[u8; 32]>::try_from(c.calendar_id.as_slice()).ok()))
    }
}

#[async_trait::async_trait]
impl ArchiveNest for RpcArchiveNest {
    async fn supports_hidden_tiers(&self) -> Result<bool, ArchiveNestError> {
        SubscriptionsClient::new(Arc::clone(&self.nest))
            .nest_supports(HIDDEN_TIERS)
            .await
            .map_err(nest_err)
    }

    async fn list_archive_folders(&self) -> Result<Vec<ArchiveFolderRef>, ArchiveNestError> {
        // The unrendered rows, each sealed row named from custody by hash
        // (`path-sealing.md` § the set-name plane): this client carries no
        // label custody, and a sealed set rests no plaintext name — the
        // rendered `list` would drop every archive folder. Archive folders are
        // owner-created, so custody names every one of them.
        let listing = FoldersClient::new(Arc::clone(&self.nest))
            .list_wire()
            .await
            .map_err(nest_err)?;
        let custody = self
            .folder_keys
            .load()
            .await
            .map_err(|e| ArchiveNestError::Transport(format!("folder-key custody: {e:#}")))?;
        let mut out = Vec::new();
        // Recognized by the marker alone (`archive-import.md` § Storage): a
        // folder has no type, so no field of the row narrows the walk.
        for summary in fauna_client_folders::custody::named_from_custody(listing.folders, &custody)
        {
            let bytes = match self.read_path(&summary.name, MARKER_PATH).await {
                Ok(Some(bytes)) => bytes,
                Ok(None) => continue,
                // A marker no judged row of the folder names was never
                // written by this owner's import — the folder is not one of
                // ours, exactly like an undecodable marker below. A history
                // the nest withheld stays an error: that is not an answer.
                Err(ArchiveNestError::Rejected { code, .. }) if code == ENTRY_UNLICENSED_CODE => {
                    tracing::warn!(
                        target: "fauna_archive_import",
                        "folder {} lists an archive marker no judged row licenses; skipping",
                        summary.name
                    );
                    continue;
                }
                Err(e) => return Err(e),
            };
            // A folder carrying an unreadable marker is simply not one of ours
            // — never a hard failure of the listing every hydrate runs.
            let Ok(marker) = fauna_cbor::decode_strict::<ArchiveMarker>(&bytes) else {
                tracing::warn!(
                    target: "fauna_archive_import",
                    "folder {} has an undecodable archive marker; skipping",
                    summary.name
                );
                continue;
            };
            out.push(ArchiveFolderRef {
                folder: summary.name,
                marker,
            });
        }
        Ok(out)
    }

    async fn create_folder(&self, name: &str) -> Result<(), ArchiveNestError> {
        // Through the shared create helper: the set's nonce lands in the
        // owner's custody before the nest sees the set, and the set is sealed
        // from birth under the owner's root, passed explicitly since this
        // client carries no label custody.
        let owner = BackupKey::derive(self.keypair()?.secret_bytes());
        fauna_client_folders::create_set_with_owner_root(
            &FoldersClient::new(Arc::clone(&self.nest)),
            &*self.folder_keys,
            FolderCreateRequest {
                name: name.to_string(),
                // Private: the archive rests owner-sealed, never declassified.
                audience: Some("private".to_string()),
                ..Default::default()
            },
            Some(owner),
        )
        .await
        .map(|_| ())
        // A taken name arrives as `fauna.folders.conflict`, which the run's
        // `-2`, `-3`, … loop matches on.
        .map_err(|e| match e {
            fauna_client_folders::SetLifecycleError::Nest(e) => nest_err(e),
            fauna_client_folders::SetLifecycleError::Custody(e) => {
                ArchiveNestError::Transport(format!("{e:#}"))
            }
        })
    }

    async fn write_file(
        &self,
        folder: &str,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), ArchiveNestError> {
        let sealed = fauna_sync_engine::seal::seal_blob(&bytes, Some((self.seal_root()?, None)))
            .map_err(walk_err)?;
        let store_keys: Vec<ContentHash> = sealed.chunks.iter().map(|(h, _)| *h).collect();
        let missing: HashSet<ContentHash> = self
            .sync_http
            .check_chunks(&store_keys)
            .await
            .map_err(walk_err)?
            .into_iter()
            .collect();
        // Sequential, like the fetcher's download loop below and for the same
        // reason: the engine's pooled transfer path is private to it. The
        // same follow-up lifts that pool behind a public seam for both
        // directions; the raw zip's chunk set is where it will show first.
        for (hash, body) in &sealed.chunks {
            if missing.contains(hash) {
                self.sync_http
                    .upload_chunk(hash, body)
                    .await
                    .map_err(walk_err)?;
            }
        }
        self.sync_http
            .upload_manifest(&sealed.manifest_bytes)
            .await
            .map_err(walk_err)?;
        // Sealed path label, under the same owner root the chunks sealed under.
        // `None` (no root, or a seal failure) records plaintext-only — the
        // ratified degrade, never a user-facing error
        // (`file-sync.md` § Sealed names & paths).
        let path_sealed = self
            .owner_keys()
            .ok()
            .and_then(|keys| fauna_core::label_custody::seal_path_from_keys(&keys, path).ok());
        self.record_client(folder)
            .await?
            .changes_record(
                folder,
                self.device_id_hex.clone(),
                path,
                Some(hex::encode(sealed.manifest_hash.digest())),
                bytes.len() as i64,
                // Readers treat `create` and `modify` identically; a re-write of
                // the same path (the run's `state/import.cbor` checkpoints)
                // records a new head plus a new version either way.
                "create",
                // Owner-only (private) folder: no M2 content-key generation.
                None,
                None,
                path_sealed,
                None,
                None,
                None,
            )
            .await
            .map_err(nest_err)?;
        Ok(())
    }

    async fn read_file(
        &self,
        folder: &str,
        path: &str,
    ) -> Result<Option<Vec<u8>>, ArchiveNestError> {
        self.read_path(folder, path).await
    }

    async fn open_folder_archive(
        &self,
        folder: &str,
        path: &str,
    ) -> Result<Option<SharedSource>, ArchiveNestError> {
        let Some(file) = self.head_file(folder, path).await? else {
            return Ok(None);
        };
        let len = file_len_by_manifest(
            &self.fetcher(),
            &file.keys,
            file.manifest,
            file.content_key_version,
        )
        .await
        .map_err(walk_err)?;
        let reader = Arc::new(ManifestRangeReader {
            fetcher: self.fetcher(),
            file,
            path: path.to_string(),
        });
        Ok(Some(Arc::new(FolderArchiveSource::new(reader, len))))
    }

    async fn provision_owner_only_tier(&self) -> Result<TierGate, ArchiveNestError> {
        self.author()?
            .provision_owner_only_tier()
            .await
            .map(gate_of)
            .map_err(author_err)
    }

    async fn provision_followers_tier(&self) -> Result<TierGate, ArchiveNestError> {
        self.author()?
            .provision_followers_tier()
            .await
            .map(gate_of)
            .map_err(author_err)
    }

    async fn create_post(&self, signed_bytes: Vec<u8>) -> Result<String, ArchiveNestError> {
        PostsClient::new(Arc::clone(&self.nest))
            .posts_create(signed_bytes)
            .await
            .map(|reply| reply.post_id)
            .map_err(nest_err)
    }

    async fn upload_public_media(
        &self,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<UploadedMedia, ArchiveNestError> {
        let api = self.nest.auth().content_api();
        let blob = upload_public_post_blob_bytes(&api, name, &bytes)
            .await
            .map_err(ArchiveNestError::Transport)?;
        uploaded_media(blob)
    }

    async fn upload_sealed_media(
        &self,
        name: &str,
        bytes: Vec<u8>,
        seal: &MediaSeal,
    ) -> Result<UploadedMedia, ArchiveNestError> {
        let api = self.nest.auth().content_api();
        let period = PeriodMediaSeal {
            seal_id: seal.seal_id,
            tier: seal.tier.clone(),
            period_version: seal.period_version,
            period_key: seal.period_key.clone(),
        };
        let blob = upload_period_sealed_media(&api, name, &bytes, &period)
            .await
            .map_err(ArchiveNestError::Transport)?;
        uploaded_media(blob)
    }

    async fn upload_gated_body(&self, sealed: Vec<u8>) -> Result<String, ArchiveNestError> {
        let api = self.nest.auth().content_api();
        upload_gated_post_blob(&api, sealed)
            .await
            .map_err(ArchiveNestError::Transport)
    }

    async fn calendar_ready(&self) -> Result<bool, ArchiveNestError> {
        let Some((actor_id, _msek)) = self.dav_context().await? else {
            return Ok(false);
        };
        Ok(self.first_calendar(actor_id).await?.is_some())
    }

    async fn put_event(&self, event: &ImportedEvent) -> Result<(), ArchiveNestError> {
        let Some((actor_id, msek)) = self.dav_context().await? else {
            return Err(ArchiveNestError::Unsupported(
                "calendar is not enabled for this actor".to_string(),
            ));
        };
        let Some(calendar_id) = self.first_calendar(actor_id).await? else {
            return Err(ArchiveNestError::NotFound(
                "a calendar to import events into".to_string(),
            ));
        };
        // The owner's own answer: their roster line with its PARTSTAT, the
        // organizer only when they hosted (`ImportedEvent::owner_reply`).
        let reply = event.owner_reply(self.self_email().await);
        let end = event
            .end
            .unwrap_or(Timestamp(event.start.0.saturating_add(3_600 * 1_000_000)));
        let fields = EventFields {
            summary: event.summary.clone(),
            dtstart: rfc3339_utc(event.start),
            dtend: rfc3339_utc(end),
            location: event.location.clone().unwrap_or_default(),
            url: event.url.clone().unwrap_or_default(),
            description: event.description.clone().unwrap_or_default(),
            uid: event.uid.clone(),
            status: "confirmed".to_string(),
            // `dtstamp` is stamped from the write timestamp by
            // `seal_and_put_event`; everything else this import does not carry
            // is the empty default.
            ..Default::default()
        };
        let ext = FaunaEventExt {
            // The asymmetric `interested` refinement over the owner's wire
            // `TENTATIVE`: the owner is the only roster entry an import knows
            // (attendee lists stay in the sealed model — § What each category
            // becomes, the events row).
            interested_attendees: if reply.interested {
                reply.attendees.iter().map(|a| a.email.clone()).collect()
            } else {
                vec![]
            },
            // An imported event never arrived over the inbound scheduling rail,
            // so it carries no organizer binding and no resolution hints — it is
            // *unbound*, and a later inbound CANCEL / updating REQUEST falls back
            // to resolve-or-refuse rather than to "anyone may"
            // (`caldav-server.md` § Who may mutate an existing event over the
            // inbound rail). That is exactly `Default`, and taking it by
            // struct-update keeps the next sidecar field from breaking this site
            // the way the organizer binding did.
            ..Default::default()
        };
        CalDavClient::new(Arc::clone(&self.nest))
            .seal_and_put_event(
                &actor_id,
                &calendar_id,
                &uid_hash(&event.uid),
                &msek,
                &fields,
                &reply.attendees,
                &reply.organizer,
                Some(&ext),
                Timestamp::now_secs(),
                None,
            )
            .await
            .map_err(put_event_err)?;
        Ok(())
    }
}

// ── the licensed read ──────────────────────────────────────────────────────

/// One archive-folder file a judged change row licenses: the manifest the
/// head listed, and the keys and stamp the licensed open runs under.
struct LicensedFile {
    manifest: ContentHash,
    /// The owner reader bound to the licensing row's signer, sealed-only.
    keys: FileDownloadKeys,
    /// The licensing row's signed stamp (`None` for this owner-only folder,
    /// which carries no M2 generation — read off the row, never assumed).
    content_key_version: Option<u64>,
}

/// The live record for `path` in `folder`, or `None` when no live record
/// names it. `fauna.sync.files` is the head projection over the change log
/// and carries the `manifest_hash` the walk needs — the home nest's unsigned
/// word, which [`licensed_head`] licenses before any byte is read.
async fn head_entry<R>(
    sync: &SyncRpc<R>,
    folder: &str,
    path: &str,
) -> Result<Option<SyncFile>, ArchiveNestError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    let reply = sync.files(folder, None).await.map_err(nest_err)?;
    Ok(find_head(reply.files, path))
}

/// The head of `path` in `folder`, licensed by the judged change row of the
/// folder naming exactly the listed manifest under the hash of `path`
/// (`backup-restore.md` § Security Properties → Integrity;
/// `archive-import.md` § Storage) — the shared licensed read
/// ([`LicensedManifestReader`]), the snapshot entry reader's own, never a
/// copy. `None` when nothing is listed at the path.
///
/// The listing names an address and nothing more: the licence is read BEFORE
/// any byte, from the signed version history judged under `seat` (resolved
/// only once something is listed — the marker scan asks every custody-named
/// folder, and most hold no marker). No such row → [`ENTRY_UNLICENSED_CODE`],
/// the manifest never fetched; a history the nest withholds → `Transport`.
/// Never a fallback to the listing. The licensed open keeps
/// [`FileDownloadKeys::sealed_only`] even where the row would lift it: the
/// archive folder is never plaintext.
async fn licensed_head<R>(
    sync: &SyncRpc<R>,
    owner_keys: &FileDownloadKeys,
    folder: &str,
    path: &str,
    seat: impl AsyncFnOnce() -> Result<ReaderSeat, ArchiveNestError>,
) -> Result<Option<LicensedFile>, ArchiveNestError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    let Some(head) = head_entry(sync, folder, path).await? else {
        return Ok(None);
    };
    let digest = fauna_core::hex32::decode(&head.manifest_hash)
        .map_err(|e| ArchiveNestError::Transport(format!("manifest hash for {path}: {e}")))?;
    let manifest = ContentHash::from_digest_raw(digest);
    let seat = seat().await?;
    let set = SetAddress {
        folder: folder.to_string(),
        folder_hash: None,
    };
    let reader = LicensedManifestReader::new(sync, &seat, &set);
    let licence = reader
        .require_licence(path, &manifest)
        .await
        .map_err(licence_err)?;
    Ok(Some(LicensedFile {
        manifest,
        keys: reader.licensed_keys(owner_keys, &licence).sealed_only(),
        content_key_version: licence.content_key_version,
    }))
}

/// The whole of one licensed file, over the shared download walk.
async fn read_licensed(
    fetcher: &dyn BlobFetcher,
    file: &LicensedFile,
    path: &str,
) -> Result<Vec<u8>, ArchiveNestError> {
    download_file_bytes_by_manifest(
        fetcher,
        &file.keys,
        file.manifest,
        file.content_key_version,
        path,
    )
    .await
    .map_err(walk_err)
}

// ── the head lookup ────────────────────────────────────────────────────────

/// The `fauna.sync.files` row naming `path`, by the key the nest actually
/// serves: the **convergent path salt first**, the plaintext path second.
///
/// An archive folder is minted `audience: "private"`
/// ([`RpcArchiveNest::create_folder`]) and a private folder **rests no
/// plaintext path** — the S9 scrub (`bins/fauna-nest/src/db/mod.rs`,
/// `rests_plaintext_paths() == is_public_audience()`, applied
/// at record time in `sync_handlers::record_change_core`). The listing's
/// required `path` field then arrives as the `""` scrub sentinel and the row's
/// identity lives in its `path_hash` / `path_sealed` pair, which
/// `fauna_protocol::sync::SyncFile::path_hash` states outright: *"a seal whose
/// salt is missing is unrenderable the moment the plaintext `path` scrubs."*
/// Matching the plaintext alone therefore missed **every** file this
/// seam ever wrote — the model read, the marker scan every hydrate runs, and
/// the resume's range-read open — and the in-memory `nest::fakes::FakeNest`
/// could not show it, because it keys whole files by their plaintext path.
///
/// The plaintext arm is kept, and is second on purpose: it serves only the
/// ratified plaintext classes (a `public`-audience folder, whose names and
/// paths are URLs), which
/// an archive folder never is. Its `!is_empty()` guard is what stops the
/// sentinel from matching an empty `path` argument.
fn find_head(files: Vec<SyncFile>, path: &str) -> Option<SyncFile> {
    let salt = fauna_core::sync::path_hash(path);
    files.into_iter().find(|f| {
        f.path_hash
            .as_ref()
            .is_some_and(|h| h.as_slice() == salt.as_slice())
            || (!f.path.is_empty() && f.path == path)
    })
}

// ── the platform opener ────────────────────────────────────────────────────

/// The native [`ArchiveOpener`]: the export zip is a file on disk.
pub struct FileOpener;

impl ArchiveOpener for FileOpener {
    fn open(&self, path: &str) -> Result<SharedSource, ArchiveNestError> {
        match FileSource::open(path) {
            Ok(source) => Ok(Arc::new(source)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(ArchiveNestError::NotFound(path.to_string()))
            }
            Err(e) => Err(ArchiveNestError::Unsupported(format!("open {path}: {e}"))),
        }
    }

    fn file_name(&self, path: &str) -> String {
        std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "archive.zip".to_string())
    }
}

// ── the constructor ────────────────────────────────────────────────────────

/// Build the machine over a live connection: the WS-RPC seam, the native file
/// opener, and the owner's identity (every imported post is signed by the app of
/// the person it belongs to — § Architectural rules).
///
/// `device_id` is the sync device id every change record this import writes is
/// stamped with — the same one the app's other sync surfaces use.
///
/// Refuses a bearer-only connection: the archive folder rests owner-sealed under
/// a key derived from the identity seed, which such a connection does not hold.
///
/// `period_keys` is the owner's period-key custody — the reserved owner-only
/// and followers tiers' keys, which the gated posts seal under; `folder_keys`
/// the account's folder-key custody the archive folder's nonce lives in;
/// `mail` the account's mail custody — the MSEK an imported event's encrypted
/// CalDAV write seals under.
pub fn build_archive_import_machine(
    nest: Arc<NestClient>,
    device_id: [u8; 32],
    period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    folder_keys: Arc<dyn fauna_client_folders::FolderKeyStore>,
    mail: Arc<dyn fauna_client_config::MailStore>,
) -> Result<ArchiveImportMachine, ArchiveNestError> {
    let secret = {
        let Some(keypair) = nest.auth().keypair() else {
            return Err(ArchiveNestError::Unsupported(BEARER_ONLY.to_string()));
        };
        *keypair.secret_bytes()
    };
    let sync_http = Arc::new(ChunkStore::new(Arc::clone(nest.auth()), &device_id));
    let seam = RpcArchiveNest {
        nest,
        device_id_hex: hex::encode(device_id),
        sync_http,
        self_email: tokio::sync::OnceCell::new(),
        set_nonces: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        period_keys,
        folder_keys,
        mail,
    };
    Ok(ArchiveImportMachine::new(
        Arc::new(seam),
        Arc::new(FileOpener),
        ActorKeypair::from_secret(secret),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A `RangeReader` over a `Vec`, counting fetches.
    struct CountingReader {
        bytes: Vec<u8>,
        fetches: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl RangeReader for CountingReader {
        async fn read_range(&self, offset: u64, len: u64) -> Result<Vec<u8>, ArchiveNestError> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            let start = (offset as usize).min(self.bytes.len());
            let end = (start + len as usize).min(self.bytes.len());
            Ok(self.bytes[start..end].to_vec())
        }
    }

    fn read(source: &FolderArchiveSource, offset: u64, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        let n = source.read_at(offset, &mut buf).expect("read_at");
        buf.truncate(n);
        buf
    }

    /// The range-read bridge: sequential `read_at`s inside one 1 MiB block cost
    /// one fetch; a read straddling two blocks costs two; a re-read of a cached
    /// block costs none. (A `RangeReader` fake counts fetches.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn folder_archive_source_reads_ahead_by_block() {
        let bytes: Vec<u8> = (0..3 * READ_AHEAD_BLOCK).map(|i| (i % 251) as u8).collect();
        let reader = Arc::new(CountingReader {
            bytes: bytes.clone(),
            fetches: AtomicUsize::new(0),
        });
        let source = FolderArchiveSource::new(reader.clone(), bytes.len() as u64);
        assert_eq!(source.len(), 3 * READ_AHEAD_BLOCK);

        assert_eq!(read(&source, 0, 1000), bytes[0..1000]);
        assert_eq!(
            reader.fetches.load(Ordering::SeqCst),
            1,
            "one block fetched"
        );

        assert_eq!(read(&source, 1000, 1000), bytes[1000..2000]);
        assert_eq!(
            reader.fetches.load(Ordering::SeqCst),
            1,
            "the same block is served from the cache"
        );

        let straddle = READ_AHEAD_BLOCK - 500;
        assert_eq!(
            read(&source, straddle, 1000),
            bytes[straddle as usize..straddle as usize + 1000]
        );
        assert_eq!(
            reader.fetches.load(Ordering::SeqCst),
            2,
            "the second block is fetched, the first was still cached"
        );

        // The slot holds one block, so re-reading the *first* one now costs a
        // fetch again — the bound that keeps a multi-gigabyte zip out of memory.
        assert_eq!(read(&source, 0, 1000), bytes[0..1000]);
        assert_eq!(reader.fetches.load(Ordering::SeqCst), 3, "one slot, no map");

        // Past the end reads nothing and fetches nothing; a read clamped by the
        // end of the file returns the short tail.
        assert!(read(&source, 3 * READ_AHEAD_BLOCK, 16).is_empty());
        assert_eq!(reader.fetches.load(Ordering::SeqCst), 3);
        assert_eq!(read(&source, 3 * READ_AHEAD_BLOCK - 4, 16).len(), 4);
    }

    /// The glue's half of the run's folder-naming contract
    /// (`run.rs::create_named_folder`): a taken name must come back as
    /// `Rejected { code: "fauna.folders.conflict" }` — that exact code is what
    /// makes the loop try `<base>-2`, `<base>-3`, … instead of failing the
    /// import. Anything that never reached nest stays `Transport`, which the
    /// same loop propagates rather than suffixing around.
    #[test]
    fn a_taken_folder_name_maps_to_the_conflict_code() {
        let conflict = NestClientError::Rpc(fauna_protocol::RpcError {
            code: "fauna.folders.conflict".to_string(),
            message: Box::new(fauna_protocol::LocalizedText::new("error.folders.conflict")),
            details: None,
            extra: Default::default(),
        });
        match nest_err(conflict) {
            ArchiveNestError::Rejected { code, .. } => assert_eq!(code, "fauna.folders.conflict"),
            other => panic!("expected a coded rejection, got {other:?}"),
        }
        assert!(matches!(
            nest_err(NestClientError::RpcTimeout),
            ArchiveNestError::Transport(_)
        ));
    }

    /// `raw/<original filename>` takes the archive's own name; a path with no
    /// file component falls back rather than writing `raw/`.
    #[test]
    fn file_name_uses_the_archives_own_name() {
        let opener = FileOpener;
        assert_eq!(
            opener.file_name(&format!("{}fb-export.zip", std::path::MAIN_SEPARATOR_STR)),
            "fb-export.zip"
        );
        assert_eq!(opener.file_name("fb-export.zip"), "fb-export.zip");
        assert_eq!(opener.file_name(".."), "archive.zip");
    }

    /// The head lookup resolves a **scrubbed** row by its convergent salt and a
    /// plaintext row by its path — the two shapes `fauna.sync.files` can serve.
    ///
    /// The scrubbed one is what an archive folder actually looks like on the
    /// wire (private audience ⇒ no resting plaintext path ⇒ the `""` sentinel),
    /// and matching only the plaintext missed it every time: the model read
    /// then failed as "model/posts.cbor is missing from the archive folder"
    /// with the file sitting right there. The fake nest keys files by plaintext
    /// path, so only a hand-built wire row can pin this.
    #[test]
    fn find_head_resolves_a_scrubbed_row_by_its_salt() {
        let scrubbed = SyncFile {
            // The ratified scrub sentinel on this required wire field.
            path: String::new(),
            path_hash: Some(fauna_protocol::ByteBuf::from(
                fauna_core::sync::path_hash("model/posts.cbor").to_vec(),
            )),
            manifest_hash: "aa".repeat(32),
            size_bytes: 12,
            ..Default::default()
        };
        let plaintext = SyncFile {
            path: "raw/x.zip".to_string(),
            path_hash: None,
            manifest_hash: "bb".repeat(32),
            size_bytes: 34,
            ..Default::default()
        };
        let rows = vec![scrubbed.clone(), plaintext.clone()];

        assert_eq!(
            find_head(rows.clone(), "model/posts.cbor").as_ref(),
            Some(&scrubbed),
            "a scrubbed row resolves by its path_hash"
        );
        assert_eq!(
            find_head(rows.clone(), "raw/x.zip").as_ref(),
            Some(&plaintext),
            "a plaintext row still resolves by its path"
        );
        assert!(
            find_head(rows.clone(), "model/albums.cbor").is_none(),
            "an unrelated path matches neither row"
        );
        // The sentinel is not a wildcard: an empty query must not match the
        // scrubbed row's empty `path` (the guard the plaintext arm carries).
        assert!(
            find_head(rows, "").is_none(),
            "the \"\" sentinel matches nothing"
        );
    }

    /// Microseconds → the RFC 3339 UTC string `EventFields` documents.
    #[test]
    fn timestamps_render_as_rfc3339_utc() {
        assert_eq!(rfc3339_utc(Timestamp(0)), "1970-01-01T00:00:00Z");
        // Micros, not seconds: 1_800_000_000 s == 2027-01-15T08:00:00Z.
        assert_eq!(
            rfc3339_utc(Timestamp(1_800_000_000_000_000)),
            "2027-01-15T08:00:00Z"
        );
        // Sub-second precision is truncated, never rounded up into the next
        // second (an event's DTSTART is a wall-clock instant, not a duration).
        assert_eq!(
            rfc3339_utc(Timestamp(1_800_000_000_999_999)),
            "2027-01-15T08:00:00Z"
        );
    }
}

/// The archive folder's reads are licensed by the judged change row naming
/// exactly the listed manifest under the path (`backup-restore.md` § Security
/// Properties → Integrity). Each refusal is red when the
/// reader opens the head projection's address without the licence.
#[cfg(test)]
mod licence_tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use fauna_client_sync::SetNonceSource;
    use fauna_client_testkit::{RejectingRequester, block_on};
    use fauna_core::blob_seal::SealedBlob;
    use fauna_protocol::files::{FileVersionInfo, FilesVersionsListReply};
    use fauna_protocol::folders::{FolderSummary, FoldersListReply, KIND_FOLDERS_LIST};
    use fauna_protocol::sync::{SyncChange, SyncFilesReply};
    use fauna_protocol::sync_writer_sig::ChangeSigner;

    const KIND_SYNC_FILES: &str = "fauna.sync.files";
    const KIND_VERSIONS_LIST: &str = "fauna.files.versions.list";
    const FOLDER: &str = "facebook-archive";
    const NONCE: [u8; 32] = [7; 32];
    const MODEL: &str = "model/posts.cbor";
    const STATE: &str = "state/import.cbor";
    const SECRET: [u8; 32] = [0x51; 32];

    fn owner() -> ActorKeypair {
        ActorKeypair::from_secret(SECRET)
    }

    /// The reader `RpcArchiveNest::owner_keys` builds, exactly.
    fn owner_keys() -> FileDownloadKeys {
        FileDownloadKeys::owner(BackupKey::derive(&SECRET)).sealed_only()
    }

    /// A blob store in a `HashMap`, counting manifest and chunk fetches so a
    /// test can pin that a refused read cost no byte.
    #[derive(Default)]
    struct MapFetcher {
        blobs: Mutex<HashMap<[u8; 32], Vec<u8>>>,
        manifests: Mutex<usize>,
        chunks: Mutex<usize>,
    }

    impl MapFetcher {
        fn stock(&self, blob: &SealedBlob) {
            let mut blobs = self.blobs.lock().unwrap();
            blobs.insert(blob.manifest_hash.digest(), blob.manifest_bytes.clone());
            for (key, body) in &blob.chunks {
                blobs.insert(key.digest(), body.clone());
            }
        }

        fn get(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
            self.blobs
                .lock()
                .unwrap()
                .get(&hash.digest())
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("404"))
        }

        /// `(manifests, chunks)` fetched so far.
        fn fetches(&self) -> (usize, usize) {
            (
                *self.manifests.lock().unwrap(),
                *self.chunks.lock().unwrap(),
            )
        }
    }

    #[async_trait::async_trait]
    impl BlobFetcher for MapFetcher {
        async fn fetch_manifest(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
            *self.manifests.lock().unwrap() += 1;
            self.get(hash)
        }

        async fn fetch_chunks(
            &self,
            store_keys: &[ContentHash],
            _path: &str,
        ) -> anyhow::Result<Vec<Vec<u8>>> {
            *self.chunks.lock().unwrap() += store_keys.len();
            store_keys.iter().map(|k| self.get(k)).collect()
        }
    }

    /// `bytes` as the run writes them (`write_file`): sealed under the
    /// owner's convergent root.
    fn sealed(bytes: &[u8]) -> SealedBlob {
        let root = BackupKey::derive(&SECRET).convergent_chunk_root();
        fauna_core::blob_seal::seal_blob(bytes, Some((root, None))).unwrap()
    }

    /// `bytes` as a home nest builds them for itself: plaintext, at a valid
    /// address.
    fn plaintext(bytes: &[u8]) -> SealedBlob {
        fauna_core::blob_seal::seal_blob(bytes, None).unwrap()
    }

    /// The head projection's row for `path`, scrubbed as a private folder's
    /// rests, listing `manifest`.
    fn head(path: &str, manifest: &ContentHash) -> SyncFile {
        SyncFile {
            path: String::new(),
            path_hash: Some(fauna_protocol::ByteBuf::from(
                fauna_core::sync::path_hash(path).to_vec(),
            )),
            manifest_hash: fauna_core::hex32::encode(&manifest.digest()),
            size_bytes: 1,
            ..Default::default()
        }
    }

    /// The owner's signed row naming `manifest` under `path` — what the run's
    /// own `write_file` records.
    fn version(path: &str, manifest: &ContentHash) -> FileVersionInfo {
        version_under(path, manifest, NONCE)
    }

    /// [`version`], signed under `nonce` — another set's, for a row that
    /// does not verify here.
    fn version_under(path: &str, manifest: &ContentHash, nonce: [u8; 32]) -> FileVersionInfo {
        let author = owner();
        let mut row = SyncChange {
            seq: 3,
            path_hash: fauna_core::hex32::encode(&fauna_core::sync::path_hash(path)),
            manifest_hash: Some(fauna_core::hex32::encode(&manifest.digest())),
            size_bytes: 10,
            change_type: "create".into(),
            created_at: 1_000,
            device_id: Some(fauna_core::hex32::encode(&[4; 32])),
            author_actor_id: Some(author.actor_id().to_hex()),
            ..Default::default()
        };
        ChangeSigner::direct(&author)
            .sign_row(&mut row, nonce)
            .unwrap();
        FileVersionInfo {
            path_hash: fauna_core::sync::path_hash(path).to_vec().into(),
            version_num: row.seq,
            manifest_hash: manifest.digest().to_vec().into(),
            size_bytes: row.size_bytes,
            created_at: row.created_at,
            folder: Some(FOLDER.into()),
            author_actor_id: author.actor_id().to_hex(),
            device_id: Some([4; 32].to_vec().into()),
            change_type: Some("create".into()),
            signature: row.signature.clone(),
            signer_key: row.signer_key.clone(),
            ..Default::default()
        }
    }

    fn folders_list() -> FoldersListReply {
        FoldersListReply {
            folders: vec![FolderSummary {
                name: FOLDER.into(),
                role: Some("owner".into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// A home nest listing `files` at the head and serving `versions` as the
    /// folder's history (`None`: the history is withheld).
    fn nest(files: Vec<SyncFile>, versions: Option<Vec<FileVersionInfo>>) -> RejectingRequester {
        let nest = RejectingRequester::new()
            .reply(
                KIND_SYNC_FILES,
                &SyncFilesReply {
                    files,
                    extra: Default::default(),
                },
            )
            .reply(KIND_FOLDERS_LIST, &folders_list());
        match versions {
            Some(versions) => nest.reply(
                KIND_VERSIONS_LIST,
                &FilesVersionsListReply {
                    versions,
                    ..Default::default()
                },
            ),
            None => nest,
        }
    }

    fn seat() -> ReaderSeat {
        ReaderSeat {
            own: Some(owner().actor_id().0),
            nonces: Some(SetNonceSource::Fixed(NONCE)),
            ..Default::default()
        }
    }

    /// `RpcArchiveNest::read_path`'s composition over a scripted nest.
    fn read(
        nest: &RejectingRequester,
        fetcher: &MapFetcher,
        path: &str,
    ) -> Result<Option<Vec<u8>>, ArchiveNestError> {
        let sync = SyncRpc::new(nest);
        block_on(async {
            let Some(file) =
                licensed_head(&sync, &owner_keys(), FOLDER, path, async || Ok(seat())).await?
            else {
                return Ok(None);
            };
            read_licensed(fetcher, &file, path).await.map(Some)
        })
    }

    fn is_unlicensed(e: &ArchiveNestError) -> bool {
        matches!(e, ArchiveNestError::Rejected { code, .. } if code == ENTRY_UNLICENSED_CODE)
    }

    /// The positive case: a sealed file the run wrote and signed reads —
    /// whole, and by the range open's length walk.
    #[test]
    fn a_sealed_file_the_run_wrote_and_signed_reads() {
        let blob = sealed(b"the model the run wrote");
        let fetcher = MapFetcher::default();
        fetcher.stock(&blob);
        let nest = nest(
            vec![head(MODEL, &blob.manifest_hash)],
            Some(vec![version(MODEL, &blob.manifest_hash)]),
        );

        let bytes = read(&nest, &fetcher, MODEL).expect("licensed");
        assert_eq!(bytes.as_deref(), Some(&b"the model the run wrote"[..]));

        let sync = SyncRpc::new(&nest);
        let len = block_on(async {
            let file = licensed_head(&sync, &owner_keys(), FOLDER, MODEL, async || Ok(seat()))
                .await
                .expect("licensed")
                .expect("listed");
            assert!(
                file.keys.sealed_only,
                "the archive folder is never plaintext"
            );
            file_len_by_manifest(
                &fetcher,
                &file.keys,
                file.manifest,
                file.content_key_version,
            )
            .await
        })
        .expect("length");
        assert_eq!(len, b"the model the run wrote".len() as u64);
    }

    /// Nothing listed at the path is `None`, with no history read.
    #[test]
    fn an_unlisted_path_reads_none_without_a_history_read() {
        let nest = nest(vec![], Some(vec![]));
        assert_eq!(read(&nest, &MapFetcher::default(), MODEL).unwrap(), None);
        assert_eq!(nest.kinds(), [KIND_SYNC_FILES]);
    }

    /// A home nest serving a self-built plaintext model file at a listed
    /// address: no judged row names it, so it is refused before any byte.
    #[test]
    fn a_self_built_plaintext_model_file_is_refused_before_any_fetch() {
        let forged = plaintext(b"posts the nest wrote for the owner");
        let fetcher = MapFetcher::default();
        fetcher.stock(&forged);
        let nest = nest(vec![head(MODEL, &forged.manifest_hash)], Some(vec![]));

        let err = read(&nest, &fetcher, MODEL).expect_err("refused");
        assert!(is_unlicensed(&err), "{err}");
        assert_eq!(fetcher.fetches(), (0, 0), "no byte for an unlicensed read");
    }

    /// Even a row naming a plaintext manifest does not open it: the archive
    /// folder is never plaintext, so the licensed open stays sealed-only and
    /// refuses before any chunk.
    #[test]
    fn a_licensed_plaintext_manifest_is_still_refused_before_any_chunk() {
        let plain = plaintext(b"plaintext in a folder that never holds any");
        let fetcher = MapFetcher::default();
        fetcher.stock(&plain);
        let nest = nest(
            vec![head(STATE, &plain.manifest_hash)],
            Some(vec![version(STATE, &plain.manifest_hash)]),
        );

        let err = read(&nest, &fetcher, STATE).expect_err("refused");
        assert!(
            err.to_string()
                .contains(PermanentApplyFailure::PLAINTEXT_REFUSED.reason),
            "{err}"
        );
        assert_eq!(fetcher.fetches().1, 0, "no chunk of a plaintext manifest");
    }

    /// Another path's owner-sealed manifest listed at `model/posts.cbor`: the
    /// seal is the owner's, but the only row naming it names another path, so
    /// it is refused unfetched.
    #[test]
    fn another_paths_sealed_manifest_is_refused_unfetched() {
        let other = sealed(b"the owner's checkpoint, not their posts");
        let fetcher = MapFetcher::default();
        fetcher.stock(&other);
        let nest = nest(
            vec![head(MODEL, &other.manifest_hash)],
            Some(vec![version(STATE, &other.manifest_hash)]),
        );

        let err = read(&nest, &fetcher, MODEL).expect_err("refused");
        assert!(is_unlicensed(&err), "{err}");
        assert_eq!(fetcher.fetches(), (0, 0));
    }

    /// A row that does not verify is not a row (ruling (3)): the owner's own
    /// sealed file, under a row signed under another set's nonce, licenses
    /// nothing in this folder and is refused unfetched.
    #[test]
    fn a_row_that_does_not_verify_licenses_nothing() {
        let blob = sealed(b"the owner's model, recorded in another set");
        let fetcher = MapFetcher::default();
        fetcher.stock(&blob);
        let nest = nest(
            vec![head(MODEL, &blob.manifest_hash)],
            Some(vec![version_under(MODEL, &blob.manifest_hash, [8; 32])]),
        );

        let err = read(&nest, &fetcher, MODEL).expect_err("refused");
        assert!(is_unlicensed(&err), "{err}");
        assert_eq!(fetcher.fetches(), (0, 0));
    }

    /// A history the nest withholds refuses the read — transiently, so a
    /// resume retries it — and never opens the listed manifest.
    #[test]
    fn a_withheld_history_refuses_the_read_unopened() {
        let blob = sealed(b"sealed, and the nest serves no history");
        let fetcher = MapFetcher::default();
        fetcher.stock(&blob);
        let nest = nest(vec![head(MODEL, &blob.manifest_hash)], None);

        let err = read(&nest, &fetcher, MODEL).expect_err("refused");
        assert!(matches!(err, ArchiveNestError::Transport(_)), "{err}");
        assert_eq!(fetcher.fetches(), (0, 0));
    }
}
