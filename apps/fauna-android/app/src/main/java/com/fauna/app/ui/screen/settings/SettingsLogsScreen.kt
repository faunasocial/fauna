package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.CopyButton
import com.fauna.app.ui.components.LogEntryList
import com.fauna.app.ui.components.LogLevelFilter
import com.fauna.app.ui.components.renderLogs
import com.fauna.ffi.logClear
import com.fauna.ffi.logSnapshot
import uniffi.fauna_log.LogEntry
import uniffi.fauna_log.LogRow
import social.fauna.generated.Ids

/**
 * The Settings → Logs sub-page (`settings-logs`) — the client's durable, in-app
 * log record (observability.md § Surfaces). Renders the **process-global**
 * `fauna_log` ring (`logSnapshot()`) newest-first with a severity filter, a
 * copy-to-clipboard affordance, and a clear button. No client handle needed —
 * the ring is a process global filled by every `tracing` event, so the page
 * self-wires (priority #2: capture lives in shared `fauna-log`, this is dumb
 * rendering). The android twin of linux `settings/logs.rs`.
 *
 * The FFI ring reads live in this VM-less wrapper; the stateless
 * [SettingsLogsContent] takes the already-fetched entries + callbacks so it
 * renders under the Robolectric Compose harness (which can't load the native
 * `.so`) with seeded `LogEntry` lists.
 */
@Composable
fun SettingsLogsScreen(navController: NavController) {
    // Held source (oldest-first, as fauna_log returns it); filtered client-side
    // so the shared render is byte-identical to the admin Logs view.
    var source by remember { mutableStateOf<List<LogEntry>>(emptyList()) }
    var filterIndex by remember { mutableIntStateOf(0) }

    // Re-read the ring on entry so events captured since app start appear.
    LaunchedEffect(Unit) {
        source = runCatching { logSnapshot() }.getOrDefault(emptyList())
    }

    // Filter + render through shared fauna-log (FFI); the stateless Content takes
    // the pre-computed rows so it stays Robolectric-safe.
    val rendered = remember(source, filterIndex) { renderLogs(source, filterIndex) }

    SettingsLogsContent(
        rows = rendered.rows,
        copyText = rendered.copyText,
        filterIndex = filterIndex,
        onFilterChange = { filterIndex = it },
        onClear = {
            runCatching { logClear() }
            source = runCatching { logSnapshot() }.getOrDefault(emptyList())
        },
        onBack = { navController.popBackStack() },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsLogsContent(
    rows: List<LogRow>,
    copyText: String,
    filterIndex: Int,
    onFilterChange: (Int) -> Unit,
    onClear: () -> Unit,
    onBack: () -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.logs_title),
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
        },
    ) { padding ->
        // settings-logs — the page landmark the e2e (`app.logs.is_page_visible`)
        // waits on. error-message is the app-wide MessageBanner (no per-page dup).
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(16.dp)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .testTag(Ids.SETTINGS_LOGS),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResource(R.string.logs_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            LogLevelFilter(selectedIndex = filterIndex, onSelect = onFilterChange)

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                CopyButton(
                    testTag = Ids.LOG_COPY_BUTTON,
                    text = copyText,
                    label = stringResource(R.string.logs_copy_button),
                )
                Button(
                    onClick = onClear,
                    colors = ButtonDefaults.buttonColors(
                        containerColor = MaterialTheme.colorScheme.error,
                    ),
                    modifier = Modifier.testTag(Ids.LOG_CLEAR_BUTTON),
                ) { Text(stringResource(R.string.logs_clear_button)) }
            }

            LogEntryList(rows, emptyText = stringResource(R.string.logs_empty))
        }
    }
}
