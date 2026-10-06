//! Snapshot-related DAO helpers beyond the main `sync_storage` snapshot
//! CRUD. The functions here target the `snapshots` table directly (not
//! `segment_records`), so they belong here rather than in
//! `crate::segments::records_db`.
//!
//! T5 additions (spec D11):
//! - `hard_delete_snapshot` — single-transaction hard delete (no pending action, no
//!   soft-delete window). Called by the immediate-delete handler.
//! - `resolve_snapshot_owner` — returns the actor id that owns the snapshot row
//!   via the `folders.actor_id` join. Used to enforce owner-only auth on
//!   immediate delete.
//!
//! T6 additions (message-kind snapshot create):
//! - `get_or_create_reserved_folder` — look up (or create) the per-actor `__<kind>`
//!   pseudo folder that anchors message-kind snapshots to an owner.
//! - `create_message_kind_snapshot_row` — inserts a message-kind snapshot row with
//!   `message_kind` / `message_manifest` / `placement_manifest` populated.
//!
//! T7 additions (message-kind snapshot restore):
//! - `get_snapshot_kind_manifests` — fetch the BARE-serialised
//!   `(message_manifest, placement_manifest)` blob pair for a snapshot row.
//!   Used by the restore-dispatch handler to load the pinned manifests
//!   before replaying.
//! - `insert_restore_history` — append one row to the `restore_history` table
//!   recording an outcome (one row per kind-aware restore).
//! - `bridge_active_for_actor` — pre-condition predicate; stub returns
//!   `Ok(false)` today, will wire to `subscribe_mailbox_state` once
//!   IMAP-restore Plan 1 lands.
//! - `has_wrapped_mls_blobs` — advisory wrapped-MLS-blob presence check; warns
//!   the restore caller when bridge AUTH will fail until the blobs are restored.

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension};

/// Load every active (not soft-deleted, not deletion-pending) message-kind
/// snapshot's `message_manifest` blob for `(actor_id, kind)`. The Plan 3
/// compaction worker calls this to seed its `PinSet` with every segment id
/// that a live snapshot is still holding onto.
pub fn list_active_message_kind_snapshot_manifests(
    conn: &Connection,
    actor_id: &[u8; 32],
    kind: &str,
) -> Result<Vec<Vec<u8>>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.message_manifest
             FROM snapshots s
             JOIN folders f ON s.folder_id = f.id
             WHERE f.actor_id = ?1
               AND s.message_kind = ?2
               AND s.message_manifest IS NOT NULL
               AND s.soft_deleted = 0
               AND s.deletion_pending = 0",
        )
        .context("prepare list_active_message_kind_snapshot_manifests")?;
    let rows = stmt
        .query_map(rusqlite::params![&actor_id[..], kind], |r| {
            r.get::<_, Vec<u8>>(0)
        })
        .context("query_map list_active_message_kind_snapshot_manifests")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("row list_active_message_kind_snapshot_manifests")?);
    }
    Ok(out)
}

/// Default cap for `list_message_kind_snapshots` when `limit == 0`.
const SNAPSHOT_LIST_DEFAULT_LIMIT: u32 = 100;

/// List the actor's message-kind snapshots, newest first, optionally
/// filtered to one `kind`. Excludes soft-deleted and pending-delete
/// rows. Owner-scoped: the handler passes the bearer actor, so a bearer
/// only ever sees their own snapshots. `limit == 0` falls back to
/// [`SNAPSHOT_LIST_DEFAULT_LIMIT`]. Backs `fauna.filesync.snapshot.list`
/// — the Backups restore picker's local snapshot source
/// (`docs/goal/ui/backups.md` § Restore from backup destination). Only
/// message-kind rows are listed today; folder snapshots stay on the
/// HTTP list until that surface migrates (the `SnapshotSummaryRow` shape
/// already admits them via `message_kind == None`).
pub fn list_message_kind_snapshots(
    conn: &Connection,
    actor_id: &[u8; 32],
    kind: Option<&str>,
    limit: u32,
) -> Result<Vec<fauna_protocol::filesync::SnapshotSummaryRow>> {
    let effective_limit = if limit == 0 {
        SNAPSHOT_LIST_DEFAULT_LIMIT
    } else {
        limit
    };
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.created_at, s.message_kind, s.file_count, s.total_bytes, s.device_id
             FROM snapshots s
             JOIN folders f ON s.folder_id = f.id
             WHERE f.actor_id = ?1
               AND s.message_kind IS NOT NULL
               AND (?2 IS NULL OR s.message_kind = ?2)
               AND s.soft_deleted = 0
               AND s.deletion_pending = 0
             ORDER BY s.created_at DESC, s.id DESC
             LIMIT ?3",
        )
        .context("prepare list_message_kind_snapshots")?;
    let rows = stmt
        .query_map(
            rusqlite::params![actor_id.as_slice(), kind, effective_limit as i64],
            |r| {
                Ok(fauna_protocol::filesync::SnapshotSummaryRow {
                    id: r.get(0)?,
                    created_at: r.get(1)?,
                    message_kind: r.get::<_, Option<String>>(2)?,
                    file_count: r.get(3)?,
                    total_bytes: r.get(4)?,
                    device_id: r
                        .get::<_, Option<Vec<u8>>>(5)?
                        .map(fauna_protocol::ByteBuf::from),
                    // `tags`/`tags_sealed` stay default-`None`: message-kind
                    // snapshots are created without user tags (no `tags`
                    // parameter on `create_message_kind`), so this arm has
                    // nothing to project — the pair rides the folder arm.
                    //
                    // The four § 2 lifecycle fields stay default-inactive for a
                    // stronger reason: the query above filters
                    // `soft_deleted = 0 AND deletion_pending = 0`, so a row
                    // reaching here is active by construction. This is the
                    // ratified split — the lifecycle fields are populated in
                    // folder mode only, and this mode keeps excluding both
                    // classes (`backup-restore.md` § 2 *Row lifecycle fields*).
                    ..Default::default()
                })
            },
        )
        .context("query_map list_message_kind_snapshots")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("row list_message_kind_snapshots")?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Immediate-delete DAO (spec D11)
// ---------------------------------------------------------------------------

/// Hard-delete a snapshot and all of its `snapshot_files` rows in one
/// transaction.  Skips the 48 h pending-action and 30 d soft-delete windows.
///
/// `snapshot_files.snapshot_id` declares `ON DELETE CASCADE` and `foreign_keys`
/// is ON (`db/mod.rs`), so deleting the `snapshots` row alone would already
/// remove the `snapshot_files` rows; we delete them explicitly first anyway for
/// deterministic ordering and as belt-and-suspenders. (Note: `bridge_restore_*`
/// children of `snapshots` do NOT cascade — see `delete_folder_for_user`,
/// which clears them when a whole set is removed.)
pub fn hard_delete_snapshot(conn: &Connection, snapshot_id: i64) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .context("begin hard_delete_snapshot transaction")?;
    tx.execute(
        "DELETE FROM snapshot_files WHERE snapshot_id = ?1",
        rusqlite::params![snapshot_id],
    )
    .context("DELETE snapshot_files for hard_delete_snapshot")?;
    tx.execute(
        "DELETE FROM snapshots WHERE id = ?1",
        rusqlite::params![snapshot_id],
    )
    .context("DELETE snapshots for hard_delete_snapshot")?;
    tx.commit().context("commit hard_delete_snapshot")?;
    Ok(())
}

/// Resolve the actor that owns the snapshot by following
/// `snapshots.folder_id -> folders.actor_id`.
///
/// Works for both folder snapshots (where `message_kind IS NULL`) and
/// message-kind snapshots (T6 points `folder_id` at the per-actor `__<kind>`
/// pseudo folder — the owner is still `folders.actor_id`).
///
/// Returns `None` if the snapshot row or its folder no longer exists.
pub fn resolve_snapshot_owner(conn: &Connection, snapshot_id: i64) -> Result<Option<[u8; 32]>> {
    let result: rusqlite::Result<Vec<u8>> = conn.query_row(
        "SELECT f.actor_id
         FROM snapshots s
         JOIN folders f ON f.id = s.folder_id
         WHERE s.id = ?1",
        rusqlite::params![snapshot_id],
        |row| row.get(0),
    );
    match result {
        Ok(bytes) => {
            let arr: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow!("actor_id blob is not 32 bytes for snapshot {snapshot_id}"))?;
            Ok(Some(arr))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context("resolve_snapshot_owner"),
    }
}

// ---------------------------------------------------------------------------
// Message-kind snapshot DAO (T6)
// ---------------------------------------------------------------------------

/// True iff `name` is a reserved internal folder name — see
/// [`fauna_core::sync::is_reserved_folder_name`], which owns the `__`
/// convention.
///
/// Reserved folders are minted with a `__` prefix — `__<kind>` for the
/// per-actor message-kind sets (`__index`, `__mail`, `__drafts`, …; see
/// [`get_or_create_reserved_folder`]) and `__conv/<channel_hex>` for the
/// per-channel conversation sets (see [`get_or_create_reserved_conv_folder`]).
/// They anchor internal sync surfaces and are never user backup targets, so
/// user-facing projections (notably the backups dropdown behind
/// `fauna.sync.backup_status`) must exclude them.
///
/// Re-exported here rather than defined here (S5): the shared seal funnel must
/// refuse to seal a routing constant, so the predicate had to become shared
/// Rust. Every existing nest call site keeps working unchanged.
pub use fauna_core::sync::is_reserved_folder_name;

/// The ONE seam that routes a folder to the `backup_custody` plane: a
/// **custody copy** — the blind sealed mirror of another location's rail or
/// folder, which this nest provisioned itself and marked
/// `folders.custody_copy` (`reserved-folders.md` § Destination capability).
/// Everything else — rails and every ordinary folder — records to the
/// `sync_changes` head feed (`file-sync.md` § Membership → *Target
/// state — head unification*). Every custody-vs-head routing or gating
/// decision calls this with the row's two columns, never an open-coded read of
/// the flag. The name conjunct is defence in depth: the column's CHECK already
/// makes a custody copy on a non-reserved name unrepresentable.
pub fn is_reserved_custody_copy(custody_copy: bool, name: &str) -> bool {
    custody_copy && is_reserved_folder_name(name)
}

/// Get or create the per-actor pseudo folder that anchors message-kind
/// snapshots for `kind` to the owning actor.
///
/// The reserved folder's name is `__<kind>` (e.g. `__mail`) and its
/// `actor_id` is the owner.  The combination `(name, actor_id)` is unique,
/// so concurrent inserts on the same actor+kind are safe — the row is
/// created exactly once.
///
/// Returns the `folders.id` of the (new or existing) row.
///
/// Kinds whose `is_high_cadence_reserved_kind` returns true are created with
/// the `high_cadence` flag set.
pub fn get_or_create_reserved_folder(
    conn: &Connection,
    actor_id: &[u8; 32],
    kind: &str,
) -> Result<i64> {
    let name = format!("__{kind}");
    let now = super::now_epoch_secs();
    let high_cadence = is_high_cadence_reserved_kind(kind);

    // Attempt insert; ignore UNIQUE constraint violation (row already exists).
    conn.execute(
        "INSERT OR IGNORE INTO folders (name, actor_id, created_at, high_cadence)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![name, &actor_id[..], now, high_cadence as i64],
    )
    .context("insert reserved folder")?;

    adopt_rail_row(conn, &name, actor_id)
}

/// Read back the reserved row a rail mint's `INSERT OR IGNORE` landed on —
/// refusing it when it is a **custody copy**.
///
/// The `INSERT OR IGNORE` adopts whatever row already holds `(name, scope)`,
/// and the only row it can meet that is not this rail is a custody copy
/// (`folders.custody_copy`): one of the two nest-side provisioners minted it on
/// the first custody write, and no client can create a reserved name
/// (`fauna.folders.create` refuses the namespace whole). Adopting it would
/// co-mingle a live offsite backup with this nest's own rail, and re-classing
/// it would drop it out of the GC's manifest-class walk (`backup/gc.rs`) and
/// reclaim that backup. So the mint REFUSES — the same call both provisioners
/// make when the two roles collide (`reserved-folders.md` § Destination
/// capability: the role holding live state wins, the newcomer is refused).
/// Unreachable through the enroll flow, which never points an owner's backup
/// destination at their own home nest; the recovery, if it is ever reached, is
/// the ordinary owner `fauna.folders.delete` of the custody copy, then retry.
fn adopt_rail_row(conn: &Connection, name: &str, scope: &[u8; 32]) -> Result<i64> {
    let (id, custody_copy): (i64, bool) = conn
        .query_row(
            "SELECT id, custody_copy FROM folders WHERE name = ?1 AND actor_id = ?2",
            rusqlite::params![name, &scope[..]],
            |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
        )
        .context("get reserved folder id")?;
    if is_reserved_custody_copy(custody_copy, name) {
        anyhow::bail!(
            "refusing to mint the {name:?} rail over a backup custody copy on this nest: \
             the row is another location's blind mirror, and adopting it would strand its \
             offsite chunks"
        );
    }
    Ok(id)
}

/// Get or create the per-channel pseudo folder that anchors conv
/// message-kind snapshots to a channel (Plan 8 Decision 3).
///
/// Unlike mail/calendar — whose reserved folder is the bare name `__<kind>`
/// scoped by `actor_id` — a conv snapshot's *scope* is the `channel_id`
/// (the MLS group), and one nest holds many channels of the same kind. The
/// channel hex lives in the name (`__conv/<channel_hex>`) for legibility and
/// historical reasons (it predates the per-actor `UNIQUE(name, actor_id)`
/// key; a bare `__conv` would have collided across channels under the old
/// global `UNIQUE(name)`). The `folders.actor_id` column (the folder's
/// *scope key*) carries the `channel_id`, so the existing
/// `resolve_snapshot_owner` / `list_active_message_kind_snapshot_manifests`
/// / `create_message_kind_snapshot_row` machinery works unchanged — they all
/// key on `folders.actor_id`.
///
/// Not high-cadence (conv has no placement layer). `INSERT OR IGNORE` +
/// read-back makes it idempotent and concurrency-safe.
///
/// Refuses to adopt a custody copy exactly as [`get_or_create_reserved_folder`]
/// does ([`adopt_rail_row`]): a conv custody copy is provisioned under the same
/// `(__conv/<hex>, channel_id)` key this mint writes.
///
/// Returns the `folders.id` of the (new or existing) row.
pub fn get_or_create_reserved_conv_folder(conn: &Connection, channel_id: &[u8; 32]) -> Result<i64> {
    let name = format!("__conv/{}", hex::encode(channel_id));
    let now = super::now_epoch_secs();

    conn.execute(
        "INSERT OR IGNORE INTO folders (name, actor_id, created_at, high_cadence)
         VALUES (?1, ?2, ?3, 0)",
        rusqlite::params![name, &channel_id[..], now],
    )
    .context("insert reserved conv folder")?;

    adopt_rail_row(conn, &name, channel_id)
}

/// Returns `true` iff `kind` names a reserved folder that should be
/// flushed at high cadence (5s / 100 events) instead of the default
/// 30s-quiet snapshot cadence.
///
/// Per `file-sync.md` § Reserved folders § High-cadence flush:
/// `__mail-placement`, `__calendar-placement` and `__card-placement` carry
/// the flag. Other reserved folders (`__mail`, `__calendar`,
/// `__card`) do not — content records are larger and snapshot-cadence-grained
/// loss is acceptable.
///
/// `__card-placement` is here because it is the structural twin of
/// `__calendar-placement` (same kilobyte-scale records, same DR cost of losing
/// recent writes), and the two DAV bridge stores must not diverge in DR posture.
/// Note the argument is the **folder** kind — the on-disk dirs are
/// `__calendar-placement` / `__card-placement` — not the `FramedSegmentStore`
/// label, which for calendar is the shorter `"cal-placement"`.
pub fn is_high_cadence_reserved_kind(kind: &str) -> bool {
    matches!(
        kind,
        "mail-placement" | "calendar-placement" | "card-placement"
    )
}

/// Insert a message-kind snapshot row into the `snapshots` table.
///
/// `folder_id` must be the reserved `__<kind>` folder for the actor
/// (obtained via `get_or_create_reserved_folder`).  The `message_kind`,
/// `message_manifest`, and `placement_manifest` columns are populated; all
/// folder-specific columns (`file_count`, `total_bytes`, `parent_id`, `tags`,
/// `device_id`) are set to their zero/NULL defaults.
///
/// Returns the new snapshot's row id.
///
/// **Same-second repeats dedup to one row, keeping the fresher capture.**
/// `snapshots` carries `UNIQUE(folder_id, created_at)` and `created_at` is
/// seconds-granularity, so two creates for the same `(actor, kind)` inside one
/// wall-clock second collide. `backup-restore.md` § 1 rules that a create must
/// never surface a `snapshot.internal` for that collision ("no client-causable
/// error state") and § 5 states message-kind snapshots are "each `(scope, kind,
/// time)` is one snapshots row" — so the collision is dedup, not an error. The
/// folder twin (`sync_storage.rs::create_snapshot`) already did this; this
/// path did not, which is the drift resolved here.
///
/// It differs from the twin in one deliberate way: it **updates** the existing
/// row's manifests instead of returning it untouched. A folder capture reads
/// the `sync_changes` projection, so both racers pin logically the same state;
/// a message-kind capture re-reads `mail_segments`/`conv_segments` after a
/// `finalize_open`, so the second capture can be genuinely *newer*. Returning
/// the first row unchanged would silently discard that newer capture. Writing
/// the fresher pair keeps the constraint's one-row-per-second promise **and**
/// loses nothing: both callers get a row at least as fresh as their own
/// capture, and the manifests carry no GC reference (the reference walk reads
/// `snapshot_files`, which message-kind rows never populate), so replacing them
/// cannot orphan a chunk.
pub fn create_message_kind_snapshot_row(
    conn: &Connection,
    folder_id: i64,
    message_kind: &str,
    message_manifest: Option<&[u8]>,
    placement_manifest: Option<&[u8]>,
) -> Result<i64> {
    let tx = conn
        .unchecked_transaction()
        .context("begin create_message_kind_snapshot_row transaction")?;
    let now = super::now_epoch_secs();
    let insert = tx.execute(
        "INSERT INTO snapshots
             (folder_id, created_at, file_count, total_bytes,
              parent_id, device_id, deletion_pending, soft_deleted,
              message_kind, message_manifest, placement_manifest)
         VALUES (?1, ?2, 0, 0, NULL, NULL, 0, 0, ?3, ?4, ?5)",
        rusqlite::params![
            folder_id,
            now,
            message_kind,
            message_manifest,
            placement_manifest,
        ],
    );
    let id = match insert {
        Ok(_) => tx.last_insert_rowid(),
        Err(rusqlite::Error::SqliteFailure(f, _))
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            // Scoped to a row of the SAME kind: a collision against a folder
            // row (`message_kind IS NULL`) is not this dedup and must still
            // surface, rather than be papered over as success.
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT id FROM snapshots
                     WHERE folder_id = ?1 AND created_at = ?2 AND message_kind = ?3",
                    rusqlite::params![folder_id, now, message_kind],
                    |row| row.get(0),
                )
                .optional()
                .context("look up existing same-second message-kind snapshot")?;
            let existing_id = existing.ok_or_else(|| {
                anyhow!(
                    "snapshots UNIQUE violation for folder {folder_id} at {now} \
                     with no same-second '{message_kind}' row to dedup against"
                )
            })?;
            tx.execute(
                "UPDATE snapshots SET message_manifest = ?1, placement_manifest = ?2
                 WHERE id = ?3",
                rusqlite::params![message_manifest, placement_manifest, existing_id],
            )
            .context("refresh same-second message-kind snapshot manifests")?;
            existing_id
        }
        Err(e) => return Err(e).context("INSERT message-kind snapshot row"),
    };
    tx.commit()
        .context("commit create_message_kind_snapshot_row")?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Message-kind snapshot restore DAO (T7)
// ---------------------------------------------------------------------------

/// Fetch the BARE-serialised `(message_manifest, placement_manifest)` blob
/// pair for a snapshot row. Both columns are `NULL` for folder snapshots
/// (or for message-kind snapshots whose creator didn't capture one — e.g.
/// `placement_manifest` is `NULL` until IMAP-restore Plan 1
/// lands).
///
/// Returns `Ok((None, None))` if the snapshot row does not exist.
pub fn get_snapshot_kind_manifests(
    conn: &Connection,
    snapshot_id: i64,
) -> Result<(Option<Vec<u8>>, Option<Vec<u8>>)> {
    let row = conn
        .query_row(
            "SELECT message_manifest, placement_manifest FROM snapshots WHERE id = ?1",
            rusqlite::params![snapshot_id],
            |r| {
                Ok((
                    r.get::<_, Option<Vec<u8>>>(0)?,
                    r.get::<_, Option<Vec<u8>>>(1)?,
                ))
            },
        )
        .optional()
        .context("query_row get_snapshot_kind_manifests")?;
    Ok(row.unwrap_or((None, None)))
}

/// Append one row to `restore_history` recording the outcome of a
/// kind-aware snapshot restore. Not transactional with the restore SQL
/// itself — callers insert this row only after the restore tx has
/// committed.
///
/// `kinds_restored` is the literal kind string for now (`"mail"` or
/// `"calendar"`); the column is `TEXT NOT NULL` and free-form so a future
/// multi-kind restore can write a comma-separated value.
///
/// `source_member_id` carries backup-destination provenance (which group
/// member's segment chunk pull supplied the segment files); landed in
/// Plan 4. Pass `None` until that plan wires it in.
pub fn insert_restore_history(
    conn: &Connection,
    actor_id: &[u8; 32],
    snapshot_id: i64,
    kinds_restored: &str,
    source_member_id: Option<&[u8]>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO restore_history (completed_at, actor_id, snapshot_id, kinds_restored, source_member_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            super::now_epoch_secs(),
            actor_id.as_slice(),
            snapshot_id,
            kinds_restored,
            source_member_id,
        ],
    )
    .context("INSERT restore_history row")?;
    Ok(())
}

/// Default cap for `list_restore_history` when the caller passes
/// `limit == 0`.
const RESTORE_HISTORY_DEFAULT_LIMIT: u32 = 100;

/// List the actor's `restore_history` rows, newest first. Owner-scoped:
/// rows for any other actor are never returned (the handler passes the
/// bearer actor). `limit == 0` falls back to
/// [`RESTORE_HISTORY_DEFAULT_LIMIT`]. Backs
/// `fauna.filesync.snapshot.list_restore_history`
/// (`docs/goal/ui/backups.md` § Restore history).
pub fn list_restore_history(
    conn: &Connection,
    actor_id: &[u8; 32],
    limit: u32,
) -> Result<Vec<fauna_protocol::filesync::RestoreHistoryRow>> {
    let effective_limit = if limit == 0 {
        RESTORE_HISTORY_DEFAULT_LIMIT
    } else {
        limit
    };
    let mut stmt = conn
        .prepare(
            "SELECT id, completed_at, snapshot_id, kinds_restored, source_member_id
             FROM restore_history
             WHERE actor_id = ?1
             ORDER BY completed_at DESC, id DESC
             LIMIT ?2",
        )
        .context("prepare list_restore_history")?;
    let rows = stmt
        .query_map(
            rusqlite::params![actor_id.as_slice(), effective_limit as i64],
            |r| {
                Ok(fauna_protocol::filesync::RestoreHistoryRow {
                    id: r.get(0)?,
                    completed_at: r.get(1)?,
                    snapshot_id: r.get(2)?,
                    kinds_restored: r.get(3)?,
                    source_member_id: r.get::<_, Option<Vec<u8>>>(4)?,
                    extra: Default::default(),
                })
            },
        )
        .context("query_map list_restore_history")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("row list_restore_history")?);
    }
    Ok(out)
}

/// List the `bridge_restore_divergence` rows recorded against one
/// snapshot, newest first. The handler enforces owner-only access (via
/// `resolve_snapshot_owner`) before calling this; the DAO itself does no
/// auth. Backs `fauna.filesync.snapshot.list_restore_divergence`
/// (`docs/goal/ui/backups.md` § Restore divergence).
pub fn list_restore_divergence(
    conn: &Connection,
    snapshot_id: i64,
) -> Result<Vec<fauna_protocol::filesync::RestoreDivergenceRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, snapshot_id, observed_at, protocol, collection, mua_id,
                    client_modseq, server_modseq, lost_event_count
             FROM bridge_restore_divergence
             WHERE snapshot_id = ?1
             ORDER BY observed_at DESC, id DESC",
        )
        .context("prepare list_restore_divergence")?;
    let rows = stmt
        .query_map(rusqlite::params![snapshot_id], |r| {
            Ok(fauna_protocol::filesync::RestoreDivergenceRow {
                id: r.get(0)?,
                snapshot_id: r.get(1)?,
                observed_at: r.get(2)?,
                protocol: r.get(3)?,
                collection: r.get(4)?,
                mua_id: r.get::<_, Option<String>>(5)?,
                client_modseq: r.get(6)?,
                server_modseq: r.get(7)?,
                lost_event_count: r.get(8)?,
                extra: Default::default(),
            })
        })
        .context("query_map list_restore_divergence")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("row list_restore_divergence")?);
    }
    Ok(out)
}

/// Pre-condition predicate: is a bridge currently serving this actor?
///
/// **Stub today** — returns `Ok(false)` unconditionally. The real
/// predicate will query the `subscribe_mailbox_state` registration table
/// added by IMAP-restore Plan 1; until that table exists,
/// the restore handler proceeds on the admin's word that the bridge
/// was stopped first. The plan-3 dispatch wiring still consults this
/// predicate so the future swap is a one-line change here.
pub fn bridge_active_for_actor(_conn: &Connection, _actor_id: &[u8; 32]) -> Result<bool> {
    // TODO(nest-imap-restore-plan-1): wire to subscribe_mailbox_state
    // once IMAP-restore Plan 1 lands the registration
    // table. The plan-3 restore dispatch already calls this predicate
    // and treats `true` as HTTP 409 — flipping this stub is the only
    // change needed in plan-3-of-message-segment-store at that time.
    Ok(false)
}

/// Advisory pre-condition: does the actor have any wrapped MLS blobs?
///
/// Used by the restore handler to attach a warning to its 200 response
/// when the bridge's wrapped-MLS-blob bundle is missing
/// — bridge AUTH will fail until the bundle is restored. The restore
/// proceeds either way; this predicate is purely informational.
pub fn has_wrapped_mls_blobs(conn: &Connection, actor_id: &[u8; 32]) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bridge_wrapped_mls_blobs WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
            |r| r.get(0),
        )
        .context("query has_wrapped_mls_blobs")?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    #[test]
    fn high_cadence_kind_classifier() {
        assert!(is_high_cadence_reserved_kind("mail-placement"));
        assert!(is_high_cadence_reserved_kind("calendar-placement"));
        // The card journal is calendar's twin — same records, same DR cost.
        assert!(is_high_cadence_reserved_kind("card-placement"));
        assert!(!is_high_cadence_reserved_kind("mail"));
        assert!(!is_high_cadence_reserved_kind("calendar"));
        // Content segments never ride the fast cadence, cards included.
        assert!(!is_high_cadence_reserved_kind("card"));
        assert!(!is_high_cadence_reserved_kind("config"));
        assert!(!is_high_cadence_reserved_kind(""));
        // The FramedSegmentStore label, not a folder kind — must NOT match.
        assert!(!is_high_cadence_reserved_kind("cal-placement"));
    }

    #[test]
    fn reserved_folder_name_classifier() {
        // Per-actor message-kind reserved sets.
        assert!(is_reserved_folder_name("__config"));
        assert!(is_reserved_folder_name("__index"));
        assert!(is_reserved_folder_name("__mail"));
        // Per-channel conversation reserved sets.
        assert!(is_reserved_folder_name("__conv/deadbeef"));
        // User backup sets are not reserved.
        assert!(!is_reserved_folder_name("documents-abc"));
        assert!(!is_reserved_folder_name("photos"));
        assert!(!is_reserved_folder_name("_single-underscore"));
        assert!(!is_reserved_folder_name(""));
    }

    #[tokio::test]
    async fn reserved_folder_mail_placement_sets_high_cadence() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x42u8; 32];
        db.get_or_create_reserved_folder(&actor, "mail-placement")
            .await
            .unwrap();
        let row = db
            .get_folder("__mail-placement")
            .await
            .unwrap()
            .expect("__mail-placement folder");
        assert!(row.high_cadence, "mail-placement must be high_cadence");
    }

    #[tokio::test]
    async fn reserved_folder_calendar_placement_sets_high_cadence() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x43u8; 32];
        db.get_or_create_reserved_folder(&actor, "calendar-placement")
            .await
            .unwrap();
        let row = db
            .get_folder("__calendar-placement")
            .await
            .unwrap()
            .expect("__calendar-placement folder");
        assert!(row.high_cadence, "calendar-placement must be high_cadence");
    }

    #[tokio::test]
    async fn reserved_folder_mail_not_high_cadence() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x44u8; 32];
        db.get_or_create_reserved_folder(&actor, "mail")
            .await
            .unwrap();
        let row = db
            .get_folder("__mail")
            .await
            .unwrap()
            .expect("__mail folder");
        assert!(!row.high_cadence, "mail (content) must NOT be high_cadence");
    }

    #[tokio::test]
    async fn reserved_conv_folder_per_channel_distinct_and_idempotent() {
        let db = CacheDb::open_in_memory().unwrap();
        let ch1 = [0x51u8; 32];
        let ch2 = [0x52u8; 32];

        let id1 = db.get_or_create_reserved_conv_folder(&ch1).await.unwrap();
        let id2 = db.get_or_create_reserved_conv_folder(&ch2).await.unwrap();
        assert_ne!(id1, id2, "distinct channels → distinct folder rows");

        // Idempotent: a repeat call returns the same id (INSERT OR IGNORE).
        let id1_again = db.get_or_create_reserved_conv_folder(&ch1).await.unwrap();
        assert_eq!(id1_again, id1, "repeat call must reuse the existing row");

        // The name carries the channel hex; the actor_id column is the channel.
        let row = db
            .get_folder(&format!("__conv/{}", hex::encode(ch1)))
            .await
            .unwrap()
            .expect("__conv/<hex> folder for ch1");
        assert_eq!(row.id, id1);
        assert_eq!(row.name, format!("__conv/{}", hex::encode(ch1)));
        assert_eq!(row.actor_id, ch1.to_vec(), "actor_id column = channel_id");
        assert!(
            !row.high_cadence,
            "conv reserved set must NOT be high_cadence"
        );
    }

    #[tokio::test]
    async fn list_restore_history_returns_actor_rows_newest_first() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x90u8; 32];
        let other = [0x91u8; 32];

        let fs = db
            .get_or_create_reserved_folder(&actor, "mail")
            .await
            .unwrap();
        let snap = db
            .create_message_kind_snapshot_row(fs, "mail", None, None)
            .await
            .unwrap();
        db.insert_restore_history(&actor, snap, "mail", None)
            .await
            .unwrap();
        db.insert_restore_history(&actor, snap, "calendar", None)
            .await
            .unwrap();

        // A different actor's row must not leak into the result. We cannot
        // give `other` its own `__mail` folder in the same DB —
        // `folders.name` is globally UNIQUE (migration V1), a latent
        // multi-user bug tracked separately. The list query scopes purely
        // on `restore_history.actor_id`, so reusing `snap` (it only needs
        // to satisfy the snapshot_id FK) exercises the isolation property
        // without tripping that constraint.
        db.insert_restore_history(&other, snap, "mail", None)
            .await
            .unwrap();

        let rows = db.list_restore_history(&actor, 0).await.unwrap();
        assert_eq!(rows.len(), 2, "only the queried actor's rows");
        assert!(rows.iter().all(|r| r.snapshot_id == snap));
        // Same completed_at (now) → id-DESC tiebreak puts the calendar row
        // (inserted second, higher id) first.
        assert!(rows[0].id > rows[1].id, "newest-first ordering");
        assert_eq!(rows[0].kinds_restored, "calendar");
    }

    #[tokio::test]
    async fn list_restore_divergence_reads_what_the_writer_wrote() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x92u8; 32];

        let fs = db
            .get_or_create_reserved_folder(&actor, "mail")
            .await
            .unwrap();
        let snap = db
            .create_message_kind_snapshot_row(fs, "mail", None, None)
            .await
            .unwrap();
        // write_divergence_row keys to the most recent restore_history row.
        db.insert_restore_history(&actor, snap, "mail", None)
            .await
            .unwrap();
        {
            let conn = db.conn().await;
            let tx = conn.unchecked_transaction().unwrap();
            crate::restore::divergence::write_divergence_row(
                &tx,
                &actor,
                "imap",
                "INBOX",
                Some("Thunderbird/115.0"),
                99,
                42,
                1_700_000_000,
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let rows = db.list_restore_divergence(snap).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].snapshot_id, snap);
        assert_eq!(rows[0].protocol, "imap");
        assert_eq!(rows[0].collection, "INBOX");
        assert_eq!(rows[0].mua_id.as_deref(), Some("Thunderbird/115.0"));
        assert_eq!(rows[0].client_modseq, 99);
        assert_eq!(rows[0].server_modseq, 42);
        assert_eq!(rows[0].lost_event_count, 57);

        // A snapshot with no divergence rows returns empty.
        assert!(
            db.list_restore_divergence(999_999)
                .await
                .unwrap()
                .is_empty()
        );
    }

    // Confirms the column index in the list query path lines up with
    // the SELECT column list (a parallel path to `get_folder`).
    #[tokio::test]
    async fn list_folders_round_trips_high_cadence() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x46u8; 32];
        db.get_or_create_reserved_folder(&actor, "mail-placement")
            .await
            .unwrap();
        db.get_or_create_reserved_folder(&actor, "mail")
            .await
            .unwrap();

        let rows = db.list_folders().await.unwrap();
        let placement = rows
            .iter()
            .find(|r| r.name == "__mail-placement")
            .expect("__mail-placement listed");
        let mail = rows
            .iter()
            .find(|r| r.name == "__mail")
            .expect("__mail listed");
        assert!(placement.high_cadence);
        assert!(!mail.high_cadence);
    }

    // ── Same-second create dedup (the 2026-07-24 tui Backups report) ──
    //
    // `snapshots` carries `UNIQUE(folder_id, created_at)` with
    // seconds-granularity `created_at`. These tests are latency-independent by
    // construction (convention 14): they drive the DAO directly, back to back,
    // which lands both calls inside one second without any sleep or timing
    // assertion. They are the headless pin for the client-visible symptom —
    // `fauna.filesync.snapshot.internal` on a plain repeat create.

    #[tokio::test]
    async fn message_kind_same_second_repeat_dedups_instead_of_erroring() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x71u8; 32];
        let fs = db
            .get_or_create_reserved_folder(&actor, "mail")
            .await
            .unwrap();

        let first = db
            .create_message_kind_snapshot_row(fs, "mail", Some(b"manifest-1"), Some(b"place-1"))
            .await
            .expect("first create");
        // Before the fix this second call escaped as a ConstraintViolation and
        // the handler mapped it to `fauna.filesync.snapshot.internal`.
        let second = db
            .create_message_kind_snapshot_row(fs, "mail", Some(b"manifest-2"), Some(b"place-2"))
            .await
            .expect("same-second repeat must not error");

        assert_eq!(
            first, second,
            "one row per (scope, kind, second) — the repeat must dedup to the same id"
        );

        // Newer capture wins: a message-kind capture re-reads the segment store,
        // so the second call's manifests can be genuinely fresher than the
        // first's and must not be silently discarded.
        let (content, placement) = db.get_snapshot_kind_manifests(first).await.unwrap();
        assert_eq!(content.as_deref(), Some(&b"manifest-2"[..]));
        assert_eq!(placement.as_deref(), Some(&b"place-2"[..]));

        // Exactly one row exists for the set — the dedup did not leave a second.
        let rows = db
            .list_message_kind_snapshots(&actor, Some("mail"), 0)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "expected a single deduped row, got {rows:?}");
    }

    #[tokio::test]
    async fn message_kind_same_second_dedup_is_scoped_to_one_reserved_set() {
        // Two actors, and two kinds for one actor, each anchor a distinct
        // reserved folder — so none of them share the UNIQUE key and no
        // dedup should occur between them.
        let db = CacheDb::open_in_memory().unwrap();
        let a = [0x72u8; 32];
        let b = [0x73u8; 32];

        let a_mail = db.get_or_create_reserved_folder(&a, "mail").await.unwrap();
        let a_conv = db.get_or_create_reserved_folder(&a, "conv").await.unwrap();
        let b_mail = db.get_or_create_reserved_folder(&b, "mail").await.unwrap();

        let s1 = db
            .create_message_kind_snapshot_row(a_mail, "mail", Some(b"a-mail"), None)
            .await
            .unwrap();
        let s2 = db
            .create_message_kind_snapshot_row(a_conv, "conv", Some(b"a-conv"), None)
            .await
            .unwrap();
        let s3 = db
            .create_message_kind_snapshot_row(b_mail, "mail", Some(b"b-mail"), None)
            .await
            .unwrap();

        assert_ne!(s1, s2, "different kinds must not dedup together");
        assert_ne!(s1, s3, "different actors must not dedup together");
        assert_ne!(s2, s3);
    }

    #[tokio::test]
    async fn same_second_collision_with_a_folder_row_still_surfaces() {
        // The dedup is scoped to a row of the SAME message kind. A collision
        // against a folder row (`message_kind IS NULL`) is a different
        // condition and must NOT be silently reported as success — otherwise
        // the caller would receive a folder snapshot id for a message-kind
        // create. Unreachable through the handlers today (reserved sets are
        // refused on the folder create path), so this pins the guard rather
        // than a live flow.
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x74u8; 32];
        let fs = db
            .get_or_create_reserved_folder(&actor, "mail")
            .await
            .unwrap();

        // Hand-write a folder-shaped row occupying (fs, now).
        let now = crate::db::now_epoch_secs();
        {
            let conn = db.conn().await;
            conn.execute(
                "INSERT INTO snapshots
                     (folder_id, created_at, file_count, total_bytes,
                      parent_id, device_id, deletion_pending, soft_deleted,
                      message_kind, message_manifest, placement_manifest)
                 VALUES (?1, ?2, 0, 0, NULL, NULL, 0, 0, NULL, NULL, NULL)",
                rusqlite::params![fs, now],
            )
            .unwrap();
        }

        let err = db
            .create_message_kind_snapshot_row(fs, "mail", Some(b"m"), None)
            .await
            .expect_err("a folder-row collision must not be swallowed as a dedup");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("no same-second"),
            "expected the explicit no-row-to-dedup-against error, got: {rendered}"
        );
    }
}
