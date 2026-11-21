package com.fauna.app.ui.screen.onboarding

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.test.SemanticsNodeInteraction
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasScrollAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextClearance
import androidx.compose.ui.test.performTextInput
import androidx.navigation.compose.rememberNavController
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.OnboardingHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.onboarding.OnboardingMachine
import com.fauna.ffi.onboarding.OnboardingObserver
import com.fauna.ffi.onboarding.emergencyLockoutPossibility
import com.fauna.ffi.onboarding.identityImportPossibility
import com.fauna.ffi.onboarding.inviteCodeDraftPossibility
import com.fauna.ffi.onboarding.recoveryEntryPossibility
import kotlinx.coroutines.flow.MutableStateFlow
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_core.Possibility

/**
 * The signed-out forms' submits are painted from their shared draft answers
 * (`docs/goal/ui/README.md` § Offer only what is possible, rule 4's form case,
 * and rule 6): for each form a blank, a malformed and a complete draft — the
 * blocked ones disabled with the answer's own line beside the submit, the
 * complete one enabled. Over a REAL `OnboardingMachine` (the handle and claim
 * submits ask the machine's own answers, which fold in its in-flight state),
 * so FFI-touching → `just android-host-test`. The handle, invite-code and
 * claim drafts judge only blankness (the nest or the check judges the rest),
 * so their "malformed" draft is whitespace.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class SignedOutFormDraftsTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val context: Context = ApplicationProvider.getApplicationContext()

    private fun host(): OnboardingHost {
        val tick = MutableStateFlow(0L)
        val machine = OnboardingMachine(object : OnboardingObserver {
            override fun `onChanged`() {
                tick.value = tick.value + 1
            }
        })
        val host = mock(OnboardingHost::class.java)
        whenever(host.machine).thenReturn(machine)
        whenever(host.tick).thenReturn(tick)
        return host
    }

    private fun show(content: @Composable () -> Unit) = composeTestRule.setContent(content)

    /** The submit, scrolled into view on the pages that scroll (the restore
     *  and the lock); the others have no scroll container to ask. */
    private fun submit(tag: String): SemanticsNodeInteraction {
        val node = composeTestRule.onNodeWithTag(tag)
        val scrolls = composeTestRule.onAllNodes(hasScrollAction()).fetchSemanticsNodes().isNotEmpty()
        return if (scrolls) node.performScrollTo() else node
    }

    /** The blocked answer's own line is on screen (it may also be the page's
     *  idle status line, so at least one node carries it). */
    private fun assertReasonShown(answer: Possibility) {
        val reason = resolveLocalized(context, (answer as Possibility.Blocked).reason)!!
        assertTrue(reason.isNotBlank())
        composeTestRule.waitForIdle()
        assertTrue(
            "the reason \"$reason\" is painted beside the submit",
            composeTestRule.onAllNodes(hasText(reason)).fetchSemanticsNodes().isNotEmpty(),
        )
    }

    private fun type(tag: String, text: String) {
        composeTestRule.onNodeWithTag(tag).performTextClearance()
        if (text.isNotEmpty()) composeTestRule.onNodeWithTag(tag).performTextInput(text)
    }

    @Test
    fun importSubmitPaintsFromTheImportGrammar() {
        val vm = IdentityImportVM(host(), mock(FfiAccountRegistry::class.java))
        show { IdentityImportScreen(navController = rememberNavController(), vm = vm) }

        submit(Ids.IMPORT_SUBMIT_BUTTON).assertIsNotEnabled()
        assertReasonShown(identityImportPossibility(""))

        type(Ids.PASTE_SECRET_FIELD, "not-a-key")
        submit(Ids.IMPORT_SUBMIT_BUTTON).assertIsNotEnabled()
        assertReasonShown(identityImportPossibility("not-a-key"))

        type(Ids.PASTE_SECRET_FIELD, SECRET)
        submit(Ids.IMPORT_SUBMIT_BUTTON).assertIsEnabled()
    }

    @Test
    fun recoveryEntrySubmitPaintsFromTheRestoresOwnChecks() {
        val vm = RecoveryEntryVM(host(), mock(FfiAccountRegistry::class.java))
        show { RecoveryEntryScreen(navController = rememberNavController(), vm = vm) }

        submit(Ids.RECOVERY_ENTRY_SUBMIT_BUTTON).assertIsNotEnabled()
        assertReasonShown(recoveryEntryPossibility("", ""))

        type(Ids.RECOVERY_ENTRY_PHRASE_FIELD, "not a kit")
        submit(Ids.RECOVERY_ENTRY_SUBMIT_BUTTON).assertIsNotEnabled()
        assertReasonShown(recoveryEntryPossibility("not a kit", ""))

        // A bare 64-hex kit carries no account: the account is the next field.
        type(Ids.RECOVERY_ENTRY_PHRASE_FIELD, SECRET)
        submit(Ids.RECOVERY_ENTRY_SUBMIT_BUTTON).assertIsNotEnabled()
        assertReasonShown(recoveryEntryPossibility(SECRET, ""))

        type(Ids.RECOVERY_ENTRY_ACCOUNT_FIELD, "alice@example.com")
        submit(Ids.RECOVERY_ENTRY_SUBMIT_BUTTON).assertIsEnabled()
    }

    @Test
    fun lockoutPaintsSecretThenAccountThenWord() {
        val vm = EmergencyLockoutEntryVM(host())
        show { EmergencyLockoutEntryScreen(navController = rememberNavController(), vm = vm) }

        submit(Ids.SESSIONS_LOCKOUT_BUTTON).assertIsNotEnabled()
        assertReasonShown(emergencyLockoutPossibility("", "", ""))

        type(Ids.PASTE_SECRET_FIELD, SECRET)
        type(Ids.RECOVERY_ENTRY_ACCOUNT_FIELD, "no-domain")
        submit(Ids.SESSIONS_LOCKOUT_BUTTON).assertIsNotEnabled()
        assertReasonShown(emergencyLockoutPossibility(SECRET, "no-domain", ""))

        type(Ids.RECOVERY_ENTRY_ACCOUNT_FIELD, "alice@example.com")
        type(Ids.SESSIONS_LOCKOUT_CONFIRM_FIELD, "lock")
        submit(Ids.SESSIONS_LOCKOUT_BUTTON).assertIsNotEnabled()
        assertReasonShown(emergencyLockoutPossibility(SECRET, "alice@example.com", "lock"))

        type(Ids.SESSIONS_LOCKOUT_CONFIRM_FIELD, "LOCK")
        submit(Ids.SESSIONS_LOCKOUT_BUTTON).assertIsEnabled()
    }

    @Test
    fun handleCheckPaintsFromTheMachinesAnswer() {
        val host = host()
        host.machine.setCurrentHandle("")
        val vm = HandleEntryVM(host)
        show { HandleEntryScreen(navController = rememberNavController(), onWizardExit = {}, vm = vm) }

        submit(Ids.HANDLE_CHECK_BUTTON).assertIsNotEnabled()
        assertReasonShown(host.machine.handleCheckPossibility(""))

        type(Ids.HANDLE_INPUT, "   ")
        submit(Ids.HANDLE_CHECK_BUTTON).assertIsNotEnabled()
        assertReasonShown(host.machine.handleCheckPossibility("   "))

        type(Ids.HANDLE_INPUT, "alice@example.com")
        submit(Ids.HANDLE_CHECK_BUTTON).assertIsEnabled()
    }

    @Test
    fun inviteCodeCheckPaintsFromTheDraft() {
        val vm = InviteRequestVM(host())
        show { InviteRequestScreen(navController = rememberNavController(), onWizardExit = {}, vm = vm) }

        submit(Ids.INVITE_CODE_CHECK_BUTTON).assertIsNotEnabled()
        assertReasonShown(inviteCodeDraftPossibility(""))

        type(Ids.INVITE_CODE_INPUT, "   ")
        submit(Ids.INVITE_CODE_CHECK_BUTTON).assertIsNotEnabled()
        assertReasonShown(inviteCodeDraftPossibility("   "))

        type(Ids.INVITE_CODE_INPUT, "CODE-123")
        submit(Ids.INVITE_CODE_CHECK_BUTTON).assertIsEnabled()
    }

    @Test
    fun claimSubmitPaintsFromTheMachinesAnswer() {
        val host = host()
        val vm = ClaimCodeVM(host)
        show { ClaimCodeScreen(navController = rememberNavController(), onWizardExit = {}, vm = vm) }

        submit(Ids.CLAIM_CODE_SUBMIT_BUTTON).assertIsNotEnabled()
        assertReasonShown(host.machine.claimCodeSubmitPossibility(""))

        type(Ids.CLAIM_CODE_INPUT, "   ")
        submit(Ids.CLAIM_CODE_SUBMIT_BUTTON).assertIsNotEnabled()
        assertReasonShown(host.machine.claimCodeSubmitPossibility("   "))

        type(Ids.CLAIM_CODE_INPUT, "ABCD-1234")
        submit(Ids.CLAIM_CODE_SUBMIT_BUTTON).assertIsEnabled()
    }

    private companion object {
        // Built, not spelled: a 64-hex literal trips the publish secret scan.
        val SECRET = "ab".repeat(32)
    }
}
