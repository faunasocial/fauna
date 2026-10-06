package com.fauna.app.ui.navigation

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavGraphBuilder
import androidx.navigation.NavHostController
import androidx.navigation.NavType
import androidx.navigation.compose.composable
import androidx.navigation.navArgument
import com.fauna.app.ui.screen.bridges.BridgesScreen
import com.fauna.app.ui.screen.feed.FeedComposeScreen
import com.fauna.app.ui.screen.feed.FeedScreen
import com.fauna.app.ui.screen.feed.PostDetailScreen
import com.fauna.app.ui.screen.personalization.LabelerCatalogScreen
import com.fauna.app.ui.screen.personalization.PersonalizationScreen
import com.fauna.app.ui.screen.search.SearchResultsScreen
import com.fauna.app.ui.util.localized
import social.fauna.generated.Ids

// The parts of the app shell (`FaunaNavHost`) the **kids** build type
// excises — **the built half** (`family-safety.md` § The account age band,
// the kids-app bullet, item (4); `dynamic-features.md` § Compile-time
// excision). Compiled into `debug`, `release`, `storeSafe` and `foss` from
// `src/noKids/`; the `kids` build type compiles the empty twin under
// `src/kids/java/com/fauna/app/ui/navigation/KidsExcisedNav.kt` instead, so
// none of these destinations, effects or banners — nor the screens only they
// reach — exist in a kids artifact.

/**
 * Register the destinations the kids build type compiles out: the feed and
 * its compose / post-detail routes, search results, every bridge (the Bridges
 * and Nostr pages, the mail-settings family, the Bluesky page, the admin mail /
 * calendar / contacts / files / aliases / pending-bridges / DNS pages), web
 * publishing (the user's web page and the admin apex), monetization (the
 * consumer subscriptions page) and third-party anything (connected apps, the
 * community-labeler catalog, and the Personalization page whose every facet —
 * trained topics, signal sharing, labeler subscriptions — rides the feed or the
 * catalog).
 */
fun NavGraphBuilder.kidsExcisedDestinations(navController: NavHostController) {
    composable("feed") { FeedScreen(navController) }
    composable("nostr") { com.fauna.app.ui.screen.nostr.NostrScreen() }
    composable("bridges") { BridgesScreen(navController) }
    // Connected apps (connected-apps.md; placed after Task delegation,
    // settings.md § Navigation model): the Requests tray, the roster of
    // everything acting for the user from outside the seven apps, and the
    // Blocked apps list, over the shared ConnectedAppsMachine.
    composable("settings/connected-apps") {
        com.fauna.app.ui.screen.settings.ConnectedAppsScreen(navController)
    }
    // Mail-settings family (hub + sub-pages); reached from the
    // mail-settings hub's nav rows (mail-settings.md + siblings).
    composable("settings/mail-settings") {
        com.fauna.app.ui.screen.settings.MailSettingsScreen(navController)
    }
    // Bluesky — the dedicated ATProto integration-depth page
    // (docs/goal/ui/atproto.md); reached from the Settings
    // rail row above.
    composable("settings/atproto") {
        com.fauna.app.ui.screen.settings.AtprotoSettingsScreen(navController)
    }
    composable("settings/web") {
        com.fauna.app.ui.screen.settings.WebSettingsScreen(navController)
    }
    // Consumer-side subscriptions (`subscription-settings`,
    // monetization.md § Pillar 1). Sibling of mail/web settings;
    // the e2e navigates by route (no ui.yaml settings-link id).
    composable("settings/subscriptions") {
        com.fauna.app.ui.screen.settings.SubscriptionSettingsScreen(navController)
    }
    composable("settings/mail-aliases") {
        com.fauna.app.ui.screen.settings.MailAliasesScreen(navController)
    }
    composable("settings/mail-lists") {
        com.fauna.app.ui.screen.settings.MailListsScreen(navController)
    }
    composable("settings/mail-list-members/{listIdHex}/{listName}") { entry ->
        val listIdHex = entry.arguments?.getString("listIdHex") ?: ""
        val listName = java.net.URLDecoder.decode(
            entry.arguments?.getString("listName") ?: "", "UTF-8",
        )
        com.fauna.app.ui.screen.settings.MailListMembersScreen(
            navController, listIdHex, listName,
        )
    }
    composable("settings/mail-export") {
        com.fauna.app.ui.screen.settings.MailExportScreen(navController)
    }
    composable("settings/mail-import") {
        com.fauna.app.ui.screen.settings.MailImportScreen(navController)
    }
    composable("settings/mail-spam") {
        com.fauna.app.ui.screen.settings.MailSpamScreen(navController)
    }
    // Settings → Personalization (content-moderation-and-ranking.md §
    // Composition, ratified 2026-07-06) + its Community-labelers catalog
    // sub-page — two Settings sub-pages over one shared LabelerCatalogVM
    // (mirrors linux views/personalization/mod.rs).
    composable("settings/personalization") { PersonalizationScreen(navController) }
    composable("settings/labeler-catalog") { LabelerCatalogScreen(navController) }
    // Flat admin-mail pages (Bundle B; admin.md § 4/§ 6,
    // mail-bridge-lifecycle.md § Pending approval).
    composable("settings/admin-mail") {
        com.fauna.app.ui.screen.settings.AdminMailScreen(navController)
    }
    // Flat admin-calendar page (admin.md § 8 Calendar): the
    // CalDAV-enable sibling of admin-mail. Email + calendar are two
    // independently enableable features of one MDA bridge
    // (caldav-server.md § Independent enablement).
    composable("settings/admin-calendar") {
        com.fauna.app.ui.screen.settings.AdminCalendarScreen(navController)
    }
    // Flat admin-contacts page (admin.md § Contacts): the
    // CardDAV-enable contacts sibling of admin-calendar
    // (carddav-server.md § Independent enablement).
    composable("settings/admin-contacts") {
        com.fauna.app.ui.screen.settings.AdminContactsScreen(navController)
    }
    // Flat admin-files page (admin.md § Files): the
    // WebDAV-enable files sibling of admin-contacts
    // (webdav-server.md § Independent enablement).
    composable("settings/admin-files") {
        com.fauna.app.ui.screen.settings.AdminFilesScreen(navController)
    }
    composable("settings/admin-web") {
        com.fauna.app.ui.screen.settings.AdminWebScreen(navController)
    }
    composable("settings/admin-aliases") {
        com.fauna.app.ui.screen.settings.AdminAliasesScreen(navController)
    }
    composable("settings/admin-bridges-pending") {
        com.fauna.app.ui.screen.settings.AdminBridgesPendingScreen(navController)
    }
    // admin-dns: the full DNS record matrix + managed-mode master
    // switch (dns-management.md § App surface). The standalone
    // admin-services page was removed in the 2026-06-04 redesign
    // (admin.md § Admin IA redesign) — its one live toggle (pairing)
    // moved to admin-nest, the dns master switch lives here.
    composable("settings/admin-dns") {
        com.fauna.app.ui.screen.settings.AdminDnsScreen(navController)
    }
    composable("search/{query}") { entry ->
        val query = java.net.URLDecoder.decode(
            entry.arguments?.getString("query") ?: "", "UTF-8"
        )
        SearchResultsScreen(navController, query)
    }

    // New Phase 2 detail routes
    composable("feed/compose") { FeedComposeScreen(navController) }
    composable(
        "feed/post/{postId}?source={source}",
        arguments = listOf(
            navArgument("postId") { type = NavType.StringType },
            navArgument("source") { type = NavType.StringType; defaultValue = "" }
        )
    ) { entry ->
        val postId = java.net.URLDecoder.decode(
            entry.arguments?.getString("postId") ?: "", "UTF-8"
        )
        val source = entry.arguments?.getString("source") ?: ""
        PostDetailScreen(navController, postId, source)
    }
}

/**
 * The bridge legs of the universal post-auth hook, run once on the authed app
 * surface's mount beside `PostAuthGlueVM`'s legs.
 *
 * First-authenticated-setup glue (onboarding.md § Enable-email at claim ·
 * mail-credentials.md § Auto-enable for new users · caldav-server.md
 * § Independent enablement): run once now that the authed session exists.
 * Mail is am_i_admin-discriminated (admin honors the enable-email checkbox;
 * a new non-admin user auto-mints per the deployment policy); CalDAV/CardDAV
 * fire their own Admin-class set_*_enabled toggle AND — on a calendar-only
 * / contacts-only first setup — mint the admin's shared MSEK, gated on
 * whether an earlier sibling already did (exactly one MSEK-minting path per
 * first setup). Each gates independently, but SEQUENTIALLY in one effect,
 * not four independent LaunchedEffects: the mint gates need the ORIGINAL
 * mail/CalDAV request booleans, snapshotted below BEFORE either glue call
 * consumes its own latch, so the gate can never depend on which coroutine
 * happens to run first (MailEnableGlueVM.applyPendingCaldavEnable's doc).
 * The three cheap DAV flips run AHEAD of the mail mint (whose duration is
 * unbounded under load) — safe because every mint gate below decides off
 * the PEEKED request booleans, never off whether mail's own mint actually
 * finished, so reordering changes latency, not correctness. Mirrors the
 * "mail awaited first blocks the cheap DAV flips behind it" coupling
 * windows was fixed for (onboarding.md § the four-enable coupling).
 * No-op on every ordinary authed launch (the first-setup latches are
 * null/false).
 */
@Composable
fun KidsExcisedPostAuthEffects() {
    val mailGlue: com.fauna.app.ui.viewmodel.MailEnableGlueVM = hiltViewModel()
    LaunchedEffect(Unit) {
        val mailWasRequested = mailGlue.peekMailWasRequested()
        val caldavWasRequested = mailGlue.peekCaldavWasRequested()
        mailGlue.applyPendingCaldavEnable(mailWasRequested)
        mailGlue.applyPendingCarddavEnable(mailWasRequested, caldavWasRequested)
        mailGlue.applyPendingWebdavEnable()
        mailGlue.provisionMailAtFirstSetup()
    }
    // encryption-at-rest.md § Capability tiering → Content-sealing epochs:
    // slide the published mail epoch-seal-key horizon forward at the same
    // universal post-auth hook. Best-effort, logged only.
    LaunchedEffect(Unit) { mailGlue.refreshMailEpochSchedule() }
    // critical-alerts.md § Mechanism → Who runs the detector: run the
    // feeders that have no page of their own, at the same universal
    // post-auth hook — immediately, then every RE_SWEEP_INTERVAL_SECS for
    // as long as the identity lives. Not a suspend call: the loop is launched on
    // CriticalAlertsHost's own process-wide scope, so this effect returns
    // right away instead of blocking on a coroutine that never completes.
    LaunchedEffect(Unit) { mailGlue.runCriticalAlertSweepLoop() }
}

/**
 * Global, permanent, non-dismissable "something is very wrong" banner
 * (`docs/goal/behavior/critical-alerts.md`; ui.yaml `global:` `critical-alerts`
 * / `critical-alert[N]`, user-approved 2026-07-23). Mounted alongside
 * [ConnectionStatusBar]/[MessageBanner]/[SupervisedIndicator] (outside any nav
 * route's composable), so it renders on every authenticated page. Absent while
 * no alert is active (the presence rule ui.yaml states); an active alert
 * disappears only when its condition is re-checked and found resolved, never
 * from a user gesture — unlike [MessageBanner]'s rows, these carry no dismiss
 * button. Destructive-styled (same red as the `error-message` banner) and
 * deliberately plain: visual polish is a per-app follow-on, not part of the rendering contract.
 *
 * Thin VM-backed wrapper over the stateless [CriticalAlertsBannerContent] —
 * mirrors the `*Content` split `AtprotoSettingsScreen`/`AtprotoSettingsContent`
 * use, so the rendering itself is testable with no Hilt/VM/FFI.
 */
@Composable
fun CriticalAlertsBanner(vm: com.fauna.app.ui.viewmodel.CriticalAlertsVM = hiltViewModel()) {
    val alerts by vm.active.collectAsState()
    CriticalAlertsBannerContent(alerts)
}

/** Stateless rendering of [alerts] — see [CriticalAlertsBanner]. */
@Composable
fun CriticalAlertsBannerContent(alerts: List<uniffi.fauna_client_alerts.CriticalAlertRow>) {
    if (alerts.isEmpty()) return
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(Ids.CRITICAL_ALERTS),
    ) {
        alerts.forEach { row ->
            val text = row.lines.mapNotNull { localized(it) }.joinToString(" ")
            Text(
                text = text,
                color = Color.White,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier
                    .fillMaxWidth()
                    .background(Color(0xFFB00020))
                    .padding(horizontal = 16.dp, vertical = 10.dp)
                    .testTag(Ids.CRITICAL_ALERT),
            )
        }
    }
}
