package com.fauna.app.ui.screen.bridges

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.FeedTriple
import com.fauna.app.core.SourceAskRows
import com.fauna.app.core.UrlOpener
import com.fauna.app.core.sourceAskRows
import com.fauna.ffi.FfiFeedRequestState
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.BridgesVM
import com.fauna.app.ui.viewmodel.SettingsAdminGateVM
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiInstalledPlugin
import com.fauna.ffi.FfiPluginCard
import com.fauna.app.ui.util.AutomationAttributePack
import com.fauna.ffi.FfiBridgeFollow
import com.fauna.ffi.FfiFollowRequestRow
import com.fauna.ffi.FfiBridgeSetting
import com.fauna.ffi.FfiBridgeStatus
import com.fauna.ffi.FfiCborValue
import com.fauna.ffi.FfiLinkBlock
import com.fauna.ffi.FfiSettingControl
import com.fauna.ffi.bridgeLinkBlock
import com.fauna.ffi.bridgeModeApplies
import com.fauna.ffi.followDisplay
import com.fauna.ffi.isUnifiedBridgesPageBridge
import com.fauna.ffi.settingControl
import social.fauna.generated.Ids

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun BridgesScreen(
    navController: NavController? = null,
    vm: BridgesVM = hiltViewModel(),
    adminGate: SettingsAdminGateVM = hiltViewModel(),
) {
    val bridges by vm.bridges.collectAsState()
    val isAdmin by adminGate.isAdmin.collectAsState()
    val plugins by vm.plugins.collectAsState()
    val pluginInstallOpened by vm.pluginInstallOpened.collectAsState()
    val pluginCards by vm.pluginCards.collectAsState()
    val pluginBindOpened by vm.pluginBindOpened.collectAsState()
    val follows by vm.follows.collectAsState()
    val followRequests by vm.followRequests.collectAsState()
    val isLoading by vm.isLoading.collectAsState()
    val error by vm.error.collectAsState()
    val feedAsks by vm.feedAsks.collectAsState()
    val refusedFeed by vm.refusedFeed.collectAsState()
    val context = LocalContext.current
    val appMessages = LocalAppMessages.current

    LaunchedEffect(Unit) {
        vm.refresh()
        // The settings cards are every account's (`fauna.plugins.available`).
        vm.refreshPluginCards()
    }

    // The installed plugins are the admin's alone (`fauna.plugins.list`).
    LaunchedEffect(isAdmin) { if (isAdmin) vm.refreshPlugins() }

    LaunchedEffect(error) {
        appMessages.showError(error)
    }

    if (isLoading && bridges.isEmpty()) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }
        return
    }

    // Nostr and Bluesky are NOT unified-Bridges-page bridges — each has its own
    // dedicated page (NostrScreen/NostrVM; the `atproto` page,
    // docs/goal/ui/atproto.md § Migration step 2). The predicate is applied
    // HERE, at the page's render, not inside BridgesVM.refresh() — filtering at
    // fetch time starves the shared `bridges` snapshot the AT Protocol page's own
    // BridgesVM instance reads to find its own row (atproto.md § linux leg
    // gotcha: it also silently killed the Bluesky notification-poll trigger,
    // which reads the same reply).
    val visibleBridges = remember(bridges) { bridges.filter { isUnifiedBridgesPageBridge(it.id) } }

    LazyColumn(
        modifier = Modifier.fillMaxSize().padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp)
    ) {
        items(visibleBridges, key = { it.id }) { bridge ->
            BridgeCard(
                bridge = bridge,
                bridgeFollows = follows[bridge.id] ?: emptyList(),
                followRequests = followRequests[bridge.id] ?: emptyList(),
                onLink = { mode, fields ->
                    vm.linkBridge(bridge.id, mode, fields) { url ->
                        UrlOpener.open(context, url)
                    }
                },
                onUnlink = { vm.unlinkBridge(bridge.id) },
                onUpdateSetting = { key, value -> vm.updateSetting(bridge.id, key, value) },
                onAddFollow = { id, petname -> vm.addFollow(bridge.id, id, petname) },
                onRemoveFollow = { followId -> vm.removeFollow(bridge.id, followId) },
                onResolveFollowRequest = { id, approve -> vm.resolveFollowRequest(bridge.id, id, approve) },
                sourceAsks = remember(feedAsks, refusedFeed, bridge.id) {
                    sourceAskRows(feedAsks, refusedFeed, bridge.id)
                },
                onAskSource = vm::requestFeedSource,
            )
        }
        // Every installed plugin's settings card, after the bridges, for every account.
        pluginCards?.let { cards ->
            items(cards, key = { it.principalIdHex }) { card ->
                PluginCard(
                    card = card,
                    onPress = { vm.pressPluginCard(card) },
                    onUpdateSetting = { key, value -> vm.updatePluginSetting(card.principalIdHex, key, value) },
                )
            }
            if (pluginBindOpened) {
                item(key = "plugin-bind-opened") {
                    Text(
                        stringResource(R.string.bridges_plugin_bind_opened),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
        if (isAdmin) {
            item(key = "plugins") {
                PluginsSection(
                    plugins = plugins,
                    installOpened = pluginInstallOpened,
                    onInstall = { url -> vm.installPlugin(url) {} },
                    onUninstall = vm::uninstallPlugin,
                )
            }
        }
    }
}

/**
 * One account's settings card for an installed plugin, as one more
 * `bridge-card` (`third-party.md` § The principal model → *Hosted principals*):
 * the shared `plugin_card_text`'s name, badge and publisher as text, its verb as
 * the card's `bridge-action-button` — Connect while unbound, Disconnect while
 * bound, carrying the plugin's `client_id` and a `bound` value so a reader can
 * tell which card is which plugin's — and, once bound, its settings through the
 * one settings renderer ([SettingRow]). Reference painter:
 * `apps/fauna-tui/src/bridges.rs::plugin_card_elements`.
 */
@Composable
internal fun PluginCard(
    card: FfiPluginCard,
    onPress: () -> Unit,
    onUpdateSetting: (String, FfiCborValue) -> Unit,
) {
    val context = LocalContext.current
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.BRIDGE_CARD)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(card.name, style = MaterialTheme.typography.titleMedium)
            Text(
                "${resolveLocalized(context, card.badge).orEmpty()}\n${resolveLocalized(context, card.publisher).orEmpty()}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            // The settings are the bound account's own values; unbound there are none.
            val settingsGate = faunaGate("fauna.plugins.set_settings")
            card.settings.forEach { SettingRow(it, onUpdateSetting, enabled = settingsGate.enabled) }
            if (card.settings.isNotEmpty()) DisabledControlReasonText(settingsGate.reason)
            val gate = faunaGate(if (card.bound) "fauna.principals.revoke" else "fauna.plugins.bind")
            val label = stringResource(if (card.bound) R.string.bridges_plugin_disconnect else R.string.bridges_plugin_connect)
            val buttonModifier = Modifier
                .testTag(Ids.BRIDGE_ACTION_BUTTON)
                .semantics {
                    stateDescription = AutomationAttributePack.pack(
                        "client_id" to card.clientId,
                        "bound" to if (card.bound) "true" else "false",
                    )
                }
            if (card.bound) {
                OutlinedButton(
                    onClick = onPress,
                    enabled = gate.enabled,
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                    modifier = buttonModifier,
                ) { Text(label) }
            } else {
                Button(onClick = onPress, enabled = gate.enabled, modifier = buttonModifier) { Text(label) }
            }
            DisabledControlReasonText(gate.reason)
        }
    }
}

/**
 * The admin's hosted plugins (`bridges.md` § Hosted plugins on this page): one
 * `bridge-plugin-item` per installed plugin — its name, publisher and runner
 * status, each the shared row's text (`installed_plugin_text`) — Uninstall behind
 * an inline confirm, then the install entry, the document URL an admin gives
 * (`third-party.md` § The runner contract → *The install-approval leg*).
 * Reference painter: `apps/fauna-tui/src/bridges.rs::plugin_elements`.
 */
@Composable
internal fun PluginsSection(
    plugins: List<FfiInstalledPlugin>?,
    installOpened: Boolean,
    onInstall: (String) -> Unit,
    onUninstall: (String) -> Unit,
) {
    val context = LocalContext.current
    // The install row whose inline uninstall confirm is open.
    var armed by remember { mutableStateOf<String?>(null) }
    var url by remember { mutableStateOf("") }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(R.string.bridges_plugins_heading), style = MaterialTheme.typography.titleMedium)
        if (plugins != null) {
            if (plugins.isEmpty()) {
                Text(
                    stringResource(R.string.bridges_plugins_empty),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            plugins.forEach { row ->
                val isArmed = armed == row.principalIdHex
                Card(modifier = Modifier.fillMaxWidth().testTag(Ids.BRIDGE_PLUGIN_ITEM)) {
                    Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text(
                            row.name,
                            style = MaterialTheme.typography.titleSmall,
                            modifier = Modifier
                                .testTag(Ids.BRIDGE_PLUGIN_NAME)
                                .semantics { stateDescription = row.clientId },
                        )
                        Text(
                            resolveLocalized(context, row.publisher).orEmpty(),
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.testTag(Ids.BRIDGE_PLUGIN_PUBLISHER),
                        )
                        Text(
                            row.status.joinToString("\n") { resolveLocalized(context, it).orEmpty() },
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier
                                .testTag(Ids.BRIDGE_PLUGIN_STATUS)
                                .semantics { stateDescription = row.state },
                        )
                        if (isArmed) {
                            Text(
                                stringResourceFmt(R.string.bridges_plugin_uninstall_prompt, row.name),
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                        val uninstallGate = faunaGate("fauna.plugins.uninstall")
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            OutlinedButton(
                                onClick = {
                                    if (isArmed) {
                                        armed = null
                                        onUninstall(row.principalIdHex)
                                    } else {
                                        armed = row.principalIdHex
                                    }
                                },
                                enabled = uninstallGate.enabled,
                                colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                                modifier = Modifier.testTag(Ids.BRIDGE_PLUGIN_UNINSTALL),
                            ) {
                                Text(
                                    stringResource(
                                        if (isArmed) R.string.bridges_plugin_uninstall_confirm
                                        else R.string.bridges_plugin_uninstall,
                                    ),
                                )
                            }
                            if (isArmed) {
                                TextButton(onClick = { armed = null }) {
                                    Text(stringResource(R.string.bridges_plugin_uninstall_cancel))
                                }
                            }
                        }
                        DisabledControlReasonText(uninstallGate.reason)
                    }
                }
            }
        }
        OutlinedTextField(
            value = url,
            onValueChange = { url = it },
            label = { Text(stringResource(R.string.bridges_plugin_install_label)) },
            singleLine = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Done),
            modifier = Modifier.fillMaxWidth().testTag(Ids.BRIDGE_PLUGIN_INSTALL_URL),
        )
        val installGate = faunaGate("fauna.plugins.install", enabled = url.isNotBlank())
        Button(
            onClick = {
                onInstall(url)
                url = ""
            },
            enabled = installGate.enabled,
            modifier = Modifier.testTag(Ids.BRIDGE_PLUGIN_INSTALL_SUBMIT),
        ) {
            Text(stringResource(R.string.bridges_plugin_install_submit))
        }
        DisabledControlReasonText(installGate.reason)
        if (installOpened) {
            Text(
                stringResource(R.string.bridges_plugin_install_opened),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/**
 * One bridge's full detail surface (identity, settings, follows, link/unlink
 * action). `internal` (not `private`) so the `atproto` page's Linked-account
 * panel can embed it verbatim for the "bluesky" row — the same reuse linux's
 * `build_bridge_detail_content` / apple's `BridgeCardContent` / web's
 * `BridgeCard.svelte` provide (docs/goal/ui/atproto.md § Layout & flow,
 * § Migration step 2 — zero new element IDs).
 */
@Composable
internal fun BridgeCard(
    bridge: FfiBridgeStatus,
    bridgeFollows: List<FfiBridgeFollow>,
    onLink: (mode: String, fields: Map<String, String>) -> Unit,
    onUnlink: () -> Unit,
    onUpdateSetting: (String, FfiCborValue) -> Unit,
    onAddFollow: (String, String?) -> Unit,
    onRemoveFollow: (String) -> Unit,
    // The waiting follow requests (bridges.md § Follow requests) — shown only
    // for a bridge that declares `supportsFollowRequests`; the embeds whose
    // provider never does (Bluesky) take the empty defaults.
    followRequests: List<FfiFollowRequestRow> = emptyList(),
    onResolveFollowRequest: (String, Boolean) -> Unit = { _, _ -> },
    // The ward's feed-source ask rows for THIS card (family-safety.md
    // § Feed-source approvals) — `sourceAskRows(...)` computed by the stateful
    // caller; empty (the default, and the common case) paints nothing.
    sourceAsks: SourceAskRows = SourceAskRows(),
    onAskSource: (FeedTriple) -> Unit = {},
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.BRIDGE_CARD)) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(bridge.name, style = MaterialTheme.typography.titleMedium)

            if (!bridge.available) {
                Spacer(Modifier.height(8.dp))
                Text(stringResource(R.string.bridges_not_available_node), color = MaterialTheme.colorScheme.outline)
                return@Column
            }

            Spacer(Modifier.height(8.dp))

            if (bridge.linked) {
                LinkedSection(
                    bridge, bridgeFollows, followRequests, onUnlink, onUpdateSetting,
                    onAddFollow, onRemoveFollow, onResolveFollowRequest,
                )
            } else {
                UnlinkedSection(bridge, onLink)
            }

            SourceAskSection(sourceAsks, onAskSource)
        }
    }
}

/**
 * The ward's feed-source ask rows inside one `bridge-card` (tui's
 * `bridges::source_ask_rows`; linux's `fill_source_asks`): first one
 * `bridge-source-request-state` per durable ask — pending, or APPROVED → the
 * "try again" PROMPT, a label and never a button, since the grant is
 * single-use and the ward redeems it by repeating the original Link / Add
 * Follow, which stays on the card (rule (e)) — then one
 * `bridge-source-request-button` per refused triple with no durable ask yet,
 * each carrying ITS OWN triple. Paints nothing when there is nothing to show.
 * No offline-gate declaration: `fauna.family.feed_source.request` is
 * `OfflineQueued`, which the gate never greys.
 */
@Composable
internal fun SourceAskSection(rows: SourceAskRows, onAsk: (FeedTriple) -> Unit) {
    if (rows.isEmpty) return
    Column(modifier = Modifier.padding(top = 8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        rows.states.forEach { state ->
            Text(
                stringResource(
                    when (state) {
                        FfiFeedRequestState.APPROVED -> R.string.bridges_source_request_approved
                        FfiFeedRequestState.PENDING -> R.string.bridges_source_request_pending
                    },
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.BRIDGE_SOURCE_REQUEST_STATE),
            )
        }
        rows.asks.forEach { triple ->
            // A landed ask's re-read swaps this button for its state; a
            // re-press before then is a quiet nest-side no-op, and a failed
            // ask leaves it live for a retry.
            OutlinedButton(
                onClick = { onAsk(triple) },
                modifier = Modifier.testTag(Ids.BRIDGE_SOURCE_REQUEST_BUTTON),
            ) { Text(stringResource(R.string.bridges_source_request_button)) }
        }
    }
}

@Composable
private fun LinkedSection(
    bridge: FfiBridgeStatus,
    bridgeFollows: List<FfiBridgeFollow>,
    followRequests: List<FfiFollowRequestRow>,
    onUnlink: () -> Unit,
    onUpdateSetting: (String, FfiCborValue) -> Unit,
    onAddFollow: (String, String?) -> Unit,
    onRemoveFollow: (String) -> Unit,
    onResolveFollowRequest: (String, Boolean) -> Unit,
) {
    // Identity
    bridge.identity?.let { id ->
        Text(
            "${id.label}: ${id.display}",
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.testTag(Ids.BRIDGE_IDENTITY),
        )
        Spacer(Modifier.height(8.dp))
    }

    // Settings
    bridge.settings.forEach { setting ->
        SettingRow(setting, onUpdateSetting)
    }

    // Follows
    if (bridge.supportsFollows) {
        Spacer(Modifier.height(8.dp))
        Column(modifier = Modifier.testTag(Ids.BRIDGE_FOLLOWS_LIST)) {
            Text(stringResource(R.string.bridges_follows), style = MaterialTheme.typography.titleSmall)
            Spacer(Modifier.height(4.dp))
            bridgeFollows.forEach { follow ->
                Row(
                    modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    // The item id sits on the display text itself, so a row's
                    // read-back is the shared display line (the nickname, else
                    // the id), not the row's joined children.
                    Text(
                        followDisplay(follow.id, follow.petname),
                        modifier = Modifier.weight(1f).testTag(Ids.BRIDGE_FOLLOW_ITEM),
                        style = MaterialTheme.typography.bodyMedium
                    )
                    IconButton(onClick = { onRemoveFollow(follow.id) },
                        modifier = Modifier.testTag(Ids.BRIDGE_FOLLOW_REMOVE)) {
                        Icon(Icons.Default.Delete, stringResource(R.string.bridges_remove_follow), tint = MaterialTheme.colorScheme.error)
                    }
                }
            }
            AddFollowRow(onAddFollow)
        }
    }

    // Follow requests: the heading keeps its count at zero; each row paints the
    // shared `display` line and forwards two clicks.
    if (bridge.supportsFollowRequests) {
        Spacer(Modifier.height(8.dp))
        Column {
            Text(
                stringResource(R.string.bridges_follow_requests)
                    .replace("{count}", followRequests.size.toString()),
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.BRIDGE_FOLLOW_REQUESTS_LIST),
            )
            Spacer(Modifier.height(4.dp))
            followRequests.forEach { request ->
                Row(
                    modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp)
                        .testTag(Ids.BRIDGE_FOLLOW_REQUEST_ITEM),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Text(
                        request.display,
                        modifier = Modifier.weight(1f),
                        style = MaterialTheme.typography.bodyMedium
                    )
                    TextButton(onClick = { onResolveFollowRequest(request.id, true) },
                        modifier = Modifier.testTag(Ids.BRIDGE_FOLLOW_REQUEST_APPROVE)) {
                        Text(stringResource(R.string.bridges_approve_follow_request))
                    }
                    TextButton(onClick = { onResolveFollowRequest(request.id, false) },
                        modifier = Modifier.testTag(Ids.BRIDGE_FOLLOW_REQUEST_REFUSE)) {
                        Text(stringResource(R.string.bridges_refuse_follow_request))
                    }
                }
            }
        }
    }

    // Feed-subscription elements (bridge-feed-subscriptions / bridge-feed-item /
    // bridge-subscribe-feed-button) were RETIRED 2026-06-28 per the Feed-only
    // decision (bridges.md § Layout & flow): bridge feed subscription lives on the
    // Feed page's bridge-form-*, not on the bridge card.

    // The follows list and the per-setting editors above stay live on purpose:
    // `add_follow`/`remove_follow` are **OfflineQueued** and
    // `set_settings` is **OfflineSafe**, and ruling 1 desensitizes class 3 only.
    // Per android's never-gating rule they carry no `faunaGate` call either —
    // the checker would refuse one. Only the unlink commits.
    val unlinkGate = faunaGate("fauna.bridges.unlink")
    Spacer(Modifier.height(12.dp))
    OutlinedButton(onClick = onUnlink,
        enabled = unlinkGate.enabled,
        modifier = Modifier.testTag(Ids.BRIDGE_ACTION_BUTTON)) {
        Text(stringResource(R.string.bridges_unlink))
    }
    DisabledControlReasonText(unlinkGate.reason)
}

@Composable
private fun SettingRow(
    setting: FfiBridgeSetting,
    onUpdate: (String, FfiCborValue) -> Unit,
    // A plugin card's rows are `OnlineOnly` (`fauna.plugins.set_settings`): the
    // card desensitizes them offline. A bridge's stay live (`OfflineSafe`).
    enabled: Boolean = true,
) {
    // A row is `bridge-setting[<key>]` — keyed by the setting, whatever control
    // its type paints (the `settings-nav-row[<key>]` idiom; ui.yaml
    // `elements.bridge-setting`).
    val rowTag = "${Ids.BRIDGE_SETTING}[${setting.key}]"
    // Which control paints is the shared classifier's call
    // (`fauna_client_bridges::setting_control`) — no spelling is matched here.
    when (settingControl(setting.settingType)) {
        FfiSettingControl.TOGGLE -> {
            Row(
                modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween
            ) {
                Text(setting.label)
                Switch(
                    checked = setting.value.boolOrFalse(),
                    onCheckedChange = { onUpdate(setting.key, FfiCborValue.Bool(it)) },
                    enabled = enabled,
                    modifier = Modifier.testTag(rowTag),
                )
            }
        }
        FfiSettingControl.SELECT -> {
            val options = setting.options ?: return
            var expanded by remember { mutableStateOf(false) }
            val currentLabel = options.find {
                it.value == setting.value
            }?.label ?: setting.value.displayText()

            Row(
                modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween
            ) {
                Text(setting.label)
                Box {
                    TextButton(
                        onClick = { expanded = true },
                        enabled = enabled,
                        modifier = Modifier.testTag(rowTag),
                    ) {
                        Text(currentLabel)
                    }
                    DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                        options.forEach { opt ->
                            DropdownMenuItem(
                                text = { Text(opt.label) },
                                onClick = {
                                    onUpdate(setting.key, opt.value)
                                    expanded = false
                                }
                            )
                        }
                    }
                }
            }
        }
        FfiSettingControl.NUMBER -> {
            var numberText by remember(setting.value) {
                mutableStateOf(setting.value.displayText())
            }
            fun commit() {
                if (numberText == setting.value.displayText()) return
                com.fauna.ffi.parseCountI64(numberText)?.let { onUpdate(setting.key, FfiCborValue.Integer(it)) }
            }
            val commitNow by rememberUpdatedState(::commit)
            OutlinedTextField(
                value = numberText,
                onValueChange = { numberText = it },
                label = { Text(setting.label) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { commit() }),
                enabled = enabled,
                modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)
                    .testTag(rowTag).commitOnTap { commitNow() },
            )
        }
        FfiSettingControl.UNKNOWN -> {
            // Forward-compat fallback: a setting_type this build predates (a
            // newer nest) — a read-only line, never a dropped setting.
            Text(
                "${setting.label}: ${setting.value.displayText()}",
                modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp).testTag(rowTag),
            )
        }
        FfiSettingControl.TEXT -> {
            var textValue by remember(setting.value) {
                mutableStateOf(setting.value.displayText())
            }
            val commitNow by rememberUpdatedState({
                if (textValue != setting.value.displayText()) {
                    onUpdate(setting.key, FfiCborValue.Text(textValue))
                }
            })
            Row(
                modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                OutlinedTextField(
                    value = textValue,
                    onValueChange = { textValue = it },
                    label = { Text(setting.label) },
                    modifier = Modifier.weight(1f).testTag(rowTag)
                        .commitOnTap { commitNow() },
                    singleLine = true,
                    enabled = enabled,
                )
                TextButton(onClick = { onUpdate(setting.key, FfiCborValue.Text(textValue)) }, enabled = enabled) {
                    Text(stringResource(R.string.common_save))
                }
            }
        }
    }
}

/**
 * A tap on an editable commits it — the automation vocabulary's meaning of a
 * click on a field (tui's `Role::InputCommit`, linux's `Entry::activate`), and
 * harmless to a person: [commit] is the field's own Done / Save action, so a tap
 * that changes nothing sends nothing. Observed on the initial pass and never
 * consumed, so the field's own focus and caret handling is untouched.
 */
private fun Modifier.commitOnTap(commit: () -> Unit): Modifier =
    pointerInput(Unit) {
        awaitPointerEventScope {
            while (true) {
                val event = awaitPointerEvent(PointerEventPass.Initial)
                if (event.type == PointerEventType.Release) commit()
            }
        }
    }

@Composable
private fun AddFollowRow(onAddFollow: (String, String?) -> Unit) {
    var followId by remember { mutableStateOf("") }
    var petname by remember { mutableStateOf("") }

    Row(
        modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp)
    ) {
        OutlinedTextField(
            value = followId,
            onValueChange = { followId = it },
            label = { Text(stringResource(R.string.bridges_id_to_follow)) },
            modifier = Modifier.weight(1f).testTag(Ids.BRIDGE_FOLLOW_ID_INPUT),
            singleLine = true
        )
        OutlinedTextField(
            value = petname,
            onValueChange = { petname = it },
            label = { Text(stringResource(R.string.bridges_petname_optional)) },
            modifier = Modifier.weight(1f).testTag(Ids.BRIDGE_FOLLOW_PETNAME_INPUT),
            singleLine = true
        )
        IconButton(
            onClick = {
                if (followId.isNotBlank()) {
                    onAddFollow(followId, petname.ifBlank { null })
                    followId = ""
                    petname = ""
                }
            },
            modifier = Modifier.testTag(Ids.BRIDGE_ADD_FOLLOW_BUTTON)
        ) {
            Icon(Icons.Default.Add, stringResource(R.string.bridges_add_follow))
        }
    }
}

@Composable
private fun UnlinkedSection(
    bridge: FfiBridgeStatus,
    onLink: (mode: String, fields: Map<String, String>) -> Unit
) {
    // The platform string-match is the SHARED rule (lifted 2026-08-15 from the
    // seven per-app copies — `fauna_client_bridges::mode_applies`); android
    // supplies only its canonical name.
    val modes = bridge.linkModes?.filter { bridgeModeApplies(it.platform, "android") }
        ?: emptyList()

    // Why linking is unavailable (null = it IS), from the shared Rust rule so
    // android cannot drift from the other six apps. Previously a null
    // `linkModes` — the nest's documented degraded shape when `provider.status()`
    // errors — hit an early `?: return` that rendered NOTHING: no button, no
    // message, and never the nest's own explanation. bridges.md § Errors & edge
    // cases: disabled, not absent.
    val block = bridgeLinkBlock(bridge.linked, bridge.error, modes.size.toUInt())
    if (block != null) {
        Text(
            when (block) {
                is FfiLinkBlock.ProviderError -> block.message
                is FfiLinkBlock.NoApplicableMode -> stringResource(R.string.bridges_no_link_method)
            },
            color = MaterialTheme.colorScheme.outline,
            modifier = Modifier.testTag(Ids.BRIDGE_LINK_BLOCKED_REASON)
        )
        Spacer(Modifier.height(8.dp))
        Button(
            onClick = {},
            enabled = false,
            modifier = Modifier.testTag(Ids.BRIDGE_ACTION_BUTTON)
        ) {
            Text(stringResourceFmt(R.string.bridges_link, bridge.name))
        }
        return
    }

    var selectedMode by remember { mutableStateOf(modes.first()) }
    val fieldValues = remember(selectedMode.mode) {
        mutableStateMapOf<String, String>()
    }

    if (modes.size > 1) {
        modes.forEach { mode ->
            Row(
                modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp),
                verticalAlignment = Alignment.CenterVertically
            ) {
                RadioButton(
                    selected = selectedMode.mode == mode.mode,
                    onClick = { selectedMode = mode }
                )
                Text(mode.label, modifier = Modifier.padding(start = 4.dp))
            }
        }
        Spacer(Modifier.height(8.dp))
    }

    // Metadata-driven link form: one bridge-link-field-{key} input per declared
    // field (bridges.md § Element IDs — the testid is the raw provider
    // BridgeLinkField.key, mirroring linux's `bridge-link-field-{}`). On this
    // feed-side page that is bridge-link-field-handle for Bluesky's oauth mode.
    selectedMode.fields.forEach { field ->
        OutlinedTextField(
            value = fieldValues[field.key] ?: "",
            onValueChange = { fieldValues[field.key] = it },
            label = { Text(field.label) },
            placeholder = field.placeholder?.let { ph -> { Text(ph) } },
            modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)
                .testTag("bridge-link-field-${field.key}"),
            singleLine = true
        )
    }

    // The mode picker and every credential field above are buffers; only this
    // submits. (The blocked-reason variant of this same id, further up, is
    // already `enabled = false` on the page's own predicate — that one needs no
    // gate, and it stays dead offline or not.)
    val linkGate = faunaGate("fauna.bridges.link")
    Spacer(Modifier.height(8.dp))
    Button(onClick = { onLink(selectedMode.mode, fieldValues.toMap()) },
        enabled = linkGate.enabled,
        modifier = Modifier.testTag(Ids.BRIDGE_ACTION_BUTTON)) {
        Text(stringResourceFmt(R.string.bridges_link, bridge.name))
    }
    DisabledControlReasonText(linkGate.reason)
}

// ── FfiCborValue display helpers ────────────────────────────────────────
// Per-bridge setting values cross the WS-RPC seam as FfiCborValue (CBOR stays
// in Rust). These render/extract the scalar shapes the Bridges UI uses; the
// providers only emit bool / text / integer setting values.

private fun FfiCborValue.boolOrFalse(): Boolean =
    (this as? FfiCborValue.Bool)?.v ?: false

private fun FfiCborValue.displayText(): String = when (this) {
    is FfiCborValue.Text -> v
    is FfiCborValue.Integer -> v.toString()
    is FfiCborValue.Bool -> v.toString()
    is FfiCborValue.Float -> v.toString()
    else -> ""
}
