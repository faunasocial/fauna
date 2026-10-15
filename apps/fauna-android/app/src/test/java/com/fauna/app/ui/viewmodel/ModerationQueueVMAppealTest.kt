package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.mockito.ArgumentMatchers.anyString
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * The moderation queue's appeal form (`docs/goal/behavior/moderation.md` § Legal
 * takedown — the transparency triple's second leg): the three properties each app
 * must keep, and the shared fold gating the dispatch. [ApiClient] is mocked, as in
 * [ModerationQueueVMTest]; the shared `appealFormView` / `appealVerdict` run for
 * real, so this needs the host `.so` (`just android-host-test`).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ModerationQueueVMAppealTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun makeVm(): Pair<ModerationQueueVM, ApiClient> {
        val api = mock(ApiClient::class.java)
        runTest {
            whenever(api.moderationActions()).thenThrow(RuntimeException("no queue in this test"))
            whenever(api.abuseReportMine()).thenThrow(RuntimeException("no ledger in this test"))
        }
        return ModerationQueueVM(api) to api
    }

    @Test
    fun aReasonlessAppealNeverReachesTheWire() = runTest {
        val (vm, api) = makeVm()
        vm.openAppeal("post-1")

        vm.submitAppeal()

        verify(api, never()).moderationAppeal(anyString(), anyString())
        assertNotNull("the form stays open", vm.appealDraft.value)
    }

    @Test
    fun theDraftKeepsTheContentIdItWasOpenedAgainst() = runTest {
        val (vm, api) = makeVm()
        vm.openAppeal("post-1")
        vm.setAppealReason("  I hold the licence.  ")
        // The queue refetches (here: fails and keeps its rows) — the draft is not
        // keyed on a row index, so it still names the decision it was opened on.
        vm.load()

        vm.submitAppeal()

        verify(api).moderationAppeal("post-1", "I hold the licence.")
    }

    @Test
    fun aRecordedAppealClosesTheFormAndSaysSoOnAppealStatus() = runTest {
        val (vm, _) = makeVm()
        vm.openAppeal("post-1")
        vm.setAppealReason("I hold the licence.")
        // The construction-time ledger read is stubbed to fail, so the error line
        // is not empty to begin with — the appeal must leave it exactly as it was.
        val errorBefore = vm.errorMessage.value

        vm.submitAppeal()

        assertNull("success closes the form", vm.appealDraft.value)
        assertEquals("moderation.appeal_recorded", vm.appealStatus.value?.key)
        assertEquals("a recorded appeal is not a page error", errorBefore, vm.errorMessage.value)
    }

    @Test
    fun aRefusedAppealKeepsTheFormOpenAndNeverBlanksThePage() = runTest {
        val (vm, api) = makeVm()
        whenever(api.moderationAppeal("post-1", "I hold the licence."))
            .thenThrow(RuntimeException("fauna.moderation.not_found"))
        vm.openAppeal("post-1")
        vm.setAppealReason("I hold the licence.")
        val errorBefore = vm.errorMessage.value

        vm.submitAppeal()

        assertEquals("the form stays open so the reason can be resent", "post-1", vm.appealDraft.value?.contentId)
        assertEquals("I hold the licence.", vm.appealDraft.value?.reason)
        assertEquals("moderation.appeal_failed", vm.appealStatus.value?.key)
        assertEquals("a refusal is form feedback, not error-message", errorBefore, vm.errorMessage.value)
    }

    @Test
    fun cancellingDropsTheDraftAndTheStaleStatusTogether() = runTest {
        val (vm, _) = makeVm()
        vm.openAppeal("post-1")
        vm.setAppealReason("I hold the licence.")
        vm.submitAppeal()
        vm.openAppeal("post-1")

        vm.cancelAppeal()

        assertNull(vm.appealDraft.value)
        assertNull(vm.appealStatus.value)
    }
}
