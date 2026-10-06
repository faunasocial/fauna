package com.fauna.app.ui.screen.settings

import android.content.Context
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.viewmodel.LinkedNestsVM
import java.text.DateFormat
import java.util.Date
import uniffi.fauna_client_pair.ForwardQueueStatus
import uniffi.fauna_client_pair.LinkedNestRow
import uniffi.fauna_client_pair.LinkedNestStatus
import uniffi.fauna_client_pair.LinkedNestsSnapshot
import uniffi.fauna_client_pair.TrustBackupKind
import uniffi.fauna_client_pair.TrustBackupRow
import uniffi.fauna_client_pair.TrustEventKind
import uniffi.fauna_client_pair.TrustFolder
import uniffi.fauna_client_pair.TrustGenerationRow
import uniffi.fauna_client_pair.TrustGenerationStatus
import uniffi.fauna_client_pair.TrustGrantDuration
import uniffi.fauna_client_pair.TrustGrantRow
import uniffi.fauna_client_pair.TrustHistoryRow
import uniffi.fauna_client_pair.TrustLens
import uniffi.fauna_client_pair.TrustMintOption
import uniffi.fauna_client_pair.TrustRestoreOutcome
import uniffi.fauna_client_pair.TrustScope
import uniffi.fauna_client_pair.backupStatusLabel
import uniffi.fauna_client_pair.grantScopeLabels
import uniffi.fauna_client_pair.mintOptionLabel
import uniffi.fauna_client_pair.statusLabel
import social.fauna.generated.Ids

/**
 * The user-facing "Nests" page — per-user nest pairing (multi-homing) + the v1
 * **nest-trust** facet (renamed from "Linked nests" 2026-07-08, docs/goal/ui/nests.md).
 *
 * A **user** surface (not the admin shell): the user links one of their own
 * nests to sync this account's content, lists the nests their content lives on
 * (home nest first, then pairings), unlinks a pairing, and — on each nest row —
 * sees a **trust facet**: what content-processing that nest has been *trusted
 * to read* (the grants the client minted to it), with a per-row **Now/History**
 * lens over the client-authoritative signed grant-event log, per-grant
 * renew/revoke, and the required honest bound of revocation. The admin's only
 * pairing control is the admin `admin-service-pairing-toggle` (admin nest
 * page). Target state: docs/goal/ui/nests.md (page UX + trust facet) +
 * docs/goal/behavior/linked-nests.md (the linking half, unchanged); the Linux
 * lead is apps/fauna-linux/src/settings/linked_nests.rs.
 *
 * Stateless [LinkedNestsContent] is split out for the Robolectric test harness
 * (mirrors `AdminNestScreen`/`AdminNestContent`); the VM-bound
 * [LinkedNestsScreen] is the wrapper the NavHost mounts. Per priority #2 this
 * shell holds no pairing/trust logic — it renders the [LinkedNestsVM]'s
 * snapshot and dispatches Link/Unlink/SetLens/Renew/Revoke/Mint plus the two
 * backup revokes and a generation restore; all sequencing lives in the shared
 * `LinkedNestsMachine` over UniFFI. The **home** row additionally carries the
 * backup trust rows (`nest-trust-backup-item`, `nests.md` § Trust facet —
 * backup rows, ratified 2026-07-24; linux + web are the landed references) —
 * the seal grant and one writer row per destination, rendered in the Now lens
 * after the content-grant rows. Their revokes land on *different* nests,
 * which is the whole point of the row: see [BackupItem]. Right after those,
 * the home row carries the retained-generation rows
 * (`nest-trust-generation-item`, `nests.md` § Trust facet — generation
 * recovery, ratified 2026-07-29; linux + tui are the landed references) — what
 * the owner can roll back to at a backup destination once they have revoked a
 * rogue source's writer grant: see [GenerationItem] for the three ratified
 * honesty invariants a shell must get right. The mint flow (scope-first
 * picker, `nests.md`
 * § Mint, ratified 2026-07-13; web + linux are the landed references) renders
 * `LinkedNestRow.mintOptions` (the FFI row carries the shared
 * `view_model::mint_options` catalog), maps each to its localized
 * `nests_mint_option_*` label, derives the holder from the scope choice, and
 * dispatches `Mint` — no mint/holder logic in the shell (priority
 * #1/#2). The page's `page-heading` comes from this
 * screen's own TopAppBar title and its `error-message` from the global
 * MessageBanner (the snapshot error is routed there via [LocalAppMessages]).
 * Nav route `settings/nests` (the shared cross-app nav id), matching the
 * testids, i18n and page label.
 */
@Composable
fun LinkedNestsScreen(
    navController: NavController,
    vm: LinkedNestsVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val custodyNestRows by vm.custodyNestRows.collectAsState()
    val escrowHolders by vm.escrowHolders.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = androidx.compose.ui.platform.LocalContext.current

    // Surface the snapshot error through the global error-message banner.
    LaunchedEffect(errorMessage) {
        errorMessage?.let { appMessages.showError(it) }
    }

    LinkedNestsContent(
        snapshot = snapshot,
        onBack = { navController.popBackStack() },
        onLink = vm::link,
        onUnlink = vm::unlink,
        onSetLens = vm::setLens,
        onRenew = vm::renew,
        onRevoke = vm::revoke,
        onMint = vm::mint,
        onSetBlessed = vm::setBlessed,
        onRevokeBackupSeal = vm::revokeBackupSeal,
        onRevokeBackupWriter = vm::revokeBackupWriter,
        onRestoreGeneration = vm::restoreGeneration,
        onRetryForwards = vm::retryForwards,
        onDiscardForwards = vm::discardForwards,
        custodyNestRows = custodyNestRows,
        escrowHolders = escrowHolders,
        onRevokeCustody = vm::revokeCustody,
        receiptStatusLine = { com.fauna.app.ui.util.custodyReceiptStatusText(context, it) },
        heldBytesLine = { com.fauna.app.ui.util.custodyHeldBytesText(context, it) },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LinkedNestsContent(
    snapshot: LinkedNestsSnapshot,
    onBack: () -> Unit,
    onLink: (String) -> Unit,
    onUnlink: (String) -> Unit,
    onSetLens: (String, TrustLens) -> Unit,
    onRenew: (ByteArray) -> Unit,
    onRevoke: (ByteArray) -> Unit,
    // Mint a scope-first trust grant to one of the nest's content-processor
    // holders: (nestId, holderBridgeId, scope). The holder is derived shell-side
    // from the scope choice (nests.md § Mint).
    onMint: (String, String, List<TrustScope>, TrustGrantDuration) -> Unit,
    // The two backup-trust revokes (`nest-trust-backup-revoke`). Kept as separate
    // callbacks — one per shared `LinkedNestsAction` variant — because they land
    // on DIFFERENT nests: the seal press freezes the home nest's ability to seal
    // new backups (revoked at the source), while a writer press withdraws its
    // authorization at ONE destination, spoken over that destination's own
    // connection. This layer only names which row was pressed; the shared machine
    // routes each to the right nest (`nests.md` § Trust facet — backup rows).
    onRevokeBackupSeal: () -> Unit,
    onRevokeBackupWriter: (String) -> Unit,
    // Roll one retained backup generation back to live
    // (`nest-trust-generation-restore`, `nests.md` § Trust facet — generation
    // recovery): (destinationId, folderName, pathHash, manifestHash) — the
    // row's own address triple, never a row index. The flattened
    // `trustGenerations` list spans destinations, so an index-addressed
    // restore would promote the wrong generation the moment this list is
    // filtered or re-ordered.
    onRestoreGeneration: (String, String, String, String) -> Unit,
    // The page-level forward queue's two actions (`nests-forward-retry-button` /
    // `nests-forward-discard-button`, `nests.md` § Forward queue) — the shared
    // machine's `RetryForwards` / `DiscardForwards`, each re-listing after.
    onRetryForwards: () -> Unit = {},
    onDiscardForwards: () -> Unit = {},
    // The per-nest blessing (`nest-trust-blessed-toggle`): (nestId, blessed).
    onSetBlessed: (String, Boolean) -> Unit = { _, _ -> },
    // The shared duration catalog + its labels over UniFFI — injected so the
    // Content stays FFI-free for the Robolectric harness.
    durationOptions: () -> List<TrustGrantDuration> = { uniffi.fauna_client_pair.mintDurationOptions() },
    durationLabel: (TrustGrantDuration) -> uniffi.fauna_core.LocalizedText =
        { uniffi.fauna_client_pair.durationLabel(it) },
    // The shared `fauna_core::format::short_id` over UniFFI — injected so the
    // Content stays FFI-free for the Robolectric harness (mirrors
    // RestoreHistoryContent's `hexShort`).
    shortId: (String) -> String = { com.fauna.ffi.shortId(it) },
    // Custodian nests (`nests.md` § Trust facet — custody rows): the
    // NEST-anchored custody rows, each its own `nests-item` after the linked
    // rows (the tui order). The two receipt lines are injected like the Devices
    // page's, so the Content stays FFI-free for the Robolectric harness.
    custodyNestRows: List<uniffi.fauna_client_capabilities.CustodyHolderRowView> = emptyList(),
    // Hex identities holding the account's escrow — the escrow-holder badge
    // (`participants.md` § The participant model → Roles) renders on the nest
    // row whose `nestId` is among them.
    escrowHolders: Set<String> = emptySet(),
    onRevokeCustody: (ByteArray, ByteArray?) -> Unit = { _, _ -> },
    receiptStatusLine: (uniffi.fauna_client_capabilities.CustodyReceiptRowView) -> String = { "" },
    heldBytesLine: (uniffi.fauna_client_capabilities.CustodyReceiptRowView) -> String = { "" },
) {
    var showForm by remember { mutableStateOf(false) }
    // The link-form value: a nest address (URL) → both-ends LinkBoth, or a 64-hex
    // identity → single-end Link. The shared classifier routes it (see the VM).
    var addInput by remember { mutableStateOf("") }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.nests_title),
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
        LazyColumn(
            modifier = Modifier.padding(padding).fillMaxSize(),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            // Heading blurb + the add affordance.
            item {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        stringResource(R.string.nests_description),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Button(
                        onClick = {
                            addInput = ""
                            showForm = true
                        },
                        enabled = snapshot.status != LinkedNestStatus.WORKING,
                        modifier = Modifier.testTag(Ids.NESTS_ADD_BUTTON),
                    ) {
                        Text(stringResource(R.string.nests_add_button))
                    }
                }
            }

            // Inline add form (revealed by the add button; not a modal).
            if (showForm) {
                item {
                    Card(modifier = Modifier.fillMaxWidth()) {
                        Column(
                            modifier = Modifier.padding(16.dp),
                            verticalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            Text(
                                stringResource(R.string.nests_add_button),
                                style = MaterialTheme.typography.titleMedium,
                            )
                            OutlinedTextField(
                                value = addInput,
                                onValueChange = { addInput = it },
                                label = { Text(stringResource(R.string.nests_add_input_placeholder)) },
                                singleLine = true,
                                modifier = Modifier.fillMaxWidth().testTag(Ids.NESTS_ADD_INPUT),
                            )
                            // The commit gates, not the buffer: the input above and
                            // the cancel beside it stay live with no nest, and the
                            // form's own non-empty predicate is handed over rather
                            // than re-tested next to the verdict.
                            val addGate = faunaGate(
                                "fauna.pair.add",
                                enabled = addInput.isNotBlank(),
                            )
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                Button(
                                    onClick = {
                                        onLink(addInput)
                                        showForm = false
                                    },
                                    enabled = addGate.enabled,
                                    modifier = Modifier.testTag(Ids.NESTS_ADD_SUBMIT_BUTTON),
                                ) { Text(stringResource(R.string.nests_add_submit)) }
                                OutlinedButton(
                                    onClick = { showForm = false },
                                    modifier = Modifier.testTag(Ids.NESTS_ADD_CANCEL_BUTTON),
                                ) { Text(stringResource(R.string.nests_add_cancel)) }
                            }
                            DisabledControlReasonText(addGate.reason)
                        }
                    }
                }
            }

            // Forward queue (page-level, conditional — nests.md § Forward queue):
            // after the add form, before the nest rows, only while the connected
            // nest reports posts of the user's still waiting to reach its relay.
            val queue = snapshot.forwardQueue?.takeIf { it.queued > 0uL }
            if (queue != null) {
                item {
                    ForwardQueueBlock(
                        queue = queue,
                        onRetry = onRetryForwards,
                        onDiscard = onDiscardForwards,
                    )
                }
            }

            // The nest list: home row first (nests.md § Layout), then pairings.
            val rows = listOfNotNull(snapshot.home) + snapshot.pairings
            if (rows.isEmpty() && custodyNestRows.isEmpty()) {
                item {
                    Text(
                        stringResource(R.string.nests_empty),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            } else {
                items(rows) { row ->
                    NestItemRow(
                        row = row,
                        shortId = shortId,
                        holdsEscrow = row.nestId.lowercase() in escrowHolders,
                        // Recovery is addressed to the OWNER's own backup
                        // destinations, so the notice is home-row-scoped —
                        // never rendered on a pairing row (`nests.md` §
                        // Trust facet — generation recovery).
                        restoreOutcome = if (row.isHome) snapshot.restoreOutcome else null,
                        onUnlink = { onUnlink(row.nestId) },
                        onSetLens = { lens -> onSetLens(row.nestId, lens) },
                        onRenew = onRenew,
                        onRevoke = onRevoke,
                        onMint = { holder, scope, duration -> onMint(row.nestId, holder, scope, duration) },
                        onSetBlessed = { blessed -> onSetBlessed(row.nestId, blessed) },
                        durationOptions = durationOptions,
                        durationLabel = durationLabel,
                        onRevokeBackupSeal = onRevokeBackupSeal,
                        onRevokeBackupWriter = onRevokeBackupWriter,
                        onRestoreGeneration = onRestoreGeneration,
                    )
                }
                items(custodyNestRows) { custody ->
                    CustodyNestItemRow(
                        row = custody,
                        shortId = shortId,
                        onRevokeCustody = onRevokeCustody,
                        receiptStatusLine = receiptStatusLine,
                        heldBytesLine = heldBytesLine,
                    )
                }
            }
        }
    }
}

/**
 * One custodian-NEST `nests-item` + its `nest-trust-custody-*` family — tui's
 * `push_custody_nest_item`, field for field. The copy is the shared
 * `devices.custody_*` strings the Devices `custody-holder-card` paints (receipt
 * three-state honesty, held bytes, revoke beside its REQUIRED honest-bound
 * note); the scope line is trust vocabulary only.
 */
@Composable
private fun CustodyNestItemRow(
    row: uniffi.fauna_client_capabilities.CustodyHolderRowView,
    shortId: (String) -> String,
    onRevokeCustody: (ByteArray, ByteArray?) -> Unit,
    receiptStatusLine: (uniffi.fauna_client_capabilities.CustodyReceiptRowView) -> String,
    heldBytesLine: (uniffi.fauna_client_capabilities.CustodyReceiptRowView) -> String,
) {
    val host = shortId(com.fauna.app.core.HexUtil.bytesToHex(row.host))
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.NESTS_ITEM)) {
        Column(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.nests_custody_nest_label).replace("{host}", host),
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.NESTS_ITEM_LABEL),
            )
            Column(
                modifier = Modifier.testTag(Ids.NEST_TRUST_CUSTODY_ITEM),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    stringResource(R.string.devices_custody_holder_scope),
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.NEST_TRUST_CUSTODY_SCOPE),
                )
                // Three states, three strings — never collapsed, never empty.
                Text(
                    receiptStatusLine(row.receipt),
                    style = MaterialTheme.typography.bodySmall,
                    color = when (row.receiptState) {
                        uniffi.fauna_client_capabilities.CustodyReceiptStateView.STALE ->
                            MaterialTheme.colorScheme.error
                        else -> MaterialTheme.colorScheme.onSurfaceVariant
                    },
                    modifier = Modifier.testTag(Ids.NEST_TRUST_CUSTODY_RECEIPT_STATUS),
                )
                Text(
                    heldBytesLine(row.receipt),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.NEST_TRUST_CUSTODY_HELD_BYTES),
                )
                Text(
                    stringResource(R.string.devices_custody_revoke_bound_note),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                val revokeGate = faunaGate("fauna.capabilities.revoke", enabled = !row.pending)
                OutlinedButton(
                    onClick = { onRevokeCustody(row.grantId, row.custodianKey) },
                    // A pending ceremony has minted nothing to revoke yet.
                    enabled = revokeGate.enabled,
                    colors = ButtonDefaults.outlinedButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                    modifier = Modifier.testTag(Ids.NEST_TRUST_CUSTODY_REVOKE_BUTTON),
                ) {
                    Text(stringResource(R.string.devices_custody_revoke))
                }
                DisabledControlReasonText(revokeGate.reason)
            }
        }
    }
}

/**
 * The page-level forward-queue block (`nests-forward-*`): the count, the stuck
 * half's what-to-check when any entry is past the retry ceiling, the nest's own
 * latest failure when one was recorded, and the two actions — mirroring tui's
 * `push_forward_queue` field for field.
 *
 * The reason is partly relay-chosen text (`private-mode.md` § Post
 * Forwarding): the shared projection already control-stripped it, and it is
 * painted through a plain [Text] of a `String` — never an `AnnotatedString`,
 * HTML or link-detecting view — so `<b>` stays literal.
 */
@Composable
private fun ForwardQueueBlock(
    queue: ForwardQueueStatus,
    onRetry: () -> Unit,
    onDiscard: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            var summary = stringResourceFmt(R.string.nests_forward_queue_summary, queue.queued.toString())
            if (queue.stuck > 0uL) {
                summary += " " + stringResourceFmt(R.string.nests_forward_queue_stuck, queue.stuck.toString())
            }
            Text(
                summary,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.testTag(Ids.NESTS_FORWARD_QUEUE),
            )
            val error = queue.lastError?.takeIf { it.isNotEmpty() }
            if (error != null) {
                Text(
                    stringResourceFmt(R.string.nests_forward_queue_last_error, error),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.NESTS_FORWARD_QUEUE_REASON),
                )
            }
            val retryGate = faunaGate("fauna.pair.forward_retry")
            val discardGate = faunaGate("fauna.pair.forward_discard")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(
                    onClick = onRetry,
                    enabled = retryGate.enabled,
                    modifier = Modifier.testTag(Ids.NESTS_FORWARD_RETRY_BUTTON),
                ) { Text(stringResource(R.string.nests_forward_retry)) }
                OutlinedButton(
                    onClick = onDiscard,
                    enabled = discardGate.enabled,
                    modifier = Modifier.testTag(Ids.NESTS_FORWARD_DISCARD_BUTTON),
                ) { Text(stringResource(R.string.nests_forward_discard)) }
            }
            DisabledControlReasonText(retryGate.reason)
        }
    }
}

/**
 * One `nests-item` row, projected from a [LinkedNestRow]: the identity line,
 * then the trust facet. The home row (`isHome`) carries no Unlink and no
 * sync-caps/expiry lines — it is the user's own connected nest, not a pairing
 * (`nests.md` § Layout).
 */
@Composable
private fun NestItemRow(
    row: LinkedNestRow,
    shortId: (String) -> String,
    holdsEscrow: Boolean,
    restoreOutcome: TrustRestoreOutcome?,
    onUnlink: () -> Unit,
    onSetLens: (TrustLens) -> Unit,
    onRenew: (ByteArray) -> Unit,
    onRevoke: (ByteArray) -> Unit,
    onMint: (String, List<TrustScope>, TrustGrantDuration) -> Unit,
    onRevokeBackupSeal: () -> Unit,
    onRevokeBackupWriter: (String) -> Unit,
    onRestoreGeneration: (String, String, String, String) -> Unit,
    onSetBlessed: (Boolean) -> Unit,
    durationOptions: () -> List<TrustGrantDuration>,
    durationLabel: (TrustGrantDuration) -> uniffi.fauna_core.LocalizedText,
) {
    val abbreviated = shortId(row.nestId)
    val labelText = row.label?.takeIf { it.isNotEmpty() } ?: abbreviated

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.NESTS_ITEM)) {
        Column(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                labelText,
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.NESTS_ITEM_LABEL),
            )
            Text(
                abbreviated,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.NESTS_ITEM_NEST_ID),
            )
            // The escrow-holder role badge: this nest holds the account's
            // generation-key escrow — derived from recorded escrow receipts,
            // never asserted by the nest itself.
            if (holdsEscrow) {
                Text(
                    stringResource(R.string.nests_escrow_holder_badge),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.testTag(Ids.PARTICIPANT_ESCROW_HOLDER_BADGE),
                )
            }

            if (!row.isHome) {
                val context = LocalContext.current
                val caps = row.capabilityLabels.joinToString(", ") {
                    resolveLocalized(context, it) ?: it.key
                }
                Text(
                    caps,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.NESTS_ITEM_CAPABILITIES),
                )
                val expiry =
                    if (row.expiresAt != null) stringResource(R.string.nests_expiry_label)
                    else stringResource(R.string.nests_expiry_never)
                Text(
                    expiry,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.NESTS_ITEM_EXPIRY),
                )
                val unlinkGate = faunaGate("fauna.pair.revoke")
                OutlinedButton(
                    onClick = onUnlink,
                    enabled = unlinkGate.enabled,
                    colors = ButtonDefaults.outlinedButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                    modifier = Modifier.testTag(Ids.NESTS_ITEM_UNLINK_BUTTON),
                ) {
                    Text(stringResource(R.string.nests_unlink))
                }
                DisabledControlReasonText(unlinkGate.reason)
            }

            TrustFacet(
                row = row,
                restoreOutcome = restoreOutcome,
                onSetLens = onSetLens,
                onRenew = onRenew,
                onRevoke = onRevoke,
                onMint = onMint,
                onRevokeBackupSeal = onRevokeBackupSeal,
                onRevokeBackupWriter = onRevokeBackupWriter,
                onRestoreGeneration = onRestoreGeneration,
                onSetBlessed = onSetBlessed,
                durationOptions = durationOptions,
                durationLabel = durationLabel,
            )
        }
    }
}

/**
 * The trust facet for one nest row: the always-present Now/History lens
 * toggle, then the active lens's content — the grant list (Now, with a
 * `nest-trust-empty` state when the nest holds none) or the grant-event
 * timeline (History). `SetLens` flips `row.lens` locally (no nest round-trip);
 * the recomposition re-renders the active lens (`nests.md` § Trust facet).
 */
@Composable
private fun TrustFacet(
    row: LinkedNestRow,
    restoreOutcome: TrustRestoreOutcome?,
    onSetLens: (TrustLens) -> Unit,
    onRenew: (ByteArray) -> Unit,
    onRevoke: (ByteArray) -> Unit,
    onMint: (String, List<TrustScope>, TrustGrantDuration) -> Unit,
    onRevokeBackupSeal: () -> Unit,
    onRevokeBackupWriter: (String) -> Unit,
    onRestoreGeneration: (String, String, String, String) -> Unit,
    onSetBlessed: (Boolean) -> Unit,
    durationOptions: () -> List<TrustGrantDuration>,
    durationLabel: (TrustGrantDuration) -> uniffi.fauna_core.LocalizedText,
) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            val nowSelected = row.lens == TrustLens.NOW
            Button(
                onClick = { onSetLens(TrustLens.NOW) },
                colors = if (nowSelected) ButtonDefaults.buttonColors()
                else ButtonDefaults.outlinedButtonColors(),
                modifier = Modifier.testTag(Ids.NEST_TRUST_VIEW_NOW),
            ) { Text(stringResource(R.string.nests_view_now)) }
            Button(
                onClick = { onSetLens(TrustLens.HISTORY) },
                colors = if (!nowSelected) ButtonDefaults.buttonColors()
                else ButtonDefaults.outlinedButtonColors(),
                modifier = Modifier.testTag(Ids.NEST_TRUST_VIEW_HISTORY),
            ) { Text(stringResource(R.string.nests_view_history)) }
        }

        when (row.lens) {
            TrustLens.NOW -> {
                // `nest-trust-empty` claims the nest is trusted with NOTHING, so a
                // backup row suppresses it even with zero content grants
                // (`nests.md:99`) — a nest that seals and uploads your messages is
                // plainly trusted, and rendering "not trusted to read anything"
                // directly above "Backs up your messages for you" states the
                // opposite of the row beneath it.
                if (row.trustGrants.isEmpty() && row.trustBackups.isEmpty()) {
                    Text(
                        stringResource(R.string.nests_not_trusted),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.testTag(Ids.NEST_TRUST_EMPTY),
                    )
                } else if (row.trustGrants.isNotEmpty()) {
                    Column(
                        modifier = Modifier.testTag(Ids.NEST_TRUST_GRANT_LIST),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        row.trustGrants.forEach { grant ->
                            GrantItem(
                                grant = grant,
                                onRenew = onRenew,
                                onRevoke = onRevoke,
                            )
                        }
                    }
                }
                // Backup trust rows (nests.md § Trust facet — backup rows, ratified
                // 2026-07-24) — AFTER the content-processing grant rows, home row
                // only (the shared machine populates them nowhere else, since both
                // grants empower the source nest). Outside the branch above because
                // they render alongside *either* arm: beside the grant list when
                // content grants exist, and on their own when none do (the empty
                // state is suppressed in that case — see the condition above).
                row.trustBackups.forEach { backup ->
                    BackupItem(
                        backup = backup,
                        onRevokeSeal = onRevokeBackupSeal,
                        onRevokeWriter = onRevokeBackupWriter,
                    )
                }
                // Retained generations (nests.md § Trust facet — generation
                // recovery, ratified 2026-07-29) — AFTER the backup trust
                // rows, home row only (recovery is addressed to the OWNER's
                // own backup destinations, never a pairing). Same
                // both-arms placement as the backup rows above: renders
                // alongside either arm, never gated by them.
                row.trustGenerations.forEach { generation ->
                    GenerationItem(
                        generation = generation,
                        onRestore = { g ->
                            onRestoreGeneration(g.destinationId, g.folderName, g.pathHash, g.manifestHash)
                        },
                    )
                }
                // The restore-outcome notice (`nest-trust-generation-notice`,
                // ratified 2026-07-29) — home-row-scoped, NOT per-row: a
                // restore's outcome describes the page's last action, not any
                // one generation row. Registered whenever the home row's Now
                // lens renders, EMPTY until a restore resolves. Distinct from
                // `error-message`: only a genuinely failed call reaches that;
                // `PastRecoveryWindow` is a product state, never an error.
                if (row.isHome) {
                    Text(
                        generationNoticeText(restoreOutcome)?.let { stringResource(it) } ?: "",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.testTag(Ids.NEST_TRUST_GENERATION_NOTICE),
                    )
                }
                // Mint flow (scope-first picker, nests.md § Mint) — after the
                // grant list / empty state, only when the shared option catalog
                // is non-empty (empty ⇒ nothing derivable or no discoverable
                // holder — never a picker that can only error).
                // The per-nest blessing (`nest-trust-blessed-toggle`, nests.md
                // § Expiry / renewal → Duration and blessing) — home row only,
                // like the rest of the facet.
                if (row.isHome) {
                    BlessedToggle(blessed = row.blessed, onSetBlessed = onSetBlessed)
                }
                if (row.mintOptions.isNotEmpty()) {
                    MintFlow(
                        options = row.mintOptions,
                        defaultDuration = row.mintDefaultDuration,
                        durationOptions = durationOptions,
                        durationLabel = durationLabel,
                        onMint = onMint,
                    )
                }
            }
            TrustLens.HISTORY -> {
                Column(
                    modifier = Modifier.testTag(Ids.NEST_TRUST_HISTORY_LIST),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    row.trustHistory.forEach { h ->
                        Text(
                            historyLine(h),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.testTag(Ids.NEST_TRUST_HISTORY_ITEM),
                        )
                    }
                }
            }
        }
    }
}

/**
 * One backup trust row (`nest-trust-backup-item`) for the Now lens on the home
 * nest's row: the scope line, when the trust was given, the row state, the
 * REQUIRED honest-bound copy, and the freeze-the-backup affordance
 * (`nests.md` § Trust facet — backup rows).
 *
 * Two row kinds share the component (`nests.md:63`): the seal grant, revoked at
 * the source nest, and one writer row per destination, revoked **at the
 * destination** — the shared machine routes each press to the right nest, so
 * this layer only names which row was pressed.
 *
 * Deliberately no lasts-until / renew / History twin: both grants are standing
 * live nest reads, not folds of the signed grant-event log (`nests.md:99`).
 */
@Composable
private fun BackupItem(
    backup: TrustBackupRow,
    onRevokeSeal: () -> Unit,
    onRevokeWriter: (String) -> Unit,
) {
    val scopeText = when (backup.kind) {
        TrustBackupKind.SEAL -> stringResource(R.string.nests_backup_scope_seal)
        TrustBackupKind.WRITER ->
            stringResourceFmt(R.string.nests_backup_scope_writer, backup.destinationLabel)
    }
    // `nest-trust-backup-since` renders EMPTY on the seal row — that grant carries
    // no timestamp on the wire (`nests.md:67`). The element is still present so
    // the row's leaf set doesn't vary by kind. The prefix is resolved here
    // (composable context) and joined in the plain `let` below.
    val sincePrefix = stringResource(R.string.nests_backup_since)
    val sinceText = backup.since?.let { "$sincePrefix ${formatEpochSeconds(it)}" } ?: ""

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.NEST_TRUST_BACKUP_ITEM)) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                scopeText,
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.NEST_TRUST_BACKUP_SCOPE),
            )
            Text(
                sinceText,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.NEST_TRUST_BACKUP_SINCE),
            )
            Text(
                resolveLocalized(LocalContext.current, backupStatusLabel(backup.status)).orEmpty(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.NEST_TRUST_BACKUP_STATUS),
            )
            // REQUIRED honest-bound copy — revoking freezes only NEW writes;
            // custody already held remains until the holder reclaims it. Never
            // over-promise.
            Text(
                stringResource(
                    when (backup.kind) {
                        TrustBackupKind.SEAL -> R.string.nests_backup_bound_note_seal
                        TrustBackupKind.WRITER -> R.string.nests_backup_bound_note_writer
                    },
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.NEST_TRUST_BACKUP_BOUND_NOTE),
            )
            // A DISCRIMINANT site: the row's kind decides which wire kind the
            // revoke issues, so the gate is handed the SAME `when` the action
            // takes rather than a second expression that could drift from it —
            // and the shared table, never a Kotlin class test, decides. Both
            // arms are OnlineOnly today; if one is ever reclassified this
            // control follows for free. (apple declares both kinds on its one
            // site for the same reason.)
            val backupGate = faunaGate(
                when (backup.kind) {
                    TrustBackupKind.SEAL -> "fauna.backup.nest_key.revoke"
                    TrustBackupKind.WRITER -> "fauna.backup.writer_grant.revoke"
                },
            )
            OutlinedButton(
                onClick = {
                    when (backup.kind) {
                        TrustBackupKind.SEAL -> onRevokeSeal()
                        TrustBackupKind.WRITER -> onRevokeWriter(backup.destinationId)
                    }
                },
                enabled = backupGate.enabled,
                colors = ButtonDefaults.outlinedButtonColors(
                    contentColor = MaterialTheme.colorScheme.error,
                ),
                modifier = Modifier.testTag(Ids.NEST_TRUST_BACKUP_REVOKE),
            ) { Text(stringResource(R.string.nests_backup_revoke)) }
            DisabledControlReasonText(backupGate.reason)
        }
    }
}

/**
 * One retained-generation row (`nest-trust-generation-item`) in the Now lens
 * on the home nest's row: what the owner can roll back to inside the custody
 * grace window T (`nests.md` § Trust facet — generation recovery). Mirrors
 * linux's `build_generation_item` field-for-field — three ratified honesty
 * invariants a shell must get right (`nests.md:141`-`:145`):
 *
 * - An `Unreachable` row renders **no** restore affordance (invariant 1) —
 *   there is no address to restore, and offering the affordance would imply
 *   we knew something we do not. It is also why the row exists at all: a
 *   destination that could not be asked must never render as "nothing to
 *   recover", the false reassurance a hostile source buys.
 * - A row with no plaintext `path` renders its **hash** rather than being
 *   hidden or skipped (invariant 2) — the rows a rogue source produced are
 *   exactly the ones a user needs to see.
 * - `restoreOutcome` is typed and rendered on the sibling
 *   `nest-trust-generation-notice` element, never folded onto the generic
 *   error path (invariant 3 — see [generationNoticeText]).
 *
 * The three value leaves (superseded/expires/size) render EMPTY on an
 * unreachable row rather than a zero timestamp or "0 B", which would read as
 * fact — the elements stay registered so the row's leaf set does not vary by
 * status (the same shape [BackupItem]'s seal-row `since` uses). Ordering is
 * the destination's, preserved by the shared projection — this layer never
 * re-sorts (`nests.md:138`). The restore action carries the row's own
 * **address triple**, never a row index: the list spans destinations, so an
 * index-addressed restore would promote the wrong version the moment this
 * flattened list is filtered or re-ordered.
 */
@Composable
private fun GenerationItem(
    generation: TrustGenerationRow,
    onRestore: (TrustGenerationRow) -> Unit,
) {
    val unreachable = generation.status == TrustGenerationStatus.UNREACHABLE
    val context = LocalContext.current

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.NEST_TRUST_GENERATION_ITEM)) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                generationPathText(generation),
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.NEST_TRUST_GENERATION_PATH),
            )
            val supersededPrefix = stringResource(R.string.nests_generation_superseded)
            Text(
                if (unreachable) "" else "$supersededPrefix ${formatEpochSeconds(generation.supersededAt)}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.NEST_TRUST_GENERATION_SUPERSEDED),
            )
            // REQUIRED quota-bound copy (nests.md § Required copy): a user near
            // their cap on a destination can see usage a supersede storm
            // inflated until T elapses, and this leaf is where that is
            // explicable rather than mysterious.
            Text(
                if (unreachable) "" else stringResourceFmt(
                    R.string.nests_generation_expires,
                    formatEpochSeconds(generation.expiresAt),
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.NEST_TRUST_GENERATION_EXPIRES),
            )
            Text(
                if (unreachable) "" else ValueFormat.byteSize(context, generation.sizeBytes),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.NEST_TRUST_GENERATION_SIZE),
            )
            Text(
                stringResource(
                    if (unreachable) R.string.nests_generation_status_unreachable
                    else R.string.nests_generation_status_listed,
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.NEST_TRUST_GENERATION_STATUS),
            )
            if (generationShowsRestore(generation)) {
                val restoreGate = faunaGate("fauna.backup.generation.restore")
                Button(
                    onClick = { onRestore(generation) },
                    enabled = restoreGate.enabled,
                    modifier = Modifier.testTag(Ids.NEST_TRUST_GENERATION_RESTORE),
                ) { Text(stringResource(R.string.nests_generation_restore)) }
                DisabledControlReasonText(restoreGate.reason)
            }
        }
    }
}

/**
 * The `nest-trust-generation-path` identity leaf (`nests.md:143`,`:144`).
 *
 * **Ratified invariant, mirrored from linux's `generation_path_text`.** On an
 * `Unreachable` row this names the DESTINATION that went dark, since there is
 * no generation to identify; on a `Listed` row with no plaintext `path` (a
 * sealed-path custody row or a rogue source's) this renders the
 * `pathHash` rather than hiding or skipping the row — the rows a rogue source
 * produced are exactly the ones a user needs to see.
 */
@Composable
private fun generationPathText(generation: TrustGenerationRow): String {
    if (generation.status == TrustGenerationStatus.UNREACHABLE) return generation.destinationLabel
    return generation.path?.let { stringResourceFmt(R.string.nests_generation_path, it) }
        ?: stringResourceFmt(R.string.nests_generation_path_unknown, generation.pathHash)
}

/**
 * Whether `nest-trust-generation-restore` renders for this row (`nests.md:143`).
 *
 * **Ratified invariant, mirrored from linux's `generation_shows_restore`.** An
 * `Unreachable` row carries no restore address — offering the affordance
 * would imply we knew something we do not, and it is also why the row exists
 * at all: a destination we could not ask must never render as "nothing to
 * recover", the false reassurance a hostile source buys.
 */
private fun generationShowsRestore(generation: TrustGenerationRow): Boolean =
    generation.status != TrustGenerationStatus.UNREACHABLE

/**
 * The `nest-trust-generation-notice` string resource for the page's last
 * restore action (`nests.md` § Trust facet — generation recovery, ratified
 * 2026-07-29). Never an error — `PastRecoveryWindow` is a product state, not a
 * failure (`nests.md:145`) — so this never rides the `error-message` element.
 * `null` (no restore has run yet) renders an empty notice.
 */
private fun generationNoticeText(restoreOutcome: TrustRestoreOutcome?): Int? = when (restoreOutcome) {
    TrustRestoreOutcome.RESTORED -> R.string.nests_generation_restored
    TrustRestoreOutcome.PAST_RECOVERY_WINDOW -> R.string.nests_generation_past_window
    null -> null
}

/**
 * One current-grant row (`nest-trust-grant-item`) for the Now lens: the scope
 * line ("Trusted to read: Mail, Calendar"), lasts-until, liveness status, the
 * REQUIRED honest-bound copy, and per-grant renew/revoke. `grantId` round-trips
 * unchanged into the Renew/Revoke dispatch.
 */
@Composable
private fun GrantItem(
    grant: TrustGrantRow,
    onRenew: (ByteArray) -> Unit,
    onRevoke: (ByteArray) -> Unit,
) {
    val context = LocalContext.current
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.NEST_TRUST_GRANT_ITEM)) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                // `nests.trusted_to_read` carries no placeholder: the scope line
                // follows it, as on every other app.
                "${stringResource(R.string.nests_trusted_to_read)} ${scopeLine(context, grant.scope, grant.folder)}",
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.NEST_TRUST_GRANT_SCOPE),
            )
            Text(
                stringResourceFmt(R.string.nests_lasts_until, formatEpochSeconds(grant.lastsUntil)),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.NEST_TRUST_GRANT_LASTS_UNTIL),
            )
            Text(
                resolveLocalized(context, statusLabel(grant.liveness)).orEmpty(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.NEST_TRUST_GRANT_STATUS),
            )
            // REQUIRED honest-bound copy (nests.md § Honest bound) — never
            // over-promise. A bounded (content-sealing-epochs) mail grant gets the
            // stronger, crypto-bounded wording WITH the honest INFO-A caveat; every
            // other kind/regime keeps the standing trust-until-revoke wording
            // (flip-checklist line 6). Never re-derive the (class, kind, tier)
            // check here — the shared predicate is the single source of truth
            // (priority #2).
            Text(
                stringResource(
                    if (uniffi.fauna_client_pair.trustScopeIsBoundedMailGrant(grant.scope)) {
                        R.string.nests_bound_note_bounded_mail
                    } else {
                        R.string.nests_bound_note_standing
                    },
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.NEST_TRUST_GRANT_BOUND_NOTE),
            )
            val renewGate = faunaGate("fauna.capabilities.renew")
            val revokeGate = faunaGate("fauna.capabilities.revoke")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { onRenew(grant.grantId) },
                    enabled = renewGate.enabled,
                    modifier = Modifier.testTag(Ids.NEST_TRUST_GRANT_RENEW),
                ) { Text(stringResource(R.string.nests_renew)) }
                OutlinedButton(
                    onClick = { onRevoke(grant.grantId) },
                    enabled = revokeGate.enabled,
                    colors = ButtonDefaults.outlinedButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                    modifier = Modifier.testTag(Ids.NEST_TRUST_GRANT_REVOKE),
                ) { Text(stringResource(R.string.nests_revoke)) }
            }
            // One reason for the pair — they gate together (both OnlineOnly) and
            // sit in one row, so two identical captions would be noise.
            DisabledControlReasonText(renewGate.reason)
        }
    }
}

/**
 * The scope-first mint flow for one nest row (`nest-trust-grant-mint-button` →
 * `nest-trust-mint-scope-select` [→ `nest-trust-mint-holder-select`] →
 * `nest-trust-mint-confirm-button`; nests.md § Mint, ratified 2026-07-13). The
 * scope select's options are the shared `LinkedNestRow.mintOptions` catalog
 * verbatim (one use-case option each, labeled shell-side — priority #2); the
 * holder is derived from the chosen option, and the holder select renders only
 * when an option lists more than one candidate (the ambiguity case; every
 * option derives exactly one today). Confirm dispatches `Mint` and the
 * recomposition collapses the form. Mirrors the web + linux `build_mint_flow`.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MintFlow(
    options: List<TrustMintOption>,
    defaultDuration: TrustGrantDuration,
    durationOptions: () -> List<TrustGrantDuration>,
    durationLabel: (TrustGrantDuration) -> uniffi.fauna_core.LocalizedText,
    onMint: (String, List<TrustScope>, TrustGrantDuration) -> Unit,
) {
    var showForm by remember { mutableStateOf(false) }
    // How long the new trust lasts — the row's default until the owner picks.
    var duration by remember(defaultDuration) { mutableStateOf(defaultDuration) }
    var durationExpanded by remember { mutableStateOf(false) }
    var selected by remember { mutableStateOf<TrustMintOption?>(null) }
    var holderPick by remember { mutableStateOf<String?>(null) }
    var scopeExpanded by remember { mutableStateOf(false) }
    var holderExpanded by remember { mutableStateOf(false) }

    val context = LocalContext.current
    val placeholder = stringResource(R.string.nests_mint_scope_placeholder)

    // ARMING IS LOCAL: the mint button below only opens the form, so it stays
    // live with no nest — only the confirm commits (`fauna.capabilities.mint`).
    val mintGate = faunaGate("fauna.capabilities.mint", enabled = selected != null)

    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Button(
            onClick = {
                duration = defaultDuration
                showForm = true
            },
            modifier = Modifier.testTag(Ids.NEST_TRUST_GRANT_MINT_BUTTON),
        ) { Text(stringResource(R.string.nests_mint_button)) }

        if (showForm) {
            val chosen = selected
            ExposedDropdownMenuBox(
                expanded = scopeExpanded,
                onExpandedChange = { scopeExpanded = !scopeExpanded },
            ) {
                OutlinedTextField(
                    value = chosen?.let { resolveLocalized(context, mintOptionLabel(it)).orEmpty() } ?: placeholder,
                    onValueChange = {},
                    readOnly = true,
                    trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = scopeExpanded) },
                    modifier = Modifier
                        .fillMaxWidth()
                        .menuAnchor()
                        .testTag(Ids.NEST_TRUST_MINT_SCOPE_SELECT),
                )
                ExposedDropdownMenu(
                    expanded = scopeExpanded,
                    onDismissRequest = { scopeExpanded = false },
                ) {
                    options.forEach { option ->
                        DropdownMenuItem(
                            text = { Text(resolveLocalized(context, mintOptionLabel(option)).orEmpty()) },
                            onClick = {
                                selected = option
                                holderPick =
                                    if (option.holderCandidates.size > 1) option.holderCandidates.first() else null
                                scopeExpanded = false
                            },
                        )
                    }
                }
            }

            // Conditional holder select — shown only on the >1-candidate
            // ambiguity arm (unreachable today; every option derives one holder).
            if (chosen != null && chosen.holderCandidates.size > 1) {
                ExposedDropdownMenuBox(
                    expanded = holderExpanded,
                    onExpandedChange = { holderExpanded = !holderExpanded },
                ) {
                    OutlinedTextField(
                        value = holderPick ?: chosen.holderCandidates.first(),
                        onValueChange = {},
                        readOnly = true,
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = holderExpanded) },
                        modifier = Modifier
                            .fillMaxWidth()
                            .menuAnchor()
                            .testTag(Ids.NEST_TRUST_MINT_HOLDER_SELECT),
                    )
                    ExposedDropdownMenu(
                        expanded = holderExpanded,
                        onDismissRequest = { holderExpanded = false },
                    ) {
                        chosen.holderCandidates.forEach { cand ->
                            DropdownMenuItem(
                                text = { Text(cand) },
                                onClick = {
                                    holderPick = cand
                                    holderExpanded = false
                                },
                            )
                        }
                    }
                }
            }

            // The duration select (`nest-trust-mint-duration-select`): the
            // shared option list and labels, pre-selecting the row's default.
            ExposedDropdownMenuBox(
                expanded = durationExpanded,
                onExpandedChange = { durationExpanded = !durationExpanded },
            ) {
                OutlinedTextField(
                    value = resolveLocalized(context, durationLabel(duration)).orEmpty(),
                    onValueChange = {},
                    readOnly = true,
                    trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = durationExpanded) },
                    modifier = Modifier
                        .fillMaxWidth()
                        .menuAnchor()
                        .testTag(Ids.NEST_TRUST_MINT_DURATION_SELECT),
                )
                ExposedDropdownMenu(
                    expanded = durationExpanded,
                    onDismissRequest = { durationExpanded = false },
                ) {
                    durationOptions().forEach { d ->
                        DropdownMenuItem(
                            text = { Text(resolveLocalized(context, durationLabel(d)).orEmpty()) },
                            onClick = {
                                duration = d
                                durationExpanded = false
                            },
                        )
                    }
                }
            }

            Button(
                onClick = {
                    val option = selected ?: return@Button
                    // Derived holder: the single candidate; ambiguity → the
                    // holder select's pick (visible iff >1 candidate).
                    val holder =
                        if (option.holderCandidates.size > 1) holderPick ?: return@Button
                        else option.holderCandidates.first()
                    showForm = false
                    selected = null
                    holderPick = null
                    onMint(holder, option.scope, duration)
                },
                enabled = mintGate.enabled,
                modifier = Modifier.testTag(Ids.NEST_TRUST_MINT_CONFIRM_BUTTON),
            ) { Text(stringResource(R.string.nests_mint_confirm)) }
            DisabledControlReasonText(mintGate.reason)
        }
    }
}

/**
 * The per-nest blessing (`nest-trust-blessed-toggle`): a checkbox whose
 * `state` the driver reads through the toggle convention — the Compose
 * `ToggleableState` semantics carry it. Toggling dispatches `SetBlessed`, a
 * `fauna.state.blessed-nests` write (`fauna.account.state.put`) — offline-safe, so ungated.
 */
@Composable
private fun BlessedToggle(blessed: Boolean, onSetBlessed: (Boolean) -> Unit) {
    Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
        androidx.compose.material3.Checkbox(
            checked = blessed,
            onCheckedChange = { onSetBlessed(it) },
            modifier = Modifier.testTag(Ids.NEST_TRUST_BLESSED_TOGGLE),
        )
        Text(stringResource(R.string.nests_blessed_toggle))
    }
}

/**
 * A grant's scope line, joined ("Mail, Calendar") — the shared
 * [grantScopeLabels], which names a folder grant's folder (`folder`, resolved
 * in shared Rust) in place of the bare folder read, each label resolved through
 * the app's own i18n lookup (`nests.scope_*`; priority #2, no per-app match
 * statement). Plain (non-composable) — `context` is captured by the caller so
 * this is safe from `joinToString`'s transform lambda, which isn't
 * @Composable-aware. Mirrors the web `scopeLine` / linux and tui `scope_line`.
 */
private fun scopeLine(context: Context, scope: List<TrustScope>, folder: TrustFolder?): String =
    grantScopeLabels(scope, folder).joinToString(", ") { resolveLocalized(context, it).orEmpty() }

/**
 * One History-lens row's self-describing line ("Trusted to read ‹scope› ·
 * ‹when›" etc.). The `history_*` i18n strings carry `{scope}`/`{when}` named
 * placeholders (resolved positionally in textual order by [stringResourceFmt]).
 */
@Composable
private fun historyLine(h: TrustHistoryRow): String {
    val scope = scopeLine(LocalContext.current, h.scope, h.folder)
    val when_ = formatEpochSeconds(h.at)
    val resId = when (h.kind) {
        TrustEventKind.MINT -> R.string.nests_history_minted
        TrustEventKind.RENEW -> R.string.nests_history_renewed
        TrustEventKind.REVOKE -> R.string.nests_history_revoked
    }
    return stringResourceFmt(resId, scope, when_)
}

/**
 * Format a unix-seconds timestamp as an absolute locale-aware date + time (no
 * shared value-formatting decision applies here — `lasts_until` is a future
 * expiry, not a past event, so [com.fauna.app.ui.util.ValueFormat.relativeTime]'s
 * relative-past framing doesn't fit). Trust timestamps (`lastsUntil`, history
 * `at`) are epoch **seconds** (the shared row's wire unit). Mirrors the web
 * `fmtEpochSecs` (`new Date(secs * 1000).toLocaleString()`) / linux
 * `format_unix_local`.
 */
private fun formatEpochSeconds(secs: Long): String =
    DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(Date(secs * 1000))
