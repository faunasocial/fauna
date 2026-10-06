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
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.MailImportVM
import uniffi.fauna_client_mail_settings.ImportSourceKind
import uniffi.fauna_client_mail_settings.ImportStep
import uniffi.fauna_client_mail_settings.ImportTlsMode
import uniffi.fauna_client_mail_settings.MailImportSnapshot
import uniffi.fauna_client_mail_settings.SourceMailboxOption
import uniffi.fauna_client_mail_settings.importSourceKindLabel
import uniffi.fauna_client_mail_settings.importTlsModeLabel
import social.fauna.generated.Ids

/**
 * The per-account `mail-import` wizard (`mailbox-migration.md`): Source → Scope
 * → Confirm → Progress → Done, pulling a person's existing mail off a foreign
 * IMAP server (Gmail / Outlook / iCloud / generic) into their Fauna mailbox. A
 * sub-page of the mail-settings hub; the five steps share one element set, shown
 * conditionally by `snapshot.step`.
 *
 * Stateless [MailImportContent] is split out for the Compose test harness; the
 * VM-bound [MailImportScreen] is the wrapper the NavHost mounts. Leads: tui
 * (apps/fauna-tui/src/settings/mail_import.rs) and linux
 * (apps/fauna-linux/src/settings/mail_import.rs).
 *
 * ## Unlike [MailExportScreen], the backend is REAL
 *
 * Both machine seams ship, so Connect dials a genuine foreign server and
 * Start/Pause/Resume/Cancel drive a genuine `import_sessions` row. Anything on
 * the error banner is a real nest or source answer.
 *
 * ## Per-provider Source-step field visibility
 *
 * `mailbox-migration.md` § Wizard steps step 1's "Required fields", as
 * transcribed by the tui lead: Gmail/iCloud show username + app-password (plus
 * that provider's help line); Outlook shows the OAuth button AND the IMAP
 * fallback beside it; Generic shows the IMAP fields alone. `source-username` is
 * the one field painted for every kind. The OAuth button is painted but never
 * enabled — no app wires the Microsoft Graph dance yet
 * (`mailbox-migration.md`'s own "Not in scope" list).
 *
 * ## The text fields are DRAFTS, committed at the transition
 *
 * Every Source/Scope field is `remember`ed here and the whole form commits as
 * ONE ordered dispatch at Connect / Next ([MailImportVM.connect] /
 * [MailImportVM.scopeNext]) — tui's `connect_actions` / `scope_next_actions`. A
 * `Set*` per keystroke would be an FFI hop per character, and on the leads it
 * also races the Connect tap badly enough to log in with a truncated password.
 * Because the drafts live here and `render` never writes back into them, a
 * failed connect keeps the credentials on screen for the retry § Wizard steps
 * step 2 promises.
 */
@Composable
fun MailImportScreen(
    navController: NavController,
    vm: MailImportVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    MailImportContent(
        snapshot = snapshot,
        // The two picker vocabularies come from the shared Rust maps; FFI lives
        // here, off the testable Content (the MailExportScreen shape).
        sourceKindLabel = { k -> resolveLocalized(context, importSourceKindLabel(k)).orEmpty() },
        tlsModeLabel = { m -> resolveLocalized(context, importTlsModeLabel(m)).orEmpty() },
        onBack = { navController.popBackStack() },
        onSelectSourceKind = vm::selectSourceKind,
        onSetTlsMode = vm::setTlsMode,
        onConnect = vm::connect,
        onToggleMailbox = vm::toggleMailbox,
        onScopeNext = vm::scopeNext,
        onPrev = vm::back,
        onStart = vm::start,
        onPause = vm::pause,
        onResume = vm::resume,
        onCancel = vm::cancel,
        onViewImported = { navController.popBackStack() },
        onReviewSkipped = { navController.popBackStack() },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MailImportContent(
    snapshot: MailImportSnapshot,
    sourceKindLabel: (ImportSourceKind) -> String,
    tlsModeLabel: (ImportTlsMode) -> String,
    onBack: () -> Unit,
    onSelectSourceKind: (ImportSourceKind) -> Unit,
    onSetTlsMode: (ImportTlsMode) -> Unit,
    onConnect: (ImportSourceKind, String, String, String, String) -> Unit,
    onToggleMailbox: (String) -> Unit,
    onScopeNext: (String, String) -> Unit,
    onPrev: () -> Unit,
    onStart: () -> Unit,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onCancel: () -> Unit,
    onViewImported: () -> Unit,
    onReviewSkipped: () -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.mail_import_title),
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
                ImportStep.SOURCE -> SourceStep(snapshot, sourceKindLabel, tlsModeLabel, onSelectSourceKind, onSetTlsMode, onConnect)
                ImportStep.SCOPE -> ScopeStep(snapshot, onToggleMailbox, onScopeNext, onPrev)
                ImportStep.CONFIRM -> ConfirmStep(snapshot, sourceKindLabel, onPrev, onStart)
                ImportStep.PROGRESS -> ProgressStep(snapshot, onPause, onResume, onCancel)
                ImportStep.DONE -> DoneStep(snapshot, onViewImported, onReviewSkipped)
            }
        }
    }
}

@Composable
private fun SourceStep(
    snapshot: MailImportSnapshot,
    sourceKindLabel: (ImportSourceKind) -> String,
    tlsModeLabel: (ImportTlsMode) -> String,
    onSelectSourceKind: (ImportSourceKind) -> Unit,
    onSetTlsMode: (ImportTlsMode) -> Unit,
    onConnect: (ImportSourceKind, String, String, String, String) -> Unit,
) {
    // Page-local drafts — see the class docs on why these are not per-keystroke
    // dispatches. Keyed on nothing, so they survive every re-render of this step
    // (which is what keeps a failed connect's credentials on screen).
    var host by remember { mutableStateOf("") }
    var port by remember { mutableStateOf("") }
    var username by remember { mutableStateOf("") }
    // ONE buffer behind both the app-password and password fields: a given
    // provider shows exactly one of them, and it carries the same credential.
    var password by remember { mutableStateOf("") }

    val kind = snapshot.sourceKind
    val appPasswordKind = kind == ImportSourceKind.GMAIL || kind == ImportSourceKind.I_CLOUD
    val imapFieldsKind = kind == ImportSourceKind.OUTLOOK || kind == ImportSourceKind.GENERIC

    // Re-seed the host/port drafts from the snapshot's own value whenever the
    // picked kind actually CHANGES — `SelectSourceKind`'s provider preset
    // (shared Rust `apply_client_action`, never re-derived here) only exists
    // in the RE-PUBLISHED snapshot after the dispatch resolves. `LaunchedEffect`
    // alone fires on the FIRST composition too (there is no "previous" kind to
    // compare against), which would re-seed on every page open/hydrate — not a
    // kind change, and it broke `connectCarriesTheTypedSourceFormOut` by
    // pre-filling "993" before the test's own `performTextInput` typed into an
    // apparently-empty field. `previousKind` tracks the transition explicitly so
    // the effect is a no-op on mount. Never on every snapshot either: a
    // re-render mid-form (a failed Connect, a TLS-mode pick) must not clobber
    // what the user typed — apple's `syncDraftsFromSnapshot` shape.
    // Unconditional when a real change does fire — Generic has no preset of its
    // own, so it keeps whatever the previous kind painted, matching apple's own
    // accepted quirk.
    var previousKind by remember { mutableStateOf<ImportSourceKind?>(null) }
    LaunchedEffect(kind) {
        if (previousKind != null) {
            host = snapshot.host
            port = snapshot.port.toString()
        }
        previousKind = kind
    }

    Text(
        stringResource(R.string.mail_import_description),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Text(stringResource(R.string.mail_import_source_title), style = MaterialTheme.typography.titleMedium)
    Column(
        modifier = Modifier.testTag(Ids.MAIL_IMPORT_SOURCE_PICKER),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        SourceOption(ImportSourceKind.GMAIL, kind, sourceKindLabel, onSelectSourceKind)
        SourceOption(ImportSourceKind.OUTLOOK, kind, sourceKindLabel, onSelectSourceKind)
        SourceOption(ImportSourceKind.I_CLOUD, kind, sourceKindLabel, onSelectSourceKind)
        SourceOption(ImportSourceKind.GENERIC, kind, sourceKindLabel, onSelectSourceKind)
    }

    // Painted for every kind — nothing in ui.yaml scopes `source-username` to a
    // subset the way the other four are annotated.
    OutlinedTextField(
        value = username,
        onValueChange = { username = it },
        label = { Text(stringResource(R.string.mail_import_source_username_placeholder)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_IMPORT_SOURCE_USERNAME),
    )

    if (appPasswordKind) {
        OutlinedTextField(
            value = password,
            onValueChange = { password = it },
            label = { Text(stringResource(R.string.mail_import_source_app_password_label)) },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_IMPORT_SOURCE_APP_PASSWORD),
        )
        Text(
            stringResource(
                if (kind == ImportSourceKind.GMAIL) {
                    R.string.mail_import_source_app_password_help_gmail
                } else {
                    R.string.mail_import_source_app_password_help_icloud
                }
            ),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }

    if (kind == ImportSourceKind.OUTLOOK) {
        // Painted because ui.yaml requires the element exist, but never enabled:
        // no app wires the Microsoft Graph dance yet. The IMAP fallback below is
        // the real path for an Outlook account today.
        OutlinedButton(
            onClick = {},
            enabled = false,
            modifier = Modifier.testTag(Ids.MAIL_IMPORT_SOURCE_OAUTH_BUTTON),
        ) { Text(stringResource(R.string.mail_import_source_oauth_button)) }
    }

    if (imapFieldsKind) {
        OutlinedTextField(
            value = host,
            onValueChange = { host = it },
            label = { Text(stringResource(R.string.mail_import_source_host_placeholder)) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_IMPORT_SOURCE_HOST),
        )
        OutlinedTextField(
            value = port,
            onValueChange = { port = it },
            label = { Text(stringResource(R.string.mail_import_source_port_placeholder)) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_IMPORT_SOURCE_PORT),
        )
        Column(
            modifier = Modifier.testTag(Ids.MAIL_IMPORT_SOURCE_TLS_MODE),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            TlsOption(ImportTlsMode.IMPLICIT, snapshot.tlsMode, tlsModeLabel, onSetTlsMode)
            TlsOption(ImportTlsMode.START_TLS, snapshot.tlsMode, tlsModeLabel, onSetTlsMode)
        }
        OutlinedTextField(
            value = password,
            onValueChange = { password = it },
            label = { Text(stringResource(R.string.mail_import_source_password_placeholder)) },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_IMPORT_SOURCE_PASSWORD),
        )
    }

    Button(
        onClick = { onConnect(kind, host, port, username, password) },
        modifier = Modifier.testTag(Ids.MAIL_IMPORT_CONNECT_BUTTON),
    ) { Text(stringResource(R.string.mail_import_connect_button)) }
}

@Composable
private fun SourceOption(
    value: ImportSourceKind,
    selected: ImportSourceKind,
    label: (ImportSourceKind) -> String,
    onSelect: (ImportSourceKind) -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        RadioButton(selected = value == selected, onClick = { onSelect(value) })
        Text(label(value))
    }
}

@Composable
private fun TlsOption(
    value: ImportTlsMode,
    selected: ImportTlsMode,
    label: (ImportTlsMode) -> String,
    onSelect: (ImportTlsMode) -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        RadioButton(selected = value == selected, onClick = { onSelect(value) })
        Text(label(value))
    }
}

@Composable
private fun ScopeStep(
    snapshot: MailImportSnapshot,
    onToggleMailbox: (String) -> Unit,
    onScopeNext: (String, String) -> Unit,
    onPrev: () -> Unit,
) {
    var dateFrom by remember { mutableStateOf("") }
    // MB — the field's unit; the action takes bytes. Pre-filled with the
    // machine's own default so the value on screen is the value in force.
    var maxSizeMb by remember { mutableStateOf(DEFAULT_MAX_SIZE_MB) }

    Text(stringResource(R.string.mail_import_scope_title), style = MaterialTheme.typography.titleMedium)

    Column(modifier = Modifier.testTag(Ids.MAIL_IMPORT_SCOPE_MAILBOXES)) {
        Text(stringResource(R.string.mail_import_scope_mailboxes_label), style = MaterialTheme.typography.bodyMedium)
        if (snapshot.mailboxes.isEmpty()) {
            Text(
                stringResource(R.string.mail_import_scope_mailboxes_empty),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            snapshot.mailboxes.forEach { box: SourceMailboxOption ->
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.testTag(Ids.MAIL_IMPORT_SCOPE_MAILBOX_ITEM),
                ) {
                    Checkbox(checked = box.selected, onCheckedChange = { onToggleMailbox(box.name) })
                    Text(box.name)
                }
            }
        }
    }

    OutlinedTextField(
        value = dateFrom,
        onValueChange = { dateFrom = it },
        label = { Text(stringResource(R.string.mail_import_scope_date_from_placeholder)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_IMPORT_SCOPE_DATE_FROM),
    )
    OutlinedTextField(
        value = maxSizeMb,
        onValueChange = { maxSizeMb = it },
        label = { Text(stringResource(R.string.mail_import_scope_max_size_label)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_IMPORT_SCOPE_MAX_SIZE),
    )
    // Informational only: no MailImportAction changes the destination mapping,
    // and the goal doc names no control for it either (§ Wizard steps step 3).
    Text(
        stringResource(R.string.mail_import_scope_mailbox_mapping_label),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.testTag(Ids.MAIL_IMPORT_SCOPE_MAILBOX_MAPPING),
    )

    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedButton(onClick = onPrev, modifier = Modifier.testTag(Ids.WIZARD_BACK_BUTTON)) {
            Text(stringResource(R.string.mail_import_back))
        }
        Button(
            onClick = { onScopeNext(dateFrom, maxSizeMb) },
            modifier = Modifier.testTag(Ids.WIZARD_NEXT_BUTTON),
        ) { Text(stringResource(R.string.mail_import_next)) }
    }
}

@Composable
private fun ConfirmStep(
    snapshot: MailImportSnapshot,
    sourceKindLabel: (ImportSourceKind) -> String,
    onPrev: () -> Unit,
    onStart: () -> Unit,
) {
    val selected = snapshot.mailboxes.filter { it.selected }
    Text(stringResource(R.string.mail_import_confirm_title), style = MaterialTheme.typography.titleMedium)
    Text(
        stringResourceFmt(
            R.string.mail_import_confirm_summary_fmt,
            sourceKindLabel(snapshot.sourceKind),
            selected.size,
            // The Confirm estimate is the sum of the SELECTED mailboxes' own
            // reported counts (§ Wizard steps step 4) — no estimated-bytes or
            // wall-clock field exists on the snapshot, so neither is claimed.
            selected.sumOf { it.messageCount.toLong() },
        ),
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.testTag(Ids.MAIL_IMPORT_CONFIRM_SUMMARY),
    )
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedButton(onClick = onPrev, modifier = Modifier.testTag(Ids.WIZARD_BACK_BUTTON)) {
            Text(stringResource(R.string.mail_import_back))
        }
        Button(onClick = onStart, modifier = Modifier.testTag(Ids.MAIL_IMPORT_START_BUTTON)) {
            Text(stringResource(R.string.mail_import_start_button))
        }
    }
}

@Composable
private fun ProgressStep(
    snapshot: MailImportSnapshot,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onCancel: () -> Unit,
) {
    Text(stringResource(R.string.mail_import_progress_title), style = MaterialTheme.typography.titleMedium)
    Text(
        stringResourceFmt(
            R.string.mail_import_progress_summary_fmt,
            snapshot.importedCount, snapshot.totalCount, snapshot.skippedCount, snapshot.erroredCount,
        ),
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.testTag(Ids.MAIL_IMPORT_PROGRESS_SUMMARY),
    )
    LinearProgressIndicator(
        // Shared `fauna_core::format::quota_fraction` — the same zero-guarded,
        // clamped ratio every app's quota/progress bar uses (value-formatting.md
        // § Quota fraction), not a mail-import-specific hand-roll.
        progress = {
            com.fauna.ffi.quotaFraction(snapshot.importedCount.toLong(), snapshot.totalCount.toLong()).toFloat()
        },
        modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_IMPORT_PROGRESS_BAR),
    )
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedButton(onClick = onPause, modifier = Modifier.testTag(Ids.MAIL_IMPORT_PAUSE_BUTTON)) {
            Text(stringResource(R.string.mail_import_pause_button))
        }
        OutlinedButton(onClick = onResume, modifier = Modifier.testTag(Ids.MAIL_IMPORT_RESUME_BUTTON)) {
            Text(stringResource(R.string.mail_import_resume_button))
        }
        OutlinedButton(
            onClick = onCancel,
            colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
            modifier = Modifier.testTag(Ids.MAIL_IMPORT_CANCEL_BUTTON),
        ) { Text(stringResource(R.string.mail_import_cancel_button)) }
    }
    // Per-mailbox rows (indexed). The machine tracks only GLOBAL counts — no
    // per-mailbox breakdown exists on the snapshot the way export's does — so
    // each row shows its PLANNED message count, the tui lead's own
    // accurate-to-what-exists simplification.
    Text(
        stringResource(R.string.mail_import_error_log_title),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Column(modifier = Modifier.testTag(Ids.MAIL_IMPORT_ERROR_LOG)) {
        snapshot.errorLog.forEach { line ->
            Text(line, style = MaterialTheme.typography.bodySmall)
        }
    }
    Column(modifier = Modifier.testTag(Ids.MAIL_IMPORT_MAILBOX_PROGRESS_LIST)) {
        snapshot.mailboxes.filter { it.selected }.forEach { box: SourceMailboxOption ->
            Row(modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM)) {
                Text(
                    box.name,
                    modifier = Modifier.weight(1f).testTag(Ids.MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM_NAME),
                )
                Text(
                    stringResourceFmt(R.string.mail_import_progress_row_fmt, box.messageCount),
                    modifier = Modifier.testTag(Ids.MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM_PROGRESS),
                )
            }
        }
    }
}

@Composable
private fun DoneStep(
    snapshot: MailImportSnapshot,
    onViewImported: () -> Unit,
    onReviewSkipped: () -> Unit,
) {
    Text(stringResource(R.string.mail_import_done_title), style = MaterialTheme.typography.titleMedium)
    Text(
        stringResourceFmt(
            R.string.mail_import_done_summary_fmt,
            snapshot.importedCount, snapshot.skippedCount, snapshot.erroredCount,
        ),
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.testTag(Ids.MAIL_IMPORT_DONE_SUMMARY),
    )
    // Neither button is backed by a MailImportAction — the shared machine never
    // wired a "view inbox" / "skip log" RPC. Both leave the wizard, the tui
    // lead's own resolution; "Review skipped" cannot deep-link to a skip-log
    // page that exists nowhere yet (the log is inline on Progress instead).
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Button(onClick = onViewImported, modifier = Modifier.testTag(Ids.MAIL_IMPORT_VIEW_IMPORTED_BUTTON)) {
            Text(stringResource(R.string.mail_import_view_imported_button))
        }
        OutlinedButton(onClick = onReviewSkipped, modifier = Modifier.testTag(Ids.MAIL_IMPORT_REVIEW_SKIPPED_BUTTON)) {
            Text(stringResource(R.string.mail_import_review_skipped_button))
        }
    }
}

/** The machine's own default (§ Wizard steps step 3), in MB. */
private const val DEFAULT_MAX_SIZE_MB = "50"
