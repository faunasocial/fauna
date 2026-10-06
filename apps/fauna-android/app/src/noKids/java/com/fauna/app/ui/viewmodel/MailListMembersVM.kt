package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.ListsStatus
import uniffi.fauna_client_mail_settings.MailListMembersAction
import uniffi.fauna_client_mail_settings.MailListMembersMachine
import uniffi.fauna_client_mail_settings.MailListMembersSnapshot
import javax.inject.Inject

/**
 * Renders the shared `MailListMembersMachine` (libs/fauna-client-mail-settings,
 * over UniFFI) for one list's `mail-list-members` page — add / batch-import /
 * unsubscribe / resubscribe. Scoped to a single `list_id`; the page is reached
 * from a `mail-lists` row's members button, so the list id + name arrive as nav
 * args and are passed to [bind] (the machine is constructed per-list). The
 * backend is live (`mail-mass-mailing.md` § Impl status today); actions
 * dispatch through the shared machine to the real RPCs, with a failure
 * surfacing via the global error-message banner. Linux lead:
 * apps/fauna-linux/src/settings/mail_list_members.rs.
 */
@HiltViewModel
class MailListMembersVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private var machine: MailListMembersMachine? = null
    private var bound = false

    val snapshot = MutableStateFlow(emptySnapshot("", ""))
    val errorMessage = MutableStateFlow<String?>(null)

    // Distinguishes "list open, snapshot pending" from "list loaded"
    // (`ui/README.md` § Copy comprehensibility rule 5). Android's route always
    // supplies a concrete `listIdHex` (`{listIdHex}` is a required nav-graph
    // path segment — `bind` is never reached with no list open, unlike
    // linux's placeholder-list architecture), so unlike linux/tui there is no
    // separate "no list open" state to track here — just loading vs. loaded.
    // Flips true exactly once, alongside the first real snapshot publish
    // below (success or retries-exhausted).
    val hydrated = MutableStateFlow(false)

    /** Build the per-list machine from the nav args (idempotent) and hydrate. */
    fun bind(listIdHex: String, listName: String) {
        if (bound) return
        bound = true
        val m = api.buildMailListMembersMachine(listIdHex, listName)
        if (m == null) {
            snapshot.value = emptySnapshot(listIdHex, listName)
            return
        }
        machine = m
        snapshot.value = m.snapshot()
        hydrate()
    }

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
            hydrated.value = true
            publish(m)
        }
    }

    fun addMember(address: String) = dispatch(MailListMembersAction.AddMember(address = address))
    fun batchImport(addresses: String) = dispatch(MailListMembersAction.BatchImport(addresses = addresses))
    fun unsubscribe(address: String) = dispatch(MailListMembersAction.Unsubscribe(address = address))
    fun resubscribe(address: String) = dispatch(MailListMembersAction.Resubscribe(address = address))

    private fun dispatch(action: MailListMembersAction) {
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

    private fun publish(m: MailListMembersMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
    }

    private companion object {
        fun emptySnapshot(listIdHex: String, listName: String) = MailListMembersSnapshot(
            listIdHex = listIdHex,
            listName = listName,
            members = emptyList(),
            subscribedCount = 0u,
            unsubscribedCount = 0u,
            status = ListsStatus.IDLE,
            error = null,
            lastImport = null,
        )
    }
}
