package com.fauna.app.ui.screen.feed

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Favorite
import androidx.compose.material.icons.filled.FavoriteBorder
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.semantics.SemanticsProperties
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import uniffi.fauna_feed.LikeState
import uniffi.fauna_feed.likeStateWord
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

    // The heart's `state` attribute (its `stateDescription`) is the shared
    // `PostSummary.likeState` word — on | off | queued | unsent
    // (account-offline-mutation.md § The unsent-changes surface, point 2),
    // spelled by shared Rust (`likeStateWord`), never by this app.
    @Test
    fun likeStateWordsAreTheSharedSpelling() {
        assertEquals("on", likeStateWord(LikeState.ON))
        assertEquals("off", likeStateWord(LikeState.OFF))
        assertEquals("queued", likeStateWord(LikeState.QUEUED))
        assertEquals("unsent", likeStateWord(LikeState.UNSENT))
    }

    @Test
    fun stateWordRidesTheButtonsStateDescription() {
        var clicked = false
        composeTestRule.setContent {
            InteractionButton(
                testTag = "feed-like-button",
                icon = Icons.Default.FavoriteBorder,
                contentDescription = "Like",
                count = 0L,
                stateWord = likeStateWord(LikeState.UNSENT),
                onClick = { clicked = true },
            )
        }
        composeTestRule.onNodeWithTag("feed-like-button")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, "unsent"))
            .performClick()
        // An unsent heart's gesture is still the like door (the shared manager retries).
        assertTrue(clicked)
    }
}
