package com.fauna.app.core.conversations

import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiDraftsSyncInterface
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withContext
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * The shell pin for android's launch restore (`conversation-drafts.md` §
 * Persistence → *A restore the outgoing account started fills nothing after an
 * identity change*): [launchDraftsRestore] reads the target manager AND its
 * identity epoch before the load is launched. The library test cannot see this
 * — it pins `restore_drafts_at` refusing a moved epoch, not that the shell read
 * the epoch before the fetch.
 *
 * Mutation-verified: reading `currentTarget()` or `identityEpoch()` inside the
 * `launch`, or after the load returns, turns the switch tests red.
 *
 * No wall clock: the load is held by a gate the test opens (e2e-conventions.md
 * convention 14), on a queued test dispatcher the test steps with `runCurrent`.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class DraftsRestoreCaptureTest {

    /**
     * A QUEUED dispatcher, not an eager one: like `Dispatchers.IO` in production,
     * the launch body does not start until the test lets it (`runCurrent`), so a
     * switch can land between `launchDraftsRestore` returning and the body
     * running. An eager scope would run a capture placed inside `launch` before
     * the switch and pin only the read-after-load half.
     */
    private fun TestScope.hostScope() = CoroutineScope(StandardTestDispatcher(testScheduler))

    /** A manager stand-in: `restoreDraftsAt` refuses a moved epoch, as the real one does. */
    private class FakeTarget(var epoch: ULong) : DraftsRestoreTarget {
        val filled = mutableListOf<ByteArray>()
        override fun identityEpoch(): ULong = epoch
        override suspend fun restoreDraftsAt(epoch: ULong, bytes: ByteArray) {
            if (epoch == this.epoch) filled += bytes
        }

        /** `clear_for_identity_change` advances the epoch. */
        fun clearForIdentityChange() {
            epoch += 1uL
        }
    }

    private class HeldSync(private val reply: ByteArray?) : FfiDraftsSyncInterface {
        val entered = CompletableDeferred<Unit>()
        val gate = CompletableDeferred<Unit>()

        override suspend fun `load`(): ByteArray? {
            entered.complete(Unit)
            // A suspended UniFFI call is not cancelled mid-call.
            withContext(NonCancellable) { gate.await() }
            return reply
        }

        override suspend fun `saveIfChanged`(`snapshot`: ByteArray): Boolean = false
    }

    @Test
    fun aLoadTheOutgoingAccountStartedFillsNothingAfterTheSwitch() = runTest {
        val outgoing = FakeTarget(1uL)
        val incoming = FakeTarget(1uL)
        var current: DraftsRestoreTarget = outgoing
        val sync = HeldSync(byteArrayOf(1, 2, 3))

        launchDraftsRestore(hostScope(), sync) { current }

        // The switch lands before the launch body runs (a capture inside
        // `launch` would see the incoming manager) ...
        outgoing.clearForIdentityChange()
        current = incoming
        runCurrent()
        assertTrue("the load is in flight", sync.entered.isCompleted)
        // ... and the reply arrives after it (a capture after the load would too).
        sync.gate.complete(Unit)
        runCurrent()

        assertTrue("the outgoing manager was filled", outgoing.filled.isEmpty())
        assertTrue("the incoming manager was filled", incoming.filled.isEmpty())
    }

    @Test
    fun aLoadFinishingOnAClearedManagerFillsNothing() = runTest {
        val target = FakeTarget(1uL)
        val sync = HeldSync(byteArrayOf(9))

        launchDraftsRestore(hostScope(), sync) { target }
        runCurrent()
        assertTrue(sync.entered.isCompleted)

        target.clearForIdentityChange()
        sync.gate.complete(Unit)
        runCurrent()

        assertTrue("a cleared manager was filled", target.filled.isEmpty())
    }

    @Test
    fun aLoadWithNoSwitchFillsTheManagerItWasStartedOn() = runTest {
        val target = FakeTarget(1uL)
        val sync = HeldSync(byteArrayOf(4, 5))

        launchDraftsRestore(hostScope(), sync) { target }
        runCurrent()
        sync.gate.complete(Unit)
        runCurrent()

        assertEquals(1, target.filled.size)
        assertTrue(target.filled[0].contentEquals(byteArrayOf(4, 5)))
    }
}
