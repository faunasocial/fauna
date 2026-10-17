package com.fauna.app.ui.screen.conversations

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_conversations.BodyFormat
import uniffi.fauna_conversations.ConversationsManager
import uniffi.fauna_conversations.MessageBadges
import uniffi.fauna_conversations.Rail
import uniffi.fauna_conversations.RailInboundMessage
import uniffi.fauna_conversations.TypedAddress

/**
 * Leaving the thread for the list closes it (`docs/goal/ui/conversations.md` §
 * State & data shape → *When a thread is read*): the shared manager reads
 * whatever arrives in the SELECTED thread, so a single-pane shell that never
 * clears the selection on the way back reads every later arrival on sight and
 * the row never shows `dm-unread-indicator` again. Drives a REAL manager
 * (UniFFI over host JNA) through the same composable the detail screen mounts.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ClearSelectionOnLeaveTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private var nextStampMs = System.currentTimeMillis() + 1_000

    private fun inject(manager: ConversationsManager, sender: String, body: String) {
        nextStampMs += 1_000
        manager.injectInboundForTest(
            RailInboundMessage(
                rail = Rail.FAUNA_MLS,
                sender = TypedAddress.Fauna(sender, ByteArray(32)),
                recipients = listOf(TypedAddress.Fauna("me@self-nest.test", ByteArray(32))),
                subject = null,
                body = body,
                bodyFormat = BodyFormat.MARKDOWN,
                timestampMs = nextStampMs,
                messageId = "msg-${java.util.UUID.randomUUID()}",
                inReplyTo = null,
                attachments = emptyList(),
                badges = MessageBadges(
                    encrypted = false,
                    signed = false,
                    verified = false,
                    contentWarning = null,
                ),
                legalTakedownRef = null,
                planeRef = null,
            ),
        )
    }

    private fun unreadOf(manager: ConversationsManager, threadId: String): UInt =
        manager.snapshot().threads.single { it.threadId == threadId }.unreadCount

    @Test
    fun aThreadLeftForTheListIsNoLongerBeingRead() {
        val manager = ConversationsManager().also { it.installMockBackendsForTest() }
        inject(manager, "alpha@self-nest.test", "first message")
        val thread = manager.snapshot().threads.single().threadId

        var detailShown by mutableStateOf(true)
        manager.selectThread(thread)
        composeTestRule.setContent {
            if (detailShown) ClearSelectionOnLeave(manager)
        }
        composeTestRule.waitForIdle()

        inject(manager, "alpha@self-nest.test", "arrives while the thread is open")
        assertEquals("an arrival in the open thread is read on sight", 0u, unreadOf(manager, thread))

        composeTestRule.runOnUiThread { detailShown = false }
        composeTestRule.waitForIdle()

        assertEquals("leaving the thread screen closes the thread", null, manager.snapshot().selectedThreadId)

        inject(manager, "alpha@self-nest.test", "arrives after going back to the list")
        assertTrue(
            "a thread the user left for the list must paint unread again for later arrivals",
            unreadOf(manager, thread) > 0u,
        )
    }
}
