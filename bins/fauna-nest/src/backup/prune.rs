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
    FolderRetention, SNAPSHOT_HARD_FLOOR, SnapshotMeta, evaluate_folder_retention_at,
    parse_folder_retention,
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
    let policy = match parse_folder_retention(fs.retention_policy.as_deref()) {
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
        .list_snapshots(fs.id)
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
