package com.fauna.app.ui.screen.settings

import com.fauna.app.ui.util.ValueFormat
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.ArrowForward
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.ui.viewmodel.AdminDashboardVM
import social.fauna.generated.Ids

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminDashboardScreen(
    navController: NavController,
    vm: AdminDashboardVM = hiltViewModel()
) {
    val stats by vm.stats.collectAsState()
    val isLoading by vm.isLoading.collectAsState()
    val error by vm.errorMessage.collectAsState()
    val context = LocalContext.current

    LaunchedEffect(Unit) { vm.loadStats() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.admin_dashboard_title), modifier = Modifier.testTag(Ids.ADMIN_DASHBOARD_HEADING)) },
                navigationIcon = {
                    IconButton(
                        onClick = { navController.popBackStack() },
                        modifier = Modifier.testTag(Ids.ADMIN_NAV_BACK),
                    ) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.common_back))
                    }
                },
                actions = {
                    IconButton(onClick = { vm.loadStats() }) {
                        Icon(Icons.Default.Refresh, stringResource(R.string.common_refresh))
                    }
                }
            )
        }
    ) { padding ->
        if (isLoading && stats == null) {
            Box(modifier = Modifier.fillMaxSize().padding(padding),
                contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        } else {
            Column(
                modifier = Modifier.padding(padding).padding(16.dp).fillMaxSize()
                    .verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(16.dp)
            ) {
                // Entry to the user-administration hub (admin.md § Users). No
                // ui.yaml ID — the e2e navigates via the action layer's route.
                Card(modifier = Modifier.fillMaxWidth()) {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.admin_users_page_title)) },
                        leadingContent = { Icon(Icons.Default.Person, contentDescription = null) },
                        trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                        modifier = Modifier.clickable { navController.navigate("settings/admin-users") }
                    )
                }

                // Entry to the Tiers page (admin.md § 3 Settings, nav-labelled
                // "Tiers" after the 2026-06-04 redesign): tier-cap editing only.
                // No ui.yaml ID — the e2e navigates via the action layer's route.
                Card(modifier = Modifier.fillMaxWidth()) {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.admin_settings_page_title)) },
                        leadingContent = { Icon(Icons.Default.Settings, contentDescription = null) },
                        trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                        modifier = Modifier.clickable { navController.navigate("settings/admin-settings") }
                    )
                }

                // Entry to the Nest page (admin.md § N Nest): the read-only
                // storage-mode indicator + admin pairing toggle + Factory Reset
                // danger zone (moved off Settings/Services in the 2026-06-04
                // redesign). No ui.yaml ID — the e2e navigates via the action route.
                Card(modifier = Modifier.fillMaxWidth()) {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.admin_nest_page_title)) },
                        leadingContent = { Icon(Icons.Default.Tune, contentDescription = null) },
                        trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                        modifier = Modifier.clickable { navController.navigate("settings/admin-nest") }
                    )
                }

                // The bridge and web-publishing admin pages — compiled out of
                // the kids build type (`KidsExcisedNav.kt`).
                if (!BuildConfig.KIDS) {
                    // Flat admin-mail pages (Bundle B; admin.md § 4/§ 6). The in-shell
                    // switcher entries (admin.md § Navigation model step 2). No ui.yaml
                    // ID — the e2e navigates via the action layer's route.
                    Card(modifier = Modifier.fillMaxWidth()) {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.admin_mail_page_title)) },
                            leadingContent = { Icon(Icons.Default.Email, contentDescription = null) },
                            trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                            modifier = Modifier.clickable { navController.navigate("settings/admin-mail") }
                        )
                    }
                    // admin-calendar: the deployment-wide CalDAV-enable toggle (admin.md
                    // § 8 Calendar), the sibling of admin-mail. No ui.yaml ID — the e2e
                    // navigates via the action layer's route.
                    Card(modifier = Modifier.fillMaxWidth()) {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.admin_calendar_page_title)) },
                            leadingContent = { Icon(Icons.Default.CalendarMonth, contentDescription = null) },
                            trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                            modifier = Modifier.clickable { navController.navigate("settings/admin-calendar") }
                        )
                    }
                    // admin-contacts: the deployment-wide CardDAV-enable toggle (admin.md
                    // § Contacts), the contacts sibling of admin-calendar. No ui.yaml ID —
                    // the e2e navigates via the action layer's route.
                    Card(modifier = Modifier.fillMaxWidth()) {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.admin_contacts_page_title)) },
                            leadingContent = { Icon(Icons.Default.Contacts, contentDescription = null) },
                            trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                            modifier = Modifier.clickable { navController.navigate("settings/admin-contacts") }
                        )
                    }
                    // admin-files: the deployment-wide WebDAV-enable toggle (admin.md
                    // § Files), the files sibling of admin-contacts. No ui.yaml ID —
                    // the e2e navigates via the action layer's route.
                    Card(modifier = Modifier.fillMaxWidth()) {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.admin_files_page_title)) },
                            leadingContent = { Icon(Icons.Default.Folder, contentDescription = null) },
                            trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                            modifier = Modifier.clickable { navController.navigate("settings/admin-files") }
                        )
                    }
                    Card(modifier = Modifier.fillMaxWidth()) {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.admin_aliases_page_title)) },
                            leadingContent = { Icon(Icons.Default.AlternateEmail, contentDescription = null) },
                            trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                            modifier = Modifier.clickable { navController.navigate("settings/admin-aliases") }
                        )
                    }
                    // admin-web: the nest-wide apex-actor designation (web-content-
                    // hosting.md § Admin apex hosting). A contextual admin page (sibling
                    // of admin-mail); no ui.yaml ID — the e2e navigates by route.
                    Card(modifier = Modifier.fillMaxWidth()) {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.admin_web_page_title)) },
                            leadingContent = { Icon(Icons.Default.Public, contentDescription = null) },
                            trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                            modifier = Modifier.clickable { navController.navigate("settings/admin-web") }
                        )
                    }
                    Card(modifier = Modifier.fillMaxWidth()) {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.admin_bridges_pending_title)) },
                            leadingContent = { Icon(Icons.Default.Dns, contentDescription = null) },
                            trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                            modifier = Modifier.clickable { navController.navigate("settings/admin-bridges-pending") }
                        )
                    }
                    // admin-dns: the full DNS record matrix + managed-mode master
                    // switch (dns-management.md § App surface). The standalone
                    // admin-services page was removed in the 2026-06-04 redesign.
                    Card(modifier = Modifier.fillMaxWidth()) {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.admin_dns_title)) },
                            leadingContent = { Icon(Icons.Default.Language, contentDescription = null) },
                            trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                            modifier = Modifier.clickable { navController.navigate("settings/admin-dns") }
                        )
                    }
                }
                // admin-custody-hosting: the nest-wide custody-hosting registry. A CONTEXTUAL detail page like admin-dns/admin-logs;
                // no ui.yaml nav-list entry.
                Card(modifier = Modifier.fillMaxWidth()) {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.admin_custody_hosting_title)) },
                        leadingContent = { Icon(Icons.Default.Storage, contentDescription = null) },
                        trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                        modifier = Modifier.clickable { navController.navigate("settings/admin-custody-hosting") }
                    )
                }
                // Admin Logs — the nest's fauna-log ring over fauna.admin.logs
                // (observability.md § Surfaces). In-shell switcher entry; no
                // ui.yaml ID — the e2e navigates via the action layer's route.
                Card(modifier = Modifier.fillMaxWidth()) {
                    ListItem(
                        headlineContent = { Text(stringResource(R.string.admin_logs_page_title)) },
                        leadingContent = { Icon(Icons.Default.Description, contentDescription = null) },
                        trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                        modifier = Modifier.clickable { navController.navigate("settings/admin-logs") }
                    )
                }

                stats?.let { s ->
                    Text(stringResource(R.string.settings_admin_page_overview), style = MaterialTheme.typography.titleMedium)
                    Row(modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        StatCard(Icons.Default.Person, stringResource(R.string.common_users), "${s.totalUsers}",
                            Modifier.weight(1f))
                        StatCard(Icons.Default.Wifi, stringResource(R.string.common_connected), "${s.wsConnections}",
                            Modifier.weight(1f))
                    }

                    Card(modifier = Modifier.fillMaxWidth()) {
                        Column(modifier = Modifier.padding(16.dp)) {
                            Text(stringResource(R.string.common_storage), style = MaterialTheme.typography.titleSmall)
                            Spacer(Modifier.height(8.dp))
                            Row(modifier = Modifier.fillMaxWidth(),
                                horizontalArrangement = Arrangement.SpaceBetween) {
                                Column {
                                    Text(stringResource(R.string.common_inbox), style = MaterialTheme.typography.labelMedium)
                                    Text(ValueFormat.byteSize(context, s.totalInboxBytes))
                                }
                                Column {
                                    Text(stringResource(R.string.common_files), style = MaterialTheme.typography.labelMedium)
                                    Text(ValueFormat.byteSize(context, s.totalStorageBytes))
                                }
                                Column {
                                    Text(stringResource(R.string.settings_admin_page_total), style = MaterialTheme.typography.labelMedium)
                                    Text(ValueFormat.byteSize(context,
                                        s.totalInboxBytes + s.totalStorageBytes))
                                }
                            }
                        }
                    }

                    if (s.usersByTier.isNotEmpty()) {
                        Card(modifier = Modifier.fillMaxWidth()) {
                            Column(modifier = Modifier.padding(16.dp)) {
                                Text(stringResource(R.string.common_users_by_tier), style = MaterialTheme.typography.titleSmall)
                                Spacer(Modifier.height(8.dp))
                                s.usersByTier.forEach { (tier, count) ->
                                    Row(modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp),
                                        horizontalArrangement = Arrangement.SpaceBetween) {
                                        Text(tier.replaceFirstChar { it.uppercase() })
                                        Text("$count")
                                    }
                                }
                                if (s.suspendedUsers > 0) {
                                    HorizontalDivider(modifier = Modifier.padding(vertical = 4.dp))
                                    Row(modifier = Modifier.fillMaxWidth(),
                                        horizontalArrangement = Arrangement.SpaceBetween) {
                                        Text(stringResource(R.string.admin_dashboard_suspended), color = MaterialTheme.colorScheme.error)
                                        Text("${s.suspendedUsers}",
                                            color = MaterialTheme.colorScheme.error)
                                    }
                                }
                            }
                        }
                    }
                }

                error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            }
        }
    }
}

@Composable
private fun StatCard(icon: ImageVector, label: String, value: String, modifier: Modifier) {
    Card(modifier = modifier.testTag(Ids.ADMIN_STAT_CARD)) {
        Column(modifier = Modifier.padding(16.dp),
            horizontalAlignment = Alignment.CenterHorizontally) {
            Icon(icon, contentDescription = null, modifier = Modifier.size(24.dp),
                tint = MaterialTheme.colorScheme.primary)
            Spacer(Modifier.height(4.dp))
            Text(value, style = MaterialTheme.typography.headlineSmall,
                modifier = Modifier.testTag(Ids.ADMIN_STAT_CARD_VALUE))
            Text(label, style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.ADMIN_STAT_CARD_LABEL))
        }
    }
}
