package com.fauna.app.ui.viewmodel

import com.fauna.app.core.AftermathProgressStore
import com.fauna.app.core.ApiClient
import com.fauna.app.core.InheritedFilterMarks
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.SuccessionAftermathHost
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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

    private fun aftermath() = SuccessionAftermathHost(mock(InheritedFilterMarks::class.java), mock(AftermathProgressStore::class.java))


    private fun makeVm(): Fixture {
        val api = mock(ApiClient::class.java)
        val host = mock(OnboardingHost::class.java)
        return Fixture(PostAuthGlueVM(api, host, aftermath()), api)
    }

    // ── box-recovery.md § The plane-era recovery floor, (c) The writes — the
    // custody leg is the only capture, and a run that ends with custody
    // unconfirmed for an admin surfaces on the recovery-custody warning. ──

    @Test
    fun runDeploymentSeedCustodyLeg_nothingOwedShowsNothing() = runTest {
        val (vm, api) = makeVm()
        whenever(api.selfHealDeploymentSeedCustody()).thenReturn(null)
        assertNull(vm.runDeploymentSeedCustodyLeg())
        verify(api).selfHealDeploymentSeedCustody()
    }

    @Test
    fun runDeploymentSeedCustodyLeg_passesTheSharedWarningThrough() = runTest {
        // Which warning a run owes is the shared Rust mapping's call
        // (`custody_leg_warning`, tested in fauna-client-config); the glue only
        // hands that LocalizedText on to the resolver, untouched.
        val warning = uniffi.fauna_core.LocalizedText("launch.recovery_custody_mismatch", emptyMap())
        val (vm, api) = makeVm()
        whenever(api.selfHealDeploymentSeedCustody()).thenReturn(warning)
        assertEquals(warning, vm.runDeploymentSeedCustodyLeg())
    }

    // ── identity-succession.md § The RecoveryKey → *Creation UX*: the kit the
    // sign-up `recovery_kit` screen minted registers at the signed-in edge —
    // THAT root (the one the user wrote down), never a fresh one. ──

    @Test
    fun registerDeferredRecoveryKit_registersTheConfirmedKit() = runTest {
        val api = mock(ApiClient::class.java)
        val host = mock(OnboardingHost::class.java)
        whenever(host.consumePendingRecoveryKit()).thenReturn(KIT)
        PostAuthGlueVM(api, host, aftermath()).registerDeferredRecoveryKit()
        verify(api).registerDeferredRecoveryKit(KIT)
    }

    @Test
    fun registerDeferredRecoveryKit_aSkippedKitRegistersNothing() = runTest {
        val api = mock(ApiClient::class.java)
        val host = mock(OnboardingHost::class.java)
        whenever(host.consumePendingRecoveryKit()).thenReturn(null)
        PostAuthGlueVM(api, host, aftermath()).registerDeferredRecoveryKit()
        verify(api, never()).registerDeferredRecoveryKit(org.mockito.ArgumentMatchers.anyString())
    }

    // ── onboarding-provisioning.md § 4: the wizard-verified DNS credential is
    // sealed at the signed-in edge, once, over the shared rule. ──

    @Test
    fun sealCapturedDnsCredential_sealsTheCapturedCredential() = runTest {
        val api = mock(ApiClient::class.java)
        val host = mock(OnboardingHost::class.java)
        val cred = com.fauna.ffi.onboarding.CapturedDnsCredential(
            "hetzner", mapOf("api_token" to "t"), "hetzner (example.test)",
        )
        whenever(host.consumePendingDnsCredential()).thenReturn(cred)
        PostAuthGlueVM(api, host, aftermath()).sealCapturedDnsCredential()
        verify(api).sealCapturedDnsCredential(cred)
    }

    @Test
    fun sealCapturedDnsCredential_nothingCapturedSealsNothing() = runTest {
        val api = mock(ApiClient::class.java)
        val host = mock(OnboardingHost::class.java)
        whenever(host.consumePendingDnsCredential()).thenReturn(null)
        PostAuthGlueVM(api, host, aftermath()).sealCapturedDnsCredential()
        // Mockito's `any()` answers null for the non-null Kotlin parameter (an NPE
        // that leaves the verification unfinished for the next test) — and with
        // nothing captured the VM touches the api not at all.
        org.mockito.Mockito.verifyNoInteractions(api)
    }

    private companion object {
        const val KIT =
            "a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf"
    }
}
