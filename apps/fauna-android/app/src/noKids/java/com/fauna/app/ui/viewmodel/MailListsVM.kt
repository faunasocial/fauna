package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.ListDraft
import uniffi.fauna_client_mail_settings.ListsStatus
import uniffi.fauna_client_mail_settings.MailListsAction
import uniffi.fauna_client_mail_settings.MailListsMachine
import uniffi.fauna_client_mail_settings.MailListsSnapshot
import javax.inject.Inject

/**
 * Renders the shared `MailListsMachine` (libs/fauna-client-mail-settings, over
 * UniFFI) for the per-account `mail-lists` page — list / create / edit / delete
 * mailing lists (a list is a sixth alias kind). Per priority #2 this view-model
 * holds **no** list logic. The backend (`fauna.bridges.*_account_list`) is
 * live (`mail-mass-mailing.md` § Impl status today); create/update/delete
 * dispatch through the shared machine to the real RPCs, with a failure
 * surfacing via the global error-message banner. Linux lead:
 * apps/fauna-linux/src/settings/mail_lists.rs.
 */
@HiltViewModel
class MailListsVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val machine: MailListsMachine? = api.buildMailListsMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    // Distinguishes "still loading" from "resolved empty" (`ui/README.md` §
    // Copy comprehensibility rule 5 — an un-hydrated first paint must not
    // claim "No lists yet"). Flips true exactly once, alongside the first
    // real snapshot publish below (success or retries-exhausted).
    val hydrated = MutableStateFlow(false)

    init {
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

    fun create(draft: ListDraft) = dispatch(MailListsAction.Create(draft = draft))

    fun update(listIdHex: String, draft: ListDraft) =
        dispatch(MailListsAction.Update(listIdHex = listIdHex, draft = draft))

    fun delete(listIdHex: String) = dispatch(MailListsAction.Delete(listIdHex = listIdHex))

    private fun dispatch(action: MailListsAction) {
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

    private fun publish(m: MailListsMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
    }

    private companion object {
        val EMPTY_SNAPSHOT = MailListsSnapshot(
            lists = emptyList(),
            localDomains = emptyList(),
            status = ListsStatus.IDLE,
            error = null,
        )
    }
}
