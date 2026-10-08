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
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.MailListMembersVM
import uniffi.fauna_client_mail_settings.ImportResult
import uniffi.fauna_client_mail_settings.MemberStatus
import uniffi.fauna_client_mail_settings.MemberView
import uniffi.fauna_client_mail_settings.memberStatusLabel
import social.fauna.generated.Ids

/**
 * The `mail-list-members` page (`mail-mass-mailing.md`): one list's members —
 * add / batch-import / unsubscribe / resubscribe. Reached from a `mail-lists`
 * row's members button (the list id + name arrive as nav args).
 *
 * Stateless [MailListMembersContent] is split out for the Compose test harness;
 * the VM-bound [MailListMembersScreen] is the wrapper the NavHost mounts. The
 * backend is live; actions dispatch through the shared machine to the real
 * RPCs, with a failure surfacing via the global error-message banner. Mirrors
 * the Linux lead (apps/fauna-linux/src/settings/mail_list_members.rs).
 */
@Composable
fun MailListMembersScreen(
    navController: NavController,
    listIdHex: String,
    listName: String,
    vm: MailListMembersVM = hiltViewModel(),
) {
    LaunchedEffect(listIdHex) { vm.bind(listIdHex, listName) }

    val snapshot by vm.snapshot.collectAsState()
    val hydrated by vm.hydrated.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    MailListMembersContent(
        listName = snapshot.listName.ifEmpty { listName },
        members = snapshot.members,
        hydrated = hydrated,
        subscribedCount = snapshot.subscribedCount,
        unsubscribedCount = snapshot.unsubscribedCount,
        lastImport = snapshot.lastImport,
        // Resolve the status label through the shared single-source map
        // (fauna_client_mail_settings::member_status_label → LocalizedText key),
        // kept in the stateful Screen so the Content stays Robolectric-safe (no FFI).
        statusLabel = { status -> resolveLocalized(context, memberStatusLabel(status)).orEmpty() },
        onBack = { navController.popBackStack() },
        onAddMember = vm::addMember,
        onBatchImport = vm::batchImport,
        onUnsubscribe = vm::unsubscribe,
        onResubscribe = vm::resubscribe,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MailListMembersContent(
    listName: String,
    members: List<MemberView>,
    // Loading-vs-loaded (`ui/README.md` rule 5). Defaults true so
    // existing/other-purpose call sites keep rendering the resolved state.
    hydrated: Boolean = true,
    subscribedCount: UInt,
    unsubscribedCount: UInt,
    // The last import's tally (`MailListMembersSnapshot.last_import`), painted as
    // the shared `mail_lists.import_result` line on `mail-list-members-import-result`
    // and hidden again when the import sheet reopens (tui's `import_result_shown`).
    lastImport: ImportResult? = null,
    statusLabel: (MemberStatus) -> String,
    onBack: () -> Unit,
    onAddMember: (String) -> Unit,
    onBatchImport: (String) -> Unit,
    onUnsubscribe: (String) -> Unit,
    onResubscribe: (String) -> Unit,
) {
    var showAdd by remember { mutableStateOf(false) }
    var showImport by remember { mutableStateOf(false) }
    var addAddress by remember { mutableStateOf("") }
    var importText by remember { mutableStateOf("") }
    // The tally the user already dismissed by reopening the sheet; cleared at
    // submit so a second import with identical counts still paints.
    var dismissedImport by remember { mutableStateOf<ImportResult?>(null) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        if (listName.isNotEmpty()) listName else stringResource(R.string.mail_lists_members_title),
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
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(16.dp)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResourceFmt(
                    R.string.mail_lists_summary_fmt,
                    subscribedCount.toString(),
                    unsubscribedCount.toString(),
                ),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_SUMMARY),
            )

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { showAdd = true; addAddress = "" },
                    modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_ADD_BUTTON),
                ) { Text(stringResource(R.string.mail_lists_add_member_button)) }
                OutlinedButton(
                    onClick = { showImport = true; importText = ""; dismissedImport = lastImport },
                    modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_IMPORT_BUTTON),
                ) { Text(stringResource(R.string.mail_lists_import_button)) }
            }

            if (showAdd) {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        OutlinedTextField(
                            value = addAddress,
                            onValueChange = { addAddress = it },
                            label = { Text(stringResource(R.string.mail_lists_add_member_placeholder)) },
                            singleLine = true,
                            modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_LIST_MEMBERS_ADD_SHEET_ADDRESS_INPUT),
                        )
                        // The sheet's SUBMIT declares; its address input, its
                        // cancel and the opener that revealed it all stay live
                        // (the commit gates, not the buffer; arming is local).
                        // ⚠ It carries its own predicate too — an empty address
                        // closes it with no nest in sight — so the gate composes
                        // with `isNotBlank()` rather than replacing it.
                        val addGate = faunaGate(
                            "fauna.bridges.add_list_member",
                            enabled = addAddress.isNotBlank(),
                        )
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(
                                onClick = { onAddMember(addAddress); showAdd = false },
                                enabled = addGate.enabled,
                                modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_ADD_SHEET_SUBMIT_BUTTON),
                            ) { Text(stringResource(R.string.mail_lists_add_member_submit)) }
                            OutlinedButton(
                                onClick = { showAdd = false },
                                modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_ADD_SHEET_CANCEL_BUTTON),
                            ) { Text(stringResource(R.string.mail_lists_add_member_cancel)) }
                        }
                        DisabledControlReasonText(addGate.reason)
                    }
                }
            }

            if (lastImport != null && lastImport != dismissedImport) {
                Text(
                    stringResourceFmt(
                        R.string.mail_lists_import_result,
                        lastImport.added.toString(),
                        lastImport.skippedDuplicate.toString(),
                        lastImport.skippedInvalid.toString(),
                    ),
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_IMPORT_RESULT),
                )
            }

            if (showImport) {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        OutlinedTextField(
                            value = importText,
                            onValueChange = { importText = it },
                            label = { Text(stringResource(R.string.mail_lists_import_placeholder)) },
                            minLines = 3,
                            modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_LIST_MEMBERS_IMPORT_SHEET_INPUT),
                        )
                        // Same split as the add sheet: the import textarea and
                        // cancel stay live, the submit declares.
                        val importGate = faunaGate(
                            "fauna.bridges.batch_import_list_members",
                            enabled = importText.isNotBlank(),
                        )
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Button(
                                onClick = {
                                    dismissedImport = null
                                    onBatchImport(importText)
                                    showImport = false
                                },
                                enabled = importGate.enabled,
                                modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_IMPORT_SHEET_SUBMIT_BUTTON),
                            ) { Text(stringResource(R.string.mail_lists_import_submit)) }
                            OutlinedButton(
                                onClick = { showImport = false },
                                modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_IMPORT_SHEET_CANCEL_BUTTON),
                            ) { Text(stringResource(R.string.mail_lists_import_cancel)) }
                        }
                        DisabledControlReasonText(importGate.reason)
                    }
                }
            }

            // Loading vs. loaded (`ui/README.md` rule 5). Android's route
            // always supplies a concrete list id (`bind` is unreachable with
            // no list open — see MailListMembersVM.hydrated), so the only two
            // reachable states are "still loading" and "a real list's
            // snapshot has landed". Unlike the pre-fix code, a real list with
            // zero members renders NO chrome once loaded — matches tui's
            // reference (apps/fauna-tui/src/settings/mail_list_members.rs):
            // Add/Import are enabled by then and the subscribed/unsubscribed
            // count summary above already communicates the state, so a
            // separate empty-state reason would be redundant. This also
            // drops the prior bug that borrowed the Lists page's
            // `mail_lists_empty` ("No lists yet") under a Members heading.
            if (!hydrated) {
                Text(
                    stringResource(R.string.mail_lists_members_loading),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else if (members.isNotEmpty()) {
                members.forEach { member ->
                    MemberRow(
                        member = member,
                        statusLabel = statusLabel,
                        onUnsubscribe = onUnsubscribe,
                        onResubscribe = onResubscribe,
                    )
                }
            }
        }
    }
}

/** One `mail-list-members-list-item` row, projected from a [MemberView]. The
 *  status label comes from the shared `member_status_label` map via [statusLabel]
 *  (injected so this Content stays FFI-free for Robolectric). */
@Composable
private fun MemberRow(
    member: MemberView,
    statusLabel: (MemberStatus) -> String,
    onUnsubscribe: (String) -> Unit,
    onResubscribe: (String) -> Unit,
) {
    val subscribed = member.status == MemberStatus.SUBSCRIBED
    val statusText = statusLabel(member.status)

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_LIST_MEMBERS_LIST_ITEM)) {
        Row(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    member.address,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_LIST_ITEM_ADDRESS),
                )
                Text(
                    member.subscribedAtMs?.toString() ?: "—",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_LIST_ITEM_SUBSCRIBED_AT),
                )
                Text(
                    statusText,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_LIST_ITEM_STATUS),
                )
            }
            // ⚠ NOT a discriminant, despite reading like one. The row renders
            // two DIFFERENT controls with two different ids under an `if` —
            // `subscribed` picks which button *exists*, not which kind one
            // button issues — so each is an ordinary single-kind declaration
            // with its own literal. (Contrast the backups destination form,
            // where one control's kind turns on form state and the gate must be
            // handed the same expression the action takes.)
            if (subscribed) {
                val unsubscribeGate = faunaGate("fauna.bridges.unsubscribe_list_member")
                Column(horizontalAlignment = Alignment.End) {
                    OutlinedButton(
                        onClick = { onUnsubscribe(member.address) },
                        enabled = unsubscribeGate.enabled,
                        modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_LIST_ITEM_UNSUBSCRIBE_BUTTON),
                    ) { Text(stringResource(R.string.mail_lists_unsubscribe)) }
                    DisabledControlReasonText(unsubscribeGate.reason)
                }
            } else {
                val resubscribeGate = faunaGate("fauna.bridges.resubscribe_list_member")
                Column(horizontalAlignment = Alignment.End) {
                    OutlinedButton(
                        onClick = { onResubscribe(member.address) },
                        enabled = resubscribeGate.enabled,
                        modifier = Modifier.testTag(Ids.MAIL_LIST_MEMBERS_LIST_ITEM_RESUBSCRIBE_BUTTON),
                    ) { Text(stringResource(R.string.mail_lists_resubscribe)) }
                    DisabledControlReasonText(resubscribeGate.reason)
                }
            }
        }
    }
}
