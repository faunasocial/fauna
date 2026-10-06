package com.fauna.app.testing

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Mirrors linux's `test_agent::profile_nav_target` unit tests exactly (same
 * cases, same constants) — [TestAgent.profileNavTarget] is the android twin of
 * that pure function, both resolving the state-protocol nav-stack `actor_id`
 * against the viewer's own actor id so a self-naming entry normalizes back to
 * the SELF profile rather than rendering the viewer's own profile in OTHER
 * shape (`profile.md` § Layout & flow → Another's profile).
 */
class TestAgentProfileNavTest {

    private val me = "aa11bb22cc33dd44"
    private val other = "ff99ee88dd77cc66"

    @Test
    fun anOtherActorIdResolvesToThatActor() {
        assertEquals(other, TestAgent.profileNavTarget(other, me))
    }

    @Test
    fun theViewersOwnActorIdNormalizesToSelf() {
        assertNull(TestAgent.profileNavTarget(me, me))
    }

    @Test
    fun comparisonIsTrimAndCaseInsensitive() {
        assertNull(TestAgent.profileNavTarget("  ${me.uppercase()} ", me))
        assertEquals(other, TestAgent.profileNavTarget("  $other  ", me))
    }

    @Test
    fun blankActorIdResolvesToSelf() {
        assertNull(TestAgent.profileNavTarget("", me))
        assertNull(TestAgent.profileNavTarget("   ", me))
    }

    @Test
    fun unknownViewerIdentityResolvesTheEntryAsOther() {
        assertEquals(other, TestAgent.profileNavTarget(other, null))
        assertEquals(other, TestAgent.profileNavTarget(other, ""))
    }
}
