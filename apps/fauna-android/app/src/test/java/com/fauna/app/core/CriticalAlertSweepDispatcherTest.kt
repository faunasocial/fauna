package com.fauna.app.core

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Pins [CriticalAlertSweepDispatcher]'s loop/one-pass split — the android twin
 * of windows' `CriticalAlertsSweepTests` and apple's
 * `CriticalAlertsLoopClaimTests` (`critical-alerts.md` § Implementation status
 * today). Red before the dispatcher existed: the post-auth hook only ever
 * started a loop (`LaunchedEffect(Unit)`), and the e2e same-actor `session`
 * patch tore the running one down (`clearAll`) while recomposing nothing, so a
 * re-established session had neither loop nor pass.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class CriticalAlertSweepDispatcherTest {

    private class Probe {
        var loops = 0
        var passes = 0
        val release = CompletableDeferred<Unit>()
        val loop: suspend () -> Unit = { loops += 1; release.await() }
        val pass: suspend () -> Unit = { passes += 1 }
    }

    private fun dispatcher() =
        CriticalAlertSweepDispatcher(CoroutineScope(UnconfinedTestDispatcher()))

    @Test
    fun firstSignInStartsTheLoop() {
        val d = dispatcher()
        val p = Probe()
        d.start("alice@nest", p.loop, p.pass)
        assertEquals(1, p.loops)
        assertEquals(0, p.passes)
    }

    @Test
    fun sameIdentityReestablishRunsOnePassNotASecondLoop() {
        val d = dispatcher()
        val p = Probe()
        d.start("alice@nest", p.loop, p.pass)
        d.start("alice@nest", p.loop, p.pass)
        d.start("alice@nest", p.loop, p.pass)
        assertEquals("one loop for the identity, however often it re-authenticates", 1, p.loops)
        assertEquals(2, p.passes)
    }

    @Test
    fun identityChangeStartsALoopOfItsOwn() {
        val d = dispatcher()
        val p = Probe()
        d.start("alice@nest", p.loop, p.pass)
        d.start("bob@nest", p.loop, p.pass)
        d.start("alice@other-nest", p.loop, p.pass)
        assertEquals(3, p.loops)
        assertEquals(0, p.passes)
    }

    @Test
    fun forgetThenSameIdentityStartsAFreshLoop() {
        // `clearAll` (sign-out / switch / factory reset) stops the old loop, so
        // the next sign-in as the SAME actor must not take the one-pass arm.
        val d = dispatcher()
        val p = Probe()
        d.start("alice@nest", p.loop, p.pass)
        d.forget()
        d.start("alice@nest", p.loop, p.pass)
        assertEquals(2, p.loops)
        assertEquals(0, p.passes)
    }

    @Test
    fun anEndedLoopNoLongerCoversItsIdentity() {
        val d = dispatcher()
        val p = Probe()
        d.start("alice@nest", p.loop, p.pass)
        p.release.complete(Unit)
        d.start("alice@nest", p.loop, p.pass)
        assertEquals("a loop that ended (failed connect) must be replaced, not shadowed", 2, p.loops)
        assertEquals(0, p.passes)
    }

    @Test
    fun aDepartedIdentitysLoopEndingLateDoesNotClearItsSuccessor() {
        val d = dispatcher()
        val old = Probe()
        val new = Probe()
        d.start("alice@nest", old.loop, old.pass)
        d.forget()
        d.start("bob@nest", new.loop, new.pass)
        old.release.complete(Unit)
        d.start("bob@nest", new.loop, new.pass)
        assertEquals(1, new.loops)
        assertEquals("bob's loop is still the live one", 1, new.passes)
    }
}
