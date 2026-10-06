package com.fauna.app.ui.screen.status

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.R
import com.fauna.app.ui.components.CopyableRow
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.viewmodel.StatusVM
import com.fauna.ffi.shortId
import social.fauna.generated.Ids

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun StatusScreen(
    vm: StatusVM = hiltViewModel()
) {
    val account by vm.account.collectAsState()
    val quota by vm.quota.collectAsState()
    val context = LocalContext.current
    val appMessages = LocalAppMessages.current

    LaunchedEffect(Unit) { vm.refresh() }

    Scaffold(
        topBar = { TopAppBar(title = { Text(stringResource(R.string.common_status)) }) }
    ) { padding ->
        Column(modifier = Modifier.padding(padding).padding(16.dp)) {
            account?.let { acc ->
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text(stringResource(R.string.common_account), style = MaterialTheme.typography.titleMedium)
                        Spacer(Modifier.height(8.dp))
                        Text("${stringResource(R.string.common_handle)}: ${acc.handle}")
                        Text("${stringResource(R.string.common_domain)}: ${acc.domain}")
                        Text("${stringResource(R.string.common_tier)}: ${acc.tier}")
                        CopyableRow(
                            label = stringResource(R.string.common_actor_id),
                            displayValue = shortId(acc.actorId),
                            fullValue = acc.actorId,
                            testTag = Ids.STATUS_ACTOR_ID_COPY_BTN,
                            onCopied = {
                                appMessages.showInfo(context.getString(R.string.settings_account_page_copied_clipboard))
                            }
                        )
                        CopyableRow(
                            label = stringResource(R.string.common_node_url),
                            displayValue = acc.nodeUrl,
                            fullValue = acc.nodeUrl,
                            testTag = Ids.STATUS_NODE_URL_COPY_BTN,
                            onCopied = {
                                appMessages.showInfo(context.getString(R.string.settings_account_page_copied_clipboard))
                            }
                        )
                    }
                }
            }

            Spacer(Modifier.height(16.dp))

            quota?.let { q ->
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text(stringResource(R.string.common_storage), style = MaterialTheme.typography.titleMedium)
                        Spacer(Modifier.height(8.dp))
                        val used = q.storage.usedBytes
                        val limit = q.storage.maxBytes
                        Text("${ValueFormat.byteSize(context, used)} / ${ValueFormat.byteSize(context, limit)}")
                        Spacer(Modifier.height(4.dp))
                        // Shared `fauna_core::format::quota_fraction` — the 0.0..=1.0 bar-fill
                        // fraction (guards maxBytes <= 0 → 0, clamps over-quota → 1),
                        // single-sourced with linux/web/windows (value-formatting.md §
                        // Quota fraction); no `if (limit > 0)` guard needed, the fn self-guards.
                        LinearProgressIndicator(
                            progress = { com.fauna.ffi.quotaFraction(used, limit).toFloat() },
                            modifier = Modifier.fillMaxWidth()
                        )
                    }
                }
            }
        }
    }
}
