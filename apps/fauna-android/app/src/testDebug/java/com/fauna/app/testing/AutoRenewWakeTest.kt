package com.fauna.app.testing

import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The tier_1 pin for android's `auto_renew_wake` leg ([AutoRenewWake]) — what
 * `test_admin_dns_cert_auto_renew_tick.py` pokes, proven without the emulator:
 * the wake ends the loop's six-hour wait, one landing mid-pass is kept for the
 * next wait, and a wake with no loop running — never minted, or the loop's job
 * cancelled with its session — is refused by name, never acked (convention 11).
 */
class AutoRenewWakeTest {
    private val sixHoursMs = 6L * 60 * 60 * 1000

    @Before
    fun clear() = AutoRenewWake.clearForTest()

    @Test
    fun aWakeWithNoTickRunningIsRefusedByNameNeverAcked() {
        val shown = AutoRenewWake.apply(AutoRenewWake.AUTO_RENEW_WAKE)
        assertNotNull("no tick runs in this JVM, so the wake must be refused", shown)
        assertTrue(shown!!, shown.contains("auto_renew_wake") && shown.contains("no auto-renew tick"))
    }

    @Test
    fun aWakeEndsTheWaitAndAnEarlyOneIsKept() = runBlocking {
        val loop = async(start = CoroutineStart.UNDISPATCHED) {
            val wait = TestAgent.autoRenewWaiter()
            // Mid-pass: the wake lands before the wait begins, and is kept.
            assertNull(AutoRenewWake.apply(AutoRenewWake.AUTO_RENEW_WAKE))
            wait(sixHoursMs)
            // Mid-wait: a wake from outside ends it.
            wait(sixHoursMs)
        }
        yield()
        assertNull(AutoRenewWake.apply(AutoRenewWake.AUTO_RENEW_WAKE))
        withTimeout(5_000) { loop.await() }
    }

    @Test
    fun aCancelledLoopsWakeIsRefused() = runBlocking {
        val loop = async(start = CoroutineStart.UNDISPATCHED) {
            val wait = TestAgent.autoRenewWaiter()
            wait(sixHoursMs)
        }
        loop.cancel()
        val shown = AutoRenewWake.apply(AutoRenewWake.AUTO_RENEW_WAKE)
        assertNotNull("the cancelled session's tick must not be acked", shown)
    }
}
