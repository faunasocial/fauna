package com.fauna.app.core.events

import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiEventDrafts
import com.fauna.ffi.FfiEventDraftsSyncInterface
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withContext
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * [EventDraftsHost] at the **identity change** — the seam
 * `account-scoping.md` § The scoping taxonomy → the in-memory corollary
 * requires of every background loop that WRITES actor-scoped state: *"a loop
 * that holds no cancellation handle cannot be stopped by any list, so the seam
 * comes before the drop"*, and *"the failure is silent by construction … so it
 * needs a red-first test at the identity change, not an inspection."*
 *
 * The host is a process-wide `@Singleton` holding one identity's draft, and its
 * launch restore is a `fauna.drafts.get` round-trip that can be in flight for
 * the whole request deadline — started, on purpose, *before* `client.connect()`.
 * So the drop and the restore genuinely overlap in production, and what these
 * pin is that A's restore resolving inside B's session reaches nothing: not the
 * rail, and so not B's first keystroke, which would otherwise carry A's summary
 * into a `saveDrafts` sealed under **B's** `BackupKey`.
 *
 * **Why the fake models the FFI call as `NonCancellable`.** Cancelling the
 * restore `Job` is necessary but never sufficient: a coroutine already suspended
 * inside the UniFFI `restoreDrafts()` call is not cancelled mid-call, so it
 * still returns and still tries to assign. A fake that let cancellation stop it
 * would make these tests pass against a `Job`-only fix and prove nothing.
 *
 * **No wall clock anywhere** (e2e-conventions.md convention 14): the host takes
 * its `CoroutineScope`, the tests hand it an unconfined one, and every "has this
 * happened yet" question is answered by opening a gate — a causal barrier, not a
 * settle-sleep. These are negative asserts, which is exactly the case where a
 * sleep gives no verdict at all.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class EventDraftsHostTest {

    /**
     * The host's coroutine scope, on an **unconfined** test dispatcher: every
     * launch runs eagerly to its first real suspension point, and completing a
     * gate resumes the waiter inline. So the gates alone order the test — no
     * `advanceUntilIdle`, and above all no wall clock (e2e-conventions.md
     * convention 14, which these negative asserts are exactly the case for).
     *
     * Deliberately NOT `runTest`'s own `backgroundScope`: the host outlives no
     * test, and keeping its scope out of the test's job tree means a fake left
     * parked inside its `NonCancellable` FFI call cannot hang `runTest`'s
     * end-of-test wait.
     */
    private fun TestScope.hostScope() = CoroutineScope(UnconfinedTestDispatcher(testScheduler))

    private fun draft(summary: String) =
        FfiEventDrafts(summary, "2026-09-02T18:00", "2026-09-02T19:00", "", "")

    /**
     * One session's `FfiEventDraftsSync`, with the round-trip held open by a
     * gate the test opens when it wants the restore to land. [saved] records
     * what this actor's handle was asked to persist — the second half of the
     * leak, and the half that crosses into at-rest state.
     */
    private class FakeEventDraftsSync(
        private val restored: FfiEventDrafts?,
    ) : FfiEventDraftsSyncInterface {
        val entered = CompletableDeferred<Unit>()
        val gate = CompletableDeferred<Unit>()
        val saved = mutableListOf<FfiEventDrafts>()

        override suspend fun `restoreDrafts`(): FfiEventDrafts? {
            entered.complete(Unit)
            // The UniFFI call itself: a suspended foreign call resumes whatever
            // the Job did, so the gate wait must not be cancellable.
            withContext(NonCancellable) { gate.await() }
            return restored
        }

        override suspend fun `saveDrafts`(
            `summary`: String,
            `dtstart`: String,
            `dtend`: String,
            `description`: String,
            `location`: String,
        ) {
            saved += FfiEventDrafts(summary, dtstart, dtend, description, location)
        }
    }

    /**
     * The base case: a restore that resolves after its own session was dropped
     * must not seed the rail. Before the identity seam this assigned
     * unconditionally — `stopDraftsSync` had nulled `_draft`, and the late
     * restore put the departing actor's draft straight back.
     */
    @Test
    fun aRestoreResolvingAfterTheDropDoesNotSeedTheRail() = runTest {
        val scope = hostScope()
        val host = EventDraftsHost(scope)
        val a = FakeEventDraftsSync(draft("A's offsite"))

        host.startDraftsSync(a)
        assertTrue("the restore is in flight", a.entered.isCompleted)

        host.stopDraftsSync()
        a.gate.complete(Unit)

        assertNull("a dropped session's restore reached the rail", host.draft.value)
    }

    /**
     * The one that carries the leak all the way: B signs in with **no** saved
     * draft — the ordinary case — so B's own restore assigns nothing and there
     * is nothing to overwrite a stale value for B's whole session. A's late
     * restore must not become B's resumable draft.
     */
    @Test
    fun theDepartingActorsRestoreDoesNotReachTheNextActorsSession() = runTest {
        val scope = hostScope()
        val host = EventDraftsHost(scope)
        val a = FakeEventDraftsSync(draft("A's offsite"))

        host.startDraftsSync(a)
        assertTrue(a.entered.isCompleted)

        // The switch: drop, then the incoming actor connects.
        host.stopDraftsSync()
        val b = FakeEventDraftsSync(null)
        host.startDraftsSync(b)
        b.gate.complete(Unit)

        // A's round-trip only lands now, well inside B's session.
        a.gate.complete(Unit)

        assertNull(
            "the incoming actor was handed the departing actor's draft",
            host.draft.value,
        )
    }

    /**
     * A new session must clear the rail it inherited rather than trusting the
     * outgoing drop to have run — `startDraftsSync` is reachable without a
     * preceding `stopDraftsSync` (a reconnect), and a restore that returns
     * `null` assigns nothing, so an uncleared rail simply keeps showing what
     * the last session left.
     */
    @Test
    fun startingASessionClearsTheRailItInherited() = runTest {
        val scope = hostScope()
        val host = EventDraftsHost(scope)
        val a = FakeEventDraftsSync(draft("A's offsite"))

        host.startDraftsSync(a)
        a.gate.complete(Unit)
        assertEquals("A's offsite", host.draft.value?.summary)

        val b = FakeEventDraftsSync(null)
        host.startDraftsSync(b)

        assertNull("the incoming session inherited a rail it never wrote", host.draft.value)
    }

    /**
     * The positive control, and it is load-bearing: the guard must reject a
     * *foreign* session's restore, not the restore. A seam that also drops the
     * in-session case would silently retire draft persistence on android —
     * exactly the empty-form bug `test_event_draft_persistence.py` exists for.
     */
    @Test
    fun aRestoreLandingInsideItsOwnSessionStillSeedsTheRail() = runTest {
        val scope = hostScope()
        val host = EventDraftsHost(scope)
        val a = FakeEventDraftsSync(draft("lunch with the auditors"))

        host.startDraftsSync(a)
        a.gate.complete(Unit)

        assertEquals("lunch with the auditors", host.draft.value?.summary)
    }

    /** The drop's own half, which was already correct — pinned so the seam work
     *  cannot regress it. */
    @Test
    fun theDropEmptiesTheRail() = runTest {
        val scope = hostScope()
        val host = EventDraftsHost(scope)
        val a = FakeEventDraftsSync(draft("A's offsite"))

        host.startDraftsSync(a)
        a.gate.complete(Unit)
        assertEquals("A's offsite", host.draft.value?.summary)

        host.stopDraftsSync()

        assertNull(host.draft.value)
    }
}
