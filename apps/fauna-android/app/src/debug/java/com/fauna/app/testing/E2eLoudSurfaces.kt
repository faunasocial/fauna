package com.fauna.app.testing

import android.app.Activity
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.view.View
import android.view.ViewGroup
import android.view.ViewTreeObserver
import android.view.inspector.WindowInspector
import androidx.compose.ui.node.RootForTest
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import com.fauna.ffi.FfiConnectionReportsForTest
import com.fauna.ffi.FfiConnectionState
import com.fauna.ffi.FfiNestClient
import com.fauna.ffi.FfiPaintedElement
import com.fauna.ffi.FfiPaintedErrorTallyForTest
import com.fauna.ffi.criticalAlertSweepWakeForTest
import com.fauna.ffi.criticalAlertsRegistry
import org.json.JSONObject
import java.util.WeakHashMap

/**
 * The loud surfaces' four e2e seams on android — `fauna_e2e_agent::
 * {ALERT_SWEEP_WAKE, RECONNECT_BACKOFF, CONNECTION_REPORTS_KEY,
 * PAINTED_ERRORS_KEY}`, which own the cross-app contracts — plus the sweep's
 * pass counters (`ALERT_SWEEP_PASSES_KEY`) those journeys anchor on. Every one
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

    private val main by lazy { Handler(Looper.getMainLooper()) }

    /** Main-thread only. A frame read is already queued. */
    private var paintedFrameQueued = false

    /** Main-thread only. The window roots carrying our draw listener. */
    private val observedRoots = WeakHashMap<View, Unit>()

    /** Main-thread only. The last `painted_errors` value logged. */
    private var lastShown = ""

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
     * Write `connection_reports`, `painted_errors` and `alert_sweep_passes` into
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
        // The sweep's own counters, bumped inside the shared sweep on the ONE
        // process-wide registry (`criticalAlertsRegistry()` hands back the same
        // `Arc` every call) — never an android-side count. A pair, not one
        // number: a re-established session stacks sweep loops, so the waiter
        // compares `completed` against the `started` it read at plant time.
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
        lastShown = ""
    }

    /** Test seam: feed one frame read off [root] now, on the calling (main)
     *  thread — what a draw on that window schedules. */
    internal fun observeFrameForTest(root: View) {
        paintedErrors.observe(paintedElementsOf(root))
    }
}
