package com.fauna.app.ui.screen.nostr

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.payments.ZapSignerItem
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.components.QrCanvas
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.localizedNested
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.CUSTODIAL_MODES
import com.fauna.app.ui.viewmodel.NostrVM
import com.fauna.app.ui.viewmodel.boolSetting
import com.fauna.app.ui.viewmodel.parseRelayList
import com.fauna.ffi.FfiBridgeFollow
import com.fauna.ffi.FfiBridgeStatus
import com.fauna.ffi.FfiBridgeToggleOption
import com.fauna.ffi.FfiCborValue
import com.fauna.ffi.FfiCreateBunkerInviteReply
import com.fauna.ffi.FfiException
import com.fauna.ffi.FfiFeatureRow
import com.fauna.ffi.nostrContentToggleOptions
import com.fauna.ffi.nostrKeySourceLabel
import com.fauna.ffi.nostrLinkModeLabel
import com.fauna.ffi.qrMatrix
import com.fauna.ffi.shortId
import social.fauna.generated.Ids

/**
 * The standalone **Nostr** page (`docs/goal/ui/nostr.md`; page structure
 * ratified 2026-06-13 — Nostr keeps its own dedicated page, the same
 * treatment as mail, NOT folded into the generic Bridges list). Reached via
 * the top-level `nostr-tab` drawer entry (`FaunaNavHost`), mirroring linux
 * (`apps/fauna-linux/src/settings/nostr_tab.rs`) and the shared FaunaKit
 * `NostrSettingsView` (macOS/iOS) — both are the reference render. All Nostr
 * logic is shared Rust reached over the unified `fauna.bridges.*` control
 * plane (`bridge_id:"nostr"`), the same seam [com.fauna.app.ui.viewmodel.BridgesVM]
 * drives generically for every other bridge; this page hardcodes the
 * Nostr-specific fields (link modes, the 5 content flags, the JSON
 * `relay_list` setting) instead of the generic per-key settings renderer,
 * mirroring apple/linux (priority #1/#3).
 *
 * DMs are not on this page: a Nostr DM is a bridged room on the unified
 * Conversations surface (nostr.md § Implementation status today → DMs).
 *
 * Stateless [NostrContent] is split out so it renders under the Compose test
 * harness with seeded [FfiBridgeStatus]/[FfiBridgeFollow] data — no FFI, no
 * Hilt (priority #2; the `FoldersContent`/`FamilyContent` idiom).
 */
@Composable
fun NostrScreen(vm: NostrVM = hiltViewModel()) {
    val registered by vm.registered.collectAsState()
    val bridge by vm.bridge.collectAsState()
    val follows by vm.follows.collectAsState()
    val bunkerInvite by vm.bunkerInvite.collectAsState()
    val zapSigners by vm.zapSigners.collectAsState()
    val zapSignerGateRow by vm.zapSignerGateRow.collectAsState()
    val npubConfirmationOwed by vm.npubConfirmationOwed.collectAsState()
    val isLoading by vm.isLoading.collectAsState()
    val error by vm.error.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(Unit) { vm.refresh() }
    LaunchedEffect(error) { appMessages.showError(error) }

    if (isLoading && bridge == null && registered) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }
        return
    }

    NostrContent(
        registered = registered,
        bridge = bridge,
        follows = follows,
        bunkerInvite = bunkerInvite,
        zapSigners = zapSigners,
        zapSignerGateRow = zapSignerGateRow,
        npubConfirmationOwed = npubConfirmationOwed,
        onLink = vm::link,
        onUnlink = vm::unlink,
        onUpdateSetting = vm::updateSetting,
        onAddRelay = vm::addRelay,
        onRemoveRelay = vm::removeRelay,
        onAddFollow = vm::addFollow,
        onRemoveFollow = vm::removeFollow,
        onConnectApp = vm::connectApp,
        onAddZapSigner = vm::addZapSigner,
        onRemoveZapSigner = vm::removeZapSigner,
        onConfirmNpub = vm::confirmNpub,
    )
}

@Composable
fun NostrContent(
    registered: Boolean,
    bridge: FfiBridgeStatus?,
    follows: List<FfiBridgeFollow>,
    bunkerInvite: FfiCreateBunkerInviteReply? = null,
    zapSigners: List<ZapSignerItem> = emptyList(),
    zapSignerGateRow: FfiFeatureRow? = null,
    npubConfirmationOwed: Boolean = false,
    onLink: (mode: String, fields: Map<String, String>) -> Unit,
    onUnlink: () -> Unit,
    onUpdateSetting: (key: String, value: FfiCborValue) -> Unit,
    onAddRelay: (String) -> Unit,
    onRemoveRelay: (String) -> Unit,
    onAddFollow: (pubkey: String, petname: String?) -> Unit,
    onRemoveFollow: (String) -> Unit,
    onConnectApp: () -> Unit = {},
    onAddZapSigner: (pubkey: String, label: String) -> Unit = { _, _ -> },
    onRemoveZapSigner: (String) -> Unit = {},
    onConfirmNpub: () -> Unit = {},
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
    ) {
        Text(
            stringResource(R.string.nostr_title),
            style = MaterialTheme.typography.headlineSmall,
            modifier = Modifier.testTag(Ids.PAGE_HEADING),
        )
        Spacer(Modifier.height(16.dp))

        when {
            !registered -> {
                Text(
                    stringResource(R.string.nostr_unavailable),
                    color = MaterialTheme.colorScheme.outline,
                )
            }
            bridge == null -> {
                // Still loading (registered defaults true until the first
                // refresh resolves) — nothing to render yet.
            }
            bridge.linked -> {
                LinkedAccountSection(bridge, onUnlink)
                // Succession-aftermath npub confirm banner (leg 3 —
                // nostr.md § Key succession and rotation; tui/linux
                // reference). Dismissible, never a blocking modal — gated
                // purely on the predicate, never a one-shot local flag.
                if (npubConfirmationOwed) {
                    Spacer(Modifier.height(16.dp))
                    NpubConfirmBanner(
                        npub = bridge.identity?.value.orEmpty(),
                        onYes = onConfirmNpub,
                        // "No / nothing is linked" reuses the EXISTING
                        // unlink gesture (nostr.md:75: the remedy is "the
                        // existing page machinery") — never a bespoke flow.
                        onNo = onUnlink,
                    )
                }
                Spacer(Modifier.height(16.dp))
                ContentSettingsSection(bridge, onUpdateSetting)
                Spacer(Modifier.height(16.dp))
                RelaysSection(bridge, onAddRelay, onRemoveRelay)
                Spacer(Modifier.height(16.dp))
                FollowsSection(follows, onAddFollow, onRemoveFollow)
                if (bridge.mode in CUSTODIAL_MODES) {
                    Spacer(Modifier.height(16.dp))
                    ConnectedAppsSection(bunkerInvite, onConnectApp)
                }
                // Zap signers — gated on LINKED alone, unlike Connected apps
                // above: designating who may speak for your money is
                // orthogonal to where your key lives. `zaps` is a subset
                // member of `payments` (dynamic-features.md § Charter
                // members) and rides the SAME android family compile
                // condition (§ Platform-family surface excision): the glue
                // layer's src/payments-vs-src/noPayments split keeps this
                // section's inputs typechecking in a storeSafe build, and
                // this constant is what keeps the section itself — and its
                // nostr-zap-signer-* ids — out of that artifact (the release shrinker folds
                // the branch).
                if (BuildConfig.PAYMENTS) {
                    Spacer(Modifier.height(16.dp))
                    ZapSignersSection(zapSigners, zapSignerGateRow, onAddZapSigner, onRemoveZapSigner)
                }
            }
            else -> {
                UnlinkedAccountSection(onLink)
            }
        }
    }
}

@Composable
private fun LinkedAccountSection(bridge: FfiBridgeStatus, onUnlink: () -> Unit) {
    val clipboard = LocalClipboardManager.current
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(stringResource(R.string.nostr_account_title), style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(8.dp))

            bridge.identity?.let { identity ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(stringResource(R.string.nostr_account_public_key), style = MaterialTheme.typography.labelMedium)
                        Text(identity.value, style = MaterialTheme.typography.bodySmall, maxLines = 1)
                    }
                    IconButton(
                        onClick = { clipboard.setText(AnnotatedString(identity.value)) },
                        modifier = Modifier.testTag(Ids.NOSTR_PUBKEY_COPY_BTN),
                    ) {
                        Icon(Icons.Default.ContentCopy, stringResource(R.string.common_copy))
                    }
                }
                Spacer(Modifier.height(8.dp))
            }

            bridge.mode?.let { mode ->
                Text(stringResource(R.string.nostr_account_signing_mode), style = MaterialTheme.typography.labelMedium)
                Text(localized(nostrKeySourceLabel(mode)) ?: mode, style = MaterialTheme.typography.bodyMedium)
                Spacer(Modifier.height(8.dp))
            }

            // Unlinking is a nest call (`fauna.bridges.unlink`, OnlineOnly) —
            // the SAME kind BridgesScreen's per-bridge unlink already declares.
            // Found by hand, not by the rule-4 probe: that differential is
            // per-KIND, so a SECOND control issuing an already-declared kind is
            // invisible to it. The identity copy above stays live (clipboard).
            val unlinkGate = faunaGate("fauna.bridges.unlink")
            OutlinedButton(
                onClick = onUnlink,
                enabled = unlinkGate.enabled,
                modifier = Modifier.testTag(Ids.NOSTR_UNLINK_BUTTON),
            ) {
                Text(stringResource(R.string.nostr_account_unlink))
            }
            DisabledControlReasonText(unlinkGate.reason)
        }
    }
}

/**
 * Succession-aftermath npub confirm banner — shown only while
 * [NostrVM.npubConfirmationOwed] says a confirmation is owed. `npub` is
 * whatever [FfiBridgeStatus.identity] resolved, mirroring linux's
 * `nostr_tab.rs` banner text; the two buttons wire straight to the shared
 * `fauna_client_config` calls via [com.fauna.app.core.ApiClient] — zero logic
 * owed here (priority #2).
 */
@Composable
private fun NpubConfirmBanner(npub: String, onYes: () -> Unit, onNo: () -> Unit) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                stringResourceFmt(R.string.nostr_npub_confirm_banner, npub),
                modifier = Modifier.testTag(Ids.NOSTR_NPUB_CONFIRM_BANNER),
            )
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = onYes, modifier = Modifier.testTag(Ids.NOSTR_NPUB_CONFIRM_YES_BUTTON)) {
                    Text(stringResource(R.string.nostr_npub_confirm_yes_button))
                }
                OutlinedButton(onClick = onNo, modifier = Modifier.testTag(Ids.NOSTR_NPUB_CONFIRM_NO_BUTTON)) {
                    Text(stringResource(R.string.nostr_npub_confirm_no_button))
                }
            }
        }
    }
}

// The web `nip07` browser-extension mode is web-only (nostr.md § Architectural
// rules #4) — native apps offer a NIP-46 remote signer instead, mirroring
// apple's `NostrVM.LinkMode.remote`. Labels come from the shared
// `fauna_client_bridges::nostr_link_mode_label` map (FFI: [nostrLinkModeLabel])
// rather than a hand-rolled mode → R.string table, mirroring linux/tui.
private val LINK_MODES = listOf("generate", "import", "remote")

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun UnlinkedAccountSection(onLink: (mode: String, fields: Map<String, String>) -> Unit) {
    var mode by remember { mutableStateOf(LINK_MODES.first()) }
    var nsec by remember { mutableStateOf("") }
    var bunkerUrl by remember { mutableStateOf("") }
    var expanded by remember { mutableStateOf(false) }

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(stringResource(R.string.nostr_link_account_title), style = MaterialTheme.typography.titleMedium)
            Text(
                stringResource(R.string.nostr_link_account_description),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(8.dp))

            val currentLabel = localized(nostrLinkModeLabel(mode)) ?: mode
            ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = !expanded }) {
                OutlinedTextField(
                    value = currentLabel,
                    onValueChange = {},
                    readOnly = true,
                    label = { Text(stringResource(R.string.nostr_link_account_mode_label)) },
                    trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
                    modifier = Modifier.fillMaxWidth().menuAnchor().testTag(Ids.NOSTR_LINK_MODE),
                )
                ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                    LINK_MODES.forEach { wireMode ->
                        DropdownMenuItem(
                            text = { Text(localized(nostrLinkModeLabel(wireMode)) ?: wireMode) },
                            onClick = { mode = wireMode; expanded = false },
                        )
                    }
                }
            }

            if (mode == "import") {
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = nsec,
                    onValueChange = { nsec = it },
                    label = { Text(stringResource(R.string.nostr_link_account_nsec_label)) },
                    placeholder = { Text(stringResource(R.string.nostr_link_account_nsec_placeholder)) },
                    singleLine = true,
                    visualTransformation = androidx.compose.ui.text.input.PasswordVisualTransformation(),
                    modifier = Modifier.fillMaxWidth().testTag(Ids.NOSTR_NSEC_INPUT),
                )
            }
            if (mode == "remote") {
                // NIP-46 bunker URL — no ui.yaml id (native-only field, mirrors apple).
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = bunkerUrl,
                    onValueChange = { bunkerUrl = it },
                    placeholder = { Text("bunker://pubkey?relay=wss://...") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            }

            Spacer(Modifier.height(12.dp))
            Button(
                onClick = {
                    val fields = when (mode) {
                        "import" -> mapOf("nsec" to nsec)
                        "remote" -> mapOf("bunker_url" to bunkerUrl)
                        else -> emptyMap()
                    }
                    onLink(mode, fields)
                },
                modifier = Modifier.testTag(Ids.NOSTR_LINK_BUTTON),
            ) {
                Text(stringResource(R.string.nostr_link_account_link_button))
            }
        }
    }
}

@Composable
private fun ContentSettingsSection(bridge: FfiBridgeStatus, onUpdateSetting: (String, FfiCborValue) -> Unit) {
    // Element id, wire key, default AND label all come from the shared catalog
    // (`nostr.md` § Where logic lives) — android used to hand-write one
    // `settingToggle(...)` call per row, the fifth of seven copies of the same
    // table. `remember` because the catalog is a UniFFI call, not a constant.
    val toggles = remember { nostrContentToggleOptions() }
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(stringResource(R.string.nostr_settings_title), style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(8.dp))
            for (toggle in toggles) {
                settingToggle(toggle, bridge.settings, onUpdateSetting)
            }
        }
    }
}

@Composable
private fun settingToggle(
    toggle: FfiBridgeToggleOption,
    settings: List<com.fauna.ffi.FfiBridgeSetting>,
    onUpdate: (String, FfiCborValue) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(localized(toggle.label) ?: "")
            // The catalog's second line, where a row has one — the same
            // subtitle linux renders under its SwitchRow.
            toggle.subtitle?.let { subtitle ->
                Text(
                    localized(subtitle) ?: "",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.outline,
                )
            }
        }
        Switch(
            checked = boolSetting(settings, toggle.key, toggle.defaultOn),
            onCheckedChange = { onUpdate(toggle.key, FfiCborValue.Bool(it)) },
            modifier = Modifier.testTag(toggle.uiId),
        )
    }
}

@Composable
private fun RelaysSection(
    bridge: FfiBridgeStatus,
    onAddRelay: (String) -> Unit,
    onRemoveRelay: (String) -> Unit,
) {
    var newRelay by remember { mutableStateOf("") }
    val relays = parseRelayList(bridge.settings)

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(stringResource(R.string.nostr_relays_title), style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(8.dp))

            if (relays.isEmpty()) {
                Text(stringResource(R.string.nostr_relays_none), color = MaterialTheme.colorScheme.outline)
            } else {
                relays.forEach { relay ->
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp).testTag(Ids.NOSTR_RELAY_ITEM),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.SpaceBetween,
                    ) {
                        Text(relay, modifier = Modifier.weight(1f), maxLines = 1)
                        IconButton(
                            onClick = { onRemoveRelay(relay) },
                            modifier = Modifier.testTag(Ids.NOSTR_REMOVE_RELAY),
                        ) {
                            Icon(Icons.Default.Delete, stringResource(R.string.common_remove), tint = MaterialTheme.colorScheme.error)
                        }
                    }
                }
            }

            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = newRelay,
                    onValueChange = { newRelay = it },
                    placeholder = { Text(stringResource(R.string.nostr_relays_placeholder)) },
                    singleLine = true,
                    modifier = Modifier.weight(1f).testTag(Ids.NOSTR_RELAY_INPUT),
                )
                IconButton(
                    onClick = { onAddRelay(newRelay); newRelay = "" },
                    modifier = Modifier.testTag(Ids.NOSTR_ADD_RELAY),
                ) {
                    Icon(Icons.Default.Add, stringResource(R.string.nostr_relays_add))
                }
            }
        }
    }
}

@Composable
private fun FollowsSection(
    follows: List<FfiBridgeFollow>,
    onAddFollow: (String, String?) -> Unit,
    onRemoveFollow: (String) -> Unit,
) {
    var pubkey by remember { mutableStateOf("") }
    var petname by remember { mutableStateOf("") }

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(stringResource(R.string.nostr_follows_title), style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(8.dp))

            if (follows.isEmpty()) {
                Text(stringResource(R.string.nostr_follows_none), color = MaterialTheme.colorScheme.outline)
            } else {
                follows.forEach { follow ->
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp).testTag(Ids.NOSTR_FOLLOW_ITEM),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.SpaceBetween,
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text(follow.id, maxLines = 1)
                            follow.petname?.takeIf { it.isNotEmpty() }?.let {
                                Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                        }
                        IconButton(
                            onClick = { onRemoveFollow(follow.id) },
                            modifier = Modifier.testTag(Ids.NOSTR_REMOVE_FOLLOW),
                        ) {
                            Icon(Icons.Default.Delete, stringResource(R.string.common_remove), tint = MaterialTheme.colorScheme.error)
                        }
                    }
                }
            }

            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = pubkey,
                    onValueChange = { pubkey = it },
                    placeholder = { Text(stringResource(R.string.nostr_follows_pubkey_placeholder)) },
                    singleLine = true,
                    modifier = Modifier.weight(1f).testTag(Ids.NOSTR_FOLLOW_PUBKEY_INPUT),
                )
                OutlinedTextField(
                    value = petname,
                    onValueChange = { petname = it },
                    placeholder = { Text(stringResource(R.string.nostr_follows_petname_placeholder)) },
                    singleLine = true,
                    modifier = Modifier.weight(1f).testTag(Ids.NOSTR_FOLLOW_PETNAME_INPUT),
                )
                IconButton(
                    onClick = {
                        onAddFollow(pubkey, petname.ifBlank { null })
                        pubkey = ""
                        petname = ""
                    },
                    modifier = Modifier.testTag(Ids.NOSTR_ADD_FOLLOW),
                ) {
                    Icon(Icons.Default.Add, stringResource(R.string.nostr_follows_add))
                }
            }
        }
    }
}

/**
 * Zap signers (the NIP-57 trust root — `docs/goal/behavior/monetization.md`
 * § Zap receipts — the trust model; `nostr.md` § Layout & flow item 7).
 * Mirrors [FollowsSection]'s roster shape (add/remove, no local mirror —
 * every mutation re-lists). Rendered for ANY linked account, unlike
 * [ConnectedAppsSection] above: designating who may speak for your money is
 * orthogonal to where your key lives.
 *
 * ⚠ **The STORED pubkey, never the typed input** — [ZapSignerItem] rows
 * come straight from the nest's reply, which is why every mutation re-lists
 * instead of pushing a locally-built row: the nest normalizes to lowercase,
 * and only that form ever matches a real receipt.
 *
 * [zapSignerGateRow] is the `zaps` gated-feature row's Dim-3 courtesy read
 * for the add button — `null` while available OR un-hydrated (an
 * un-hydrated read must leave the button LIVE; the nest, not the app, is the
 * enforcement floor). Removal is never gated — de-escalation.
 */
@Composable
private fun ZapSignersSection(
    zapSigners: List<ZapSignerItem>,
    zapSignerGateRow: FfiFeatureRow?,
    onAddZapSigner: (pubkey: String, label: String) -> Unit,
    onRemoveZapSigner: (String) -> Unit,
) {
    var pubkey by remember { mutableStateOf("") }
    var label by remember { mutableStateOf("") }
    // The Dim-3 courtesy gate: the decision is READ off the shared row,
    // never re-derived — a page composing its own meet would miss the
    // `payments` subset edge a `payments` deny reaches `zaps` through.
    val zapSignerAddGateReason = zapSignerGateRow?.let {
        if (it.affordance == "available") null else (localizedNested(it.restriction) ?: localized(it.status))
    }
    // Designating a signer is a commit (`fauna.nostr.zap_signers.add`); the
    // pubkey and label fields beside it are buffers and stay live. The Dim-3
    // verdict is handed over as the call site's own predicate, so a feature
    // deny still wins with a nest present.
    val addGate = faunaGate("fauna.nostr.zap_signers.add", zapSignerAddGateReason == null)

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(stringResource(R.string.nostr_zap_signers_title), style = MaterialTheme.typography.titleMedium)
            Text(
                stringResource(R.string.nostr_zap_signers_description),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(8.dp))

            if (zapSigners.isEmpty()) {
                Text(
                    stringResource(R.string.nostr_zap_signers_none),
                    color = MaterialTheme.colorScheme.outline,
                    modifier = Modifier.testTag(Ids.NOSTR_ZAP_SIGNER_EMPTY),
                )
            } else {
                zapSigners.forEach { signer ->
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp).testTag(Ids.NOSTR_ZAP_SIGNER_ITEM),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.SpaceBetween,
                    ) {
                        Text(
                            "${signer.label.ifBlank { stringResource(R.string.nostr_zap_signers_unnamed) }} — ${shortId(signer.signerPubkey)}",
                            maxLines = 1,
                            modifier = Modifier.weight(1f),
                        )
                        // Undesignating a signer is an immediate commit — no
                        // arming step, so the row button IS the commit
                        // (`fauna.nostr.zap_signers.remove`). Same per-row shape
                        // as the bunker revoke below.
                        val removeGate = faunaGate("fauna.nostr.zap_signers.remove")
                        Column {
                            IconButton(
                                onClick = { onRemoveZapSigner(signer.signerPubkey) },
                                enabled = removeGate.enabled,
                                modifier = Modifier.testTag(Ids.NOSTR_ZAP_SIGNER_REMOVE),
                            ) {
                                Icon(Icons.Default.Delete, stringResource(R.string.common_remove), tint = MaterialTheme.colorScheme.error)
                            }
                            DisabledControlReasonText(removeGate.reason)
                        }
                    }
                }
            }

            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = pubkey,
                    onValueChange = { pubkey = it },
                    placeholder = { Text(stringResource(R.string.nostr_zap_signers_pubkey_placeholder)) },
                    singleLine = true,
                    modifier = Modifier.weight(1f).testTag(Ids.NOSTR_ZAP_SIGNER_PUBKEY_INPUT),
                )
                OutlinedTextField(
                    value = label,
                    onValueChange = { label = it },
                    placeholder = { Text(stringResource(R.string.nostr_zap_signers_label_placeholder)) },
                    singleLine = true,
                    modifier = Modifier.weight(1f).testTag(Ids.NOSTR_ZAP_SIGNER_LABEL_INPUT),
                )
                IconButton(
                    onClick = {
                        onAddZapSigner(pubkey, label)
                        pubkey = ""
                        label = ""
                    },
                    enabled = addGate.enabled,
                    modifier = Modifier.testTag(Ids.NOSTR_ZAP_SIGNER_ADD_BTN),
                ) {
                    Icon(Icons.Default.Add, stringResource(R.string.nostr_zap_signers_add))
                }
            }
            // The Dim-3 reason is the more specific one and wins: `addGate.reason`
            // is non-null only when the outage is what closed the button.
            DisabledControlReasonText(addGate.reason ?: zapSignerAddGateReason)
        }
    }
}

/**
 * Connected apps (NIP-46 bunker) invite start — `docs/goal/ui/nostr.md` §
 * Layout & flow item 6. Rendered only for a linked custodial account
 * (`generated`/`imported`), mirrored by the caller's [CUSTODIAL_MODES] gate.
 * Mint an invite → the one-time `bunker://` connect string reveals (string + QR
 * + copy). The pending/active roster and its per-row revoke moved to the
 * Connected apps page (`connected-apps.md` § Architectural rules): the Nostr
 * page keeps only the start, so a connection is listed in exactly one place.
 */
@Composable
private fun ConnectedAppsSection(
    bunkerInvite: FfiCreateBunkerInviteReply?,
    onConnectApp: () -> Unit,
) {
    val clipboard = LocalClipboardManager.current

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(stringResource(R.string.nostr_connected_apps_title), style = MaterialTheme.typography.titleMedium)
            Text(
                stringResource(R.string.nostr_connected_apps_description),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(8.dp))

            // No separate confirm: this button IS the commit — one click mints a
            // pending connection on the nest (`fauna.nostr.bunker.create_invite`),
            // so it is the control that declares. The copy button and the QR /
            // connect-string reveal below it are pure local reads of an invite
            // already in hand and stay live.
            val inviteGate = faunaGate("fauna.nostr.bunker.create_invite")
            Button(
                onClick = onConnectApp,
                enabled = inviteGate.enabled,
                modifier = Modifier.testTag(Ids.NOSTR_BUNKER_CONNECT_BTN),
            ) {
                Text(stringResource(R.string.nostr_connected_apps_connect_button))
            }
            DisabledControlReasonText(inviteGate.reason)

            if (bunkerInvite != null) {
                Spacer(Modifier.height(8.dp))
                Text(stringResource(R.string.nostr_connected_apps_reveal_title), style = MaterialTheme.typography.labelMedium)
                Spacer(Modifier.height(8.dp))
                // Encoded once per invite (not every recomposition), and a failure
                // just skips the QR — the string reveal below is the real fallback,
                // matching encodeIdentityQr's catch-and-null discipline.
                val qr = remember(bunkerInvite.connectString) {
                    try {
                        qrMatrix(bunkerInvite.connectString)
                    } catch (e: FfiException) {
                        null
                    }
                }
                qr?.let {
                    QrCanvas(
                        matrix = it,
                        modifier = Modifier.size(200.dp).testTag(Ids.NOSTR_BUNKER_CONNECT_QR),
                    )
                    Spacer(Modifier.height(8.dp))
                }
                Text(
                    bunkerInvite.connectString,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.NOSTR_BUNKER_CONNECT_STRING),
                )
                Spacer(Modifier.height(4.dp))
                OutlinedButton(
                    onClick = { clipboard.setText(AnnotatedString(bunkerInvite.connectString)) },
                    modifier = Modifier.testTag(Ids.NOSTR_BUNKER_CONNECT_COPY_BTN),
                ) {
                    Text(stringResource(R.string.common_copy))
                }
                Text(stringResource(R.string.nostr_connected_apps_reveal_hint), style = MaterialTheme.typography.labelSmall)
            }
        }
    }
}
