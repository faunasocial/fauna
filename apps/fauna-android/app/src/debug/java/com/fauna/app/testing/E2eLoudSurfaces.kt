package com.fauna.app.testing

import android.app.Activity
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.view.View
import android.view.ViewGroup
import android.view.ViewTreeObserver
import android.view.inspector.WindowInspector
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.findRootCoordinates
import androidx.compose.ui.node.RootForTest
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import com.fauna.ffi.FfiConnectionReportsForTest
import com.fauna.ffi.FfiConnectionState
import com.fauna.ffi.FfiKnockNoticesForTest
import com.fauna.ffi.FfiLaidOutText
import com.fauna.ffi.FfiLayoutSize
import com.fauna.ffi.FfiNestClient
import com.fauna.ffi.FfiPaintedElement
import com.fauna.ffi.FfiPaintedErrorTallyForTest
import com.fauna.ffi.FfiPaintedTextAreaForTest
import com.fauna.ffi.criticalAlertSweepWakeForTest
import com.fauna.ffi.criticalAlertsRegistry
import org.json.JSONObject
import java.util.WeakHashMap

/**
 * The loud surfaces' four e2e seams on android — `fauna_e2e_agent::
 * {ALERT_SWEEP_WAKE, RECONNECT_BACKOFF, CONNECTION_REPORTS_KEY,
 * PAINTED_ERRORS_KEY}`, which own the cross-app contracts — plus the sweep's
 * pass counters (`ALERT_SWEEP_PASSES_KEY`) those journeys anchor on, and the
 * knock-toast log (`KNOCK_NOTICES_KEY`), the same shape of firing-site seam, and
 * convention 17's `painted_text_area` record (`PAINTED_TEXT_AREA_KEY`), read off
 * the same frame as `painted_errors`. Every one
 * is a thin call into `fauna-ffi`'s `test-helpers` exports (`e2e_seams.rs`,
 * `critical_alerts.rs`), so android counts, parses and wakes with the same Rust
 * as tui, linux, web, windows and apple (apple's `E2eLoudSurfaces.swift` and
 * windows' `E2eLoudSurfaces.cs` are the sibling legs).
 *
 * Lives in the debug source set with the rest of the automation surface
 * (convention 15): the wrapper types exist only in the test flavor's bindings.
 * The one production caller — the connection pump, `ApiClient.
 * startConnectionStatePump` — reaches [observeConnectionReport] through
 * [TestAgent.observeConnectionReport], whose shipping twin (`src/noAgent`) is a
 * no-op.
 */
object E2eLoudSurfaces {
    // Replaced only by [clearForTest]; `@Volatile` because the pump feeds on
    // `connectionScope`'s IO thread, the tally on the main thread, and the
    // state push reads both on the poll thread (each wrapper locks itself).
    @Volatile private var connectionReports = FfiConnectionReportsForTest()
    @Volatile private var paintedErrors = FfiPaintedErrorTallyForTest()
    @Volatile private var knockNotices = FfiKnockNoticesForTest()
    @Volatile private var paintedTextArea = FfiPaintedTextAreaForTest()

    private val main by lazy { Handler(Looper.getMainLooper()) }

    /** Main-thread only. A frame read is already queued. */
    private var paintedFrameQueued = false

    /** Main-thread only. The window roots carrying our draw listener. */
    private val observedRoots = WeakHashMap<View, Unit>()

    /** Main-thread only. The last `painted_errors` value logged. */
    private var lastShown = ""

    /** Main-thread only. The last `painted_text_area` value logged. */
    private var lastStarved = ""

    private var activity: java.lang.ref.WeakReference<Activity>? = null

    /**
     * Count one connection-state value the indicator received — EVERY value,
     * repeats included: a repeat is a report and never a transition, which is
     * how "further failed attempts left 'Cannot connect' standing" is told apart
     * from "nothing happened" (`transport-connection.md` § `Unreachable`).
     */
    fun observeConnectionReport(state: FfiConnectionState) {
        connectionReports.observe(state)
    }

    /** A knock push reached the knock-toast arm (before the muted gate). */
    fun observeKnockNotice() {
        knockNotices.observe()
    }

    /** A knock toast was handed to the platform: the catalog key its text
     *  resolved from (`null` for a verbatim text), the painted text, and the
     *  notification id it replaces under. */
    fun recordKnockNotice(key: String?, text: String, replaceId: Int) {
        knockNotices.record(key, text, replaceId.toUInt())
    }

    /**
     * Start feeding `painted_errors` from the composition, in process — never
     * the out-of-process UiAutomator bridge, whose accessibility walk is a
     * sampler between frames and misses an error that came and went
     * (`e2e-latency-independent-assertions.md` § Implementation status today).
     *
     * Each window root gets a draw listener; a draw marks the frame dirty and
     * the composition's semantics are read once on the next main-looper turn,
     * after the burst that changed it (windows reads once per layout pass, web
     * once per animation frame, apple once per registry change). A window that
     * opens later (a dialog, a popup) is picked up on the next read, which every
     * state push also triggers. Called from [TestAgent.start]; idempotent.
     */
    fun installPaintedErrorObserver(activity: Activity) {
        this.activity = java.lang.ref.WeakReference(activity)
        main.post { observePaintedFrame() }
    }

    private val onDraw = ViewTreeObserver.OnDrawListener { schedulePaintedFrame() }

    private fun schedulePaintedFrame() {
        if (paintedFrameQueued) return
        paintedFrameQueued = true
        main.post {
            paintedFrameQueued = false
            observePaintedFrame()
        }
    }

    /** Feed the tally one frame: every tagged element and its visible text.
     *  The tally applies the contract's error-surface predicate itself. */
    private fun observePaintedFrame() {
        val roots = windowRoots()
        for (root in roots) {
            if (observedRoots.put(root, Unit) == null && root.viewTreeObserver.isAlive) {
                root.viewTreeObserver.addOnDrawListener(onDraw)
            }
        }
        paintedErrors.observe(roots.flatMap { paintedElementsOf(it) })
        paintedTextArea.observe(roots.flatMap { laidOutTextOf(it) })
        val starved = paintedTextArea.json()
        if (starved != lastStarved) {
            lastStarved = starved
            android.util.Log.i("fauna.e2e", "[painted-text-area] $starved")
        }
        // One log line per change of what is painted, never per frame: the
        // evidence a count that did (or did not) move can be checked against.
        val shown = paintedErrors.json()
        if (shown != lastShown) {
            lastShown = shown
            android.util.Log.i("fauna.e2e", "[painted-errors] $shown")
        }
    }

    /** Every window this process shows — the activity's and any dialog's or
     *  popup's — or the activity's alone below API 29. */
    private fun windowRoots(): List<View> {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            runCatching { WindowInspector.getGlobalWindowViews().filter { it.isAttachedToWindow } }
                .getOrNull()?.let { return it }
        }
        return listOfNotNull(activity?.get()?.window?.decorView)
    }

    /**
     * The `(test id, visible text)` pairs one window paints: every Compose root
     * under [root], walked through its UNMERGED semantics tree. A tagged node's
     * text is its own `Text`/`EditableText`, else its descendants' joined — the
     * tag often sits on a container around the `Text` that carries the words.
     * Nodes not placed are skipped: they are composed but not painted.
     */
    internal fun paintedElementsOf(root: View): List<FfiPaintedElement> {
        val out = mutableListOf<FfiPaintedElement>()
        composeRoots(root).forEach { owner ->
            walk(owner.semanticsOwner.unmergedRootSemanticsNode, out)
        }
        return out
    }

    private fun composeRoots(view: View): List<RootForTest> {
        val found = mutableListOf<RootForTest>()
        fun visit(v: View) {
            if (v is RootForTest) {
                found += v
                return
            }
            if (v is ViewGroup) for (i in 0 until v.childCount) visit(v.getChildAt(i))
        }
        visit(view)
        return found
    }

    private fun walk(node: SemanticsNode, out: MutableList<FfiPaintedElement>) {
        if (!node.layoutInfo.isPlaced) return
        node.config.getOrNull(SemanticsProperties.TestTag)?.let { tag ->
            out += FfiPaintedElement(id = tag, text = visibleText(node))
        }
        node.children.forEach { walk(it, out) }
    }

    /**
     * Convention 17's `painted_text_area` feed: every PLACED, test-tagged node
     * with non-empty text of its OWN under [root], with the room the layout gave
     * it in DIP (`frame_invariants.py::_check_painted_text_has_area` owns the
     * contract; the shared `fauna_e2e_contract::PaintedTextArea` keeps the
     * starved ones). A descendant's text is never the tag's: it is judged in
     * its own node, with its own box.
     *
     * ⚠ The measure is the ROOM, never the measured size alone. Compose lets a
     * child measure LARGER than its constraints (`requiredWidth`, a text that
     * cannot wrap) and centres the overflow, painting it unless a clip layer
     * cuts it — so a node's own size says nothing about what a human sees (the
     * WinUI `ActualWidth` trap, `PaintedTextAreaObserver.cs`). The box is the
     * node's bounds mapped up with `clipBounds = true`, which intersects it with
     * every clip layer on the way (`Modifier.clip`, `clipToBounds`, a scroll
     * container's viewport) and with nothing else.
     *
     * A scroll container's viewport is NOT such a clip: a node scrolled out of
     * view keeps its box. So a node under a scroll container is mapped only up
     * to that container's CONTENT (its inner coordinates — below its own clip
     * layer and scroll offset), then capped by the container's own room, size by
     * size; positions are ignored there, so the cap can miss a starved element,
     * never invent one. A scroll container is a semantics node carrying a
     * scroll axis range (`verticalScroll`, `LazyColumn`, a pager).
     *
     * `pane` is the room of the nearest semantics ancestor that has area (null
     * when none has), `depth` the box-less semantics ancestors between them.
     */
    internal fun laidOutTextOf(root: View): List<FfiLaidOutText> {
        val out = mutableListOf<FfiLaidOutText>()
        composeRoots(root).forEach { owner ->
            val density = owner.density.density
            val top = owner.semanticsOwner.unmergedRootSemanticsNode
            val coords = top.layoutInfo.coordinates
            if (!coords.isAttached) return@forEach
            val anchor = TextRoom(coords.findRootCoordinates(), Float.POSITIVE_INFINITY, Float.POSITIVE_INFINITY)
            walkRoom(top, anchor, null, 0, density, out)
        }
        return out
    }

    /** What the room walk carries down: the coordinates a node's box is mapped
     *  into, and the size cap that anchor's own room sets. */
    private data class TextRoom(val anchor: LayoutCoordinates, val capW: Float, val capH: Float)

    private fun walkRoom(
        node: SemanticsNode,
        room: TextRoom,
        pane: FfiLayoutSize?,
        depth: Int,
        density: Float,
        out: MutableList<FfiLaidOutText>,
    ) {
        if (!node.layoutInfo.isPlaced) return
        val coords = node.layoutInfo.coordinates
        if (!coords.isAttached || !room.anchor.isAttached) return
        val bounds = room.anchor.localBoundingBoxOf(coords, clipBounds = true)
        val width = minOf(bounds.width / density, room.capW)
        val height = minOf(bounds.height / density, room.capH)
        node.config.getOrNull(SemanticsProperties.TestTag)?.let { tag ->
            val text = ownText(node).trim()
            if (text.isNotEmpty()) {
                out += FfiLaidOutText(
                    id = tag,
                    text = text,
                    width = round1(width),
                    height = round1(height),
                    pane = pane,
                    depth = depth.toUInt(),
                )
            }
        }
        val hasArea = width >= 1f && height >= 1f
        val innerPane = if (hasArea) FfiLayoutSize(round1(width), round1(height)) else pane
        val innerDepth = if (hasArea) 0 else depth + 1
        val scrolls = node.config.getOrNull(SemanticsProperties.VerticalScrollAxisRange) != null ||
            node.config.getOrNull(SemanticsProperties.HorizontalScrollAxisRange) != null
        val innerRoom = if (scrolls) TextRoom(coords, width, height) else room
        node.children.forEach { walkRoom(it, innerRoom, innerPane, innerDepth, density, out) }
    }

    private fun round1(v: Float): Double = Math.round(v.toDouble() * 10.0) / 10.0

    private fun ownText(node: SemanticsNode): String =
        listOfNotNull(
            node.config.getOrNull(SemanticsProperties.EditableText)?.text,
            node.config.getOrNull(SemanticsProperties.Text)?.joinToString(" ") { it.text },
        ).filter { it.isNotEmpty() }.joinToString(" ")

    private fun visibleText(node: SemanticsNode): String {
        val own = ownText(node)
        if (own.isNotEmpty()) return own
        return node.children
            .filter { it.layoutInfo.isPlaced }
            .map { visibleText(it) }
            .filter { it.isNotEmpty() }
            .joinToString(" ")
    }

    /**
     * Write `connection_reports`, `painted_errors`, `painted_text_area`, `knock_notices` and `alert_sweep_passes` into
     * the agent's top-level state, decoded (a raw JSON string would
     * double-encode at the wire). All three are process-wide and start at zero,
     * so they are published unconditionally — zeros are an answer, the key's
     * absence a refusal. The painted frame is read once more on the main thread,
     * so a change no draw announced is seen by the next push.
     */
    fun putState(state: JSONObject) {
        main.post { observePaintedFrame() }
        state.put("connection_reports", JSONObject(connectionReports.json()))
        state.put("painted_errors", JSONObject(paintedErrors.json()))
        // `fauna_e2e_agent::PAINTED_TEXT_AREA_KEY` — the last laid-out frame's
        // starved text; `{"starved": []}` before the first read is an answer.
        state.put("painted_text_area", JSONObject(paintedTextArea.json()))
        // `fauna_e2e_agent::KNOCK_NOTICES_KEY` — process-wide from zero like the
        // two above, so published unconditionally: `seen` 0 is an answer.
        state.put("knock_notices", JSONObject(knockNotices.json()))
        // The sweep's own counters, bumped inside the shared sweep on the ONE
        // process-wide registry (`criticalAlertsRegistry()` hands back the same
        // `Arc` every call) — never an android-side count. A pair, not one
        // number: the waiter compares `completed` against the `started` it read
        // at plant time. A same-identity re-establish runs one pass, not a
        // second loop (the shared `startCriticalAlertSweep`'s claim).
        val alerts = criticalAlertsRegistry()
        state.put(
            "alert_sweep_passes",
            JSONObject()
                .put("started", alerts.sweepPassesStarted().toLong())
                .put("completed", alerts.sweepPassesCompleted().toLong()),
        )
    }

    /**
     * `alert_sweep_wake` / `reconnect_backoff`. Returns the refusal sentence, or
     * `null` once the seam landed — convention 11: a seam that did not land is
     * refused loudly, never acked.
     */
    fun apply(action: String, cmd: JSONObject, nest: FfiNestClient?): String? = when (action) {
        // End the current identity's re-sweep WAIT so the production LOOP
        // (`KidsExcisedNav`'s post-auth hook → `run_critical_alert_sweep_loop`)
        // sweeps again; the caller's barrier is `alert_sweep_passes`, never this
        // ack. Never a one-shot pass: that would pass "announced without a
        // restart" with the loop deleted.
        ALERT_SWEEP_WAKE ->
            if (criticalAlertSweepWakeForTest()) {
                null
            } else {
                "$action: no sweep loop runs for this identity, so there is nothing to wake"
            }
        // Pace this session's reconnect retries (never the `Unreachable`
        // threshold), or restore them with `{}`. The payload is the command's
        // own fields, handed to shared Rust verbatim, which parses and refuses a
        // malformed one.
        RECONNECT_BACKOFF -> {
            val fields = JSONObject(cmd.toString())
            for (key in listOf("id", "action", "__action")) fields.remove(key)
            if (nest == null) {
                "$action: no fauna client, so no connection to pace"
            } else {
                try {
                    nest.setReconnectBackoffForTest(fields.toString())
                    null
                } catch (e: Exception) {
                    "$action: ${e.message}"
                }
            }
        }
        else -> "$action: not a loud-surface seam"
    }

    const val ALERT_SWEEP_WAKE = "alert_sweep_wake"
    const val RECONNECT_BACKOFF = "reconnect_backoff"

    /** Test seam: fresh counters, so a Robolectric pin reads its own reports. */
    internal fun clearForTest() {
        connectionReports = FfiConnectionReportsForTest()
        paintedErrors = FfiPaintedErrorTallyForTest()
        knockNotices = FfiKnockNoticesForTest()
        paintedTextArea = FfiPaintedTextAreaForTest()
        lastShown = ""
        lastStarved = ""
    }

    /** Test seam: feed one frame read off [root] now, on the calling (main)
     *  thread — what a draw on that window schedules. */
    internal fun observeFrameForTest(root: View) {
        paintedErrors.observe(paintedElementsOf(root))
        paintedTextArea.observe(laidOutTextOf(root))
    }
}
