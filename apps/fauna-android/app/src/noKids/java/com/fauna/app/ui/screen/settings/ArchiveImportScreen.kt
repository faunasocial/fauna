package com.fauna.app.ui.screen.settings

import android.content.Context
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
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
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.UrlOpener
import com.fauna.app.ui.components.TokenSelect
import com.fauna.app.ui.components.rememberTwoClickArm
import com.fauna.app.ui.util.resolveComposed
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.ArchiveImportVM
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import social.fauna.generated.Ids
import uniffi.fauna_archive_import_machine.ArchiveImportSnapshot
import uniffi.fauna_archive_import_machine.ArchiveImportStep
import uniffi.fauna_archive_import_machine.ArchiveSourceKind
import uniffi.fauna_archive_import_machine.ArchiveImportPageText
import uniffi.fauna_archive_import_machine.AudienceMode
import uniffi.fauna_archive_import_machine.CategoryRow
import uniffi.fauna_archive_import_machine.RunState
import uniffi.fauna_archive_import_machine.archiveImportAudienceModeLabel
import uniffi.fauna_archive_import_machine.archiveImportPageText
import uniffi.fauna_archive_import_machine.archiveImportSourceHelp
import uniffi.fauna_archive_import_machine.archiveImportSourceKindLabel
import java.io.File

/**
 * The user-facing **archive-import** wizard — Settings → Import from other
 * services (`docs/goal/behavior/archive-import.md` § The wizard and its
 * machine). A dumb renderer of `ArchiveImportSnapshot` and a dispatcher of
 * `ArchiveImportAction` over the shared machine ([ArchiveImportVM]); every word
 * shown for a token — a platform, its help, a category, a run state, an
 * audience mode — comes from the machine crate's shared vocabulary fns, never
 * a Kotlin re-spelling. Element IDs match `tests/e2e-unified/ui.yaml`
 * `archive-import` exactly. Lifts the tui lead page and the FaunaKit
 * `ArchiveImportView`; the [MailImportScreen] shape (stateless
 * [ArchiveImportContent] for the Compose test harness, the VM-bound wrapper the
 * NavHost mounts).
 *
 * - **Exactly one step is in the tree**, which keeps the reused
 *   `wizard-back-button` / `wizard-next-button` and the two
 *   `archive-import-error-log` copies unique at any moment.
 * - **The path and date fields are page-local drafts**, committed as one
 *   ordered dispatch at Open archive / Next.
 * - **The archive is a path field**, as on windows and the two Apple apps;
 *   Open archive with an empty field opens the system document picker, whose
 *   pick is copied into the app's own cache (the machine reads a file path,
 *   and the run reads the archive again while it imports) and opened.
 * - **A category toggle echoes synchronously**, so a `state` read right after
 *   the click describes that click (the FaunaKit `pendingSelection` reasoning).
 * - **The profile prefill is offered, never applied silently**: enabled while
 *   the snapshot carries the archive's profile and the tap has not applied it.
 */
@Composable
fun ArchiveImportScreen(
    navController: NavController,
    vm: ArchiveImportVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val unavailable by vm.unavailable.collectAsState()
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var pickedPath by remember { mutableStateOf<String?>(null) }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            val path = withContext(Dispatchers.IO) { copyPickedArchive(context, uri) }
            if (path != null) {
                pickedPath = path
                vm.openArchive(path)
            }
        }
    }

    ArchiveImportContent(
        snapshot = snapshot,
        errorMessage = errorMessage,
        unavailable = unavailable,
        pickedPath = pickedPath,
        labels = remember(context) { ArchiveImportLabels.of(context) },
        onBack = { navController.popBackStack() },
        onSelectSource = vm::selectSource,
        onOpenExportPage = { url -> UrlOpener.open(context, url) },
        onOpenArchive = vm::openArchive,
        onPickArchive = { picker.launch(arrayOf("application/zip", "application/octet-stream")) },
        onToggleCategory = vm::toggleCategory,
        onSetAudienceMode = vm::setAudienceMode,
        onScopeNext = vm::scopeNext,
        onNext = vm::next,
        onPrev = vm::back,
        onStart = vm::start,
        onPause = vm::pause,
        onResume = vm::resume,
        onCancel = vm::cancel,
        onApplyProfile = vm::applyProfile,
        onViewImported = { vm.viewImported { navController.navigate("feed") } },
        onOpenFolders = { navController.navigate("settings/folders") },
    )
}

/** Copy a picked archive into `cacheDir/archive-import/`, keeping its name;
 *  `null` when the provider gives no bytes. */
private fun copyPickedArchive(context: Context, uri: Uri): String? {
    val name = uri.lastPathSegment?.substringAfterLast('/')?.takeIf { it.isNotBlank() } ?: "archive.zip"
    val dir = File(context.cacheDir, "archive-import").apply { mkdirs() }
    val out = File(dir, name)
    val input = context.contentResolver.openInputStream(uri) ?: return null
    input.use { src -> out.outputStream().use { dst -> src.copyTo(dst) } }
    return out.absolutePath
}

/**
 * The page's token words, resolved off the shared Rust vocabulary fns — FFI
 * lives here, off the testable Content (the [MailImportScreen] shape); the
 * Compose test hands in fakes.
 */
class ArchiveImportLabels(
    val sourceKind: (ArchiveSourceKind) -> String,
    val sourceHelp: (ArchiveSourceKind) -> String,
    val audienceMode: (AudienceMode) -> String,
    val pageText: (ArchiveImportSnapshot) -> ArchiveImportLines,
) {
    companion object {
        fun of(context: Context) = ArchiveImportLabels(
            sourceKind = { archiveImportSourceKindLabel(it) },
            sourceHelp = { resolveLocalized(context, archiveImportSourceHelp(it)).orEmpty() },
            audienceMode = { resolveLocalized(context, archiveImportAudienceModeLabel(it)).orEmpty() },
            pageText = { ArchiveImportLines.of(context, archiveImportPageText(it)) },
        )
    }
}

/** One render's composed lines, resolved to strings — the shared
 *  `ArchiveImportPageText` record with each `ComposedText` run through
 *  [resolveComposed]. `scopeRows` runs parallel to `snapshot.categories`;
 *  `progressRows` is already the selected, importable categories. */
class ArchiveImportLines(
    val sourceLink: String,
    val archiveSummary: String?,
    val scopeRows: List<String>,
    val audienceSummary: String,
    val confirmSummary: String,
    val progressSummary: String,
    val progressFraction: Double,
    val progressRows: List<Pair<String, String>>,
    val doneSummary: String,
    val folderLink: String,
) {
    companion object {
        fun of(context: Context, t: ArchiveImportPageText) = ArchiveImportLines(
            sourceLink = resolveComposed(context, t.sourceLink),
            archiveSummary = t.archiveSummary?.let { resolveComposed(context, it) },
            scopeRows = t.scopeRows.map { resolveComposed(context, it) },
            audienceSummary = resolveComposed(context, t.audienceSummary),
            confirmSummary = resolveComposed(context, t.confirmSummary),
            progressSummary = resolveComposed(context, t.progressSummary),
            progressFraction = t.progressFraction,
            progressRows = t.progressRows.map {
                (resolveLocalized(context, it.label).orEmpty()) to resolveComposed(context, it.progress)
            },
            doneSummary = resolveComposed(context, t.doneSummary),
            folderLink = resolveComposed(context, t.folderLink),
        )
    }
}

private val SOURCE_KINDS = listOf(ArchiveSourceKind.FACEBOOK, ArchiveSourceKind.INSTAGRAM)
private val AUDIENCE_MODES = listOf(AudienceMode.ORIGINAL, AudienceMode.ONLY_ME)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ArchiveImportContent(
    snapshot: ArchiveImportSnapshot?,
    errorMessage: String?,
    unavailable: Boolean,
    pickedPath: String?,
    labels: ArchiveImportLabels,
    onBack: () -> Unit,
    onSelectSource: (ArchiveSourceKind) -> Unit,
    onOpenExportPage: (String) -> Unit,
    onOpenArchive: (String) -> Unit,
    onPickArchive: () -> Unit,
    onToggleCategory: (String, Boolean) -> Unit,
    onSetAudienceMode: (AudienceMode) -> Unit,
    onScopeNext: (String, String) -> Unit,
    onNext: () -> Unit,
    onPrev: () -> Unit,
    onStart: () -> Unit,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onCancel: () -> Unit,
    onApplyProfile: () -> Unit,
    onViewImported: () -> Unit,
    onOpenFolders: () -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.archive_import_title),
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
            Text(
                stringResource(R.string.archive_import_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (unavailable) {
                Text(stringResource(R.string.archive_import_unavailable))
            } else if (snapshot != null) {
                // Every composed line of the page, once per snapshot.
                val lines = remember(snapshot) { labels.pageText(snapshot) }
                when (snapshot.step) {
                    ArchiveImportStep.SOURCE -> SourceStep(snapshot, labels, lines, onSelectSource, onOpenExportPage, onNext)
                    ArchiveImportStep.ARCHIVE -> ArchiveStep(snapshot, lines, pickedPath, onOpenArchive, onPickArchive, onNext, onPrev)
                    ArchiveImportStep.SCOPE -> ScopeStep(snapshot, labels, lines, onToggleCategory, onSetAudienceMode, onScopeNext, onPrev)
                    ArchiveImportStep.CONFIRM -> ConfirmStep(lines, onStart, onPrev)
                    ArchiveImportStep.PROGRESS -> ProgressStep(snapshot, lines, onPause, onResume, onCancel, onPrev)
                    ArchiveImportStep.DONE -> DoneStep(snapshot, lines, onApplyProfile, onViewImported, onOpenFolders, onPrev)
                }
            }
            // Built only when there is something to say (convention 2's rider):
            // the bridge reads visibility as existence in the semantics tree.
            if (!errorMessage.isNullOrEmpty()) {
                Text(
                    errorMessage,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
                )
            }
        }
    }
}

// ── step 1 — source ─────────────────────────────────────────────────────

@Composable
private fun SourceStep(
    snapshot: ArchiveImportSnapshot,
    labels: ArchiveImportLabels,
    lines: ArchiveImportLines,
    onSelectSource: (ArchiveSourceKind) -> Unit,
    onOpenExportPage: (String) -> Unit,
    onNext: () -> Unit,
) {
    // The picker echoes the pick at once; the machine's answer replaces it.
    var pending by remember { mutableStateOf<ArchiveSourceKind?>(null) }
    LaunchedEffect(snapshot.sourceKind) { pending = null }
    val kind = pending ?: snapshot.sourceKind
    Text(stringResource(R.string.archive_import_source_title), style = MaterialTheme.typography.titleMedium)
    TokenSelect(
        testTagValue = Ids.ARCHIVE_IMPORT_SOURCE_PICKER,
        selected = kind.name,
        options = SOURCE_KINDS.map { it.name to labels.sourceKind(it) },
        onSelect = { token ->
            SOURCE_KINDS.firstOrNull { it.name == token }?.let {
                pending = it
                onSelectSource(it)
            }
        },
    )
    Text(
        labels.sourceHelp(kind),
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_SOURCE_HELP),
    )
    // The chosen platform's export page — the snapshot's derived address, a
    // shared constant; activating it hands exactly that address to the opener.
    val url = snapshot.sourceExportUrl
    TextButton(
        onClick = { if (url.isNotEmpty()) onOpenExportPage(url) },
        modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_SOURCE_EXPORT_LINK),
    ) { Text(lines.sourceLink) }
    NextButton(onNext)
}

// ── step 2 — archive ────────────────────────────────────────────────────

@Composable
private fun ArchiveStep(
    snapshot: ArchiveImportSnapshot,
    lines: ArchiveImportLines,
    pickedPath: String?,
    onOpenArchive: (String) -> Unit,
    onPickArchive: () -> Unit,
    onNext: () -> Unit,
    onPrev: () -> Unit,
) {
    // Seeded once from the snapshot (a hydrate that found an unfinished
    // import names its archive again), never over what was typed.
    var path by remember { mutableStateOf(snapshot.archivePath) }
    LaunchedEffect(pickedPath) { if (pickedPath != null) path = pickedPath }
    Text(stringResource(R.string.archive_import_archive_title), style = MaterialTheme.typography.titleMedium)
    OutlinedTextField(
        value = path,
        onValueChange = { path = it },
        label = { Text(stringResource(R.string.archive_import_archive_path_placeholder)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().testTag(Ids.ARCHIVE_IMPORT_ARCHIVE_PATH),
    )
    OutlinedButton(
        onClick = { if (path.isBlank()) onPickArchive() else onOpenArchive(path) },
        modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_ARCHIVE_OPEN_BUTTON),
    ) { Text(stringResource(R.string.archive_import_archive_open_button)) }
    lines.archiveSummary?.let { summary ->
        Text(
            summary,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_ARCHIVE_SUMMARY),
        )
    }
    BackNextRow(onPrev, onNext)
}

// ── step 3 — scope ──────────────────────────────────────────────────────

@Composable
private fun ScopeStep(
    snapshot: ArchiveImportSnapshot,
    labels: ArchiveImportLabels,
    lines: ArchiveImportLines,
    onToggleCategory: (String, Boolean) -> Unit,
    onSetAudienceMode: (AudienceMode) -> Unit,
    onScopeNext: (String, String) -> Unit,
    onPrev: () -> Unit,
) {
    var dateFrom by remember { mutableStateOf(snapshot.dateFrom) }
    var dateTo by remember { mutableStateOf(snapshot.dateTo) }
    // The synchronous echo of a toggle, dropped once the machine's own
    // answer for that row arrives (the ToggleCategory action carries the
    // wanted value, so the echo never disagrees with the outcome).
    val pending = remember { mutableStateMapOf<String, Boolean>() }
    LaunchedEffect(snapshot.categories) {
        snapshot.categories.forEach { row -> if (pending[row.token] == row.selected) pending.remove(row.token) }
    }
    Text(stringResource(R.string.archive_import_scope_title), style = MaterialTheme.typography.titleMedium)
    Column(modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_SCOPE_CATEGORIES)) {
        Text(stringResource(R.string.archive_import_scope_categories_label), style = MaterialTheme.typography.bodyMedium)
        snapshot.categories.forEachIndexed { i, row ->
            CategoryItem(row, lines.scopeRows[i], pending[row.token] ?: row.selected) { next ->
                pending[row.token] = next
                onToggleCategory(row.token, next)
            }
        }
    }

    val mode = snapshot.audienceMode
    Text(stringResource(R.string.archive_import_scope_audience_mode_label), style = MaterialTheme.typography.bodyMedium)
    TokenSelect(
        testTagValue = Ids.ARCHIVE_IMPORT_SCOPE_AUDIENCE_MODE,
        selected = mode.name,
        options = AUDIENCE_MODES.map { it.name to labels.audienceMode(it) },
        onSelect = { token -> AUDIENCE_MODES.firstOrNull { it.name == token }?.let(onSetAudienceMode) },
    )
    Text(
        lines.audienceSummary,
        style = MaterialTheme.typography.bodySmall,
        modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_SCOPE_AUDIENCE_SUMMARY),
    )
    if (snapshot.nestSupportsHiddenTiers == false) {
        Text(
            stringResource(R.string.archive_import_scope_hidden_tiers_unavailable),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    OutlinedTextField(
        value = dateFrom,
        onValueChange = { dateFrom = it },
        label = { Text(stringResource(R.string.archive_import_scope_date_from_placeholder)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().testTag(Ids.ARCHIVE_IMPORT_SCOPE_DATE_FROM),
    )
    OutlinedTextField(
        value = dateTo,
        onValueChange = { dateTo = it },
        label = { Text(stringResource(R.string.archive_import_scope_date_to_placeholder)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().testTag(Ids.ARCHIVE_IMPORT_SCOPE_DATE_TO),
    )
    BackNextRow(onPrev) { onScopeNext(dateFrom, dateTo) }
}

/** One indexed `archive-import-scope-category-item`: a toggle for an
 *  importable category, text for a model-only one (kept in the archive
 *  folder, never selectable); its `state` is `on` / `off` / `kept`. */
@Composable
private fun CategoryItem(
    row: CategoryRow,
    text: String,
    selected: Boolean,
    onToggle: (Boolean) -> Unit,
) {
    if (row.importable) {
        val state = if (selected) "on" else "off"
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .testTag(Ids.ARCHIVE_IMPORT_SCOPE_CATEGORY_ITEM)
                .toggleable(value = selected, role = Role.Checkbox, onValueChange = onToggle)
                .semantics { stateDescription = state },
        ) {
            Checkbox(checked = selected, onCheckedChange = null)
            Text(text)
        }
    } else {
        Text(
            text,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier
                .testTag(Ids.ARCHIVE_IMPORT_SCOPE_CATEGORY_ITEM)
                .semantics { stateDescription = "kept" },
        )
    }
}

// ── step 4 — confirm ────────────────────────────────────────────────────

@Composable
private fun ConfirmStep(
    lines: ArchiveImportLines,
    onStart: () -> Unit,
    onPrev: () -> Unit,
) {
    Text(stringResource(R.string.archive_import_confirm_title), style = MaterialTheme.typography.titleMedium)
    Text(
        lines.confirmSummary,
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_CONFIRM_SUMMARY),
    )
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        BackButton(onPrev)
        Button(onClick = onStart, modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_START_BUTTON)) {
            Text(stringResource(R.string.archive_import_start_button))
        }
    }
}

// ── step 5 — progress ───────────────────────────────────────────────────

@Composable
private fun ProgressStep(
    snapshot: ArchiveImportSnapshot,
    lines: ArchiveImportLines,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onCancel: () -> Unit,
    onPrev: () -> Unit,
) {
    Text(stringResource(R.string.archive_import_progress_title), style = MaterialTheme.typography.titleMedium)
    Text(
        lines.progressSummary,
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_PROGRESS_SUMMARY),
    )
    val fraction = lines.progressFraction
    LinearProgressIndicator(
        progress = { fraction.toFloat() },
        // The bar's filled fraction is its `fraction` attribute, as on tui.
        modifier = Modifier.fillMaxWidth()
            .testTag(Ids.ARCHIVE_IMPORT_PROGRESS_BAR)
            .semantics { stateDescription = String.format(java.util.Locale.ROOT, "%.4f", fraction) },
    )
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedButton(onClick = onPause, modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_PAUSE_BUTTON)) {
            Text(stringResource(R.string.archive_import_pause_button))
        }
        OutlinedButton(onClick = onResume, modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_RESUME_BUTTON)) {
            Text(stringResource(R.string.archive_import_resume_button))
        }
        // Two-click confirm (`common.md` § Two-click confirm): what landed is kept.
        val cancelArm = rememberTwoClickArm()
        OutlinedButton(
            onClick = { cancelArm.press(onCancel) },
            colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
            modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_CANCEL_BUTTON),
        ) {
            Text(
                stringResource(
                    if (cancelArm.armed) R.string.common_confirm_q else R.string.archive_import_cancel_button,
                ),
            )
        }
    }
    SkipLog(snapshot)
    // One row per selected importable category with its settled counts.
    Column(modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST)) {
        lines.progressRows.forEach { (name, progress) ->
            Row(modifier = Modifier.fillMaxWidth().testTag(Ids.ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST_ITEM)) {
                Text(
                    name,
                    modifier = Modifier.weight(1f).testTag(Ids.ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST_ITEM_NAME),
                )
                Text(
                    progress,
                    modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST_ITEM_PROGRESS),
                )
            }
        }
    }
    // A concluded run may be left (Back starts a fresh wizard); a running,
    // paused or errored one is resumable and has no Back.
    if (snapshot.runState == RunState.CANCELLED || snapshot.runState == RunState.COMPLETED) {
        BackButton(onPrev)
    }
}

@Composable
private fun SkipLog(snapshot: ArchiveImportSnapshot) {
    Text(
        stringResource(R.string.archive_import_error_log_title),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Text(
        snapshot.skipLog.joinToString("\n"),
        style = MaterialTheme.typography.bodySmall,
        modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_ERROR_LOG),
    )
}

// ── step 6 — done ───────────────────────────────────────────────────────

@Composable
private fun DoneStep(
    snapshot: ArchiveImportSnapshot,
    lines: ArchiveImportLines,
    onApplyProfile: () -> Unit,
    onViewImported: () -> Unit,
    onOpenFolders: () -> Unit,
    onPrev: () -> Unit,
) {
    // Review skipped is a view toggle: the skip log is already on the snapshot.
    var showSkipped by remember { mutableStateOf(false) }
    Text(stringResource(R.string.archive_import_done_title), style = MaterialTheme.typography.titleMedium)
    Text(
        lines.doneSummary,
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_DONE_SUMMARY),
    )
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Button(onClick = onViewImported, modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_VIEW_IMPORTED_BUTTON)) {
            Text(stringResource(R.string.archive_import_view_imported_button))
        }
        OutlinedButton(
            onClick = { showSkipped = !showSkipped },
            modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_REVIEW_SKIPPED_BUTTON),
        ) { Text(stringResource(R.string.archive_import_review_skipped_button)) }
    }
    if (showSkipped) SkipLog(snapshot)
    // Enabled while the run's end found a profile in the archive and the tap
    // has not applied it yet — a second tap never re-publishes it.
    OutlinedButton(
        onClick = onApplyProfile,
        enabled = snapshot.archiveProfile != null && !snapshot.profileApplied,
        modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_PROFILE_PREFILL_BUTTON),
    ) { Text(stringResource(R.string.archive_import_profile_prefill_button)) }
    TextButton(onClick = onOpenFolders, modifier = Modifier.testTag(Ids.ARCHIVE_IMPORT_FOLDER_LINK)) {
        Text(lines.folderLink)
    }
    BackButton(onPrev)
}

// ── shared buttons ──────────────────────────────────────────────────────

@Composable
private fun BackButton(onPrev: () -> Unit) {
    OutlinedButton(onClick = onPrev, modifier = Modifier.testTag(Ids.WIZARD_BACK_BUTTON)) {
        Text(stringResource(R.string.archive_import_back))
    }
}

@Composable
private fun NextButton(onNext: () -> Unit) {
    Button(onClick = onNext, modifier = Modifier.testTag(Ids.WIZARD_NEXT_BUTTON)) {
        Text(stringResource(R.string.archive_import_next))
    }
}

@Composable
private fun BackNextRow(onPrev: () -> Unit, onNext: () -> Unit) {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        BackButton(onPrev)
        NextButton(onNext)
    }
}
