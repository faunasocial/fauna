package com.fauna.app.core

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The work a finished wizard deferred stays with its own account and runs only
 * at that account's `Online` (`onboarding.md` § Wizard exit handling → *The
 * finished wizard holds*): after a failed append-mode finish, `launch_retry`'s
 * switcher may sign the PREVIOUS account in, and its authed shell must not
 * consume the new account's latches ([OnboardingHost]'s `consumePending…`).
 */
class DeferredWorkOwnerTest {

    @Test
    fun theOwnersOwnOnlineRunsTheWork() {
        assertTrue(deferredWorkRunsFor(owner = "bbbb", active = "bbbb"))
        assertTrue(deferredWorkRunsFor(owner = "BBBB", active = "bbbb"))
    }

    @Test
    fun anotherAccountsOnlineLeavesTheWorkHeld() {
        assertFalse(deferredWorkRunsFor(owner = "bbbb", active = "aaaa"))
        assertFalse(deferredWorkRunsFor(owner = "bbbb", active = null))
    }

    @Test
    fun noRecordedOwnerGatesNothing() {
        assertTrue(deferredWorkRunsFor(owner = null, active = "aaaa"))
        assertTrue(deferredWorkRunsFor(owner = null, active = null))
    }
}
