package com.fauna.app.service

import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ShellLog
import com.fauna.ffi.FfiConnectionState
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import uniffi.fauna_client_pair.LinkedNestsAction

/**
 * The blessed-grant auto-renew loop's android seat (`nests.md` § Expiry /
 * renewal → *Duration and blessing*): phones dispatch `AutoRenew` at app
 * foreground only — no background interval (tui/linux/web add one every
 * `AUTO_RENEW_CHECK_SECS`). The sweep itself is shared Rust
 * (`LinkedNestsMachine::auto_renew`): it renews only on a nest whose PROVEN
 * identity is blessed, each due grant by its own mint-time length, and
 * degrades silently to renewing less. This class only schedules.
 *
 * Constructed and registered from [com.fauna.app.FaunaApp.onCreate], the
 * [CustodianPushKick] shape.
 */
@Singleton
class NestsAutoRenew @Inject constructor(
    private val api: ApiClient,
) : DefaultLifecycleObserver {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var job: Job? = null

    /** Observe the process foreground lifecycle. Call once, on the main thread. */
    fun register() {
        ProcessLifecycleOwner.get().lifecycle.addObserver(this)
    }

    override fun onStart(owner: LifecycleOwner) {
        job?.cancel()
        job = scope.launch {
            // Immediate if already connected, else once the signed-in client
            // connects; cancelled cleanly if backgrounded first.
            api.connectionState.first { it == FfiConnectionState.CONNECTED }
            val machine = api.buildLinkedNestsMachine() ?: return@launch
            runCatching { machine.dispatch(LinkedNestsAction.AutoRenew) }
                .onFailure { ShellLog.w("NestsAutoRenew", "auto-renew pass failed: ${it.message}") }
        }
    }

    override fun onStop(owner: LifecycleOwner) {
        job?.cancel()
        job = null
    }
}
