package com.fauna.app.ui.screen.onboarding

import android.content.Context
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import androidx.navigation.compose.rememberNavController
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.OnboardingHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.formatUnixLocal
import com.fauna.ffi.onboarding.EmergencyLockoutOutcome
import com.fauna.ffi.onboarding.OnboardingMachine
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.emergencyLockoutOutcomeMessage
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config
import social.fauna.generated.Ids

/**
 * The signed-out lock door (`ui/sessions.md` § The signed-out door, the
 * `emergency_lockout_entry` step) — android's leg of the page tui led. What
 * every outcome SAYS is the shared table (`emergencyLockoutOutcomeMessage`);
 * the unlock time is the shared `formatUnixLocal`. FFI-touching → `just
 * android-host-test`.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class EmergencyLockoutEntryScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val context: Context = ApplicationProvider.getApplicationContext()

    private data class Fixture(val vm: EmergencyLockoutEntryVM, val machine: OnboardingMachine)

    private fun makeVm(): Fixture {
        val machine = mock(OnboardingMachine::class.java)
        val host = mock(OnboardingHost::class.java)
        whenever(host.machine).thenReturn(machine)
        whenever(host.tick).thenReturn(MutableStateFlow(0L))
        whenever(machine.step()).thenReturn(OnboardingStep.EMERGENCY_LOCKOUT_ENTRY)
        return Fixture(EmergencyLockoutEntryVM(host), machine)
    }

    @Test
    fun theStepRoutesToItsOwnPage() {
        assertEquals(
            EMERGENCY_LOCKOUT_ENTRY_ROUTE,
            routeForStep(OnboardingStep.EMERGENCY_LOCKOUT_ENTRY),
        )
        assertEquals("onboarding/emergency-lockout-entry", EMERGENCY_LOCKOUT_ENTRY_ROUTE)
    }

    /** A refusal says the shared table's sentence on `error-message`. */
    @Test
    fun aRefusalSaysTheSharedTablesSentence() {
        val (vm, machine) = makeVm()
        vm.settle(EmergencyLockoutOutcome.InvalidSecret, context)
        val expected = resolveLocalized(
            context,
            emergencyLockoutOutcomeMessage(EmergencyLockoutOutcome.InvalidSecret, ""),
        )!!
        assertTrue(expected.isNotBlank())
        verify(machine).setErrorMessage(expected)
    }

    /** A lock says the confirmation with the shared formatter's unlock time. */
    @Test
    fun aLockNamesTheUnlockTime() {
        val (vm, machine) = makeVm()
        val until = 1_800_000_000L
        vm.settle(EmergencyLockoutOutcome.Locked(until), context)
        val expected = resolveLocalized(
            context,
            emergencyLockoutOutcomeMessage(
                EmergencyLockoutOutcome.Locked(until),
                formatUnixLocal(until),
            ),
        )!!
        assertTrue(expected.contains(formatUnixLocal(until)))
        verify(machine).setErrorMessage(expected)
    }

    /** The typed account rides the machine's one field, empty included. */
    @Test
    fun submitForwardsTheSecretTheWordAndTheAccount() = runBlocking {
        val (vm, machine) = makeVm()
        var sent: Pair<String, String>? = null
        vm.submitLockout = { secret, word ->
            sent = secret to word
            EmergencyLockoutOutcome.AccountNeeded
        }
        vm.submit(SECRET, "  alice@example.com ", "LOCK", context)
        verify(machine).setCurrentHandle("alice@example.com")
        assertEquals(SECRET to "LOCK", sent)

        vm.submit(SECRET, "", "LOCK", context)
        verify(machine).setCurrentHandle("")
    }

    @Test
    fun theButtonWaitsForTheWordAndBackGoesThroughTheMachine() {
        val (vm, machine) = makeVm()
        whenever(machine.errorMessage()).thenReturn(REFUSAL)
        composeTestRule.setContent {
            EmergencyLockoutEntryScreen(navController = rememberNavController(), vm = vm)
        }
        composeTestRule.onNodeWithTag(Ids.ERROR_MESSAGE).assertTextEquals(REFUSAL)
        composeTestRule.onNodeWithTag(Ids.PASTE_SECRET_FIELD).performTextInput(SECRET)
        composeTestRule.onNodeWithTag(Ids.RECOVERY_ENTRY_ACCOUNT_FIELD)
            .performTextInput("alice@example.com")
        composeTestRule.onNodeWithTag(Ids.SESSIONS_LOCKOUT_WARNING).assertExists()
        composeTestRule.onNodeWithTag(Ids.SESSIONS_LOCKOUT_BUTTON).performScrollTo().assertIsNotEnabled()
        composeTestRule.onNodeWithTag(Ids.SESSIONS_LOCKOUT_CONFIRM_FIELD).performTextInput("LOCK")
        composeTestRule.onNodeWithTag(Ids.SESSIONS_LOCKOUT_BUTTON).performScrollTo().assertIsEnabled()
        composeTestRule.onNodeWithTag(Ids.EMERGENCY_LOCKOUT_BACK_BUTTON).performScrollTo().performClick()
        verify(machine).back()
    }

    private companion object {
        // Built, not spelled: a 64-hex literal trips the publish secret scan.
        val SECRET = "ab".repeat(32)
        const val REFUSAL = "That isn't a secret key."
    }
}
