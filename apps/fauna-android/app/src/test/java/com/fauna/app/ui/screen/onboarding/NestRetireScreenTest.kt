package com.fauna.app.ui.screen.onboarding

import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.printToString
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import com.fauna.app.core.ActorScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.SessionAccount
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when`
import org.robolectric.annotation.Config

/**
 * The `nest_retire` page's credentials state over a REAL `NestRetireMachine`
 * (`nest-retirement.md` § Layout & flow; the states after verify need a cloud,
 * which `test_nest_retire.py` walks against the fake one on the emulator).
 * What this pins, with the Compose tree as the witness:
 *
 *  - the `vps_config` provider row is reused by id and keyed by provider id
 *    (`vps-provider-row[<provider_id>]`) — the walk's `RetireActions` presses
 *    exactly these — with `retire-back-button` beside it;
 *  - the credential form and verify button appear only once a provider is
 *    picked, and verify stays dead until the required field is typed AND the
 *    inputs the page announced have landed (`expect_app_inputs`);
 *  - Back from credentials leaves the page, telling the host no session was
 *    dropped.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class NestRetireScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun vm(): NestRetireVM {
        val session = mock(SessionAccount::class.java)
        // An unresolvable literal: the page asks for no lookup of an IP and the
        // saved-host path stays off the network.
        `when`(session.nestUrl).thenReturn("http://127.0.0.1:9")
        return NestRetireVM(
            api = mock(ApiClient::class.java),
            host = mock(OnboardingHost::class.java),
            sessionAccount = session,
            actorScope = mock(ActorScope::class.java),
        )
    }

    @Test
    fun theCredentialsStateReusesTheProviderRowByIdAndGatesVerify() {
        var exited: Boolean? = null
        val vm = vm()
        composeTestRule.setContent {
            NestRetireScreen(entry = RetireEntry.Launch, onExit = { exited = it }, vm = vm)
        }

        try {
            composeTestRule.waitUntil(timeoutMillis = 10_000) {
                composeTestRule.onAllNodesWithTagCount("vps-provider-row[hetzner]") > 0
            }
        } catch (e: androidx.compose.ui.test.ComposeTimeoutException) {
            throw AssertionError(
                "no provider row; machine=${vm.machine} tree=" +
                    composeTestRule.onRoot().printToString(),
                e,
            )
        }
        composeTestRule.onNodeWithTag("retire-back-button").assertExists()
        composeTestRule.onNodeWithTag("vps-verify-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("vps-credentials-form-api-token").assertDoesNotExist()

        composeTestRule.onNodeWithTag("vps-provider-row[hetzner]").performClick()
        composeTestRule.onNodeWithTag("vps-credentials-form-api-token").assertExists()
        composeTestRule.onNodeWithTag("vps-verify-button").assertExists().assertIsNotEnabled()

        composeTestRule.onNodeWithTag("vps-credentials-form-api-token").performTextInput("tok")
        // Enabled once the announced inputs (the held-DNS read) have landed.
        composeTestRule.waitUntil(timeoutMillis = 15_000) {
            runCatching { composeTestRule.onNodeWithTag("vps-verify-button").assertIsEnabled() }.isSuccess
        }

        // Below the fold of the scrolling page in a test window.
        composeTestRule.onNodeWithTag("retire-back-button").performScrollTo().performClick()
        assertEquals(false, exited)
    }

    private fun androidx.compose.ui.test.junit4.ComposeContentTestRule.onAllNodesWithTagCount(tag: String): Int =
        onAllNodes(androidx.compose.ui.test.hasTestTag(tag)).fetchSemanticsNodes().size
}
