package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SessionAccount
import com.fauna.app.core.OnboardingHost
import com.fauna.app.ui.util.getStringFmt
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiAdminRegionView
import com.fauna.ffi.FfiIssuerForcedArm
import com.fauna.ffi.FfiIssuerForcedConfirmView
import com.fauna.ffi.FfiIssuerKeyView
import com.fauna.ffi.FfiSeedRotationConfirmView
import com.fauna.ffi.onboarding.AdminNatModeMachine
import com.fauna.ffi.onboarding.NatModeState
import com.fauna.ffi.onboarding.qualifyReclaimHandle
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.NodeMode
import uniffi.fauna_launch_machine.LaunchPersistence
import uniffi.fauna_launch_machine.mintAndPersistPendingFactoryReset
import javax.inject.Inject

/**
 * View-model for the `admin-nest` page (admin.md § N Nest) — the home for the
 * nest-wide admin settings that aren't a feature page, introduced by the
 * per-page-services redesign (2026-06-04, admin.md § Admin IA redesign):
 *
 *  - the admin **pairing** policy toggle (`fauna.admin.services.{list,update}`
 *    name `pairing`, the one live service flag that gates `fauna.pair.add` — moved
 *    off the removed Services page; reflectively re-read after a write),
 *  - the **NAT-mode** control (`fauna.setup.nat_mode` via the shared
 *    `AdminNatModeMachine` — the post-onboarding change surface for the NAT
 *    axis the wizard's `nat_mode_choice` page confirms once at claim), and
 *  - the **Factory Reset** danger zone (`fauna.admin.factory_reset` — moved off
 *    Settings).
 *
 * Per priority #2 this view-model holds no nest logic; it drives the shared
 * `fauna.*` WS-RPC kinds through [ApiClient] (never the deleted `/admin/api`
 * HTTP twins) and mirrors them into StateFlows. Mirrors the Linux reference
 * `apps/fauna-linux/src/views/admin.rs` `build_nest_page` (+ `client.rs::factory_reset`).
 */
@HiltViewModel
class AdminNestVM @Inject constructor(
    private val api: ApiClient,
    private val host: OnboardingHost,
    private val sessionAccount: SessionAccount,
    /**
     * The same `LaunchPersistence` the LaunchMachine branches on at launch (Hilt
     * binds one singleton over [SessionAccount]) — so the pending-factory-reset row
     * this page writes pre-dispatch is byte-for-byte the row the relaunch routes on.
     * Never write that slot directly; go through `mintAndPersistPendingFactoryReset`.
     */
    private val persistence: LaunchPersistence,
    /**
     * The ONE canonical actor-scoped drop (`account-scoping.md` § the in-memory
     * corollary). The factory reset used to call `api.clearAuth()` alone, which
     * ran none of the registered closers — one of the two sites that made
     * android's two funnels diverge.
     */
    private val actorScope: com.fauna.app.core.ActorScope,
    /** For the localized factory-reset error strings (the DevicesVM pattern). */
    @ApplicationContext private val appContext: Context,
) : ViewModel() {

    /** Admin pairing policy flag — backs `admin-service-pairing-toggle`. */
    val pairing = MutableStateFlow(false)

    /**
     * Deployment client-facing API serving port (`fauna.setup.status` reply
     * `serving_port`, default 443) — seeds `admin-nest-serving-port-input`. The
     * symmetric twin of the CalDAV port (admin-calendar), but a `fauna.admin.*`
     * call with no shared policy machine.
     */
    val servingPort = MutableStateFlow(443)

    /**
     * `fauna.setup.status` `fronted_by_router` — true behind the cloud :443 SNI
     * router, where the chosen port is inert (the nest rejects a write). The
     * screen renders `admin-nest-serving-port-input` read-only when true; editable
     * only on a direct listener (the default). nest/common.md § Serving ports.
     */
    val frontedByRouter = MutableStateFlow(false)

    /**
     * Host-OS maintenance (installers/vps.md § Host OS Maintenance § 4), read off
     * the same `fauna.setup.status` fetch the serving port uses. `osSecurityUpdates`
     * backs the `nest-os-updates-count` badge (rendered only when `> 0`);
     * `osRebootPending` backs the `nest-os-restart-now-button` (rendered only when
     * true). Both feed the always-present status line through the shared
     * `os_maintenance_status_label`. Defaults 0/false → "OS up to date" on a nest
     * with no host maintenance channel (dev/desktop nest), no false alarm.
     */
    val osSecurityUpdates = MutableStateFlow(0)
    val osRebootPending = MutableStateFlow(false)

    /** Disables the restart-now button mid-dispatch (re-tap guard). */
    val restartingNow = MutableStateFlow(false)

    /** Disables the pairing toggle + serving-port save mid-write (re-dispatch guard). */
    val working = MutableStateFlow(false)

    /** Page-scoped error surface (`error-message`): pairing + factory-reset fail here. */
    val error = MutableStateFlow<String?>(null)

    /**
     * Declared region (`admin-nest-region-*`, `fauna.admin.region.{get,set}`)
     * — the deployment's legal situs, the region tier's one human choice
     * (region-blocking.md § Region determination; dynamic-features.md § The
     * region tier). Every rendering decision is the shared
     * `FfiAdminRegionView` fold (tui's `admin/nest.rs`, the reference leg) —
     * this VM decides nothing about the plane. `null` before the first read
     * resolves; the screen renders the fresh-install defaults meanwhile
     * (mirrors linux, which seeds `admin-nest-region-status` to
     * `REGION_NONE` before its own first read lands).
     */
    val regionView = MutableStateFlow<FfiAdminRegionView?>(null)

    /** Disables the region save/withdraw buttons mid-dispatch (re-tap guard). */
    val regionWorking = MutableStateFlow(false)

    /**
     * Shared admin NAT-mode control (admin.md § Nest → NAT-mode control) — the
     * post-onboarding change surface for the NAT axis the wizard's
     * `nat_mode_choice` page confirms once at claim. Rides the pre-identity
     * WS-RPC transport over the raw `(nest_url, secret_hex)` session pair (the
     * payload signature is the authorization, not this client's bearer
     * connection) — mirrors linux `client.rs::admin_nat_mode_machine` / web
     * `createAdminNatModeMachine`. Null when the session pair isn't cached
     * (shouldn't happen on an authenticated admin page); the control then
     * simply never renders live state, degrading gracefully rather than
     * crashing.
     */
    private val natMachine: AdminNatModeMachine? =
        sessionAccount.nestUrl?.let { url ->
            sessionAccount.secretHex?.let { secret -> AdminNatModeMachine(url, secret) }
        }

    /** `NatModeSnapshot` mirrors — back `admin-nest-nat-mode-*`. */
    val natSelectedMode = MutableStateFlow(NodeMode.PUBLIC)
    val natMessage = MutableStateFlow<LocalizedText?>(null)
    val natSubmitEnabled = MutableStateFlow(false)
    val natSubmitting = MutableStateFlow(false)

    /**
     * The deployment-identity rotation ceremony's arm state (`admin-nest-seed-rotate-*`) — `null` = un-armed (the section shows only
     * the arm button). Mirrors linux `views/admin.rs`'s
     * `SeedRotateConfirmState` three-state shape exactly:
     *  - [SeedRotateConfirmState.Loading] — armed, roster read in flight
     *  - [SeedRotateConfirmState.Failed] — roster read errored
     *  - [SeedRotateConfirmState.Ready] — roster resolved, `view.canConfirm`
     *    gates the confirm button and `view.blockedReason` explains a `false`
     *
     * Set to [SeedRotateConfirmState.Loading] SYNCHRONOUSLY by [armSeedRotate]
     * — before the coroutine's suspend call, never after — because the e2e
     * journey (`test_nest_rotation_admin_journey.py`) asserts the confirm
     * surface exists (disabled) in the same frame the arm click's reply
     * returns, matching linux's synchronous paint-before-dispatch.
     */
    val seedRotateConfirm = MutableStateFlow<SeedRotateConfirmState?>(null)

    /** The ceremony's one-sentence verdict (`admin-nest-seed-rotate-status`),
     *  present only after a confirm attempt. */
    val seedRotateStatus = MutableStateFlow<String?>(null)

    sealed class SeedRotateConfirmState {
        object Loading : SeedRotateConfirmState()
        data class Failed(val message: String) : SeedRotateConfirmState()
        data class Ready(val view: FfiSeedRotationConfirmView) : SeedRotateConfirmState()
    }

    // ── Outside-app sign-in keys (`admin-nest-oauth-*`; authorization-server.md
    // § The issuer → Two rotation arms, admin.md § N Nest). The nest-held OAuth
    // issuer key set and its refresh-token secret. Every sentence — a key
    // row's line, the ordinary arm's cost, the armed confirm, all three
    // verdicts — is a shared `fauna_client_admin` fold reached through the
    // UniFFI face (`libs/fauna-ffi/src/admin.rs`); this view-model holds the
    // section's state and wires the four gestures, deciding nothing. Mirrors
    // tui's `admin/mod.rs` (the lead app, the reference leg): `OauthKeysRead`,
    // `ArmedOauthForced`, `oauth_{confirm,status,in_flight}`.

    /** The key set's read — the ONE precondition all three controls share.
     *  Key rows paint only from [Ready]: "not asked yet" ([Unread]) and
     *  "couldn't find out" ([Failed]) get the `admin-nest-oauth-key-reason`
     *  line instead, never an empty list that would read as "no keys". */
    sealed class OauthKeysRead {
        object Unread : OauthKeysRead()
        data class Ready(val view: FfiIssuerKeyView) : OauthKeysRead()
        /** [reason] is already worded (`admin.nest_page.oauth_keys_error`). */
        data class Failed(val reason: String) : OauthKeysRead()
    }

    /** The armed forced confirm: which arm, and its cost AS FOLDED WHEN IT WAS
     *  ARMED (`issuerForcedConfirmView` over the set the admin was looking at)
     *  — never re-folded while armed. One arm at a time. */
    data class OauthArmed(val arm: FfiIssuerForcedArm, val confirm: FfiIssuerForcedConfirmView)

    /**
     * The whole section's state, held in ONE value so every transition lands
     * as ONE StateFlow write — above all a dispatch's end, where the verdict,
     * the re-read key set and the released in-flight guard must appear
     * together (tui's single `Outcome::OauthDone { status, keys }`). Were they
     * three flows, a reader could see the verdict beside the pre-call rows —
     * and the cross-app journey reads the rows the moment the verdict stops
     * saying "Working…".
     */
    data class OauthSection(
        /** Loaded alongside the page's other reads, as its own independent
         *  state: a failed read fails nothing else on the page (any read error
         *  must still paint
         *  the rest). */
        val keys: OauthKeysRead = OauthKeysRead.Unread,
        /** `admin-nest-oauth-confirm-*` — `null` while un-armed. */
        val armed: OauthArmed? = null,
        /** `admin-nest-oauth-status` — the last control's verdict, `null` until
         *  a control was used. Deliberately never the page's `error-message`:
         *  every success here has consequences worth words, and a failure must
         *  not claim nothing changed. */
        val status: String? = null,
        /** Whether an issuer control's call is in flight. Every one of the three
         *  kinds mints on the nest, so all three controls desensitize while it
         *  is (the ordinary arm has no confirm to disarm). */
        val inFlight: Boolean = false,
    )

    val oauth = MutableStateFlow(OauthSection())

    // ── Legal takedown (`admin-nest-takedown-*`; moderation.md
    // § Legal takedown → Invocation surface, ruled 2026-08-16). Every gating/wording decision is the shared
    // `fauna_client_moderation::takedown` fold
    // ([com.fauna.ffi.takedownFormView] / [com.fauna.ffi.takedownVerdict]) —
    // this view-model paints and wires; nothing here decides. The draft
    // fields stay LOCAL Compose state inside `TakedownSection` (the
    // servingPort-edit pattern below) — nothing else on the page observes
    // them, so lifting them here would buy nothing. Mirrors linux's
    // `build_nest_page` (the reference leg).

    /** The armed confirm's captured form + fold — `null` while un-armed.
     *  Captured at arm time and never re-derived while armed (mirrors
     *  [seedRotateConfirm]'s discipline): a field the admin keeps typing
     *  after arming cannot silently change what the confirm named. Cleared
     *  (disarm-before-dispatch) by [confirmTakedown] before dispatch, so a
     *  double click cannot dispatch a second compulsory act. */
    data class TakedownArmed(
        val contentId: String,
        val conversation: Boolean,
        val legalReference: String,
        val restore: Boolean,
        val view: com.fauna.ffi.FfiTakedownFormView,
    )
    val takedownArmed = MutableStateFlow<TakedownArmed?>(null)

    /** The dispatch's own verdict (`admin-nest-takedown-status`), present
     *  only after an attempt. Not an error banner: success is the common
     *  case and the two verbs earn different sentences. */
    val takedownStatus = MutableStateFlow<String?>(null)

    /** Arm — capture the form + the fold `TakedownSection` already computed,
     *  so the confirm names exactly what a later dispatch will send even if
     *  the admin keeps typing. A driver-forced call with a refused fold is a
     *  no-op (the button renders disabled with the fold's stated reason). */
    fun armTakedown(
        contentId: String,
        conversation: Boolean,
        legalReference: String,
        restore: Boolean,
        view: com.fauna.ffi.FfiTakedownFormView,
    ) {
        if (!view.canSubmit) return
        takedownStatus.value = null
        takedownArmed.value = TakedownArmed(contentId, conversation, legalReference, restore, view)
    }

    fun cancelTakedown() {
        takedownArmed.value = null
    }

    fun confirmTakedown() {
        val armed = takedownArmed.value ?: return
        takedownArmed.value = null
        takedownStatus.value = appContext.getString(R.string.admin_nest_page_takedown_working)
        viewModelScope.launch {
            val result = runCatching {
                api.legalTakedown(armed.contentId, armed.conversation, armed.legalReference, armed.restore)
            }
            val verdict = com.fauna.ffi.takedownVerdict(armed.restore, result.exceptionOrNull()?.message)
            takedownStatus.value = resolveLocalized(appContext, verdict)
        }
    }

    init {
        hydrate()
        hydrateNatMode()
        hydrateRegion()
        hydrateOauthKeys()
    }

    /** Re-render the three NAT StateFlows from the machine's current snapshot
     *  (dispatch-style: view awaits an action, then re-reads — the
     *  `nat_mode_choice.rs` idiom, mirrored from linux/web). */
    private fun renderNatSnapshot() {
        val snap = natMachine?.snapshot() ?: return
        natSelectedMode.value = snap.selectedMode
        natMessage.value = snap.message
        natSubmitEnabled.value = snap.submitEnabled
        natSubmitting.value = snap.state is NatModeState.Submitting
    }

    /** Page load: read `fauna.setup.status` and pre-select the current
     *  `node_mode` (absent ⇒ public). Called once at VM creation, mirroring
     *  linux's `outer.connect_map` / web's `onMount` page-show hydrate. */
    private fun hydrateNatMode() {
        viewModelScope.launch {
            natMachine?.hydrate()
            renderNatSnapshot()
        }
    }

    /** Radio click (`admin-nest-nat-mode-{public,private}-radio`). Synchronous
     *  like linux/web — no network call, just a local state flip. */
    fun selectNatMode(mode: NodeMode) {
        natMachine?.select(mode)
        renderNatSnapshot()
    }

    /** Save (`admin-nest-nat-mode-save-button`): sign + commit the selected
     *  mode via the mutable `fauna.setup.nat_mode`. Save stays enabled after
     *  success — the set is mutable and an immediate re-flip is allowed. */
    fun saveNatMode() {
        viewModelScope.launch {
            natMachine?.submit()
            renderNatSnapshot()
        }
    }

    /** Load the admin pairing flag + serving port. `adminServicesList` is a
     *  single NestClient RPC — the transport already parks it while the
     *  socket comes up (transport.md § Request lifecycle step 3). */
    private fun hydrate() {
        viewModelScope.launch {
            try {
                pairing.value = api.adminServicesList().pairing
                // Serving port + OS-maintenance fields are best-effort reads
                // off the same anonymous setup-status; a failure leaves them
                // at their defaults rather than failing the whole hydrate.
                runCatching {
                    val status = api.nestSetupStatus()
                    servingPort.value = status.servingPort.toInt()
                    frontedByRouter.value = status.frontedByRouter
                    osSecurityUpdates.value = status.osSecurityUpdatesPending.toInt()
                    osRebootPending.value = status.osRebootPending
                }
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that
                // has not landed yet (NestClient::request_inner).
            }
        }
    }

    /** Set the client-facing API serving port over `fauna.admin.set_serving_port`,
     *  then re-read `setup.status` so the field reflects the persisted value
     *  (proving the write landed). `port` is a pre-validated u16 in [1, 65535]
     *  (the screen range-checks before calling). The new port binds on the next
     *  nest restart. Mirrors [setPairing]'s reflective write-then-reread. */
    fun setServingPort(port: Int) {
        viewModelScope.launch {
            working.value = true
            try {
                api.adminSetServingPort(port.toUShort())
                servingPort.value = api.nestSetupStatus().servingPort.toInt()
                error.value = null
            } catch (e: Exception) {
                error.value = e.message
            } finally {
                working.value = false
            }
        }
    }

    /** Surface a client-side invalid-port message on the page `error-message`
     *  (an out-of-range u16 is rejected before any dispatch — the screen passes
     *  the localized string). Mirrors the linux/web invalid-port handling. */
    fun reportInvalidServingPort(message: String) {
        error.value = message
    }

    /** Page load: read `fauna.admin.region.get`, folded through the shared
     *  `admin_region_view` — every rendering decision lives there
     *  (region-blocking.md § Region determination; tui `admin/nest.rs`, the
     *  reference leg). Best-effort like the serving-port/OS-maintenance reads
     *  in [hydrate]: a failure leaves the section at its fresh-install
     *  defaults rather than failing the whole page load. Called once at VM
     *  creation, mirroring [hydrateNatMode]. */
    private fun hydrateRegion() {
        viewModelScope.launch {
            runCatching { api.adminRegionStatus() }.onSuccess { regionView.value = it }
        }
    }

    /** Declare (`admin-nest-region-save-button`, `region` already validated
     *  by the screen via `com.fauna.ffi.adminParseRegionCode`) or withdraw
     *  (`admin-nest-region-withdraw-button`, `region = null`) — both
     *  `fauna.admin.region.set`. Re-reads on success so the section re-seeds
     *  from the persisted declaration (a re-declaration also retires the
     *  previous region's feature-policy document nest-side). Mirrors
     *  [setServingPort]'s reflective write-then-reread. */
    fun setRegion(region: String?) {
        viewModelScope.launch {
            regionWorking.value = true
            try {
                api.adminSetRegion(region)
                runCatching { api.adminRegionStatus() }.onSuccess { regionView.value = it }
                error.value = null
            } catch (e: Exception) {
                error.value = e.message
            } finally {
                regionWorking.value = false
            }
        }
    }

    /** Surface a client-side invalid-region-code message on the page
     *  `error-message` (a malformed code is rejected before any dispatch —
     *  the screen passes the localized string, since the shared validator's
     *  `Err` carries its own i18n KEY, not a display sentence — same
     *  discipline as [reportInvalidServingPort]). Mirrors linux/tui, which
     *  both render their own local `REGION_INVALID` resource and discard the
     *  `Err` payload outright. */
    fun reportInvalidRegion(message: String) {
        error.value = message
    }

    /** Expedite the host's idle-gated reboot over `fauna.admin.request_host_restart`
     *  (the "restart now" affordance), then re-read `setup.status` so the indicator
     *  reflects any change. Rejected (`no_host`) on a nest with no maintenance mount
     *  → surfaced on the page `error-message`. Mirrors web `restartNow` / linux
     *  `client.rs::request_host_restart`. */
    fun restartNow() {
        viewModelScope.launch {
            restartingNow.value = true
            error.value = null
            try {
                api.adminRequestHostRestart()
                val status = api.nestSetupStatus()
                osSecurityUpdates.value = status.osSecurityUpdatesPending.toInt()
                osRebootPending.value = status.osRebootPending
            } catch (e: Exception) {
                error.value = e.message
            } finally {
                restartingNow.value = false
            }
        }
    }

    /** Flip the admin `pairing` flag, then re-read so the toggle reflects the
     *  persisted state (matches Linux's reflective sync). */
    fun setPairing(enabled: Boolean) {
        viewModelScope.launch {
            working.value = true
            try {
                api.adminServicesUpdate("pairing", enabled)
                pairing.value = api.adminServicesList().pairing
                error.value = null
            } catch (e: Exception) {
                error.value = e.message
            } finally {
                working.value = false
            }
        }
    }

    /**
     * Factory-reset this nest (mail-bridge-lifecycle.md § Factory reset). The nest
     * exits + restarts into the wipe right after replying, so the post-reset claim
     * code must survive a client death at any point — gap CR-1
     * (`nest/common.md` § Client-state recoverability).
     *
     * Ordering is the whole fix: **mint + durably persist the code, then dispatch
     * with it pinned.** The shared `mintAndPersistPendingFactoryReset` returns the
     * code only once the `(nest_url, handle, claim_code)` row is in the store, so
     * the crash-unsafe ordering (learn the code from the reply, persist after) is
     * unrepresentable here. If this client dies anywhere after that call, the
     * relaunch finds the slot and the LaunchMachine routes
     * `PENDING_FACTORY_RESET` → the pre-filled claim (see [AppLaunchVM.navTargetFor]).
     *
     * The slot carries the handle re-qualified to `<handle>@<domain>` (so the
     * re-claim re-registers the primary mail domain — else the bridge idles and mail
     * silently breaks). On success we tear down the authenticated session **keeping local
     * credentials** (the box was wiped, not the client), and invoke [onComplete] to
     * flip the app into onboarding. The in-memory latch is kept as the same-process
     * fast path, but the SLOT is now the source of truth for the code.
     */
    fun factoryReset(onComplete: () -> Unit) {
        viewModelScope.launch {
            error.value = null
            try {
                // Source the bare handle AUTHORITATIVELY from the still-live admin
                // session (`fauna.account.get`) before the wipe, falling back to
                // the cache — the keyring can be empty/locked at reset time, and a
                // re-claim now *requires* a non-empty handle. Mirrors Linux
                // `client.rs::factory_reset`.
                val authoritative = runCatching { api.accountGet().handle }
                    .getOrNull()?.takeIf { it.isNotEmpty() }
                val bareHandle = authoritative ?: sessionAccount.handle.orEmpty()
                // The shared `qualifyReclaimHandle` does the bare→`@domain`
                // qualification (and the nest-URL-host parse) once for all six
                // apps (priority #2) — no per-app hand-rolled helper. Compute
                // it BEFORE the mint so the slot's handle is the one the re-claim
                // will actually use.
                val nestUrl = sessionAccount.nestUrl.orEmpty()
                val qualifiedHandle = qualifyReclaimHandle(
                    bareHandle,
                    sessionAccount.domain,
                    nestUrl,
                )

                // CR-1: mint + persist BEFORE dispatching, then pin. The shared helper
                // writes the row, READS IT BACK, and returns null if it did not land
                // (EncryptedSharedPreferences.commit() can fail on a full disk and the
                // store swallows it — "saved" is a claim to verify, not to trust).
                val pinnedCode = mintAndPersistPendingFactoryReset(
                    persistence,
                    nestUrl,
                    qualifiedHandle,
                )
                if (pinnedCode == null) {
                    // Do NOT dispatch. A reset whose code we failed to persist wipes
                    // the box against a code nobody holds — CR-1 again, and worse,
                    // because the client would believe it was safe. Refusing to start
                    // is always recoverable; the nest is untouched.
                    error.value = appContext.getString(
                        R.string.admin_settings_page_factory_reset_persist_failed
                    )
                    return@launch
                }
                // The returned code is durable, so a SIGKILL from here on lands in a
                // state a relaunch can recover. The nest honors a pinned code verbatim,
                // so this is the code already in the slot; take it from the reply anyway
                // so the nest stays the authority on what the box booted with.
                val code = api.factoryReset(pinnedCode)

                // The re-qualified handle rides in the pending-factory-reset slot
                // itself, and the launch flow's PENDING_FACTORY_RESET row (evaluated
                // before every other) seeds the re-claim from that slot, so the
                // @domain survives without touching the account's handle cache
                // (apple's `AdminNestVM.factoryReset` shape).
                host.pendingFactoryResetClaimCode = code
                // The box was wiped and this session is over: drop everything
                // scoped to it through the one canonical door, not just the
                // session rails. The custodian push loop in particular held a
                // host bound to the now-wiped nest.
                actorScope.dropActorScopedState()
                onComplete()
            } catch (e: Exception) {
                // The box is untouched on a failed reset.
                error.value = e.message
            }
        }
    }

    /**
     * Arm the deployment-identity rotation ceremony (`admin-nest-seed-rotate-button`):
     * paint [SeedRotateConfirmState.Loading] synchronously, then read the
     * roster. Mirrors linux `views/admin.rs`'s arm-click handler — the
     * synchronous paint is load-bearing (see [seedRotateConfirm]'s doc).
     */
    fun armSeedRotate() {
        seedRotateConfirm.value = SeedRotateConfirmState.Loading
        viewModelScope.launch {
            val result = runCatching { api.seedRotateRoster() }
            // A late reply after cancel must not silently re-arm — mirrors
            // linux `set_seed_rotate_roster`'s `if borrow().is_none() { return }`.
            if (seedRotateConfirm.value !is SeedRotateConfirmState.Loading) return@launch
            seedRotateConfirm.value = result.fold(
                onSuccess = { SeedRotateConfirmState.Ready(it) },
                onFailure = { SeedRotateConfirmState.Failed(it.message ?: it.toString()) },
            )
        }
    }

    /** Disarm (`admin-nest-seed-rotate-cancel-button`): drop back to un-armed. */
    fun cancelSeedRotate() {
        seedRotateConfirm.value = null
    }

    /**
     * Confirm the ceremony (`admin-nest-seed-rotate-confirm-button`): disarm
     * SYNCHRONOUSLY before dispatch (double-click guard — a second tap during
     * the in-flight rotation must not chain a second one), then rotate and
     * paint the verdict. There is no fan-out afterwards: the plane drive
     * publishes the successor's custody row before the dispatch
     * (`box-recovery.md` § The plane-era recovery floor, *(c) The writes*).
     */
    fun confirmSeedRotate() {
        val armed = seedRotateConfirm.value
        seedRotateConfirm.value = null
        if (armed !is SeedRotateConfirmState.Ready || !armed.view.canConfirm) return
        seedRotateStatus.value = appContext.getString(R.string.admin_nest_page_rotate_seed_working)
        viewModelScope.launch {
            try {
                val result = api.rotateDeploymentSeed()
                seedRotateStatus.value = resolveLocalized(appContext, result.verdict)
            } catch (e: Exception) {
                seedRotateStatus.value = e.message
            }
        }
    }

    // ── Outside-app sign-in keys (`admin-nest-oauth-*`) ───────────────────────
    // Every guard below is checked and every state change written
    // SYNCHRONOUSLY, before the coroutine's first suspension, so a second tap
    // can never slip between a check and its write (the seed-rotate
    // discipline above; `viewModelScope` is `Main.immediate`, and every caller
    // is a click handler on the main thread). Each transition is ONE write of
    // [oauth] — see [OauthSection].

    /** The key set, once it has answered — `null` otherwise. */
    private fun OauthSection.answeredView(): FfiIssuerKeyView? = (keys as? OauthKeysRead.Ready)?.view

    /** `fauna.oauth.issuer_key_status`, read and folded — or the worded reason
     *  it could not be (`admin.nest_page.oauth_keys_error`, `{cause}` = the
     *  error). Never throws: a failure is a [OauthKeysRead.Failed] for the
     *  section's own reason line. */
    private suspend fun readOauthKeys(): OauthKeysRead =
        try {
            OauthKeysRead.Ready(api.adminIssuerKeyStatus())
        } catch (e: Exception) {
            OauthKeysRead.Failed(
                appContext.getStringFmt(
                    R.string.admin_nest_page_oauth_keys_error,
                    e.message ?: e.toString(),
                )
            )
        }

    /** Page load — alongside [hydrateRegion], as its own independent state. */
    private fun hydrateOauthKeys() {
        viewModelScope.launch {
            val keys = readOauthKeys()
            oauth.update { it.copy(keys = keys) }
        }
    }

    /**
     * A dispatch's shared tail: re-read the key set FIRST (a failed re-read
     * becomes the reason line), and only once it has returned publish the
     * verdict, the re-read set and the released in-flight guard as ONE write —
     * tui's `Outcome::OauthDone { status, keys }`. Until then the status keeps
     * saying "Working…", so a reader waiting for the verdict can never find it
     * beside the pre-call rows.
     */
    private suspend fun finishOauthCall(verdict: String) {
        val keys = readOauthKeys()
        oauth.update { it.copy(keys = keys, status = verdict, inFlight = false) }
    }

    /** Word a call that never reached the FFI face (no connection — the face
     *  itself never throws) as the section's own failure sentence, so the
     *  status never sticks at "Working…". */
    private fun oauthCallFailed(e: Exception): String =
        appContext.getStringFmt(
            R.string.admin_nest_page_oauth_rotate_failed,
            e.message ?: e.toString(),
        )

    /**
     * `admin-nest-oauth-rotate-button` — the ordinary rotation. No confirm:
     * nothing breaks (its cost is stated beside the button). A driver-forced
     * press honours the same two guards the button's `enabled` reads.
     */
    fun rotateIssuerKey() {
        val section = oauth.value
        if (section.inFlight || section.answeredView() == null) return
        // A forced confirm armed beside it named a key count this rotation is
        // about to change — disarm it rather than let it state a stale cost.
        oauth.value = section.copy(
            armed = null,
            inFlight = true,
            status = appContext.getString(R.string.admin_nest_page_oauth_working),
        )
        viewModelScope.launch {
            val verdict = try {
                resolveLocalized(appContext, api.adminRotateIssuerKey()).orEmpty()
            } catch (e: Exception) {
                oauthCallFailed(e)
            }
            finishOauthCall(verdict)
        }
    }

    /**
     * `admin-nest-oauth-force-rotate-button` / `-secret-force-rotate-button` —
     * arm the one shared inline confirm. Folds the confirm NOW, over the set the
     * admin is looking at, and dispatches nothing. Refused before the set has
     * answered (the confirm could not name what it drops) and while a call is
     * in flight.
     */
    fun armOauthForced(arm: FfiIssuerForcedArm) {
        val section = oauth.value
        if (section.inFlight) return
        val view = section.answeredView() ?: return
        val confirm = com.fauna.ffi.issuerForcedConfirmView(arm, view)
        oauth.value = section.copy(status = null, armed = OauthArmed(arm, confirm))
    }

    /** `admin-nest-oauth-cancel-button` — disarm, dispatching nothing. */
    fun cancelOauthForced() {
        oauth.update { it.copy(armed = null) }
    }

    /**
     * `admin-nest-oauth-confirm-button` — dispatch exactly the armed arm.
     * [arm] is the arm the pressed confirm was RENDERED for: a mismatch (or a
     * call already in flight) dispatches nothing and keeps what the admin can
     * see. Otherwise disarm FIRST — a double press must not dispatch a second
     * forced rotation, which would drop the very key the first minted.
     */
    fun confirmOauthForced(arm: FfiIssuerForcedArm) {
        val section = oauth.value
        val armed = section.armed ?: return
        if (armed.arm != arm || section.inFlight) return
        oauth.value = section.copy(
            armed = null,
            inFlight = true,
            status = appContext.getString(R.string.admin_nest_page_oauth_working),
        )
        viewModelScope.launch {
            val verdict = try {
                resolveLocalized(appContext, api.adminForceRotateIssuer(armed.arm)).orEmpty()
            } catch (e: Exception) {
                oauthCallFailed(e)
            }
            finishOauthCall(verdict)
        }
    }
}
