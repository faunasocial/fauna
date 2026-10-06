package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.HexUtil
import com.fauna.app.core.UndenyDecide
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.localizedNested
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.FamilyVM
import com.fauna.ffi.FfiContentPolicy
import com.fauna.ffi.FfiFamilyApprovalEntry
import com.fauna.ffi.FfiFamilyContentNotice
import com.fauna.ffi.FfiFamilyIncomingTransfer
import com.fauna.ffi.FfiFamilyStatus
import com.fauna.ffi.FfiFamilyWardDevice
import com.fauna.ffi.FfiFamilyWardInfo
import com.fauna.ffi.FfiReachPolicy
import com.fauna.ffi.FfiScreenTimePolicy
import com.fauna.ffi.ageBandLine
import com.fauna.ffi.approvalDisplayText
import com.fauna.ffi.contentFloorLabel
import com.fauna.ffi.contentFloorOptions
import com.fauna.ffi.contentNoticeLine
import com.fauna.ffi.feedSourcesLabel
import com.fauna.ffi.feedSourcesOptions
import com.fauna.ffi.formatTimeOfDay
import com.fauna.ffi.parseDailyMinutes
import com.fauna.ffi.parseTimeOfDay
import com.fauna.ffi.reachPolicySummary
import com.fauna.ffi.unknownPeerDmLabel
import com.fauna.ffi.unknownPeerDmOptions
import com.fauna.ffi.unknownSenderLabel
import com.fauna.ffi.unknownSenderOptions
import com.fauna.ffi.usageTodayLine
import social.fauna.generated.Ids

/**
 * The **`family`** page (family-safety.md § App surface): one shared
 * `fauna.family.status` read renders both the guardian section (ward list,
 * the one shared 4-knob reach-policy editor, the typed approvals queue,
 * contact pre-approve, transfer, graduate) and the supervised section
 * (guardian handle + read-only policy summary), plus the incoming-transfer
 * prompt for a proposed guardian. Lifts the linux/web reference
 * same ui.yaml IDs (priority #1).
 *
 * VM-free [FamilyContent] is split out for the Robolectric harness (no Hilt,
 * no [FamilyVM]) — it still calls the real `unknownSenderOptions`/
 * `feedSourcesLabel`/`reachPolicySummary` shared-Rust reach-policy catalog
 * over UniFFI (family-safety.md § Where logic lives), which is why
 * `android-host-test` (not a bare `:app:testDebugUnitTest`) is required to
 * run it. The VM-bound [FamilyScreen] is what the NavHost mounts.
 * `page-heading` is the TopAppBar title; `error-message` is the global
 * MessageBanner (the same idiom `SubscriptionSettingsScreen` uses).
 */
@Composable
fun FamilyScreen(
    navController: NavController,
    vm: FamilyVM = hiltViewModel(),
) {
    val status by vm.status.collectAsState()
    val approvals by vm.approvals.collectAsState()
    val selectedWardActorId by vm.selectedWardActorId.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }
    LaunchedEffect(Unit) { vm.refresh() }

    FamilyContent(
        status = status,
        approvals = approvals,
        selectedWardActorId = selectedWardActorId,
        onBack = { navController.popBackStack() },
        onSelectWard = vm::selectWard,
        onSavePolicy = vm::savePolicy,
        onSaveError = { vm.errorMessage.value = it },
        onMarkDevice = vm::markDevice,
        onAllowBlockedPeer = vm::allowBlockedPeer,
        onDecideApproval = vm::decideApproval,
        onAddContact = vm::addContact,
        onGraduate = vm::graduate,
        onProposeTransfer = vm::proposeTransfer,
        onCancelTransfer = vm::cancelTransfer,
        onAcceptIncomingTransfer = vm::acceptIncomingTransfer,
        onDeclineIncomingTransfer = vm::declineIncomingTransfer,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FamilyContent(
    status: FfiFamilyStatus?,
    approvals: List<FfiFamilyApprovalEntry>,
    selectedWardActorId: ByteArray?,
    onBack: () -> Unit,
    onSelectWard: (ByteArray) -> Unit,
    onSavePolicy: (ByteArray, FfiReachPolicy) -> Unit,
    onSaveError: (String) -> Unit,
    onMarkDevice: (ByteArray, String, Boolean) -> Unit,
    onAllowBlockedPeer: (UndenyDecide) -> Unit,
    onDecideApproval: (FfiFamilyApprovalEntry, Boolean) -> Unit,
    onAddContact: (ByteArray, ByteArray) -> Unit,
    onGraduate: (ByteArray) -> Unit,
    onProposeTransfer: (ByteArray, ByteArray) -> Unit,
    onCancelTransfer: (ByteArray) -> Unit,
    onAcceptIncomingTransfer: (ByteArray) -> Unit,
    onDeclineIncomingTransfer: (ByteArray) -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.family_title), modifier = Modifier.testTag(Ids.PAGE_HEADING)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.common_back))
                    }
                },
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(16.dp)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(
                stringResource(R.string.family_title),
                style = MaterialTheme.typography.titleLarge,
                modifier = Modifier.testTag(Ids.FAMILY_HEADING),
            )

            if (status != null) {
                // ── Incoming-transfer prompt (any user can be a proposed guardian) ──
                if (status.incomingTransfers.isNotEmpty()) {
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(
                            stringResource(R.string.family_incoming_transfers_heading),
                            style = MaterialTheme.typography.titleMedium,
                        )
                        status.incomingTransfers.forEach { incoming ->
                            IncomingTransferRow(
                                incoming = incoming,
                                onAccept = { onAcceptIncomingTransfer(incoming.supervisedActorId) },
                                onDecline = { onDeclineIncomingTransfer(incoming.supervisedActorId) },
                            )
                        }
                    }
                }

                // ── Supervised section (rendered when the caller is supervised) ──
                val supervisedBy = status.supervisedBy
                if (supervisedBy != null) {
                    Column(modifier = Modifier.testTag(Ids.FAMILY_SUPERVISED_SECTION), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(
                            stringResourceFmt(R.string.family_guardian_label, supervisedBy.handle),
                            modifier = Modifier.testTag(Ids.FAMILY_GUARDIAN_HANDLE),
                            style = MaterialTheme.typography.bodyLarge,
                        )
                        Text(stringResource(R.string.family_policy_summary_heading), style = MaterialTheme.typography.titleSmall)
                        val policy = status.policy
                        if (policy != null) {
                            Text(
                                formatPolicySummary(policy, status.usageTodayMinutes),
                                modifier = Modifier.testTag(Ids.FAMILY_POLICY_SUMMARY),
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        }
                        // This account's own band + how it was established
                        // (family-safety.md § App surface → *Age-band surfaces*) —
                        // the shared `ageBandLine` the guardian's ward row also
                        // calls, so the two cannot disagree; absent, never
                        // placeholdered, when there is no nameable band.
                        status.ageBand
                            ?.let { localizedNested(ageBandLine(it.band, it.provenance, true)) }
                            ?.let { line ->
                                Text(
                                    line,
                                    modifier = Modifier.testTag(Ids.FAMILY_AGE_BAND_SUMMARY),
                                    style = MaterialTheme.typography.bodyMedium,
                                )
                            }
                    }
                }

                // ── Guardian section (rendered when the caller guards ≥1 account) ──
                if (status.wards.isNotEmpty()) {
                    GuardianSection(
                        wards = status.wards,
                        approvals = approvals,
                        selectedWardActorId = selectedWardActorId,
                        onSelectWard = onSelectWard,
                        onSavePolicy = onSavePolicy,
                        onSaveError = onSaveError,
                        onMarkDevice = onMarkDevice,
                        onAllowBlockedPeer = onAllowBlockedPeer,
                        onDecideApproval = onDecideApproval,
                        onAddContact = onAddContact,
                        onGraduate = onGraduate,
                        onProposeTransfer = onProposeTransfer,
                        onCancelTransfer = onCancelTransfer,
                    )
                }
            }
        }
    }
}

@Composable
private fun IncomingTransferRow(
    incoming: FfiFamilyIncomingTransfer,
    onAccept: () -> Unit,
    onDecline: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.FAMILY_INCOMING_TRANSFER_ITEM)) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                stringResourceFmt(R.string.family_incoming_transfer_text, incoming.guardianHandle, incoming.supervisedHandle),
                style = MaterialTheme.typography.bodyMedium,
            )
            // BOTH declare — unusually, the "no" is a commit too. Declining a
            // guardianship transfer is not a local dismissal: it tells the
            // proposing guardian's nest the offer was refused
            // (`fauna.family.transfer.decline`), so it needs a nest exactly as
            // much as the accept does. This is the one place in the fan-out so
            // far where a cancel-shaped control is NOT the live sibling.
            val acceptGate = faunaGate("fauna.family.transfer.accept")
            val declineGate = faunaGate("fauna.family.transfer.decline")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = onAccept,
                    enabled = acceptGate.enabled,
                    modifier = Modifier.testTag(Ids.FAMILY_INCOMING_TRANSFER_ACCEPT_BUTTON),
                ) {
                    Text(stringResource(R.string.family_incoming_transfer_accept_button))
                }
                OutlinedButton(
                    onClick = onDecline,
                    enabled = declineGate.enabled,
                    modifier = Modifier.testTag(Ids.FAMILY_INCOMING_TRANSFER_DECLINE_BUTTON),
                ) {
                    Text(stringResource(R.string.family_incoming_transfer_decline_button))
                }
            }
            DisabledControlReasonText(acceptGate.reason)
        }
    }
}

@Composable
private fun GuardianSection(
    wards: List<FfiFamilyWardInfo>,
    approvals: List<FfiFamilyApprovalEntry>,
    selectedWardActorId: ByteArray?,
    onSelectWard: (ByteArray) -> Unit,
    onSavePolicy: (ByteArray, FfiReachPolicy) -> Unit,
    onSaveError: (String) -> Unit,
    onMarkDevice: (ByteArray, String, Boolean) -> Unit,
    onAllowBlockedPeer: (UndenyDecide) -> Unit,
    onDecideApproval: (FfiFamilyApprovalEntry, Boolean) -> Unit,
    onAddContact: (ByteArray, ByteArray) -> Unit,
    onGraduate: (ByteArray) -> Unit,
    onProposeTransfer: (ByteArray, ByteArray) -> Unit,
    onCancelTransfer: (ByteArray) -> Unit,
) {
    Column(modifier = Modifier.testTag(Ids.FAMILY_GUARDIAN_SECTION), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(stringResource(R.string.family_wards_heading), style = MaterialTheme.typography.titleMedium)
        wards.forEach { ward ->
            WardRow(
                ward = ward,
                selected = selectedWardActorId?.contentEquals(ward.actorId) == true,
                onClick = { onSelectWard(ward.actorId) },
            )
        }

        // The one shared (non-indexed) policy editor + per-ward actions load
        // whichever ward is selected — family-safety.md § App surface: "a
        // guardian with several wards selects a family-ward-item row to load
        // that ward into the shared editor."
        val selectedWard = wards.find { selectedWardActorId?.contentEquals(it.actorId) == true }
        if (selectedWard != null) {
            PolicyEditor(ward = selectedWard, onSavePolicy = onSavePolicy, onSaveError = onSaveError)
            DeviceMarkSection(ward = selectedWard, onMarkDevice = onMarkDevice)
            BlockedPeersSection(ward = selectedWard, onAllow = onAllowBlockedPeer)
            ContactAddRow(supervisedActorId = selectedWard.actorId, onAddContact = onAddContact)
            TransferSection(ward = selectedWard, onProposeTransfer = onProposeTransfer, onCancelTransfer = onCancelTransfer)
            GraduateSection(ward = selectedWard, onGraduate = onGraduate)
        }

        // Not ward-scoped — one `approvals.list` read returns every ward's queue.
        Text(stringResource(R.string.family_approvals_heading), style = MaterialTheme.typography.titleMedium)
        if (approvals.isEmpty()) {
            Text(
                stringResource(R.string.family_no_approvals),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            approvals.forEach { entry -> ApprovalRow(entry = entry, onDecide = onDecideApproval) }
        }
    }
}

@Composable
private fun WardRow(ward: FfiFamilyWardInfo, selected: Boolean, onClick: () -> Unit) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .testTag(Ids.FAMILY_WARD_ITEM),
        colors = if (selected) {
            CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.secondaryContainer)
        } else {
            CardDefaults.cardColors()
        },
    ) {
        Column(modifier = Modifier.padding(12.dp)) {
            Text(
                ward.handle,
                modifier = Modifier.testTag(Ids.FAMILY_WARD_HANDLE),
                style = MaterialTheme.typography.bodyMedium,
            )
            // The ward's band + provenance (family-safety.md § App surface →
            // *Age-band surfaces*), only when the ward has a nameable band — a
            // band-less admission paints no row, never a placeholder.
            ward.ageBand
                ?.let { localizedNested(ageBandLine(it.band, it.provenance, false)) }
                ?.let { line ->
                    Text(
                        line,
                        modifier = Modifier.testTag(Ids.FAMILY_WARD_AGE_BAND),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            // Guardian Notify readout (family-safety.md § Guardian Notify) — one
            // "{category}: {count}" line per flagged category today, rendered
            // only when non-empty so a ward with nothing flagged stays clean.
            // Mirrors linux/web's placement directly under the handle, inside
            // the same indexed ward row.
            if (ward.contentNotices.isNotEmpty()) {
                Text(
                    formatContentNotices(ward.contentNotices),
                    modifier = Modifier.testTag(Ids.FAMILY_WARD_CONTENT_NOTICES),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            // The screen-time readout, on the same terms: rendered only when
            // the nest actually accounted a figure for this ward, which it
            // does only while a daily budget is set (family-safety.md §
            // Screen time — "no usage accounting without a declared
            // policy"). No budget → `null` → no row clutter. The wording is
            // the shared `usageTodayLine`, the same call the ward's own
            // summary makes, so guardian and child cannot be shown different
            // numbers.
            val used = ward.usageTodayMinutes
            if (used != null) {
                Text(
                    formatWardUsageToday(used, ward.policy.screenTime),
                    modifier = Modifier.testTag(Ids.FAMILY_WARD_USAGE_TODAY),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/** The guardian's per-ward `family-ward-content-notices` readout — one
 *  "{label}: {value}" line per Guardian Notify count on the ward
 *  (family-safety.md § Guardian Notify: category + count only, never
 *  content), over the shared `contentNoticeLine`; mirrors
 *  [formatPolicySummary]. */
@Composable
private fun formatContentNotices(notices: List<FfiFamilyContentNotice>): String {
    val lines = notices.map { n ->
        val line = contentNoticeLine(n.category, n.count)
        "${localized(line.label)}: ${localized(line.value)}"
    }
    return lines.joinToString("\n")
}

/** The guardian's per-ward `family-ward-usage-today` readout — the day's
 *  cross-device screen-time total for one ward (family-safety.md § Screen
 *  time). The number, its label and the "of budget" framing all come from
 *  shared Rust via [usageTodayLine] — the same call the ward's own summary
 *  makes ([formatPolicySummary]), which is what makes the goal doc's
 *  transparency promise structural. */
@Composable
private fun formatWardUsageToday(usedMinutes: UInt, screenTime: FfiScreenTimePolicy?): String {
    val line = usageTodayLine(usedMinutes, screenTime?.dailyMinutes)
    return "${localized(line.label)}: ${localized(line.value)}"
}

/**
 * The shared 4-knob reach-policy editor (family-safety.md § Guardian policy).
 * Local edit state re-derives from [ward]'s policy whenever the *selected
 * ward* changes (keyed on its hex actor id) — switching wards discards
 * unsaved edits, matching the windows/linux/web reference.
 *
 * **Fail-closed knob render (load-bearing — do NOT re-derive):** an
 * unparseable `unknown_sender_mail`/`feed_sources` wire value normalizes to
 * the strictest option (`hold`/`block`) at load time, never the permissive
 * one. Within a major version a client may be older than its nest, so a
 * newer nest can store a value this client can't name; rendering it as
 * `allow` would show the guardian a policy weaker than the one actually
 * enforced, and an untouched Save would write that permissive value back —
 * silently downgrading the ward's protection. Normalizing at load (not just
 * at display) means an untouched Save instead converges on the safe value.
 * windows shipped the bug this guards against (defaulted to `allow`/index 0);
 * linux/web/apple all normalize at load — this mirrors them.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PolicyEditor(
    ward: FfiFamilyWardInfo,
    onSavePolicy: (ByteArray, FfiReachPolicy) -> Unit,
    onSaveError: (String) -> Unit,
) {
    val wardKey = HexUtil.bytesToHex(ward.actorId)
    var contactApproval by remember(wardKey) { mutableStateOf(ward.policy.contactApproval) }
    var unknownSenderWire by remember(wardKey) { mutableStateOf(normalizeUnknownSenderWire(ward.policy.unknownSenderMail)) }
    var federationContact by remember(wardKey) { mutableStateOf(ward.policy.federationContact) }
    var feedSourcesWire by remember(wardKey) { mutableStateOf(normalizeFeedSourcesWire(ward.policy.feedSources)) }
    // The bridge-DM gate (family-safety.md § The bridge-DM gate): whether a DM
    // arriving over an already-connected bridge account, from a peer the ward
    // has never messaged, is held for guardian review — the twin `feed_sources`
    // cannot cover (that one only gates NEW connections). `unknownPeerDm` is
    // `Option<String>` on the wire — absent means "leave unchanged" — so
    // [unknownPeerDmTouched] is armed ONLY by a genuine select, never by this
    // seed assignment (the same discipline as linux's `unknown_peer_dm_edited`
    // / apple's `unknownPeerDmTouched`); an untouched Save must not echo back a
    // rendered value and silently rewrite the ward's stored knob.
    var unknownPeerDmWire by remember(wardKey) { mutableStateOf(normalizeUnknownPeerDmWireForRender(ward.policy.unknownPeerDm)) }
    var unknownPeerDmTouched by remember(wardKey) { mutableStateOf(false) }
    // v1.x content pillar (family-safety.md § Content policy) — the four
    // per-category floors + the Notify toggle. An unset policy (`null`) seeds
    // every floor to `inherit` (the ward's own preferences decide); a stored
    // value android cannot parse normalizes fail-closed to `block` at load, the
    // same load-bearing rule as the reach knobs above.
    var contentNsfwWire by remember(wardKey) { mutableStateOf(normalizeContentFloorWire(ward.policy.contentPolicy?.nsfw ?: "inherit")) }
    var contentSpamWire by remember(wardKey) { mutableStateOf(normalizeContentFloorWire(ward.policy.contentPolicy?.spam ?: "inherit")) }
    var contentPhishingWire by remember(wardKey) { mutableStateOf(normalizeContentFloorWire(ward.policy.contentPolicy?.phishing ?: "inherit")) }
    var contentCommercialWire by remember(wardKey) { mutableStateOf(normalizeContentFloorWire(ward.policy.contentPolicy?.commercial ?: "inherit")) }
    var contentNotify by remember(wardKey) { mutableStateOf(ward.policy.contentNotify ?: false) }
    // Screen time (family-safety.md § Screen time, Slice E) — the usage
    // window's two `HH:MM` bounds and the daily budget in whole minutes.
    // Free-text rather than a numeric/time picker so an empty field means
    // "this control is unset", which is how a guardian clears a limit; the
    // three strings are parsed through shared Rust ([parseTimeOfDay] /
    // [parseDailyMinutes]) at Save time, never here — mirrors linux/web.
    val screenTimePolicy = ward.policy.screenTime
    var screenWindowStart by remember(wardKey) {
        mutableStateOf(screenTimePolicy?.windowStart?.let(::formatTimeOfDay) ?: "")
    }
    var screenWindowEnd by remember(wardKey) {
        mutableStateOf(screenTimePolicy?.windowEnd?.let(::formatTimeOfDay) ?: "")
    }
    var screenDailyMinutes by remember(wardKey) {
        mutableStateOf(screenTimePolicy?.dailyMinutes?.toString() ?: "")
    }

    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(stringResource(R.string.family_policy_contact_approval_label), modifier = Modifier.weight(1f))
            Switch(
                checked = contactApproval,
                onCheckedChange = { contactApproval = it },
                modifier = Modifier.testTag(Ids.FAMILY_POLICY_CONTACT_APPROVAL_TOGGLE),
            )
        }
        PolicySelectDropdown(
            testId = "family-policy-unknown-sender-select",
            labelRes = R.string.family_policy_unknown_sender_label,
            options = remember { unknownSenderOptions().map { it.value } },
            selectedWire = unknownSenderWire,
            labelFor = { localized(unknownSenderLabel(it)) ?: it },
            onSelectWire = { unknownSenderWire = it },
        )
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(stringResource(R.string.family_policy_federation_label), modifier = Modifier.weight(1f))
            Switch(
                checked = federationContact,
                onCheckedChange = { federationContact = it },
                modifier = Modifier.testTag(Ids.FAMILY_POLICY_FEDERATION_TOGGLE),
            )
        }
        PolicySelectDropdown(
            testId = "family-policy-feed-sources-select",
            labelRes = R.string.family_policy_feed_sources_label,
            options = remember { feedSourcesOptions().map { it.value } },
            selectedWire = feedSourcesWire,
            labelFor = { localized(feedSourcesLabel(it)) ?: it },
            onSelectWire = { feedSourcesWire = it },
        )
        // `feed_sources` only gates NEW connections; inbound DMs riding an
        // already-connected bridge account are the separate `unknown_peer_dm`
        // knob below (family-safety.md § The bridge-DM gate). The caption
        // names it by its own label.
        Text(
            stringResource(R.string.family_policy_feed_sources_caveat),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        PolicySelectDropdown(
            testId = "family-policy-unknown-peer-dm-select",
            labelRes = R.string.family_policy_unknown_peer_dm_label,
            options = remember { unknownPeerDmOptions().map { it.value } },
            selectedWire = unknownPeerDmWire,
            labelFor = { localized(unknownPeerDmLabel(it)) ?: it },
            onSelectWire = { unknownPeerDmWire = it; unknownPeerDmTouched = true },
        )
        // ── Content policy (v1.x pillar 2, family-safety.md § Content policy) ──
        // A per-category floor: inherit | collapse | block, over the same shared
        // `contentFloorOptions`/`contentFloorLabel` catalog the linux/web legs use
        // (priority #1/#2). Each select round-trips the wire value; the Notify
        // toggle carries category+count only, never content (§ Guardian Notify).
        PolicySelectDropdown(
            testId = "family-policy-content-nsfw-select",
            labelRes = R.string.family_policy_content_nsfw_label,
            options = remember { contentFloorOptions().map { it.value } },
            selectedWire = contentNsfwWire,
            labelFor = { localized(contentFloorLabel(it)) ?: it },
            onSelectWire = { contentNsfwWire = it },
        )
        PolicySelectDropdown(
            testId = "family-policy-content-spam-select",
            labelRes = R.string.family_policy_content_spam_label,
            options = remember { contentFloorOptions().map { it.value } },
            selectedWire = contentSpamWire,
            labelFor = { localized(contentFloorLabel(it)) ?: it },
            onSelectWire = { contentSpamWire = it },
        )
        PolicySelectDropdown(
            testId = "family-policy-content-phishing-select",
            labelRes = R.string.family_policy_content_phishing_label,
            options = remember { contentFloorOptions().map { it.value } },
            selectedWire = contentPhishingWire,
            labelFor = { localized(contentFloorLabel(it)) ?: it },
            onSelectWire = { contentPhishingWire = it },
        )
        PolicySelectDropdown(
            testId = "family-policy-content-commercial-select",
            labelRes = R.string.family_policy_content_commercial_label,
            options = remember { contentFloorOptions().map { it.value } },
            selectedWire = contentCommercialWire,
            labelFor = { localized(contentFloorLabel(it)) ?: it },
            onSelectWire = { contentCommercialWire = it },
        )
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(stringResource(R.string.family_policy_content_notify_label), modifier = Modifier.weight(1f))
            Switch(
                checked = contentNotify,
                onCheckedChange = { contentNotify = it },
                modifier = Modifier.testTag(Ids.FAMILY_POLICY_CONTENT_NOTIFY_TOGGLE),
            )
        }
        // ── Screen time (v1.x pillar 3, family-safety.md § Screen time) ──
        // The usage window is the hours the ward MAY use the account (so "no
        // device after 21:00" is the window 07:00-21:00) and may wrap
        // midnight; the daily budget is whole minutes, counted across every
        // device. Both are optional and independent; clearing a field removes
        // that control.
        Text(stringResource(R.string.family_policy_screen_heading), style = MaterialTheme.typography.titleSmall)
        OutlinedTextField(
            value = screenWindowStart,
            onValueChange = { screenWindowStart = it },
            singleLine = true,
            label = { Text(stringResource(R.string.family_policy_screen_window_start_label)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.FAMILY_POLICY_SCREEN_WINDOW_START_INPUT),
        )
        OutlinedTextField(
            value = screenWindowEnd,
            onValueChange = { screenWindowEnd = it },
            singleLine = true,
            label = { Text(stringResource(R.string.family_policy_screen_window_end_label)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.FAMILY_POLICY_SCREEN_WINDOW_END_INPUT),
        )
        OutlinedTextField(
            value = screenDailyMinutes,
            onValueChange = { screenDailyMinutes = it },
            singleLine = true,
            label = { Text(stringResource(R.string.family_policy_screen_daily_minutes_label)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.FAMILY_POLICY_SCREEN_DAILY_MINUTES_INPUT),
        )
        Text(
            stringResource(R.string.family_policy_screen_caveat),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Button(
            onClick = {
                // Fallible ONLY because of screen time: the three text inputs
                // are the one place a guardian can type something the policy
                // cannot hold. Both the per-field parse ([parseTimeOfDay] /
                // [parseDailyMinutes]) and the cross-field rules bounds-come-
                // in-pairs / start==end ambiguous) run through the SAME shared
                // code the nest runs at `policy.update`, so a save that would
                // be refused never leaves the client and the guardian sees why
                // locally — mirrors linux's `editor_policy`.
                val screenTime = try {
                    FfiScreenTimePolicy(
                        windowStart = parseTimeOfDay(screenWindowStart),
                        windowEnd = parseTimeOfDay(screenWindowEnd),
                        dailyMinutes = parseDailyMinutes(screenDailyMinutes),
                    )
                } catch (e: Exception) {
                    onSaveError(e.message ?: "invalid screen-time input")
                    return@Button
                }
                onSavePolicy(
                    ward.actorId,
                    FfiReachPolicy(
                        contactApproval = contactApproval,
                        unknownSenderMail = unknownSenderWire,
                        federationContact = federationContact,
                        feedSources = feedSourcesWire,
                        // v1.x content pillar (Slice C) — the editor now builds the
                        // four floors + Notify, so it sends them present (replace
                        // semantics, mirroring the linux/web reference which likewise
                        // dropped the prior "leave unchanged" null). An all-`inherit`
                        // policy is a valid no-op floor.
                        contentPolicy = FfiContentPolicy(
                            nsfw = contentNsfwWire,
                            spam = contentSpamWire,
                            phishing = contentPhishingWire,
                            commercial = contentCommercialWire,
                        ),
                        // v1.x screen-time pillar (Slice E) — the editor now
                        // builds it too, so it sends it present (replace
                        // semantics, same as content above). An all-empty
                        // editor sends the all-`null` default, which is how a
                        // guardian removes every limit.
                        screenTime = screenTime,
                        contentNotify = contentNotify,
                        // `unknown_peer_dm` stays `Option<String>` on the wire —
                        // absent means "leave unchanged" (family-safety.md §
                        // Policy-update compatibility) — so it rides out only
                        // when the guardian actually touched the select; an
                        // untouched editor must never silently rewrite the
                        // ward's knob back to whatever this render happened to
                        // show.
                        unknownPeerDm = if (unknownPeerDmTouched) unknownPeerDmWire else null,
                    ),
                )
            },
            modifier = Modifier.testTag(Ids.FAMILY_POLICY_SAVE_BUTTON),
        ) { Text(stringResource(R.string.family_policy_save_button)) }
    }
}

/**
 * The guardian-enrolled-device marker (family-safety.md § Full visibility for
 * young children, Slice F) — one `family-device-mark-item` row per device of
 * the SELECTED ward, lifting the linux/web reference. Lives
 * inside the per-ward editor, not on the ward rows, so with several wards on
 * the page the indexed IDs still address exactly one ward's devices
 * unambiguously.
 */
@Composable
private fun DeviceMarkSection(ward: FfiFamilyWardInfo, onMarkDevice: (ByteArray, String, Boolean) -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(R.string.family_ward_devices_heading), style = MaterialTheme.typography.titleSmall)
        Text(
            stringResourceFmt(R.string.family_ward_devices_hint, ward.handle),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (ward.devices.isEmpty()) {
            Text(
                stringResource(R.string.family_no_ward_devices),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            ward.devices.forEach { device ->
                DeviceMarkRow(
                    device = device,
                    // The device id, not a row index, carries the flip — a
                    // concurrent refetch that reorders the ward's devices must
                    // never route a mark at the wrong one (this flag is the
                    // security promise "the child cannot remove the guardian's
                    // device").
                    onToggle = { marked -> onMarkDevice(ward.actorId, device.deviceId, marked) },
                )
            }
        }
    }
}

/**
 * The un-deny surface (family-safety.md § The bridge-DM gate → *The un-deny
 * surface*): the SELECTED ward's denied bridge-DM peers
 * (`FfiFamilyWardInfo.blockedDmPeers`), one `family-blocked-peer-item` each with
 * its `family-blocked-peer-allow-button` INSIDE it. Inside the per-ward editor,
 * beside the device list, for the same reason: the indexed IDs then belong to
 * exactly one ward. Not batched behind Save — the flip is its own
 * `approvals.decide` call. The empty case is STATED, not a blank gap — a
 * guardian who denied nobody and a surface that failed to load would otherwise
 * look identical. tui: `family.rs`; linux: `fill_blocked_peers`.
 *
 * ⚠ Rule (f): each button carries an [UndenyDecide] of ITS OWN row's peer,
 * captured by value when the row composes — never an index into a list a
 * refetch could reorder. Pointing every button at row 0 would un-deny the
 * wrong person while the surface still looked correct.
 *
 * No offline-gate declaration: `fauna.family.approvals.decide` is
 * `OfflineQueued`, which the gate never greys.
 */
@Composable
internal fun BlockedPeersSection(ward: FfiFamilyWardInfo, onAllow: (UndenyDecide) -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(R.string.family_blocked_peers_heading), style = MaterialTheme.typography.titleSmall)
        Text(
            stringResourceFmt(R.string.family_blocked_peers_hint, ward.handle),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (ward.blockedDmPeers.isEmpty()) {
            Text(
                stringResource(R.string.family_no_blocked_peers),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            ward.blockedDmPeers.forEach { peer ->
                val decide = UndenyDecide(ward.actorId, peer)
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    modifier = Modifier.fillMaxWidth().testTag(Ids.FAMILY_BLOCKED_PEER_ITEM),
                ) {
                    // The row's own text IS the peer id — the only name this
                    // nest has for an external bridge peer.
                    Text(peer.peerId, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
                    OutlinedButton(
                        onClick = { onAllow(decide) },
                        modifier = Modifier.testTag(Ids.FAMILY_BLOCKED_PEER_ALLOW_BUTTON),
                    ) { Text(stringResource(R.string.family_blocked_peer_allow)) }
                }
            }
        }
    }
}

/**
 * One device row's widget tree — split out, like linux's `device_row_widgets`,
 * so the toggle is a `testTag` CHILD of its own `family-device-mark-item`
 * `Row` rather than a sibling. A flat paint returns nothing for either the
 * marked or the unmarked device on a scoped read — a false pass on the
 * negative assertion, which is exactly how tui's ward-side badge shipped
 * broken (fixed).
 */
@Composable
private fun DeviceMarkRow(device: FfiFamilyWardDevice, onToggle: (Boolean) -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        modifier = Modifier.fillMaxWidth().testTag(Ids.FAMILY_DEVICE_MARK_ITEM),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(device.label, style = MaterialTheme.typography.bodyMedium)
            // "Guardian device" — deliberately the same string the child reads
            // on their own device list (`devices_guardian_marked_badge`).
            Text(
                stringResource(R.string.family_device_mark_label),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Switch(
            checked = device.guardianMarked,
            onCheckedChange = onToggle,
            modifier = Modifier.testTag(Ids.FAMILY_DEVICE_MARK_TOGGLE),
        )
    }
}

/**
 * The wire value to seed the editor with, fail-closed via the shared
 * [unknownSenderLabel] resolver rather than a per-app fail-closed constant
 * (family-safety.md § Where logic lives): the catalog option whose label KEY
 * matches the resolved (already-fail-closed) label IS the fail-closed wire
 * value, so this needs no local knowledge of which option that is — a newer
 * nest adding a value android can't parse degrades the same way a garbage
 * string would.
 */
private fun normalizeUnknownSenderWire(wire: String): String {
    val resolvedKey = unknownSenderLabel(wire).key
    return unknownSenderOptions().firstOrNull { it.label.key == resolvedKey }?.value ?: wire
}

/** See [normalizeUnknownSenderWire]. */
private fun normalizeFeedSourcesWire(wire: String): String {
    val resolvedKey = feedSourcesLabel(wire).key
    return feedSourcesOptions().firstOrNull { it.label.key == resolvedKey }?.value ?: wire
}

/**
 * The mirror image of [normalizeUnknownSenderWire]/[normalizeFeedSourcesWire]:
 * an ABSENT `unknown_peer_dm` renders the `allow` DEFAULT, never the
 * fail-closed `hold` (family-safety.md § The bridge-DM gate — the nest omits
 * a knob sitting at its default, so absence here means "already allow", not
 * "a value this client could not parse"; pinned by `fauna_core::format`'s
 * `an_absent_unknown_peer_dm_renders_its_allow_default_not_the_fail_closed_value`).
 * A PRESENT value still normalizes fail-closed like every other knob.
 */
private fun normalizeUnknownPeerDmWireForRender(wire: String?): String {
    val resolvedKey = unknownPeerDmLabel(wire ?: "allow").key
    return unknownPeerDmOptions().firstOrNull { it.label.key == resolvedKey }?.value ?: (wire ?: "allow")
}

/** See [normalizeUnknownSenderWire]; a content floor fails closed to `block`
 *  (never the permissive `inherit`) via the shared [contentFloorLabel] resolver. */
private fun normalizeContentFloorWire(wire: String): String {
    val resolvedKey = contentFloorLabel(wire).key
    return contentFloorOptions().firstOrNull { it.label.key == resolvedKey }?.value ?: wire
}

/** The four knobs as "{label}: {value}" lines, over the shared
 *  `fauna_core::format::reach_policy_summary` (family-safety.md § Where logic
 *  lives, replacing the former hand-rolled boolean/label join) — the
 *  supervised side's read-only transparency view (family-safety.md § Client
 *  surface). [usageTodayMinutes] folds the screen-time readout into this same
 *  summary rather than claiming a new ui.yaml ID — the supervised section's
 *  element set is `family-guardian-handle` + `family-policy-summary`, and the
 *  ward's usage *is* a line of "the active policy, read-only", over the SAME
 *  shared [usageTodayLine] call the guardian's own per-ward readout makes
 *  ([formatWardUsageToday]), so the two surfaces cannot show different
 *  figures (family-safety.md § Screen time — the ward's summary shows the
 *  same number). `null` renders nothing at all: no budget, no accounting. */
@Composable
private fun formatPolicySummary(policy: FfiReachPolicy, usageTodayMinutes: UInt?): String {
    // `joinToString`'s nullable `transform` param isn't treated as
    // composable-safe by the Compose compiler even though the function is
    // `inline` — resolve each line via `.map` (a non-nullable, always-inlined
    // transform) first, then join the plain strings.
    val lines = reachPolicySummary(policy).map { line -> "${localized(line.label)}: ${localized(line.value)}" }.toMutableList()
    if (usageTodayMinutes != null) {
        val line = usageTodayLine(usageTodayMinutes, policy.screenTime?.dailyMinutes)
        lines.add("${localized(line.label)}: ${localized(line.value)}")
    }
    return lines.joinToString("\n")
}

/**
 * A cross-app label-select dropdown (ui.yaml types `family-policy-*-select`
 * as `select`): the anchor's display value is the localized *label*, not the
 * wire value — the e2e `actions/family.py` `set_unknown_sender`/
 * `set_feed_sources` drive `driver.select(id, label)` and
 * `unknown_sender_label()`/`feed_sources_label()` read the displayed label
 * back, mirroring `AdminUsersScreen`'s `TierDropdown`. The VM/save path still
 * round-trips the wire value.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PolicySelectDropdown(
    testId: String,
    labelRes: Int,
    options: List<String>,
    selectedWire: String,
    labelFor: @Composable (String) -> String,
    onSelectWire: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = !expanded }) {
        OutlinedTextField(
            value = labelFor(selectedWire),
            onValueChange = {},
            readOnly = true,
            label = { Text(stringResource(labelRes)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier.menuAnchor().fillMaxWidth().testTag(testId),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { wire ->
                DropdownMenuItem(
                    text = { Text(labelFor(wire)) },
                    onClick = { expanded = false; onSelectWire(wire) },
                )
            }
        }
    }
}

@Composable
private fun ContactAddRow(supervisedActorId: ByteArray, onAddContact: (ByteArray, ByteArray) -> Unit) {
    var input by remember { mutableStateOf("") }
    var invalid by remember { mutableStateOf(false) }
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedTextField(
            value = input,
            onValueChange = { input = it; invalid = false },
            singleLine = true,
            label = { Text(stringResource(R.string.family_contact_add_placeholder)) },
            isError = invalid,
            modifier = Modifier.weight(1f).testTag(Ids.FAMILY_CONTACT_ADD_INPUT),
        )
        Button(
            onClick = {
                val bytes = runCatching { HexUtil.hexToBytes(input) }.getOrNull()
                if (bytes == null) {
                    invalid = true
                } else {
                    onAddContact(supervisedActorId, bytes)
                    input = ""
                }
            },
            modifier = Modifier.testTag(Ids.FAMILY_CONTACT_ADD_BUTTON),
        ) { Text(stringResource(R.string.family_contact_add_button)) }
    }
    if (invalid) {
        Text(
            stringResource(R.string.family_contact_add_invalid_actor_id),
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodySmall,
        )
    }
}

/**
 * Transfer initiation per selected ward, swapping for the nest-confirmed
 * pending marker + cancel while a proposal is outstanding (family-safety.md
 * § Graduation & transfer) — same hex-actor-id input convention as
 * [ContactAddRow].
 */
@Composable
private fun TransferSection(
    ward: FfiFamilyWardInfo,
    onProposeTransfer: (ByteArray, ByteArray) -> Unit,
    onCancelTransfer: (ByteArray) -> Unit,
) {
    val pending = ward.pendingTransfer
    if (pending != null) {
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                stringResourceFmt(R.string.family_transfer_pending, pending.proposedGuardianHandle),
                modifier = Modifier.testTag(Ids.FAMILY_TRANSFER_PENDING),
                style = MaterialTheme.typography.bodyMedium,
            )
            // Also a commit, not a local dismissal: withdrawing a pending
            // proposal has to reach the nest (`fauna.family.transfer.cancel`).
            val cancelGate = faunaGate("fauna.family.transfer.cancel")
            OutlinedButton(
                onClick = { onCancelTransfer(ward.actorId) },
                enabled = cancelGate.enabled,
                modifier = Modifier.testTag(Ids.FAMILY_TRANSFER_CANCEL_BUTTON),
            ) { Text(stringResource(R.string.family_transfer_cancel_button)) }
            DisabledControlReasonText(cancelGate.reason)
        }
    } else {
        var input by remember(HexUtil.bytesToHex(ward.actorId)) { mutableStateOf("") }
        var invalid by remember { mutableStateOf(false) }
        // The commit gates, not the buffer: the guardian's hex id can be typed
        // and its validity flagged with no nest — only the proposal itself
        // needs one. This control carries NO predicate of its own (validity is
        // checked in the click handler, which is why a bad hex sets `invalid`
        // rather than grewing the button), so the verdict is its only author.
        val proposeGate = faunaGate("fauna.family.transfer")
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(
                value = input,
                onValueChange = { input = it; invalid = false },
                singleLine = true,
                label = { Text(stringResource(R.string.family_transfer_placeholder)) },
                isError = invalid,
                modifier = Modifier.weight(1f).testTag(Ids.FAMILY_TRANSFER_INPUT),
            )
            Button(
                onClick = {
                    val bytes = runCatching { HexUtil.hexToBytes(input) }.getOrNull()
                    if (bytes == null) invalid = true else onProposeTransfer(ward.actorId, bytes)
                },
                enabled = proposeGate.enabled,
                modifier = Modifier.testTag(Ids.FAMILY_TRANSFER_BUTTON),
            ) { Text(stringResource(R.string.family_transfer_button)) }
        }
        DisabledControlReasonText(proposeGate.reason)
    }
}

/** Begin-graduation reveal → `AlertDialog` confirm, the same idiom as
 *  `MailSettingsScreen`'s Disable-mail confirm (family-safety.md § Graduation
 *  & transfer: graduation is an in-place conversion, no data migration). */
@Composable
private fun GraduateSection(ward: FfiFamilyWardInfo, onGraduate: (ByteArray) -> Unit) {
    var showConfirm by remember(HexUtil.bytesToHex(ward.actorId)) { mutableStateOf(false) }
    Button(
        onClick = { showConfirm = true },
        modifier = Modifier.testTag(Ids.FAMILY_GRADUATE_BUTTON),
    ) { Text(stringResource(R.string.family_graduate_button)) }

    if (showConfirm) {
        // Arming is local: `family-graduate-button` above only reveals this
        // dialog and stays live; the confirm is what converts the ward in place
        // (`fauna.family.graduate`).
        val graduateGate = faunaGate("fauna.family.graduate")
        AlertDialog(
            onDismissRequest = { showConfirm = false },
            title = { Text(stringResource(R.string.family_graduate_button)) },
            text = { Text(stringResourceFmt(R.string.family_graduate_confirm_button, ward.handle)) },
            confirmButton = {
                Column {
                    TextButton(
                        onClick = { showConfirm = false; onGraduate(ward.actorId) },
                        enabled = graduateGate.enabled,
                        modifier = Modifier.testTag(Ids.FAMILY_GRADUATE_CONFIRM_BUTTON),
                    ) { Text(stringResourceFmt(R.string.family_graduate_confirm_button, ward.handle)) }
                    DisabledControlReasonText(graduateGate.reason)
                }
            },
            dismissButton = {
                TextButton(onClick = { showConfirm = false }) { Text(stringResource(R.string.common_cancel)) }
            },
        )
    }
}

@Composable
private fun ApprovalRow(entry: FfiFamilyApprovalEntry, onDecide: (FfiFamilyApprovalEntry, Boolean) -> Unit) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.FAMILY_APPROVAL_ITEM)) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                approvalDisplayText(entry) ?: stringResource(R.string.family_approval_no_sender),
                style = MaterialTheme.typography.bodyMedium,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { onDecide(entry, true) },
                    modifier = Modifier.testTag(Ids.FAMILY_APPROVAL_APPROVE_BUTTON),
                ) { Text(stringResource(R.string.family_approve)) }
                OutlinedButton(
                    onClick = { onDecide(entry, false) },
                    modifier = Modifier.testTag(Ids.FAMILY_APPROVAL_DENY_BUTTON),
                ) { Text(stringResource(R.string.family_deny)) }
            }
        }
    }
}

// The approval row's display rule (family-safety.md § Don't surface content) now lives in
// shared Rust as `com.fauna.ffi.approvalDisplayText` — it returns `peer_address` for a
// non-empty `mail_hold`, `summary` for every other kind, and `null` for a held null-path
// `mail_hold` (whose `peer_address` is deliberately empty), which `ApprovalRow` above
// resolves to the localized no-sender label.
