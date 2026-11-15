package com.fauna.app.service

import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.app.p2pshare.SharePlaneControls
import com.fauna.app.p2pshare.SharePlaneEdges
import com.fauna.app.p2pshare.SharePlaneHost
import com.fauna.ffi.FfiConnectionState
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.first

/**
 * The process-lifecycle edge of android's share plane (`p2p-shared-set-build.md`
 * § Phone peers — design, decision 2) — `ProcessLifecycleOwner`, the signal
 * [CustodianPushKick] and [AccountRuntimeForeground] already use. `ON_START`
 * starts the plane once the client is connected; `ON_STOP` stops it, so a
 * backgrounded phone holds no socket; an identity change stops the outgoing
 * actor's plane and, still in front, starts the incoming one's. The sequencing
 * is [SharePlaneEdges] (unit-tested); the plane is [SharePlaneHost], whose
 * `storeSafe`/`kids` twin is inert. The same edges start and stop the relay
 * seat ([RelayServingSeat]) in every flavor (`relay-serving.md` § Relay serving
 * → *Which seats hold*).
 *
 * Registered from [com.fauna.app.FaunaApp.onCreate], constructed eagerly there,
 * so the identity closer registers before any session runs.
 */
@Singleton
class SharePlaneForeground @Inject constructor(
    api: ApiClient,
    host: SharePlaneHost,
    relay: RelayServingSeat,
    accountStores: AccountStores,
) : DefaultLifecycleObserver {

    // The relay seat rides the plane's edges in every flavor: the plane's
    // `storeSafe`/`kids` twin is inert, the relay seat is not excisable.
    private val edges = SharePlaneEdges(
        plane = SharePlaneControls(listOf(host, relay)),
        awaitConnected = { api.connectionState.first { it == FfiConnectionState.CONNECTED } },
        scope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
    )

    init {
        accountStores.registerCloser("share-plane") { edges.identityChanged() }
    }

    /** Observe the process foreground lifecycle. Call once, on the main thread. */
    fun register() {
        ProcessLifecycleOwner.get().lifecycle.addObserver(this)
    }

    override fun onStart(owner: LifecycleOwner) = edges.foreground()

    override fun onStop(owner: LifecycleOwner) = edges.background()
}
