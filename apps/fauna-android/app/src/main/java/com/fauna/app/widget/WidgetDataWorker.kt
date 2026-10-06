package com.fauna.app.widget

import android.content.Context
import androidx.glance.appwidget.GlanceAppWidgetManager
import androidx.glance.appwidget.state.updateAppWidgetState
import androidx.hilt.work.HiltWorker
import androidx.work.*
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ShellLog
import dagger.assisted.Assisted
import dagger.assisted.AssistedInject
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import java.util.concurrent.TimeUnit

@HiltWorker
class WidgetDataWorker @AssistedInject constructor(
    @Assisted context: Context,
    @Assisted params: WorkerParameters,
    private val api: ApiClient
) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result {
        return try {
            val count = try {
                api.fetchInbox().size
            } catch (_: Exception) {
                0
            }

            val manager = GlanceAppWidgetManager(applicationContext)
            val glanceIds = manager.getGlanceIds(FaunaWidget::class.java)
            for (glanceId in glanceIds) {
                updateAppWidgetState(applicationContext, glanceId) { prefs ->
                    prefs[UNREAD_COUNT_KEY] = count
                }
                FaunaWidget().update(applicationContext, glanceId)
            }
            Result.success()
        } catch (_: Exception) {
            Result.retry()
        }
    }

    companion object {
        /**
         * Re-fold the widget's unread badge onto the account that is active now
         * (`account-scoping.md` § The scoping taxonomy — the count is a class-4
         * nest-authoritative replica: wipe-tolerant, since `fetch_inbox` re-derives
         * it, but still account-scoped, so leaving the outgoing account's number on
         * the home screen is the same cross-account read as rendering its rows).
         *
         * Zero it synchronously rather than waiting for the refresh: the incoming
         * account's count is not knowable until the nest answers, and a stale
         * number is worse than none. The one-shot then fills it in; if the app is
         * offline the worker's own catch leaves it at zero and the 15-minute
         * periodic pass corrects it later.
         */
        suspend fun refold(context: Context) {
            val manager = GlanceAppWidgetManager(context)
            for (glanceId in manager.getGlanceIds(FaunaWidget::class.java)) {
                updateAppWidgetState(context, glanceId) { prefs -> prefs[UNREAD_COUNT_KEY] = 0 }
                FaunaWidget().update(context, glanceId)
            }
        }

        /**
         * The switch/sign-out entry point: clear the badge, then queue one refresh.
         * Fire-and-forget — a switch must not block on the widget, and every step
         * is individually recoverable by the periodic pass.
         */
        fun refoldForAccountSwitch(context: Context) {
            val appContext = context.applicationContext
            CoroutineScope(Dispatchers.IO).launch {
                runCatching { refold(appContext) }
            }
            // Individually recoverable like the zero-first refold above (the
            // periodic pass corrects either half later) — but unlike it, this
            // call was UNGUARDED: a switch/sign-out must not block on the
            // widget, yet `WorkManager.getInstance` (uninitialized, mid-boot,
            // or a caller whose Context is not fully wired) could throw
            // synchronously and take the caller's whole teardown coroutine
            // down with it, which is precisely the "must not block on the
            // widget" contract this function documents for itself.
            runCatching {
                WorkManager.getInstance(appContext).enqueue(
                    OneTimeWorkRequestBuilder<WidgetDataWorker>().setConstraints(
                        Constraints.Builder()
                            .setRequiredNetworkType(NetworkType.CONNECTED)
                            .build()
                    ).build()
                )
            }.onFailure { ShellLog.w("WidgetDataWorker", "refold enqueue failed: ${it.message}") }
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
