package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.ShellLog
import com.fauna.ffi.FfiSessionsPage
import com.fauna.ffi.FfiSessionsSnapshot
import com.fauna.ffi.sessionsLockoutConfirmWord
import com.fauna.ffi.sessionsRevokeOthersPossibility
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.fauna_devices_machine.DevicesMachine
import uniffi.fauna_devices_machine.DevicesObserver

/**
 * The page's render state — everything [com.fauna.app.ui.screen.settings.SessionsContent]
 * paints. [snapshot] is the shared fold's answer verbatim (rows + page error);
 * the two confirm states ([revokeOthersArmed], [lockWord]) are the only state
 * this app owns (`ui/sessions.md` § Where logic lives — "app glue is the render,
 * the two confirm states").
 */
data class SessionsUiState(
    /** Null until the first hydrate lands — `sessions-empty` never paints before it. */
    val snapshot: FfiSessionsSnapshot? = null,
    val revokeOthersArmed: Boolean = false,
    val lockWord: String = "",
    val busy: Boolean = false,
    /**
     * `sessions-revoke-others-button`'s answer — the shared
     * `sessions_view::revoke_others_possibility` over [snapshot] and [busy]
     * (`ui/sessions.md` § Errors & edge cases), re-asked by the VM on every
     * state change. The arm and the confirm both ask it again. The default is
     * for the stateless harness only.
     */
    val revokeOthersAnswer: Possibility = Possibility.Possible,
    /** A press the answer refused, painted on `error-message` above the
     *  snapshot's own error; cleared by the next press that goes. */
    val refusal: LocalizedText? = null,
)

/** The revoke-others answer over [state]'s fold and in-flight act. */
internal fun revokeOthersAnswerOf(state: SessionsUiState): Possibility =
    sessionsRevokeOthersPossibility(state.snapshot, state.busy)

/**
 * Settings → Sessions (`docs/goal/ui/sessions.md`). Holds no fold, sort or
 * keep-the-painted-rows rule: every load and act is one call into the UniFFI
 * face (`libs/fauna-ffi/src/sessions.rs`), which runs the shared
 * `SessionsClient::pass` and answers the page to paint and whether the lock
 * landed. Hydrates on the nav edge and re-snapshots after each act — tui's
 * Devices / Mail / Privacy sub-page shape, linux's `settings/sessions.rs`.
 */
@HiltViewModel
class SessionsVM @Inject constructor(
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
) : ViewModel() {

    private val _state = MutableStateFlow(answered(SessionsUiState()))
    val state: StateFlow<SessionsUiState> = _state.asStateFlow()

    /** [s] with its revoke-others answer re-asked — every state write goes
     *  through [set], so the button never paints a stale answer. */
    private fun answered(s: SessionsUiState): SessionsUiState =
        s.copy(revokeOthersAnswer = revokeOthersAnswerOf(s))

    private fun set(transform: (SessionsUiState) -> SessionsUiState) =
        _state.update { answered(transform(it)) }

    /** Rule 6 at the arm and the confirm: ask the answer again; a refusal
     *  goes on `error-message` and nothing happens. `true` when refused. */
    private fun refusesRevokeOthers(): Boolean {
        val answer = revokeOthersAnswerOf(_state.value)
        if (answer is Possibility.Possible) return false
        set { it.copy(refusal = (answer as? Possibility.Blocked)?.reason, revokeOthersArmed = false) }
        return true
    }

    /** The lock's word, `LOCK`, from shared Rust — for the render's enable
     *  check only; the face re-checks what was typed in its action arm. */
    val lockConfirmWord: String by lazy { sessionsLockoutConfirmWord() }

    /** The page's Devices machine: names device-minted sessions (the shared
     *  fold's `principal` join) and owns the sealed-label render. Its ticks are
     *  ignored — the face refreshes it inside each pass. */
    private var devices: DevicesMachine? = null

    private val ownDeviceId: String? get() = secureStorage.resolveDeviceId()

    /** The four calls, a seam only so the VM tests need no live nest. */
    internal var load: suspend (DevicesMachine?, String?, FfiSessionsSnapshot?) -> FfiSessionsPage =
        api::sessionsLoad
    internal var revoke: suspend (DevicesMachine?, String?, FfiSessionsSnapshot?, String) -> FfiSessionsPage =
        api::sessionsRevoke
    internal var revokeOthers: suspend (DevicesMachine?, String?, FfiSessionsSnapshot?) -> FfiSessionsPage =
        api::sessionsRevokeOthers
    internal var lockout: suspend (DevicesMachine?, String?, FfiSessionsSnapshot?, String) -> FfiSessionsPage =
        api::sessionsLockout

    /** The nav-edge hydrate. */
    fun hydrate(onLocked: () -> Unit) = run(onLocked) { d, own, prev -> load(d, own, prev) }

    /** `session-revoke-button[i]` — no confirm (recoverable by re-auth). */
    fun revoke(tokenId: String, onLocked: () -> Unit) =
        run(onLocked) { d, own, prev -> revoke(d, own, prev, tokenId) }

    /** `sessions-revoke-others-button` — the first press only arms the confirm,
     *  and only when the answer says there is something to end: before the
     *  first read folds, with no other sign-in listed, or in flight, it
     *  refuses on `error-message` (`ui/sessions.md` § Errors & edge cases). */
    fun armRevokeOthers() {
        if (refusesRevokeOthers()) return
        set { it.copy(revokeOthersArmed = true, refusal = null) }
    }

    /** `sessions-revoke-others-cancel-button`. */
    fun cancelRevokeOthers() = set { it.copy(revokeOthersArmed = false) }

    /** `sessions-revoke-others-confirm-button` — the second press, which asks
     *  the same answer again before anything reaches the nest. */
    fun confirmRevokeOthers(onLocked: () -> Unit) {
        if (refusesRevokeOthers()) return
        set { it.copy(revokeOthersArmed = false, refusal = null) }
        run(onLocked) { d, own, prev -> revokeOthers(d, own, prev) }
    }

    fun onLockWordChange(value: String) = set { it.copy(lockWord = value) }

    /** `sessions-lockout-button` — the typed word goes to the face verbatim,
     *  which refuses a wrong one on `error-message` and reaches no nest. */
    fun lock(onLocked: () -> Unit) {
        val word = _state.value.lockWord
        run(onLocked) { d, own, prev -> lockout(d, own, prev, word) }
    }

    private fun run(
        onLocked: () -> Unit,
        call: suspend (DevicesMachine?, String?, FfiSessionsSnapshot?) -> FfiSessionsPage,
    ) {
        if (_state.value.busy) return
        set { it.copy(busy = true) }
        viewModelScope.launch {
            val page = try {
                call(ensureDevices(), ownDeviceId, _state.value.snapshot)
            } catch (e: Exception) {
                // Only "not connected" reaches here — every pass answers a page.
                ShellLog.w("SessionsVM", "[sessions] the pass could not start: ${e.message}")
                null
            }
            // A landed pass paints its own error; an earlier refusal is stale.
            set { it.copy(busy = false, snapshot = page?.snapshot ?: it.snapshot, refusal = null) }
            page?.snapshot?.let { SessionsTestState.paintedTokenIds = it.rows.map { r -> r.tokenId } }
            if (page?.locked == true) {
                // The lock landed: every bearer is revoked and the nest mints no
                // new one, so the app leaves the shell for its launch surface,
                // whose re-run challenge lands the locked notice (leg 2).
                ShellLog.w("SessionsVM", "[sessions] the account lock landed — leaving for the launch surface")
                set { it.copy(lockWord = "") }
                onLocked()
            }
        }
    }

    private fun ensureDevices(): DevicesMachine? {
        devices?.let { return it }
        return api.buildDevicesMachine(NoTicks)?.also { built ->
            built.setThisDeviceRow(ownDeviceId)
            devices = built
        }
    }

    override fun onCleared() {
        SessionsTestState.paintedTokenIds = null
        super.onCleared()
    }

    private object NoTicks : DevicesObserver {
        override fun onChanged() {}
    }
}

/**
 * The painted token-id list, published for the e2e agent's `settings` state
 * key (`session_token_ids` — `actions/sessions.py::painted_token_ids`), which
 * every app answers from the rows it painted. Null before the first hydrate.
 */
object SessionsTestState {
    @Volatile
    var paintedTokenIds: List<String>? = null
}
