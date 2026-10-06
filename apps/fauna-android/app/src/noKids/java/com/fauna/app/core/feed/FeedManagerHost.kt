package com.fauna.app.core.feed

import com.fauna.app.core.ApiClient
import com.fauna.app.core.ShellLog
import com.fauna.app.core.conversations.ConversationsManagerHost
import com.fauna.ffi.FfiFeedManager
import com.fauna.ffi.autosaveDebounceMs
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_feed.FeedSnapshot
import uniffi.fauna_feed.FeedSnapshotObserver
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Process-wide owner of the observer→snapshot bridge for the shared Feed-page
 * [FfiFeedManager]. Mirrors
 * [com.fauna.app.core.conversations.ConversationsManagerHost]: one observable
 * surface the Feed list / compose / post-detail screens all render off, so
 * navigating between the three routes (which each `hiltViewModel` a fresh
 * `FeedVM`) shares the SAME snapshot — the post-detail screen renders the
 * `PostSummary` the list loaded, not a fresh empty manager (docs/goal/ui/feed.md
 * §"Architectural rules" #1/#2: observer-driven rendering, no client-side
 * post-list state).
 *
 * The manager itself is a connection-bound singleton owned by [ApiClient] (built
 * lazily over the post-auth WS-RPC socket, torn down on sign-out); this host owns
 * only the [FeedSnapshotObserver] and the [snapshot] `StateFlow`. On every
 * manager notification the observer republishes `snapshot()`; on a
 * sign-out→sign-in the manager is rebuilt, which [manager] detects (identity
 * change) and reseeds the snapshot for.
 */
@Singleton
class FeedManagerHost @Inject constructor(
    private val api: ApiClient,
    private val conversationsManagerHost: ConversationsManagerHost,
) {
    private val _snapshot = MutableStateFlow<FeedSnapshot?>(null)

    /**
     * Compose screens `collectAsState()` this; every manager notification pushes
     * a fresh snapshot. Null before the manager is built (nest not yet connected)
     * and after sign-out.
     */
    val snapshot: StateFlow<FeedSnapshot?> = _snapshot.asStateFlow()

    /**
     * The manager whose `snapshot()` [observer] republishes — held so the
     * arbitrary-Rust-thread `onChanged` callback reads the *current* manager even
     * across a sign-out→sign-in rebuild.
     */
    private var seen: FfiFeedManager? = null

    // ── Draft persistence v2, posts rail (file-sync.md § Drafts Sync,
    // docs/goal/ui/feed.md § Persistence) ───────────────────────────────────
    //
    // The android twin of linux `feed/drafts.rs` and this app's own
    // ConversationsManagerHost drafts leg: pure trigger glue over the shared
    // `FfiDraftsSync` (owns the seal, the WS-RPC `fauna.drafts.{get,put}`
    // calls, the launch gate, and the last-saved baseline) + `FfiFeedManager`'s
    // own `draftsSnapshotBytes`/`restoreDrafts` pair. [ApiClient] builds the
    // sync handle eagerly at connect ([ApiClient.postsDraftsSync]); this host
    // restores onto the manager and arms the debounced autosave the moment a
    // fresh manager appears in [manager] (login, or a re-auth/account-switch
    // rebuild) — never at connect time, because unlike conversations the
    // manager does not exist yet then (it is built lazily on first Feed-page
    // access, above).
    private val draftsScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var draftsSaveJob: Job? = null

    // `onChanged()` fires from arbitrary Rust threads; MutableStateFlow.value is
    // thread-safe and `collectAsState()` observes on the composition context, so
    // no explicit main-thread marshaling is needed (as in ConversationsManagerHost,
    // unlike the Windows INotifyPropertyChanged path). The same notification
    // re-arms the debounced draft autosave (a cheap no-op via `saveIfChanged`'s
    // baseline compare when the tick wasn't a compose edit).
    private val observer = object : FeedSnapshotObserver {
        override fun onChanged() {
            _snapshot.value = seen?.snapshot()
            scheduleDraftsSave()
        }
    }

    init {
        // Re-read `own_rooms` on the conversations plane's OWN change tick
        // (`ui/feed.md` § Encryption at rest → *Room-restricted — the app
        // half*), not just on Feed re-entry — a room joined/left then reaches
        // the composer with no Feed page open. Also re-attempts the seam
        // install below: harmless if already installed ("the last one wins"),
        // and the only path that reaches a manager built BEFORE a session
        // existed (see [installRoomPostSeam]'s doc).
        conversationsManagerHost.onTick = {
            seen?.let { m -> draftsScope.launch { installRoomPostSeam(m) } }
        }
    }

    /**
     * The shared Feed manager over the current connection, or null until the nest
     * is connected (the caller renders an empty page and retries on the next
     * gesture). Reseeds [snapshot] when [ApiClient] hands back a freshly-built
     * manager (the sign-out→sign-in case — the new instance has this host's
     * observer re-registered), and restores that account's persisted feed-compose
     * draft onto it.
     */
    fun manager(): FfiFeedManager? {
        val m = api.feedManager(observer) ?: return null
        if (m !== seen) {
            seen = m
            _snapshot.value = m.snapshot()
            restoreDrafts(m)
            draftsScope.launch { installRoomPostSeam(m) }
        }
        return m
    }

    /**
     * Install the conversations session as [m]'s room-post key seam and
     * re-read `own_rooms` (`ui/feed.md` § Encryption at rest → *Room-restricted
     * — the app half*) — called where the feed manager and the session first
     * coexist. On this app's ordinary login path the session is always built
     * first (`ApiClient.ensureNestConnected` starts it before any Feed-page
     * access can build a manager), so [manager]'s fresh-build call above is
     * the ordinary install point and covers a re-auth too (a rebuilt manager
     * re-runs this against the rebuilt session). The [conversationsManagerHost]
     * tick above covers the other order (a manager already built when the
     * session appears) by re-attempting on every conversations change —
     * a no-op once installed, since `setRoomPostKeys` only ever replaces the
     * seam with the same or a fresher session. No session yet ⇒ leaves every
     * room post locked and no room offered, the honest state.
     */
    private suspend fun installRoomPostSeam(m: FfiFeedManager) {
        val session = conversationsManagerHost.session ?: return
        runCatching { m.setRoomPostKeys(session) }
            .onFailure { ShellLog.w("FeedRoomSeam", "install failed: ${it.message}") }
        runCatching { m.refreshOwnRooms() }
            .onFailure { ShellLog.w("FeedRoomSeam", "refreshOwnRooms failed: ${it.message}") }
    }

    // ── Composer file-attach staging (`ui/media.md` § Encryption at rest) ─────
    //
    // The picked file's EXIF-stripped bytes, held HERE rather than on `FeedVM`
    // — a fresh `FeedVM` instance is created every time the compose screen is
    // navigated to (`hiltViewModel()`), but this singleton is the one thing
    // both the real picker (`FeedComposeScreen`) and the e2e `compose.file`
    // TestAgent injection (`TestAgent.kt`, which has no reference to any live
    // VM) can actually reach — the same reason `attachedFile`'s *metadata*
    // already lives on the manager's own compose state rather than on the VM.
    // Never uploaded here: the seal depends on the composer's staged audience,
    // which is not known until submit (`FeedVM.submitPost`).
    private var pendingAttachmentBytes: ByteArray? = null

    /** Hold (or, with `null`, drop) a picked file's bytes for [pendingAttachmentBytes]. */
    fun stageAttachmentBytes(bytes: ByteArray?) {
        pendingAttachmentBytes = bytes
    }

    /** The held pick, if any — read (not cleared) at submit; [stageAttachmentBytes]
     *  with `null` is the explicit clear, so a failed submit can retry. */
    fun pendingAttachmentBytes(): ByteArray? = pendingAttachmentBytes

    /**
     * Restore this actor's persisted feed-compose draft onto the just-built
     * manager (launch / re-auth). A load failure is logged and left non-fatal —
     * an unreachable nest must not blank the composer. Cancels any debounce job
     * still pending from a previous manager instance first, so a stale save
     * can't fire against the new session out of order.
     */
    private fun restoreDrafts(manager: FfiFeedManager) {
        val sync = api.postsDraftsSync() ?: return
        synchronized(this) {
            draftsSaveJob?.cancel()
            draftsSaveJob = null
        }
        draftsScope.launch {
            try {
                sync.load()?.let { manager.restoreDrafts(it) }
            } catch (e: Exception) {
                ShellLog.w("FeedDrafts", "restore drafts failed: ${e.message}")
            }
        }
    }

    /**
     * Debounced persist after a compose change: each manager notification
     * re-arms an [autosaveDebounceMs] timer and only the last one fires, so a
     * burst of keystrokes coalesces into one upload. Mirrors
     * `ConversationsManagerHost.scheduleDraftsSave`. The feed manager notifies
     * far more often than the conversations one — every reload, resolved quote,
     * link preview and score adjustment ticks the same observer — which is fine
     * rather than something to filter here: a non-compose tick costs one cheap
     * snapshot-bytes compare inside `saveIfChanged` against the last-saved
     * baseline and stops (the linux leg's module doc makes the same point).
     * `synchronized` because `onChanged()` fires from arbitrary Rust threads —
     * it serializes the job swap against [restoreDrafts].
     */
    private fun scheduleDraftsSave() {
        val sync = synchronized(this) {
            val s = api.postsDraftsSync() ?: return
            draftsSaveJob?.cancel()
            s
        }
        val job = draftsScope.launch {
            delay(autosaveDebounceMs().toLong())
            try {
                val m = seen ?: return@launch
                sync.saveIfChanged(m.draftsSnapshotBytes())
            } catch (e: Exception) {
                ShellLog.d("FeedDrafts", "save drafts failed (transient): ${e.message}")
            }
        }
        synchronized(this) { draftsSaveJob = job }
    }
}
