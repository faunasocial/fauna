package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.CriticalAlertsHost
import com.fauna.app.core.OnboardingHost
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test
import org.mockito.ArgumentMatchers.anyBoolean
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import uniffi.fauna_client_mail_settings.MailSettingsMachine

/**
 * The calendar-only / contacts-only shared-MSEK mint gate (`docs/goal/behavior/caldav-server.md` § Independent enablement) —
 * [MailEnableGlueVM.applyPendingCaldavEnable] / [MailEnableGlueVM.applyPendingCarddavEnable]
 * must mint the admin's shared MSEK exactly when their own toggle was
 * requested AND no earlier sibling in the mail → CalDAV → CardDAV sequence
 * already minted it — mirroring apple's `applyPendingCaldavEnable` /
 * `applyPendingCarddavEnable` gating (`MailEnableGlue.swift`). FFI-free
 * (mocks [ApiClient] / [OnboardingHost] directly, mirroring [AppLaunchVMTest] /
 * [DevicesVMTest]) — no Robolectric/JNA needed since nothing here crosses the
 * UniFFI boundary for real.
 */
@ExperimentalCoroutinesApi
class MailEnableGlueVMTest {

    private data class Fixture(
        val vm: MailEnableGlueVM,
        val api: ApiClient,
        val host: OnboardingHost,
        val machine: MailSettingsMachine,
    )

    private fun makeVm(): Fixture {
        val api = mock(ApiClient::class.java)
        val host = mock(OnboardingHost::class.java)
        val criticalAlertsHost = mock(CriticalAlertsHost::class.java)
        val machine = mock(MailSettingsMachine::class.java)
        val vm = MailEnableGlueVM(api, host, criticalAlertsHost)
        return Fixture(vm, api, host, machine)
    }

    // ── CalDAV — mints the shared MSEK iff caldav was requested AND mail was not ──

    @Test
    fun caldavRequestedAndMailWasNot_mintsTheSharedMsek() = runTest {
        val (vm, api, host, machine) = makeVm()
        whenever(host.consumePendingCaldavEnable()).thenReturn(true)
        whenever(api.checkIsAdmin()).thenReturn(true)
        whenever(api.buildMailSettingsMachine()).thenReturn(machine)

        vm.applyPendingCaldavEnable(mailWasEnabled = false)

        verify(api).setCaldavEnabled(true)
        verify(machine).enableCaldavMailboxWithGeneratedPassword("Default")
    }

    @Test
    fun caldavRequestedButMailAlreadyMinted_togglesButDoesNotMintAgain() = runTest {
        val (vm, api, host, _) = makeVm()
        whenever(host.consumePendingCaldavEnable()).thenReturn(true)

        vm.applyPendingCaldavEnable(mailWasEnabled = true)

        verify(api).setCaldavEnabled(true)
        verify(api, never()).checkIsAdmin()
        verify(api, never()).buildMailSettingsMachine()
    }

    @Test
    fun caldavNotRequested_isANoOp() = runTest {
        val (vm, api, host, _) = makeVm()
        whenever(host.consumePendingCaldavEnable()).thenReturn(false)

        vm.applyPendingCaldavEnable(mailWasEnabled = false)

        verify(api, never()).setCaldavEnabled(anyBoolean())
        verify(api, never()).buildMailSettingsMachine()
    }

    @Test
    fun caldavRequestedAndMailWasNotButCallerIsNotAdmin_doesNotMint() = runTest {
        val (vm, api, host, _) = makeVm()
        whenever(host.consumePendingCaldavEnable()).thenReturn(true)
        whenever(api.checkIsAdmin()).thenReturn(false)

        vm.applyPendingCaldavEnable(mailWasEnabled = false)

        verify(api).setCaldavEnabled(true)
        verify(api, never()).buildMailSettingsMachine()
    }

    // ── CardDAV — mints iff carddav was requested AND neither mail NOR CalDAV was ──

    @Test
    fun carddavRequestedAndNeitherMailNorCaldavWas_mintsTheSharedMsek() = runTest {
        val (vm, api, host, machine) = makeVm()
        whenever(host.consumePendingCarddavEnable()).thenReturn(true)
        whenever(api.checkIsAdmin()).thenReturn(true)
        whenever(api.buildMailSettingsMachine()).thenReturn(machine)

        vm.applyPendingCarddavEnable(mailWasEnabled = false, caldavWasEnabled = false)

        verify(api).setCarddavEnabled(true)
        verify(machine).enableCarddavMailboxWithGeneratedPassword("Default")
    }

    @Test
    fun carddavRequestedButMailAlreadyMinted_doesNotMintAgain() = runTest {
        val (vm, api, host, _) = makeVm()
        whenever(host.consumePendingCarddavEnable()).thenReturn(true)

        vm.applyPendingCarddavEnable(mailWasEnabled = true, caldavWasEnabled = false)

        verify(api).setCarddavEnabled(true)
        verify(api, never()).buildMailSettingsMachine()
    }

    @Test
    fun carddavRequestedButCaldavAlreadyMinted_doesNotMintAgain() = runTest {
        val (vm, api, host, _) = makeVm()
        whenever(host.consumePendingCarddavEnable()).thenReturn(true)

        vm.applyPendingCarddavEnable(mailWasEnabled = false, caldavWasEnabled = true)

        verify(api).setCarddavEnabled(true)
        verify(api, never()).buildMailSettingsMachine()
    }

    @Test
    fun carddavNotRequested_isANoOp() = runTest {
        val (vm, api, host, _) = makeVm()
        whenever(host.consumePendingCarddavEnable()).thenReturn(false)

        vm.applyPendingCarddavEnable(mailWasEnabled = false, caldavWasEnabled = false)

        verify(api, never()).setCarddavEnabled(anyBoolean())
        verify(api, never()).buildMailSettingsMachine()
    }

    // ── Non-consuming peeks — the launch-glue call site snapshots these BEFORE
    // provisionMailAtFirstSetup/applyPendingCaldavEnable consume their own
    // latches, so the mint gates above never depend on call order. ──

    @Test
    fun peekMailWasRequested_readsTheHostLatchWithoutConsumingIt() {
        val (vm, _, host, _) = makeVm()
        whenever(host.peekPendingFirstSetupMail()).thenReturn(true)

        assertEquals(true, vm.peekMailWasRequested())
        verify(host, never()).consumePendingFirstSetupMail()
    }

    @Test
    fun peekMailWasRequested_nullLatchReadsAsFalse() {
        val (vm, _, host, _) = makeVm()
        whenever(host.peekPendingFirstSetupMail()).thenReturn(null)

        assertEquals(false, vm.peekMailWasRequested())
    }

    @Test
    fun peekCaldavWasRequested_readsTheHostLatchWithoutConsumingIt() {
        val (vm, _, host, _) = makeVm()
        whenever(host.peekPendingCaldavEnable()).thenReturn(true)

        assertEquals(true, vm.peekCaldavWasRequested())
        verify(host, never()).consumePendingCaldavEnable()
    }
}
