package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiReportShareEntry
import com.fauna.ffi.FfiReportShareStatus
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.MailSpamAction
import uniffi.fauna_client_mail_settings.MailSpamMachine
import uniffi.fauna_client_mail_settings.MailSpamSnapshot
import uniffi.fauna_client_mail_settings.SpamStatus
import javax.inject.Inject

/**
 * Renders the shared `MailSpamMachine` (libs/fauna-client-mail-settings, over
 * UniFFI) for the per-account `mail-spam` page — reset the per-user Bayesian
 * model, opt in/out of the deployment baseline, and undo individual training
 * events. Per priority #2 this view-model holds **no** spam logic; it owns the
 * machine, mirrors its snapshot into a StateFlow, and dispatches actions. The
 * per-user feedback loop is not yet built nest-side (`mail-spam.md` § Impl status
 * today), so every action surfaces the seam's `unimplemented` rejection via
 * `snapshot.error` → the global error-message banner; the page is never faked
 * green.
 *
 * Distributed report sharing (`report-sharing.md` § Client wire) rides a
 * separate, already-landed `FfiModerationClient` seam — a bool opt-in plus a
 * read-only transparency list, not a state machine — so it gets its own
 * `reportShare`/`reportSharePublished` flows instead of folding into the
 * `MailSpamMachine` snapshot. Linux lead: apps/fauna-linux/src/settings/mail_spam.rs.
 */
@HiltViewModel
class MailSpamVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val machine: MailSpamMachine? = api.buildMailSpamMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    // Distributed report-sharing (report-sharing.md § Client wire) is a small
    // dedicated flow over FfiModerationClient directly — NOT part of the
    // MailSpamMachine (it's a bool + a read-only transparency list, not a state
    // machine; mirrors linux's separate hydrate_report_share/set_report_share).
    val reportShare = MutableStateFlow(false)
    val reportSharePublished = MutableStateFlow<List<FfiReportShareEntry>>(emptyList())

    // Per-account spam-threshold override — another small dedicated flow over a plain RPC
    // pair, not the MailSpamMachine, same shape as report-share above. `null`
    // = follows the admin default; `0u` is a real setting, distinct from `null`.
    val thresholdOverride = MutableStateFlow<UInt?>(null)

    init {
        hydrate()
        hydrateReportShare()
        hydrateThresholdOverride()
    }

    /** Load training history + the contribution flag; retry while the socket
     *  comes up (mirrors [LinkedNestsVM]). */
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

    fun resetModel() = dispatch(MailSpamAction.ResetModel)

    fun setContributeBaseline(contribute: Boolean) =
        dispatch(MailSpamAction.SetContributeBaseline(contribute = contribute))

    fun undo(historyIdHex: String) =
        dispatch(MailSpamAction.UndoTraining(historyIdHex = historyIdHex))

    /** Load the report-share opt-in + published transparency list. A single
     *  NestClient RPC — the transport already parks it while the socket
     *  comes up (transport.md § Request lifecycle step 3). */
    private fun hydrateReportShare() {
        viewModelScope.launch {
            try {
                publishReportShare(api.reportShareStatus())
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that
                // has not landed yet (NestClient::request_inner).
            }
        }
    }

    /** Set the report-share opt-in, then re-read status so the toggle + published
     *  list reflect the persisted value (opting out withdraws this actor's
     *  reports, which may shrink the list — mirrors linux `set_report_share`). */
    fun setReportShare(share: Boolean) {
        viewModelScope.launch {
            try {
                api.reportShareSet(share)
                publishReportShare(api.reportShareStatus())
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    private fun publishReportShare(status: FfiReportShareStatus) {
        reportShare.value = status.share
        reportSharePublished.value = status.published
    }

    /** Load the threshold override. A single NestClient RPC — the transport
     *  already parks it while the socket comes up (transport.md § Request
     *  lifecycle step 3). */
    private fun hydrateThresholdOverride() {
        viewModelScope.launch {
            try {
                thresholdOverride.value = api.spamThresholdOverrideGet()
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that
                // has not landed yet (NestClient::request_inner).
            }
        }
    }

    /** Set (or clear, with `null`) the override, then reflect the persisted
     *  value the nest confirmed — never the local edit (mirrors
     *  [setReportShare]). */
    fun setThresholdOverride(value: UInt?) {
        viewModelScope.launch {
            try {
                thresholdOverride.value = api.spamThresholdOverrideSet(value)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    private fun dispatch(action: MailSpamAction) {
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

    private fun publish(m: MailSpamMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
    }

    private companion object {
        val EMPTY_SNAPSHOT = MailSpamSnapshot(
            events = emptyList(),
            contributeBaseline = false,
            status = SpamStatus.IDLE,
            error = null,
        )
    }
}
