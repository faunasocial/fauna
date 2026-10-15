package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.ShellLog
import com.fauna.ffi.FfiSessionsPage
import com.fauna.ffi.FfiSessionsSnapshot
import com.fauna.ffi.sessionsLockoutConfirmWord
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
)

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

    private val _state = MutableStateFlow(SessionsUiState())
    val state: StateFlow<SessionsUiState> = _state.asStateFlow()

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

    /** `sessions-revoke-others-button` — the first press only arms the confirm. */
    fun armRevokeOthers() = _state.update { it.copy(revokeOthersArmed = true) }

    /** `sessions-revoke-others-cancel-button`. */
    fun cancelRevokeOthers() = _state.update { it.copy(revokeOthersArmed = false) }

    /** `sessions-revoke-others-confirm-button` — the second press. */
    fun confirmRevokeOthers(onLocked: () -> Unit) {
        _state.update { it.copy(revokeOthersArmed = false) }
        run(onLocked) { d, own, prev -> revokeOthers(d, own, prev) }
    }

    fun onLockWordChange(value: String) = _state.update { it.copy(lockWord = value) }

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
        _state.update { it.copy(busy = true) }
        viewModelScope.launch {
            val page = try {
                call(ensureDevices(), ownDeviceId, _state.value.snapshot)
            } catch (e: Exception) {
                // Only "not connected" reaches here — every pass answers a page.
                ShellLog.w("SessionsVM", "[sessions] the pass could not start: ${e.message}")
                null
            }
            _state.update { it.copy(busy = false, snapshot = page?.snapshot ?: it.snapshot) }
            page?.snapshot?.let { SessionsTestState.paintedTokenIds = it.rows.map { r -> r.tokenId } }
            if (page?.locked == true) {
                // The lock landed: every bearer is revoked and the nest mints no
                // new one, so the app leaves the shell for its launch surface,
                // whose re-run challenge lands the locked notice (leg 2).
                ShellLog.w("SessionsVM", "[sessions] the account lock landed — leaving for the launch surface")
                _state.update { it.copy(lockWord = "") }
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
