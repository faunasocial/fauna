package com.fauna.app.service

import androidx.lifecycle.LifecycleOwner
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiConnectionState
import com.fauna.ffi.FfiCustodianHost
import com.fauna.ffi.FfiCustodianPushHandle
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.atLeastOnce
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * [CustodianPushKick]'s **identity seam** (`account-scoping.md` § The scoping
 * taxonomy → the in-memory corollary: "the background loops that WRITE that
 * state must be retired by the same drop", and "a loop that holds no
 * cancellation handle cannot be stopped by any list, so the seam comes before
 * the drop").
 *
 * This loop is the one android surface where shared Rust states the hazard
 * outright — `CustodianHost::run_push_debounce`: *"this loop does not self-exit
 * when the source disconnects, because with no periodic tick nothing wakes it
 * to notice"* — and the loop's host is built from ONE actor's client, secret
 * and per-actor sealed store. Before the seam it was cancelled only on
 * `onStop`, i.e. on **backgrounding**, which an in-app account switch is not.
 *
 * Every case below is red against that pre-fix shape, and they are deliberately
 * the two DIRECTIONS of the defect, because fixing one by breaking the other is
 * the easy mistake (windows' suite pins the same pair): the outgoing actor's
 * loop must end, **and** the incoming actor must get one of its own.
 *
 * Latency-independence (testing.md convention 14): the loop is a coroutine, so
 * the positive waits are deadline polls against a generous ceiling, never a
 * fixed sleep. The load-bearing negative assert — that the native cancel has
 * *already* happened when the drop returns — needs no wait at all, which is
 * exactly the point of doing it synchronously.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class CustodianPushKickTest {

    private companion object {
        /** A generous ceiling, not an expected latency. */
        const val BUDGET_MS = 10_000L
        const val DEVICE_ID = "aa"
        const val BASE_DIR = "/tmp/fauna-test-custodian"
    }

    private class Rig {
        val api: ApiClient = mock(ApiClient::class.java)
        val storage: SecureStorage = mock(SecureStorage::class.java)
        val stores: AccountStores = mock(AccountStores::class.java)

        /** One fresh host + handle per turn, so "which actor's" is observable. */
        val hosts = mutableListOf<FfiCustodianHost>()
        val handles = mutableListOf<FfiCustodianPushHandle>()

        val kick: CustodianPushKick

        init {
            whenever(api.connectionState)
                .thenReturn(MutableStateFlow(FfiConnectionState.CONNECTED))
            whenever(storage.deviceId).thenReturn(DEVICE_ID)
            whenever(stores.custodianStoreBaseDir()).thenReturn(BASE_DIR)
            runBlocking {
                // Stubbed by VALUE, never with argument matchers: this is a
                // `suspend fun`, so the compiled signature carries a trailing
                // Continuation the matcher count would not account for
                // (`InvalidUseOfMatchersException`). The existing
                // ContentPolicyStoreTest stubs its suspend calls the same way.
                whenever(api.buildCustodianHost(DEVICE_ID, BASE_DIR)).thenAnswer {
                    val host = mock(FfiCustodianHost::class.java)
                    val handle = mock(FfiCustodianPushHandle::class.java)
                    runBlocking { whenever(host.startPushDebounce()).thenReturn(handle) }
                    synchronized(hosts) {
                        hosts.add(host)
                        handles.add(handle)
                    }
                    host
                }
            }
            kick = CustodianPushKick(api, storage, stores)
        }

        fun foreground() = kick.onStart(mock(LifecycleOwner::class.java))

        fun turnsStarted(): Int = synchronized(hosts) { hosts.size }

        /** Deadline poll — the latency-independent form of "the loop got there". */
        fun awaitTurns(n: Int) {
            val deadline = System.currentTimeMillis() + BUDGET_MS
            while (turnsStarted() < n) {
                if (System.currentTimeMillis() > deadline) {
                    throw AssertionError(
                        "custodian push loop reached only ${turnsStarted()} turn(s), wanted $n " +
                            "within ${BUDGET_MS}ms",
                    )
                }
                Thread.sleep(5)
            }
        }
    }

    /**
     * **The seam itself, and the sign-out ordering guarantee it exists for.**
     *
     * When the canonical drop returns, the outgoing actor's Rust loop must
     * ALREADY be cancelled and its handles freed — not merely scheduled to be,
     * on the loop coroutine's own `finally`. Sign-out deletes
     * `<filesDir>/<actor-hex>/` immediately afterwards, and that delete must not
     * race a live sealed-store handle.
     *
     * So this asserts with no wait after the drop: any need to poll here would
     * itself be the bug.
     */
    @Test
    fun theDropCancelsTheOutgoingLoopSynchronously() {
        val rig = Rig()
        rig.foreground()
        rig.awaitTurns(1)
        val outgoingHost = rig.hosts[0]
        val outgoingHandle = rig.handles[0]

        rig.kick.dropForIdentityChange()

        verify(outgoingHandle).cancel()
        verify(outgoingHandle).close()
        verify(outgoingHost).close()
    }

    /**
     * **The inverse direction: the INCOMING actor gets a loop of its own.**
     *
     * The pre-fix code returned early from `onStart` while `job != null`, so
     * after an in-app switch the incoming actor had no push kick for the rest of
     * the foreground session — and a fix that merely cancelled the job at the
     * identity change would keep that defect exactly. A fresh turn, with a
     * freshly built host, is what proves the seam retires an actor rather than
     * the whole loop.
     */
    @Test
    fun theIncomingActorGetsItsOwnLoopAfterTheDrop() {
        val rig = Rig()
        rig.foreground()
        rig.awaitTurns(1)

        rig.kick.dropForIdentityChange()

        rig.awaitTurns(2)
        assertEquals("the incoming actor's host is a NEW one", 2, rig.hosts.size)
        runBlocking { verify(rig.hosts[1]).startPushDebounce() }
        verify(rig.hosts[1], never()).close()
    }

    /**
     * **No accumulation.** windows' equivalent leak spawned one cadence per
     * login, each still acting for a retired identity. Three identity changes in
     * one foreground session must leave exactly ONE live loop: every earlier
     * turn's handle cancelled and its host closed, and only the newest untouched.
     */
    @Test
    fun threeIdentityChangesLeaveOneLiveLoopNotThree() {
        val rig = Rig()
        rig.foreground()
        rig.awaitTurns(1)

        repeat(3) { turn ->
            rig.kick.dropForIdentityChange()
            rig.awaitTurns(turn + 2)
        }

        assertEquals(4, rig.hosts.size)
        for (i in 0 until 3) {
            verify(rig.handles[i]).cancel()
            verify(rig.hosts[i]).close()
        }
        verify(rig.hosts[3], never()).close()
        verify(rig.handles[3], never()).cancel()
    }

    /**
     * **Backgrounding still stops everything.** The identity seam is an
     * *additional* condition, never a replacement: `onStop` must end the loop
     * outright and start no further turn, or a backgrounded process would keep a
     * custodian host alive against the OS's expectations.
     */
    @Test
    fun backgroundingStopsTheLoopAndStartsNoFurtherTurn() {
        val rig = Rig()
        rig.foreground()
        rig.awaitTurns(1)

        rig.kick.onStop(mock(LifecycleOwner::class.java))

        verify(rig.handles[0], atLeastOnce()).cancel()
        verify(rig.hosts[0], atLeastOnce()).close()
        // No new turn may begin: the deadline poll would find a second host.
        val deadline = System.currentTimeMillis() + 500
        while (System.currentTimeMillis() < deadline) {
            assertEquals("backgrounding must not re-arm", 1, rig.turnsStarted())
            Thread.sleep(5)
        }
    }

    /**
     * **The drop registers itself next to its state**, so the canonical drop
     * reaches it without keeping a list — the inner half of android's two-level
     * drop (`ActorScope`'s design note). Pinning the registration, not just the
     * behavior, is what makes "on neither funnel" — android's actual finding —
     * a test failure rather than a silence.
     */
    @Test
    fun theSeamRegistersWithTheCanonicalDrop() {
        val rig = Rig()
        val registered = mockingDetails(rig.stores).invocations
            .filter { it.method.name == "registerCloser" }
            .map { it.arguments[0] }
        assertEquals(listOf<Any>("custodian-push-kick"), registered)
    }
}
