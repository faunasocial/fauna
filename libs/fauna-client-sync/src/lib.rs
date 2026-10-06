//! Typed-call wrapper for the `fauna.sync.*` WS-RPC kinds — the device-
//! sync control plane that migrated off `/api/v1/sync/*` HTTP (Track B13;
//! `docs/goal/architecture/api-layers.md` § File Sync,
//! `docs/goal/behavior/file-sync.md` § Status).
//!
//! All eight device-sync control-plane kinds live here now —
//! `fauna.sync.{register,changes.{list,record},status,files,backup_status,
//! devices.{list,delete}}` (plus the sibling `conflicts.{list,resolve}`). The
//! `fauna-sync-engine` drives `register` /
//! `changes.{list,record}` (the write/catch-up path); the Devices /
//! sync-status / Backups surfaces drive the reads. Priority #2: one shared
//! adapter, lifted by every app rather than reimplemented per-app. The
//! HTTP twins (`sync_routes`) are retired per surface as each adopts these
//! kinds.
//!
//! Pattern: same shape as `fauna-client-snapshots` — a thin
//! `SyncClient<R: RpcRequester>`, one async method per kind, no state
//! machine, wasm-clean (no `fauna-client` dependency).

use fauna_protocol::files::{
    FileVersionInfo, FilesVersionsGetRequest, FilesVersionsListReply, FilesVersionsListRequest,
    FilesVersionsUndeleteReply, FilesVersionsUndeleteRequest,
};
use fauna_protocol::folders::{
    ConflictCandidate, ConflictReportReply, ConflictReportRequest, ConflictResolveReply,
    ConflictResolveRequest, ConflictsListReply, ConflictsListRequest,
};
use fauna_protocol::sync::{
    DeviceGrantRegisterReply, DeviceGrantRegisterRequest, DeviceGrantRevokeReply,
    DeviceGrantRevokeRequest, SyncBackupStatusReply, SyncBackupStatusRequest,
    SyncChangeRecordReply, SyncChangeRecordRequest, SyncChangesListReply, SyncChangesListRequest,
    SyncChangesSupersedeReply, SyncChangesSupersedeRequest, SyncDeviceDeleteReply,
    SyncDeviceDeleteRequest, SyncDeviceP2pParticipationSetReply,
    SyncDeviceP2pParticipationSetRequest, SyncDevicesListReply, SyncDevicesListRequest,
    SyncFilesReply, SyncFilesRequest, SyncRegisterReply, SyncRegisterRequest, SyncStatusReply,
    SyncStatusRequest,
};
use fauna_protocol::{ByteBuf, RpcErrorClass, RpcRequester};

/// The twins' `default_capabilities` — a device registered without an explicit
/// capability set is `read,write` (the `sync_devices` column default).
fn default_capabilities() -> String {
    "read,write".to_string()
}

// The shared desktop sync-agent control client (provisioner + convergence
// delegate + folder-binding reconcile model) — every desktop, since it reaches
// the agent through the one `fauna_ipc::endpoint` seam (`sync-agent.md`
// § Consumers): the per-user unix socket on linux/macOS, the per-SID named pipe
// on windows. Consumed directly by the linux GTK client and fauna-tui (on every
// OS it runs on), and via the `fauna-ffi` adapter by FaunaKit/macOS. wasm has no
// local agent.
#[cfg(any(unix, windows))]
pub mod agent;

// The shared desktop agent spawner (`sync-agent.md` § Packaging + lifecycle) —
// the channel-agnostic half of the desktop `AgentSpawner`, lifted out of the
// linux GTK client so fauna-tui consumes it instead of a second copy
// (priority #2). Per-OS arms inside: the systemd user unit on unix, a detached
// child on windows. Same gating as the `agent` module it complements.
#[cfg(any(unix, windows))]
pub mod agent_spawner;

// The app's attachment lease on the agent (`RequestMethod::AttachApp`): while
// a desktop app is open the agent's push arm leaves banners to it.
#[cfg(any(unix, windows))]
pub mod attachment;
#[cfg(any(unix, windows))]
pub mod reseed_wire;

// The projection readers' judge (writer-signed change records, ruling (3)):
// Media items and version-history entries, verified before they are listed.
pub mod row_judge;

// The one restore decision the three restore doors share — the Media restore,
// the sync agent's verb and the conflict review list (ruling (10)(a)/(b)/(f)).
pub mod restore_branch;
pub use restore_branch::ChooseWinnerError;

pub use fauna_protocol::sync;
// The conflict kinds are namespaced `fauna.sync.conflicts.*` but their wire
// types live in `fauna_protocol::folders` (alongside the folder surface
// they ship with) — re-export so consumers reach `ConflictCandidate` etc.
pub use fauna_protocol::folders;
// The version-history kinds (`fauna.files.versions.*`) are part of the
// file-sync surface (file-sync.md § File Versions) — re-export so consumers
// reach `FileVersionInfo` etc.
pub use fauna_protocol::files;

/// A pre-resolved conflict outcome for `SyncClient::conflicts_report_with`
/// (auto-resolve, file-sync.md § Conflicts ratified 2026-07-10). The nest
/// propagates it transactionally: loser retention row + winner head row land
/// with the conflict row, so the reporting host does no record ordering.
#[derive(Debug, Clone)]
pub struct ResolvedOutcome {
    /// `"merged"` | `"latest_wins"`.
    pub resolution: String,
    /// Hex manifest hash of the winning version (the new head).
    pub winning_manifest_hash: String,
    /// Byte size of the winner — required when the winner is not one of the
    /// report's candidates (a merged result); ignored for candidate winners.
    pub winning_size_bytes: Option<i64>,
    /// M2 content-key generation of the winner's chunks (bound sets; merged
    /// results only — candidate winners carry their own).
    pub winning_content_key_version: Option<u64>,
    /// Causal watermark for the propagated winner head row (the 2026-08-02
    /// ruling — `ConflictReportRequest::winning_derived_through`): the seq the
    /// resolving device had incorporated through when it computed the
    /// resolution (for an in-order apply pass, the incoming row's own seq).
    pub winning_derived_through: Option<i64>,
    /// Causal watermark for the loser retention row — the reporter's ledger
    /// ancestor seq, when known (`losing_derived_through` on the wire).
    pub losing_derived_through: Option<i64>,
    /// The winner carries the reporter's UNPUBLISHED pre-merge novelty and
    /// must mint EDIT-class (the same-anchor ruling, 2026-08-05 —
    /// `ConflictReportRequest::winning_carries_novelty` has the wire
    /// contract).
    pub winning_carries_novelty: Option<bool>,
}

/// The sealed companions one conflict report carries (path-sealing S6-a,
/// `file-sync.md` § Sealed names & paths).
///
/// Mint with [`ConflictLabels::seal`] — never field-by-field. This is the
/// *second* conflict-report builder in the tree (`SyncEngine::report_conflict_ws`
/// is the other), and two builders quietly disagreeing on root, salt or field
/// tag is exactly the "one funnel per crate" wart the security review
/// filed as a standing risk; routing both through
/// `fauna_core::label_custody` is what stops this slice from making it worse.
///
/// It is a struct rather than three more parameters because
/// `conflicts_report_with` already carries `#[allow(clippy::too_many_arguments)]`.
#[derive(Debug, Clone, Default)]
pub struct ConflictLabels {
    /// BLAKE3 of the normalized path — the row's routing key, and the salt both
    /// seals below were minted under.
    pub path_hash: Option<ByteBuf>,
    /// The path sealed under the caller's label root.
    pub path_sealed: Option<ByteBuf>,
    /// The free-text details sealed under the same root.
    pub details_sealed: Option<ByteBuf>,
}

impl ConflictLabels {
    /// Seal one conflict's path + details through the shared
    /// `fauna_core::label_custody` funnel.
    ///
    /// `root: None` is a caller holding no key material: `path_hash` is still
    /// computed (it is nest-derivable anyway and is the row's routing key) and
    /// both seals stay `None`. ⚠ Post-flip a sealless report is REFUSED by the
    /// nest on every sealed plane (`path_seal_required` — file-sync.md § Sealed
    /// names & paths); only a `web`-mode set's ratified-plaintext rail still
    /// accepts one, so a keyless caller on a sealed set should expect the typed
    /// refusal, not a silently-degraded row.
    ///
    /// Returns `Err` only on a genuine crypto failure, which the caller should
    /// propagate rather than swallow: unlike the media upload gesture (which
    /// degrades so a seal error cannot orphan a just-stored blob), a conflict
    /// report has nothing already committed to protect, so there is no reason to
    /// fail open on a confidentiality control here.
    pub fn seal(
        root: Option<&fauna_core::path_crypto::LabelRoot>,
        path: &str,
        details: Option<&str>,
    ) -> anyhow::Result<Self> {
        let path_hash = Some(ByteBuf::from(fauna_core::sync::path_hash(path).to_vec()));
        let Some(root) = root else {
            return Ok(Self {
                path_hash,
                ..Default::default()
            });
        };
        Ok(Self {
            path_hash,
            path_sealed: Some(ByteBuf::from(fauna_core::label_custody::seal_path(
                root, path,
            )?)),
            details_sealed: match details {
                Some(d) => Some(ByteBuf::from(
                    fauna_core::label_custody::seal_conflict_details(root, path, d)?,
                )),
                None => None,
            },
        })
    }
}

/// Typed `fauna.sync.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the wasm
/// SPA passes its `WsRpcClient`. Errors propagate as the transport's
/// `R::Error`.
pub struct SyncClient<R: RpcRequester> {
    nest: R,
    /// Writer signing for [`Self::changes_record`] — every record through this
    /// client is signed when set ([`Self::with_record_signing`]).
    signing: Option<RecordSigning>,
}

/// How a [`SyncClient`] signs the change records it sends (writer-signed change
/// records, `mls-group-key-material.md` § M2 → *Writer-signed change records*
/// (1)–(2)): the host's signer, and the set nonce each folder's records bind
/// to. A folder whose nonce does not resolve records unsigned (logged) — the
/// nest refuses that once it enforces, so the gap is loud, never silent.
#[derive(Clone)]
pub struct RecordSigning {
    /// The host's signer (the principal writer key, or the identity key).
    pub signer: std::sync::Arc<fauna_protocol::sync_writer_sig::ChangeSigner>,
    /// Where each record's set nonce comes from.
    pub set_nonce: SetNonceSource,
}

/// Where a [`RecordSigning`] finds the set nonce a record binds to.
#[derive(Clone)]
pub enum SetNonceSource {
    /// An engine bound to one set: that set's nonce, whatever the folder.
    Fixed([u8; 32]),
    /// A client that records into any of the holder's sets (media, restore,
    /// archive import): the nonce by folder name, from the holder's roster +
    /// custody ([`fauna_core::folder_keys::FolderKeyResolver::set_nonce`]).
    Resolver(std::sync::Arc<dyn fauna_core::folder_keys::FolderKeyResolver>),
    /// A snapshot of set nonce lineages by folder name, resolved by the caller
    /// from custody it already read (the sync agent's own content-key
    /// resolution) — for a host that holds no key resolver but addresses sets
    /// by name. A recorder that knows live nonces alone builds it with
    /// [`Self::by_folder`]; one that read custody's retired nonces too carries
    /// them, so its readers judge history (ruling (11)(c)).
    ByFolder(
        std::sync::Arc<std::collections::HashMap<String, fauna_core::folder_keys::SetNonceLineage>>,
    ),
}

impl SetNonceSource {
    /// [`Self::ByFolder`] over live nonces alone — each set's lineage holds
    /// its live nonce and nothing retired.
    pub fn by_folder(live: std::collections::HashMap<String, [u8; 32]>) -> Self {
        Self::ByFolder(std::sync::Arc::new(
            live.into_iter()
                .map(|(folder, nonce)| {
                    (
                        folder,
                        fauna_core::folder_keys::SetNonceLineage {
                            live: Some(nonce),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        ))
    }

    /// `folder`'s set nonce — what a record for it binds to, and what a reader
    /// verifies its rows under. `Ok(None)`: no nonce in custody for the set;
    /// `Err`: the resolver's read failed.
    pub async fn lookup(&self, folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        match self {
            SetNonceSource::Fixed(nonce) => Ok(Some(*nonce)),
            SetNonceSource::ByFolder(map) => Ok(map.get(folder).and_then(|l| l.live)),
            SetNonceSource::Resolver(resolver) => resolver.set_nonce(folder).await,
        }
    }

    /// `folder`'s nonce **lineage** — the live nonce with its minter and the
    /// set's retired nonces with theirs (`writer-signed-change-records.md`
    /// ruling (11)(b)), what a projection reader judges history under. A fixed
    /// source knows the live nonce alone; a snapshot knows what it was built
    /// with.
    ///
    /// `name_hash` is the set's address; `folder` is empty for a sealed set a
    /// projection names by hash alone, which only a resolver answers — a
    /// fixed or snapshot source keys by name.
    pub async fn lookup_lineage(
        &self,
        folder: &str,
        name_hash: &[u8; 32],
    ) -> anyhow::Result<fauna_core::folder_keys::SetNonceLineage> {
        match self {
            SetNonceSource::Resolver(resolver) => resolver.set_lineage(folder, name_hash).await,
            SetNonceSource::ByFolder(map) => Ok(map.get(folder).cloned().unwrap_or_default()),
            _ => Ok(fauna_core::folder_keys::SetNonceLineage {
                live: self.lookup(folder).await?,
                ..Default::default()
            }),
        }
    }
}

impl RecordSigning {
    /// Sign `req` for its folder, or leave it unsigned with a warning when the
    /// folder's nonce does not resolve. Public so the record requests built
    /// outside [`SyncClient::changes_record`] (the engine's cross-nest relay,
    /// the re-seed delivery) take the same funnel.
    pub async fn sign(&self, req: &mut SyncChangeRecordRequest) {
        let Some(nonce) = self.nonce_for(&req.folder, "change record").await else {
            return;
        };
        if let Err(e) = self.signer.sign_record(req, nonce) {
            tracing::warn!(folder = %req.folder, "change record left unsigned: {e}");
        }
    }

    /// Sign a resolved report's winner head row and the retained loser's row
    /// for its folder (rulings (1)(ii), (10)(d) — one
    /// `ChangeSigner::sign_report`) — an unresolved report mints no row and is
    /// left as is; an unresolvable nonce leaves it unsigned with a warning,
    /// like [`Self::sign`].
    pub async fn sign_report(&self, req: &mut ConflictReportRequest) {
        if req.resolution.is_none() {
            return;
        }
        let Some(nonce) = self.nonce_for(&req.folder, "conflict report").await else {
            return;
        };
        if let Err(e) = self.signer.sign_report(req, nonce) {
            tracing::warn!(folder = %req.folder, "conflict report left unsigned: {e}");
        }
    }

    /// Sign a choose-winner resolve of the listed `conflict` for its folder
    /// (ruling (1)(ii)); a mark-only resolve is left as is.
    pub async fn sign_choose_winner(
        &self,
        req: &mut ConflictResolveRequest,
        conflict: &folders::SyncConflict,
    ) {
        if req.winning_manifest_hash.is_none() {
            return;
        }
        let Some(nonce) = self.nonce_for(&conflict.folder, "conflict resolve").await else {
            return;
        };
        if let Err(e) = self.signer.sign_choose_winner(req, conflict, nonce) {
            tracing::warn!(folder = %conflict.folder, "conflict resolve left unsigned: {e}");
        }
    }

    /// The set nonce `folder`'s records bind to, or `None` (warned: the
    /// `what` goes out unsigned) when it does not resolve.
    async fn nonce_for(&self, folder: &str, what: &str) -> Option<[u8; 32]> {
        let nonce = match self.set_nonce.lookup(folder).await {
            Ok(nonce) => nonce,
            Err(e) => {
                tracing::warn!(%folder, "{what} left unsigned: {e}");
                return None;
            }
        };
        if nonce.is_none() {
            tracing::warn!(
                %folder,
                "{what} left unsigned: no set nonce in custody for this folder"
            );
        }
        nonce
    }
}

impl<R: RpcRequester> SyncClient<R> {
    pub fn new(nest: R) -> Self {
        Self {
            nest,
            signing: None,
        }
    }

    /// Sign every change record this client sends ([`RecordSigning`]).
    #[must_use]
    pub fn with_record_signing(mut self, signing: RecordSigning) -> Self {
        self.signing = Some(signing);
        self
    }

    /// `fauna.sync.register` — register a device for file sync under the
    /// connection actor (the engine host per folder). Capabilities default to `"read,write"` (the twin's
    /// default); the reply echoes the stored hex device id. Re-registering the
    /// same id is idempotent on the nest.
    ///
    /// `label_sealed` is the label's seal, minted by the caller through
    /// `fauna_core::label_custody::seal_device_label` — this client is generic
    /// over its transport and holds no key material, so the seal cannot be
    /// computed here. It is a **required** parameter rather than a
    /// `register_sealed` sibling on purpose: a sealless registration is a
    /// deliberate choice (a machine-authored label, or a keyless-writer seam
    /// whose row rests nameless post-flip until the device's next keyed
    /// register), and every writer should have to make it in the open.
    /// See `file-sync.md` § Sealed names & paths.
    pub async fn register(
        &self,
        device_id: impl Into<String>,
        label: impl Into<String>,
        label_sealed: Option<Vec<u8>>,
    ) -> Result<SyncRegisterReply, R::Error> {
        self.nest
            .request(
                "fauna.sync.register",
                SyncRegisterRequest {
                    device_id: device_id.into(),
                    label: label.into(),
                    label_sealed: label_sealed.map(ByteBuf::from),
                    capabilities: default_capabilities(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.sync.changes.list` — catch-up poll. With `folder`, returns that
    /// owned set's changes with `seq > since` (optionally excluding one hex
    /// `device_id`'s own echoes); without it, the connection actor's changes.
    /// Replay-safe pure read.
    pub async fn changes_list(
        &self,
        folder: Option<String>,
        device_id: Option<String>,
        since: i64,
    ) -> Result<SyncChangesListReply, R::Error> {
        self.nest
            .request(
                "fauna.sync.changes.list",
                fauna_protocol::folders::addressed(SyncChangesListRequest {
                    folder,
                    device_id,
                    since,
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.sync.changes.supersede` — mark the path's pre-head manifest rows
    /// superseded so nest GC reclaims their now-unreferenced chunks (the M2
    /// pre-bind re-seal reclaim, `mls-group-key-material.md` § M2 bullet B).
    /// `manifest_hash` is the path's current head, which the caller MUST have
    /// verified retrievable + decryptable end-to-end first — the nest refuses
    /// (head-mismatch) if it isn't the live head, so the head itself is never
    /// markable and a stale/wrong verify can't reclaim live data. Owner-only,
    /// write-capable device. Idempotent (a re-run marks 0 rows).
    pub async fn changes_supersede(
        &self,
        folder: impl Into<String>,
        device_id: impl Into<String>,
        path: impl Into<String>,
        manifest_hash: impl Into<String>,
    ) -> Result<SyncChangesSupersedeReply, R::Error> {
        self.nest
            .request(
                "fauna.sync.changes.supersede",
                fauna_protocol::folders::addressed(SyncChangesSupersedeRequest {
                    folder: folder.into(),
                    device_id: device_id.into(),
                    path: path.into(),
                    manifest_hash: manifest_hash.into(),
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.sync.backup_status` — the bearer's folders with their most
    /// recent change timestamps (the Backups page's per-set "last backed
    /// up" line). Replay-safe pure read.
    pub async fn backup_status(&self) -> Result<SyncBackupStatusReply, R::Error> {
        self.nest
            .request(
                "fauna.sync.backup_status",
                SyncBackupStatusRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.sync.status` — the sync state of one folder: its source device
    /// (+ online flag) and every destination (device/S3) with per-destination
    /// online + sync-type. The media page's "source online" indicator reads
    /// `source_online`. The WS-RPC twin of `GET /api/v1/sync/status`; the
    /// handler ownership-checks `folder` (an unowned set → `fauna.sync.not_found`
    /// / permission error). Replay-safe pure read.
    pub async fn status(&self, folder: impl Into<String>) -> Result<SyncStatusReply, R::Error> {
        self.nest
            .request(
                "fauna.sync.status",
                fauna_protocol::folders::addressed(SyncStatusRequest {
                    folder: folder.into(),
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.sync.files` — the files in one folder, each with its path,
    /// manifest hash, size, and last-update time (the media page's file list).
    /// The WS-RPC twin of `GET /api/v1/sync/files`; ownership-checked like
    /// `status`. Replay-safe pure read.
    pub async fn files(&self, folder: impl Into<String>) -> Result<SyncFilesReply, R::Error> {
        self.nest
            .request(
                "fauna.sync.files",
                fauna_protocol::folders::addressed(SyncFilesRequest {
                    folder: folder.into(),
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.files.versions.list` — the version history of one synced file,
    /// oldest→newest (`docs/goal/behavior/file-sync.md` § File Versions): a
    /// projection over the append-only `sync_changes` history, scoped to the
    /// caller's readable sets. `path_hash` is BLAKE3 of the normalized
    /// forward-slash folder-relative path (derive it in shared Rust, never
    /// per-app); `folder` narrows to one named set — pass it whenever the
    /// caller knows the set, since a bare `path_hash` can collide across sets.
    /// Replay-safe pure read.
    pub async fn versions_list(
        &self,
        path_hash: [u8; 32],
        folder: Option<String>,
    ) -> Result<FilesVersionsListReply, R::Error> {
        self.versions_list_with(path_hash, folder, false).await
    }

    /// [`Self::versions_list_with`] scoped to `folder`, every version paired
    /// with the one shared judge's verdict over its row
    /// ([`FileVersionInfo::as_change_row`](fauna_protocol::files::FileVersionInfo::as_change_row),
    /// the reply's `signer_certs`) — writer-signed change records, ruling (3).
    /// Every version-history reader lists — and restores — through this, so a
    /// version that does not verify is never offered: the list keeps
    /// [`RowVerdict::admits`](fauna_protocol::sync_row_verify::RowVerdict::admits),
    /// and a restore refuses the rest. The verdict comes from the LIST because
    /// only the list reply carries the certs a delegated signer's row chains
    /// through (`fauna.files.versions.get` answers a bare version).
    pub async fn versions_list_judged(
        &self,
        path_hash: [u8; 32],
        folder: &str,
        include_pruned: bool,
        seat: &row_judge::ReaderSeat,
    ) -> Result<
        Vec<(
            fauna_protocol::files::FileVersionInfo,
            fauna_protocol::sync_row_verify::RowVerdict,
        )>,
        R::Error,
    >
    where
        R::Error: RpcErrorClass + core::fmt::Display,
    {
        let reply = self
            .versions_list_with(path_hash, Some(folder.to_string()), include_pruned)
            .await?;
        let rows: Vec<_> = reply
            .versions
            .iter()
            .map(|v| row_judge::ProjectedRow {
                folder,
                folder_hash: None,
                row: v.as_change_row(),
            })
            .collect();
        let verdicts = seat
            .judge(&self.nest)
            .judge(&rows, &reply.signer_certs)
            .await?;
        let mut versions = reply.versions;
        for (version, verdict) in versions.iter_mut().zip(&verdicts) {
            // Ruling (8)(c), as on a Media item: the version is attributed to
            // the verdict's WRITER, and carries whether it was signed as this
            // seat's current identity — what a restore branches on.
            if let fauna_protocol::sync_row_verify::RowVerdict::Verified { writer, .. }
            | fauna_protocol::sync_row_verify::RowVerdict::History { writer, .. } = verdict
            {
                version.author_actor_id = hex::encode(writer);
            }
            version.signed_as_current = verdict.signed_as(seat.own.as_ref());
            version.signed_as = crate::row_judge::verified_signer(verdict);
        }
        Ok(versions.into_iter().zip(verdicts).collect())
    }

    /// `fauna.files.versions.list` with `include_pruned` — `true` also returns
    /// soft-pruned rows (each carrying `pruned` + `purge_after`), the recovery
    /// browse of `file-versions.md` § Retention (3); `false` = the historic
    /// live-only listing. Replay-safe pure read.
    pub async fn versions_list_with(
        &self,
        path_hash: [u8; 32],
        folder: Option<String>,
        include_pruned: bool,
    ) -> Result<FilesVersionsListReply, R::Error> {
        self.nest
            .request(
                "fauna.files.versions.list",
                fauna_protocol::folders::addressed(FilesVersionsListRequest {
                    path_hash: ByteBuf::from(path_hash.to_vec()),
                    folder,
                    include_pruned: include_pruned.then_some(true),
                    ..Default::default()
                }),
            )
            .await
    }

    /// `fauna.files.versions.undelete` — restore a soft-pruned version to the
    /// listable population (`file-versions.md` § Retention (3), the
    /// snapshot-undelete twin on the version plane). Owner-scoped; a version
    /// that is not currently soft-pruned answers `fauna.files.not_found`.
    pub async fn versions_undelete(
        &self,
        path_hash: [u8; 32],
        version_num: i64,
    ) -> Result<FilesVersionsUndeleteReply, R::Error> {
        self.nest
            .request(
                "fauna.files.versions.undelete",
                FilesVersionsUndeleteRequest {
                    path_hash: ByteBuf::from(path_hash.to_vec()),
                    version_num,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.files.versions.get` — one version's metadata by `(path_hash,
    /// version_num)`, where `version_num` is the version's stable `seq` (a
    /// `versions_list` item's `version_num`). Missing or outside the caller's
    /// readable sets → the `fauna.files.not_found` rejection. Replay-safe pure
    /// read.
    pub async fn versions_get(
        &self,
        path_hash: [u8; 32],
        version_num: i64,
    ) -> Result<FileVersionInfo, R::Error> {
        self.nest
            .request(
                "fauna.files.versions.get",
                FilesVersionsGetRequest {
                    path_hash: ByteBuf::from(path_hash.to_vec()),
                    version_num,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.sync.conflicts.list` — the bearer's unresolved sync conflicts,
    /// each with its candidate versions (`SyncConflict.candidates`) the user
    /// may choose between. The Devices/Peers conflict surface reads from here.
    /// Replay-safe pure read. See `docs/goal/behavior/file-sync.md` § Conflicts.
    pub async fn conflicts_list(&self) -> Result<ConflictsListReply, R::Error> {
        self.conflicts_list_with(false).await
    }

    /// `fauna.sync.conflicts.list` with `include_resolved` — `true` also
    /// returns resolved rows (the auto-resolve review list, file-sync.md
    /// § Conflicts ratified 2026-07-10); `false` = the historic
    /// unresolved-only contract. Replay-safe pure read.
    pub async fn conflicts_list_with(
        &self,
        include_resolved: bool,
    ) -> Result<ConflictsListReply, R::Error> {
        self.nest
            .request(
                "fauna.sync.conflicts.list",
                ConflictsListRequest {
                    include_resolved: include_resolved.then_some(true),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.sync.conflicts.resolve` — resolve conflict `id`, **unsigned**.
    /// `None` is the candidate-free (mark-only) resolve (flips `resolved_at`,
    /// propagates nothing) — the one resolve that signs nothing. `Some(hash)`
    /// sends a choose-winner with no signature, which a nest enforcing writer
    /// signatures refuses: a choose-winner the device signs goes through
    /// [`Self::conflicts_resolve_judged`], never off the listed row alone
    /// (`writer-signed-change-records.md` ruling (10)(f)). An unknown /
    /// already-resolved conflict returns `fauna.sync.not_found`.
    pub async fn conflicts_resolve(
        &self,
        id: i64,
        winning_manifest_hash: Option<String>,
    ) -> Result<ConflictResolveReply, R::Error> {
        self.nest
            .request(
                "fauna.sync.conflicts.resolve",
                ConflictResolveRequest {
                    id,
                    winning_manifest_hash,
                    ..Default::default()
                },
            )
            .await
    }

    /// `fauna.sync.conflicts.resolve` choose-winner of `conflict` (as
    /// `conflicts.list` served it), keeping the version `vouched` names —
    /// signed when this client holds a [`RecordSigning`] (ruling (1)(ii)).
    ///
    /// The conflict row is the nest's word, so the head it mints is signed
    /// only over a version the judged history vouches for (ruling (10)(f)):
    /// [`restore_branch::JudgedCandidate::vouches_for_winner`] must hold —
    /// the decision verbatim, the row's path the version's, and the candidate
    /// the nest mints from carrying the version's signed device, size and
    /// generation. Otherwise nothing is sent
    /// ([`ChooseWinnerError::NotVouched`]).
    pub async fn conflicts_resolve_judged(
        &self,
        conflict: &folders::SyncConflict,
        vouched: &restore_branch::JudgedCandidate,
    ) -> Result<ConflictResolveReply, ChooseWinnerError<R::Error>> {
        if !vouched.vouches_for_winner(conflict) {
            return Err(ChooseWinnerError::NotVouched);
        }
        let mut req = ConflictResolveRequest {
            id: conflict.id,
            winning_manifest_hash: Some(hex::encode(&vouched.version.manifest_hash)),
            ..Default::default()
        };
        if let Some(signing) = &self.signing {
            signing.sign_choose_winner(&mut req, conflict).await;
        }
        self.nest
            .request("fauna.sync.conflicts.resolve", req)
            .await
            .map_err(ChooseWinnerError::Rpc)
    }

    /// `fauna.sync.conflicts.report` — record a conflict the engine detected on
    /// `path`, with its `[local, incoming]` candidate versions (each a
    /// `{manifest_hash, device_id, size_bytes, created_at}`). The nest stores the
    /// candidates so `conflicts.list` returns them and the user can pick a winner.
    /// The engine reports through here (the candidate-free legacy HTTP twin was
    /// removed in the Track-C rip). See `docs/goal/behavior/file-sync.md` § Conflicts.
    #[allow(clippy::too_many_arguments)]
    pub async fn conflicts_report(
        &self,
        folder: impl Into<String>,
        device_id: impl Into<String>,
        path: impl Into<String>,
        conflict_type: impl Into<String>,
        details: Option<String>,
        candidates: Vec<ConflictCandidate>,
        labels: ConflictLabels,
    ) -> Result<ConflictReportReply, R::Error> {
        self.conflicts_report_with(
            folder,
            device_id,
            path,
            conflict_type,
            details,
            candidates,
            None,
            labels,
        )
        .await
    }

    /// `fauna.sync.conflicts.report` with an optional pre-resolved
    /// [`ResolvedOutcome`] (auto-resolve, ratified 2026-07-10): the conflict
    /// lands already resolved on the nest — which transactionally retains the
    /// losing version and propagates the winner as the new head — and feeds
    /// the review list instead of a blocking chooser.
    #[allow(clippy::too_many_arguments)]
    pub async fn conflicts_report_with(
        &self,
        folder: impl Into<String>,
        device_id: impl Into<String>,
        path: impl Into<String>,
        conflict_type: impl Into<String>,
        details: Option<String>,
        candidates: Vec<ConflictCandidate>,
        resolution: Option<ResolvedOutcome>,
        labels: ConflictLabels,
    ) -> Result<ConflictReportReply, R::Error> {
        let (
            resolution,
            winning_manifest_hash,
            winning_size_bytes,
            winning_content_key_version,
            winning_derived_through,
            losing_derived_through,
            winning_carries_novelty,
        ) = match resolution {
            Some(o) => (
                Some(o.resolution),
                Some(o.winning_manifest_hash),
                o.winning_size_bytes,
                o.winning_content_key_version,
                o.winning_derived_through,
                o.losing_derived_through,
                o.winning_carries_novelty,
            ),
            None => (None, None, None, None, None, None, None),
        };
        let path = path.into();
        let ConflictLabels {
            path_hash,
            path_sealed,
            details_sealed,
        } = labels;
        let mut req = ConflictReportRequest {
            folder: folder.into(),
            device_id: device_id.into(),
            path,
            conflict_type: conflict_type.into(),
            details,
            candidates,
            resolution,
            winning_manifest_hash,
            winning_size_bytes,
            winning_content_key_version,
            winning_derived_through,
            losing_derived_through,
            winning_carries_novelty,
            path_hash,
            path_sealed,
            details_sealed,
            ..Default::default()
        };
        if let Some(signing) = &self.signing {
            signing.sign_report(&mut req).await;
        }
        self.nest.request("fauna.sync.conflicts.report", req).await
    }

    /// `fauna.sync.devices.list` — every device the bearer actor has registered
    /// with fauna-sync, each with its capabilities, online flag, and per-folder
    /// roles. The page-level all-devices list the Devices page renders (distinct
    /// from `fauna.folders.devices`, which is per-set). The WS-RPC twin of
    /// `GET /api/v1/sync/devices`. Replay-safe pure read.
    pub async fn devices_list(&self) -> Result<SyncDevicesListReply, R::Error> {
        self.nest
            .request(
                "fauna.sync.devices.list",
                SyncDevicesListRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.sync.devices.delete` — unregister a device (the Devices page's
    /// `device-remove-button`), removing it from every folder membership. The
    /// WS-RPC twin of `DELETE /api/v1/sync/devices/{id}`. An unknown device
    /// returns `fauna.sync.not_found`.
    pub async fn devices_delete(
        &self,
        device_id: impl Into<String>,
    ) -> Result<SyncDeviceDeleteReply, R::Error> {
        self.nest
            .request(
                "fauna.sync.devices.delete",
                SyncDeviceDeleteRequest {
                    device_id: device_id.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.sync.devices.p2p_participation.set` — the devices page's
    /// `device-p2p-participation-toggle` (`p2p.md` § Per-device
    /// participation). Build the **self arm** (this device reporting its own
    /// state) with [`build_p2p_participation_report`]; the **owner arm** is
    /// the bare `{device_id, participating: false}` and can only ask another
    /// device to turn its listeners off. A failed
    /// report (any error, `unknown_kind` included) is ordinary; the device-local state
    /// is the authority either way, so a failed report costs only what a
    /// sibling's card paints.
    pub async fn devices_p2p_participation_set(
        &self,
        req: SyncDeviceP2pParticipationSetRequest,
    ) -> Result<SyncDeviceP2pParticipationSetReply, R::Error> {
        self.nest
            .request(
                fauna_protocol::sync::KIND_SYNC_DEVICES_P2P_PARTICIPATION_SET,
                req,
            )
            .await
    }

    /// `fauna.sync.device_grant.register` — store a root-key-signed,
    /// `RenewBearer`-scoped `DeviceAuthorization` on the actor's `device_id`
    /// row, enabling the sync agent's app-dead bearer renewal over
    /// `fauna.auth.device_handshake` (`sync-agent.md` § Credential model;
    /// additive 2026-07-19). The enrollment ceremony is the one production
    /// caller, registering the store principal's grant
    /// ([`build_principal_grant`]); the device must be registered first
    /// (`fauna.sync.register` → else `fauna.sync.not_found`). A failed
    /// registration is an ordinary error — callers
    /// degrade to app-pushed `RefreshBearer` only. Idempotent per-row UPDATE.
    pub async fn device_grant_register(
        &self,
        device_id: impl Into<String>,
        authorization: fauna_core::encoding::EmbedAsBytes,
    ) -> Result<DeviceGrantRegisterReply, R::Error> {
        self.nest
            .request(
                "fauna.sync.device_grant.register",
                DeviceGrantRegisterRequest {
                    device_id: device_id.into(),
                    authorization,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.sync.device_grant.revoke` — retire ONE renewal grant, named by
    /// its device public key, leaving the device row itself (`sync-agent.md`
    /// § Credential model → the RULED 2026-08-15 block; additive 2026-08-15).
    ///
    /// The live caller is the sign-out retirement of the machine's store
    /// principal (`shutdown_for_sign_out`), which proves possession of the key
    /// (build `req` with [`build_grant_revoke_request`]); the kind also accepts
    /// a request authorized by the account session alone.
    ///
    /// On any failure (an ordinary error) the ruled degradation
    /// is to KEEP the local credential and retry on the
    /// next renewal, never to delete it locally on a failure the nest never saw.
    /// A `revoked: false` answer is success (nothing was left to clear).
    pub async fn device_grant_revoke(
        &self,
        req: DeviceGrantRevokeRequest,
    ) -> Result<DeviceGrantRevokeReply, R::Error> {
        self.nest
            .request("fauna.sync.device_grant.revoke", req)
            .await
    }
}

/// Build the self-arm `fauna.sync.device_grant.revoke` request: prove
/// possession of the key being retired by signing
/// `DEVICE_GRANT_REVOKE_V1 ‖ actor_id ‖ device_key ‖ timestamp_be ‖ nonce`
/// with it (`sync-agent.md` § Credential model → the RULED 2026-08-15 block,
/// decision 2).
///
/// Possession of this key already *is* the authority the grant confers — it
/// mints bearers — so proving it authorizes nothing new: the request can only
/// destroy the caller's own credential. That is the whole reason a bearer-only
/// process may run this at all.
///
/// The nonce is fresh per request because the signature is deterministic:
/// without it, two retirement attempts for the same key in the same
/// millisecond would produce identical bytes and the nest's single-use replay
/// guard would refuse the second — the same reason the handshake carries one.
pub fn build_grant_revoke_request(
    actor_id: &[u8; 32],
    device_key: &ed25519_dalek::SigningKey,
) -> DeviceGrantRevokeRequest {
    use ed25519_dalek::Signer;

    let device_key_bytes = device_key.verifying_key().to_bytes();
    let timestamp_ms = fauna_core::data::Timestamp::now_millis();
    let nonce_hex = fauna_core::identity::random_hex(32);
    // Infallible: `random_hex` emits lowercase hex by construction.
    let nonce = hex::decode(&nonce_hex).unwrap_or_default();
    let msg = fauna_protocol::auth::device_grant_revoke_signed_message(
        actor_id,
        &device_key_bytes,
        timestamp_ms,
        &nonce,
    );
    let signature = device_key.sign(&msg);
    DeviceGrantRevokeRequest {
        device_key: fauna_core::hex32::encode(&device_key_bytes),
        timestamp_ms: Some(timestamp_ms),
        nonce: Some(nonce_hex),
        signature: Some(hex::encode(signature.to_bytes())),
        extra: Default::default(),
    }
}

/// Build the self-arm `fauna.sync.devices.p2p_participation.set` request: this
/// device's own report of whether it runs its peer listeners, proven by the
/// row's principal — the T10 writer key the enrollment granted on
/// `device_id` (`p2p.md` § Per-device participation). Nonce-fresh per
/// request for the same reason [`build_grant_revoke_request`] is.
pub fn build_p2p_participation_report(
    actor_id: &[u8; 32],
    device_key: &ed25519_dalek::SigningKey,
    device_id: &[u8; 32],
    participating: bool,
) -> SyncDeviceP2pParticipationSetRequest {
    use ed25519_dalek::Signer;

    let device_key_bytes = device_key.verifying_key().to_bytes();
    let timestamp_ms = fauna_core::data::Timestamp::now_millis();
    let nonce_hex = fauna_core::identity::random_hex(32);
    let nonce = hex::decode(&nonce_hex).unwrap_or_default();
    let msg = fauna_protocol::auth::device_p2p_participation_signed_message(
        actor_id,
        &device_key_bytes,
        device_id,
        participating,
        timestamp_ms,
        &nonce,
    );
    let signature = device_key.sign(&msg);
    SyncDeviceP2pParticipationSetRequest {
        device_id: fauna_core::hex32::encode(device_id),
        participating,
        timestamp_ms: Some(timestamp_ms),
        nonce: Some(nonce_hex),
        signature: Some(hex::encode(signature.to_bytes())),
        extra: Default::default(),
    }
}

/// Mint the **store principal's** enrollment grant (the machine's
/// model: `sync-agent.md` § Credential model, the store-principal convergence bullet;
/// `account-data-plane.md` § The store device principal): sign a
/// `DeviceAuthorization { capabilities: [RenewBearer, SyncWrite], expires_at: None }`
/// with the root key **over the store's writer public key** — the principal
/// IS the writer key (T10), so no fresh keypair is minted and no secret is
/// returned (the writer key already lives
/// in the T10 credential slot). The caller persists the wire in the slot
/// (`PrincipalSlot::store_device_authorization`, which refuses a grant over
/// any other key) and registers it via
/// [`SyncClient::device_grant_register`] under the writer-pub-hex device id —
/// one `sync_devices` row per (machine, account).
///
/// No expiry by design: an expiring grant would silently stop app-dead sync;
/// revocation (device-row deletion) is the control.
///
/// `SyncWrite` because the principal writer key is the key every host on the
/// machine signs file-sync change records with (`mls-group-key-material.md`
/// § M2 → *Writer-signed change records* (1)); a slot still carrying a
/// `[RenewBearer]`-only grant is re-certified under the SAME key at the next
/// seed-holding assembly ([`principal_grant_is_current`]).
pub fn build_principal_grant(
    identity: &fauna_core::identity::ActorKeypair,
    writer_pub: &[u8; 32],
) -> Result<fauna_core::encoding::EmbedAsBytes, fauna_core::error::Error> {
    let auth = fauna_core::data::DeviceAuthorization {
        actor_id: identity.actor_id(),
        device_key: *writer_pub,
        capabilities: PRINCIPAL_GRANT_CAPABILITIES.to_vec(),
        created_at: fauna_core::data::Timestamp::now(),
        expires_at: None,
    };
    let (bytes, env) = fauna_core::encoding::sign_envelope(identity, &auth)?;
    Ok(fauna_core::encoding::EmbedAsBytes::from_signed(bytes, env))
}

/// The capabilities [`build_principal_grant`] mints — the one list the
/// ceremony and its heal check share.
pub const PRINCIPAL_GRANT_CAPABILITIES: [fauna_core::data::Capability; 2] = [
    fauna_core::data::Capability::RenewBearer,
    fauna_core::data::Capability::SyncWrite,
];

/// Whether a stored principal grant already conveys everything
/// [`build_principal_grant`] mints today — `false` is the ceremony's heal
/// signal (re-mint under the same key at the next seed-holding assembly).
pub fn principal_grant_is_current(auth: &fauna_core::data::DeviceAuthorization) -> bool {
    PRINCIPAL_GRANT_CAPABILITIES
        .iter()
        .all(|required| auth.capabilities.iter().any(|c| c.grants(required)))
}

/// Name the machine's `sync_devices` row: `fauna.sync.register` under the
/// app's derived device id with the user-visible sealed label — the
/// provisioner's one nest leg (`sync-agent-credentials.md` § Credential model
/// → the RULED 2026-09-28 block, decision 2). It mints **no** credential: the
/// store principal, registered on this same row by the enrollment ceremony, is
/// the machine's only renewal credential. A register never touches the grant
/// columns and a grant register never touches the label, so the two passes
/// may land in either order and the row ends up named and carrying the
/// principal.
///
/// Reached through **one** caller, `agent::SyncAgentProvisioner::start`, which
/// every desktop host arrives at — the native ones over `fauna-ffi`'s
/// `FfiSyncAgentProvisioner::start` (priority #2).
///
/// Best-effort: a failure is logged and returns `false`. The enrollment's own
/// register-create (under [`SELF_REGISTER_LABEL`]) still makes the row exist,
/// and the next provision names it.
pub async fn register_this_machine<R>(
    nest: R,
    identity: &fauna_core::identity::ActorKeypair,
    device_id: String,
    device_label: String,
) -> bool
where
    R: RpcRequester + Send + Sync,
    R::Error: std::fmt::Display,
{
    let sync = SyncClient::new(nest);
    // The seal is the registering owner's root by the shortest available
    // route: `identity` is exactly the material `BackupKey::derive` takes, the
    // same construction the engine's owner-key arm uses (`file-sync.md`
    // § Sealed names & paths).
    let owner_key = fauna_core::crypto::BackupKey::derive(identity.secret_bytes());
    let label_seal_root = fauna_core::path_crypto::LabelRoot::owner_of(&owner_key);
    let label_sealed = device_label_salt(&device_id).and_then(|salt| {
        fauna_core::label_custody::seal_device_label(&label_seal_root, &salt, &device_label)
            .unwrap_or_else(|e| {
                // Registration is best-effort, so a seal failure must not abort
                // it either — register sealless (the row rests nameless
                // post-flip) and let the next successful keyed register
                // re-stamp it.
                tracing::warn!("sealing the device label failed, registering plaintext-only: {e}");
                None
            })
    });
    match sync.register(device_id, device_label, label_sealed).await {
        Ok(_) => true,
        Err(e) => {
            // A tier device-cap refusal lands here too (`devices.md` § Step 4);
            // this helper's `R::Error` is only `Display`, so it is not
            // classified here — the account runtime's enrollment pass meets the
            // same refusal for the same device and records it for the Devices
            // page (`EnrollmentRefusal`), which is where the user learns the
            // remedy.
            tracing::warn!(
                "sync.register failed; the machine's row stays unnamed until the next provision: {e}"
            );
            false
        }
    }
}

/// The 32-byte device id a device-label seal salts under, decoded from the hex
/// form every `fauna.sync.register` caller carries.
///
/// `None` for a device id that is not 32 bytes of hex — the label then registers
/// plaintext-only rather than sealing under a salt no reader could reproduce.
/// Unreachable in practice (every producer hex-encodes a `[u8; 32]`), which is
/// why it degrades rather than erroring.
fn device_label_salt(device_id_hex: &str) -> Option<[u8; 32]> {
    fauna_core::hex32::decode(device_id_hex).ok()
}

/// Placeholder device label for a self-heal registration. Only ever used the first
/// time an otherwise-unregistered device records a change; the device's
/// *authoritative* label is set by its real registration (location-map / engine),
/// whose upsert supersedes this.
///
/// Re-exported from `fauna-core`, which owns it alongside the other two
/// machine-authored labels so `label_custody::is_synthetic_device_label` can
/// refuse all three in one place (`file-sync.md` § Sealed names & paths →
/// *Deliberate non-seals*).
pub use fauna_core::label_custody::SELF_REGISTER_LABEL;

/// True when `e` is the nest's dedicated `fauna.sync.device_unregistered`
/// rejection. Keyed on the exact code so it never fires for a generic
/// `permission_denied` (unknown/revoked actor, kind not permitted, or a *read-only*
/// device, which must stay read-only rather than be silently upgraded to write).
fn is_device_unregistered<E: RpcErrorClass>(e: &E) -> bool {
    e.as_rpc_error()
        .is_some_and(|rpc| rpc.code == "fauna.sync.device_unregistered")
}

/// The `fauna.sync.device_grant.register` refusal that means **the row is not
/// there yet** — register it first, then retry the grant. Keyed on the exact
/// code, which that kind returns for this one reason.
///
/// The code is the *generic* `fauna.sync.not_found` rather than a dedicated
/// one, deliberately: a newer client must keep working against an older nest
/// (`version-compatibility.md`), and giving this answer a new code would have
/// left the enrollment pass unable to recognise it there — the machine would
/// retry forever instead of registering. Context disambiguates: this is only
/// ever consulted on a `device_grant.register` reply, where
/// [`is_access_revoked`]'s reading of the same code (a demoted writer on
/// `changes.record`) cannot arise.
pub fn is_device_grant_no_device<E: RpcErrorClass>(e: &E) -> bool {
    e.as_rpc_error()
        .is_some_and(|rpc| rpc.code == "fauna.sync.not_found")
}

/// The `fauna.sync.device_grant.register` refusal that **no retry can ever
/// clear**: the nest's revocation memory says this device key was tombstoned by
/// a device deletion, so the machine has been removed from the account and only
/// a successor principal revives it (`sync-agent.md` § Credential model →
/// RULED 2026-08-15, decision 4).
///
/// Its own code precisely because the alternative — three retryable
/// `invalid_grant` malformations sharing one code with a permanent refusal — is
/// how a removal becomes a silent latched death.
///
/// This same answer is the **evidence gate of principal succession**
/// (`account-data-plane.md` § The store device principal → *Principal
/// succession after a device delete*, decision 1): a ceremony-capable sign-in
/// seeing it mints a successor principal. The code symbol is
/// [`fauna_protocol::RpcError::CODE_SYNC_DEVICE_GRANT_REVOKED`], shared with
/// the emitter.
pub fn is_device_grant_revoked<E: RpcErrorClass>(e: &E) -> bool {
    e.as_rpc_error()
        .is_some_and(|rpc| rpc.code == fauna_protocol::RpcError::CODE_SYNC_DEVICE_GRANT_REVOKED)
}

/// The `fauna.sync.register` refusal that means **the account is at its tier's
/// device cap** (`devices.md` § Step 4): the nest refused a new `device_id` and
/// wrote nothing. It clears only when a slot frees, so a caller names it with
/// that remedy (remove a device, or ask the admin for a bigger tier) rather
/// than reading it as an offline nest. The code symbol is
/// [`fauna_protocol::RpcError::CODE_SYNC_DEVICE_LIMIT_EXCEEDED`], shared with
/// the emitter.
pub fn is_device_limit_exceeded<E: RpcErrorClass>(e: &E) -> bool {
    e.as_rpc_error()
        .is_some_and(|rpc| rpc.code == fauna_protocol::RpcError::CODE_SYNC_DEVICE_LIMIT_EXCEEDED)
}

/// The **write-grant revocation** codes: the authoritative nest has refused this
/// caller's write on a set it holds a folder binding for, and no retry will
/// change that (`file-sync.md` § Multi-writer shared sets — D4, fail-closed AND
/// loud). The engine treats exactly these as terminal and parks
/// (`fauna_sync_engine::access_gate`); everything else stays retryable.
///
/// Two codes because the two planes fold a demotion differently, and both are
/// the *authoritative* nest's answer — never the client's advisory `access`:
///
/// - `fauna.federation.forbidden` — cross-nest. The home nest's
///   `require_foreign_writer` refuses the `write_token.mint` or the
///   `folder.changes.record` relay, and the own nest passes the peer's typed
///   error through untouched (`rpc_errors::map_peer_relay_error`).
/// - `fauna.sync.not_found` — same-nest. `resolve_writable_folder` folds
///   "demoted to reader" into the same `not_found` a stranger gets (ST-RES-1:
///   a reader probing the write plane learns nothing). The other same-nest
///   readings of that code on `changes.record` — the set was deleted or renamed
///   — want the *identical* park: the binding can no longer write, so parking
///   fail-closed and telling the user is right for all three.
///
/// Deliberately NOT included: `fauna.federation.peer_nest_outdated` (a version
/// gap — NeedsUpdate, and the grant may be perfectly valid),
/// `fauna.sync.device_unregistered` (self-heals — [`is_device_unregistered`]),
/// `permission_denied` (the connection-level device-capability refusal, not a
/// set grant), and every transport fault (`as_rpc_error` is `None` — the request
/// never reached a nest, so it asserts nothing about the grant).
pub fn is_access_revoked<E: RpcErrorClass>(e: &E) -> bool {
    e.as_rpc_error().is_some_and(|rpc| {
        rpc.code == "fauna.federation.forbidden" || rpc.code == "fauna.sync.not_found"
    })
}

/// Write paths that must survive a not-yet-registered sync device.
///
/// Split from the main `impl` by the extra `R::Error: RpcErrorClass` bound — the
/// pure kind-composition calls above need no error classification.
impl<R: RpcRequester> SyncClient<R>
where
    R::Error: RpcErrorClass,
{
    /// `fauna.sync.changes.record` — record a file change against an owned set;
    /// returns the assigned monotonic `seq`. Pass `manifest_hash = None` for a
    /// delete. `content_key_version` is the M2 content-key generation the chunks
    /// were sealed under (`None` for owner-only sets / deletes). `thumbnail_hash`
    /// is the uploader's hex thumbnail-blob pointer (`None` until a producer
    /// supplies one); the nest stores it opaque and surfaces it via
    /// `fauna.media.list`.
    ///
    /// `path_sealed` is the canonical dag-cbor
    /// `fauna_core::path_crypto::SealedLabel` covering `path`, computed by the
    /// caller **under the root that already seals the set's chunks**
    /// (`docs/goal/behavior/file-sync.md` § Sealed names & paths). This crate
    /// deliberately does not compute it: the seal root lives with the writer,
    /// and the one funnel that owns the derivation is
    /// `fauna_sync_engine::SyncEngine::record_change`. `None` = this writer holds
    /// no seal root, which records plaintext-only exactly as before the expand
    /// phase.
    ///
    /// **Self-healing by construction**: on the nest's dedicated
    /// `fauna.sync.device_unregistered` rejection, register write-capable under
    /// [`SELF_REGISTER_LABEL`] and retry **once**. A folder-less control-plane
    /// client (the web SPA, the Windows sync helper on a fresh profile) may never
    /// have registered a device, yet legitimately records changes. The heal is
    /// deliberately NOT bypassable — this is the only public record entry. It used
    /// to be an opt-in sibling (`changes_record_self_healing`); the sync engine's
    /// record path called the raw variant and every upload's record on an
    /// unregistered device died un-healed (live 2026-07-17: files stuck
    /// sync-pending forever, empty version history). A *read-only* device is never
    /// silently upgraded — only the exact `device_unregistered` code heals (see
    /// [`is_device_unregistered`]).
    /// `derived_through` / `is_resolution` are the causal-watermark pair
    /// (`SyncChangeRecordRequest::derived_through` has the contract). Writers
    /// that do not track an anchor pass `None` — honestly "unknown causality",
    /// which readers read as no anchor.
    #[allow(clippy::too_many_arguments)]
    pub async fn changes_record(
        &self,
        folder: impl Into<String>,
        device_id: impl Into<String>,
        path: impl Into<String>,
        manifest_hash: Option<String>,
        size_bytes: i64,
        change_type: impl Into<String>,
        content_key_version: Option<u64>,
        thumbnail_hash: Option<String>,
        path_sealed: Option<Vec<u8>>,
        derived_through: Option<i64>,
        is_resolution: Option<bool>,
    ) -> Result<SyncChangeRecordReply, R::Error> {
        let mut req = SyncChangeRecordRequest {
            nest_url: None,
            channel_id: None,
            folder: folder.into(),
            device_id: device_id.into(),
            path: path.into(),
            manifest_hash,
            size_bytes,
            change_type: change_type.into(),
            content_key_version,
            thumbnail_hash,
            path_sealed: path_sealed.map(ByteBuf::from),
            name_hash: None,
            derived_through,
            is_resolution,
            extra: Default::default(),
            signature: None,
            signer_key: None,
            signer_cert: None,
        };
        // Signed BEFORE the address funnel: the set's nonce resolves by the
        // set's name, which `addressed` takes off the request. The signature
        // binds no address field, so addressing after it changes nothing it
        // covers.
        if let Some(signing) = &self.signing {
            signing.sign(&mut req).await;
        }
        let req = fauna_protocol::folders::addressed(req);
        match self
            .nest
            .request("fauna.sync.changes.record", req.clone())
            .await
        {
            Ok(reply) => Ok(reply),
            Err(e) if is_device_unregistered(&e) => {
                // Machine-authored placeholder — no seal, and the funnel would
                // refuse one anyway (`is_synthetic_device_label`). The real
                // registration that supersedes this row carries the seal.
                self.register(req.device_id.clone(), SELF_REGISTER_LABEL, None)
                    .await?;
                self.nest.request("fauna.sync.changes.record", req).await
            }
            Err(e) => Err(e),
        }
    }

    /// **Restore version N of a file** — `docs/goal/behavior/file-sync.md` § Restore.
    ///
    /// Restore is *re-point, never re-upload*: an ordinary `modify` change carrying
    /// the historical version's `manifest_hash`, `size_bytes`, and — the sealed-set
    /// edge — its historical `content_key_version` **verbatim**, so readers select
    /// `key_for(that generation)` rather than the current one. No byte moves; the
    /// record becomes the new head *and* a new version, so restore is reversible.
    ///
    /// Returns the recording row's `seq` (the restore's own `version_num`).
    ///
    /// ⚠ **This propagates to *other* devices only.** Catch-up skips a device's own
    /// changes, so a caller that also holds the file locally must re-point its own
    /// copy as an explicit second step (§ Restore, *the recording device must
    /// re-point its own local copy*). Callers whose surface has no local file (the
    /// nest-side Media library) have nothing to do.
    ///
    /// `path_sealed` rides through unchanged — a restore records an ordinary
    /// `modify`, so it seals its path exactly as any other record does (see
    /// [`Self::changes_record`]). A caller holding no seal root passes `None`
    /// and records plaintext-only, as before the expand phase.
    #[allow(clippy::too_many_arguments)]
    pub async fn restore_version(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<SyncChangeRecordReply, R::Error> {
        self.changes_record(
            folder,
            device_id,
            path,
            Some(manifest_hash),
            size_bytes,
            "modify",
            content_key_version,
            None,
            path_sealed,
            // A restore is a deliberate USER action re-pointing the head — a
            // fresh edit semantically, never a resolution; this crate holds no
            // catch-up anchor, so causality is honestly unknown.
            None,
            None,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = SyncClient::new(MockRequester);
    }

    // ── restore_version + self-healing record ───────────────────────────────

    /// Rejects the first `changes.record` with the nest's dedicated
    /// `fauna.sync.device_unregistered` code, then lets everything through —
    /// the folder-less-client gap `changes_record`'s built-in self-heal covers.
    #[derive(Default)]
    struct UnregisteredOnceRequester {
        calls: std::sync::Mutex<Vec<(&'static str, Vec<u8>)>>,
        rejected_once: std::sync::atomic::AtomicBool,
    }

    #[derive(Debug)]
    struct RejectErr(fauna_protocol::RpcError);

    impl core::fmt::Display for RejectErr {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "{}", self.0.code)
        }
    }

    impl RpcErrorClass for RejectErr {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&fauna_protocol::RpcError> {
            Some(&self.0)
        }
    }

    impl RpcRequester for UnregisteredOnceRequester {
        type Error = RejectErr;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            self.calls.lock().unwrap().push((kind, bytes.to_vec()));

            if kind == "fauna.sync.changes.record"
                && !self
                    .rejected_once
                    .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                return Err(RejectErr(fauna_protocol::RpcError::new(
                    "fauna.sync.device_unregistered",
                    "device_unregistered",
                )));
            }

            let reply = match kind {
                "fauna.sync.register" => fauna_protocol::encode_canonical(&SyncRegisterReply {
                    device_id: "dev-1".into(),
                    extra: Default::default(),
                }),
                "fauna.sync.changes.record" => {
                    fauna_protocol::encode_canonical(&SyncChangeRecordReply {
                        seq: 42,
                        extra: Default::default(),
                    })
                }
                other => panic!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    #[test]
    fn restore_version_records_an_ordinary_modify_carrying_the_historical_generation() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());

        block_on(client.restore_version(
            "docs",
            "dev-1",
            "docs/report.txt",
            "aabb".to_string(),
            100,
            Some(5),
            None,
        ))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.changes.record");
        let req: SyncChangeRecordRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");

        // Restore is re-point, never re-upload: an ordinary `modify` re-pointing the
        // historical manifest, size, and — the sealed-set edge — generation verbatim.
        assert_eq!(req.change_type, "modify");
        assert_eq!(req.manifest_hash.as_deref(), Some("aabb"));
        assert_eq!(req.size_bytes, 100);
        assert_eq!(req.content_key_version, Some(5));
        assert_eq!(req.thumbnail_hash, None);
        assert_eq!(req.path, "docs/report.txt");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "docs"
        ));
    }

    /// The self-heal must live IN `changes_record` itself, not beside it as an
    /// opt-in sibling: `SyncEngine::record_change` called the raw variant and
    /// every windows upload's record died `device_unregistered` with no heal
    /// (live 2026-07-17: files stuck sync-pending forever, empty version
    /// history). One public record entry, healing by default, means no caller
    /// can repeat that bug.
    #[test]
    fn changes_record_itself_self_heals_an_unregistered_device() {
        let rec = std::sync::Arc::new(UnregisteredOnceRequester::default());
        let client = SyncClient::new(rec.clone());

        let reply = block_on(client.changes_record(
            "docs",
            "dev-1",
            "docs/report.txt",
            Some("aabb".to_string()),
            100,
            "create",
            None,
            None,
            None,
            None,
            None,
        ))
        .expect("self-heal then succeed");
        assert_eq!(reply.seq, 42);

        let calls = rec.calls.lock().unwrap();
        let kinds: Vec<&str> = calls.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            [
                "fauna.sync.changes.record", // rejected: device_unregistered
                "fauna.sync.register",       // heal
                "fauna.sync.changes.record", // retry, once
            ]
        );
    }

    #[test]
    fn restore_version_self_heals_an_unregistered_device_and_retries_once() {
        let rec = std::sync::Arc::new(UnregisteredOnceRequester::default());
        let client = SyncClient::new(rec.clone());

        let reply = block_on(client.restore_version(
            "docs",
            "dev-1",
            "docs/report.txt",
            "aabb".to_string(),
            100,
            None,
            None,
        ))
        .expect("self-heal then succeed");
        assert_eq!(reply.seq, 42);

        let calls = rec.calls.lock().unwrap();
        let kinds: Vec<&str> = calls.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            [
                "fauna.sync.changes.record", // rejected: device_unregistered
                "fauna.sync.register",       // heal
                "fauna.sync.changes.record", // retry, once
            ]
        );

        let reg: SyncRegisterRequest =
            fauna_protocol::decode_strict(&calls[1].1).expect("register decodes");
        assert_eq!(reg.device_id, "dev-1");
        assert_eq!(reg.label, SELF_REGISTER_LABEL);
    }

    /// A generic rejection must NOT be healed — a read-only device stays read-only
    /// rather than being silently upgraded to write.
    #[test]
    fn a_non_device_unregistered_rejection_is_not_healed() {
        struct AlwaysDenied;
        impl RpcRequester for AlwaysDenied {
            type Error = RejectErr;
            async fn request<Req, Reply>(
                &self,
                _kind: &'static str,
                _payload: Req,
            ) -> Result<Reply, Self::Error>
            where
                Req: serde::Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                Err(RejectErr(fauna_protocol::RpcError::new(
                    "fauna.auth.permission_denied",
                    "denied",
                )))
            }
        }

        let client = SyncClient::new(AlwaysDenied);
        let err = block_on(client.restore_version("docs", "d", "p", "aa".into(), 1, None, None))
            .expect_err("permission_denied must propagate");
        assert_eq!(err.0.code, "fauna.auth.permission_denied");
    }

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.sync.register" => fauna_protocol::encode_canonical(&SyncRegisterReply {
                device_id: String::new(),
                extra: Default::default(),
            }),
            "fauna.sync.changes.list" => fauna_protocol::encode_canonical(&SyncChangesListReply {
                changes: vec![],
                ..Default::default()
            }),
            "fauna.sync.changes.record" => {
                fauna_protocol::encode_canonical(&SyncChangeRecordReply {
                    seq: 1,
                    extra: Default::default(),
                })
            }
            "fauna.sync.changes.supersede" => {
                fauna_protocol::encode_canonical(&SyncChangesSupersedeReply {
                    superseded: 0,
                    extra: Default::default(),
                })
            }
            "fauna.sync.backup_status" => {
                fauna_protocol::encode_canonical(&SyncBackupStatusReply {
                    folders: vec![],
                    extra: Default::default(),
                })
            }
            "fauna.sync.status" => fauna_protocol::encode_canonical(&SyncStatusReply {
                folder: String::new(),
                source_online: false,
                extra: Default::default(),
            }),
            "fauna.sync.files" => fauna_protocol::encode_canonical(&SyncFilesReply {
                files: vec![],
                extra: Default::default(),
            }),
            "fauna.sync.conflicts.list" => fauna_protocol::encode_canonical(&ConflictsListReply {
                conflicts: vec![],
                extra: Default::default(),
            }),
            "fauna.sync.conflicts.report" => {
                fauna_protocol::encode_canonical(&ConflictReportReply {
                    id: 1,
                    extra: Default::default(),
                })
            }
            "fauna.sync.conflicts.resolve" => {
                fauna_protocol::encode_canonical(&ConflictResolveReply {
                    resolved: true,
                    winning_manifest_hash: None,
                    extra: Default::default(),
                })
            }
            "fauna.sync.devices.list" => fauna_protocol::encode_canonical(&SyncDevicesListReply {
                devices: vec![],
                extra: Default::default(),
            }),
            "fauna.sync.devices.delete" => {
                fauna_protocol::encode_canonical(&SyncDeviceDeleteReply {
                    deleted: true,
                    folders_removed_from: 0,
                    extra: Default::default(),
                })
            }
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn register_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.register("aabb", "laptop", None)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.register");
        let req: SyncRegisterRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.device_id, "aabb");
        assert_eq!(req.label, "laptop");
        assert_eq!(req.capabilities, "read,write");
    }

    #[test]
    fn changes_list_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.changes_list(Some("docs".into()), None, 7)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.changes.list");
        let req: SyncChangesListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "docs"
        ));
        assert_eq!(req.since, 7);
    }

    #[test]
    fn changes_record_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.changes_record(
            "docs",
            "aabb",
            "a.txt",
            Some("c3".into()),
            4096,
            "Created",
            Some(2),
            Some("deadbeef".into()),
            None,
            Some(17),
            Some(true),
        ))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.changes.record");
        let req: SyncChangeRecordRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "docs"
        ));
        assert_eq!(req.device_id, "aabb");
        assert_eq!(req.path, "a.txt");
        assert_eq!(req.manifest_hash.as_deref(), Some("c3"));
        assert_eq!(req.size_bytes, 4096);
        assert_eq!(req.change_type, "Created");
        assert_eq!(req.content_key_version, Some(2));
        assert_eq!(req.thumbnail_hash.as_deref(), Some("deadbeef"));
        assert_eq!(req.derived_through, Some(17));
        assert_eq!(req.is_resolution, Some(true));
    }

    #[test]
    fn changes_supersede_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.changes_supersede("docs", "aabb", "a.txt", "c3")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.changes.supersede");
        let req: SyncChangesSupersedeRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "docs"
        ));
        assert_eq!(req.device_id, "aabb");
        assert_eq!(req.path, "a.txt");
        assert_eq!(req.manifest_hash, "c3");
    }

    #[test]
    fn backup_status_composes_kind() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.backup_status()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.backup_status");
        let _req: SyncBackupStatusRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn status_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.status("photos")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.status");
        let req: SyncStatusRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "photos"
        ));
    }

    #[test]
    fn files_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.files("photos")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.files");
        let req: SyncFilesRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &req, "photos"
        ));
    }

    #[test]
    fn conflicts_list_composes_kind() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.conflicts_list()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.conflicts.list");
        let _req: ConflictsListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn conflicts_report_composes_kind_and_candidates() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        let candidates = vec![ConflictCandidate {
            manifest_hash: "aa".into(),
            device_id: "bb".into(),
            size_bytes: 12,
            created_at: 5,
            ..Default::default()
        }];
        block_on(
            client.conflicts_report(
                "photos",
                "dev1",
                "notes.txt",
                "concurrent_edit",
                Some("diverged".into()),
                candidates,
                ConflictLabels::seal(
                    Some(&fauna_core::path_crypto::LabelRoot::owner([3u8; 32])),
                    "notes.txt",
                    Some("diverged"),
                )
                .expect("sealing a path and a details string cannot fail"),
            ),
        )
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.conflicts.report");
        let req: ConflictReportRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.folder, "photos");
        assert_eq!(req.path, "notes.txt");
        assert_eq!(req.conflict_type, "concurrent_edit");
        assert_eq!(req.candidates.len(), 1);
        assert_eq!(req.candidates[0].manifest_hash, "aa");
        // The plain report is the degraded unresolved shape.
        assert_eq!(req.resolution, None);
        assert_eq!(req.winning_manifest_hash, None);
        // Path-sealing S6-a: the sealed companions ride the request, and the
        // salt on the wire is the path's own hash — the value the seal was
        // minted under, so a scrubbed row stays openable.
        assert_eq!(
            req.path_hash.as_deref().map(|b| &b[..]),
            Some(&fauna_core::sync::path_hash("notes.txt")[..])
        );
        assert!(req.path_sealed.is_some(), "the path must ship sealed");
        assert!(req.details_sealed.is_some(), "details must ship sealed");
    }

    /// A caller with no key material still addresses the row by hash, and ships
    /// no seal at all rather than a seal under some wrong root.
    #[test]
    fn a_keyless_reporter_ships_the_hash_and_no_seal() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(
            client.conflicts_report(
                "photos",
                "dev1",
                "notes.txt",
                "concurrent_edit",
                Some("diverged".into()),
                vec![],
                ConflictLabels::seal(None, "notes.txt", Some("diverged"))
                    .expect("hash-only cannot fail"),
            ),
        )
        .expect("infallible mock");
        let (_, payload) = rec.recorded();
        let req: ConflictReportRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req.path_hash.as_deref().map(|b| &b[..]),
            Some(&fauna_core::sync::path_hash("notes.txt")[..])
        );
        assert_eq!(req.path_sealed, None);
        assert_eq!(req.details_sealed, None);
    }

    #[test]
    fn conflicts_report_with_resolution_composes_pre_resolved_shape() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.conflicts_report_with(
            "photos",
            "dev1",
            "notes.txt",
            "concurrent_edit",
            None,
            vec![],
            Some(ResolvedOutcome {
                resolution: "latest_wins".into(),
                winning_manifest_hash: "cc".repeat(32),
                winning_size_bytes: Some(7),
                winning_content_key_version: None,
                // Deliberately distinct, and distinct from each other: the two
                // watermarks address different rows (winner head vs. loser
                // retention), so a pass-through that crossed or collapsed them
                // would still satisfy a single shared value.
                winning_derived_through: Some(31),
                losing_derived_through: Some(29),
                winning_carries_novelty: Some(true),
            }),
            ConflictLabels::seal(None, "notes.txt", None).expect("hash-only cannot fail"),
        ))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.conflicts.report");
        let req: ConflictReportRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.resolution.as_deref(), Some("latest_wins"));
        assert_eq!(req.winning_manifest_hash, Some("cc".repeat(32)));
        assert_eq!(req.winning_size_bytes, Some(7));
        // The causal watermark pair rides the same struct — a reporter that
        // drops it silently makes every propagated head row look ancestor-less
        // to the receiver's judge (conflicts.md clause 5).
        assert_eq!(req.winning_derived_through, Some(31));
        assert_eq!(req.losing_derived_through, Some(29));
        // The winner class (the same-anchor ruling, 2026-08-05): a dropped
        // flag re-stamps a novelty-carrying winner as a resolution, which
        // receivers byte-free-skip — the live leg-4a loss.
        assert_eq!(req.winning_carries_novelty, Some(true));
    }

    fn identity_signing(nonce: [u8; 32]) -> (fauna_core::identity::ActorKeypair, RecordSigning) {
        let account = fauna_core::identity::ActorKeypair::generate();
        let signing = RecordSigning {
            signer: std::sync::Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
                &account,
            )),
            set_nonce: SetNonceSource::Fixed(nonce),
        };
        (account, signing)
    }

    /// A Media-shaped record (upload create / delete tombstone) leaves the
    /// client signed under the set's nonce AND addressed by the set's hash
    /// alone. The nonce resolves by the set's name, so the record is signed
    /// before the address funnel takes that name off — signing after it
    /// looked up `""` and sent every Media write unsigned, which the nest
    /// refuses (`writer-signed-change-records.md`; `path-sealing.md` § the
    /// set-name plane).
    #[test]
    fn a_media_record_is_signed_under_the_named_sets_nonce_and_sent_by_hash() {
        use fauna_protocol::sync_writer_sig::SignedChange;
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let account = fauna_core::identity::ActorKeypair::generate();
        let nonce = [0x5A; 32];
        let signing = RecordSigning {
            signer: std::sync::Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
                &account,
            )),
            set_nonce: SetNonceSource::by_folder([("photos".to_string(), nonce)].into()),
        };
        let client = SyncClient::new(rec.clone()).with_record_signing(signing);
        for (change_type, manifest) in [("create", Some("cc".repeat(32))), ("delete", None)] {
            block_on(client.changes_record(
                "photos",
                "4b".repeat(32),
                "a.jpg",
                manifest.clone(),
                12,
                change_type,
                None,
                None,
                Some(b"sealed-path".to_vec()),
                None,
                None,
            ))
            .expect("infallible mock");
            let (kind, payload) = rec.recorded();
            assert_eq!(kind, "fauna.sync.changes.record");
            let req: SyncChangeRecordRequest =
                fauna_protocol::decode_strict(&payload).expect("decodes");
            assert!(req.folder.is_empty(), "the plaintext set name stays home");
            assert_eq!(
                req.name_hash.as_deref().map(Vec::as_slice),
                Some(&fauna_core::path_crypto::set_name_hash("photos")[..]),
                "the set travels by its hash"
            );
            let signature = req
                .signature
                .as_deref()
                .unwrap_or_else(|| panic!("a {change_type} record goes out signed"));
            let statement = SignedChange::for_record(&req, account.actor_id().0, nonce)
                .expect("a signable record");
            fauna_protocol::sync_writer_sig::verify_statement(
                &statement,
                signature,
                req.signer_key.as_deref().expect("signer key"),
                &fauna_protocol::sync_writer_sig::SignerCertCache::new(),
                fauna_core::data::Timestamp::now(),
            )
            .expect("the signature verifies under the set's nonce");
        }
    }

    /// A signing client signs the resolved report's winner head row exactly
    /// as the nest mints it (ruling (1)(ii)) — the engine reports through this
    /// funnel.
    #[test]
    fn a_signing_client_signs_the_resolved_reports_winner_row() {
        use fauna_protocol::sync_writer_sig::SignedChange;
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let (account, signing) = identity_signing([7; 32]);
        let client = SyncClient::new(rec.clone()).with_record_signing(signing);
        block_on(client.conflicts_report_with(
            "photos",
            hex::encode([0x3a; 32]),
            "notes.txt",
            "concurrent_edit",
            None,
            vec![],
            Some(ResolvedOutcome {
                resolution: "merged".into(),
                winning_manifest_hash: "cc".repeat(32),
                winning_size_bytes: Some(7),
                winning_content_key_version: Some(2),
                winning_derived_through: Some(31),
                losing_derived_through: Some(29),
                winning_carries_novelty: None,
            }),
            ConflictLabels::seal(None, "notes.txt", None).expect("hash-only cannot fail"),
        ))
        .expect("infallible mock");
        let (_, payload) = rec.recorded();
        let req: ConflictReportRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        let statement = SignedChange::for_resolved_report(&req, account.actor_id().0, [7; 32])
            .unwrap()
            .expect("resolved");
        assert_eq!(statement.device_id, [0x3a; 32]);
        assert_eq!(statement.derived_through, Some(31));
        assert!(statement.is_resolution);
        assert_eq!(
            req.winner_signature.as_ref().map(|s| s.to_vec()),
            Some(statement.sign(account.signing_key()).to_vec())
        );
        assert_eq!(
            req.winner_signer_key.as_ref().map(|k| k.to_vec()),
            Some(account.actor_id().0.to_vec())
        );
    }

    /// Answers `conflicts.list` with one conflict and records every resolve.
    struct OneConflictRequester {
        conflict: folders::SyncConflict,
        resolves: std::sync::Mutex<Vec<ConflictResolveRequest>>,
    }

    impl RpcRequester for OneConflictRequester {
        type Error = core::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let reply = match kind {
                "fauna.sync.conflicts.list" => {
                    fauna_protocol::encode_canonical(&ConflictsListReply {
                        conflicts: vec![self.conflict.clone()],
                        extra: Default::default(),
                    })
                }
                "fauna.sync.conflicts.resolve" => {
                    self.resolves
                        .lock()
                        .unwrap()
                        .push(fauna_protocol::decode_strict(&bytes).expect("decodes"));
                    fauna_protocol::encode_canonical(&ConflictResolveReply {
                        resolved: true,
                        winning_manifest_hash: None,
                        extra: Default::default(),
                    })
                }
                other => panic!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// A signing client signs a choose-winner resolve over the listed
    /// conflict's winning candidate (ruling (1)(ii)) — only once the judged
    /// history vouches for it (ruling (10)(f)); a mark-only resolve stays
    /// unsigned.
    #[test]
    fn a_signing_client_signs_the_choose_winner_head_row() {
        use fauna_protocol::sync_writer_sig::SignedChange;
        let conflict = folders::SyncConflict {
            id: 9,
            folder: "photos".into(),
            path_hash: ByteBuf::from(fauna_core::sync::path_hash("a.txt").to_vec()),
            path_sealed: Some(ByteBuf::from(vec![1, 2])),
            candidates: vec![ConflictCandidate {
                manifest_hash: "dd".repeat(32),
                device_id: "4b".repeat(32),
                size_bytes: 12,
                content_key_version: Some(3),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rec = std::sync::Arc::new(OneConflictRequester {
            conflict: conflict.clone(),
            resolves: Default::default(),
        });
        let (account, signing) = identity_signing([8; 32]);
        let client = SyncClient::new(rec.clone()).with_record_signing(signing);
        let vouched = judged_winner(&conflict, account.actor_id().0);
        block_on(client.conflicts_resolve_judged(&conflict, &vouched)).expect("vouched");
        block_on(client.conflicts_resolve(9, None)).expect("infallible mock");
        let resolves = rec.resolves.lock().unwrap();
        let statement = SignedChange::for_choose_winner(
            &conflict,
            &"dd".repeat(32),
            account.actor_id().0,
            [8; 32],
        )
        .unwrap();
        assert_eq!(
            resolves[0].winner_signature.as_ref().map(|s| s.to_vec()),
            Some(statement.sign(account.signing_key()).to_vec())
        );
        assert_eq!(
            resolves[1].winner_signature, None,
            "mark-only mints nothing"
        );
    }

    /// The judged version a conflict's single candidate names, signed as
    /// `own` — what a device holds after the judged lookup (ruling (10)(a)).
    fn judged_winner(
        conflict: &folders::SyncConflict,
        own: [u8; 32],
    ) -> restore_branch::JudgedCandidate {
        let c = &conflict.candidates[0];
        let version = fauna_protocol::files::FileVersionInfo {
            path_hash: conflict.path_hash.clone(),
            version_num: 1,
            manifest_hash: ByteBuf::from(hex::decode(&c.manifest_hash).unwrap()),
            size_bytes: c.size_bytes,
            content_key_version: c.content_key_version,
            device_id: Some(ByteBuf::from(hex::decode(&c.device_id).unwrap())),
            ..Default::default()
        };
        restore_branch::JudgedCandidate::find(
            vec![(
                version,
                fauna_protocol::sync_row_verify::RowVerdict::Verified {
                    writer: own,
                    signed_as: own,
                    origin: fauna_core::encoding::AuthoringOrigin::Direct,
                },
            )],
            fauna_core::sync::path_hash("a.txt"),
            &c.manifest_hash,
            Some(&own),
        )
        .expect("the version is judged")
    }

    /// A conflict row that lies about the version it names — here its size —
    /// is refused on the device: nothing is sent (ruling (10)(f)).
    #[test]
    fn a_choose_winner_the_history_does_not_vouch_for_sends_nothing() {
        let conflict = folders::SyncConflict {
            id: 9,
            folder: "photos".into(),
            path_hash: ByteBuf::from(fauna_core::sync::path_hash("a.txt").to_vec()),
            candidates: vec![ConflictCandidate {
                manifest_hash: "dd".repeat(32),
                device_id: "4b".repeat(32),
                size_bytes: 12,
                ..Default::default()
            }],
            ..Default::default()
        };
        let rec = std::sync::Arc::new(OneConflictRequester {
            conflict: conflict.clone(),
            resolves: Default::default(),
        });
        let (account, signing) = identity_signing([8; 32]);
        let client = SyncClient::new(rec.clone()).with_record_signing(signing);
        let vouched = judged_winner(&conflict, account.actor_id().0);
        let mut lying = conflict.clone();
        lying.candidates[0].size_bytes = 999;
        assert!(matches!(
            block_on(client.conflicts_resolve_judged(&lying, &vouched)),
            Err(ChooseWinnerError::NotVouched)
        ));
        assert!(rec.resolves.lock().unwrap().is_empty(), "nothing sent");
    }

    #[test]
    fn conflicts_list_with_resolved_sets_flag() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.conflicts_list_with(true)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.conflicts.list");
        let req: ConflictsListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.include_resolved, Some(true));
    }

    #[test]
    fn conflicts_resolve_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.conflicts_resolve(7, Some("abc123".into()))).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.conflicts.resolve");
        let req: ConflictResolveRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.id, 7);
        assert_eq!(req.winning_manifest_hash.as_deref(), Some("abc123"));
    }

    #[test]
    fn devices_list_composes_kind() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.devices_list()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.devices.list");
        let _req: SyncDevicesListRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn devices_delete_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SyncClient::new(rec.clone());
        block_on(client.devices_delete("dev-42")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.sync.devices.delete");
        let req: SyncDeviceDeleteRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.device_id, "dev-42");
    }

    /// The provisioner's nest leg names the machine's row and **mints
    /// nothing**: one `fauna.sync.register` under the app's own device id with
    /// the sealed label, and no `device_grant.register` — the store principal
    /// is the machine's only renewal credential (`sync-agent-credentials.md`
    /// § Credential model → the RULED 2026-09-28 block, decisions 1–2).
    #[test]
    fn registering_this_machine_names_the_row_and_mints_no_grant() {
        let identity = fauna_core::identity::ActorKeypair::generate();
        let own = fauna_core::hex32::encode(&[0x4d; 32]);
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        assert!(block_on(register_this_machine(
            rec.clone(),
            &identity,
            own.clone(),
            "Work laptop".to_string(),
        )));
        assert_eq!(
            rec.kinds(),
            vec!["fauna.sync.register"],
            "one register, no second credential"
        );
        let (_, payload) = rec.recorded();
        let req: SyncRegisterRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req.device_id, own,
            "the machine's named row is the app's own id"
        );
        assert!(
            req.label_sealed.is_some(),
            "the user-visible label rides sealed"
        );
    }

    /// `build_grant_revoke_request` produces exactly what the nest's self arm
    /// verifies: the named key signs the DOMAIN-TAGGED revoke payload over its
    /// own public key, timestamp and nonce.
    #[test]
    fn build_grant_revoke_request_proves_possession_of_the_retiring_key() {
        // verify-ok(test): this module signs with a locally generated key and checks
        // its own signature back — no wire-supplied key reaches it, so the permissive
        // trait is harmless here. Production verification goes through
        // `fauna_core::identity::verify_detached`; the walk guard
        // `fauna-core/tests/one_ed25519_verification_shape.rs` reads this marker.
        use ed25519_dalek::Verifier;

        let actor = fauna_core::identity::ActorKeypair::generate();
        let device = ed25519_dalek::SigningKey::from_bytes(&[0x31; 32]);
        let req = build_grant_revoke_request(&actor.actor_id().0, &device);

        let device_key = device.verifying_key().to_bytes();
        assert_eq!(req.device_key, fauna_core::hex32::encode(&device_key));
        let nonce = hex::decode(req.nonce.as_deref().expect("nonce present")).expect("hex nonce");
        let sig_bytes =
            hex::decode(req.signature.as_deref().expect("signature present")).expect("hex sig");
        let signature =
            ed25519_dalek::Signature::from_slice(&sig_bytes).expect("64-byte signature");
        let msg = fauna_protocol::auth::device_grant_revoke_signed_message(
            &actor.actor_id().0,
            &device_key,
            req.timestamp_ms.expect("timestamp present"),
            &nonce,
        );
        device
            .verifying_key()
            .verify(&msg, &signature)
            .expect("the retiring key signed the revoke payload");

        // And NOT the mint payload — the two differ only by domain tag, so a
        // builder that reached for the wrong one would still verify here
        // against `msg` while handing the nest a signature that mints.
        let mint_msg = fauna_protocol::auth::device_handshake_signed_message(
            &actor.actor_id().0,
            &device_key,
            req.timestamp_ms.expect("timestamp present"),
            &[0x5e; 32],
            &nonce,
        );
        assert!(
            device
                .verifying_key()
                .verify(&mint_msg, &signature)
                .is_err(),
            "a retirement signature must not verify as a handshake — otherwise \
             the domain separation is decorative"
        );
    }

    /// The nonce is fresh per request. Ed25519 is deterministic, so two
    /// retirement attempts for the same key in the same millisecond would
    /// otherwise be byte-identical and the nest's single-use replay guard would
    /// refuse the second — turning the ruled "retry after each renewal" into a
    /// loop that can never make progress after its first failure.
    #[test]
    fn each_grant_revoke_request_carries_a_fresh_nonce() {
        let actor = fauna_core::identity::ActorKeypair::generate();
        let device = ed25519_dalek::SigningKey::from_bytes(&[0x32; 32]);
        let a = build_grant_revoke_request(&actor.actor_id().0, &device);
        let b = build_grant_revoke_request(&actor.actor_id().0, &device);
        assert_ne!(a.nonce, b.nonce, "nonces must not repeat");
        assert_ne!(
            a.signature, b.signature,
            "and the signatures they uniquify must not either"
        );
    }

    /// The principal grant covers the CALLER-SUPPLIED writer key — never a
    /// fresh keypair (the principal IS the writer key, T10; a grant over any
    /// other key is refused by `PrincipalSlot::store_device_authorization`).
    #[test]
    fn build_principal_grant_signs_over_the_writer_key() {
        let identity = fauna_core::identity::ActorKeypair::generate();
        let writer_pub = [0x5a; 32];
        let wire = build_principal_grant(&identity, &writer_pub).expect("mint grant");

        let (bytes, env) = wire.into_signed().expect("well-formed envelope");
        let auth: fauna_core::data::DeviceAuthorization =
            fauna_core::encoding::decode_signed_bytes(&bytes).expect("decodes");
        fauna_core::encoding::verify_envelope(&auth, &bytes, &env)
            .expect("verifies under the identity key");
        assert_eq!(auth.actor_id, identity.actor_id());
        assert_eq!(
            auth.device_key, writer_pub,
            "the grant must cover the writer key it was asked to cover"
        );
        assert!(principal_grant_is_current(&auth));
        assert!(matches!(
            auth.capabilities.as_slice(),
            [
                fauna_core::data::Capability::RenewBearer,
                fauna_core::data::Capability::SyncWrite
            ]
        ));
        // A `[RenewBearer]`-only grant (no `SyncWrite`) is the
        // ceremony's heal signal.
        let renew_only = fauna_core::data::DeviceAuthorization {
            capabilities: vec![fauna_core::data::Capability::RenewBearer],
            ..auth
        };
        assert!(!principal_grant_is_current(&renew_only));
        assert!(auth.expires_at.is_none(), "no expiry by design");
    }

    /// A bare wire error, so the classifiers can be exercised without a
    /// transport. `None` models a transport fault (the request never reached a
    /// nest), which must never be read as a grant statement.
    struct WireErr(Option<fauna_protocol::RpcError>);

    impl RpcErrorClass for WireErr {
        fn is_rejection(&self) -> bool {
            self.0.is_some()
        }
        fn as_rpc_error(&self) -> Option<&fauna_protocol::RpcError> {
            self.0.as_ref()
        }
    }

    fn rejected(code: &str) -> WireErr {
        WireErr(Some(fauna_protocol::RpcError::new(code, "error.test")))
    }

    /// D4: exactly the two authoritative write-grant refusals park the engine —
    /// the cross-nest `require_foreign_writer` `forbidden` and the same-nest
    /// ST-RES-1 `not_found` fold.
    #[test]
    fn access_revoked_fires_on_both_authoritative_refusals() {
        assert!(
            is_access_revoked(&rejected("fauna.federation.forbidden")),
            "cross-nest: the home nest refused the mint/record relay"
        );
        assert!(
            is_access_revoked(&rejected("fauna.sync.not_found")),
            "same-nest: resolve_writable_folder folds a demotion to not_found"
        );
    }

    /// The discriminating half: parking is terminal, so a code that does NOT
    /// assert "your grant is gone" must never reach it. `peer_nest_outdated` is
    /// the sharpest — it rides the same cross-nest relay as the real refusal, and
    /// parking on it would strand a perfectly valid writer on a peer-version gap.
    #[test]
    fn access_revoked_does_not_fire_on_retryable_or_unrelated_codes() {
        for code in [
            "fauna.federation.peer_nest_outdated", // version gap, grant may be fine
            "fauna.sync.device_unregistered",      // self-heals (is_device_unregistered)
            "fauna.sync.permission_denied",        // device capability, not a set grant
            "fauna.protocol.internal",             // transient
            "fauna.nest.outdated",
        ] {
            assert!(
                !is_access_revoked(&rejected(code)),
                "{code} must not park the engine"
            );
        }
        // A transport fault never reached a nest, so it asserts nothing at all.
        assert!(!is_access_revoked(&WireErr(None)));
    }

    /// The tier's device cap reads on its own wire code and on nothing else: not
    /// the register's other refusals, not the storage-quota twin (a different
    /// remedy), and not a transport fault, which never reached a nest.
    #[test]
    fn device_limit_exceeded_fires_only_on_the_tier_cap_refusal() {
        assert!(is_device_limit_exceeded(&rejected(
            "fauna.sync.device_limit_exceeded"
        )));
        for code in [
            "fauna.sync.invalid_request", // the reserved WebDAV pseudo-device id
            "fauna.sync.forbidden",       // no users row, or the tier lookup failed
            "fauna.sync.storage_quota_exceeded", // the storage cap: free space, not a slot
            "fauna.protocol.internal",
        ] {
            assert!(
                !is_device_limit_exceeded(&rejected(code)),
                "{code} is not the device cap"
            );
        }
        assert!(!is_device_limit_exceeded(&WireErr(None)));
    }
}
