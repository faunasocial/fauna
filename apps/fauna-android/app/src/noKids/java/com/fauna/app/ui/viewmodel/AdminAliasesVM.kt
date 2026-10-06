package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.ForwarderAction
import uniffi.fauna_client_mail_settings.ForwarderMachine
import uniffi.fauna_client_mail_settings.ForwarderStatus
import uniffi.fauna_client_mail_settings.ForwardersSnapshot
import javax.inject.Inject

/**
 * Renders the shared `ForwarderMachine` (libs/fauna-client-mail-settings, over
 * UniFFI) for the admin `admin-aliases` page — list / create / delete external
 * forwarders. Per priority #2 this view-model holds **no** forwarder logic; it
 * owns the machine, mirrors its snapshot into a StateFlow, and dispatches actions.
 * The backend is built (`admin.md` § 4 Impl status), so actions are genuinely
 * green; rejections surface via `snapshot.error` → the dedicated
 * `admin-aliases-action-error` element. Linux lead:
 * apps/fauna-linux/src/views/admin.rs.
 */
@HiltViewModel
class AdminAliasesVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val machine: ForwarderMachine? = api.buildForwardersMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)

    init {
        hydrate()
    }

    /** Load the forwarder list + the hosted-domain picker options; retry while
     *  the socket comes up (mirrors [MailSpamVM]). */
    private fun hydrate() {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.hydrate()
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that has not landed yet
                // (NestClient::request_inner), and the machine records any real failure
                // in its own snapshot, which the publish below surfaces.
            }
            publish(m)
        }
    }

    fun create(localDomain: String, pattern: String, forwardTarget: String) =
        dispatch(
            ForwarderAction.Create(
                localDomain = localDomain,
                pattern = pattern,
                forwardTarget = forwardTarget,
            ),
        )

    fun delete(aliasIdHex: String) =
        dispatch(ForwarderAction.Delete(aliasIdHex = aliasIdHex))

    private fun dispatch(action: ForwarderAction) {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.dispatch(action)
            } catch (_: Exception) {
                // Dispatch errors are folded into snapshot.error by the machine;
                // a hard FFI failure leaves the prior snapshot in place.
            }
            publish(m)
        }
    }

    private fun publish(m: ForwarderMachine) {
        snapshot.value = m.snapshot()
    }

    private companion object {
        val EMPTY_SNAPSHOT = ForwardersSnapshot(
            forwarders = emptyList(),
            localDomains = emptyList(),
            status = ForwarderStatus.IDLE,
            error = null,
        )
    }
}
