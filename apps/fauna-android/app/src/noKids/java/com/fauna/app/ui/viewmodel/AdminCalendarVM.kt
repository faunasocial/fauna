package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.ui.util.machinePossibility
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.CaldavPolicyAction
import uniffi.fauna_client_mail_settings.CaldavPolicyMachine
import uniffi.fauna_client_mail_settings.CaldavPolicySnapshot
import uniffi.fauna_client_mail_settings.CaldavPolicyStatus
import uniffi.fauna_core.ReadState
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
    @ApplicationContext context: Context,
) : ViewModel() {

    private val machine: CaldavPolicyMachine? = api.buildCaldavPolicyMachine()

    // Every write this page makes waits for `get_mail_config` (admin.md
    // § Errors & edge cases, *Controls wait for their read*): the shared
    // `toggle_possibility` — still loading, failed to load, still saving.
    private val write = ReadGatedWrite(context) { machinePossibility(machine) { it.togglePossibility() } }

    /** What the toggle (and any save beside it) is painted from. */
    val toggleAnswer: StateFlow<uniffi.fauna_core.Possibility> = write.answer

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    /** A return by Back to this page reads it again (`AdminEntryReread`). */
    val entryReread = AdminEntryReread(::hydrate)

    init {
        hydrate()
        // A read that has not answered is read again when the nest connection
        // comes back (`docs/goal/behavior/admin.md` § Errors & edge cases,
        // *Controls wait for their read*; tui's `reconnect_reread_op`): only
        // while the shared machine's `read_state` is unresolved, so a port
        // typed over an answered page survives.
        viewModelScope.rereadOnReconnect(
            api.reconnectTick,
            answered = { machine?.readState() == ReadState.RESOLVED },
            reread = ::hydrate,
        )
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
        // The press asks the answer the toggle was painted from, live: a press
        // racing the read, or a second one while the first is on its way, is
        // refused with its reason on `error-message` and nothing is sent.
        if (!write.begin { errorMessage.value = it }) return
        val m = machine ?: return write.end()
        viewModelScope.launch {
            try {
                m.dispatch(action)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            write.end()
            publish(m)
        }
    }

    private fun publish(m: CaldavPolicyMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
        write.refresh()
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
