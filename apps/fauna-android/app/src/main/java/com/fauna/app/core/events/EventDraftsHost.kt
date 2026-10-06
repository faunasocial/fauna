package com.fauna.app.core.events

import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import com.fauna.app.core.ShellLog
import com.fauna.ffi.FfiEventDrafts
import com.fauna.ffi.FfiEventDraftsSyncInterface
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
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Process-wide owner of the events-rail live draft — draft-persistence v2's
 * third rail (`docs/goal/behavior/reserved-folders.md` § Drafts Sync;
 * `docs/goal/ui/events.md` § Persistence), the android twin of web's
 * `event-drafts.ts` module state and native's `apps/fauna-linux/src/views/
 * events/drafts.rs` / `apps/fauna-tui/src/events/drafts.rs`.
 *
 * Unlike [com.fauna.app.core.conversations.ConversationsManagerHost] and
 * `FeedManagerHost`, there is no shared manager to hang a `DraftsSync` off —
 * the Events page has none on any app (`reserved-folders.md`'s 2026-08-17
 * ruling: the three trigger shapes stay three) — so this host itself owns the
 * live draft directly, exposed reactively via [draft].
 *
 * **The fourth rule is why [draft] is a `StateFlow`, not a plain getter**
 * (`reserved-folders.md` § Drafts Sync): the rail object must own the live
 * draft and update it on every edit *immediately*, not when the debounce
 * fires, and a form that opens while the launch restore is still in flight
 * must still see it land — a one-shot accessor read only at open cannot do
 * that (the exact bug `test_event_draft_persistence.py[web]` caught: an
 * empty form for a draft the nest still held, after a Settings-and-back
 * inside one session). The opener *reads* [draft] without consuming it;
 * whether resuming is safe (declining when the compose already holds
 * authored text) is the caller's decision, never this host's.
 */
@Singleton
class EventDraftsHost internal constructor(
    // Injected so `EventDraftsHostTest` can drive the restore/save coroutines
    // on a test dispatcher and use an explicitly opened gate as its causal
    // barrier — the identity-seam cases assert that something does NOT happen,
    // and e2e-conventions.md convention 14 forbids settling that with a sleep.
    // Production always gets the scope the `@Inject` constructor builds.
    private val scope: CoroutineScope,
) : DefaultLifecycleObserver {
    @Inject constructor() : this(CoroutineScope(SupervisorJob() + Dispatchers.IO))

    private var sync: FfiEventDraftsSyncInterface? = null
    private var saveJob: Job? = null

    /**
     * **The identity seam** (`account-scoping.md` § The scoping taxonomy → the
     * in-memory corollary: the loops that WRITE actor-scoped state must be
     * retired by the same drop, and one holding no cancellation handle needs a
     * seam of its own). Bumped by BOTH [startDraftsSync] and [stopDraftsSync],
     * captured by every coroutine this host launches, and re-checked under the
     * lock immediately before that coroutine writes anything.
     *
     * ⚠ **Cancelling the coroutine instead is not sufficient, and this is the
     * whole reason the counter exists.** The launch restore is a
     * `fauna.drafts.get` round-trip kicked off *before* `client.connect()`
     * ([com.fauna.app.core.ApiClient]), so it can be in flight for the request
     * deadline; a coroutine already suspended inside the UniFFI call is not
     * cancelled mid-call, so it returns and assigns however promptly its `Job`
     * was cancelled. The check has to sit at the WRITE.
     *
     * Guarded by the same monitor as [sync]: `synchronized(this)`.
     */
    private var generation: Long = 0
    // The raw last-edited record, all-empty included — distinct from [draft],
    // which normalizes an all-empty record to `null` for the resume decision.
    // The leave-flush and the debounced save both need the raw value: an
    // in-flight "clear" (a day-cell fresh start, a successful create) is
    // exactly the all-empty write that must still reach the nest.
    private var lastEdited: FfiEventDrafts? = null
    private var registeredForLifecycle = false

    private val _draft = MutableStateFlow<FfiEventDrafts?>(null)

    /** The live draft, or `null` for "nothing to resume" — seeded by the
     *  launch restore, updated immediately by every [onEdit]. An all-empty
     *  record reads as `null` here (indistinguishable from no draft; the
     *  form's own default already is empty), matching
     *  `fauna_client_caldav::drafts::EventDrafts::is_empty`. */
    val draft: StateFlow<FfiEventDrafts?> = _draft.asStateFlow()

    /**
     * Wire draft persistence for the just-connected session: restore the
     * owner's persisted event draft on launch. Called once per session by
     * [com.fauna.app.core.ApiClient]'s connect path; [stopDraftsSync] retires
     * it on logout. A restore failure is logged and left non-fatal — the
     * `FfiEventDraftsSync` launch gate then stays closed, so a later autosave
     * can't clobber the unread blob (no-data-loss).
     */
    fun startDraftsSync(s: FfiEventDraftsSyncInterface) {
        val mine = synchronized(this) {
            sync = s
            // Retire whatever the previous session left in flight, and clear the
            // rail for the INCOMING actor rather than trusting the outgoing drop
            // to have run: this is reachable without a preceding
            // [stopDraftsSync] (a reconnect), and the restore below assigns only
            // on a non-null result — so an uncleared rail simply keeps showing
            // the last session's draft for the whole of this one.
            generation += 1
            lastEdited = null
            generation
        }
        _draft.value = null
        if (!registeredForLifecycle) {
            registeredForLifecycle = true
            // The leave-flush door (reserved-folders.md § The leave-flush
            // promise): the OS may kill a backgrounded app at any moment, so a
            // pending debounced save must not wait for its timer. Registered
            // once for the process lifetime — `sync == null` between sessions
            // makes [onStop] a no-op rather than needing to unregister.
            ProcessLifecycleOwner.get().lifecycle.addObserver(this)
        }
        scope.launch {
            try {
                val restored = s.restoreDrafts()
                // Re-check AFTER the round-trip: this is the write the seam
                // exists for. A restore that outlived its own session must reach
                // nothing — otherwise the departing actor's half-written event
                // becomes the incoming actor's resumable draft, and their first
                // keystroke sends the whole form to `saveDrafts` under THEIR
                // handle, sealed under THEIR `BackupKey`.
                if (restored != null && synchronized(this@EventDraftsHost) { generation == mine }) {
                    _draft.value = restored
                }
            } catch (e: Exception) {
                ShellLog.w("EventDrafts", "restore drafts failed: ${e.message}")
            }
        }
    }

    /** Retire the autosave on logout. The [FfiEventDraftsSync] handle itself
     *  is closed by [com.fauna.app.core.ApiClient]. A pending debounced save
     *  is dropped rather than flushed — it belongs to the outgoing actor
     *  (mirrors web's `resetEventDrafts`). */
    fun stopDraftsSync() {
        synchronized(this) {
            // Bump first: everything below is the drop, and the seam comes
            // before the drop. Cancelling `saveJob` remains right for the
            // pending debounce (it has not entered the FFI call yet, so
            // cancellation does stop it) — the counter is what covers the calls
            // that are already inside one.
            generation += 1
            saveJob?.cancel()
            saveJob = null
            sync = null
            lastEdited = null
        }
        _draft.value = null
    }

    /**
     * Update the live draft immediately (the fourth rule) and re-arm the
     * debounced upload. Called on every `event-form` field edit.
     */
    fun onEdit(
        summary: String,
        dtstart: String,
        dtend: String,
        description: String,
        location: String,
    ) {
        val d = FfiEventDrafts(summary, dtstart, dtend, description, location)
        lastEdited = d
        _draft.value = if (isEmptyDraft(d)) null else d
        scheduleSave(d)
    }

    /**
     * Empty the rail — a successful create (on the success path, not at the
     * submit click), or a day-cell "start fresh" (`events.md` § Persistence).
     * Goes through the same debounced path as [onEdit] (mirrors web's
     * `clearEventDraft`): a discard racing a teardown is covered by the
     * leave-flush, same as any other pending edit.
     */
    fun clear() = onEdit("", "", "", "", "")

    private fun isEmptyDraft(d: FfiEventDrafts) =
        d.summary.isEmpty() && d.dtstart.isEmpty() && d.dtend.isEmpty() &&
            d.description.isEmpty() && d.location.isEmpty()

    private fun scheduleSave(d: FfiEventDrafts) {
        val (s, mine) = synchronized(this) {
            val cur = sync ?: return
            saveJob?.cancel()
            cur to generation
        }
        val job = scope.launch {
            delay(autosaveDebounceMs().toLong())
            // The debounce is normally retired by `saveJob.cancel()`; the
            // generation covers the window where this has already left the
            // delay. A pending save belongs to the outgoing actor and is
            // dropped rather than flushed (the ratified behaviour, mirroring
            // web's `resetEventDrafts`).
            if (synchronized(this@EventDraftsHost) { generation != mine }) return@launch
            try {
                s.saveDrafts(d.summary, d.dtstart, d.dtend, d.description, d.location)
            } catch (e: Exception) {
                ShellLog.d("EventDrafts", "save drafts failed (transient): ${e.message}")
            }
        }
        synchronized(this) {
            // A `stopDraftsSync` may have raced in between; don't resurrect it.
            if (generation == mine && sync != null) saveJob = job else job.cancel()
        }
    }

    /** The leave-flush (`reserved-folders.md` § The leave-flush promise, the
     *  gap this rail lands closed from the start): force an immediate save of
     *  [lastEdited], bypassing the debounce, on the app's background
     *  transition. Mirrors web's `flushEventDraftNow`, wired to
     *  `visibilitychange`/`pagehide` there and to the process lifecycle here —
     *  best-effort like any such handler (no reliable async work guaranteed
     *  after `onStop`), same posture the goal doc states for the web leg. */
    override fun onStop(owner: LifecycleOwner) {
        // `sync`, `lastEdited` and the generation are read as ONE atomic triple.
        // Reading `lastEdited` outside the lock was the symmetric tear: a
        // session change landing between the two reads paired the INCOMING
        // actor's draft with the DEPARTING actor's captured handle, writing B's
        // text into A's `__drafts` under A's `BackupKey`.
        val (s, d, mine) = synchronized(this) {
            saveJob?.cancel()
            saveJob = null
            Triple(sync ?: return, lastEdited ?: return, generation)
        }
        scope.launch {
            if (synchronized(this@EventDraftsHost) { generation != mine }) return@launch
            try {
                s.saveDrafts(d.summary, d.dtstart, d.dtend, d.description, d.location)
            } catch (e: Exception) {
                ShellLog.d("EventDrafts", "leave-flush event draft failed: ${e.message}")
            }
        }
    }
}
