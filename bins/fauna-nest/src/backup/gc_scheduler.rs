//! Background GC scheduler.
//!
//! Periodically schedules auto-prune on all folders (per each set's own
//! resting `folders.retention_policy` column — `backup-restore.md` § 8
//! RULING), then runs garbage collection to reclaim orphaned blobs, then purges
//! soft-deleted snapshots whose 30-day recovery window has elapsed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;

use crate::backup::service::BackupService;

pub struct GcScheduler {
    backup_service: Arc<BackupService>,
    /// The `__post` segment store the reference walk
    /// reads live post bodies from (`gc::PostBodySource`, step 2f).
    post_segments: Arc<fauna_segment_store::SegmentManager>,
    interval: Duration,
    grace_period_secs: u64,
    running: AtomicBool,
}

impl GcScheduler {
    pub fn new(
        backup_service: Arc<BackupService>,
        post_segments: Arc<fauna_segment_store::SegmentManager>,
        interval: Duration,
        grace_period_secs: u64,
    ) -> Self {
        Self {
            backup_service,
            post_segments,
            interval,
            grace_period_secs,
            running: AtomicBool::new(false),
        }
    }

    /// Spawn the background GC task. Returns a JoinHandle.
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.interval);
            loop {
                interval.tick().await;
                if self.running.swap(true, Ordering::SeqCst) {
                    tracing::debug!("gc scheduler: previous run still active, skipping");
                    continue;
                }
                if let Err(e) = self.run_gc_cycle().await {
                    tracing::error!(error = %e, "gc scheduler error");
                }
                self.running.store(false, Ordering::SeqCst);
            }
        })
    }

    /// Run one full GC cycle: prune all folders, then collect garbage.
    pub async fn run_gc_cycle(&self) -> Result<()> {
        // Phase 1: schedule auto-prune per each set's own retention column
        self.prune_all_folders().await?;

        // Phase 2: Garbage collect orphaned blobs
        let dyn_store = self.backup_service.local_blob_store();
        let result = super::gc::garbage_collect(
            self.backup_service.db(),
            &dyn_store,
            super::gc::PostBodySource {
                segments: &self.post_segments,
            },
            self.grace_period_secs as i64,
            self.backup_service.encryption_key(),
            false,
        )
        .await?;

        tracing::info!(
            deleted_blobs = result.deleted_blobs,
            deleted_bytes = result.deleted_bytes,
            skipped_grace = result.skipped_grace_period,
            manifest_decode_failures = result.manifest_decode_failures,
            record_blob_refs = result.record_blob_refs,
            record_read_failures = result.record_read_failures,
            "gc cycle complete"
        );

        // Phase 3: Purge expired soft-deleted snapshots
        let expired = self.backup_service.db().list_expired_soft_deleted().await?;
        if !expired.is_empty() {
            let count = self.backup_service.db().delete_snapshots(&expired).await?;
            tracing::info!(
                purged = count,
                "gc scheduler: purged expired soft-deleted snapshots"
            );
        }

        // Phase 3b: purge expired soft-pruned VERSIONS (file-versions.md
        // § Retention (3)) — stamp `superseded_at` on rows whose 30-day
        // `purge_after` has elapsed; the *next* cycle's sweep then reclaims
        // their chunks once the stamp is older than the orphan grace. A
        // guard-skipped row (no strictly newer row for its path — a shape this
        // pipeline never produces) stays soft-pruned and recoverable; loud,
        // never destructive.
        let (purged, guarded) = self
            .backup_service
            .db()
            .purge_expired_soft_pruned_versions()
            .await?;
        if purged > 0 {
            tracing::info!(purged, "gc scheduler: purged expired soft-pruned versions");
        }
        if guarded > 0 {
            tracing::warn!(
                guarded,
                "gc scheduler: expired soft-pruned versions SKIPPED by the \
                 newer-row guard — left recoverable (file-versions.md § Retention)"
            );
        }

        Ok(())
    }

    /// Schedule automatic pruning across **every** actor's folders, per each
    /// set's own resting `retention_policy` column (`backup-restore.md` § 8
    /// RULING, 2026-08-01). The mechanism — and the § 7 layers it routes
    /// through instead of deleting — is on
    /// [`crate::backup::prune::schedule_auto_prune`].
    ///
    /// ⚠ Like its scheduler-side twin this reads no client-sealed state: it
    /// rests under keys the nest never holds, so such a read could only ever
    /// come back empty. Do not reintroduce it.
    ///
    /// One set's failure must not abort the sweep — a policy this nest cannot
    /// act on is a reason to skip that set, never to stop pruning the rest.
    async fn prune_all_folders(&self) -> Result<()> {
        let db = self.backup_service.db();
        for fs in &db.list_folders().await? {
            if let Err(e) = super::prune::schedule_auto_prune(db, fs).await {
                tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(&fs.name),
                    error = %e,
                    "gc scheduler: auto-prune scheduling failed"
                );
            }
            // The version plane's leg of the same phase (file-versions.md
            // § Retention): reads the sibling `version_retention` column,
            // schedules a 7-day cancellable `VersionBulkPrune`, never deletes.
            // Same isolation rule — one set's failure never stops the sweep.
            if let Err(e) = super::version_prune::schedule_version_auto_prune(db, fs).await {
                tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(&fs.name),
                    error = %e,
                    "gc scheduler: version auto-prune scheduling failed"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use fauna_core::data::ContentHash;

    /// An empty throwaway `__post` store for the record walk (`gc` step 2f) —
    /// these tests seed no posts.
    fn post_segments(tmp: &tempfile::TempDir) -> Arc<fauna_segment_store::SegmentManager> {
        Arc::new(fauna_segment_store::SegmentManager::new(
            tmp.path().join("post-segments"),
            "post",
        ))
    }

    #[tokio::test]
    async fn gc_scheduler_run_cycle_cleans_orphans() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();

        let actor_id = [1u8; 32];
        db.create_folder("gc-sched-test", &actor_id).await.unwrap();

        let svc = Arc::new(
            BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
        );
        let store = svc.local_blob_store();

        // Create an orphaned blob
        let orphan = ContentHash::from_digest_raw([0xDDu8; 32]);
        store.put(&orphan, b"orphan data").await.unwrap();
        db.put_blob_metadata(&orphan.digest(), 11, "chunk", None, None)
            .await
            .unwrap();

        let scheduler = GcScheduler::new(
            svc,
            post_segments(&tmp),
            Duration::from_secs(3600),
            0, // 0 grace period for test
        );

        scheduler.run_gc_cycle().await.unwrap();

        assert!(
            !store.exists(&orphan).await.unwrap(),
            "orphan should be deleted by GC"
        );
    }

    #[tokio::test]
    async fn gc_scheduler_purges_expired_soft_deleted_snapshots() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();

        let actor_id = [2u8; 32];
        let fs_id = db.create_folder("gc-purge-test", &actor_id).await.unwrap();

        // Insert a snapshot with an explicit created_at so UNIQUE constraint is satisfied
        let snap_id = db.insert_snapshot_at(fs_id, 1000).await.unwrap();

        // Soft-delete the snapshot (sets purge_after = now + 30 days)
        db.soft_delete_snapshot(snap_id).await.unwrap();

        // Force purge_after to the past (epoch 0) so it appears expired
        db.set_snapshot_purge_after(snap_id, 0).await.unwrap();

        let svc = Arc::new(
            BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
        );
        let scheduler = GcScheduler::new(svc, post_segments(&tmp), Duration::from_secs(3600), 0);

        scheduler.run_gc_cycle().await.unwrap();

        // Snapshot should be hard-deleted
        let row = db.get_snapshot(snap_id).await.unwrap();
        assert!(
            row.is_none(),
            "expired soft-deleted snapshot should be purged"
        );
    }

    #[tokio::test]
    async fn gc_scheduler_run_cycle_respects_grace_period() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();

        let actor_id = [1u8; 32];
        db.create_folder("gc-grace-test", &actor_id).await.unwrap();

        let svc = Arc::new(
            BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
        );
        let store = svc.local_blob_store();

        // Create an orphaned blob (just now, within grace period)
        let orphan = ContentHash::from_digest_raw([0xEEu8; 32]);
        store.put(&orphan, b"recent orphan").await.unwrap();
        db.put_blob_metadata(&orphan.digest(), 13, "chunk", None, None)
            .await
            .unwrap();

        let scheduler = GcScheduler::new(
            svc,
            post_segments(&tmp),
            Duration::from_secs(3600),
            1800, // 30 min grace period
        );

        scheduler.run_gc_cycle().await.unwrap();

        assert!(
            store.exists(&orphan).await.unwrap(),
            "recent orphan should be protected by grace period"
        );
    }

    /// The canonical per-set policy JSON the shared creation wizard writes
    /// (`backup-restore.md` § 8 RULING consequence (ii)).
    fn policy_json(max_snapshots: u32, max_age_days: u32) -> String {
        serde_json::to_string(&fauna_folders_machine::state::RetentionPolicy {
            max_snapshots,
            max_age_days,
        })
        .unwrap()
    }

    async fn set_retention(db: &Arc<CacheDb>, name: &str, actor_id: &[u8; 32], json: &str) {
        assert!(
            db.update_folder_for_user(
                name,
                actor_id,
                crate::db::FolderUpdate {
                    retention_policy: Some(Some(json)),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
            "fixture: the retention column must actually be written"
        );
    }

    /// The GC sweep's own pin — `prune_all_folders` walks **every** actor's
    /// sets, a scope the scheduler-side tests never reach — and the arming pin
    /// for `backup-restore.md` § 8 RULING consequence (iii).
    ///
    /// Two things at once, because they are the same guarantee from both sides:
    /// a set with a user-chosen policy **is** pruned to it, and a set with no
    /// policy is left completely alone by the identical sweep. The second half
    /// is the no-user-data-loss assertion — it is the state every folder on
    /// every deployment is in until someone fills in the wizard.
    ///
    /// ⚠️ Rewritten 2026-08-01, deliberately, when auto-prune was armed. It
    /// previously seeded a client-sealed config blob (the `__config` rail,
    /// since retired) and asserted the sweep pruned (or did not prune) on the
    /// strength of it. That reader is deleted: the blob rested sealed under the
    /// CLIENT's BackupKey, so the sweep could never read it, and the
    /// authoritative resting copy is this column.
    #[tokio::test]
    async fn the_gc_sweep_prunes_to_the_per_set_column_and_only_when_one_is_set() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let actor_id = [7u8; 32];

        // Two sets under one actor: one with a policy, one without.
        let armed = db.create_folder("prune-target", &actor_id).await.unwrap();
        let untouched = db.create_folder("no-policy", &actor_id).await.unwrap();
        let mut armed_ids = Vec::new();
        for ts in [1000, 2000, 3000, 4000, 5000] {
            armed_ids.push(db.insert_snapshot_at(armed, ts).await.unwrap());
            db.insert_snapshot_at(untouched, ts).await.unwrap();
        }
        set_retention(&db, "prune-target", &actor_id, &policy_json(3, 0)).await;

        let svc = Arc::new(
            BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
        );
        let scheduler = GcScheduler::new(svc, post_segments(&tmp), Duration::from_secs(3600), 0);
        scheduler.run_gc_cycle().await.unwrap();

        // Layer 2, not deletion: the two out-of-bounds snapshots are marked
        // pending inside a cancellable window; every row is still present.
        let snaps = db.list_snapshots(armed).await.unwrap();
        assert_eq!(snaps.len(), 5, "a scheduled prune deletes NOTHING");
        let pending: Vec<i64> = snaps
            .iter()
            .filter(|s| s.deletion_pending)
            .map(|s| s.id)
            .collect();
        assert_eq!(
            pending,
            vec![armed_ids[1], armed_ids[0]],
            "the two OLDEST are scheduled; the 3 newest the policy keeps are not"
        );
        assert!(
            snaps.iter().all(|s| !s.soft_deleted),
            "soft-delete is the executor's job, after the 7-day window"
        );

        // The set with no policy is untouched by the same sweep.
        assert!(
            db.list_snapshots(untouched)
                .await
                .unwrap()
                .iter()
                .all(|s| !s.deletion_pending && !s.soft_deleted),
            "a set with no retention policy must keep everything, forever"
        );
    }

    /// Obligation 5 of the arming gate: a policy the nest cannot read in the
    /// canonical shape is REFUSED, never guessed at. A `keep_*` JSON
    /// written into this same column by a writer disagreeing with the canonical
    /// shape is left in place (`backup-restore.md` § 8 → *The off-shape
    /// at-rest residual*), so this is a live at-rest shape, not a hypothetical
    /// — and guessing at it would prune against a number the user never chose.
    #[tokio::test]
    async fn an_unreadable_retention_policy_prunes_nothing() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let actor_id = [8u8; 32];

        let fs_id = db.create_folder("off-shape", &actor_id).await.unwrap();
        for ts in [1000, 2000, 3000, 4000, 5000] {
            db.insert_snapshot_at(fs_id, ts).await.unwrap();
        }
        set_retention(
            &db,
            "off-shape",
            &actor_id,
            r#"{"keep_last":1,"keep_daily":7}"#,
        )
        .await;

        let svc = Arc::new(
            BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
        );
        GcScheduler::new(svc, post_segments(&tmp), Duration::from_secs(3600), 0)
            .run_gc_cycle()
            .await
            .unwrap();

        assert!(
            db.list_snapshots(fs_id)
                .await
                .unwrap()
                .iter()
                .all(|s| !s.deletion_pending && !s.soft_deleted),
            "an off-shape policy must keep everything"
        );
    }

    /// The hard floor (`backup-restore.md` § 7 Layer 1 / § 8) survives the
    /// sweep: a user who asks to keep one snapshot still keeps three. The
    /// engine-level pin lives in `retention.rs`; this one proves the floor is
    /// actually reached through the production sweep, not merely available.
    #[tokio::test]
    async fn the_gc_sweep_never_schedules_below_the_hard_floor() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let actor_id = [9u8; 32];

        let fs_id = db.create_folder("floor-test", &actor_id).await.unwrap();
        for ts in [1000, 2000, 3000, 4000, 5000] {
            db.insert_snapshot_at(fs_id, ts).await.unwrap();
        }
        set_retention(&db, "floor-test", &actor_id, &policy_json(1, 0)).await;

        let svc = Arc::new(
            BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
        );
        GcScheduler::new(svc, post_segments(&tmp), Duration::from_secs(3600), 0)
            .run_gc_cycle()
            .await
            .unwrap();

        let surviving = db
            .list_snapshots(fs_id)
            .await
            .unwrap()
            .iter()
            .filter(|s| !s.deletion_pending && !s.soft_deleted)
            .count();
        assert_eq!(
            surviving,
            crate::backup::retention::SNAPSHOT_HARD_FLOOR,
            "keep_1 still leaves the floor of 3"
        );
    }

    /// Idempotence, and the reason the candidate population excludes
    /// `deletion_pending` rows: a second sweep inside the 7-day window must not
    /// queue the same snapshots again. Without the filter each GC cycle would
    /// mint a duplicate pending action forever, and the user's single cancel
    /// gesture would not reach them.
    #[tokio::test]
    async fn a_second_sweep_inside_the_window_queues_nothing_new() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let actor_id = [10u8; 32];

        let fs_id = db.create_folder("idempotent", &actor_id).await.unwrap();
        for ts in [1000, 2000, 3000, 4000, 5000] {
            db.insert_snapshot_at(fs_id, ts).await.unwrap();
        }
        set_retention(&db, "idempotent", &actor_id, &policy_json(3, 0)).await;

        let svc = Arc::new(
            BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap(),
        );
        let scheduler = GcScheduler::new(svc, post_segments(&tmp), Duration::from_secs(3600), 0);
        scheduler.run_gc_cycle().await.unwrap();
        scheduler.run_gc_cycle().await.unwrap();

        let actions = db.list_pending_actions_for_actor(&actor_id).await.unwrap();
        assert_eq!(
            actions.len(),
            1,
            "two sweeps, one pending action — the second found no fresh candidates"
        );
    }
}
