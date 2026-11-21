package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.AppMessages
import com.fauna.app.core.OnboardingHost
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.verify
import org.mockito.Mockito.verifyNoInteractions
import org.mockito.Mockito.`when` as whenever

/**
 * The post-claim serving enablement hand-off (`docs/goal/behavior/onboarding.md`
 * § 3b *Mechanism*) — [MailEnableGlueVM.applyPostClaimServingEnablement] takes the
 * four onboarding latches together once and makes the ONE shared call with them;
 * it fires nothing itself and does nothing on a returning-user relaunch. The
 * `am_i_admin` discrimination, the DAV toggles and the one-MSEK-mint-path gate
 * live in the shared Rust step, pinned there. A step the nest refused for good
 * comes back as the page's `error-message` text and is painted on the shell's
 * [AppMessages] error (`onboarding.md` § 3b *The step completes; it is not
 * fired once*). FFI-free (mocks [ApiClient] /
 * [OnboardingHost] directly, mirroring [PostAuthGlueVMTest]).
 */
@ExperimentalCoroutinesApi
class MailEnableGlueVMTest {

    private data class Fixture(
        val vm: MailEnableGlueVM,
        val api: ApiClient,
        val host: OnboardingHost,
        val messages: AppMessages,
    )

    private fun makeVm(): Fixture {
        val api = mock(ApiClient::class.java)
        val host = mock(OnboardingHost::class.java)
        val vm = MailEnableGlueVM(api, host)
        return Fixture(vm, api, host, AppMessages())
    }

    @Test
    fun freshOnboarding_makesTheOneSharedCallWithAllFourLatchedIntents() = runTest {
        val (vm, api, host, messages) = makeVm()
        whenever(host.consumePendingServingEnablement())
            .thenReturn(OnboardingHost.ServingEnablementIntents(email = true, caldav = false, carddav = true, webdav = false))

        vm.applyPostClaimServingEnablement(messages)

        verify(api).applyPostClaimServingEnablement(
            email = true, caldav = false, carddav = true, webdav = false,
        )
    }

    @Test
    fun returningUserRelaunch_nullLatch_firesNothing() = runTest {
        val (vm, api, host, messages) = makeVm()
        whenever(host.consumePendingServingEnablement()).thenReturn(null)

        vm.applyPostClaimServingEnablement(messages)

        verifyNoInteractions(api)
    }

    @Test
    fun refusalForGood_isPaintedOnTheCurrentPagesErrorMessage() = runTest {
        val (vm, api, host, messages) = makeVm()
        whenever(host.consumePendingServingEnablement())
            .thenReturn(OnboardingHost.ServingEnablementIntents(email = true, caldav = true, carddav = false, webdav = false))
        whenever(api.applyPostClaimServingEnablement(email = true, caldav = true, carddav = false, webdav = false))
            .thenReturn("Mail could not be turned on: the nest refused it")

        vm.applyPostClaimServingEnablement(messages)

        assertEquals("Mail could not be turned on: the nest refused it", messages.error.value)
    }

    @Test
    fun everyStepSucceeded_leavesTheErrorMessageAlone() = runTest {
        val (vm, api, host, messages) = makeVm()
        whenever(host.consumePendingServingEnablement())
            .thenReturn(OnboardingHost.ServingEnablementIntents(email = true, caldav = false, carddav = false, webdav = false))
        whenever(api.applyPostClaimServingEnablement(email = true, caldav = false, carddav = false, webdav = false))
            .thenReturn(null)

        vm.applyPostClaimServingEnablement(messages)

        assertNull(messages.error.value)
    }
}
