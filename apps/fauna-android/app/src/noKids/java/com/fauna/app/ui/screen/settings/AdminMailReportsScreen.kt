package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.navigation.exitAdminToPrimary
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.AdminMailReportsVM
import social.fauna.generated.Ids
import uniffi.fauna_client_mail_settings.DmarcCountRow
import uniffi.fauna_client_mail_settings.DmarcReportRowView
import uniffi.fauna_client_mail_settings.DmarcReportsSnapshot
import uniffi.fauna_client_mail_settings.dmarcReportWindowOptions

/**
 * The `admin-mail-reports` admin sub-page — the DMARC aggregate reports peer mail
 * servers send this nest about its own domains (`dmarc-reporting.md` § Admin pane
 * summary surface). Stateless [AdminMailReportsContent] is split out for the
 * Compose test harness; [AdminMailReportsScreen] is the VM-bound wrapper the
 * NavHost mounts. A dumb renderer of the shared `DmarcReportsMachine`: before the
 * first read answers only the heading, back button, filters and refresh paint
 * ("no reports" and "not read yet" are different facts); errors ride the global
 * error-message banner. tui lead: apps/fauna-tui/src/admin/mail_reports.rs.
 */
@Composable
fun AdminMailReportsScreen(
    navController: NavController,
    vm: AdminMailReportsVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    AdminMailReportsContent(
        snapshot = snapshot,
        onBack = { navController.exitAdminToPrimary() },
        onRefresh = vm::refresh,
        onSetWindow = vm::setWindow,
        onSetDomain = vm::setDomain,
        onOpenReport = { vm.openReport(it.ourDomain, it.submitter, it.reportId) },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminMailReportsContent(
    snapshot: DmarcReportsSnapshot,
    onBack: () -> Unit,
    onRefresh: () -> Unit,
    onSetWindow: (UInt) -> Unit,
    onSetDomain: (String?) -> Unit,
    onOpenReport: (DmarcReportRowView) -> Unit,
    // The shared window options and `fauna_core::format::format_unix_local_date`,
    // injected FFI-free so the Compose test stays off the native path.
    windowOptions: List<UInt> = remember { dmarcReportWindowOptions() },
    formatDate: (Long) -> String = { com.fauna.ffi.formatUnixLocalDate(it) },
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_mail_reports_page_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(
                        onClick = onBack,
                        modifier = Modifier.testTag(Ids.ADMIN_NAV_BACK),
                    ) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.common_back))
                    }
                },
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(16.dp)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResource(R.string.admin_mail_reports_page_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // Filters: window raw value = day count, domain raw value = domain ("" = every).
            val windowLabels = windowOptions.map {
                it.toString() to stringResourceFmt(R.string.admin_mail_reports_page_window_days, it.toString())
            }
            SelectField(
                stringResource(R.string.admin_mail_reports_page_window_label),
                windowLabels,
                snapshot.windowDays.toString(),
                { onSetWindow(it.toUInt()) },
                Ids.ADMIN_MAIL_REPORTS_WINDOW_SELECT,
            )
            val everyDomain = stringResource(R.string.admin_mail_reports_page_every_domain)
            val domainOptions = (listOf("") + snapshot.localDomains).map { it to it.ifEmpty { everyDomain } }
            SelectField(
                stringResource(R.string.admin_mail_reports_page_domain_label),
                domainOptions,
                snapshot.ourDomain ?: "",
                { onSetDomain(it.ifEmpty { null }) },
                Ids.ADMIN_MAIL_REPORTS_DOMAIN_SELECT,
            )
            OutlinedButton(
                onClick = onRefresh,
                modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_REFRESH_BUTTON),
            ) { Text(stringResource(R.string.admin_mail_reports_page_refresh)) }

            if (snapshot.loaded) Loaded(snapshot, formatDate, onOpenReport)
        }
    }
}

@Composable
private fun Loaded(
    snapshot: DmarcReportsSnapshot,
    formatDate: (Long) -> String,
    onOpenReport: (DmarcReportRowView) -> Unit,
) {
    val totals = snapshot.totals
    Column(
        modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_SUMMARY),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(
            stringResource(R.string.admin_mail_reports_page_summary_title),
            style = MaterialTheme.typography.titleMedium,
        )
        Text(
            stringResourceFmt(
                R.string.admin_mail_reports_page_messages,
                totals.messageCount.toString(),
                totals.reportCount.toString(),
                totals.submitterCount.toString(),
            ),
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_MESSAGES),
        )
        Text(
            stringResourceFmt(
                R.string.admin_mail_reports_page_alignment,
                totals.dmarcPass.toString(), totals.dmarcPassPercent.toString(),
                totals.dkimPass.toString(), totals.dkimPassPercent.toString(),
                totals.spfPass.toString(), totals.spfPassPercent.toString(),
            ),
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_ALIGNMENT),
        )
        Text(
            stringResourceFmt(
                R.string.admin_mail_reports_page_dispositions,
                totals.dispositionNone.toString(),
                totals.dispositionQuarantine.toString(),
                totals.dispositionReject.toString(),
            ),
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_DISPOSITIONS),
        )
    }
    if (snapshot.truncated) {
        Text(
            stringResource(R.string.admin_mail_reports_page_truncated),
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_TRUNCATED),
        )
    }
    if (snapshot.reports.isEmpty()) {
        Text(
            stringResource(R.string.admin_mail_reports_page_empty),
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_EMPTY),
        )
    }
    if (snapshot.sources.isNotEmpty()) {
        CountList(
            stringResource(R.string.admin_mail_reports_page_sources_title),
            snapshot.sources,
            Ids.ADMIN_MAIL_REPORTS_SOURCE,
            Ids.ADMIN_MAIL_REPORTS_SOURCE_IP,
            Ids.ADMIN_MAIL_REPORTS_SOURCE_COUNT,
        )
    }
    if (snapshot.senders.isNotEmpty()) {
        CountList(
            stringResource(R.string.admin_mail_reports_page_senders_title),
            snapshot.senders,
            Ids.ADMIN_MAIL_REPORTS_SENDER,
            Ids.ADMIN_MAIL_REPORTS_SENDER_DOMAIN,
            Ids.ADMIN_MAIL_REPORTS_SENDER_COUNT,
        )
    }
    if (snapshot.reports.isNotEmpty()) {
        Text(
            stringResource(R.string.admin_mail_reports_page_reports_title),
            style = MaterialTheme.typography.titleMedium,
        )
        snapshot.reports.forEach { ReportRow(it, formatDate, onOpenReport) }
    }
    snapshot.opened?.let {
        Text(
            stringResource(R.string.admin_mail_reports_page_raw_title),
            style = MaterialTheme.typography.titleMedium,
        )
        Text(it.xml, modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_RAW))
    }
}

/** A top list: the container id repeats per row, each row holding its value + count leaves. */
@Composable
private fun CountList(title: String, rows: List<DmarcCountRow>, rowTag: String, valueTag: String, countTag: String) {
    Text(title, style = MaterialTheme.typography.titleMedium)
    rows.forEach { row ->
        Row(
            modifier = Modifier.fillMaxWidth().testTag(rowTag),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(row.value, modifier = Modifier.weight(2f).testTag(valueTag))
            Text(
                stringResourceFmt(R.string.admin_mail_reports_page_row_messages, row.count.toString()),
                modifier = Modifier.weight(1f).testTag(countTag),
            )
        }
    }
}

/** One `admin-mail-reports-report` row; its view button opens that row's own key. */
@Composable
private fun ReportRow(r: DmarcReportRowView, formatDate: (Long) -> String, onOpen: (DmarcReportRowView) -> Unit) {
    Column(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_MAIL_REPORTS_REPORT)) {
        Text(r.submitter, modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_REPORT_SUBMITTER))
        Text(r.ourDomain, modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_REPORT_DOMAIN))
        Text(
            stringResourceFmt(
                R.string.admin_mail_reports_page_report_period,
                formatDate(r.beginAt), formatDate(r.endAt),
            ),
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_REPORT_PERIOD),
        )
        Text(
            stringResourceFmt(R.string.admin_mail_reports_page_row_messages, r.messageCount.toString()),
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_REPORT_MESSAGES),
        )
        Text(
            stringResourceFmt(
                R.string.admin_mail_reports_page_report_pass,
                r.dmarcPass.toString(), r.messageCount.toString(),
            ),
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_REPORT_PASS),
        )
        OutlinedButton(
            onClick = { onOpen(r) },
            modifier = Modifier.testTag(Ids.ADMIN_MAIL_REPORTS_REPORT_VIEW_BUTTON),
        ) { Text(stringResource(R.string.admin_mail_reports_page_view)) }
    }
}
