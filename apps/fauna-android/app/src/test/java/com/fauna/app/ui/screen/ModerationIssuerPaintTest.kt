package com.fauna.app.ui.screen

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.screen.moderation.ModerationQueueContent
import com.fauna.app.ui.util.AutomationAttributePack
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_client_moderation.QueueRow
import uniffi.fauna_client_moderation.QueueRowSource
import uniffi.fauna_core.LocalizedText

/**
 * A row another nest delivered names its issuer (`moderation.md` § Legal
 * takedown → *The author homed on another nest*): the shared `issued_by`
 * suffix rides on the action line (no id of its own), and the
 * `content-label-badge` packs the issuer's domain as the `issuer` fact beside
 * `confidence` / `action` (§ Element IDs → *Queue row facts*); this nest's own
 * decision packs an empty `issuer`, which the bridge reads as not set.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ModerationIssuerPaintTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun takenDown(issuer: String?) = QueueRow(
        contentId = "server-1",
        contentType = "post",
        category = "illegal",
        confidencePerMille = 1000u,
        // `ObligationAction::TakenDown`.
        action = 7u,
        timestamp = 0L,
        source = QueueRowSource.SERVER,
        issuerNest = issuer,
        issuedBy = issuer?.let {
            LocalizedText("moderation.queue.issued_by", mapOf("nest" to it))
        },
    )

    private fun render(row: QueueRow) = composeTestRule.setContent {
        ModerationQueueContent(queue = listOf(row), isLoading = false, onCorrect = {})
    }

    @Test
    fun aDeliveredRowNamesItsIssuer() {
        render(takenDown("issuer.example"))
        composeTestRule
            .onNodeWithText("Removed under legal obligation · decided by issuer.example")
            .assertExists()
        composeTestRule.onNodeWithTag("content-label-badge").assert(
            SemanticsMatcher.expectValue(
                SemanticsProperties.StateDescription,
                AutomationAttributePack.pack(
                    "confidence" to "100%",
                    "action" to "Removed under legal obligation",
                    "issuer" to "issuer.example",
                ),
            ),
        )
    }

    @Test
    fun anOwnDecisionNamesNoIssuer() {
        render(takenDown(null))
        composeTestRule.onNodeWithText("Removed under legal obligation").assertExists()
        composeTestRule.onNodeWithTag("content-label-badge").assert(
            SemanticsMatcher.expectValue(
                SemanticsProperties.StateDescription,
                AutomationAttributePack.pack(
                    "confidence" to "100%",
                    "action" to "Removed under legal obligation",
                    "issuer" to "",
                ),
            ),
        )
    }
}
