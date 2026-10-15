package com.fauna.app.ui.screen.devices

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
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
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.HexUtil
import com.fauna.app.ui.components.CopyButton
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.components.TokenSelect
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.custodyHeldBytesText
import com.fauna.app.ui.util.custodyReceiptStatusText
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.resolveLocalizedNested
import com.fauna.app.ui.viewmodel.CustodyGesture
import com.fauna.app.ui.viewmodel.DevicesVM
import com.fauna.ffi.shortId
import uniffi.fauna_devices_machine.DeviceSummary
import uniffi.fauna_devices_machine.DevicesSnapshot
import uniffi.fauna_devices_machine.FleetMemberSummary
import social.fauna.generated.Ids
import uniffi.fauna_client_capabilities.CustodyHeldRowView
import uniffi.fauna_client_capabilities.CustodyMintCandidateView
import uniffi.fauna_client_capabilities.CustodyOfferRowView

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
    val custodyHeld by vm.custodyHeld.collectAsState()
    val custodyOffers by vm.custodyOffers.collectAsState()
    val custodyOfferTargets by vm.custodyOfferTargets.collectAsState()
    val keylessPrincipals by vm.keylessPrincipals.collectAsState()
    val mintCandidates by vm.mintCandidates.collectAsState()
    val memberRemovePending by vm.memberRemovePending.collectAsState()
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
            keylessPrincipals = keylessPrincipals,
            custodyRows = custodyRows,
            custodyHeld = custodyHeld,
            custodyOffers = custodyOffers,
            custodyOfferTargets = custodyOfferTargets,
            mintCandidates = mintCandidates,
            onCustodyGesture = vm::runCustodyGesture,
            // Both custody lines are resolved out here and injected as plain
            // strings — the `conflictBadgeLabels` idiom that keeps the stateless
            // Content free of FFI calls under the Compose test harness.
            receiptStatusLine = { receipt -> custodyReceiptStatusText(context, receipt) },
            heldBytesLine = { receipt -> custodyHeldBytesText(context, receipt) },
            memberRemovePending = memberRemovePending,
            onMemberArm = vm::armMemberRemoval,
            onMemberCancel = vm::cancelMemberRemoval,
            onMemberConfirm = vm::confirmMemberRemoval,
            // The shared absolute-local timestamp door, injected so the stateless
            // Content stays FFI-free under Robolectric (the `statusLabel` idiom).
            memberEnrolledAt = { ms -> com.fauna.ffi.formatUnixLocalMs(ms) },
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
    // ── T16 custody facet (devices.md § Custody facet, pieces 1–3 + the mint) ──
    // Piece 1: the enrolled principals the shared `keyless_posture` rule marks
    // relay-only — a roster card whose `principal` is in here wears
    // `device-keyless-posture-badge`.
    keylessPrincipals: Set<String> = emptySet(),
    // The shared fold's three row families, straight off the boundary — NOT
    // `DevicesSnapshot` state (the facet folds the `fauna.state.custody-ceremony` entries the keyless
    // machine cannot read), which is why they arrive as their own parameters.
    custodyRows: List<uniffi.fauna_client_capabilities.CustodyHolderRowView> = emptyList(),
    custodyHeld: List<CustodyHeldRowView> = emptyList(),
    custodyOffers: List<CustodyOfferRowView> = emptyList(),
    // The offers (lowercase-hex grant id) whose card renders the target select —
    // the shared `custody_offer_shows_target_select` answer, never a row field.
    custodyOfferTargets: Set<String> = emptySet(),
    // The mint flow: null = closed, else the host options it is open over.
    mintCandidates: List<CustodyMintCandidateView>? = null,
    // Every custody control's one sink — each gesture carries its grant id,
    // never a row index, which a refold can re-point at a different custody.
    onCustodyGesture: (CustodyGesture) -> Unit = {},
    // The two custody lines, already resolved (same injection reasoning as
    // [statusLabel]): the three-state receipt status, and held-bytes-against-budget
    // with the degraded marker riding it.
    receiptStatusLine: (uniffi.fauna_client_capabilities.CustodyReceiptRowView) -> String = { "" },
    heldBytesLine: (uniffi.fauna_client_capabilities.CustodyReceiptRowView) -> String = { "" },
    // ── Members without a matching entry (devices.md § Members without a matching entry) ──
    // The armed card's fleet id (`FleetMemberSummary.deviceId`), or null — keyed,
    // never positional; the VM owns it.
    memberRemovePending: String? = null,
    onMemberArm: (String) -> Unit = {},
    onMemberCancel: () -> Unit = {},
    onMemberConfirm: (String) -> Unit = {},
    // `{when}` for `device-member-enrolled-at`: the shared absolute-local
    // timestamp door over `enrolledAtMs`, injected like [statusLabel].
    memberEnrolledAt: (Long) -> String = { "" },
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
                    keyless = device.principal?.let { it in keylessPrincipals } == true,
                    ownFingerprint = snapshot?.ownFingerprint,
                )
            }
        }
        // The member group: signed-in fleet devices no roster row accounts for.
        // Renders NOTHING while empty — no title, no note.
        val members = snapshot?.members.orEmpty()
        if (members.isNotEmpty()) {
            SectionHeader(stringResource(R.string.devices_members_title))
            Text(
                stringResource(R.string.devices_member_note),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.DEVICE_MEMBER_NOTE),
            )
            members.forEach { member ->
                MemberCard(
                    member,
                    armed = memberRemovePending == member.deviceId,
                    onMemberArm, onMemberCancel, onMemberConfirm, memberEnrolledAt,
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
                CustodyHolderCard(
                    row,
                    { grantId, holder -> onCustodyGesture(CustodyGesture.Revoke(grantId, holder)) },
                    receiptStatusLine,
                    heldBytesLine,
                )
            }
        }
        // The host side — "what I hold for others" — then the offers waiting on
        // this account's consent, then the ask: the facet's order, below the
        // roster (none of these are this account's devices). Each group is
        // hidden while empty, as above.
        if (custodyHeld.isNotEmpty()) {
            SectionHeader(stringResource(R.string.devices_custody_held_section))
            custodyHeld.forEach { row -> CustodyHeldCard(row, heldBytesLine, onCustodyGesture) }
        }
        // The cards carry their own titles ("‹owner› asks this device…"), so
        // the group needs no header.
        custodyOffers.forEach { offer ->
            CustodyOfferCard(
                offer,
                showsTarget = HexUtil.bytesToHex(offer.grantId) in custodyOfferTargets,
                onCustodyGesture,
            )
        }
        CustodyMintFlow(mintCandidates, onCustodyGesture)
    }
}

/**
 * One `custody-held-card` — a custody this device holds for another account
 * (`devices.md` § Custody facet, piece 3): the owner, the scope, the metered
 * bytes against the budget, the budget input, stop and remove. Every control is
 * live — each raises one [CustodyGesture] the VM runs through the shared
 * `run_custody_act` and answers on `error-message`.
 */
@Composable
private fun CustodyHeldCard(
    row: CustodyHeldRowView,
    heldBytesLine: (uniffi.fauna_client_capabilities.CustodyReceiptRowView) -> String,
    onCustodyGesture: (CustodyGesture) -> Unit,
) {
    // The budget input seeds from the shared `budget_draft` text and re-seeds on
    // every fold that changes it (a committed budget, a re-folded registry row),
    // so a stale local draft never outlives the persisted cap.
    val seed = localized(row.budgetDraft).orEmpty()
    var draft by remember(HexUtil.bytesToHex(row.grantId), seed) { mutableStateOf(seed) }
    val commit = { onCustodyGesture(CustodyGesture.SetBudget(row.grantId, draft)) }
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.CUSTODY_HELD_CARD)) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                stringResource(R.string.devices_custody_held_owner)
                    .replace("{owner}", shortId(HexUtil.bytesToHex(row.owner))),
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.CUSTODY_HELD_OWNER),
            )
            val scopes = row.scopes
            Text(
                if (scopes != null && !scopes.wholeAccount) {
                    scopes.scopes.joinToString(", ")
                } else {
                    stringResource(R.string.devices_custody_held_scope_account)
                },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.CUSTODY_HELD_SCOPE),
            )
            Text(
                heldBytesLine(row.receipt),
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.CUSTODY_HELD_BYTES),
            )
            // Commit on the keyboard's Done — a single-line field also performs
            // its IME action on a hardware Enter, which is the commit the e2e
            // driver presses (`drivers/android.py::COMMIT_ON_ENTER_INPUTS`).
            OutlinedTextField(
                value = draft,
                onValueChange = { draft = it },
                label = { Text(stringResource(R.string.devices_custody_budget_label)) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { commit() }),
                modifier = Modifier.fillMaxWidth().testTag(Ids.CUSTODY_HELD_BUDGET_INPUT),
            )
            // The reclaim is a nest call (OnlineOnly), so offline it greys with the
            // shared reason; stop/budget are not gated here.
            val removeGate = faunaGate("fauna.custody.hosting.remove")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                // Stopping pauses the hold; it does not free the space. Once
                // stopped the control says so and disables — the bytes stay
                // until the custody is removed.
                OutlinedButton(
                    onClick = { onCustodyGesture(CustodyGesture.Stop(row.grantId)) },
                    enabled = !row.stopped,
                    modifier = Modifier.testTag(Ids.CUSTODY_HELD_STOP_BUTTON),
                ) {
                    Text(
                        stringResource(
                            if (row.stopped) R.string.devices_custody_stopped_bytes_remain
                            else R.string.devices_custody_stop,
                        ),
                    )
                }
                // Always available, stopped or not: stop is the pause and this
                // is the reclaim.
                TextButton(
                    onClick = { onCustodyGesture(CustodyGesture.Remove(row.grantId)) },
                    enabled = removeGate.enabled,
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                    modifier = Modifier.testTag(Ids.CUSTODY_HELD_REMOVE_BUTTON),
                ) { Text(stringResource(R.string.devices_custody_remove)) }
            }
            DisabledControlReasonText(removeGate.reason)
        }
    }
}

/**
 * One `custody-offer-card` — the consent surface (`devices.md` § Custody facet,
 * piece 3). The floor copy is REQUIRED before accept: what this device would see
 * — the shape, never the content. The target select renders ONLY where
 * [showsTarget] (the shared `custody_offer_shows_target_select` answer) says so —
 * absent otherwise, never disabled — and the accept then binds what it picks.
 */
@Composable
private fun CustodyOfferCard(
    offer: CustodyOfferRowView,
    showsTarget: Boolean,
    onCustodyGesture: (CustodyGesture) -> Unit,
) {
    var onNest by remember(HexUtil.bytesToHex(offer.grantId)) { mutableStateOf(false) }
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.CUSTODY_OFFER_CARD)) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                stringResource(R.string.devices_custody_offer_title)
                    .replace("{owner}", shortId(HexUtil.bytesToHex(offer.owner))),
                style = MaterialTheme.typography.titleSmall,
            )
            Text(
                stringResource(R.string.devices_custody_offer_floor),
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.CUSTODY_OFFER_FLOOR_NOTE),
            )
            if (showsTarget) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        stringResource(R.string.devices_custody_offer_target_label),
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(end = 8.dp),
                    )
                    TokenSelect(
                        testTagValue = Ids.CUSTODY_OFFER_TARGET_SELECT,
                        selected = if (onNest) TARGET_NEST else TARGET_DEVICE,
                        options = listOf(
                            TARGET_DEVICE to stringResource(R.string.devices_custody_offer_target_device),
                            TARGET_NEST to stringResource(R.string.devices_custody_offer_target_nest),
                        ),
                        onSelect = { onNest = it == TARGET_NEST },
                    )
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    // Where the select is absent the accept binds this device.
                    onClick = {
                        onCustodyGesture(CustodyGesture.Accept(offer.grantId, showsTarget && onNest))
                    },
                    modifier = Modifier.testTag(Ids.CUSTODY_OFFER_ACCEPT_BUTTON),
                ) { Text(stringResource(R.string.devices_custody_offer_accept)) }
                TextButton(
                    onClick = { onCustodyGesture(CustodyGesture.Decline(offer.grantId)) },
                    modifier = Modifier.testTag(Ids.CUSTODY_OFFER_DECLINE_BUTTON),
                ) { Text(stringResource(R.string.devices_custody_offer_decline)) }
            }
        }
    }
}

/** The target select's two tokens — this device (the default) or my nest. */
private const val TARGET_DEVICE = "device"
private const val TARGET_NEST = "nest"

/**
 * The offer-initiation flow (`custody-mint-*`): the button, and — once the VM has
 * read the candidates ([candidates] non-null) — the host select, the REQUIRED
 * floor copy, confirm and cancel. v1 offers the Account scope with the default
 * window over an EXISTING 1:1 conversation, so the options are those
 * conversations. Each option's token is the host's lowercase-hex actor id, its
 * label the conversation's own; confirm is live only once a host is chosen (a
 * control that cannot succeed is not offered).
 */
@Composable
private fun CustodyMintFlow(
    candidates: List<CustodyMintCandidateView>?,
    onCustodyGesture: (CustodyGesture) -> Unit,
) {
    OutlinedButton(
        onClick = { onCustodyGesture(CustodyGesture.MintOpen) },
        modifier = Modifier.testTag(Ids.CUSTODY_MINT_BUTTON),
    ) { Text(stringResource(R.string.devices_custody_mint_button)) }
    if (candidates == null) return
    // Re-opening over a new candidate list starts from the placeholder again.
    var chosen by remember(candidates) { mutableStateOf("") }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            stringResource(R.string.devices_custody_mint_host_label),
            style = MaterialTheme.typography.bodySmall,
        )
        TokenSelect(
            testTagValue = Ids.CUSTODY_MINT_HOST_SELECT,
            selected = chosen,
            options = listOf("" to stringResource(R.string.devices_custody_mint_host_placeholder)) +
                candidates.map { HexUtil.bytesToHex(it.host) to it.label },
            onSelect = { chosen = it },
        )
        Text(
            stringResource(R.string.devices_custody_mint_floor),
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.testTag(Ids.CUSTODY_MINT_FLOOR_NOTE),
        )
        val pick = candidates.firstOrNull { HexUtil.bytesToHex(it.host) == chosen }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                // Passed back unchanged — the page picks a row, it never
                // assembles a channel.
                onClick = { pick?.let { onCustodyGesture(CustodyGesture.Mint(it.host, it.channelHex)) } },
                enabled = pick != null,
                modifier = Modifier.testTag(Ids.CUSTODY_MINT_CONFIRM_BUTTON),
            ) { Text(stringResource(R.string.devices_custody_mint_confirm)) }
            TextButton(
                onClick = { onCustodyGesture(CustodyGesture.MintCancel) },
                modifier = Modifier.testTag(Ids.CUSTODY_MINT_CANCEL_BUTTON),
            ) { Text(stringResource(R.string.common_cancel)) }
        }
    }
}

/**
 * One `custody-holder-card` — a cross-account custodian device holding sealed
 * copies of this account's planes (`devices.md` § Custody facet, piece 2). The
 * host-side families (piece 3) and the mint render below it, from the same fold.
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

/**
 * One `device-member-card` — a verified fleet member no roster row accounts for.
 * Shows only what the fleet record asserts: the shared-rendered fingerprint and
 * the self-asserted enrollment instant. Removal is two-step and key-addressed:
 * the remove button arms THIS card (its confirm/cancel pair replaces the button),
 * confirm hands back the member's fleet id.
 */
@Composable
private fun MemberCard(
    member: FleetMemberSummary,
    armed: Boolean,
    onArm: (String) -> Unit,
    onCancel: () -> Unit,
    onConfirm: (String) -> Unit,
    enrolledAt: (Long) -> String,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.DEVICE_MEMBER_CARD)) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                // The fingerprint arrives rendered (`fleet_fingerprint`) — never a local formatter.
                stringResource(R.string.devices_member_fingerprint)
                    .replace("{fingerprint}", member.fingerprint),
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.DEVICE_MEMBER_FINGERPRINT),
            )
            Text(
                stringResource(R.string.devices_member_enrolled_at)
                    .replace("{when}", enrolledAt(member.enrolledAtMs)),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.DEVICE_MEMBER_ENROLLED_AT),
            )
            if (armed) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(
                        onClick = { onConfirm(member.deviceId) },
                        colors = ButtonDefaults.buttonColors(
                            containerColor = MaterialTheme.colorScheme.error,
                        ),
                        modifier = Modifier.testTag(Ids.DEVICE_MEMBER_REMOVE_CONFIRM_BUTTON),
                    ) { Text(stringResource(R.string.devices_member_remove_confirm)) }
                    TextButton(
                        onClick = onCancel,
                        modifier = Modifier.testTag(Ids.DEVICE_MEMBER_REMOVE_CANCEL_BUTTON),
                    ) { Text(stringResource(R.string.common_cancel)) }
                }
            } else {
                OutlinedButton(
                    onClick = { onArm(member.deviceId) },
                    modifier = Modifier.testTag(Ids.DEVICE_MEMBER_REMOVE_BUTTON),
                ) { Text(stringResource(R.string.common_remove)) }
            }
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
    keyless: Boolean = false,
    // This device's own fleet fingerprint (`DevicesSnapshot.ownFingerprint`),
    // painted beside `device-this-mark-badge` on the marked row only.
    ownFingerprint: String? = null,
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
            // Piece 1 of the custody facet (`devices.md` § Custody facet): an
            // own device enrolled relay-only wears the DERIVED posture — a fact,
            // never a toggle. The shared `keyless_posture` rule decides
            // ([DevicesVM.keylessPrincipals]); every unknown reads as "not
            // keyless", so the marker never rests on a guess.
            if (keyless) {
                Text(
                    stringResource(R.string.devices_keyless_posture_badge),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.DEVICE_KEYLESS_POSTURE_BADGE),
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
                // Beside the marker, the user's half of the comparison the member
                // group asks for; absent while the snapshot carries none.
                if (ownFingerprint != null) {
                    Text(
                        stringResource(R.string.devices_own_fingerprint)
                            .replace("{fingerprint}", ownFingerprint),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.testTag(Ids.DEVICE_OWN_FINGERPRINT),
                    )
                }
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
