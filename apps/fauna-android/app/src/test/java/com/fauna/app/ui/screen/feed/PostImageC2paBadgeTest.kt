package com.fauna.app.ui.screen.feed

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_core.RenderBlock
import uniffi.fauna_core.RenderDocument

/**
 * Compose-level coverage for [PostImageC2paBadge] — the `c2pa-badge` on
 * feed's post_detail (ui.yaml, `used_in: feed.post_detail`). A pure Compose
 * leaf taking the post `document` + an injected `checkC2pa` suspend lambda
 * (mirrors `ConversationDetailScreen`'s `loadLinkPreviewImageBytes` idiom),
 * so it runs on the host JVM exactly like [FeedPostActionsMenuTest].
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class PostImageC2paBadgeTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun documentWithImage(hash: String = "aa".repeat(32)) =
        RenderDocument(blocks = listOf(RenderBlock.Image(hash = hash, alt = "")))

    @Test
    fun rendersBadgeWhenBlobIsC2paVerified() {
        composeTestRule.setContent {
            PostImageC2paBadge(document = documentWithImage(), checkC2pa = { true })
        }
        composeTestRule.waitForIdle()
        composeTestRule.onNodeWithTag("c2pa-badge").assertExists()
    }

    @Test
    fun hidesBadgeWhenBlobIsNotC2paVerified() {
        composeTestRule.setContent {
            PostImageC2paBadge(document = documentWithImage(), checkC2pa = { false })
        }
        composeTestRule.waitForIdle()
        composeTestRule.onNodeWithTag("c2pa-badge").assertDoesNotExist()
    }

    @Test
    fun hidesBadgeAndSkipsCheckWhenDocumentHasNoImage() {
        var checkCalled = false
        composeTestRule.setContent {
            PostImageC2paBadge(
                document = RenderDocument(blocks = emptyList()),
                checkC2pa = { checkCalled = true; true },
            )
        }
        composeTestRule.waitForIdle()
        composeTestRule.onNodeWithTag("c2pa-badge").assertDoesNotExist()
        assert(!checkCalled) { "checkC2pa must not be called when the post has no media hash" }
    }
}
