package com.fauna.app.ui.navigation

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.StringRes
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import androidx.navigation.NavController
import androidx.navigation.NavType
import androidx.navigation.compose.*
import androidx.navigation.navArgument
import androidx.compose.material3.windowsizeclass.WindowWidthSizeClass
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.BuildConfig
import com.fauna.app.core.AppMessages
import com.fauna.app.core.AppState
import com.fauna.app.ui.components.ReportHost
import com.fauna.app.ui.screen.LaunchAccountIndexUnreadableScreen
import com.fauna.app.ui.screen.LaunchIdentityChangedScreen
import com.fauna.app.ui.screen.LaunchNeedsUpdateScreen
import com.fauna.app.ui.screen.LaunchSignInRefusedScreen
import com.fauna.app.ui.screen.LaunchRetryScreen
import com.fauna.app.ui.screen.onboarding.*
import com.fauna.app.ui.screen.conversations.*
import com.fauna.app.ui.screen.contacts.ContactsScreen
import com.fauna.app.ui.screen.profile.ProfileScreen
import com.fauna.app.ui.screen.media.*
import com.fauna.app.ui.screen.backups.*
import com.fauna.app.ui.screen.devices.DevicesRosterScreen
import com.fauna.app.ui.screen.folders.FoldersScreen
import com.fauna.app.ui.screen.events.*
import com.fauna.app.ui.screen.notifications.NotificationsScreen
import com.fauna.app.ui.screen.settings.*
import com.fauna.app.ui.screen.status.*
import com.fauna.app.ui.util.LocalConnectionState
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.AccountSettingsVM
import com.fauna.app.ui.viewmodel.ConnectionStatusVM
import com.fauna.app.ui.viewmodel.ScreenTimeLockVM
import com.fauna.app.ui.viewmodel.SupervisedIndicatorVM
import com.fauna.ffi.connectionStateLabel
import kotlinx.coroutines.launch
import social.fauna.generated.Ids

val LocalSnackbarHostState = staticCompositionLocalOf<SnackbarHostState> {
    error("No SnackbarHostState provided")
}

val LocalAppMessages = staticCompositionLocalOf<AppMessages> {
    error("No AppMessages provided")
}

/**
 * Global connection-status indicator pinned to the top of the shell content —
 * the live nest WS-RPC connection state (`connection-status`), the Android twin
 * of linux's top-of-sidebar indicator and the web SPA's `connectionStatus`
 * store. Reads Connected/Connecting…/Disconnected off the shared
 * `NestClient::connection_state()` watch (pumped through UniFFI); a transient
 * Watchtower-swap gap shows as "Connecting…", never an error banner.
 */
@Composable
fun ConnectionStatusBar(vm: ConnectionStatusVM = hiltViewModel()) {
    val state by vm.connectionState.collectAsState()
    // The state → label decision is the shared `connection_state_label`
    // (transport.md § Connection-status indicator) — routes through the same
    // one owner web (wasm) and linux/tui (fauna_core::format) already do,
    // instead of hand-rolling the match here.
    val label = localized(connectionStateLabel(state)).orEmpty()
    Text(
        text = label,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp)
            .testTag(Ids.CONNECTION_STATUS)
    )
}

/**
 * Persistent message banner displayed at the top of the content area.
 * Uses Modifier.testTag("error-message") so E2E tests can read the error text.
 */
@Composable
fun MessageBanner() {
    val appMessages = LocalAppMessages.current
    val pageError by appMessages.error.collectAsState()
    val refusal by appMessages.refusedAgentCommand.collectAsState()
    val warning by appMessages.warning.collectAsState()
    val info by appMessages.info.collectAsState()

    // A refused test-agent command outranks the page's own error — precedence
    // owned by `AppMessages.errorForDisplay`; recomputed here off the two
    // collected states so the banner recomposes when either changes.
    val error = refusal ?: pageError

    when {
        error != null -> BannerRow(
            text = error,
            background = Color(0xFFB00020),
            testTag = Ids.ERROR_MESSAGE,
            onDismiss = {
                // Whichever is actually on screen (a human tapping dismiss is
                // outside the refusal slot's reset-only test contract).
                if (refusal != null) appMessages.clearRefusedAgentCommand()
                else appMessages.clearError()
            }
        )
        warning != null -> BannerRow(
            text = warning!!,
            background = Color(0xFFF9A825),
            testTag = Ids.WARNING_MESSAGE,
            onDismiss = { appMessages.clearWarning() }
        )
        info != null -> BannerRow(
            text = info!!,
            background = Color(0xFF1565C0),
            testTag = Ids.INFO_MESSAGE,
            onDismiss = { appMessages.clearInfo() }
        )
    }
}

/**
 * Global, permanent, non-dismissable chrome for supervised accounts
 * (family-safety.md § App surface — `supervised-indicator`): renders only
 * when `fauna.family.status.supervised_by` is set, and navigates to the
 * `family` page on tap. Mounted alongside [ConnectionStatusBar]/[MessageBanner]
 * (outside any nav route's composable), so it shows on every authenticated
 * page — the one conditionally-present global element.
 */
@Composable
fun SupervisedIndicator(navController: NavController, vm: SupervisedIndicatorVM = hiltViewModel()) {
    val guardianHandle by vm.supervisedByHandle.collectAsState()
    guardianHandle?.let { handle ->
        Text(
            text = stringResourceFmt(R.string.family_supervised_indicator, handle),
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier
                .fillMaxWidth()
                .clickable { navController.navigate("settings/family") }
                .padding(horizontal = 16.dp, vertical = 8.dp)
                .testTag(Ids.SUPERVISED_INDICATOR),
        )
    }
}

/**
 * The ward's full-screen screen-time lock (family-safety.md § Screen time,
 * Slice E; ui.yaml `global:` `screen-time-lock` / `screen-time-lock-message`,
 * user-approved 2026-07-15) — a conditionally-present global overlay covering
 * the page content ONLY (mounted around [NavHost] in a `Box`, not the
 * TopAppBar/drawer chrome), so the header's `supervised-indicator` — the
 * ward's route back to the Family page — stays reachable while locked.
 * Deliberately absent while [onFamilyPage] is true: the goal-doc invariant is
 * that a locked ward can always see who supervises them and what the policy
 * is.
 *
 * **Client-enforced by construction** — this overlay IS the enforcement
 * (family-safety.md § Don't do these: *"Don't put screen-time or content
 * enforcement on the nest"*). No policy logic lives here: the verdict and its
 * wording both come from [com.fauna.app.core.ScreenTimeStore], the single
 * decision point, via [ScreenTimeLockVM].
 */
@Composable
fun ScreenTimeLock(onFamilyPage: Boolean, vm: ScreenTimeLockVM = hiltViewModel()) {
    val message by vm.lockMessage.collectAsState()
    if (onFamilyPage || message == null) return
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .testTag(Ids.SCREEN_TIME_LOCK),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
            modifier = Modifier.padding(24.dp),
        ) {
            Text(stringResource(R.string.family_screen_lock_title), style = MaterialTheme.typography.headlineSmall)
            Text(
                message ?: "",
                modifier = Modifier.testTag(Ids.SCREEN_TIME_LOCK_MESSAGE),
                style = MaterialTheme.typography.bodyMedium,
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
            )
            Text(
                stringResource(R.string.family_screen_lock_family_hint),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
            )
        }
    }
}

@Composable
private fun BannerRow(text: String, background: Color, testTag: String, onDismiss: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(background)
            .padding(horizontal = 16.dp, vertical = 10.dp)
            .testTag(testTag),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Text(
            text = text,
            color = Color.White,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.weight(1f)
        )
        IconButton(onClick = onDismiss) {
            Icon(Icons.Default.Close, contentDescription = stringResource(R.string.common_dismiss), tint = Color.White)
        }
    }
}

/**
 * A nav-drawer destination. [labelRes] is the shared i18n key the peers' nav use
 * for this destination (priority #1/#3 — same nav labels on every app, mirroring
 * web `+layout.svelte` / linux `sidebar.rs::label()`).
 */
data class DrawerItem(
    val route: String,
    @StringRes val labelRes: Int,
    val icon: ImageVector,
)

/** Resolve the drawer label from the shared i18n key. */
@Composable
private fun DrawerItem.resolveLabel(): String = stringResource(labelRes)

val drawerItems = listOfNotNull(
    DrawerItem("conversations", R.string.conversations_list_title, Icons.Default.Email),
    DrawerItem("contacts", R.string.common_contacts, Icons.Default.Person),
    DrawerItem("profile", R.string.profile_title, Icons.Default.AccountCircle),
    // No `p2p` drawer item: ui.yaml navigation.layouts.mobile.android omits p2p and
    // the p2p page notes say Android "hosts the peer node headlessly via its
    // foreground service" — the WG-era contacts/QR-invite screens were removed
    // 2026-07-13 (p2p.md § Implementation status today, invite path).
    DrawerItem("events", R.string.events_title, Icons.Default.DateRange),
    if (BuildConfig.KIDS) null else DrawerItem("feed", R.string.common_feed, Icons.Default.Star),
    // Nostr keeps its own dedicated top-level tab (nostr.md § Page structure,
    // ratified 2026-06-13) — a genuinely first-class integration, not folded
    // into the generic Bridges list. `nostr-tab` is an unconditional
    // `navigation.tabs` entry (ui.yaml), same category as `bridges-tab`.
    if (BuildConfig.KIDS) null else DrawerItem("nostr", R.string.nostr_title, Icons.Default.Public),
    if (BuildConfig.KIDS) null else DrawerItem("bridges", R.string.common_bridges, Icons.Default.Share),
    DrawerItem("media", R.string.media_title, Icons.Default.Menu),
    DrawerItem("backups", R.string.backups_title, Icons.Default.Lock),
    // Devices (roster) + Folders are now Settings sub-pages (2026-06-28 sync/
    // folder UI unification): reached inside Settings, not as a top-level drawer
    // item. `Peers`/top-level Devices is removed (settings.md § Navigation model).
    DrawerItem("notifications", R.string.common_notifications, Icons.Default.Notifications),
    // Standalone Moderation page (moderation.md § Architectural rules 1) — android's
    // top-level nav analogue of the macOS/linux sidebar tab; auto-tagged
    // `moderation-tab`.
    DrawerItem("moderation", R.string.settings_moderation, Icons.Default.Shield),
    DrawerItem("settings", R.string.common_settings, Icons.Default.Settings),
)

/**
 * Top-level drawer destinations that are **shells** — surfaces you enter and
 * exit, whose sub-page position is shell state rather than session state
 * (`ui/README.md` § Navigation model → *Entering a shell lands on its
 * canonical entry*). android has exactly one: Settings, which also hosts the
 * admin pages as `settings/admin*` sub-pages rather than as a second shell.
 *
 * Everything else in [drawerItems] is an ordinary page, and an ordinary page
 * keeping its own stack across a visit is outside this rule (qualification 3)
 * — unless it is also in [PUSH_STACK_PAGES] below, which is a narrower,
 * different rule.
 */
private val SHELL_ROUTES = setOf("settings")

/**
 * Single-pane push-stack drawer destinations whose own detail can *cover*
 * them (`ui/README.md` § Navigation model → *A nav to a page shows that
 * page's own primary surface (push-stack pages; ratified 2026-08-28)*).
 * `conversations` pushes `conversation/{threadId}` as a flat sibling, plain
 * `navigate()`d from [ConversationListScreen] — not nested under
 * `conversations` in the graph — so the drawer's multi-stack idiom below can
 * save and restore a covered detail exactly the way it can restore a shell's
 * stale sub-page. Measured 2026-09-27 by [ConversationsPushStackReEntryTest].
 *
 * Distinct from [SHELL_ROUTES]: this targets an ordinary page, not a shell,
 * and (unlike the shell edge) resets on *every* nav that targets the page —
 * the ratified rule carries no reload exception, because a push-stack page
 * has no "sub-page you are entitled to stay on" the way a shell does.
 */
private val PUSH_STACK_PAGES = setOf("conversations")

/**
 * Navigate the way the drawer does — the one place the top-level nav options
 * live, so both the drawer and the test agent cross the same edge and a test
 * can drive the real thing rather than a replica of it.
 *
 * `popUpTo(start) { saveState }` + `restoreState` is the multi-stack bottom-nav
 * idiom: each top-level destination keeps its own back stack across switches.
 * That is right for ordinary pages and **wrong for a shell** — measured
 * 2026-08-14 by [DrawerShellReEntryTest], which is what settled android's
 * long-open "unmeasured" entry in `ui/README.md` § Navigation model: over
 * android's flat graph the saved stack really is restored, so leaving Settings
 * from `settings/account` and re-entering through the drawer landed the user
 * back on `settings/account` instead of the Settings root. The same restore
 * is **also wrong for a [PUSH_STACK_PAGES] page** for the identical reason —
 * measured 2026-09-27 by [ConversationsPushStackReEntryTest]: leaving
 * `conversations` with a thread open and re-entering through the drawer
 * restored the thread, covering the list the rule says a targeting nav must
 * show.
 *
 * So the restore is withheld:
 *
 *  * entering a **shell** from outside it → no restore, land on the canonical
 *    entry; re-selecting the shell you are already in → restore (a *reload*,
 *    not an edge — qualification 1 forbids evicting the user from the
 *    sub-page they are on);
 *  * navigating to a **push-stack page** → no restore, land on the page's own
 *    root, on every nav that targets it (no reload exception — a push-stack
 *    page has no sub-page the user is entitled to keep, the way a shell's
 *    non-canonical sub-pages are);
 *  * any other destination → unchanged.
 *
 * Intra-page navigation (`ConversationListScreen`'s `navigate("conversation/…")`)
 * and deep links are untouched: they are plain `navigate(route)` calls that
 * never come through here.
 */
fun NavController.navigateToDrawerRoute(route: String) {
    val current = currentDestination?.route
    val alreadyInside = current == route || current?.startsWith("$route/") == true
    val enteringShell = route in SHELL_ROUTES && !alreadyInside
    val targetingPushStackPage = route in PUSH_STACK_PAGES
    navigate(route) {
        popUpTo(graph.startDestinationId) { saveState = true }
        launchSingleTop = true
        restoreState = !enteringShell && !targetingPushStackPage
    }
}

/**
 * The onboarding wizard's route table — shared by the cold-boot launch-routing
 * flow (`AppLaunchVM.NavTarget.Wizard`) and the "Add account" append-mode
 * overlay (`appState.isAddingAccount`), which drives the SAME shared
 * `OnboardingHost.machine` (a process-wide Hilt singleton) fresh off a reset.
 * The screens themselves are unaware of which caller mounted them.
 */
@Composable
private fun OnboardingWizardNavHost(
    navController: androidx.navigation.NavHostController,
    startDestination: String,
    onWizardExit: (com.fauna.ffi.onboarding.WizardOutcome) -> Unit,
    // What a sign-out's erase could not remove (`AppState.signOutResidue`),
    // painted as `identity_choice`'s `sign-out-residue` view. `null` on every
    // other entry to the wizard, including the "Add account" overlay, which
    // erases nothing.
    signOutResidue: com.fauna.ffi.FfiSignOutResidue? = null,
    onRemoveAgain: (com.fauna.ffi.FfiSignOutResidue) -> Unit = {},
) {
    NavHost(navController, startDestination = startDestination) {
        composable("onboarding/identity-choice") {
            IdentityChoiceScreen(
                navController,
                signOutResidue = signOutResidue,
                onRemoveAgain = onRemoveAgain,
            )
        }
        composable("onboarding/identity-created") { IdentityCreatedScreen(navController) }
        composable("onboarding/identity-import") { IdentityImportScreen(navController) }
        composable(RECOVERY_KIT_ROUTE) { RecoveryKitScreen(navController) }
        composable(RECOVERY_ENTRY_ROUTE) { RecoveryEntryScreen(navController) }
        composable("onboarding/handle-entry") {
            HandleEntryScreen(navController, onWizardExit)
        }
        composable("onboarding/invite-request") {
            InviteRequestScreen(navController, onWizardExit)
        }
        composable("onboarding/dns-config") {
            DnsConfigScreen(navController)
        }
        composable("onboarding/vps-config") {
            VpsConfigScreen(navController, onWizardExit)
        }
        composable("onboarding/nest-provisioning") {
            NestProvisioningScreen(navController, onWizardExit)
        }
        composable("onboarding/dns-post-instructions") {
            DnsPostInstructionsScreen(navController, onWizardExit)
        }
        // Not an OnboardingStep — the "Almost ready" surface is keyed on
        // wizardOutcome() == AwaitingManualDns, reached both from the
        // dns-post-instructions exit and from the launch machine's
        // deferred-DNS row (AppLaunchVM.navTargetFor).
        composable("onboarding/almost-ready") {
            AwaitingManualDnsScreen(navController, onWizardExit)
        }
        composable("onboarding/claim-code") {
            ClaimCodeScreen(navController, onWizardExit)
        }
        // The terminal admin-path setup step, reached directly on a
        // successful claim (onboarding.md § 3b-bis).
        composable("onboarding/nat-mode-choice") {
            NatModeChoiceScreen(navController, onWizardExit)
        }
        // The one-tap "trust this box" offer (onboarding.md § 3b-ter),
        // reached from nat-mode-choice's two exits once OnboardingHost
        // declares setRendersTrustPrompt(true).
        composable("onboarding/trust-prompt") {
            TrustPromptScreen(navController, onWizardExit)
        }
        // box-recovery.md § Recovery UI (step 4) — Task E.
        composable("onboarding/nest-recovery") {
            NestRecoveryScreen(navController)
        }
        composable("onboarding/recover-selfhosted-instructions") {
            RecoverSelfhostedInstructionsScreen(navController)
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FaunaNavHost(
    widthSizeClass: WindowWidthSizeClass = WindowWidthSizeClass.Compact,
    appState: AppState = AppState(),
) {
    val navController = rememberNavController()
    LaunchedEffect(navController) { appState.navController = navController }
    val snackbarHostState = remember { SnackbarHostState() }

    if (appState.isAddingAccount) {
        // "Add account" (long-term-store.md § Multi-account evolution) — the
        // append-mode wizard, driven fresh off the SAME shared `OnboardingHost`
        // machine the cold-boot wizard uses (a process-wide Hilt singleton, reset
        // by AccountSettingsVM.beginAddAccount() before this branch mounts).
        // Bypasses AppLaunchVM/LaunchMachine entirely — this is a fresh onboard
        // for a NEW identity, not a launch-routing decision for the current one.
        val addAccountVm: AccountSettingsVM = hiltViewModel()
        val appendNavController = rememberNavController()
        OnboardingWizardNavHost(
            navController = appendNavController,
            startDestination = "onboarding/identity-choice",
            onWizardExit = {
                addAccountVm.completeAddAccount(it) {
                    appState.isAddingAccount = false
                    appState.isOnboarding = true
                }
            },
        )
    } else if (appState.isOnboarding) {
        val launchVm: com.fauna.app.ui.viewmodel.AppLaunchVM = hiltViewModel()

        // Per docs/goal/behavior/onboarding.md §App-launch routing. The
        // LaunchMachine in shared Rust drives silent-challenge HTTP +
        // bearer lifecycle + 401-reactive; FaunaNavHost just observes
        // the resulting LaunchPhase and mounts the right surface.
        //
        // Cold start: kick off machine.start() once; subsequent retries
        // call machine.retrySilentChallenge() via launchVm.retry(). The
        // machine's observer feeds snapshot back into the StateFlow we
        // collect here.
        val snapshot by launchVm.snapshot.collectAsState()
        LaunchedEffect(Unit) { launchVm.start() }

        val onWizardExit = { _: com.fauna.ffi.onboarding.WizardOutcome ->
            appState.isOnboarding = false
            Unit
        }

        when (
            val target = launchVm.navTargetFor(
                snapshot.phase,
                snapshot.accountIndexRefusal,
                snapshot.signInRefused,
                supersededSuccessor = snapshot.supersededSuccessor,
            )
        ) {
            null -> { /* in-flight: cold-start blank screen */ }
            com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget.Authenticated -> {
                LaunchedEffect(Unit) {
                    launchVm.connectActiveSession()
                    appState.isOnboarding = false
                }
            }
            com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget.TransientError -> {
                val scope = rememberCoroutineScope()
                LaunchRetryScreen(
                    onRetry = { scope.launch { launchVm.retry() } },
                    onUseDifferentNest = {
                        launchVm.seedForFallthrough()
                        navController.navigate("onboarding/handle-entry")
                    },
                )
            }
            com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget.NeedsUpdate -> {
                // The nest reported it is outdated — a non-retry surface that
                // renders the localized `last_error` in `error-message`, no Retry
                // (version-compatibility.md Dim 4). Only "Use a different nest".
                LaunchNeedsUpdateScreen(
                    message = snapshot.lastError ?: "",
                    onUseDifferentNest = {
                        launchVm.seedForFallthrough()
                        navController.navigate("onboarding/handle-entry")
                    },
                )
            }
            com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget.SignInRefused -> {
                // The nest no longer signs this identity in (suspended or
                // removed — deliberately unsaid). Unlike NeedsUpdate this one
                // HAS Retry: the admin's restore is the way back in
                // (onboarding.md § App-launch routing, previously-signed-in row).
                val scope = rememberCoroutineScope()
                LaunchSignInRefusedScreen(
                    onRetry = { scope.launch { launchVm.retry() } },
                    onUseDifferentNest = {
                        launchVm.seedForFallthrough()
                        navController.navigate("onboarding/handle-entry")
                    },
                )
            }
            com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget.IdentityChanged -> {
                // The nest's pinned deployment identity changed (security.md §
                // Transport trust) — no Retry CTA, only "trust this
                // nest" or "use a different nest".
                val scope = rememberCoroutineScope()
                LaunchIdentityChangedScreen(
                    onTrust = { scope.launch { launchVm.trustIdentity() } },
                    onUseDifferentNest = {
                        launchVm.seedForFallthrough()
                        navController.navigate("onboarding/handle-entry")
                    },
                )
            }
            is com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget.Superseded -> {
                // This identity was succeeded (identity-succession.md § Propagation
                // → *Own device fleet*): the import flow, with the machine-held
                // reason on its `error-message` — claim-free first, naming the
                // successor only once the chain walk verifies it
                // (`AppLaunchVM.routeSupersededRefusal` owns the why).
                val claimFree = stringResource(R.string.onboarding_launch_identity_superseded)
                val verifiedTemplate = stringResource(R.string.onboarding_launch_identity_superseded_verified)
                LaunchedEffect(target.claimedSuccessor) {
                    launchVm.routeSupersededRefusal(
                        target.claimedSuccessor,
                        claimFree,
                        verifiedReason = { successor -> verifiedTemplate.replace("{successor}", successor) },
                    )
                }
                OnboardingWizardNavHost(navController, "onboarding/identity-import", onWizardExit)
            }
            is com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget.AccountIndexUnreadable -> {
                // The saved account index is present and this build cannot use
                // it (version-compatibility.md § 5 item 9) — never a retry or
                // fallthrough. `resetAccountIndex()` re-runs the launch machine
                // itself, so the recomposition off the updated `snapshot` is
                // what lands on `WizardAt(IdentityChoice)`; no explicit
                // `navController.navigate` needed here.
                val scope = rememberCoroutineScope()
                LaunchAccountIndexUnreadableScreen(
                    refusal = target.refusal,
                    onReset = { scope.launch { launchVm.resetAccountIndex() } },
                )
            }
            is com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget.Wizard -> {
                val scope = rememberCoroutineScope()
                // The signed-out launch re-sweeps a residue a previous sign-out
                // recorded FIRST, and paints only what is still left
                // (`account-scoping.md` § Erasure follows scope → *the residue
                // surface*). Skipped when this process's own sign-out already
                // handed one over — that one is fresh.
                LaunchedEffect(Unit) {
                    if (appState.signOutResidue == null) {
                        appState.signOutResidue = launchVm.recheckSignOutResidue()
                    }
                }
                OnboardingWizardNavHost(
                    navController,
                    target.startDestination,
                    onWizardExit,
                    signOutResidue = appState.signOutResidue,
                    onRemoveAgain = { residue ->
                        scope.launch {
                            appState.signOutResidue = launchVm.retrySignOutResidue(residue)
                        }
                    },
                )
            }
        }
    } else {
        val drawerState = rememberDrawerState(DrawerValue.Closed)
        val scope = rememberCoroutineScope()

        // Convention 17 layer (c) — `e2e-systematic-ui-walks.md` § The
        // convention. `focus_move` needs a live root FocusManager to call
        // `moveFocus` on; `TestAgent` (src/debug) is not a Composable, so it
        // cannot read `LocalFocusManager.current` itself. `SideEffect` re-runs
        // on every successful recomposition (mirroring apple's
        // `FocusRegionRegistry` re-register-every-render-pass idiom), so a
        // config change or navigation never leaves a stale reference behind.
        // `BuildConfig.DEBUG`-gated so R8 folds this to nothing in release —
        // `TestAgent.focusManager` in the release/storeSafe twin is a no-op
        // field nothing ever reads (testing.md convention 15).
        if (BuildConfig.DEBUG) {
            val focusManager = androidx.compose.ui.platform.LocalFocusManager.current
            SideEffect { com.fauna.app.testing.TestAgent.focusManager = focusManager }
        }

        val postAuthGlue: com.fauna.app.ui.viewmodel.PostAuthGlueVM = hiltViewModel()
        // The bridge legs of the same hook (mail / CalDAV / CardDAV / WebDAV
        // first-setup enables, the mail epoch refresh, the critical-alert
        // sweep) — compiled out of the kids build type.
        KidsExcisedPostAuthEffects()

        // security.md § Post-auth surfacing: one-shot post-auth identity
        // re-check, at the same universal post-auth hook — the android leg
        // of linux/tui/macOS/iOS/web's post-auth `NestIdentityChanged`
        // routing. A `true` result means the
        // check found the nest's identity changed mid-session and already
        // tore the session down (credentials kept); re-enter the real
        // launch flow exactly like a same-account reconnect
        // ([AccountSettingsVM.switchAccount]'s `onComplete`), which re-runs
        // the same [com.fauna.app.ui.viewmodel.AppLaunchVM] machine instance
        // and lands it on `launch_identity_changed` with a live machine
        // behind the trust button.
        val launchVmPostAuth: com.fauna.app.ui.viewmodel.AppLaunchVM = hiltViewModel()
        LaunchedEffect(Unit) {
            if (launchVmPostAuth.performPostAuthSilentSignIn()) {
                appState.isOnboarding = true
            }
        }
        // The same verdicts MID-SESSION: a reconnect supervisor that stopped
        // for good on one (a suspension's 4401 teardown + refused re-mint,
        // a supersession, a changed nest identity) ends the session through
        // the same door and the same re-entry — the launch surface with no
        // relaunch (`onboarding.md` § App-launch routing, the
        // previously-signed-in row; `security.md` § Post-auth surfacing).
        // A supersession THIS device's own stolen-identity ceremony caused is
        // held back while that ceremony owns the Account page
        // (`AppLaunchVM.routeSessionEnding`, `StolenCeremonyHold`).
        LaunchedEffect(Unit) {
            launchVmPostAuth.sessionEnding.collect { verdict ->
                launchVmPostAuth.routeSessionEnding(verdict) { appState.isOnboarding = true }
            }
        }

        // onboarding.md § 3b-ter: mint the one-tap trust answer latched on
        // the trust_prompt page. No-op unless the user granted it.
        LaunchedEffect(Unit) { postAuthGlue.mintDefaultTrustSet() }
        // identity-succession.md § The RecoveryKey → Creation UX: register the
        // kit confirmed on the sign-up recovery_kit page. No-op if skipped.
        LaunchedEffect(Unit) { postAuthGlue.registerDeferredRecoveryKit() }
        // box-recovery.md § The plane-era recovery floor, (c) The writes: the
        // deployment-seed custody leg at the post-auth edge, on every connect —
        // the only capture, so the nest's `nest_actor_id` survives total box
        // loss. Unlike the best-effort mail/caldav glue, custody left
        // unconfirmed for an admin is surfaced as a warning banner — the
        // android idiom of linux's toast (cleared on the next navigation, like
        // the page-scoped errors). Nothing shows when custody is held or this
        // identity is not an admin here.
        val custodyMismatchMsg = stringResource(R.string.launch_recovery_custody_mismatch)
        val custodyFailedMsg = stringResource(R.string.launch_recovery_custody_failed)
        LaunchedEffect(Unit) {
            when (postAuthGlue.runDeploymentSeedCustodyLeg()) {
                com.fauna.app.ui.viewmodel.PostAuthGlueVM.RecoveryCustodyOutcome.NOT_PROTECTED_MISMATCH ->
                    appState.messages.showWarning(custodyMismatchMsg)
                com.fauna.app.ui.viewmodel.PostAuthGlueVM.RecoveryCustodyOutcome.NOT_PROTECTED_FAILED ->
                    appState.messages.showWarning(custodyFailedMsg)
                com.fauna.app.ui.viewmodel.PostAuthGlueVM.RecoveryCustodyOutcome.OK -> {}
            }
        }
        // domains-and-tls-bootstrap.md § Host-address acquisition: fire-and-forget
        // leg at the same universal post-auth hook. Best-effort, logged only.
        LaunchedEffect(Unit) { postAuthGlue.reportHostAddress() }
        // file-sync.md § Sealed names & paths → Implementation status today
        // (S8 D1 + D3): stamp missing sealed siblings + snapshot tag seals,
        // same universal post-auth hook.
        LaunchedEffect(Unit) { postAuthGlue.runSealBackfill() }

        var searchQuery by remember { mutableStateOf("") }
        var isSearchActive by remember { mutableStateOf(false) }

        val currentBackStackEntry by navController.currentBackStackEntryAsState()
        val currentRoute = currentBackStackEntry?.destination?.route
        val currentTitle =
            drawerItems.find { currentRoute?.startsWith(it.route) == true }?.resolveLabel() ?: "Fauna"

        // Clear messages on navigation — errors are page-scoped
        LaunchedEffect(currentRoute) {
            appState.messages.clear()
        }

        // The offline gate's transport state, read off the SAME view model the
        // `connection-status` indicator below reads (`ConnectionStatusBar`), so
        // the indicator and every gated control cannot disagree about what
        // "connected" means — W4 (account-data-plane.md § Workstreams) phase 4, `account-data-plane.md` § The
        // offline-mutation contract → *How a surface asks*. Provided here, at
        // the shell, because the gate must reach every page.
        val connectionVm: ConnectionStatusVM = hiltViewModel()
        val gateConnectionState by connectionVm.connectionState.collectAsState()

        CompositionLocalProvider(
            LocalSnackbarHostState provides snackbarHostState,
            LocalAppMessages provides appState.messages,
            LocalConnectionState provides gateConnectionState,
        ) {
            val notifPermissionLauncher = rememberLauncherForActivityResult(
                ActivityResultContracts.RequestPermission()
            ) { /* no-op */ }

            LaunchedEffect(Unit) {
                if (android.os.Build.VERSION.SDK_INT >= 33) {
                    notifPermissionLauncher.launch(android.Manifest.permission.POST_NOTIFICATIONS)
                }
            }

            ModalNavigationDrawer(
                drawerState = drawerState,
                drawerContent = {
                    ModalDrawerSheet(modifier = Modifier.width(280.dp)) {
                        Text(
                            "Fauna",
                            style = MaterialTheme.typography.titleLarge,
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.padding(24.dp)
                        )
                        HorizontalDivider()
                        drawerItems.forEach { item ->
                            val itemLabel = item.resolveLabel()
                            NavigationDrawerItem(
                                icon = { Icon(item.icon, contentDescription = itemLabel) },
                                label = { Text(itemLabel) },
                                selected = currentRoute?.startsWith(item.route) == true,
                                onClick = {
                                    navController.navigateToDrawerRoute(item.route)
                                    scope.launch { drawerState.close() }
                                },
                                modifier = Modifier
                                    .padding(NavigationDrawerItemDefaults.ItemPadding)
                                    .testTag("${item.route}-tab")
                            )
                        }
                    }
                }
            ) {
                Scaffold(
                    snackbarHost = { SnackbarHost(snackbarHostState) },
                    topBar = {
                        if (!BuildConfig.KIDS && isSearchActive) {
                            SearchBar(
                                query = searchQuery,
                                onQueryChange = { searchQuery = it },
                                onSearch = {
                                    if (searchQuery.isNotBlank()) {
                                        navController.navigate("search/${java.net.URLEncoder.encode(searchQuery, "UTF-8")}")
                                        isSearchActive = false
                                        searchQuery = ""
                                    }
                                },
                                active = true,
                                onActiveChange = { if (!it) isSearchActive = false },
                                placeholder = { Text(stringResource(R.string.search_page_search_messages)) },
                                leadingIcon = {
                                    IconButton(
                                        onClick = { isSearchActive = false; searchQuery = "" },
                                        modifier = Modifier.testTag(Ids.SEARCH_CANCEL_BUTTON)
                                    ) {
                                        Icon(Icons.AutoMirrored.Filled.ArrowBack, "Close search")
                                    }
                                },
                                trailingIcon = {
                                    Row {
                                        if (searchQuery.isNotEmpty()) {
                                            IconButton(
                                                onClick = { searchQuery = "" },
                                                modifier = Modifier.testTag(Ids.SEARCH_CLEAR_BUTTON)
                                            ) {
                                                Icon(Icons.Default.Close, "Clear search")
                                            }
                                            IconButton(
                                                onClick = {
                                                    if (searchQuery.isNotBlank()) {
                                                        navController.navigate("search/${java.net.URLEncoder.encode(searchQuery, "UTF-8")}")
                                                        isSearchActive = false
                                                        searchQuery = ""
                                                    }
                                                },
                                                modifier = Modifier.testTag(Ids.SEARCH_SUBMIT_BUTTON)
                                            ) {
                                                Icon(Icons.Default.Search, "Submit search")
                                            }
                                        }
                                    }
                                },
                                modifier = Modifier.testTag(Ids.SEARCH_QUERY_FIELD)
                            ) {}
                        } else {
                            TopAppBar(
                                title = { Text(currentTitle, modifier = Modifier.testTag(Ids.PAGE_HEADING)) },
                                navigationIcon = {
                                    IconButton(onClick = { scope.launch { drawerState.open() } }) {
                                        Icon(Icons.Default.Menu, "Menu")
                                    }
                                },
                                actions = {
                                    // Discovery is compiled out of Fauna Kids.
                                    if (!BuildConfig.KIDS) {
                                        IconButton(
                                            onClick = { isSearchActive = true },
                                            modifier = Modifier.testTag(Ids.SEARCH_TOGGLE_BUTTON)
                                        ) {
                                            Icon(Icons.Default.Search, "Search")
                                        }
                                    }
                                }
                            )
                        }
                    }
                ) { padding ->
                    Column(Modifier.padding(padding)) {
                        ConnectionStatusBar()
                        MessageBanner()
                        CriticalAlertsBanner()
                        SupervisedIndicator(navController)
                        // The one shared report sheet + its acknowledgement
                        // (moderation.md § User-initiated reporting): mounted
                        // once, over whatever page is showing, so a report filed
                        // from a card the reporter-side hide then replaces still
                        // paints `report-status`.
                        ReportHost()
                        // A succession's successor lands on Settings → Account
                        // FIRST, and only then does the Recovery kit section
                        // claim and mint its owed kit (`identity-succession.md`
                        // § The RecoveryKey → *At succession*: navigate
                        // synchronously, mint after — entering Account clears any
                        // kit on screen, so navigating after the mint would wipe
                        // the very thing the step exists to show). A peek: it
                        // claims nothing. Effects launch after this composition
                        // applies, so the NavHost below already holds its graph.
                        LaunchedEffect(Unit) {
                            if (launchVmPostAuth.owesSuccessorKitHere()) {
                                navController.navigateToDrawerRoute("settings/account")
                            }
                        }
                        // The screen-time lock (family-safety.md § Screen
                        // time) covers the page content only, not the chrome
                        // above (supervised-indicator stays reachable) — a
                        // `Box` so the lock paints ON TOP of the NavHost
                        // rather than after it.
                        Box(modifier = Modifier.weight(1f)) {
                        NavHost(navController, "conversations", modifier = Modifier.fillMaxSize()) {
                        // Drawer routes
                        composable("conversations") { ConversationListScreen(navController) }
                        composable("contacts") { ContactsScreen(navController) }
                        // Search deep link (SearchNav.Contact — search.md § Where logic
                        // lives → *Result navigation (deep link)*): a distinct path
                        // segment, not a "contacts?openUidHash=" query variant of the
                        // bare route above — the drawer's `navigateToDrawerRoute`
                        // compares `currentDestination?.route` against the plain
                        // "contacts" string (DrawerShellReEntryTest pins this), so
                        // widening that route's own pattern would silently break
                        // shell re-entry/highlighting. ContactsVM reads `openUidHash`
                        // via SavedStateHandle exactly like `profile/{actorId}` reads
                        // `actorId`.
                        composable(
                            "contacts/card/{openUidHash}",
                            arguments = listOf(navArgument("openUidHash") { type = NavType.StringType }),
                        ) { ContactsScreen(navController) }
                        composable("profile") { ProfileScreen(navController) }
                        // OTHER actor's profile (tap-through from a contact row): the
                        // actorId arg drives ProfileVM.isSelf=false (offers + follow).
                        composable(
                            "profile/{actorId}",
                            arguments = listOf(navArgument("actorId") { type = NavType.StringType }),
                        ) { ProfileScreen(navController) }
                        composable("events") { EventsScreen(navController) }
                        composable("media") { MediaScreen(navController) }
                        // Search deep link (SearchNav.File — search.md § Where logic
                        // lives → *Result navigation (deep link)*): same rationale as
                        // `contacts/card/{openUidHash}` above — a distinct path segment
                        // rather than widening the bare "media" route's pattern.
                        // MediaVM reads `openFolderId`/`openPathHash` via SavedStateHandle.
                        composable(
                            "media/file/{openFolderId}/{openPathHash}",
                            arguments = listOf(
                                navArgument("openFolderId") { type = NavType.StringType },
                                navArgument("openPathHash") { type = NavType.StringType },
                            ),
                        ) { MediaScreen(navController) }
                        // Standalone Moderation queue (fauna.moderation.actions +
                        // train-correction-button → fauna.moderation.train; moderation.md
                        // § Layout & flow).
                        composable("moderation") {
                            com.fauna.app.ui.screen.moderation.ModerationQueueScreen(navController)
                        }

                        // Settings routes
                        composable("settings") {
                            SettingsScreen(navController)
                        }
                        composable("settings/account") {
                            AccountSettingsScreen(
                                navController,
                                onSignOut = { residue ->
                                    // Carry what the erase could not remove to
                                    // the onboarding surface this mounts — the
                                    // shell painting it is about to be replaced.
                                    appState.signOutResidue = residue
                                    appState.isOnboarding = true
                                },
                                onAccountSwitched = { appState.isOnboarding = true },
                                onAddAccount = { appState.isAddingAccount = true },
                            )
                        }
                        composable("settings/nests") {
                            com.fauna.app.ui.screen.settings.LinkedNestsScreen(navController)
                        }
                        // Task delegation (participants.md § Task delegation; placed
                        // after Nests, settings.md § Navigation model): the per-kind
                        // runner + assignment surface over the shared
                        // FfiTaskDelegationView.
                        composable("settings/task-delegation") {
                            com.fauna.app.ui.screen.settings.TaskDelegationScreen(navController)
                        }
                        // The Family surface (family-safety.md § App surface),
                        // reached via the gated in-Settings `family-tab` entry.
                        composable("settings/family") {
                            com.fauna.app.ui.screen.settings.FamilyScreen(navController)
                        }
                        composable("settings/privacy") { PrivacySettingsScreen(navController) }
                        // Members To Review — the permanent post-succession
                        // unattested-member review page.
                        composable("settings/member-review") {
                            com.fauna.app.ui.screen.settings.MemberReviewScreen(navController)
                        }
                        composable("settings/muted-words") { MutedWordsScreen(navController) }
                        composable("settings/encryption") { EncryptionSettingsScreen(navController) }
                        // Settings → Folders (2026-06-28 unification — the renamed +
                        // expanded former `settings/sync`): folder list + create wizard
                        // + per-set config (incl. folder-conflict-policy-select) + conflicts.
                        composable("settings/folders") { FoldersScreen(navController) }
                        composable("settings/status") { StatusScreen() }
                        // Settings → Logs: the client's durable fauna-log ring
                        // (observability.md § Surfaces). Reached as the {"view":
                        // "settings","id":"logs"} rail sub-page.
                        composable("settings/logs") {
                            com.fauna.app.ui.screen.settings.SettingsLogsScreen(navController)
                        }
                        composable("backups") { SnapshotListScreen(navController) }
                        // Settings → Devices (2026-06-28 unification): the device roster,
                        // re-homed under the Settings shell (was the top-level Peers/Devices
                        // page). Folder list/wizard/conflicts live on settings/folders.
                        composable("settings/devices") { DevicesRosterScreen(navController) }
                        composable("notifications") { NotificationsScreen() }

                        // Photo backup: the standalone settings/photo-backup route retired
                        // 2026-07-19 (media.md § Photo-backup reframe) — its controls now
                        // render as photo-backup-controls on settings/folders.

                        // Content moderation
                        composable("settings/admin") {
                            com.fauna.app.ui.screen.settings.AdminDashboardScreen(navController)
                        }
                        composable("settings/admin-users") {
                            com.fauna.app.ui.screen.settings.AdminUsersScreen(navController)
                        }
                        composable("settings/admin-settings") {
                            // Nav-labelled "Tiers" after the per-page-services redesign
                            // (admin.md § Admin IA redesign): tier definitions only.
                            // The storage-mode indicator + Factory Reset moved to
                            // settings/admin-nest below.
                            com.fauna.app.ui.screen.settings.AdminSettingsScreen(navController)
                        }
                        // The Nest page (admin.md § N Nest): nest-wide settings —
                        // storage-mode indicator + admin pairing toggle + Factory
                        // Reset (moved off admin-settings/admin-services in the
                        // 2026-06-04 redesign). Factory reset re-seeds onboarding at
                        // claim-code (returned code pre-filled — AppLaunchVM preserves
                        // it): flip into the onboarding flow, identity kept (clearAuth).
                        composable("settings/admin-nest") {
                            com.fauna.app.ui.screen.settings.AdminNestScreen(
                                navController,
                                onFactoryResetComplete = { appState.isOnboarding = true },
                            )
                        }
                        // admin-custody-hosting: the nest-wide custody-hosting
                        // registry. A CONTEXTUAL detail page like
                        // admin-dns/admin-logs — no ui.yaml nav-list entry, the
                        // dashboard menu is the reach point.
                        composable("settings/admin-custody-hosting") {
                            com.fauna.app.ui.screen.settings.AdminCustodyHostingScreen(navController)
                        }
                        // Admin Logs: the nest's fauna-log ring over
                        // fauna.admin.logs (observability.md § Surfaces). An
                        // in-shell switcher entry (admin.md § Navigation model).
                        composable("settings/admin-logs") {
                            com.fauna.app.ui.screen.settings.AdminLogsScreen(navController)
                        }

                        // Watched-directory config (mobile sync ingress). No longer
                        // linked from the Media page after the 2026-07-01 explorer
                        // rework; kept reachable-by-route pending a re-home decision
                        // (folder config belongs in Settings → Folders, media.md
                        // rule 4 — follow-on work tracked internally).
                        composable("media/watched") {
                            WatchedDirectoryScreen(navController)
                        }

                        composable("conversation/{threadId}") { entry ->
                            val threadId = entry.arguments?.getString("threadId") ?: ""
                            ConversationDetailScreen(navController, threadId)
                        }

                        // New-thread compose (conversations.md §"New-thread
                        // compose lives in the detail pane"). Reached from the
                        // list FAB, which calls manager.startNewConversation().
                        composable("conversation_compose") {
                            NewThreadComposeScreen(navController)
                        }

                        composable("snapshot/{snapshotId}") { entry ->
                            val snapshotId = entry.arguments?.getString("snapshotId")?.toIntOrNull() ?: 0
                            SnapshotDetailScreen(navController, snapshotId)
                        }


                        // The destinations the kids build type compiles out —
                        // the feed, search, every bridge, web publishing,
                        // monetization and third-party anything (src/noKids;
                        // an empty twin in src/kids).
                        kidsExcisedDestinations(navController)

                        composable("event/{eventId}") { entry ->
                            val eventId = entry.arguments?.getString("eventId") ?: ""
                            EventDetailScreen(navController, eventId)
                        }
                        }
                        ScreenTimeLock(onFamilyPage = currentRoute == "settings/family")
                        } // end Box
                    } // end Column
                }
            }
        }
    }
}
