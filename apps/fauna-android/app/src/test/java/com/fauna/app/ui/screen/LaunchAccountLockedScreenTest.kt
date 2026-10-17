package com.fauna.app.ui.screen

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids

/**
 * The locked launch surface (`ui/sessions.md` § Layout & flow, leg 2): the
 * standing notice carries all three sentences and only the stolen-identity
 * action; the `identity_stolen_entry` step's press starts unarmed.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class LaunchAccountLockedScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun theNoticeSaysAllThreeThingsAndOffersOnlyTheCeremony() {
        var stolen = 0
        composeTestRule.setContent {
            LaunchAccountLockedScreen(unlockTime = "2027-01-02 03:04", onStolen = { stolen++ })
        }
        val notice = composeTestRule.onNodeWithTag(Ids.LAUNCH_ACCOUNT_LOCKED_NOTICE)
            .fetchSemanticsNode().config[androidx.compose.ui.semantics.SemanticsProperties.Text]
            .joinToString("") { it.text }
        assertTrue(notice, notice.contains(getString(R.string.onboarding_launch_account_locked)))
        assertTrue(notice, notice.contains("The lock ends 2027-01-02 03:04."))
        assertTrue(notice, notice.contains(getString(R.string.onboarding_launch_account_locked_not_yours)))
        composeTestRule.onNodeWithTag(Ids.LAUNCH_RETRY_BUTTON).assertDoesNotExist()
        composeTestRule.onNodeWithTag(Ids.LAUNCH_FALLTHROUGH_BUTTON).assertDoesNotExist()
        composeTestRule.onNodeWithTag(Ids.LAUNCH_ACCOUNT_LOCKED_STOLEN_BUTTON).performClick()
        assertEquals(1, stolen)
    }

    @Test
    fun theStolenEntryPressArmsOnlyOnTheLiteralWord() {
        var pressed = 0
        composeTestRule.setContent {
            var confirm by remember { mutableStateOf("") }
            IdentityStolenEntryScreen(
                phrase = "held-kit",
                confirm = confirm,
                confirmWord = "SUCCEED",
                busy = false,
                error = null,
                onPhraseChange = {},
                onConfirmChange = { confirm = it },
                onSucceed = { pressed++ },
                onBack = {},
            )
        }
        composeTestRule.onNodeWithTag(Ids.RECOVERY_ENTRY_PHRASE_FIELD).assertExists()
        composeTestRule.onNodeWithTag(Ids.RECOVERY_ENTRY_BACK_BUTTON).assertExists()
        composeTestRule.onNodeWithTag(Ids.IDENTITY_STOLEN_BUTTON).assertIsNotEnabled()
        composeTestRule.onNodeWithTag(Ids.IDENTITY_STOLEN_CONFIRM_FIELD).performTextInput("SUCCEED")
        composeTestRule.onNodeWithTag(Ids.IDENTITY_STOLEN_BUTTON).assertIsEnabled().performClick()
        assertEquals(1, pressed)
    }

    private fun getString(resId: Int): String =
        androidx.test.core.app.ApplicationProvider.getApplicationContext<android.content.Context>()
            .getString(resId)
}
