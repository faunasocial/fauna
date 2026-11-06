package com.fauna.app.ui.screen.feed

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_feed.PostReactionGroup

/**
 * Compose-level pin for [ReactionPillRow] — the `post-reaction-pill` row under a
 * post's interaction bar (feed.md § Layout & flow → *Reaction posts count and
 * paint*, ruling 4): one pill per tally group in order, "emoji count" text, the
 * viewer's own (`viewerReactId` present) `selected`, a tap handing the group's
 * emoji to the shared `react` toggle. The leaf takes plain groups — the bluesky
 * skip rides `classifySources` one layer up — so it runs on the host JVM like
 * [FeedInteractionBarTest].
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ReactionPillRowTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val groups = listOf(
        PostReactionGroup(emoji = "❤️", count = 2L, viewerReactId = null),
        PostReactionGroup(emoji = "🔥", count = 1L, viewerReactId = "ab"),
    )

    @Test
    fun paintsOnePillPerGroupInOrderWithTheViewersOwnSelected() {
        composeTestRule.setContent { ReactionPillRow(groups) {} }
        val pills = composeTestRule.onAllNodesWithTag("post-reaction-pill")
        pills.assertCountEquals(2)
        pills[0].assertTextEquals("❤️ 2")
        pills[0].assertIsNotSelected()
        pills[1].assertTextEquals("🔥 1")
        pills[1].assertIsSelected()
    }

    @Test
    fun tapHandsTheGroupsEmojiToTheToggle() {
        var tapped: String? = null
        composeTestRule.setContent { ReactionPillRow(groups) { tapped = it } }
        composeTestRule.onAllNodesWithTag("post-reaction-pill")[1].performClick()
        assertEquals("🔥", tapped)
    }

    @Test
    fun paintsNothingWithoutGroups() {
        composeTestRule.setContent { ReactionPillRow(emptyList()) {} }
        composeTestRule.onAllNodesWithTag("post-reaction-pill").assertCountEquals(0)
    }
}
