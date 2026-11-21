package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
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
import com.fauna.app.ui.screen.onboarding.TermsLinkButton
import com.fauna.app.ui.util.isPossible
import com.fauna.app.ui.util.isShown
import com.fauna.app.ui.viewmodel.SettingsAdminGateVM
import com.fauna.app.ui.viewmodel.SettingsFamilyGateVM
import com.fauna.ffi.faunaFfiBuildVersion
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

    // An eager scrolling column, not a LazyColumn: a lazy row below the fold is
    // never composed, so it has no semantics node a driver (or an
    // accessibility service) could find or scroll to — the landing is a few
    // dozen rows, cheap to compose at once (iOS's Settings landing made the
    // same move for the same reason).
    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(vertical = 8.dp)
    ) {
        // Section 1 — Configuration
        Text(
            stringResource(R.string.settings_configuration),
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)
        )
        SettingsRow(Icons.Default.Person, stringResource(R.string.common_account), testTag = Ids.ACCOUNT_SETTINGS_LINK) { navController.navigate("settings/account") }
        // Members To Review — the permanent post-succession unattested-member
        // review page (succession-aftermath.md § Propagation item (iv); rail
        // slot user-approved 2026-08-16, directly after Account, mirrors tui /
        // linux). No ui.yaml settings-link id — reached by route, like the
        // sibling rows below.
        SettingsRow(Icons.Default.PersonSearch, stringResource(R.string.settings_member_review_page_title), testTag = navRowTag("member_review")) { navController.navigate("settings/member-review") }
        SettingsRow(Icons.Default.Lock, stringResource(R.string.settings_privacy), testTag = navRowTag("privacy")) { navController.navigate("settings/privacy") }
        // Muted words — placed right after Privacy as the sibling personal
        // content-filtering surface (settings.md § Navigation model; behavior owned
        // by moderation.md § Muted keywords). No ui.yaml link id — the e2e navigates
        // by route, like the Web / subscriptions rows.
        SettingsRow(Icons.Default.Block, stringResource(R.string.muted_words_title), testTag = navRowTag("muted-words")) { navController.navigate("settings/muted-words") }
        // Rows guarded by `!BuildConfig.KIDS` lead to destinations the kids build
        // type compiles out (`KidsExcisedNav.kt`; family-safety.md § The account
        // age band, the kids-app bullet, item (4)).
        // Personalization — the unified tier-1 ruleset home (Feeds / Muted words /
        // Community labelers facets; content-moderation-and-ranking.md § Composition,
        // ratified 2026-07-06). No ui.yaml link id — the e2e navigates by route, like
        // the row above.
        if (!BuildConfig.KIDS) { SettingsRow(Icons.Default.Tune, stringResource(R.string.personalization_title), testTag = navRowTag("personalization")) { navController.navigate("settings/personalization") } }
        SettingsRow(Icons.Default.Star, stringResource(R.string.settings_encryption_page_title), testTag = navRowTag("encryption")) { navController.navigate("settings/encryption") }
        // Devices (roster) + Folders (control plane) — the 2026-06-28 sync/folder
        // UI unification's two sub-pages, in the device/sync cluster after Encryption
        // (settings.md § Navigation model). Folders is the renamed former "Sync".
        SettingsRow(Icons.Default.Phone, stringResource(R.string.devices_title), testTag = navRowTag("devices")) { navController.navigate("settings/devices") }
        // Sessions — the sign-ins live now, directly after Devices (the machines
        // enrolled); settings.md § Navigation model, ui/sessions.md § The ruling.
        SettingsRow(Icons.Default.Key, stringResource(R.string.sessions_title), testTag = navRowTag("sessions")) { navController.navigate("settings/sessions") }
        SettingsRow(Icons.Default.Folder, stringResource(R.string.folders_title), testTag = navRowTag("folders")) { navController.navigate("settings/folders") }
        // Label + testids renamed "Nests" 2026-07-08, the nav id/route `nests`
        // 2026-10-02 (docs/goal/ui/nests.md; the shared e2e nav id).
        SettingsRow(Icons.Default.Share, stringResource(R.string.nests_title), testTag = navRowTag("nests")) { navController.navigate("settings/nests") }
        SettingsRow(Icons.Default.Schedule, stringResource(R.string.task_delegation_title), testTag = navRowTag("task-delegation")) { navController.navigate("settings/task-delegation") }
        // Connected apps — everything acting for the user from outside the seven
        // apps (connected-apps.md; slot directly after Task delegation,
        // settings.md § Navigation model). No ui.yaml link id — the e2e navigates
        // by route, like the rows around it.
        if (!BuildConfig.KIDS) { SettingsRow(Icons.Default.Apps, stringResource(R.string.connected_apps_title), testTag = navRowTag("connected-apps")) { navController.navigate("settings/connected-apps") } }
        if (!BuildConfig.KIDS) { SettingsRow(Icons.Default.Email, stringResource(R.string.mail_settings_title), testTag = navRowTag("mail-settings")) { navController.navigate("settings/mail-settings") } }
        // The six mail sub-page rows (settings.md § Navigation model: the same
        // page set and ids as the desktop rail), gated on the session's shared
        // mail-usable answer (mail-settings.md § Errors & edge cases, the
        // mail-usable gate); the kids twin answers absent, so none is composed.
        MailSubpageRows(com.fauna.app.ui.navigation.mailUsableAnswer()) { navController.navigate(it) }
        // Import from other services (archive-import.md § The wizard and its
        // machine; settings.md § Navigation model puts it directly after Mail
        // import).
        if (!BuildConfig.KIDS) { SettingsRow(Icons.Default.Download, stringResource(R.string.archive_import_title), testTag = navRowTag("archive-import")) { navController.navigate("settings/archive-import") } }
        // Bluesky — the dedicated ATProto integration-depth page
        // (docs/goal/ui/atproto.md § Layout & flow; settings.md § Navigation
        // model places it after Nostr, which android's Settings rail doesn't
        // carry as its own row — Nostr is a top-level drawer tab here). No
        // ui.yaml link id — the e2e navigates by route, like the rows below.
        if (!BuildConfig.KIDS) { SettingsRow(Icons.Default.Public, stringResource(R.string.atproto_settings_title), testTag = navRowTag("atproto")) { navController.navigate("settings/atproto") } }
        // User "Web" page — the per-user subdomain opt-in (web-content-hosting.md
        // § Published-post management). No ui.yaml link id — the e2e navigates by route.
        if (!BuildConfig.KIDS) { SettingsRow(Icons.Default.Language, stringResource(R.string.web_settings_title), testTag = navRowTag("web-settings")) { navController.navigate("settings/web") } }
        // Consumer-side subscriptions (`subscription-settings`, monetization.md
        // § Pillar 1). No ui.yaml link id — the e2e navigates by route.
        if (!BuildConfig.KIDS) { SettingsRow(Icons.Default.Favorite, stringResource(R.string.subscriptions_title), testTag = navRowTag("subscription-settings")) { navController.navigate("settings/subscriptions") } }

        HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))

        // Section 2 — Data
        Text(
            stringResource(R.string.settings_data),
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)
        )
        // Photo backup no longer has a standalone settings row (media.md §
        // Photo-backup reframe, 2026-07-19) — its controls render as
        // photo-backup-controls on settings/folders, reached via the "Folders"
        // row below.
        SettingsRow(Icons.Default.Refresh, stringResource(R.string.backups_title), testTag = navRowTag("backups")) { navController.navigate("backups") }
        SettingsRow(Icons.Default.Info, stringResource(R.string.common_status), testTag = navRowTag("status")) { navController.navigate("settings/status") }
        SettingsRow(Icons.Default.Description, stringResource(R.string.logs_title), testTag = navRowTag("settings-logs")) { navController.navigate("settings/logs") }
        // The gated `family-tab` entry (family-safety.md § App surface): the
        // in-Settings Family-surface entry, rendered only when the caller has any
        // family relationship or a pending/incoming transfer (fail-closed).
        if (hasFamilyRelationship) {
            SettingsRow(Icons.Default.FamilyRestroom, stringResource(R.string.family_title), testTag = Ids.FAMILY_TAB) { navController.navigate("settings/family") }
        }
        // The gated `admin-tab` entry (admin.md § Navigation model): the in-Settings
        // admin-shell entry, painted from the shared `admin_entry_possibility`
        // over the `am-i-admin` read (admin.md § Errors & edge cases, *Role and
        // state gates answer one `Possibility` too*) — absent for a non-admin,
        // fail-closed until the read answers; the press asks it again.
        if (com.fauna.ffi.adminEntryPossibility(isAdmin).isShown) {
            SettingsRow(Icons.Default.Shield, stringResource(R.string.settings_nest_admin), testTag = Ids.ADMIN_TAB) {
                if (com.fauna.ffi.adminEntryPossibility(adminGateVm.isAdmin.value).isPossible) {
                    navController.navigate("settings/admin")
                }
            }
        }

        HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))

        // Section 3 — About: the one product version every app and the nest share
        // (`product-version.md` § The model), read from shared Rust
        // (`faunaFfiBuildVersion`), and the terms link beside it, which opens the
        // shared `fauna_core::TERMS_URL` in the browser
        // (`terms-and-content-guidelines.md` § The decisions, item 4 (b)).
        Text(
            stringResource(R.string.settings_about),
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)
        )
        ListItem(
            headlineContent = { Text(stringResource(R.string.settings_general_page_version)) },
            trailingContent = {
                Text(faunaFfiBuildVersion(), modifier = Modifier.testTag(Ids.SETTINGS_APP_VERSION))
            },
        )
        TermsLinkButton(Ids.SETTINGS_TERMS_LINK, Modifier.padding(horizontal = 8.dp))
    }
}

/** The Settings rail-row id for the page [key] opens (ui.yaml `navigation.sub_page_nav_rows`). */
private fun navRowTag(key: String) = "${Ids.SETTINGS_NAV_ROW}[$key]"

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
