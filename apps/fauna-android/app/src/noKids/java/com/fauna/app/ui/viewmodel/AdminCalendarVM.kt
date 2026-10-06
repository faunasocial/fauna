package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.CaldavPolicyAction
import uniffi.fauna_client_mail_settings.CaldavPolicyMachine
import uniffi.fauna_client_mail_settings.CaldavPolicySnapshot
import uniffi.fauna_client_mail_settings.CaldavPolicyStatus
import javax.inject.Inject

/**
 * Renders the shared `CaldavPolicyMachine` (libs/fauna-client-mail-settings, over
 * UniFFI) for the flat admin `admin-calendar` page — the deployment-wide
 * CalDAV-enable toggle, the calendar sibling of `admin-mail`'s mail-enable toggle
 * (`admin.md` § 8 Calendar; `caldav-server.md` § Independent enablement). Per
 * priority #2 this view-model holds **no** policy logic; it owns the machine,
 * mirrors its snapshot into a StateFlow, and dispatches actions. The toggle reads
 * `caldav_enabled` from the Admin read twin `get_mail_config` and writes via
 * `set_caldav_enabled` (both live nest WS-RPC kinds — no nest work); a nest
 * rejection surfaces via `snapshot.error` → the global error-message banner, never
 * faked green. Linux lead: apps/fauna-linux/src/settings/admin_calendar.rs.
 */
@HiltViewModel
class AdminCalendarVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val machine: CaldavPolicyMachine? = api.buildCaldavPolicyMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    init {
        hydrate()
    }

    /** Load the effective config (`get_mail_config` → `caldav_enabled`); retry
     *  while the socket comes up. */
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

    fun setCaldavEnabled(enabled: Boolean) =
        dispatch(CaldavPolicyAction.SetCaldavEnabled(enabled = enabled))

    /** Commit the admin-set CalDAV listener port (`set_caldav_port`). The caller
     *  (the stateless content) has already validated `port` ∈ [1, 65535]. */
    fun setCaldavPort(port: Int) =
        dispatch(CaldavPolicyAction.SetCaldavPort(port = port.toUShort()))

    private fun dispatch(action: CaldavPolicyAction) {
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

    private fun publish(m: CaldavPolicyMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
    }

    private companion object {

        // Placeholder used only when the nest socket is absent (machine == null);
        // a live machine returns the catalog-default snapshot, then hydrate()
        // replaces it with the effective config.
        val EMPTY_SNAPSHOT = CaldavPolicySnapshot(
            caldavEnabled = false,
            caldavPort = DEFAULT_CALDAV_PORT,
            status = CaldavPolicyStatus.IDLE,
            error = null,
        )

        // `bridge_routing::DEFAULT_CALDAV_PORT` (caldav-server.md § Network exposure).
        const val DEFAULT_CALDAV_PORT: UShort = 8443u
    }
}
