package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.OnboardingHost
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runTest
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
 * live in the shared Rust step, pinned there. FFI-free (mocks [ApiClient] /
 * [OnboardingHost] directly, mirroring [PostAuthGlueVMTest]).
 */
@ExperimentalCoroutinesApi
class MailEnableGlueVMTest {

    private data class Fixture(
        val vm: MailEnableGlueVM,
        val api: ApiClient,
        val host: OnboardingHost,
    )

    private fun makeVm(): Fixture {
        val api = mock(ApiClient::class.java)
        val host = mock(OnboardingHost::class.java)
        val vm = MailEnableGlueVM(api, host)
        return Fixture(vm, api, host)
    }

    @Test
    fun freshOnboarding_makesTheOneSharedCallWithAllFourLatchedIntents() = runTest {
        val (vm, api, host) = makeVm()
        whenever(host.consumePendingServingEnablement())
            .thenReturn(OnboardingHost.ServingEnablementIntents(email = true, caldav = false, carddav = true, webdav = false))

        vm.applyPostClaimServingEnablement()

        verify(api).applyPostClaimServingEnablement(
            email = true, caldav = false, carddav = true, webdav = false,
        )
    }

    @Test
    fun returningUserRelaunch_nullLatch_firesNothing() = runTest {
        val (vm, api, host) = makeVm()
        whenever(host.consumePendingServingEnablement()).thenReturn(null)

        vm.applyPostClaimServingEnablement()

        verifyNoInteractions(api)
    }
}
