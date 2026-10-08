package com.fauna.app.ui.screen.feed

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.FavoriteBorder
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for [InteractionButton] — the feed interaction-bar
 * affordance (feed.md § Interaction bar, ratified 2026-06-27). Verifies the two
 * ratified presentation rules: each button is **icon + count**, and the **count
 * is hidden when 0** (a clean icon-only button until the post has activity). The
 * button is a pure Compose leaf taking a plain `Long` count — no FFI seam, no
 * `PostSummary`, no `.so` (the counts ride the shared snapshot one layer up), so
 * this runs on the host JVM exactly like [FeedCreateDialogTest]. Also covers the
 * `active` lit-state paint (feed.md § Implementation status today — "the like
 * button became a toggle 2026-08-11"): a distinct content description per state
 * is the only lit-state signal Compose semantics expose to a test without a
 * screenshot, mirroring apple's `heart`/`heart.fill` pair.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class FeedInteractionBarTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        count: Long,
        active: Boolean = false,
        onClick: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            InteractionButton(
                testTag = "feed-like-button",
                icon = if (active) Icons.Default.Favorite else Icons.Default.FavoriteBorder,
                contentDescription = if (active) "Liked" else "Like",
                count = count,
                active = active,
                onClick = onClick,
            )
        }
    }

    @Test
    fun countHiddenWhenZero() {
        render(count = 0L)
        composeTestRule.onNodeWithTag("feed-like-button").assertExists("button missing")
        // No count text at all — the icon-only state (feed.md: hidden when 0).
        composeTestRule.onNodeWithText("0").assertDoesNotExist()
    }

    @Test
    fun countShownWhenPositive() {
        render(count = 5L)
        composeTestRule.onNodeWithTag("feed-like-button").assertExists("button missing")
        // The count rides the button's merged text beside the icon's label (the
        // count Text's own semantics are cleared so the number is read once).
        composeTestRule.onNodeWithTag("feed-like-button").assertTextEquals("Like 5")
    }

    @Test
    fun clickFiresOnTaggedButton() {
        var clicked = false
        render(count = 0L, onClick = { clicked = true })
        composeTestRule.onNodeWithTag("feed-like-button").performClick()
        assertTrue("tapping the button should fire onClick", clicked)
    }

    @Test
    fun activeExposesSelectedSemantics() {
        render(count = 1L, active = true)
        composeTestRule.onNodeWithTag("feed-like-button").assertIsSelected()
    }

    @Test
    fun inactiveExposesNotSelectedSemantics() {
        render(count = 1L, active = false)
        composeTestRule.onNodeWithTag("feed-like-button").assertIsNotSelected()
    }
}
