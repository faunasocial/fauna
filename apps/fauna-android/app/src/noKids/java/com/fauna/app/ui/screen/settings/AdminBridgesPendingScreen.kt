package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
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
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.AdminBridgesPendingVM
import uniffi.fauna_client_mail_settings.ApprovedBridgeView
import uniffi.fauna_client_mail_settings.BridgeApprovalStatus
import uniffi.fauna_client_mail_settings.PendingBridgeView
import uniffi.fauna_client_mail_settings.bridgeDisplayName
import java.text.DateFormat
import java.util.Date
import social.fauna.generated.Ids

/**
 * The admin `admin-bridges-pending` page (`mail-bridge-lifecycle.md` § Pending
 * approval): the pending-bridge approval feed. Each card shows a connecting
 * bridge's pubkey fingerprint, requested role, source IP, and first-seen time,
 * with approve / reject actions — the admin verifies the pubkey against the
 * admin's expected fingerprint before approving.
 *
 * Stateless [AdminBridgesPendingContent] is split out for the Compose test
 * harness; the VM-bound [AdminBridgesPendingScreen] is the wrapper the NavHost
 * mounts as an admin sub-page. Dumb renderer of the shared `BridgeApprovalMachine`
 * (libs/fauna-client-mail-settings, over UniFFI) — no approval logic in the shell
 * (priority #2). Two scope gaps the wire doesn't expose yet (code-behind-goal):
 * `source_ip` is always a dash (`ServiceUserInfo` carries no source IP), and
 * reject uses a two-click inline confirm (no separate ui.yaml confirm element).
 * Mirrors the Linux lead (apps/fauna-linux/src/views/admin.rs build_pending_bridge_card).
 */
@Composable
fun AdminBridgesPendingScreen(
    navController: NavController,
    vm: AdminBridgesPendingVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    AdminBridgesPendingContent(
        pending = snapshot.pending,
        approved = snapshot.approved,
        working = snapshot.status == BridgeApprovalStatus.WORKING,
        // Resolve the per-role display name through the shared single-source map
        // (fauna_client_mail_settings::bridge_display_name → LocalizedText key),
        // kept in the stateful Screen so the Content stays Robolectric-safe (no FFI).
        displayName = { role -> resolveLocalized(context, bridgeDisplayName(role)).orEmpty() },
        onBack = { navController.popBackStack() },
        onApprove = vm::approve,
        onReject = vm::reject,
        onRotate = vm::rotate,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminBridgesPendingContent(
    pending: List<PendingBridgeView>,
    approved: List<ApprovedBridgeView>,
    working: Boolean,
    displayName: (String) -> String,
    onBack: () -> Unit,
    onApprove: (String, String) -> Unit,
    onReject: (String) -> Unit,
    onRotate: (String) -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_bridges_pending_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(
                        onClick = onBack,
                        modifier = Modifier.testTag(Ids.ADMIN_NAV_BACK),
                    ) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.common_back))
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
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResource(R.string.admin_bridges_pending_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            Text(
                stringResource(R.string.admin_bridges_pending_pending_section),
                style = MaterialTheme.typography.titleSmall,
            )
            if (pending.isEmpty()) {
                Text(
                    stringResource(R.string.admin_bridges_pending_empty),
                    style = MaterialTheme.typography.bodyLarge,
                )
                Text(
                    stringResource(R.string.admin_bridges_pending_empty_desc),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                pending.forEach { bridge ->
                    PendingBridgeCard(
                        bridge = bridge,
                        working = working,
                        displayName = displayName,
                        onApprove = onApprove,
                        onReject = onReject,
                    )
                }
            }

            Text(
                stringResource(R.string.admin_bridges_pending_approved_section),
                style = MaterialTheme.typography.titleSmall,
            )
            if (approved.isEmpty()) {
                Text(
                    stringResource(R.string.admin_bridges_pending_approved_empty),
                    style = MaterialTheme.typography.bodyLarge,
                )
            } else {
                approved.forEach { bridge ->
                    ApprovedBridgeCard(
                        bridge = bridge,
                        working = working,
                        displayName = displayName,
                        onRotate = onRotate,
                    )
                }
            }
        }
    }
}

/** One `admin-bridges-pending-card`, projected from a [PendingBridgeView]. */
@Composable
private fun PendingBridgeCard(
    bridge: PendingBridgeView,
    working: Boolean,
    displayName: (String) -> String,
    onApprove: (String, String) -> Unit,
    onReject: (String) -> Unit,
) {
    var rejectArmed by remember { mutableStateOf(false) }
    val firstSeen = remember(bridge.firstSeenAt) {
        DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT)
            .format(Date(bridge.firstSeenAt.toLong()))
    }

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_BRIDGES_PENDING_CARD)) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            // Friendly per-role display name above the technical role (admin.md
            // § Bridge display naming): the one MDA bridge serves mail AND calendar,
            // so only its card names calendar. The role→name map is shared Rust
            // (fauna_client_mail_settings::bridge_display_name), resolved in the
            // stateful Screen and injected so the Content stays FFI-free. The role
            // string still renders below (it drives the per-role allowlist on
            // approve), so nothing is lost.
            Text(
                displayName(bridge.requestedRole),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.testTag(Ids.ADMIN_BRIDGES_PENDING_CARD_NAME),
            )
            FieldRow(
                caption = stringResource(R.string.admin_bridges_pending_role),
                value = bridge.requestedRole,
                valueTestTag = "admin-bridges-pending-requested-role",
            )
            FieldRow(
                caption = stringResource(R.string.admin_bridges_pending_pubkey),
                value = bridge.pubkeyHex,
                valueTestTag = "admin-bridges-pending-pubkey-hex",
                monospace = true,
            )
            FieldRow(
                caption = stringResource(R.string.admin_bridges_pending_source_ip),
                value = bridge.sourceIp ?: stringResource(R.string.admin_bridges_pending_source_ip_unknown),
                valueTestTag = "admin-bridges-pending-source-ip",
            )
            FieldRow(
                caption = stringResource(R.string.admin_bridges_pending_first_seen),
                value = firstSeen,
                valueTestTag = "admin-bridges-pending-first-seen-at",
            )

            // A REJECT is a commit — the fourth time this fan-out has met that
            // shape, after the family transfer-decline, the paywall copy-link
            // and the admit-queue deny. Refusing an admission is still a nest
            // write, so it declares exactly as the approve beside it does.
            val approveGate = faunaGate("fauna.bridges.approve_pending_bridge", enabled = !working)
            // Gates whole, arm click included — the `mail-spam-reset-model-button`
            // precedent: a two-click inline confirm has no opener and reveals
            // nothing, so arming a control that cannot fire is theatre.
            val rejectGate = faunaGate("fauna.bridges.reject_pending_bridge", enabled = !working)
            Row(
                modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Button(
                    onClick = { onApprove(bridge.pubkeyHex, bridge.requestedRole) },
                    enabled = approveGate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_BRIDGES_PENDING_APPROVE_BUTTON),
                ) { Text(stringResource(R.string.admin_bridges_pending_approve)) }
                OutlinedButton(
                    onClick = {
                        // Two-click inline confirm (mail-bridge-lifecycle.md
                        // § rejection flow gates reject behind a confirm; no
                        // separate ui.yaml element prescribed).
                        if (rejectArmed) {
                            rejectArmed = false
                            onReject(bridge.pubkeyHex)
                        } else {
                            rejectArmed = true
                        }
                    },
                    enabled = rejectGate.enabled,
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.testTag(Ids.ADMIN_BRIDGES_PENDING_REJECT_BUTTON),
                ) {
                    Text(
                        if (rejectArmed) stringResource(R.string.common_confirm)
                        else stringResource(R.string.admin_bridges_pending_reject),
                    )
                }
            }
            // One reason for the pair — both close on the same verdict.
            DisabledControlReasonText(approveGate.reason ?: rejectGate.reason)
        }
    }
}

/**
 * One `admin-bridges-approved-card`, projected from an [ApprovedBridgeView]
 * (admin.md § Approved-bridges roster). The rotate button reveals the inline
 * [RotateBridgeConfirm] form — mirrors the `mail-rotate-keys-confirm` inline
 * reveal (MailSettingsScreen.kt RotateKeysForm), not a dialog/sheet.
 */
@Composable
private fun ApprovedBridgeCard(
    bridge: ApprovedBridgeView,
    working: Boolean,
    displayName: (String) -> String,
    onRotate: (String) -> Unit,
) {
    var rotateOpen by remember { mutableStateOf(false) }
    val approvedAt = remember(bridge.approvedAt) {
        bridge.approvedAt?.let {
            DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(Date(it.toLong()))
        } ?: "—"
    }

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_BRIDGES_APPROVED_CARD)) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(
                displayName(bridge.role),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.testTag(Ids.ADMIN_BRIDGES_APPROVED_CARD_NAME),
            )
            FieldRow(
                caption = stringResource(R.string.admin_bridges_pending_role),
                value = bridge.role,
                valueTestTag = "admin-bridges-approved-role",
            )
            FieldRow(
                caption = stringResource(R.string.admin_bridges_pending_pubkey),
                value = bridge.pubkeyHex,
                valueTestTag = "admin-bridges-approved-pubkey-hex",
                monospace = true,
            )
            FieldRow(
                caption = stringResource(R.string.admin_bridges_pending_approved_at),
                value = approvedAt,
                valueTestTag = "admin-bridges-approved-approved-at",
            )

            if (rotateOpen) {
                RotateBridgeConfirm(
                    working = working,
                    onConfirm = {
                        onRotate(bridge.pubkeyHex)
                        rotateOpen = false
                    },
                    onCancel = { rotateOpen = false },
                )
            } else {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
                    horizontalArrangement = Arrangement.End,
                ) {
                    OutlinedButton(
                        onClick = { rotateOpen = true },
                        enabled = !working,
                        colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                        modifier = Modifier.testTag(Ids.ADMIN_BRIDGES_APPROVED_ROTATE_BUTTON),
                    ) { Text(stringResource(R.string.admin_bridges_pending_rotate)) }
                }
            }
        }
    }
}

/**
 * The rotate-service-user-key confirm form (`admin-bridges-rotate-confirm`
 * element set), rendered inline under the approved card (no DKIM warning for
 * any role: mail-bridge-lifecycle.md § Service-user re-keying).
 */
@Composable
private fun RotateBridgeConfirm(
    working: Boolean,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(stringResource(R.string.admin_bridges_rotate_title), style = MaterialTheme.typography.titleMedium)
        Text(
            stringResource(R.string.admin_bridges_rotate_warning),
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.testTag(Ids.ADMIN_BRIDGES_ROTATE_WARNING_TEXT),
        )
        // ⚠ THE KIND IS `revoke_service_user`, NOT a "rotate" anything — and
        // that mismatch is why an earlier census graded this kind as having no
        // android render site at all and filed it as an unbuilt section. Rotating
        // an approved bridge's service-user key IS revoking it: the machine's
        // `Rotate` action calls `nest.revoke_service_user`, and the bridge then
        // re-enrols with a fresh key (`mail-bridge-lifecycle.md` § Service-user
        // re-keying). Grade a kind by following the dispatcher to its
        // `request(...)`, never by matching the control's own vocabulary.
        //
        // The CONFIRM declares; the opener that revealed this form stays live,
        // because the warning text above is worth reading with no nest.
        val rotateGate = faunaGate("fauna.bridges.revoke_service_user", enabled = !working)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = onConfirm,
                enabled = rotateGate.enabled,
                colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
                modifier = Modifier.testTag(Ids.ADMIN_BRIDGES_ROTATE_CONFIRM_BUTTON),
            ) { Text(stringResource(R.string.admin_bridges_rotate_confirm)) }
            OutlinedButton(
                onClick = onCancel,
                enabled = !working,
                modifier = Modifier.testTag(Ids.ADMIN_BRIDGES_ROTATE_CANCEL_BUTTON),
            ) { Text(stringResource(R.string.admin_bridges_rotate_cancel)) }
        }
        DisabledControlReasonText(rotateGate.reason)
    }
}

/** A caption + value row; the testTag goes on the VALUE (mirrors linux pending_field_row). */
@Composable
private fun FieldRow(
    caption: String,
    value: String,
    valueTestTag: String,
    monospace: Boolean = false,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.Top,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            caption,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.width(96.dp),
        )
        Text(
            value,
            style = MaterialTheme.typography.bodyMedium,
            fontFamily = if (monospace) FontFamily.Monospace else null,
            modifier = Modifier.weight(1f).testTag(valueTestTag),
        )
    }
}
