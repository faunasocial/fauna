package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.util.AutomationAttributePack
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility

/**
 * [MailSubpageRows] — the Settings landing's six mail sub-page rows, painted
 * from the shared mail-usable answer (`mail-settings.md` § Errors & edge
 * cases, the mail-usable gate; `settings.md` § Navigation model, the same page
 * set and ids as the desktop rail): absent → none; blocked → each disabled
 * with the reason published on it and painted beneath; possible → live, each
 * opening its own route.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MailSubpageRowsTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun text(id: Int) = RuntimeEnvironment.getApplication().getString(id)

    private val keys = listOf(
        "mail-aliases", "mail-spam", "mail-export", "mail-import", "mail-lists", "mail-list-members",
    )

    private fun tag(key: String) = "${Ids.SETTINGS_NAV_ROW}[$key]"

    private val opened = mutableListOf<String>()

    private fun render(answer: Possibility) {
        composeTestRule.setContent {
            LazyColumn { item { MailSubpageRows(answer) { opened += it } } }
        }
    }

    @Test
    fun absentComposesNoRow() {
        render(Possibility.Absent)
        keys.forEach { composeTestRule.onNodeWithTag(tag(it)).assertDoesNotExist() }
    }

    @Test
    fun blockedShowsEverySixRowDisabledWithTheReasonPublishedAndPainted() {
        render(Possibility.Blocked(LocalizedText("mail_settings.usable_needs_mail_on", emptyMap())))
        val reason = text(R.string.mail_settings_usable_needs_mail_on)
        val packed = AutomationAttributePack.pack("reason" to reason, "state" to "")
        keys.forEach {
            composeTestRule.onNodeWithTag(tag(it))
                .assertExists()
                .assertIsNotEnabled()
                .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, packed))
        }
        composeTestRule.onAllNodesWithText(reason).assertCountEquals(1)
        composeTestRule.onNodeWithTag(tag("mail-aliases")).performClick()
        assertEquals(emptyList<String>(), opened)
    }

    @Test
    fun possibleShowsEveryRowLiveOpeningItsOwnRoute() {
        render(Possibility.Possible)
        composeTestRule.onAllNodesWithText(text(R.string.mail_settings_usable_needs_mail_on)).assertCountEquals(0)
        keys.forEach {
            composeTestRule.onNodeWithTag(tag(it)).assertIsEnabled().performClick()
        }
        assertEquals(keys.map { "settings/$it" }, opened)
    }
}
