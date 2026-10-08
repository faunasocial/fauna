package com.fauna.app.ui.viewmodel

import android.content.Context
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ActorScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.SecureStorage
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.FfiEraseResidueView
import com.fauna.ffi.FfiEraseSweep
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.inOrder
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * `AccountSettingsVM.deleteAccount` (`settings-delete-account-button` —
 * `settings.md` § Where logic lives → Account deletion, ruled 2026-08-26):
 * `fauna.account.delete` only SCHEDULES a 14-day cancellable pending action,
 * so on success the app must make NO local teardown — no registry /
 * secure-storage / actor-scope clear, no store erase. Pins the fix, where the prior code tore the session down at request time on
 * the false premise that the nest-side account was already gone.
 *
 * FFI-free (mocks every collaborator, mirrors [DevicesVMTest]) — `init`
 * collects [ApiClient.reconnectTick] on `viewModelScope`, so
 * `Dispatchers.setMain(UnconfinedTestDispatcher())` is required exactly as
 * there, and `[ApiClient.reconnectTick]` must be stubbed before construction.
 *
 * The android e2e leg (`test_delete_account.py --app android`) is
 * emulator-host-gated (android emulator) and not runnable from this
 * session — this VM test is the verification ceiling here.
 */
@ExperimentalCoroutinesApi
class AccountSettingsVMTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private data class Fixture(
        val vm: AccountSettingsVM,
        val api: ApiClient,
        val registry: FfiAccountRegistry,
        val secureStorage: SecureStorage,
        val actorScope: ActorScope,
        val accountStores: AccountStores,
        val onboardingHost: OnboardingHost,
    )

    private fun makeVm(): Fixture {
        val api = mock(ApiClient::class.java)
        val secureStorage = mock(SecureStorage::class.java)
        val registry = mock(FfiAccountRegistry::class.java)
        val onboardingHost = mock(OnboardingHost::class.java)
        val actorScope = mock(ActorScope::class.java)
        val accountStores = mock(AccountStores::class.java)
        val context = mock(Context::class.java)
        // Stubbed BEFORE construction: `AccountSettingsVM.init` collects this
        // the moment the VM is built (mirrors DevicesVMTest).
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow(replay = 0, extraBufferCapacity = 1))
        val vm = AccountSettingsVM(
            api, secureStorage, registry, onboardingHost, actorScope, accountStores, context,
            com.fauna.app.core.StolenCeremonyHold(),
        )
        return Fixture(vm, api, registry, secureStorage, actorScope, accountStores, onboardingHost)
    }

    @Test
    fun successfulDelete_touchesNoLocalStateAndSetsTheReceiptFlag() = runTest {
        val (vm, _, registry, secureStorage, actorScope, accountStores) = makeVm()

        vm.deleteAccount()

        assertTrue(vm.deleteAccountSuccess.value)
        assertNull(vm.errorMessage.value)
        verify(registry, never()).clearAll()
        verify(secureStorage, never()).clear()
        verify(actorScope, never()).dropActorScopedState()
        verify(accountStores, never()).eraseAllAccounts()
    }

    @Test
    fun failedDelete_setsErrorAndLeavesTheReceiptFlagClear() = runTest {
        val (vm, api, registry, secureStorage, actorScope, accountStores) = makeVm()
        whenever(api.deleteAccount()).thenThrow(RuntimeException("boom"))

        vm.deleteAccount()

        assertEquals("boom", vm.errorMessage.value)
        assertFalse(vm.deleteAccountSuccess.value)
        verify(registry, never()).clearAll()
        verify(secureStorage, never()).clear()
        verify(actorScope, never()).dropActorScopedState()
        verify(accountStores, never()).eraseAllAccounts()
    }

    /**
     * `signOut` AWAITS the shared account-runtime stop before the erase that
     * follows (`account-scoping.md` § Erasure follows scope, the ⚠ *An OPEN
     * store is an unerasable store* note, and "that call before its
     * `account_state_erase_*`") — and, since
     * 2026-09-15, the SIGN-OUT-shaped stop specifically
     * (`stopAccountRuntimeForSignOutAwaited`), which also retires this
     * machine's enrollment nest-side before the erase takes the writer key
     * with it (`sync-agent-credentials.md` § Credential model → *The
     * signed-out reconcile*).
     *
     * `ApiClient.clearAuth()` (inside [ActorScope.dropActorScopedState], also
     * mocked here) already fires the switch-shaped `stopAccountRuntime()`
     * fire-and-forget for every OTHER teardown path; this pins that sign-out
     * does NOT rely on that race, by asserting the AWAITED call happens
     * strictly before the erase in program order. `UnconfinedTestDispatcher`
     * runs the coroutine eagerly, so a real race (erase reordered before the
     * await resolves) would show up here as a real ordering violation, not
     * just an uninteresting "both got called".
     *
     * `WidgetDataWorker.refoldForAccountSwitch` (called after the erase, real
     * and unmocked) reaches `WorkManager.getInstance` — uninitialized here,
     * against a bare `Context` mock — so this also stands as the regression
     * pin for that call now being individually recoverable rather than able
     * to take `signOut`'s whole teardown coroutine down with it: without that
     * fix, `onComplete` would never run and the `completed` assertion below
     * would fail instead.
     */
    @Test
    fun signOut_awaitsTheAccountRuntimeStopBeforeErasing() = runTest {
        val (vm, api, registry, _, _, accountStores) = makeVm()
        val cleanCredentials =
            com.fauna.ffi.FfiCredentialSweep(survivors = emptyList(), wipeFailed = false)
        whenever(registry.clearAll()).thenReturn(cleanCredentials)
        whenever(registry.reverifyErase(cleanCredentials)).thenReturn(cleanCredentials)
        whenever(accountStores.eraseAllAccounts())
            .thenReturn(
                FfiEraseSweep(
                    erased = 0u,
                    survivors = emptyList(),
                    residue = FfiEraseResidueView(
                        survivors = 0u,
                        credentialsSurvived = false,
                        owesWork = false,
                    ),
                )
            )

        var completed = false
        var residue: com.fauna.ffi.FfiSignOutResidue? = null
        vm.signOut {
            completed = true
            residue = it
        }

        inOrder(api, accountStores).apply {
            verify(api).stopAccountRuntimeForSignOutAwaited()
            verify(accountStores).eraseAllAccounts()
        }
        assertTrue("onComplete must run", completed)
        assertNull(residue)
    }

    /**
     * The credential half of `signOut` is read back AFTER the LAST wipe
     * (`account-scoping.md` § Erasure follows scope → *the credential half is a
     * residue class too*, "Fold after the LAST wipe"). `registry.clearAll()`
     * reads back what it deleted, `secureStorage.clear()` then resets the whole
     * store, and only `reverifyErase` — asked after that reset — knows what is
     * still there. A seat that folded `clearAll`'s own answer, or re-asked
     * before the reset, would report credentials the reset already took.
     *
     * The two sweeps differ on purpose: `inOrder` pins the sequence, and the
     * `recordResidue` argument pins which answer reached the user's line.
     */
    @Test
    fun signOut_foldsTheCredentialReadBackTakenAfterTheSecureStorageReset() = runTest {
        val (vm, _, registry, secureStorage, _, accountStores) = makeVm()
        val beforeReset =
            com.fauna.ffi.FfiCredentialSweep(survivors = listOf("fauna/index"), wipeFailed = false)
        val afterReset =
            com.fauna.ffi.FfiCredentialSweep(survivors = emptyList(), wipeFailed = false)
        val sweep = FfiEraseSweep(
            erased = 0u,
            survivors = emptyList(),
            residue = FfiEraseResidueView(
                survivors = 0u,
                credentialsSurvived = false,
                owesWork = false,
            ),
        )
        whenever(registry.clearAll()).thenReturn(beforeReset)
        whenever(registry.reverifyErase(beforeReset)).thenReturn(afterReset)
        whenever(accountStores.eraseAllAccounts()).thenReturn(sweep)

        vm.signOut { }

        inOrder(registry, secureStorage, accountStores).apply {
            verify(registry).clearAll()
            verify(secureStorage).clear()
            verify(registry).reverifyErase(beforeReset)
            verify(accountStores).recordResidue(sweep, afterReset)
        }
    }

    /**
     * The append terminal registers the identity the WIZARD MACHINE holds, at
     * the `LoggedIn` exit's own nest — never a store read: the append confirm
     * wrote nothing (`long-term-store.md` § Downgrade mirror + abandoned-append
     * recovery), so the machine is the only place the appended identity exists.
     * Its device id is resolved for THAT identity, then the switch runs.
     */
    @Test
    fun completeAddAccount_registersTheMachinesIdentityAtTheExitsNestAndSwitches() {
        val f = makeVm()
        val machine = mock(com.fauna.ffi.onboarding.OnboardingMachine::class.java)
        whenever(f.onboardingHost.machine).thenReturn(machine)
        whenever(machine.effectiveSecret()).thenReturn(APPENDED_SECRET)
        whenever(f.secureStorage.deviceIdFor(APPENDED_SECRET)).thenReturn("dev-appended")
        whenever(f.registry.addAccount(APPENDED_SECRET, "https://b.example", "dev-appended"))
            .thenReturn("appended-actor")
        var completed = false

        f.vm.completeAddAccount(
            com.fauna.ffi.onboarding.WizardOutcome.LoggedIn(nestUrl = "https://b.example", handle = "bob"),
        ) { completed = true }

        verify(f.registry).addAccount(APPENDED_SECRET, "https://b.example", "dev-appended")
        verify(f.registry).setActive("appended-actor")
        verify(f.actorScope).dropActorScopedState()
        assertTrue(completed)
        assertNull(f.vm.switchError.value)
    }

    /** No identity in the machine: nothing is registered and the error surfaces. */
    @Test
    fun completeAddAccount_withNoIdentityInTheMachine_registersNothing() {
        val f = makeVm()
        val machine = mock(com.fauna.ffi.onboarding.OnboardingMachine::class.java)
        whenever(f.onboardingHost.machine).thenReturn(machine)
        whenever(machine.effectiveSecret()).thenReturn(null)
        var completed = false

        f.vm.completeAddAccount(
            com.fauna.ffi.onboarding.WizardOutcome.LoggedIn(nestUrl = "https://b.example", handle = "bob"),
        ) { completed = true }

        verify(f.registry, never()).setActive(org.mockito.ArgumentMatchers.anyString())
        assertFalse(completed)
        assertTrue(f.vm.switchError.value != null)
    }

    private companion object {
        const val APPENDED_SECRET =
            "0202020202020202020202020202020202020202020202020202020202020202"
    }
}
