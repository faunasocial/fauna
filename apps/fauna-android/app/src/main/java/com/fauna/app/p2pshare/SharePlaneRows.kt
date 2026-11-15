package com.fauna.app.p2pshare

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

/**
 * The peer-transfer surface's readings, already resolved — the seam record the
 * Folders page paints (`share-serve-status`, `share-transfer-list` and its
 * `share-transfer-item` rows). The shared code names this, never an FFI type:
 * the `storeSafe` bindings export no share plane, so only the two
 * `SharePlaneHost` twins (`src/p2pShare/` / `src/noP2pShare/`) may.
 *
 * Every string here was resolved NESTED (`resolveLocalizedNested`): the
 * transfer gate's refusal reads "Limited by {source}" where `{source}` is
 * itself an i18n key, so a flat substitution would paint the raw key — the
 * same reason tui, linux and apple resolve these nested.
 */
data class SharePlaneReadings(
    val serveStatus: String,
    val transfers: List<SharePlaneTransferRow>,
)

/** One `share-transfer-item`: its name, progress and state readings. */
data class SharePlaneTransferRow(
    val name: String,
    val progress: String,
    val state: String,
)

/**
 * Where the Folders page reads the surface from — the `SharePlaneHost` twins
 * (bound in `BindingsModule`), or a test's fake. `null` while this device is
 * not running the plane, which renders nothing at all.
 */
interface SharePlaneReadingsSource {
    val readings: StateFlow<SharePlaneReadings?>
}

/** What the edges drive — the `SharePlaneHost` twins, or a test's fake. */
interface SharePlaneControl {
    /** Start (or replace) the plane for the signed-in actor. Never throws. */
    fun start()

    /** Stop the plane: the driver ends and the seat unbinds. Never throws. */
    fun stop()
}

/**
 * Several seats driven by one set of edges, in order — the share plane and the
 * relay seat ([com.fauna.app.service.RelayServingSeat]) start and stop on the
 * same foreground edge (`relay-serving.md` § Relay serving → *Which seats
 * hold*, android's provider row). Each control never throws, so one seat's
 * failure never keeps another from its edge.
 */
class SharePlaneControls(private val controls: List<SharePlaneControl>) : SharePlaneControl {
    override fun start() = controls.forEach { it.start() }

    override fun stop() = controls.forEach { it.stop() }
}

/**
 * The android shell's whole lifecycle contract for the share plane
 * (`p2p-shared-set-build.md` § Phone peers — design, decision 2: a phone is a
 * FOREGROUND peer — the plane runs while the app is in front and stops, not
 * pauses, when it leaves). Pure sequencing over three edges, kept apart from
 * the Android types so it is unit-tested without a device:
 *
 * - **foreground** — wait for a live connection (the account-runtime-ready
 *   edge: the runtime starts on the connect path, and shared Rust waits,
 *   bounded, for the runtime and the conversations session itself), then
 *   start. Starting again replaces a running plane, so a repeat is harmless.
 * - **background** — stop at once. A backgrounded phone holds no socket.
 * - **identity change** (switch, sign-out, reset) — not a backgrounding: the
 *   plane bound to the outgoing actor is stopped and, while still in front,
 *   the next turn starts it for whoever connects next — the shape
 *   [com.fauna.app.service.CustodianPushKick] proved for its own loop.
 *
 * No ordering logic beyond that: every decision of the plane is shared Rust's.
 */
class SharePlaneEdges(
    private val plane: SharePlaneControl,
    private val awaitConnected: suspend () -> Unit,
    private val scope: CoroutineScope,
) {
    private var job: Job? = null

    @Volatile
    private var actorRetired = CompletableDeferred<Unit>()

    @Synchronized
    fun foreground() {
        if (job != null) return
        job = scope.launch {
            while (true) {
                val retired = CompletableDeferred<Unit>().also { actorRetired = it }
                awaitConnected()
                // A switch made while waiting: take the next turn instead.
                if (retired.isCompleted) continue
                plane.start()
                retired.await()
            }
        }
    }

    @Synchronized
    fun background() {
        job?.cancel()
        job = null
        plane.stop()
    }

    fun identityChanged() {
        plane.stop()
        actorRetired.complete(Unit)
    }
}
