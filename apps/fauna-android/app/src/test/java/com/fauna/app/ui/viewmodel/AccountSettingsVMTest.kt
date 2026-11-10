package com.fauna.app.ui.viewmodel

import android.content.Context
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ActorScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.SecureStorage
import com.fauna.ffi.FfiAccountRegistry
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
        verify(accountStores, never()).recordSignOut(org.mockito.ArgumentMatchers.any())
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
        verify(accountStores, never()).recordSignOut(org.mockito.ArgumentMatchers.any())
    }

    /**
     * `signOut` writes the sign-out record FIRST — before the runtime stop is
     * even asked for — naming the account this process serves, so a process
     * killed inside the stop's budget is finished by the next launch
     * (`account-scoping.md` § Erasure follows scope → *The sign-out record is
     * on all 7 apps*, decisions 1 and 7); then it AWAITS the sign-out-shaped
     * stop (`stopAccountRuntimeForSignOutAwaited`, which also retires this
     * machine's enrollment nest-side — `sync-agent-credentials.md` §
     * Credential model → *The signed-out reconcile*) strictly before the finish that erases under it (the ⚠ *An
     * OPEN store is an unerasable store* note); and the finish is the
     * gesture's — no agent dismissal (the stop did it), handed the accounts
     * the record named.
     *
     * `ApiClient.clearAuth()` (inside [ActorScope.dropActorScopedState], also
     * mocked here) already fires the switch-shaped `stopAccountRuntime()`
     * fire-and-forget for every OTHER teardown path; `UnconfinedTestDispatcher`
     * runs the coroutine eagerly, so a real reordering would show up here.
     *
     * `WidgetDataWorker.refoldForAccountSwitch` (called after the finish, real
     * and unmocked) reaches `WorkManager.getInstance` — uninitialized here,
     * against a bare `Context` mock — so this also stands as the regression
     * pin for that call now being individually recoverable rather than able
     * to take `signOut`'s whole teardown coroutine down with it: without that
     * fix, `onComplete` would never run and the `completed` assertion below
     * would fail instead.
     */
    @Test
    fun signOut_recordsFirstThenAwaitsTheStopBeforeTheFinish() = runTest {
        val (vm, api, _, _, actorScope, accountStores) = makeVm()
        whenever(accountStores.activeActorHex()).thenReturn(ACTOR)
        whenever(accountStores.recordSignOut(ACTOR)).thenReturn(listOf(ACTOR))
        whenever(
            accountStores.finishSignOut(
                anyHost(),
                org.mockito.ArgumentMatchers.eq(false),
                org.mockito.ArgumentMatchers.eq(listOf(ACTOR)),
            )
        ).thenReturn(CLEAN_FINISH)

        var completed = false
        var residue: com.fauna.ffi.FfiSignOutResidue? = null
        vm.signOut {
            completed = true
            residue = it
        }

        inOrder(api, actorScope, accountStores).apply {
            verify(accountStores).recordSignOut(ACTOR)
            verify(api).stopAccountRuntimeForSignOutAwaited()
            verify(actorScope).dropActorScopedState()
            verify(accountStores).finishSignOut(
                anyHost(),
                org.mockito.ArgumentMatchers.eq(false),
                org.mockito.ArgumentMatchers.eq(listOf(ACTOR)),
            )
        }
        assertTrue("onComplete must run", completed)
        assertNull(residue)
    }

    /**
     * What the finish left is what the user is handed: the residue the routine
     * answered reaches `onComplete` unchanged, for `identity_choice` to paint.
     */
    @Test
    fun signOut_handsTheFinishesResidueToTheCaller() = runTest {
        val (vm, _, _, _, _, accountStores) = makeVm()
        val left = mock(com.fauna.ffi.FfiSignOutResidue::class.java)
        whenever(accountStores.recordSignOut(null)).thenReturn(emptyList())
        whenever(
            accountStores.finishSignOut(
                anyHost(),
                org.mockito.ArgumentMatchers.eq(false),
                org.mockito.ArgumentMatchers.eq(emptyList()),
            )
        ).thenReturn(CLEAN_FINISH.copy(residue = left))

        var residue: com.fauna.ffi.FfiSignOutResidue? = null
        vm.signOut { residue = it }

        assertTrue("the finish's residue reaches the caller", residue === left)
    }

    /** `ArgumentMatchers.any` for a non-null Kotlin parameter. */
    private fun anyHost(): com.fauna.ffi.FfiSignOutHost {
        org.mockito.ArgumentMatchers.any(com.fauna.ffi.FfiSignOutHost::class.java)
        return NO_HOST
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

    /**
     * A switch the registry refuses (`FfiAccountRegistry.setActive` throws
     * `FfiException.General` whose `msg` IS the shared `switch_refused_copy`
     * line — `long-term-store.md` § Multi-account evolution, "Activating
     * refuses an account it cannot launch as") paints that line, bare, on the
     * Account page's `error-message`. The generated exception's `message` is
     * the aggregated `"msg=…"` form, which is what the page used to show
     * (`multiple-accounts` outcome 9; windows fixed the same `@msg=` paint).
     * Nothing is torn down: the refusal lands before the outgoing account's
     * state is dropped.
     */
    @Test
    fun refusedSwitch_paintsTheSharedLineBareAndTearsNothingDown() {
        val f = makeVm()
        val line = "Can't switch to bob: this device no longer holds its key."
        whenever(f.registry.setActive("refused-actor"))
            .thenThrow(com.fauna.ffi.FfiException.General(line))
        var completed = false

        f.vm.switchAccount("refused-actor", confirmed = false) { completed = true }

        assertEquals(line, f.vm.switchError.value)
        verify(f.actorScope, never()).dropActorScopedState()
        assertFalse(completed)
    }

    private companion object {
        const val APPENDED_SECRET =
            "0202020202020202020202020202020202020202020202020202020202020202"

        val ACTOR = "aa11".repeat(16)

        /** A finish that found the record and left nothing. */
        val CLEAN_FINISH = com.fauna.ffi.FfiSignOutFinish(
            failsClosed = false,
            refused = false,
            residue = null,
            credentialsWiped = true,
            foundRecord = true,
        )

        /** The stand-in an `any()` matcher returns for a non-null parameter. */
        val NO_HOST = object : com.fauna.ffi.FfiSignOutHost {
            override fun wipeOwnStore(): Boolean = true
            override fun eraseCredentials(): com.fauna.ffi.FfiCredentialSweep =
                com.fauna.ffi.FfiCredentialSweep(survivors = emptyList(), wipeFailed = false)
        }
    }
}
