package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.ffi.logFilterEntries
import com.fauna.ffi.logLevelForIndex
import com.fauna.ffi.logRenderedText
import com.fauna.ffi.logRows
import com.fauna.app.core.DeviceOffset
import uniffi.fauna_log.LogEntry
import uniffi.fauna_log.LogRow
import social.fauna.generated.Ids

/**
 * Shared rendering for the two Fauna log surfaces — the **Settings → Logs**
 * sub-page (the client's own `fauna-log` ring) and the **admin Logs** view (the
 * nest's ring fetched over `fauna.admin.logs`). Both render the same
 * `LogEntry` list with the same severity filter and the same indexed `log-entry`
 * rows; the only difference is the *source* (the local process-global ring vs. a
 * fetched list).
 *
 * The presentation logic — the severity filter, the one-line / row form, the time
 * format, the filter↔level map — lives ONCE in shared Rust (`fauna_log::format`,
 * exposed via `com.fauna.ffi.log_*`), so each app renders byte-identically and
 * the per-app "twin of `logs_view.rs`" is gone (priority #2/#4,
 * observability.md § Surfaces). This file holds only (a) the [renderLogs]
 * thin offset-capturing forwarder the stateful screens call (it supplies the
 * shell's local UTC offset — the "caller passes the environmental input" contract,
 * same as `relative_time`), and (b) the FFI-free row widgets that render the
 * pre-computed [LogRow]s. The android twin of linux `apps/fauna-linux/src/logs_view.rs`.
 *
 * Redaction rule (observability.md § Persistence & privacy): this layer only
 * renders what `tracing` captured; call sites are forbidden from logging message
 * plaintext or secrets. The rule is upheld at the call sites, not here.
 */

/** The rendered Logs view for a (source, filter) pair — the indexed `log-entry`
 *  rows (newest-first) plus the copy-button payload. */
data class RenderedLogs(
    val rows: List<LogRow>,
    val copyText: String,
)

/**
 * Narrow `entries` to the dropdown's severity threshold and render them through
 * the shared `fauna_log::format` exports — the rows (newest-first) + the copy
 * payload. **FFI; call from the stateful screen, never a Robolectric Content.**
 * Mirrors the windows thin forwarder (`Logs/LogsFormat.cs`).
 */
fun renderLogs(entries: List<LogEntry>, filterIndex: Int): RenderedLogs {
    // The device's local UTC offset in seconds — the environmental input the
    // shared (pure) formatter can't read itself; one door app-wide.
    val tz = DeviceOffset.utcOffsetSeconds()
    val filtered = logFilterEntries(entries, logLevelForIndex(filterIndex.toUInt()))
    return RenderedLogs(
        rows = logRows(filtered, tz),
        copyText = logRenderedText(filtered, tz),
    )
}

/** The severity filter options, index 0 = "All", 1..5 = severities — the labels
 *  for the dropdown whose index `log_level_for_index` maps to a threshold. */
@Composable
fun logFilterLabels(): List<String> = listOf(
    stringResource(R.string.logs_filter_all),
    stringResource(R.string.logs_level_error),
    stringResource(R.string.logs_level_warn),
    stringResource(R.string.logs_level_info),
    stringResource(R.string.logs_level_debug),
    stringResource(R.string.logs_level_trace),
)

/**
 * The `log-level-filter` severity picker — a native `ExposedDropdownMenuBox`
 * (ui.yaml types `log-level-filter` as `select`). The anchor's display value is
 * the selected option *label* so the e2e `actions/logs.py` `set_level` (which
 * drives `driver.select`) sees the readable value; the bridge actuates it via
 * `/element/select`. Mirrors the AdminUsers `TierDropdown`.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LogLevelFilter(
    selectedIndex: Int,
    onSelect: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    val labels = logFilterLabels()
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { expanded = !expanded },
        modifier = modifier,
    ) {
        OutlinedTextField(
            value = labels.getOrElse(selectedIndex) { labels.first() },
            onValueChange = {},
            readOnly = true,
            label = { Text(stringResource(R.string.logs_filter_label)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .widthIn(min = 160.dp)
                .testTag(Ids.LOG_LEVEL_FILTER),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            labels.forEachIndexed { index, label ->
                DropdownMenuItem(
                    text = { Text(label) },
                    onClick = {
                        expanded = false
                        onSelect(index)
                    },
                )
            }
        }
    }
}

/**
 * The indexed `log-entry` list (the pre-rendered [LogRow]s are already
 * newest-first), or `emptyText` when empty. Both Logs surfaces call this with the
 * rows their stateful screen computed via [renderLogs]. FFI-free — renders only
 * the shared row shape.
 */
@Composable
fun LogEntryList(rows: List<LogRow>, emptyText: String) {
    if (rows.isEmpty()) {
        Text(
            emptyText,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        return
    }
    Column(modifier = Modifier.fillMaxWidth()) {
        rows.forEachIndexed { index, row ->
            if (index > 0) HorizontalDivider()
            LogEntryRow(row)
        }
    }
}

/** One `log-entry` row: the shared `LEVEL · time · target` subtitle over the
 *  message (both supplied by `fauna_log::format::rows`). */
@Composable
fun LogEntryRow(row: LogRow) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 6.dp)
            .testTag(Ids.LOG_ENTRY),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Text(
            row.subtitle,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(row.message, style = MaterialTheme.typography.bodyMedium)
    }
}
