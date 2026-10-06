package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ShellLog
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.BridgeApprovalAction
import uniffi.fauna_client_mail_settings.BridgeApprovalMachine
import uniffi.fauna_client_mail_settings.BridgeApprovalSnapshot
import uniffi.fauna_client_mail_settings.BridgeApprovalStatus
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
) : ViewModel() {

    private val machine: BridgeApprovalMachine? = api.buildBridgeApprovalMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    init {
        hydrate()
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

    fun reject(pubkeyHex: String) =
        dispatch(BridgeApprovalAction.Reject(pubkeyHex = pubkeyHex))

    fun rotate(pubkeyHex: String) =
        dispatch(BridgeApprovalAction.Rotate(pubkeyHex = pubkeyHex))

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
        )
    }
}
