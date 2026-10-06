package com.fauna.app.ui.screen.backups

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.viewmodel.LocalRestoreVM
import com.fauna.app.ui.viewmodel.RestoreProgress
import com.fauna.ffi.FfiSnapshotSummary
import com.fauna.ffi.snapshotRestoreOptionLabel
import social.fauna.generated.Ids

/**
 * The local-restore action card on the Backups page
 * (`docs/goal/ui/backups.md` § Restore from backup destination → the
 * disaster-recovery / local-restore path). Lifts the linux lead
 * (apps/fauna-linux/src/views/backups/restore.rs `build_local_restore_action`)
 * onto Compose over the shared `fauna-client-snapshots` crate (priority #1
 * uniform; same ui.yaml IDs).
 *
 * The wired path is a **local** single-snapshot restore: pick a message-kind
 * snapshot, re-type its id (the friction bar), dispatch `restore_message_kind`
 * once. `restore-source-select` (the cross-location backup-destination picker) is
 * disabled at zero destinations; the cross-location chunk pull is fauna-sync's
 * Plan 4 (backups.md § Impl-status), so it stays informational here.
 *
 * Stateless [LocalRestoreContent] is split out for the Robolectric harness; the
 * VM-bound [LocalRestoreSection] is what the Backups page mounts.
 */
@Composable
fun LocalRestoreSection(
    vm: LocalRestoreVM = hiltViewModel(),
) {
    val snapshots by vm.snapshots.collectAsState()
    val hasDestinations by vm.hasDestinations.collectAsState()
    val progress by vm.progress.collectAsState()
    val configAbsent by vm.configAbsent.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { appMessages.showError(errorMessage) }
    LaunchedEffect(Unit) { vm.refresh() }

    LocalRestoreContent(
        snapshots = snapshots,
        hasDestinations = hasDestinations,
        progress = progress,
        configAbsent = configAbsent,
        onRestore = vm::restore,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LocalRestoreContent(
    snapshots: List<FfiSnapshotSummary>,
    hasDestinations: Boolean,
    progress: RestoreProgress,
    configAbsent: Boolean,
    onRestore: (Long, String) -> Unit,
) {
    var selectedIndex by remember(snapshots) { mutableStateOf(0) }
    var confirm by remember { mutableStateOf(TextFieldValue("")) }
    // Per-kind checkboxes — both default-checked. They do not gate the local
    // single-kind-per-snapshot restore (the mail+calendar pair-restore they drive
    // is the destination flow, blocked above); they render for parity with the
    // spec + linux (backups.md § Impl-status "Known Linux glue follow-ups").
    var mailChecked by remember { mutableStateOf(true) }
    var calendarChecked by remember { mutableStateOf(true) }

    val selected = snapshots.getOrNull(selectedIndex)
    val matchTarget = selected?.id?.toString().orEmpty()
    val confirmEnabled = selected != null &&
        confirm.text.isNotEmpty() &&
        confirm.text == matchTarget

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            stringResource(R.string.backups_restore_local_title),
            style = MaterialTheme.typography.titleMedium,
        )

        // ── restore-source-select — backup-destination picker, disabled at zero
        // destinations (backups.md § Restore line 42). The cross-location chunk
        // pull is fauna-sync Plan 4; the wired source here is the local snapshot
        // picker below.
        OutlinedTextField(
            value = if (hasDestinations) "" else stringResource(R.string.backups_backup_destinations_empty),
            onValueChange = {},
            readOnly = true,
            enabled = hasDestinations,
            label = { Text(stringResource(R.string.backups_backup_destinations_title)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RESTORE_SOURCE_SELECT),
        )

        // ── restore-snapshot-select — local message-kind snapshot picker.
        SnapshotSelect(
            snapshots = snapshots,
            selectedIndex = selectedIndex,
            onSelect = { selectedIndex = it },
        )

        // ── restore-kinds-checkboxes ──
        Row(
            modifier = Modifier.testTag(Ids.RESTORE_KINDS_CHECKBOXES),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            KindCheckbox(
                checked = mailChecked,
                onChecked = { mailChecked = it },
                label = stringResource(R.string.backups_restore_kinds_mail),
            )
            KindCheckbox(
                checked = calendarChecked,
                onChecked = { calendarChecked = it },
                label = stringResource(R.string.backups_restore_kinds_calendar),
            )
        }

        // ── restore-confirm-input — friction bar (re-type the snapshot id) ──
        OutlinedTextField(
            value = confirm,
            onValueChange = { confirm = it },
            singleLine = true,
            label = { Text(stringResource(R.string.backups_restore_confirm_placeholder)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RESTORE_CONFIRM_INPUT),
        )

        // ── restore-confirm-button — enabled iff typed id == selected id ──
        // The friction bar's re-typed id and the kind checkboxes above are
        // buffers and stay live with no nest; this button is the commit, and
        // it dispatches `fauna.filesync.snapshot.restore_message_kind`. The
        // page's own predicate is handed over rather than re-tested, so an
        // incomplete friction bar or a running restore still wins on a
        // connected nest.
        val restoreGate = faunaGate(
            "fauna.filesync.snapshot.restore_message_kind",
            enabled = confirmEnabled && progress != RestoreProgress.RUNNING,
        )
        Button(
            onClick = { selected?.let { onRestore(it.id, confirm.text) } },
            enabled = restoreGate.enabled,
            modifier = Modifier.testTag(Ids.RESTORE_CONFIRM_BUTTON),
        ) { Text(stringResource(R.string.backups_restore_confirm_button)) }
        DisabledControlReasonText(restoreGate.reason)

        // ── restore-progress ──
        Text(
            text = when (progress) {
                RestoreProgress.IDLE -> stringResource(R.string.backups_restore_progress_idle)
                RestoreProgress.RUNNING -> stringResource(R.string.backups_restore_progress_running)
                RestoreProgress.DONE -> stringResource(R.string.backups_restore_progress_done)
            },
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.RESTORE_PROGRESS),
        )
        // ── restore-warning — config_present == false: the restore succeeded
        // but the bridge can't sign in after restart until the account's
        // configuration is restored too (backups.md § Restore from backup
        // destination). Its own id, never error-message. ──
        if (configAbsent) {
            Text(
                stringResource(R.string.backups_restore_warning_config_absent),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.testTag(Ids.RESTORE_WARNING),
            )
        }
    }
}

/** `restore-snapshot-select` — a read-only dropdown of message-kind snapshots,
 *  labelled "{kind} (#{id})" (mirrors linux `populate_snapshot_select`). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SnapshotSelect(
    snapshots: List<FfiSnapshotSummary>,
    selectedIndex: Int,
    onSelect: (Int) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val empty = snapshots.isEmpty()
    val label = if (empty) {
        stringResource(R.string.backups_restore_no_snapshots)
    } else {
        snapshots.getOrNull(selectedIndex)?.let { snapshotLabel(it) }.orEmpty()
    }

    ExposedDropdownMenuBox(
        expanded = expanded && !empty,
        onExpandedChange = { if (!empty) expanded = !expanded },
    ) {
        OutlinedTextField(
            value = label,
            onValueChange = {},
            readOnly = true,
            enabled = !empty,
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded && !empty) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
                .testTag(Ids.RESTORE_SNAPSHOT_SELECT),
        )
        ExposedDropdownMenu(expanded = expanded && !empty, onDismissRequest = { expanded = false }) {
            snapshots.forEachIndexed { index, snap ->
                DropdownMenuItem(
                    text = { Text(snapshotLabel(snap)) },
                    onClick = {
                        onSelect(index)
                        expanded = false
                    },
                )
            }
        }
    }
}

@Composable
private fun KindCheckbox(
    checked: Boolean,
    onChecked: (Boolean) -> Unit,
    label: String,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Checkbox(
            checked = checked,
            onCheckedChange = onChecked,
            modifier = Modifier.testTag(Ids.RESTORE_KIND_CHECKBOX),
        )
        Text(label, style = MaterialTheme.typography.bodyMedium)
    }
}

private fun snapshotLabel(snap: FfiSnapshotSummary): String =
    snapshotRestoreOptionLabel(snap.messageKind, snap.id)
