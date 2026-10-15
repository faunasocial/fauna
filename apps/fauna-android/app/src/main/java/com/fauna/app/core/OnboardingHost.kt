package com.fauna.app.core

import android.app.Activity
import android.content.Context
import android.util.Log
import com.fauna.app.BuildConfig
import com.fauna.app.age.AgeClaimMachine
import com.fauna.app.age.StoreAgeClaim
import com.fauna.app.age.StoreAgeGlue
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.ageBandFromAgeRange
import com.fauna.ffi.onboarding.AgeNoncePlain
import com.fauna.ffi.onboarding.OnboardingMachine
import com.fauna.ffi.onboarding.CapturedDnsCredential
import com.fauna.ffi.onboarding.OnboardingObserver
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.RestoredPredecessorSeed
import com.fauna.ffi.onboarding.WizardOutcome
import com.fauna.app.widget.WidgetDataWorker
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.fauna_launch_machine.LaunchPersistence
import javax.inject.Inject
import javax.inject.Singleton

private const val TAG = "OnboardingHost"

/**
 * Singleton owner of the shared OnboardingMachine. Mirrors the per-app
 * "thin proxy" pattern from docs/goal/behavior/onboarding.md §"Architectural rules"
 * (observer-driven rendering, no per-stage view-model, snapshots are
 * read-only client-side, persistence on machine return values).
 *
 * Compose screens read snapshots through the machine on each `tick`
 * recomposition.
 */
@Singleton
class OnboardingHost @Inject constructor(
    private val sessionAccount: SessionAccount,
    private val registry: FfiAccountRegistry,
    private val launchPersistence: LaunchPersistence,
    // The ONE canonical actor-scoped drop (account-scoping.md § the in-memory
    // corollary) — needed here for the append-mode pending-invite switch
    // (`persistPendingInviteSlot`), the same drop `AccountSettingsVM
    // .switchAccount` runs for every other switch path.
    private val actorScope: ActorScope,
    // The account-store container the re-provision drive's custody read opens
    // (box-recovery.md § The plane-era recovery floor, *(b) The reads*) — the
    // same accessor `startAccountRuntime` and both erases use.
    private val accountStores: AccountStores,
    // For the widget re-fold on the append-mode switch, mirroring
    // `AccountSettingsVM.switchAccount`.
    @ApplicationContext private val context: Context,
) {
    private val _tick = MutableStateFlow(0L)
    /** UI subscribes via collectAsState; every machine notification bumps this. */
    val tick: StateFlow<Long> = _tick.asStateFlow()

    private val observer = object : OnboardingObserver {
        override fun `onChanged`() {
            _tick.value = _tick.value + 1
        }
    }

    /**
     * The constructor carries no provider base-URL override: that parameter was
     * automation surface riding the exported UniFFI signature and is gone from
     * every production artifact (testing.md convention 15). E2E tests inject
     * overrides via the call_machine_method bridge per
     * docs/goal/behavior/onboarding.md §"E2E bridge contract".
     */
    val machine: OnboardingMachine =
        OnboardingMachine.newWithPersistence(observer, registry.pendingProvisionStore()).also {
        // android renders the `trust_prompt` interstitial (`onboarding.md` §
        // 3b-ter, built 2026-08-14 after tui led it), so the machine routes
        // the NAT step's exits through the one-tap trust offer. This
        // capability flag is the ONLY thing that makes the step reachable —
        // leaving it undeclared exits straight to `Done`, unchanged.
        it.setRendersTrustPrompt(true)
        // …and the identity-recovery pages (`onboarding.md` § 1 Identity):
        // `RecoveryKitScreen` / `RecoveryEntryScreen`. Declaring this routes
        // `confirmGeneratedIdentity` through the sign-up kit offer; the
        // declaration and the screens land together.
        it.setRendersRecoveryKit(true)
        // box-recovery.md § The plane-era recovery floor, *(b) The reads*: the
        // re-provision drive's local custody read opens THIS device's account
        // store, which on android lives in the app container — the platform
        // default (`$HOME/.config/fauna/sync`) is the wrong place in the sandbox.
        it.setStoreContainerDir(accountStores.accountStoreContainerDir())
    }

    /** The store-age round's three shared-Rust calls, over the real machine + FFI fold. */
    private val ageClaimMachine = object : AgeClaimMachine {
        override suspend fun requestAgeNonce(): AgeNoncePlain = machine.requestAgeNonce()
        override fun ageClaimDigest(nonceHex: String, band: String, applicationId: String): ByteArray =
            machine.ageClaimDigest(nonceHex, band, applicationId)
        override fun bandFromAgeRange(lower: UInt?, upper: UInt?): String? =
            ageBandFromAgeRange(lower, upper)
    }

    /**
     * The android store-age arm (`family-safety.md` § The account age band,
     * D3 + D5): right before an admission call — the invite-request submit and
     * the code redeem — ask the Play Store for the user's age range, fold it to
     * the band, harden it with a Play Integrity classic verdict over the
     * machine-minted nonce, and set the claim on the machine, which carries it
     * on both wire bodies. Best-effort throughout (`StoreAgeClaim`): no Play,
     * no shared signal, or no verdict → a declared-only claim or none, and the
     * nest's provenance says so. Runs the real Play twin only in the
     * Play-distributed build types (`foss` compiles the inert one). A null
     * Activity (no screen behind the call) makes no claim rather than guessing.
     */
    suspend fun attachStoreAgeClaim(activity: Activity?) {
        val claim = activity?.let {
            StoreAgeClaim.build(StoreAgeGlue, ageClaimMachine, it, BuildConfig.APPLICATION_ID) { reason ->
                Log.w(TAG, "store-age claim degraded: $reason")
            }
        }
        machine.setAgeClaim(claim)
    }

    /**
     * First-authenticated-setup serving-enablement latches (onboarding.md § 3b
     * *Mechanism* · mail-credentials.md § Auto-enable for new users). Latched on
     * every `WizardOutcome::LoggedIn` (fresh onboarding only — returning users
     * skip the wizard) from the machine-derived intents (`emailEnableRequested()`
     * and its three DAV siblings — the claim-time enablement checkboxes are
     * retired with the storage-mode page; onboarding.md § 3b-bis). The authed
     * surface takes all four together exactly once via
     * [consumePendingServingEnablement] and hands them to the ONE shared step
     * (`ApiClient.applyPostClaimServingEnablement`), which discriminates
     * `am_i_admin` and fires the mail provision, the three DAV toggles and their
     * companion mints itself. `null` = no fresh onboarding completed (the launch
     * glue no-ops on a returning-user relaunch — the once-only / opt-out
     * guarantee: an opt-out is not re-minted on the next launch). Process-lifetime
     * only (not persisted): a missed enable is recoverable from the mail-settings
     * and admin pages, which fire the same idempotent paths.
     */
    @Volatile
    private var pendingServingEnablement: ServingEnablementIntents? = null

    /** The four machine-derived serving-enablement intents read off the onboarding
     *  machine at `LoggedIn`. */
    data class ServingEnablementIntents(
        val email: Boolean,
        val caldav: Boolean,
        val carddav: Boolean,
        val webdav: Boolean,
    )

    /** One-shot read of the serving-enablement latch: returns (and clears) all four
     *  intents as one boundary, or `null` when no fresh onboarding reached
     *  `LoggedIn` (a returning-user relaunch — the launch glue no-ops). */
    fun consumePendingServingEnablement(): ServingEnablementIntents? {
        val v = pendingServingEnablement
        pendingServingEnablement = null
        return v
    }

    /**
     * The one-tap "trust this box" answer latched on the `trust_prompt` page
     * (`onboarding.md` § 3b-ter), latched on `WizardOutcome::LoggedIn` for the
     * authed launch glue to mint (sibling of [pendingServingEnablement]). `true`
     * only when the user tapped grant; `false` on skip or when the app never
     * declared the capability.
     */
    @Volatile
    private var pendingTrustGranted = false

    /** One-shot read of the trust_prompt latch: returns (and clears) whether
     *  onboarding just completed with the default grant set requested. */
    fun consumePendingTrustGranted(): Boolean {
        val v = pendingTrustGranted
        pendingTrustGranted = false
        return v
    }

    /**
     * The RecoveryKey root the user confirmed on the sign-up `recovery_kit`
     * page, latched at `WizardOutcome::LoggedIn` for the authed launch glue to
     * register ([com.fauna.app.ui.viewmodel.PostAuthGlueVM.registerDeferredRecoveryKit]):
     * minted on the page but deliberately unregistered until a signed-in
     * connection exists (`identity-succession.md` § The RecoveryKey →
     * *Creation UX*). Never persisted — process-lifetime, like the trust latch;
     * `null` when skipped, never offered, or already taken.
     */
    @Volatile
    private var pendingRecoveryKit: String? = null

    /**
     * The DNS-provider credential the wizard verified at its `dns_config`
     * step (`OnboardingMachine::captured_dns_credential`), latched at
     * `WizardOutcome::LoggedIn` for the authed post-auth glue to seal into the
     * account's `fauna.state.dns` (`onboarding-provisioning.md` § 4. DNS
     * configuration, *Capture at onboarding, seal via the launched app*). The
     * wizard holds no authenticated session and the store is `BackupKey`-sealed,
     * so the seal is the signed-in edge's job, over the ONE shared rule
     * (`PostAuthGlueVM.sealCapturedDnsCredential`). Never persisted —
     * process-lifetime like the kit latch; `null` on the manual / set-up-later /
     * unverified paths, or once taken.
     */
    @Volatile
    private var pendingDnsCredential: CapturedDnsCredential? = null

    /** One-shot read of the captured-DNS-credential latch: returns (and clears) it. */
    fun consumePendingDnsCredential(): CapturedDnsCredential? {
        val v = pendingDnsCredential
        pendingDnsCredential = null
        return v
    }

    /** One-shot read of the confirmed-kit latch: returns (and clears) it. */
    fun consumePendingRecoveryKit(): String? {
        val v = pendingRecoveryKit
        pendingRecoveryKit = null
        return v
    }

    /**
     * Post-factory-reset re-claim claim code (mail-bridge-lifecycle.md § Factory
     * reset). `fauna.admin.factory_reset` returns the new claim code to the
     * client — the human never sees it — so the re-onboarding claim-code step
     * must pre-fill it. Held here (process-lifetime, like [pendingServingEnablement])
     * rather than only on the machine, because the launch flow's CLAIM_CODE
     * routing calls `seed_identity` (which repositions the wizard at HandleEntry)
     * before re-navigating to claim-code; AppLaunchVM reads this to re-navigate
     * with the code (and the re-qualified `<handle>@<domain>` from the cached handle).
     * Cleared on a successful claim (LoggedIn).
     */
    @Volatile
    var pendingFactoryResetClaimCode: String? = null

    /**
     * Set by [com.fauna.app.ui.viewmodel.AccountSettingsVM.beginAddAccount] before the
     * append wizard mounts (`appState.isAddingAccount`, `FaunaNavHost.kt`'s append
     * branch) — mirrors apple's `OnboardingVM.completeOnboarding(append:)`. Gates
     * [handleWizardExit]'s `LoggedIn` arm: the append terminal
     * ([com.fauna.app.ui.viewmodel.AccountSettingsVM.completeAddAccount]) already
     * registers and activates the new account via its own `addAccount` +
     * `switchAccount` call, so running `persistLoggedIn` here too would move
     * `active` a second time before that terminal runs, which can short-circuit its
     * `switchAccount` guard (`actorId == activeActorId.value`) and skip the
     * outgoing account's teardown. Consumed (reset to `false`) the moment a wizard
     * run reaches `Done`, so it never leaks into a later cold-boot run.
     *
     * Also read (never consumed — only [handleWizardExit]'s `Done` arm resets it)
     * by [persistPendingInviteSlot] and [handleWizardExit]'s `AwaitingManualDns`
     * arm, which adopt the appended identity (switch away from the outgoing
     * account) once the shared writer has registered it, and passed as the
     * `append` flag of the shared confirm-identity moment by
     * [com.fauna.app.ui.screen.onboarding.IdentityCreatedVM.confirm] /
     * [com.fauna.app.ui.screen.onboarding.IdentityImportVM.confirm], which then
     * write nothing (long-term-store.md § Downgrade mirror + abandoned-append
     * recovery).
     */
    @Volatile
    var appendMode: Boolean = false

    /**
     * Wizard exit handler. Per docs/goal/behavior/onboarding.md §"Wizard exit
     * handling": every Continue/Redeem call returns an OnboardingStep; on
     * Done, route on the WizardOutcome variant. Returns true when the
     * caller should leave the wizard (LoggedIn or AwaitingManualDns
     * already routed to main; the pending-invite journey stays on its own page (a "pending
     * invite" surface — Spec 3 generalises later).
     */
    /**
     * Write the pending-invite resume slot at the `wizardSubmitInviteRequest()`
     * return — "the only write moment" (onboarding.md § 3 Persistence callouts)
     * — and in **append** mode adopt it: drop the OUTGOING account's live
     * session state so it stops running behind the registry's new active
     * pointer, tui's `adopt_appended_pending_invite` shape; onboarding.md § Multi-account — "the append glue adopts on
     * the submit return": register, write the slot, switch.
     *
     * android's half of the 2026-08-12 retirement of
     * `WizardOutcome::InviteSubmitted`: that journey no longer exits the wizard,
     * so [handleWizardExit] never sees it and the write lives here instead. The
     * slot itself is assembled by shared Rust ([OnboardingMachine.pendingInviteSlot]),
     * so the nest_url and status_json rules are not re-derived per app.
     *
     * A no-op unless the machine is actually in `PendingReview`, so a failed or
     * refused submit writes nothing.
     */
    fun persistPendingInviteSlot() {
        val slot = machine.pendingInviteSlot() ?: return
        // The identity the wizard is acting as comes from the MACHINE, never
        // back from the store (onboarding.md § Long-term store contract): an
        // append run has written nothing yet, by design.
        val secretHex = machine.effectiveSecret()
        if (secretHex.isNullOrEmpty()) {
            ShellLog.w(TAG, "pending-invite slot with no identity in the wizard")
            return
        }
        val isAppend = appendMode
        // Through the SHARED per-actor registry writer, which registers and
        // activates the identity beside its slot (onboarding.md § The
        // pending-invite surface, Persistence callouts).
        runCatching {
            registry.persistPendingInvite(
                secretHex = secretHex,
                nestUrl = slot.nestUrl,
                handle = slot.handle,
                requestId = slot.requestId,
                statusJson = slot.statusJson,
            )
        }.onSuccess {
            if (isAppend) {
                // The shared writer above already made the appended identity
                // active in the registry (`set_active`); reconnecting AS it is
                // not possible yet (a pending invite has no nest to connect
                // to — the wizard's own invite-request/PendingReview surface,
                // already driven off `machine`, is that account's launch
                // surface). What must happen now is dropping the OUTGOING
                // account's live state, the same drop `AccountSettingsVM
                // .switchAccount` runs for every other switch.
                actorScope.dropActorScopedState()
                WidgetDataWorker.refoldForAccountSwitch(context)
            }
        }.onFailure { ShellLog.w(TAG, "persistPendingInvite failed: ${it.message}") }
    }

    fun handleWizardExit(step: OnboardingStep): WizardOutcome? {
        if (step != OnboardingStep.DONE) return null
        val outcome = machine.wizardOutcome() ?: return null
        val isAppend = appendMode
        appendMode = false
        when (outcome) {
            is WizardOutcome.LoggedIn -> {
                // Moment 4 — the wizard's logged-in terminal, through the shared
                // helper (`persist_logged_in`): record this identity's home nest
                // PER-ACTOR, activate it, and spend the pending-invite and
                // awaiting-DNS slots. The terminal reads the secret from the
                // MACHINE, never from the store (onboarding.md § Long-term store
                // contract). Skipped in append mode — the append terminal
                // ([com.fauna.app.ui.viewmodel.AccountSettingsVM.completeAddAccount])
                // registers and switches; see [appendMode]'s doc for why.
                if (!isAppend) {
                    val secretHex = machine.effectiveSecret()
                    if (secretHex.isNullOrEmpty()) {
                        ShellLog.w(TAG, "LoggedIn exit with no identity in the wizard")
                    } else {
                        runCatching {
                            val actorId = registry.persistLoggedIn(
                                secretHex = secretHex,
                                nestUrl = outcome.nestUrl,
                                deviceId = sessionAccount.deviceIdFor(secretHex),
                                // The reach hint (onboarding.md § Reach hint) is
                                // read off the machine at this terminal once
                                // android's leg lands; null keeps today's
                                // behaviour — the account simply waits for DNS.
                                reachIpv4 = null,
                            )
                            // The handle lands in the registry's server-data
                            // cache, so the launch flow's known-nest re-entries
                            // pre-fill it. `updateCache` overwrites all three
                            // fields — carry domain/tier forward (apple's
                            // `OnboardingVM` shape).
                            if (outcome.handle.isNotEmpty()) {
                                val entry = registry.list().firstOrNull { it.actorId == actorId }
                                registry.updateCache(actorId, outcome.handle, entry?.domain, entry?.tier)
                            }
                            // The predecessor seeds a phrase-only restore
                            // recovered — empty on every ordinary onboarding —
                            // linked to the identity just activated, HERE,
                            // before the authed shell builds its session
                            // (`identity-succession.md` § Seed escrow →
                            // *Restore path*: a session resolves predecessor
                            // keys once, so a later persist leaves
                            // predecessor-sealed data dark until a relaunch).
                            persistRestoredPredecessors(registry, actorId, machine.restoredPredecessors())
                        }.onFailure { ShellLog.w(TAG, "persistLoggedIn failed: ${it.message}") }
                    }
                    // Claim terminal #3 (gap CR-1) is spent inside
                    // `persistLoggedIn`. A skipped or failed helper (no secret, a
                    // refused add) deliberately leaves the slot set: the claim
                    // was never recorded on this device, so resuming the
                    // pre-filled claim surface is the recoverable state.
                }
                runCatching { launchPersistence.deletePendingInvite() }
                    .onFailure { ShellLog.w(TAG, "registry deletePendingInvite failed: ${it.message}") }
                // Claim terminal #1. Reaching LoggedIn means the DNS resolved and
                // the claim landed, so the deferred-DNS slot is spent. Leaving it
                // set would pin every future launch to the "Almost ready" surface
                // for a nest the user is already on — the awaiting-DNS row is
                // evaluated BEFORE the silent-challenge row. This terminal is the
                // ONE clearing moment (onboarding.md § Long-term store contract,
                // ratified 2026-09-21): never at the claim itself.
                // `persistLoggedIn` above already spends it on the cold-boot
                // path; this explicit call is what covers the append arm, which
                // skips the helper (see [appendMode]).
                runCatching { registry.clearAwaitingDns() }
                    .onFailure { ShellLog.w(TAG, "registry clearAwaitingDns failed: ${it.message}") }
                // The in-memory latch of the factory-reset claim code is the
                // same-process fast path of claim terminal #3 above.
                pendingFactoryResetClaimCode = null
                // Latch the serving-enablement intents: fresh onboarding reached
                // LoggedIn. The four machine-derived intents travel together; the
                // authed surface hands them to the ONE shared post-claim step
                // (onboarding.md § 3b *Mechanism*), which does the
                // am_i_admin discrimination itself.
                pendingServingEnablement = ServingEnablementIntents(
                    email = machine.emailEnableRequested(),
                    caldav = machine.caldavEnableRequested(),
                    carddav = machine.carddavEnableRequested(),
                    webdav = machine.webdavEnableRequested(),
                )
                // onboarding.md § 3b-ter: the one-tap trust answer latched on
                // the `trust_prompt` page. Consume-once, so a handoff that
                // runs twice mints once; `false` when the user skipped or was
                // never asked.
                pendingTrustGranted = machine.takeTrustPromptGranted()
                // The kit confirmed on the `recovery_kit` page. Consume-once;
                // `null` if skipped. An append run drops it, as tui and linux
                // do: the kit registers only on the first-sign-in launch, and
                // Settings says never-created (where the user can create one).
                val kit = machine.takePendingRecoverySecret()
                if (isAppend) {
                    if (kit != null) {
                        ShellLog.w(TAG, "add-account: the confirmed recovery kit is not registered on this path")
                    }
                } else {
                    pendingRecoveryKit = kit
                    // The DNS credential verified in the wizard: `null` on every
                    // run that did not provision a box through a supported
                    // provider. Latched here beside the kit, like every other
                    // hand-off intent the authed glue consumes once.
                    pendingDnsCredential = machine.capturedDnsCredential()
                }
            }
            // ⚠ There is deliberately no `InviteSubmitted` arm (retired
            // 2026-08-12). That exit dropped the user into a SESSIONLESS
            // authenticated shell here — one of the five divergent per-app
            // behaviors onboarding.md § Wizard exit handling deletes. The
            // journey now has no exit: the wizard stays on `invite_request` and
            // polls, and the slot is written at the submit return by
            // `persistPendingInviteSlot` below.
            is WizardOutcome.AwaitingManualDns -> {
                // The identity the wizard is acting as comes from the MACHINE,
                // never back from the store (the same rule the LoggedIn terminal
                // above follows).
                val secretHex = machine.effectiveSecret()
                // Deliberately NO nest-URL write: the nest is provisioned but not
                // claimed (its DNS is still pending), so a silent challenge against
                // it could only fail. The resume record carries its own nestUrl,
                // and the launch machine's awaiting-DNS row is what routes the
                // relaunch (onboarding.md § Long-term store contract).
                //
                // Through the SHARED per-actor writer, whose opaque-JSON slot
                // composes correctly even with an EMPTY handle — the deferred-DNS
                // exit reaches `dns_post_instructions` with no handle stage.
                //
                // Unconditional — append mode included:
                // this write IS the append's terminal for this exit
                // (long-term-store.md § Downgrade mirror + abandoned-append
                // recovery; tui and windows persist here unconditionally too), so
                // skipping it would leave the appended identity's secret and the
                // parked box's claim code in process memory for the whole "Almost
                // ready" wait. `persistAwaitingDns` registers AND activates, and
                // the append arm then switches — exactly the pending-invite
                // adoption ([persistPendingInviteSlot]). The hijack a skip once
                // guarded against (an append parked here, then backed out to
                // identity-choice and cancelled over a moved active pointer) has no
                // path left: the exit is a wizard Done, which the shared machine's
                // back() never leaves, and [appendMode] is already cleared above.
                if (secretHex.isNullOrEmpty()) {
                    ShellLog.w(TAG, "awaiting-DNS exit with no identity in the wizard")
                } else {
                    runCatching {
                        registry.persistAwaitingDns(
                            secretHex = secretHex,
                            nestUrl = outcome.nestUrl,
                            // NOT in the outcome payload — the slot requires it and
                            // the eventual LoggedIn carries it, but it is not
                            // derivable from nestUrl. The machine is where the
                            // wizard put it.
                            handle = machine.currentHandle(),
                            dnsRecordsJson = machine.awaitingDnsRecordsJson(),
                            claimCode = outcome.claimCode,
                        )
                    }.onSuccess {
                        if (isAppend) {
                            // The appended identity is now the registry's active
                            // account; drop the OUTGOING account's live state, the
                            // same switch drop [persistPendingInviteSlot]'s append
                            // arm runs. The wizard stays on "Almost ready", driven
                            // off `machine`, which is this account's launch surface.
                            actorScope.dropActorScopedState()
                            WidgetDataWorker.refoldForAccountSwitch(context)
                        }
                    }.onFailure { ShellLog.w(TAG, "persistAwaitingDns failed: ${it.message}") }
                }
            }
        }
        return outcome
    }
}

/**
 * Persist the predecessor seeds a phrase-only restore recovered (the wizard's
 * `restoredPredecessors()`), linked to [restoredActor] — the identity they are
 * predecessors *of*, named explicitly because an add-account restore persists
 * before the switch that activates it. The shared
 * `AccountRegistry::persist_restored_predecessors`, best-effort per row; an
 * ordinary onboarding recovered nothing and writes nothing.
 */
internal fun persistRestoredPredecessors(
    registry: FfiAccountRegistry,
    restoredActor: String?,
    predecessors: List<RestoredPredecessorSeed>,
) {
    if (predecessors.isEmpty()) return
    runCatching { registry.persistRestoredPredecessors(restoredActor, predecessors) }
        .onFailure { ShellLog.w(TAG, "persistRestoredPredecessors failed: ${it.message}") }
}
