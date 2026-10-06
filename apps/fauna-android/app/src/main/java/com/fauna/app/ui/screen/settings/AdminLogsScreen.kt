package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.CopyButton
import com.fauna.app.ui.components.LogEntryList
import com.fauna.app.ui.components.LogLevelFilter
import com.fauna.app.ui.components.renderLogs
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.viewmodel.AdminLogsVM
import uniffi.fauna_log.LogRow
import social.fauna.generated.Ids

/**
 * The admin Logs page (`admin-logs`, observability.md § Surfaces) — the nest's
 * in-memory `fauna-log` ring fetched over `fauna.admin.logs` ([AdminLogsVM]) and
 * rendered with the SAME widget (and the same `log-entry` / `log-level-filter` /
 * `log-copy-button` component IDs) as the client's own Settings → Logs page.
 * No Clear — there is no admin RPC to wipe the nest ring. The android twin of
 * linux `views/admin.rs` `build_admin_logs_page`.
 *
 * The VM fetches once and holds the source; the stateless [AdminLogsContent]
 * filters it client-side and renders it, so it exercises under the Robolectric
 * Compose harness with seeded `LogEntry` lists.
 */
@Composable
fun AdminLogsScreen(
    navController: NavController,
    vm: AdminLogsVM = hiltViewModel(),
) {
    val entries by vm.entries.collectAsState()
    val error by vm.error.collectAsState()
    val appMessages = LocalAppMessages.current
    var filterIndex by remember { mutableIntStateOf(0) }

    LaunchedEffect(Unit) { vm.load() }
    LaunchedEffect(error) { error?.let { appMessages.showError(it) } }

    // Filter + render through shared fauna-log (FFI); the stateless Content takes
    // the pre-computed rows so it stays Robolectric-safe.
    val rendered = remember(entries, filterIndex) { renderLogs(entries, filterIndex) }

    AdminLogsContent(
        rows = rendered.rows,
        copyText = rendered.copyText,
        filterIndex = filterIndex,
        onFilterChange = { filterIndex = it },
        onBack = { navController.popBackStack() },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminLogsContent(
    rows: List<LogRow>,
    copyText: String,
    filterIndex: Int,
    onFilterChange: (Int) -> Unit,
    onBack: () -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_logs_page_title),
                        modifier = Modifier.testTag(Ids.ADMIN_LOGS_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(
                        onClick = onBack,
                        modifier = Modifier.testTag(Ids.ADMIN_NAV_BACK),
                    ) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.common_back),
                        )
                    }
                },
            )
        },
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
                stringResource(R.string.admin_logs_page_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            LogLevelFilter(selectedIndex = filterIndex, onSelect = onFilterChange)

            // Copy only — no Clear on the admin view (no RPC to wipe the nest ring).
            CopyButton(
                testTag = Ids.LOG_COPY_BUTTON,
                text = copyText,
                label = stringResource(R.string.logs_copy_button),
            )

            LogEntryList(rows, emptyText = stringResource(R.string.admin_logs_page_empty))
        }
    }
}
