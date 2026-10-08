package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.components.rememberTwoClickArm
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.viewmodel.MailListsVM
import uniffi.fauna_client_mail_settings.ListDraft
import uniffi.fauna_client_mail_settings.ListView
import uniffi.fauna_client_mail_settings.ListsStatus
import java.net.URLEncoder
import social.fauna.generated.Ids

/**
 * The per-account `mail-lists` page (`mail-mass-mailing.md`): list / create /
 * edit / delete mailing lists, with a per-row "view members" jump to
 * [MailListMembersScreen]. A sub-page of the mail-settings hub.
 *
 * Stateless [MailListsContent] is split out for the Compose test harness; the
 * VM-bound [MailListsScreen] is the wrapper the NavHost mounts. The backend is
 * live; create/update/delete dispatch through the shared machine to the real
 * RPCs, with a failure surfacing via the global error-message banner. Mirrors
 * the Linux lead (apps/fauna-linux/src/settings/mail_lists.rs).
 */
@Composable
fun MailListsScreen(
    navController: NavController,
    vm: MailListsVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val hydrated by vm.hydrated.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    MailListsContent(
        lists = snapshot.lists,
        localDomains = snapshot.localDomains,
        hydrated = hydrated,
        working = snapshot.status == ListsStatus.WORKING,
        onBack = { navController.popBackStack() },
        onCreate = vm::create,
        onUpdate = vm::update,
        onDelete = vm::delete,
        onViewMembers = { list ->
            val name = URLEncoder.encode(list.friendlyName, "UTF-8")
            navController.navigate("settings/mail-list-members/${list.listIdHex}/$name")
        },
    )
}

private sealed interface ListSheetMode {
    object Closed : ListSheetMode
    object Add : ListSheetMode
    data class Edit(val list: ListView) : ListSheetMode
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MailListsContent(
    lists: List<ListView>,
    localDomains: List<String>,
    // Loading-vs-resolved-empty (`ui/README.md` rule 5). Defaults true so
    // existing/other-purpose call sites keep rendering the resolved state.
    hydrated: Boolean = true,
    working: Boolean,
    onBack: () -> Unit,
    onCreate: (ListDraft) -> Unit,
    onUpdate: (String, ListDraft) -> Unit,
    onDelete: (String) -> Unit,
    onViewMembers: (ListView) -> Unit,
) {
    var sheet by remember { mutableStateOf<ListSheetMode>(ListSheetMode.Closed) }
    val canAdd = localDomains.isNotEmpty() && !working

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.mail_lists_title),
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
                stringResource(R.string.mail_lists_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (localDomains.isEmpty()) {
                Text(
                    stringResource(R.string.mail_lists_no_domain),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }
            Button(
                onClick = { sheet = ListSheetMode.Add },
                enabled = canAdd,
                modifier = Modifier.testTag(Ids.MAIL_LISTS_ADD_BUTTON),
            ) { Text(stringResource(R.string.mail_lists_add_button)) }

            when (val mode = sheet) {
                is ListSheetMode.Add -> ListSheet(
                    editing = null,
                    localDomains = localDomains,
                    onSubmit = { draft -> onCreate(draft); sheet = ListSheetMode.Closed },
                    onCancel = { sheet = ListSheetMode.Closed },
                )
                is ListSheetMode.Edit -> ListSheet(
                    editing = mode.list,
                    localDomains = localDomains,
                    onSubmit = { draft -> onUpdate(mode.list.listIdHex, draft); sheet = ListSheetMode.Closed },
                    onCancel = { sheet = ListSheetMode.Closed },
                )
                ListSheetMode.Closed -> {}
            }

            // Loading vs. resolved-empty (`ui/README.md` rule 5): an
            // un-hydrated first paint must not claim "No lists yet".
            if (!hydrated) {
                Text(
                    stringResource(R.string.mail_lists_loading),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else if (lists.isEmpty()) {
                Text(
                    stringResource(R.string.mail_lists_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                lists.forEach { list ->
                    ListRow(
                        list = list,
                        working = working,
                        onEdit = { sheet = ListSheetMode.Edit(list) },
                        onViewMembers = { onViewMembers(list) },
                        onDelete = { onDelete(list.listIdHex) },
                    )
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ListSheet(
    editing: ListView?,
    localDomains: List<String>,
    onSubmit: (ListDraft) -> Unit,
    onCancel: () -> Unit,
) {
    var name by remember(editing) { mutableStateOf(editing?.friendlyName ?: "") }
    var localPart by remember(editing) { mutableStateOf(editing?.localPart ?: "") }
    var domain by remember(editing) {
        mutableStateOf(editing?.localDomain ?: localDomains.firstOrNull() ?: "")
    }
    var description by remember(editing) { mutableStateOf(editing?.description ?: "") }
    var helpUrl by remember(editing) { mutableStateOf(editing?.listHelpUrl ?: "") }
    var archiveUrl by remember(editing) { mutableStateOf(editing?.listArchiveUrl ?: "") }
    var perSend by remember(editing) { mutableStateOf(editing?.recipientsPerSend?.toString() ?: "") }
    var domainExpanded by remember { mutableStateOf(false) }

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.mail_lists_form_title), style = MaterialTheme.typography.titleMedium)

            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text(stringResource(R.string.mail_lists_name_placeholder)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_LISTS_ADD_SHEET_NAME_INPUT),
            )
            OutlinedTextField(
                value = localPart,
                onValueChange = { localPart = it },
                label = { Text(stringResource(R.string.mail_lists_local_part_placeholder)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_LISTS_ADD_SHEET_LOCAL_PART_INPUT),
            )
            // Domain picker over the user's owned domains.
            ExposedDropdownMenuBox(
                expanded = domainExpanded,
                onExpandedChange = { domainExpanded = it },
                modifier = Modifier.testTag(Ids.MAIL_LISTS_ADD_SHEET_DOMAIN_PICKER),
            ) {
                OutlinedTextField(
                    value = domain,
                    onValueChange = {},
                    readOnly = true,
                    label = { Text(stringResource(R.string.mail_lists_domain_label)) },
                    trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = domainExpanded) },
                    modifier = Modifier.fillMaxWidth().menuAnchor(),
                )
                ExposedDropdownMenu(expanded = domainExpanded, onDismissRequest = { domainExpanded = false }) {
                    localDomains.forEach { d ->
                        DropdownMenuItem(text = { Text(d) }, onClick = { domain = d; domainExpanded = false })
                    }
                }
            }
            OutlinedTextField(
                value = description,
                onValueChange = { description = it },
                label = { Text(stringResource(R.string.mail_lists_description_placeholder)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_LISTS_ADD_SHEET_DESCRIPTION_INPUT),
            )
            OutlinedTextField(
                value = helpUrl,
                onValueChange = { helpUrl = it },
                label = { Text(stringResource(R.string.mail_lists_list_help_placeholder)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_LISTS_ADD_SHEET_LIST_HELP_URL_INPUT),
            )
            OutlinedTextField(
                value = archiveUrl,
                onValueChange = { archiveUrl = it },
                label = { Text(stringResource(R.string.mail_lists_list_archive_placeholder)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_LISTS_ADD_SHEET_LIST_ARCHIVE_URL_INPUT),
            )
            OutlinedTextField(
                value = perSend,
                onValueChange = { perSend = it.filter(Char::isDigit) },
                label = { Text(stringResource(R.string.mail_lists_per_send_placeholder)) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_LISTS_ADD_SHEET_PER_SEND_CAP_INPUT),
            )

            // ⚠ A DISCRIMINANT: one submit, one composable, two kinds. This
            // sheet serves both call sites — Add binds `onSubmit` to `onCreate`,
            // Edit binds it to `onUpdate` — and the two are DIFFERENT registered
            // kinds, so the gate is handed the same expression the action turns
            // on rather than a Kotlin class test or a coin-flip guess. tui, the
            // oracle this page is measured against, carries the identical mode
            // on its own action (`MailListsSubmit { editing }`,
            // `settings/mod.rs:3205-3206`) for exactly this reason: were either
            // arm ever reclassified, a single-kind declaration would silently
            // become wrong. `editing` is the sheet's own parameter and is what
            // seeds every field below, so it cannot drift from `onSubmit`'s
            // binding without the form visibly showing the wrong list.
            val submitGate = faunaGate(
                if (editing != null) "fauna.bridges.update_account_list"
                else "fauna.bridges.create_account_list",
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = {
                        onSubmit(
                            ListDraft(
                                friendlyName = name,
                                localPart = localPart,
                                localDomain = domain,
                                description = description,
                                listHelpUrl = helpUrl,
                                listArchiveUrl = archiveUrl,
                                recipientsPerSend = perSend.toUIntOrNull(),
                            )
                        )
                    },
                    enabled = submitGate.enabled,
                    modifier = Modifier.testTag(Ids.MAIL_LISTS_ADD_SHEET_SUBMIT_BUTTON),
                ) { Text(stringResource(R.string.mail_lists_submit)) }
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier.testTag(Ids.MAIL_LISTS_ADD_SHEET_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.mail_lists_cancel)) }
            }
            DisabledControlReasonText(submitGate.reason)
        }
    }
}

/** One `mail-lists-list-item` row, projected from a [ListView]. */
@Composable
private fun ListRow(
    list: ListView,
    working: Boolean,
    onEdit: () -> Unit,
    onViewMembers: () -> Unit,
    onDelete: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_LISTS_LIST_ITEM)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                "${list.friendlyName} · ${list.address}",
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.MAIL_LISTS_LIST_ITEM_NAME),
            )
            Text(
                "${list.memberCount}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.MAIL_LISTS_LIST_ITEM_MEMBER_COUNT),
            )
            Text(
                list.lastSendAtMs?.toString() ?: "—",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.MAIL_LISTS_LIST_ITEM_LAST_SEND),
            )
            Text(
                "${list.sendsToday} / ${list.recipientsToday}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.MAIL_LISTS_LIST_ITEM_QUOTA),
            )
            // Edit REVEALS the sheet (arming is local) and Members NAVIGATES —
            // both stay live with no nest, and they are this row's live
            // siblings beside the dead delete, so a blanket grey cannot pass.
            val deleteGate = faunaGate("fauna.bridges.delete_account_list", enabled = !working)
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(
                    onClick = onEdit,
                    enabled = !working,
                    modifier = Modifier.testTag(Ids.MAIL_LISTS_LIST_ITEM_EDIT_BUTTON),
                ) { Text(stringResource(R.string.mail_lists_edit)) }
                OutlinedButton(
                    onClick = onViewMembers,
                    modifier = Modifier.testTag(Ids.MAIL_LISTS_LIST_ITEM_MEMBERS_BUTTON),
                ) { Text(stringResource(R.string.mail_lists_members)) }
                // Two-click confirm (`common.md`): the armed label says the
                // members go with the list.
                val deleteArm = rememberTwoClickArm()
                OutlinedButton(
                    onClick = { deleteArm.press(onDelete) },
                    enabled = deleteGate.enabled,
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.testTag(Ids.MAIL_LISTS_LIST_ITEM_DELETE_BUTTON),
                ) {
                    Text(
                        stringResource(
                            if (deleteArm.armed) R.string.mail_lists_delete_confirm
                            else R.string.mail_lists_delete,
                        ),
                    )
                }
            }
            DisabledControlReasonText(deleteGate.reason)
        }
    }
}
