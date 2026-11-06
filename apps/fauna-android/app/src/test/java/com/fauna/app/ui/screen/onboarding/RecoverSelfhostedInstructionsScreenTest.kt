package com.fauna.app.ui.screen.onboarding

import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.navigation.compose.rememberNavController
import com.fauna.app.core.AccountStores
import com.fauna.app.core.OnboardingHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.onboarding.OnboardingMachine
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config
import social.fauna.generated.Ids

/**
 * The self-hosted recovery page (`box-recovery.md` § Recovery UI (step 4)):
 * the resolved `FAUNA_DEPLOYMENT_SEED=…` line replaces the pending string and
 * enables Copy; an unresolved line keeps Copy disabled. FFI-touching (the
 * generated bindings load) → `just android-host-test`.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class RecoverSelfhostedInstructionsScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun makeVm(nestUrl: String = "https://box.example"): RecoverSelfhostedInstructionsVM {
        val machine = mock(OnboardingMachine::class.java)
        val host = mock(OnboardingHost::class.java)
        val stores = mock(AccountStores::class.java)
        whenever(host.machine).thenReturn(machine)
        whenever(machine.effectiveSecret()).thenReturn(SECRET)
        whenever(machine.recoverySelectedNestId()).thenReturn(BOX)
        whenever(machine.nestUrl()).thenReturn(nestUrl)
        whenever(stores.accountStoreContainerDir()).thenReturn("/tmp/store")
        return RecoverSelfhostedInstructionsVM(host, stores)
    }

    @Test
    fun theResolvedLineIsShownAndCopyIsEnabled() {
        val vm = makeVm()
        var asked: Pair<String?, String>? = null
        vm.fetchCommand = { url, _, id, _ -> asked = url to id; COMMAND }
        composeTestRule.setContent {
            RecoverSelfhostedInstructionsScreen(navController = rememberNavController(), vm = vm)
        }
        composeTestRule.waitUntil(5_000) { vm.command.value != null }
        composeTestRule.onNodeWithTag(Ids.RECOVER_SELFHOSTED_COMMAND).assertTextEquals(COMMAND)
        composeTestRule.onNodeWithTag(Ids.RECOVER_SELFHOSTED_COPY_BUTTON).assertIsEnabled()
        assertEquals("https://box.example" to BOX, asked)
    }

    @Test
    fun anUnresolvedLineKeepsCopyDisabled() {
        val vm = makeVm()
        vm.fetchCommand = { _, _, _, _ -> throw IllegalStateException("nest unreachable") }
        composeTestRule.setContent {
            RecoverSelfhostedInstructionsScreen(navController = rememberNavController(), vm = vm)
        }
        composeTestRule.waitForIdle()
        assertNull(vm.command.value)
        composeTestRule.onNodeWithTag(Ids.RECOVER_SELFHOSTED_COPY_BUTTON).assertIsNotEnabled()
    }

    @Test
    fun aDeadNestStillResolvesFromTheDevicesOwnStore() {
        // No saved nest URL: the pre-login getter is asked with a null URL and
        // answers from the device's own store alone.
        val vm = makeVm(nestUrl = "")
        var asked: Pair<String?, String>? = null
        vm.fetchCommand = { url, _, id, _ -> asked = url to id; COMMAND }
        composeTestRule.setContent {
            RecoverSelfhostedInstructionsScreen(navController = rememberNavController(), vm = vm)
        }
        composeTestRule.waitUntil(5_000) { vm.command.value != null }
        composeTestRule.onNodeWithTag(Ids.RECOVER_SELFHOSTED_COMMAND).assertTextEquals(COMMAND)
        composeTestRule.onNodeWithTag(Ids.RECOVER_SELFHOSTED_COPY_BUTTON).assertIsEnabled()
        assertEquals(null to BOX, asked)
    }

    @Test
    fun aBoxWithNoCustodiedSeedKeepsCopyDisabled() {
        val vm = makeVm()
        vm.fetchCommand = { _, _, _, _ -> null }
        composeTestRule.setContent {
            RecoverSelfhostedInstructionsScreen(navController = rememberNavController(), vm = vm)
        }
        composeTestRule.waitForIdle()
        assertNull(vm.command.value)
        composeTestRule.onNodeWithTag(Ids.RECOVER_SELFHOSTED_COPY_BUTTON).assertIsNotEnabled()
    }

    private companion object {
        val SECRET = "01".repeat(32)
        const val BOX = "aa11bb22"
        val COMMAND = "FAUNA_DEPLOYMENT_SEED=" + "01".repeat(32)
    }
}
