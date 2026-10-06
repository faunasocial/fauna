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
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.ShellLog
import dagger.assisted.Assisted
import dagger.assisted.AssistedInject
import java.util.concurrent.TimeUnit

/**
 * Android's background trigger for hosting this device's **client-device backup
 * custodian** — slice 3d's mobile arm (`docs/goal/ui/backups.md` § Third
 * destination kind; `docs/goal/behavior/backup-restore.md` § Background Tasks).
 *
 * The *pull* direction — and, since the slice-5 flip deleted the in-app upload
 * driver, the only backup direction an app drives at all (the source nest is the
 * segment-backup writer). Every moving part is
 * shared Rust — the pull, the store, the custody policy, and *which registry row
 * is mine* — reached through the `FfiCustodianHost` UniFFI handle
 * (`libs/fauna-ffi/src/custodian_host.rs`). Nothing here decides anything the
 * desktop host (`bins/fauna-sync-agent/src/custodian.rs`) decides differently.
 *
 * **Why a worker at all, when desktop uses a daemon.** A backup that only
 * advances while the user has the app open is not a backup, and android has no
 * per-user agent to put the host in. The OS scheduler is the phone's substitute,
 * which is why the FFI exports `runAllKinds()` — one pass per wake — and
 * deliberately exports **no** `startForever`: a forever-loop here would double
 * the period WorkManager already owns.
 *
 * **Not an enrolled custodian → clean success no-op.** [ApiClient
 * .buildCustodianHost] returns null when the source nest's destination registry
 * holds no row for this device, which is the ordinary state of most devices.
 * That is a successful pass, not a retry: nothing about waking again sooner
 * would produce a row.
 *
 * **Construct-run-drop per job:** the handle owns a
 * sealed store and a live pull driver, so it is built fresh in [doWork] and
 * `close()`d in `finally`. This worker holds nothing between passes. The
 * low-latency *foreground* wake is the sibling [CustodianPushKick], which holds
 * one host for the foreground session and runs the shared push-debounce loop;
 * the two are complementary — WorkManager owns the background period, the holder
 * owns the foreground wake.
 */
@HiltWorker
class CustodianHostWorker @AssistedInject constructor(
    @Assisted context: Context,
    @Assisted params: WorkerParameters,
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
    private val accountStores: AccountStores,
) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result {
        // No device id yet (pre-onboarding) → this device cannot be the subject
        // of a registry row, so there is nothing to match on.
        val deviceId = secureStorage.deviceId ?: return Result.success()

        // Not connected / not authenticated (WorkManager woke us in a fresh
        // background process before login), or the registry read failed because
        // the phone is offline. Both are ordinary and both retry on the next
        // scheduled slice — a custodian that gave up after one bad poll would be
        // exactly the silent stop the check-in surface exists to expose.
        val host = try {
            api.buildCustodianHost(deviceId, accountStores.custodianStoreBaseDir())
        } catch (e: Exception) {
            ShellLog.w(TAG, "build custodian host failed: ${e.message}")
            return Result.retry()
        } ?: return Result.success()

        return try {
            val summary = host.runAllKinds()
            // `runAllKinds` never fails as a whole — a per-kind failure is logged
            // in Rust and the pass continues — so the summary is how this trigger
            // learns what happened. cap_reached comes from the pass's own verdict,
            // never from held >= cap: a pass that stops AT its cap ends below it.
            ShellLog.d(
                TAG,
                "custodian pass: stored=${summary.storedSegments} " +
                    "tombstoned=${summary.tombstonedSegments} " +
                    "held=${summary.heldBytes} capReached=${summary.capReached}",
            )
            Result.success()
        } catch (e: Exception) {
            ShellLog.w(TAG, "custodian pass failed: ${e.message}")
            Result.retry()
        } finally {
            host.close()
        }
    }

    companion object {
        private const val TAG = "CustodianHostWorker"
        private const val UNIQUE_WORK_NAME = "fauna_custodian_host"

        /**
         * Enqueue the periodic custodian pull pass. 15 min is WorkManager's
         * minimum period and the cadence `ui/backups.md` § Scheduling settles
         * for every destination tuple — the same one the desktop driver uses,
         * taken by reference rather than re-chosen here.
         * [ExistingPeriodicWorkPolicy.KEEP] so re-running this at every app start
         * does not reset the schedule.
         *
         * Enqueued unconditionally: the pass no-ops until
         * this device is enrolled, so there is no enrollment signal to wait for
         * and no re-enqueue owed when one arrives.
         */
        fun enqueuePeriodicWork(context: Context) {
            val request = PeriodicWorkRequestBuilder<CustodianHostWorker>(
                15, TimeUnit.MINUTES,
            ).setConstraints(
                Constraints.Builder()
                    // Every pass both reads the registry and fetches segments
                    // over the network; without a live connection the wake can
                    // only fail.
                    .setRequiredNetworkType(NetworkType.CONNECTED)
                    .build(),
            ).build()

            WorkManager.getInstance(context).enqueueUniquePeriodicWork(
                UNIQUE_WORK_NAME,
                ExistingPeriodicWorkPolicy.KEEP,
                request,
            )
        }
    }
}
