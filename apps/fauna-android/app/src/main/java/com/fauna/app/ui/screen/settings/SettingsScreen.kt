package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.ui.viewmodel.SettingsAdminGateVM
import com.fauna.app.ui.viewmodel.SettingsFamilyGateVM
import social.fauna.generated.Ids

@Composable
fun SettingsScreen(
    navController: NavController,
    adminGateVm: SettingsAdminGateVM = hiltViewModel(),
    familyGateVm: SettingsFamilyGateVM = hiltViewModel(),
) {
    // The admin-shell entry (`admin-tab`) is gated on the shared `am-i-admin`
    // result (admin.md § Navigation model): on mobile it is the idiomatic
    // in-Settings entry, shown only to admins. Fail-closed (starts false).
    val isAdmin by adminGateVm.isAdmin.collectAsState()
    // The Family-surface entry (`family-tab`) is gated on `fauna.family.status`
    // returning any relationship or pending transfer (family-safety.md § Client
    // surface). Fail-closed (starts false).
    val hasFamilyRelationship by familyGateVm.hasFamilyRelationship.collectAsState()

    LazyColumn(
        contentPadding = PaddingValues(vertical = 8.dp),
        modifier = Modifier.fillMaxSize()
    ) {
        // Section 1 — Configuration
        item {
            Text(
                stringResource(R.string.settings_configuration),
                style = MaterialTheme.typography.titleSmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)
            )
        }
        item { SettingsRow(Icons.Default.Person, stringResource(R.string.common_account), testTag = Ids.ACCOUNT_SETTINGS_LINK) { navController.navigate("settings/account") } }
        // Members To Review — the permanent post-succession unattested-member
        // review page (succession-aftermath.md § Propagation item (iv); rail
        // slot user-approved 2026-08-16, directly after Account, mirrors tui /
        // linux). No ui.yaml settings-link id — reached by route, like the
        // sibling rows below.
        item { SettingsRow(Icons.Default.PersonSearch, stringResource(R.string.settings_member_review_page_title)) { navController.navigate("settings/member-review") } }
        item { SettingsRow(Icons.Default.Lock, stringResource(R.string.settings_privacy)) { navController.navigate("settings/privacy") } }
        // Muted words — placed right after Privacy as the sibling personal
        // content-filtering surface (settings.md § Navigation model; behavior owned
        // by moderation.md § Muted keywords). No ui.yaml link id — the e2e navigates
        // by route, like the Web / subscriptions rows.
        item { SettingsRow(Icons.Default.Block, stringResource(R.string.muted_words_title)) { navController.navigate("settings/muted-words") } }
        // Rows guarded by `!BuildConfig.KIDS` lead to destinations the kids build
        // type compiles out (`KidsExcisedNav.kt`; family-safety.md § The account
        // age band, the kids-app bullet, item (4)).
        // Personalization — the unified tier-1 ruleset home (Feeds / Muted words /
        // Community labelers facets; content-moderation-and-ranking.md § Composition,
        // ratified 2026-07-06). No ui.yaml link id — the e2e navigates by route, like
        // the row above.
        if (!BuildConfig.KIDS) item { SettingsRow(Icons.Default.Tune, stringResource(R.string.personalization_title)) { navController.navigate("settings/personalization") } }
        item { SettingsRow(Icons.Default.Star, stringResource(R.string.settings_encryption_page_title)) { navController.navigate("settings/encryption") } }
        // Devices (roster) + Folders (control plane) — the 2026-06-28 sync/folder
        // UI unification's two sub-pages, in the device/sync cluster after Encryption
        // (settings.md § Navigation model). Folders is the renamed former "Sync".
        item { SettingsRow(Icons.Default.Phone, stringResource(R.string.devices_title)) { navController.navigate("settings/devices") } }
        item { SettingsRow(Icons.Default.Folder, stringResource(R.string.folders_title)) { navController.navigate("settings/folders") } }
        // Label + testids renamed "Nests" 2026-07-08, the nav id/route `nests`
        // 2026-10-02 (docs/goal/ui/nests.md; the shared e2e nav id).
        item { SettingsRow(Icons.Default.Share, stringResource(R.string.nests_title), testTag = Ids.LINKED_NESTS_LINK) { navController.navigate("settings/nests") } }
        item { SettingsRow(Icons.Default.Schedule, stringResource(R.string.task_delegation_title)) { navController.navigate("settings/task-delegation") } }
        // Connected apps — everything acting for the user from outside the seven
        // apps (connected-apps.md; slot directly after Task delegation,
        // settings.md § Navigation model). No ui.yaml link id — the e2e navigates
        // by route, like the rows around it.
        if (!BuildConfig.KIDS) item { SettingsRow(Icons.Default.Apps, stringResource(R.string.connected_apps_title)) { navController.navigate("settings/connected-apps") } }
        if (!BuildConfig.KIDS) item { SettingsRow(Icons.Default.Email, stringResource(R.string.mail_settings_title), testTag = Ids.MAIL_SETTINGS_LINK) { navController.navigate("settings/mail-settings") } }
        // Bluesky — the dedicated ATProto integration-depth page
        // (docs/goal/ui/atproto.md § Layout & flow; settings.md § Navigation
        // model places it after Nostr, which android's Settings rail doesn't
        // carry as its own row — Nostr is a top-level drawer tab here). No
        // ui.yaml link id — the e2e navigates by route, like the rows below.
        if (!BuildConfig.KIDS) item { SettingsRow(Icons.Default.Public, stringResource(R.string.atproto_settings_title)) { navController.navigate("settings/atproto") } }
        // User "Web" page — the per-user subdomain opt-in (web-content-hosting.md
        // § Published-post management). No ui.yaml link id — the e2e navigates by route.
        if (!BuildConfig.KIDS) item { SettingsRow(Icons.Default.Language, stringResource(R.string.web_settings_title)) { navController.navigate("settings/web") } }
        // Consumer-side subscriptions (`subscription-settings`, monetization.md
        // § Pillar 1). No ui.yaml link id — the e2e navigates by route.
        if (!BuildConfig.KIDS) item { SettingsRow(Icons.Default.Favorite, stringResource(R.string.subscriptions_title)) { navController.navigate("settings/subscriptions") } }

        item { HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp)) }

        // Section 2 — Data
        item {
            Text(
                stringResource(R.string.settings_data),
                style = MaterialTheme.typography.titleSmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)
            )
        }
        // Photo backup no longer has a standalone settings row (media.md §
        // Photo-backup reframe, 2026-07-19) — its controls render as
        // photo-backup-controls on settings/folders, reached via the "Folders"
        // row below.
        item { SettingsRow(Icons.Default.Refresh, stringResource(R.string.backups_title)) { navController.navigate("backups") } }
        item { SettingsRow(Icons.Default.Info, stringResource(R.string.common_status)) { navController.navigate("settings/status") } }
        item { SettingsRow(Icons.Default.Description, stringResource(R.string.logs_title)) { navController.navigate("settings/logs") } }
        // The gated `family-tab` entry (family-safety.md § App surface): the
        // in-Settings Family-surface entry, rendered only when the caller has any
        // family relationship or a pending/incoming transfer (fail-closed).
        if (hasFamilyRelationship) {
            item { SettingsRow(Icons.Default.FamilyRestroom, stringResource(R.string.family_title), testTag = Ids.FAMILY_TAB) { navController.navigate("settings/family") } }
        }
        // The gated `admin-tab` entry (admin.md § Navigation model): the in-Settings
        // admin-shell entry, rendered only when `am-i-admin` passes (fail-closed).
        if (isAdmin) {
            item { SettingsRow(Icons.Default.Shield, stringResource(R.string.settings_nest_admin), testTag = Ids.ADMIN_TAB) { navController.navigate("settings/admin") } }
        }
    }
}

@Composable
private fun SettingsRow(icon: ImageVector, label: String, testTag: String? = null, onClick: () -> Unit) {
    ListItem(
        headlineContent = { Text(label) },
        leadingContent = {
            Icon(icon, contentDescription = label)
        },
        modifier = Modifier
            .let { if (testTag != null) it.testTag(testTag) else it }
            .clickable(onClick = onClick)
    )
}
