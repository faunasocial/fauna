package com.fauna.app.core.feed

import com.fauna.app.core.ApiClient
import com.fauna.app.core.conversations.ConversationsManagerHost
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config
import com.fauna.app.testing.FaunaRobolectricTestRunner

/**
 * [FeedManagerHost.stageAttachmentBytes] / [FeedManagerHost.pendingAttachmentBytes]
 * — the composer's pick-to-submit hold, moved onto this SINGLETON (rather than
 * onto the per-navigation `FeedVM`) so both the real picker
 * (`FeedComposeScreen.kt`) and the `compose.file`[compose-file] e2e TestAgent
 * injection (`TestAgent.kt`), which holds no reference to any live `FeedVM`
 * instance, stage into the one place a later real submit reads from
 * (`FeedVM.submitPost`).
 *
 * `api` is never touched by either method (it is used only inside [FeedManagerHost.manager]),
 * so a bare mock is enough — no FFI manager needs to exist for this hold to work.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class FeedManagerHostAttachmentStagingTest {

    private fun newHost() = FeedManagerHost(
        mock(ApiClient::class.java),
        mock(ConversationsManagerHost::class.java),
    )

    @Test
    fun nothingStagedByDefault() {
        assertNull(newHost().pendingAttachmentBytes())
    }

    @Test
    fun stagedBytesAreHeldUntilRead() {
        val host = newHost()
        val bytes = byteArrayOf(1, 2, 3)
        host.stageAttachmentBytes(bytes)
        assertArrayEquals(bytes, host.pendingAttachmentBytes())
        // A second read does NOT clear it — [FeedVM.submitPost] peeks the held
        // bytes and only [FeedVM.stageAttachmentBytes]-with-null (its own
        // success path, or the compose-file-ready chip's remove affordance)
        // ever drops them, so a failed submit still has the pick for a retry.
        assertArrayEquals(bytes, host.pendingAttachmentBytes())
    }

    @Test
    fun clearingWithNullDropsTheHeldPick() {
        val host = newHost()
        host.stageAttachmentBytes(byteArrayOf(9))
        host.stageAttachmentBytes(null)
        assertNull(host.pendingAttachmentBytes())
    }

    @Test
    fun aFreshPickOverridesThePreviousOne() {
        val host = newHost()
        host.stageAttachmentBytes(byteArrayOf(1))
        val second = byteArrayOf(2, 2)
        host.stageAttachmentBytes(second)
        assertArrayEquals(second, host.pendingAttachmentBytes())
    }
}
