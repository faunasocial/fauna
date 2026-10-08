package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ActorScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SessionAccount
import com.fauna.app.core.HexUtil
import com.fauna.app.core.LaunchObserverImpl
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.SignOutCredentialEraser
import com.fauna.app.core.ShellLog
import com.fauna.app.core.StolenCeremonyHold
import com.fauna.app.core.SuccessionHandoff
import com.fauna.ffi.FfiAccountRegistry
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import uniffi.fauna_launch_machine.AccountIndexRefusal
import uniffi.fauna_launch_machine.LaunchIdentity
import uniffi.fauna_launch_machine.LaunchMachine
import uniffi.fauna_launch_machine.LaunchPersistence
import uniffi.fauna_launch_machine.LaunchPhase
import uniffi.fauna_launch_machine.LaunchSnapshot
import uniffi.fauna_launch_machine.LaunchWizardEntry
import uniffi.fauna_launch_machine.resolvedDialUrl

/**
 * Drives the App-launch flow per docs/goal/behavior/onboarding.md §App-launch
 * routing. Owns the shared-Rust [LaunchMachine] (silent-challenge HTTP +
 * bearer-token lifecycle + 401-reactive) and maps [LaunchPhase] snapshots
 * to navigation outcomes the [com.fauna.app.ui.navigation.FaunaNavHost]
 * mounts.
 *
 * Replaced the previous Kotlin-only computeLaunchAction implementation
 * 2026-05-04 with this shared-Rust silent-challenge design (design tracked
 * internally).
 */
@HiltViewModel
class AppLaunchVM @Inject constructor(
    private val machine: LaunchMachine,
    observer: LaunchObserverImpl,
    private val host: OnboardingHost,
    private val sessionAccount: SessionAccount,
    private val apiClient: ApiClient,
    /**
     * The ONE canonical actor-scoped drop (`account-scoping.md` § the in-memory
     * corollary). The post-auth identity-change re-entry below used to call
     * `apiClient.clearAuth()` alone, which ran none of the registered closers —
     * the second of the two sites that made android's two funnels diverge, and
     * the one where it matters most: a proven nest-identity change is exactly
     * when nothing of the old session may survive.
     */
    private val actorScope: com.fauna.app.core.ActorScope,
    private val launchPersistence: LaunchPersistence,
    // The three deps `resetAccountIndex()` needs to run the documented floor
    // (`long-term-store.md` § Cleanup contract) — the same erase
    // `AccountSettingsVM.signOut()` runs; none of these were needed here
    // before this row.
    private val registry: FfiAccountRegistry,
    private val secureStorage: SecureStorage,
    private val accountStores: AccountStores,
    /** Holds back a supersession THIS device's own stolen-identity ceremony
     *  caused while that ceremony owns the Account page ([routeSessionEnding]). */
    private val ceremonyHold: StolenCeremonyHold,
) : ViewModel() {

    /** Observable snapshot the NavHost collects as state. */
    val snapshot: StateFlow<LaunchSnapshot> = observer.snapshot

    init {
        // The live identity channel (conversations.md § State & data shape ->
        // *Self-address: live, never baked*). `snapshot.identity` carries whatever
        // the nest confirmed on the most recent silent challenge, so this is "the
        // one place identity state lands" the § names — and reacting to it is what
        // keeps the self-address from being baked at build time.
        //
        // Without it, the address would be assembled in `ApiClient` from the
        // session account's cached handle/domain alone — the registry cache the
        // silent challenge writes (`saveAuthenticated`), i.e. whatever the most
        // recent challenge left, never pushed into a session already built.
        viewModelScope.launch {
            snapshot.map { it.identity }.distinctUntilChanged().collect(::applyIdentity)
        }
    }

    /**
     * Land a nest-confirmed identity: push the address into the live
     * conversations session so both rails (SMTP `From:` and the FaunaMls routing
     * domain) heal at use time. The registry's server-data cache every other
     * reader reads is already refreshed by the silent challenge itself
     * (`RegistryLaunchPersistence::save_authenticated`).
     *
     * An unresolved half is dropped rather than composed: an empty local part or
     * an empty domain is the forbidden `"@nest.example"` shape, which the same §
     * says must be treated exactly like a missing address — the session keeps the
     * empty address it was built with and a send refuses locally.
     */
    private fun applyIdentity(identity: LaunchIdentity?) {
        // The handle is sometimes @-qualified (the MailSettingsVM precedent),
        // so normalize to bare before pairing it with the domain.
        val handle = identity?.handle.orEmpty().substringBefore("@")
        val domain = identity?.domain.orEmpty()
        if (handle.isBlank() || domain.isBlank()) return
        apiClient.setConversationsSelfAddress("$handle@$domain")
    }

    /** Suspend; runs the LaunchMachine's four-case routing and HTTP. */
    suspend fun start() = machine.start()

    /**
     * Connect (or reconnect, on an account switch) [ApiClient]'s WS-RPC session for
     * the now-active account. Call once `navTargetFor` yields [NavTarget.Authenticated]
     * — on a cold boot AND on every re-entry into this branch after a switch (the same
     * `LaunchedEffect(Unit)` re-fires, since Compose treats re-entering the
     * `appState.isOnboarding` branch as a fresh composable instance).
     *
     * Prior to this, `ApiClient.authenticate()` had NO caller anywhere in the app
     * (a regression from the 2026-05-04 LaunchMachine rewrite, which delegated
     * silent-challenge to shared Rust but dropped the `apiClient.nodeUrl = …;
     * apiClient.authenticate(secret)` call the old `runSilentChallenge()` used to
     * make) — `nestClient` stayed permanently null and every WS-RPC accessor threw.
     * `sessionAccount` reads the registry's ACTIVE account on every access, so this is
     * also the switch rebuild's reconnect step — no separate mechanism needed.
     *
     * The socket dials [resolvedDialUrl], never the stored literal directly —
     * the post-`LoggedIn` leg of the dial seam (`docs/goal/behavior/onboarding.md`
     * § 3b, `fauna_launch_machine::dial::resolved_dial_url`). `sessionAccount`
     * itself keeps the literal typed URL; only this connection site resolves.
     */
    suspend fun connectActiveSession() {
        val secret = sessionAccount.secretHex ?: return
        val storedNestUrl = sessionAccount.nestUrl.orEmpty()
        apiClient.nodeUrl = resolvedDialUrl(storedNestUrl)
        apiClient.authenticate(secret)
    }

    /**
     * Retry CTA on the launch retry screen. Re-runs the silent challenge
     * against the saved nest; no-op if the machine isn't in
     * Offline { transient: true }.
     */
    suspend fun retry() = machine.retrySilentChallenge()

    /**
     * "Trust this nest" CTA on the `launch_identity_changed` warning
     * (security.md § Transport trust). Forgets the stale TOFU pin
     * and re-runs the silent challenge on `machine` — the SAME instance that
     * produced the `IdentityChanged` verdict, since it re-reads secret +
     * nest_url off that state. No-op on any machine not in `IdentityChanged`.
     */
    suspend fun trustIdentity() = machine.trustNestIdentity()

    /**
     * `account-index-reset-confirm-button`'s action — the malformed verdict's
     * documented floor (`long-term-store.md` § Cleanup contract): erase every
     * account's per-actor slots and the index, then
     * re-run the launch machine so it re-routes off the now-empty store
     * (lands on `WizardAt(IdentityChoice)` — fresh onboarding). The same
     * sequence `AccountSettingsVM.signOut()` runs (registered here purely for
     * this reset — nothing else on the launch screen needed these deps
     * before this row), minus the residue toast: the confirm already stated
     * the residual BEFORE the click (its own warning text switches to
     * `INDEX_MALFORMED_RESET_RESIDUAL`), so there is nothing left to surface
     * afterward beyond the log line.
     */
    suspend fun resetAccountIndex() {
        // Sign-out-shaped: the erase below takes this machine's account-store
        // slot (the writer key) with it, same as `AccountSettingsVM.signOut`
        // (`sync-agent-credentials.md` § Credential model → *The signed-out
        // reconcile*).
        apiClient.stopAccountRuntimeForSignOutAwaited()
        val credentials = SignOutCredentialEraser(registry, secureStorage).eraseCredentials()
        actorScope.dropActorScopedState()
        // Recorded like a sign-out's residue, so the signed-out launch the
        // re-routing below lands on re-sweeps it ([recheckSignOutResidue]).
        val residue = accountStores.recordResidue(accountStores.eraseAllAccounts(), credentials)
        if (residue != null) {
            ShellLog.w("AppLaunchVM", "account-index reset left data behind")
        }
        machine.start()
    }

    /**
     * The signed-out launch's silent re-check (`account-scoping.md` § Erasure
     * follows scope → *the residue surface*): a residue a previous sign-out
     * recorded is re-swept FIRST, and comes back only if something is still
     * left — the residue `identity_choice` then paints. Shared Rust leaves the
     * record alone while the registry holds an account. Twin of tui's
     * `account_scope::recheck_residue_at_launch`.
     */
    suspend fun recheckSignOutResidue(): com.fauna.ffi.FfiSignOutResidue? =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            accountStores.recheckResidueAtLaunch(SignOutCredentialEraser(registry, secureStorage))
        }

    /**
     * Remove Again (`sign-out-residue-retry-button`): the shared re-sweep over
     * what [residue] recorded. `null` — the device is now clean, the view closes.
     */
    suspend fun retrySignOutResidue(
        residue: com.fauna.ffi.FfiSignOutResidue,
    ): com.fauna.ffi.FfiSignOutResidue? =
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            accountStores.retryResidue(residue, SignOutCredentialEraser(registry, secureStorage))
        }

    /**
     * One-shot best-effort post-auth identity re-check (`security.md` §
     * Post-auth surfacing) — the android analog of linux's `silent_sign_in()`
     * and apple's `performPostAuthSilentSignIn()`: re-run the same silent
     * challenge the boot-time [machine] ran, once, against the now-
     * authenticated session, so a nest identity that changes AFTER this
     * boot's own challenge is still caught before the session goes stale.
     * Two verdicts escalate: `FfiException.NestIdentityChanged`, and a `null`
     * result — `fauna.auth.not_registered`, the nest no longer signing this
     * identity in (suspended or removed while signed in; `onboarding.md`
     * § App-launch routing, the previously-signed-in row, and the shared
     * `SilentSignInVerdict::NotRegistered` rule tui/linux/apple follow).
     * Every other outcome (success, transient reachability) is swallowed,
     * since a live session must never tear itself down over a flaky refresh,
     * only a proven verdict.
     *
     * On escalation, tears the session down WITHOUT erasing credentials
     * ([com.fauna.app.core.ActorScope.dropActorScopedState] — a drop, never an
     * erase) and returns `true` so the caller re-enters
     * the real launch flow (`appState.isOnboarding = true`, the same "no
     * relaunch" reconnect [switchAccount][com.fauna.app.ui.viewmodel.AccountSettingsVM.switchAccount]
     * already uses) — which re-runs THIS SAME [machine] instance's [start]
     * and re-derives the verdict fresh. Never paint the surface from a
     * synthesized phase: the re-trust button drives [trustIdentity] on the
     * machine that produced the verdict, and a synthesized phase would render
     * an identical-looking surface whose button silently did nothing — the
     * dead-button trap every leg has had to avoid.
     */
    suspend fun performPostAuthSilentSignIn(): Boolean {
        val nodeUrl = sessionAccount.nestUrl?.ifBlank { null } ?: return false
        val secretHex = sessionAccount.secretHex?.ifBlank { null } ?: return false
        return try {
            val signedIn = com.fauna.ffi.silentChallenge(nodeUrl, HexUtil.hexToBytes(secretHex))
            if (signedIn == null) {
                escalateSessionEnding(com.fauna.ffi.FfiSessionEndingVerdict.SIGN_IN_REFUSED)
                true
            } else {
                false
            }
        } catch (e: com.fauna.ffi.FfiException.NestIdentityChanged) {
            ShellLog.w(
                "AppLaunchVM",
                "[post-auth] silent_challenge found the nest identity changed — re-entering launch",
            )
            actorScope.dropActorScopedState()
            true
        } catch (e: Exception) {
            // Reachability / classification noise — best-effort, never escalate.
            false
        }
    }

    /**
     * The session-ending verdicts a stopped reconnect supervisor reports
     * ([ApiClient.sessionEnding]) — for the authenticated shell to route
     * through [escalateSessionEnding] and then re-enter launch.
     */
    val sessionEnding = apiClient.sessionEnding

    /**
     * The one door every mid-session session-ending verdict takes on android —
     * the twin of apple's `escalateSessionEnding` and windows'
     * `SessionEndingRoute`: tear the session down WITHOUT erasing credentials
     * (a drop, never an erase — a suspension may be lifted, and only the
     * user's own sign-out erases), after which the caller re-enters the real
     * launch flow (`appState.isOnboarding = true`). The re-run [machine] then
     * meets the verdict on its own challenge and owns the surface — the
     * refused surface for `SIGN_IN_REFUSED`, the identity-changed warning for
     * `NEST_IDENTITY_CHANGED`, the import route for `SUPERSEDED`.
     */
    fun escalateSessionEnding(verdict: com.fauna.ffi.FfiSessionEndingVerdict) {
        ShellLog.w("AppLaunchVM", "[post-auth] session-ending verdict $verdict — re-entering launch")
        actorScope.dropActorScopedState()
    }

    /** Whether the active account is a succession's successor still owed its
     *  kit — the post-auth navigation's peek ([SuccessionHandoff.owesKitTo]). */
    fun owesSuccessorKitHere(): Boolean =
        SuccessionHandoff.owesKitTo(runCatching { registry.active() }.getOrNull())

    /**
     * Route a mid-session session-ending verdict to [escalateSessionEnding] and
     * then [reenterLaunch] — apple's `escalateSessionEnding(_:ceremonyHold:)`.
     * A supersession goes through the [StolenCeremonyHold]: this device's own
     * stolen-identity ceremony is what supersedes the identity, so the
     * reconnect supervisor's `SUPERSEDED` typically arrives before the
     * ceremony's own result is handled, and escalating then would tear the
     * Account page — and a parked key that is the only copy in existence —
     * down with it (`settings.md` § Recovery kit → *The persist-failure message
     * survives the page*). A nest-identity change or a refused sign-in is never
     * the ceremony's doing, so it escalates at once.
     */
    fun routeSessionEnding(verdict: com.fauna.ffi.FfiSessionEndingVerdict, reenterLaunch: () -> Unit) {
        val perform = {
            escalateSessionEnding(verdict)
            reenterLaunch()
        }
        if (verdict == com.fauna.ffi.FfiSessionEndingVerdict.SUPERSEDED) {
            ceremonyHold.escalate(perform)
        } else {
            perform()
        }
    }

    /**
     * Drop back into the wizard at handle_entry with the identity
     * pre-seeded. Used by LaunchRetryScreen's "Use a different nest" CTA.
     */
    fun seedForFallthrough(): String {
        val secret = sessionAccount.secretHex ?: return "onboarding/identity-choice"
        host.machine.seedIdentity(secret)
        return "onboarding/handle-entry"
    }

    /**
     * The launch arm for a **succeeded** identity — the android twin of apple's
     * `SupersededLaunchRoute`, windows' `OnboardingViewModel.RouteSupersededRefusalAsync`
     * and tui's `route_superseded_to_import`.
     *
     * **No new ui.yaml elements.** The affordance IS the existing import flow
     * (page `identity_import`), the reason on that page's existing
     * `error-message`. The reason goes through the machine
     * (`beginImportIdentityWithReason`: step and reason in one mutation), so
     * the import screen reads it off the machine like every other
     * observer-driven surface.
     *
     * **The first message deliberately does not name the successor.** The
     * refusal's successor is only *claimed* until the registration chain proves
     * it; presenting it as fact would make this client trust the nest as an
     * authorizer. The claim goes to the log; the anonymous chain walk
     * ([resolve], production `successionResolveVerifiedSuccessor`) upgrades the
     * message through [verifiedReason] once it verifies a successor, and every
     * failure leaves [claimFreeReason] standing.
     *
     * The refused identity's secret and nest come from [sessionAccount] — the
     * registry's session material for the active account, never a legacy
     * mirror. Without them the claim-free message is final.
     *
     * **Adopt when this device already holds the verified successor's key** —
     * the state a lost succession reply from THIS device's own ceremony leaves
     * behind (`identity-succession.md` § Implementation status today, *a lost
     * submit reply no longer destroys the account*): the ceremony persisted the
     * successor without activating it, and its undecidable arm promised that
     * reopening the app signs in as it. Then there is nothing to import. The
     * shared `adoptHeldSuccessor` decides (and records the succession link);
     * this records the kit and the group sweep the adoption owes
     * ([SuccessionHandoff.recordRelaunchAdoption]) BEFORE handing the switch to
     * [adopt] (`succession-propagation.md` § Propagation → *Own device fleet*,
     * the relaunch-adoption clause). Both halves are proofs, never claims: the
     * successor is the chain's answer, and the key is one this device minted
     * and kept. apple's `SupersededLaunchRoute` and tui's
     * `App::adopt_held_successor` are the twins.
     */
    suspend fun routeSupersededRefusal(
        claimedSuccessor: String,
        claimFreeReason: String,
        verifiedReason: (String) -> String,
        adopt: suspend (String) -> Unit = ::adoptSuccessor,
        resolve: suspend (String, ByteArray) -> String? = { url, secret ->
            com.fauna.ffi.successionResolveVerifiedSuccessor(url, secret)
        },
    ) {
        val secretHex = sessionAccount.secretHex?.ifBlank { null }
        val nestUrl = sessionAccount.nestUrl?.ifBlank { null }
        ShellLog.e(
            "AppLaunchVM",
            "[launch-machine] this identity was succeeded (claimed successor $claimedSuccessor) " +
                "— routing to the identity-import flow",
        )
        if (secretHex != null) host.machine.seedIdentity(secretHex)
        host.machine.beginImportIdentityWithReason(claimFreeReason)

        if (secretHex == null || nestUrl == null) {
            ShellLog.w("AppLaunchVM", "[launch] no session material for the refused identity — cannot verify the succession")
            return
        }
        val verified = try {
            resolve(nestUrl, HexUtil.hexToBytes(secretHex))
        } catch (e: Exception) {
            // A malformed secret, or the walk itself failing: the claim-free message stands.
            ShellLog.w("AppLaunchVM", "[launch] could not verify the succession: ${e.message}")
            return
        } ?: return
        // The walk can land after the user navigated away; asserting a
        // supersession over whatever they are doing now would show a banner
        // from a flow they have already handled.
        if (host.machine.step() != com.fauna.ffi.onboarding.OnboardingStep.IDENTITY_IMPORT) return
        val predecessor = runCatching { registry.active() }.getOrNull()
        if (predecessor != null && registry.adoptHeldSuccessor(predecessor, verified)) {
            ShellLog.i("AppLaunchVM", "[launch] this device holds the verified successor $verified; adopting it")
            SuccessionHandoff.recordRelaunchAdoption(predecessor, verified)
            adopt(verified)
            return
        }
        host.machine.beginImportIdentityWithReason(verifiedReason(verified))
    }

    /**
     * The relaunch adoption's account switch — the launch-surface twin of
     * `AccountSettingsVM.switchAccount`: activate the successor, drop whatever
     * the refused identity left scoped, and re-run THIS launch machine, which
     * re-reads the now-active account's material and routes it to
     * `Authenticated` (the same re-route [resetAccountIndex] relies on).
     */
    private suspend fun adoptSuccessor(successor: String) {
        try {
            registry.setActive(successor)
        } catch (e: Exception) {
            ShellLog.e("AppLaunchVM", "[launch] activating the adopted successor failed: ${e.message}")
            return
        }
        actorScope.dropActorScopedState()
        machine.start()
    }

    /**
     * Map a [LaunchPhase] to a navigation outcome, applying any required
     * side effects on the OnboardingMachine (seed identity / pending
     * invite / navigate-to-invite-request) along the way.
     *
     * In-flight phases (Boot / Hydrating / SilentChallenge / Refreshing)
     * return null — the caller renders a blank screen and waits.
     *
     * Called directly in a Composable body ([FaunaNavHost]'s `when`), so it
     * may re-run on every recomposition for the same [phase] — the Rust
     * seed methods are idempotent on identity, so repeat invocation is safe,
     * but any future non-idempotent side effect added here needs the same
     * guarantee or must be hoisted into a `derivedStateOf(snapshot.phase)`.
     *
     * [accountIndexRefusal] — the saved account index is present and this
     * build cannot use it (`version-compatibility.md` § 5 item 9) — is
     * checked BEFORE `phase` is even matched: the machine reads
     * `LaunchPersistence.accountIndexRefusal` before it even attempts
     * `loadIdentity`, so this outranks `IdentityChanged`, `WizardAt`, and
     * the generic `Offline` rows (`onboarding.md` § App-launch routing).
     * It rides the snapshot's additive side channel rather than a
     * `LaunchPhase` variant (mirrors `superseded_successor`'s reasoning),
     * so a caller that never reads it still lands on `NeedsUpdate` off
     * `phase` alone. Twin of `apps/fauna-tui/src/launch.rs`'s `route()`.
     *
     * [signInRefused] — the snapshot's `sign_in_refused` side channel, set
     * only while the phase is `Offline { transient: false }` — upgrades that
     * terminal row into the dedicated `launch_sign_in_refused` surface (the
     * same sentence, plus Retry). It ranks below [accountIndexRefusal] and
     * is ignored on every other phase, exactly as tui's `route()` does.
     *
     * [supersededSuccessor] — the snapshot's `superseded_successor` side
     * channel: the nest refused this identity because it was SUCCEEDED
     * (`identity-succession.md` § Propagation → *Own device fleet*). Left to
     * `phase`, the machine's `Offline { transient: false }` projection would
     * paint "update your nest" over an identity problem, with no way out; it
     * routes to the import flow instead ([NavTarget.Superseded],
     * [routeSupersededRefusal]). Ranked below [accountIndexRefusal] and above
     * [signInRefused], as tui's `route()` and apple's `dispatchLaunch` rank it.
     */
    fun navTargetFor(
        phase: LaunchPhase,
        accountIndexRefusal: AccountIndexRefusal? = null,
        signInRefused: Boolean = false,
        supersededSuccessor: String? = null,
    ): NavTarget? {
        if (accountIndexRefusal != null) {
            return NavTarget.AccountIndexUnreadable(accountIndexRefusal)
        }
        if (supersededSuccessor != null && phase is LaunchPhase.Offline) {
            return NavTarget.Superseded(supersededSuccessor)
        }
        if (signInRefused && phase is LaunchPhase.Offline) {
            return NavTarget.SignInRefused
        }
        return navTargetForPhase(phase)
    }

    private fun navTargetForPhase(phase: LaunchPhase): NavTarget? = when (phase) {
        is LaunchPhase.Boot,
        is LaunchPhase.Hydrating,
        is LaunchPhase.SilentChallenge,
        is LaunchPhase.Refreshing -> null

        is LaunchPhase.Online -> NavTarget.Authenticated

        // `transient: true` → a reachability failure the client can't reliably
        // classify, so offer Retry. `transient: false` → terminal; today the only
        // such case the launch flow produces is the nest authoritatively
        // reporting it is outdated (`fauna.nest.outdated` → degraded mode), which
        // is NON-retryable → the NeedsUpdate surface (localized `last_error`, no
        // Retry button). version-compatibility.md Dim 4 / onboarding.md
        // § App-launch routing (version-mismatch row).
        is LaunchPhase.Offline ->
            if (phase.transient) NavTarget.TransientError else NavTarget.NeedsUpdate

        // The nest's pinned deployment identity changed, or a pinned nest can no
        // longer prove any identity (security.md § Transport trust —
        // the SSH `known_hosts` model). Auto-entry is BLOCKED and the bearer
        // already dropped machine-side; surface the localized warning with NO
        // Retry CTA (a retry cannot change the verdict and must never silently
        // re-pin) — only "trust this nest" (trustIdentity()) or "use a different
        // nest". Before this arm existed this phase fell into no arm at all
        // (non-exhaustive `when`, a compile error) — linux/tui/web already fixed
        // the equivalent gap. ui.yaml's `launch_identity_changed` was widened out
        // of `platforms: [web]` to all apps (user-approved, rule A, 2026-07-13).
        is LaunchPhase.IdentityChanged -> NavTarget.IdentityChanged

        is LaunchPhase.WizardAt -> when (phase.entry) {
            LaunchWizardEntry.IDENTITY_CHOICE -> NavTarget.Wizard("onboarding/identity-choice")
            LaunchWizardEntry.HANDLE_ENTRY -> {
                val secret = sessionAccount.secretHex
                if (!secret.isNullOrEmpty()) host.machine.seedIdentity(secret)
                NavTarget.Wizard("onboarding/handle-entry")
            }
            // The deferred-DNS row (onboarding.md § App-launch routing). The machine
            // checks it BEFORE the silent-challenge row: while the records are not yet
            // at the registrar the nest is unreachable by definition, so a challenge
            // could only fail through to the retry surface.
            //
            // Seed identity first (so signing works for the eventual claim), then the
            // record — read back through the same store the machine branched on, so
            // the record seeded is the record routed on.
            LaunchWizardEntry.AWAITING_MANUAL_DNS -> {
                val secret = sessionAccount.secretHex
                if (!secret.isNullOrEmpty()) host.machine.seedIdentity(secret)
                // Through the SAME per-actor `LaunchPersistence` the machine branched
                // on to pick this entry.
                val rec = launchPersistence.loadAwaitingDns()
                if (rec == null) {
                    // The machine routed here off this very slot, so its absence now
                    // means the store changed underneath us. The identity survives;
                    // handle-entry is the safe re-entry, rather than an "Almost ready"
                    // page with no records to show.
                    NavTarget.Wizard("onboarding/handle-entry")
                } else {
                    // The `...Json` seeder takes the slot's records verbatim, so
                    // Kotlin never parses them. Deserializing `dnsRecordsJson` here
                    // would mean mapping serde's `record_type` onto the binding's
                    // `recordType` by hand — get that wrong and the records come
                    // back empty, leaving the user an "Almost ready" page with
                    // nothing to add at their registrar.
                    host.machine.seedAwaitingManualDnsJson(
                        nestUrl = rec.nestUrl,
                        handle = rec.handle,
                        dnsRecordsJson = rec.dnsRecordsJson,
                        claimCode = rec.claimCode,
                    )
                    NavTarget.Wizard("onboarding/almost-ready")
                }
            }
            // Factory-reset resume (gap CR-1, nest/common.md § Client-state
            // recoverability). The admin dispatched a factory reset and this client
            // died before the re-claim completed — possibly before the reply that
            // carried the claim code ever rendered. The code survives only because it
            // was minted and persisted BEFORE dispatch, so seed the wizard's claim
            // page from the slot: the same surface as CLAIM_CODE below, but
            // pre-filled. The machine checks this row before every other one (the box
            // is wiped, so a silent challenge could only fail through to retry).
            //
            // Seed identity first (so signing works for the re-claim), then re-navigate
            // — seedIdentity repositions the wizard at HandleEntry. The record is read
            // back through the same store the machine branched on, so what we seed is
            // what it routed on.
            LaunchWizardEntry.PENDING_FACTORY_RESET -> {
                val secret = sessionAccount.secretHex
                if (!secret.isNullOrEmpty()) host.machine.seedIdentity(secret)
                // Through the same per-actor `LaunchPersistence` the machine
                // branched on, like the two sibling resume rows.
                val rec = launchPersistence.loadPendingFactoryReset()
                if (rec == null) {
                    // The machine routed here off this very slot, so its absence now
                    // means the store changed underneath us. The identity survives;
                    // handle-entry is the safe re-entry, rather than a claim page
                    // asking for a code that exists nowhere.
                    NavTarget.Wizard("onboarding/handle-entry")
                } else {
                    host.machine.navigateToClaimCodeForKnownNestWithCode(
                        nestUrl = rec.nestUrl,
                        handle = rec.handle,
                        code = rec.claimCode,
                    )
                    NavTarget.Wizard("onboarding/claim-code")
                }
            }
            LaunchWizardEntry.INVITE_REQUEST -> {
                val secret = sessionAccount.secretHex
                if (!secret.isNullOrEmpty()) host.machine.seedIdentity(secret)
                // Same reasoning as the AWAITING_MANUAL_DNS arm above — read back
                // through the registry-backed `LaunchPersistence`.
                val pending = launchPersistence.loadPendingInvite()
                if (pending != null) {
                    host.machine.seedPendingInvite(
                        nestUrl = pending.nestUrl,
                        handle = pending.handle,
                        requestId = pending.requestId,
                        statusJson = pending.statusJson,
                    )
                } else {
                    host.machine.navigateToInviteRequestForKnownNest(
                        nestUrl = sessionAccount.nestUrl ?: "",
                        handle = sessionAccount.handle ?: "",
                    )
                }
                NavTarget.Wizard("onboarding/invite-request")
            }
            LaunchWizardEntry.CLAIM_CODE -> {
                // Saved identity + saved nest URL that is reachable but unclaimed
                // (setup-status.claimed == false). Seed the identity and position
                // the wizard at claim_code for the known nest so the user can
                // become the admin. Per docs/goal/behavior/onboarding.md
                // §App-launch routing (unclaimed-nest row).
                val secret = sessionAccount.secretHex
                if (!secret.isNullOrEmpty()) host.machine.seedIdentity(secret)
                // After a factory reset (mail-bridge-lifecycle.md § Factory reset)
                // the danger zone latched the returned claim code (the human never
                // saw it) and re-qualified the cached handle to `<handle>@<domain>`
                // (so the re-claim re-registers the primary mail domain). seed_identity
                // above repositions the wizard at HandleEntry, so we must re-navigate
                // to claim_code regardless — WITH the latched code when present so it
                // pre-fills, else the ordinary cold-unclaimed path (admin types it).
                val pendingCode = host.pendingFactoryResetClaimCode
                if (!pendingCode.isNullOrEmpty()) {
                    host.machine.navigateToClaimCodeForKnownNestWithCode(
                        nestUrl = sessionAccount.nestUrl ?: "",
                        handle = sessionAccount.handle ?: "",
                        code = pendingCode,
                    )
                } else {
                    host.machine.navigateToClaimCodeForKnownNest(
                        nestUrl = sessionAccount.nestUrl ?: "",
                        handle = sessionAccount.handle ?: "",
                    )
                }
                NavTarget.Wizard("onboarding/claim-code")
            }
        }
    }

    sealed class NavTarget {
        data object Authenticated : NavTarget()
        data class Wizard(val startDestination: String) : NavTarget()
        data object TransientError : NavTarget()

        /** Terminal version mismatch: the nest reported it is outdated
         *  (`Offline { transient: false }`). A non-retry "update required"
         *  surface — the localized `last_error` with no Retry CTA. */
        data object NeedsUpdate : NavTarget()

        /** The nest answered the silent challenge with a refusal — the account
         *  was suspended or removed (`LaunchSnapshot.signInRefused`). The
         *  `launch_sign_in_refused` surface: the honest sentence, **with**
         *  Retry (the admin's restore is the way back in) and "Use a
         *  different nest". */
        data object SignInRefused : NavTarget()

        /** The nest refused this identity because it was succeeded
         *  (`LaunchSnapshot.supersededSuccessor`, the CLAIMED successor). The
         *  import flow, its reason set by [routeSupersededRefusal]. */
        data class Superseded(val claimedSuccessor: String) : NavTarget()

        /** The nest's pinned deployment identity changed (`LaunchPhase.IdentityChanged`).
         *  A non-retry warning surface — "trust this nest" (trustIdentity()) or
         *  "use a different nest", never a silent re-pin. */
        data object IdentityChanged : NavTarget()

        /** The saved account index is present and this build cannot use it
         *  (`version-compatibility.md` § 5 item 9). Never a retry or
         *  fallthrough — the nest was never contacted. The `NewerBuild`
         *  verdict offers nothing else; the `Malformed` verdict may reach
         *  the documented floor (`long-term-store.md` § Cleanup contract),
         *  but only through a confirm that states the residual first. */
        data class AccountIndexUnreadable(val refusal: AccountIndexRefusal) : NavTarget()
    }
}
