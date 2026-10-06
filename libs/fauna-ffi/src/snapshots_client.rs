//! UniFFI façade for the `fauna.filesync.snapshot.*` WS-RPC kinds — the
//! Backups page snapshot surface: the folder snapshot table (`list`),
//! snapshot detail (`get`), the create / delete / prune / check / diff
//! actions, and the restore surface — restore history + divergence reads
//! (`list_restore_history` / `list_restore_divergence`) and the local
//! restore action (`restore_message_kind`), per `docs/goal/ui/backups.md`
//! §§ Restore history / Restore divergence / Restore from backup
//! destination. The WS-RPC twins of the deleted `/api/v1/snapshots/*`
//! HTTP (only the snapshot **byte downloads** stay HTTP residue —
//! `api-layers.md` § Snapshots).
//!
//! [`FfiSnapshotsClient`] wraps `fauna_client_snapshots::SnapshotsClient`
//! (which wraps the shared `NestClient`); the mirror records below are the
//! FFI-visible shape of the `fauna_protocol::filesync::*` replies the
//! Backups page consumes. The Rust-native Linux app calls the same
//! `SnapshotsClient` directly — this seam gives Apple / Windows / Android
//! the identical surface over UniFFI. Construct via
//! [`crate::nest_client::FfiNestClient::snapshots`].
//!
//! Mirror convention (matching `events_client.rs`): only the fields the
//! Backups page renders are mirrored — the freeform `extra` forward-compat
//! map and snapshot fields the UI never shows (parent_id, message_kind on
//! the detail, manifest hashes) are dropped at the boundary. `device_id`
//! crosses as raw `Vec<u8>` (the caller hex-encodes at the UI edge);
//! timestamps / counts / sizes ride as `i64`.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_snapshots::SnapshotsClient;
use fauna_client_snapshots::filesync::SnapshotRetentionPolicy;

use crate::{FfiError, stringify};

// ── reply mirrors ────────────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::filesync::SnapshotSummaryRow`] — one row
/// of the folder snapshot table.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSnapshotSummary {
    pub id: i64,
    pub created_at: i64,
    /// `Some("mail")` / `Some("calendar")` for message-kind snapshots;
    /// `None` for folder snapshots.
    pub message_kind: Option<String>,
    pub file_count: i64,
    pub total_bytes: i64,
    /// Raw 32-byte capturing-device id (the caller hex-encodes at the UI
    /// edge); `None` = unattributed. Drives the Backups per-device filter.
    pub device_id: Option<Vec<u8>>,
}

/// FFI mirror of [`fauna_protocol::filesync::SnapshotFileEntry`] — one file
/// recorded in a snapshot (the Backups detail file list shows path / size /
/// type).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSnapshotFileEntry {
    pub path: String,
    pub size_bytes: i64,
    pub file_type: String,
}

/// FFI mirror of [`fauna_protocol::filesync::SnapshotGetReply`] — snapshot
/// metadata + file listing.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSnapshotGetReply {
    pub id: i64,
    pub folder: String,
    pub created_at: i64,
    pub file_count: i64,
    pub total_bytes: i64,
    pub tags: Vec<String>,
    /// Raw 32-byte device id (the caller hex-encodes); `None` = unattributed.
    pub device_id: Option<Vec<u8>>,
    pub files: Vec<FfiSnapshotFileEntry>,
}

/// FFI mirror of [`fauna_protocol::filesync::SnapshotCreateFolderReply`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSnapshotCreateFolderReply {
    pub id: i64,
    pub file_count: i64,
    pub total_bytes: i64,
    pub created_at: i64,
    pub tags: Vec<String>,
    pub device_id: Option<Vec<u8>>,
}

/// FFI mirror of [`fauna_protocol::filesync::PrunableSnapshot`] — one prune
/// candidate, surfaced on a dry-run so the Backups RetentionEditor sheet can
/// preview which snapshots a policy would remove.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiPrunableSnapshot {
    pub id: i64,
    pub created_at: i64,
    pub tags: Vec<String>,
}

/// FFI mirror of [`fauna_protocol::filesync::SnapshotPruneReply`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSnapshotPruneReply {
    pub dry_run: bool,
    /// Count pruned (actual) or that would be pruned (dry-run).
    pub pruned: i64,
    /// Count remaining (actual) or that would be kept (dry-run).
    pub remaining: i64,
    /// Dry-run: the prune candidates the RetentionEditor sheet lists. Actual
    /// (`dry_run == false`): empty.
    pub snapshots: Vec<FfiPrunableSnapshot>,
}

/// FFI mirror of [`fauna_protocol::filesync::SnapshotCheckReply`] — the
/// integrity-check status, the per-stage counts the Backups IntegrityCheck
/// sheet renders. (Per-finding detail is the shared backups machine's
/// `structured_errors` read.)
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSnapshotCheckReply {
    /// `"ok"` or `"errors"`.
    pub status: String,
    /// Derived from `status` (`== "ok"`) in shared Rust so clients surface the
    /// pass/fail badge without re-deriving the wire string.
    pub is_ok: bool,
    pub snapshots_checked: i64,
    pub files_checked: i64,
    pub manifests_checked: i64,
    pub chunks_checked: i64,
    pub missing_manifests: i64,
    pub missing_chunks: i64,
    pub corrupt_manifests: i64,
}

/// FFI mirror of [`fauna_protocol::filesync::SnapshotDiffEntry`] — an added
/// or removed file in a snapshot diff.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSnapshotDiffEntry {
    pub path: String,
    pub size_bytes: i64,
}

/// FFI mirror of [`fauna_protocol::filesync::SnapshotModifiedEntry`] — a file
/// present in both snapshots with a changed size.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSnapshotModifiedEntry {
    pub path: String,
    pub old_size: i64,
    pub new_size: i64,
}

/// FFI mirror of [`fauna_protocol::filesync::SnapshotDiffSummary`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSnapshotDiffSummary {
    pub added_count: i64,
    pub removed_count: i64,
    pub modified_count: i64,
    pub added_bytes: i64,
    pub removed_bytes: i64,
    pub net_bytes: i64,
}

/// FFI mirror of [`fauna_protocol::filesync::SnapshotDiffReply`] — the
/// added / removed / modified folders between two snapshots of one folder.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSnapshotDiffReply {
    pub snapshot_a: i64,
    pub snapshot_b: i64,
    pub added: Vec<FfiSnapshotDiffEntry>,
    pub removed: Vec<FfiSnapshotDiffEntry>,
    pub modified: Vec<FfiSnapshotModifiedEntry>,
    pub summary: FfiSnapshotDiffSummary,
}

/// FFI mirror of [`fauna_protocol::filesync::RestoreHistoryRow`] — one
/// `restore_history` row (Backups § Restore history). `source_member_id`
/// `None` renders as "local snapshot" (Plan 4 populates the destination
/// provenance — `None` everywhere until then); the caller hex-encodes at the
/// UI edge. `snapshot_id` keys the per-row divergence read.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiRestoreHistoryRow {
    pub id: i64,
    pub completed_at: i64,
    pub snapshot_id: i64,
    /// `"mail"` / `"calendar"` / (future) `"mail+calendar"`.
    pub kinds_restored: String,
    /// Backup-destination provenance; `None` → "local snapshot".
    pub source_member_id: Option<Vec<u8>>,
}

/// FFI mirror of [`fauna_protocol::filesync::RestoreDivergenceRow`] — one
/// `bridge_restore_divergence` row (Backups § Restore divergence). Forensic:
/// server state won the restore; the row records what the MUA lost.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiRestoreDivergenceRow {
    pub id: i64,
    pub snapshot_id: i64,
    pub observed_at: i64,
    /// `"imap"` or `"caldav"`.
    pub protocol: String,
    /// Mailbox name (IMAP) or calendar id hex (CalDAV).
    pub collection: String,
    /// MUA identity (advisory); `None` → "(unknown)" at the UI edge.
    pub mua_id: Option<String>,
    pub client_modseq: i64,
    pub server_modseq: i64,
    /// `max(client_modseq - server_modseq, 0)` — coarse "~N writes lost".
    pub lost_event_count: i64,
}

/// FFI mirror of [`fauna_protocol::filesync::SnapshotRestoreMessageKindReply`]
/// — the local-restore action result. `config_present == false` warns that
/// the bridge can't AUTH after restart until the wrapped-MLS-blob bundle is restored too;
/// `note` carries the human-readable warning (empty otherwise).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSnapshotRestoreReply {
    pub snapshot_id: i64,
    pub kind: String,
    pub config_present: bool,
    pub note: String,
}

// ── label custody: the two construction seams ────────────────────────────

/// This connection's shared-set key resolver — `None` when the build has no
/// resolver to give (the Go mail-bridge's `--no-default-features` shape, and
/// wasm).
///
/// Factored out because the two directions want *opposite* things from a
/// missing resolver, and the difference is easy to get wrong when the wiring is
/// hand-rolled per call site (a review found three sites, all keyless). See [`owner_read_snapshots_client`] for the read direction and
/// [`FfiSnapshotsClient::client`] for the seal direction.
///
/// ⚠ The gate must name **this crate's** feature — `feature = "mls"` compiles to
/// a permanently-false cfg here and would silently disable the resolver in every
/// profile including the default one.
pub(crate) fn folder_key_resolver(
    nest: &Arc<NestClient>,
    secret: &[u8; 32],
) -> Option<Arc<dyn fauna_core::folder_keys::FolderKeyResolver>> {
    #[cfg(all(feature = "folders-author", not(target_arch = "wasm32")))]
    {
        // The custody is the seat's; `secret` only marks an identity-holding
        // connection (a keyless one has nothing to resolve for).
        let _ = secret;
        Some(Arc::new(fauna_client_folders::NestFolderKeyResolver::new(
            Arc::clone(nest),
            crate::account_runtime::folder_key_store(),
        )))
    }
    #[cfg(not(all(feature = "folders-author", not(target_arch = "wasm32"))))]
    {
        let _ = (nest, secret);
        None
    }
}

/// A [`SnapshotsClient`] wired to **read** this owner's sealed file rows — the
/// single construction seam for every FFI surface that reads them.
///
/// This exists because a review found the same defect at three
/// separate `SnapshotsClient::new` call sites, each of which *held the owner
/// secret already* and spent it on chunks a few dozen lines later. Hand-rolling
/// the wiring per site is what let that happen, so the sites now share one.
///
/// **Always keyed, unlike the sealing façade** — and that asymmetry is the whole
/// point, so do not "unify" the two by giving this one the keyless fallback:
///
/// - On the **seal** direction a missing resolver must NOT fall back to the
///   owner root: a bound set would seal a label-audience field under a root no
///   roster member can open, and a snapshot is immutable, so the roster loses it
///   permanently ([`FfiSnapshotsClient::client`] carries the
///   full account). Sealing nothing is the honest degrade there.
/// - On the **read** direction the same fallback is strictly *correct*: opening
///   what the owner root can open renders **more** rows and can never mis-seal
///   anything, because this client never writes. Refusing to wire the owner key
///   just because there is no resolver would reintroduce the exact bug — an empty listing where the reader holds the key.
///
/// Gated on `sync-engine-host` to match its two consumers (the full restore and
/// the per-file download) — the Go mail-bridge's `--no-default-features` build
/// drops all three together and has no restore surface to wire.
#[cfg(feature = "sync-engine-host")]
pub(crate) fn owner_read_snapshots_client(
    nest: Arc<NestClient>,
    owner_secret: &[u8; 32],
) -> SnapshotsClient<Arc<NestClient>> {
    let resolver = folder_key_resolver(&nest, owner_secret);
    SnapshotsClient::new(nest).with_label_custody(fauna_core::label_custody::LabelCustody::new(
        resolver,
        Some(fauna_core::crypto::BackupKey::derive(owner_secret)),
    ))
}

// ── FfiSnapshotsClient ───────────────────────────────────────────────────

/// UniFFI handle for the `fauna.filesync.snapshot.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::snapshots`]; methods are exposed to
/// Swift as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiSnapshotsClient {
    nest: Arc<NestClient>,
}

impl FfiSnapshotsClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    /// The typed call surface, with this connection's **label custody** wired —
    /// a REAL resolver, not owner-only. It is what makes the Backups "back up
    /// now" gesture a **keyed** writer, so a snapshot's tags seal on the way out
    /// (path-sealing S6-d), and windows — the one app that sends a tag today —
    /// is an FFI app.
    ///
    /// ⚠ **The resolver is load-bearing, and owner-only custody here would be a
    /// silent defect** — and this is exactly the façade where it was found. `LabelCustody::owner_only` does NOT fail closed on a bound set
    /// the way this comment used to claim: its `keys_for` no-resolver arm yields
    /// `FileDownloadKeys::owner(..)` with `mls_group_id: None`, so
    /// `label_seal_root`'s bound-set bail is skipped and the label seals under
    /// the **owner** root. Tags are a label-audience field, so at the flip the
    /// plaintext scrubs, the sealed sibling exists (so S8 skips the row), no
    /// roster member can open it — and a snapshot is immutable, so unlike every
    /// re-recordable plane there is no later gesture that re-stamps it: the
    /// roster loses the tags permanently.
    ///
    /// With the resolver wired, a bound set resolves its M2 content keys and
    /// seals under the generation its roster can open, exactly as linux already
    /// does (`apps/fauna-linux/src/client.rs::label_custody` — same
    /// `BackupKey::derive(secret)`, so no cross-app root divergence). An unbound
    /// set still takes the owner arm, which is correct there: the owner is the
    /// whole audience. The same custody serves the read path, so a bound set's
    /// sealed labels render for this connection too.
    ///
    /// `None` keypair (a bearer-only connection) ⇒ keyless custody, which renders
    /// and seals exactly as this crate did before sealing: not at all.
    fn client(&self) -> SnapshotsClient<Arc<NestClient>> {
        let client = SnapshotsClient::new(Arc::clone(&self.nest));
        let Some(keypair) = self.nest.auth().keypair() else {
            return client;
        };
        let secret = *keypair.secret_bytes();
        // The resolver lives behind `fauna-client-folders/mls`, which this crate
        // pulls in via its own `folders-author` feature (it reads the roster to
        // learn a set's bound-ness). Without it there is no way to reach a bound
        // set's M2 generation, so this façade seals NOTHING rather than falling
        // back to the owner root — the trap is exactly that the owner-root
        // fallback looks like a graceful degrade and is really silent
        // member-side loss. Not sealing leaves an honest S8 backfill row;
        // sealing wrongly does not.
        //
        // ⚠ This early return is the SEAL direction's rule and must not be
        // copied to a read-only surface, where the same fallback is correct and
        // its absence is a real bug — [`owner_read_snapshots_client`] is that
        // surface's seam.
        let Some(resolver) = folder_key_resolver(&self.nest, &secret) else {
            return client;
        };
        client.with_label_custody(fauna_core::label_custody::LabelCustody::new(
            Some(resolver),
            Some(fauna_core::crypto::BackupKey::derive(&secret)),
        ))
    }
}

#[fauna_uniffi_async::export]
impl FfiSnapshotsClient {
    /// `fauna.filesync.snapshot.list` — list snapshots, newest first.
    /// `folder: Some(name)` lists that folder's snapshots (the Backups
    /// table); `message_kind` filters the owner-implicit message-kind list
    /// when `folder` is `None`. `limit == 0` → server default.
    pub async fn snapshot_list(
        &self,
        message_kind: Option<String>,
        folder: Option<String>,
        limit: u32,
    ) -> Result<Vec<FfiSnapshotSummary>, FfiError> {
        let reply = self
            .client()
            .list(message_kind, folder, limit)
            .await
            .map_err(stringify)?;
        Ok(reply
            .rows
            .into_iter()
            .map(|r| FfiSnapshotSummary {
                id: r.id,
                created_at: r.created_at,
                message_kind: r.message_kind,
                file_count: r.file_count,
                total_bytes: r.total_bytes,
                device_id: r.device_id.map(|b| b.into_vec()),
            })
            .collect())
    }

    /// `fauna.filesync.snapshot.get` — snapshot metadata + file listing.
    pub async fn snapshot_get(&self, snapshot_id: i64) -> Result<FfiSnapshotGetReply, FfiError> {
        let r = self.client().get(snapshot_id).await.map_err(stringify)?;
        Ok(FfiSnapshotGetReply {
            id: r.id,
            folder: r.folder,
            created_at: r.created_at,
            file_count: r.file_count,
            total_bytes: r.total_bytes,
            tags: r.tags,
            device_id: r.device_id.map(|b| b.into_vec()),
            files: r
                .files
                .into_iter()
                .map(|f| FfiSnapshotFileEntry {
                    path: f.path,
                    size_bytes: f.size_bytes,
                    file_type: f.file_type,
                })
                .collect(),
        })
    }

    /// `fauna.filesync.snapshot.create_folder` — capture a point-in-time
    /// snapshot of a synced folder. `tags` empty + unattributed (no
    /// `device_id`) for the client-driven capture the Backups "back up now"
    /// button issues.
    pub async fn snapshot_create_folder(
        &self,
        folder: String,
        tags: Vec<String>,
    ) -> Result<FfiSnapshotCreateFolderReply, FfiError> {
        let r = self
            .client()
            .create_folder(folder, tags, None)
            .await
            .map_err(stringify)?;
        Ok(FfiSnapshotCreateFolderReply {
            id: r.id,
            file_count: r.file_count,
            total_bytes: r.total_bytes,
            created_at: r.created_at,
            tags: r.tags,
            device_id: r.device_id.map(|b| b.into_vec()),
        })
    }

    /// `fauna.filesync.snapshot.delete` — queue a 48 h soft-delete. The
    /// reply (pending-action id) is discarded; the Backups page reloads.
    pub async fn snapshot_delete(&self, snapshot_id: i64) -> Result<(), FfiError> {
        self.client().delete(snapshot_id).await.map_err(stringify)?;
        Ok(())
    }

    /// `fauna.filesync.snapshot.prune` — apply a restic-style retention
    /// policy to a folder. Only the buckets the Backups prune button sets
    /// are exposed (`None` = bucket disabled); `dry_run` reports candidates
    /// without deleting.
    #[allow(clippy::too_many_arguments)] // UniFFI export — argument count is protocol-dictated
    pub async fn snapshot_prune(
        &self,
        folder: String,
        dry_run: bool,
        keep_last: Option<u32>,
        keep_daily: Option<u32>,
        keep_weekly: Option<u32>,
        keep_monthly: Option<u32>,
        keep_yearly: Option<u32>,
    ) -> Result<FfiSnapshotPruneReply, FfiError> {
        let policy = SnapshotRetentionPolicy {
            keep_last,
            keep_daily,
            keep_weekly,
            keep_monthly,
            keep_yearly,
            ..Default::default()
        };
        let r = self
            .client()
            .prune(folder, dry_run, policy)
            .await
            .map_err(stringify)?;
        Ok(FfiSnapshotPruneReply {
            dry_run: r.dry_run,
            pruned: r.pruned,
            remaining: r.remaining,
            snapshots: r
                .snapshots
                .into_iter()
                .map(|s| FfiPrunableSnapshot {
                    id: s.id,
                    created_at: s.created_at,
                    tags: s.tags,
                })
                .collect(),
        })
    }

    /// `fauna.filesync.snapshot.check` — integrity scan over the blob store
    /// for one folder.
    pub async fn snapshot_check(
        &self,
        folder: String,
        verify_content: bool,
    ) -> Result<FfiSnapshotCheckReply, FfiError> {
        let r = self
            .client()
            .check(folder, verify_content)
            .await
            .map_err(stringify)?;
        Ok(FfiSnapshotCheckReply {
            is_ok: r.is_ok(),
            status: r.status,
            snapshots_checked: r.snapshots_checked,
            files_checked: r.files_checked,
            manifests_checked: r.manifests_checked,
            chunks_checked: r.chunks_checked,
            missing_manifests: r.missing_manifests,
            missing_chunks: r.missing_chunks,
            corrupt_manifests: r.corrupt_manifests,
        })
    }

    /// `fauna.filesync.snapshot.diff` — compare two snapshots of the same file
    /// set → added / removed / modified files (the Backups diff view).
    pub async fn snapshot_diff(&self, a: i64, b: i64) -> Result<FfiSnapshotDiffReply, FfiError> {
        let r = self.client().diff(a, b).await.map_err(stringify)?;
        Ok(FfiSnapshotDiffReply {
            snapshot_a: r.snapshot_a,
            snapshot_b: r.snapshot_b,
            added: r
                .added
                .into_iter()
                .map(|e| FfiSnapshotDiffEntry {
                    path: e.path,
                    size_bytes: e.size_bytes,
                })
                .collect(),
            removed: r
                .removed
                .into_iter()
                .map(|e| FfiSnapshotDiffEntry {
                    path: e.path,
                    size_bytes: e.size_bytes,
                })
                .collect(),
            modified: r
                .modified
                .into_iter()
                .map(|e| FfiSnapshotModifiedEntry {
                    path: e.path,
                    old_size: e.old_size,
                    new_size: e.new_size,
                })
                .collect(),
            summary: FfiSnapshotDiffSummary {
                added_count: r.summary.added_count,
                removed_count: r.summary.removed_count,
                modified_count: r.summary.modified_count,
                added_bytes: r.summary.added_bytes,
                removed_bytes: r.summary.removed_bytes,
                net_bytes: r.summary.net_bytes,
            },
        })
    }

    /// `fauna.filesync.snapshot.list_restore_history` — the bearer's
    /// `restore_history` rows, newest first (Backups § Restore history).
    /// `limit == 0` → server default. Replay-safe pure read.
    pub async fn snapshot_list_restore_history(
        &self,
        limit: u32,
    ) -> Result<Vec<FfiRestoreHistoryRow>, FfiError> {
        let reply = self
            .client()
            .list_restore_history(limit)
            .await
            .map_err(stringify)?;
        Ok(reply
            .rows
            .into_iter()
            .map(|r| FfiRestoreHistoryRow {
                id: r.id,
                completed_at: r.completed_at,
                snapshot_id: r.snapshot_id,
                kinds_restored: r.kinds_restored,
                source_member_id: r.source_member_id,
            })
            .collect())
    }

    /// `fauna.filesync.snapshot.list_restore_divergence` — the forensic
    /// divergence rows recorded against one snapshot's restore (Backups
    /// § Restore divergence). Owner-only; the nest checks the bearer owns the
    /// snapshot. Replay-safe pure read.
    pub async fn snapshot_list_restore_divergence(
        &self,
        snapshot_id: i64,
    ) -> Result<Vec<FfiRestoreDivergenceRow>, FfiError> {
        let reply = self
            .client()
            .list_restore_divergence(snapshot_id)
            .await
            .map_err(stringify)?;
        Ok(reply
            .rows
            .into_iter()
            .map(|r| FfiRestoreDivergenceRow {
                id: r.id,
                snapshot_id: r.snapshot_id,
                observed_at: r.observed_at,
                protocol: r.protocol,
                collection: r.collection,
                mua_id: r.mua_id,
                client_modseq: r.client_modseq,
                server_modseq: r.server_modseq,
                lost_event_count: r.lost_event_count,
            })
            .collect())
    }

    /// `fauna.filesync.snapshot.restore_message_kind` — restore one
    /// message-kind snapshot into `bridge_imap_*` / `bridge_caldav_*` and
    /// rebuild `segment_records`. `confirm_id` must equal `snapshot_id`
    /// stringified (the re-type friction bar); a mismatch returns
    /// `fauna.filesync.snapshot.confirm_mismatch` without mutating state.
    pub async fn snapshot_restore_message_kind(
        &self,
        snapshot_id: i64,
        confirm_id: String,
    ) -> Result<FfiSnapshotRestoreReply, FfiError> {
        let r = self
            .client()
            .restore_message_kind(snapshot_id, confirm_id)
            .await
            .map_err(stringify)?;
        Ok(FfiSnapshotRestoreReply {
            snapshot_id: r.snapshot_id,
            kind: r.kind,
            config_present: r.config_present,
            note: r.note,
        })
    }
}

// ── immediate-delete (gated; dropped from the Go mail-bridge build) ──────────
//
// The owner-only immediate-delete snapshot surface (`backups.md` § User actions
// + the `immediate-delete-confirm-modal`). Gated behind the default-on
// `snapshots-immediate-delete` feature so the Go mail-bridge
// `--no-default-features` build drops it (the bridge has no Backups UI) and the
// checked-in Go bindings stay byte-identical — the rest of `FfiSnapshotsClient`
// is unconditional + Go-emittable, so a new ungated export would force an
// off-win Go regen (memory reference_ffi_gate_conversations_session_excludes_go).

/// The exact acknowledge string the immediate-delete modal collects
/// (`immediate-delete-acknowledge-input`). Surfaced from the shared protocol
/// constant so the modal displays + validates the SAME string the nest checks
/// byte-for-byte — a client literal could drift and enable a confirm the nest
/// then rejects with `acknowledge_mismatch`.
#[cfg(feature = "snapshots-immediate-delete")]
#[uniffi::export]
pub fn immediate_delete_ack_text() -> String {
    fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT.to_string()
}

/// `fauna_client_snapshots::immediate_delete_button_enabled` — the Backups
/// immediate-delete friction-bar predicate (`docs/goal/ui/backups.md`
/// Architectural rule 4): the retyped snapshot id AND the acknowledge phrase
/// must both match exactly, with no delete already in flight. `target_id`
/// empty means no snapshot selected (always disabled). Five apps
/// (web, windows, android, apple, linux) each hand-rolled this boolean before
/// this export existed; each adopts it in place of its own re-derivation.
#[cfg(feature = "snapshots-immediate-delete")]
#[uniffi::export]
pub fn immediate_delete_button_enabled(
    deleting: bool,
    confirm_id: String,
    target_id: String,
    acknowledge_typed: String,
) -> bool {
    fauna_client_snapshots::immediate_delete_button_enabled(
        deleting,
        &confirm_id,
        &target_id,
        &acknowledge_typed,
    )
}

/// A `restore-snapshot-select` option's label: `"{kind} (#{id})"`. Shared so
/// every app renders identical text — android, apple and windows each
/// hand-rolled this one-line format before this export existed; linux and
/// tui call [`fauna_client_snapshots::snapshot_restore_option_label`]
/// directly (no FFI hop needed).
#[uniffi::export]
pub fn snapshot_restore_option_label(message_kind: Option<String>, id: i64) -> String {
    fauna_client_snapshots::snapshot_restore_option_label(message_kind.as_deref(), id)
}

#[cfg(feature = "snapshots-immediate-delete")]
#[fauna_uniffi_async::export]
impl FfiSnapshotsClient {
    /// `fauna.filesync.snapshot.delete_immediate` — owner-only immediate
    /// snapshot delete (the Backups immediate-delete modal's confirm). The nest
    /// still enforces the hard floor (≥3 active) + owner-only; the modal
    /// pre-gates the friction bar (`confirm_id` == the snapshot id retyped,
    /// `acknowledge` == [`immediate_delete_ack_text`]) before enabling its
    /// confirm. The reply (snapshot id + the 14 d segment retention) carries no
    /// UI-needed data — the page just reloads — so it is discarded.
    pub async fn snapshot_delete_immediate(
        &self,
        snapshot_id: i64,
        confirm_id: String,
        acknowledge: String,
    ) -> Result<(), FfiError> {
        self.client()
            .delete_immediate(snapshot_id, confirm_id, acknowledge)
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// The S8 D3 snapshot tag-seal backfill (file-sync.md § Sealed names &
    /// paths → Implementation status today): stamp `folder`'s snapshots
    /// whose `tags_sealed` is still missing (the immutable snapshot plane's
    /// only catch-up path — no later mutation re-seals a tagged snapshot).
    /// Call once per OWNED set (skip rows where the set's own `role` is
    /// `"member"` — a member's stamp is one the flip's scrub cannot
    /// attribute to the owner). `self.client()` already wires the resolver + owner
    /// `BackupKey` — no second derivation here.
    pub async fn backfill_tag_seals(
        &self,
        folder: String,
    ) -> Result<FfiTagSealBackfillReport, FfiError> {
        let report = self
            .client()
            .backfill_tag_seals(&folder)
            .await
            .map_err(stringify)?;
        Ok(FfiTagSealBackfillReport {
            stamped: report.stamped as u32,
            unsealable: report.unsealable as u32,
            stamp_failures: report.stamp_failures as u32,
        })
    }
}

/// FFI mirror of [`fauna_client_snapshots::TagSealBackfillReport`] (S8 D3).
/// All-zero = converged, the steady state.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiTagSealBackfillReport {
    pub stamped: u32,
    pub unsealable: u32,
    pub stamp_failures: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The regression pin — driven from the FAÇADE, not a replica of
    /// its custody** (twin of `folders_client`'s; full rationale there). The
    /// fauna-core pins cannot observe this crate reverting `client()` to
    /// `LabelCustody::owner_only` — proven. This pin builds the
    /// façade over an offline `NestClient` (`client()` only constructs, no
    /// connection is made) and asserts the custody the typed client will seal
    /// tags and render labels with carries both arms.
    ///
    /// Mutation: revert `client()` to owner-only → `has_resolver()` false →
    /// exactly this pin reds (`cargo test -p fauna-ffi --lib`, the same crate
    /// as the production change).
    #[cfg(all(feature = "folders-author", not(target_arch = "wasm32")))]
    #[test]
    fn the_facade_hands_its_client_a_resolver_wired_custody() {
        let nest = NestClient::new(
            "wss://unreachable.invalid".into(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        );
        let facade = FfiSnapshotsClient::from_nest(nest);
        let client = facade.client();
        let custody = client.label_custody();
        let custody_shape = (custody.has_resolver(), custody.has_owner_key());
        assert_eq!(
            custody_shape,
            (true, true),
            "the tags seal funnel needs BOTH arms: the resolver so a bound \
             set's tags seal under the M2 generation its roster can open \
             (owner-only custody here silently seals where no member \
             can follow, and a snapshot has no re-record arm to recover with), \
             and the owner key so an unbound set still seals and renders (the \
             positive-control arm)"
        );
    }

    /// The twin of the pin above, for the **read** seam every restore
    /// consumer now builds through. Same reasoning: a custody-shape pin
    /// in `fauna-core` cannot observe *this* crate handing a restore path a bare
    /// `SnapshotsClient::new`, which is precisely what it did at three sites
    /// until 2026-08-03.
    ///
    /// Mutation: drop the `with_label_custody` from
    /// `owner_read_snapshots_client` → `has_owner_key()` false → this reds.
    #[test]
    fn the_read_seam_is_always_keyed() {
        let nest = NestClient::new(
            "wss://unreachable.invalid".into(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        );
        let client = owner_read_snapshots_client(nest, &[7u8; 32]);
        assert!(
            client.label_custody().has_owner_key(),
            "a restore read with no owner key renders every sealed row to Omit, and the \
             caller then restores an empty directory and reports success — bug. The \
             owner arm is not optional on this seam in ANY build"
        );
    }

    /// The resolver arm rides the read seam too, so a **bound** set's rows
    /// render for a restore and are not silently dropped from it.
    #[cfg(all(feature = "folders-author", not(target_arch = "wasm32")))]
    #[test]
    fn the_read_seam_carries_the_resolver_in_a_default_build() {
        let nest = NestClient::new(
            "wss://unreachable.invalid".into(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        );
        let client = owner_read_snapshots_client(nest, &[7u8; 32]);
        assert!(
            client.label_custody().has_resolver(),
            "a restore of a shared set must reach its M2 generation, or the member's own \
             rows omit from their own restore"
        );
    }

    /// The modal displays this string for the user to retype AND validates the
    /// typed acknowledge against it; if it ever drifted from the nest's
    /// `IMMEDIATE_DELETE_ACK_TEXT` the confirm would enable but the nest would
    /// reject with `acknowledge_mismatch`. Pin them equal.
    #[cfg(feature = "snapshots-immediate-delete")]
    #[test]
    fn ack_text_matches_protocol_const() {
        assert_eq!(
            immediate_delete_ack_text(),
            fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT
        );
    }

    #[test]
    fn summary_mirror_carries_message_kind_and_device_id() {
        let row = FfiSnapshotSummary {
            id: 7,
            created_at: 1_700_000_000,
            message_kind: None,
            file_count: 3,
            total_bytes: 4096,
            device_id: Some(vec![0x44; 32]),
        };
        assert_eq!(row.id, 7);
        assert!(row.message_kind.is_none());
        assert_eq!(row.device_id.as_deref(), Some(&[0x44u8; 32][..]));
    }

    #[test]
    fn prune_reply_carries_candidates() {
        let reply = FfiSnapshotPruneReply {
            dry_run: true,
            pruned: 2,
            remaining: 5,
            snapshots: vec![FfiPrunableSnapshot {
                id: 11,
                created_at: 1_700_000_000,
                tags: vec!["manual".into()],
            }],
        };
        assert!(reply.dry_run);
        assert_eq!(reply.snapshots.len(), 1);
        assert_eq!(reply.snapshots[0].id, 11);
    }

    #[test]
    fn check_reply_carries_counts() {
        let reply = FfiSnapshotCheckReply {
            status: "ok".into(),
            is_ok: true,
            snapshots_checked: 4,
            files_checked: 40,
            manifests_checked: 40,
            chunks_checked: 100,
            missing_manifests: 0,
            missing_chunks: 0,
            corrupt_manifests: 0,
        };
        assert!(reply.is_ok);
        assert_eq!(reply.chunks_checked, 100);
    }

    #[test]
    fn diff_reply_mirror_holds_all_buckets() {
        let reply = FfiSnapshotDiffReply {
            snapshot_a: 1,
            snapshot_b: 2,
            added: vec![FfiSnapshotDiffEntry {
                path: "new.txt".into(),
                size_bytes: 10,
            }],
            removed: vec![],
            modified: vec![FfiSnapshotModifiedEntry {
                path: "a.txt".into(),
                old_size: 5,
                new_size: 7,
            }],
            summary: FfiSnapshotDiffSummary {
                added_count: 1,
                removed_count: 0,
                modified_count: 1,
                added_bytes: 10,
                removed_bytes: 0,
                net_bytes: 12,
            },
        };
        assert_eq!(reply.added[0].path, "new.txt");
        assert_eq!(reply.modified[0].new_size, 7);
        assert_eq!(reply.summary.net_bytes, 12);
    }

    #[test]
    fn get_reply_mirror_maps_device_id_and_files() {
        let reply = FfiSnapshotGetReply {
            id: 1,
            folder: "documents".into(),
            created_at: 1_700_000_000,
            file_count: 1,
            total_bytes: 512,
            tags: vec!["manual".into()],
            device_id: Some(vec![0x44; 32]),
            files: vec![FfiSnapshotFileEntry {
                path: "a/b.txt".into(),
                size_bytes: 512,
                file_type: "file".into(),
            }],
        };
        assert_eq!(reply.device_id.as_deref(), Some(&[0x44u8; 32][..]));
        assert_eq!(reply.files[0].path, "a/b.txt");
    }

    #[test]
    fn restore_history_row_none_source_is_local_snapshot() {
        let row = FfiRestoreHistoryRow {
            id: 3,
            completed_at: 1_700_000_000,
            snapshot_id: 42,
            kinds_restored: "mail".into(),
            source_member_id: None,
        };
        // `None` → "local snapshot" at the UI edge; `snapshot_id` keys the
        // per-row divergence read.
        assert!(row.source_member_id.is_none());
        assert_eq!(row.snapshot_id, 42);
        assert_eq!(row.kinds_restored, "mail");
    }

    #[test]
    fn divergence_row_carries_forensic_modseqs_and_mua() {
        let row = FfiRestoreDivergenceRow {
            id: 9,
            snapshot_id: 42,
            observed_at: 1_700_000_500,
            protocol: "imap".into(),
            collection: "INBOX".into(),
            mua_id: None,
            client_modseq: 120,
            server_modseq: 100,
            lost_event_count: 20,
        };
        // `mua_id` None → "(unknown)" at the UI edge; lost = client - server.
        assert!(row.mua_id.is_none());
        assert_eq!(row.lost_event_count, 20);
        assert_eq!(row.protocol, "imap");
    }

    #[test]
    fn restore_reply_carries_config_present_and_note() {
        let reply = FfiSnapshotRestoreReply {
            snapshot_id: 42,
            kind: "mail".into(),
            config_present: false,
            note: "WARN: wrapped-MLS-blob bundle not present; bridge AUTH will fail until it is restored".into(),
        };
        assert!(!reply.config_present);
        assert!(!reply.note.is_empty());
        assert_eq!(reply.kind, "mail");
    }
}
