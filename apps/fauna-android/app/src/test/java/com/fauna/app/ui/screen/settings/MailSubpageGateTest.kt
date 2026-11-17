package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Text
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
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
 * [MailSubpageGateContent] — the entry gate every one of the six mail
 * sub-pages' routes is wrapped in (`mail-settings.md` § Errors & edge cases,
 * the mail-usable gate): while the shared answer is not possible only the
 * heading, the reason and Back are composed, never the page.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MailSubpageGateTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun text(id: Int) = RuntimeEnvironment.getApplication().getString(id)

    private val pages = listOf(
        R.string.mail_aliases_title,
        R.string.mail_lists_title,
        R.string.mail_lists_members_title,
        R.string.mail_export_title,
        R.string.mail_import_title,
        R.string.mail_spam_title,
    )

    private var backs = 0

    private fun render(answer: Possibility, titleRes: Int) {
        composeTestRule.setContent {
            MailSubpageGateContent(
                answer = answer,
                title = text(titleRes),
                onBack = { backs++ },
            ) {
                Text("the page as built", modifier = Modifier.testTag("page-body"))
            }
        }
    }

    @Test
    fun blockedShowsOnlyHeadingAndReasonOnEachOfTheSixSubpages() {
        val blocked = Possibility.Blocked(LocalizedText("mail_settings.usable_needs_mail_on", emptyMap()))
        composeTestRule.setContent {
            Column {
                pages.forEach { titleRes ->
                    MailSubpageGateContent(answer = blocked, title = text(titleRes), onBack = {}) {
                        Text("the page as built", modifier = Modifier.testTag("page-body"))
                    }
                }
            }
        }
        composeTestRule.onAllNodesWithTag(Ids.PAGE_HEADING).assertCountEquals(pages.size)
        pages.forEach { composeTestRule.onNodeWithText(text(it)).assertExists() }
        composeTestRule.onAllNodesWithText(text(R.string.mail_settings_usable_needs_mail_on))
            .assertCountEquals(pages.size)
        composeTestRule.onAllNodesWithTag("page-body").assertCountEquals(0)
    }

    @Test
    fun blockedShowsOnlyHeadingReasonAndBack() {
        val blocked = Possibility.Blocked(LocalizedText("mail_settings.usable_needs_mail_on", emptyMap()))
        render(blocked, R.string.mail_spam_title)
        composeTestRule.onNodeWithTag(Ids.PAGE_HEADING).assertTextEquals(text(R.string.mail_spam_title))
        composeTestRule.onNodeWithText(text(R.string.mail_settings_usable_needs_mail_on)).assertIsDisplayed()
        composeTestRule.onAllNodesWithTag("page-body").assertCountEquals(0)
        composeTestRule.onNodeWithContentDescription(text(R.string.common_back)).performClick()
        assertEquals(1, backs)
    }

    @Test
    fun absentShowsTheUnavailableNotice() {
        render(Possibility.Absent, R.string.mail_export_title)
        composeTestRule.onNodeWithTag(Ids.PAGE_HEADING).assertTextEquals(text(R.string.mail_export_title))
        composeTestRule.onNodeWithText(text(R.string.mail_settings_unavailable_notice)).assertIsDisplayed()
        composeTestRule.onAllNodesWithTag("page-body").assertCountEquals(0)
    }

    @Test
    fun waitingForTheFirstReadShowsTheReadingReason() {
        render(
            Possibility.Blocked(LocalizedText("mail_settings.usable_reading", emptyMap())),
            R.string.mail_import_title,
        )
        composeTestRule.onNodeWithText(text(R.string.mail_settings_usable_reading)).assertIsDisplayed()
        composeTestRule.onAllNodesWithTag("page-body").assertCountEquals(0)
    }

    @Test
    fun possibleShowsThePageAsBuilt() {
        render(Possibility.Possible, R.string.mail_aliases_title)
        composeTestRule.onNodeWithTag("page-body").assertIsDisplayed()
        composeTestRule.onAllNodesWithText(text(R.string.mail_settings_usable_needs_mail_on)).assertCountEquals(0)
    }
}
