package com.fauna.app.core.push

import android.content.Context
import com.fauna.app.core.NotificationHelper
import com.fauna.app.core.ShellLog
import com.fauna.app.testing.TestAgent
import com.fauna.ffi.FfiNestClient
import com.fauna.ffi.FfiPushEndpoint
import com.fauna.ffi.FfiPushRegistration
import com.fauna.ffi.pushClearOptIn
import com.fauna.ffi.pushIntent
import com.fauna.ffi.pushPayloadParse
import dagger.hilt.android.qualifiers.ApplicationContext
import java.io.File
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull

/**
 * Push notifications on android — the `web-push` transport through a
 * UnifiedPush distributor (`common.md` § Push Notifications → *Transports*):
 * the distributor's endpoint is a Web Push endpoint, so the nest dials it as it
 * dials a browser's.
 *
 * Everything stateful is the shared `fauna_client_push::registration` machine
 * behind [FfiPushRegistration] (the install intent bit, the which-actor record,
 * the leave-shape drops — `common.md` § Push Notifications → *Registration*).
 * This class names android's inputs to it, as windows' `PushSession` and tui's
 * `push.rs` name theirs:
 *
 *  - **the device id** — `SessionAccount.resolveDeviceId()`, the id this
 *    connection announces, so the row and the announce are one value;
 *  - **the intent store** — one file in `filesDir`, install-scoped: no
 *    sign-out erase touches it (`account-scoping.md` class 2);
 *  - **the subscription** — the endpoint the distributor hands
 *    [FaunaPushService], which differs from the other apps only in arriving
 *    later, in a callback: [enable] waits for it, and one the distributor
 *    sends on its own (a re-register, a changed endpoint) re-arms the row.
 */
@Singleton
class PushHost @Inject constructor(
    @ApplicationContext private val context: Context,
    private val notificationHelper: NotificationHelper,
) {
    /** Why a toggle-on did not opt this install in. */
    sealed class EnableFailure {
        /** This phone has no UnifiedPush distributor installed. */
        object NoDistributor : EnableFailure()
        /** No signed-in session to register through. */
        object NoSession : EnableFailure()
        /** The distributor or the nest refused, or did not answer in time. */
        data class Failed(val message: String?) : EnableFailure()
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** The install-scoped intent file — what the Settings toggle renders. */
    val intentPath: String = File(context.filesDir, INTENT_FILE_NAME).path

    private val distributor: PushDistributor
        get() = TestAgent.pushDistributor ?: UnifiedPushDistributor

    /** This session's registration; `null` while signed out. */
    @Volatile private var registration: FfiPushRegistration? = null
    @Volatile private var client: FfiNestClient? = null

    /** A toggle-on waiting for the distributor's endpoint. */
    @Volatile private var pendingEndpoint: CompletableDeferred<FfiPushEndpoint>? = null

    /** The stored bit — what the toggle shows. Local, so it paints before any
     *  connection. */
    fun optedIn(): Boolean = pushIntent(intentPath).optedIn

    /**
     * At sign-in (launch, switch-in): announce this device on the session's
     * connection, and when this install opted in ask the distributor for the
     * endpoint again — its answer re-arms the row, never opting the install in.
     * Best-effort: a failure is logged and the session proceeds.
     */
    suspend fun onSessionStart(nest: FfiNestClient, actorId: String?, deviceId: String?) {
        if (actorId.isNullOrEmpty() || deviceId.isNullOrEmpty()) {
            ShellLog.w(TAG, "no device id for this account; not announcing")
            return
        }
        try {
            val reg = nest.pushRegistration(intentPath, actorId, deviceId)
            registration = reg
            client = nest
            reg.announcePresence()
            if (reg.intent().optedIn) requestEndpoint(nest)
        } catch (e: Exception) {
            ShellLog.w(TAG, "session start: ${e.message}")
        }
    }

    /**
     * The session is going (`ApiClient.clearAuth`): nothing re-arms through it
     * from here. Returns its registration when [identityEnds] — the leave
     * gesture (switch-out, sign-out, reset) whose row [dropLeavingActorRow]
     * then drops over the leaving client, before that client disconnects; a
     * rebuild that keeps the same identity leaves the row alone.
     */
    fun onSessionEnd(identityEnds: Boolean): FfiPushRegistration? {
        val leaving = registration
        registration = null
        client = null
        pendingEndpoint?.cancel()
        pendingEndpoint = null
        return if (identityEnds) leaving else null
    }

    /** The user's Enable: get an endpoint from the distributor, then register
     *  it and set the bit. `null` on success. */
    suspend fun enable(): EnableFailure? {
        val reg = registration ?: return EnableFailure.NoSession
        val nest = client ?: return EnableFailure.NoSession
        if (!distributor.ensureDistributor(context)) return EnableFailure.NoDistributor
        val waiting = CompletableDeferred<FfiPushEndpoint>()
        pendingEndpoint = waiting
        return try {
            requestEndpoint(nest)
            val endpoint = withTimeout(ENDPOINT_WAIT_MS) { waiting.await() }
            reg.enable(endpoint)
            null
        } catch (e: Exception) {
            ShellLog.w(TAG, "enable failed: ${e.message}")
            EnableFailure.Failed(e.message)
        } finally {
            if (pendingEndpoint === waiting) pendingEndpoint = null
        }
    }

    /** The user's Disable: the bit clears first (off stays off with no
     *  connection), then the row goes and the distributor forgets the
     *  endpoint. */
    suspend fun disable() {
        try {
            val reg = registration
            if (reg != null) reg.disable() else pushClearOptIn(intentPath)
        } finally {
            runCatching { distributor.unregister(context) }
        }
    }

    /**
     * A leave gesture (switch-out, sign-out, reset): drop the leaving actor's
     * row, issued on the leaving session's own client before it disconnects —
     * the last point its authority is in hand (`common.md` § Registration, the
     * leave-shapes). Bounded and best-effort — the teardown proceeds whatever
     * happens; the bit is untouched, so the next sign-in re-arms.
     */
    suspend fun dropLeavingActorRow(reg: FfiPushRegistration) {
        val done = withTimeoutOrNull(LEAVE_BOUND_MS) {
            try {
                reg.dropActorRow()
            } catch (e: Exception) {
                ShellLog.w(TAG, "dropping the leaving account's row failed: ${e.message}")
            }
        }
        if (done == null) ShellLog.w(TAG, "dropping the leaving account's row did not finish in time; leaving anyway")
    }

    /** [FaunaPushService.onNewEndpoint]: a toggle-on waiting for it takes it;
     *  any other is the distributor's own (re-)delivery, which re-arms. */
    fun onEndpoint(endpoint: FfiPushEndpoint) {
        val waiting = pendingEndpoint
        if (waiting != null && waiting.complete(endpoint)) return
        val reg = registration ?: return
        scope.launch {
            try {
                reg.rearm(endpoint)
            } catch (e: Exception) {
                ShellLog.w(TAG, "re-arm failed: ${e.message}")
            }
        }
    }

    /** [FaunaPushService.onRegistrationFailed]: a waiting toggle-on fails. */
    fun onEndpointRefused(reason: String) {
        pendingEndpoint?.completeExceptionally(IllegalStateException(reason))
    }

    /** [FaunaPushService.onMessage]: show the decrypted message's strings. */
    fun onMessage(content: ByteArray) {
        val payload = pushPayloadParse(content)
        notificationHelper.postPushNotification(payload.title, payload.body)
    }

    private suspend fun requestEndpoint(nest: FfiNestClient) {
        if (!distributor.ensureDistributor(context)) {
            ShellLog.w(TAG, "no UnifiedPush distributor installed; nothing to re-arm through")
            return
        }
        distributor.register(context, nest.push().vapidKey())
    }

    companion object {
        private const val TAG = "Push"

        /** The intent file's name under `filesDir` — the e2e bridge deletes it
         *  on a seeded (fresh-install) launch. */
        const val INTENT_FILE_NAME = "push-intent.cbor"

        /** How long a toggle-on waits for the distributor's endpoint. */
        private const val ENDPOINT_WAIT_MS = 20_000L

        /** How long a leave gesture waits on its drop (windows' bound). */
        private const val LEAVE_BOUND_MS = 3_000L
    }
}
