package com.fauna.app.ui.screen.settings

import com.fauna.app.ui.util.paintedReason
import com.fauna.app.ui.util.localized

import com.fauna.app.ui.navigation.exitAdminToPrimary
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
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.and
import com.fauna.app.ui.util.askForm
import com.fauna.app.ui.util.blockedReason
import com.fauna.app.ui.util.formRefusal
import com.fauna.app.ui.util.faunaFormGate
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.isPossible
import uniffi.fauna_core.Possibility
import com.fauna.app.ui.viewmodel.AdminAliasesVM
import com.fauna.app.ui.viewmodel.AdminEntryRereadEffect
import uniffi.fauna_client_mail_settings.ForwarderStatus
import uniffi.fauna_client_mail_settings.ForwarderView
import social.fauna.generated.Ids
import com.fauna.app.ui.controls.Button
import com.fauna.app.ui.controls.DropdownMenuItem
import com.fauna.app.ui.controls.OutlinedButton
import com.fauna.app.ui.controls.OutlinedTextField
import com.fauna.app.ui.controls.ExposedDropdownMenuBox

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
    AdminEntryRereadEffect(vm.entryReread)
    val snapshot by vm.snapshot.collectAsState()
    // Collected so a move of the add answer (the read landing, a create going
    // out and coming back) repaints the form, whose answers fold it.
    val addAnswer by vm.addAnswer.collectAsState()
    val listRead by vm.listRead.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    AdminAliasesContent(
        forwarders = snapshot.forwarders,
        localDomains = snapshot.localDomains,
        actionError = snapshot.error,
        working = snapshot.status == ForwarderStatus.WORKING,
        onBack = { navController.exitAdminToPrimary() },
        onCreate = vm::create,
        onDelete = vm::delete,
        // Asked on every recomposition the snapshot drives, so a new read
        // repaints the form.
        formPossibility = addAnswer.let { vm.formPossibility() },
        createPossibility = vm::createPossibility,
        onOpenDomains = { navController.navigate("settings/admin-dns") },
        onRefused = appMessages::showError,
        forwardersRead = askForm { com.fauna.ffi.readPossibility(listRead) },
    )
}

/** The form's reason when the nest hosts no local domain — the one blocked
 *  answer the Domains step unblocks (a pending read gives another). */
private const val NO_HOSTED_DOMAIN = "admin.aliases_page.no_hosted_domain"


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
    // The add form's gate (`admin.md` § 4, *A forwarder needs a hosted
    // domain*): the shared `ForwarderMachine` answers, injected so the content
    // renders with no native call.
    formPossibility: Possibility = Possibility.Possible,
    createPossibility: (String) -> Possibility = { Possibility.Possible },
    onOpenDomains: () -> Unit = {},
    // The submit's draft answer — the shared
    // `ForwarderMachine::create_draft_possibility` (a domain picked, a
    // non-blank local part and destination; admin.md § Errors & edge cases,
    // the form bullet), folded AFTER [createPossibility] so a page still
    // loading says so first. Fails open off the native path ([askForm]).
    createDraftPossibility: (domain: String, pattern: String, target: String) -> Possibility = { d, p, t ->
        askForm { com.fauna.ffi.forwarderCreateDraftPossibility(d, p, t) }
    },
    // A press the draft answer refuses shows its reason on the page-level
    // `error-message` banner (admin.md § Errors & edge cases, the form bullet;
    // as tui and linux do) — the dedicated action-error line stays the nest's.
    onRefused: (String) -> Unit = {},
    // The forwarder list read's answer — `read_possibility` over the shared
    // `ForwarderMachine::read_state` (admin.md § Errors & edge cases,
    // *Controls wait for their read*): the empty text is painted only once
    // the list was read; until then the read's own reason stands in.
    forwardersRead: Possibility = Possibility.Possible,
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
                formPossibility = formPossibility,
                createPossibility = createPossibility,
                createDraftPossibility = createDraftPossibility,
                onCreate = onCreate,
                onOpenDomains = onOpenDomains,
                onRefused = onRefused,
            )

            HorizontalDivider()

            // ── Forwarder list (indexed; one row per ForwarderView) ──
            val unread = localized(forwardersRead.blockedReason)
            if (unread != null) {
                Text(
                    unread,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else if (forwarders.isEmpty()) {
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
    formPossibility: Possibility,
    createPossibility: (String) -> Possibility,
    createDraftPossibility: (String, String, String) -> Possibility,
    onCreate: (String, String, String) -> Unit,
    onOpenDomains: () -> Unit,
    onRefused: (String) -> Unit,
) {
    val context = LocalContext.current
    var domain by remember(localDomains) { mutableStateOf(localDomains.firstOrNull() ?: "") }
    var pattern by remember { mutableStateOf("") }
    var target by remember { mutableStateOf("") }
    var expanded by remember { mutableStateOf(false) }
    // The picker and fields follow the form's answer; the submit its own
    // answer over the picked domain folded with the draft's, asked again at
    // the press.
    val canAdd = formPossibility.isPossible
    // Published on each form control as its automation `reason`.
    val formReason = localized(formPossibility.blockedReason)
    val createAnswer = createPossibility(domain) and createDraftPossibility(domain, pattern, target)

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
                    // The reason goes OUTERMOST: the anchor publishes its own
                    // `Collapsed`/`Expanded` state description, and the outer
                    // semantics modifier wins a property both set.
                    modifier = Modifier
                        .paintedReason(formReason)
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
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_ALIASES_FORWARDER_ADD_PATTERN_INPUT)
                    .paintedReason(formReason),
            )
            OutlinedTextField(
                value = target,
                onValueChange = { target = it },
                singleLine = true,
                enabled = canAdd,
                label = { Text(stringResource(R.string.admin_aliases_page_forwarder_target)) },
                placeholder = { Text(stringResource(R.string.admin_aliases_page_forwarder_target_placeholder)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_ALIASES_FORWARDER_ADD_TARGET_INPUT)
                    .paintedReason(formReason),
            )
            // ⚠ Both this submit and the per-row delete dispatch through the
            // SHARED `ForwarderMachine`, so the gate goes at the call site, not
            // inside the machine — the same split `TierDropdown` and
            // `SaveButton` earned, now for a third reason: a machine is not a
            // composable at all, and a kind literal has to sit where the checker
            // can read it. The domain select, pattern and target inputs above
            // follow the form's shared answer.
            val createGate = faunaFormGate(
                createAnswer,
                faunaGate("fauna.bridges.create_forwarder", enabled = createAnswer.isPossible),
            )
            Button(
                onClick = {
                    val refusal = formRefusal(
                        context,
                        createPossibility(domain) and createDraftPossibility(domain, pattern, target),
                    )
                    if (refusal != null) {
                        // The typed draft stays in the form.
                        refusal.reason?.let(onRefused)
                    } else {
                        onCreate(domain, pattern.trim(), target.trim())
                        pattern = ""
                        target = ""
                    }
                },
                enabled = createGate.enabled,
                modifier = Modifier.testTag(Ids.ADMIN_ALIASES_FORWARDER_ADD_SUBMIT_BUTTON).paintedReason(createGate.reason),
            ) { Text(stringResource(R.string.admin_aliases_page_create_forwarder)) }
            DisabledControlReasonText(createGate.reason)
            // The one step that unblocks the form: the DNS page, where a domain
            // is added — offered only for want of a hosted domain.
            if (formPossibility.blockedReason?.key == NO_HOSTED_DOMAIN) {
                OutlinedButton(
                    onClick = onOpenDomains,
                    modifier = Modifier.testTag(Ids.ADMIN_ALIASES_FORWARDER_ADD_DOMAINS_STEP),
                ) { Text(stringResource(R.string.admin_aliases_page_open_domains)) }
            }
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
