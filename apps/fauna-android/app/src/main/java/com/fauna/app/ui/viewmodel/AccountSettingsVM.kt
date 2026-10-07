package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ActorScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.HexUtil
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.SignOutCredentialEraser
import com.fauna.app.widget.WidgetDataWorker
import com.fauna.ffi.FfiAccountEntry
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.FfiFeatureRow
import com.fauna.ffi.FfiPendingActionSummary
import com.fauna.ffi.FfiQuotaGetReply
import com.fauna.ffi.accountDisplayLabel
import com.fauna.ffi.onboarding.WizardOutcome
import com.fauna.ffi.actorIdFromSecret
import com.fauna.ffi.validateHandle
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class AccountSettingsVM @Inject constructor(
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
    private val registry: FfiAccountRegistry,
    private val onboardingHost: OnboardingHost,
    // The ONE canonical actor-scoped drop (account-scoping.md § the in-memory
    // corollary). Every teardown path below calls it and keeps no list of its
    // own.
    private val actorScope: ActorScope,
    // Owns the account-scoped content stores; sign-out / delete-account erase
    // them alongside the credentials (account-scoping.md § Erasure follows scope).
    private val accountStores: AccountStores,
    // For the widget re-fold on switch/sign-out only.
    @ApplicationContext private val context: Context,
) : ViewModel() {

    val newHandle = MutableStateFlow("")
    val changingHandle = MutableStateFlow(false)
    val changeHandleError = MutableStateFlow<String?>(null)
    val changeHandleSuccess = MutableStateFlow(false)
    val exporting = MutableStateFlow(false)
    val exportError = MutableStateFlow<String?>(null)
    val errorMessage = MutableStateFlow<String?>(null)
    // Transient toast for [deleteAccount] — the pending-actions section below
    // (`settings.md` § Pending actions) is the STANDING receipt; this is only
    // the immediate "it worked" confirmation, same role changeHandleSuccess
    // plays for the sibling verb.
    val deleteAccountSuccess = MutableStateFlow(false)

    val quota = MutableStateFlow<FfiQuotaGetReply?>(null)

    // Pending actions (`settings.md` § Pending actions) — the STANDING
    // account-page section listing the cancellable window the three delayed
    // verbs (handle change, account delete, snapshot delete) open. tui/linux/
    // web shipped this first; mirrors their shape. `null` = not yet hydrated
    // (bare title); `emptyList()` = hydrated and empty; non-empty = counted
    // title with rows — never a settled "nothing scheduled" claim before the
    // first list read lands.
    val pendingActions = MutableStateFlow<List<FfiPendingActionSummary>?>(null)
    val pendingActionsError = MutableStateFlow<String?>(null)

    // `feature-limits-section` — null (not `emptyList()`) is the "read hasn't
    // resolved yet" state, so the screen renders nothing until the real
    // `fauna.features.status` reply arrives (`settings.md` § Layout & flow
    // item 2b: "renders only once the transparency read resolves").
    val features = MutableStateFlow<List<FfiFeatureRow>?>(null)

    // ── Multi-account switcher (long-term-store.md § Multi-account evolution,
    // Stage 1) ── Holds no identity state of its own beyond a snapshot of the
    // registry read: every mutation re-reads via [reloadAccounts], mirroring
    // apple's AccountSwitcherVM (the registry, not a cache, is the authority).
    val accounts = MutableStateFlow<List<FfiAccountEntry>>(emptyList())
    val activeActorId = MutableStateFlow<String?>(null)
    val switchError = MutableStateFlow<String?>(null)

    init {
        loadQuota()
        loadFeatures()
        loadPendingActions()
        reloadAccounts()
        // Re-fetch account quota on each WS reconnect (transport.md § Push
        // events), part of the linux WsEvent::Reconnected re-fetch set.
        viewModelScope.launch {
            api.reconnectTick.collect {
                loadQuota()
                loadFeatures()
            }
        }
    }

    /** Re-read the registry — called on init and after every add/remove/switch, so
     *  the switcher list-refreshes in place with no re-navigation. */
    fun reloadAccounts() {
        accounts.value = registry.list()
        activeActorId.value = registry.active()
    }

    fun isActive(entry: FfiAccountEntry): Boolean = entry.actorId == activeActorId.value

    /** Whether activating `actorId` requires a re-auth confirmation
     *  (long-term-store.md § Multi-account evolution → Per-account re-auth). Read
     *  fresh from the last [reloadAccounts] snapshot — the screen re-reads on
     *  appear, so a flag the admin auto-default set is visible before the tap. */
    fun requiresConfirm(actorId: String): Boolean =
        accounts.value.firstOrNull { it.actorId == actorId }?.requireConfirmToActivate ?: false

    /** Flip a row's require-confirm-to-activate flag (the `account-require-confirm-toggle`
     *  write path). Marks the flag user-set registry-side (`set_require_confirm`),
     *  so the admin auto-default never overrides an explicit choice afterwards.
     *  Setting the flag never prompts — only activating a flagged account does. */
    fun setRequireConfirm(actorId: String, require: Boolean) {
        switchError.value = null
        try {
            registry.setRequireConfirm(actorId, require)
        } catch (e: Exception) {
            switchError.value = e.message
            return
        }
        reloadAccounts()
    }

    /** Row title — the shared FFI fn, never a hand-rolled "handle, else short actor
     *  id" fallback (linux/web once drifted on the empty-handle case;
     *  value-formatting.md § Account display label). */
    fun accountLabel(entry: FfiAccountEntry): String = accountDisplayLabel(entry.handle, entry.actorId)

    /**
     * Make `actorId` the active account and reconnect the live session as it — no
     * relaunch (long-term-store.md § Multi-account evolution, Decision 1).
     * Mutation-first (registry write before any teardown): a bad actor id bails
     * before anything is torn down. `onComplete` flips `appState.isOnboarding = true`
     * (the composable owns `AppState`, this VM does not), which re-enters the launch
     * routing and re-authenticates as the new account
     * ([AppLaunchVM.connectActiveSession]) — the same "no relaunch" reconnect apple/
     * linux implement natively, here via re-invoking the already-fresh-reading
     * `LaunchMachine`.
     *
     * `confirmed` selects the post-re-auth path: a require-confirm account
     * (`requiresConfirm`) is switched via `setActiveConfirmed` only AFTER the
     * caller's native re-auth prompt succeeded ([com.fauna.app.core.AccountReauth]);
     * an unflagged account uses plain `setActive`. `setActive` refuses a flagged
     * account registry-side (`ConfirmationRequired`) — the backstop for a caller
     * that forgot the gate (long-term-store.md § Per-account re-auth).
     */
    fun switchAccount(actorId: String, confirmed: Boolean, onComplete: () -> Unit) {
        if (actorId == activeActorId.value) return
        switchError.value = null
        try {
            if (confirmed) registry.setActiveConfirmed(actorId) else registry.setActive(actorId)
        } catch (e: Exception) {
            switchError.value = e.message
            return
        }
        // Drop everything scoped to the OUTGOING account — the session rails,
        // the process-wide handles opened against that account's FILES, and the
        // background loops that write them (account-scoping.md § the switch/
        // sign-out isolation contract, and § the in-memory corollary). Ordered
        // after the registry write, so the reopen below resolves under the
        // incoming account.
        actorScope.dropActorScopedState()
        // Class-4, wipe-tolerant: zero the widget's unread badge and let the
        // worker re-derive it for the incoming account, rather than leaving the
        // outgoing account's count on the home screen.
        WidgetDataWorker.refoldForAccountSwitch(context)
        reloadAccounts()
        onComplete()
    }

    /** Drop a NON-active account from this install (its per-actor slots + index
     *  entry) — offered only for non-active rows, matching linux/apple (removing
     *  the active one would need an immediate re-route, out of Stage-1 scope). No
     *  confirmation dialog: the identity itself is not destroyed, only forgotten
     *  here; re-adding it re-imports the same secret. */
    fun removeAccount(actorId: String) {
        switchError.value = null
        try {
            registry.remove(actorId)
        } catch (e: Exception) {
            switchError.value = e.message
            return
        }
        // ...and that actor's content stores, not only its credential slots
        // (account-scoping.md § Erasure follows scope — *removing one account
        // erases that actor's stores*). Only ever a non-active account here, so
        // this never touches the live session's own stores.
        accountStores.eraseAccount(actorId)
        reloadAccounts()
    }

    /** "Add account" entry — reset the shared onboarding machine so the append
     *  wizard starts fresh at identity-choice (the caller flips
     *  `appState.isAddingAccount = true` to mount it; long-term-store.md
     *  § Multi-account evolution). Same machine the cold-boot wizard drives —
     *  `OnboardingHost` is a process-wide Hilt singleton, so no new plumbing is
     *  needed for the append-mode screens to render off it. */
    fun beginAddAccount() {
        onboardingHost.machine.reset()
        onboardingHost.appendMode = true
    }

    /**
     * The append wizard's exit — the append terminal. The appended identity is
     * taken from the WIZARD MACHINE (`effectiveSecret()`) and the exit's own
     * [outcome] (its `LoggedIn` nest), never from the store: the append confirm
     * wrote nothing (`confirmIdentity(…, append = true)`, long-term-store.md
     * § Downgrade mirror + abandoned-append recovery), so until this call the
     * identity exists only in the machine. The device id is resolved for THIS
     * identity ([SecureStorage.deviceIdFor]), never the outgoing account's — two
     * accounts sharing one id is the linkage `sync-agent-credentials.md`
     * decision 5 forbids.
     * Register it in the registry — [FfiAccountRegistry.addAccount] does NOT
     * auto-activate past the first-ever account — then activate + reconnect as
     * it via the same [switchAccount] path "Add account" promises
     * ("then activates the new account", ui.yaml `account-add-button`).
     */
    fun completeAddAccount(outcome: WizardOutcome, onComplete: () -> Unit) {
        switchError.value = null
        val secretHex = onboardingHost.machine.effectiveSecret()
        if (secretHex.isNullOrEmpty()) {
            switchError.value = "Add account finished with no identity to register"
            return
        }
        val nestUrl = (outcome as? WizardOutcome.LoggedIn)?.nestUrl
        val newActorId = try {
            registry.addAccount(secretHex, nestUrl, secureStorage.deviceIdFor(secretHex))
        } catch (e: Exception) {
            switchError.value = e.message
            return
        }
        // A phrase restore's predecessor seeds, BEFORE the switch builds the
        // new session and linked to the added identity by name — it is not
        // active until the switch lands (`identity-succession.md` § Seed
        // escrow → *Restore path*; linux and FaunaKit: add → persist → switch).
        com.fauna.app.core.persistRestoredPredecessors(
            registry, newActorId, onboardingHost.machine.restoredPredecessors(),
        )
        // A freshly added account is never require-confirm-flagged (the flag
        // defaults off), so the append switch is the unconfirmed path — no re-auth.
        switchAccount(newActorId, confirmed = false, onComplete)
    }

    private fun loadQuota() {
        viewModelScope.launch {
            try {
                quota.value = api.fetchQuota()
            } catch (_: Exception) {
                // Quota display is non-critical
            }
        }
    }

    private fun loadFeatures() {
        viewModelScope.launch {
            try {
                features.value = api.fetchFeatures()
            } catch (_: Exception) {
                // Feature-limits display is non-critical, same as quota above.
            }
        }
    }

    /** Fired on init, and after either delayed verb this screen hosts
     *  (`changeHandle`/`deleteAccount`) completes, and after a cancel — never
     *  trust a stale read. */
    fun loadPendingActions() {
        viewModelScope.launch {
            try {
                pendingActions.value = api.pendingActionsList()
                pendingActionsError.value = null
            } catch (e: Exception) {
                // The section keeps its bare title on a failed read — no
                // basis for any other claim.
                pendingActionsError.value = e.message
            }
        }
    }

    /** `pending-action-cancel-button` — one click, no confirm: cancelling is
     *  the safe direction. Ends on a fresh list read, never a local removal,
     *  so the row count always reflects the nest's own state. */
    fun cancelPendingAction(id: Long) {
        viewModelScope.launch {
            try {
                api.pendingActionCancel(id)
                loadPendingActions()
            } catch (e: Exception) {
                pendingActionsError.value = e.message
            }
        }
    }

    fun actorId(): String? =
        secureStorage.secretHex?.let {
            try { HexUtil.bytesToHex(actorIdFromSecret(HexUtil.hexToBytes(it))) }
            catch (_: Exception) { null }
        }

    fun nodeUrl(): String? = api.nodeUrl.ifEmpty { null }

    fun deviceId(): String? = secureStorage.deviceId

    /**
     * The 64-hex identity secret, for the identity-export QR (settings.md § Identity
     * export). Read straight from the platform secure store, never from a settings
     * snapshot (architecture/apps/common.md § Credential storage).
     */
    fun secretHex(): String? = secureStorage.secretHex

    /**
     * The cached handle, ridden into the export QR's payload so a scanned import pre-fills
     * the handle step. Passed through as-is (matching linux), so a handle-less client
     * writes the bare secret form.
     */
    fun handle(): String? = secureStorage.handle?.ifBlank { null }

    fun changeHandle() {
        val handle = newHandle.value.trim()
        if (handle.isEmpty()) return
        // Client-side format validation via the shared canonical validator — the
        // SAME rules the nest enforces (`fauna_protocol::handle::validate_handle`),
        // so feedback is instant and identical across every app (settings.md
        // § Where logic lives → Handle change). A *taken* handle stays
        // server-authoritative and surfaces from the change RPC reply below.
        validateHandle(handle)?.let { msg ->
            changeHandleError.value = msg
            changeHandleSuccess.value = false
            return
        }
        viewModelScope.launch {
            changingHandle.value = true
            changeHandleError.value = null
            changeHandleSuccess.value = false
            try {
                api.changeHandle(handle)
                newHandle.value = ""
                changeHandleSuccess.value = true
                loadPendingActions()
            } catch (e: Exception) {
                changeHandleError.value = e.message
            }
            changingHandle.value = false
        }
    }

    suspend fun exportData(): ByteArray? {
        exporting.value = true
        exportError.value = null
        return try {
            api.exportData()
        } catch (e: Exception) {
            exportError.value = e.message
            null
        } finally {
            exporting.value = false
        }
    }

    /**
     * `onComplete` is handed the **sign-out residue** — `null` when the erase
     * left nothing behind, and otherwise what the `sign-out-residue` view on
     * `identity_choice` paints about data still on this device, already recorded
     * so it outlives the process (`account-scoping.md` § Erasure follows scope →
     * *the residue surface*). The caller carries it to the onboarding surface the
     * sign-out hands back: the erase is best-effort BY DESIGN — sign-out
     * completes even when a scope will not go — but proceeding must not look
     * identical to succeeding.
     */
    fun signOut(onComplete: (com.fauna.ffi.FfiSignOutResidue?) -> Unit) {
        viewModelScope.launch {
            // Await the shared account-runtime stop BEFORE anything below —
            // while [api]'s client still names the departing account
            // (`account-scoping.md` § Erasure follows scope, "that call
            // before its `account_state_erase_*`"). `clearAuth()` (inside
            // [ActorScope.dropActorScopedState] below) also fires the
            // switch-shaped stop fire-and-forget for every OTHER teardown
            // path, which is exactly the race the erase below must not run:
            // an in-flight or still-open account store is an unerasable store
            // (the ⚠ note this row closes), and only awaiting it here — not
            // spawning it — proves the ordering. Sign-out-shaped, not the
            // switch stop: the erase below takes this machine's account-store
            // slot (the writer key) with it, so this also retires the
            // enrollment nest-side first (`sync-agent-credentials.md` §
            // Credential model → *The signed-out reconcile*), matching
            // linux/tui/macOS/iOS/windows.
            api.stopAccountRuntimeForSignOutAwaited()

            // Erase every account's per-actor slots and the index
            // (long-term-store.md § Cleanup contract) — not just the active
            // account's. Delete-only, so a crash mid-clear cannot resurrect the
            // identity being erased.
            // `clearAll` reads back what it deleted — the only witness this seat
            // has, since the store's delete is best-effort — and `reverifyErase`
            // re-asks after `secureStorage.clear()`, the platform store's own
            // wholesale reset, so a key that reset took is not reported. Carried to
            // the residue below: dropping it painted a clean "Signed out" over
            // surviving credentials (`account-scoping.md` § Erasure follows scope).
            // The same sequence the residue retry re-runs ([SignOutCredentialEraser]).
            val credentials = SignOutCredentialEraser(registry, secureStorage).eraseCredentials()
            // Drop BEFORE the erase, not merely before returning: the drop is what
            // ends the custodian push loop and closes the sealed store it holds
            // open, and deleting that directory out from under a live handle is the
            // deleted-inode hazard AccountStores.closeOpenStores documents.
            actorScope.dropActorScopedState()
            // ...and every account-scoped CONTENT store, not only the credential
            // namespace (account-scoping.md § Erasure follows scope). Credentials
            // first: if the erase dies partway, the identity is already unreachable,
            // whereas the reverse order could leave readable content with live
            // credentials. Install-scoped state (logs, host-keyed nest pins) survives.
            // What the sweep could not remove goes to the log by path and to the
            // user by count, on the onboarding surface this hands them
            // (`principles.md` § The user always controls their data puts the delete
            // affordance in the app, and a log is not one). Every word of the line
            // is the shared projection's.
            val residue = accountStores.recordResidue(accountStores.eraseAllAccounts(), credentials)
            WidgetDataWorker.refoldForAccountSwitch(context)
            onComplete(residue)
        }
    }

    // Ruled 2026-08-26 (`settings.md` § Where logic lives → Account deletion):
    // `fauna.account.delete` only SCHEDULES a 14-day cancellable pending
    // action — auth stays untouched and every local content store stays
    // readable until it later executes (account-scoping.md § Erasure follows
    // scope binds at EXECUTION, never at request time), so unlike sign-out
    // there is nothing client-side to clear yet. No registry / secure-storage
    // / actor-scope clear, no store erase, no navigation: the user stays on
    // this page, signed in, until the pending-actions row's cancel button (or
    // the window closing) decides the outcome.
    fun deleteAccount() {
        viewModelScope.launch {
            try {
                api.deleteAccount()
                deleteAccountSuccess.value = true
                loadPendingActions()
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }
}
