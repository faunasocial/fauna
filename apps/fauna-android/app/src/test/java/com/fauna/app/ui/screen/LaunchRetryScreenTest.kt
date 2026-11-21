package com.fauna.app.ui.screen

import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertHasClickAction
import androidx.compose.ui.test.assertHasNoClickAction
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiAccountEntry
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
 * box. The page it opens is walked by `test_nest_retire.py`. And its account
 * switcher (`onboarding.md` § Wizard exit handling → *The finished wizard
 * holds*): painted only with more than one account, the active one plain.
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

    // ── The account switcher (`onboarding.md` § Wizard exit handling → *The
    // finished wizard holds*): the way back to the previous account after a
    // failed append-mode finish.

    private fun entry(actorId: String, handle: String) =
        FfiAccountEntry(
            actorId = actorId,
            handle = handle,
            domain = null,
            tier = null,
            requireConfirmToActivate = false,
        )

    @Test
    fun switcherPaintsOneItemPerAccountWithTheActiveOnePlain() {
        val switched = mutableListOf<String>()
        composeTestRule.setContent {
            LaunchRetryScreen(
                onRetry = {},
                onUseDifferentNest = {},
                accounts = listOf(entry("aaaa", "alice"), entry("bbbb", "bob")),
                // The new, unreachable account is the active one.
                activeActorId = "bbbb",
                accountLabel = { it.handle ?: it.actorId },
                onSwitch = { switched += it },
            )
        }

        composeTestRule.onNodeWithTag(Ids.ACCOUNT_SWITCHER_LIST).assertExists()
        composeTestRule.onAllNodesWithTag("${Ids.ACCOUNT_SWITCHER_ITEM}[0]").assertCountEquals(1)
        composeTestRule.onAllNodesWithTag("${Ids.ACCOUNT_SWITCHER_ITEM}[1]").assertCountEquals(1)
        composeTestRule.onNodeWithTag("${Ids.ACCOUNT_SWITCHER_ITEM}[2]").assertDoesNotExist()
        // Registry order: row 0 is the first account, a switch.
        composeTestRule.onNodeWithTag("account-item-handle[0]", useUnmergedTree = true)
            .assertTextEquals("alice")
        composeTestRule.onNodeWithTag("${Ids.ACCOUNT_SWITCHER_ITEM}[0]").assertHasClickAction()
        // Row 1 is the active account: plain, never a switch.
        composeTestRule.onNodeWithTag("account-item-handle[1]").assertTextEquals("bob")
        composeTestRule.onNodeWithTag("${Ids.ACCOUNT_SWITCHER_ITEM}[1]").assertHasNoClickAction()
        // Settings' per-row extras are not part of this surface.
        composeTestRule.onNodeWithTag(Ids.ACCOUNT_REQUIRE_CONFIRM_TOGGLE, useUnmergedTree = true)
            .assertDoesNotExist()
        composeTestRule.onNodeWithTag(Ids.ACCOUNT_ADD_BUTTON).assertDoesNotExist()

        composeTestRule.onNodeWithTag("${Ids.ACCOUNT_SWITCHER_ITEM}[0]").performScrollTo().performClick()
        assertEquals(listOf("aaaa"), switched)
    }

    @Test
    fun oneAccountPaintsNoSwitcher() {
        composeTestRule.setContent {
            LaunchRetryScreen(
                onRetry = {},
                onUseDifferentNest = {},
                accounts = listOf(entry("aaaa", "alice")),
                activeActorId = "aaaa",
            )
        }

        composeTestRule.onNodeWithTag(Ids.ACCOUNT_SWITCHER_LIST).assertDoesNotExist()
        composeTestRule.onNodeWithTag("${Ids.ACCOUNT_SWITCHER_ITEM}[0]").assertDoesNotExist()
        composeTestRule.onNodeWithTag(Ids.ERROR_MESSAGE).assertDoesNotExist()
    }

    @Test
    fun refusedSwitchIsSaidInErrorMessageAndTheSurfaceStays() {
        val refusal = "Can't switch to alice: sign in again from Add account."
        composeTestRule.setContent {
            LaunchRetryScreen(
                onRetry = {},
                onUseDifferentNest = {},
                accounts = listOf(entry("aaaa", "alice"), entry("bbbb", "bob")),
                activeActorId = "bbbb",
                switchError = refusal,
            )
        }

        composeTestRule.onNodeWithTag(Ids.ERROR_MESSAGE).assertTextEquals(refusal)
        composeTestRule.onNodeWithTag(Ids.LAUNCH_RETRY_BUTTON).assertExists()
        composeTestRule.onNodeWithTag(Ids.ACCOUNT_SWITCHER_LIST).assertExists()
    }

    private fun getString(resId: Int): String =
        androidx.test.core.app.ApplicationProvider.getApplicationContext<android.content.Context>()
            .getString(resId)
}
