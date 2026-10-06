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
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.viewmodel.AdminAliasesVM
import uniffi.fauna_client_mail_settings.ForwarderStatus
import uniffi.fauna_client_mail_settings.ForwarderView
import social.fauna.generated.Ids

/**
 * The admin `admin-aliases` page (`admin.md` § 4 / `mail-aliases.md` § Kind 7):
 * external **forwarders** — an address on a hosted local domain that forwards
 * inbound to an external destination with no local mailbox. The forwarder slice
 * is the only surface on this page (the per-domain catch-all designation lives on
 * the admin-dns per-domain row, not here).
 *
 * Stateless [AdminAliasesContent] is split out for the Compose test harness; the
 * VM-bound [AdminAliasesScreen] is the wrapper the NavHost mounts as an admin
 * sub-page. Dumb renderer of the shared `ForwarderMachine`
 * (libs/fauna-client-mail-settings, over UniFFI) — no forwarder logic in the
 * shell (priority #2). Backend is built → genuinely green; errors
 * (`conflicts_with_existing_alias` / `reserved_local_part` / `validate_forward_target`)
 * surface via the dedicated `admin-aliases-action-error` element. Mirrors the
 * Linux lead (apps/fauna-linux/src/views/admin.rs build_admin_aliases_page).
 */
@Composable
fun AdminAliasesScreen(
    navController: NavController,
    vm: AdminAliasesVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()

    AdminAliasesContent(
        forwarders = snapshot.forwarders,
        localDomains = snapshot.localDomains,
        actionError = snapshot.error,
        working = snapshot.status == ForwarderStatus.WORKING,
        onBack = { navController.popBackStack() },
        onCreate = vm::create,
        onDelete = vm::delete,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminAliasesContent(
    forwarders: List<ForwarderView>,
    localDomains: List<String>,
    actionError: String?,
    working: Boolean,
    onBack: () -> Unit,
    onCreate: (String, String, String) -> Unit,
    onDelete: (String) -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_aliases_page_title),
                        modifier = Modifier.testTag(Ids.ADMIN_ALIASES_HEADING),
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
            // ── External forwarders (Kind 7) — the only section on this page ──
            Column(modifier = Modifier.testTag(Ids.ADMIN_ALIASES_FORWARDERS_SECTION)) {
                Text(
                    stringResource(R.string.admin_aliases_page_forwarders_title),
                    style = MaterialTheme.typography.titleMedium,
                )
                Text(
                    stringResource(R.string.admin_aliases_page_forwarders_desc),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            // ── Action error (dedicated; surfaces the seam's rejection) ──
            if (!actionError.isNullOrEmpty()) {
                Text(
                    actionError,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.ADMIN_ALIASES_ACTION_ERROR),
                )
            } else {
                // Keep the element in the tree for ui.yaml conformance.
                Box(modifier = Modifier.testTag(Ids.ADMIN_ALIASES_ACTION_ERROR))
            }

            // ── Add form ──
            AddForwarderForm(
                localDomains = localDomains,
                working = working,
                onCreate = onCreate,
            )

            HorizontalDivider()

            // ── Forwarder list (indexed; one row per ForwarderView) ──
            if (forwarders.isEmpty()) {
                Text(
                    stringResource(R.string.admin_aliases_page_no_forwarders),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                forwarders.forEach { fwd ->
                    ForwarderRow(forwarder = fwd, working = working, onDelete = onDelete)
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun AddForwarderForm(
    localDomains: List<String>,
    working: Boolean,
    onCreate: (String, String, String) -> Unit,
) {
    var domain by remember(localDomains) { mutableStateOf(localDomains.firstOrNull() ?: "") }
    var pattern by remember { mutableStateOf("") }
    var target by remember { mutableStateOf("") }
    var expanded by remember { mutableStateOf(false) }
    val canAdd = localDomains.isNotEmpty() && !working

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            // Domain picker (select over hosted local domains).
            ExposedDropdownMenuBox(
                expanded = expanded,
                onExpandedChange = { if (canAdd) expanded = !expanded },
            ) {
                OutlinedTextField(
                    value = domain,
                    onValueChange = {},
                    readOnly = true,
                    enabled = canAdd,
                    label = { Text(stringResource(R.string.admin_aliases_page_forwarder_domain)) },
                    trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
                    modifier = Modifier
                        .menuAnchor()
                        .fillMaxWidth()
                        .testTag(Ids.ADMIN_ALIASES_FORWARDER_ADD_DOMAIN_SELECT),
                )
                ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                    localDomains.forEach { d ->
                        DropdownMenuItem(
                            text = { Text(d) },
                            onClick = { domain = d; expanded = false },
                        )
                    }
                }
            }

            OutlinedTextField(
                value = pattern,
                onValueChange = { pattern = it },
                singleLine = true,
                enabled = canAdd,
                label = { Text(stringResource(R.string.admin_aliases_page_forwarder_local_part)) },
                placeholder = { Text(stringResource(R.string.admin_aliases_page_forwarder_local_part_placeholder)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_ALIASES_FORWARDER_ADD_PATTERN_INPUT),
            )
            OutlinedTextField(
                value = target,
                onValueChange = { target = it },
                singleLine = true,
                enabled = canAdd,
                label = { Text(stringResource(R.string.admin_aliases_page_forwarder_target)) },
                placeholder = { Text(stringResource(R.string.admin_aliases_page_forwarder_target_placeholder)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_ALIASES_FORWARDER_ADD_TARGET_INPUT),
            )
            // ⚠ Both this submit and the per-row delete dispatch through the
            // SHARED `ForwarderMachine`, so the gate goes at the call site, not
            // inside the machine — the same split `TierDropdown` and
            // `SaveButton` earned, now for a third reason: a machine is not a
            // composable at all, and a kind literal has to sit where the checker
            // can read it. The domain select, pattern and target inputs above are
            // pure buffers and stay live.
            val createGate = faunaGate("fauna.bridges.create_forwarder", enabled = canAdd)
            Button(
                onClick = {
                    val p = pattern.trim()
                    val t = target.trim()
                    if (domain.isNotEmpty() && p.isNotEmpty() && t.isNotEmpty()) {
                        onCreate(domain, p, t)
                        pattern = ""
                        target = ""
                    }
                },
                enabled = createGate.enabled,
                modifier = Modifier.testTag(Ids.ADMIN_ALIASES_FORWARDER_ADD_SUBMIT_BUTTON),
            ) { Text(stringResource(R.string.admin_aliases_page_create_forwarder)) }
            DisabledControlReasonText(createGate.reason)
        }
    }
}

/** One `admin-aliases-forwarder-list` row, projected from a [ForwarderView]. */
@Composable
private fun ForwarderRow(forwarder: ForwarderView, working: Boolean, onDelete: (String) -> Unit) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_ALIASES_FORWARDER_LIST)) {
        Row(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    forwarder.address,
                    style = MaterialTheme.typography.bodyLarge,
                    fontFamily = FontFamily.Monospace,
                    modifier = Modifier.testTag(Ids.ADMIN_ALIASES_FORWARDER_ROW_ADDRESS),
                )
                Text(
                    forwarder.forwardTarget,
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.ADMIN_ALIASES_FORWARDER_ROW_TARGET),
                )
            }
            // The row's only control, and it dispatches — see the call-site note
            // on the create submit for why the gate is not inside
            // `ForwarderMachine`.
            val deleteGate = faunaGate("fauna.bridges.delete_forwarder", enabled = !working)
            Column(horizontalAlignment = Alignment.End) {
                OutlinedButton(
                    onClick = { onDelete(forwarder.aliasIdHex) },
                    enabled = deleteGate.enabled,
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.testTag(Ids.ADMIN_ALIASES_FORWARDER_ROW_DELETE_BUTTON),
                ) { Text(stringResource(R.string.admin_aliases_page_delete_forwarder)) }
                DisabledControlReasonText(deleteGate.reason)
            }
        }
    }
}
