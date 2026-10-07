package com.fauna.app.ui.screen.onboarding

import android.content.ClipboardManager
import android.content.Context
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.core.OnboardingHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.onboarding.OnboardingMachine
import com.fauna.ffi.onboarding.OnboardingStep
import kotlinx.coroutines.flow.MutableStateFlow
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config
import social.fauna.generated.Ids

/**
 * The sign-up `recovery_kit` offer (`onboarding.md` § 1 Identity;
 * `identity-succession.md` § The RecoveryKey → *Which encoding each affordance
 * carries*) — android's leg of the page tui led. Every value comes off the
 * shared machine: the display is the bare 64-hex, the copy button carries the
 * ONE `fauna://recovery` URI (never the hex), and the escrow line says the
 * deferred state, the only state this position can be in.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class RecoveryKitScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val context: Context = ApplicationProvider.getApplicationContext()

    private fun mount(secret: String?, uri: String?): OnboardingMachine {
        val machine = mock(OnboardingMachine::class.java)
        val host = mock(OnboardingHost::class.java)
        whenever(host.machine).thenReturn(machine)
        whenever(host.tick).thenReturn(MutableStateFlow(0L))
        whenever(machine.recoveryKitSecretHex()).thenReturn(secret)
        whenever(machine.recoveryKitUri()).thenReturn(uri)
        whenever(machine.step()).thenReturn(OnboardingStep.HANDLE_ENTRY)
        val vm = RecoveryKitVM(host)
        composeTestRule.setContent {
            val nav = rememberNavController()
            navController = nav
            NavHost(nav, startDestination = RECOVERY_KIT_ROUTE) {
                composable(RECOVERY_KIT_ROUTE) { RecoveryKitScreen(navController = nav, vm = vm) }
                composable(HANDLE_ENTRY_ROUTE) {}
            }
        }
        return machine
    }

    private lateinit var navController: NavHostController

    /** ui.yaml's transitions: confirm and skip both land on handle_entry, where the machine is. */
    private fun assertLandedOnHandleEntry() {
        composeTestRule.waitForIdle()
        assertEquals(HANDLE_ENTRY_ROUTE, navController.currentDestination?.route)
    }

    @Test
    fun theDisplayIsTheBareHexAndTheEscrowLineIsTheDeferredOne() {
        mount(SECRET, URI)
        composeTestRule.onNodeWithTag(Ids.RECOVERY_KIT_SECRET_DISPLAY).assertTextEquals(SECRET)
        composeTestRule.onNodeWithTag(Ids.RECOVERY_KIT_ESCROW_STATUS)
            .assertTextEquals(context.getString(R.string.onboarding_recovery_kit_escrow_deferred))
        composeTestRule.onNodeWithTag(Ids.RECOVERY_KIT_DESCRIPTION).assertExists()
        composeTestRule.onNodeWithTag(Ids.RECOVERY_KIT_QR).assertExists()
    }

    @Test
    fun theCopyButtonCarriesTheUriNeverTheHex() {
        mount(SECRET, URI)
        composeTestRule.onNodeWithTag(Ids.RECOVERY_KIT_SECRET_COPY_BTN).performScrollTo().performClick()
        val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        assertEquals(URI, clipboard.primaryClip?.getItemAt(0)?.text?.toString())
    }

    @Test
    fun confirmAndSkipAreTheMachinesOwnVerbs() {
        val machine = mount(SECRET, URI)
        composeTestRule.onNodeWithTag(Ids.RECOVERY_KIT_CONFIRM_BUTTON).performScrollTo().assertIsEnabled().performClick()
        verify(machine).confirmRecoveryKit()
        verify(machine, never()).skipRecoveryKit()
        assertLandedOnHandleEntry()
    }

    @Test
    fun skipIsOneClick() {
        val machine = mount(SECRET, URI)
        composeTestRule.onNodeWithTag(Ids.RECOVERY_KIT_SKIP_BUTTON).performScrollTo().performClick()
        verify(machine).skipRecoveryKit()
        verify(machine, never()).confirmRecoveryKit()
        assertLandedOnHandleEntry()
    }

    /** Nothing minted (outside the screen's lifetime): nothing to confirm or copy, no QR. */
    @Test
    fun withNothingMintedConfirmIsOffAndNoQrIsDrawn() {
        mount(null, null)
        composeTestRule.onNodeWithTag(Ids.RECOVERY_KIT_SECRET_DISPLAY)
            .assertTextEquals(context.getString(R.string.onboarding_recovery_kit_not_minted))
        composeTestRule.onNodeWithTag(Ids.RECOVERY_KIT_CONFIRM_BUTTON).assertIsNotEnabled()
        composeTestRule.onNodeWithTag(Ids.RECOVERY_KIT_SECRET_COPY_BTN).assertIsNotEnabled()
        composeTestRule.onNodeWithTag(Ids.RECOVERY_KIT_QR).assertDoesNotExist()
    }

    private companion object {
        const val HANDLE_ENTRY_ROUTE = "onboarding/handle-entry"
        const val SECRET =
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
        const val URI = "fauna://recovery?v=1&kit=0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20&account=abcd"
    }
}
