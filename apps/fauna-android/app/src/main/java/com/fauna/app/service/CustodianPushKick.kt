package com.fauna.app.service

import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.ShellLog
import com.fauna.ffi.FfiConnectionState
import com.fauna.ffi.FfiCustodianHost
import com.fauna.ffi.FfiCustodianPushHandle
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/**
 * Foreground push-kick for this device's **client-device backup custodian** —
 * the low-latency sibling of the periodic [CustodianHostWorker]
 * (`docs/goal/behavior/backup-restore.md`
 * § Background Tasks; `docs/goal/ui/backups.md` § Third destination kind).
 * WorkManager owns the 15-min background cadence; this holder owns the
 * foreground wake.
 *
 * **Where the debounce lives: shared Rust.** This is a thin trigger only. The
 * subscribe-to-pushes + coalescing debounce + pull pass is
 * `CustodianPull::run_push_debounce` (`libs/fauna-sync-engine`), reached through
 * [com.fauna.ffi.FfiCustodianHost.startPushDebounce] — the **push-only** loop,
 * with no periodic tick, because WorkManager already drives the period and
 * `run_forever` would double it. iOS's foreground scheduler consumes the same
 * FFI. The debounce window and the push subscriptions are shared Rust's, not
 * this shell's, so the actor filter deciding *whose* pushes wake a device is the
 * same one every platform gets.
 *
 * **Cancelling is required, not hygiene.** The push-only loop has no periodic
 * tick, so nothing else would ever wake it to notice the source went away —
 * shared Rust says so at the loop itself ("this loop does not self-exit when
 * the source disconnects"). So the handle is cancelled *and* closed, and the
 * host closed after it, at **both** ends of its life.
 *
 * **Gated on foreground ∧ connected — and scoped to ONE actor.** This is the
 * pair, and getting only the first half right was the bug
 * (`account-scoping.md` § Implementation status → the `android (in-memory)`
 * ledger row). `ProcessLifecycleOwner` gives the foreground signal and the loop
 * additionally waits for a live WS connection; but a host is built from ONE
 * actor's client, secret and per-actor sealed store, and **an in-app account
 * switch is not a backgrounding** — it tears down and rebuilds in the
 * foreground. Cancelling only on `onStop` therefore left the *outgoing* actor's
 * host, its push subscriptions and its open sealed store running into the
 * incoming account's session, while the incoming actor got no push kick at all
 * for the rest of that foreground session. The identity seam below is what
 * makes the actor half real: a drop cannot stop a loop that holds no
 * cancellation handle, so this class holds one and retires it at the identity
 * change — foreground/background remains an *additional* condition, never the
 * identity seam.
 *
 * **One host per (foreground session × actor).** The push subscriptions live on
 * the persistent `NestClient`'s broker and survive reconnects, so there is no
 * rebuild per reconnect — only per actor.
 *
 * **Not an enrolled custodian → this loop simply idles**, exactly as its mail
 * sibling idles on zero configured destinations: the builder returns null and
 * there is nothing to hold for this actor.
 *
 * Registered from [com.fauna.app.FaunaApp.onCreate] via Hilt; see that call site
 * for the Robolectric-safe wrapping.
 */
@Singleton
class CustodianPushKick @Inject constructor(
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
    private val accountStores: AccountStores,
) : DefaultLifecycleObserver {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var job: Job? = null

    /**
     * The identity seam. The loop's native resources are held HERE rather than
     * only as locals inside [serveOneActor], so [dropForIdentityChange] can end
     * the Rust loop **synchronously** on the caller's thread — which is what a
     * sign-out needs, since [AccountStores.eraseAllAccounts] deletes the very
     * directory this store has open, and a teardown that merely rode the
     * coroutine's `finally` would still be in flight when the delete ran.
     *
     * `@Volatile`: written from the loop's IO dispatcher, read by the drop on
     * whatever thread the teardown site runs on.
     */
    @Volatile
    private var handle: FfiCustodianPushHandle? = null

    @Volatile
    private var host: FfiCustodianHost? = null

    /**
     * Retires the actor currently being served, so [serveOneActor] returns and
     * the outer loop re-arms for the incoming one. Distinct from cancelling
     * [job], which means "stop entirely" (backgrounded).
     */
    @Volatile
    private var actorRetired: CompletableDeferred<Unit> = CompletableDeferred()

    init {
        // The drop registers next to the state it drops (`account-scoping.md`
        // § the in-memory corollary). This class is constructed eagerly from
        // FaunaApp.onCreate, so the registration is not subject to the lazy
        // construction that makes a registry unreliable for surfaces nobody has
        // injected yet.
        accountStores.registerCloser("custodian-push-kick") { dropForIdentityChange() }
    }

    /** Observe the process foreground lifecycle. Call once, on the main thread. */
    fun register() {
        ProcessLifecycleOwner.get().lifecycle.addObserver(this)
    }

    override fun onStart(owner: LifecycleOwner) {
        // Foregrounded. Guard against a redundant start (ProcessLifecycleOwner
        // debounces fg/bg, but the guard makes a double-onStart a no-op).
        if (job != null) return
        job = scope.launch { runWhileForegrounded() }
    }

    override fun onStop(owner: LifecycleOwner) {
        // Backgrounded: stop entirely. Cancelling the job runs `serveOneActor`'s
        // finally, ending the Rust push loop and closing the native handle +
        // sealed store; the synchronous teardown below makes that immediate
        // rather than merely scheduled.
        job?.cancel()
        job = null
        closeNativeHandles()
    }

    /**
     * **The identity seam** — invoked by the canonical drop
     * ([com.fauna.app.core.ActorScope.dropActorScopedState]) through this
     * class's registered closer, on an account switch, sign-out,
     * delete-account, factory reset or post-auth identity-change re-entry.
     *
     * Synchronously ends the outgoing actor's Rust loop and frees its native
     * handles — `cancel()` is sync and idempotent on both sides of the FFI, and
     * doing it here rather than in the coroutine's `finally` is what lets a
     * sign-out's erase run against a store that is already closed. Then retires
     * the actor, which wakes [serveOneActor] so the outer loop takes a fresh
     * turn for the incoming actor: the loop parks on the connection wait again
     * and re-reads the device id and store dir, so nothing of the outgoing
     * actor's is carried across.
     *
     * Deliberately does NOT touch [job]: an identity change is not a
     * backgrounding, and cancelling the job here would leave the incoming actor
     * unserved for the rest of the foreground session — the exact inverse
     * defect this seam exists to prevent.
     */
    fun dropForIdentityChange() {
        closeNativeHandles()
        actorRetired.complete(Unit)
    }

    /**
     * Free the Rust loop + its handles, synchronously and idempotently.
     * `cancel()` ends the loop, which is what releases its push subscriptions;
     * `close()` frees the handle Arc (its `Drop` also cancels); the host
     * `close()` drops the sealed store.
     */
    private fun closeNativeHandles() {
        val h = handle
        val s = host
        handle = null
        host = null
        runCatching { h?.cancel() }
        runCatching { h?.close() }
        runCatching { s?.close() }
    }

    /**
     * Serve one actor after another for as long as the process is foregrounded.
     * Each turn ends when that actor is retired ([dropForIdentityChange]); the
     * whole loop ends when the job is cancelled (backgrounded).
     *
     * The turn-per-actor shape is what keeps exactly ONE live push loop: there
     * is one [job], and within it one turn at a time, so N logins in a
     * foreground session leave one loop rather than N.
     */
    private suspend fun runWhileForegrounded() {
        while (true) {
            actorRetired = CompletableDeferred()
            serveOneActor()
        }
    }

    /**
     * Hold one custodian host + push-debounce loop for ONE actor. Returns when
     * that actor is retired; throws `CancellationException` out if the process
     * is backgrounded, which ends the outer loop.
     */
    private suspend fun serveOneActor() {
        val retired = actorRetired

        // Wait for a live connection — immediate if already connected, else once
        // this actor logs in. Cancelled cleanly if backgrounded first.
        api.connectionState.first { it == FfiConnectionState.CONNECTED }

        // The identity may have changed while we waited (a switch performed
        // before the incoming account finished connecting). Yield the turn
        // rather than building a host for an actor already retired; the outer
        // loop's next turn re-reads everything for whoever is active now.
        if (retired.isCompleted) return

        // No device id yet (pre-onboarding) → no registry row can name this
        // device, so there is nothing to match on. Wait out this actor rather
        // than spinning: the next identity change starts the next turn.
        val deviceId = secureStorage.deviceId ?: return retired.await()

        val built = try {
            api.buildCustodianHost(deviceId, accountStores.custodianStoreBaseDir())
        } catch (e: Exception) {
            ShellLog.w(TAG, "build custodian host failed: ${e.message}")
            null
        } ?: return retired.await()

        val started: FfiCustodianPushHandle = try {
            host = built
            built.startPushDebounce()
        } catch (e: Exception) {
            ShellLog.w(TAG, "start custodian push-debounce failed: ${e.message}")
            closeNativeHandles()
            return retired.await()
        }
        handle = started

        try {
            // Hold the loop until this actor is retired, or the process is
            // backgrounded (which cancels us out of the await).
            retired.await()
        } finally {
            // Idempotent with the seam's own synchronous teardown: whichever
            // runs first frees the handles and the other is a no-op. This arm is
            // what covers the backgrounding path, where no seam fires.
            closeNativeHandles()
        }
    }

    companion object {
        private const val TAG = "CustodianPushKick"
    }
}
