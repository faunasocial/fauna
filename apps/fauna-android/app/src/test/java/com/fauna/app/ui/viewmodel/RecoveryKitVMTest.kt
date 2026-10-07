package com.fauna.app.ui.viewmodel

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.ApiClient
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiMintedKit
import com.fauna.ffi.FfiRecoveryKitStatus
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
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
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.verifyNoInteractions
import org.robolectric.annotation.Config
import org.mockito.Mockito.`when` as whenever

/**
 * [RecoveryKitVM]'s handler-side contract (`docs/goal/ui/settings.md`
 * § Recovery kit): the kit-in-hand ceremonies refuse an empty field OUT LOUD
 * before any ceremony runs (a test agent driving the id reaches the handler
 * whatever the render did), a mint is displayed once with its copy/QR URI, a
 * failure shows nothing, and the held secret never outlives the page or the
 * identity it was minted for.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class RecoveryKitVMTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val context = ApplicationProvider.getApplicationContext<android.app.Application>()
    private val phraseRequired = context.getString(com.fauna.app.R.string.settings_recovery_kit_kit_phrase_required)

    private val registered = FfiRecoveryKitStatus(
        kind = "registered",
        allowsCreate = false,
        allowsReplace = true,
        allowsLost = true,
        allowsStolen = true,
        allowsEscrowReseal = false,
        pendingNewPubkeyHex = null,
        pendingLandsAt = null,
    )

    private fun makeVm(api: ApiClient = mock(ApiClient::class.java)) = RecoveryKitVM(api, context) to api

    @Test
    fun replaceWithAnEmptyFieldRefusesBeforeAnyCeremony() = runTest {
        val (vm, api) = makeVm()

        vm.createOrReplaceKit(usingHeldPhrase = true)

        assertEquals(phraseRequired, vm.errorMessage.value)
        verifyNoInteractions(api)
        assertFalse(vm.state.value.busy)
    }

    @Test
    fun vetoWithAnEmptyFieldRefusesBeforeAnyCeremony() = runTest {
        val (vm, api) = makeVm()

        vm.vetoPendingReplacement()

        assertEquals(phraseRequired, vm.errorMessage.value)
        verifyNoInteractions(api)
    }

    @Test
    fun resealWithAnEmptyFieldRefusesBeforeAnyCeremony() = runTest {
        val (vm, api) = makeVm()

        vm.resealEscrowWithHeldKit()

        assertEquals(phraseRequired, vm.errorMessage.value)
        verifyNoInteractions(api)
    }

    @Test
    fun aMintIsShownOnceWithItsUriAndTheFieldIsCleared() = runTest {
        val (vm, api) = makeVm()
        val secret = "1a".repeat(32)
        whenever(api.recoveryCreateKit("held-kit")).thenReturn(
            FfiMintedKit(secretHex = secret, escrowStored = false, landsAt = null),
        )
        whenever(api.recoveryKitDisplayUri(secret)).thenReturn("fauna://recovery?k=$secret")
        whenever(api.recoveryKitStatus()).thenReturn(registered)

        vm.onPhraseChange("held-kit")
        vm.createOrReplaceKit(usingHeldPhrase = true)

        val s = vm.state.value
        assertEquals(secret, s.mintedSecretHex)
        assertEquals("fauna://recovery?k=$secret", s.mintedKitUri)
        // A failed escrow put is carried as a flag, never as an error.
        assertFalse(s.mintedEscrowStored)
        assertNull(vm.errorMessage.value)
        assertEquals("", s.phraseInput)
        assertEquals(registered, s.status)
        assertFalse(s.busy)
        // `toString` never carries the secret.
        assertFalse(s.toString().contains(secret))
    }

    @Test
    fun aRefusedUriKeepsTheDisplayOnTheBareKit() = runTest {
        val (vm, api) = makeVm()
        val secret = "2b".repeat(32)
        whenever(api.recoveryRequestSeedAloneReplacement()).thenReturn(
            FfiMintedKit(secretHex = secret, escrowStored = false, landsAt = 1_900_000_000L),
        )
        whenever(api.recoveryKitDisplayUri(secret)).thenThrow(RuntimeException("no params"))
        whenever(api.recoveryKitStatus()).thenReturn(registered)

        vm.requestSeedAloneReplacement()

        assertEquals(secret, vm.state.value.mintedSecretHex)
        assertNull(vm.state.value.mintedKitUri)
        assertEquals(1_900_000_000L, vm.state.value.mintedLandsAt)
    }

    @Test
    fun aFailedMintShowsNothingAndSaysWhy() = runTest {
        val (vm, api) = makeVm()
        whenever(api.recoveryCreateKit(null)).thenThrow(RuntimeException("nest unreachable"))

        vm.createOrReplaceKit(usingHeldPhrase = false)

        assertNull(vm.state.value.mintedSecretHex)
        assertTrue(vm.errorMessage.value!!.contains("nest unreachable"))
        assertFalse(vm.state.value.busy)
    }

    @Test
    fun anUnreadableChainClaimsNoState() = runTest {
        val (vm, api) = makeVm()
        whenever(api.recoveryKitStatus()).thenThrow(RuntimeException("offline"))

        vm.loadStatus()

        assertNull(vm.state.value.status)
        assertTrue(vm.errorMessage.value!!.contains("offline"))
    }

    @Test
    fun leavingThePageDropsTheKitAndThePhrase() = runTest {
        val (vm, api) = makeVm()
        val secret = "3c".repeat(32)
        whenever(api.recoveryCreateKit(null)).thenReturn(
            FfiMintedKit(secretHex = secret, escrowStored = true, landsAt = null),
        )
        whenever(api.recoveryKitStatus()).thenReturn(registered)
        vm.createOrReplaceKit(usingHeldPhrase = false)
        vm.onPhraseChange("typed")

        vm.clearHeldSecrets()

        assertNull(vm.state.value.mintedSecretHex)
        assertNull(vm.state.value.mintedKitUri)
        assertEquals("", vm.state.value.phraseInput)
    }

    @Test
    fun anIdentityChangeDropsThePreviousAccountsStatusAndKit() = runTest {
        val (vm, api) = makeVm()
        val secret = "4d".repeat(32)
        whenever(api.recoveryKitStatus()).thenReturn(registered)
        whenever(api.recoveryCreateKit(null)).thenReturn(
            FfiMintedKit(secretHex = secret, escrowStored = true, landsAt = null),
        )
        vm.hydrate("aa".repeat(32))
        vm.createOrReplaceKit(usingHeldPhrase = false)
        assertEquals(secret, vm.state.value.mintedSecretHex)

        // The next read fails, so whatever status remains came from before.
        whenever(api.recoveryKitStatus()).thenThrow(RuntimeException("offline"))
        vm.hydrate("bb".repeat(32))

        assertNull(vm.state.value.mintedSecretHex)
        assertNull(vm.state.value.status)
    }
}
