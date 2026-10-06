//! Background snapshot scheduler.
//!
//! Periodically checks all folders for new changes and auto-creates
//! snapshots after a configurable quiet period.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use anyhow::Result;

use crate::backup::service::BackupService;

/// Minimum seconds between full-DB hot-copy backups. The scheduler ticks every
/// ~60s for folder snapshots, but the DB hot-copy is gated to this much longer
/// cadence: running it EVERY tick (and never pruning) wrote a fresh ~8 MB DB copy
/// every 60s to the blob store and filled example.com's disk to 68 GB.
/// Hourly + keep-last-N retention (`KEEP_SQLITE_BACKUPS`) keeps the self-backup
/// store bounded. See `docs/goal/behavior/backup-restore.md` § Self-backup retention.
const DB_BACKUP_INTERVAL_SECS: i64 = 3600;

/// How often the scheduler wakes to look for folders owing an automatic
/// snapshot. Named here rather than spelled at the one call site so the
/// test-hook pass ([`crate::snapshot_scheduler_test_hook`]) runs the
/// **production** cadence instead of a second, drifting copy of it.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// Seconds of quiet after a folder's last change before the nest cuts a
/// snapshot of it — for every folder whose owner has not chosen their own
/// (`folders.nest_snapshot_quiet_secs`, the three-state per-folder override
/// `CacheDb::folder_needs_snapshot` resolves against this). Same reason as
/// [`CHECK_INTERVAL`] for living here.
pub const DEFAULT_QUIET_SECS: i64 = 30;

pub struct SnapshotScheduler {
    backup_service: Arc<BackupService>,
    check_interval: Duration,
    quiet_secs: i64,
    running: AtomicBool,
    /// Unix timestamp of last DB hot-copy backup; -1 means never run.
    last_db_backup: AtomicI64,
    /// Unix timestamp of last logical dump; -1 means never run.
    last_logical_dump: AtomicI64,
}

impl SnapshotScheduler {
    pub fn new(
        backup_service: Arc<BackupService>,
        check_interval: Duration,
        quiet_secs: i64,
    ) -> Self {
        Self {
            backup_service,
            check_interval,
            quiet_secs,
            running: AtomicBool::new(false),
            last_db_backup: AtomicI64::new(-1),
            last_logical_dump: AtomicI64::new(-1),
        }
    }

    /// Spawn the background scheduler task. Returns a JoinHandle.
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.check_interval);
            loop {
                interval.tick().await;
                if self.running.swap(true, Ordering::SeqCst) {
                    tracing::debug!("snapshot scheduler: previous run still active, skipping");
                    continue;
                }
                if let Err(e) = self.check_all_folders().await {
                    tracing::error!(error = %e, "snapshot scheduler error");
                }

                let now = fauna_core::data::Timestamp::now_secs();

                // Hot-copy database backup — gated to DB_BACKUP_INTERVAL_SECS
                // (hourly), NOT every 60s tick. Each backup self-prunes to
                // KEEP_SQLITE_BACKUPS, so the self-backup store stays bounded.
                // (every-60s + never-pruned filled the disk to 68 GB.)
                let last_db = self.last_db_backup.load(Ordering::SeqCst);
                if last_db < 0 || now - last_db >= DB_BACKUP_INTERVAL_SECS {
                    match self.backup_service.backup_database().await {
                        Ok(_) => self.last_db_backup.store(now, Ordering::SeqCst),
                        Err(e) => tracing::error!("database backup failed: {e:#}"),
                    }
                }

                // Daily logical dump
                let last = self.last_logical_dump.load(Ordering::SeqCst);
                let should_dump = last < 0 || now - last >= 86400;
                if should_dump {
                    match self.backup_service.logical_dump().await {
                        Ok(_) => {
                            self.last_logical_dump.store(now, Ordering::SeqCst);
                            tracing::info!("logical dump completed");
                        }
                        Err(e) => tracing::error!("logical dump failed: {e:#}"),
                    }
                }

                self.running.store(false, Ordering::SeqCst);
            }
        })
    }

    /// One pass over every folder: snapshot each that owes one, then auto-prune
    /// it. Returns how many automatic snapshots this pass cut.
    ///
    /// `pub` for the `test-hooks` route that runs exactly one pass on demand
    /// ([`crate::snapshot_scheduler_test_hook`]): the production tick is a
    /// 60-second loop, so a tier_3 witness of "a folder you changed and then
    /// left alone gets a snapshot by itself" would otherwise have to wait out a
    /// wall clock — which `e2e-conventions.md` convention 14 forbids.
    pub async fn check_all_folders(&self) -> Result<usize> {
        let db = self.backup_service.db();
        let folders = db.list_folders().await?;
        let mut created = 0usize;
        for fs in &folders {
            if db.folder_needs_snapshot(fs.id, self.quiet_secs).await? {
                let parent_id = db
                    .list_snapshots(fs.id)
                    .await
                    .ok()
                    .and_then(|snaps| snaps.first().map(|s| s.id));

                // Untagged by construction (the scheduler names nothing), so there
                // is no display copy to seal — and this nest-side writer holds no
                // key that could seal one anyway.
                match db
                    .create_snapshot_v2(fs.id, None, &[], None, parent_id)
                    .await
                {
                    Ok(snap) => {
                        created += 1;
                        tracing::info!(
                            folder = %fauna_core::log_redact::log_folder_name(&fs.name),
                            snapshot_id = snap.id,
                            files = snap.file_count,
                            "auto-snapshot created"
                        );
                        if let Err(e) = self.auto_prune_folder(fs.id).await {
                            tracing::warn!(
                                folder = %fauna_core::log_redact::log_folder_name(&fs.name),
                                error = %e,
                                "auto-prune failed"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            folder = %fauna_core::log_redact::log_folder_name(&fs.name),
                            error = %e,
                            "auto-snapshot failed"
                        );
                    }
                }
            }
        }
        Ok(created)
    }

    /// Apply auto-prune for a folder, per the set's own resting
    /// `retention_policy` column — the authoritative copy ruled 2026-08-01
    /// (`docs/goal/behavior/backup-restore.md` § 8 RULING). It **schedules**
    /// through § 7's deletion-safety layers and never deletes; the mechanism,
    /// and why both auto-prune consumers share one implementation of it, is on
    /// [`crate::backup::prune::schedule_auto_prune`].
    ///
    /// ⚠ This deliberately reads no client-sealed state. Account state rests
    /// sealed under keys the nest never holds, so such a read could only ever
    /// come back empty — and the step that would "fix" it (handing the nest the
    /// client's key) inverts the sealing model. Do not reintroduce it.
    pub async fn auto_prune_folder(&self, folder_id: i64) -> anyhow::Result<()> {
        let db = self.backup_service.db();
        let Some(fs) = db.get_folder_by_id(folder_id).await? else {
            return Ok(());
        };
        crate::backup::prune::schedule_auto_prune(db, &fs).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    fn make_svc(db: Arc<CacheDb>, tmp: &std::path::Path) -> Arc<BackupService> {
        Arc::new(BackupService::new(db, None, false, tmp.to_path_buf(), None).unwrap())
    }

    #[tokio::test]
    async fn scheduler_creates_snapshot_for_new_changes() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        db.create_folder("sched-test", &[1u8; 32]).await.unwrap();
        let fs = db.get_folder("sched-test").await.unwrap().unwrap();

        let ph: [u8; 32] = *blake3::hash(b"photo.jpg").as_bytes();
        db.record_sync_change(
            &[1u8; 32],
            &ph,
            Some(&[0xAAu8; 32]),
            100,
            "create",
            Some(fs.id),
            None,
            Some("photo.jpg"),
        )
        .await
        .unwrap();

        let svc = make_svc(db.clone(), tmp.path());
        let scheduler = SnapshotScheduler::new(svc, Duration::from_secs(60), 0);
        scheduler.check_all_folders().await.unwrap();

        let snaps = db.list_snapshots(fs.id).await.unwrap();
        assert_eq!(snaps.len(), 1, "scheduler should have created a snapshot");
    }

    /// An ordinary Backup folder's records ride the `sync_changes` head feed
    /// since the phase 3 head unification (2026-08-17) — the scheduler
    /// auto-snapshots it from the same plane as every other folder.
    #[tokio::test]
    async fn scheduler_creates_snapshot_for_backup_folder_from_the_head_feed() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let actor = [1u8; 32];
        db.create_folder_with_options("photo-library", &actor, crate::db::FolderOptions::default())
            .await
            .unwrap();
        let fs = db.get_folder("photo-library").await.unwrap().unwrap();

        let ph: [u8; 32] = *blake3::hash(b"IMG_0001.heic").as_bytes();
        db.record_sync_change(
            &actor,
            &ph,
            Some(&[0xAAu8; 32]),
            2048,
            "create",
            Some(fs.id),
            None,
            Some("IMG_0001.heic"),
        )
        .await
        .unwrap();

        let svc = make_svc(db.clone(), tmp.path());
        let scheduler = SnapshotScheduler::new(svc, Duration::from_secs(60), 0);
        scheduler.check_all_folders().await.unwrap();

        let snaps = db.list_snapshots(fs.id).await.unwrap();
        assert_eq!(
            snaps.len(),
            1,
            "scheduler should auto-snapshot a Backup folder from the head feed"
        );
        assert_eq!(
            snaps[0].file_count, 1,
            "the auto-snapshot must capture the recorded path, not be empty"
        );
    }

    /// A reserved (`__`) backup **destination** set (custodian-held custody
    /// for another location's data) is never auto-snapshotted — snapshot pins
    /// would defeat the custodian's latest-per-path reclamation
    /// (`message-segment-store.md` § GC-safety).
    #[tokio::test]
    async fn scheduler_skips_reserved_backup_destination_set() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let actor = [1u8; 32];
        db.create_folder_with_options(
            "__mail",
            &actor,
            crate::db::FolderOptions {
                custody_copy: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let fs = db.get_folder("__mail").await.unwrap().unwrap();

        let ph: [u8; 32] = *blake3::hash(b"seg-00000001.dat").as_bytes();
        db.upsert_backup_custody(
            &actor,
            fs.id,
            &ph,
            Some("seg-00000001.dat"),
            &[0xBBu8; 32],
            4096,
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap();

        let svc = make_svc(db.clone(), tmp.path());
        let scheduler = SnapshotScheduler::new(svc, Duration::from_secs(60), 0);
        scheduler.check_all_folders().await.unwrap();

        let snaps = db.list_snapshots(fs.id).await.unwrap();
        assert!(
            snaps.is_empty(),
            "a reserved backup destination set must never be auto-snapshotted"
        );
    }

    /// The nest place's `snapshots` property is the **user's** knob, and it is
    /// off-by-choice, never off-by-default: the folder below has an unsnapshotted
    /// change and would be snapshotted on the very next tick but for the explicit
    /// `false` (folders re-model phase 2 § Places → the nest place;
    /// `backup-restore.md` § 8 *The nest place's snapshot policy*).
    #[tokio::test]
    async fn scheduler_skips_a_folder_whose_nest_place_keeps_no_snapshots() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let actor = [1u8; 32];
        db.create_folder("no-snapshots", &actor).await.unwrap();
        let fs = db.get_folder("no-snapshots").await.unwrap().unwrap();

        let ph: [u8; 32] = *blake3::hash(b"paper.txt").as_bytes();
        db.record_sync_change(
            &actor,
            &ph,
            Some(&[0xAAu8; 32]),
            100,
            "create",
            Some(fs.id),
            None,
            Some("paper.txt"),
        )
        .await
        .unwrap();

        db.update_folder_for_user(
            "no-snapshots",
            &actor,
            crate::db::FolderUpdate {
                nest_snapshots: Some(Some(false)),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let svc = make_svc(db.clone(), tmp.path());
        let scheduler = SnapshotScheduler::new(svc, Duration::from_secs(60), 0);
        scheduler.check_all_folders().await.unwrap();

        assert!(
            db.list_snapshots(fs.id).await.unwrap().is_empty(),
            "a folder whose nest place keeps no snapshots must not be auto-snapshotted"
        );
    }

    /// Turning the knob back to *unset* restores the nest-wide behavior — the
    /// column's three states are on / off / "nothing authoritative said", and the
    /// third one is the only honest resting value for every folder whose
    /// owner never chose.
    #[tokio::test]
    async fn clearing_the_snapshots_knob_restores_the_nest_wide_default() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let actor = [1u8; 32];
        db.create_folder("re-armed", &actor).await.unwrap();
        let fs = db.get_folder("re-armed").await.unwrap().unwrap();

        let ph: [u8; 32] = *blake3::hash(b"paper.txt").as_bytes();
        db.record_sync_change(
            &actor,
            &ph,
            Some(&[0xAAu8; 32]),
            100,
            "create",
            Some(fs.id),
            None,
            Some("paper.txt"),
        )
        .await
        .unwrap();

        for value in [Some(false), None] {
            db.update_folder_for_user(
                "re-armed",
                &actor,
                crate::db::FolderUpdate {
                    nest_snapshots: Some(value),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }

        let svc = make_svc(db.clone(), tmp.path());
        let scheduler = SnapshotScheduler::new(svc, Duration::from_secs(60), 0);
        scheduler.check_all_folders().await.unwrap();

        assert_eq!(
            db.list_snapshots(fs.id).await.unwrap().len(),
            1,
            "clearing the knob must return the folder to the nest-wide default (snapshot)"
        );
    }

    /// A folder's own quiet period overrides the nest-wide one. Asserted through
    /// the *state* the scheduler reaches, not a wall-clock wait: the change was
    /// recorded now, the nest-wide quiet is 0 (snapshot immediately), and the
    /// folder's own quiet is a full day — so the only latency-independent verdict
    /// is "no snapshot yet".
    #[tokio::test]
    async fn a_folders_own_quiet_period_overrides_the_nest_wide_one() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let actor = [1u8; 32];
        db.create_folder("slow-cadence", &actor).await.unwrap();
        let fs = db.get_folder("slow-cadence").await.unwrap().unwrap();

        let ph: [u8; 32] = *blake3::hash(b"draft.md").as_bytes();
        db.record_sync_change(
            &actor,
            &ph,
            Some(&[0xAAu8; 32]),
            100,
            "create",
            Some(fs.id),
            None,
            Some("draft.md"),
        )
        .await
        .unwrap();

        db.update_folder_for_user(
            "slow-cadence",
            &actor,
            crate::db::FolderUpdate {
                nest_snapshot_quiet_secs: Some(Some(86_400)),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let svc = make_svc(db.clone(), tmp.path());
        let scheduler = SnapshotScheduler::new(svc, Duration::from_secs(60), 0);
        scheduler.check_all_folders().await.unwrap();

        assert!(
            db.list_snapshots(fs.id).await.unwrap().is_empty(),
            "the folder's own quiet period must win over the nest-wide 0"
        );
    }

    #[tokio::test]
    async fn scheduler_skips_already_covered_folder() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        db.create_folder("covered", &[1u8; 32]).await.unwrap();
        let fs = db.get_folder("covered").await.unwrap().unwrap();

        let ph: [u8; 32] = *blake3::hash(b"doc.txt").as_bytes();
        db.record_sync_change(
            &[1u8; 32],
            &ph,
            Some(&[0xBBu8; 32]),
            50,
            "create",
            Some(fs.id),
            None,
            Some("doc.txt"),
        )
        .await
        .unwrap();

        // Manually create snapshot first
        db.create_snapshot_v2(fs.id, None, &[], None, None)
            .await
            .unwrap();

        let svc = make_svc(db.clone(), tmp.path());
        let scheduler = SnapshotScheduler::new(svc, Duration::from_secs(60), 0);
        scheduler.check_all_folders().await.unwrap();

        let snaps = db.list_snapshots(fs.id).await.unwrap();
        assert_eq!(
            snaps.len(),
            1,
            "scheduler should not create duplicate snapshot"
        );
    }

    #[tokio::test]
    async fn scheduler_respects_quiet_period() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        db.create_folder("quiet", &[1u8; 32]).await.unwrap();
        let fs = db.get_folder("quiet").await.unwrap().unwrap();

        let ph: [u8; 32] = *blake3::hash(b"new.txt").as_bytes();
        db.record_sync_change(
            &[1u8; 32],
            &ph,
            Some(&[0xCCu8; 32]),
            100,
            "create",
            Some(fs.id),
            None,
            Some("new.txt"),
        )
        .await
        .unwrap();

        // Use a very large quiet_secs so the change is "too recent"
        let svc = make_svc(db.clone(), tmp.path());
        let scheduler = SnapshotScheduler::new(svc, Duration::from_secs(60), 999999);
        scheduler.check_all_folders().await.unwrap();

        let snaps = db.list_snapshots(fs.id).await.unwrap();
        assert_eq!(snaps.len(), 0, "quiet period not met — no snapshot created");
    }

    /// The scheduler-side arming pin (`backup-restore.md` § 8 RULING
    /// consequence (iii)): the auto-snapshot's own follow-on prune reads the
    /// set's resting `retention_policy` column and **schedules** through § 7's
    /// layers.
    ///
    /// ⚠️ Rewritten 2026-08-01, deliberately. Its predecessors
    /// (`scheduler_auto_prunes_with_retention_policy` /
    /// `scheduler_no_prune_without_config`) seeded a client-sealed config blob
    /// (the `__config` rail, since retired) and asserted on
    /// `db.delete_snapshots`' effect. Both facts they pinned are gone on
    /// purpose: the nest cannot read client-sealed state, and an automatic
    /// prune must never reach the hard-delete primitive at all.
    #[tokio::test]
    async fn auto_prune_schedules_from_the_per_set_column_without_deleting() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let actor_id = [1u8; 32];
        let tmp = tempfile::tempdir().unwrap();

        let fs_id = db.create_folder("test-prune", &actor_id).await.unwrap();
        let mut ids = Vec::new();
        for ts in [1000, 2000, 3000, 4000, 5000] {
            ids.push(db.insert_snapshot_at(fs_id, ts).await.unwrap());
        }
        db.update_folder_for_user(
            "test-prune",
            &actor_id,
            crate::db::FolderUpdate {
                retention_policy: Some(Some(
                    &serde_json::to_string(&fauna_folders_machine::state::RetentionPolicy {
                        max_snapshots: 4,
                        max_age_days: 0,
                    })
                    .unwrap(),
                )),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let svc = make_svc(db.clone(), tmp.path());
        SnapshotScheduler::new(svc, Duration::from_secs(60), 0)
            .auto_prune_folder(fs_id)
            .await
            .unwrap();

        let snaps = db.list_snapshots(fs_id).await.unwrap();
        assert_eq!(snaps.len(), 5, "scheduling must not delete any row");
        let pending: Vec<i64> = snaps
            .iter()
            .filter(|s| s.deletion_pending)
            .map(|s| s.id)
            .collect();
        assert_eq!(
            pending,
            vec![ids[0]],
            "exactly the one snapshot beyond the bound of 4 enters the window"
        );
    }

    /// The state every folder is in until a user fills in the wizard: no
    /// policy column, therefore nothing is ever scheduled. A regression here is
    /// silent, nest-wide data loss, so it is pinned on the scheduler leg as well
    /// as the GC leg.
    #[tokio::test]
    async fn auto_prune_without_a_policy_schedules_nothing() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let actor_id = [1u8; 32];
        let tmp = tempfile::tempdir().unwrap();

        let fs_id = db.create_folder("no-policy", &actor_id).await.unwrap();
        for ts in [1000, 2000, 3000, 4000, 5000] {
            db.insert_snapshot_at(fs_id, ts).await.unwrap();
        }
        assert!(
            db.get_folder_by_id(fs_id)
                .await
                .unwrap()
                .unwrap()
                .retention_policy
                .is_none(),
            "precondition: a freshly created set has no policy"
        );

        let svc = make_svc(db.clone(), tmp.path());
        SnapshotScheduler::new(svc, Duration::from_secs(60), 0)
            .auto_prune_folder(fs_id)
            .await
            .unwrap();

        let snaps = db.list_snapshots(fs_id).await.unwrap();
        assert_eq!(snaps.len(), 5);
        assert!(
            snaps.iter().all(|s| !s.deletion_pending && !s.soft_deleted),
            "no policy = keep everything"
        );
    }
}
