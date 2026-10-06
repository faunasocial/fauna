package com.fauna.app.ui.screen.backups

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.DeleteForever
import androidx.compose.material.icons.filled.DeleteSweep
import androidx.compose.material.icons.filled.Restore
import androidx.compose.material.icons.filled.VerifiedUser
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshContainer
import androidx.compose.material3.pulltorefresh.rememberPullToRefreshState
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.text
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.getStringFmt
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.BackupsVM
import uniffi.fauna_backups_machine.BackupOp
import uniffi.fauna_backups_machine.BackupsSnapshot
import uniffi.fauna_backups_machine.PolicyState
import uniffi.fauna_backups_machine.PrunePreview
import uniffi.fauna_backups_machine.SnapshotRow
import uniffi.fauna_backups_machine.SnapshotState
import social.fauna.generated.Ids

/**
 * The Backups page's snapshot half, rendered off the shared `BackupsMachine`
 * (`ui/backups.md` § Snapshot-list shape). This screen paints
 * `BackupsVM.snapshot` and dispatches gestures; it holds no page logic.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SnapshotListScreen(
    navController: NavController,
    vm: BackupsVM = hiltViewModel()
) {
    val snap by vm.snapshot.collectAsState()
    val context = LocalContext.current
    val immediateDeleteTargetId by vm.immediateDeleteTargetId.collectAsState()
    val appMessages = LocalAppMessages.current

    var dropdownExpanded by remember { mutableStateOf(false) }
    var snapshotPendingDelete by remember { mutableStateOf<Long?>(null) }

    // The machine owns the error text. A completed check with errors is a
    // RESULT, not an error (Architectural rule 6) — it renders on its own
    // surface below and never reaches this banner.
    val errorText = resolveLocalized(context, snap?.error)
    LaunchedEffect(errorText) {
        appMessages.showError(errorText)
    }

    val pullToRefreshState = rememberPullToRefreshState()
    if (pullToRefreshState.isRefreshing) {
        LaunchedEffect(true) {
            vm.refresh()
            pullToRefreshState.endRefresh()
        }
    }

    LaunchedEffect(Unit) { vm.start() }

    // Single-flight (§ *Create* ruling): while an op is in flight EVERY mutating
    // control is disabled. One predicate so no control can drift off it — the
    // FAB included, which used to stay live through its own create.
    val busy = snap?.inProgressOp != null
    val armed = !busy && snap?.selectedFolder != null

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.backups_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING)
                    )
                },
                actions = {
                    IconButton(
                        onClick = { vm.prunePreview() },
                        enabled = armed,
                        modifier = Modifier.testTag(Ids.SNAPSHOT_PRUNE_BUTTON)
                    ) {
                        Icon(
                            Icons.Default.DeleteSweep,
                            contentDescription = stringResource(R.string.backups_prune_snapshots)
                        )
                    }
                    IconButton(
                        onClick = { vm.checkIntegrity() },
                        enabled = armed,
                        modifier = Modifier.testTag(Ids.SNAPSHOT_CHECK_BUTTON)
                    ) {
                        Icon(
                            Icons.Default.VerifiedUser,
                            contentDescription = stringResource(R.string.backups_verify_integrity)
                        )
                    }
                }
            )
        },
        floatingActionButton = {
            FloatingActionButton(
                onClick = { if (armed) vm.createSnapshot() },
                modifier = Modifier.testTag(Ids.SNAPSHOT_CREATE_BUTTON)
            ) {
                Icon(Icons.Default.Add, stringResource(R.string.backups_create_snapshot))
            }
        }
    ) { padding ->
        Box(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
                .nestedScroll(pullToRefreshState.nestedScrollConnection)
        ) {
            Column(modifier = Modifier.fillMaxSize()) {

                val folders = snap?.folders.orEmpty()
                // The folder selector. ONE element carries the id — the empty
                // state used to tag its placeholder Text as well, so the page
                // painted `backup-folder-selector` twice and an indexed read
                // could land on either.
                ExposedDropdownMenuBox(
                    expanded = dropdownExpanded && folders.isNotEmpty(),
                    onExpandedChange = { if (folders.isNotEmpty()) dropdownExpanded = !dropdownExpanded },
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)
                ) {
                    OutlinedTextField(
                        value = snap?.selectedFolder ?: "",
                        onValueChange = {},
                        readOnly = true,
                        enabled = folders.isNotEmpty() && !busy,
                        label = { Text(stringResource(R.string.backups_folder)) },
                        // A disabled control the user can see states why (Copy
                        // comprehensibility rule 5).
                        supportingText = if (folders.isEmpty()) {
                            { Text(stringResource(R.string.common_no_folders_configured)) }
                        } else null,
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = dropdownExpanded) },
                        modifier = Modifier
                            .menuAnchor()
                            .fillMaxWidth()
                            .testTag(Ids.BACKUP_FOLDER_SELECTOR)
                    )
                    ExposedDropdownMenu(
                        expanded = dropdownExpanded && folders.isNotEmpty(),
                        onDismissRequest = { dropdownExpanded = false }
                    ) {
                        folders.forEach { fs ->
                            DropdownMenuItem(
                                text = { Text(fs.name) },
                                onClick = {
                                    vm.selectFolder(fs.name)
                                    dropdownExpanded = false
                                }
                            )
                        }
                    }
                }

                // `last-backed-up` — ONE non-indexed element, ALWAYS painted: the
                // selected set's newest snapshot, or "never". It used to render
                // only when a row existed and to derive itself from the cached
                // rows, so an empty set silently kept the previous set's line.
                Text(
                    text = snap?.lastBackedUp?.let {
                        stringResourceFmt(
                            R.string.backups_last_backed_up_at,
                            // ⚠ epoch SECONDS on the wire, milliseconds here — the
                            // pre-machine code passed them straight through, so every
                            // backups timestamp on android rendered as a 1970 date.
                            ValueFormat.relativeTime(context, it * 1000L)
                        )
                    } ?: stringResource(R.string.backups_last_backed_up_never),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier
                        .padding(horizontal = 16.dp, vertical = 4.dp)
                        .testTag(Ids.LAST_BACKED_UP)
                )

                // Names which op is in flight, so the disabled action row above
                // is not left unexplained.
                snap?.inProgressOp?.let { op ->
                    Caption(busyText(context, op))
                }

                CheckResultLine(snap)
                PrunePreviewSection(snap, vm)

                // `backup-audit-alert` banners (backups.md § Audit-alert surface),
                // mounted OUTSIDE/ABOVE the `snapshot-list` scroller below: a
                // warning that a backup is falling behind must be visible without
                // scrolling past the snapshot list to find it (mirrors linux/web).
                BackupAuditAlerts()

                LazyColumn(modifier = Modifier.fillMaxSize().testTag(Ids.SNAPSHOT_LIST)) {
                    // Backup-destination management section (backups.md § Manage
                    // backup destinations) — add/edit/remove + status rows, above
                    // the snapshot list so the page scrolls as one.
                    item { BackupDestinationsSection() }
                    item { HorizontalDivider() }
                    // Wire order (newest-first) — the app does not re-sort.
                    items(snap?.snapshots.orEmpty()) { row ->
                        ListItem(
                            headlineContent = {
                                Text(stringResourceFmt(R.string.backups_snapshot, row.id))
                            },
                            supportingContent = {
                                Column {
                                    Text(snapshotRowText(context, row))
                                }
                            },
                            trailingContent = {
                                Row {
                                    // Recovery is offered ONLY out of `SoftDeleted`
                                    // (backups.md § *Soft-deleted rows*) — the
                                    // control's PRESENCE is the row-state observable,
                                    // the same shape as `snapshot-prune-execute-button`.
                                    // The machine refuses the gesture for any other
                                    // row, so this render guard is the affordance
                                    // rule, never the enforcement.
                                    if (row.state is SnapshotState.SoftDeleted) {
                                        IconButton(
                                            onClick = { vm.undeleteSnapshot(row.id) },
                                            enabled = !busy,
                                            modifier = Modifier.testTag(Ids.SNAPSHOT_UNDELETE_BUTTON)
                                        ) {
                                            Icon(
                                                Icons.Default.Restore,
                                                contentDescription = stringResource(R.string.backups_snapshot_undelete_button)
                                            )
                                        }
                                    }
                                    // Immediate-delete — sibling of the soft-delete
                                    // button (backups.md § Element IDs). NEVER a
                                    // one-click action: it only OPENS the friction-bar
                                    // modal (Architectural rule 4).
                                    IconButton(
                                        onClick = { vm.openImmediateDelete(row.id) },
                                        enabled = !busy,
                                        modifier = Modifier.testTag(Ids.SNAPSHOT_IMMEDIATE_DELETE_BUTTON)
                                    ) {
                                        Icon(
                                            Icons.Default.DeleteForever,
                                            contentDescription = stringResource(R.string.backups_immediate_delete_button)
                                        )
                                    }
                                    IconButton(
                                        onClick = { snapshotPendingDelete = row.id },
                                        enabled = !busy,
                                        modifier = Modifier.testTag(Ids.SNAPSHOT_DELETE_BUTTON)
                                    ) {
                                        Icon(
                                            Icons.Default.Delete,
                                            contentDescription = stringResource(R.string.backups_delete_snapshot)
                                        )
                                    }
                                }
                            },
                            modifier = Modifier
                                .testTag(Ids.SNAPSHOT_ITEM)
                                .clickable {
                                    vm.openSnapshot(row.id)
                                    navController.navigate("snapshot/${row.id}")
                                }
                        )
                        HorizontalDivider()
                    }
                    if (snap?.snapshots.orEmpty().isEmpty() && snap?.selectedFolder != null) {
                        item {
                            ListItem(
                                headlineContent = { Text(stringResource(R.string.backups_no_snapshots)) },
                                supportingContent = { Text(stringResource(R.string.backups_no_snapshots_desc)) }
                            )
                        }
                    }

                    // Restore surface (backups.md §§ Restore from backup
                    // destination / Restore history / Restore divergence), below
                    // the snapshot list so the page scrolls as one: the
                    // local-restore action card first, then the read-only history
                    // + forensic divergence (mirrors linux build_restore_section).
                    item { HorizontalDivider() }
                    item { LocalRestoreSection() }
                    item { HorizontalDivider() }
                    item { RestoreHistorySection() }
                }
            }

            if (snap?.inProgressOp == BackupOp.REFRESH && snap?.snapshots.orEmpty().isEmpty()) {
                CircularProgressIndicator(modifier = Modifier.align(Alignment.Center))
            }
            PullToRefreshContainer(
                state = pullToRefreshState,
                modifier = Modifier.align(Alignment.TopCenter)
            )
        }
    }

    // Delete confirmation (client glue — the apps that ship one keep theirs).
    snapshotPendingDelete?.let { id ->
        AlertDialog(
            onDismissRequest = { snapshotPendingDelete = null },
            title = { Text(stringResource(R.string.backups_delete_snapshot)) },
            text = { Text(stringResource(R.string.backups_delete_snapshot_confirm)) },
            confirmButton = {
                TextButton(onClick = {
                    vm.deleteSnapshot(id)
                    snapshotPendingDelete = null
                }) { Text(stringResource(R.string.common_delete)) }
            },
            dismissButton = {
                TextButton(onClick = { snapshotPendingDelete = null }) {
                    Text(stringResource(R.string.common_cancel))
                }
            }
        )
    }

    // Immediate-delete friction-bar modal (backups.md § User actions,
    // Architectural rule 4) — NEVER a one-click affordance: the confirm binds the
    // MACHINE's predicate, which threads the real in-flight flag. The VM closes
    // the modal when the row leaves the machine's list, so the nest's
    // hard_floor_breach rejection leaves it standing with the error surfaced.
    immediateDeleteTargetId?.let { id ->
        val ackText = remember { vm.immediateDeleteAckText() }
        ImmediateDeleteConfirmModal(
            snapshotId = id,
            ackText = ackText,
            busy = busy,
            confirmEnabled = { typedId, typedAck -> vm.immediateDeleteEnabled(typedId, id.toString(), typedAck) },
            onConfirm = { typedId, typedAck -> vm.deleteSnapshotImmediate(id, typedId, typedAck) },
            onCancel = { vm.cancelImmediateDelete() },
        )
    }
}

/**
 * The immediate-delete friction bar (`backups.md` § User actions, Architectural
 * rule 4) — NEVER a one-click affordance.
 *
 * Split out of [SnapshotListScreen] so the offline gate on its confirm is
 * provable under Robolectric without a Hilt VM, the same move
 * `AccountSettingsScreen` and `ModerationQueueScreen` needed this session. The
 * machine's predicate is passed as [confirmEnabled] rather than a plain Boolean
 * so the typed-in-the-moment values still reach it exactly as they did inline.
 */
@Composable
internal fun ImmediateDeleteConfirmModal(
    snapshotId: Long,
    ackText: String,
    busy: Boolean,
    confirmEnabled: (String, String) -> Boolean,
    onConfirm: (String, String) -> Unit,
    onCancel: () -> Unit,
) {
    var confirmId by remember(snapshotId) { mutableStateOf(TextFieldValue("")) }
    var acknowledge by remember(snapshotId) { mutableStateOf(TextFieldValue("")) }
    val enabled = confirmEnabled(confirmId.text, acknowledge.text)
    AlertDialog(
        onDismissRequest = onCancel,
        modifier = Modifier.testTag(Ids.IMMEDIATE_DELETE_CONFIRM_MODAL),
        title = { Text(stringResourceFmt(R.string.backups_immediate_delete_modal_title, snapshotId)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(stringResource(R.string.backups_immediate_delete_warning))
                OutlinedTextField(
                    value = confirmId,
                    onValueChange = { confirmId = it },
                    singleLine = true,
                    label = { Text(stringResource(R.string.backups_immediate_delete_confirm_id_placeholder)) },
                    modifier = Modifier.fillMaxWidth().testTag(Ids.IMMEDIATE_DELETE_CONFIRM_INPUT),
                )
                Text(stringResource(R.string.backups_immediate_delete_acknowledge_prompt))
                // The exact phrase to type, shown for the user to copy.
                Text(ackText, style = MaterialTheme.typography.bodySmall)
                OutlinedTextField(
                    value = acknowledge,
                    onValueChange = { acknowledge = it },
                    singleLine = true,
                    label = { Text(stringResource(R.string.backups_immediate_delete_acknowledge_placeholder)) },
                    modifier = Modifier.fillMaxWidth().testTag(Ids.IMMEDIATE_DELETE_ACKNOWLEDGE_INPUT),
                )
            }
        },
        confirmButton = {
            // Arming is local: `snapshot-immediate-delete-button` opens this
            // friction bar and stays live. The confirm is the commit, and its
            // predicate comes from the MACHINE (the typed id + acknowledgement)
            // — handed over, never replaced, so the friction bar still governs
            // and the gate only composes with it.
            val immediateGate = faunaGate(
                "fauna.filesync.snapshot.delete_immediate",
                enabled = enabled,
            )
            Column {
                TextButton(
                    onClick = { onConfirm(confirmId.text, acknowledge.text) },
                    enabled = immediateGate.enabled,
                    modifier = Modifier.testTag(Ids.IMMEDIATE_DELETE_CONFIRM_BUTTON),
                ) { Text(stringResource(R.string.backups_immediate_delete_confirm_button)) }
                DisabledControlReasonText(immediateGate.reason)
            }
        },
        dismissButton = {
            TextButton(
                onClick = onCancel,
                enabled = !busy,
                modifier = Modifier.testTag(Ids.IMMEDIATE_DELETE_CANCEL_BUTTON),
            ) { Text(stringResource(R.string.backups_immediate_delete_cancel_button)) }
        }
    )
}

/** The check verdict — the shared `is_ok` predicate, **called**, never re-derived
 *  from a status string or from the error counts. */
@Composable
internal fun CheckResultLine(snap: BackupsSnapshot?) {
    val result = snap?.checkResult ?: return
    // `snapshot-check-result` — present only while a verdict stands (the early
    // return above), and never routed to `error-message` (rule 6).
    Caption(
        modifier = Modifier.testTag(Ids.SNAPSHOT_CHECK_RESULT),
        text = if (result.isOk) {
            stringResourceFmt(
                R.string.backups_check_result_ok,
                result.snapshotsChecked,
                result.filesChecked,
                result.chunksChecked,
            )
        } else {
            stringResourceFmt(
                R.string.backups_check_result_errors,
                result.missingManifests,
                result.missingChunks,
                result.corruptManifests,
            )
        }
    )
}

/** The prune dry-run surface. Execute is offered ONLY from here, and the two
 *  no-op policy states say *why* nothing would be pruned rather than showing an
 *  empty success. */
@Composable
private fun PrunePreviewSection(snap: BackupsSnapshot?, vm: BackupsVM) {
    val preview = snap?.prunePreview ?: return
    PrunePreviewContent(
        preview = preview,
        busy = snap.inProgressOp != null,
        onExecute = { vm.pruneExecute() },
        onCancel = { vm.cancelPrunePreview() },
    )
}

/** The stateless half — the `BackupAuditAlertsContent` idiom, so the render
 *  contract (which ids appear in which state) is provable in a Robolectric
 *  Compose test rather than only on a device the host emulator setup gates.
 *  The whole point of these ids is to make the dry-run/execute distinction
 *  observable; an unobservable-here implementation of them would be a poor joke. */
@Composable
internal fun PrunePreviewContent(
    preview: PrunePreview,
    busy: Boolean,
    onExecute: () -> Unit,
    onCancel: () -> Unit,
) {
    val context = LocalContext.current
    val title = stringResource(R.string.backups_prune_preview_title)
    val verdict = when {
        preview.policyState == PolicyState.NOT_SET ->
            stringResource(R.string.backups_prune_policy_not_set)
        preview.policyState == PolicyState.UNPARSEABLE ->
            stringResource(R.string.backups_prune_policy_unparseable)
        preview.candidates.isEmpty() ->
            stringResource(R.string.backups_prune_preview_nothing)
        else -> stringResourceFmt(
            R.string.backups_prune_preview_counts,
            preview.wouldPrune,
            preview.remaining,
        )
    }
    // `snapshot-prune-preview` — the standing dry run. The caller's early return
    // is what makes its presence the state: no preview, no element.
    //
    // The VERDICT rides the id'd element's OWN text, beside the title (tui's
    // `prune_preview_elements` shape): "nothing to prune" and "no retention
    // policy configured for this set" both stand a preview and both offer no
    // execute, so only the text can say which one the nest returned
    // (`ui/backups.md` § Errors & edge cases — *Prune with no candidates*). A
    // tagged Column carries no text of its own, so the bridge read "" and the two
    // states were indistinguishable. Declared here rather than by merging
    // descendants, which would fold the two buttons' tags into this node.
    Column(
        modifier = Modifier
            .padding(horizontal = 16.dp, vertical = 4.dp)
            .testTag(Ids.SNAPSHOT_PRUNE_PREVIEW)
            .semantics { text = AnnotatedString("$title  $verdict") },
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Text(title, style = MaterialTheme.typography.titleSmall)
        Caption(verdict)
        // The candidate list stays untagged under the preview: ui.yaml scopes the
        // rows no id of their own, and the verdict above already carries the counts.
        if (preview.policyState == PolicyState.APPLIED) {
            preview.candidates.forEach { candidate ->
                Caption(
                    stringResourceFmt(
                        R.string.backups_prune_preview_candidate,
                        candidate.id,
                        ValueFormat.relativeTime(context, candidate.createdAt * 1000L),
                    )
                )
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            // An armed execute over zero candidates would promise an effect it
            // cannot have.
            if (preview.policyState == PolicyState.APPLIED &&
                preview.candidates.isNotEmpty() &&
                !busy
            ) {
                val pruneGate = faunaGate("fauna.filesync.snapshot.prune_set_policy")
                TextButton(
                    onClick = onExecute,
                    enabled = pruneGate.enabled,
                    modifier = Modifier.testTag(Ids.SNAPSHOT_PRUNE_EXECUTE_BUTTON),
                ) {
                    Text(stringResource(R.string.backups_prune_execute_button))
                }
                DisabledControlReasonText(pruneGate.reason)
            }
            TextButton(
                onClick = onCancel,
                modifier = Modifier.testTag(Ids.SNAPSHOT_PRUNE_CANCEL_BUTTON),
            ) {
                Text(stringResource(R.string.backups_prune_cancel_button))
            }
        }
    }
}

@Composable
private fun Caption(text: String, modifier: Modifier = Modifier) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = modifier.padding(horizontal = 16.dp, vertical = 2.dp),
    )
}

/** Every `BackupOp` names itself on the busy line via the shared
 *  `fauna_backups_machine::busy_text` (`com.fauna.ffi.busyText`) — no app hand-
 *  rolls which key an operation maps to (`docs/goal/ui/backups.md` § Where
 *  logic lives). */
internal fun busyText(context: android.content.Context, op: BackupOp): String =
    resolveLocalized(context, com.fauna.ffi.busyText(op)).orEmpty()

/**
 * The visible row text: file count + formatted size + time, plus the non-`Active`
 * lifecycle state with the deadline the user can still act on, and — once a check
 * has run this session — the derived integrity verdict.
 *
 * The lifecycle + integrity suffixes route through the shared
 * `fauna_backups_machine::{snapshot_state_text, snapshot_integrity_text}` (via
 * `com.fauna.ffi`) — this screen owns only the timestamp formatting
 * (`ValueFormat.relativeTime`) and which of a state's own fields carries the
 * deadline; the shared fns own which i18n key and the dated/undated fallback
 * (`docs/goal/ui/backups.md` § Where logic lives). `Unknown` integrity paints
 * NOTHING (tui's ruling, inherited): the word "unknown" on this page would
 * read as a finding.
 */
internal fun snapshotRowText(context: android.content.Context, row: SnapshotRow): String {
    val sb = StringBuilder()
    sb.append(context.getStringFmt(R.string.backups_file_count, row.fileCount))
    sb.append(", ")
    sb.append(ValueFormat.byteSize(context, row.totalBytes))
    sb.append(" · ")
    // Epoch SECONDS on the wire; `relativeTime` takes milliseconds.
    sb.append(ValueFormat.relativeTime(context, row.createdAt * 1000L))
    val deadline = when (val state = row.state) {
        is SnapshotState.Active -> null
        is SnapshotState.DeletionPending -> state.executeAfter
        is SnapshotState.SoftDeleted -> state.purgeAfter
    }
    val formattedDeadline = deadline?.let { ValueFormat.relativeTime(context, it * 1000L) }
    resolveLocalized(context, com.fauna.ffi.snapshotStateText(row.state, formattedDeadline))?.let {
        sb.append("  ")
        sb.append(it)
    }
    resolveLocalized(context, com.fauna.ffi.snapshotIntegrityText(row.integrity))?.let {
        sb.append("  ")
        sb.append(it)
    }
    return sb.toString()
}
