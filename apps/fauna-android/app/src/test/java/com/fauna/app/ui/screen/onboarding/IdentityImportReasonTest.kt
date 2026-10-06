package com.fauna.app.ui.screen.onboarding

import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.navigation.compose.rememberNavController
import com.fauna.app.core.OnboardingHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.onboarding.OnboardingMachine
import kotlinx.coroutines.flow.MutableStateFlow
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config
import social.fauna.generated.Ids

/**
 * The import page paints the MACHINE's reason on `error-message` — the one a
 * launch-time route set atomically with the step
 * (`AppLaunchVM.routeSupersededRefusal` → `beginImportIdentityWithReason`,
 * `identity-succession.md` § Propagation → *Own device fleet*). Before this the
 * page painted only its own validation error, so a succeeded device landed on
 * import with no word of why its session ended.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class IdentityImportReasonTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun theMachineHeldReasonIsPaintedAndFollowsTheMachine() {
        val tick = MutableStateFlow(0L)
        val machine = mock(OnboardingMachine::class.java)
        val host = mock(OnboardingHost::class.java)
        whenever(host.machine).thenReturn(machine)
        whenever(host.tick).thenReturn(tick)
        whenever(machine.errorMessage()).thenReturn(CLAIM_FREE)
        val vm = IdentityImportVM(host, mock(FfiAccountRegistry::class.java))

        composeTestRule.setContent {
            IdentityImportScreen(navController = rememberNavController(), vm = vm)
        }
        composeTestRule.onNodeWithTag(Ids.ERROR_MESSAGE).assertTextEquals(CLAIM_FREE)

        // The verified upgrade arrives as a later machine mutation; the page
        // re-reads on the tick rather than holding the first message.
        whenever(machine.errorMessage()).thenReturn(VERIFIED)
        tick.value = 1L
        composeTestRule.onNodeWithTag(Ids.ERROR_MESSAGE).assertTextEquals(VERIFIED)
    }

    private companion object {
        const val CLAIM_FREE = "This identity was succeeded — import the new identity to continue."
        const val VERIFIED = "Your account now belongs to the verified successor."
    }
}
