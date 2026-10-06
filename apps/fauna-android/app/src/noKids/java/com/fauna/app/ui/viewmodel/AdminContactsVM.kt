package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.CarddavPolicyAction
import uniffi.fauna_client_mail_settings.CarddavPolicyMachine
import uniffi.fauna_client_mail_settings.CarddavPolicySnapshot
import uniffi.fauna_client_mail_settings.CarddavPolicyStatus
import javax.inject.Inject

/**
 * Renders the shared `CarddavPolicyMachine` (libs/fauna-client-mail-settings,
 * over UniFFI) for the flat admin `admin-contacts` page — the deployment-wide
 * CardDAV-enable toggle, the contacts sibling of `admin-calendar`'s
 * CalDAV-enable toggle (`admin.md` § Contacts; `carddav-server.md`
 * § Independent enablement). Per priority #2 this view-model holds **no**
 * policy logic; it owns the machine, mirrors its snapshot into a StateFlow, and
 * dispatches actions. The toggle reads `carddav_enabled` from the Admin read
 * twin `get_mail_config` and writes via `set_carddav_enabled` (both live nest
 * WS-RPC kinds — no nest work); a nest rejection surfaces via `snapshot.error`
 * → the global error-message banner, never faked green. No port knob — CardDAV
 * rides the shared DAV listener admin-calendar's port input governs. Linux
 * lead: apps/fauna-linux/src/settings/admin_contacts.rs.
 */
@HiltViewModel
class AdminContactsVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val machine: CarddavPolicyMachine? = api.buildCarddavPolicyMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    init {
        hydrate()
    }

    /** Load the effective config (`get_mail_config` → `carddav_enabled`); retry
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

    fun setCarddavEnabled(enabled: Boolean) =
        dispatch(CarddavPolicyAction.SetCarddavEnabled(enabled = enabled))

    private fun dispatch(action: CarddavPolicyAction) {
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

    private fun publish(m: CarddavPolicyMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
    }

    private companion object {

        // Placeholder used only when the nest socket is absent (machine == null);
        // a live machine returns the catalog-default snapshot, then hydrate()
        // replaces it with the effective config.
        val EMPTY_SNAPSHOT = CarddavPolicySnapshot(
            carddavEnabled = false,
            status = CarddavPolicyStatus.IDLE,
            error = null,
        )
    }
}
