package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.testing.TestAgent
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.shareFile
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.MailExportVM
import uniffi.fauna_mail.ExportFormat
import uniffi.fauna_client_mail_settings.ExportStep
import uniffi.fauna_client_mail_settings.MailExportSnapshot
import uniffi.fauna_client_mail_settings.MailboxOption
import uniffi.fauna_client_mail_settings.MailboxProgressView
import uniffi.fauna_client_mail_settings.exportFormatLabel
import social.fauna.generated.Ids
import java.io.File

/**
 * The per-account `mail-export` wizard (`mail-export.md`): Format → Scope →
 * Confirm → Progress → Done, mbox / Maildir++ / EML-zip with sealed-blob
 * delivery. A sub-page of the mail-settings hub; the five steps share one
 * element set, shown conditionally by `snapshot.step`.
 *
 * Stateless [MailExportContent] is split out for the Compose test harness; the
 * VM-bound [MailExportScreen] is the wrapper the NavHost mounts. The export is
 * real: [MailExportVM] drives the shared loop, Progress repaints on its tick,
 * and Download writes the archive into an app-owned directory — the Done
 * summary then names the saved file, and the finished file is offered through
 * the share sheet (the user's own destination, as the account data export
 * does). Mirrors the leads (apps/fauna-linux/src/settings/mail_export.rs, the
 * FaunaKit `MailExportView`).
 */
@Composable
fun MailExportScreen(
    navController: NavController,
    vm: MailExportVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    // § Download flow's last inch on android: only a complete, terminated
    // archive is ever announced (a refused one leaves no file), and it goes to
    // the share sheet from the app-owned save directory. Under the e2e harness
    // the file stays where the harness reads it, with no sheet over the app —
    // the same harness-presence gate `UrlOpener` uses.
    LaunchedEffect(vm) {
        vm.savedArchives.collect { path ->
            if (TestAgent.isE2EActive) return@collect
            try {
                context.shareFile(File(path), mimeType = "application/zstd")
            } catch (e: Exception) {
                appMessages.showError(e.message ?: e.toString())
            }
        }
    }

    MailExportContent(
        snapshot = snapshot,
        // Format label via shared `export_format_label` (mail-export.md § Shared
        // export_format_label formatter); FFI lives here, off the testable Content.
        formatLabel = { fmt -> resolveLocalized(context, exportFormatLabel(fmt)).orEmpty() },
        onBack = { navController.popBackStack() },
        onSelectFormat = vm::selectFormat,
        onToggleMailbox = vm::toggleMailbox,
        onSetDateFrom = vm::setDateFrom,
        onSetDateTo = vm::setDateTo,
        onSetStripHeaders = vm::setStripHeaders,
        onNext = vm::next,
        onPrev = vm::back,
        onStart = vm::start,
        onPause = vm::pause,
        onResume = vm::resume,
        onCancel = vm::cancel,
        onDiscard = vm::discard,
        onDownload = vm::download,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MailExportContent(
    snapshot: MailExportSnapshot,
    formatLabel: (ExportFormat) -> String,
    onBack: () -> Unit,
    onSelectFormat: (ExportFormat) -> Unit,
    onToggleMailbox: (String) -> Unit,
    onSetDateFrom: (String) -> Unit,
    onSetDateTo: (String) -> Unit,
    onSetStripHeaders: (Boolean) -> Unit,
    onNext: () -> Unit,
    onPrev: () -> Unit,
    onStart: () -> Unit,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onCancel: () -> Unit,
    onDiscard: () -> Unit,
    onDownload: () -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.mail_export_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.common_back),
                        )
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
            when (snapshot.step) {
                ExportStep.FORMAT -> FormatStep(snapshot.format, formatLabel, onSelectFormat, onNext)
                ExportStep.SCOPE -> ScopeStep(snapshot, onToggleMailbox, onSetDateFrom, onSetDateTo, onSetStripHeaders, onPrev, onNext)
                ExportStep.CONFIRM -> ConfirmStep(snapshot, formatLabel, onPrev, onStart)
                ExportStep.PROGRESS -> ProgressStep(snapshot, onPause, onResume, onCancel)
                ExportStep.DONE -> DoneStep(snapshot, formatLabel, onDownload, onDiscard)
            }
        }
    }
}

@Composable
private fun FormatStep(
    format: ExportFormat,
    formatLabel: (ExportFormat) -> String,
    onSelectFormat: (ExportFormat) -> Unit,
    onNext: () -> Unit,
) {
    // Screen intro — the peers (web MailExportSection) render mail_export.description
    // above the format picker; android omitted it.
    Text(
        stringResource(R.string.mail_export_description),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Text(stringResource(R.string.mail_export_format_title), style = MaterialTheme.typography.titleMedium)
    Column(modifier = Modifier.testTag(Ids.MAIL_EXPORT_FORMAT_PICKER), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        FormatOption(ExportFormat.MBOX, format, formatLabel, onSelectFormat)
        FormatOption(ExportFormat.MAILDIR_PLUS, format, formatLabel, onSelectFormat)
        FormatOption(ExportFormat.EML_ZIP, format, formatLabel, onSelectFormat)
    }
    // Shared wizard-next-button (user-approved 2026-08-29, mirrors mail-import's
    // approved shape). Painted twice on this page (here and in ScopeStep) — safe
    // because `MailExportContent`'s `when (snapshot.step)` composes only the
    // active step, so only one copy is ever in the semantics tree at a time.
    Button(onClick = onNext, modifier = Modifier.testTag(Ids.WIZARD_NEXT_BUTTON)) {
        Text(stringResource(R.string.mail_export_next))
    }
}

@Composable
private fun FormatOption(
    value: ExportFormat,
    selected: ExportFormat,
    formatLabel: (ExportFormat) -> String,
    onSelect: (ExportFormat) -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        RadioButton(selected = value == selected, onClick = { onSelect(value) })
        Text(formatLabel(value))
    }
}

@Composable
private fun ScopeStep(
    snapshot: MailExportSnapshot,
    onToggleMailbox: (String) -> Unit,
    onSetDateFrom: (String) -> Unit,
    onSetDateTo: (String) -> Unit,
    onSetStripHeaders: (Boolean) -> Unit,
    onPrev: () -> Unit,
    onNext: () -> Unit,
) {
    Text(stringResource(R.string.mail_export_scope_title), style = MaterialTheme.typography.titleMedium)

    Column(modifier = Modifier.testTag(Ids.MAIL_EXPORT_SCOPE_MAILBOXES)) {
        Text(stringResource(R.string.mail_export_scope_mailboxes_label), style = MaterialTheme.typography.bodyMedium)
        if (snapshot.mailboxes.isEmpty()) {
            Text(
                stringResource(R.string.mail_export_scope_mailboxes_empty),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            snapshot.mailboxes.forEach { box: MailboxOption ->
                // Indexed row (ui.yaml `mail-export-scope-mailbox-item`, indexed:
                // true, user-approved 2026-08-29); `mailbox_selected` reads the
                // `state` on/off attribute rather than the checkbox glyph, so it
                // rides `stateDescription` (same mechanism as
                // ConversationsComposeBar's recipient-resolve-status). `mergeDescendants`
                // is required — without it this node's own semantics carry no Text,
                // since the name lives on the child `Text` composable, not the Row.
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .testTag(Ids.MAIL_EXPORT_SCOPE_MAILBOX_ITEM)
                        .semantics(mergeDescendants = true) {
                            stateDescription = if (box.selected) "on" else "off"
                        },
                ) {
                    Checkbox(checked = box.selected, onCheckedChange = { onToggleMailbox(box.name) })
                    Text(box.name)
                }
            }
        }
    }

    OutlinedTextField(
        value = snapshot.dateFrom,
        onValueChange = onSetDateFrom,
        label = { Text(stringResource(R.string.mail_export_scope_date_from_placeholder)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_EXPORT_SCOPE_DATE_FROM),
    )
    OutlinedTextField(
        value = snapshot.dateTo,
        onValueChange = onSetDateTo,
        label = { Text(stringResource(R.string.mail_export_scope_date_to_placeholder)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_EXPORT_SCOPE_DATE_TO),
    )
    ToggleRow(
        labelRes = R.string.mail_export_scope_strip_headers_label,
        subtitleRes = R.string.mail_export_scope_strip_headers_subtitle,
        checked = snapshot.stripHeaders,
        onCheckedChange = onSetStripHeaders,
        testId = "mail-export-scope-strip-headers-toggle",
    )

    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedButton(onClick = onPrev, modifier = Modifier.testTag(Ids.WIZARD_BACK_BUTTON)) {
            Text(stringResource(R.string.mail_export_back))
        }
        Button(onClick = onNext, modifier = Modifier.testTag(Ids.WIZARD_NEXT_BUTTON)) {
            Text(stringResource(R.string.mail_export_next))
        }
    }
}

@Composable
private fun ConfirmStep(
    snapshot: MailExportSnapshot,
    formatLabel: (ExportFormat) -> String,
    onPrev: () -> Unit,
    onStart: () -> Unit,
) {
    Text(stringResource(R.string.mail_export_confirm_title), style = MaterialTheme.typography.titleMedium)
    Text(
        stringResourceFmt(
            R.string.mail_export_confirm_summary_fmt,
            formatLabel(snapshot.format),
            snapshot.mailboxes.count { it.selected },
        ),
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.testTag(Ids.MAIL_EXPORT_CONFIRM_SUMMARY),
    )
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedButton(onClick = onPrev, modifier = Modifier.testTag(Ids.WIZARD_BACK_BUTTON)) {
            Text(stringResource(R.string.mail_export_back))
        }
        Button(onClick = onStart, modifier = Modifier.testTag(Ids.MAIL_EXPORT_START_BUTTON)) {
            Text(stringResource(R.string.mail_export_start_button))
        }
    }
}

@Composable
private fun ProgressStep(
    snapshot: MailExportSnapshot,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onCancel: () -> Unit,
) {
    Text(stringResource(R.string.mail_export_progress_title), style = MaterialTheme.typography.titleMedium)
    Text(
        stringResourceFmt(
            R.string.mail_export_progress_summary_fmt,
            snapshot.exportedCount, snapshot.totalCount, snapshot.skippedCount, snapshot.erroredCount,
        ),
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.testTag(Ids.MAIL_EXPORT_PROGRESS_SUMMARY),
    )
    LinearProgressIndicator(
        // Shared `fauna_core::format::quota_fraction` — the same zero-guarded,
        // clamped ratio every app's quota/progress bar uses (value-formatting.md
        // § Quota fraction), not a mail-export-specific hand-roll.
        progress = {
            com.fauna.ffi.quotaFraction(snapshot.exportedCount.toLong(), snapshot.totalCount.toLong()).toFloat()
        },
        modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_EXPORT_PROGRESS_BAR),
    )
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedButton(onClick = onPause, modifier = Modifier.testTag(Ids.MAIL_EXPORT_PAUSE_BUTTON)) {
            Text(stringResource(R.string.mail_export_pause_button))
        }
        OutlinedButton(onClick = onResume, modifier = Modifier.testTag(Ids.MAIL_EXPORT_RESUME_BUTTON)) {
            Text(stringResource(R.string.mail_export_resume_button))
        }
        OutlinedButton(
            onClick = onCancel,
            colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
            modifier = Modifier.testTag(Ids.MAIL_EXPORT_CANCEL_BUTTON),
        ) { Text(stringResource(R.string.mail_export_cancel_button)) }
    }
    // Per-mailbox progress list (indexed).
    Column(modifier = Modifier.testTag(Ids.MAIL_EXPORT_MAILBOX_PROGRESS_LIST)) {
        snapshot.mailboxProgress.forEach { row: MailboxProgressView ->
            Row(modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_EXPORT_MAILBOX_PROGRESS_LIST_ITEM)) {
                Text(
                    row.name,
                    modifier = Modifier.weight(1f).testTag(Ids.MAIL_EXPORT_MAILBOX_PROGRESS_LIST_ITEM_NAME),
                )
                Text(
                    "${row.exported} / ${row.total}",
                    modifier = Modifier.testTag(Ids.MAIL_EXPORT_MAILBOX_PROGRESS_LIST_ITEM_PROGRESS),
                )
            }
        }
    }
    Column(modifier = Modifier.testTag(Ids.MAIL_EXPORT_ERROR_LOG)) {
        Text(stringResource(R.string.mail_export_error_log_title), style = MaterialTheme.typography.titleSmall)
        snapshot.errorLog.forEach { line -> Text(line, style = MaterialTheme.typography.bodySmall) }
    }
}

@Composable
private fun DoneStep(
    snapshot: MailExportSnapshot,
    formatLabel: (ExportFormat) -> String,
    onDownload: () -> Unit,
    onDiscard: () -> Unit,
) {
    Text(stringResource(R.string.mail_export_done_title), style = MaterialTheme.typography.titleMedium)
    Text(
        snapshot.blobBytes?.let {
            // Once the archive is on disk the summary says WHERE — the visible
            // answer to the Download press (tui's and linux's shape).
            if (snapshot.savedArchivePath.isNotEmpty()) {
                stringResourceFmt(
                    R.string.mail_export_saved_summary_fmt,
                    formatLabel(snapshot.format), it, snapshot.savedArchivePath,
                )
            } else {
                stringResourceFmt(R.string.mail_export_done_summary_fmt, formatLabel(snapshot.format), it)
            }
        } ?: formatLabel(snapshot.format),
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.testTag(Ids.MAIL_EXPORT_DONE_SUMMARY),
    )
    Text(
        snapshot.downloadUrl.ifEmpty { stringResource(R.string.mail_export_download_url_label) },
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.testTag(Ids.MAIL_EXPORT_DOWNLOAD_URL),
    )
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Button(
            onClick = onDownload,
            enabled = snapshot.downloadUrl.isNotEmpty(),
            modifier = Modifier.testTag(Ids.MAIL_EXPORT_DOWNLOAD_BUTTON),
        ) { Text(stringResource(R.string.mail_export_download_button)) }
        OutlinedButton(
            onClick = onDiscard,
            colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
            modifier = Modifier.testTag(Ids.MAIL_EXPORT_DISCARD_BUTTON),
        ) { Text(stringResource(R.string.mail_export_discard_button)) }
    }
}

@Composable
private fun ToggleRow(
    labelRes: Int,
    subtitleRes: Int,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    testId: String,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(stringResource(labelRes), style = MaterialTheme.typography.bodyLarge)
            Text(
                stringResource(subtitleRes),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Switch(checked = checked, onCheckedChange = onCheckedChange, modifier = Modifier.testTag(testId))
    }
}
