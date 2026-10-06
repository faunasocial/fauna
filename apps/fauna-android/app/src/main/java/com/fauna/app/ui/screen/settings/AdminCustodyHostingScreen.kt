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
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.viewmodel.AdminCustodyHostingVM
import com.fauna.ffi.FfiAdminHostingRow
import com.fauna.ffi.FfiReceiptState
import social.fauna.generated.Ids

/**
 * The admin Held-Custody page (`admin-custody-hosting`, account-data-plane.md
 * § Two-sided bounds) — the
 * nest-wide custody-hosting registry: every host's hosting row, with honest
 * metering, and a remove behind an arm/confirm addressing the `(host, grant)`
 * pair. A CONTEXTUAL detail page like `admin-dns`/`admin-logs`/
 * `admin-bridges-pending` — reached from the admin dashboard's menu, absent
 * from ui.yaml's `navigation.admin_pages`.
 *
 * Dumb renderer of the shared `AdminHostingClient` + `admin_hosting_rows`
 * fold over UniFFI ([AdminCustodyHostingVM] → `ApiClient.adminHostingList`/
 * `adminHostingRemove` → `FfiAdminClient.custodyHostingList`/
 * `custodyHostingRemove`) — no projection logic here (priority #2). Mirrors
 * the linux leg (`apps/fauna-linux/src/views/admin.rs`
 * `build_custody_hosting_page`) and tui's lead
 * (`apps/fauna-tui/src/admin/custody_hosting.rs`).
 */
@Composable
fun AdminCustodyHostingScreen(
    navController: NavController,
    vm: AdminCustodyHostingVM = hiltViewModel(),
) {
    val rows by vm.rows.collectAsState()
    val error by vm.error.collectAsState()
    val status by vm.status.collectAsState()
    val working by vm.working.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    val removedText = stringResource(R.string.admin_custody_hosting_removed)
    val removedWithStoreText = stringResource(R.string.admin_custody_hosting_removed_with_store)
    val missingText = stringResource(R.string.admin_custody_hosting_remove_missing)
    val budgetDefaultText = stringResource(R.string.admin_custody_hosting_budget_default)

    LaunchedEffect(Unit) { vm.load() }
    LaunchedEffect(error) { error?.let { appMessages.showError(it) } }

    AdminCustodyHostingContent(
        rows = rows,
        status = status,
        working = working,
        // Resolved through the shared FFI (ValueFormat.byteSize / shortId) in
        // the stateful Screen and injected as plain functions, so the Content
        // stays Robolectric-safe (no native library load) — mirrors
        // AdminBridgesPendingScreen's injected `displayName`.
        budgetText = { cap -> if (cap == 0L) budgetDefaultText else ValueFormat.byteSize(context, cap) },
        heldText = { held -> ValueFormat.byteSize(context, held) },
        shortId = { hex -> com.fauna.ffi.shortId(hex) },
        onBack = { navController.popBackStack() },
        onRemove = { host, grant -> vm.remove(host, grant, removedText, removedWithStoreText, missingText) },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminCustodyHostingContent(
    rows: List<FfiAdminHostingRow>?,
    status: String?,
    working: Boolean,
    budgetText: (Long) -> String,
    heldText: (Long) -> String,
    shortId: (String) -> String,
    onBack: () -> Unit,
    onRemove: (String, ByteArray) -> Unit,
) {
    // Mirrors tui/linux/web: the single page-local armed-row key (not per-row
    // indexed state) — opening a new row's confirm silently retargets it, and
    // only the currently-armed row's own remove button disables itself.
    var armedKey by remember { mutableStateOf<String?>(null) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_custody_hosting_title),
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
        },
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
                stringResource(R.string.admin_custody_hosting_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // Pre-hydrate paints neither count nor empty state: "nobody has
            // asked this nest to hold anything" and "the read has not
            // answered yet" are different facts.
            if (rows != null) {
                Text(
                    stringResource(R.string.admin_custody_hosting_count).replace("{count}", rows.size.toString()),
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.testTag(Ids.ADMIN_CUSTODY_HOSTING_COUNT),
                )

                if (rows.isEmpty()) {
                    Text(
                        stringResource(R.string.admin_custody_hosting_empty),
                        style = MaterialTheme.typography.bodyLarge,
                        modifier = Modifier.testTag(Ids.ADMIN_CUSTODY_HOSTING_EMPTY),
                    )
                }

                status?.let {
                    Text(it, style = MaterialTheme.typography.bodyMedium)
                }

                rows.forEachIndexed { i, row ->
                    val key = row.hostActorId + ":" + row.grantId.joinToString(",")
                    HostingRow(
                        index = i,
                        row = row,
                        armed = armedKey == key,
                        working = working,
                        budgetText = budgetText,
                        heldText = heldText,
                        shortId = shortId,
                        onArm = { armedKey = key },
                        onCancel = { armedKey = null },
                        onConfirm = { armedKey = null; onRemove(row.hostActorId, row.grantId) },
                    )
                }
            }
        }
    }
}

/** One `admin-custody-hosting-row` card. */
@Composable
private fun HostingRow(
    index: Int,
    row: FfiAdminHostingRow,
    armed: Boolean,
    working: Boolean,
    budgetText: (Long) -> String,
    heldText: (Long) -> String,
    shortId: (String) -> String,
    onArm: () -> Unit,
    onCancel: () -> Unit,
    onConfirm: () -> Unit,
) {
    val receiptText = when (row.receiptState) {
        FfiReceiptState.FRESH -> stringResource(R.string.admin_custody_hosting_receipt_fresh)
        FfiReceiptState.STALE -> stringResource(R.string.admin_custody_hosting_receipt_stale)
        FfiReceiptState.NO_RECEIPT_YET -> stringResource(R.string.admin_custody_hosting_receipt_none)
    }

    Card(modifier = Modifier.fillMaxWidth().testTag("${Ids.ADMIN_CUSTODY_HOSTING_ROW}-$index")) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            FieldRow(
                caption = stringResource(R.string.admin_custody_hosting_host),
                value = shortId(row.hostActorId),
                valueTestTag = "${Ids.ADMIN_CUSTODY_HOSTING_HOST}-$index",
                monospace = true,
            )
            FieldRow(
                caption = stringResource(R.string.admin_custody_hosting_owner),
                value = shortId(row.ownerActorId),
                valueTestTag = "${Ids.ADMIN_CUSTODY_HOSTING_OWNER}-$index",
                monospace = true,
            )
            FieldRow(
                caption = stringResource(R.string.admin_custody_hosting_url),
                value = row.ownerNestUrl,
                valueTestTag = "${Ids.ADMIN_CUSTODY_HOSTING_URL}-$index",
                monospace = true,
            )
            FieldRow(
                caption = stringResource(R.string.admin_custody_hosting_budget),
                value = budgetText(row.retainedBytesCap),
                valueTestTag = "${Ids.ADMIN_CUSTODY_HOSTING_BUDGET}-$index",
            )
            FieldRow(
                caption = stringResource(R.string.admin_custody_hosting_held),
                value = heldText(row.heldBytes),
                valueTestTag = "${Ids.ADMIN_CUSTODY_HOSTING_HELD}-$index",
            )
            FieldRow(
                caption = "",
                // A stopped row still holds its bytes — remove exists precisely
                // because stop alone does not free them.
                value = if (row.stopped) stringResource(R.string.admin_custody_hosting_stopped)
                        else stringResource(R.string.admin_custody_hosting_active),
                valueTestTag = "${Ids.ADMIN_CUSTODY_HOSTING_STOPPED}-$index",
            )
            FieldRow(
                caption = "",
                value = receiptText,
                valueTestTag = "${Ids.ADMIN_CUSTODY_HOSTING_RECEIPT}-$index",
            )

            if (armed) {
                Column(
                    modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text(
                        stringResource(R.string.admin_custody_hosting_remove_confirm_title),
                        style = MaterialTheme.typography.titleMedium,
                    )
                    Text(
                        stringResource(R.string.admin_custody_hosting_remove_confirm_body),
                        style = MaterialTheme.typography.bodySmall,
                    )
                    // Only the CONFIRM dispatches
                    // `fauna.admin.custody_hosting.remove`; arming and cancelling
                    // touch nothing on the wire, so they stay live with no nest
                    // (tui maps both to no kind at all). The page predicate it
                    // already had is handed over rather than re-tested beside the
                    // gate, so a request in flight still wins while connected.
                    val removeGate = faunaGate(
                        "fauna.admin.custody_hosting.remove",
                        enabled = !working,
                    )
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Button(
                            onClick = onConfirm,
                            enabled = removeGate.enabled,
                            colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
                            modifier = Modifier.testTag(Ids.ADMIN_CUSTODY_HOSTING_REMOVE_CONFIRM_BUTTON),
                        ) { Text(stringResource(R.string.admin_custody_hosting_remove_confirm)) }
                        OutlinedButton(
                            onClick = onCancel,
                            enabled = !working,
                            modifier = Modifier.testTag(Ids.ADMIN_CUSTODY_HOSTING_REMOVE_CANCEL_BUTTON),
                        ) { Text(stringResource(R.string.admin_custody_hosting_remove_cancel)) }
                    }
                    DisabledControlReasonText(removeGate.reason)
                }
            } else {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
                    horizontalArrangement = Arrangement.End,
                ) {
                    OutlinedButton(
                        onClick = onArm,
                        enabled = !working,
                        colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                        modifier = Modifier.testTag("${Ids.ADMIN_CUSTODY_HOSTING_REMOVE_BUTTON}-$index"),
                    ) { Text(stringResource(R.string.admin_custody_hosting_remove)) }
                }
            }
        }
    }
}

/** A caption + value row; the testTag goes on the VALUE (mirrors the
 *  admin-bridges-pending FieldRow). An empty caption is used for the two
 *  single-word status fields (stopped/receipt), which read fine unlabelled. */
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
        if (caption.isNotEmpty()) {
            Text(
                caption,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.width(96.dp),
            )
        }
        Text(
            value,
            style = MaterialTheme.typography.bodyMedium,
            fontFamily = if (monospace) FontFamily.Monospace else null,
            modifier = Modifier.weight(1f).testTag(valueTestTag),
        )
    }
}
