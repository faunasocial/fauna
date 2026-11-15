package com.fauna.app.ui.screen.settings

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_core.IntentDescription
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.UnsentChange
import uniffi.fauna_core.UnsentState

/**
 * The standing unsent-changes section ([UnsentChangesCard];
 * `account-offline-mutation.md` § The unsent-changes surface, point 1): the
 * three title states, one row per change with the injected shared words,
 * retry and discard on a not-sent row only, the confirm only once that row's
 * discard is armed. FFI-free: the describer and status line are injected, the
 * rows are plain UniFFI records. tui's
 * `the_unsent_changes_section_is_standing_and_offers_retry_on_a_not_sent_row_only`
 * is the twin.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class UnsentChangesCardTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun change(id: String, state: UnsentState) = UnsentChange(
        intentId = id,
        kind = "fauna.posts.interact",
        scope = "posts.interact:p",
        state = state,
        createdAt = 0UL,
        attempts = 0U,
        description = IntentDescription(
            label = LocalizedText("settings.unsent_changes.kind_like", emptyMap()),
            target = null,
        ),
        hold = null,
    )

    private val rows = listOf(
        change("aa", UnsentState.Unsent(reasonCode = "not_found")),
        change("bb", UnsentState.Held),
    )

    private fun render(
        unsentChanges: List<UnsentChange>?,
        discardArmed: String? = null,
        onRetry: (String) -> Unit = {},
        onDiscard: (String) -> Unit = {},
        onConfirmDiscard: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            UnsentChangesCard(
                unsentChanges = unsentChanges,
                discardArmed = discardArmed,
                onRetry = onRetry,
                onDiscard = onDiscard,
                onConfirmDiscard = onConfirmDiscard,
                describe = { "Like on a post" },
                status = { row -> if (row.state is UnsentState.Unsent) "Not sent — gone" else "Waiting" },
            )
        }
    }

    @Test
    fun bareTitleBeforeTheFirstRead() {
        render(unsentChanges = null)
        composeTestRule.onNodeWithTag("unsent-changes-section").assertTextEquals("Unsent changes")
        composeTestRule.onAllNodesWithTag("unsent-change-item").assertCountEquals(0)
    }

    @Test
    fun emptyLineOnceReadAndEmpty() {
        render(unsentChanges = emptyList())
        composeTestRule.onNodeWithTag("unsent-changes-section")
            .assertTextEquals("Every change has reached your nest.")
        composeTestRule.onAllNodesWithTag("unsent-change-item").assertCountEquals(0)
    }

    @Test
    fun countedTitleAndOneRowPerChangeWithTheSharedWords() {
        render(unsentChanges = rows)
        composeTestRule.onNodeWithTag("unsent-changes-section").assertTextEquals("Unsent changes (2)")
        composeTestRule.onAllNodesWithTag("unsent-change-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("unsent-change-description", useUnmergedTree = true)[0]
            .assertTextEquals("Like on a post")
        composeTestRule.onAllNodesWithTag("unsent-change-status", useUnmergedTree = true)[0]
            .assertTextEquals("Not sent — gone")
        composeTestRule.onAllNodesWithTag("unsent-change-status", useUnmergedTree = true)[1]
            .assertTextEquals("Waiting")
    }

    @Test
    fun retryAndDiscardOnTheNotSentRowOnly() {
        render(unsentChanges = rows)
        composeTestRule.onAllNodesWithTag("unsent-change-retry-button", useUnmergedTree = true)
            .assertCountEquals(1)
        composeTestRule.onAllNodesWithTag("unsent-change-discard-button", useUnmergedTree = true)
            .assertCountEquals(1)
        composeTestRule.onAllNodesWithTag("unsent-change-discard-confirm-button", useUnmergedTree = true)
            .assertCountEquals(0)
    }

    @Test
    fun retryFiresWithTheIntentId() {
        var retried: String? = null
        render(unsentChanges = rows, onRetry = { retried = it })
        composeTestRule.onNodeWithTag("unsent-change-retry-button", useUnmergedTree = true).performClick()
        assertEquals("aa", retried)
    }

    @Test
    fun discardArmsTheConfirmOnThatRowAndConfirmDiscards() {
        var discarded: String? = null
        composeTestRule.setContent {
            var armed by remember { mutableStateOf<String?>(null) }
            UnsentChangesCard(
                unsentChanges = rows,
                discardArmed = armed,
                onRetry = {},
                onDiscard = { armed = it },
                onConfirmDiscard = { discarded = it },
                describe = { "Like on a post" },
                status = { "Not sent" },
            )
        }
        composeTestRule.onAllNodesWithTag("unsent-change-discard-confirm-button", useUnmergedTree = true)
            .assertCountEquals(0)
        composeTestRule.onNodeWithTag("unsent-change-discard-button", useUnmergedTree = true).performClick()
        composeTestRule.onNodeWithTag("unsent-change-discard-confirm-button", useUnmergedTree = true)
            .assertExists().performClick()
        assertEquals("aa", discarded)
    }
}
