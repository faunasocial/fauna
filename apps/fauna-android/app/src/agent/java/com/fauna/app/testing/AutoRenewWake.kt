package com.fauna.app.testing

import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.withTimeoutOrNull

/**
 * What ends the certificate auto-renew tick's wait early — android's leg of
 * `fauna_e2e_agent::AUTO_RENEW_WAKE` (`auto_renew_wake`, convention 14), so the
 * tick witness (`test_admin_dns_cert_auto_renew_tick.py`) drives the production
 * loop's own next pass instead of waiting out its six-hour clock. tui's
 * `loop_wake::LoopWake` is the reference shape; linux's is the same.
 *
 * Lives in the debug source set with the rest of the automation surface
 * (convention 15). The production loop (`KidsExcisedApi.startAutoRenewCadence`)
 * reaches it through [TestAgent.autoRenewWaiter], whose shipping twin
 * (`src/noAgent`) hands back a bare `delay` — so a release build's tick runs on
 * its clock alone and carries no notifier.
 *
 * One per loop: each [waiter] call mints a fresh wake and records the loop's own
 * job beside it, so a wake while no loop is running — pre-auth, or after the
 * session's `autoRenewJob` was cancelled — is refused rather than acked into
 * nothing (convention 11), and a departed loop's handle is fired by nothing.
 */
object AutoRenewWake {
    private class Current(val wake: Channel<Unit>, val loop: Job?)

    @Volatile private var current: Current? = null

    /**
     * Mint the calling loop's wait: sleeps `ms`, or until [apply] ends it first.
     * A wake that lands while a pass is running is kept (a conflated channel
     * stores it) and ends the very next wait, so a test never loses its poke to
     * a busy loop.
     */
    suspend fun waiter(): suspend (Long) -> Unit {
        val wake = Channel<Unit>(Channel.CONFLATED)
        current = Current(wake, currentCoroutineContext()[Job])
        return { ms -> withTimeoutOrNull(ms) { wake.receive() } }
    }

    /** `auto_renew_wake`: the refusal sentence, or `null` once the wake landed. */
    fun apply(action: String): String? {
        val now = current
        return if (now != null && now.loop?.isActive != false) {
            now.wake.trySend(Unit)
            null
        } else {
            "$action: no authenticated session, so no auto-renew tick to wake"
        }
    }

    const val AUTO_RENEW_WAKE = "auto_renew_wake"

    /** Test seam: forget the current loop, so a pin starts from "none running". */
    internal fun clearForTest() {
        current = null
    }
}
