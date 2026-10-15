package com.fauna.app.ui.screen

import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids

/**
 * `launch_retry`'s retire entry (`nest-retirement.md` § Layout & flow):
 * `launch-retire-button` is ALWAYS shown — it needs only a cloud token, no
 * custodied seed — unlike `launch-recover-button`, which waits for a custodied
 * box. The page it opens is walked by `test_nest_retire.py`.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class LaunchRetryScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun retireEntryShowsWithNoRecoverableBoxAndOpensThePage() {
        var opened = 0
        composeTestRule.setContent {
            LaunchRetryScreen(
                onRetry = {},
                onUseDifferentNest = {},
                recoverableBoxes = emptyList(),
                onRetire = { opened++ },
            )
        }

        composeTestRule.onNodeWithTag(Ids.LAUNCH_RECOVER_BUTTON).assertDoesNotExist()
        composeTestRule.onNodeWithTag(Ids.LAUNCH_RETIRE_BUTTON)
            .assertExists()
            .assertTextEquals(getString(R.string.onboarding_launch_retire_server))
        composeTestRule.onNodeWithTag(Ids.LAUNCH_RETIRE_BUTTON).performClick()
        assertEquals(1, opened)
    }

    private fun getString(resId: Int): String =
        androidx.test.core.app.ApplicationProvider.getApplicationContext<android.content.Context>()
            .getString(resId)
}
