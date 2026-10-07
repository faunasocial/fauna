package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.OnboardingHost
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * [PostAuthGlueVM]'s user-visible leg — the deployment-seed custody leg and
 * the recovery-custody warning it drives. FFI-free (mocks [ApiClient] /
 * [OnboardingHost] directly, mirroring [MailEnableGlueVMTest]).
 */
@ExperimentalCoroutinesApi
class PostAuthGlueVMTest {

    private data class Fixture(val vm: PostAuthGlueVM, val api: ApiClient)

    private fun makeVm(): Fixture {
        val api = mock(ApiClient::class.java)
        val host = mock(OnboardingHost::class.java)
        return Fixture(PostAuthGlueVM(api, host), api)
    }

    // ── box-recovery.md § The plane-era recovery floor, (c) The writes — the
    // custody leg is the only capture, and a run that ends with custody
    // unconfirmed for an admin surfaces on the recovery-custody warning. ──

    private suspend fun custodyOutcomeFor(
        leg: com.fauna.ffi.FfiDeploymentSeedSelfHeal,
    ): PostAuthGlueVM.RecoveryCustodyOutcome {
        val (vm, api) = makeVm()
        whenever(api.selfHealDeploymentSeedCustody()).thenReturn(leg)
        val outcome = vm.runDeploymentSeedCustodyLeg()
        verify(api).selfHealDeploymentSeedCustody()
        return outcome
    }

    @Test
    fun runDeploymentSeedCustodyLeg_heldCapturedOrNotAdminShowsNothing() = runTest {
        assertEquals(
            PostAuthGlueVM.RecoveryCustodyOutcome.OK,
            custodyOutcomeFor(com.fauna.ffi.FfiDeploymentSeedSelfHeal.AlreadyCustodied),
        )
        assertEquals(
            PostAuthGlueVM.RecoveryCustodyOutcome.OK,
            custodyOutcomeFor(com.fauna.ffi.FfiDeploymentSeedSelfHeal.NotAdmin),
        )
        assertEquals(
            PostAuthGlueVM.RecoveryCustodyOutcome.OK,
            custodyOutcomeFor(
                com.fauna.ffi.FfiDeploymentSeedSelfHeal.Captured(
                    com.fauna.ffi.FfiDeploymentSeedCapture.WROTE,
                ),
            ),
        )
    }

    @Test
    fun runDeploymentSeedCustodyLeg_unconfirmedCustodyWarns() = runTest {
        assertEquals(
            PostAuthGlueVM.RecoveryCustodyOutcome.NOT_PROTECTED_FAILED,
            custodyOutcomeFor(com.fauna.ffi.FfiDeploymentSeedSelfHeal.HandoffUnavailable),
        )
        assertEquals(
            PostAuthGlueVM.RecoveryCustodyOutcome.NOT_PROTECTED_FAILED,
            custodyOutcomeFor(com.fauna.ffi.FfiDeploymentSeedSelfHeal.NestHoldsNoSeed),
        )
        assertEquals(
            PostAuthGlueVM.RecoveryCustodyOutcome.NOT_PROTECTED_MISMATCH,
            custodyOutcomeFor(
                com.fauna.ffi.FfiDeploymentSeedSelfHeal.Captured(
                    com.fauna.ffi.FfiDeploymentSeedCapture.REFUSED_MISMATCH,
                ),
            ),
        )
    }

    @Test
    fun runDeploymentSeedCustodyLeg_aThrownLegWarns() = runTest {
        val (vm, api) = makeVm()
        whenever(api.selfHealDeploymentSeedCustody())
            .thenThrow(RuntimeException("bound nest id unresolved"))

        assertEquals(
            PostAuthGlueVM.RecoveryCustodyOutcome.NOT_PROTECTED_FAILED,
            vm.runDeploymentSeedCustodyLeg(),
        )
    }

    // ── identity-succession.md § The RecoveryKey → *Creation UX*: the kit the
    // sign-up `recovery_kit` screen minted registers at the signed-in edge —
    // THAT root (the one the user wrote down), never a fresh one. ──

    @Test
    fun registerDeferredRecoveryKit_registersTheConfirmedKit() = runTest {
        val api = mock(ApiClient::class.java)
        val host = mock(OnboardingHost::class.java)
        whenever(host.consumePendingRecoveryKit()).thenReturn(KIT)
        PostAuthGlueVM(api, host).registerDeferredRecoveryKit()
        verify(api).registerDeferredRecoveryKit(KIT)
    }

    @Test
    fun registerDeferredRecoveryKit_aSkippedKitRegistersNothing() = runTest {
        val api = mock(ApiClient::class.java)
        val host = mock(OnboardingHost::class.java)
        whenever(host.consumePendingRecoveryKit()).thenReturn(null)
        PostAuthGlueVM(api, host).registerDeferredRecoveryKit()
        verify(api, never()).registerDeferredRecoveryKit(org.mockito.ArgumentMatchers.anyString())
    }

    private companion object {
        const val KIT =
            "a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf"
    }
}
