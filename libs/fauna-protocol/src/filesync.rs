//! `fauna.filesync.*` WS-RPC kinds — file-sync feature surfaces that
//! moved from HTTP per `docs/goal/architecture/transport.md` § Goal
//! ("new feature surfaces extend WS-RPC rather than growing HTTP") +
//! `docs/goal/architecture/api-layers.md` § WS-RPC migration status.
//!
//! Hosts the message-kind snapshot kinds (mail / calendar snapshot
//! create, owner-only immediate delete, owner-only restore, owner-
//! implicit list + restore-history/divergence) AND the **folder
//! snapshot control surface** (Track B15):
//! `create_folder` / `get` / `delete` (queued 48 h) / `undelete` /
//! `prune` / `check` / `diff`, plus the `list` fold-in (a `folder`
//! filter folds folder snapshots into the existing owner-implicit
//! message-kind list — `SnapshotSummaryRow` already admits them via
//! `message_kind == None`). Only the snapshot **byte downloads** stay
//! HTTP residue (the ZIP-archive restore + single-file download —
//! `transport.md` § HTTP residue, large-body streaming downloads).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use fauna_core::identity::ActorId;

use crate::ByteBuf;
use fauna_cbor::Value;

// ── fauna.filesync.snapshot.create_message_kind ─────────────────

/// `fauna.filesync.snapshot.create_message_kind` — create a message-
/// kind snapshot (mail or calendar) for the bearer's actor.
///
/// Owner-implicit: the WS handshake actor is the snapshot owner; no
/// explicit `actor_id` is carried on the wire. Atomicity: the nest
/// serialises both the kind manifest and the matching placement
/// manifest in one SQLite transaction so the snapshot is never written
/// with a stale content/placement pairing.
///
/// Pure-backup destinations refused with
/// `fauna.filesync.snapshot.pure_backup_destination`; unknown kind
/// returned as `fauna.filesync.snapshot.unknown_kind`; calendar kind
/// returns `fauna.filesync.snapshot.calendar_not_implemented` until the
/// calendar segment store rollout lands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotCreateMessageKindRequest {
    pub kind: String,
    /// The snapshot scope. `None` = bearer-scoped (mail/cal: the bearer's
    /// actor; conv: snapshot ALL the bearer's channels, batched). `Some` =
    /// that scope (conv: a single `channel_id`; mail/cal ignore it — always
    /// bearer-scoped). Uniform with `CompactRequest.actor_id`.
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    #[serde(default)]
    pub actor_id: Option<ActorId>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotCreateMessageKindReply {
    pub snapshot_id: i64,
    /// All snapshot row ids created (len ≥ 1; `snapshot_id` mirrors
    /// `snapshot_ids[0]` for single-snapshot callers). Conv's batched
    /// "snapshot all my channels" returns one id per channel here.
    #[serde(default)]
    pub snapshot_ids: Vec<i64>,
    pub kind: String,
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub actor_id: ActorId,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.filesync.snapshot.delete_immediate ────────────────────

/// `fauna.filesync.snapshot.delete_immediate` — owner-only immediate
/// snapshot delete (spec D11). Skips the 48 h `SnapshotDelete` pending
/// action and the 30 d soft-delete window; still enforces the hard
/// floor of 3 active snapshots per folder.
///
/// Both `confirm_id` (the snapshot id retyped as a string) and
/// `acknowledge` (exactly [`IMMEDIATE_DELETE_ACK_TEXT`]) are required —
/// any mismatch returns `confirm_mismatch` / `acknowledge_mismatch`
/// with no state mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDeleteImmediateRequest {
    pub snapshot_id: i64,
    pub confirm_id: String,
    pub acknowledge: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDeleteImmediateReply {
    pub snapshot_id: i64,
    /// Compaction's outputs (the segments that were freed) still
    /// observe their 14 d tombstoned-segment retention — the override
    /// governs the snapshot lifecycle, not the segment lifecycle.
    pub segment_retention_days: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The exact acknowledge string required for immediate delete. Compared
/// byte-for-byte — no trim, no case-folding.
pub const IMMEDIATE_DELETE_ACK_TEXT: &str = "I understand this is immediate and irreversible.";

/// The **hard floor** of active snapshots a set always keeps
/// (`docs/goal/behavior/backup-restore.md` § 7 Layer 1: *"The hard floor (3
/// active snapshots) is always enforced"*). A snapshot may be deleted only
/// while the set holds **more** than this many active ones (neither
/// soft-deleted nor deletion-pending), and no prune, override or remediation
/// breaches it.
///
/// Here, beside [`IMMEDIATE_DELETE_ACK_TEXT`], for that constant's reason:
/// both ends need the one number. The nest enforces it, and an app must be
/// able to say *"this snapshot can't be deleted yet"* rather than offer a
/// delete the nest will refuse — which it cannot do from a copy that may drift.
/// It already had: the interactive delete path hard-coded `count > 3` beside
/// the pruner's own constant, two spellings of one rule.
pub const SNAPSHOT_HARD_FLOOR: usize = 3;

/// Whether deleting one **active** snapshot of a set is allowed, given how
/// many active snapshots it holds — the [`SNAPSHOT_HARD_FLOOR`] rule stated
/// once, for the nest's refusal and an app's affordance alike.
pub fn snapshot_delete_allowed(active_snapshots: usize) -> bool {
    active_snapshots > SNAPSHOT_HARD_FLOOR
}

// ── fauna.filesync.snapshot.restore_message_kind ────────────────

/// `fauna.filesync.snapshot.restore_message_kind` — owner-only message-
/// kind restore. Dispatches on the snapshot row's `message_kind`
/// (`"mail"` or `"calendar"`); single SQLite transaction replays the
/// pinned placement manifest + rebuilds `segment_records` from the
/// reconstituted segment file footers.
///
/// Pre-conditions (refusal does not mutate state):
/// 1. `confirm_id` matches snapshot id → otherwise `confirm_mismatch`.
/// 2. Bridge not serving the actor → otherwise `bridge_active`.
/// 3. Wrapped-MLS-blob presence (`has_wrapped_mls_blobs`) is advisory; reported as
///    `config_present` in the reply.
///
/// The pinned `placement_manifest` is replayed into `bridge_imap_*`
/// (mail) / `bridge_caldav_*` (calendar, metadata-only in v1) inside the
/// restore transaction; the matching content manifest rebuilds the
/// `segment_records` mirror from the reconstituted segment files. Calendar
/// restore is metadata-only until the calendar content segment store
/// ships.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRestoreMessageKindRequest {
    pub snapshot_id: i64,
    pub confirm_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRestoreMessageKindReply {
    pub snapshot_id: i64,
    pub kind: String,
    /// Advisory: false if the actor's wrapped-MLS-blob bundle
    /// (`has_wrapped_mls_blobs`) is absent; restore still proceeded but the
    /// bridge can't AUTH after restart until the bundle is restored too. The
    /// field name is wire-frozen from when the check was named for `__config`.
    pub config_present: bool,
    /// Human-readable warning attached when `config_present == false`;
    /// empty string otherwise.
    pub note: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.filesync.snapshot.list_restore_history ────────────────

/// One `restore_history` row, owner-scoped. Surfaced on the Backups page
/// § Restore history (`docs/goal/ui/backups.md`): `restore-history-item`
/// renders `completed_at` + `kinds_restored` + the source (the
/// destination that supplied the chunks, or "local snapshot" when
/// `source_member_id` is absent). `snapshot_id` keys the per-row
/// divergence banner (the UI calls `list_restore_divergence` with it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreHistoryRow {
    pub id: i64,
    pub completed_at: i64,
    pub snapshot_id: i64,
    /// `"mail"` / `"calendar"` / (future) `"mail+calendar"`.
    pub kinds_restored: String,
    /// Backup-destination provenance; `None` renders as "local snapshot"
    /// (Plan 4 populates it — `None` everywhere until then).
    #[serde(default, with = "serde_bytes")]
    pub source_member_id: Option<Vec<u8>>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.filesync.snapshot.list_restore_history` — owner-implicit list
/// of the bearer actor's restore history, newest first. No `snapshot_id`
/// on the wire; the bearer's WS-handshake actor scopes the query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRestoreHistoryListRequest {
    /// Cap on returned rows (0 → server default). Newest-first.
    #[serde(default)]
    pub limit: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRestoreHistoryListReply {
    pub rows: Vec<RestoreHistoryRow>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.filesync.snapshot.list_restore_divergence ─────────────

/// One `bridge_restore_divergence` row. Surfaced on the Backups page
/// § Restore divergence: `restore-divergence-details-item` renders
/// `collection` + `mua_id` (`(unknown)` when `None`) + `client_modseq` +
/// `server_modseq` + `lost_event_count` ("~N writes lost"). Forensic —
/// server state won; the row records what the MUA lost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreDivergenceRow {
    pub id: i64,
    pub snapshot_id: i64,
    pub observed_at: i64,
    /// `"imap"` or `"caldav"`.
    pub protocol: String,
    /// Mailbox name (IMAP) or calendar id hex (CalDAV).
    pub collection: String,
    /// MUA identity (advisory): IMAP RFC 2971 ID or CalDAV `User-Agent`;
    /// `None` when the protocol offered nothing.
    pub mua_id: Option<String>,
    pub client_modseq: i64,
    pub server_modseq: i64,
    /// `max(client_modseq - server_modseq, 0)` — coarse, see spec § D10.
    pub lost_event_count: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.filesync.snapshot.list_restore_divergence` — owner-only list of
/// the divergence rows recorded against one snapshot's restore. The
/// caller must own the snapshot (same `resolve_snapshot_owner` check as
/// `restore_message_kind`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRestoreDivergenceListRequest {
    pub snapshot_id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRestoreDivergenceListReply {
    pub rows: Vec<RestoreDivergenceRow>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.filesync.snapshot.list ────────────────────────────────

/// One snapshot summary row. The forward-compatible / unified snapshot
/// list shape: today only message-kind snapshots (`message_kind` is
/// `Some("mail")` / `Some("calendar")`) are listed, but the shape also
/// fits folder snapshots (`message_kind == None`) for when the
/// folder HTTP list migrates off `GET /api/v1/snapshots`
/// (`docs/goal/architecture/api-layers.md` § Snapshots — the JSON list
/// is WS-RPC-migratable; only the byte downloads are HTTP residue).
///
/// Surfaced on the Backups page § Restore from backup destination as the
/// `restore-snapshot-select` options (local snapshot path).
///
/// Derives `Default` for the growing-wire-type fixture convention — construct
/// test literals with `..Default::default()` so additive fields never break
/// them (and two branches growing this struct merge cleanly).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SnapshotSummaryRow {
    pub id: i64,
    pub created_at: i64,
    /// `Some("mail")` / `Some("calendar")` for message-kind snapshots;
    /// `None` for folder snapshots (not listed today — reserved for the
    /// folder HTTP-list migration).
    pub message_kind: Option<String>,
    pub file_count: i64,
    pub total_bytes: i64,
    /// Raw 32-byte capturing-device id (mirrors [`SnapshotGetReply`]); `None`
    /// = unattributed. Drives the per-device snapshot filter on the Backups
    /// page. `#[serde(default)]` so an absent id round-trips as unattributed.
    #[serde(default)]
    pub device_id: Option<ByteBuf>,
    /// The snapshot's plaintext `tags` (display copy), rideable while the
    /// dual write rests them — the S8 D3 stamp pass reads these to seal from.
    /// Ships **ungated** because every reader of this listing is the label
    /// audience by construction: the folder mode resolves through
    /// `resolve_readable_folder`, which has **no admin arm**, and the
    /// message-kind mode is bearer-self-scoped (owner). `None` = no tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// `snapshots.tags_sealed` verbatim — the sealed display copy beside its
    /// plaintext, so the S8 D3 stamp pass can see "tags present but unsealed
    /// (or sealed under the wrong root axis)" from the listing alone instead
    /// of an N+1 `snapshot.get` walk. Same audience argument as
    /// [`Self::tags`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags_sealed: Option<ByteBuf>,
    /// The row rests inside its 30-day `purge_after` recovery window — the
    /// snapshot was soft-deleted (by a prune, or by a `delete`'s pending
    /// action expiring) and `fauna.filesync.snapshot.undelete` still serves
    /// it. **Folder mode only**; the owner-implicit message-kind mode
    /// excludes soft-deleted rows entirely, so this stays `false` there.
    ///
    /// `#[serde(default)]` per `backup-restore.md` § 2 *Row lifecycle
    /// fields*: an absent key reads as "active".
    #[serde(default)]
    pub soft_deleted: bool,
    /// When a [`Self::soft_deleted`] row becomes eligible for GC's phase-3
    /// purge — the "recoverable until" the row renders. `None` on an active
    /// row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge_after: Option<i64>,
    /// The row rests inside a **cancellable** deletion window: a `delete`'s
    /// 48 h pending action, or an automatic prune's 7-day
    /// `SnapshotBulkPrune`. Not yet deleted — the user can still cancel it
    /// from the pending-actions surface. Folder mode only, as above.
    #[serde(default)]
    pub deletion_pending: bool,
    /// When a [`Self::deletion_pending`] row's pending action fires (and the
    /// snapshot becomes soft-deleted, opening [`Self::purge_after`]) — the
    /// deadline the cancel window runs to. Read from the pending action, not
    /// from the snapshot row. `None` when nothing is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execute_after: Option<i64>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.filesync.snapshot.list` — list snapshots, newest first. Two
/// modes, discriminated by `folder`:
///
/// - **`folder: None`** (default) — owner-implicit message-kind list:
///   the bearer's WS-handshake actor scopes the query (no `actor_id` on
///   the wire); only rows with `message_kind IS NOT NULL` are returned;
///   soft-deleted and pending-delete rows are excluded.
/// - **`folder: Some(name)`** — folder-scoped list (Track B15 fold-in
///   of the legacy `GET /api/v1/snapshots?folder=`): every snapshot of
///   the named folder, behavior-preserving (incl. soft-deleted rows, as
///   the HTTP twin returned). Folder snapshots carry `message_kind ==
///   None` in [`SnapshotSummaryRow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SnapshotListRequest {
    /// Filter to one message kind (`"mail"` / `"calendar"`); `None` lists
    /// every message-kind snapshot the bearer owns. Ignored in folder
    /// mode (`folder` set).
    #[serde(default)]
    pub message_kind: Option<String>,
    /// Folder name — switches to the folder-scoped list (B15). `None`
    /// = owner-implicit message-kind list (the original behavior).
    #[serde(default)]
    pub folder: Option<String>,
    /// Cap on returned rows (0 → server default). Newest-first.
    #[serde(default)]
    pub limit: u32,
    /// Hash-first addressing (S5b) — see
    /// `SnapshotCreateFolderRequest::name_hash`. Ignored in message-kind mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotListReply {
    pub rows: Vec<SnapshotSummaryRow>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.filesync.snapshot.create_folder ─────────────────────

/// `fauna.filesync.snapshot.create_folder` — capture a point-in-time
/// snapshot of a synced folder's current state. The folder
/// counterpart of [`SnapshotCreateMessageKindRequest`]; file_set-name
/// scoped, `User | Admin` (the HTTP twin was plain `BearerAuth`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SnapshotCreateFolderRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Raw 32-byte device id (the HTTP twin took it hex-encoded). Wrong
    /// length → `invalid_request`; `None` = unattributed.
    #[serde(default)]
    pub device_id: Option<ByteBuf>,
    /// Hash-first addressing (S5b, `file-sync.md` § Sealed names & paths):
    /// when present, resolves via `name_hash` before falling back to
    /// [`Self::folder`] — see `fauna_protocol::folders::FolderUpdateRequest::name_hash`
    /// for the full rationale. A malformed (non-32-byte) hash is refused,
    /// never silently ignored. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    /// [`Self::tags`], sealed by the creating client — the display copy of
    /// `snapshots.tags` (path-sealing S6-d, `docs/goal/behavior/file-sync.md`
    /// § Sealed names & paths). Opaque to the nest, which stores it and never
    /// opens it.
    ///
    /// **This request is the only writer there will ever be.** `snapshots.tags`
    /// has exactly one production writer (`create_snapshot_v2`, from this
    /// field's plaintext sibling), the nest holds no key, and a snapshot row —
    /// unlike a folder row — has no later gesture that revisits it, so there
    /// is no catch-up stamp to fall back on. A create that does not seal
    /// carries no sealed tags, and none rest.
    ///
    /// Minted through `fauna_core::label_custody::seal_snapshot_tags`: salt =
    /// the set's `name_hash`, random nonce, sealed under the set's **label
    /// audience** root (a bound set's M2 generation, or the owner root when
    /// unbound) — the same root the set's own paths seal under, so exactly the
    /// readers who can render its file list can render a snapshot's tags.
    /// `None` from a keyless writer: the snapshot carries no sealed tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags_sealed: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotCreateFolderReply {
    pub id: i64,
    pub file_count: i64,
    pub total_bytes: i64,
    pub created_at: i64,
    pub parent_id: Option<i64>,
    pub tags: Vec<String>,
    pub device_id: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.filesync.snapshot.get ─────────────────────────────────

/// `fauna.filesync.snapshot.get` — snapshot metadata + file listing.
/// Works for folder and message-kind rows alike (the message-kind
/// branch resolves `actor_id`); `User | Admin`, no per-resource owner
/// check (behavior-preserving — the HTTP twin had none).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotGetRequest {
    pub snapshot_id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One file recorded in a snapshot (`SnapshotFileRow`). `manifest_hash`
/// rides as raw bytes (the HTTP twin hex-encoded it); the actual content
/// is fetched via the `manifest_hash` → blob routes (HTTP residue).
///
/// `Default` so fixtures can be written `..Default::default()`: this type keeps
/// growing additive fields (`path_hash`, `path_sealed`), and hand-listing every
/// field is what makes two branches independently growing this struct collide on
/// the grown axis instead of merging cleanly.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SnapshotFileEntry {
    pub path: String,
    pub manifest_hash: ByteBuf,
    pub size_bytes: i64,
    pub mtime: i64,
    pub mode: i64,
    pub file_type: String,
    pub symlink_target: Option<String>,
    /// BLAKE3 of the normalized path (`fauna_core::sync::path_hash`) — the
    /// equality-only addressing key the snapshot surfaces re-keyed onto in
    /// path-sealing S1. Withheld (`None`) from a reader outside the set's label
    /// audience, together with `path_sealed` — the pair ships or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hash: Option<ByteBuf>,
    /// The snapshot row's `path`, sealed — see
    /// [`crate::sync::SyncChange::path_sealed`]. Copied verbatim and keylessly
    /// from the membership projection at snapshot creation, so the envelope's
    /// own `gen` names the key that opens it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// `Default` so fixtures can use `..Default::default()` — two branches each
// growing this reply then merge cleanly instead of colliding on the new field.
// The `folder` add below is exactly that case.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SnapshotGetReply {
    pub id: i64,
    pub folder: String,
    pub created_at: i64,
    pub file_count: i64,
    pub total_bytes: i64,
    pub parent_id: Option<i64>,
    pub tags: Vec<String>,
    pub device_id: Option<ByteBuf>,
    /// `Some("mail")` / `Some("calendar")` for message-kind snapshots.
    pub message_kind: Option<String>,
    /// Resolved owner for message-kind snapshots (`None` for folder).
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub actor_id: Option<ActorId>,
    pub files: Vec<SnapshotFileEntry>,
    /// [`Self::tags`], sealed — the display copy of `snapshots.tags`
    /// (path-sealing S6-d). Copied verbatim from the row the creating client
    /// sealed; this nest holds no key that opens it. Rendered client-side by
    /// `fauna_core::label_custody::render_snapshot_tags`, salted by
    /// [`Self::folder_hash`].
    ///
    /// **Projected to the seal's audience only**, together with the pair below
    /// and with every entry's `path_hash`/`path_sealed`: a reader who cannot
    /// open the seal has no functional use for it, and its salt is an unkeyed
    /// digest of a dictionary-shaped name (`encryption-at-rest.md` § Carve-outs
    /// — the gate is `folder_authz::FolderReadGrant::is_label_audience()`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags_sealed: Option<ByteBuf>,
    /// [`Self::folder`], sealed — see
    /// [`crate::folders::FolderSummary::name_sealed`]. The
    /// [`SnapshotDiffReply::folder_sealed`] twin, added by S6-d for the same
    /// reason `diff` has one: without it this reply's set name goes blank and
    /// unrecoverable at the flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_sealed: Option<ByteBuf>,
    /// The convergent salt [`Self::folder_sealed`] opens under
    /// (`fauna_core::path_crypto::set_name_hash`) — ships as a pair with the
    /// seal or not at all.
    ///
    /// It is **also [`Self::tags_sealed`]'s salt**, which is why S6-d could add
    /// the tag seal to this reply without minting a fifth digest shape: the set
    /// name's digest already addresses this set on 23 request types (S5b) and
    /// already rests in `folders.name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.filesync.snapshot.stamp_labels ────────────────────────

/// `fauna.filesync.snapshot.stamp_labels` — stamp (or re-stamp) a
/// snapshot's `tags_sealed` display copy in place. The **one** new wire
/// kind the S8 backfill licenses (D3): snapshots are immutable, so unlike
/// every other sealed plane there is no later mutation for a missing tag
/// seal to ride — this stamp is the plane's only catch-up path.
///
/// Contract, load-bearing on every clause:
/// - **Stamp-only** — carries the seal and never plaintext; the nest writes
///   `tags_sealed` and touches nothing else. It cannot add, remove, or
///   change tags: the nest refuses (`invalid_request`) when the row has no
///   tags to seal (keyed on the nest-computed `tag_hashes`, which survives
///   the plaintext scrub), so a sealed blob can never conjure tags onto a
///   tag-less snapshot.
/// - **Overwrite allowed, but only along ONE axis**: the nest accepts a stamp iff the column is empty,
///   or the **resting** envelope does not name a key generation (an
///   owner-root `gen: None` seal, or bytes that parse for nobody) — the residue on a *bound* set's snapshot, which is the only
///   reason this kind overwrites at all. A seal that already names a
///   generation opens for its whole audience and is frozen;
///   `invalid_request`. Every stamp an honest `backfill_tag_seals` sends is
///   still accepted; what it removes is a label-audience member's ability
///   to replace the owner's snapshot labels arbitrarily and repeatedly —
///   which after the S9 plaintext scrub would leave the planted rendering
///   as the only one. The write is a compare-and-swap on the bytes the
///   predicate decided against, so a concurrent stamp is refused, not
///   clobbered. The nest still holds no key: it reads the resting
///   envelope's header, never any ciphertext, and never inspects the
///   incoming bytes at all — format-validating those would make an older
///   nest refuse a newer client's envelope revision.
/// - **Label-audience only** — authorized through `authorize_snapshot`'s
///   `FolderReadGrant` (S5e) with `is_label_audience()` required: a
///   Q5-admin reader can neither read nor plant a seal here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SnapshotStampLabelsRequest {
    pub snapshot_id: i64,
    /// The canonical `fauna_core::path_crypto::SealedLabel` bytes over the
    /// snapshot's tag list (`seal_snapshot_tags` — label-audience root, salt
    /// = the set's `name_hash`, random nonce). Opaque to the nest.
    pub tags_sealed: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SnapshotStampLabelsReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.filesync.snapshot.delete ──────────────────────────────

/// `fauna.filesync.snapshot.delete` — queue a snapshot for deletion via
/// a 48 h pending action (the soft-delete path; folder or message-kind
/// row both supported). Distinct from the owner-only
/// `delete_immediate` override. Refused with `hard_floor_breach` if it
/// would breach the floor of 3 active snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDeleteRequest {
    pub snapshot_id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDeleteReply {
    pub pending_action_id: i64,
    pub execute_after: i64,
    /// Always `"pending"` on success (mirrors the HTTP 202 body).
    pub status: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.filesync.snapshot.undelete ────────────────────────────

/// `fauna.filesync.snapshot.undelete` — cancel a pending deletion /
/// recover a soft-deleted snapshot before GC. Refused with
/// `not_soft_deleted` if the snapshot is not soft-deleted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotUndeleteRequest {
    pub snapshot_id: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotUndeleteReply {
    pub undeleted: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.filesync.snapshot.prune ───────────────────────────────

/// Retention policy (restic-style buckets). Mirrors the nest
/// `backup::retention::RetentionPolicy`; the handler maps it field-for-
/// field. All counts are `Option<u32>` (absent = bucket disabled).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SnapshotRetentionPolicy {
    #[serde(default)]
    pub keep_last: Option<u32>,
    #[serde(default)]
    pub keep_hourly: Option<u32>,
    #[serde(default)]
    pub keep_daily: Option<u32>,
    #[serde(default)]
    pub keep_weekly: Option<u32>,
    #[serde(default)]
    pub keep_monthly: Option<u32>,
    #[serde(default)]
    pub keep_yearly: Option<u32>,
    /// Never prune a snapshot carrying ANY tag.
    #[serde(default)]
    pub keep_tags: Vec<String>,
    /// Keep all snapshots newer than this many seconds.
    #[serde(default)]
    pub keep_within_secs: Option<i64>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.filesync.snapshot.prune` — apply a retention policy to a file
/// set. `dry_run` reports candidates without deleting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SnapshotPruneRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub policy: SnapshotRetentionPolicy,
    /// Hash-first addressing (S5b) — see
    /// `SnapshotCreateFolderRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One prune candidate (dry-run only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrunableSnapshot {
    pub id: i64,
    pub created_at: i64,
    pub tags: Vec<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.filesync.snapshot.prune` reply (unified dry-run / actual).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotPruneReply {
    /// Echoes the request mode.
    pub dry_run: bool,
    /// Dry-run: the count that *would* be pruned. Actual: the count pruned.
    pub pruned: i64,
    /// Dry-run: the count that *would* be kept. Actual: the count remaining.
    pub remaining: i64,
    /// Dry-run: the prune candidates. Actual: empty.
    #[serde(default)]
    pub snapshots: Vec<PrunableSnapshot>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.filesync.snapshot.prune_set_policy ────────────────────

/// What the nest could make of the folder's resting `retention_policy`
/// column, reported back so a client can say *why* nothing was pruned.
///
/// Deliberately three arms rather than an `Option`, mirroring the nest's own
/// `backup::retention::FolderRetention`: `not_set` is the common healthy
/// state ("no retention policy configured for this set"), while `unparseable`
/// is a writer disagreeing with the canonical 2-field shape and **surfaces
/// loudly** — a live at-rest shape today (`backup-restore.md` § 8 RULING
/// consequence (ii)). Both prune nothing.
pub mod policy_state {
    /// The set's policy was read and evaluated.
    pub const APPLIED: &str = "applied";
    /// No policy column, or one binding nothing. Prunes nothing.
    pub const NOT_SET: &str = "not_set";
    /// The column could not be read as the canonical shape. Prunes nothing.
    pub const UNPARSEABLE: &str = "unparseable";
}

/// `fauna.filesync.snapshot.prune_set_policy` — apply **this set's own
/// resting retention policy**, preview then execute.
///
/// Distinct from [`SnapshotPruneRequest`] by design: that kind takes a
/// client-supplied `keep_*` union policy, and `backup-restore.md` § 8 forbids
/// silently re-mapping the shipped 2-field bounds onto that vocabulary. This
/// kind carries **no policy at all** — the nest reads
/// `folders.retention_policy` and evaluates it through the same armed path
/// as the automatic prune. The explicit kind stays the surface for a caller
/// that genuinely carries its own policy (third-party clients); the 7 apps' `snapshot-prune-button` speaks this one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SnapshotPruneSetPolicyRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// Preview without deleting. The page offers execute only from a
    /// preview (`ui/backups.md` § Snapshot-list shape, *Prune* ruling).
    #[serde(default)]
    pub dry_run: bool,
    /// Hash-first addressing (S5b) — see
    /// [`SnapshotCreateFolderRequest::name_hash`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.filesync.snapshot.prune_set_policy` reply — the explicit kind's
/// prune shape plus the policy verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SnapshotPruneSetPolicyReply {
    /// Echoes the request mode.
    pub dry_run: bool,
    /// Dry-run: the count that *would* be pruned. Actual: the count pruned.
    pub pruned: i64,
    /// Dry-run: the count that *would* be kept. Actual: the count remaining.
    /// Counts the **active** population only — the same population the
    /// policy binds over.
    pub remaining: i64,
    /// Dry-run: the prune candidates. Actual: empty.
    #[serde(default)]
    pub snapshots: Vec<PrunableSnapshot>,
    /// One of [`policy_state`]'s three constants. A client renders the
    /// non-`applied` arms as *why* the count is zero, never as an empty
    /// success.
    pub policy_state: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.filesync.snapshot.check ───────────────────────────────

/// `fauna.filesync.snapshot.check` — verify every chunk of every
/// snapshot in a folder still exists in the blob store (optionally
/// verifying chunk content). Refused with `backup_unavailable` when the
/// backup service is not configured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SnapshotCheckRequest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    #[serde(default)]
    pub verify_content: bool,
    /// Hash-first addressing (S5b) — see
    /// `SnapshotCreateFolderRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One structured `fauna.filesync.snapshot.check` finding — hash-only, the
/// same redaction discipline as a nest-side log line
/// (`docs/goal/behavior/file-sync.md` § Sealed names & paths, S7 the log +
/// error-string scrub) even though this reply is owner-scoped: the ratified
/// fix is uniform hash-only error text everywhere a finding can be
/// interpolated into a string, not a per-surface risk call, because a
/// plaintext path in a reply is one copy-paste away from a support ticket or
/// a future log call. The requesting owner already holds their own
/// decrypted snapshot listing and can join on `path_hash`/`manifest_hash` to
/// render a friendly message; the nest never needs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SnapshotCheckError {
    /// A stable machine-readable finding kind, e.g. `"missing_manifest"`,
    /// `"missing_chunk"`, `"chunk_hash_mismatch"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hash: Option<ByteBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<ByteBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SnapshotCheckReply {
    /// `"ok"` or `"errors"`.
    pub status: String,
    pub snapshots_checked: i64,
    pub files_checked: i64,
    pub manifests_checked: i64,
    pub chunks_checked: i64,
    pub missing_manifests: i64,
    pub missing_chunks: i64,
    pub corrupt_manifests: i64,
    /// One entry per finding; the client renders its own friendly text. (The
    /// human-readable `errors` twin, kept for older clients, left the wire
    /// with the compat-remnant sweep.)
    #[serde(default)]
    pub structured_errors: Vec<SnapshotCheckError>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl SnapshotCheckReply {
    /// The wire `status` value when every checked snapshot verified cleanly.
    pub const STATUS_OK: &'static str = "ok";

    /// `true` when the integrity check found no errors — the single typed read
    /// of the stringly-typed wire `status` (`"ok"` vs `"errors"`).
    pub fn is_ok(&self) -> bool {
        self.status == Self::STATUS_OK
    }
}

// ── fauna.filesync.snapshot.diff ────────────────────────────────

/// `fauna.filesync.snapshot.diff` — compare two snapshots (which must
/// belong to the same folder) → added / removed / modified files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDiffRequest {
    pub a: i64,
    pub b: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// An added or removed file in a snapshot diff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDiffEntry {
    pub path: String,
    pub size_bytes: i64,
    /// BLAKE3 of the normalized path — the key the diff join itself runs on
    /// since path-sealing S1 (`backup/diff.rs`). Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hash: Option<ByteBuf>,
    /// The entry's `path`, sealed — see [`crate::sync::SyncChange::path_sealed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A file present in both snapshots with a changed size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotModifiedEntry {
    pub path: String,
    pub old_size: i64,
    pub new_size: i64,
    /// BLAKE3 of the normalized path — the diff join key. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hash: Option<ByteBuf>,
    /// The entry's `path`, sealed — see [`crate::sync::SyncChange::path_sealed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SnapshotDiffSummary {
    pub added_count: i64,
    pub removed_count: i64,
    pub modified_count: i64,
    pub added_bytes: i64,
    pub removed_bytes: i64,
    pub net_bytes: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// `Default` so fixtures can use `..Default::default()` — two branches each
// growing this reply then merge cleanly instead of colliding on the new field.
// The `folder` add below is exactly that case.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SnapshotDiffReply {
    pub snapshot_a: i64,
    pub snapshot_b: i64,
    pub added: Vec<SnapshotDiffEntry>,
    pub removed: Vec<SnapshotDiffEntry>,
    pub modified: Vec<SnapshotModifiedEntry>,
    pub summary: SnapshotDiffSummary,
    /// The folder both snapshots belong to (the nest rejects a cross-set
    /// pair, so there is exactly one). **Required**: a reply without it is
    /// refused at decode — the nest names the set unconditionally, refusing
    /// the diff outright when the set row is gone (the optional form served
    /// only a nest predating the sealed-label expand, retired under the
    /// compat-remnant sweep — `docs/goal/architecture/version-compatibility.md`
    /// § Dimension 2, the fourth exception). Post-scrub this is the
    /// empty-string sentinel; the seal pair below recovers the name.
    ///
    /// Carried so the client can resolve **label custody** for the sealed-path
    /// render (`docs/goal/behavior/file-sync.md` § Sealed names & paths) the
    /// same way `SnapshotGetReply` already lets it — a diff is requested by the
    /// two snapshot ids alone, so without this the reply is the only place the
    /// set name could come from.
    pub folder: String,
    /// [`Self::folder`], sealed — see
    /// [`crate::folders::FolderSummary::name_sealed`]. Opaque to the nest;
    /// rendered client-side by `fauna_core::label_custody::render_set_name`.
    /// Path-sealing S5c-2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_sealed: Option<ByteBuf>,
    /// The convergent salt [`Self::folder_sealed`] opens under
    /// (`fauna_core::path_crypto::set_name_hash`) — ships as a pair with the
    /// seal or not at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn create_request_round_trips() {
        let req = SnapshotCreateMessageKindRequest {
            kind: "mail".into(),
            actor_id: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SnapshotCreateMessageKindRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn create_request_round_trips_with_conv_scope() {
        // conv per-channel scope carries the channel id in actor_id.
        let req = SnapshotCreateMessageKindRequest {
            kind: "conv".into(),
            actor_id: Some(ActorId([0x33u8; 32])),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SnapshotCreateMessageKindRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn create_reply_round_trips() {
        let reply = SnapshotCreateMessageKindReply {
            snapshot_id: 42,
            snapshot_ids: vec![42],
            kind: "mail".into(),
            actor_id: ActorId([0x11u8; 32]),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SnapshotCreateMessageKindReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn delete_immediate_request_round_trips() {
        let req = SnapshotDeleteImmediateRequest {
            snapshot_id: 7,
            confirm_id: "7".into(),
            acknowledge: IMMEDIATE_DELETE_ACK_TEXT.into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SnapshotDeleteImmediateRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn delete_immediate_reply_round_trips() {
        let reply = SnapshotDeleteImmediateReply {
            snapshot_id: 7,
            segment_retention_days: 14,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SnapshotDeleteImmediateReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn restore_request_round_trips() {
        let req = SnapshotRestoreMessageKindRequest {
            snapshot_id: 99,
            confirm_id: "99".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SnapshotRestoreMessageKindRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn restore_reply_round_trips() {
        let reply = SnapshotRestoreMessageKindReply {
            snapshot_id: 99,
            kind: "mail".into(),
            config_present: true,
            note: String::new(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SnapshotRestoreMessageKindReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn restore_history_list_request_round_trips() {
        let req = SnapshotRestoreHistoryListRequest {
            limit: 50,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SnapshotRestoreHistoryListRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn restore_history_list_reply_round_trips() {
        let reply = SnapshotRestoreHistoryListReply {
            rows: vec![
                RestoreHistoryRow {
                    id: 1,
                    completed_at: 1_700_000_000,
                    snapshot_id: 7,
                    kinds_restored: "mail".into(),
                    source_member_id: None,
                    extra: Default::default(),
                },
                RestoreHistoryRow {
                    id: 2,
                    completed_at: 1_700_000_100,
                    snapshot_id: 8,
                    kinds_restored: "calendar".into(),
                    source_member_id: Some(vec![0xABu8; 32]),
                    extra: Default::default(),
                },
            ],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SnapshotRestoreHistoryListReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn restore_divergence_list_request_round_trips() {
        let req = SnapshotRestoreDivergenceListRequest {
            snapshot_id: 7,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SnapshotRestoreDivergenceListRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn restore_divergence_list_reply_round_trips() {
        let reply = SnapshotRestoreDivergenceListReply {
            rows: vec![RestoreDivergenceRow {
                id: 1,
                snapshot_id: 7,
                observed_at: 1_700_000_500,
                protocol: "caldav".into(),
                collection: "aabbcc".into(),
                mua_id: Some("Apple Calendar/14.0".into()),
                client_modseq: 99,
                server_modseq: 42,
                lost_event_count: 57,
                extra: Default::default(),
            }],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SnapshotRestoreDivergenceListReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn snapshot_list_request_round_trips() {
        let req = SnapshotListRequest {
            message_kind: Some("mail".into()),
            folder: None,
            limit: 25,
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SnapshotListRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn snapshot_list_request_no_filter_round_trips() {
        let req = SnapshotListRequest {
            message_kind: None,
            folder: None,
            limit: 0,
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SnapshotListRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn snapshot_list_request_folder_mode_round_trips() {
        let req = SnapshotListRequest {
            message_kind: None,
            folder: Some("documents".into()),
            limit: 10,
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SnapshotListRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn create_folder_round_trips() {
        let req = SnapshotCreateFolderRequest {
            folder: "documents".into(),
            tags: vec!["nightly".into()],
            device_id: Some(ByteBuf::from(vec![0x44u8; 32])),
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<SnapshotCreateFolderRequest>(&bytes).unwrap(), req);

        let reply = SnapshotCreateFolderReply {
            id: 5,
            file_count: 12,
            total_bytes: 4096,
            created_at: 1_700_000_000,
            parent_id: Some(4),
            tags: vec!["nightly".into()],
            device_id: Some(ByteBuf::from(vec![0x44u8; 32])),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<SnapshotCreateFolderReply>(&bytes).unwrap(), reply);
    }

    #[test]
    fn get_round_trips() {
        let req = SnapshotGetRequest {
            snapshot_id: 7,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<SnapshotGetRequest>(&bytes).unwrap(), req);

        let reply = SnapshotGetReply {
            id: 7,
            folder: "documents".into(),
            created_at: 1_700_000_000,
            file_count: 2,
            total_bytes: 1024,
            parent_id: None,
            tags: vec![],
            device_id: None,
            message_kind: None,
            actor_id: None,
            files: vec![SnapshotFileEntry {
                path: "a/b.txt".into(),
                manifest_hash: ByteBuf::from(vec![0xAAu8; 32]),
                size_bytes: 512,
                mtime: 1_700_000_000,
                mode: 0o644,
                file_type: "file".into(),
                symlink_target: None,
                path_hash: None,
                path_sealed: None,
                extra: Default::default(),
            }],
            // Struct-update form for the rest, so a future field growing on
            // this reply does not break the literal again — as S6-d's sealing
            // trio did. The `Default` derive above the struct exists for this.
            ..Default::default()
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<SnapshotGetReply>(&bytes).unwrap(), reply);
    }

    #[test]
    fn get_reply_message_kind_with_actor_round_trips() {
        let reply = SnapshotGetReply {
            id: 9,
            folder: "__mail/aabb".into(),
            created_at: 1_700_000_100,
            file_count: 0,
            total_bytes: 0,
            parent_id: None,
            tags: vec![],
            device_id: None,
            message_kind: Some("mail".into()),
            actor_id: Some(ActorId([0x55u8; 32])),
            files: vec![],
            ..Default::default()
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<SnapshotGetReply>(&bytes).unwrap(), reply);
    }

    /// S6-d's sealing trio rides the same reply as `extra`'s `#[serde(flatten)]`
    /// catch-all, and each field is `skip_serializing_if = "Option::is_none"`.
    /// That pairing is exactly where a `Some` can round-trip back into `extra`
    /// instead of its typed field — neither round-trip test above populates
    /// them, so nothing pinned the wire shape of the fields the break was about.
    #[test]
    fn get_reply_sealed_trio_round_trips() {
        let reply = SnapshotGetReply {
            id: 11,
            folder: "photos".into(),
            tags: vec![],
            tags_sealed: Some(ByteBuf::from(vec![0x11u8; 48])),
            folder_sealed: Some(ByteBuf::from(vec![0x22u8; 48])),
            folder_hash: Some(ByteBuf::from(vec![0x33u8; 32])),
            ..Default::default()
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded = decode::<SnapshotGetReply>(&bytes).unwrap();
        assert_eq!(decoded, reply);
        assert!(
            decoded.extra.is_empty(),
            "the sealing trio must decode into its typed fields, not the flatten catch-all"
        );
    }

    #[test]
    fn delete_round_trips() {
        let req = SnapshotDeleteRequest {
            snapshot_id: 7,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<SnapshotDeleteRequest>(&bytes).unwrap(), req);

        let reply = SnapshotDeleteReply {
            pending_action_id: 42,
            execute_after: 1_700_172_800,
            status: "pending".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<SnapshotDeleteReply>(&bytes).unwrap(), reply);
    }

    #[test]
    fn undelete_round_trips() {
        let req = SnapshotUndeleteRequest {
            snapshot_id: 7,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<SnapshotUndeleteRequest>(&bytes).unwrap(), req);

        let reply = SnapshotUndeleteReply {
            undeleted: true,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<SnapshotUndeleteReply>(&bytes).unwrap(), reply);
    }

    #[test]
    fn prune_round_trips() {
        let req = SnapshotPruneRequest {
            folder: "documents".into(),
            dry_run: true,
            policy: SnapshotRetentionPolicy {
                keep_last: Some(5),
                keep_daily: Some(7),
                keep_tags: vec!["keep".into()],
                keep_within_secs: Some(86_400),
                ..Default::default()
            },
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<SnapshotPruneRequest>(&bytes).unwrap(), req);

        let reply = SnapshotPruneReply {
            dry_run: true,
            pruned: 2,
            remaining: 5,
            snapshots: vec![PrunableSnapshot {
                id: 1,
                created_at: 1_700_000_000,
                tags: vec![],
                extra: Default::default(),
            }],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<SnapshotPruneReply>(&bytes).unwrap(), reply);
    }

    #[test]
    fn prune_set_policy_round_trips() {
        let req = SnapshotPruneSetPolicyRequest {
            folder: "documents".into(),
            dry_run: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(
            decode::<SnapshotPruneSetPolicyRequest>(&bytes).unwrap(),
            req
        );

        let reply = SnapshotPruneSetPolicyReply {
            dry_run: true,
            pruned: 2,
            remaining: 3,
            snapshots: vec![PrunableSnapshot {
                id: 7,
                created_at: 1_700_000_000,
                tags: vec![],
                extra: Default::default(),
            }],
            policy_state: policy_state::APPLIED.into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(
            decode::<SnapshotPruneSetPolicyReply>(&bytes).unwrap(),
            reply
        );
    }

    /// The four `backup-restore.md` § 2 lifecycle fields are **additive**:
    /// an encoding with no lifecycle keys at all must decode as
    /// an ordinary active row rather than failing or inventing a state.
    #[test]
    fn snapshot_summary_row_lifecycle_fields_are_additive() {
        let pre_field = SnapshotSummaryRow {
            id: 3,
            created_at: 1_700_000_000,
            file_count: 9,
            total_bytes: 4096,
            ..Default::default()
        };
        let bytes = encode_canonical(&pre_field).unwrap();
        let decoded = decode::<SnapshotSummaryRow>(&bytes).unwrap();
        assert!(!decoded.soft_deleted);
        assert!(!decoded.deletion_pending);
        assert_eq!(decoded.purge_after, None);
        assert_eq!(decoded.execute_after, None);
        assert_eq!(decoded, pre_field);

        let recovering = SnapshotSummaryRow {
            id: 4,
            created_at: 1_700_000_000,
            soft_deleted: true,
            purge_after: Some(1_702_592_000),
            deletion_pending: true,
            execute_after: Some(1_700_172_800),
            ..Default::default()
        };
        let bytes = encode_canonical(&recovering).unwrap();
        assert_eq!(decode::<SnapshotSummaryRow>(&bytes).unwrap(), recovering);
    }

    #[test]
    fn check_round_trips() {
        let req = SnapshotCheckRequest {
            folder: "documents".into(),
            verify_content: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<SnapshotCheckRequest>(&bytes).unwrap(), req);

        let reply = SnapshotCheckReply {
            status: "ok".into(),
            snapshots_checked: 3,
            files_checked: 10,
            manifests_checked: 10,
            chunks_checked: 40,
            ..Default::default()
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<SnapshotCheckReply>(&bytes).unwrap(), reply);
    }

    #[test]
    fn diff_round_trips() {
        let req = SnapshotDiffRequest {
            a: 1,
            b: 2,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(decode::<SnapshotDiffRequest>(&bytes).unwrap(), req);

        let reply = SnapshotDiffReply {
            snapshot_a: 1,
            snapshot_b: 2,
            added: vec![SnapshotDiffEntry {
                path: "new.txt".into(),
                size_bytes: 100,
                path_hash: None,
                path_sealed: None,
                extra: Default::default(),
            }],
            removed: vec![],
            modified: vec![SnapshotModifiedEntry {
                path: "x.txt".into(),
                old_size: 10,
                new_size: 20,
                path_hash: None,
                path_sealed: None,
                extra: Default::default(),
            }],
            summary: SnapshotDiffSummary {
                added_count: 1,
                removed_count: 0,
                modified_count: 1,
                added_bytes: 100,
                removed_bytes: 0,
                net_bytes: 110,
                extra: Default::default(),
            },
            folder: "docs".into(),
            // Struct-update form for the rest, so a future field growing on
            // this reply does not break the literal again — as `folder` did.
            ..Default::default()
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(decode::<SnapshotDiffReply>(&bytes).unwrap(), reply);
    }

    /// `folder` is required: a diff reply that does not name its set is
    /// refused at decode (the optional form served only a nest predating the
    /// sealed-label expand — the compat-remnant sweep).
    #[test]
    fn snapshot_diff_reply_without_a_folder_is_refused_at_decode() {
        let mut map = std::collections::BTreeMap::new();
        map.insert("snapshot_a".to_string(), Value::Integer(1));
        map.insert("snapshot_b".to_string(), Value::Integer(2));
        map.insert("added".to_string(), Value::List(vec![]));
        map.insert("removed".to_string(), Value::List(vec![]));
        map.insert("modified".to_string(), Value::List(vec![]));
        let summary = [
            "added_count",
            "removed_count",
            "modified_count",
            "added_bytes",
            "removed_bytes",
            "net_bytes",
        ]
        .into_iter()
        .map(|k| (k.to_string(), Value::Integer(0)))
        .collect::<std::collections::BTreeMap<_, _>>();
        map.insert("summary".to_string(), Value::Map(summary));
        let bytes = encode_canonical(&Value::Map(map.clone())).unwrap();
        assert!(
            decode::<SnapshotDiffReply>(&bytes).is_err(),
            "a reply without `folder` must not decode"
        );
        map.insert("folder".to_string(), Value::String("docs".into()));
        let bytes = encode_canonical(&Value::Map(map)).unwrap();
        assert_eq!(decode::<SnapshotDiffReply>(&bytes).unwrap().folder, "docs");
    }

    #[test]
    fn snapshot_list_reply_round_trips() {
        let reply = SnapshotListReply {
            rows: vec![
                SnapshotSummaryRow {
                    id: 7,
                    created_at: 1_700_000_000,
                    message_kind: Some("mail".into()),
                    file_count: 3,
                    total_bytes: 4096,
                    device_id: Some(ByteBuf::from(vec![0x44u8; 32])),
                    tags: Some(vec!["manual".into()]),
                    tags_sealed: Some(ByteBuf::from(vec![9u8; 12])),
                    ..Default::default()
                },
                SnapshotSummaryRow {
                    id: 8,
                    created_at: 1_700_000_100,
                    message_kind: Some("calendar".into()),
                    file_count: 1,
                    total_bytes: 512,
                    ..Default::default()
                },
            ],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SnapshotListReply = decode(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn snapshot_check_is_ok() {
        let mut reply = SnapshotCheckReply {
            status: "ok".into(),
            snapshots_checked: 1,
            files_checked: 1,
            manifests_checked: 1,
            chunks_checked: 1,
            ..Default::default()
        };
        assert!(reply.is_ok());
        reply.status = "errors".into();
        assert!(!reply.is_ok());
    }
}
