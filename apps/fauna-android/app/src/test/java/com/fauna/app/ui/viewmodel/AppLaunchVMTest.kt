package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.LaunchObserverImpl
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.SessionAccount
import com.fauna.ffi.onboarding.OnboardingMachine
import com.fauna.ffi.onboarding.OnboardingStep
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
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
import org.mockito.ArgumentMatchers.anyString
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import uniffi.fauna_launch_machine.AccountIndexRefusal
import uniffi.fauna_launch_machine.AwaitingDnsRecord
import uniffi.fauna_launch_machine.LaunchIdentity
import uniffi.fauna_launch_machine.LaunchMachine
import uniffi.fauna_launch_machine.LaunchPersistence
import uniffi.fauna_launch_machine.LaunchPhase
import uniffi.fauna_launch_machine.LaunchSnapshot
import uniffi.fauna_launch_machine.LaunchWizardEntry
import uniffi.fauna_launch_machine.PendingFactoryResetRecord
import uniffi.fauna_launch_machine.PendingInviteRecord
import uniffi.fauna_launch_machine.RefreshReason
import uniffi.fauna_launch_machine.TokenStatus

@OptIn(ExperimentalCoroutinesApi::class)
class AppLaunchVMTest {

    // `AppLaunchVM` collects the launch snapshot's identity on `viewModelScope`
    // (Dispatchers.Main.immediate), which a plain JVM test has no loop for.
    // Unconfined so the collector runs eagerly on construction and on every
    // emission, making the identity assertions below synchronous.
    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    @Test
    fun online_yieldsAuthenticated_noMachineSeeding() {
        val (vm, host, _) = makeVm()
        val target = vm.navTargetFor(LaunchPhase.Online)
        assertEquals(AppLaunchVM.NavTarget.Authenticated, target)
        verify(host.machine, never()).seedIdentity(anyString())
        verify(host.machine, never()).seedPendingInvite(anyString(), anyString(), anyString(), anyString())
        verify(host.machine, never()).navigateToInviteRequestForKnownNest(anyString(), anyString())
    }

    @Test
    fun wizardAtIdentityChoice_yieldsWizard_noSeeding() {
        val (vm, host, store) = makeVm()
        whenever(store.secretHex).thenReturn(null)
        val target = vm.navTargetFor(LaunchPhase.WizardAt(LaunchWizardEntry.IDENTITY_CHOICE))
        assertEquals(AppLaunchVM.NavTarget.Wizard("onboarding/identity-choice"), target)
        verify(host.machine, never()).seedIdentity(anyString())
    }

    @Test
    fun wizardAtHandleEntry_seedsIdentity() {
        val (vm, host, store) = makeVm()
        whenever(store.secretHex).thenReturn("aabbcc")
        val target = vm.navTargetFor(LaunchPhase.WizardAt(LaunchWizardEntry.HANDLE_ENTRY))
        assertEquals(AppLaunchVM.NavTarget.Wizard("onboarding/handle-entry"), target)
        verify(host.machine).seedIdentity("aabbcc")
    }

    @Test
    fun wizardAtInviteRequest_withPending_seedsPendingInvite() {
        val (vm, host, store, persistence) = makeVm()
        whenever(store.secretHex).thenReturn("aabbcc")
        whenever(persistence.loadPendingInvite()).thenReturn(
            PendingInviteRecord(
                nestUrl = "https://nest.example",
                handle = "alice@example",
                requestId = "req-1",
                statusJson = "{}",
            )
        )
        val target = vm.navTargetFor(LaunchPhase.WizardAt(LaunchWizardEntry.INVITE_REQUEST))
        assertEquals(AppLaunchVM.NavTarget.Wizard("onboarding/invite-request"), target)
        verify(host.machine).seedIdentity("aabbcc")
        verify(host.machine).seedPendingInvite("https://nest.example", "alice@example", "req-1", "{}")
        verify(host.machine, never()).navigateToInviteRequestForKnownNest(anyString(), anyString())
    }

    @Test
    fun wizardAtInviteRequest_withoutPending_navigatesForKnownNest() {
        val (vm, host, store, persistence) = makeVm()
        whenever(store.secretHex).thenReturn("aabbcc")
        whenever(store.nestUrl).thenReturn("https://nest.example")
        whenever(store.handle).thenReturn("alice@example")
        whenever(persistence.loadPendingInvite()).thenReturn(null)
        val target = vm.navTargetFor(LaunchPhase.WizardAt(LaunchWizardEntry.INVITE_REQUEST))
        assertEquals(AppLaunchVM.NavTarget.Wizard("onboarding/invite-request"), target)
        verify(host.machine).seedIdentity("aabbcc")
        verify(host.machine).navigateToInviteRequestForKnownNest("https://nest.example", "alice@example")
        verify(host.machine, never()).seedPendingInvite(anyString(), anyString(), anyString(), anyString())
    }

    @Test
    fun wizardAtInviteRequest_emptyCachedHandle_passesEmptyString() {
        val (vm, host, store, persistence) = makeVm()
        whenever(store.secretHex).thenReturn("aabbcc")
        whenever(store.nestUrl).thenReturn("https://nest.example")
        whenever(store.handle).thenReturn(null)
        whenever(persistence.loadPendingInvite()).thenReturn(null)
        vm.navTargetFor(LaunchPhase.WizardAt(LaunchWizardEntry.INVITE_REQUEST))
        verify(host.machine).navigateToInviteRequestForKnownNest("https://nest.example", "")
    }

    // ── AWAITING_MANUAL_DNS / PENDING_FACTORY_RESET — read through the
    // registry-backed `LaunchPersistence` the machine branched on. ─────────────

    @Test
    fun wizardAtAwaitingManualDns_withRecord_seedsFromRegistryBackedPersistence() {
        val (vm, host, store, persistence) = makeVm()
        whenever(store.secretHex).thenReturn("aabbcc")
        // The `test_smoke_i` regression shape: the deferred-DNS exit has no
        // handle yet, and the per-actor slot must still round-trip it (an
        // empty field is data).
        whenever(persistence.loadAwaitingDns()).thenReturn(
            AwaitingDnsRecord(
                nestUrl = "https://nest.example",
                handle = "",
                dnsRecordsJson = """[{"record_type":"A","name":"@","value":"203.0.113.7"}]""",
                claimCode = "DNS-CODE",
            )
        )
        val target = vm.navTargetFor(LaunchPhase.WizardAt(LaunchWizardEntry.AWAITING_MANUAL_DNS))
        assertEquals(AppLaunchVM.NavTarget.Wizard("onboarding/almost-ready"), target)
        verify(host.machine).seedIdentity("aabbcc")
        verify(host.machine).seedAwaitingManualDnsJson(
            nestUrl = "https://nest.example",
            handle = "",
            dnsRecordsJson = """[{"record_type":"A","name":"@","value":"203.0.113.7"}]""",
            claimCode = "DNS-CODE",
        )
    }

    @Test
    fun wizardAtAwaitingManualDns_noRecord_fallsBackToHandleEntry() {
        val (vm, host, store, persistence) = makeVm()
        whenever(store.secretHex).thenReturn("aabbcc")
        whenever(persistence.loadAwaitingDns()).thenReturn(null)
        val target = vm.navTargetFor(LaunchPhase.WizardAt(LaunchWizardEntry.AWAITING_MANUAL_DNS))
        assertEquals(AppLaunchVM.NavTarget.Wizard("onboarding/handle-entry"), target)
        verify(host.machine, never()).seedAwaitingManualDnsJson(
            anyString(), anyString(), anyString(), anyString(),
        )
    }

    @Test
    fun wizardAtPendingFactoryReset_withRecord_seedsTheClaimFromRegistryBackedPersistence() {
        val (vm, host, store, persistence) = makeVm()
        whenever(store.secretHex).thenReturn("aabbcc")
        whenever(persistence.loadPendingFactoryReset()).thenReturn(
            PendingFactoryResetRecord(
                nestUrl = "https://nest.example",
                handle = "alice@example.com",
                claimCode = "RESET-CODE",
                mintedAtSecs = 0uL,
            )
        )
        val target = vm.navTargetFor(LaunchPhase.WizardAt(LaunchWizardEntry.PENDING_FACTORY_RESET))
        assertEquals(AppLaunchVM.NavTarget.Wizard("onboarding/claim-code"), target)
        verify(host.machine).seedIdentity("aabbcc")
        verify(host.machine).navigateToClaimCodeForKnownNestWithCode(
            nestUrl = "https://nest.example",
            handle = "alice@example.com",
            code = "RESET-CODE",
        )
    }

    @Test
    fun wizardAtPendingFactoryReset_noRecord_fallsBackToHandleEntry() {
        val (vm, host, store, persistence) = makeVm()
        whenever(store.secretHex).thenReturn("aabbcc")
        whenever(persistence.loadPendingFactoryReset()).thenReturn(null)
        val target = vm.navTargetFor(LaunchPhase.WizardAt(LaunchWizardEntry.PENDING_FACTORY_RESET))
        assertEquals(AppLaunchVM.NavTarget.Wizard("onboarding/handle-entry"), target)
        verify(host.machine, never()).navigateToClaimCodeForKnownNestWithCode(
            anyString(), anyString(), anyString(),
        )
    }

    @Test
    fun offlineTransient_yieldsTransientError() {
        val (vm, _, _) = makeVm()
        val target = vm.navTargetFor(LaunchPhase.Offline(transient = true))
        assertEquals(AppLaunchVM.NavTarget.TransientError, target)
    }

    @Test
    fun offlineTerminal_yieldsNeedsUpdate() {
        val (vm, _, _) = makeVm()
        // The dedicated terminal-error surface landed
        // (2026-06-15): today the only Offline{transient=false} case the
        // launch flow produces is an outdated nest (fauna.nest.outdated),
        // which is non-retryable → NeedsUpdate, not the retryable
        // TransientError this test pinned as a placeholder.
        val target = vm.navTargetFor(LaunchPhase.Offline(transient = false))
        assertEquals(AppLaunchVM.NavTarget.NeedsUpdate, target)
    }

    @Test
    fun signInRefused_onOfflineTerminal_yieldsSignInRefused() {
        // onboarding.md § App-launch routing, previously-signed-in row: the
        // machine projects the refusal onto `Offline { transient: false }` and
        // sets `sign_in_refused`; reading it upgrades the NeedsUpdate fallback
        // into the dedicated page (with Retry).
        val (vm, _, _) = makeVm()
        val target = vm.navTargetFor(LaunchPhase.Offline(transient = false), signInRefused = true)
        assertEquals(AppLaunchVM.NavTarget.SignInRefused, target)
    }

    @Test
    fun signInRefused_flagOnOtherPhases_isIgnored() {
        val (vm, _, _) = makeVm()
        assertEquals(
            AppLaunchVM.NavTarget.Authenticated,
            vm.navTargetFor(LaunchPhase.Online, signInRefused = true),
        )
    }

    @Test
    fun accountIndexRefusal_outranksSignInRefused() {
        val (vm, _, _) = makeVm()
        val refusal = AccountIndexRefusal.Malformed
        val target = vm.navTargetFor(LaunchPhase.Offline(transient = false), refusal, signInRefused = true)
        assertEquals(AppLaunchVM.NavTarget.AccountIndexUnreadable(refusal), target)
    }

    @Test
    fun accountIndexRefusal_outranksOfflineTerminal_andEveryOtherRow() {
        // version-compatibility.md § 5 item 9 / onboarding.md § App-launch
        // routing: the account-index refusal is checked BEFORE `phase` is
        // even matched — it must win even though the machine also projects
        // it onto `Offline { transient: false }`, the same phase NeedsUpdate
        // owns above.
        val (vm, _, _) = makeVm()
        val refusal = AccountIndexRefusal.Malformed
        val target = vm.navTargetFor(LaunchPhase.Offline(transient = false), refusal)
        assertEquals(AppLaunchVM.NavTarget.AccountIndexUnreadable(refusal), target)
    }

    @Test
    fun accountIndexRefusal_absent_fallsBackToPhaseRouting() {
        val (vm, _, _) = makeVm()
        val target = vm.navTargetFor(LaunchPhase.Offline(transient = false), accountIndexRefusal = null)
        assertEquals(AppLaunchVM.NavTarget.NeedsUpdate, target)
    }

    @Test
    fun inFlightPhases_returnNull() {
        val (vm, host, _) = makeVm()
        // In-flight states; the NavHost stays on the cold-start blank
        // screen. navTargetFor returns null so the caller doesn't
        // navigate prematurely.
        assertNull(vm.navTargetFor(LaunchPhase.Boot))
        assertNull(vm.navTargetFor(LaunchPhase.Hydrating))
        assertNull(vm.navTargetFor(LaunchPhase.SilentChallenge(attempt = 1u)))
        assertNull(vm.navTargetFor(LaunchPhase.Refreshing(RefreshReason.SCHEDULED_TTL)))
        verify(host.machine, never()).seedIdentity(anyString())
        verify(host.machine, never()).seedPendingInvite(anyString(), anyString(), anyString(), anyString())
        verify(host.machine, never()).navigateToInviteRequestForKnownNest(anyString(), anyString())
    }

    // ── A succeeded identity (identity-succession.md § Propagation → *Own
    // device fleet*): the refusal rides the snapshot's `supersededSuccessor`
    // side channel on `Offline { transient: false }` and routes to import —
    // ranked below the account-index row and above the sign-in-refused row,
    // exactly as tui's `route()` and apple's `dispatchLaunch` rank them. ────

    @Test
    fun aSupersededRefusalRoutesToImportNotToNeedsUpdate() {
        val (vm, _, _) = makeVm()
        val target = vm.navTargetFor(LaunchPhase.Offline(transient = false), supersededSuccessor = CLAIMED)
        assertEquals(AppLaunchVM.NavTarget.Superseded(CLAIMED), target)
    }

    // ── Rotation-chain fork evidence (box-recovery.md § Client acceptance): the
    // snapshot's `identityFork` bit turns the identity-changed phase into the
    // fork surface naming both heads; with the bit clear it stays the ordinary
    // trust-offering surface. ────

    @Test
    fun forkEvidenceRoutesToTheForkSurfaceNamingBothHeads() {
        val (vm, _, _) = makeVm()
        val target = vm.navTargetFor(LaunchPhase.IdentityChanged("aa", "bb"), identityFork = true)
        assertEquals(AppLaunchVM.NavTarget.IdentityForked("aa", "bb"), target)
    }

    @Test
    fun anIdentityChangeWithoutForkEvidenceKeepsTheOrdinaryTrustSurface() {
        val (vm, _, _) = makeVm()
        val target = vm.navTargetFor(LaunchPhase.IdentityChanged("aa", "bb"))
        assertEquals(AppLaunchVM.NavTarget.IdentityChanged, target)
    }

    @Test
    fun aSupersededRefusalOutranksSignInRefused() {
        val (vm, _, _) = makeVm()
        val target = vm.navTargetFor(
            LaunchPhase.Offline(transient = false),
            signInRefused = true,
            supersededSuccessor = CLAIMED,
        )
        assertEquals(AppLaunchVM.NavTarget.Superseded(CLAIMED), target)
    }

    @Test
    fun theAccountIndexRowOutranksASupersededRefusal() {
        val (vm, _, _) = makeVm()
        val refusal = AccountIndexRefusal.Malformed
        val target = vm.navTargetFor(
            LaunchPhase.Offline(transient = false),
            refusal,
            supersededSuccessor = CLAIMED,
        )
        assertEquals(AppLaunchVM.NavTarget.AccountIndexUnreadable(refusal), target)
    }

    @Test
    fun theSupersededChannelIsIgnoredOffTheOfflinePhase() {
        val (vm, _, _) = makeVm()
        assertEquals(
            AppLaunchVM.NavTarget.Authenticated,
            vm.navTargetFor(LaunchPhase.Online, supersededSuccessor = CLAIMED),
        )
    }

    @Test
    fun theRouteSeedsTheRefusedIdentityAndSetsTheClaimFreeReasonAtomically() = runTest {
        val f = makeVm(secretHex = SECRET, nestUrl = NEST)
        whenever(f.host.machine.step()).thenReturn(OnboardingStep.IDENTITY_IMPORT)
        f.vm.routeSupersededRefusal(CLAIMED, CLAIM_FREE, ::verifiedReason) { _, _ -> null }
        verify(f.host.machine).seedIdentity(SECRET)
        // The claimed successor is never in the reason: it is a claim until the
        // chain proves it, and naming it would trust the nest as an authorizer.
        verify(f.host.machine).beginImportIdentityWithReason(CLAIM_FREE)
        verify(f.host.machine, never()).beginImportIdentityWithReason(verifiedReason(CLAIMED))
    }

    @Test
    fun aVerifiedSuccessorUpgradesTheReasonToNameIt() = runTest {
        val f = makeVm(secretHex = SECRET, nestUrl = NEST)
        whenever(f.host.machine.step()).thenReturn(OnboardingStep.IDENTITY_IMPORT)
        var walked: Pair<String, ByteArray>? = null
        f.vm.routeSupersededRefusal(CLAIMED, CLAIM_FREE, ::verifiedReason) { url, secret ->
            walked = url to secret
            VERIFIED
        }
        // The walk is anonymous, over the refused identity's own secret and nest.
        assertEquals(NEST, walked?.first)
        assertEquals(SECRET, walked?.second?.joinToString("") { "%02x".format(it) })
        verify(f.host.machine).beginImportIdentityWithReason(verifiedReason(VERIFIED))
    }

    @Test
    fun aVerifiedSuccessorLandingAfterTheUserLeftImportChangesNothing() = runTest {
        val f = makeVm(secretHex = SECRET, nestUrl = NEST)
        whenever(f.host.machine.step()).thenReturn(OnboardingStep.HANDLE_ENTRY)
        f.vm.routeSupersededRefusal(CLAIMED, CLAIM_FREE, ::verifiedReason) { _, _ -> VERIFIED }
        verify(f.host.machine, never()).beginImportIdentityWithReason(verifiedReason(VERIFIED))
    }

    @Test
    fun aFailedWalkLeavesTheClaimFreeReasonStanding() = runTest {
        val f = makeVm(secretHex = SECRET, nestUrl = NEST)
        whenever(f.host.machine.step()).thenReturn(OnboardingStep.IDENTITY_IMPORT)
        f.vm.routeSupersededRefusal(CLAIMED, CLAIM_FREE, ::verifiedReason) { _, _ ->
            throw IllegalStateException("walk failed")
        }
        verify(f.host.machine).beginImportIdentityWithReason(CLAIM_FREE)
        verify(f.host.machine, never()).beginImportIdentityWithReason(verifiedReason(VERIFIED))
    }

    @Test
    fun withoutSessionMaterialTheClaimFreeReasonIsFinal() = runTest {
        val f = makeVm()
        var walkedAtAll = false
        f.vm.routeSupersededRefusal(CLAIMED, CLAIM_FREE, ::verifiedReason) { _, _ ->
            walkedAtAll = true
            VERIFIED
        }
        verify(f.host.machine, never()).seedIdentity(anyString())
        verify(f.host.machine).beginImportIdentityWithReason(CLAIM_FREE)
        assertEquals(false, walkedAtAll)
    }

    // ── The live identity channel (conversations.md § State & data shape →
    // *Self-address: live, never baked*): the nest-confirmed identity is pushed
    // into the live session. The registry's server-data cache is written by the
    // silent challenge itself (`save_authenticated`), not here. ────────────────

    @Test
    fun aResolvedIdentityHealsTheLiveSession() {
        val f = makeVm()
        f.snapshots.value = f.snapshots.value.copy(
            identity = LaunchIdentity(handle = "alice", domain = "nest.example", tier = "free")
        )
        verify(f.apiClient).setConversationsSelfAddress("alice@nest.example")
    }

    @Test
    fun anAtQualifiedHandleIsNormalizedToBareBeforePairing() {
        // The handle is sometimes @-qualified (the MailSettingsVM precedent),
        // and the nest may report it that way; pairing it verbatim
        // would compose "alice@example@nest.example".
        val f = makeVm()
        f.snapshots.value = f.snapshots.value.copy(
            identity = LaunchIdentity(handle = "alice@example", domain = "nest.example", tier = "free")
        )
        verify(f.apiClient).setConversationsSelfAddress("alice@nest.example")
    }

    @Test
    fun anUnresolvedHalfIsNeverComposedIntoAnAddress() {
        // The § treats an empty local part or an empty domain exactly like a
        // missing address — "@nest.example" is the forbidden shape a client
        // synthesizes from an unresolved handle plus a host. Push nothing; the
        // session keeps the empty address it was built with and a send refuses
        // locally.
        val f = makeVm()
        f.snapshots.value = f.snapshots.value.copy(
            identity = LaunchIdentity(handle = "", domain = "nest.example", tier = "free")
        )
        f.snapshots.value = f.snapshots.value.copy(
            identity = LaunchIdentity(handle = "alice", domain = "", tier = "free")
        )
        verify(f.apiClient, never()).setConversationsSelfAddress(anyString())
    }

    // ── The held-successor adoption (`succession-propagation.md` § Propagation →
    // *Own device fleet*, the relaunch-adoption clause): a lost succession
    // reply left this device holding the verified successor's key, so the
    // launch adopts it — recording the kit AND the sweep it owes BEFORE the
    // switch — instead of sending the user to import a key they already hold.

    @Test
    fun aHeldVerifiedSuccessorIsAdoptedWithItsObligationsRecordedFirst() = runTest {
        com.fauna.app.core.SuccessionHandoff.clearOnFactoryReset()
        val f = makeVm(secretHex = SECRET, nestUrl = NEST)
        whenever(f.host.machine.step()).thenReturn(OnboardingStep.IDENTITY_IMPORT)
        whenever(f.registry.active()).thenReturn(PREDECESSOR)
        whenever(f.registry.adoptHeldSuccessor(PREDECESSOR, VERIFIED)).thenReturn(true)
        var owedAtAdopt: Pair<Boolean, String?>? = null
        var adopted: String? = null

        f.vm.routeSupersededRefusal(
            CLAIMED, CLAIM_FREE, ::verifiedReason,
            resolve = { _, _ -> VERIFIED },
            adopt = { successor ->
                owedAtAdopt = com.fauna.app.core.SuccessionHandoff.owesKitTo(successor) to
                    com.fauna.app.core.SuccessionHandoff.sweepOwedTo
                adopted = successor
            },
        )

        assertEquals(VERIFIED, adopted)
        // Both obligations were already recorded when the switch ran.
        assertEquals(true to VERIFIED, owedAtAdopt)
        assertEquals(PREDECESSOR, com.fauna.app.core.SuccessionHandoff.predecessorActorIdHex)
        // Adopted, so there is nothing to import — no verified reason is painted.
        verify(f.host.machine, never()).beginImportIdentityWithReason(verifiedReason(VERIFIED))
        com.fauna.app.core.SuccessionHandoff.clearOnFactoryReset()
    }

    @Test
    fun aSuccessorThisDeviceDoesNotHoldStaysOnTheImportRoute() = runTest {
        com.fauna.app.core.SuccessionHandoff.clearOnFactoryReset()
        val f = makeVm(secretHex = SECRET, nestUrl = NEST)
        whenever(f.host.machine.step()).thenReturn(OnboardingStep.IDENTITY_IMPORT)
        whenever(f.registry.active()).thenReturn(PREDECESSOR)
        whenever(f.registry.adoptHeldSuccessor(PREDECESSOR, VERIFIED)).thenReturn(false)
        var adopted = false

        f.vm.routeSupersededRefusal(
            CLAIMED, CLAIM_FREE, ::verifiedReason,
            resolve = { _, _ -> VERIFIED },
            adopt = { adopted = true },
        )

        assertEquals(false, adopted)
        assertNull(com.fauna.app.core.SuccessionHandoff.sweepOwedTo)
        assertEquals(false, com.fauna.app.core.SuccessionHandoff.kitOwed)
        verify(f.host.machine).beginImportIdentityWithReason(verifiedReason(VERIFIED))
    }

    // ── The held-back own supersession (`settings.md` § Recovery kit → *The
    // persist-failure message survives the page*). ─────────────────────────

    @Test
    fun aSupersessionDuringTheOwnCeremonyIsHeldUntilItEnds() {
        val hold = com.fauna.app.core.StolenCeremonyHold()
        val f = makeVm(hold = hold)
        var reentered = 0
        hold.ceremonyStarted()

        f.vm.routeSessionEnding(com.fauna.ffi.FfiSessionEndingVerdict.SUPERSEDED) { reentered++ }

        assertEquals(0, reentered)
        verify(f.actorScope, never()).dropActorScopedState()
        // Ended off Account with nothing parked → the owed escalation runs now.
        hold.ceremonyEnded(adopted = false, messageParked = false)
        assertEquals(1, reentered)
        verify(f.actorScope).dropActorScopedState()
    }

    @Test
    fun aVerdictThatIsNotTheCeremonysOwnEscalatesAtOnce() {
        val hold = com.fauna.app.core.StolenCeremonyHold()
        val f = makeVm(hold = hold)
        var reentered = 0
        hold.ceremonyStarted()

        f.vm.routeSessionEnding(com.fauna.ffi.FfiSessionEndingVerdict.NEST_IDENTITY_CHANGED) { reentered++ }

        assertEquals(1, reentered)
        verify(f.actorScope).dropActorScopedState()
    }

    /** [persistence] is the registry-backed `LaunchPersistence` `AppLaunchVM` reads
     *  the three wizard-resume records through; [store] is the read-only
     *  [SessionAccount] (the active account's session material). */
    private data class Fixture(
        val vm: AppLaunchVM,
        val host: OnboardingHost,
        val store: SessionAccount,
        val persistence: LaunchPersistence,
        // Positional: phase, token, lastError, supersededSuccessor, identityFork, identity, accountIndexRefusal.
        val snapshots: MutableStateFlow<LaunchSnapshot> = MutableStateFlow(
            LaunchSnapshot(LaunchPhase.Boot, TokenStatus.None, null, null, false, null, null)
        ),
        val apiClient: ApiClient = mock(ApiClient::class.java),
        val registry: com.fauna.ffi.FfiAccountRegistry = mock(com.fauna.ffi.FfiAccountRegistry::class.java),
        val actorScope: com.fauna.app.core.ActorScope = mock(com.fauna.app.core.ActorScope::class.java),
        val machine: LaunchMachine = mock(LaunchMachine::class.java),
        val currency: com.fauna.app.core.LaunchCurrency = com.fauna.app.core.LaunchCurrency(),
        val accountStores: com.fauna.app.core.AccountStores = mock(com.fauna.app.core.AccountStores::class.java),
    )

    // ── The locked surface (`ui/sessions.md` § Layout & flow, leg 2;
    // `devices.md` § The locked state) ──────────────────────────────────────

    @Test
    fun locked_onOfflineTerminal_yieldsAccountLocked() {
        val (vm, _, _) = makeVm()
        val target = vm.navTargetFor(LaunchPhase.Offline(transient = false), lockedUntilSecs = UNTIL)
        assertEquals(AppLaunchVM.NavTarget.AccountLocked(UNTIL), target)
    }

    @Test
    fun locked_outranksTheOtherOfflineSideChannels() {
        // The machine's `Offline { transient: false }` projection would otherwise
        // paint NeedsUpdate (or the refused surface) over a lock with no way out.
        val (vm, _, _) = makeVm()
        val target = vm.navTargetFor(
            LaunchPhase.Offline(transient = false),
            signInRefused = true,
            supersededSuccessor = CLAIMED,
            lockedUntilSecs = UNTIL,
        )
        assertEquals(AppLaunchVM.NavTarget.AccountLocked(UNTIL), target)
    }

    @Test
    fun locked_onOtherPhases_isIgnored() {
        val (vm, _, _) = makeVm()
        assertEquals(
            AppLaunchVM.NavTarget.Authenticated,
            vm.navTargetFor(LaunchPhase.Online, lockedUntilSecs = UNTIL),
        )
    }

    @Test
    fun theNoticeFollowsTheSharedRule_andHoldsWhileTheUserIsMidCeremony() {
        val (vm, _, _) = makeVm()
        val snap = bootSnapshot()
        val locked = AppLaunchVM.NavTarget.AccountLocked(UNTIL)
        // No notice shown yet: the machine's target, untouched.
        assertNull(vm.followLock(null, snap, null, false) { _, _ -> true })
        // Shown, and the shared rule says it still holds (mid-flight refresh).
        assertEquals(locked, vm.followLock(null, snap, UNTIL, false) { _, _ -> true })
        // The rule lets go: the machine's settled target wins.
        assertEquals(
            AppLaunchVM.NavTarget.Authenticated,
            vm.followLock(AppLaunchVM.NavTarget.Authenticated, snap, UNTIL, false) { _, _ -> false },
        )
        // Mid-ceremony on `identity_stolen_entry`: never yanked away.
        assertEquals(
            locked,
            vm.followLock(AppLaunchVM.NavTarget.Authenticated, snap, UNTIL, true) { _, _ -> false },
        )
        // A new lock with another unlock time repaints the notice for it.
        val relocked = AppLaunchVM.NavTarget.AccountLocked(UNTIL + 86_400u)
        assertEquals(relocked, vm.followLock(relocked, snap, UNTIL, false) { _, _ -> true })
    }

    @Test
    fun theLockedCeremonyReChecksBothGatesInTheActionArm() = runTest {
        val f = makeVm(secretHex = SECRET, nestUrl = NEST)
        var called = false
        f.vm.succeedLocked = { _, _, _ -> called = true; error("must not run") }
        assertEquals(
            AppLaunchVM.LockedCeremonyResult.NotArmed,
            f.vm.succeedStolenLocked("held-kit", "succeed"),
        )
        assertEquals(
            AppLaunchVM.LockedCeremonyResult.NoPhrase,
            f.vm.succeedStolenLocked("", RecoveryKitVM.STOLEN_CONFIRM_WORD),
        )
        assertEquals(false, called)
    }

    @Test
    fun aPersistedLockedCeremonySwitchesToTheSuccessorAndReRunsTheMachine() = runTest {
        val f = makeVm(secretHex = SECRET, nestUrl = NEST)
        whenever(f.registry.active()).thenReturn(PREDECESSOR)
        var seen: Triple<String, ByteArray, String>? = null
        f.vm.succeedLocked = { url, secret, phrase ->
            seen = Triple(url, secret, phrase)
            lockedOutcome(persisted = true)
        }
        val result = f.vm.succeedStolenLocked("held-kit", RecoveryKitVM.STOLEN_CONFIRM_WORD)
        assertEquals(AppLaunchVM.LockedCeremonyResult.SignedIn, result)
        assertEquals(NEST, seen!!.first)
        assertEquals("held-kit", seen!!.third)
        verify(f.registry).setActive(VERIFIED)
        verify(f.actorScope).dropActorScopedState()
        verify(f.machine).start()
    }

    @Test
    fun anUnpersistedLockedCeremonyPutsTheKeyOnScreenAndSwitchesNothing() = runTest {
        val f = makeVm(secretHex = SECRET, nestUrl = NEST)
        f.vm.succeedLocked = { _, _, _ -> lockedOutcome(persisted = false) }
        val result = f.vm.succeedStolenLocked("held-kit", RecoveryKitVM.STOLEN_CONFIRM_WORD)
        assertEquals(AppLaunchVM.LockedCeremonyResult.PersistFailed(SUCCESSOR_SEED), result)
        verify(f.registry, never()).setActive(anyString())
        verify(f.machine, never()).start()
    }

    // ── A verdict renders only while its own launch is still the current one
    // (`onboarding.md` § App-launch routing; multiple-accounts outcome 17) ──

    /**
     * Red-verify by making `LaunchCurrency.deliver` return `true` and
     * `retireInFlight` do nothing: the replaced and the retired launch's
     * verdicts then both count as routed and their machines stay on screen.
     */
    @Test
    fun aReplacedOrRetiredLaunchIsDroppedAndTheCurrentOneRoutes() = runTest {
        val currency = com.fauna.app.core.LaunchCurrency()
        val stale = mock(LaunchMachine::class.java)
        val newer = mock(LaunchMachine::class.java)
        val retired = mock(LaunchMachine::class.java)
        val fresh = ArrayDeque(listOf(stale, newer, retired))
        val f = makeVm()
        val vm = AppLaunchVM(
            { fresh.removeFirst() }, currency, mock(LaunchObserverImpl::class.java).also {
                whenever(it.snapshot).thenReturn(f.snapshots)
            },
            f.host, f.store, f.apiClient, f.actorScope, f.persistence, f.registry,
            mock(com.fauna.app.core.SecureStorage::class.java),
            cleanStores(),
            com.fauna.app.core.StolenCeremonyHold(),
        )
        vm.finishDispatcher = Dispatchers.Unconfined

        // The stale launch is still awaiting its nest when a newer one begins
        // (the switcher's re-entry): its verdict lands second and is dropped.
        val staleVerdict = holdVerdict(stale)
        val staleDrive = launch { vm.start() }
        testScheduler.runCurrent()
        assertTrue("the stale launch is in flight", staleVerdict.isInitialized())
        vm.start()
        assertTrue("the newer launch holds the surface", currency.isShown(newer))
        staleVerdict.value.resumeWith(Result.success(Unit))
        staleDrive.join()
        assertEquals(1L to 1L, currency.routedCount to currency.supersededCount)
        assertTrue("a superseded launch never takes the surface back", currency.isShown(newer))

        // The current launch's verdict routed even though a session may be live
        // (an ownership test, never a presence test), and then holds nothing in
        // flight: the session patch retires nothing of a settled launch.
        assertFalse("a settled launch is not retired", currency.retireInFlight())
        assertTrue(currency.isShown(newer))

        // A launch still in flight when the session patch builds a newer session.
        val retiredVerdict = holdVerdict(retired)
        val retiredDrive = launch { vm.start() }
        testScheduler.runCurrent()
        assertTrue(currency.retireInFlight())
        assertFalse("a retired launch leaves the surface", currency.isShown(retired))
        retiredVerdict.value.resumeWith(Result.success(Unit))
        retiredDrive.join()
        assertEquals(1L to 2L, currency.routedCount to currency.supersededCount)
    }

    /** Stub [machine]'s `start()` to suspend until the test resumes the handed-back
     *  continuation — a launch provably in flight, with no clock involved. */
    private suspend fun holdVerdict(machine: LaunchMachine): Lazy<kotlin.coroutines.Continuation<Unit>> {
        var held: kotlin.coroutines.Continuation<Unit>? = null
        // Stubbed from a coroutine: Mockito matches a suspend method without its
        // trailing continuation, and the answer reads that continuation raw.
        org.mockito.Mockito.doAnswer { invocation ->
            @Suppress("UNCHECKED_CAST")
            held = invocation.rawArguments.last() as kotlin.coroutines.Continuation<Unit>
            kotlin.coroutines.intrinsics.COROUTINE_SUSPENDED
        }.`when`(machine).start()
        return object : Lazy<kotlin.coroutines.Continuation<Unit>> {
            override val value get() = checkNotNull(held) { "start() was never called" }
            override fun isInitialized() = held != null
        }
    }

    private fun lockedOutcome(persisted: Boolean) = com.fauna.ffi.FfiStolenOutcome(
        kind = "landed",
        message = null,
        landed = com.fauna.ffi.FfiLandedSuccession(
            successorSecretHex = SUCCESSOR_SEED,
            newActorIdHex = VERIFIED,
            persisted = persisted,
            sweep = com.fauna.ffi.FfiSweepView(
                kind = "not-run", detail = null, groups = 0u, groupsOldLeafRemoved = 0u,
                unattestedMembers = 0u, owesWork = true,
            ),
            reviewRoster = emptyList(),
            sweepStateJson = """{"kind":"not-run"}""",
            succeededAt = 1_900_000_000L,
        ),
        carriesTheOnlySeed = false,
    )

    private fun makeVm(
        secretHex: String? = null,
        nestUrl: String? = null,
        hold: com.fauna.app.core.StolenCeremonyHold = com.fauna.app.core.StolenCeremonyHold(),
    ): Fixture {
        val store = mock(SessionAccount::class.java)
        whenever(store.secretHex).thenReturn(secretHex)
        whenever(store.nestUrl).thenReturn(nestUrl)
        val persistence = mock(LaunchPersistence::class.java)
        val onboardingMachine = mock(OnboardingMachine::class.java)
        val host = mock(OnboardingHost::class.java)
        whenever(host.machine).thenReturn(onboardingMachine)
        val machine = mock(LaunchMachine::class.java)
        val observer = mock(LaunchObserverImpl::class.java)
        val snapshots = MutableStateFlow(bootSnapshot())
        whenever(observer.snapshot).thenReturn(snapshots)
        val apiClient = mock(ApiClient::class.java)
        val actorScope = mock(com.fauna.app.core.ActorScope::class.java)
        val registry = mock(com.fauna.ffi.FfiAccountRegistry::class.java)
        val secureStorage = mock(com.fauna.app.core.SecureStorage::class.java)
        val accountStores = cleanStores()
        val currency = com.fauna.app.core.LaunchCurrency()
        val vm = AppLaunchVM(
            { machine }, currency, observer, host, store, apiClient, actorScope, persistence,
            registry, secureStorage, accountStores, hold,
        )
        vm.finishDispatcher = Dispatchers.Unconfined
        return Fixture(
            vm, host, store, persistence, snapshots, apiClient, registry, actorScope, machine,
            currency, accountStores,
        )
    }

    // ── The cold boot's sign-out finish (`account-scoping.md` § Erasure follows
    //    scope → *The sign-out record is on all 7 apps*, decisions 3 and 4) ──

    /** The launch's finish runs once, before the machine reads the registry. */
    @Test
    fun theColdBootFinishRunsOnceAndBeforeTheMachineStarts() = runTest {
        val f = makeVm()

        f.vm.start()
        f.vm.start()

        org.mockito.Mockito.inOrder(f.accountStores, f.machine).apply {
            verify(f.accountStores).finishSignOut(anyHost(), eqCaller(com.fauna.ffi.FfiFinishCaller.LAUNCH), org.mockito.ArgumentMatchers.isNull())
            // Both launches drive the machine; only the first ran the finish.
            verify(f.machine, org.mockito.Mockito.times(2)).start()
        }
        verify(f.accountStores, org.mockito.Mockito.times(1))
            .finishSignOut(anyHost(), anyCaller(), org.mockito.ArgumentMatchers.any())
        assertFalse(f.vm.failedClosed.value)
    }

    /**
     * A refused finish whose record still owes its wipe FAILS CLOSED: the
     * machine is never started (nothing routes over the registry), and the
     * refusal waits for the wizard to paint; Remove Again re-runs the routine
     * and, once it no longer refuses, the launch routes.
     */
    @Test
    fun aFinishThatFailsClosedNeverStartsTheMachineUntilRemoveAgainClearsIt() = runTest {
        val f = makeVm()
        val refusal = mock(com.fauna.ffi.FfiSignOutResidue::class.java)
        whenever(
            f.accountStores.finishSignOut(anyHost(), anyCaller(), org.mockito.ArgumentMatchers.any())
        ).thenReturn(
            CLEAN_FINISH.copy(failsClosed = true, refused = true, residue = refusal),
            CLEAN_FINISH,
        )

        f.vm.start()

        verify(f.machine, never()).start()
        assertTrue("the launch fails closed", f.vm.failedClosed.value)
        assertTrue("the refusal waits for the wizard", f.vm.launchResidue.value === refusal)

        assertNull("a clean re-run closes the view", f.vm.retrySignOutResidue())
        assertFalse(f.vm.failedClosed.value)
        verify(f.machine).start()
    }

    private fun cleanStores(): com.fauna.app.core.AccountStores {
        val stores = mock(com.fauna.app.core.AccountStores::class.java)
        whenever(
            stores.finishSignOut(anyHost(), anyCaller(), org.mockito.ArgumentMatchers.any())
        ).thenReturn(CLEAN_FINISH)
        return stores
    }

    /** `ArgumentMatchers.any` for a non-null Kotlin parameter. */
    /** Mockito's `eq` answers `null` for an enum, which a Kotlin non-null parameter refuses. */
    private fun eqCaller(caller: com.fauna.ffi.FfiFinishCaller): com.fauna.ffi.FfiFinishCaller {
        org.mockito.ArgumentMatchers.eq(caller)
        return caller
    }

    /** Mockito's `any` answers `null`, which a Kotlin non-null parameter refuses. */
    private fun anyCaller(): com.fauna.ffi.FfiFinishCaller {
        org.mockito.ArgumentMatchers.any(com.fauna.ffi.FfiFinishCaller::class.java)
        return com.fauna.ffi.FfiFinishCaller.LAUNCH
    }

    private fun anyHost(): com.fauna.ffi.FfiSignOutHost {
        org.mockito.ArgumentMatchers.any(com.fauna.ffi.FfiSignOutHost::class.java)
        return NO_HOST
    }

    private companion object {
        val PREDECESSOR = "e3".repeat(32)
        val CLAIMED = "c1".repeat(32)
        val VERIFIED = "d2".repeat(32)
        val SUCCESSOR_SEED = "5e".repeat(32)
        const val UNTIL: ULong = 1_800_000_000u
        val SECRET = "ab".repeat(32)
        const val NEST = "http://127.0.0.1:18600"
        const val CLAIM_FREE = "This identity was succeeded — import the new identity to continue."
        fun verifiedReason(successor: String) = "Your account now belongs to $successor."

        /** A launch finish with nothing owed. */
        val CLEAN_FINISH = com.fauna.ffi.FfiSignOutFinish(
            failsClosed = false,
            refused = false,
            residue = null,
            credentialsWiped = false,
            foundRecord = false,
        )

        /** The stand-in an `any()` matcher returns for a non-null parameter. */
        val NO_HOST = object : com.fauna.ffi.FfiSignOutHost {
            override fun wipeOwnStore(): Boolean = true
            override fun eraseCredentials(): com.fauna.ffi.FfiCredentialSweep =
                com.fauna.ffi.FfiCredentialSweep(survivors = emptyList(), wipeFailed = false)
        }
    }

    private fun bootSnapshot() = LaunchSnapshot(
        phase = LaunchPhase.Boot,
        token = TokenStatus.None,
        lastError = null,
        supersededSuccessor = null,
        // Set only when a silent challenge is refused because the nest's
        // identity FORKED. Never at Boot.
        identityFork = false,
        identity = null,
        // Absent until the launch machine's first persistence read refuses
        // the account index. Never at Boot.
        accountIndexRefusal = null,
    )
}
