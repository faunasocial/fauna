package com.fauna.app.service

import com.fauna.app.core.ApiClient
import com.fauna.app.core.ShellLog
import com.fauna.app.p2pshare.SharePlaneControl
import com.fauna.app.provider.FfiOnDemandSource
import com.fauna.ffi.FfiNestClient
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

/**
 * android's **relay seat** for its provider's on-demand hosts
 * (`relay-serving.md` § Relay serving → *Which seats hold*, android's provider
 * row): while the app is in front, the app's own connection announces every set
 * whose host this process holds and answers the nest's asks from each host's
 * hydrated and kept bodies; on the background edge the stop withdraws the
 * announce, so a backgrounded phone is asked for nothing
 * (`p2p-shared-set-build.md` § Phone peers — design, decision 2).
 *
 * Driven by the same edges as the share plane ([SharePlaneForeground]), but
 * main-source, never behind the `p2p-share` excision: relay serving is not an
 * excisable feature, so the store-safe and kids flavors serve too. Every
 * decision — the announce, the host rescan, the withdrawal — is shared Rust's
 * (`fauna_sync_engine::relay_seat`); this is the two calls.
 *
 * Every start and stop runs on one serial scope, in the order the edges send
 * them.
 */
@Singleton
class RelayServingSeat @Inject constructor(
    private val api: ApiClient,
    private val onDemand: FfiOnDemandSource,
) : SharePlaneControl {

    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    private val serial = CoroutineScope(SupervisorJob() + Dispatchers.IO.limitedParallelism(1))

    /** The client the running seat was started on — the one to stop it on. */
    @Volatile private var running: FfiNestClient? = null

    override fun start() {
        serial.launch {
            try {
                val (actorId, deviceId) = onDemand.relaySeatInputs()
                    ?: return@launch ShellLog.w(TAG, "no signed-in account: relay serving not started")
                val client = api.nestRpc()
                client.startRelayServing(actorId, deviceId)
                running = client
                ShellLog.i(TAG, STARTED_LINE)
            } catch (e: Exception) {
                ShellLog.w(TAG, "start failed; this device serves no relay ask: ${e.message}")
            }
        }
    }

    override fun stop() {
        serial.launch {
            val client = running
            running = null
            runCatching { client?.stopRelayServing() }
                .onSuccess { if (client != null) ShellLog.i(TAG, STOPPED_LINE) }
                .onFailure { ShellLog.w(TAG, "stop failed: ${it.message}") }
        }
    }

    companion object {
        private const val TAG = "RelayServingSeat"

        /** Logged once the seat runs — the foreground edge's observable outcome. */
        const val STARTED_LINE = "relay serving started: this device answers relay asks from its provider's hosts"

        /** Logged once the seat is down and its announce withdrawn. */
        const val STOPPED_LINE = "relay serving stopped: this device answers no relay ask until it is in front again"
    }
}
