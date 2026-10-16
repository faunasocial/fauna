package com.fauna.app.ui.screen.status

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.R
import com.fauna.app.ui.components.CopyableRow
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.screen.settings.RegionSettingsSection
import com.fauna.app.ui.util.StatusCatalogLookup
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
    val legs by vm.legs.collectAsState()
    val features by vm.features.collectAsState()
    val ownLimits by vm.ownLimits.state.collectAsState()
    val context = LocalContext.current
    val appMessages = LocalAppMessages.current

    LaunchedEffect(Unit) { vm.refresh() }
    // A refused cell or a failed write lands on the page's `error-message`
    // (the global banner), as on every other app.
    LaunchedEffect(ownLimits.error) { ownLimits.error?.let { appMessages.showError(it) } }

    Scaffold(
        topBar = { TopAppBar(title = { Text(stringResource(R.string.common_status)) }) }
    ) { padding ->
        Column(modifier = Modifier.padding(padding).verticalScroll(rememberScrollState()).padding(16.dp)) {
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

            // Quota, then Feature limits as its sibling "what bounds me" surface
            // (`settings.md` § Layout & flow items 2 / 2b) — each paints only
            // once its read resolves, so a section never tells the user nothing
            // restricts them before its data has landed.
            quota?.let { q ->
                QuotaSection(q)
                Spacer(Modifier.height(16.dp))
            }
            features?.let { rows ->
                FeatureLimitsSection(rows, ownLimits, vm.ownLimits.actions())
                Spacer(Modifier.height(16.dp))
            }

            // The Node, Encryption and Build sections, painted from the shared
            // snapshot's text projection (`fauna_client_status::render`, over
            // the FFI as `statusText`; `ui/status.md` § Layout & flow). A
            // section whose leg is null is not painted — no header, no
            // placeholder, no zero — and the element text is the bare value the
            // witnesses read; the label beside it is the shared `status.*` string.
            // No Sync section: android has no local sync agent.
            val text = remember(legs, context) {
                com.fauna.ffi.statusText(legs, System.currentTimeMillis(), StatusCatalogLookup(context))
            }
            val nodeDomain = text.nodeDomain
            val nodeVersion = text.nodeVersion
            if (nodeDomain != null && nodeVersion != null) {
                StatusLegCard(stringResource(R.string.status_node_title)) {
                    StatusLegRow(stringResource(R.string.common_domain), Ids.STATUS_NODE_DOMAIN, nodeDomain)
                    StatusLegRow(stringResource(R.string.admin_dashboard_version), Ids.STATUS_NODE_VERSION, nodeVersion)
                }
            }
            val keyPackages = text.mlsKeyPackages
            val channels = text.mlsChannels
            if (keyPackages != null && channels != null) {
                StatusLegCard(stringResource(R.string.settings_encryption_page_title)) {
                    StatusLegRow(stringResource(R.string.status_encryption_key_packages), Ids.STATUS_MLS_KEY_PACKAGES, keyPackages)
                    StatusLegRow(stringResource(R.string.status_encryption_dm_channels), Ids.STATUS_MLS_CHANNELS, channels)
                }
            }
            // Absent on an unstamped build — never a `dev` placeholder.
            text.buildSha?.let { sha ->
                StatusLegCard(stringResource(R.string.status_build_title)) {
                    StatusLegRow(stringResource(R.string.status_build_commit), Ids.STATUS_BUILD_SHA, sha)
                }
            }

            // The region content plane's transparency surface
            // (`settings-region-section`, region-blocking.md § The blocked render
            // and the transparency surface) — on the Status sub-page, its home on
            // every sub-paged settings shell (linux's `views/status.rs`, apple's).
            // Always present.
            RegionSettingsSection()
        }
    }
}

/** One Status leg section: a titled card (the shape of the cards above). */
@Composable
private fun StatusLegCard(title: String, content: @Composable ColumnScope.() -> Unit) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(title, style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(8.dp))
            content()
        }
    }
    Spacer(Modifier.height(16.dp))
}

/** A label and its bare value; the test tag sits on the value alone, so a
 *  witness reads exactly the projection's text and never the label. */
@Composable
private fun StatusLegRow(label: String, testTag: String, value: String) {
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
        Text(label)
        Text(value, modifier = Modifier.testTag(testTag))
    }
}
