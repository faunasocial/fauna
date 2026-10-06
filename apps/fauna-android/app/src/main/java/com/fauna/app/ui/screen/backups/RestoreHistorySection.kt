package com.fauna.app.ui.screen.backups

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.R
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.RestoreHistoryVM
import com.fauna.ffi.FfiRestoreDivergenceRow
import com.fauna.ffi.FfiRestoreHistoryRow
import social.fauna.generated.Ids

/**
 * The read-only restore surface on the Backups page (`docs/goal/ui/backups.md`
 * §§ Restore history / Restore divergence): a collapsible `restore-history-section`
 * listing the bearer's `restore_history` rows, each with a forensic
 * `restore-divergence-banner` (when ≥1 MUA reconnected with newer state) that
 * opens the close-only `restore-divergence-details-modal`. Lifts the linux lead
 * (apps/fauna-linux/src/views/backups/restore.rs) onto Compose over the shared
 * `fauna-client-snapshots` crate (priority #1 uniform; same ui.yaml IDs).
 *
 * The local-restore *action* (picker + friction bar) is the next slice; this
 * slice is the read side.
 *
 * Stateless [RestoreHistoryContent] is split out for the Robolectric harness; the
 * FFI-backed timestamp formatter and restore-source short-hex formatter (shared
 * `fauna_core::format::hex_short` over UniFFI) are injected so the Content stays
 * FFI-free.
 */
@Composable
fun RestoreHistorySection(
    vm: RestoreHistoryVM = hiltViewModel(),
) {
    val history by vm.history.collectAsState()
    val divergence by vm.divergence.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(errorMessage) { appMessages.showError(errorMessage) }
    LaunchedEffect(Unit) { vm.refresh() }

    RestoreHistoryContent(
        history = history,
        divergenceBySnapshot = divergence,
        // completed_at is epoch SECONDS (backups.md § Restore history); ValueFormat
        // wants millis. FFI lives here, off the testable Content.
        whenLabel = { secs -> ValueFormat.relativeTime(context, secs * 1000) },
        // Restore-source short hex via the shared `fauna_core::format::hex_short`
        // over UniFFI — injected here so the Content stays FFI-free (backups.md
        // § Where logic lives → Restore-source short hex).
        hexShort = { com.fauna.ffi.hexShort(it) },
    )
}

@Composable
fun RestoreHistoryContent(
    history: List<FfiRestoreHistoryRow>,
    divergenceBySnapshot: Map<Long, List<FfiRestoreDivergenceRow>>,
    whenLabel: (Long) -> String = { it.toString() },
    // Injected (like whenLabel) so the Content stays FFI-free: the wrapper passes
    // the shared `hex_short` over UniFFI, the test passes an FFI-free stub.
    hexShort: (ByteArray) -> String,
) {
    // Default open when ≥1 row exists, collapsed when empty (backups.md).
    var expanded by remember(history.isEmpty()) { mutableStateOf(history.isNotEmpty()) }
    var modalRows by remember { mutableStateOf<List<FfiRestoreDivergenceRow>?>(null) }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 12.dp)
            .testTag(Ids.RESTORE_HISTORY_SECTION),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .clickable { expanded = !expanded },
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text(
                stringResource(R.string.backups_restore_section_title),
                style = MaterialTheme.typography.titleMedium,
            )
            Icon(
                if (expanded) Icons.Default.ExpandLess else Icons.Default.ExpandMore,
                contentDescription = null,
            )
        }

        if (expanded) {
            Column(
                modifier = Modifier.fillMaxWidth().testTag(Ids.RESTORE_HISTORY_LIST),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                history.forEach { row ->
                    RestoreHistoryRowItem(
                        row = row,
                        divergence = divergenceBySnapshot[row.snapshotId].orEmpty(),
                        whenLabel = whenLabel,
                        hexShort = hexShort,
                        onBannerClick = { modalRows = it },
                    )
                }
            }
        }
    }

    modalRows?.let { rows ->
        DivergenceDetailsModal(rows = rows, onClose = { modalRows = null })
    }
}

/** One `restore-history-item` row + its forensic `restore-divergence-banner`. */
@Composable
private fun RestoreHistoryRowItem(
    row: FfiRestoreHistoryRow,
    divergence: List<FfiRestoreDivergenceRow>,
    whenLabel: (Long) -> String,
    hexShort: (ByteArray) -> String,
    onBannerClick: (List<FfiRestoreDivergenceRow>) -> Unit,
) {
    val source = row.sourceMemberId
        ?.let { hexShort(it) }
        ?: stringResource(R.string.backups_restore_source_local)

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.RESTORE_HISTORY_ITEM)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                stringResourceFmt(
                    R.string.backups_restore_history_row,
                    row.kindsRestored,
                    source,
                    whenLabel(row.completedAt),
                ),
                style = MaterialTheme.typography.bodyMedium,
            )
            if (divergence.isNotEmpty()) {
                Row(
                    modifier = Modifier
                        .clickable { onBannerClick(divergence) }
                        .testTag(Ids.RESTORE_DIVERGENCE_BANNER),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Icon(
                        Icons.Default.Warning,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.error,
                    )
                    Text(
                        stringResourceFmt(
                            R.string.backups_restore_divergence_banner,
                            divergence.size.toString(),
                        ),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        }
    }
}

/**
 * The forensic `restore-divergence-details-modal` — one
 * `restore-divergence-details-item` per row + the lost-writes footer. Close-only
 * (no action buttons; server state already won the restore).
 */
@Composable
private fun DivergenceDetailsModal(
    rows: List<FfiRestoreDivergenceRow>,
    onClose: () -> Unit,
) {
    val unknownMua = stringResource(R.string.backups_restore_divergence_unknown_mua)
    AlertDialog(
        onDismissRequest = onClose,
        modifier = Modifier.testTag(Ids.RESTORE_DIVERGENCE_DETAILS_MODAL),
        title = { Text(stringResource(R.string.backups_restore_divergence_modal_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                rows.forEach { d ->
                    Text(
                        stringResourceFmt(
                            R.string.backups_restore_divergence_detail_row,
                            d.collection,
                            d.muaId ?: unknownMua,
                            d.clientModseq.toString(),
                            d.serverModseq.toString(),
                            d.lostEventCount.toString(),
                        ),
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.testTag(Ids.RESTORE_DIVERGENCE_DETAILS_ITEM),
                    )
                }
                Text(
                    stringResource(R.string.backups_restore_divergence_footer),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
        confirmButton = {
            TextButton(onClick = onClose) {
                Text(stringResource(R.string.backups_restore_divergence_close))
            }
        },
    )
}
