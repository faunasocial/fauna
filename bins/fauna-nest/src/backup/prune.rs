//! Automatic retention pruning — the one place both auto-prune consumers agree.
//!
//! `docs/goal/behavior/backup-restore.md` § 8 RULING (2026-08-01) fixes the
//! authoritative resting copy of a retention policy as the **per-set
//! `folders.retention_policy` column**, and § 8 consequence (iii) gates arming
//! it on the no-user-data-loss invariant. This module is the armed path, shared
//! by the two consumers the RULING names:
//!
//! - [`SnapshotScheduler::auto_prune_folder`](crate::backup::scheduler::SnapshotScheduler::auto_prune_folder)
//!   — fires after each auto-snapshot, on the one set that just changed;
//! - [`GcScheduler::prune_all_folders`](crate::backup::gc_scheduler) — sweeps
//!   every actor's sets on the 6-hourly GC cycle.
//!
//! **They share this function rather than each re-deriving the policy read.**
//! Two independent copies of a delete predicate is how one of them ends up a
//! version behind the other, and the direction that drifts is deletion.
//!
//! # The route (§ 7 Deletion Safety, all three layers)
//!
//! A prune here **schedules**; it never deletes. In order:
//!
//! 1. **Layer 1** — [`evaluate_folder_retention_at`] never proposes a
//!    candidate set that would leave fewer than
//!    [`SNAPSHOT_HARD_FLOOR`] active snapshots, and the candidate population is
//!    the same *active* population `count_active_snapshots` counts, so this
//!    floor and the interactive delete handler's agree by construction.
//! 2. **Layer 2** — one `SnapshotBulkPrune` pending action per prune, carrying
//!    the ratified **7-day** `execute_after`. The user cancels it from their app
//!    like any other pending action. Its targets are marked
//!    `deletion_pending`, which is also what makes this function idempotent:
//!    the next cycle no longer sees them as candidates, so a slow window cannot
//!    pile up duplicate actions.
//! 3. **Layer 3** — when the window elapses, the executor
//!    (`pending_actions::execute_action`, arm `"snapshot.bulk_prune"`)
//!    *soft*-deletes each target, opening a further 30-day `purge_after`
//!    recovery window served by `fauna.filesync.snapshot.undelete`.
//!
//! Only after that does GC's phase 3 purge the rows. Nothing on this path calls
//! `delete_snapshots`.

use std::sync::Arc;

use anyhow::Result;

use super::retention::{
    FolderRetention, SNAPSHOT_HARD_FLOOR, SnapshotMeta, effective_folder_retention,
    evaluate_folder_retention_at,
};
use crate::db::{CacheDb, FolderRow, SnapshotRow};

/// The § 7 candidate population: a snapshot that is neither soft-deleted nor
/// already inside a cancellable window.
///
/// **Every prune consumer — [`schedule_auto_prune`] here and the
/// `fauna.filesync.snapshot.prune` wire handler — MUST share this predicate**
/// rather than re-derive it, for the module-header reason: two copies of a
/// delete predicate drift, and the direction that drifts is deletion. Counting
/// non-active rows (a) re-proposes snapshots an earlier cycle already
/// scheduled, (b) computes the hard floor against a different population than
/// `count_active_snapshots`, and (c) — the data-loss bug — lets a
/// prune reach *into* the recovery windows the deletion-safety layers opened:
/// hard-deleting a `soft_deleted` row closes its 30-day undelete window, and
/// hard-deleting a `deletion_pending` row forecloses its 7-day cancel.
pub fn is_active_snapshot(s: &SnapshotRow) -> bool {
    !s.soft_deleted && !s.deletion_pending
}

/// Schedule an automatic prune for one folder, per its resting policy column.
///
/// Returns the number of snapshots scheduled for pruning (0 when the set has no
/// policy, an unreadable one, or nothing out of bounds). Never deletes.
pub async fn schedule_auto_prune(db: &Arc<CacheDb>, fs: &FolderRow) -> Result<usize> {
    let policy = match effective_folder_retention(fs) {
        FolderRetention::Policy(p) => p,
        // The healthy default on every set that has never been given a policy:
        // keep everything, silently.
        FolderRetention::NotSet => return Ok(0),
        // A writer disagreeing with the canonical 2-field shape
        // (`backup-restore.md` § 8 consequence (ii)) — a `keep_*` JSON
        // written here by such a writer is deliberately left at rest (§ 8 → *The off-shape at-rest residual*). Refuse and say
        // so; a guess would prune against a number the user never chose.
        FolderRetention::Unparseable => {
            tracing::warn!(
                folder = %fauna_core::log_redact::log_folder_name(&fs.name),
                "retention policy is not the canonical {{max_snapshots, max_age_days}} shape — \
                 keeping every snapshot (backup-restore.md § 8)"
            );
            return Ok(0);
        }
    };

    // The candidate population is the ACTIVE snapshots only — the shared
    // [`is_active_snapshot`] predicate; its doc comment carries the three
    // reasons, all of them deletion-shaped.
    let mut metas: Vec<SnapshotMeta> = db
        .list_set_snapshots(fs.id)
        .await?
        .iter()
        .filter(|s| is_active_snapshot(s))
        .map(|s| SnapshotMeta::from_tag_hashes(s.id, s.created_at, s.tag_hashes.as_deref()))
        .collect();
    metas.reverse(); // list_snapshots is newest-first; the evaluator wants oldest-first.

    let now = crate::db::now_epoch_secs();
    let prunable_ids = evaluate_folder_retention_at(&policy, &metas, now);
    if prunable_ids.is_empty() {
        return Ok(0);
    }

    // Layer 2: the cancellable window. One action for the whole batch — the
    // user's cancel gesture is "keep my snapshots", not a per-row decision.
    let payload = serde_json::json!({ "snapshot_ids": prunable_ids }).to_string();
    let action_id = db
        .create_pending_action(
            &crate::pending_actions::ActionType::SnapshotBulkPrune,
            &fs.actor_id,
            Some(&fs.id.to_string()),
            Some(&payload),
            None,
        )
        .await?;

    for id in &prunable_ids {
        db.mark_snapshot_deletion_pending(*id).await?;
    }

    tracing::info!(
        folder = %fauna_core::log_redact::log_folder_name(&fs.name),
        action_id,
        scheduled = prunable_ids.len(),
        retained = metas.len() - prunable_ids.len(),
        floor = SNAPSHOT_HARD_FLOOR,
        "auto-prune scheduled: snapshots enter the cancellable window (backup-restore.md § 7)"
    );
    Ok(prunable_ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::retention::MESSAGE_KIND_DEFAULT_RETENTION;

    /// `n` snapshot rows on the set — message-kind rows when `kind` is given,
    /// as the creators write them on a reserved row, else folder rows.
    async fn with_snapshots(
        db: &Arc<CacheDb>,
        folder_id: i64,
        kind: Option<&str>,
        n: i64,
    ) -> FolderRow {
        {
            let conn = db.conn().await;
            for i in 0..n {
                conn.execute(
                    "INSERT INTO snapshots (folder_id, created_at, file_count, total_bytes, message_kind)
                     VALUES (?1, ?2, 0, 0, ?3)",
                    rusqlite::params![folder_id, 1_000 + i, kind],
                )
                .unwrap();
            }
        }
        db.get_folder_by_id(folder_id).await.unwrap().unwrap()
    }

    /// `backup-restore.md` § 8 → *Message-kind rows*: an unset kind row keeps
    /// the newest 48, through the cancellable pending action — never a delete.
    #[tokio::test]
    async fn an_unset_kind_row_schedules_its_overflow_through_the_pending_action() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let actor = [0x51u8; 32];
        let fs_id = db
            .get_or_create_reserved_folder(&actor, "mail")
            .await
            .unwrap();
        let fs = with_snapshots(&db, fs_id, Some("mail"), 50).await;
        assert_eq!(fs.retention_policy, None, "nobody chose a policy");
        assert_eq!(
            effective_folder_retention(&fs),
            FolderRetention::Policy(MESSAGE_KIND_DEFAULT_RETENTION)
        );

        assert_eq!(schedule_auto_prune(&db, &fs).await.unwrap(), 2);
        let snaps = db.list_set_snapshots(fs_id).await.unwrap();
        assert_eq!(snaps.len(), 50, "a prune schedules — nothing is deleted");
        assert_eq!(snaps.iter().filter(|s| s.deletion_pending).count(), 2);
        assert!(snaps.iter().all(|s| !s.soft_deleted));
        let actions = db.list_pending_actions_for_actor(&actor).await.unwrap();
        assert_eq!(actions.len(), 1, "one cancellable SnapshotBulkPrune action");
    }

    /// The default is the kind rows' alone: an ordinary folder with no policy
    /// still keeps everything.
    #[tokio::test]
    async fn a_folder_row_with_no_policy_keeps_everything() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let actor = [0x52u8; 32];
        let fs_id = db.create_folder("docs", &actor).await.unwrap();
        let fs = with_snapshots(&db, fs_id, None, 50).await;
        assert_eq!(effective_folder_retention(&fs), FolderRetention::NotSet);
        assert_eq!(schedule_auto_prune(&db, &fs).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_conv_channel_row_takes_the_kind_default() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let fs_id = db
            .get_or_create_reserved_conv_folder(&[0x53u8; 32])
            .await
            .unwrap();
        let fs = db.get_folder_by_id(fs_id).await.unwrap().unwrap();
        assert_eq!(
            effective_folder_retention(&fs),
            FolderRetention::Policy(MESSAGE_KIND_DEFAULT_RETENTION)
        );
    }
}
