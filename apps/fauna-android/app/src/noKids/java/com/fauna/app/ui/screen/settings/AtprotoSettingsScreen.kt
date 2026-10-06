package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.FeedTriple
import com.fauna.app.core.SourceAskRows
import com.fauna.app.core.UrlOpener
import com.fauna.app.core.sourceAskRows
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.screen.bridges.BridgeCard
import com.fauna.app.ui.util.SuppressScreenCapture
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.formatNamed
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.AtprotoVM
import com.fauna.app.ui.viewmodel.BridgesVM
import com.fauna.ffi.FfiBridgeFollow
import com.fauna.ffi.FfiBridgeLinkField
import com.fauna.ffi.FfiBridgeLinkMode
import com.fauna.ffi.FfiBridgeStatus
import com.fauna.ffi.FfiCborValue
import com.fauna.ffi.delegationCapabilityLabel
import com.fauna.ffi.delegationLivenessLabel
import com.fauna.ffi.identityStatusLabel
import kotlinx.coroutines.launch
import uniffi.fauna_atproto_settings_machine.AppCredentialRow
import uniffi.fauna_atproto_settings_machine.AtprotoSettingsSnapshot
import uniffi.fauna_atproto_settings_machine.depthLevelOptions
import java.text.DateFormat
import java.util.Date
import social.fauna.generated.Ids

/**
 * The **Bluesky** settings page (`docs/goal/ui/atproto.md`): one page answers
 * one question — how deep is this user's Bluesky integration? The spine is
 * the four-rung integration-depth selector; every other control reveals below
 * it as a sub-setting of the level that makes it meaningful:
 *
 * - **selector** (`atproto-depth-*`) — one ordered choice; selecting a
 *   *different* level stages the transition card, never mutates the level
 *   directly (Off → Linked is the one effect-free move, applied on select).
 *   Hosted rungs grey-with-reason on a non-public domain.
 * - **transition card** (`atproto-depth-confirm-card`) — the composed effect
 *   lines, rendered *verbatim* from the machine's `pendingTransition.lines`.
 * - **Linked-account panel** — the consume-side link surface at level =
 *   `linked`: the shared [BridgeCard] embedded verbatim from the Bridges page
 *   (zero new element IDs — ui/atproto.md § Element IDs). Replaces the Bluesky
 *   provider row the unified Bridges page used to carry (§ Migration step 2).
 * - **hosted panel** — pre-mint: the DID-method radio; post-mint: the
 *   identity summary (`atproto-hosted-handle`).
 * - **full-PDS panel** — the F1 login-plane surface (app credentials,
 *   connected-app sessions, the external-apps kill-switch), gated on level =
 *   `hosted_full`.
 * - **delete presence** (`atproto-delete-presence`) — visible whenever a
 *   hosted identity exists; the destructive flow itself is S5-scoped (no
 *   click handler yet, matching every other app).
 *
 * Backed by the shared `AtprotoSettingsMachine` via [AtprotoVM] (observer +
 * secret hybrid, see that class's doc). The Linked panel reads a SECOND,
 * independent [BridgesVM] instance rather than a shared app-wide cache —
 * android has no such cache (unlike linux/apple/tui); this mirrors web's own
 * independent-fetch precedent for the same reason.
 *
 * Stateless [AtprotoSettingsContent] is split out for the Compose test
 * harness; the VM-bound [AtprotoSettingsScreen] is the wrapper the NavHost
 * mounts. Linux lead: apps/fauna-linux/src/settings/atproto.rs.
 *
 * ## What declares a wire kind here
 *
 * Nine class-3 (OnlineOnly) kinds over eleven [faunaGate] declarations — the
 * largest single page in android's offline-gate fan-out
 * (`account-data-plane.md` § The offline-mutation contract). Every kind comes
 * from the lead app's fallback-free `Action::wire_kind`
 * (`apps/fauna-tui/src/settings/mod.rs`), never from a control's name:
 *
 * - **the four depth rungs AND the transition card's confirm** —
 *   `atproto.set_integration_level`, one kind across both, because
 *   `select_level` reaches the nest directly on the effect-free Off -> Linked
 *   move and stages the card on every other pick. Two declaration sites, one
 *   kind.
 * - **the delete-presence confirm** — `atproto.delete_presence`.
 * - **the external-apps kill switch** — `atproto.set_external_apps_enabled`; a
 *   dispatch-on-change toggle IS the commit.
 * - **mint / revoke on a credential row** —
 *   `atproto.provision_app_credential` / `.revoke_app_credential`.
 * - **a connected app's revoke** — `atproto.revoke_session`.
 * - **consent approve AND deny** — `atproto.resolve_consent`, one verdict for
 *   the pair: the "no" is the same wire call with a different boolean, and the
 *   browser blocked on the other side only gets a clean refusal once it lands.
 * - **delegation authorize (BOTH call sites) and revoke** —
 *   `atproto.provision_authoring_delegation` / `.revoke_authoring_delegation`.
 *   The first-grant button and the live-row RENEWAL are the same ceremony, so
 *   declaring only the branch reachable today would ungate the other silently.
 *
 * Everything else must survive the outage beside them, and the page is graded
 * on that half: the **contest confirm** — signed and submitted on this device's
 * own connection to the public PLC directory, no nest call at all, which is the
 * whole point of a remedy for a nest that may be the attacker (the lead app
 * pins `AtprotoRequestContest` -> no kind); the credential **reveal** —
 * class Read, recovered from this device's own `fauna.state.atproto` store
 * under the D3 custody split; the delete-presence **opener** (gating it would
 * make the confirm unreachable rather than dead); every **cancel**; and the
 * DID-method radio and history-backfill checkbox, which are drafts the machine
 * holds locally until a confirm commits them.
 */
@Composable
fun AtprotoSettingsScreen(
    navController: NavController,
    vm: AtprotoVM = hiltViewModel(),
    bridgesVm: BridgesVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val allBridges by bridgesVm.bridges.collectAsState()
    val follows by bridgesVm.follows.collectAsState()
    val feedAsks by bridgesVm.feedAsks.collectAsState()
    val refusedFeed by bridgesVm.refusedFeed.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(Unit) {
        vm.refresh()
        bridgesVm.refresh()
    }

    val errorText = remember(snapshot.error) { resolveLocalized(context, snapshot.error) }
    LaunchedEffect(errorText) { errorText?.let { appMessages.showError(it) } }
    // The embedded card's own failures — a guardian refusal of the link
    // included, which must STAY on `error-message` beside its ask button
    // (family-safety.md § Feed-source approvals, rule (b)).
    val bridgesError by bridgesVm.error.collectAsState()
    LaunchedEffect(bridgesError) { bridgesError?.let { appMessages.showError(it) } }

    AtprotoSettingsContent(
        snapshot = snapshot,
        blueskyBridge = allBridges.find { it.id == "bluesky" },
        blueskyFollows = follows["bluesky"] ?: emptyList(),
        onBack = { navController.popBackStack() },
        onOpenContestConfirm = vm::openContestConfirm,
        onCancelContest = vm::cancelContest,
        onRequestContest = vm::requestContest,
        onOpenDeleteConfirm = vm::openDeleteConfirm,
        onCancelDelete = vm::cancelDelete,
        onConfirmDelete = vm::confirmDelete,
        onSelectLevel = vm::selectLevel,
        onConfirmTransition = vm::confirmTransition,
        onCancelTransition = vm::cancelTransition,
        onSetDidMethod = vm::setDidMethod,
        onSetHistoryBackfill = vm::setHistoryBackfill,
        onSetExternalAppsEnabled = vm::setExternalAppsEnabled,
        onMint = vm::mint,
        onRevealSecret = vm::revealSecret,
        onRevoke = vm::revoke,
        onAuthorizeExternalApps = vm::authorizeExternalApps,
        onDeauthorizeExternalApps = vm::deauthorizeExternalApps,
        onLinkBridge = { mode, fields ->
            bridgesVm.linkBridge("bluesky", mode, fields) { url ->
                UrlOpener.open(context, url)
            }
        },
        onUnlinkBridge = { bridgesVm.unlinkBridge("bluesky") },
        onUpdateBridgeSetting = { key, value -> bridgesVm.updateSetting("bluesky", key, value) },
        onAddBridgeFollow = { id, petname -> bridgesVm.addFollow("bluesky", id, petname) },
        onRemoveBridgeFollow = { followId -> bridgesVm.removeFollow("bluesky", followId) },
        blueskySourceAsks = remember(feedAsks, refusedFeed) { sourceAskRows(feedAsks, refusedFeed, "bluesky") },
        onAskBridgeSource = bridgesVm::requestFeedSource,
    )
}

/** A plausible unlinked-surface placeholder for when no `fauna.bridges.list`
 *  reply has landed yet, or the nest was built without the bluesky provider
 *  feature (the e2e nest today — ui/atproto.md's own test note). The honest
 *  state in either case is "nothing is linked"; mirrors tui's `embed_bridge_card`
 *  synthetic case. The real reply (the instant it arrives) always supersedes
 *  this — [AtprotoSettingsScreen] passes it whenever `allBridges` has a
 *  "bluesky" row. */
private fun syntheticUnlinkedBluesky() = FfiBridgeStatus(
    id = "bluesky",
    name = "Bluesky",
    available = true,
    linked = false,
    identity = null,
    mode = null,
    settings = emptyList(),
    supportsFollows = false,
    linkModes = listOf(
        FfiBridgeLinkMode(
            mode = "oauth",
            label = "Bluesky",
            clientAction = "oauth_redirect",
            platform = null,
            fields = listOf(FfiBridgeLinkField("handle", "Handle", "text", null)),
        ),
    ),
    error = null,
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AtprotoSettingsContent(
    snapshot: AtprotoSettingsSnapshot,
    blueskyBridge: FfiBridgeStatus?,
    blueskyFollows: List<FfiBridgeFollow>,
    onBack: () -> Unit,
    onOpenContestConfirm: () -> Unit,
    onCancelContest: () -> Unit,
    onRequestContest: () -> Unit,
    onOpenDeleteConfirm: () -> Unit,
    onCancelDelete: () -> Unit,
    onConfirmDelete: () -> Unit,
    onSelectLevel: (String) -> Unit,
    onConfirmTransition: () -> Unit,
    onCancelTransition: () -> Unit,
    onSetDidMethod: (String) -> Unit,
    onSetHistoryBackfill: (Boolean) -> Unit,
    onSetExternalAppsEnabled: (Boolean) -> Unit,
    // Mint a new app credential; returns (credentialId, secret) so the caller
    // can show the secret inline on the just-minted row (the only time it is
    // ever shown — never in the passive snapshot, D3).
    onMint: suspend (label: String, dmAllowed: Boolean) -> Pair<String, String>?,
    onRevealSecret: suspend (credentialId: String) -> String?,
    onRevoke: (credentialId: String) -> Unit,
    onAuthorizeExternalApps: () -> Unit,
    onDeauthorizeExternalApps: () -> Unit,
    onLinkBridge: (mode: String, fields: Map<String, String>) -> Unit,
    onUnlinkBridge: () -> Unit,
    onUpdateBridgeSetting: (String, FfiCborValue) -> Unit,
    onAddBridgeFollow: (String, String?) -> Unit,
    onRemoveBridgeFollow: (String) -> Unit,
    // The ward's feed-source ask rows for the embedded card (family-safety.md
    // § Feed-source approvals) — the Bridges page's own, since the card is.
    blueskySourceAsks: SourceAskRows = SourceAskRows(),
    onAskBridgeSource: (FeedTriple) -> Unit = {},
) {
    val scope = rememberCoroutineScope()
    // Secrets revealed (by mint or explicit reveal) this session, keyed by
    // credentialId. Never persisted, never part of the snapshot (D3) — mirrors
    // the linux lead's `ctx.revealed` cache.
    val revealed = remember { mutableStateMapOf<String, String>() }
    val defaultCredentialLabelTemplate = stringResource(R.string.atproto_settings_default_credential_label)

    // security.md § On-screen secret exposure, rule 2: an app credential is a
    // minted, revocable capability, so suppress capture for exactly as long as one
    // is on screen. Gated on the map rather than the page — a page-wide flag would
    // leave the whole Bluesky screen unscreenshottable with no secret in sight.
    if (revealed.isNotEmpty()) {
        SuppressScreenCapture()
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.atproto_settings_title),
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
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .testTag(Ids.ATPROTO_PAGE)
                .padding(padding)
                .padding(16.dp)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            // ── The 72 h recovery-fork contest — LEADS the page, above the
            //    selector (behavior/atproto-identity-custody.md § The 72 h
            //    recovery-fork contest). Every string below is
            //    machine-composed and rendered verbatim — mirrors
            //    apps/fauna-linux/src/settings/atproto.rs and
            //    apps/fauna-tui/src/settings/atproto.rs::contest_elements,
            //    the reference shape every shell copies. ──
            snapshot.contest?.let { card ->
                ContestCard(card = card, onOpen = onOpenContestConfirm)
                snapshot.contestConfirm?.let { confirm ->
                    ContestConfirmCard(
                        confirm = confirm,
                        onConfirm = onRequestContest,
                        onCancel = onCancelContest,
                    )
                }
            }

            DepthSelector(snapshot, onSelectLevel)

            snapshot.pendingTransition?.let { card ->
                TransitionCard(
                    card = card,
                    historyBackfill = snapshot.historyBackfill,
                    onSetHistoryBackfill = onSetHistoryBackfill,
                    onConfirm = onConfirmTransition,
                    onCancel = onCancelTransition,
                )
            }

            if (snapshot.level == "linked") {
                BridgeCard(
                    bridge = blueskyBridge ?: syntheticUnlinkedBluesky(),
                    bridgeFollows = blueskyFollows,
                    onLink = onLinkBridge,
                    onUnlink = onUnlinkBridge,
                    onUpdateSetting = onUpdateBridgeSetting,
                    onAddFollow = onAddBridgeFollow,
                    onRemoveFollow = onRemoveBridgeFollow,
                    sourceAsks = blueskySourceAsks,
                    onAskSource = onAskBridgeSource,
                )
            }

            val targetingHosted = snapshot.pendingTransition?.targetLevel?.startsWith("hosted") == true
            val atHosted = snapshot.level.startsWith("hosted")
            if (atHosted || targetingHosted) {
                HostedPanel(snapshot, onSetDidMethod)
            }

            // The identity summary: gated on the IDENTITY, not the level.
            // `ui/atproto.md` § Errors & edge cases: "A deactivated identity
            // at level Off/Linked: the identity summary renders (marked
            // deactivated) so the user can see what re-enabling restores."
            // This used to sit inside `HostedPanel`, the one place the rule
            // can never hold — the states it names are exactly the two
            // `atHosted || targetingHosted` closes on.
            // `apps/fauna-tui/src/settings/atproto.rs` leads the fix; this is
            // the trickle-down leg.
            snapshot.identity?.let { identity ->
                // The one shared reading (`identity_status_label`); an
                // unrecognized status degrades to its wire word.
                val statusContext = LocalContext.current
                val statusLabel = remember(identity.status) {
                    resolveLocalized(statusContext, identityStatusLabel(identity.status)) ?: identity.status
                }
                Text(
                    "${stringResourceFmt(R.string.atproto_settings_hosted_handle_prefix, identity.handle)} · " +
                        "${stringResourceFmt(R.string.atproto_settings_hosted_method_prefix, identity.method)} · $statusLabel",
                    modifier = Modifier.testTag(Ids.ATPROTO_HOSTED_HANDLE),
                )
            }

            if (snapshot.showDeletePresence) {
                OutlinedButton(
                    onClick = onOpenDeleteConfirm,
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.testTag(Ids.ATPROTO_DELETE_PRESENCE),
                ) {
                    Text(stringResource(R.string.atproto_settings_delete_presence_button))
                }
            }
            // Its own confirm card — never the depth selector's. The copy is
            // the machine's, rendered verbatim.
            snapshot.deleteConfirm?.let { confirm ->
                DeleteConfirmCard(confirm = confirm, onConfirm = onConfirmDelete, onCancel = onCancelDelete)
            }

            if (snapshot.level == "hosted_full") {
                FullPdsPanel(
                    snapshot = snapshot,
                    revealed = revealed,
                    defaultCredentialLabelTemplate = defaultCredentialLabelTemplate,
                    onSetExternalAppsEnabled = onSetExternalAppsEnabled,
                    onMint = { scope.launch { onMint(formatNamed(defaultCredentialLabelTemplate, listOf(snapshot.credentials.size + 1)), false)?.let { (id, secret) -> revealed[id] = secret } } },
                    onRevealSecret = { id -> scope.launch { onRevealSecret(id)?.let { revealed[id] = it } } },
                    onRevoke = onRevoke,
                    onAuthorizeExternalApps = onAuthorizeExternalApps,
                    onDeauthorizeExternalApps = onDeauthorizeExternalApps,
                )
            }
        }
    }
}

/** The recovery-fork contest card (`atproto-contest-card`). This function
 *  derives nothing — every string is machine-composed and rendered
 *  verbatim; the only decision here is which ids exist, read straight off
 *  the snapshot (`card.showContest` for the button). That is deliberate:
 *  the copy names what undoing does NOT restore, and seven shells
 *  paraphrasing that sentence seven ways is how a user ends up believing a
 *  compromised box was fully evicted when it was not.
 */
@Composable
private fun ContestCard(card: uniffi.fauna_atproto_settings_machine.ContestCardRow, onOpen: () -> Unit) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .testTag(Ids.ATPROTO_CONTEST_CARD)
            .semantics { stateDescription = card.state },
    ) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.atproto_settings_contest_card_heading), style = MaterialTheme.typography.titleMedium)
            Text(localized(card.detail) ?: "", modifier = Modifier.testTag(Ids.ATPROTO_CONTEST_DETAIL), style = MaterialTheme.typography.bodyMedium)
            card.deadline?.let { deadline ->
                Text(
                    localized(deadline) ?: "",
                    modifier = Modifier.testTag(Ids.ATPROTO_CONTEST_DEADLINE),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            // Decision 2: the button exists only where pressing it can work.
            if (card.showContest) {
                Button(
                    onClick = onOpen,
                    colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.testTag(Ids.ATPROTO_CONTEST),
                ) {
                    Text(stringResource(R.string.atproto_settings_contest_button))
                }
            }
        }
    }
}

@Composable
private fun ContestConfirmCard(
    confirm: uniffi.fauna_atproto_settings_machine.ContestConfirmCardModel,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ATPROTO_CONTEST_CONFIRM_CARD)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            for (line in confirm.lines) {
                Text(localized(line) ?: "", style = MaterialTheme.typography.bodyMedium)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = onCancel, enabled = !confirm.inProgress, modifier = Modifier.testTag(Ids.ATPROTO_CONTEST_CANCEL)) {
                    Text(stringResource(R.string.atproto_settings_contest_cancel_button))
                }
                Button(
                    onClick = onConfirm,
                    enabled = !confirm.inProgress,
                    colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.testTag(Ids.ATPROTO_CONTEST_CONFIRM),
                ) {
                    Text(stringResource(R.string.atproto_settings_contest_confirm_button))
                }
            }
        }
    }
}

@Composable
private fun DepthSelector(snapshot: AtprotoSettingsSnapshot, onSelectLevel: (String) -> Unit) {
    // Rung order, ids, copy and the hosted-gate flag come from the shared
    // catalog (`atproto.md` § Where logic lives already named the machine the
    // owner of "level logic … all of it"). Android used to carry a
    // `DEPTH_LEVELS` list AND a parallel `rungTitles` map, held in sync by the
    // level string alone.
    val rungs = remember { depthLevelOptions() }
    val reasonText = localized(snapshot.hostedGateReason)
    // ONE verdict for the whole selector, because the rungs issue ONE kind.
    // `selectLevel` reaches the nest directly only on the effect-free
    // Off -> Linked move; every other pick stages the transition card whose
    // confirm issues it — and the lead app collapses BOTH paths onto this one
    // kind for exactly that reason (`Action::AtprotoSelectLevel |
    // Action::AtprotoConfirmTransition` in `apps/fauna-tui/src/settings/mod.rs`).
    // So there is no reachable rung that stays available with no nest, and one
    // reason renders beneath the group rather than four beside four rungs
    // (§ R11 — per affordance, never a banner).
    val levelGate = faunaGate("fauna.bridges.atproto.set_integration_level")

    Column(
        modifier = Modifier
            .testTag(Ids.ATPROTO_DEPTH_SELECTOR)
            .semantics { stateDescription = snapshot.level },
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(stringResource(R.string.atproto_settings_depth_heading), style = MaterialTheme.typography.titleMedium)
        for (rung in rungs) {
            val level = rung.level
            val id = rung.uiId
            val active = snapshot.level == level
            // `hosted` is the catalog's fact, not a `startsWith("hosted")`
            // sniff this screen re-runs on a level string it did not define.
            val gated = rung.hosted && !snapshot.hostedAllowed
            // The page's own hosted gate AND the offline verdict. Composed, never
            // replaced: a hosted rung on a non-public domain keeps its own, more
            // specific reason (rendered inline below) whether or not a nest is
            // reachable, and a reconnect restores exactly this intent.
            val enabled = (!gated || active) && levelGate.enabled
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .selectable(selected = active, enabled = enabled, onClick = { if (level != snapshot.level) onSelectLevel(level) })
                    .padding(vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RadioButton(
                    selected = active,
                    enabled = enabled,
                    onClick = { if (level != snapshot.level) onSelectLevel(level) },
                    modifier = Modifier.testTag(id),
                )
                Column(modifier = Modifier.padding(start = 4.dp)) {
                    Text(localized(rung.title) ?: "", style = MaterialTheme.typography.bodyLarge)
                    Text(
                        localized(rung.description) ?: "",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    if (gated && !reasonText.isNullOrEmpty()) {
                        Text(reasonText, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                    }
                }
            }
        }
        DisabledControlReasonText(levelGate.reason)
    }
}

@Composable
private fun TransitionCard(
    card: uniffi.fauna_atproto_settings_machine.TransitionCardModel,
    historyBackfill: Boolean,
    onSetHistoryBackfill: (Boolean) -> Unit,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ATPROTO_DEPTH_CONFIRM_CARD)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.atproto_settings_depth_card_heading), style = MaterialTheme.typography.titleMedium)
            for (line in card.lines) {
                Text(localized(line) ?: "", style = MaterialTheme.typography.bodyMedium)
            }
            if (card.showHistoryBackfill) {
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.testTag(Ids.ATPROTO_HISTORY_BACKFILL)) {
                    Checkbox(checked = historyBackfill, onCheckedChange = onSetHistoryBackfill)
                    Text(stringResource(R.string.atproto_settings_history_backfill_label))
                }
            }
            // The commit gates, not the buffer: the card's own history-backfill
            // checkbox above is a draft the machine holds locally, so it stays
            // live and the user may stage the whole transition with no nest.
            val confirmGate = faunaGate(
                "fauna.bridges.atproto.set_integration_level",
                enabled = !card.inProgress,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = onConfirm, enabled = confirmGate.enabled, modifier = Modifier.testTag(Ids.ATPROTO_DEPTH_CONFIRM)) {
                    Text(stringResource(R.string.atproto_settings_depth_confirm_button))
                }
                // Backing out of a staged transition is a pure-local machine
                // mutation (`AtprotoVM.cancelTransition` never dispatches) and
                // must not become unreachable because the nest went away.
                OutlinedButton(onClick = onCancel, enabled = !card.inProgress, modifier = Modifier.testTag(Ids.ATPROTO_DEPTH_CANCEL)) {
                    Text(stringResource(R.string.atproto_settings_depth_cancel_button))
                }
            }
            DisabledControlReasonText(confirmGate.reason)
        }
    }
}

/** Pre-mint: the DID-method radio + the either-way handle line. The identity
 *  summary (`atproto-hosted-handle`) is NOT here — it renders independently
 *  of this panel's `atHosted || targetingHosted` gate (see the call site). */
@Composable
private fun HostedPanel(snapshot: AtprotoSettingsSnapshot, onSetDidMethod: (String) -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        if (snapshot.showDidMethodRadio) {
            Column(modifier = Modifier.testTag(Ids.ATPROTO_DID_METHOD), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(stringResource(R.string.atproto_settings_did_method_heading), style = MaterialTheme.typography.titleSmall)
                DidMethodRung("atproto-did-method-plc", R.string.atproto_settings_did_method_plc_title, R.string.atproto_settings_did_method_plc_desc, snapshot.didMethod == "plc") { onSetDidMethod("plc") }
                DidMethodRung("atproto-did-method-web", R.string.atproto_settings_did_method_web_title, R.string.atproto_settings_did_method_web_desc, snapshot.didMethod == "web") { onSetDidMethod("web") }
                if (snapshot.handlePreview.isNotEmpty()) {
                    Text(
                        stringResourceFmt(R.string.atproto_settings_handle_either_way, snapshot.handlePreview),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun DeleteConfirmCard(
    confirm: uniffi.fauna_atproto_settings_machine.DeleteConfirmCardModel,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ATPROTO_DELETE_CONFIRM_CARD)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            for (line in confirm.lines) {
                Text(localized(line) ?: "", style = MaterialTheme.typography.bodyMedium)
            }
            // The confirm is the round trip — `AtprotoSettingsMachine::confirm_delete`
            // calls `nest_api.delete_presence()`. The OPENER above and the cancel
            // beside it stay live: gating the opener would make this confirm
            // unreachable rather than dead, and backing out of a destructive
            // ceremony must never need a nest.
            val deleteGate = faunaGate(
                "fauna.bridges.atproto.delete_presence",
                enabled = !confirm.inProgress,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = onCancel, enabled = !confirm.inProgress, modifier = Modifier.testTag(Ids.ATPROTO_DELETE_CANCEL)) {
                    Text(stringResource(R.string.atproto_settings_delete_cancel_button))
                }
                Button(
                    onClick = onConfirm,
                    enabled = deleteGate.enabled,
                    colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.testTag(Ids.ATPROTO_DELETE_CONFIRM),
                ) {
                    Text(stringResource(R.string.atproto_settings_delete_confirm_button))
                }
            }
            DisabledControlReasonText(deleteGate.reason)
        }
    }
}

@Composable
private fun DidMethodRung(id: String, titleRes: Int, descRes: Int, selected: Boolean, onClick: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().selectable(selected = selected, onClick = onClick),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        RadioButton(selected = selected, onClick = onClick, modifier = Modifier.testTag(id))
        Column(modifier = Modifier.padding(start = 4.dp)) {
            Text(stringResource(titleRes), style = MaterialTheme.typography.bodyMedium)
            Text(stringResource(descRes), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

/** The F1 login-plane surface: kill-switch, app credentials and the D10
 *  authoring-delegation row — in that order. Gated by the caller on level =
 *  `hosted_full`. The consent card and the connected-app rows moved to the
 *  Connected apps page (`connected-apps.md` § Architectural rules) — a row
 *  moves, it is never shown twice. */
@Composable
private fun FullPdsPanel(
    snapshot: AtprotoSettingsSnapshot,
    revealed: MutableMap<String, String>,
    defaultCredentialLabelTemplate: String,
    onSetExternalAppsEnabled: (Boolean) -> Unit,
    onMint: () -> Unit,
    onRevealSecret: (String) -> Unit,
    onRevoke: (String) -> Unit,
    onAuthorizeExternalApps: () -> Unit,
    onDeauthorizeExternalApps: () -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        // mail-settings-enabled-toggle idiom: rendered unconditionally, bound
        // to computed state, never wrapped in an `if`. Non-destructive in both
        // directions — the lists below stay rendered while this is off.
        // A dispatch-on-change toggle IS the commit — there is no Save beside it
        // to carry the declaration, so the switch itself declares.
        val killSwitchGate = faunaGate("fauna.bridges.atproto.set_external_apps_enabled")
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(stringResource(R.string.atproto_settings_external_apps_toggle), modifier = Modifier.weight(1f))
            Switch(
                checked = snapshot.externalAppsEnabled,
                onCheckedChange = onSetExternalAppsEnabled,
                enabled = killSwitchGate.enabled,
                modifier = Modifier.testTag(Ids.ATPROTO_EXTERNAL_APPS_ENABLE),
            )
        }
        DisabledControlReasonText(killSwitchGate.reason)

        Column(modifier = Modifier.testTag(Ids.ATPROTO_APP_CREDENTIALS_LIST), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.atproto_settings_app_credentials_heading), style = MaterialTheme.typography.titleSmall)
            if (snapshot.credentials.isEmpty()) {
                Text(stringResource(R.string.atproto_settings_app_credentials_empty), color = MaterialTheme.colorScheme.onSurfaceVariant)
            } else {
                snapshot.credentials.forEach { cred ->
                    CredentialItemRow(cred, revealed[cred.credentialId], onRevealSecret, onRevoke)
                }
            }
            val mintGate = faunaGate("fauna.bridges.atproto.provision_app_credential")
            Button(onClick = onMint, enabled = mintGate.enabled, modifier = Modifier.testTag(Ids.ATPROTO_APP_CREDENTIAL_MINT)) {
                Text(stringResource(R.string.atproto_settings_mint_button))
            }
            DisabledControlReasonText(mintGate.reason)
        }

        DelegationRowSection(snapshot.delegation, onAuthorizeExternalApps, onDeauthorizeExternalApps)
    }
}

/** Epoch-millis in the page's own date idiom (the credential/session rows above
 *  use the same `DateFormat.MEDIUM`). Named because the delegation row feeds it
 *  MICROseconds/1000 from the signed cert while the rows above feed it wire
 *  milliseconds directly — keeping the conversion at each call site is what
 *  makes the unit mismatch visible rather than buried in a helper. */
private fun fmtMillis(millis: Long): String =
    DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(millis))

/** The D10 authoring-delegation row (`atproto-delegation-*`) — what authorizes
 *  an external ATProto app to *post* as this account, as opposed to merely
 *  signing in (the kill-switch + credential + connected-app groups above govern
 *  that). Six IDs user-approved 2026-07-29, `-last-used` 2026-07-31; tui leads,
 *  linux/apple/web are the built references.
 *
 *  Two states, one always-present control:
 *
 *  - **`null`** — no delegation, OR one whose stored cert failed the
 *    client-side verify under this account's own identity key. The row and its
 *    leaves are WITHHELD, never rendered as a grant the user cannot be shown to
 *    have made (the mismatch surfaces on `error-message`, which the machine has
 *    already set). Only `-authorize` renders.
 *  - **non-null** — the four leaves render. `-authorize` STAYS, because
 *    re-authorizing IS the renewal gesture: provisioning overwrites the cert, so
 *    a lapsed grant recovers in one gesture with no revoke first. Hiding it once
 *    authorized would force the revoke-then-re-mint flow the ruling forbids. */
@Composable
private fun DelegationRowSection(
    row: uniffi.fauna_atproto_settings_machine.DelegationRow?,
    onAuthorize: () -> Unit,
    onDeauthorize: () -> Unit,
) {
    val context = LocalContext.current
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(R.string.atproto_settings_delegation_heading), style = MaterialTheme.typography.titleSmall)
        if (row == null) {
            Text(stringResource(R.string.atproto_settings_delegation_empty), color = MaterialTheme.colorScheme.onSurfaceVariant)
            // Two call sites, one kind: this is the first-grant button, the one
            // below is the RENEWAL of a live grant. Both are the same D10 mint
            // ceremony (`AtprotoSettingsMachine::authorize_external_apps`), so
            // both declare — a declaration on only the reachable-today branch
            // would ungate the other state silently.
            val authorizeGate = faunaGate("fauna.bridges.atproto.provision_authoring_delegation")
            Button(onClick = onAuthorize, enabled = authorizeGate.enabled, modifier = Modifier.testTag(Ids.ATPROTO_DELEGATION_AUTHORIZE)) {
                Text(stringResource(R.string.atproto_settings_delegation_authorize_button))
            }
            DisabledControlReasonText(authorizeGate.reason)
            return@Column
        }

        // Both label maps come from SHARED Rust rather than a Kotlin `when`:
        // tui and linux call `DelegationRow::{capability_labels,status_label}`
        // directly, and a fourth hand-written copy is where the apps start
        // disagreeing (priority #4). An unrecognized value degrades to its wire
        // form — dropping one would understate a grant.
        val scope = remember(row.capabilities) {
            row.capabilities.joinToString(", ") { cap ->
                resolveLocalized(context, delegationCapabilityLabel(cap)) ?: cap
            }
        }
        val status = remember(row.liveness) {
            resolveLocalized(context, delegationLivenessLabel(row.liveness)) ?: row.liveness
        }

        Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ATPROTO_DELEGATION_ROW)) {
            Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(
                    stringResourceFmt(R.string.atproto_settings_delegation_scope_prefix, scope),
                    modifier = Modifier.testTag(Ids.ATPROTO_DELEGATION_SCOPE),
                    style = MaterialTheme.typography.bodyMedium,
                )
                // MICROseconds on this row — cert-derived, unlike the
                // millisecond credential/session rows above. Dividing by the
                // wrong factor dates the grant ~50 000 years out.
                Text(
                    row.expiresAtMicros?.let { expires ->
                        stringResourceFmt(
                            R.string.atproto_settings_delegation_lasts_until,
                            fmtMillis(row.authorizedAtMicros.toLong() / 1_000),
                            fmtMillis(expires.toLong() / 1_000),
                        )
                    } ?: stringResourceFmt(
                        R.string.atproto_settings_delegation_lasts_until_no_expiry,
                        fmtMillis(row.authorizedAtMicros.toLong() / 1_000),
                    ),
                    modifier = Modifier.testTag(Ids.ATPROTO_DELEGATION_LASTS_UNTIL),
                    style = MaterialTheme.typography.bodySmall,
                )
                // The liveness WIRE spelling rides `contentDescription` so the
                // e2e asserts the STATE, not its prose (the same channel
                // `atproto-consent-code` uses for its raw value).
                Text(
                    status,
                    modifier = Modifier
                        .testTag(Ids.ATPROTO_DELEGATION_STATUS)
                        .semantics { contentDescription = row.liveness },
                    style = MaterialTheme.typography.bodyMedium,
                )
                // ADVISORY (D10 § Audit). Every leaf above derives from the
                // SIGNED cert, re-verified client-side under this account's own
                // identity key; this one does not — the nest simply asserts it,
                // with nothing signing it. So the wording hedges and the hint
                // line points at the feed's delegated-origin-badge, which IS
                // read from signed bytes. An absent stamp means nothing was
                // REPORTED, never that nothing happened: a nest that
                // under-reports is exactly what this value cannot detect.
                Text(
                    row.lastUsedAtMillis?.let {
                        stringResourceFmt(R.string.atproto_settings_delegation_last_used, fmtMillis(it))
                    } ?: stringResource(R.string.atproto_settings_delegation_last_used_never),
                    modifier = Modifier
                        .testTag(Ids.ATPROTO_DELEGATION_LAST_USED)
                        .semantics { contentDescription = "advisory" },
                    style = MaterialTheme.typography.bodySmall,
                )
                Text(
                    stringResource(R.string.atproto_settings_delegation_last_used_hint),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        // TWO verdicts, not one: re-authorize provisions and revoke destroys the
        // signing sub-key, so they issue DIFFERENT kinds and each carries its own
        // reason. Both happen to be OnlineOnly, but reading one verdict for the
        // pair would hard-code that coincidence into the page.
        val reauthorizeGate = faunaGate("fauna.bridges.atproto.provision_authoring_delegation")
        val revokeDelegationGate = faunaGate("fauna.bridges.atproto.revoke_authoring_delegation")
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = onAuthorize, enabled = reauthorizeGate.enabled, modifier = Modifier.testTag(Ids.ATPROTO_DELEGATION_AUTHORIZE)) {
                Text(stringResource(R.string.atproto_settings_delegation_reauthorize_button))
            }
            OutlinedButton(
                onClick = onDeauthorize,
                enabled = revokeDelegationGate.enabled,
                colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                modifier = Modifier.testTag(Ids.ATPROTO_DELEGATION_REVOKE),
            ) {
                Text(stringResource(R.string.atproto_settings_delegation_revoke_button))
            }
        }
        DisabledControlReasonText(reauthorizeGate.reason)
        DisabledControlReasonText(revokeDelegationGate.reason)
    }
}

/** One `atproto-app-credential-item` row. Reveal is gated on `revealable`,
 *  shown even when not gated if this session already revealed it — the
 *  reveal button's own label swaps to the secret, mirroring the linux lead. */
@Composable
private fun CredentialItemRow(
    cred: AppCredentialRow,
    revealedSecret: String?,
    onRevealSecret: (String) -> Unit,
    onRevoke: (String) -> Unit,
) {
    val created = remember(cred.createdAtMillis) { DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(cred.createdAtMillis)) }
    val lastUsedText = cred.lastUsedAtMillis?.let {
        stringResourceFmt(R.string.atproto_settings_credential_last_used_prefix, DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(it)))
    } ?: stringResource(R.string.atproto_settings_credential_never_used)

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ATPROTO_APP_CREDENTIAL_ITEM)) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(cred.label, style = MaterialTheme.typography.bodyLarge)
            Text(
                "${stringResourceFmt(R.string.atproto_settings_credential_created_prefix, created)} · $lastUsedText",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            // Reveal, inches to the left, declares NOTHING and stays live: the
            // lead app treats it as a class-Read local read, which ruling 1
            // leaves alone — the secret is recovered from this device's own
            // `fauna.state.atproto` store, never from the nest (the D3 custody split). It is
            // this row's discriminator: a blanket grey of the credential card
            // would kill it too.
            val revokeGate = faunaGate("fauna.bridges.atproto.revoke_app_credential")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (cred.revealable || revealedSecret != null) {
                    TextButton(
                        onClick = { if (revealedSecret == null) onRevealSecret(cred.credentialId) },
                        enabled = revealedSecret == null,
                        modifier = Modifier.testTag(Ids.ATPROTO_APP_CREDENTIAL_REVEAL),
                    ) {
                        Text(if (revealedSecret != null) revealedSecret else stringResource(R.string.atproto_settings_reveal_button))
                    }
                }
                OutlinedButton(
                    onClick = { onRevoke(cred.credentialId) },
                    enabled = revokeGate.enabled,
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.testTag(Ids.ATPROTO_APP_CREDENTIAL_REVOKE),
                ) {
                    Text(stringResource(R.string.atproto_settings_revoke_button))
                }
            }
            DisabledControlReasonText(revokeGate.reason)
        }
    }
}
