package com.fauna.app.core

import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.testing.TestAgent
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.inOrder
import org.mockito.Mockito.mock
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.robolectric.annotation.Config

/**
 * [ActorScope] is android's ONE canonical actor-scoped drop
 * (`account-scoping.md` § The scoping taxonomy → the in-memory corollary:
 * "exactly one canonical drop per app, and every teardown site calls it with no
 * list of its own").
 *
 * These pin the two properties the rest of the app leans on. Both were red
 * before the class existed: there was no single door at all — a teardown site
 * called `ApiClient.clearAuth()`, or `AccountStores.endActiveAccountSession()`,
 * or (the factory reset and the post-auth identity-change re-entry) only the
 * first, running none of the registered closers.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ActorScopeTest {

    private fun scope(): Triple<ActorScope, ApiClient, AccountStores> {
        val api = mock(ApiClient::class.java)
        val stores = mock(AccountStores::class.java)
        return Triple(ActorScope(api, stores), api, stores)
    }

    /**
     * **The drop runs the closers BEFORE tearing the client down, and this order
     * is load-bearing, not cosmetic.**
     *
     * The closers include the custodian push loop's native teardown, which holds
     * a host built over the very nest client `clearAuth()` closes — and, on
     * sign-out, holds open the very directory `AccountStores.eraseAllAccounts()`
     * is about to delete. Reversing these two lines reinstates the
     * deleted-inode hazard `AccountStores.closeOpenStores` documents.
     */
    @Test
    fun theDropRunsTheClosersBeforeTearingDownTheSession() {
        val (actorScope, api, stores) = scope()

        actorScope.dropActorScopedState()

        inOrder(stores, api).apply {
            verify(stores).endActiveAccountSession()
            verify(api).clearAuth()
        }
    }

    /**
     * **Idempotent.** Sign-out drops and then erases (and the erase closes the
     * open stores again on its own); a crash-and-retry path may drop twice. A
     * second drop must be a clean no-op rather than an error, since teardown
     * sites call this on paths where failing is not an option — the credentials
     * are already gone by then.
     */
    @Test
    fun droppingTwiceIsSafe() {
        val (actorScope, api, stores) = scope()

        actorScope.dropActorScopedState()
        actorScope.dropActorScopedState()

        verify(stores, times(2)).endActiveAccountSession()
        verify(api, times(2)).clearAuth()
    }

    /**
     * **Every drop counts one initiated teardown** on the e2e session
     * generation (`fauna_e2e_agent::SESSION_GENERATION_KEY`). This is the one
     * door every teardown site calls, so counting here — at the top, before
     * anything is torn down — is what makes `assert_no_relaunch`'s "no teardown
     * happened" a proof on android rather than a count of the sites someone
     * remembered to instrument.
     */
    @Test
    fun everyDropBumpsTheSessionGeneration() {
        val (actorScope, _, _) = scope()
        val before = TestAgent.sessionGeneration

        actorScope.dropActorScopedState()
        actorScope.dropActorScopedState()

        assertEquals(before + 2, TestAgent.sessionGeneration)
    }
}
