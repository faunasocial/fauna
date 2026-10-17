package com.fauna.app.ui.screen

import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.screen.moderation.ModerationQueueContent
import com.fauna.app.ui.viewmodel.ModerationQueueVM
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_client_moderation.QueueRow
import uniffi.fauna_client_moderation.QueueRowSource
import uniffi.fauna_core.LocalizedText

/**
 * Compose-level coverage of the moderation queue's **appeal** paint
 * (`moderation.md` § Legal takedown — the transparency triple's second leg).
 * Every decision is shared Rust's (`row_is_appealable`, `appeal_form_view`,
 * `appeal_verdict`); this pins which element carries which fold:
 *
 * - the handle is per ROW KIND — a server row paints `appeal-button`, a local
 *   detection paints none;
 * - the form paints under the row its draft names (the `content_id` captured at
 *   open time, never a row index), with the submit control dead until a reason is
 *   typed;
 * - the outcome rides `appeal-status`, a sibling of the queue, never the
 *   page-level error banner.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ModerationAppealPaintTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun queueRow(id: String, source: QueueRowSource) = QueueRow(
        contentId = id,
        contentType = "post",
        category = "illegal",
        confidencePerMille = 1000u,
        action = if (source == QueueRowSource.LOCAL) null else 7u,
        timestamp = 0L,
        source = source,
    )

    private val queue = listOf(
        queueRow("server-1", QueueRowSource.SERVER),
        queueRow("local-1", QueueRowSource.LOCAL),
    )

    @Test
    fun onlyAServerRowOffersAnAppeal() {
        composeTestRule.setContent {
            ModerationQueueContent(queue = queue, isLoading = false, onCorrect = {})
        }
        composeTestRule.onAllNodesWithTag("train-correction-button").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("appeal-button").assertCountEquals(1)
    }

    @Test
    fun theAppealHandleHandsBackItsOwnRow() {
        var opened: String? = null
        composeTestRule.setContent {
            ModerationQueueContent(
                queue = queue,
                isLoading = false,
                onCorrect = {},
                onOpenAppeal = { opened = it.contentId },
            )
        }
        composeTestRule.onNodeWithTag("appeal-button").performClick()
        assertEquals("server-1", opened)
    }

    @Test
    fun theFormIsAbsentUntilADraftIsOpen() {
        composeTestRule.setContent {
            ModerationQueueContent(queue = queue, isLoading = false, onCorrect = {})
        }
        composeTestRule.onAllNodesWithTag("appeal-reason-input").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("appeal-submit-button").assertCountEquals(0)
    }

    @Test
    fun aReasonlessAppealIsNotSubmittable() {
        composeTestRule.setContent {
            ModerationQueueContent(
                queue = queue,
                isLoading = false,
                onCorrect = {},
                appealDraft = ModerationQueueVM.AppealDraft("server-1", ""),
            )
        }
        composeTestRule.onNodeWithTag("appeal-reason-input").assertExists()
        composeTestRule.onNodeWithTag("appeal-cancel-button").assertExists()
        composeTestRule.onNodeWithTag("appeal-submit-button").assertIsNotEnabled()
    }

    @Test
    fun aReasonedAppealSubmitsAndCancelDropsIt() {
        var submitted = 0
        var cancelled = 0
        composeTestRule.setContent {
            ModerationQueueContent(
                queue = queue,
                isLoading = false,
                onCorrect = {},
                appealDraft = ModerationQueueVM.AppealDraft("server-1", "I hold the licence."),
                onSubmitAppeal = { submitted++ },
                onCancelAppeal = { cancelled++ },
            )
        }
        composeTestRule.onNodeWithTag("appeal-submit-button").assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("appeal-cancel-button").performClick()
        assertEquals(1, submitted)
        assertEquals(1, cancelled)
    }

    @Test
    fun theOutcomeLinePaintsOnAppealStatus() {
        composeTestRule.setContent {
            ModerationQueueContent(
                queue = queue,
                isLoading = false,
                onCorrect = {},
                appealStatus = LocalizedText("moderation.appeal_recorded", emptyMap()),
            )
        }
        composeTestRule.onNodeWithTag("appeal-status")
            .assertTextContains("Appeal recorded. An administrator will review it.")
        // The queue is still being read: the row's handle survives the outcome.
        composeTestRule.onAllNodesWithTag("appeal-button").assertCountEquals(1)
    }
}
