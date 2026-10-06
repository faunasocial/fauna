package com.fauna.app.service

import android.content.Context
import androidx.hilt.work.HiltWorker
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import com.fauna.app.core.PhotoBackupEngine
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.ShellLog
import dagger.assisted.Assisted
import dagger.assisted.AssistedInject
import java.util.concurrent.TimeUnit

/**
 * The background trigger for photo backup — the periodic sibling of the
 * `photo-backup-sync-now-button` one-shot [SyncService].
 *
 * Photos are a **folder** with "one-way, continuous ingress"
 * (`docs/goal/ui/folders.md` § Photo backup); until this worker landed, the
 * only thing that ever called [PhotoBackupEngine.syncNewPhotos] was that manual
 * button, so `photo-backup-enable-toggle` ("Back up photos") persisted an
 * `autoPhotoBackup` flag that no automatic pass ever read. This is the pass.
 *
 * **The period is a constant** — phase 5's de-knob (`file-sync.md` § Config)
 * retired the per-folder scan-frequency choice, so the ingress cadence is the
 * shared 300 s reconcile constant, which WorkManager's 15-minute periodic
 * floor absorbs (today's default behavior exactly; the surviving user-facing
 * time control is the nest place's snapshot policy). The row-read +
 * reschedule-per-pass loop this worker used to run is deleted with the knob.
 *
 * Like [CustodianHostWorker], which has always run a flat 15 min period
 * (`backup-restore.md` § Background Tasks).
 *
 * The unmetered-network constraint is the scheduling-time twin of
 * [com.fauna.app.core.NetworkMonitor.shouldSyncPhotos]'s per-file gate, which
 * still runs inside the pass — the constraint keeps the OS from waking us on
 * cellular at all, the gate stops an in-flight pass when Wi-Fi drops mid-way.
 */
@HiltWorker
class PhotoBackupWorker @AssistedInject constructor(
    @Assisted context: Context,
    @Assisted params: WorkerParameters,
    private val photoBackupEngine: PhotoBackupEngine,
    private val secureStorage: SecureStorage,
) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result {
        // The user turned "Back up photos" off. Stay enqueued (cheap, and the
        // toggle can come back on without an app restart) but do nothing.
        if (!secureStorage.autoPhotoBackup) return Result.success()

        // No device id yet (pre-onboarding), or WorkManager woke us in a fresh
        // background process before login — nothing this device can back up
        // this cycle; the OS retries on the next slice.
        if (secureStorage.deviceId == null) return Result.success()

        return try {
            // Idempotent by construction: the pass re-scans MediaStore and skips
            // anything the per-asset dedup ledger already recorded, so a slice
            // the OS kills mid-way simply resumes next time.
            photoBackupEngine.syncNewPhotos()
            Result.success()
        } catch (e: Exception) {
            ShellLog.w("PhotoBackupWorker", "photo backup pass failed: ${e.message}")
            Result.retry()
        }
    }

    companion object {
        private const val UNIQUE_WORK_NAME = "fauna_photo_library_backup"

        /**
         * WorkManager's minimum periodic interval
         * ([androidx.work.PeriodicWorkRequest.MIN_PERIODIC_INTERVAL_MILLIS]) —
         * the effective period: the constant 300 s reconcile cadence sits
         * below it and rounds up (phase 5's de-knob; today's default behavior
         * exactly).
         */
        private const val MIN_PERIOD_MINUTES = 15L

        /**
         * Enqueue (or re-period) the recurring photo-backup pass at the
         * constant period.
         *
         * [ExistingPeriodicWorkPolicy.UPDATE] rather than `KEEP`, kept on
         * purpose through the de-knob: a device that scheduled a longer
         * period under the retired row-driven cadence re-periods to the
         * constant on its next app start, instead of keeping the stale
         * schedule forever. UPDATE keeps the running schedule when the period
         * is unchanged, so the app-start call is still idempotent.
         */
        fun enqueuePeriodicWork(context: Context) {
            val request = PeriodicWorkRequestBuilder<PhotoBackupWorker>(
                MIN_PERIOD_MINUTES, TimeUnit.MINUTES,
            ).setConstraints(
                Constraints.Builder()
                    // Wi-Fi-only, matching NetworkMonitor.shouldSyncPhotos() —
                    // photo backup never spends the user's cellular data.
                    .setRequiredNetworkType(NetworkType.UNMETERED)
                    .build(),
            ).build()

            WorkManager.getInstance(context).enqueueUniquePeriodicWork(
                UNIQUE_WORK_NAME,
                ExistingPeriodicWorkPolicy.UPDATE,
                request,
            )
        }
    }
}
