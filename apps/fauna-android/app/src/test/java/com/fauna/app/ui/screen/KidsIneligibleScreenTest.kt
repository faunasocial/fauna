package com.fauna.app.ui.screen

import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids

/**
 * `kids_ineligible` (`account-age-band.md` § The account age band, the
 * kids-app bullet, item (3)): the explanation and the sign-out, with
 * `error-message` painted only after a failed sign-out.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class KidsIneligibleScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun rendersExplanationAndSignOutWithNoError() {
        var signedOut = 0
        composeTestRule.setContent { KidsIneligibleScreen(onSignOut = { signedOut++ }) }

        composeTestRule.onNodeWithTag(Ids.KIDS_INELIGIBLE_MESSAGE).assertExists()
        composeTestRule.onNodeWithTag(Ids.ERROR_MESSAGE).assertDoesNotExist()
        composeTestRule.onNodeWithTag(Ids.SIGN_OUT_BUTTON).performClick()
        assertEquals(1, signedOut)
    }

    @Test
    fun paintsTheErrorElementOnlyWhenSignOutFailed() {
        composeTestRule.setContent { KidsIneligibleScreen(onSignOut = {}, error = "boom") }

        composeTestRule.onNodeWithTag(Ids.ERROR_MESSAGE).assertExists()
    }
}
