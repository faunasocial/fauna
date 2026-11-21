package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ShellLog
import com.fauna.app.ui.util.answerRefusal
import com.fauna.app.ui.util.machinePossibility
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.BridgeApprovalAction
import uniffi.fauna_client_mail_settings.BridgeApprovalMachine
import uniffi.fauna_client_mail_settings.BridgeApprovalSnapshot
import uniffi.fauna_client_mail_settings.BridgeApprovalStatus
import uniffi.fauna_client_mail_settings.BridgeConfirm
import uniffi.fauna_core.Possibility
import uniffi.fauna_core.ReadState
import javax.inject.Inject

/**
 * Renders the shared `BridgeApprovalMachine` (libs/fauna-client-mail-settings,
 * over UniFFI) for the admin `admin-bridges-pending` page — list / approve /
 * reject pending mail bridges. Per priority #2 this view-model holds **no**
 * approval logic; it owns the machine, mirrors its snapshot into a StateFlow,
 * and dispatches actions. The backend is real (`mail-bridge-lifecycle.md`
 * § Pending approval); errors surface via `snapshot.error` → the global
 * error-message banner. Linux lead: apps/fauna-linux/src/views/admin.rs.
 */
@HiltViewModel
class AdminBridgesPendingVM @Inject constructor(
    private val api: ApiClient,
    @ApplicationContext private val context: Context,
) : ViewModel() {

    private val machine: BridgeApprovalMachine? = api.buildBridgeApprovalMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    /** Where the pending and approved list read stands — the shared
     *  `BridgeApprovalMachine::read_state` (admin.md § Errors & edge cases,
     *  *Controls wait for their read*): each section says it is loading (or
     *  failed) until the read resolves, never its empty text. A machine that
     *  could not be built never reads. */
    val listRead = MutableStateFlow(machine?.readState() ?: ReadState.FAILED)

    /** The open confirm's answer — `confirm_possibility`: live only while
     *  something is armed and no act is on its way. */
    val confirmAnswer = MutableStateFlow(confirmNow())

    private fun confirmNow(): Possibility = machinePossibility(machine) { it.confirmPossibility() }

    /** A return by Back to this page reads it again (`AdminEntryReread`). */
    val entryReread = AdminEntryReread(::hydrate)

    init {
        hydrate()
        // A read that has not answered is read again when the nest connection
        // comes back (`docs/goal/behavior/admin.md` § Errors & edge cases,
        // *Controls wait for their read*; tui's `reconnect_reread_op`): only
        // while the shared machine's `read_state` is unresolved, so a draft
        // over an answered page survives.
        viewModelScope.rereadOnReconnect(
            api.reconnectTick,
            answered = { machine?.readState() == ReadState.RESOLVED },
            reread = ::hydrate,
        )
    }

    /** Load the pending-bridge feed; retry while the socket comes up. */
    private fun hydrate() {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.hydrate()
            } catch (e: Exception) {
                // One attempt: the transport already waits out a socket that has not landed yet
                // (NestClient::request_inner), and the machine records any real failure
                // in its own snapshot, which the publish below surfaces.
                ShellLog.d("AdminBridgesPendingVM", "pending-bridge hydrate attempt failed: ${e.message}")
            }
            publish(m)
        }
    }

    fun approve(pubkeyHex: String, role: String) =
        dispatch(BridgeApprovalAction.Approve(pubkeyHex = pubkeyHex, role = role))

    /**
     * Open the reject / rotate confirm in the shared machine. Sync (arming reaches no
     * nest), so the page paints the confirm in the click's own frame; the open act is
     * read back as `snapshot.armed` — the page keeps no confirm state of its own.
     */
    fun arm(confirm: BridgeConfirm) {
        val m = machine ?: return
        m.arm(confirm)
        publish(m)
    }

    /** The open confirm's cancel button: close it, no write. */
    fun disarm() {
        val m = machine ?: return
        m.disarm()
        publish(m)
    }

    /**
     * The open confirm's confirm button: the press asks `confirm_possibility`
     * and takes the arm with `begin_confirm` before dispatching, so the
     * confirm paints in flight at once and a second press is refused with
     * *still saving* (or *nothing to confirm*) on `error-message` — never the
     * machine's raw invalid-state error — and nothing reaches the nest.
     */
    fun confirm() {
        val m = machine
        if (m == null || !m.beginConfirm()) {
            answerRefusal(context, confirmNow())?.reason?.let { errorMessage.value = it }
            return
        }
        // Paint the in-flight answer now, before the act's round trip.
        publish(m)
        dispatch(BridgeApprovalAction.Confirm)
    }

    private fun dispatch(action: BridgeApprovalAction) {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.dispatch(action)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            publish(m)
        }
    }

    private fun publish(m: BridgeApprovalMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
        listRead.value = m.readState()
        confirmAnswer.value = m.confirmPossibility()
    }

    private companion object {
        val EMPTY_SNAPSHOT = BridgeApprovalSnapshot(
            pending = emptyList(),
            approved = emptyList(),
            mailEnabled = null,
            caldavEnabled = null,
            carddavEnabled = null,
            webdavEnabled = null,
            status = BridgeApprovalStatus.IDLE,
            error = null,
            armed = null,
        )
    }
}
