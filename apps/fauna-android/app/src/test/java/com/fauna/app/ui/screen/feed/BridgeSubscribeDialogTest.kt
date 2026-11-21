package com.fauna.app.ui.screen.feed

import androidx.compose.ui.test.*
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.core.AppMessages
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_feed.AvailableBridge

/**
 * The bridge-subscribe form's form case (`docs/goal/ui/feed.md` § Errors &
 * edge cases; `docs/goal/ui/README.md` § Offer only what is possible, rule 4):
 * `bridge-form-subscribe-button` is painted from the shared
 * `feed_bridge_subscribe_possibility` (no URI -> `feed.bridge_form.uri_required`,
 * no bridge -> `feed.bridge_form.kind_required`, the reason beside it), and the
 * form closes only once the subscription is saved — a failure keeps it open
 * with what was typed. Needs the host `.so` (`just android-host-test`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class BridgeSubscribeDialogTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val bluesky = AvailableBridge(id = "bluesky", name = "Bluesky")
    private val appMessages = AppMessages()

    private fun render(
        bridges: List<AvailableBridge> = listOf(bluesky),
        saved: Boolean = true,
        onSubscribe: (String, String, String) -> Unit = { _, _, _ -> },
        onDismiss: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            CompositionLocalProvider(LocalAppMessages provides appMessages) {
                BridgeSubscribeDialog(
                    bridges = bridges,
                    onDismiss = onDismiss,
                    onSubscribe = { k, u, n -> onSubscribe(k, u, n); if (saved) null else "the nest refused" },
                )
            }
        }
    }

    @Test
    fun noUriIsDisabledWithTheSharedReason() {
        render()
        composeTestRule.onNodeWithTag("bridge-form-subscribe-button").assertIsNotEnabled()
        composeTestRule.onNodeWithText("Type the feed URI first.").assertExists()

        composeTestRule.onNodeWithTag("bridge-form-uri-input").performTextInput("at://did:plc:x/feed")
        composeTestRule.onNodeWithTag("bridge-form-subscribe-button").assertIsEnabled()
        composeTestRule.onNodeWithText("Type the feed URI first.").assertDoesNotExist()
    }

    @Test
    fun noBridgeIsDisabledWithTheKindReason() {
        render(bridges = emptyList())
        composeTestRule.onNodeWithTag("bridge-form-uri-input").performTextInput("at://did:plc:x/feed")
        composeTestRule.onNodeWithTag("bridge-form-subscribe-button").assertIsNotEnabled()
        composeTestRule.onNodeWithText("Choose a bridge first.").assertExists()
    }

    @Test
    fun aFailedSaveKeepsTheFormOpenWithTheUri() {
        var dismissals = 0
        val subscribed = mutableListOf<Triple<String, String, String>>()
        render(saved = false, onSubscribe = { k, u, n -> subscribed += Triple(k, u, n) }, onDismiss = { dismissals++ })
        composeTestRule.onNodeWithTag("bridge-form-uri-input").performTextInput("at://did:plc:x/feed")
        composeTestRule.onNodeWithTag("bridge-form-subscribe-button").performClick()
        composeTestRule.waitForIdle()

        assertEquals(listOf(Triple("bluesky", "at://did:plc:x/feed", "")), subscribed)
        assertEquals(0, dismissals)
        composeTestRule.onNodeWithTag("bridge-form-uri-input").assertTextContains("at://did:plc:x/feed")
        // README § Offer only what is possible, rule 7: the form's own
        // `error-message` says why; the page's banner stands down.
        composeTestRule.onNodeWithTag("error-message").assertTextEquals("the nest refused")
        assertEquals(null, appMessages.error.value)
    }

    @Test
    fun aSavedSubscriptionClosesTheForm() {
        var dismissals = 0
        render(saved = true, onDismiss = { dismissals++ })
        composeTestRule.onNodeWithTag("bridge-form-uri-input").performTextInput("at://did:plc:x/feed")
        composeTestRule.onNodeWithTag("bridge-form-subscribe-button").performClick()
        composeTestRule.waitForIdle()
        assertEquals(1, dismissals)
    }
}
