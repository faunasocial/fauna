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
import uniffi.fauna_launch_machine.AccountIndexRefusal

/**
 * Compose-level coverage for the stateless [LaunchAccountIndexUnreadableScreen]
 * (`launch_account_index_unreadable`, `version-compatibility.md` § 5 item 9)
 * — the android leg, witnessed here rather than by the tier_3 e2e suite
 * (`test_account_index_unreadable_launch.py`), which is blocked on the
 * Android emulator, which only runs on the dedicated emulator machine. Mirrors
 * `AdminNestContentTest.factoryResetConfirmFlowFires`'s reveal-then-confirm
 * shape and pins the same three states
 * `apps/fauna-tui/src/walk.rs::dump_onboarding_and_launch_surfaces` walks:
 * the version verdict (nothing offered), the malformed verdict before the
 * first press, and after it (the confirm, stating the residual).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class LaunchAccountIndexUnreadableScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(refusal: AccountIndexRefusal, onReset: () -> Unit = {}) {
        composeTestRule.setContent {
            LaunchAccountIndexUnreadableScreen(refusal = refusal, onReset = onReset)
        }
    }

    @Test
    fun newerBuild_paintsOnlyTheWarning() {
        render(
            AccountIndexRefusal.NewerBuild(
                indexV = 2.toUShort(),
                indexMin = 2.toUShort(),
                binV = 1.toUShort(),
            ),
        )

        composeTestRule.onNodeWithTag(Ids.ACCOUNT_INDEX_REFUSAL_WARNING)
            .assertExists()
            .assertTextEquals(getString(R.string.onboarding_launch_index_newer_build))
        composeTestRule.onNodeWithTag(Ids.ACCOUNT_INDEX_RESET_BUTTON).assertDoesNotExist()
        composeTestRule.onNodeWithTag(Ids.ACCOUNT_INDEX_RESET_CONFIRM_BUTTON).assertDoesNotExist()
    }

    @Test
    fun malformed_revealsConfirmOnlyAfterTheFirstPress_andStatesTheResidualBeforeItRuns() {
        var reset = false
        render(AccountIndexRefusal.Malformed, onReset = { reset = true })

        composeTestRule.onNodeWithTag(Ids.ACCOUNT_INDEX_REFUSAL_WARNING)
            .assertTextEquals(getString(R.string.onboarding_launch_index_malformed))
        composeTestRule.onNodeWithTag(Ids.ACCOUNT_INDEX_RESET_BUTTON).assertExists()
        composeTestRule.onNodeWithTag(Ids.ACCOUNT_INDEX_RESET_CONFIRM_BUTTON).assertDoesNotExist()

        composeTestRule.onNodeWithTag(Ids.ACCOUNT_INDEX_RESET_BUTTON).performClick()

        // The reveal is purely local — onReset must NOT have fired yet.
        assertEquals(false, reset)
        composeTestRule.onNodeWithTag(Ids.ACCOUNT_INDEX_RESET_BUTTON).assertDoesNotExist()
        composeTestRule.onNodeWithTag(Ids.ACCOUNT_INDEX_RESET_CONFIRM_BUTTON)
            .assertExists()
            .assertTextEquals(getString(R.string.onboarding_launch_index_malformed_reset_confirm))
        composeTestRule.onNodeWithTag(Ids.ACCOUNT_INDEX_REFUSAL_WARNING)
            .assertTextEquals(getString(R.string.onboarding_launch_index_malformed_reset_residual))

        composeTestRule.onNodeWithTag(Ids.ACCOUNT_INDEX_RESET_CONFIRM_BUTTON).performClick()
        assertEquals(true, reset)
    }

    private fun getString(resId: Int): String =
        androidx.test.core.app.ApplicationProvider.getApplicationContext<android.content.Context>()
            .getString(resId)
}
