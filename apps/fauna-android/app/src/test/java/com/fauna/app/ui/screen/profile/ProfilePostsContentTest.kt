package com.fauna.app.ui.screen.profile

import androidx.compose.material3.Text
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.screen.feed.PostActionsMenu
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for [ProfilePostsContent] — the profile page's Posts tab
 * (`profile.md` § Layout & flow → *The Posts tab*): `profile-posts-section` hosts
 * the window's rows, `profile-posts-empty` paints iff the shared empty-state
 * decision says empty, and a card's ⋯ menu opens IN PLACE on the profile, the
 * section staying on screen.
 *
 * FFI-free, like [com.fauna.app.ui.screen.feed.FeedPostActionsMenuTest]: the
 * stateful [ProfilePostsSection] resolves the window snapshot (via the native
 * `feedEmptyState` and `FfiFeedManager.authorWindow`) one layer up and hands
 * this paint plain row keys, the empty flag and the card slot. The card here is
 * the feed card's own ⋯ menu ([PostActionsMenu]); the full [com.fauna.app.ui.screen.feed.PostCard]
 * needs a Hilt [com.fauna.app.ui.viewmodel.FeedVM] over the native manager and
 * is covered by the e2e (`tests/e2e-unified/tests/test_profile.py`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ProfilePostsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(keys: List<String>, showEmpty: Boolean) {
        composeTestRule.setContent {
            ProfilePostsContent(postKeys = keys, showEmpty = showEmpty) { index ->
                Text(keys[index], modifier = Modifier.testTag("post-card"))
                PostActionsMenu(
                    isOwn = true,
                    markedVerb = null,
                    onTrainVerbTapped = {},
                    onDelete = {},
                )
            }
        }
    }

    @Test
    fun rendersOneCardPerWindowRowInOrder() {
        render(listOf("post-b", "post-a"), showEmpty = false)
        composeTestRule.onNodeWithTag("profile-posts-section").assertIsDisplayed()
        val cards = composeTestRule.onAllNodesWithTag("post-card")
        cards.assertCountEquals(2)
        cards[0].assertTextEquals("post-b")
        cards[1].assertTextEquals("post-a")
        composeTestRule.onNodeWithTag("profile-posts-empty").assertDoesNotExist()
    }

    @Test
    fun emptyStatePaintsOnlyWhenTheSharedDecisionSaysEmpty() {
        render(emptyList(), showEmpty = true)
        composeTestRule.onNodeWithTag("profile-posts-section").assertIsDisplayed()
        composeTestRule.onNodeWithTag("profile-posts-empty").assertIsDisplayed()
        composeTestRule.onAllNodesWithTag("post-card").assertCountEquals(0)
    }

    @Test
    fun nothingBelowTheSectionWhileTheFirstReadIsInFlight() {
        // No snapshot yet / Loading: the shared empty state is absent.
        render(emptyList(), showEmpty = false)
        composeTestRule.onNodeWithTag("profile-posts-section").assertExists()
        composeTestRule.onNodeWithTag("profile-posts-empty").assertDoesNotExist()
        composeTestRule.onAllNodesWithTag("post-card").assertCountEquals(0)
    }

    @Test
    fun postActionsMenuOpensInPlaceKeepingTheSection() {
        render(listOf("post-a"), showEmpty = false)
        composeTestRule.onNodeWithTag("feed-post-actions-button").performClick()
        composeTestRule.onNodeWithTag("feed-post-delete-button").assertIsDisplayed()
        composeTestRule.onNodeWithTag("profile-posts-section").assertIsDisplayed()
    }
}
