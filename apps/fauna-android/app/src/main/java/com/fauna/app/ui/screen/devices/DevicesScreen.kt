package com.fauna.app.ui.screen.devices

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.HexUtil
import com.fauna.app.ui.components.CopyButton
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.custodyHeldBytesText
import com.fauna.app.ui.util.custodyReceiptStatusText
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.resolveLocalizedNested
import com.fauna.app.ui.viewmodel.DevicesVM
import com.fauna.ffi.shortId
import uniffi.fauna_devices_machine.DeviceSummary
import uniffi.fauna_devices_machine.DevicesSnapshot
import social.fauna.generated.Ids

/**
 * Settings → Devices (`docs/goal/ui/devices.md`): the device **roster only**.
 *
 * Since the 2026-06-28 sync/folder UI unification this page is the roster slice
 * of the shared [DevicesSnapshot]; the folder list / conflicts / create wizard
 * moved to Settings → Folders (`ui/screen/folders/FoldersScreen.kt`, the same
 * [DevicesVM]/[DevicesSnapshot]). `Peers` is no longer a top-level nav item — this
 * is a Settings sub-page reached via the Settings list, so it carries its own
 * back-navigating top bar like the other settings sub-pages.
 *
 * Stateless [DevicesRosterContent] is split out so it renders under the Compose
 * test harness with a seeded snapshot — **no page logic client-side**: the roster
 * reads `snapshot.devices` and the one gesture (remove) forwards to the machine
 * (priority #2, observer-driven rendering per the doc's § Architectural rules).
 * The page error, the custody gesture's own error, and the standing
 * enrollment-refusal notice ([DevicesVM.enrollmentNotice] — devices.md §
 * Errors & edge cases, in that precedence order) all flow to the navigation
 * shell's `error-message` banner via [LocalAppMessages]; the notice never
 * reaches [DevicesRosterContent] itself.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DevicesRosterScreen(
    navController: NavController,
    vm: DevicesVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val custodyRows by vm.custodyRows.collectAsState()
    val sharingError by vm.sharingError.collectAsState()
    val enrollmentNotice by vm.enrollmentNotice.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(Unit) { vm.start() }

    // One combined effect, not three independent ones: a roster gesture's own
    // error (errorText, then sharingError — the custody gesture's own already-
    // finished sentence, e2e convention 11) always outranks the standing
    // enrollment-refusal notice (devices.md § Errors & edge cases, mirroring
    // linux's devices_error_text) — and re-asserting on every key change is
    // what re-paints the notice after the shell's own per-nav
    // `LaunchedEffect(currentRoute) { messages.clear() }` (FaunaNavHost) wipes it.
    val errorText = localized(snapshot?.error)
    LaunchedEffect(errorText, sharingError, enrollmentNotice) {
        val text = errorText ?: sharingError ?: enrollmentNotice
        if (text != null) appMessages.showError(text)
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.devices_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = { navController.popBackStack() }) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.common_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        DevicesRosterContent(
            modifier = Modifier.padding(padding),
            snapshot = snapshot,
            localDeviceId = vm.localDeviceId,
            actorId = vm.actorId,
            onRemoveDevice = vm::removeDevice,
            onSetP2pParticipation = vm::setP2pParticipation,
            statusLabel = { online ->
                resolveLocalized(context, com.fauna.ffi.deviceStatusLabel(online)).orEmpty()
            },
            placeLabel = { originates, accepts, appliesDeletes ->
                // Nested resolve: the two/three-flag templates carry the
                // `devices.wizard.place_*` KEYS as their argument values.
                resolveLocalizedNested(
                    context,
                    com.fauna.ffi.devicePlaceLabel(originates, accepts, appliesDeletes),
                ).orEmpty()
            },
            custodyRows = custodyRows,
            onRevokeCustody = vm::revokeCustody,
            // Both custody lines are resolved out here and injected as plain
            // strings — the `conflictBadgeLabels` idiom that keeps the stateless
            // Content free of FFI calls under the Compose test harness.
            receiptStatusLine = { receipt -> custodyReceiptStatusText(context, receipt) },
            heldBytesLine = { receipt -> custodyHeldBytesText(context, receipt) },
        )
    }
}

@Composable
fun DevicesRosterContent(
    snapshot: DevicesSnapshot?,
    // This app's own locally-stored device id, or null pre-registration —
    // drives `device-this-mark-badge` (`devices.md` § This-device marker).
    localDeviceId: String?,
    // This client's own actor ID, or null pre-registration — drives the
    // page-level `peer-actor-id-copy-btn` below.
    actorId: String?,
    onRemoveDevice: (Int) -> Unit,
    // `device-p2p-participation-toggle[index]`'s click — `(index, on)`, the
    // machine's one gesture (it picks the arm; `p2p.md` § Per-device
    // participation).
    onSetP2pParticipation: (Int, Boolean) -> Unit = { _, _ -> },
    // `device-status` wording from the shared `device_status_label` map, resolved
    // by the stateful caller and injected so this Content stays FFI-free for
    // Robolectric.
    statusLabel: (Boolean) -> String,
    // `device-folder-role-badge` chip wording from the shared
    // `device_place_label` composer (same injection reasoning as
    // [statusLabel]) — states one `DeviceFolderRole`'s three place flags
    // (originates, accepts, appliesDeletes) through the SAME labels the create
    // wizard's place checkboxes carry, resolved nested.
    placeLabel: (Boolean, Boolean, Boolean) -> String,
    // ── T16 custody facet, owner side (devices.md § Custody facet, piece 2) ──
    // The shared fold's owner-side rows, straight off the boundary — NOT
    // `DevicesSnapshot` state (the facet folds the `fauna.state.custody-ceremony` entries the keyless
    // machine cannot read), which is why they arrive as their own parameter.
    custodyRows: List<uniffi.fauna_client_capabilities.CustodyHolderRowView> = emptyList(),
    // Revoke, carrying the row's grant id + accept-bound custodian key — never a
    // row index, which a refold can re-point at a different custody.
    onRevokeCustody: (ByteArray, ByteArray?) -> Unit = { _, _ -> },
    // The two custody lines, already resolved (same injection reasoning as
    // [statusLabel]): the three-state receipt status, and held-bytes-against-budget
    // with the degraded marker riding it.
    receiptStatusLine: (uniffi.fauna_client_capabilities.CustodyReceiptRowView) -> String = { "" },
    heldBytesLine: (uniffi.fauna_client_capabilities.CustodyReceiptRowView) -> String = { "" },
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(horizontal = 16.dp)
            .verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        // Single page-level instance (ui.yaml `indexed: false`, `devices.md` §
        // Layout & flow point 2) — copies THIS client's own actor ID, for
        // handing to a new device being paired. Rendered unconditionally
        // (even with an empty roster, since pairing the first device is
        // exactly when this is needed), never inside a `DeviceCard`.
        CopyButton(
            testTag = Ids.PEER_ACTOR_ID_COPY_BTN,
            text = actorId.orEmpty(),
            label = stringResource(R.string.devices_copy_actor_id),
        )
        val devices = snapshot?.devices.orEmpty()
        SectionHeader(stringResource(R.string.devices_enrolled_devices))
        if (devices.isEmpty()) {
            EmptyHint(stringResource(R.string.devices_no_devices))
        } else {
            devices.forEachIndexed { index, device ->
                DeviceCard(
                    device, index, localDeviceId, onRemoveDevice, onSetP2pParticipation,
                    statusLabel, placeLabel,
                )
            }
        }
        // The T16 custody facet's owner side — "who holds my data". A custody
        // whose accept bound the host's NEST belongs to the Nests page's
        // `nest-trust-custody-*` family instead: one custody never renders in
        // both places (the nest-custodian identity fact, ruled 2026-08-17), and
        // `custodian_nest_url` is the marker that says which.
        val custodyDeviceRows = custodyRows.filter { it.custodianNestUrl == null }
        // Hidden entirely when empty: an account with no custodians has nothing
        // to say here, and a titled-but-empty section reads as a feature that
        // failed to load. Same rule as linux's `build_custody_section`.
        if (custodyDeviceRows.isNotEmpty()) {
            SectionHeader(stringResource(R.string.devices_custody_holder_section))
            custodyDeviceRows.forEach { row ->
                CustodyHolderCard(row, onRevokeCustody, receiptStatusLine, heldBytesLine)
            }
        }
    }
}

/**
 * One `custody-holder-card` — a cross-account custodian device holding sealed
 * copies of this account's planes (`devices.md` § Custody facet, piece 2).
 *
 * Piece 2 only, and that is a prerequisite fact rather than a shortcut: pieces 1
 * (keyless-posture badge) and 3 (the held-for-others card with its budget input
 * and stop control) both need the W3 (account-data-plane.md § Workstreams) account store, which reaches no UniFFI app
 * yet — so the boundary exports neither store-writing gesture, and a card
 * offering controls that cannot succeed is exactly what the ratified piece 3
 * forbids.
 */
@Composable
private fun CustodyHolderCard(
    row: uniffi.fauna_client_capabilities.CustodyHolderRowView,
    onRevokeCustody: (ByteArray, ByteArray?) -> Unit,
    receiptStatusLine: (uniffi.fauna_client_capabilities.CustodyReceiptRowView) -> String,
    heldBytesLine: (uniffi.fauna_client_capabilities.CustodyReceiptRowView) -> String,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.CUSTODY_HOLDER_CARD)) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                // The counterpart account, abbreviated through the SAME shared
                // `short_id` every app uses, so an actor reads identically
                // across the seven UIs.
                shortId(HexUtil.bytesToHex(row.host)),
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.CUSTODY_HOLDER_NAME),
            )
            // Three states, three strings — never collapsed, never empty (the A7
            // honesty rule): a stale custodian must read as degraded redundancy
            // the owner can see, not as an absent row.
            Text(
                receiptStatusLine(row.receipt),
                style = MaterialTheme.typography.bodySmall,
                color = when (row.receiptState) {
                    uniffi.fauna_client_capabilities.CustodyReceiptStateView.STALE ->
                        MaterialTheme.colorScheme.error
                    else -> MaterialTheme.colorScheme.onSurfaceVariant
                },
                modifier = Modifier.testTag(Ids.CUSTODY_HOLDER_RECEIPT_STATUS),
            )
            Text(
                heldBytesLine(row.receipt),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.CUSTODY_HOLDER_HELD_BYTES),
            )
            // The honest bound, stated beside the control it bounds (REQUIRED —
            // `ui/nests.md` § Trust facet, custody rows): revoking stops future
            // carriage and serving on honest boxes; copies already held stay
            // held, and stay sealed forever. A pending ceremony has minted
            // nothing to revoke, so the note would over-promise there and the
            // control is disabled instead.
            if (!row.pending) {
                Text(
                    stringResource(R.string.devices_custody_revoke_bound_note),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            TextButton(
                onClick = { onRevokeCustody(row.grantId, row.custodianKey) },
                // A control that cannot succeed is not offered: while the
                // ceremony is pending there is no minted grant to revoke and no
                // bound holder to name.
                enabled = !row.pending,
                modifier = Modifier.testTag(Ids.CUSTODY_HOLDER_REVOKE_BUTTON),
            ) { Text(stringResource(R.string.devices_custody_revoke)) }
        }
    }
}

@Composable
private fun DeviceCard(
    device: DeviceSummary,
    index: Int,
    localDeviceId: String?,
    onRemoveDevice: (Int) -> Unit,
    onSetP2pParticipation: (Int, Boolean) -> Unit,
    statusLabel: (Boolean) -> String,
    placeLabel: (Boolean, Boolean, Boolean) -> String,
) {
    var showRemoveDialog by remember(device.deviceId) { mutableStateOf(false) }
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.DEVICE_CARD)) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    device.label,
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.weight(1f).testTag(Ids.DEVICE_NAME),
                )
                Text(
                    statusLabel(device.online),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.DEVICE_STATUS),
                )
            }
            // The guardian-enrolled-device marker (family-safety.md § Full visibility
            // for young children, Slice F). The ward's OWN list renders it — the child
            // must always see which device their guardian enrolled (transparency by
            // construction); always false on an unsupervised account, so the badge is
            // simply absent there. The nest already refuses removal of a marked row
            // (typed fauna.sync.guardian_marked, surfacing on the page's shared
            // error-message banner) — no client-side gate needed here.
            if (device.guardianMarked) {
                Text(
                    stringResource(R.string.devices_guardian_marked_badge),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.testTag(Ids.DEVICE_GUARDIAN_MARK_BADGE),
                )
            }
            // Not mutually exclusive with the guardian badge above — a
            // guardian marking their own enrolled device can legitimately
            // carry both (`devices.md` § This-device marker).
            if (device.deviceId == localDeviceId) {
                Text(
                    stringResource(R.string.devices_this_device_badge),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.testTag(Ids.DEVICE_THIS_MARK_BADGE),
                )
            }
            // One `device-folder-role-badge` chip per folder this device
            // carries, stating its place in that set (`DeviceSummary.folders`,
            // the roster slice's own field) — composed from the SAME place
            // labels the create wizard's checkboxes carry (`device_place_label`,
            // nested resolve).
            // Indexed: a device in three sets paints three chips; nothing
            // renders when the device carries no sets. Reference: apple
            // `DevicesContent.swift`'s `RoleBadge` row (`devices.md` §
            // Element table — `device-folder-role-badge`).
            if (device.folders.isNotEmpty()) {
                Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    device.folders.forEach { fs ->
                        Text(
                            placeLabel(fs.originates, fs.accepts, fs.appliesDeletes),
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier.testTag(Ids.DEVICE_FOLDER_ROLE_BADGE),
                        )
                    }
                }
            }
            // `device-p2p-participation-toggle` (`p2p.md` § Per-device
            // participation — rule 5's off switch; ID user-approved
            // 2026-09-25). Drawn exactly as the machine painted the row
            // (`DeviceSummary.p2pParticipationPaint`: own-ness, checked, label,
            // actionable) — the app never re-derives which row is its own, and
            // the machine picks the arm the click takes and paints any refusal
            // on `error-message`. The machine paints every row at every
            // snapshot, so a null paint is a row not painted yet: nothing to
            // draw. One toggleable node carries the label text, the checked
            // state and the click (the Material labelled-switch shape).
            // Reference: tui `settings/devices.rs`.
            device.p2pParticipationPaint?.let { paint ->
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .fillMaxWidth()
                        .toggleable(
                            value = paint.checked,
                            enabled = paint.actionable,
                            role = Role.Switch,
                            onValueChange = { onSetP2pParticipation(index, !paint.checked) },
                        )
                        .testTag(Ids.DEVICE_P2P_PARTICIPATION_TOGGLE),
                ) {
                    Text(
                        localized(paint.label).orEmpty(),
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.weight(1f),
                    )
                    Switch(checked = paint.checked, onCheckedChange = null, enabled = paint.actionable)
                }
            }
            Text(
                shortId(device.deviceId),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Spacer(Modifier.weight(1f))
                IconButton(
                    onClick = { showRemoveDialog = true },
                    modifier = Modifier.testTag(Ids.DEVICE_REMOVE_BUTTON),
                ) { Icon(Icons.Default.Delete, stringResource(R.string.common_delete)) }
            }
        }
    }
    if (showRemoveDialog) {
        AlertDialog(
            onDismissRequest = { showRemoveDialog = false },
            title = { Text(stringResource(R.string.devices_remove_member)) },
            text = { Text(device.label) },
            confirmButton = {
                TextButton(onClick = {
                    showRemoveDialog = false
                    onRemoveDevice(index)
                }) { Text(stringResource(R.string.common_delete)) }
            },
            dismissButton = {
                TextButton(onClick = { showRemoveDialog = false }) {
                    Text(stringResource(R.string.common_cancel))
                }
            },
        )
    }
}

@Composable
private fun SectionHeader(text: String, modifier: Modifier = Modifier) {
    Text(
        text,
        style = MaterialTheme.typography.titleMedium,
        modifier = modifier.padding(vertical = 8.dp),
    )
}

@Composable
private fun EmptyHint(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}
