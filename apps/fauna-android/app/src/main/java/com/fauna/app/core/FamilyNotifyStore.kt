package com.fauna.app.core

import com.fauna.ffi.FfiFamilyContentNotice
import com.fauna.ffi.guardianEnforcedCategories
import com.fauna.ffi.notifyReportMinIntervalSecs
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.fauna_core.ContentLabelEntry
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Ward-side **Guardian Notify** counter (`family-safety.md` § Guardian Notify)
 * — the android twin of linux `content_policy.rs`'s `NotifyAccumulator` and
 * web's `familyNotify.ts` `NotifyBuffer`. Counts the viewer's own
 * GUARDIAN-floor render-enforcement events per category — **coarse counts
 * only, never a content id** — deduped by `(item id, category)` within the
 * local day, and batches them for `fauna.family.notify_report`, flushing at
 * most once per the shared [notifyReportMinIntervalSecs].
 *
 * *Which* categories count is the shared [guardianEnforcedCategories] (only
 * the guardian floor, never the ward's own thresholds — [ContentPolicyStore]
 * composes both, but Notify is a lens on the guardian's policy alone), so
 * android never drifts from linux/web on what Notify reports.
 */
@Singleton
class FamilyNotifyStore @Inject constructor(
    private val api: ApiClient,
    private val contentPolicyStore: ContentPolicyStore,
    accountStores: AccountStores,
) {
    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())

    private val pending = mutableMapOf<String, Int>()
    private val seen = mutableSetOf<Pair<String, String>>()
    private var seenDay = Long.MIN_VALUE
    private var lastFlush: Long? = null
    private var offsetMinutes = 0

    init {
        // Drop this actor's buffered counts + dedup state on an account switch or
        // sign-out — web's resetForActorChange() shape. Everything held here is
        // one ward's enforcement history, so it is account-scoped; the `seen` set
        // is the sharp edge: carrying it across a switch would make the incoming
        // ward's first enforcement on a re-used item id silently UNcounted (an
        // under-report to their guardian that looks exactly like "nothing
        // happened"). Pending counts are dropped, not flushed: there is no
        // identity left in this process to attribute them to once the switch
        // completes, and Guardian Notify is explicitly coarse/best-effort
        // (family-safety.md § Guardian Notify trust bound), so losing a partial
        // bucket at a switch is within its contract — attributing it to the
        // wrong actor would not be.
        accountStores.registerCloser("family-notify") { reset() }
        scope.launch {
            while (true) {
                delay(CHECK_INTERVAL_MS)
                flushIfDue()
            }
        }
    }

    /**
     * Count any **guardian-floor** render-enforcement on [itemId] (a feed post
     * or DM message). A no-op unless the ward's `content_notify` knob is on AND
     * the guardian floor bites on one of this item's [labels] — never the
     * ward's own-threshold collapses. Deduped per item per local day, so a
     * re-render never re-counts.
     */
    @Synchronized
    fun record(itemId: String, labels: List<ContentLabelEntry>) {
        val inputs = contentPolicyStore.inputs.value
        if (!inputs.contentNotify) return
        val contentPolicy = inputs.contentPolicy ?: return
        val cats = guardianEnforcedCategories(labels, contentPolicy)
        if (cats.isEmpty()) return

        val offset = DeviceOffset.utcOffsetMinutes()
        offsetMinutes = offset
        val nowSecs = System.currentTimeMillis() / 1000
        val localDay = Math.floorDiv(nowSecs + offset * 60L, 86_400L)
        if (localDay != seenDay) {
            seen.clear()
            seenDay = localDay
        }
        // The dedup key is the (item, category) PAIR itself, which is linux's
        // shape (`HashSet<(String, &'static str)>`) — no separator character is
        // involved, so no item id can collide with a different pair by
        // containing the separator, and no escape has to survive review. It
        // replaces a joined key whose separator was written as a RAW NUL BYTE
        // rather than an escape, which made git classify this Kotlin source as
        // binary: `grep`/`rg` skipped the file silently, `git diff` showed
        // "Binary files differ", and a concurrent edit would have been an
        // unmergeable conflict.
        for (cat in cats) {
            if (seen.add(itemId to cat)) {
                pending[cat] = (pending[cat] ?: 0) + 1
            }
        }
    }

    /** Drain the batched report if a flush is due (the interval gate below) and
     *  fire it — best-effort, fire-and-forget (a modified client under-reports —
     *  family-safety.md § Guardian Notify trust bound). `internal`, not
     *  `private`, so a test can force a due-check synchronously instead of
     *  waiting on [CHECK_INTERVAL_MS] — the same reason web's `NotifyBuffer`
     *  exposes `checkNow()`. */
    internal fun flushIfDue() {
        val due = takeDue() ?: return
        scope.launch {
            runCatching { api.familyNotifyReport(due.first, due.second) }
        }
    }

    @Synchronized
    private fun takeDue(): Pair<List<FfiFamilyContentNotice>, Int>? {
        if (pending.isEmpty()) return null
        val now = System.currentTimeMillis() / 1000
        // "batched (at most hourly)": at least one full interval between
        // flushes; the first report (no prior flush) is eager.
        val last = lastFlush
        if (last != null && now - last < notifyReportMinIntervalSecs().toLong()) return null
        val entries = pending.map { (cat, count) -> FfiFamilyContentNotice(category = cat, count = count.toUInt()) }
        pending.clear()
        lastFlush = now
        return entries to offsetMinutes
    }

    @Synchronized
    private fun reset() {
        pending.clear()
        seen.clear()
        seenDay = Long.MIN_VALUE
        lastFlush = null
        offsetMinutes = 0
    }

    companion object {
        /** How often the flush loop wakes to check whether a report is due. The
         *  report itself fires at most hourly ([notifyReportMinIntervalSecs]);
         *  this only bounds the latency of the first report after the ward flags
         *  something, so the check is cheap and the send is rare. */
        private const val CHECK_INTERVAL_MS = 5_000L
    }
}
