package com.fauna.app.ui.viewmodel

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * [rereadOnReconnect] pins `docs/goal/behavior/admin.md` § Errors & edge cases,
 * *Controls wait for their read*: a read that has not answered is read again
 * when the nest connection comes back; an answered one is not (so a draft over
 * it survives). The tui twin is `admin::reconnect_reread_op`'s crate test
 * `a_failed_admin_read_is_asked_again_on_reconnect`.
 */
@ExperimentalCoroutinesApi
class AdminRereadTest {

    @Test
    fun aFailedAdminReadIsAskedAgainOnReconnectAndAnAnsweredOneIsNot() = runTest {
        val ticks = MutableSharedFlow<Unit>()
        var answered = false
        var rereads = 0
        val job = eagerScope(this).rereadOnReconnect(
            ticks,
            answered = { answered },
            reread = { rereads++ },
        )

        // The read failed: the connection coming back asks it again.
        ticks.emit(Unit)
        assertEquals(1, rereads)

        // The re-read answered: the next reconnect asks nothing.
        answered = true
        ticks.emit(Unit)
        assertEquals(1, rereads)

        job.cancel()
    }

    /**
     * `admin.md` *Controls wait for their read*: every entry reads again. The
     * first entry is the view-model's `init` read (no second one); each later
     * composition entry — a return by Back — reads. A trip to the background does
     * not re-compose the page, so it calls nothing (a draft survives).
     */
    @Test
    fun aReturnByBackReadsAgainAndTheFirstEntryDoesNot() {
        var rereads = 0
        val entry = AdminEntryReread { rereads++ }

        entry.onEnter() // first composition: init already read
        assertEquals(0, rereads)

        entry.onEnter() // Back return to the page still on the stack
        assertEquals(1, rereads)

        entry.onEnter() // another Back return
        assertEquals(2, rereads)
    }

    /** The collector runs eagerly, so each `emit` is seen before it returns. */
    private fun eagerScope(scope: kotlinx.coroutines.test.TestScope) =
        kotlinx.coroutines.CoroutineScope(scope.coroutineContext + UnconfinedTestDispatcher(scope.testScheduler))
}
