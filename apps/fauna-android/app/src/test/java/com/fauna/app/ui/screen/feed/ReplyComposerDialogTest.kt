package com.fauna.app.ui.screen.feed

import androidx.compose.ui.test.*
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.core.AppMessages
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
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

    private var dismissed = false
    private val appMessages = AppMessages()

    private fun render(
        audience: ReplyAudience?,
        saved: Boolean = true,
        onSend: (String, Boolean) -> Unit = { _, _ -> },
    ) {
        composeTestRule.setContent {
            CompositionLocalProvider(LocalAppMessages provides appMessages) {
                if (!dismissed) {
                    ReplyComposerDialogContent(
                        audience = audience,
                        roomLabel = "Garden",
                        gatedTier = "Patrons",
                        onSend = { t, c -> onSend(t, c); if (saved) null else "the nest refused" },
                        onDismiss = { dismissed = true },
                    )
                }
            }
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

    // ── The form case (feed.md § Errors & edge cases; README § Offer only
    // what is possible, rule 4): painted from `feed_reply_submit_possibility`,
    // and the dialog closes only on a saved reply.

    @Test
    fun aBlankReplyIsDisabledWithTheSharedReason() {
        render(audience = null)
        composeTestRule.onNodeWithTag("feed-reply-submit-button").assertIsNotEnabled()
        composeTestRule.onNodeWithText("Write your reply first.").assertExists()

        composeTestRule.onNodeWithTag("feed-reply-text-field").performTextInput("hi")
        composeTestRule.onNodeWithTag("feed-reply-submit-button").assertIsEnabled()
        composeTestRule.onNodeWithText("Write your reply first.").assertDoesNotExist()
    }

    @Test
    fun aFailedSaveKeepsTheDialogOpenWithTheText() {
        val sent = mutableListOf<Pair<String, Boolean>>()
        render(audience = null, saved = false) { t, c -> sent += t to c }
        composeTestRule.onNodeWithTag("feed-reply-text-field").performTextInput("keep me")
        composeTestRule.onNodeWithTag("feed-reply-submit-button").performClick()
        composeTestRule.waitForIdle()

        assertEquals(listOf("keep me" to false), sent)
        assertFalse(dismissed)
        composeTestRule.onNodeWithTag("feed-reply-dialog").assertExists()
        composeTestRule.onNodeWithTag("feed-reply-text-field").assertTextContains("keep me")
    }

    // README § Offer only what is possible, rule 7: the open form's own
    // `error-message` says why, and the page's banner stands down meanwhile.
    @Test
    fun aFailedSaveSaysWhyInsideTheDialogAndThePageErrorStandsDown() {
        appMessages.showError("an older page error")
        render(audience = null, saved = false)
        composeTestRule.onNodeWithTag("error-message").assertDoesNotExist()
        composeTestRule.onNodeWithTag("feed-reply-text-field").performTextInput("keep me")
        composeTestRule.onNodeWithTag("feed-reply-submit-button").performClick()
        composeTestRule.waitForIdle()

        composeTestRule.onNodeWithTag("error-message").assertTextEquals("the nest refused")
        assertEquals(null, appMessages.error.value)
    }

    @Test
    fun aSavedReplyClosesTheDialog() {
        render(audience = null, saved = true)
        composeTestRule.onNodeWithTag("feed-reply-text-field").performTextInput("sent")
        composeTestRule.onNodeWithTag("feed-reply-submit-button").performClick()
        composeTestRule.waitForIdle()
        assertTrue(dismissed)
        composeTestRule.onNodeWithTag("error-message").assertDoesNotExist()
    }
}
