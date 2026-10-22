package com.fauna.app.ui.screen.settings

import com.fauna.app.ui.navigation.exitAdminToPrimary
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.stringResourceFmt
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.ui.viewmodel.AdminDashboardVM
import com.fauna.ffi.FfiAdminDashboardSnapshot
import social.fauna.generated.Ids

@Composable
fun AdminDashboardScreen(
    navController: NavController,
    vm: AdminDashboardVM = hiltViewModel()
) {
    val snapshot by vm.snapshot.collectAsState()
    val isLoading by vm.isLoading.collectAsState()
    val error by vm.errorMessage.collectAsState()

    LaunchedEffect(Unit) { vm.loadStats() }

    AdminDashboardContent(
        snapshot = snapshot,
        isLoading = isLoading,
        error = error,
        onBack = { navController.exitAdminToPrimary() },
        onRefresh = { vm.loadStats() },
        onNavigate = { route -> navController.navigate(route) },
    )
}

/**
 * The stateless dashboard: the admin page entries, then the shared card set
 * ([snapshot]'s cards, in the fold's order) and the failed-read line. Split
 * from [AdminDashboardScreen] so it renders without Hilt or the native FFI.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminDashboardContent(
    snapshot: FfiAdminDashboardSnapshot?,
    isLoading: Boolean,
    error: String?,
    onBack: () -> Unit,
    onRefresh: () -> Unit,
    onNavigate: (String) -> Unit,
) {
    val context = LocalContext.current

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.admin_dashboard_title), modifier = Modifier.testTag(Ids.ADMIN_DASHBOARD_HEADING)) },
                navigationIcon = {
                    IconButton(
                        onClick = onBack,
                        modifier = Modifier.testTag(Ids.ADMIN_NAV_BACK),
                    ) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.common_back))
                    }
                },
                actions = {
                    IconButton(onClick = onRefresh) {
                        Icon(Icons.Default.Refresh, stringResource(R.string.common_refresh))
                    }
                }
            )
        }
    ) { padding ->
        if (isLoading && snapshot == null) {
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
                        modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-users]").clickable { onNavigate("settings/admin-users") }
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
                        modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-settings]").clickable { onNavigate("settings/admin-settings") }
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
                        modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-nest]").clickable { onNavigate("settings/admin-nest") }
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
                            modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-mail]").clickable { onNavigate("settings/admin-mail") }
                        )
                    }
                    // admin-mail-reports: the DMARC aggregate-report summary
                    // (dmarc-reporting.md § Admin pane summary surface), after admin-mail.
                    Card(modifier = Modifier.fillMaxWidth()) {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.admin_mail_reports_page_title)) },
                            leadingContent = { Icon(Icons.Default.Email, contentDescription = null) },
                            trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                            modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-mail-reports]").clickable { onNavigate("settings/admin-mail-reports") }
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
                            modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-calendar]").clickable { onNavigate("settings/admin-calendar") }
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
                            modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-contacts]").clickable { onNavigate("settings/admin-contacts") }
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
                            modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-files]").clickable { onNavigate("settings/admin-files") }
                        )
                    }
                    Card(modifier = Modifier.fillMaxWidth()) {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.admin_aliases_page_title)) },
                            leadingContent = { Icon(Icons.Default.AlternateEmail, contentDescription = null) },
                            trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                            modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-aliases]").clickable { onNavigate("settings/admin-aliases") }
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
                            modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-web]").clickable { onNavigate("settings/admin-web") }
                        )
                    }
                    Card(modifier = Modifier.fillMaxWidth()) {
                        ListItem(
                            headlineContent = { Text(stringResource(R.string.admin_bridges_pending_title)) },
                            leadingContent = { Icon(Icons.Default.Dns, contentDescription = null) },
                            trailingContent = { Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null) },
                            modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-bridges-pending]").clickable { onNavigate("settings/admin-bridges-pending") }
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
                            modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-dns]").clickable { onNavigate("settings/admin-dns") }
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
                        modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-custody-hosting]").clickable { onNavigate("settings/admin-custody-hosting") }
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
                        modifier = Modifier.testTag("${Ids.ADMIN_NAV_ROW}[admin-logs]").clickable { onNavigate("settings/admin-logs") }
                    )
                }

                // The shared card set (admin.md § 1. Dashboard → *The card
                // set*): the fold's cards in its order, each an indexed
                // `admin-stat-card`. Android chooses no card of its own.
                snapshot?.let { snap ->
                    if (snap.stats.isNotEmpty()) {
                        Text(stringResource(R.string.settings_admin_page_overview), style = MaterialTheme.typography.titleMedium)
                    }
                    snap.stats.forEach { card ->
                        StatCard(
                            label = resolveLocalized(context, card.label).orEmpty(),
                            value = resolveLocalized(context, card.value).orEmpty(),
                        )
                    }
                }

                // A failed read drops only its own cards; its detail rides
                // `error-message` (admin.md § Navigation model). The id leaves
                // the tree when there is nothing to say.
                if (!error.isNullOrEmpty()) {
                    Text(
                        stringResourceFmt(R.string.admin_dashboard_load_error, error),
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
                    )
                }
            }
        }
    }
}

@Composable
private fun StatCard(label: String, value: String) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_STAT_CARD)) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(label, style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.ADMIN_STAT_CARD_LABEL))
            Text(value, style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.testTag(Ids.ADMIN_STAT_CARD_VALUE))
        }
    }
}
