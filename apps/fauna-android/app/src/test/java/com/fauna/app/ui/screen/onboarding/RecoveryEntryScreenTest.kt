package com.fauna.app.ui.screen.onboarding

import android.content.Context
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import androidx.navigation.compose.rememberNavController
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.core.LogicalSecretStore
import com.fauna.app.core.MemoryBackend
import com.fauna.app.core.OnboardingHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.onboarding.OnboardingMachine
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.RecoveryEntryOutcome
import com.fauna.ffi.onboarding.recoveryEntryOutcomeMessage
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.ArgumentMatchers.anyString
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config
import social.fauna.generated.Ids

/**
 * The phrase-only identity restore (`onboarding.md` § 1 Identity, the
 * `recovery_entry` step) — android's leg of the page tui led. What every
 * refusal SAYS is the shared table (`recoveryEntryOutcomeMessage`);
 * `Superseded` alone routes to import carrying why
 * (`beginImportIdentityWithReason`); a restored seed is committed exactly as
 * an import commits (the shared confirm-identity moment). FFI-touching (the
 * real registry + the shared message table) → `just android-host-test`.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class RecoveryEntryScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val context: Context = ApplicationProvider.getApplicationContext()
    private val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))

    private data class Fixture(val vm: RecoveryEntryVM, val machine: OnboardingMachine)

    private fun makeVm(appendMode: Boolean = false): Fixture {
        val machine = mock(OnboardingMachine::class.java)
        val host = mock(OnboardingHost::class.java)
        whenever(host.machine).thenReturn(machine)
        whenever(host.tick).thenReturn(MutableStateFlow(0L))
        whenever(host.appendMode).thenReturn(appendMode)
        whenever(machine.step()).thenReturn(OnboardingStep.RECOVERY_ENTRY)
        return Fixture(RecoveryEntryVM(host, registry), machine)
    }

    /** Superseded routes, never speaks: the import page, carrying why. */
    @Test
    fun supersededRoutesToImportWithTheReason() {
        val (vm, machine) = makeVm()
        vm.settle(RecoveryEntryOutcome.Superseded, context)
        verify(machine).beginImportIdentityWithReason(
            context.getString(R.string.onboarding_recovery_entry_superseded),
        )
        verify(machine, never()).setErrorMessage(anyString())
    }

    /** Every other refusal says the shared table's sentence on `error-message`. */
    @Test
    fun aRefusalSaysTheSharedTablesSentence() {
        val (vm, machine) = makeVm()
        vm.settle(RecoveryEntryOutcome.InvalidKit, context)
        val expected = resolveLocalized(context, recoveryEntryOutcomeMessage(RecoveryEntryOutcome.InvalidKit))!!
        assertEquals(context.getString(R.string.onboarding_recovery_entry_invalid_kit), expected)
        verify(machine).setErrorMessage(expected)
        assertNull("a refusal commits nothing", registry.active())
    }

    /** A restored seed is committed the way an import commits it. */
    @Test
    fun aRestoredSeedIsCommittedLikeAnImport() {
        val (vm, machine) = makeVm()
        whenever(machine.effectiveSecret()).thenReturn(SECRET)
        vm.settle(RecoveryEntryOutcome.Restored, context)
        assertEquals(SECRET, registry.sessionMaterial(registry.active()!!)?.secretHex)
    }

    /** …and in append mode, through the same shared moment, which writes nothing. */
    @Test
    fun anAppendRestoreWritesNothingYet() {
        val (vm, machine) = makeVm(appendMode = true)
        whenever(machine.effectiveSecret()).thenReturn(SECRET)
        vm.settle(RecoveryEntryOutcome.Restored, context)
        assertNull(registry.active())
    }

    /**
     * The typed account rides on the machine's one account field, ALWAYS —
     * empty included, so a handle an earlier flow left behind never targets an
     * account the user did not type.
     */
    @Test
    fun submitForwardsTheTypedAccountAndThePhrase() = runBlocking {
        val (vm, machine) = makeVm()
        var sent: String? = null
        vm.submitEntry = { phrase -> sent = phrase; RecoveryEntryOutcome.NoEscrow }
        vm.submit(" $PHRASE ", "  alice@example.com ", context)
        verify(machine).setCurrentHandle("alice@example.com")
        assertEquals(" $PHRASE ", sent)

        vm.submit(PHRASE, "", context)
        verify(machine).setCurrentHandle("")
    }

    @Test
    fun thePagePaintsTheMachinesMessageAndItsFields() {
        val (vm, machine) = makeVm()
        whenever(machine.errorMessage()).thenReturn(REFUSAL)
        composeTestRule.setContent {
            RecoveryEntryScreen(navController = rememberNavController(), vm = vm)
        }
        composeTestRule.onNodeWithTag(Ids.ERROR_MESSAGE).assertTextEquals(REFUSAL)
        composeTestRule.onNodeWithTag(Ids.RECOVERY_ENTRY_PHRASE_FIELD).performTextInput(PHRASE)
        composeTestRule.onNodeWithTag(Ids.RECOVERY_ENTRY_ACCOUNT_FIELD).performTextInput("alice@example.com")
        composeTestRule.onNodeWithTag(Ids.RECOVERY_ENTRY_SUBMIT_BUTTON).assertExists()
        composeTestRule.onNodeWithTag(Ids.RECOVERY_ENTRY_BACK_BUTTON).performScrollTo().performClick()
        verify(machine).back()
    }

    private companion object {
        const val SECRET =
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
        const val PHRASE =
            "a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf"
        const val REFUSAL = "That is not a recovery kit."
    }
}
