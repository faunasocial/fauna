package com.fauna.app.service

import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import com.fauna.app.core.ApiClient
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * The android seat of the shared account-runtime foreground signal
 * (`p2p-shared-set-build.md` § Phone peers — design, decision 2: a phone peer
 * runs while the app is in front and *stops* — it does not pause — when the app
 * leaves the foreground, so a backgrounded phone holds no socket).
 *
 * Only the call lives here: `set_account_runtime_foreground` in shared Rust
 * stands the same-account peer leg down before it returns on `false`, runs one
 * binding pass on `true`, and remembers the last value process-wide. Apple's
 * twin is `FaunaClient`'s lifecycle hooks over `APIClient.setAccountRuntimeForeground`.
 *
 * Registered from [com.fauna.app.FaunaApp.onCreate], the [CustodianPushKick]
 * shape. The scope runs one call at a time (parallelism 1), so a stop followed
 * by a start reach shared Rust in lifecycle order.
 *
 * The edges alone do not cover every launch, so [seed] tells shared Rust the
 * process's real place before the runtime starts: a process that never started
 * an activity never fires [onStart] or [onStop], and an edge that lands before
 * [ApiClient] holds a client is dropped by its no-client guard — either way the
 * runtime would start on the Rust default, in front, and bind the same-account
 * peer leg with the app not open (`account-sync-plane.md` § Implementation
 * status today).
 */
@Singleton
class AccountRuntimeForeground @Inject constructor(
    private val api: ApiClient,
) : DefaultLifecycleObserver {

    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO.limitedParallelism(1))

    /** Observe the process foreground lifecycle. Call once, on the main thread. */
    fun register() {
        ProcessLifecycleOwner.get().lifecycle.addObserver(this)
    }

    override fun onStart(owner: LifecycleOwner) {
        scope.launch { api.setAccountRuntimeForeground(true) }
    }

    override fun onStop(owner: LifecycleOwner) {
        scope.launch { api.setAccountRuntimeForeground(false) }
    }

    companion object {
        /**
         * Held by every send of the signal ([ApiClient.setAccountRuntimeForeground]
         * and [seed]). The process lifecycle moves its state BEFORE it dispatches
         * [onStart]/[onStop], so a seed that reads the state under this lock is
         * always followed by the send of any edge it did not see.
         */
        internal val signalLock = Mutex()

        /** Whether this process has an activity started — the app is in front. */
        fun processInFront(): Boolean =
            ProcessLifecycleOwner.get().lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)

        /**
         * Send the process's place now, before the account runtime starts —
         * read and sent under [signalLock], so a later edge still wins.
         */
        suspend fun seed(
            inFront: () -> Boolean = ::processInFront,
            send: suspend (Boolean) -> Unit,
        ) {
            signalLock.withLock { send(inFront()) }
        }
    }
}
