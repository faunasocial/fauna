package com.fauna.app.ui.viewmodel

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.SessionAccount
import com.fauna.app.core.SuccessionAftermathHost
import com.fauna.app.core.feed.FeedManagerHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiFeedManager
import com.fauna.ffi.FfiReportShareStatus
import com.fauna.ffi.FfiSessionsPage
import com.fauna.ffi.FfiSessionsSnapshot
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.ArgumentMatchers.anyBoolean
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config
import uniffi.fauna_core.Possibility

/**
 * Rules 5–7 of `docs/goal/ui/README.md` § Offer only what is possible on the
 * personal settings pages (`engagement-cues.md` § Layer B,
 * `connected-apps.md` § Errors & edge cases, `sessions.md` § Errors & edge
 * cases, `succession-propagation.md` § Propagation): each control's handler
 * asks its shared answer again, refuses before its read or without the session
 * it acts through, says why on the page's `error-message`, and sends nothing —
 * never a guessed write, never a silent return. tui's
 * `personal_controls_refuse_before_their_reads_and_say_why` twin. Needs the
 * host `libfauna_ffi.so` (the answers are the shared predicates).
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class PersonalControlsPossibilityTest {

    private val context = ApplicationProvider.getApplicationContext<android.app.Application>()

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun stillLoading() = context.getString(R.string.common_still_loading)

    // ── Personalization ─────────────────────────────────────────────────────

    @Test
    fun theShareToggleRefusesWithNoFeedManagerAndSaysWhy() {
        val vm = SignalShareVM(mock(FeedManagerHost::class.java), context)

        assertTrue(vm.shareAnswer.value is Possibility.Blocked)
        vm.toggleShare()

        assertEquals(context.getString(R.string.errors_feed_not_ready), vm.errorMessage.value)
    }

    @Test
    fun theShareToggleNeverFlipsAnUnreadValue() {
        val host = mock(FeedManagerHost::class.java)
        val manager = mock(FfiFeedManager::class.java)
        whenever(host.manager()).thenReturn(manager)
        val vm = SignalShareVM(host, context)

        vm.toggleShare()

        assertEquals(stillLoading(), vm.errorMessage.value)
        runBlocking { verify(manager, never()).setSignalSharing(anyBoolean()) }
    }

    @Test
    fun theShareToggleWritesTheFlipOfTheConfirmedValueOnceRead() {
        val host = mock(FeedManagerHost::class.java)
        val manager = mock(FfiFeedManager::class.java)
        whenever(host.manager()).thenReturn(manager)
        val confirmedOn = FfiReportShareStatus(share = true, published = emptyList())
        runBlocking {
            whenever(manager.setSignalSharing(false))
                .thenReturn(FfiReportShareStatus(share = false, published = emptyList()))
        }
        val vm = SignalShareVM(host, context)

        vm.applyRead(confirmedOn)
        assertEquals(Possibility.Possible, vm.shareAnswer.value)
        vm.toggleShare()

        runBlocking { verify(manager).setSignalSharing(false) }
        assertFalse(vm.share.value)
        assertNull(vm.errorMessage.value)
    }

    @Test
    fun aFailedFirstReadKeepsTheToggleBlocked() {
        val host = mock(FeedManagerHost::class.java)
        whenever(host.manager()).thenReturn(mock(FfiFeedManager::class.java))
        val vm = SignalShareVM(host, context)

        vm.applyRead(null)

        assertTrue(vm.shareAnswer.value is Possibility.Blocked)
    }

    @Test
    fun clearEngagementRefusesWithNoFeedManagerAndSaysWhy() {
        val api = mock(ApiClient::class.java)
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        val vm = TrainedTopicsVM(api, mock(FeedManagerHost::class.java), context)

        assertTrue(vm.clearAnswer.value is Possibility.Blocked)
        vm.clearEngagementData()

        assertEquals(context.getString(R.string.errors_feed_not_ready), vm.errorMessage.value)
    }

    // ── Connected apps ──────────────────────────────────────────────────────

    @Test
    fun theExternalAppsSwitchRefusesBeforeTheRead() {
        assertTrue(ConnectedAppsVM.externalAppsAnswer(null) is Possibility.Blocked)
        val vm = ConnectedAppsVM(mock(ApiClient::class.java), mock(SessionAccount::class.java), context)

        vm.toggleExternalApps()

        assertEquals(stillLoading(), vm.refusalMessage.value)
    }

    // ── Sessions ────────────────────────────────────────────────────────────

    private fun sessionsVm(): SessionsVM =
        SessionsVM(mock(ApiClient::class.java), mock(SecureStorage::class.java))

    @Test
    fun signOutEverywhereElseRefusesBeforeTheFirstRead() {
        val vm = sessionsVm()

        assertTrue(vm.state.value.revokeOthersAnswer is Possibility.Blocked)
        vm.armRevokeOthers()

        assertFalse(vm.state.value.revokeOthersArmed)
        assertEquals("common.still_loading", vm.state.value.refusal?.key)
    }

    @Test
    fun signOutEverywhereElseRefusesWithNoOtherSignInToEnd() {
        val vm = sessionsVm()
        var calls = 0
        vm.load = { _, _, _ ->
            FfiSessionsPage(snapshot = FfiSessionsSnapshot(rows = emptyList(), error = null), locked = false)
        }
        vm.revokeOthers = { _, _, prev -> calls++; FfiSessionsPage(snapshot = prev!!, locked = false) }
        vm.hydrate {}

        vm.armRevokeOthers()
        vm.confirmRevokeOthers {}

        assertFalse(vm.state.value.revokeOthersArmed)
        assertEquals("sessions.no_other_sessions", vm.state.value.refusal?.key)
        assertEquals(0, calls)
    }

    // ── Member review ───────────────────────────────────────────────────────

    @Test
    fun removeRefusesWithoutTheConversationsSession() {
        val api = mock(ApiClient::class.java)
        runBlocking { whenever(api.memberReviewList()).thenReturn(emptyList()) }
        val aftermath = mock(SuccessionAftermathHost::class.java)
        whenever(aftermath.configStageSettled).thenReturn(MutableSharedFlow())
        val vm = MemberReviewVM(api, context, aftermath)

        assertTrue(vm.removeAnswer.value is Possibility.Blocked)
        val person = ByteArray(32) { 7 }
        vm.remove(person)

        assertEquals(
            context.getString(R.string.settings_recovery_kit_review_remove_not_ready),
            vm.errorMessage.value,
        )
        runBlocking { verify(api, never()).memberReviewRemove(person) }
    }
}
