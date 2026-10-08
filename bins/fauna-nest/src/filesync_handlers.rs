//! `fauna.filesync.snapshot.*` WS-RPC handlers. Two families:
//!
//! - **Message-kind snapshots** — create / immediate-delete / restore /
//!   owner-implicit list (+ restore history / divergence). Owner-implicit
//!   (the bearer actor IS the snapshot owner); self-enforced via
//!   `resolve_snapshot_owner`, no allowlist arm. The HTTP twins that
//!   briefly shipped these (`?kind=`, `?immediate=`, restore message-kind
//!   branch) were retired before any client consumed them.
//! - **Folder snapshot control** (Track B15) — create_folder / get /
//!   delete (queued 48 h) / undelete / prune / check / diff + the `list`
//!   fold-in. file_set-name / snapshot-id scoped → a two-layer gate: the
//!   `require_permission` (`User | Admin`) caller-class check, **plus** an
//!   owner gate (review N1 / the F1 IDOR): snapshot-id kinds go through
//!   `authorize_snapshot` (owner-equality, or conv-channel membership);
//!   file_set-name kinds resolve via `get_folder_for_actor` (caller-scoped
//!   lookup). The HTTP snapshot twins and `stats_routes.rs` were **DELETED**
//!   in the WS-RPC-everywhere rip, and the two legacy-plaintext byte routes
//!   (ZIP restore, single-file download) in the compat-remnant sweep; no
//!   snapshot route stays HTTP.
//!
//! See:
//! - `docs/goal/architecture/message-segment-store.md` § Manual compact
//!   endpoint / Immediate-delete override / Restore dispatch on
//!   message_kind.
//! - `docs/goal/behavior/backup-restore.md` § 5 (Message-kind
//!   snapshots) / § 6 (Message-kind restore) / § 7 (Layered safety).
//! - `docs/goal/architecture/api-layers.md` § WS-RPC migration status.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use fauna_core::identity::ActorId;
use fauna_protocol::filesync::{
    IMMEDIATE_DELETE_ACK_TEXT, PrunableSnapshot, SnapshotCheckReply, SnapshotCheckRequest,
    SnapshotCreateFolderReply, SnapshotCreateFolderRequest, SnapshotCreateMessageKindReply,
    SnapshotCreateMessageKindRequest, SnapshotDeleteImmediateReply, SnapshotDeleteImmediateRequest,
    SnapshotDeleteReply, SnapshotDeleteRequest, SnapshotDiffEntry, SnapshotDiffReply,
    SnapshotDiffRequest, SnapshotDiffSummary, SnapshotFileEntry, SnapshotGetReply,
    SnapshotGetRequest, SnapshotListReply, SnapshotListRequest, SnapshotModifiedEntry,
    SnapshotPruneReply, SnapshotPruneRequest, SnapshotPruneSetPolicyReply,
    SnapshotPruneSetPolicyRequest, SnapshotRestoreDivergenceListReply,
    SnapshotRestoreDivergenceListRequest, SnapshotRestoreHistoryListReply,
    SnapshotRestoreHistoryListRequest, SnapshotRestoreMessageKindReply,
    SnapshotRestoreMessageKindRequest, SnapshotStampLabelsReply, SnapshotStampLabelsRequest,
    SnapshotSummaryRow, SnapshotUndeleteReply, SnapshotUndeleteRequest, policy_state,
};
use fauna_protocol::{ByteBuf, RpcError, Value, decode_strict as decode, encode_canonical};
use fauna_segment_store::VersionedManifest;

use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

pub fn register_filesync_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.filesync.snapshot.create_message_kind",
        RpcKindMeta {
            forbid_replay: false,
            // load + serialize Manifest can be slow on large mail
            // spools (segments enumerated + footers parsed); match the
            // segments.compact deadline so identical scaling holds.
            default_deadline: Duration::from_secs(30),
            handler: create_message_kind_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.delete_immediate",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: delete_immediate_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.restore_message_kind",
        RpcKindMeta {
            forbid_replay: false,
            // Full restore: drop bridge_imap_* rows + segment_records
            // rebuild from on-disk segment footers.
            default_deadline: Duration::from_secs(60),
            handler: restore_message_kind_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.list_restore_history",
        RpcKindMeta {
            forbid_replay: false,
            // Read-only single-table SELECT, owner-scoped.
            default_deadline: Duration::from_secs(5),
            handler: list_restore_history_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.list_restore_divergence",
        RpcKindMeta {
            forbid_replay: false,
            // Read-only single-table SELECT, owner-checked.
            default_deadline: Duration::from_secs(5),
            handler: list_restore_divergence_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.list",
        RpcKindMeta {
            forbid_replay: false,
            // Read-only indexed SELECT (owner-scoped, or folder-scoped
            // in the B15 fold-in branch).
            default_deadline: Duration::from_secs(5),
            handler: list_handler(),
        },
    );
    // ── Folder snapshot control surface (Track B15) ───────────
    b.add(
        "fauna.filesync.snapshot.create_folder",
        RpcKindMeta {
            forbid_replay: false,
            // Enumerates the folder's current sync_changes to build the
            // snapshot row; match create_message_kind's deadline.
            default_deadline: Duration::from_secs(30),
            handler: create_folder_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.get",
        RpcKindMeta {
            forbid_replay: false,
            // Metadata SELECTs + file listing.
            default_deadline: Duration::from_secs(5),
            handler: get_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.stamp_labels",
        RpcKindMeta {
            // Idempotent by construction: re-stamping the same seal re-writes
            // the same opaque column value; nothing is minted or accumulated.
            forbid_replay: false,
            // One authz SELECT + one single-column UPDATE.
            default_deadline: Duration::from_secs(5),
            handler: stamp_labels_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.delete",
        RpcKindMeta {
            forbid_replay: false,
            // Hard-floor check + pending-action create (48 h queued delete).
            default_deadline: Duration::from_secs(5),
            handler: delete_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.undelete",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: undelete_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.prune",
        RpcKindMeta {
            forbid_replay: false,
            // Retention eval + (non-dry-run) bulk delete.
            default_deadline: Duration::from_secs(30),
            handler: prune_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.prune_set_policy",
        RpcKindMeta {
            forbid_replay: false,
            // Same work as `prune`, minus the client-supplied policy.
            default_deadline: Duration::from_secs(30),
            handler: prune_set_policy_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.check",
        RpcKindMeta {
            forbid_replay: false,
            // Integrity scan over the blob store (chunk reads).
            default_deadline: Duration::from_secs(60),
            handler: check_handler(),
        },
    );
    b.add(
        "fauna.filesync.snapshot.diff",
        RpcKindMeta {
            forbid_replay: false,
            // Enumerates the file lists of both snapshots.
            default_deadline: Duration::from_secs(30),
            handler: diff_handler(),
        },
    );
}

// ── Error helpers ──────────────────────────────────────────────

fn malformed(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::malformed_ns("filesync.snapshot", reason)
}

fn permission_denied(reason: &str) -> RpcError {
    crate::rpc_errors::permission_denied_ns("filesync.snapshot", reason)
}

fn not_found(reason: &str) -> RpcError {
    crate::rpc_errors::not_found_ns("filesync.snapshot", reason)
}

fn unknown_kind(reason: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.filesync.snapshot.unknown_kind",
        "error.filesync.snapshot.unknown_kind",
    );
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

fn pure_backup_destination() -> RpcError {
    crate::rpc_errors::pure_backup_destination_ns(
        "filesync.snapshot",
        "snapshot create requires local plaintext-framed segments; this destination holds opaque chunks only",
    )
}

fn confirm_mismatch() -> RpcError {
    let mut e = RpcError::new(
        "fauna.filesync.snapshot.confirm_mismatch",
        "error.filesync.snapshot.confirm_mismatch",
    );
    e.details = Some(Box::new(Value::String(
        "confirm_id does not match snapshot id".into(),
    )));
    e
}

fn acknowledge_mismatch() -> RpcError {
    let mut e = RpcError::new(
        "fauna.filesync.snapshot.acknowledge_mismatch",
        "error.filesync.snapshot.acknowledge_mismatch",
    );
    e.details = Some(Box::new(Value::String(
        "acknowledge text does not match".into(),
    )));
    e
}

fn hard_floor_breach() -> RpcError {
    let mut e = RpcError::new(
        "fauna.filesync.snapshot.hard_floor_breach",
        "error.filesync.snapshot.hard_floor_breach",
    );
    e.details = Some(Box::new(Value::String(
        "cannot delete snapshot: hard floor of 3 active snapshots would be breached".into(),
    )));
    e
}

fn bridge_active() -> RpcError {
    let mut e = RpcError::new(
        "fauna.filesync.snapshot.bridge_active",
        "error.filesync.snapshot.bridge_active",
    );
    e.details = Some(Box::new(Value::String(
        "bridge is serving this actor; stop the bridge before restoring".into(),
    )));
    e
}

fn lock_contention() -> RpcError {
    let mut e = RpcError::new(
        "fauna.filesync.snapshot.lock_contention",
        "error.filesync.snapshot.lock_contention",
    );
    e.details = Some(Box::new(Value::String(
        "snapshot GC or compaction in progress; retry after it completes".into(),
    )));
    e
}

fn internal(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns("filesync.snapshot", reason)
}

fn encode_reply<T: serde::Serialize>(reply: &T) -> Result<Bytes, RpcError> {
    encode_canonical(reply)
        .map(|v| Bytes::from(v.to_vec()))
        .map_err(|e| internal(format!("encode reply: {e}")))
}

fn invalid_request(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::invalid_request_ns("filesync.snapshot", reason)
}

/// Parse a wire `name_hash` into a fixed 32-byte array — see
/// `crate::routes::parse_name_hash`.
fn parse_name_hash(name_hash: &Option<ByteBuf>) -> Result<Option<[u8; 32]>, RpcError> {
    crate::routes::parse_name_hash(name_hash, |msg| invalid_request(msg))
}

fn not_soft_deleted() -> RpcError {
    let mut e = RpcError::new(
        "fauna.filesync.snapshot.not_soft_deleted",
        "error.filesync.snapshot.not_soft_deleted",
    );
    e.details = Some(Box::new(Value::String(
        "snapshot is not soft-deleted".into(),
    )));
    e
}

fn backup_unavailable() -> RpcError {
    let mut e = RpcError::new(
        "fauna.filesync.snapshot.backup_unavailable",
        "error.filesync.snapshot.backup_unavailable",
    );
    e.details = Some(Box::new(Value::String(
        "backup service not configured".into(),
    )));
    e
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm — the WS-RPC counterpart of the HTTP `BearerAuth` extractor gate. Used
/// by the folder control kinds (Track B15), which are file_set-name /
/// snapshot-id scoped rather than owner-implicit like the message-kind kinds.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// Resolve + authorize a **snapshot-id-scoped** operation, returning the loaded
/// `(snapshot, folder)` plus the [`FolderReadGrant`] naming **why** the caller
/// was admitted, so callers reuse all three without re-querying.
///
/// **The grant is returned, not collapsed to a bool** (path-sealing S5e). This gate admits a Q5 `AdminDiscovery` reader to a
/// group-bound set's `get`/`diff`; it used to discard which arm let them in, so
/// the reply planes *could not* project their sealed labels per reader even
/// though `encryption-at-rest.md` § Carve-outs requires it — `path_hash` "is
/// projected on the wire only to a label's audience". A typed answer the callers
/// throw away is an invariant with no enforcement, so it is threaded out here.
/// Write callers that have no label to project bind it as `_`.
///
/// Snapshot ids are global `AUTOINCREMENT` (`db/migrations.rs`), so the
/// `require_permission` caller-*class* gate alone lets any authenticated user
/// walk `1..N` across every other user's backups. This is the missing owner
/// gate (review N1 / the F1 IDOR). It mirrors the conv-membership branch of
/// `restore_message_kind`:
/// - **conv set** (`message_kind == "conv"`; the `__conv/<hex>` set carries
///   `folders.actor_id = channel_id`) → channel membership (or admin),
/// - **every other set** (regular backups, `__mail`/`__post`/… owner-scoped) →
///   strict owner-equality `actor == folders.actor_id`.
///
/// `not_found` (not `forbidden`) when the snapshot/folder row is absent,
/// matching the HTTP twin.
///
/// **S2-P3 (shared folders):** a **group-bound** folder (`mls_group_id IS NOT
/// NULL`, bound via `fauna.folders.share`) admits, on a [`SnapshotAccess::Read`]
/// (`get`/`diff`), any roster member of the derived
/// `ChannelId::from_group_id(mls_group_id)` channel (or admin) *alongside* the
/// owner — `crate::folder_authz::can_read_folder`. **Writes**
/// ([`SnapshotAccess::Write`] — `delete`/`undelete`) stay **owner-only** for a
/// group-bound set (the owner is the sole writer in Slice 2; members are
/// read-only). Owner-only sets and the conv branch are unchanged.
async fn authorize_snapshot(
    state: &AppState,
    snapshot_id: i64,
    actor: &[u8; 32],
    access: SnapshotAccess,
) -> Result<
    (
        crate::db::SnapshotRow,
        crate::db::FolderRow,
        crate::folder_authz::FolderReadGrant,
    ),
    RpcError,
> {
    use crate::folder_authz::FolderReadGrant;
    let snap = state
        .db
        .get_snapshot(snapshot_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| not_found("snapshot not found"))?;
    let fs = state
        .db
        .get_folder_by_id(snap.folder_id)
        .await
        .map_err(internal)?
        // A snapshot whose folder row is gone is unreachable to anyone.
        .ok_or_else(|| not_found("snapshot not found"))?;
    let scope: [u8; 32] = fs
        .actor_id
        .as_slice()
        .try_into()
        .map_err(|_| internal("folder actor_id is not 32 bytes"))?;
    let grant = if snap.message_kind.as_deref() == Some("conv") {
        // Conv scope key is the channel_id, not a user; authorize on roster
        // membership (admin override matches restore_message_kind). Conv is
        // collaborative — members read AND mutate, unchanged by S2-P3.
        let members = state
            .db
            .list_channel_actors(&scope)
            .await
            .map_err(internal)?;
        let is_admin = state.db.is_admin(actor).await.map_err(internal)?;
        if members.contains(actor) {
            // A channel participant holds the conversation's MLS key material,
            // so they are the audience of anything sealed under it.
            FolderReadGrant::Member
        } else if is_admin {
            // The admin override admits them without any key for the channel —
            // the same non-audience position the Q5 folder grant describes.
            FolderReadGrant::AdminDiscovery
        } else {
            return Err(permission_denied("not a member of this snapshot's channel"));
        }
    } else if fs.mls_group_id.is_some() {
        // S2-P3: a group-bound shared *file* set (message_kind = None). Reads
        // admit owner + roster member + admin; writes stay owner-only.
        match access {
            SnapshotAccess::Read => crate::folder_authz::can_read_folder(&state.db, &fs, actor)
                .await
                .map_err(internal)?
                .ok_or_else(|| permission_denied("not a member of this folder's group"))?,
            SnapshotAccess::Write => {
                if actor != &scope {
                    return Err(permission_denied(
                        "only the owner may modify this shared folder",
                    ));
                }
                FolderReadGrant::Owner
            }
        }
    } else if actor != &scope {
        return Err(permission_denied("not your snapshot"));
    } else {
        FolderReadGrant::Owner
    };
    Ok((snap, fs, grant))
}

/// Whether an [`authorize_snapshot`] call gates a **read** (`get`/`diff`) or a
/// **write** (`delete`/`undelete`). Only a group-bound shared folder
/// distinguishes the two (members read, owner-only writes); for owner-only sets
/// and conv channels the level is immaterial (the same actor set is admitted).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SnapshotAccess {
    Read,
    Write,
}

/// Decode an optional raw device id (the HTTP twin took it hex-encoded) into
/// the `[u8; 32]` `create_snapshot_v2` expects. Wrong length → invalid_request.
fn device_id_array(device_id: &Option<ByteBuf>) -> Result<Option<[u8; 32]>, RpcError> {
    match device_id {
        None => Ok(None),
        Some(b) => crate::rpc_errors::require_bytes32("device_id", b.as_ref())
            .map(Some)
            .map_err(invalid_request),
    }
}

// ── fauna.filesync.snapshot.create_message_kind ─────────────────

fn create_message_kind_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            let req: SnapshotCreateMessageKindRequest = decode(&payload).map_err(malformed)?;

            // Reject pure-backup destinations: snapshots pin a manifest
            // of local plaintext-framed segment files, which a pure-
            // backup destination does not have. Mail, post, calendar and
            // card are author-scoped, so they gate here on the bearer's own
            // actor; conv gates per-channel inside create_conv.
            if matches!(req.kind.as_str(), "mail" | "post" | "calendar" | "card") {
                crate::bridge_routing_handlers::refuse_if_pure_backup(
                    &state,
                    &req.kind,
                    &actor,
                    pure_backup_destination,
                )
                .await?;
            }

            // Conv snapshots scope to a channel (not the bearer's actor) and a
            // single create can pin N channels at once (batched "snapshot all
            // my channels"), so conv has its own multi-row block. mail/calendar
            // stay on the single-channel → single-row path below.
            if req.kind == "conv" {
                return create_conv(&state, &actor, req.actor_id).await;
            }

            let (content_blob, placement_blob): (Option<Vec<u8>>, Option<Vec<u8>>) =
                match req.kind.as_str() {
                    "mail" => {
                        // Pin both the content manifest and the placement
                        // manifest (backup-restore.md § 6 "serialises both
                        // manifests in the same SQLite transaction so a
                        // message-kind snapshot is never written with a stale
                        // content/placement pairing"). restore_mail replays the
                        // placement manifest into bridge_imap_*; a snapshot
                        // without it is un-restorable.
                        //
                        // finalize_open before load_manifest on *both* stores so
                        // the captured pair reflects all appended-but-unflushed
                        // records.
                        state
                            .mail_segments
                            .finalize_open(&actor)
                            .await
                            .map_err(internal)?;
                        let m = state
                            .mail_segments
                            .load_manifest(&actor)
                            .await
                            .map_err(internal)?;
                        let content = fauna_core::encoding::canonical_encode(&m)
                            .map_err(|e| internal(format!("Manifest: {e}")))?;

                        state
                            .mail_placement
                            .finalize_open(&actor)
                            .await
                            .map_err(internal)?;
                        let pm = state
                            .mail_placement
                            .load_manifest(&actor)
                            .await
                            .map_err(internal)?;
                        let placement = fauna_core::encoding::canonical_encode(&pm)
                            .map_err(|e| internal(format!("MailPlacementManifest: {e}")))?;

                        (Some(content), Some(placement))
                    }
                    "post" => {
                        // Posts have NO placement layer (unlike mail) —
                        // placement_manifest is NULL (like conv). Pin only the
                        // content manifest. finalize_open before load_manifest so
                        // the captured manifest reflects all appended-but-
                        // unflushed records.
                        state
                            .post_segments
                            .finalize_open(&actor)
                            .await
                            .map_err(internal)?;
                        let m = state
                            .post_segments
                            .load_manifest(&actor)
                            .await
                            .map_err(internal)?;
                        let content = fauna_core::encoding::canonical_encode(&m)
                            .map_err(|e| internal(format!("post Manifest: {e}")))?;
                        (Some(content), None)
                    }
                    "calendar" => {
                        // Mail's shape exactly (backup-restore.md § snapshot
                        // create): pin the content manifest AND the placement
                        // manifest together — restore_calendar replays the
                        // placement into bridge_caldav_* and rebuilds rows
                        // from the content manifest + disk segments; a
                        // snapshot without either half is un-restorable.
                        // finalize_open before load_manifest on both stores
                        // so the captured pair reflects all appended-but-
                        // unflushed records.
                        state
                            .cal_segments
                            .finalize_open(&actor)
                            .await
                            .map_err(internal)?;
                        let m = state
                            .cal_segments
                            .load_manifest(&actor)
                            .await
                            .map_err(internal)?;
                        let content = fauna_core::encoding::canonical_encode(&m)
                            .map_err(|e| internal(format!("calendar Manifest: {e}")))?;

                        state
                            .cal_placement
                            .finalize_open(&actor)
                            .await
                            .map_err(internal)?;
                        let pm = state
                            .cal_placement
                            .load_manifest(&actor)
                            .await
                            .map_err(internal)?;
                        let placement = pm
                            .encode()
                            .map_err(|e| internal(format!("CalPlacementManifest: {e}")))?;

                        (Some(content), Some(placement))
                    }
                    "card" => {
                        // Calendar's twin — the two DAV stores share one DR
                        // posture (carddav-server.md § Storage model).
                        state
                            .card_segments
                            .finalize_open(&actor)
                            .await
                            .map_err(internal)?;
                        let m = state
                            .card_segments
                            .load_manifest(&actor)
                            .await
                            .map_err(internal)?;
                        let content = fauna_core::encoding::canonical_encode(&m)
                            .map_err(|e| internal(format!("card Manifest: {e}")))?;

                        state
                            .card_placement
                            .finalize_open(&actor)
                            .await
                            .map_err(internal)?;
                        let pm = state
                            .card_placement
                            .load_manifest(&actor)
                            .await
                            .map_err(internal)?;
                        let placement = pm
                            .encode()
                            .map_err(|e| internal(format!("CardPlacementManifest: {e}")))?;

                        (Some(content), Some(placement))
                    }
                    _ => {
                        return Err(unknown_kind(
                            "kind must be 'mail', 'post', 'calendar', or 'card'",
                        ));
                    }
                };

            let fs_id = state
                .db
                .get_or_create_reserved_folder(&actor, &req.kind)
                .await
                .map_err(internal)?;

            let snap_id = state
                .db
                .create_message_kind_snapshot_row(
                    fs_id,
                    &req.kind,
                    content_blob.as_deref(),
                    placement_blob.as_deref(),
                )
                .await
                .map_err(internal)?;

            encode_reply(&SnapshotCreateMessageKindReply {
                snapshot_id: snap_id,
                snapshot_ids: vec![snap_id],
                kind: req.kind,
                actor_id: ActorId(actor),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

/// Create one conv snapshot per target channel (Plan 8 T4).
///
/// A conv snapshot pins the channel's segment `Manifest` (no placement layer
/// — `placement_manifest = NULL`), anchored to the `__conv/<channel_hex>`
/// reserved folder whose `actor_id` column carries the channel id.
///
/// Scope resolution:
/// - `Some(channel)` → per-channel snapshot; the bearer must be a current
///   channel member (`bearer ∈ list_channel_actors`), else `permission_denied`.
/// - `None` → batched snapshot of every channel the bearer belongs to
///   (`list_actor_channels(bearer)`). With zero channels this is a vacuous
///   success: `snapshot_ids = []`, `snapshot_id = 0`.
///
/// Authorization is channel-membership (not owner-equality), consistent with
/// conv ingest/reads (Plan 8 Decision 4).
async fn create_conv(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    scope: Option<ActorId>,
) -> Result<Bytes, RpcError> {
    let channels: Vec<[u8; 32]> = match scope {
        Some(ch) => {
            let members = state
                .db
                .list_channel_actors(&ch.0)
                .await
                .map_err(internal)?;
            if !members.contains(actor) {
                return Err(permission_denied("not a channel member"));
            }
            vec![ch.0]
        }
        None => state
            .db
            .list_actor_channels(actor)
            .await
            .map_err(internal)?,
    };

    // Gate 4 (Plan 9), hoisted above the row-creating loop below:
    // a pure-backup destination for any target channel holds opaque chunks
    // only — there is no local plaintext-framed manifest to pin. Refuse the
    // whole batched call before a single snapshot row is created, so a batch
    // that meets a pure-backup channel at position k never leaves rows 0..k-1
    // committed behind a reply that was never sent.
    for channel in &channels {
        crate::bridge_routing_handlers::refuse_if_pure_backup(
            state,
            "conv",
            channel,
            pure_backup_destination,
        )
        .await?;
    }

    // One snapshot row per channel. Each row carries its own create timestamp
    // via create_message_kind_snapshot_row's per-row tx; within a single
    // batched call these land within milliseconds (see the Plan 8 T4
    // consistent-T note — refactoring the row DAO to share one timestamp is
    // not warranted for the millisecond skew).
    let mut ids = Vec::with_capacity(channels.len());
    for channel in &channels {
        // finalize_open before load_manifest so the captured manifest reflects
        // all appended-but-unflushed records (mirrors the mail path).
        state
            .conv_segments
            .finalize_open(channel)
            .await
            .map_err(internal)?;
        let m = state
            .conv_segments
            .load_manifest(channel)
            .await
            .map_err(internal)?;
        let content = fauna_core::encoding::canonical_encode(&m)
            .map_err(|e| internal(format!("conv Manifest: {e}")))?;

        let fs_id = state
            .db
            .get_or_create_reserved_conv_folder(channel)
            .await
            .map_err(internal)?;
        let id = state
            .db
            .create_message_kind_snapshot_row(fs_id, "conv", Some(&content), None)
            .await
            .map_err(internal)?;
        ids.push(id);
    }

    encode_reply(&SnapshotCreateMessageKindReply {
        snapshot_id: ids.first().copied().unwrap_or(0),
        snapshot_ids: ids,
        kind: "conv".into(),
        actor_id: ActorId(*actor),
        extra: std::collections::BTreeMap::new(),
    })
}

// ── fauna.filesync.snapshot.delete_immediate ────────────────────

fn delete_immediate_handler() -> RpcHandler {
    Box::new(|state, bearer, payload| {
        Box::pin(async move {
            let req: SnapshotDeleteImmediateRequest = decode(&payload).map_err(malformed)?;

            let snap = state
                .db
                .get_snapshot(req.snapshot_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("snapshot not found"))?;

            // Hard floor (Layer 1) — owner-only path still enforces.
            let allowed = state
                .db
                .check_snapshot_delete_allowed(snap.folder_id)
                .await
                .map_err(internal)?;
            if !allowed {
                return Err(hard_floor_breach());
            }

            // Owner-only: resolve owner via folders join, compare to bearer.
            let owner = state
                .db
                .resolve_snapshot_owner(req.snapshot_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| internal("snapshot owner unresolved"))?;
            if bearer != owner {
                return Err(permission_denied("immediate delete is owner only"));
            }

            if req.confirm_id != req.snapshot_id.to_string() {
                return Err(confirm_mismatch());
            }
            if req.acknowledge != IMMEDIATE_DELETE_ACK_TEXT {
                return Err(acknowledge_mismatch());
            }

            state
                .db
                .hard_delete_snapshot(req.snapshot_id)
                .await
                .map_err(internal)?;

            encode_reply(&SnapshotDeleteImmediateReply {
                snapshot_id: req.snapshot_id,
                segment_retention_days: 14,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.filesync.snapshot.restore_message_kind ────────────────

fn restore_message_kind_handler() -> RpcHandler {
    Box::new(|state, bearer, payload| {
        Box::pin(async move {
            let req: SnapshotRestoreMessageKindRequest = decode(&payload).map_err(malformed)?;

            let snap = state
                .db
                .get_snapshot(req.snapshot_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("snapshot not found"))?;

            let kind = snap
                .message_kind
                .clone()
                .ok_or_else(|| unknown_kind("snapshot is not a message-kind snapshot"))?;

            if req.confirm_id != req.snapshot_id.to_string() {
                return Err(confirm_mismatch());
            }

            // `resolve_snapshot_owner` returns the folder's scope key: the
            // *actor* for mail/calendar, the *channel_id* for conv (the
            // `__conv/<hex>` reserved folder carries `actor_id = channel_id`).
            let owner = state
                .db
                .resolve_snapshot_owner(req.snapshot_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| internal("snapshot owner unresolved"))?;

            // Auth + restore-precondition gate, branched on kind (Plan 8
            // Decision 4). Conv authorizes on channel membership (owner is the
            // channel, so owner-equality is meaningless) and has no bridge
            // serving it; mail/calendar keep owner-equality + the bridge gate.
            if kind == "conv" {
                let members = state
                    .db
                    .list_channel_actors(&owner)
                    .await
                    .map_err(internal)?;
                let is_admin = state.db.is_admin(&bearer).await.map_err(internal)?;
                if !members.contains(&bearer) && !is_admin {
                    return Err(permission_denied(
                        "conv restore requires channel membership",
                    ));
                }
                // No bridge serves conversations — skip the bridge gate (the
                // predicate is a `false` stub today regardless).
            } else {
                if bearer != owner {
                    return Err(permission_denied("message-kind restore is owner only"));
                }

                // Pre-condition: bridge not serving this actor. Applies only to
                // bridge-served kinds (mail / calendar). Posts have no bridge
                // serving them (the feed read runs off the `content` projection,
                // not a bridge), so they skip it like conv. Predicate is stubbed
                // today (returns false) — flips real when
                // IMAP-restore Plan 1 lands.
                if kind != "post" {
                    let bridge_active_now = state
                        .db
                        .bridge_active_for_actor(&owner)
                        .await
                        .map_err(internal)?;
                    if bridge_active_now {
                        return Err(bridge_active());
                    }
                }
            }

            // Advisory: wrapped-MLS-blob presence (`has_wrapped_mls_blobs`). This is a
            // bridge-AUTH concern (mail/calendar bridges need the wrapped MLS
            // blob bundle). conv and post have no bridge serving them, so the
            // advisory is N/A — treat it as present so the reply carries no
            // spurious "bridge AUTH will fail" note on a successful restore.
            let config_present = if kind == "conv" || kind == "post" {
                true
            } else {
                state
                    .db
                    .has_wrapped_mls_blobs(&owner)
                    .await
                    .unwrap_or(false)
            };

            // Snapshot-GC advisory lock — mutually exclusive with
            // compaction + the ZIP-path "restore" lock.
            let lock_holder = format!("restore-{}", req.snapshot_id);
            let got_lock = state
                .db
                .try_acquire_op_lock("gc", -1, &lock_holder)
                .await
                .map_err(internal)?;
            if !got_lock {
                return Err(lock_contention());
            }

            let (content_blob, placement_blob) =
                match state.db.get_snapshot_kind_manifests(req.snapshot_id).await {
                    Ok(pair) => pair,
                    Err(e) => {
                        let _ = state.db.release_op_lock("gc", -1).await;
                        return Err(internal(format!("get_snapshot_kind_manifests: {e}")));
                    }
                };

            let outcome = match kind.as_str() {
                "mail" => {
                    restore_mail(
                        &state,
                        &owner,
                        content_blob.as_deref(),
                        placement_blob.as_deref(),
                    )
                    .await
                }
                "calendar" => {
                    restore_calendar(
                        &state,
                        &owner,
                        content_blob.as_deref(),
                        placement_blob.as_deref(),
                    )
                    .await
                }
                "card" => {
                    restore_card(
                        &state,
                        &owner,
                        content_blob.as_deref(),
                        placement_blob.as_deref(),
                    )
                    .await
                }
                "conv" => {
                    // `owner` is the channel_id (folder's scope key). Restore
                    // reverts the channel's conv segment_records to the pinned
                    // manifest and reinstates the restoring member's roster row.
                    restore_conv(&state, &owner, content_blob.as_deref(), &bearer).await
                }
                "post" => {
                    // `owner` is the post author (folder's scope key). Restore
                    // is ADDITIVE (no DELETE) — recovers the snapshot's posts into
                    // the mirror + feed-index projection without dropping newer
                    // posts (no-data-loss). No placement layer.
                    restore_post(&state, &owner, content_blob.as_deref()).await
                }
                _ => {
                    let _ = state.db.release_op_lock("gc", -1).await;
                    return Err(unknown_kind("snapshot has unrecognised message_kind"));
                }
            };

            let _ = state.db.release_op_lock("gc", -1).await;

            match outcome {
                Ok(()) => {
                    // Record under the bearer, not the folder's scope key:
                    // `list_restore_history` is bearer-scoped, so the restoring
                    // member must own the row. For mail/calendar bearer == owner
                    // (owner-equality enforced above) so this is equivalent; for
                    // conv the bearer is the member, the owner the channel.
                    if let Err(e) = state
                        .db
                        .insert_restore_history(&bearer, req.snapshot_id, &kind, None)
                        .await
                    {
                        tracing::warn!("restore_history insert: {e}");
                    }
                    let note = if config_present {
                        String::new()
                    } else {
                        "WARN: wrapped-MLS-blob bundle not present; bridge AUTH will fail until it is restored".to_string()
                    };
                    encode_reply(&SnapshotRestoreMessageKindReply {
                        snapshot_id: req.snapshot_id,
                        kind,
                        config_present,
                        note,
                        extra: std::collections::BTreeMap::new(),
                    })
                }
                Err(RestoreError::Other(e)) => {
                    tracing::error!("restore: {e:#}");
                    Err(internal("restore failed"))
                }
            }
        })
    })
}

// ── fauna.filesync.snapshot.list_restore_history ────────────────

fn list_restore_history_handler() -> RpcHandler {
    Box::new(|state, bearer, payload| {
        Box::pin(async move {
            let req: SnapshotRestoreHistoryListRequest = decode(&payload).map_err(malformed)?;
            // Owner-implicit: the bearer's WS-handshake actor scopes the
            // query; no snapshot_id on the wire. A bearer only ever sees
            // their own restore history.
            let rows = state
                .db
                .list_restore_history(&bearer, req.limit)
                .await
                .map_err(internal)?;
            encode_reply(&SnapshotRestoreHistoryListReply {
                rows,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.filesync.snapshot.list_restore_divergence ─────────────

fn list_restore_divergence_handler() -> RpcHandler {
    Box::new(|state, bearer, payload| {
        Box::pin(async move {
            let req: SnapshotRestoreDivergenceListRequest = decode(&payload).map_err(malformed)?;

            // Owner-only: divergence rows expose what a MUA lost, keyed to
            // a snapshot. Same auth gate as restore_message_kind — resolve
            // the snapshot's owner and require the bearer to match.
            let owner = state
                .db
                .resolve_snapshot_owner(req.snapshot_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("snapshot not found"))?;
            if bearer != owner {
                return Err(permission_denied("restore divergence is owner only"));
            }

            let rows = state
                .db
                .list_restore_divergence(req.snapshot_id)
                .await
                .map_err(internal)?;
            encode_reply(&SnapshotRestoreDivergenceListReply {
                rows,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.filesync.snapshot.list ────────────────────────────────

fn list_handler() -> RpcHandler {
    Box::new(|state, bearer, payload| {
        Box::pin(async move {
            // `User | Admin` gate (covers both modes; the message-kind
            // branch additionally self-scopes to the bearer's own rows).
            require_permission(&state, &bearer, "fauna.filesync.snapshot.list").await?;
            let req: SnapshotListRequest = decode(&payload).map_err(malformed)?;

            // Parsed unconditionally, like every other `name_hash`-accepting kind
            // — a malformed hash refuses here rather than being skipped on the
            // message-kind branch that does not read `folder`.
            let name_hash = parse_name_hash(&req.name_hash)?;

            // `name_hash` selects a set on its own: post-flip it is the only
            // selector a client has, so reading it only inside the
            // `folder.is_some()` arm would silently answer with the bearer's
            // message-kind rows instead of the set they addressed.
            let rows = if req.folder.is_some() || name_hash.is_some() {
                // B15 folder fold-in: file_set-name scoped, behavior-
                // preserving vs the `GET /api/v1/snapshots?folder=` twin
                // (returns all rows for the set, incl. soft-deleted).
                // Read access (N1 + S2-P3): resolve the caller's own (name, actor)
                // row OR a group-bound shared set of that name they're a member of
                // — never the name-only `get_folder` (cross-user list). A
                // not-found / not-yours both fold to `not_found` (ST-RES-1).
                // Hash-first when present, so the empty name is never read.
                let name = req.folder.as_deref().unwrap_or("");
                let fs = crate::folder_authz::resolve_readable_folder(
                    &state.db,
                    name,
                    name_hash.as_ref(),
                    &bearer,
                )
                .await
                .map_err(internal)?
                .ok_or_else(|| not_found("folder not found"))?;
                let snaps = state.db.list_snapshots(fs.id).await.map_err(internal)?;
                let cap = if req.limit == 0 {
                    snaps.len()
                } else {
                    req.limit as usize
                };
                // The § 2 lifecycle deadline join. This arm deliberately
                // returns soft-deleted and deletion-pending rows, and until
                // these fields shipped every app rendered them exactly like
                // active ones — so the § 7 windows were invisible from every
                // UI. Three of the four fields ride the snapshot row itself;
                // `execute_after` lives on the pending action, hence the join.
                // Scoped to the set's OWNER, who creates the actions, rather
                // than to the reading bearer — a roster member reading a
                // group-bound set must see the same deadline the owner does.
                let deadlines = state
                    .db
                    .pending_snapshot_deletion_deadlines(&fs.actor_id)
                    .await
                    .map_err(internal)?;
                snaps
                    .iter()
                    .take(cap)
                    .map(|s| SnapshotSummaryRow {
                        id: s.id,
                        created_at: s.created_at,
                        message_kind: s.message_kind.clone(),
                        file_count: s.file_count,
                        total_bytes: s.total_bytes,
                        device_id: s.device_id.clone().map(ByteBuf::from),
                        soft_deleted: s.soft_deleted,
                        purge_after: s.purge_after,
                        deletion_pending: s.deletion_pending,
                        // Only meaningful while a window is actually open: a
                        // row whose action already fired is soft-deleted, and
                        // its remaining window is `purge_after`.
                        execute_after: s
                            .deletion_pending
                            .then(|| deadlines.get(&s.id).copied())
                            .flatten(),
                        // The seal rides UNGATED because every reader of this
                        // arm is the label audience by construction:
                        // `resolve_readable_folder` above has NO admin arm
                        // (owner / roster member only). The plaintext `tags`
                        // wire field stays empty: the tags never rest.
                        tags: None,
                        tags_sealed: s.tags_sealed.clone().map(ByteBuf::from),
                        extra: Default::default(),
                    })
                    .collect()
            } else {
                // Owner-implicit message-kind list (original behavior): the
                // bearer's WS-handshake actor scopes the query.
                state
                    .db
                    .list_message_kind_snapshots(&bearer, req.message_kind.as_deref(), req.limit)
                    .await
                    .map_err(internal)?
            };
            encode_reply(&SnapshotListReply {
                rows,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── Folder snapshot control handlers (Track B15) ──────────────
//
// These replaced the deleted HTTP snapshot twins (+ the diff twin in the
// deleted `stats_routes.rs`) — a behavior-preserving transport flip. Unlike
// the message-kind kinds above, they are file_set-name / snapshot-id
// scoped, so they layer an owner gate over the `require_permission`
// (`User | Admin`) caller-class check (review N1): snapshot-id kinds via
// `authorize_snapshot`, file_set-name kinds via the caller-scoped
// `get_folder_for_actor` lookup — together the equivalent of the HTTP
// twins' `authorize_snapshot_owner` on top of `BearerAuth`. Each is
// `db::*` / `backup::*` calls + reply shaping (no `_core` — the twins
// have no inline business logic to share).

// ── fauna.filesync.snapshot.create_folder ─────────────────────

fn create_folder_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.filesync.snapshot.create_folder").await?;
            let req: SnapshotCreateFolderRequest = decode(&payload).map_err(malformed)?;
            let device_id = device_id_array(&req.device_id)?;

            // Owner-scoped lookup (N1): resolve the caller's own folder by
            // (name, actor), never the name-only `get_folder` which returns
            // whichever actor's row sorts first for a colliding name.
            let name_hash = parse_name_hash(&req.name_hash)?;
            let fs = match name_hash {
                Some(h) => state.db.get_folder_for_actor_by_name_hash(&h, &actor).await,
                None => state.db.get_folder_for_actor(&req.folder, &actor).await,
            }
            .map_err(internal)?
            .ok_or_else(|| not_found("folder not found"))?;

            // A reserved (`__`) backup-type set is a cross-location backup
            // DESTINATION (custodian-held latest-per-path custody) — snapshot
            // pins would defeat its reclamation contract, so create refuses,
            // exactly like the message-kind four-gate above. Ordinary
            // (non-reserved) backup-type sets are the wizard's point-in-time
            // surface and snapshot from custody (`backup-restore.md` § 1).
            if crate::db::snapshots::is_reserved_custody_copy(fs.custody_copy, &fs.name) {
                return Err(pure_backup_destination());
            }

            // Parent = the most recent snapshot for this folder.
            let parent_id = state
                .db
                .list_snapshots(fs.id)
                .await
                .ok()
                .and_then(|s| s.first().map(|x| x.id));

            // The tag display copy's seal rides the request opaquely (S6-d): this
            // handler is the only snapshot create a client reaches, so it is the
            // only place the seal can enter — there is no bind/serve pass that
            // revisits a snapshot row the way one revisits a folder row.
            let snap = state
                .db
                .create_snapshot_v2(
                    fs.id,
                    device_id.as_ref(),
                    &req.tags,
                    req.tags_sealed.as_ref().map(|b| &b[..]),
                    parent_id,
                )
                .await
                .map_err(internal)?;
            let _ = state.db.update_folder_cache(fs.id).await;

            encode_reply(&SnapshotCreateFolderReply {
                id: snap.id,
                file_count: snap.file_count,
                total_bytes: snap.total_bytes,
                created_at: snap.created_at,
                parent_id: snap.parent_id,
                // Echo the caller's own wire tags: the row rests none of them
                // (only their hashes and seal), but the CREATE reply goes only to the
                // creator, who sent these bytes one frame ago — echoing keeps
                // the reply shape whole with zero disclosure.
                tags: req.tags.clone(),
                device_id: snap.device_id.clone().map(ByteBuf::from),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.filesync.snapshot.get ─────────────────────────────────

fn get_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.filesync.snapshot.get").await?;
            let req: SnapshotGetRequest = decode(&payload).map_err(malformed)?;

            // Owner/conv-membership/group-member read gate (N1 + S2-P3) — reuse
            // the loaded rows.
            let (snap, fs, grant) =
                authorize_snapshot(&state, req.snapshot_id, &actor, SnapshotAccess::Read).await?;
            let files = state
                .db
                .get_snapshot_files(req.snapshot_id)
                .await
                .map_err(internal)?;

            // For message-kind snapshots, resolve the owner actor.
            let actor_id = if snap.message_kind.is_some() {
                state
                    .db
                    .resolve_snapshot_owner(snap.id)
                    .await
                    .map_err(internal)?
                    .map(ActorId)
            } else {
                None
            };

            // The path label pair, projected to this reader (path-sealing S5e). Same gate and same reasoning as
            // `MediaItem`'s (S5d, `media_handlers.rs`): a Q5 `AdminDiscovery`
            // reader holds no key for a group-bound set, so the seal is useless
            // to them and its salt — an unkeyed digest of a dictionary-shaped
            // path — would hand back offline exactly what the seal withholds
            // (`encryption-at-rest.md` § Carve-outs: projected on the wire only
            // to a label's audience).
            let labels_visible = grant.is_label_audience();
            let files = files
                .iter()
                .map(|f| SnapshotFileEntry {
                    // "" = the ratified scrub sentinel: the plaintext column
                    // died with the S9 flip's table rebuild; an audience
                    // reader renders from the sealed pair below.
                    path: String::new(),
                    manifest_hash: ByteBuf::from(f.manifest_hash.clone()),
                    size_bytes: f.size_bytes,
                    mtime: f.mtime,
                    mode: f.mode,
                    file_type: f.file_type.clone(),
                    symlink_target: f.symlink_target.clone(),
                    // Copied verbatim from the snapshot row for the audience —
                    // this nest holds no key that opens it (`file-sync.md`
                    // § Sealed names & paths) — and withheld entirely otherwise.
                    path_hash: labels_visible.then(|| ByteBuf::from(f.path_hash.clone())),
                    path_sealed: labels_visible
                        .then(|| f.path_sealed.clone().map(ByteBuf::from))
                        .flatten(),
                    extra: Default::default(),
                })
                .collect();

            encode_reply(&SnapshotGetReply {
                id: snap.id,
                folder: fs.name,
                created_at: snap.created_at,
                file_count: snap.file_count,
                total_bytes: snap.total_bytes,
                parent_id: snap.parent_id,
                // The plaintext field stays empty: the tags never rest, and
                // their display copy is `tags_sealed` below.
                tags: Vec::new(),
                device_id: snap.device_id.clone().map(ByteBuf::from),
                message_kind: snap.message_kind.clone(),
                actor_id,
                files,
                // The tag display copy and the set-name pair, on the same gate and
                // for the same reason as the per-file pair above (S6-d). The
                // set-name pair is new on this reply — `diff` has carried it since
                // S5c-2, while `get` shipped only the plaintext `folder`, so
                // post-flip this reply's set name had no carrier at all. It is
                // also `tags_sealed`'s **salt**, so the two ship or withhold as
                // one: a seal whose salt is withheld is unopenable, and a salt
                // whose seal is withheld is a bare dictionary handle.
                tags_sealed: labels_visible
                    .then(|| snap.tags_sealed.clone().map(ByteBuf::from))
                    .flatten(),
                folder_sealed: labels_visible
                    .then(|| fs.name_sealed.clone().map(ByteBuf::from))
                    .flatten(),
                folder_hash: labels_visible
                    .then(|| fs.name_hash.clone().map(ByteBuf::from))
                    .flatten(),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.filesync.snapshot.stamp_labels (S8 D3) ────────────────

fn stamp_labels_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.filesync.snapshot.stamp_labels").await?;
            let req: SnapshotStampLabelsRequest = decode(&payload).map_err(malformed)?;

            // `Read`, not `Write`: a roster MEMBER is part of the seal's
            // audience and must be able to (re-)stamp — the window
            // re-seal explicitly relies on "any audience client" converging a
            // wrong-root seal, and writes-are-owner-only would strand a set
            // whose owner's client never runs the pass. The write this kind
            // performs is confined to the one opaque column below.
            let (snap, _fs, grant) =
                authorize_snapshot(&state, req.snapshot_id, &actor, SnapshotAccess::Read).await?;

            // Label-audience only — the whole point of the seal is that a Q5
            // admin can neither open one nor plant one (a planted seal would
            // render as roster-visible tags the roster never wrote).
            if !grant.is_label_audience() {
                return Err(permission_denied(
                    "stamping a snapshot's sealed labels needs the label audience",
                ));
            }

            // Stamp-only means the seal can never CONJURE tags: a snapshot
            // with nothing to seal refuses. Keyed on the nest-computed
            // `tag_hashes`, the one resting trace of the create's tags.
            let has_tags = snap
                .tag_hashes
                .as_deref()
                .is_some_and(|h| h != "[]" && !h.is_empty());
            if !has_tags {
                return Err(invalid_request("snapshot has no tags to seal"));
            }

            // Overwrite is allowed but NOT unconditional.
            // The one licensed re-stamp is the window axis upgrade: an
            // owner-root `gen: None` seal resting on a *bound* set's snapshot,
            // which no roster member can open. Everything else is a first
            // stamp onto an empty column. Before the fix this was a bare
            // UPDATE, so any label-audience member could replace the owner's
            // snapshot labels arbitrarily and repeatedly — and after the S9
            // plaintext scrub the planted rendering would be the only one
            // left. The nest still holds no key: it reads the envelope
            // HEADER's generation (unsealed metadata by design — it is what
            // lets a keyless server-side row copy stay openable), never the
            // ciphertext.
            //
            // ⚠ The predicate reads the RESTING bytes only, never the incoming
            // ones. Format-validating an incoming envelope would make an older
            // nest refuse a newer client's envelope revision — a
            // bidirectional-compatibility break (`version-compatibility.md`).
            // Reading only what already rests keeps every unknown shape on the
            // permissive side.
            let resting = snap.tags_sealed.as_deref();
            // The OWNER re-stamps unconditionally: the freeze
            // below reads the resting envelope's `gen` header, which is
            // unauthenticated plaintext — a member can plant `gen: Some(n)`
            // over garbage ciphertext with no keys at all, and before this arm
            // that plant froze out everyone, the owner included, forever
            // (snapshots are immutable; deleting the snapshot was the only
            // escape). The set owner is the display plane's ultimate authority,
            // so their honest stamp always wins; a *member* still cannot
            // replace a generation-rooted seal, which is the whole point of the fix.
            let licensed = grant == crate::folder_authz::FolderReadGrant::Owner
                || match resting {
                    // First stamp — the backfill's ordinary case.
                    None => true,
                    // A seal that already names a key generation is roster-rooted:
                    // its audience can open it, so there is nothing to converge and
                    // no licensed reason to replace it. Anything else — an
                    // owner-root `gen: None` seal, or an envelope that parses for
                    // nobody — is the window residue the axis re-stamp exists
                    // to fix.
                    Some(bytes) => fauna_core::path_crypto::SealedLabel::from_bytes(bytes)
                        .map(|e| e.generation.is_none())
                        .unwrap_or(true),
                };
            if !licensed {
                return Err(invalid_request(
                    "this snapshot's labels are already sealed to the roster; \
                     only the owner-root → roster-root axis upgrade may re-stamp",
                ));
            }

            // CAS on the exact bytes the predicate decided against, so a
            // concurrent stamp cannot slip between the read and the write.
            let swapped = state
                .db
                .set_snapshot_tags_sealed(req.snapshot_id, &req.tags_sealed, resting)
                .await
                .map_err(internal)?;
            if !swapped {
                return Err(invalid_request(
                    "the snapshot's sealed labels changed under this stamp — re-read and retry",
                ));
            }

            encode_reply(&SnapshotStampLabelsReply {
                ok: true,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.filesync.snapshot.delete (queued 48 h pending action) ──

fn delete_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.filesync.snapshot.delete").await?;
            let req: SnapshotDeleteRequest = decode(&payload).map_err(malformed)?;

            // Owner/conv-membership gate (N1) — before any state-changing work.
            // A group-bound shared set is owner-only for writes (members read-only).
            let (snap, _fs, _grant) =
                authorize_snapshot(&state, req.snapshot_id, &actor, SnapshotAccess::Write).await?;

            // Hard floor of 3 active snapshots (Layer 1).
            if !state
                .db
                .check_snapshot_delete_allowed(snap.folder_id)
                .await
                .map_err(internal)?
            {
                return Err(hard_floor_breach());
            }

            let target = req.snapshot_id.to_string();
            let row = crate::pending_actions::schedule(
                &state,
                &crate::pending_actions::ActionType::SnapshotDelete,
                &actor,
                Some(target.as_str()),
                None,
            )
            .await
            .map_err(internal)?;
            let action_id = row.id;

            state
                .db
                .mark_snapshot_deletion_pending(req.snapshot_id)
                .await
                .map_err(internal)?;

            encode_reply(&SnapshotDeleteReply {
                pending_action_id: action_id,
                execute_after: row.execute_after,
                status: "pending".into(),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.filesync.snapshot.undelete ────────────────────────────

fn undelete_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.filesync.snapshot.undelete").await?;
            let req: SnapshotUndeleteRequest = decode(&payload).map_err(malformed)?;

            // Owner/conv-membership gate (N1) — before reinstating the snapshot.
            // Write op → owner-only for a group-bound shared set.
            let (snap, _fs, _grant) =
                authorize_snapshot(&state, req.snapshot_id, &actor, SnapshotAccess::Write).await?;
            if !snap.soft_deleted {
                return Err(not_soft_deleted());
            }

            state
                .db
                .undelete_snapshot(req.snapshot_id)
                .await
                .map_err(internal)?;
            encode_reply(&SnapshotUndeleteReply {
                undeleted: true,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.filesync.snapshot.prune ───────────────────────────────

fn prune_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.filesync.snapshot.prune").await?;
            let req: SnapshotPruneRequest = decode(&payload).map_err(malformed)?;

            // Owner-scoped lookup (N1) — see create_folder.
            let name_hash = parse_name_hash(&req.name_hash)?;
            let fs = match name_hash {
                Some(h) => state.db.get_folder_for_actor_by_name_hash(&h, &actor).await,
                None => state.db.get_folder_for_actor(&req.folder, &actor).await,
            }
            .map_err(internal)?
            .ok_or_else(|| not_found("folder not found"))?;
            // § 7: the candidate population is the ACTIVE snapshots only — the
            // predicate shared with the automatic path (`backup::prune`). The
            // unfiltered list also carries `soft_deleted` rows (30-day undelete
            // window open) and `deletion_pending` rows (7-day cancel window
            // open); pruning those is user-data loss, and *counting* them makes
            // the policy bind over rows the user already deleted, so the bound
            // they chose never binds.
            let snaps: Vec<_> = state
                .db
                .list_snapshots(fs.id)
                .await
                .map_err(internal)?
                .into_iter()
                .filter(crate::backup::prune::is_active_snapshot)
                .collect();

            use crate::backup::retention::{
                RetentionPolicy, SNAPSHOT_HARD_FLOOR, SnapshotMeta, evaluate_retention,
            };
            let policy = RetentionPolicy {
                keep_last: req.policy.keep_last,
                keep_hourly: req.policy.keep_hourly,
                keep_daily: req.policy.keep_daily,
                keep_weekly: req.policy.keep_weekly,
                keep_monthly: req.policy.keep_monthly,
                keep_yearly: req.policy.keep_yearly,
                keep_tags: req.policy.keep_tags.clone(),
                keep_within_secs: req.policy.keep_within_secs,
            };

            // evaluate_retention wants oldest-first; list_snapshots is newest-first.
            let mut metas: Vec<SnapshotMeta> = snaps
                .iter()
                .map(|s| SnapshotMeta::from_tag_hashes(s.id, s.created_at, s.tag_hashes.as_deref()))
                .collect();
            metas.reverse();
            let mut prunable_ids = evaluate_retention(&policy, &metas);

            // § 7 Layer 1: `evaluate_retention` (the union engine) has no floor
            // of its own, so clamp its output here, mirroring
            // `evaluate_folder_retention_at`'s guarantee 2: `prunable_ids` is
            // oldest-first, so truncation hands the newest candidates back
            // until at least `SNAPSHOT_HARD_FLOOR` active snapshots remain —
            // the same bound the single-snapshot delete refuses to cross.

            let keep_at_least =
                SNAPSHOT_HARD_FLOOR.saturating_sub(snaps.len() - prunable_ids.len());
            if keep_at_least > 0 {
                prunable_ids.truncate(prunable_ids.len().saturating_sub(keep_at_least));
            }

            if req.dry_run {
                let candidates = metas
                    .iter()
                    .filter(|m| prunable_ids.contains(&m.id))
                    .map(|m| PrunableSnapshot {
                        id: m.id,
                        created_at: m.created_at,
                        // The tags never rest, so a dry run names none.
                        tags: Vec::new(),
                        extra: Default::default(),
                    })
                    .collect();
                return encode_reply(&SnapshotPruneReply {
                    dry_run: true,
                    pruned: prunable_ids.len() as i64,
                    remaining: (snaps.len() - prunable_ids.len()) as i64,
                    snapshots: candidates,
                    extra: std::collections::BTreeMap::new(),
                });
            }

            if prunable_ids.is_empty() {
                return encode_reply(&SnapshotPruneReply {
                    dry_run: false,
                    pruned: 0,
                    remaining: snaps.len() as i64,
                    snapshots: Vec::new(),
                    extra: std::collections::BTreeMap::new(),
                });
            }

            // § 7 Layer 3: *soft*-delete, never the purge primitive — each
            // pruned snapshot opens the 30-day `purge_after` recovery window
            // `fauna.filesync.snapshot.undelete` serves; only GC's phase 3
            // purges. Layer 2 (a cancellable delay) is deliberately NOT
            // interposed on this kind: the user asked for this prune
            // synchronously, and the ruling holds that soft-delete
            // plus the undelete window satisfies no-user-data-loss on its own.
            // `pruned` therefore stays truthful for both shipped callers.
            for id in &prunable_ids {
                state.db.soft_delete_snapshot(*id).await.map_err(internal)?;
            }
            encode_reply(&SnapshotPruneReply {
                dry_run: false,
                pruned: prunable_ids.len() as i64,
                remaining: (snaps.len() - prunable_ids.len()) as i64,
                snapshots: Vec::new(),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.filesync.snapshot.prune_set_policy ────────────────────

/// Apply the folder's **own resting** retention policy, preview then execute.
///
/// The Backups page's `snapshot-prune-button` means *"apply what the wizard
/// recorded for this set"*, which [`prune_handler`] structurally cannot serve:
/// it takes a client-supplied `keep_*` union policy, and `backup-restore.md`
/// § 8 forbids re-mapping the shipped 2-field bounds onto that vocabulary. The
/// measured consequence was that every app invented its own policy for the
/// button — `keep_last 3` on linux/web/android, a daily/weekly/monthly triple
/// on windows, a hand-edited sheet on apple — so one button pruned five
/// different ways and never applied what the user actually chose.
///
/// This handler therefore carries **no policy on the wire at all**. It reads
/// the column and evaluates through the *same armed path* as the automatic
/// prune (`parse_folder_retention` + `evaluate_folder_retention_at`), so
/// the button and the scheduler cannot disagree about what the user's bounds
/// mean — which is the whole point of the kind existing.
fn prune_set_policy_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.filesync.snapshot.prune_set_policy").await?;
            let req: SnapshotPruneSetPolicyRequest = decode(&payload).map_err(malformed)?;

            // Owner-scoped lookup (N1) — the same gate `prune` uses. A prune
            // is a write; a roster member reading a group-bound set may not
            // launch one.
            let name_hash = parse_name_hash(&req.name_hash)?;
            let fs = match name_hash {
                Some(h) => state.db.get_folder_for_actor_by_name_hash(&h, &actor).await,
                None => state.db.get_folder_for_actor(&req.folder, &actor).await,
            }
            .map_err(internal)?
            .ok_or_else(|| not_found("folder not found"))?;

            use crate::backup::retention::{
                FolderRetention, evaluate_folder_retention_at, parse_folder_retention,
            };

            // Both non-policy arms prune nothing and say which one it was, so
            // the page can render *why* the count is zero instead of an empty
            // success. `remaining` still reports the real active count — the
            // user asked what this set holds, and the answer does not depend
            // on whether a policy exists.
            let policy = match parse_folder_retention(fs.retention_policy.as_deref()) {
                FolderRetention::Policy(p) => p,
                arm => {
                    if arm == FolderRetention::Unparseable {
                        // Same loud refusal as the automatic path: a writer
                        // disagreeing with the canonical 2-field shape is a
                        // live at-rest condition (an off-shape writer
                        // leaves a non-canonical retention JSON here, and such rows are
                        // deliberately left — `backup-restore.md` § 8 → *The
                        // off-shape at-rest residual*), and guessing would
                        // prune against a number nobody chose.
                        tracing::warn!(
                            folder = %fauna_core::log_redact::log_folder_name(&fs.name),
                            "retention policy is not the canonical {{max_snapshots, max_age_days}} \
                             shape — pruning nothing (backup-restore.md § 8)"
                        );
                    }
                    let active = state
                        .db
                        .list_snapshots(fs.id)
                        .await
                        .map_err(internal)?
                        .into_iter()
                        .filter(crate::backup::prune::is_active_snapshot)
                        .count();
                    return encode_reply(&SnapshotPruneSetPolicyReply {
                        dry_run: req.dry_run,
                        pruned: 0,
                        remaining: active as i64,
                        snapshots: Vec::new(),
                        policy_state: if arm == FolderRetention::Unparseable {
                            policy_state::UNPARSEABLE.into()
                        } else {
                            policy_state::NOT_SET.into()
                        },
                        extra: std::collections::BTreeMap::new(),
                    });
                }
            };

            // § 7: the candidate population is the ACTIVE snapshots only —
            // the shared predicate, so this kind's floor arithmetic and
            // Layer 1's agree by construction. Counting the recovering rows
            // would make the user's bound bind over snapshots they already
            // deleted, so the bound they chose would never bind (the same trap the explicit kind fell into).
            let snaps: Vec<_> = state
                .db
                .list_snapshots(fs.id)
                .await
                .map_err(internal)?
                .into_iter()
                .filter(crate::backup::prune::is_active_snapshot)
                .collect();

            // `list_snapshots` is newest-first; the evaluator wants oldest-first.
            let mut metas: Vec<crate::backup::retention::SnapshotMeta> = snaps
                .iter()
                .map(|s| {
                    crate::backup::retention::SnapshotMeta::from_tag_hashes(
                        s.id,
                        s.created_at,
                        s.tag_hashes.as_deref(),
                    )
                })
                .collect();
            metas.reverse();

            // The evaluator owns all three guarantees (tagged spared,
            // >= SNAPSHOT_HARD_FLOOR remain, newest kept) — unlike the union
            // engine, which needs the floor clamped onto it afterwards. No
            // clamp here is therefore correct, not an omission.
            let prunable_ids =
                evaluate_folder_retention_at(&policy, &metas, crate::db::now_epoch_secs());
            let remaining = (snaps.len() - prunable_ids.len()) as i64;

            if req.dry_run {
                let candidates = metas
                    .iter()
                    .filter(|m| prunable_ids.contains(&m.id))
                    .map(|m| PrunableSnapshot {
                        id: m.id,
                        created_at: m.created_at,
                        // The tags never rest, so a dry run names none.
                        tags: Vec::new(),
                        extra: Default::default(),
                    })
                    .collect();
                return encode_reply(&SnapshotPruneSetPolicyReply {
                    dry_run: true,
                    pruned: prunable_ids.len() as i64,
                    remaining,
                    snapshots: candidates,
                    policy_state: policy_state::APPLIED.into(),
                    extra: std::collections::BTreeMap::new(),
                });
            }

            // § 7 Layer 3: *soft*-delete, never the purge primitive — each
            // pruned snapshot opens the 30-day `purge_after` window
            // `fauna.filesync.snapshot.undelete` serves; only GC's phase 3
            // purges. Layer 2 (a cancellable delay) is deliberately NOT
            // interposed, the same ruling the explicit kind carries:
            // a user-requested, previewed, synchronous prune with an undelete
            // window satisfies no-user-data-loss on its own.
            for id in &prunable_ids {
                state.db.soft_delete_snapshot(*id).await.map_err(internal)?;
            }
            encode_reply(&SnapshotPruneSetPolicyReply {
                dry_run: false,
                pruned: prunable_ids.len() as i64,
                remaining,
                snapshots: Vec::new(),
                policy_state: policy_state::APPLIED.into(),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.filesync.snapshot.check ───────────────────────────────

fn check_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.filesync.snapshot.check").await?;
            let req: SnapshotCheckRequest = decode(&payload).map_err(malformed)?;

            // Owner-scoped lookup (N1) — see create_folder.
            let name_hash = parse_name_hash(&req.name_hash)?;
            let fs = match name_hash {
                Some(h) => state.db.get_folder_for_actor_by_name_hash(&h, &actor).await,
                None => state.db.get_folder_for_actor(&req.folder, &actor).await,
            }
            .map_err(internal)?
            .ok_or_else(|| not_found("folder not found"))?;

            let backup_svc = state
                .backup_service
                .as_ref()
                .ok_or_else(backup_unavailable)?;
            let blob_store = backup_svc.local_blob_store();

            let result = crate::backup::check::integrity_check(
                &state.db,
                &blob_store,
                fs.id,
                req.verify_content,
                backup_svc.encryption_key(),
            )
            .await
            .map_err(internal)?;

            let status = if result.structured_errors.is_empty() {
                "ok"
            } else {
                "errors"
            };
            encode_reply(&SnapshotCheckReply {
                status: status.into(),
                snapshots_checked: result.snapshots_checked as i64,
                files_checked: result.files_checked as i64,
                manifests_checked: result.manifests_checked as i64,
                chunks_checked: result.chunks_checked as i64,
                missing_manifests: result.missing_manifests as i64,
                missing_chunks: result.missing_chunks as i64,
                corrupt_manifests: result.corrupt_manifests as i64,
                structured_errors: result.structured_errors,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.filesync.snapshot.diff ────────────────────────────────

fn diff_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.filesync.snapshot.diff").await?;
            let req: SnapshotDiffRequest = decode(&payload).map_err(malformed)?;

            // Owner/conv-membership/group-member read gate (N1 + S2-P3) — both
            // operands. `snapshot_diff` additionally rejects cross-folder pairs,
            // but authorize each so a non-owner can't probe existence/ownership of
            // either id. Diff is a read → members of a group-bound set are admitted.
            let (_, _, grant_a) =
                authorize_snapshot(&state, req.a, &actor, SnapshotAccess::Read).await?;
            let (_, _, grant_b) =
                authorize_snapshot(&state, req.b, &actor, SnapshotAccess::Read).await?;
            // Both label pairs this reply carries — the per-entry path pair and
            // the single top-level set-name pair — are projected to the seals'
            // audience only (path-sealing S5e). `diff`
            // was a **second producer** of the set-name pair S5c-1 withholds on
            // `MediaItem`, so leaving it unconditional here reopened the axis
            // that a fix had already closed.
            //
            // Both operands must qualify. `snapshot_diff` rejects a cross-set
            // pair below, so the two grants describe one set and agree — the
            // conjunction is belt-and-braces that cannot go the wrong way if a
            // future change ever admits a cross-set diff.
            let labels_visible = grant_a.is_label_audience() && grant_b.is_label_audience();

            let result = crate::backup::diff::snapshot_diff(&state.db, req.a, req.b)
                .await
                .map_err(|e| {
                    let msg = e.to_string();
                    if msg.contains("not found") {
                        not_found(&msg)
                    } else if msg.contains("different folders") {
                        invalid_request(msg)
                    } else {
                        internal(msg)
                    }
                })?;

            encode_reply(&SnapshotDiffReply {
                snapshot_a: result.snapshot_a,
                snapshot_b: result.snapshot_b,
                added: result
                    .added
                    .iter()
                    .map(|e| SnapshotDiffEntry {
                        path: e.path.clone(),
                        size_bytes: e.size_bytes,
                        path_hash: labels_visible.then(|| ByteBuf::from(e.path_hash.clone())),
                        path_sealed: labels_visible
                            .then(|| e.path_sealed.clone().map(ByteBuf::from))
                            .flatten(),
                        extra: Default::default(),
                    })
                    .collect(),
                removed: result
                    .removed
                    .iter()
                    .map(|e| SnapshotDiffEntry {
                        path: e.path.clone(),
                        size_bytes: e.size_bytes,
                        path_hash: labels_visible.then(|| ByteBuf::from(e.path_hash.clone())),
                        path_sealed: labels_visible
                            .then(|| e.path_sealed.clone().map(ByteBuf::from))
                            .flatten(),
                        extra: Default::default(),
                    })
                    .collect(),
                modified: result
                    .modified
                    .iter()
                    .map(|e| SnapshotModifiedEntry {
                        path: e.path.clone(),
                        old_size: e.old_size,
                        new_size: e.new_size,
                        path_hash: labels_visible.then(|| ByteBuf::from(e.path_hash.clone())),
                        path_sealed: labels_visible
                            .then(|| e.path_sealed.clone().map(ByteBuf::from))
                            .flatten(),
                        extra: Default::default(),
                    })
                    .collect(),
                summary: SnapshotDiffSummary {
                    added_count: result.summary.added_count as i64,
                    removed_count: result.summary.removed_count as i64,
                    modified_count: result.summary.modified_count as i64,
                    added_bytes: result.summary.added_bytes,
                    removed_bytes: result.summary.removed_bytes,
                    net_bytes: result.summary.net_bytes,
                    extra: Default::default(),
                },
                // Names the one set both operands belong to, so the client can
                // resolve label custody for the sealed-path render.
                folder: result.folder,
                // Strictly a pair, exactly as `MediaItem` ships it: a seal with
                // no salt is unopenable post-scrub and a salt with no seal is a
                // dictionary handle onto nothing, so neither half travels alone.
                folder_sealed: labels_visible
                    .then(|| result.folder_sealed.clone().zip(result.folder_hash.clone()))
                    .flatten()
                    .map(|(sealed, _)| ByteBuf::from(sealed)),
                folder_hash: labels_visible
                    .then(|| result.folder_sealed.clone().zip(result.folder_hash.clone()))
                    .flatten()
                    .map(|(_, hash)| ByteBuf::from(hash)),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── Shared replay helpers ──────

/// Outcome of a message-kind restore.
#[derive(Debug)]
enum RestoreError {
    /// Any internal error (I/O, SQL, decode).
    Other(anyhow::Error),
}

impl From<anyhow::Error> for RestoreError {
    fn from(e: anyhow::Error) -> Self {
        RestoreError::Other(e)
    }
}

/// Replay a pinned mail-kind snapshot into the actor's bridge_imap_*
/// rows + segment_records mirror. Runs in a single SQLite transaction
/// so a crash mid-replay leaves the actor at the previous state.
async fn restore_mail(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    content_blob: Option<&[u8]>,
    placement_blob: Option<&[u8]>,
) -> Result<(), RestoreError> {
    use anyhow::Context as _;

    #[cfg(debug_assertions)]
    debug_assert!(
        state.db.op_lock_live("gc").await.unwrap_or(false),
        "restore_mail requires the caller to hold the \"gc\" op lock \
         (halfway-rebuild guard vs. the compaction orphan reaper)"
    );

    let content_blob =
        content_blob.ok_or_else(|| anyhow::anyhow!("snapshot missing content manifest"))?;
    let manifest: fauna_segment_store::Manifest =
        fauna_core::encoding::canonical_decode(content_blob).context("decode Manifest")?;
    if manifest.kind != "mail" {
        return Err(RestoreError::Other(anyhow::anyhow!(
            "snapshot manifest kind mismatch: expected 'mail', got '{}'",
            manifest.kind
        )));
    }

    state
        .mail_segments
        .finalize_open(actor)
        .await
        .context("finalize_open before restore")?;

    let root = fauna_mail::segments::mail_segments_root(state.mail_segments.data_dir(), actor);
    for &seg_id in &manifest.kind_manifest.live_segments {
        let path = root.join(format!("seg-{:08}.dat", seg_id));
        if !path.exists() {
            return Err(RestoreError::Other(anyhow::anyhow!(
                "pinned segment {} not on disk at {}; fauna-sync chunk pull may be incomplete",
                seg_id,
                path.display()
            )));
        }
    }

    let conn = state.db.conn().await;
    let tx = conn.unchecked_transaction().context("begin restore tx")?;

    tx.execute(
        "DELETE FROM bridge_imap_messages WHERE actor_id = ?1",
        rusqlite::params![actor.as_slice()],
    )
    .context("DELETE bridge_imap_messages")?;
    tx.execute(
        "DELETE FROM bridge_imap_mailbox_state WHERE actor_id = ?1",
        rusqlite::params![actor.as_slice()],
    )
    .context("DELETE bridge_imap_mailbox_state")?;
    tx.execute(
        "DELETE FROM bridge_imap_expunged WHERE actor_id = ?1",
        rusqlite::params![actor.as_slice()],
    )
    .context("DELETE bridge_imap_expunged")?;

    tx.execute(
        "DELETE FROM segment_records WHERE scope_id = ?1 AND kind = 'mail'",
        rusqlite::params![actor.as_slice()],
    )
    .context("DELETE segment_records")?;

    let placement_bytes =
        placement_blob.ok_or_else(|| anyhow::anyhow!("snapshot missing placement manifest"))?;
    // decode_any_version: a pinned blob at any version other than the current
    // one is refused.
    let placement_manifest =
        fauna_mail::segments::placement::MailPlacementManifest::decode_any_version(placement_bytes)
            .map_err(|e| anyhow::anyhow!("decode MailPlacementManifest: {e}"))?;

    crate::restore::mail::replay_mail_manifest_into_sqlite(&tx, actor, &placement_manifest)
        .context("replay mail placement manifest")?;

    rebuild_mail_segment_records_from_disk(
        &tx,
        state,
        actor,
        &manifest.kind_manifest.live_segments,
    )
    .context("rebuild_mail_segment_records_from_disk")?;

    tx.commit().context("commit restore tx")?;
    Ok(())
}

/// One on-disk calendar/card content record, indexed for the restore row
/// rebuild: the floor carries the ingest-time columns, the `(seg_id, cid)`
/// pair addresses the envelope (for `encrypted_index_hint`), and the bucket
/// feeds the rebuilt `segment_records` mirror row.
struct DiskContentRecord<F> {
    seg_id: u32,
    bucket: String,
    cid: fauna_cbor::Cid,
    floor: F,
}

/// Open every pinned live segment under `root`, decode each record's floor
/// via `parse_floor`, and return the records + the open segment handles
/// (keyed by seg id, for envelope reads). Errors if a pinned segment is
/// missing on disk (fauna-sync chunk pull incomplete — mail's precondition).
/// The record-identity floor every restore-from-disk rebuild must clear: a
/// pinned record's filing CID MUST be the content hash of the block stored
/// under it (`message-segment-store.md` § *Record identity per kind*).
///
/// **Why this cannot be assumed.** The CARv2 reader verifies only that the
/// *framed* CID at the indexed offset equals the one asked for — it never
/// re-hashes the block. So a snapshot pinned BEFORE that kind's 2026-08-17
/// cutover reads back perfectly while carrying records filed under the retired
/// upstream/sequenced id, and a restore would re-file exactly those cids into a
/// store whose boot reset had already cleared them: permanently un-admittable
/// records, silently reintroduced, on a plane whose whole adoption predicate is
/// this re-hash.
///
/// **Fail closed, whole-restore.** A mismatch aborts the rebuild; the restore
/// transaction rolls back and the (already reset) live state stands, rather
/// than leaving a half-restored plane of mixed identity. The user's recourse is
/// the reset they already approved — a snapshot whose record identities
/// disagree with its bytes cannot be restored whole.
///
/// Costs one block read per pinned record. A restore is rare, already pulls
/// every one of these bytes over the network, and this is the one moment the
/// store adopts bytes it did not itself mint.
fn verify_pinned_record_identity(
    seg: &fauna_segment_store::FramedSegment,
    cid: &fauna_cbor::Cid,
    kind: &str,
    seg_id: u32,
) -> anyhow::Result<()> {
    let block = seg
        .read_record(cid)
        .map_err(|e| anyhow::anyhow!("read pinned {kind} record {cid} in seg {seg_id}: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "pinned {kind} seg {seg_id} lists record {cid} in its sidecar but the \
                 segment file holds no such block"
            )
        })?;
    if !cid.matches(&block) {
        anyhow::bail!(
            "pinned {kind} seg {seg_id} carries record {cid} whose block does not hash to it \
             — a snapshot from before this kind's record-identity cutover \
             (message-segment-store.md § Record identity per kind). Restoring it would \
             re-file records no custodian could ever admit; refusing the whole restore."
        );
    }
    Ok(())
}

fn walk_pinned_segments<F>(
    root: &std::path::Path,
    kind: &str,
    live_segments: &[u32],
    parse_floor: impl Fn(&[u8]) -> anyhow::Result<F>,
) -> anyhow::Result<(
    Vec<DiskContentRecord<F>>,
    std::collections::HashMap<u32, fauna_segment_store::FramedSegment>,
)> {
    use anyhow::Context as _;
    use fauna_segment_store::FramedSegment;

    let mut records = Vec::new();
    let mut segs = std::collections::HashMap::new();
    for &seg_id in live_segments {
        let path = root.join(format!("seg-{:08}.dat", seg_id));
        if !path.exists() {
            anyhow::bail!(
                "pinned segment {} not on disk at {}; fauna-sync chunk pull may be incomplete",
                seg_id,
                path.display()
            );
        }
        let seg = FramedSegment::open(&path)
            .map_err(|e| anyhow::anyhow!("open seg {seg_id} at {}: {e}", path.display()))?;
        let bucket = seg.header.bucket.clone();
        for entry in seg.iter_records() {
            verify_pinned_record_identity(&seg, &entry.cid, kind, seg_id)?;
            let floor = parse_floor(&entry.floor_metadata)
                .with_context(|| format!("decode floor metadata for seg {seg_id}"))?;
            records.push(DiskContentRecord {
                seg_id,
                bucket: bucket.clone(),
                cid: entry.cid,
                floor,
            });
        }
        segs.insert(seg_id, seg);
    }
    Ok((records, segs))
}

/// Replay a pinned calendar-kind snapshot into the actor's bridge_caldav_*
/// rows + the `segment_records` mirror, in one SQLite transaction (mail's
/// DELETE-then-rebuild revert shape; the placement manifest supplies the
/// SQL-allocated state — etag / modseq / `encrypted_fauna_ext` — that
/// content records must not carry).
///
/// Runs under the dispatch handler's `"gc"` op lock (load-bearing: the
/// compaction worker's orphan reaper tombstones live mirror rows with no
/// metadata row, and a restore caught between its mirror rebuild and its
/// row rebuild would present its whole corpus as orphaned —
/// `message-segment-store.md` § Implementation status).
async fn restore_calendar(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    content_blob: Option<&[u8]>,
    placement_blob: Option<&[u8]>,
) -> Result<(), RestoreError> {
    use anyhow::Context as _;
    use fauna_calendar::segments::placement::CalPlacementManifest;

    #[cfg(debug_assertions)]
    debug_assert!(
        state.db.op_lock_live("gc").await.unwrap_or(false),
        "restore_calendar requires the caller to hold the \"gc\" op lock \
         (halfway-rebuild guard vs. the compaction orphan reaper)"
    );

    let content_blob =
        content_blob.ok_or_else(|| anyhow::anyhow!("snapshot missing content manifest"))?;
    let manifest: fauna_segment_store::Manifest =
        fauna_core::encoding::canonical_decode(content_blob).context("decode Manifest")?;
    if manifest.kind != "calendar" {
        return Err(RestoreError::Other(anyhow::anyhow!(
            "snapshot manifest kind mismatch: expected 'calendar', got '{}'",
            manifest.kind
        )));
    }

    state
        .cal_segments
        .finalize_open(actor)
        .await
        .context("finalize_open before restore")?;

    let placement_bytes =
        placement_blob.ok_or_else(|| anyhow::anyhow!("snapshot missing placement manifest"))?;
    // decode_any_version: a pinned blob at any version other than the current
    // one is refused.
    let placement_manifest = CalPlacementManifest::decode_any_version(placement_bytes)
        .map_err(|e| anyhow::anyhow!("decode CalPlacementManifest: {e}"))?;

    let conn = state.db.conn().await;
    let tx = conn.unchecked_transaction().context("begin restore tx")?;

    for table in [
        "bridge_caldav_calendars",
        "bridge_caldav_events",
        "bridge_caldav_expunged",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE actor_id = ?1"),
            rusqlite::params![actor.as_slice()],
        )
        .with_context(|| format!("DELETE {table}"))?;
    }
    tx.execute(
        "DELETE FROM segment_records WHERE scope_id = ?1 AND kind = 'calendar'",
        rusqlite::params![actor.as_slice()],
    )
    .context("DELETE segment_records")?;

    rebuild_calendar_rows_from_disk(
        &tx,
        state,
        actor,
        &manifest.kind_manifest.live_segments,
        &placement_manifest,
    )?;

    tx.commit().context("commit restore tx")?;
    Ok(())
}

/// Rebuild the actor's calendar rows from the on-disk content segments named in
/// `live_segments` and the placement fold `placement`, inside the caller's
/// transaction: the calendars and tombstones the fold holds, one live
/// `segment_records` mirror row per record, and one `bridge_caldav_events` row
/// per placed event.
///
/// Keyed on a plain segment-id list, as the mail rebuild is, because it has the
/// same two callers: the snapshot restore ([`restore_calendar`], which clears
/// the actor's rows first) and the backup materialize
/// ([`crate::backup::materialize`], which writes onto a target the empty-target
/// rule already proved fresh). One rebuild, so the restored row shape cannot
/// fork between the two restore sources.
pub(crate) fn rebuild_calendar_rows_from_disk(
    tx: &rusqlite::Transaction<'_>,
    state: &Arc<AppState>,
    actor: &[u8; 32],
    live_segments: &[u32],
    placement_manifest: &fauna_calendar::segments::placement::CalPlacementManifest,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use fauna_calendar::segments::envelope::CalRecordEnvelope;
    use fauna_calendar::segments::floor::CalFloorMetadata;

    let root = state.cal_segments.scope_dir(actor);
    let (records, segs) = walk_pinned_segments(&root, "calendar", live_segments, |bytes| {
        CalFloorMetadata::decode(bytes).map_err(|e| anyhow::anyhow!("{e}"))
    })
    .context("walk pinned calendar segments")?;

    crate::restore::cal::replay_cal_manifest_into_sqlite(tx, actor, placement_manifest)
        .context("replay cal placement manifest")?;
    crate::restore::cal::replay_cal_tombstones_into_sqlite(tx, actor, placement_manifest)
        .context("replay cal placement tombstones")?;

    // Mirror rebuild: one live row per record in the pinned segments (mail's
    // shape). Records superseded/deleted before the snapshot come back live
    // with no metadata row; the orphan reaper re-tombstones them past its age
    // watermark and compaction reclaims — self-healing, no data at risk.
    for r in &records {
        crate::segments::records_db::insert_calendar(
            tx,
            actor,
            r.seg_id,
            &r.cid,
            &r.bucket,
            r.floor.created_at,
        )
        .context("INSERT rebuilt calendar segment_records row")?;
    }

    // Event rows: placement (etag / modseq / sidecar) ⋈ floor (identity +
    // times) + envelope (the index hint), joined on the record id. (The
    // `(calendar_id, uid_hash)` → newest-`created_at` floor-join fallback for
    // a pre-bump entry with no id was retired by the compat-remnant sweep,
    // 2026-09-24: every placement entry carries its id.)
    for e in &placement_manifest.events {
        let rec = records
            .iter()
            .find(|r| r.floor.event_id == e.event_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "placement references event {} with no record in the pinned segments",
                    hex::encode(e.event_id)
                )
            })?;
        let envelope_bytes = segs
            .get(&rec.seg_id)
            .expect("walk inserted every seg id")
            .read_record(&rec.cid)
            .map_err(|e| anyhow::anyhow!("read record {}: {e}", rec.cid))?
            .ok_or_else(|| anyhow::anyhow!("segment index lists a missing record"))?;
        let envelope = CalRecordEnvelope::decode(&envelope_bytes)
            .map_err(|e| anyhow::anyhow!("decode CalRecordEnvelope: {e}"))?;
        tx.execute(
            "INSERT INTO bridge_caldav_events
                (actor_id, calendar_id, event_id, uid_hash,
                 encrypted_index_hint, etag, modseq, ciphertext_size, internal_date,
                 created_at, encrypted_fauna_ext, record_cid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                actor.as_slice(),
                e.calendar_id.as_slice(),
                rec.floor.event_id.as_slice(),
                &rec.floor.uid_hash,
                &envelope.encrypted_index_hint,
                &e.etag,
                e.modseq as i64,
                e.ciphertext_size as i64,
                rec.floor.internal_date,
                rec.floor.created_at,
                e.encrypted_fauna_ext.as_deref(),
                // The restored row must carry the record's content cid: every
                // read/reap path resolves the body by the STORED column and
                // fails closed on NULL, so a restore that omitted it would
                // rebuild rows whose bodies are unreachable.
                &rec.cid.as_bytes()[..],
            ],
        )
        .context("INSERT rebuilt bridge_caldav_events row")?;
    }

    Ok(())
}

/// Replay a pinned card-kind snapshot — the calendar twin verbatim, over
/// bridge_carddav_* + the `"card"` mirror kind. See [`restore_calendar`]
/// for the join semantics and the `"gc"` op-lock precondition.
async fn restore_card(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    content_blob: Option<&[u8]>,
    placement_blob: Option<&[u8]>,
) -> Result<(), RestoreError> {
    use anyhow::Context as _;
    use fauna_contacts::segments::placement::CardPlacementManifest;

    #[cfg(debug_assertions)]
    debug_assert!(
        state.db.op_lock_live("gc").await.unwrap_or(false),
        "restore_card requires the caller to hold the \"gc\" op lock \
         (halfway-rebuild guard vs. the compaction orphan reaper)"
    );

    let content_blob =
        content_blob.ok_or_else(|| anyhow::anyhow!("snapshot missing content manifest"))?;
    let manifest: fauna_segment_store::Manifest =
        fauna_core::encoding::canonical_decode(content_blob).context("decode Manifest")?;
    if manifest.kind != "card" {
        return Err(RestoreError::Other(anyhow::anyhow!(
            "snapshot manifest kind mismatch: expected 'card', got '{}'",
            manifest.kind
        )));
    }

    state
        .card_segments
        .finalize_open(actor)
        .await
        .context("finalize_open before restore")?;

    let placement_bytes =
        placement_blob.ok_or_else(|| anyhow::anyhow!("snapshot missing placement manifest"))?;
    let placement_manifest = CardPlacementManifest::decode_any_version(placement_bytes)
        .map_err(|e| anyhow::anyhow!("decode CardPlacementManifest: {e}"))?;

    let conn = state.db.conn().await;
    let tx = conn.unchecked_transaction().context("begin restore tx")?;

    for table in [
        "bridge_carddav_addressbooks",
        "bridge_carddav_cards",
        "bridge_carddav_expunged",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE actor_id = ?1"),
            rusqlite::params![actor.as_slice()],
        )
        .with_context(|| format!("DELETE {table}"))?;
    }
    tx.execute(
        "DELETE FROM segment_records WHERE scope_id = ?1 AND kind = 'card'",
        rusqlite::params![actor.as_slice()],
    )
    .context("DELETE segment_records")?;

    rebuild_card_rows_from_disk(
        &tx,
        state,
        actor,
        &manifest.kind_manifest.live_segments,
        &placement_manifest,
    )?;

    tx.commit().context("commit restore tx")?;
    Ok(())
}

/// The card twin of [`rebuild_calendar_rows_from_disk`] verbatim, over
/// `bridge_carddav_*` and the `"card"` mirror kind — same two callers, same
/// reason for one rebuild.
pub(crate) fn rebuild_card_rows_from_disk(
    tx: &rusqlite::Transaction<'_>,
    state: &Arc<AppState>,
    actor: &[u8; 32],
    live_segments: &[u32],
    placement_manifest: &fauna_contacts::segments::placement::CardPlacementManifest,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use fauna_contacts::segments::envelope::CardRecordEnvelope;
    use fauna_contacts::segments::floor::CardFloorMetadata;

    let root = state.card_segments.scope_dir(actor);
    let (records, segs) = walk_pinned_segments(&root, "card", live_segments, |bytes| {
        CardFloorMetadata::decode(bytes).map_err(|e| anyhow::anyhow!("{e}"))
    })
    .context("walk pinned card segments")?;

    crate::restore::card::replay_card_manifest_into_sqlite(tx, actor, placement_manifest)
        .context("replay card placement manifest")?;
    crate::restore::card::replay_card_tombstones_into_sqlite(tx, actor, placement_manifest)
        .context("replay card placement tombstones")?;

    for r in &records {
        crate::segments::records_db::insert_card(
            tx,
            actor,
            r.seg_id,
            &r.cid,
            &r.bucket,
            r.floor.created_at,
        )
        .context("INSERT rebuilt card segment_records row")?;
    }

    // Card rows join on the record id, as the calendar twin above does (its
    // pre-bump floor-join fallback was retired with this one).
    for c in &placement_manifest.cards {
        let rec = records
            .iter()
            .find(|r| r.floor.card_id == c.card_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "placement references card {} with no record in the pinned segments",
                    hex::encode(c.card_id)
                )
            })?;
        let envelope_bytes = segs
            .get(&rec.seg_id)
            .expect("walk inserted every seg id")
            .read_record(&rec.cid)
            .map_err(|e| anyhow::anyhow!("read record {}: {e}", rec.cid))?
            .ok_or_else(|| anyhow::anyhow!("segment index lists a missing record"))?;
        let envelope = CardRecordEnvelope::decode(&envelope_bytes)
            .map_err(|e| anyhow::anyhow!("decode CardRecordEnvelope: {e}"))?;
        tx.execute(
            "INSERT INTO bridge_carddav_cards
                (actor_id, addressbook_id, card_id, uid_hash,
                 encrypted_index_hint, etag, modseq, ciphertext_size, internal_date,
                 created_at, encrypted_fauna_ext, record_cid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                actor.as_slice(),
                c.addressbook_id.as_slice(),
                rec.floor.card_id.as_slice(),
                &rec.floor.uid_hash,
                &envelope.encrypted_index_hint,
                &c.etag,
                c.modseq as i64,
                c.ciphertext_size as i64,
                rec.floor.internal_date,
                rec.floor.created_at,
                c.encrypted_fauna_ext.as_deref(),
                // See the calendar twin: the stored cid is the only way back to
                // the body, so a restore must carry it.
                &rec.cid.as_bytes()[..],
            ],
        )
        .context("INSERT rebuilt bridge_carddav_cards row")?;
    }

    Ok(())
}

/// Replay a pinned conv-kind snapshot into the channel's `segment_records`
/// mirror + the restoring member's `actor_channels` roster row. Runs in a
/// single SQLite transaction so a crash mid-replay leaves the channel at the
/// previous state. Conv sibling of [`restore_mail`] — no placement layer (conv
/// has no UIDVALIDITY equivalent), no bridge precondition (no bridge serves
/// conversations).
///
/// `channel_id` is the conv snapshot's scope (the folder's `actor_id`
/// column). `restoring_member` is the bearer whose roster row is reinstated.
async fn restore_conv(
    state: &Arc<AppState>,
    channel_id: &[u8; 32],
    content_blob: Option<&[u8]>,
    restoring_member: &[u8; 32],
) -> Result<(), RestoreError> {
    use anyhow::Context as _;

    #[cfg(debug_assertions)]
    debug_assert!(
        state.db.op_lock_live("gc").await.unwrap_or(false),
        "restore_conv requires the caller to hold the \"gc\" op lock \
         (halfway-rebuild guard vs. the compaction orphan reaper)"
    );

    let content_blob =
        content_blob.ok_or_else(|| anyhow::anyhow!("snapshot missing content manifest"))?;
    let manifest: fauna_segment_store::Manifest =
        fauna_core::encoding::canonical_decode(content_blob).context("decode Manifest")?;
    if manifest.kind != "conv" {
        return Err(RestoreError::Other(anyhow::anyhow!(
            "snapshot manifest kind mismatch: expected 'conv', got '{}'",
            manifest.kind
        )));
    }

    state
        .conv_segments
        .finalize_open(channel_id)
        .await
        .context("finalize_open before restore")?;

    let root = state.conv_segments.scope_dir(channel_id);
    for &seg_id in &manifest.kind_manifest.live_segments {
        let path = root.join(format!("seg-{:08}.dat", seg_id));
        if !path.exists() {
            return Err(RestoreError::Other(anyhow::anyhow!(
                "pinned segment {} not on disk at {}; fauna-sync chunk pull may be incomplete",
                seg_id,
                path.display()
            )));
        }
    }

    let conn = state.db.conn().await;
    let tx = conn.unchecked_transaction().context("begin restore tx")?;

    tx.execute(
        "DELETE FROM segment_records WHERE scope_id = ?1 AND kind = 'conv'",
        rusqlite::params![channel_id.as_slice()],
    )
    .context("DELETE segment_records")?;

    rebuild_conv_segment_records_from_disk(&tx, state, channel_id, &manifest)
        .context("rebuild_conv_segment_records_from_disk")?;

    // Reinstate the restoring member's roster row (idempotent). Matches
    // `db::channels::register_actor_channel`'s SQL exactly so a restore is
    // indistinguishable from a fresh join.
    tx.execute(
        "INSERT OR IGNORE INTO actor_channels (actor_id, channel_id, created_at)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![
            restoring_member.as_slice(),
            channel_id.as_slice(),
            crate::db::now_epoch_secs(),
        ],
    )
    .context("INSERT OR IGNORE actor_channels (restore roster)")?;

    tx.commit().context("commit restore tx")?;
    Ok(())
}

/// Restore an author's posts from a pinned post snapshot. Post sibling of
/// [`restore_conv`] — no placement layer, no bridge precondition — but
/// **ADDITIVE** (no `DELETE FROM segment_records`): posts are public, no-data-
/// loss content and carry a separate cross-actor feed-index projection, so a
/// restore RECOVERS the snapshot's posts (the cross-location-backup →
/// fresh/recovery-nest use case) into BOTH the `segment_records` mirror and the
/// `content` projection without dropping the author's newer posts. The
/// re-assert is idempotent ([`crate::segments::post::restore_from_manifest`]).
///
/// `author` is the post snapshot's scope (the folder's `actor_id` column).
async fn restore_post(
    state: &Arc<AppState>,
    author: &[u8; 32],
    content_blob: Option<&[u8]>,
) -> Result<(), RestoreError> {
    use anyhow::Context as _;

    #[cfg(debug_assertions)]
    debug_assert!(
        state.db.op_lock_live("gc").await.unwrap_or(false),
        "restore_post requires the caller to hold the \"gc\" op lock \
         (halfway-rebuild guard vs. the compaction orphan reaper)"
    );

    let content_blob =
        content_blob.ok_or_else(|| anyhow::anyhow!("snapshot missing content manifest"))?;
    let manifest: fauna_segment_store::Manifest =
        fauna_core::encoding::canonical_decode(content_blob).context("decode Manifest")?;
    if manifest.kind != "post" {
        return Err(RestoreError::Other(anyhow::anyhow!(
            "snapshot manifest kind mismatch: expected 'post', got '{}'",
            manifest.kind
        )));
    }

    state
        .post_segments
        .finalize_open(author)
        .await
        .context("finalize_open before restore")?;

    // Verify each pinned segment is on disk (else the fauna-sync chunk pull is
    // incomplete — mirrors restore_conv).
    let root = state.post_segments.scope_dir(author);
    for &seg_id in &manifest.kind_manifest.live_segments {
        let path = root.join(format!("seg-{:08}.dat", seg_id));
        if !path.exists() {
            return Err(RestoreError::Other(anyhow::anyhow!(
                "pinned segment {} not on disk at {}; fauna-sync chunk pull may be incomplete",
                seg_id,
                path.display()
            )));
        }
    }

    crate::segments::post::restore_from_manifest(
        &state.post_segments,
        &state.db,
        author,
        &manifest,
    )
    .await
    .context("restore_from_manifest (post)")?;
    Ok(())
}

/// Rebuild the conv `segment_records` mirror for a channel from the pinned
/// manifest's on-disk segment footers. Conv sibling of
/// [`rebuild_mail_segment_records_from_disk`]: opens each live `seg-{:08}.dat`,
/// decodes each record's [`fauna_mls::segments::ConvFloorMetadata`] floor, and
/// re-emits it through the canonical `records_db::insert_conv` writer, never a
/// hand-rolled INSERT (message-segment-store.md § `segment_records` SQLite
/// mirror — a rebuild re-emits the mirror's WHOLE column set through the
/// kind's own insert helper, ratified 2026-08-23).
fn rebuild_conv_segment_records_from_disk(
    tx: &rusqlite::Transaction<'_>,
    state: &Arc<AppState>,
    channel_id: &[u8; 32],
    manifest: &fauna_segment_store::Manifest,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use fauna_segment_store::FramedSegment;

    let root = state.conv_segments.scope_dir(channel_id);
    for &seg_id in &manifest.kind_manifest.live_segments {
        let path = root.join(format!("seg-{:08}.dat", seg_id));
        let seg = FramedSegment::open(&path)
            .map_err(|e| anyhow::anyhow!("open seg {seg_id} at {}: {e}", path.display()))?;
        let bucket = seg.header.bucket.clone();
        // Re-emit one mirror row per record from the sidecar entries (CID +
        // floor) — after re-hashing each block against the CID it is filed
        // under, the identity floor a restore must clear (see
        // `verify_pinned_record_identity`). Mirrors
        // rebuild_mail_segment_records_from_disk.
        for entry in seg.iter_records() {
            verify_pinned_record_identity(&seg, &entry.cid, "conv", seg_id)?;
            let floor = fauna_mls::segments::parse_floor(&entry.floor_metadata)
                .map_err(|e| anyhow::anyhow!("decode conv floor metadata for seg {seg_id}: {e}"))?;
            // segment_records.record_cid is the entry's full 36-byte Cid — the
            // exact key the CARv2 index uses. Through the canonical writer, not
            // a hand-rolled INSERT: `restore_conv` DELETEs the channel's whole
            // conv mirror before this runs, so any column this rebuild omits is
            // DESTROYED, not left stale. A hand-listed column set previously
            // omitted `changed_seq`, so a restored conv record sat on the `0`
            // sentinel — invisible to the class-1 feed walk's `changed_seq > ?`
            // cursor until the next boot's backfill healed it.
            crate::segments::records_db::insert_conv(
                tx,
                channel_id,
                seg_id,
                &entry.cid,
                &bucket,
                floor.received_at,
                floor.seq,
            )
            .context("INSERT rebuilt conv segment_records row")?;
        }
    }
    Ok(())
}

/// Rebuild the mail `segment_records` mirror for `actor` from the on-disk
/// footers of the segments named in `live_segments`.
///
/// Keyed on a plain **segment-id list**, not a snapshot `Manifest`, because it
/// now has two callers whose id lists come from different places and neither
/// should have to fabricate the other's container: snapshot restore
/// ([`restore_mail`]) reads `manifest.kind_manifest.live_segments`, while
/// materialize ([`crate::backup::materialize`]) reads the `live` list of the
/// destination-side `LiveManifestMirror` the source pushed. One rebuild, so the
/// mirror's column set can never fork between the two restore sources
/// (`message-segment-store.md` § `segment_records` SQLite mirror).
pub(crate) fn rebuild_mail_segment_records_from_disk(
    tx: &rusqlite::Transaction<'_>,
    state: &Arc<AppState>,
    actor: &[u8; 32],
    live_segments: &[u32],
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use fauna_mail::segments::{MailFloorMetadata, mail_segments_root};
    use fauna_segment_store::FramedSegment;

    let root = mail_segments_root(state.mail_segments.data_dir(), actor);
    for &seg_id in live_segments {
        let path = root.join(format!("seg-{:08}.dat", seg_id));
        let seg = FramedSegment::open(&path)
            .map_err(|e| anyhow::anyhow!("open seg {seg_id} at {}: {e}", path.display()))?;
        let bucket = seg.header.bucket.clone();
        // Re-emit one mirror row per record from the sidecar entries (CID +
        // floor) — after re-hashing each block against the CID it is filed
        // under, the identity floor a restore must clear (see
        // `verify_pinned_record_identity`).
        for entry in seg.iter_records() {
            verify_pinned_record_identity(&seg, &entry.cid, "mail", seg_id)?;
            let floor = MailFloorMetadata::decode(&entry.floor_metadata)
                .map_err(|e| anyhow::anyhow!("decode floor metadata for seg {seg_id}: {e}"))?;
            // Through the canonical mail writer, never a hand-rolled INSERT.
            // `restore_mail` DELETEs the actor's whole mail mirror before this
            // runs, so any column this rebuild omits is DESTROYED, not left
            // stale — and a hand-listed column set silently drifts from the one
            // the append path writes. It did: `seq`, `report_hash` and
            // `stored_at` are all floor-authoritative and were all dropped here,
            // while the conv sibling above has carried `floor.seq` all along.
            //
            // `seq` was the load-bearing loss: the relay drain selects
            // `WHERE seq > ?` (`records_db::list_mail_after_seq`) and SQLite's
            // `NULL > n` is NULL, so a restored corpus became permanently
            // invisible to `mail_pull` — and `next_mail_seq`
            // (`COALESCE(MAX(seq), 0) + 1`) restarted the counter at 1, hiding
            // post-restore arrivals under a puller's existing cursor too.
            // `insert_mail` also assigns `changed_seq` (the hand-rolled INSERT
            // left it 0, healed only by the next boot's backfill).
            //
            // segment_records.record_cid is the entry's full 36-byte Cid — the
            // exact key the CARv2 index uses.
            crate::segments::records_db::insert_mail(
                tx,
                actor,
                seg_id,
                &entry.cid,
                &bucket,
                floor.received_at,
                &floor.sender_domain,
                &floor.spam_disposition,
                floor.is_own_submission,
                floor.seq,
                (!floor.report_hash.is_empty()).then_some(floor.report_hash.as_slice()),
                floor.continuation_role,
                floor.stored_at,
            )
            .context("INSERT rebuilt segment_records row")?;
        }
    }
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    fn encode_req<T: serde::Serialize>(req: &T) -> Bytes {
        Bytes::from(encode_canonical(req).expect("encode req").to_vec())
    }

    fn decode_reply<T: serde::de::DeserializeOwned>(b: &Bytes) -> T {
        decode(b).expect("decode reply")
    }

    fn fixture_state() -> Arc<AppState> {
        crate::test_support::fixture_state()
    }

    async fn seed_snapshots(state: &Arc<AppState>, actor: &[u8; 32], n: u32) -> Vec<i64> {
        let folder_id = state
            .db
            .create_folder("test-set", actor)
            .await
            .expect("create_folder");
        let mut ids = Vec::new();
        for i in 0..n {
            let id = state
                .db
                .insert_snapshot_at(folder_id, i as i64)
                .await
                .expect("insert_snapshot_at");
            ids.push(id);
        }
        ids
    }

    async fn seed_mail_snapshot(state: &Arc<AppState>, actor: &[u8; 32]) -> i64 {
        crate::test_support::seed_mail_snapshot(state, actor).await
    }

    // ── create_message_kind ──────────────────────────────────

    #[tokio::test]
    async fn create_mail_persists_manifest() {
        let state = fixture_state();
        let actor = [0x44u8; 32];

        let req = SnapshotCreateMessageKindRequest {
            kind: "mail".into(),
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = create_message_kind_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect("create mail snapshot");
        let reply: SnapshotCreateMessageKindReply = decode_reply(&bytes);
        assert_eq!(reply.kind, "mail");
        assert_eq!(reply.snapshot_ids, vec![reply.snapshot_id]);
        assert_eq!(reply.actor_id.0, actor);
        assert!(reply.snapshot_id > 0);

        let row = state
            .db
            .get_snapshot(reply.snapshot_id)
            .await
            .unwrap()
            .expect("row");
        assert_eq!(row.message_kind.as_deref(), Some("mail"));
        assert!(row.message_manifest.is_some());
        // Both manifests are pinned at create time (backup-restore.md § 6
        // "serialises both manifests in the same SQLite transaction"). The
        // placement manifest is what restore_mail replays into bridge_imap_*;
        // a NULL here would make the snapshot un-restorable.
        let placement_blob = row
            .placement_manifest
            .as_ref()
            .expect("placement manifest must be pinned at create time");
        let _decoded: fauna_mail::segments::placement::MailPlacementManifest =
            fauna_core::encoding::canonical_decode(placement_blob)
                .expect("placement manifest decodes");
    }

    #[tokio::test]
    async fn create_then_restore_mail_round_trips_placement() {
        // End-to-end proof that the create→restore round trip works through
        // the production handlers: a mailbox appended to the placement
        // journal is pinned by create_message_kind, then reconstructed by
        // restore_message_kind into bridge_imap_mailbox_state. Before the
        // create-side placement-capture fix this failed at restore_mail with
        // "snapshot missing placement manifest".
        use fauna_mail::segments::placement::MailPlacementRecord;

        let state = fixture_state();
        let actor = [0x77u8; 32];

        // Append a Create for a non-standard mailbox so the round trip is
        // observable (the six standard mailboxes are seeded lazily at SELECT).
        state
            .mail_placement
            .append_event(
                &actor,
                &MailPlacementRecord::Create {
                    mailbox: "Archive".to_string(),
                    uid_validity: 4242,
                    attrs: vec![],
                },
            )
            .await
            .expect("append placement Create");

        // Create the snapshot — placement manifest must be pinned now.
        let create_bytes = create_message_kind_handler()(
            state.clone(),
            actor,
            encode_req(&SnapshotCreateMessageKindRequest {
                kind: "mail".into(),
                actor_id: None,
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("create mail snapshot");
        let snap_id = decode_reply::<SnapshotCreateMessageKindReply>(&create_bytes).snapshot_id;

        // Wipe the live bridge state so restore has something to reconstruct.
        {
            let conn = state.db.conn().await;
            conn.execute(
                "DELETE FROM bridge_imap_mailbox_state WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
            )
            .expect("clear mailbox state");
        }

        // Restore the snapshot.
        restore_message_kind_handler()(
            state.clone(),
            actor,
            encode_req(&SnapshotRestoreMessageKindRequest {
                snapshot_id: snap_id,
                confirm_id: snap_id.to_string(),
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("restore mail snapshot");

        // The Archive mailbox is back, with its pinned uid_validity.
        let row = state
            .db
            .get_bridge_imap_mailbox_state(&actor, "Archive")
            .await
            .expect("query mailbox state")
            .expect("Archive mailbox restored");
        assert_eq!(row.uid_validity, 4242);
    }

    #[tokio::test]
    async fn restore_mail_preserves_the_relay_seq_cursor_and_the_floor_columns() {
        // The mail mirror rebuild must carry EVERY column the floor is
        // authoritative for, because `restore_mail` DELETEs the actor's whole
        // mail mirror first — whatever the rebuild omits is destroyed, not
        // merely left stale.
        //
        // `seq` is the load-bearing one. The relay drain
        // (`federation_handlers::mail_pull_handler` → `mail::read_after_seq` →
        // `records_db::list_mail_after_seq`) selects `WHERE seq > ?`, and in
        // SQLite `NULL > n` is NULL — never true — so a rebuilt row with a NULL
        // `seq` is invisible to the puller FOREVER, and the ack that would purge
        // the source never comes. Worse, `records_db::next_mail_seq` is
        // `COALESCE(MAX(seq), 0) + 1`, so an all-NULL mirror restarts the
        // counter at 1 and mail arriving *after* the restore lands below a
        // puller's existing cursor and is skipped too.
        //
        // The conv sibling (`rebuild_conv_segment_records_from_disk`) has
        // carried `floor.seq` all along; mail — the original both were shaped
        // from — did not.
        use fauna_mls::wrapped_blob::SealedRecordBytes;

        let state = fixture_state();
        let actor = [0x7Au8; 32];
        let received_at = 1_715_000_000_000i64;

        // Three records → seq 1, 2, 3, each with a distinct report-hash so the
        // other dropped floor columns are pinned by the same test.
        for i in 0..3u8 {
            crate::segments::mail::append_record(
                &state.mail_segments,
                &state.db,
                &actor,
                &SealedRecordBytes::carried_at_rest_unchecked(
                    format!("sealed-body-{i}").into_bytes(),
                ),
                &SealedRecordBytes::carried_at_rest_unchecked(b"sealed-hint".to_vec()),
                crate::segments::test_helpers::floor_with_report_hash(
                    received_at,
                    vec![0xB0 | i; 32],
                ),
            )
            .await
            .expect("append_record");
        }

        let before =
            crate::segments::mail::read_after_seq(&state.mail_segments, &state.db, &actor, 0, 100)
                .await
                .expect("read before snapshot");
        assert_eq!(
            before.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "three records, seq 1..3"
        );

        // Snapshot (finalizes the open segment and pins the manifest), then
        // restore it — the DELETE + rebuild path.
        let create_bytes = create_message_kind_handler()(
            state.clone(),
            actor,
            encode_req(&SnapshotCreateMessageKindRequest {
                kind: "mail".into(),
                actor_id: None,
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("create mail snapshot");
        let snap_id = decode_reply::<SnapshotCreateMessageKindReply>(&create_bytes).snapshot_id;

        restore_message_kind_handler()(
            state.clone(),
            actor,
            encode_req(&SnapshotRestoreMessageKindRequest {
                snapshot_id: snap_id,
                confirm_id: snap_id.to_string(),
                extra: std::collections::BTreeMap::new(),
            }),
        )
        .await
        .expect("restore mail snapshot");

        // The relay can still see every restored record, at its original seq.
        let after =
            crate::segments::mail::read_after_seq(&state.mail_segments, &state.db, &actor, 0, 100)
                .await
                .expect("read after restore");
        assert_eq!(
            after.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "a restored mail corpus must stay visible to the relay's `seq > ?` \
             cursor — a NULL seq hides it from `mail_pull` permanently"
        );

        // And the counter did not restart: the next append must not collide
        // with a cursor a puller has already passed.
        {
            let conn = state.db.conn().await;
            assert_eq!(
                crate::segments::records_db::next_mail_seq(&conn, &actor).expect("next_mail_seq"),
                4,
                "MAX(seq) survives the restore, so the next record is seq 4 — an \
                 all-NULL mirror would restart at 1, under any live puller cursor"
            );
        }

        // The other two columns the rebuild dropped, both floor-authoritative.
        {
            let conn = state.db.conn().await;
            let (hashes, stored): (i64, i64) = conn
                .query_row(
                    "SELECT COUNT(report_hash), COUNT(stored_at) FROM segment_records
                      WHERE scope_id = ?1 AND kind = 'mail' AND tombstoned = 0",
                    rusqlite::params![actor.as_slice()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .expect("count rebuilt floor columns");
            assert_eq!(
                hashes, 3,
                "report_hash is floor-authoritative; dropping it at restore \
                 silently ends report aggregation for the whole corpus"
            );
            assert_eq!(
                stored, 3,
                "stored_at is floor-authoritative and gates the headless-part \
                 reaper's grace; a NULL leaks parts forever"
            );
        }
    }

    #[tokio::test]
    async fn create_calendar_pins_both_manifests() {
        // S6.9: the calendar create arm is real — mail's placement-paired
        // shape (backup-restore.md § snapshot create). Both manifests pinned;
        // a NULL placement would make the snapshot un-restorable.
        let state = fixture_state();
        let actor = [0x44u8; 32];

        let req = SnapshotCreateMessageKindRequest {
            kind: "calendar".into(),
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = create_message_kind_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect("create calendar snapshot");
        let reply: SnapshotCreateMessageKindReply = decode_reply(&bytes);
        assert_eq!(reply.kind, "calendar");

        let row = state
            .db
            .get_snapshot(reply.snapshot_id)
            .await
            .unwrap()
            .expect("row");
        assert_eq!(row.message_kind.as_deref(), Some("calendar"));
        let content_blob = row
            .message_manifest
            .as_ref()
            .expect("content manifest must be pinned at create time");
        let m: fauna_segment_store::Manifest =
            fauna_core::encoding::canonical_decode(content_blob).expect("content decodes");
        assert_eq!(m.kind, "calendar");
        let placement_blob = row
            .placement_manifest
            .as_ref()
            .expect("placement manifest must be pinned at create time");
        let pm = fauna_calendar::segments::placement::CalPlacementManifest::decode_any_version(
            placement_blob,
        )
        .expect("placement decodes");
        assert_eq!(
            pm.format_version,
            fauna_calendar::segments::placement::CAL_PLACEMENT_FORMAT_VERSION,
            "a fresh snapshot pins a v2 placement blob"
        );
    }

    #[tokio::test]
    async fn create_card_pins_both_manifests() {
        let state = fixture_state();
        let actor = [0x45u8; 32];

        let req = SnapshotCreateMessageKindRequest {
            kind: "card".into(),
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = create_message_kind_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect("create card snapshot");
        let reply: SnapshotCreateMessageKindReply = decode_reply(&bytes);
        assert_eq!(reply.kind, "card");

        let row = state
            .db
            .get_snapshot(reply.snapshot_id)
            .await
            .unwrap()
            .expect("row");
        assert_eq!(row.message_kind.as_deref(), Some("card"));
        assert!(row.message_manifest.is_some(), "content pinned");
        let placement_blob = row
            .placement_manifest
            .as_ref()
            .expect("placement manifest must be pinned at create time");
        fauna_contacts::segments::placement::CardPlacementManifest::decode_any_version(
            placement_blob,
        )
        .expect("placement decodes");
    }

    #[tokio::test]
    async fn create_unknown_kind_rejected() {
        let state = fixture_state();
        let actor = [0x44u8; 32];

        let req = SnapshotCreateMessageKindRequest {
            // A genuinely unrecognised kind (post is now a valid kind — Track C).
            kind: "widget".into(),
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let err = create_message_kind_handler()(state, actor, encode_req(&req))
            .await
            .expect_err("unknown kind must be rejected");
        assert_eq!(err.code, "fauna.filesync.snapshot.unknown_kind");
    }

    #[tokio::test]
    async fn create_mail_rejects_pure_backup_destination() {
        let state = fixture_state();
        let actor = [0xC1u8; 32];
        state
            .db
            .create_folder_with_options(
                "__mail",
                &actor,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let req = SnapshotCreateMessageKindRequest {
            kind: "mail".into(),
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let err = create_message_kind_handler()(state, actor, encode_req(&req))
            .await
            .expect_err("pure-backup destination must be rejected");
        assert_eq!(err.code, "fauna.filesync.snapshot.pure_backup_destination");
    }

    /// Destination-capability gate 4 for posts: a `__post` reserved set in
    /// a custody copy (a pure-backup destination) refuses post snapshot create
    /// — opaque chunks have no local plaintext-framed manifest to pin.
    #[tokio::test]
    async fn create_post_rejects_pure_backup_destination() {
        let state = fixture_state();
        let actor = [0xC7u8; 32];
        state
            .db
            .create_folder_with_options(
                "__post",
                &actor,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let req = SnapshotCreateMessageKindRequest {
            kind: "post".into(),
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let err = create_message_kind_handler()(state, actor, encode_req(&req))
            .await
            .expect_err("pure-backup post destination must be rejected");
        assert_eq!(err.code, "fauna.filesync.snapshot.pure_backup_destination");
    }

    // ── create_message_kind: conv (Plan 8 T4) ────────────────
    //
    // Conv snapshots scope to a channel and authorize on membership. The
    // for-test conv_segments dir is PID-shared, so we override it to a fresh
    // SegmentManager rooted in a per-test tempdir (mirrors
    // compaction.rs/compact_handler.rs `build_state`) and use distinct
    // channel/actor ids per test.

    /// (tempdir, state) with conv_segments rooted in the tempdir. Hold the
    /// TempDir for the test's lifetime so the segment files outlive the state.
    fn conv_state() -> (tempfile::TempDir, Arc<AppState>) {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        let mut state = AppState::for_test(db);
        state.conv_segments = Arc::new(fauna_segment_store::SegmentManager::new(
            tmp.path().to_path_buf(),
            "conv",
        ));
        (tmp, Arc::new(state))
    }

    /// Count `message_kind='conv'` snapshot rows anchored to a channel's
    /// `__conv/<hex>` reserved folder, asserting each carries a non-NULL
    /// `message_manifest` and NULL `placement_manifest`.
    async fn conv_snapshot_rows(state: &Arc<AppState>, channel: &[u8; 32]) -> i64 {
        let conn = state.db.conn().await;
        conn.query_row(
            "SELECT COUNT(*)
             FROM snapshots s JOIN folders f ON s.folder_id = f.id
             WHERE f.actor_id = ?1
               AND s.message_kind = 'conv'
               AND s.message_manifest IS NOT NULL
               AND s.placement_manifest IS NULL",
            rusqlite::params![&channel[..]],
            |r| r.get(0),
        )
        .expect("count conv snapshot rows")
    }

    #[tokio::test]
    async fn create_conv_per_channel_persists_manifest() {
        let (_tmp, state) = conv_state();
        let channel = [0x61u8; 32];
        let member = [0x62u8; 32];
        state
            .db
            .register_actor_channel(&member, &channel)
            .await
            .expect("register member");
        crate::segments::conv::append(
            &state.conv_segments,
            &state.db,
            &channel,
            b"sealed-conv-body",
            1_715_000_000_000,
        )
        .await
        .expect("conv append");

        let req = SnapshotCreateMessageKindRequest {
            kind: "conv".into(),
            actor_id: Some(ActorId(channel)),
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = create_message_kind_handler()(state.clone(), member, encode_req(&req))
            .await
            .expect("create conv snapshot");
        let reply: SnapshotCreateMessageKindReply = decode_reply(&bytes);
        assert_eq!(reply.kind, "conv");
        assert_eq!(reply.actor_id.0, member, "reply actor is the bearer");
        assert_eq!(reply.snapshot_ids.len(), 1);
        assert_eq!(reply.snapshot_id, reply.snapshot_ids[0]);
        assert!(reply.snapshot_id > 0);

        // The row exists with a content manifest and NULL placement.
        assert_eq!(conv_snapshot_rows(&state, &channel).await, 1);
        let row = state
            .db
            .get_snapshot(reply.snapshot_id)
            .await
            .unwrap()
            .expect("row");
        assert_eq!(row.message_kind.as_deref(), Some("conv"));
        assert!(row.message_manifest.is_some());
        assert!(
            row.placement_manifest.is_none(),
            "conv has no placement layer"
        );
    }

    #[tokio::test]
    async fn create_conv_batched_all_my_channels() {
        let (_tmp, state) = conv_state();
        let member = [0x63u8; 32];
        let ch_a = [0x64u8; 32];
        let ch_b = [0x65u8; 32];
        for ch in [&ch_a, &ch_b] {
            state
                .db
                .register_actor_channel(&member, ch)
                .await
                .expect("register member");
            crate::segments::conv::append(
                &state.conv_segments,
                &state.db,
                ch,
                b"sealed-conv-body",
                1_715_000_000_000,
            )
            .await
            .expect("conv append");
        }

        // None scope → batched snapshot of every channel the bearer belongs to.
        let req = SnapshotCreateMessageKindRequest {
            kind: "conv".into(),
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = create_message_kind_handler()(state.clone(), member, encode_req(&req))
            .await
            .expect("create batched conv snapshots");
        let reply: SnapshotCreateMessageKindReply = decode_reply(&bytes);
        assert_eq!(reply.snapshot_ids.len(), 2, "one snapshot per channel");
        assert_eq!(reply.snapshot_id, reply.snapshot_ids[0]);
        assert_eq!(conv_snapshot_rows(&state, &ch_a).await, 1);
        assert_eq!(conv_snapshot_rows(&state, &ch_b).await, 1);
    }

    #[tokio::test]
    async fn create_conv_batched_pure_backup_at_second_channel_leaves_zero_rows() {
        // The pure-backup gate is hoisted above the row-creating loop,
        // so a batched call meeting a pure-backup channel at position k > 0
        // must leave ZERO snapshot rows for the channels ahead of it — not
        // the pre-hoist behavior of committing 0..k-1 then erroring.
        let (_tmp, state) = conv_state();
        let member = [0x6Bu8; 32];
        let ch_a = [0x6Cu8; 32];
        let ch_b = [0x6Du8; 32];
        for ch in [&ch_a, &ch_b] {
            state
                .db
                .register_actor_channel(&member, ch)
                .await
                .expect("register member");
            crate::segments::conv::append(
                &state.conv_segments,
                &state.db,
                ch,
                b"sealed-conv-body",
                1_715_000_000_000,
            )
            .await
            .expect("conv append");
        }
        // ch_b (the second channel visited) is a pure-backup destination.
        let name = format!("__conv/{}", hex::encode(ch_b));
        state
            .db
            .create_folder_with_options(
                &name,
                &ch_b,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let req = SnapshotCreateMessageKindRequest {
            kind: "conv".into(),
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let err = create_message_kind_handler()(state.clone(), member, encode_req(&req))
            .await
            .expect_err("batch containing a pure-backup channel must be rejected");
        assert_eq!(err.code, "fauna.filesync.snapshot.pure_backup_destination");

        // Neither channel got a row — including ch_a, visited before ch_b.
        assert_eq!(
            conv_snapshot_rows(&state, &ch_a).await,
            0,
            "ch_a must have zero rows: the gate refuses before any row is created"
        );
        assert_eq!(conv_snapshot_rows(&state, &ch_b).await, 0);
    }

    #[tokio::test]
    async fn create_conv_non_member_denied() {
        let (_tmp, state) = conv_state();
        let channel = [0x66u8; 32];
        let outsider = [0x67u8; 32];
        // `outsider` is NOT a member of `channel`.

        let req = SnapshotCreateMessageKindRequest {
            kind: "conv".into(),
            actor_id: Some(ActorId(channel)),
            extra: std::collections::BTreeMap::new(),
        };
        let err = create_message_kind_handler()(state, outsider, encode_req(&req))
            .await
            .expect_err("non-member create must be denied");
        assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");
    }

    #[tokio::test]
    async fn create_conv_no_channels_is_vacuous_success() {
        // None scope + bearer in zero channels → vacuous success (no rows).
        let (_tmp, state) = conv_state();
        let loner = [0x68u8; 32];

        let req = SnapshotCreateMessageKindRequest {
            kind: "conv".into(),
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = create_message_kind_handler()(state, loner, encode_req(&req))
            .await
            .expect("zero-channel batched create is a no-op success");
        let reply: SnapshotCreateMessageKindReply = decode_reply(&bytes);
        assert!(reply.snapshot_ids.is_empty());
        assert_eq!(reply.snapshot_id, 0);
    }

    #[tokio::test]
    async fn create_conv_rejects_pure_backup_destination() {
        // Gate 4 (Plan 9): even a channel member cannot snapshot a channel whose
        // `__conv/<hex>` reserved set is a custody copy — no local manifest to pin.
        let (_tmp, state) = conv_state();
        let channel = [0x69u8; 32];
        let member = [0x6Au8; 32];
        state
            .db
            .register_actor_channel(&member, &channel)
            .await
            .expect("register member");
        let name = format!("__conv/{}", hex::encode(channel));
        state
            .db
            .create_folder_with_options(
                &name,
                &channel,
                crate::db::FolderOptions {
                    custody_copy: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let req = SnapshotCreateMessageKindRequest {
            kind: "conv".into(),
            actor_id: Some(ActorId(channel)),
            extra: std::collections::BTreeMap::new(),
        };
        let err = create_message_kind_handler()(state, member, encode_req(&req))
            .await
            .expect_err("pure-backup destination must be rejected");
        assert_eq!(err.code, "fauna.filesync.snapshot.pure_backup_destination");
    }

    // ── restore_message_kind: conv (Plan 8 T5) ───────────────
    //
    // Conv restore reverts a channel's conv segment_records to a pinned
    // snapshot and reinstates the restoring member's actor_channels row.
    // Auth is channel membership (not owner-equality — `resolve_snapshot_owner`
    // returns the channel_id for conv). Uses the same per-test conv_state()
    // helper (PID-shared for-test dir → distinct channel/actor ids per test).

    /// Append `n` conv records to a channel, finalize so they're flushed to a
    /// closed segment, then create a conv snapshot pinning that manifest.
    /// Returns the snapshot id. The records all land in one bucket → one
    /// segment, which the post-finalize manifest pins.
    async fn seed_conv_snapshot(state: &Arc<AppState>, channel: &[u8; 32], n: u32) -> i64 {
        for i in 0..n {
            crate::segments::conv::append(
                &state.conv_segments,
                &state.db,
                channel,
                format!("conv-body-{i}").as_bytes(),
                1_715_000_000_000,
            )
            .await
            .expect("conv append");
        }
        // finalize_open so the captured manifest reflects the flushed records
        // (mirrors the create_conv path).
        state
            .conv_segments
            .finalize_open(channel)
            .await
            .expect("finalize");
        let manifest = state
            .conv_segments
            .load_manifest(channel)
            .await
            .expect("load_manifest");
        let content =
            fauna_core::encoding::canonical_encode(&manifest).expect("serialize conv Manifest");
        let fs_id = state
            .db
            .get_or_create_reserved_conv_folder(channel)
            .await
            .expect("reserved conv folder");
        state
            .db
            .create_message_kind_snapshot_row(fs_id, "conv", Some(&content), None)
            .await
            .expect("create conv snapshot row")
    }

    #[tokio::test]
    async fn restore_conv_reverts_to_snapshot_state() {
        let (_tmp, state) = conv_state();
        let channel = [0x70u8; 32];
        let member = [0x71u8; 32];
        state
            .db
            .register_actor_channel(&member, &channel)
            .await
            .expect("register member");

        // N=2 records, snapshot at T (pins the closed segment holding them).
        let snap_id = seed_conv_snapshot(&state, &channel, 2).await;

        // Read state at T: exactly the 2 pinned records.
        let at_t = crate::segments::conv::read_after_seq(
            &state.conv_segments,
            &state.db,
            &channel,
            0,
            100,
        )
        .await
        .expect("read at T");
        assert_eq!(at_t.len(), 2, "two records at snapshot time");

        // Append M=3 more *after* the snapshot. Because the snapshot finalized
        // the open segment, these rotate into a NEW segment that the pinned
        // manifest does NOT include — so restore physically reverts them.
        for i in 0..3 {
            crate::segments::conv::append(
                &state.conv_segments,
                &state.db,
                &channel,
                format!("post-snapshot-{i}").as_bytes(),
                1_715_000_000_000,
            )
            .await
            .expect("post-snapshot append");
        }
        let after_appends = crate::segments::conv::read_after_seq(
            &state.conv_segments,
            &state.db,
            &channel,
            0,
            100,
        )
        .await
        .expect("read after appends");
        assert_eq!(
            after_appends.len(),
            5,
            "all five records visible before restore"
        );

        // Restore as the member.
        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: snap_id,
            confirm_id: snap_id.to_string(),
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = restore_message_kind_handler()(state.clone(), member, encode_req(&req))
            .await
            .expect("conv restore must succeed");
        let reply: SnapshotRestoreMessageKindReply = decode_reply(&bytes);
        assert_eq!(reply.kind, "conv");
        assert_eq!(reply.snapshot_id, snap_id);
        // conv has no bridge → no spurious "bridge AUTH" advisory.
        assert!(
            reply.config_present,
            "conv restore: wrapped-MLS-blob advisory is N/A"
        );
        assert!(
            reply.note.is_empty(),
            "conv restore must carry no bridge-AUTH note"
        );

        // Post-snapshot records are gone; exactly the first 2 (by seq) survive.
        let reverted = crate::segments::conv::read_after_seq(
            &state.conv_segments,
            &state.db,
            &channel,
            0,
            100,
        )
        .await
        .expect("read after restore");
        assert_eq!(
            reverted.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec![1, 2],
            "restore reverts to the 2 pinned records; post-snapshot records dropped"
        );
        assert_eq!(reverted[0].1, b"conv-body-0".to_vec());
        assert_eq!(reverted[1].1, b"conv-body-1".to_vec());
    }

    #[tokio::test]
    async fn restore_conv_preserves_the_feed_changed_seq_cursor_without_a_restart() {
        // The conv mirror rebuild must carry `changed_seq` at restore time, not
        // just at the next boot's backfill. `restore_conv` DELETEs the
        // channel's whole conv mirror before rebuilding it from the pinned
        // segments' floors, so a rebuild that omits `changed_seq` leaves every
        // restored row on the `0` sentinel — and the generalized feed's
        // class-1 walk (`records_db::content_feed_after`) is `HAVING at > ?`,
        // so a row at `changed_seq = 0` is invisible to a walk from cursor 0.
        // `backfill_segment_records_changed_seq` heals this at the NEXT nest
        // boot, but there is no operator and no app affordance restarts a
        // nest (`principles.md` § One configuration surface), so the window is
        // "until the box happens to reboot" — this test proves the record is
        // visible immediately, with no restart in between.
        let (_tmp, state) = conv_state();
        let channel = [0x72u8; 32];
        let member = [0x73u8; 32];
        state
            .db
            .register_actor_channel(&member, &channel)
            .await
            .expect("register member");

        let snap_id = seed_conv_snapshot(&state, &channel, 3).await;

        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: snap_id,
            confirm_id: snap_id.to_string(),
            extra: std::collections::BTreeMap::new(),
        };
        restore_message_kind_handler()(state.clone(), member, encode_req(&req))
            .await
            .expect("conv restore must succeed");

        // No restart, no backfill call — read the feed exactly as a
        // dehydrating replica would immediately after the restore.
        let conn = state.db.conn().await;
        let feed = crate::segments::records_db::content_feed_after(&conn, &channel, "conv", 0, 100)
            .expect("content_feed_after");
        assert_eq!(
            feed.len(),
            3,
            "a restored conv corpus must be visible to the class-1 feed walk \
             from cursor 0 immediately — a changed_seq=0 sentinel is invisible \
             to `HAVING at > ?` until the next boot's backfill runs"
        );
    }

    /// **A pre-cutover pinned snapshot cannot re-file old-shape record ids.**
    /// The identity guard on every restore-from-disk rebuild
    /// (`verify_pinned_record_identity`): a record whose block does not hash to
    /// the CID it is filed under is a snapshot taken before this kind's
    /// 2026-08-17 record-identity cutover, and restoring it would put
    /// permanently un-admittable records back on a store whose boot reset had
    /// already cleared them (`message-segment-store.md` § Record identity per
    /// kind → *Transition superseded*, constraint (iii) in reduced form).
    ///
    /// Reachability is the whole point: the CARv2 reader verifies only the
    /// *framed* CID at the indexed offset against the one requested — it never
    /// re-hashes the block — so such a segment reads back perfectly and the
    /// rebuild would have copied its cids into the mirror verbatim.
    ///
    /// The old shape is minted here exactly as the retired code did
    /// (`blake3(channel ‖ seq_le ‖ body)` under a dag-cbor CID) and filed
    /// through the real `SegmentManager`, which takes the CID from its caller —
    /// so this is a genuine pre-cutover segment, not a corrupted one.
    #[tokio::test]
    async fn restore_conv_refuses_a_snapshot_whose_records_predate_the_identity_cutover() {
        let (_tmp, state) = conv_state();
        let channel = [0x7au8; 32];
        let member = [0x7bu8; 32];
        state
            .db
            .register_actor_channel(&member, &channel)
            .await
            .expect("register member");

        // One record filed under the RETIRED identity: the seq-derived digest,
        // not the content hash of the bytes stored under it.
        let body = b"pre-cutover-sealed-payload".to_vec();
        let seq: i64 = 1;
        let old_digest = {
            let mut hasher = blake3::Hasher::new();
            hasher.update(&channel);
            hasher.update(&seq.to_le_bytes());
            hasher.update(&body);
            *hasher.finalize().as_bytes()
        };
        let old_cid = fauna_cbor::Cid::from_digest_dag_cbor(old_digest);
        let envelope = fauna_mls::segments::ConvRecordEnvelope::new(body.clone());
        let (content_cid, env_bytes) =
            fauna_mls::segments::encode_record(&envelope).expect("encode");
        assert_ne!(
            old_cid, content_cid,
            "fixture: the retired identity must differ from the content hash"
        );
        let floor_bytes =
            fauna_mls::segments::serialize_floor(&fauna_mls::segments::ConvFloorMetadata {
                format_version: 1,
                received_at: 1_715_000_000_000,
                seq,
            })
            .expect("floor");
        state
            .conv_segments
            .append_record_with_bucket(&channel, old_cid, &env_bytes, &floor_bytes, "2026-08")
            .await
            .expect("append under the retired identity");

        // Pin it in a manifest, exactly as the snapshot-create path does.
        state
            .conv_segments
            .finalize_open(&channel)
            .await
            .expect("finalize");
        let manifest = state
            .conv_segments
            .load_manifest(&channel)
            .await
            .expect("load_manifest");
        let content =
            fauna_core::encoding::canonical_encode(&manifest).expect("serialize conv Manifest");

        // A live post-pin record through the REAL append path — the thing the
        // restore's `DELETE FROM segment_records` would take with it. Its
        // survival is the rollback proof.
        crate::segments::conv::append(
            &state.conv_segments,
            &state.db,
            &channel,
            b"live-post-pin-record",
            1_715_000_000_001,
        )
        .await
        .expect("live append");

        // `restore_conv` now asserts the caller holds the "gc" op lock, as the
        // real dispatcher (`restore_message_kind_handler`) always does — this
        // direct call must acquire it too.
        assert!(
            state
                .db
                .try_acquire_op_lock("gc", -1, "test")
                .await
                .expect("try_acquire_op_lock"),
            "gc lock must be free in a fresh fixture"
        );
        let err = restore_conv(&state, &channel, Some(&content), &member)
            .await
            .expect_err("a pre-cutover snapshot must not restore");
        let rendered = format!("{err:?}");
        assert!(
            rendered.contains("record-identity cutover") || rendered.contains("does not hash"),
            "the refusal must name the identity cutover; got {rendered}"
        );

        // Fail-closed: the transaction rolled back whole. The old-shape cid
        // never entered the mirror, and the live record is still there.
        let rows = crate::segments::conv::read_after_seq(
            &state.conv_segments,
            &state.db,
            &channel,
            0,
            100,
        )
        .await
        .expect("read after refused restore");
        assert_eq!(
            rows.iter().map(|r| r.1.clone()).collect::<Vec<_>>(),
            vec![b"live-post-pin-record".to_vec()],
            "the refused restore must neither re-file the old-shape record nor \
             drop the live one it would have replaced"
        );
    }

    #[tokio::test]
    async fn restore_conv_reinstates_membership() {
        // restore_conv replays an `INSERT OR IGNORE actor_channels` for the
        // *restoring bearer*, so a restore reinstates that bearer's roster row.
        //
        // Membership auth reads `list_channel_actors` at call time, so a member
        // whose row is deleted before the call can no longer pass auth — there's
        // no await-spanning window to delete-then-restore as the same non-admin
        // member. We therefore drive the restore with an ADMIN bearer (admin
        // passes auth regardless of the roster) who is NOT a roster member, and
        // assert the restore creates the admin's actor_channels row. This is the
        // clean expression of "the restoring member's row is (re)instated by
        // restore" given the current-membership auth gate.
        let (_tmp, state) = conv_state();
        let channel = [0x72u8; 32];
        let admin = [0x02u8; 32];
        state.db.add_admin_actor(&admin).await.expect("add admin");

        // A real member seeds the channel + snapshot.
        let member = [0x73u8; 32];
        state
            .db
            .register_actor_channel(&member, &channel)
            .await
            .expect("register member");
        let snap_id = seed_conv_snapshot(&state, &channel, 1).await;

        // The admin is NOT a roster member of the channel pre-restore.
        assert!(
            !state
                .db
                .is_actor_in_channel(&admin, &channel)
                .await
                .expect("is_actor_in_channel"),
            "admin not a roster member before restore"
        );

        // Admin restores (auth via is_admin). The restoring bearer (admin) gets
        // an actor_channels row reinstated.
        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: snap_id,
            confirm_id: snap_id.to_string(),
            extra: std::collections::BTreeMap::new(),
        };
        restore_message_kind_handler()(state.clone(), admin, encode_req(&req))
            .await
            .expect("admin conv restore must succeed");

        assert!(
            state
                .db
                .is_actor_in_channel(&admin, &channel)
                .await
                .expect("is_actor_in_channel"),
            "restoring bearer's actor_channels row reinstated after restore"
        );
    }

    #[tokio::test]
    async fn restore_conv_non_member_denied() {
        let (_tmp, state) = conv_state();
        let channel = [0x74u8; 32];
        let member = [0x75u8; 32];
        let outsider = [0x76u8; 32];
        state
            .db
            .register_actor_channel(&member, &channel)
            .await
            .expect("register member");
        let snap_id = seed_conv_snapshot(&state, &channel, 1).await;

        // `outsider` is neither a channel member nor an admin.
        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: snap_id,
            confirm_id: snap_id.to_string(),
            extra: std::collections::BTreeMap::new(),
        };
        let err = restore_message_kind_handler()(state, outsider, encode_req(&req))
            .await
            .expect_err("non-member conv restore must be denied");
        assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");
    }

    // ── delete_immediate ─────────────────────────────────────

    #[tokio::test]
    async fn delete_immediate_owner_happy_path() {
        let state = fixture_state();
        let actor = [0xaau8; 32];
        let ids = seed_snapshots(&state, &actor, 4).await;
        let target = *ids.last().unwrap();

        let req = SnapshotDeleteImmediateRequest {
            snapshot_id: target,
            confirm_id: target.to_string(),
            acknowledge: IMMEDIATE_DELETE_ACK_TEXT.into(),
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = delete_immediate_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect("delete ok");
        let reply: SnapshotDeleteImmediateReply = decode_reply(&bytes);
        assert_eq!(reply.snapshot_id, target);
        assert_eq!(reply.segment_retention_days, 14);

        assert!(state.db.get_snapshot(target).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_immediate_rejects_non_owner() {
        let state = fixture_state();
        let owner = [0xbbu8; 32];
        let admin = [0x01u8; 32];
        state.db.add_admin_actor(&admin).await.unwrap();

        let ids = seed_snapshots(&state, &owner, 4).await;
        let target = *ids.last().unwrap();

        let req = SnapshotDeleteImmediateRequest {
            snapshot_id: target,
            confirm_id: target.to_string(),
            acknowledge: IMMEDIATE_DELETE_ACK_TEXT.into(),
            extra: std::collections::BTreeMap::new(),
        };
        let err = delete_immediate_handler()(state.clone(), admin, encode_req(&req))
            .await
            .expect_err("admin-not-owner must be rejected");
        assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");
        assert!(state.db.get_snapshot(target).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delete_immediate_rejects_bad_confirm_id() {
        let state = fixture_state();
        let actor = [0xccu8; 32];
        let ids = seed_snapshots(&state, &actor, 4).await;
        let target = *ids.last().unwrap();

        let req = SnapshotDeleteImmediateRequest {
            snapshot_id: target,
            confirm_id: (target + 1).to_string(),
            acknowledge: IMMEDIATE_DELETE_ACK_TEXT.into(),
            extra: std::collections::BTreeMap::new(),
        };
        let err = delete_immediate_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect_err("bad confirm_id must be rejected");
        assert_eq!(err.code, "fauna.filesync.snapshot.confirm_mismatch");
        assert!(state.db.get_snapshot(target).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delete_immediate_rejects_bad_acknowledge() {
        let state = fixture_state();
        let actor = [0xddu8; 32];
        let ids = seed_snapshots(&state, &actor, 4).await;
        let target = *ids.last().unwrap();

        let req = SnapshotDeleteImmediateRequest {
            snapshot_id: target,
            confirm_id: target.to_string(),
            acknowledge: "wrong text".into(),
            extra: std::collections::BTreeMap::new(),
        };
        let err = delete_immediate_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect_err("bad acknowledge must be rejected");
        assert_eq!(err.code, "fauna.filesync.snapshot.acknowledge_mismatch");
        assert!(state.db.get_snapshot(target).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delete_immediate_enforces_hard_floor() {
        let state = fixture_state();
        let actor = [0xeeu8; 32];
        // Seed exactly 3 — floor is >3, so deletion must be refused.
        let ids = seed_snapshots(&state, &actor, 3).await;
        let target = *ids.last().unwrap();

        let req = SnapshotDeleteImmediateRequest {
            snapshot_id: target,
            confirm_id: target.to_string(),
            acknowledge: IMMEDIATE_DELETE_ACK_TEXT.into(),
            extra: std::collections::BTreeMap::new(),
        };
        let err = delete_immediate_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect_err("hard floor must refuse");
        assert_eq!(err.code, "fauna.filesync.snapshot.hard_floor_breach");
        assert!(state.db.get_snapshot(target).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delete_immediate_snapshot_not_found() {
        let state = fixture_state();
        let actor = [0xffu8; 32];

        let req = SnapshotDeleteImmediateRequest {
            snapshot_id: 999_999,
            confirm_id: "999999".into(),
            acknowledge: IMMEDIATE_DELETE_ACK_TEXT.into(),
            extra: std::collections::BTreeMap::new(),
        };
        let err = delete_immediate_handler()(state, actor, encode_req(&req))
            .await
            .expect_err("missing snapshot must surface as not_found");
        assert_eq!(err.code, "fauna.filesync.snapshot.not_found");
    }

    // ── restore_message_kind ─────────────────────────────────

    #[tokio::test]
    async fn restore_rejects_bad_confirm_id() {
        let state = fixture_state();
        let actor = [0xa1u8; 32];
        let snap_id = seed_mail_snapshot(&state, &actor).await;

        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: snap_id,
            confirm_id: (snap_id + 1).to_string(),
            extra: std::collections::BTreeMap::new(),
        };
        let err = restore_message_kind_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect_err("bad confirm_id must be rejected");
        assert_eq!(err.code, "fauna.filesync.snapshot.confirm_mismatch");
        assert!(state.db.get_snapshot(snap_id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn restore_rejects_non_owner() {
        let state = fixture_state();
        let owner = [0xa2u8; 32];
        let other = [0xa3u8; 32];
        let snap_id = seed_mail_snapshot(&state, &owner).await;

        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: snap_id,
            confirm_id: snap_id.to_string(),
            extra: std::collections::BTreeMap::new(),
        };
        let err = restore_message_kind_handler()(state, other, encode_req(&req))
            .await
            .expect_err("non-owner must be rejected");
        assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");
    }

    #[tokio::test]
    async fn restore_rejects_folder_snapshot() {
        let state = fixture_state();
        let actor = [0xa5u8; 32];
        // seed_snapshots creates a folder with message_kind = NULL.
        let ids = seed_snapshots(&state, &actor, 4).await;
        let target = *ids.last().unwrap();

        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: target,
            confirm_id: target.to_string(),
            extra: std::collections::BTreeMap::new(),
        };
        let err = restore_message_kind_handler()(state, actor, encode_req(&req))
            .await
            .expect_err("folder snapshot must be rejected here");
        assert_eq!(err.code, "fauna.filesync.snapshot.unknown_kind");
    }

    #[tokio::test]
    async fn restore_snapshot_not_found() {
        let state = fixture_state();
        let actor = [0xa6u8; 32];

        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: 999_999,
            confirm_id: "999999".into(),
            extra: std::collections::BTreeMap::new(),
        };
        let err = restore_message_kind_handler()(state, actor, encode_req(&req))
            .await
            .expect_err("missing snapshot must surface as not_found");
        assert_eq!(err.code, "fauna.filesync.snapshot.not_found");
    }

    #[tokio::test]
    async fn restore_mail_replays_manifest_end_to_end() {
        // T4 integration test: exercises the full restore_message_kind_handler
        // flow for mail — decodes Manifest + MailPlacementManifest, calls
        // replay_mail_manifest_into_sqlite, and verifies bridge_imap_mailbox_state.
        let state = fixture_state();
        let actor = [0x74u8; 32];

        // Build an empty unified Manifest (no live_segments → disk-check loop
        // is a no-op; rebuild_mail_segment_records_from_disk iterates zero
        // times). Canonical-dag-cbor-encoded to match the production capture
        // path (`canonical_encode(&Manifest)` at the snapshot-capture handler)
        // and the restore decode (`canonical_decode::<Manifest>`). Must be
        // `fauna_segment_store::Manifest` (kind-tagged, 3 fields) — the wire
        // shape restore_mail decodes since the snapshot-blob encoding was
        // unified in Plan 6 T11, which dropped the legacy 2-field
        // `MailManifest`; that type does not decode interchangeably.
        let manifest = fauna_segment_store::Manifest::empty("mail");
        let content_blob =
            fauna_core::encoding::canonical_encode(&manifest).expect("serialize empty Manifest");

        // Build a MailPlacementManifest with a single INBOX mailbox.
        let mut placement = fauna_mail::segments::placement::MailPlacementManifest::new();
        placement
            .mailboxes
            .push(fauna_mail::segments::placement::MailboxState {
                name: "INBOX".to_string(),
                uid_validity: 1,
                uid_next: 2,
                highestmodseq: 1,
                attrs: vec![],
                pruned_modseq: 0,
            });
        let placement_blob = fauna_core::encoding::canonical_encode(&placement)
            .expect("serialize MailPlacementManifest");

        // Create the reserved folder and snapshot row.
        let fs_id = state
            .db
            .get_or_create_reserved_folder(&actor, "mail")
            .await
            .expect("get_or_create_reserved_folder");
        let snap_id = state
            .db
            .create_message_kind_snapshot_row(
                fs_id,
                "mail",
                Some(&content_blob),
                Some(&placement_blob),
            )
            .await
            .expect("create_message_kind_snapshot_row");

        // Call the handler — expect Ok(bytes).
        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: snap_id,
            confirm_id: snap_id.to_string(),
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = restore_message_kind_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect("restore mail end-to-end must succeed");

        // Decode and validate the reply.
        let reply: SnapshotRestoreMessageKindReply = decode_reply(&bytes);
        assert_eq!(reply.snapshot_id, snap_id);
        assert_eq!(reply.kind, "mail");

        // Verify bridge_imap_mailbox_state has the INBOX row.
        // Use an explicit scope block to drop the MutexGuard before any
        // subsequent acquisition (avoids re-entrant Mutex deadlock).
        let mb_count: i64 = {
            let conn = state.db.conn().await;
            conn.query_row(
                "SELECT COUNT(*) FROM bridge_imap_mailbox_state \
                 WHERE actor_id = ?1 AND mailbox = 'INBOX'",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("query bridge_imap_mailbox_state")
        };
        assert_eq!(mb_count, 1, "INBOX row must exist after replay");
    }

    #[tokio::test]
    async fn restore_calendar_replays_manifest_end_to_end() {
        // T5 integration test: exercises the full restore_message_kind_handler
        // flow for calendar — decodes CalPlacementManifest, calls
        // replay_cal_manifest_into_sqlite, and verifies bridge_caldav_calendars.
        let state = fixture_state();
        let actor = [0x75u8; 32];

        // Build a CalPlacementManifest with one CalendarState. The content
        // manifest is required since S6.9 — an empty calendar-kind Manifest
        // stands in (no events, so no segments to walk).
        let mut placement = fauna_calendar::segments::placement::CalPlacementManifest::new();
        placement
            .calendars
            .push(fauna_calendar::segments::placement::CalendarState {
                calendar_id: [0xCAu8; 32],
                encrypted_metadata: vec![0u8; 64],
                highestmodseq: 42,
            });
        let placement_blob = fauna_core::encoding::canonical_encode(&placement)
            .expect("serialize CalPlacementManifest");

        let content_blob = fauna_core::encoding::canonical_encode(
            &fauna_segment_store::Manifest::empty("calendar"),
        )
        .expect("serialize empty calendar Manifest");

        // Create the reserved folder and snapshot row.
        let fs_id = state
            .db
            .get_or_create_reserved_folder(&actor, "calendar")
            .await
            .expect("get_or_create_reserved_folder calendar");
        let snap_id = state
            .db
            .create_message_kind_snapshot_row(
                fs_id,
                "calendar",
                Some(&content_blob),
                Some(&placement_blob),
            )
            .await
            .expect("create_message_kind_snapshot_row calendar");

        // Call the handler — expect Ok(bytes).
        // Ownership resolves via the folders join; no extra actor row needed.
        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: snap_id,
            confirm_id: snap_id.to_string(),
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = restore_message_kind_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect("restore calendar end-to-end must succeed");

        // Decode and validate the reply.
        let reply: SnapshotRestoreMessageKindReply = decode_reply(&bytes);
        assert_eq!(reply.snapshot_id, snap_id);
        assert_eq!(reply.kind, "calendar");

        // Verify bridge_caldav_calendars has the row.
        // Scope block drops the MutexGuard before any subsequent acquisition.
        let cal_count: i64 = {
            let conn = state.db.conn().await;
            conn.query_row(
                "SELECT COUNT(*) FROM bridge_caldav_calendars WHERE actor_id = ?1",
                rusqlite::params![actor.as_slice()],
                |r| r.get(0),
            )
            .expect("query bridge_caldav_calendars")
        };
        assert_eq!(cal_count, 1, "calendar row must exist after replay");
    }

    /// Genuinely sealed test bytes — the only currency `segments::cal`
    /// appends accept (S6.12 structural seal).
    fn sealed_fixture(plaintext: &[u8]) -> Vec<u8> {
        use fauna_mls::wrapped_blob::{derive_recipient_hpke_keypair, seal_to_recipient};
        let (_secret, pubkey) = derive_recipient_hpke_keypair(&[0x5Eu8; 32]);
        seal_to_recipient(plaintext, &pubkey)
            .expect("seal test fixture")
            .to_canonical_bytes()
            .expect("canonical test fixture")
    }

    /// THE S6.9 no-data-loss heart, end to end through the production
    /// handlers: an event written the production way (segment append →
    /// empty-body row → v2 placement record) is pinned by
    /// create_message_kind, survives a full wipe, and restore_message_kind
    /// rebuilds the row (etag / modseq / sidecar from placement; identity /
    /// hint from floor + envelope), the `segment_records` mirror, the
    /// expunged tombstone — and the sealed body still serves byte-identical
    /// from the segment.
    #[tokio::test]
    async fn create_then_restore_calendar_round_trips_an_event() {
        use crate::db::bridge_caldav::{ReplaceCaldavEventOutcome, derive_caldav_event_id};
        use fauna_calendar::segments::floor::CalFloorMetadata;
        use fauna_calendar::segments::placement::CalPlacementRecord;
        use fauna_mls::wrapped_blob::SealedRecordBytes;

        let state = fixture_state();
        let actor = [0x76u8; 32];
        let cal_id = [0xCBu8; 32];
        let uid_hash = [0xCDu8; 32];
        let now = 1_752_000_000i64;

        // Seed the production way: calendar row, sealed segment append,
        // empty-body event row, v2 placement records.
        state
            .db
            .insert_bridge_caldav_calendar(&actor, &cal_id, b"meta", now)
            .await
            .expect("provision calendar");
        state
            .cal_placement
            .append_event(
                &actor,
                &CalPlacementRecord::ProvisionCalendar {
                    calendar_id: cal_id,
                    encrypted_metadata: b"meta".to_vec(),
                },
            )
            .await
            .expect("journal provision");

        let sealed_body = sealed_fixture(b"BEGIN:VEVENT the real body END:VEVENT");
        let sealed_hint = sealed_fixture(b"index-hint");
        let event_id = derive_caldav_event_id(&actor, now, &sealed_body);
        let floor = CalFloorMetadata {
            calendar_id: cal_id,
            event_id,
            uid_hash: uid_hash.to_vec(),
            ciphertext_size: sealed_body.len() as u32,
            internal_date: now,
            created_at: now,
            ..Default::default()
        };
        let record_cid = crate::segments::cal::ensure_in_segment(
            &state.cal_segments,
            &state.db,
            &actor,
            &SealedRecordBytes::verify(sealed_body.clone()).expect("sealed"),
            &SealedRecordBytes::verify(sealed_hint.clone()).expect("sealed"),
            &floor,
        )
        .await
        .expect("segment append");
        let outcome = state
            .db
            .replace_caldav_event_by_uid(
                &actor,
                &cal_id,
                &uid_hash,
                None,
                &event_id,
                &record_cid,
                &sealed_hint,
                Some(b"the-sidecar"),
                now,
                sealed_body.len() as u32,
                now,
            )
            .await
            .expect("row insert");
        let ReplaceCaldavEventOutcome::Created {
            etag,
            modseq,
            encrypted_fauna_ext,
            ..
        } = outcome
        else {
            panic!("expected Created, got {outcome:?}");
        };
        state
            .cal_placement
            .append_event(
                &actor,
                &CalPlacementRecord::PutEvent {
                    calendar_id: cal_id,
                    uid_hash,
                    etag: etag.clone(),
                    modseq: modseq as u64,
                    ciphertext_size: sealed_body.len() as u32,
                    event_id,
                    encrypted_fauna_ext,
                },
            )
            .await
            .expect("journal put");
        // A second, deleted event → a v2 tombstone the restore must carry
        // into bridge_caldav_expunged.
        state
            .cal_placement
            .append_event(
                &actor,
                &CalPlacementRecord::DeleteEvent {
                    calendar_id: cal_id,
                    uid_hash: [0xCEu8; 32],
                    modseq: modseq as u64 + 1,
                    event_id: [0xEAu8; 32],
                    deleted_at: now + 5,
                },
            )
            .await
            .expect("journal delete");

        // Snapshot through the production create handler.
        let req = SnapshotCreateMessageKindRequest {
            kind: "calendar".into(),
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = create_message_kind_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect("create snapshot");
        let reply: SnapshotCreateMessageKindReply = decode_reply(&bytes);
        let snap_id = reply.snapshot_id;

        // Wipe everything the restore must rebuild.
        {
            let conn = state.db.conn().await;
            for table in [
                "bridge_caldav_calendars",
                "bridge_caldav_events",
                "bridge_caldav_expunged",
            ] {
                conn.execute(
                    &format!("DELETE FROM {table} WHERE actor_id = ?1"),
                    rusqlite::params![actor.as_slice()],
                )
                .unwrap();
            }
            conn.execute(
                "DELETE FROM segment_records WHERE scope_id = ?1 AND kind = 'calendar'",
                rusqlite::params![actor.as_slice()],
            )
            .unwrap();
        }

        // Restore through the production handler.
        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: snap_id,
            confirm_id: snap_id.to_string(),
            extra: std::collections::BTreeMap::new(),
        };
        restore_message_kind_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect("restore must succeed");

        // The row is back with the placement-sourced allocated state.
        let (row_etag, row_modseq, row_hint, row_ext, row_uid): (
            String,
            i64,
            Vec<u8>,
            Option<Vec<u8>>,
            Vec<u8>,
        ) = {
            let conn = state.db.conn().await;
            conn.query_row(
                "SELECT etag, modseq, encrypted_index_hint, encrypted_fauna_ext, uid_hash
                 FROM bridge_caldav_events
                 WHERE actor_id = ?1 AND calendar_id = ?2 AND event_id = ?3",
                rusqlite::params![actor.as_slice(), cal_id.as_slice(), event_id.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .expect("restored event row")
        };
        assert_eq!(row_etag, etag);
        assert_eq!(row_modseq, modseq);
        assert_eq!(row_hint, sealed_hint, "hint restored from the envelope");
        assert_eq!(
            row_ext,
            Some(b"the-sidecar".to_vec()),
            "the sidecar — recoverable only from the placement journal"
        );
        assert_eq!(row_uid, uid_hash.to_vec());

        // The mirror is back and the sealed body serves byte-identical.
        let (envelope, _floor) =
            crate::segments::cal::read_record(&state.cal_segments, &state.db, &actor, &record_cid)
                .await
                .expect("read record")
                .expect("record resolvable through the rebuilt mirror");
        assert_eq!(
            envelope.encrypted_body, sealed_body,
            "the body round-trips byte-identically (no-data-loss)"
        );

        // The tombstone is back in the serve-side expunged table.
        let exp_count: i64 = {
            let conn = state.db.conn().await;
            conn.query_row(
                "SELECT COUNT(*) FROM bridge_caldav_expunged
                 WHERE actor_id = ?1 AND event_id = ?2",
                rusqlite::params![actor.as_slice(), [0xEAu8; 32].as_slice()],
                |r| r.get(0),
            )
            .expect("count expunged")
        };
        assert_eq!(exp_count, 1, "v2 tombstone restored");
    }

    /// Card twin of the calendar round-trip — the two DAV stores must not
    /// diverge in DR posture. Leaner: one card, no tombstone (the tombstone
    /// replay is pinned per kind in restore::card).
    #[tokio::test]
    async fn create_then_restore_card_round_trips_a_card() {
        use crate::db::bridge_carddav::{ReplaceCarddavCardOutcome, derive_carddav_card_id};
        use fauna_contacts::segments::floor::CardFloorMetadata;
        use fauna_contacts::segments::placement::CardPlacementRecord;
        use fauna_mls::wrapped_blob::SealedRecordBytes;

        let state = fixture_state();
        let actor = [0x77u8; 32];
        let book_id = [0xDBu8; 32];
        let uid_hash = [0xDCu8; 32];
        let now = 1_752_000_000i64;

        state
            .db
            .insert_bridge_carddav_addressbook(&actor, &book_id, b"meta", now)
            .await
            .expect("provision book");
        state
            .card_placement
            .append_event(
                &actor,
                &CardPlacementRecord::ProvisionAddressbook {
                    addressbook_id: book_id,
                    encrypted_metadata: b"meta".to_vec(),
                },
            )
            .await
            .expect("journal provision");

        let sealed_body = sealed_fixture(b"BEGIN:VCARD the real card END:VCARD");
        let sealed_hint = sealed_fixture(b"card-hint");
        let card_id = derive_carddav_card_id(&actor, now, &sealed_body);
        let floor = CardFloorMetadata {
            addressbook_id: book_id,
            card_id,
            uid_hash: uid_hash.to_vec(),
            ciphertext_size: sealed_body.len() as u32,
            internal_date: now,
            created_at: now,
            ..Default::default()
        };
        let record_cid = crate::segments::card::ensure_in_segment(
            &state.card_segments,
            &state.db,
            &actor,
            &SealedRecordBytes::verify(sealed_body.clone()).expect("sealed"),
            &SealedRecordBytes::verify(sealed_hint.clone()).expect("sealed"),
            &floor,
        )
        .await
        .expect("segment append");
        let outcome = state
            .db
            .replace_carddav_card_by_uid(
                &actor,
                &book_id,
                &uid_hash,
                None,
                &card_id,
                &record_cid,
                &sealed_hint,
                Some(b"card-sidecar"),
                now,
                sealed_body.len() as u32,
                now,
            )
            .await
            .expect("row insert");
        let ReplaceCarddavCardOutcome::Created {
            etag,
            modseq,
            encrypted_fauna_ext,
            ..
        } = outcome
        else {
            panic!("expected Created, got {outcome:?}");
        };
        state
            .card_placement
            .append_event(
                &actor,
                &CardPlacementRecord::PutCard {
                    addressbook_id: book_id,
                    uid_hash,
                    etag: etag.clone(),
                    modseq: modseq as u64,
                    ciphertext_size: sealed_body.len() as u32,
                    card_id,
                    encrypted_fauna_ext,
                },
            )
            .await
            .expect("journal put");

        let req = SnapshotCreateMessageKindRequest {
            kind: "card".into(),
            actor_id: None,
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = create_message_kind_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect("create snapshot");
        let reply: SnapshotCreateMessageKindReply = decode_reply(&bytes);
        let snap_id = reply.snapshot_id;

        {
            let conn = state.db.conn().await;
            for table in ["bridge_carddav_addressbooks", "bridge_carddav_cards"] {
                conn.execute(
                    &format!("DELETE FROM {table} WHERE actor_id = ?1"),
                    rusqlite::params![actor.as_slice()],
                )
                .unwrap();
            }
            conn.execute(
                "DELETE FROM segment_records WHERE scope_id = ?1 AND kind = 'card'",
                rusqlite::params![actor.as_slice()],
            )
            .unwrap();
        }

        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: snap_id,
            confirm_id: snap_id.to_string(),
            extra: std::collections::BTreeMap::new(),
        };
        restore_message_kind_handler()(state.clone(), actor, encode_req(&req))
            .await
            .expect("restore must succeed");

        let (row_etag, row_ext): (String, Option<Vec<u8>>) = {
            let conn = state.db.conn().await;
            conn.query_row(
                "SELECT etag, encrypted_fauna_ext FROM bridge_carddav_cards
                 WHERE actor_id = ?1 AND addressbook_id = ?2 AND card_id = ?3",
                rusqlite::params![actor.as_slice(), book_id.as_slice(), card_id.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("restored card row")
        };
        assert_eq!(row_etag, etag);
        assert_eq!(row_ext, Some(b"card-sidecar".to_vec()));

        let (envelope, _floor) = crate::segments::card::read_record(
            &state.card_segments,
            &state.db,
            &actor,
            &record_cid,
        )
        .await
        .expect("read record")
        .expect("record resolvable through the rebuilt mirror");
        assert_eq!(
            envelope.encrypted_body, sealed_body,
            "the card body round-trips byte-identically (no-data-loss)"
        );
    }

    // ── register check ───────────────────────────────────────

    #[test]
    fn register_filesync_handlers_registers_all_kinds() {
        let mut b = crate::rpc_router::RpcRouter::builder();
        register_filesync_handlers(&mut b);
        let r = b.build();
        assert!(r.contains("fauna.filesync.snapshot.create_message_kind"));
        assert!(r.contains("fauna.filesync.snapshot.delete_immediate"));
        assert!(r.contains("fauna.filesync.snapshot.restore_message_kind"));
        assert!(r.contains("fauna.filesync.snapshot.list_restore_history"));
        assert!(r.contains("fauna.filesync.snapshot.list_restore_divergence"));
        assert!(r.contains("fauna.filesync.snapshot.list"));
    }

    #[tokio::test]
    async fn list_handler_returns_only_bearer_message_kind_snapshots() {
        let state = fixture_state();
        let owner = [0xc0u8; 32];
        let other = [0xc1u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        state.db.create_user(&other, "free", "test").await.unwrap();
        let snap_id = seed_mail_snapshot(&state, &owner).await;

        // Owner sees their mail snapshot (unfiltered).
        let req = SnapshotListRequest {
            message_kind: None,
            folder: None,
            limit: 0,
            ..Default::default()
        };
        let bytes = list_handler()(state.clone(), owner, encode_req(&req))
            .await
            .expect("list snapshots");
        let reply: SnapshotListReply = decode_reply(&bytes);
        assert_eq!(reply.rows.len(), 1);
        assert_eq!(reply.rows[0].id, snap_id);
        assert_eq!(reply.rows[0].message_kind.as_deref(), Some("mail"));

        // Filtering to a kind with no snapshot is empty.
        let cal_req = SnapshotListRequest {
            message_kind: Some("calendar".into()),
            folder: None,
            limit: 0,
            ..Default::default()
        };
        let bytes = list_handler()(state.clone(), owner, encode_req(&cal_req))
            .await
            .expect("list snapshots (calendar)");
        let reply: SnapshotListReply = decode_reply(&bytes);
        assert!(reply.rows.is_empty());

        // A different bearer sees none of the owner's snapshots.
        let bytes = list_handler()(state, other, encode_req(&req))
            .await
            .expect("list snapshots (other)");
        let reply: SnapshotListReply = decode_reply(&bytes);
        assert!(reply.rows.is_empty());
    }

    // ── list_restore_history / list_restore_divergence ───────────

    #[tokio::test]
    async fn list_restore_history_returns_only_bearer_rows() {
        let state = fixture_state();
        let owner = [0xb0u8; 32];
        let other = [0xb1u8; 32];
        let snap_id = seed_mail_snapshot(&state, &owner).await;
        state
            .db
            .insert_restore_history(&owner, snap_id, "mail", None)
            .await
            .unwrap();

        let req = SnapshotRestoreHistoryListRequest {
            limit: 0,
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = list_restore_history_handler()(state.clone(), owner, encode_req(&req))
            .await
            .expect("list restore history");
        let reply: SnapshotRestoreHistoryListReply = decode_reply(&bytes);
        assert_eq!(reply.rows.len(), 1);
        assert_eq!(reply.rows[0].snapshot_id, snap_id);
        assert_eq!(reply.rows[0].kinds_restored, "mail");

        // A different bearer sees none of the owner's history.
        let bytes = list_restore_history_handler()(state, other, encode_req(&req))
            .await
            .expect("list restore history (other)");
        let reply: SnapshotRestoreHistoryListReply = decode_reply(&bytes);
        assert!(reply.rows.is_empty());
    }

    #[tokio::test]
    async fn list_restore_divergence_owner_reads_rows() {
        let state = fixture_state();
        let owner = [0xb2u8; 32];
        let snap_id = seed_mail_snapshot(&state, &owner).await;
        state
            .db
            .insert_restore_history(&owner, snap_id, "mail", None)
            .await
            .unwrap();
        {
            let conn = state.db.conn().await;
            let tx = conn.unchecked_transaction().unwrap();
            crate::restore::divergence::write_divergence_row(
                &tx,
                &owner,
                "caldav",
                "aabbcc",
                Some("Apple Calendar/14.0"),
                99,
                42,
                1_700_000_000,
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let req = SnapshotRestoreDivergenceListRequest {
            snapshot_id: snap_id,
            extra: std::collections::BTreeMap::new(),
        };
        let bytes = list_restore_divergence_handler()(state, owner, encode_req(&req))
            .await
            .expect("list restore divergence");
        let reply: SnapshotRestoreDivergenceListReply = decode_reply(&bytes);
        assert_eq!(reply.rows.len(), 1);
        assert_eq!(reply.rows[0].collection, "aabbcc");
        assert_eq!(reply.rows[0].lost_event_count, 57);
    }

    #[tokio::test]
    async fn list_restore_divergence_rejects_non_owner() {
        let state = fixture_state();
        let owner = [0xb3u8; 32];
        let other = [0xb4u8; 32];
        let snap_id = seed_mail_snapshot(&state, &owner).await;

        let req = SnapshotRestoreDivergenceListRequest {
            snapshot_id: snap_id,
            extra: std::collections::BTreeMap::new(),
        };
        let err = list_restore_divergence_handler()(state, other, encode_req(&req))
            .await
            .expect_err("non-owner must be rejected");
        assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");
    }

    #[tokio::test]
    async fn list_restore_divergence_unknown_snapshot_not_found() {
        let state = fixture_state();
        let actor = [0xb5u8; 32];
        let req = SnapshotRestoreDivergenceListRequest {
            snapshot_id: 999_999,
            extra: std::collections::BTreeMap::new(),
        };
        let err = list_restore_divergence_handler()(state, actor, encode_req(&req))
            .await
            .expect_err("missing snapshot must surface as not_found");
        assert_eq!(err.code, "fauna.filesync.snapshot.not_found");
    }

    // ── N1 owner-scoping regression tests (review 2026-06-27 § N1) ────
    //
    // F1 added `authorize_snapshot_owner` to the HTTP snapshot byte routes
    // but left the WS-RPC folder-control twins (Track B15) gated on
    // caller-class only. These assert each now rejects cross-user access.
    // `seed_snapshots` creates "test-set" owned by the given actor; any
    // non-zero attacker actor passes `require_permission` as class User
    // (`caller_class_for_actor`), so only the new owner gate can stop it.
    // Conv snapshots reuse the existing `seed_conv_snapshot` helper above
    // (channel-scoped, `actor_id = channel_id`) + `register_actor_channel`.

    #[tokio::test]
    async fn get_rejects_non_owner() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        let attacker = [0x22u8; 32];
        let ids = seed_snapshots(&state, &owner, 1).await;
        // A KNOWN user who is not the owner — an unseeded actor is refused one
        // arm earlier (central `fauna.bridges.permission_denied`), never
        // reaching the owner gate this test pins.
        state
            .db
            .create_user(&attacker, "free", "attacker")
            .await
            .unwrap();
        let req = SnapshotGetRequest {
            snapshot_id: ids[0],
            extra: Default::default(),
        };
        let err = get_handler()(state, attacker, encode_req(&req))
            .await
            .expect_err("cross-user get must be denied");
        assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");
    }

    #[tokio::test]
    async fn get_allows_owner() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        let ids = seed_snapshots(&state, &owner, 1).await;
        let req = SnapshotGetRequest {
            snapshot_id: ids[0],
            extra: Default::default(),
        };
        let bytes = get_handler()(state, owner, encode_req(&req))
            .await
            .expect("owner get succeeds");
        let reply: SnapshotGetReply = decode_reply(&bytes);
        assert_eq!(reply.id, ids[0]);
        assert_eq!(reply.folder, "test-set");
    }

    #[tokio::test]
    async fn delete_rejects_non_owner() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        let attacker = [0x22u8; 32];
        let ids = seed_snapshots(&state, &owner, 1).await;
        // A KNOWN user who is not the owner (see get_rejects_non_owner).
        state
            .db
            .create_user(&attacker, "free", "attacker")
            .await
            .unwrap();
        let req = SnapshotDeleteRequest {
            snapshot_id: ids[0],
            extra: Default::default(),
        };
        let err = delete_handler()(state, attacker, encode_req(&req))
            .await
            .expect_err("cross-user delete must be denied");
        // Owner gate fires before the hard-floor check.
        assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");
    }

    #[tokio::test]
    async fn undelete_rejects_non_owner() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        let attacker = [0x22u8; 32];
        let ids = seed_snapshots(&state, &owner, 1).await;
        state
            .db
            .soft_delete_snapshot(ids[0])
            .await
            .expect("soft delete");
        // A KNOWN user who is not the owner (see get_rejects_non_owner).
        state
            .db
            .create_user(&attacker, "free", "attacker")
            .await
            .unwrap();
        let req = SnapshotUndeleteRequest {
            snapshot_id: ids[0],
            extra: Default::default(),
        };
        let err = undelete_handler()(state, attacker, encode_req(&req))
            .await
            .expect_err("cross-user undelete must be denied");
        assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");
    }

    #[tokio::test]
    async fn diff_rejects_non_owner() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        let attacker = [0x22u8; 32];
        let ids = seed_snapshots(&state, &owner, 2).await;
        // A KNOWN user who is not the owner (see get_rejects_non_owner).
        state
            .db
            .create_user(&attacker, "free", "attacker")
            .await
            .unwrap();
        let req = SnapshotDiffRequest {
            a: ids[0],
            b: ids[1],
            extra: Default::default(),
        };
        let err = diff_handler()(state, attacker, encode_req(&req))
            .await
            .expect_err("cross-user diff must be denied");
        assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");
    }

    #[tokio::test]
    async fn prune_rejects_non_owner() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        let attacker = [0x22u8; 32];
        state
            .db
            .create_user(&attacker, "free", "test")
            .await
            .unwrap();
        seed_snapshots(&state, &owner, 1).await;
        // The caller-scoped lookup finds no "test-set" owned by the attacker.
        let req = SnapshotPruneRequest {
            folder: "test-set".into(),
            dry_run: true,
            policy: Default::default(),
            ..Default::default()
        };
        let err = prune_handler()(state, attacker, encode_req(&req))
            .await
            .expect_err("cross-user prune must be denied");
        assert_eq!(err.code, "fauna.filesync.snapshot.not_found");
    }

    #[tokio::test]
    async fn prune_allows_owner_dry_run() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        seed_snapshots(&state, &owner, 2).await;
        let req = SnapshotPruneRequest {
            folder: "test-set".into(),
            dry_run: true,
            policy: fauna_protocol::filesync::SnapshotRetentionPolicy {
                keep_last: Some(10),
                ..Default::default()
            },
            ..Default::default()
        };
        let bytes = prune_handler()(state, owner, encode_req(&req))
            .await
            .expect("owner prune dry-run succeeds");
        let reply: SnapshotPruneReply = decode_reply(&bytes);
        assert!(reply.dry_run);
    }

    #[tokio::test]
    async fn check_rejects_non_owner() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        let attacker = [0x22u8; 32];
        state
            .db
            .create_user(&attacker, "free", "test")
            .await
            .unwrap();
        seed_snapshots(&state, &owner, 1).await;
        let req = SnapshotCheckRequest {
            folder: "test-set".into(),
            verify_content: false,
            ..Default::default()
        };
        let err = check_handler()(state, attacker, encode_req(&req))
            .await
            .expect_err("cross-user check must be denied");
        assert_eq!(err.code, "fauna.filesync.snapshot.not_found");
    }

    #[tokio::test]
    async fn create_folder_rejects_non_owner() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        let attacker = [0x22u8; 32];
        state
            .db
            .create_user(&attacker, "free", "test")
            .await
            .unwrap();
        seed_snapshots(&state, &owner, 1).await;
        let req = SnapshotCreateFolderRequest {
            folder: "test-set".into(),
            tags: Vec::new(),
            device_id: None,
            ..Default::default()
        };
        let err = create_folder_handler()(state, attacker, encode_req(&req))
            .await
            .expect_err("cross-user create_folder must be denied");
        assert_eq!(err.code, "fauna.filesync.snapshot.not_found");
    }

    #[tokio::test]
    async fn create_folder_allows_owner() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        seed_snapshots(&state, &owner, 1).await;
        let req = SnapshotCreateFolderRequest {
            folder: "test-set".into(),
            tags: Vec::new(),
            device_id: None,
            ..Default::default()
        };
        let bytes = create_folder_handler()(state, owner, encode_req(&req))
            .await
            .expect("owner create_folder succeeds");
        let reply: SnapshotCreateFolderReply = decode_reply(&bytes);
        assert!(reply.id > 0);
    }

    /// S5b (`file-sync.md` § Sealed names & paths): `fauna.filesync.snapshot.check`
    /// resolves hash-first. An **empty** plaintext `folder` alongside
    /// `name_hash` proves the hash did the work (the post-flip call shape).
    #[tokio::test]
    async fn check_addresses_by_name_hash_with_empty_plaintext_name() {
        let state = fixture_state();
        let owner = [0x33u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        seed_snapshots(&state, &owner, 1).await;
        let hash = fauna_core::path_crypto::set_name_hash("test-set");

        let req = SnapshotCheckRequest {
            folder: String::new(),
            verify_content: false,
            name_hash: Some(ByteBuf::from(hash.to_vec())),
            ..Default::default()
        };
        // No backup_service configured on the test fixture, so a resolved set
        // still fails `backup_unavailable` — but that error ONLY fires after
        // the folder resolved, so reaching it (not `not_found`) proves the
        // hash addressed the row.
        let err = check_handler()(Arc::clone(&state), owner, encode_req(&req))
            .await
            .expect_err("no backup service configured");
        assert_eq!(err.code, "fauna.filesync.snapshot.backup_unavailable");

        // A malformed (non-32-byte) hash is refused, never silently ignored.
        let bad_req = SnapshotCheckRequest {
            folder: String::new(),
            verify_content: false,
            name_hash: Some(ByteBuf::from(vec![1u8, 2, 3])),
            ..Default::default()
        };
        let err = check_handler()(state, owner, encode_req(&bad_req))
            .await
            .expect_err("a 3-byte hash is not a BLAKE3 digest");
        assert_eq!(err.code, "fauna.filesync.snapshot.invalid_request");
    }

    #[tokio::test]
    async fn list_folder_rejects_non_owner() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        let attacker = [0x22u8; 32];
        state
            .db
            .create_user(&attacker, "free", "test")
            .await
            .unwrap();
        seed_snapshots(&state, &owner, 1).await;
        let req = SnapshotListRequest {
            message_kind: None,
            folder: Some("test-set".into()),
            limit: 0,
            ..Default::default()
        };
        let err = list_handler()(state, attacker, encode_req(&req))
            .await
            .expect_err("cross-user folder list must be denied");
        assert_eq!(err.code, "fauna.filesync.snapshot.not_found");
    }

    #[tokio::test]
    async fn get_conv_allows_member_rejects_non_member() {
        let state = fixture_state();
        let channel = [0xc0u8; 32];
        let member = [0x11u8; 32];
        let stranger = [0x99u8; 32];
        state.db.create_user(&member, "free", "test").await.unwrap();
        state
            .db
            .create_user(&stranger, "free", "test")
            .await
            .unwrap();
        let snap_id = seed_conv_snapshot(&state, &channel, 1).await;
        state
            .db
            .register_actor_channel(&member, &channel)
            .await
            .expect("register channel member");

        // A roster member of the channel can read the conv snapshot.
        let req = SnapshotGetRequest {
            snapshot_id: snap_id,
            extra: Default::default(),
        };
        let bytes = get_handler()(state.clone(), member, encode_req(&req))
            .await
            .expect("channel member get succeeds");
        let reply: SnapshotGetReply = decode_reply(&bytes);
        assert_eq!(reply.id, snap_id);

        // A non-member (non-admin) is denied — owner-equality is meaningless
        // for a conv set (scope key is the channel_id), so membership gates.
        let err = get_handler()(state, stranger, encode_req(&req))
            .await
            .expect_err("non-member conv get must be denied");
        assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");
    }

    /// S2-P3: a **group-bound** shared *file* set (`message_kind = None`,
    /// `mls_group_id` set via `fauna.folders.share`) admits a roster member of
    /// the derived channel on a READ (`snapshot.get`) alongside the owner; a
    /// non-member outsider is denied. This is the security-critical admission.
    #[tokio::test]
    async fn get_group_bound_folder_allows_member_rejects_outsider() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        let member = [0xb2u8; 32];
        let outsider = [0x99u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        state.db.create_user(&member, "free", "test").await.unwrap();
        state
            .db
            .create_user(&outsider, "free", "test")
            .await
            .unwrap();
        let ids = seed_snapshots(&state, &owner, 1).await; // owner owns "test-set"

        // Bind "test-set" to a client-created MLS group + roster the member —
        // the post-`folders.share` + `welcome.deliver` state. A 20-byte raw
        // group id (not fixed-32) also exercises the variable-length path.
        let group_id = vec![0x7cu8; 20];
        state
            .db
            .set_folder_mls_group("test-set", &owner, Some(&group_id))
            .await
            .expect("bind group");
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&group_id).0;
        state
            .db
            .register_actor_channel(&member, &channel_id)
            .await
            .expect("roster member");

        let req = SnapshotGetRequest {
            snapshot_id: ids[0],
            extra: Default::default(),
        };
        // Owner still reads their own (now-shared) set.
        get_handler()(state.clone(), owner, encode_req(&req))
            .await
            .expect("owner get succeeds");
        // The roster member reads it (the S2-P3 admission via is_actor_in_channel).
        let bytes = get_handler()(state.clone(), member, encode_req(&req))
            .await
            .expect("group member get succeeds");
        let reply: SnapshotGetReply = decode_reply(&bytes);
        assert_eq!(reply.id, ids[0]);
        // A non-member outsider is denied.
        let err = get_handler()(state, outsider, encode_req(&req))
            .await
            .expect_err("non-member get must be denied");
        assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");
    }

    /// S2-P3: members of a group-bound set are **read-only** — `snapshot.delete`
    /// (a write) stays owner-only. A roster member who may *read* a snapshot is
    /// still denied *deleting* it; only the owner can.
    #[tokio::test]
    async fn delete_group_bound_folder_is_owner_only() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        let member = [0xb2u8; 32];
        state.db.create_user(&owner, "free", "test").await.unwrap();
        state.db.create_user(&member, "free", "test").await.unwrap();
        let ids = seed_snapshots(&state, &owner, 4).await; // > hard floor of 3
        let group_id = vec![0x33u8; 32];
        state
            .db
            .set_folder_mls_group("test-set", &owner, Some(&group_id))
            .await
            .expect("bind group");
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&group_id).0;
        state
            .db
            .register_actor_channel(&member, &channel_id)
            .await
            .expect("roster member");

        // The member CAN read the snapshot...
        let get_req = SnapshotGetRequest {
            snapshot_id: ids[0],
            extra: Default::default(),
        };
        get_handler()(state.clone(), member, encode_req(&get_req))
            .await
            .expect("member read ok");
        // ...but CANNOT delete it (write op → owner-only for a shared set).
        let del_req = SnapshotDeleteRequest {
            snapshot_id: ids[0],
            extra: Default::default(),
        };
        let err = delete_handler()(state.clone(), member, encode_req(&del_req))
            .await
            .expect_err("member delete must be denied");
        assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");
        // The owner can delete (write authorized; owner-equality holds).
        delete_handler()(state, owner, encode_req(&del_req))
            .await
            .expect("owner delete succeeds");
    }

    /// S2-P3: `snapshot.list` (a read) resolves a group-bound shared set by name
    /// for a roster member (the change-log/discovery surface, spec § Q5).
    #[tokio::test]
    async fn list_group_bound_folder_allows_member() {
        let state = fixture_state();
        let owner = [0x11u8; 32];
        let member = [0xb2u8; 32];
        state.db.create_user(&member, "free", "test").await.unwrap();
        seed_snapshots(&state, &owner, 2).await;
        let group_id = vec![0x44u8; 32];
        state
            .db
            .set_folder_mls_group("test-set", &owner, Some(&group_id))
            .await
            .expect("bind group");
        let channel_id = fauna_mls::types::ChannelId::from_group_id(&group_id).0;
        state
            .db
            .register_actor_channel(&member, &channel_id)
            .await
            .expect("roster member");

        let req = SnapshotListRequest {
            message_kind: None,
            folder: Some("test-set".into()),
            limit: 0,
            ..Default::default()
        };
        let bytes = list_handler()(state, member, encode_req(&req))
            .await
            .expect("member list succeeds");
        let reply: SnapshotListReply = decode_reply(&bytes);
        assert_eq!(
            reply.rows.len(),
            2,
            "the member sees the shared set's snapshots"
        );
    }

    /// Structural guard: every registered `fauna.filesync.snapshot.*` kind must
    /// be classified as owner-implicit (self-scoped, no caller-supplied
    /// snapshot_id/folder) or owner-checked (carries an owner/membership
    /// gate). A new kind that is neither fails here — the cheap defense that
    /// would have caught the B15 transport-flip gap (review N1).
    #[test]
    fn every_filesync_snapshot_kind_is_owner_scoped() {
        use std::collections::BTreeSet;

        let mut b = crate::rpc_router::RpcRouter::builder();
        register_filesync_handlers(&mut b);
        let router = b.build();

        // Self-scoped to the connection actor; no caller-supplied id/name.
        let owner_implicit: BTreeSet<&str> = [
            "fauna.filesync.snapshot.create_message_kind",
            "fauna.filesync.snapshot.list_restore_history",
        ]
        .into_iter()
        .collect();

        // Take a caller-supplied snapshot_id / folder and owner-scope it
        // (authorize_snapshot / get_folder_for_actor / the already-correct
        // resolve_snapshot_owner handlers).
        let owner_checked: BTreeSet<&str> = [
            "fauna.filesync.snapshot.delete_immediate",
            "fauna.filesync.snapshot.restore_message_kind",
            "fauna.filesync.snapshot.list_restore_divergence",
            "fauna.filesync.snapshot.list",
            "fauna.filesync.snapshot.create_folder",
            "fauna.filesync.snapshot.get",
            "fauna.filesync.snapshot.delete",
            "fauna.filesync.snapshot.undelete",
            "fauna.filesync.snapshot.prune",
            // Carries no policy on the wire, but it still takes a caller-supplied
            // `folder` / `name_hash` and resolves it through the SAME owner gate
            // `prune` uses (`get_folder_for_actor{,_by_name_hash}`) — a prune is
            // a write, so a roster member reading a group-bound set may not launch
            // one. Owner-CHECKED, not owner-implicit.
            "fauna.filesync.snapshot.prune_set_policy",
            "fauna.filesync.snapshot.check",
            "fauna.filesync.snapshot.diff",
            // S8 D3. Owner-CHECKED, and deliberately gated at
            // `SnapshotAccess::Read` rather than `Write`: a roster member is
            // part of the seal's audience and must be able to re-stamp, or the
            // window re-seal would strand a set whose owner's client
            // never runs the pass. The caller-supplied `snapshot_id` still goes
            // through `authorize_snapshot`, and `is_label_audience()` narrows it
            // further, so the read-level access is a widening of *who may
            // stamp*, never a hole in *which snapshot* they may reach.
            "fauna.filesync.snapshot.stamp_labels",
        ]
        .into_iter()
        .collect();

        let registered: Vec<&str> = router
            .iter_kinds()
            .filter(|k| k.starts_with("fauna.filesync.snapshot."))
            .collect();
        assert!(
            !registered.is_empty(),
            "no fauna.filesync.snapshot.* kinds registered — test wiring broke"
        );

        for kind in registered {
            assert!(
                owner_implicit.contains(kind) || owner_checked.contains(kind),
                "new filesync snapshot kind `{kind}` is UNCLASSIFIED. It must be either \
                 owner-implicit (self-scoped, no snapshot_id/folder param) or owner-checked \
                 (authorize_snapshot / get_folder_for_actor). Add it to the correct set here \
                 AND give it an owner gate (review N1 — the F1 IDOR)."
            );
        }
    }
}
