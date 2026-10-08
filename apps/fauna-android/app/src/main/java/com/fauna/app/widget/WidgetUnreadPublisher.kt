package com.fauna.app.widget

import android.content.Context
import com.fauna.app.core.ShellLog
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone
import java.util.concurrent.Executors
import javax.inject.Inject

/**
 * The file the app publishes for its home-screen widget: `unread.json` —
 * `{"count": <int>, "updatedAt": "<iso8601>"}`, the apple `UnreadSnapshotStore`'s
 * format, so one e2e helper reads every app's snapshot
 * (`tests/e2e-unified/helpers/home_screen_widget.py`). It lives in the app's
 * private `files/widget/`; the widget renders from it, and the e2e witness reads
 * it from outside the app through `adb run-as`.
 */
class UnreadSnapshotStore(private val dir: File) {

    private val file get() = File(dir, FILE_NAME)

    /** The count last published, or null when none is (first run, or cleared). */
    fun load(): Int? = try {
        file.takeIf { it.exists() }?.readText()?.let { JSONObject(it).getInt("count") }
    } catch (e: Exception) {
        ShellLog.w(TAG, "unread snapshot unreadable: ${e.message}")
        null
    }

    /** Write atomically (temp file + rename): a reader never sees half a snapshot. */
    fun save(count: Int, updatedAtMillis: Long) {
        dir.mkdirs()
        val iso = SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss'Z'", Locale.US)
            .apply { timeZone = TimeZone.getTimeZone("UTC") }
            .format(Date(updatedAtMillis))
        val tmp = File(dir, "$FILE_NAME.tmp")
        tmp.writeText(JSONObject().put("count", count).put("updatedAt", iso).toString())
        if (!tmp.renameTo(file)) {
            tmp.delete()
            error("rename onto $file failed")
        }
    }

    fun clear() {
        file.delete()
    }

    companion object {
        const val FILE_NAME = "unread.json"
        private const val DIR_NAME = "widget"
        private const val TAG = "FaunaWidget"

        fun forApp(context: Context) = UnreadSnapshotStore(File(context.filesDir, DIR_NAME))
    }
}

/**
 * Publishes the home-screen widget's unread count: the android half of
 * `apps/common.md` § Home-screen widget, mechanism in `apps/android.md`
 * § App Widgets.
 *
 * **The number is the conversations list's own number.** [ConversationsManagerHost]'s
 * manager observer — the one place every render of the list reads through —
 * calls [publish] with the shared `ConversationsManager.unreadTotal()` fold on
 * every snapshot tick: linux's tray and launcher badge and apple's
 * `WidgetUnreadPublisher` read the same getter, so the widget can never show a
 * number the app would not, and there is no second count query.
 *
 * **Why the app publishes and the widget only reads.** A widget update, or a
 * [WidgetDataWorker] pass in a process the OS started cold, has no
 * conversations session — the bare manager would answer zero — so neither ever
 * computes the count; they paint the last one published ([rerender]).
 *
 * Stateless by design: the file is the only memory (an unchanged count is
 * neither rewritten nor repainted), so every instance agrees, and the one
 * process-wide [writer] thread keeps a [clear] ordered against the publishes
 * around it.
 *
 * [ConversationsManagerHost]: com.fauna.app.core.conversations.ConversationsManagerHost
 */
class WidgetUnreadPublisher(
    private val store: UnreadSnapshotStore,
    private val render: suspend (Int) -> Unit,
    private val dispatcher: CoroutineDispatcher = writer,
    private val now: () -> Long = System::currentTimeMillis,
) {
    @Inject
    constructor(@ApplicationContext context: Context) : this(
        UnreadSnapshotStore.forApp(context.applicationContext),
        { count -> FaunaWidget.renderAll(context.applicationContext, count) },
    )

    private val scope = CoroutineScope(SupervisorJob() + dispatcher)

    /**
     * Write [unread] for the widget and repaint it, unless it is the count already
     * written. Fire-and-forget: called from the manager observer's arbitrary Rust
     * thread, which must never block on file IO or Glance.
     */
    fun publish(unread: Int) {
        scope.launch {
            if (store.load() == unread) return@launch
            try {
                store.save(unread, now())
            } catch (e: Exception) {
                ShellLog.w("FaunaWidget", "unread snapshot write failed: ${e.message}")
                return@launch
            }
            render(unread)
        }
    }

    /**
     * Forget the count: the outgoing account's number must not stay on the home
     * screen after a switch or sign-out (`account-scoping.md` § The scoping
     * taxonomy — it is account-scoped). Zeroed now rather than left for the
     * incoming account's first tick: a stale number is worse than none.
     */
    fun clear() {
        scope.launch {
            store.clear()
            render(0)
        }
    }

    /** Paint the last published count (zero before any) — a pass with no session. */
    suspend fun rerender() = withContext(dispatcher) { render(store.load() ?: 0) }

    companion object {
        /** One writer for the process, so publishes and clears land in call order. */
        private val writer: CoroutineDispatcher = Executors.newSingleThreadExecutor { r ->
            Thread(r, "fauna-widget-publisher").apply { isDaemon = true }
        }.asCoroutineDispatcher()
    }
}

/**
 * The widget refresh pass's counters — the e2e state protocol's
 * `widget_refresh` key (`fauna_e2e_agent::WIDGET_REFRESH_KEY`, the iOS
 * `BackgroundScheduler` counters' twin). Bumped by [WidgetDataWorker] and
 * nothing else, so a pass they count is one the scheduled entry point ran —
 * never the foreground receive loop's, which ticks the same observer.
 * `lastPassCount` is the unread total the last completed pass published, or
 * null when it had no live conversations session to poll.
 */
object WidgetRefreshPasses {
    data class Counters(val started: Int, val completed: Int, val lastPassCount: Int?)

    private val lock = Any()
    private var counters = Counters(0, 0, null)

    fun snapshot(): Counters = synchronized(lock) { counters }

    /** Run one pass between the two counter bumps; a pass that throws counts as one with no session. */
    suspend fun run(pass: suspend () -> Int?) {
        synchronized(lock) { counters = counters.copy(started = counters.started + 1) }
        val count = try {
            pass()
        } catch (e: Exception) {
            ShellLog.w("FaunaWidget", "widget refresh pass failed: ${e.message}")
            null
        }
        synchronized(lock) {
            counters = counters.copy(completed = counters.completed + 1, lastPassCount = count)
        }
    }

    fun stateJson(): JSONObject = snapshot().let {
        JSONObject()
            .put("passes_started", it.started)
            .put("passes_completed", it.completed)
            .put("last_pass_count", it.lastPassCount ?: JSONObject.NULL)
    }
}
