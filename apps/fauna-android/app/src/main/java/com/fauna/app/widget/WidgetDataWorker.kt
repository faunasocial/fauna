package com.fauna.app.widget

import android.content.Context
import androidx.hilt.work.HiltWorker
import androidx.work.*
import com.fauna.app.core.ShellLog
import com.fauna.app.core.conversations.ConversationsManagerHost
import dagger.assisted.Assisted
import dagger.assisted.AssistedInject
import java.util.concurrent.TimeUnit

/**
 * The widget's background refresh (`apps/android.md` § App Widgets): one
 * conversations receive pass, whose ingest ticks the manager observer that
 * publishes the count — the same path a foreground arrival takes, and the iOS
 * `BackgroundScheduler.runWidgetRefreshPass` twin. It never computes a count
 * of its own: with a live session it hands back the shared fold the pass left
 * behind; in a process the OS started cold (no session) it repaints the last
 * published count ([ConversationsManagerHost.widgetRefreshPass]).
 */
@HiltWorker
class WidgetDataWorker @AssistedInject constructor(
    @Assisted context: Context,
    @Assisted params: WorkerParameters,
    private val conversations: ConversationsManagerHost,
) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result {
        WidgetRefreshPasses.run { conversations.widgetRefreshPass() }
        return Result.success()
    }

    companion object {
        /**
         * Re-fold the widget's unread badge onto the account that is active now
         * (`account-scoping.md` § The scoping taxonomy — the count is a class-4
         * replica: wipe-tolerant, since the incoming account's conversations fold
         * re-derives it, but still account-scoped, so leaving the outgoing
         * account's number on the home screen is the same cross-account read as
         * rendering its rows).
         *
         * Zero it now rather than waiting for the incoming session: its count is
         * not knowable until its conversations restore, and a stale number is
         * worse than none. The session's first snapshot tick publishes the real
         * count; the one-shot pass below and the 15-minute periodic one are the
         * backstops.
         */
        fun refold(context: Context) {
            WidgetUnreadPublisher(context.applicationContext).clear()
        }

        /**
         * The switch/sign-out entry point: clear the badge, then queue one refresh.
         * Fire-and-forget — a switch must not block on the widget, and every step
         * is individually recoverable by the periodic pass.
         */
        fun refoldForAccountSwitch(context: Context) {
            val appContext = context.applicationContext
            runCatching { refold(appContext) }
                .onFailure { ShellLog.w("WidgetDataWorker", "refold failed: ${it.message}") }
            // Individually recoverable like the zero-first refold above (the
            // periodic pass corrects either half later) — but unlike it, this
            // call was UNGUARDED: a switch/sign-out must not block on the
            // widget, yet `WorkManager.getInstance` (uninitialized, mid-boot,
            // or a caller whose Context is not fully wired) could throw
            // synchronously and take the caller's whole teardown coroutine
            // down with it, which is precisely the "must not block on the
            // widget" contract this function documents for itself.
            runCatching { enqueueOneShot(appContext) }
                .onFailure { ShellLog.w("WidgetDataWorker", "refold enqueue failed: ${it.message}") }
        }

        /**
         * One pass now, through WorkManager — the e2e poke
         * `WIDGET_REFRESH_SCHEDULED_PASS_NOW` (convention 14's `run_now`) runs
         * this, so the poke and the schedule construct and run the identical
         * worker.
         */
        fun enqueueOneShot(context: Context) {
            WorkManager.getInstance(context).enqueue(
                OneTimeWorkRequestBuilder<WidgetDataWorker>().setConstraints(
                    Constraints.Builder()
                        .setRequiredNetworkType(NetworkType.CONNECTED)
                        .build()
                ).build()
            )
        }

        fun enqueuePeriodicWork(context: Context) {
            val request = PeriodicWorkRequestBuilder<WidgetDataWorker>(
                15, TimeUnit.MINUTES
            ).setConstraints(
                Constraints.Builder()
                    .setRequiredNetworkType(NetworkType.CONNECTED)
                    .build()
            ).build()

            WorkManager.getInstance(context).enqueueUniquePeriodicWork(
                "fauna_widget_refresh",
                ExistingPeriodicWorkPolicy.KEEP,
                request
            )
        }
    }
}
