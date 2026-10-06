package com.fauna.app.ui.viewmodel

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_conversations.CrossGroupEviction
import uniffi.fauna_conversations.EvictionFailure
import uniffi.fauna_conversations.UnreachableSeat
import uniffi.fauna_conversations.UnreachableSeatClass

/**
 * Coverage for [removeResultMessage] — the free function [MemberReviewVM.remove]
 * composes its `error-message` text from, mirrors linux
 * `member_review.rs::remove_result_message`'s own test set. A free function
 * (not a VM member) so it is directly testable with no Hilt/FFI/coroutines in
 * play.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class MemberReviewVMTest {

    private val context = ApplicationProvider.getApplicationContext<android.app.Application>()

    private fun eviction(
        evicted: List<String> = emptyList(),
        failed: List<EvictionFailure> = emptyList(),
        unreachable: List<UnreachableSeat> = emptyList(),
    ) = CrossGroupEviction(evicted = evicted, failed = failed, unreachable = unreachable)

    /** A complete eviction (nothing failed, nothing unreachable) leaves
     *  nothing to say — the row is expected to drop out on the next re-read. */
    @Test
    fun aCompleteEvictionLeavesNoMessage() {
        assertNull(removeResultMessage(context, eviction(), "someone"))
        assertNull(removeResultMessage(context, eviction(evicted = listOf("t1", "t2")), "someone"))
    }

    /** A partial eviction with a failed group names how far it got. */
    @Test
    fun aPartialEvictionWithFailuresIsReported() {
        val outcome = eviction(
            evicted = listOf("t1"),
            failed = listOf(EvictionFailure(thread = "t2", reason = "network error")),
        )
        val msg = removeResultMessage(context, outcome, "alice")
        assertTrue(msg!!.contains("alice"))
        assertTrue(msg.contains("1"))
    }

    /** No failures and no unreachable seats, but also nothing evicted (the
     *  person was already out of every group on this device) — the
     *  "not in any of your group conversations" wording. */
    @Test
    fun nothingEvictedAndNothingUnreachableIsReportedAsNoneHere() {
        val outcome = eviction(unreachable = listOf(UnreachableSeat(channelHex = "ab", `class` = UnreachableSeatClass.FOLDER_CHANNEL)))
        val msg = removeResultMessage(context, outcome, "bob")
        assertTrue(msg!!.contains("bob"))
    }

    /** An unreachable folder-channel seat is named explicitly. */
    @Test
    fun folderSeatsAreCalledOut() {
        val outcome = eviction(
            unreachable = listOf(
                UnreachableSeat(channelHex = "ab", `class` = UnreachableSeatClass.FOLDER_CHANNEL),
                UnreachableSeat(channelHex = "cd", `class` = UnreachableSeatClass.FOLDER_CHANNEL),
            ),
        )
        val msg = removeResultMessage(context, outcome, "carol")
        assertTrue(msg!!.contains("2"))
    }

    /** An unreachable not-yet-synced chat-group seat is named explicitly, and
     *  distinctly from a folder seat. */
    @Test
    fun unsyncedSeatsAreCalledOutDistinctlyFromFolderSeats() {
        val outcome = eviction(
            unreachable = listOf(
                UnreachableSeat(channelHex = "ab", `class` = UnreachableSeatClass.CHAT_GROUP_NO_THREAD_HERE),
            ),
        )
        val msg = removeResultMessage(context, outcome, "dave")
        assertTrue(msg!!.contains("1"))
    }

    /** Both unreachable classes on the same outcome both surface, not just
     *  the first one checked. */
    @Test
    fun bothUnreachableClassesSurfaceTogether() {
        val outcome = eviction(
            unreachable = listOf(
                UnreachableSeat(channelHex = "ab", `class` = UnreachableSeatClass.FOLDER_CHANNEL),
                UnreachableSeat(channelHex = "cd", `class` = UnreachableSeatClass.CHAT_GROUP_NO_THREAD_HERE),
            ),
        )
        val msg = removeResultMessage(context, outcome, "eve")!!
        val folderMsg = context.getString(com.fauna.app.R.string.settings_recovery_kit_review_remove_folder_seats)
        val unsyncedMsg = context.getString(com.fauna.app.R.string.settings_recovery_kit_review_remove_unsynced_seats)
        // Both templates' fixed (non-placeholder) text must appear.
        assertTrue(msg.contains(folderMsg.substringBefore("{seats}").trim()))
        assertTrue(msg.contains(unsyncedMsg.substringBefore("{seats}").trim()))
    }
}
