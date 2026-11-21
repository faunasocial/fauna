package com.fauna.app.ui.screen.onboarding

import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.navigation.compose.rememberNavController
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.core.OnboardingHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.onboarding.GoalCover
import com.fauna.ffi.onboarding.OnboardingMachine
import com.fauna.ffi.onboarding.OnboardingObserver
import com.fauna.ffi.onboarding.ProviderGoal
import kotlinx.coroutines.flow.MutableStateFlow
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when`
import org.robolectric.annotation.Config
import social.fauna.generated.Ids

/**
 * A refused goal pick on the choose-providers page says why on the page's
 * `error-message` (`docs/goal/ui/README.md` § Offer only what is possible,
 * rule 7) — over a REAL `OnboardingMachine`, whose `select_goal_cover` puts
 * its refusal on the shared `error_message`. The VPS goal cannot be covered by
 * hand, so picking Manual for it is refused.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ProviderChoiceRefusalTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun aRefusedGoalPickShowsItsReasonOnErrorMessage() {
        val tick = MutableStateFlow(0L)
        val machine = OnboardingMachine(object : OnboardingObserver {
            override fun `onChanged`() {
                tick.value = tick.value + 1
            }
        })
        val host = mock(OnboardingHost::class.java)
        `when`(host.machine).thenReturn(machine)
        `when`(host.tick).thenReturn(tick)
        val vm = ProviderChoiceVM(host)
        val ctx = ApplicationProvider.getApplicationContext<android.content.Context>()
        composeTestRule.setContent {
            ProviderChoiceScreen(navController = rememberNavController(), vm = vm)
        }
        composeTestRule.onNodeWithTag(Ids.ERROR_MESSAGE).assertDoesNotExist()

        composeTestRule.runOnIdle { vm.selectCover(ProviderGoal.VPS, GoalCover.Manual) }

        val reason = ctx.getString(R.string.onboarding_provider_choice_status_cover_vps)
        composeTestRule.waitUntil(timeoutMillis = 10_000) {
            composeTestRule.onAllNodes(androidx.compose.ui.test.hasTestTag(Ids.ERROR_MESSAGE))
                .fetchSemanticsNodes().isNotEmpty()
        }
        val shown = composeTestRule.onNodeWithTag(Ids.ERROR_MESSAGE).fetchSemanticsNode()
            .config[androidx.compose.ui.semantics.SemanticsProperties.Text]
            .joinToString("") { it.text }
        assertTrue("error-message names the refusal: $shown", shown.isNotEmpty())
        assertTrue("error-message is the VPS refusal: $shown vs $reason", shown == reason)
    }
}
