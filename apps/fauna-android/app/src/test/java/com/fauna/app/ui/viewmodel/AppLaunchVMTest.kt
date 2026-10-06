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
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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
    )

    private fun makeVm(secretHex: String? = null, nestUrl: String? = null): Fixture {
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
        val accountStores = mock(com.fauna.app.core.AccountStores::class.java)
        val vm = AppLaunchVM(
            machine, observer, host, store, apiClient, actorScope, persistence,
            registry, secureStorage, accountStores,
        )
        return Fixture(vm, host, store, persistence, snapshots, apiClient)
    }

    private companion object {
        val CLAIMED = "c1".repeat(32)
        val VERIFIED = "d2".repeat(32)
        val SECRET = "ab".repeat(32)
        const val NEST = "http://127.0.0.1:18600"
        const val CLAIM_FREE = "This identity was succeeded — import the new identity to continue."
        fun verifiedReason(successor: String) = "Your account now belongs to $successor."
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
