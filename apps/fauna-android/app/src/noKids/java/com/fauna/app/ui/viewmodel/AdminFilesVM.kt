package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.WebdavPolicyAction
import uniffi.fauna_client_mail_settings.WebdavPolicyMachine
import uniffi.fauna_client_mail_settings.WebdavPolicySnapshot
import uniffi.fauna_client_mail_settings.WebdavPolicyStatus
import javax.inject.Inject

/**
 * Renders the shared `WebdavPolicyMachine` (libs/fauna-client-mail-settings,
 * over UniFFI) for the flat admin `admin-files` page — the deployment-wide
 * WebDAV-enable toggle, the files sibling of `admin-contacts`'s CardDAV-enable
 * toggle (`admin.md` § Files; `webdav-server.md` § Independent enablement).
 * Per priority #2 this view-model holds **no** policy logic; it owns the
 * machine, mirrors its snapshot into a StateFlow, and dispatches actions. The
 * toggle reads `webdav_enabled` from the Admin read twin `get_mail_config` and
 * writes via `set_webdav_enabled` (both live nest WS-RPC kinds — no nest
 * work); a nest rejection surfaces via `snapshot.error` → the global
 * error-message banner, never faked green. No port knob — WebDAV rides the
 * shared DAV listener admin-calendar's port input governs. Linux lead:
 * apps/fauna-linux/src/settings/admin_files.rs.
 */
@HiltViewModel
class AdminFilesVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val machine: WebdavPolicyMachine? = api.buildWebdavPolicyMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    init {
        hydrate()
    }

    /** Load the effective config (`get_mail_config` → `webdav_enabled`); retry
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

    fun setWebdavEnabled(enabled: Boolean) =
        dispatch(WebdavPolicyAction.SetWebdavEnabled(enabled = enabled))

    private fun dispatch(action: WebdavPolicyAction) {
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

    private fun publish(m: WebdavPolicyMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
    }

    private companion object {

        // Placeholder used only when the nest socket is absent (machine == null);
        // a live machine returns the catalog-default snapshot, then hydrate()
        // replaces it with the effective config.
        val EMPTY_SNAPSHOT = WebdavPolicySnapshot(
            webdavEnabled = false,
            status = WebdavPolicyStatus.IDLE,
            error = null,
        )
    }
}
