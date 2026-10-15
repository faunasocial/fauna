package com.fauna.app.ui.screen.feed

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_feed.ReplyAudience

/**
 * Compose-level coverage for [ReplyComposerDialogContent] — the reply dialog's
 * audience statement and explicit-public answer (restricted-posts.md § Encryption
 * at rest → *Ruling 5's build — the shape*, (d) + (e)). The dialog paints the
 * manager's `reply_audience` under `feed-reply-audience` and offers
 * `feed-reply-public-confirm` only under *public by confirmation*, unchecked at
 * every open; a checked box is what turns the send into the confirmed-public verb.
 * Pure Compose over plain inputs — no [FeedVM], no native lib.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ReplyComposerDialogTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        audience: ReplyAudience?,
        onSend: (String, Boolean) -> Unit = { _, _ -> },
    ) {
        composeTestRule.setContent {
            ReplyComposerDialogContent(
                audience = audience,
                roomLabel = "Garden",
                gatedTier = "Patrons",
                onSend = onSend,
                onDismiss = {},
            )
        }
    }

    @Test
    fun aPublicTargetPaintsNeitherAudienceNorConfirm() {
        render(audience = null)
        composeTestRule.onNodeWithTag("feed-reply-dialog").assertExists()
        composeTestRule.onNodeWithTag("feed-reply-audience").assertDoesNotExist()
        composeTestRule.onNodeWithTag("feed-reply-public-confirm").assertDoesNotExist()
    }

    @Test
    fun aSealedAudienceIsNamedAndOffersNoPublicAnswer() {
        render(audience = ReplyAudience.SEALED_TO_ROOM)
        composeTestRule.onNodeWithTag("feed-reply-audience")
            .assertTextEquals("Reply goes to Room: Garden")
        composeTestRule.onNodeWithTag("feed-reply-public-confirm").assertDoesNotExist()
    }

    @Test
    fun aTierAudienceNamesTheTier() {
        render(audience = ReplyAudience.SEALED_TO_TIER)
        composeTestRule.onNodeWithTag("feed-reply-audience")
            .assertTextEquals("Reply goes to your tier Patrons")
    }

    @Test
    fun publicByConfirmationStatesItAndTheBoxStartsUnchecked() {
        render(audience = ReplyAudience.PUBLIC_BY_CONFIRMATION)
        composeTestRule.onNodeWithTag("feed-reply-audience").assertExists()
        composeTestRule.onNodeWithTag("feed-reply-public-confirm").assertIsOff()
    }

    @Test
    fun theSendCarriesTheAnswerOnlyWhileTheBoxIsChecked() {
        val sent = mutableListOf<Pair<String, Boolean>>()
        render(audience = ReplyAudience.PUBLIC_BY_CONFIRMATION) { t, c -> sent += t to c }

        composeTestRule.onNodeWithTag("feed-reply-text-field").performTextInput("hello")
        composeTestRule.onNodeWithTag("feed-reply-submit-button").performClick()
        assertEquals(listOf("hello" to false), sent)
    }

    @Test
    fun aCheckedBoxMakesTheSendConfirmed() {
        val sent = mutableListOf<Pair<String, Boolean>>()
        render(audience = ReplyAudience.PUBLIC_BY_CONFIRMATION) { t, c -> sent += t to c }

        composeTestRule.onNodeWithTag("feed-reply-text-field").performTextInput("hello")
        composeTestRule.onNodeWithTag("feed-reply-public-confirm").performClick()
        composeTestRule.onNodeWithTag("feed-reply-public-confirm").assertIsOn()
        composeTestRule.onNodeWithTag("feed-reply-submit-button").performClick()
        assertEquals(listOf("hello" to true), sent)
    }
}
