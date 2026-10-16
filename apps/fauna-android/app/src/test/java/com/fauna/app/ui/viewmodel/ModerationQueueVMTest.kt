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
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * The moderation queue's `train-correction-button` on a **server** row
 * (`docs/goal/behavior/moderation.md` § State & data shape → *Training
 * correction*): the VM makes the ONE shared call
 * (`MailSettingsMachine::train_moderation_correction` — sealed model write, then
 * `fauna.moderation.train` always, pinned in `fauna-client-mail-settings`) and
 * keeps no sealed-path early return of its own. FFI-free ([ApiClient] mocked, as
 * in [MutedWordsVMTest]); the construction-time loads are stubbed to fail, which
 * the VM turns into an error line.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ModerationQueueVMTest {

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
    fun serverRowCorrection_makesTheOneSharedHamCall() = runTest {
        val (vm, api) = makeVm()

        vm.correct("post-1", isLocal = false)

        verify(api).trainModerationCorrection("post-1", false)
    }

    @Test
    fun serverRowCorrection_failureSurfacesOnTheErrorLine() = runTest {
        val (vm, api) = makeVm()
        whenever(api.trainModerationCorrection("post-2", false))
            .thenThrow(RuntimeException("train failed"))

        vm.correct("post-2", isLocal = false)

        assertEquals("train failed", vm.errorMessage.value)
    }

    @Test
    fun localRowCorrection_doesNotTouchTheNestTrain() = runTest {
        val (vm, api) = makeVm()
        whenever(api.moderationMessageBody("msg-1")).thenReturn(null)

        vm.correct("msg-1", isLocal = true)

        verify(api, never()).trainModerationCorrection("msg-1", false)
    }
}
