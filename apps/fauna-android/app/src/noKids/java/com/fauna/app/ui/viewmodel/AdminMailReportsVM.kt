package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.DmarcReportsAction
import uniffi.fauna_client_mail_settings.DmarcReportsMachine
import uniffi.fauna_client_mail_settings.DmarcReportsSnapshot
import uniffi.fauna_client_mail_settings.DmarcReportsStatus
import uniffi.fauna_client_mail_settings.DmarcWindowTotals
import javax.inject.Inject

/**
 * Renders the shared `DmarcReportsMachine` (libs/fauna-client-mail-settings, over
 * UniFFI) for the `admin-mail-reports` sub-page (`dmarc-reporting.md` § Admin pane
 * summary surface). Per priority #2 this view-model holds no fold logic — the
 * totals, shares, top lists and the raw view's gunzip are the machine's; it owns
 * the machine, mirrors its snapshot into a StateFlow and dispatches actions. The
 * page re-reads on open (`hydrate`), on a filter change and on refresh; there is
 * no push. tui lead: apps/fauna-tui/src/admin/mail_reports.rs.
 */
@HiltViewModel
class AdminMailReportsVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val machine: DmarcReportsMachine? = api.buildDmarcReportsMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    init {
        hydrate()
    }

    private fun hydrate() {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.hydrate()
            } catch (_: Exception) {
                // The machine records a real failure in its own snapshot, which publish() surfaces.
            }
            publish(m)
        }
    }

    /** `admin-mail-reports-refresh-button`. */
    fun refresh() = dispatch(DmarcReportsAction.Refresh)

    /** `admin-mail-reports-window-select`: a day count from the shared options. */
    fun setWindow(days: UInt) = dispatch(DmarcReportsAction.SetWindow(days = days))

    /** `admin-mail-reports-domain-select`: one local domain, or null for every domain. */
    fun setDomain(domain: String?) = dispatch(DmarcReportsAction.SetDomain(domain = domain))

    /** `admin-mail-reports-report-view-button`: the row's own key opens its XML. */
    fun openReport(ourDomain: String, submitter: String, reportId: String) =
        dispatch(DmarcReportsAction.OpenReport(ourDomain = ourDomain, submitter = submitter, reportId = reportId))

    private fun dispatch(action: DmarcReportsAction) {
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

    private fun publish(m: DmarcReportsMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
    }

    private companion object {
        // Placeholder only when the nest socket is absent (machine == null): unloaded,
        // so just the filters paint; a live machine returns its own default snapshot.
        val EMPTY_SNAPSHOT = DmarcReportsSnapshot(
            windowDays = 7u,
            ourDomain = null,
            localDomains = emptyList(),
            loaded = false,
            totals = DmarcWindowTotals(
                messageCount = 0uL, reportCount = 0uL, submitterCount = 0uL,
                dmarcPass = 0uL, dmarcPassPercent = 0u,
                dkimPass = 0uL, dkimPassPercent = 0u,
                spfPass = 0uL, spfPassPercent = 0u,
                dispositionNone = 0uL, dispositionQuarantine = 0uL, dispositionReject = 0uL,
            ),
            sources = emptyList(),
            senders = emptyList(),
            reports = emptyList(),
            truncated = false,
            opened = null,
            status = DmarcReportsStatus.IDLE,
            error = null,
        )
    }
}
