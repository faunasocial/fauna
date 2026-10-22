package com.fauna.app.ui.components

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.viewmodel.PushNotificationsVM
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids

/**
 * [PushNotificationsContent] — the Account page's push control (`settings.md`
 * § Push notifications): the toggle paints the stored bit as `on`/`off` (what
 * the cross-app `get_attr(toggle, "state")` reads), the error line renders only
 * on failure, and a tap hands the wanted value to the caller.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class PushNotificationsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun state() = composeTestRule.onNodeWithTag(Ids.PUSH_NOTIFICATIONS_OPT_IN_TOGGLE)
        .fetchSemanticsNode().config[SemanticsProperties.StateDescription]

    @Test
    fun anInstallNotOptedInPaintsOffWithNoLine() {
        composeTestRule.setContent { PushNotificationsContent(optedIn = false, busy = false, line = null, onToggle = {}) }
        composeTestRule.onNodeWithTag(Ids.PUSH_NOTIFICATIONS_SECTION).assertExists()
        assertEquals("off", state())
        composeTestRule.onNodeWithTag(Ids.PUSH_NOTIFICATIONS_ERROR).assertDoesNotExist()
    }

    @Test
    fun anOptedInInstallPaintsOn() {
        composeTestRule.setContent { PushNotificationsContent(optedIn = true, busy = false, line = null, onToggle = {}) }
        assertEquals("on", state())
    }

    @Test
    fun aTapAsksForTheOtherValue() {
        var asked: Boolean? = null
        composeTestRule.setContent { PushNotificationsContent(optedIn = false, busy = false, line = null, onToggle = { asked = it }) }
        composeTestRule.onNodeWithTag(Ids.PUSH_NOTIFICATIONS_OPT_IN_TOGGLE).performClick()
        assertEquals(true, asked)
    }

    @Test
    fun noDistributorSaysSoOnTheLine() {
        composeTestRule.setContent {
            PushNotificationsContent(optedIn = false, busy = false, line = PushNotificationsVM.Line.NO_DISTRIBUTOR, onToggle = {})
        }
        composeTestRule.onNodeWithTag(Ids.PUSH_NOTIFICATIONS_ERROR)
            .assertTextContains("no push service app", substring = true)
    }
}
