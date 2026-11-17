package com.fauna.app.p2pshare

import android.content.Context
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ShellLog
import com.fauna.app.provider.FfiOnDemandSource
import com.fauna.app.provider.buildPlacedHosts
import com.fauna.app.provider.watchPlacedHosts
import com.fauna.app.ui.util.resolveLocalizedNested
import com.fauna.ffi.FfiNestClient
import com.fauna.ffi.FfiSharePlaneListener
import com.fauna.ffi.actorIdFromSecret
import dagger.hilt.android.qualifiers.ApplicationContext
import java.io.File
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/**
 * android's seat on the **cross-user peer-transfer plane** — a foreground
 * partial peer (`p2p-shared-set-build.md` § Phone peers — design, decisions 1,
 * 2 and 6). **The built half** of the `p2p-share` glue: every build type but
 * `storeSafe` and `kids` compiles this file; those take the inert
 * `src/noP2pShare/` twin, signature-identical and naming no plane FFI symbol.
 *
 * Only the shell lives here — the three inputs and the repaint nudge:
 * - **the access** is `onDemandReplicaAccess`, over this process's on-demand
 *   hosts registry: the DocumentsProvider runs in the app's process, so the
 *   door is a direct call into the set's one host (decision 6);
 * - **the hosts** must be LIVE for the plane to see a set (the registry reports
 *   only what the provider holds), so every desired set's host is built at the
 *   start and again after every re-plan the source announces while the plane
 *   runs — one host per set, the provider's own, never a second writer;
 * - **the spool** is `noBackupFilesDir/share-spool/<actor>`: app-private, and
 *   the one directory the OS keeps out of every cloud backup, so an in-flight
 *   peer download never restores onto another device.
 *
 * Every start and stop runs on one serial scope, in the order the edges
 * ([com.fauna.app.service.SharePlaneForeground]) send them.
 */
@Singleton
class SharePlaneHost @Inject constructor(
    @ApplicationContext private val appContext: Context,
    private val api: ApiClient,
    private val onDemand: FfiOnDemandSource,
) : SharePlaneControl, SharePlaneReadingsSource {

    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    private val serial = CoroutineScope(SupervisorJob() + Dispatchers.IO.limitedParallelism(1))
    private val side = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** The client the running plane was started on — the one to stop it on. */
    @Volatile private var running: FfiNestClient? = null
    @Volatile private var hostWatch: Job? = null

    private val _readings = MutableStateFlow<SharePlaneReadings?>(null)

    /** `null` while this device is not running the plane: the surface renders nothing. */
    override val readings: StateFlow<SharePlaneReadings?> = _readings.asStateFlow()

    private val listener = object : FfiSharePlaneListener {
        override fun seatBound() = refresh()
        override fun stateChanged() = refresh()
    }

    /** Re-read the plane's process-global cell — the one authoritative read. */
    fun refresh() {
        _readings.value = runCatching { com.fauna.ffi.sharePlaneView() }.getOrNull()?.let { view ->
            SharePlaneReadings(
                serveStatus = resolveLocalizedNested(appContext, view.serveStatus).orEmpty(),
                transfers = view.transfers.map {
                    SharePlaneTransferRow(
                        name = resolveLocalizedNested(appContext, it.name).orEmpty(),
                        progress = resolveLocalizedNested(appContext, it.progress).orEmpty(),
                        state = resolveLocalizedNested(appContext, it.state).orEmpty(),
                    )
                },
            )
        }
    }

    override fun start() {
        serial.launch {
            try {
                val client = api.nestRpc()
                val secret = api.ownerSecretBytes()
                val actor = actorIdFromSecret(secret)
                val actorHex = actor.joinToString("") { "%02x".format(it) }
                buildDesiredHosts(refresh = false)
                val spool = File(appContext.noBackupFilesDir, "share-spool/$actorHex").apply { mkdirs() }
                client.startSharePlane(
                    secret,
                    com.fauna.ffi.onDemandReplicaAccess(actor),
                    spool.absolutePath,
                    listener,
                )
                running = client
                watchReplans()
            } catch (e: Exception) {
                ShellLog.w(TAG, "start failed; the peer-transfer surface stays absent: ${e.message}")
            }
            refresh()
        }
    }

    override fun stop() {
        hostWatch?.cancel()
        hostWatch = null
        serial.launch {
            val client = running
            running = null
            runCatching { client?.stopSharePlane() }
                .onSuccess { if (client != null) ShellLog.i(TAG, STOPPED_LINE) }
                .onFailure { ShellLog.w(TAG, "stop failed: ${it.message}") }
            refresh()
        }
    }

    /** Every set the presence plan presents gets its live host (built once, the provider's own). */
    private suspend fun buildDesiredHosts(refresh: Boolean) =
        onDemand.buildPlacedHosts(refresh) { id, e -> ShellLog.w(TAG, "host for $id not built: ${e.message}") }

    /** A set the plan adds while the plane runs (a share accepted in front) gets its host too. */
    private fun watchReplans() {
        hostWatch?.cancel()
        hostWatch = onDemand.watchPlacedHosts(side) { id, e ->
            ShellLog.w(TAG, "host for $id not built: ${e.message}")
        }
    }

    companion object {
        private const val TAG = "SharePlaneHost"

        /**
         * Logged once the plane is down — the background edge's observable
         * outcome (decision 2, stop not pause): nothing this device holds
         * reaches a peer, or a peer's reaches it, until the next start. The
         * two-seat witness waits on it before a save made with the seat down.
         */
        const val STOPPED_LINE = "share plane stopped: this device serves and pulls nothing until it is in front again"
    }
}
