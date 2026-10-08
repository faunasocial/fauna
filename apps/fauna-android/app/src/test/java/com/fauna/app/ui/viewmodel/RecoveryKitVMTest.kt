package com.fauna.app.ui.viewmodel

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.ApiClient
import com.fauna.app.core.StolenCeremonyHold
import com.fauna.app.core.SuccessionHandoff
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiLandedSuccession
import com.fauna.ffi.FfiMintedKit
import com.fauna.ffi.FfiOwedSweepAnswer
import com.fauna.ffi.FfiRecoveryKitStatus
import com.fauna.ffi.FfiStolenOutcome
import com.fauna.ffi.FfiSweepRetryAnswer
import com.fauna.ffi.FfiSweepView
import uniffi.fauna_core.LocalizedText
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

    private fun makeVm(api: ApiClient = mock(ApiClient::class.java)) =
        RecoveryKitVM(api, context, StolenCeremonyHold()) to api

    @Before
    fun clearHandoff() = SuccessionHandoff.clearOnFactoryReset()

    @After
    fun clearHandoffAfter() = SuccessionHandoff.clearOnFactoryReset()

    // ── The stolen-identity leg (`settings.md` § Recovery kit; apple's
    // `RecoveryKitVM` contract, ported) ─────────────────────────────────────

    private val confirmPrompt = context.getString(com.fauna.app.R.string.settings_recovery_kit_stolen_confirm_placeholder)
    private val predecessor = "aa".repeat(32)
    private val successor = "bb".repeat(32)
    private val successorSeed = "5e".repeat(32)

    private val sweep = FfiSweepView(
        kind = "ran", detail = null, groups = 2u, groupsOldLeafRemoved = 2u,
        unattestedMembers = 0u, owesWork = false,
    )

    private fun landed(persisted: Boolean) = FfiStolenOutcome(
        kind = "landed",
        message = null,
        landed = FfiLandedSuccession(
            successorSecretHex = successorSeed,
            newActorIdHex = successor,
            persisted = persisted,
            sweep = sweep,
            reviewRoster = emptyList(),
            sweepStateJson = """{"kind":"ran"}""",
            succeededAt = 1_900_000_000L,
        ),
        carriesTheOnlySeed = false,
    )

    private fun unlanded(kind: String, key: String, onlySeed: Boolean) = FfiStolenOutcome(
        kind = kind,
        message = LocalizedText(key = key, args = mapOf("message" to "nest said no", "secret" to successorSeed)),
        landed = null,
        carriesTheOnlySeed = onlySeed,
    )

    /** Arm the gate and type a kit — everything the handler re-checks. */
    private fun RecoveryKitVM.armStolen() {
        onStolenConfirmChange(RecoveryKitVM.STOLEN_CONFIRM_WORD)
        onPhraseChange("held-kit")
    }

    @Test
    fun theStolenTriggerRefusesWithoutTheLiteralConfirmWord() = runTest {
        val (vm, api) = makeVm()
        vm.onPhraseChange("held-kit")
        // Case matters: the gate compares against the literal, never localized.
        vm.onStolenConfirmChange("succeed")

        vm.succeedWithHeldKit(predecessor) { error("must not switch") }

        assertEquals(confirmPrompt, vm.errorMessage.value)
        verifyNoInteractions(api)
        assertFalse(vm.state.value.stolenArmed)
        assertFalse(vm.ceremonyHold.ceremonyInFlight)
    }

    @Test
    fun theStolenTriggerRefusesAnEmptyKitEvenWhenArmed() = runTest {
        val (vm, api) = makeVm()
        vm.onStolenConfirmChange(RecoveryKitVM.STOLEN_CONFIRM_WORD)

        vm.succeedWithHeldKit(predecessor) { error("must not switch") }

        assertEquals(phraseRequired, vm.errorMessage.value)
        verifyNoInteractions(api)
    }

    @Test
    fun theStolenTriggerRendersWhileTheStatusIsUnreadAndHidesOnlyOnAPositiveNo() {
        assertTrue(RecoveryKitUiState(status = null).stolenVisible)
        assertTrue(RecoveryKitUiState(status = registered).stolenVisible)
        assertFalse(RecoveryKitUiState(status = registered.copy(allowsStolen = false)).stolenVisible)
    }

    @Test
    fun aPersistedLandingRecordsTheHandoffThenSwitchesToTheSuccessor() = runTest {
        val (vm, api) = makeVm()
        whenever(api.successionSucceedWithHeldKit("held-kit")).thenReturn(landed(persisted = true))
        vm.armStolen()
        var switchedTo: String? = null

        vm.succeedWithHeldKit(predecessor) { switchedTo = it }

        assertEquals(successor, switchedTo)
        // The obligations the switch must not lose were recorded on the way.
        assertTrue(SuccessionHandoff.owesKitTo(successor))
        assertEquals(predecessor, SuccessionHandoff.predecessorActorIdHex)
        assertEquals(sweep, SuccessionHandoff.sweep)
        assertEquals("""{"kind":"ran"}""", SuccessionHandoff.sweepStateJson)
        assertNull(vm.errorMessage.value)
        assertFalse(vm.state.value.busy)
        // Adopted: the hold is released and any owed escalation is spent.
        assertFalse(vm.ceremonyHold.ceremonyInFlight)
        assertFalse(vm.ceremonyHold.messagePending)
    }

    @Test
    fun anUnpersistedLandingParksTheKeyAndItSurvivesEveryOtherWrite() = runTest {
        val (vm, api) = makeVm()
        whenever(api.successionSucceedWithHeldKit("held-kit")).thenReturn(landed(persisted = false))
        whenever(api.recoveryKitStatus()).thenThrow(RuntimeException("offline"))
        vm.armStolen()
        var switched = false

        vm.succeedWithHeldKit(predecessor) { switched = true }

        // Never torn down: that would take the only copy of the seed with it.
        assertFalse(switched)
        val parked = vm.errorMessage.value!!
        assertTrue(parked.contains(successorSeed))
        assertEquals(successorSeed, vm.state.value.mintedSecretHex)
        assertNull(vm.state.value.mintedKitUri)
        // The account DID move: the kit is still owed.
        assertTrue(SuccessionHandoff.owesKitTo(successor))
        assertTrue(vm.ceremonyHold.messagePending)
        assertFalse(vm.ceremonyHold.admits("another writer"))

        // Every other writer on the page is dropped while it is parked.
        vm.createOrReplaceKit(usingHeldPhrase = true)
        vm.loadStatus()
        assertEquals(parked, vm.errorMessage.value)

        // Leaving the page is the one acknowledgment; writes land again.
        vm.clearHeldSecrets()
        vm.loadStatus()
        assertTrue(vm.errorMessage.value!!.contains("offline"))
    }

    @Test
    fun anUndecidedOutcomeThatCarriesTheOnlySeedIsParkedVerbatim() = runTest {
        val (vm, api) = makeVm()
        whenever(api.successionSucceedWithHeldKit("held-kit")).thenReturn(
            unlanded("undecided", "settings.recovery_kit.stolen_outcome_unknown_unsaved", onlySeed = true),
        )
        whenever(api.recoveryKitStatus()).thenThrow(RuntimeException("offline"))
        vm.armStolen()

        vm.succeedWithHeldKit(predecessor) { error("must not switch") }

        val parked = vm.errorMessage.value!!
        assertEquals(
            context.getString(com.fauna.app.R.string.settings_recovery_kit_stolen_outcome_unknown_unsaved)
                .replace("{secret}", successorSeed).replace("{message}", "nest said no"),
            parked,
        )
        assertTrue(vm.ceremonyHold.messagePending)
        vm.loadStatus()
        assertEquals(parked, vm.errorMessage.value)
    }

    @Test
    fun nothingMovedPaintsTheSharedSentencePlainlyAndUnparked() = runTest {
        val (vm, api) = makeVm()
        whenever(api.successionSucceedWithHeldKit("held-kit")).thenReturn(
            unlanded("not-landed", "settings.recovery_kit.stolen_ceremony_failed", onlySeed = false),
        )
        whenever(api.recoveryKitStatus()).thenThrow(RuntimeException("offline"))
        vm.armStolen()

        vm.succeedWithHeldKit(predecessor) { error("must not switch") }

        assertEquals(
            context.getString(com.fauna.app.R.string.settings_recovery_kit_stolen_ceremony_failed)
                .replace("{message}", "nest said no"),
            vm.errorMessage.value,
        )
        assertFalse(SuccessionHandoff.kitOwed)
        assertFalse(vm.ceremonyHold.messagePending)
        // Not parked: the next writer replaces it.
        vm.loadStatus()
        assertTrue(vm.errorMessage.value!!.contains("offline"))
    }

    // ── The successor's owed kit (`identity-succession.md` § The RecoveryKey →
    // *At succession*): bounded retry, re-arm, no re-arm over a registered head.

    @Test
    fun theOwedKitIsMintedAndShownOnTheSuccessorsSeat() = runTest {
        val (vm, api) = makeVm()
        SuccessionHandoff.rearmUnshownKit(successor)
        val kit = "6f".repeat(32)
        whenever(api.boundActorIdHex).thenReturn(successor)
        whenever(api.recoveryCreateKit(null)).thenReturn(FfiMintedKit(secretHex = kit, escrowStored = true, landsAt = null))
        whenever(api.recoveryKitStatus()).thenReturn(registered)

        vm.dischargeOwedSuccessionKit(successor)

        assertEquals(kit, vm.state.value.mintedSecretHex)
        assertFalse(SuccessionHandoff.kitOwed)
    }

    @Test
    fun aSeatStillSigningAsThePredecessorDefersWithTheObligationKept() = runTest {
        val (vm, api) = makeVm()
        SuccessionHandoff.rearmUnshownKit(successor)
        whenever(api.boundActorIdHex).thenReturn(predecessor)

        vm.dischargeOwedSuccessionKit(successor)

        assertTrue(SuccessionHandoff.owesKitTo(successor))
        org.mockito.Mockito.verify(api, org.mockito.Mockito.never()).recoveryCreateKit(null)
    }

    @Test
    fun everyFailedMintReArmsTheObligationRatherThanSpendingIt() = runTest {
        val (vm, api) = makeVm()
        vm.successionMintBackoffMs = 0
        SuccessionHandoff.rearmUnshownKit(successor)
        whenever(api.boundActorIdHex).thenReturn(successor)
        whenever(api.recoveryCreateKit(null)).thenThrow(RuntimeException("connection lost"))
        // No head registered: a later mint could still land.
        whenever(api.recoveryKitStatus()).thenReturn(registered.copy(kind = "never-created", allowsCreate = true))

        vm.dischargeOwedSuccessionKit(successor)

        org.mockito.Mockito.verify(api, org.mockito.Mockito.times(RecoveryKitVM.SUCCESSION_MINT_ATTEMPTS))
            .recoveryCreateKit(null)
        assertTrue(SuccessionHandoff.owesKitTo(successor))
        assertNull(vm.state.value.mintedSecretHex)
    }

    @Test
    fun aRegisteredHeadThisDeviceDoesNotHoldIsNeverReArmed() = runTest {
        val (vm, api) = makeVm()
        vm.successionMintBackoffMs = 0
        SuccessionHandoff.rearmUnshownKit(successor)
        whenever(api.boundActorIdHex).thenReturn(successor)
        whenever(api.recoveryCreateKit(null)).thenThrow(RuntimeException("PriorKitRequired"))
        whenever(api.recoveryKitStatus()).thenReturn(registered)

        vm.dischargeOwedSuccessionKit(successor)

        org.mockito.Mockito.verify(api, org.mockito.Mockito.times(1)).recoveryCreateKit(null)
        assertFalse(SuccessionHandoff.kitOwed)
    }

    @Test
    fun aStrandedKitIsShownInsteadOfMintingAgain() = runTest {
        val (vm, api) = makeVm()
        val stranded = SuccessionHandoff.StrandedKit("7a".repeat(32), "fauna://recovery?k=x", true, null)
        SuccessionHandoff.rearmUnshownKit(successor, stranded)
        whenever(api.boundActorIdHex).thenReturn(successor)

        vm.dischargeOwedSuccessionKit(successor)

        assertEquals(stranded.secretHex, vm.state.value.mintedSecretHex)
        assertEquals(stranded.kitUri, vm.state.value.mintedKitUri)
        org.mockito.Mockito.verify(api, org.mockito.Mockito.never()).recoveryCreateKit(null)
    }

    // ── The sweep (`settings.md` § Recovery kit → *Finishing an unfinished
    // group sweep*; `succession-propagation.md`, the relaunch-adoption clause).

    @Test
    fun aSweptRetryReplacesTheCarriedViewAndSaysNothing() = runTest {
        val (vm, api) = makeVm()
        val fresh = sweep.copy(groupsOldLeafRemoved = 2u)
        whenever(api.successionRetryGroupSweep()).thenReturn(
            FfiSweepRetryAnswer(
                kind = "swept", message = null, sweep = fresh,
                sweepStateJson = """{"kind":"ran","fresh":true}""", reviewRoster = emptyList(),
            ),
        )

        vm.retrySweep()

        assertEquals(fresh, vm.state.value.sweepView)
        assertEquals(fresh, SuccessionHandoff.sweep)
        assertEquals("""{"kind":"ran","fresh":true}""", SuccessionHandoff.sweepStateJson)
        assertNull(vm.errorMessage.value)
    }

    @Test
    fun aRetryThatCannotSweepAnswersInWordsAndKeepsTheView() = runTest {
        val (vm, api) = makeVm()
        whenever(api.successionRetryGroupSweep()).thenReturn(
            FfiSweepRetryAnswer(
                kind = "no-old-state",
                message = LocalizedText(key = "settings.recovery_kit.sweep_retry_no_old_state", args = emptyMap()),
                sweep = null, sweepStateJson = null, reviewRoster = emptyList(),
            ),
        )

        vm.retrySweep()

        assertEquals(
            context.getString(com.fauna.app.R.string.settings_recovery_kit_sweep_retry_no_old_state),
            vm.errorMessage.value,
        )
        assertNull(vm.state.value.sweepView)
    }

    @Test
    fun anOwedSweepIsDischargedOnceAndParksTheSharedReport() = runTest {
        val (vm, api) = makeVm()
        SuccessionHandoff.recordRelaunchAdoption(predecessor, successor)
        val parked = sweep.copy(kind = "no-engine", groups = 0u, groupsOldLeafRemoved = 0u, owesWork = true)
        whenever(api.boundActorIdHex).thenReturn(successor)
        whenever(api.successionDischargeOwedSweep()).thenReturn(
            FfiOwedSweepAnswer(
                answer = FfiSweepRetryAnswer(
                    kind = "no-old-state", message = null, sweep = null,
                    sweepStateJson = null, reviewRoster = emptyList(),
                ),
                parked = parked,
                parkedStateJson = """{"kind":"no_engine"}""",
            ),
        )

        vm.dischargeOwedSweep(successor)
        vm.dischargeOwedSweep(successor)

        org.mockito.Mockito.verify(api, org.mockito.Mockito.times(1)).successionDischargeOwedSweep()
        assertEquals(parked, vm.state.value.sweepView)
        assertEquals("""{"kind":"no_engine"}""", SuccessionHandoff.sweepStateJson)
        assertNull(SuccessionHandoff.sweepOwedTo)
        // The kit is a separate obligation, still owed.
        assertTrue(SuccessionHandoff.owesKitTo(successor))
    }

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
