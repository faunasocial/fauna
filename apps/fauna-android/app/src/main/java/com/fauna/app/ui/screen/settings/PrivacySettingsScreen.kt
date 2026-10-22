package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.FilterForm
import com.fauna.app.ui.viewmodel.PrivacySettingsVM
import com.fauna.ffi.FfiEmailFilter
import com.fauna.ffi.FfiFilterActionInputs
import com.fauna.ffi.FfiFilterRuleInputs
import com.fauna.ffi.inboxModeOptions
import social.fauna.generated.Ids

/**
 * The `privacy` settings page (`settings.md`): inbox mode, email filters, and the
 * spam-preferences controls.
 *
 * Stateless [PrivacySettingsContent] is split out so it renders under the Compose
 * test harness with seeded state — the VM-bound [PrivacySettingsScreen] is the thin
 * wrapper the NavHost mounts (mirrors the sibling settings screens, e.g.
 * MailSpamScreen). The spam controls consume the **shared** presentation contract
 * (`fauna_protocol::spam` via the fauna-ffi UniFFI face): the threshold band
 * label comes from `spam_threshold_band()`, so Android never invents its own band
 * buckets (priority #2/#4; settings.md § Spam threshold slider labels).
 * That FFI call lives in the VM, off this stateless Content.
 */
@Composable
fun PrivacySettingsScreen(
    navController: NavController,
    vm: PrivacySettingsVM = hiltViewModel()
) {
    val inboxMode by vm.inboxMode.collectAsState()
    val inboxModeLoading by vm.inboxModeLoading.collectAsState()
    val inboxModeError by vm.inboxModeError.collectAsState()

    val emailFilters by vm.emailFilters.collectAsState()
    val emailFilterError by vm.emailFilterError.collectAsState()
    val creatingFilter by vm.creatingFilter.collectAsState()
    val editingFilterId by vm.editingFilterId.collectAsState()
    val editFilterPrefill by vm.editFilterPrefill.collectAsState()
    val filterMarks by vm.filterMarks.collectAsState()

    val spamThreshold by vm.spamThreshold.collectAsState()
    val phishingThreshold by vm.phishingThreshold.collectAsState()
    val spamPrefsLoading by vm.spamPrefsLoading.collectAsState()
    val spamPrefsSaved by vm.spamPrefsSaved.collectAsState()
    val spamPrefsError by vm.spamPrefsError.collectAsState()

    LaunchedEffect(Unit) { vm.loadAll() }

    PrivacySettingsContent(
        inboxMode = inboxMode,
        inboxModeLoading = inboxModeLoading,
        inboxModeError = inboxModeError,
        emailFilters = emailFilters,
        emailFilterError = emailFilterError,
        creatingFilter = creatingFilter,
        editingFilterId = editingFilterId,
        editFilterPrefill = editFilterPrefill,
        isFilterEditable = vm::isFilterEditable,
        spamThreshold = spamThreshold,
        phishingThreshold = phishingThreshold,
        spamPrefsLoading = spamPrefsLoading,
        spamPrefsSaved = spamPrefsSaved,
        spamPrefsError = spamPrefsError,
        bandKeyFor = vm::spamBandKey,
        onBack = { navController.popBackStack() },
        onInboxModeChange = vm::updateInboxMode,
        onDeleteFilter = vm::deleteFilter,
        filterMarks = filterMarks,
        onKeepFilterMark = vm::keepFilterMark,
        onCreateFilter = vm::createFilter,
        onEditFilter = vm::beginEditFilter,
        onSaveFilter = vm::saveFilter,
        onCancelEditFilter = vm::cancelEditFilter,
        onSpamThresholdChange = { vm.spamThreshold.value = it },
        onPhishingThresholdChange = { vm.phishingThreshold.value = it },
        onSaveSpamPrefs = vm::saveSpamPreferences,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PrivacySettingsContent(
    inboxMode: String?,
    inboxModeLoading: Boolean,
    inboxModeError: String?,
    emailFilters: List<FfiEmailFilter>,
    emailFilterError: String?,
    creatingFilter: Boolean,
    editingFilterId: Long?,
    editFilterPrefill: FilterForm?,
    isFilterEditable: (FfiEmailFilter) -> Boolean,
    spamThreshold: Float,
    phishingThreshold: Float,
    spamPrefsLoading: Boolean,
    spamPrefsSaved: Boolean,
    spamPrefsError: String?,
    bandKeyFor: (Float) -> String,
    onBack: () -> Unit,
    onInboxModeChange: (String) -> Unit,
    onDeleteFilter: (Long) -> Unit,
    onCreateFilter: (FilterForm) -> Unit,
    onEditFilter: (Long) -> Unit,
    onSaveFilter: (FilterForm) -> Unit,
    onCancelEditFilter: () -> Unit,
    onSpamThresholdChange: (Float) -> Unit,
    onPhishingThresholdChange: (Float) -> Unit,
    onSaveSpamPrefs: () -> Unit,
    // The ids of the rules awaiting the owner's post-succession verdict
    // (`succession-aftermath.md` § Adjudicating what the aftermath carries
    // across) — empty by default, so a harness that never seeds them renders
    // no mark.
    filterMarks: List<Long> = emptyList(),
    onKeepFilterMark: (Long) -> Unit = {},
) {
    // showCreateForm opens the shared form empty; editingFilterId (VM state,
    // set only after a successful filters_get decode) opens it pre-populated
    // — the sheet's own visibility follows either.
    var showCreateForm by remember { mutableStateOf(false) }
    val showFilterForm = showCreateForm || editingFilterId != null

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.settings_privacy),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.common_back))
                    }
                }
            )
        }
    ) { padding ->
        LazyColumn(
            modifier = Modifier.padding(padding).fillMaxSize(),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp)
        ) {
            // Section 1: Inbox Mode
            item {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(8.dp)
                        ) {
                            Text(stringResource(R.string.status_inbox_privacy_title), style = MaterialTheme.typography.titleMedium)
                            if (inboxModeLoading) {
                                CircularProgressIndicator(
                                    modifier = Modifier.size(16.dp),
                                    strokeWidth = 2.dp
                                )
                            }
                        }
                        Spacer(Modifier.height(8.dp))

                        // Canonical inbox-mode radio group. The wire values sent to
                        // the nest are open/allow_knock/contacts_only/closed — the
                        // only set the nest accepts (bins/fauna-nest/src/db/contacts.rs
                        // VALID_INBOX_MODES) and the set ui.yaml `inbox-mode-selector`
                        // + the linux/apple lead render (apps/fauna-linux/src/settings/
                        // privacy.rs, FaunaKit PrivacySettingsView). Each row's test id
                        // is `inbox-mode-<wire-value>` so the shared e2e actuates the
                        // same element on every app; the catalog (values, labels,
                        // descriptions, canonical order) comes from the shared
                        // `inboxModeOptions()` UniFFI door (`fauna-ffi/src/contacts_
                        // client.rs`; settings.md § Where logic lives → *The inbox-mode
                        // selector's rows*), not a hand-rolled list.
                        val inboxModeCatalog = remember { inboxModeOptions() }
                        inboxModeCatalog.forEach { option ->
                            Row(
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .selectable(
                                        selected = inboxMode == option.value,
                                        enabled = !inboxModeLoading,
                                        role = Role.RadioButton,
                                        onClick = { onInboxModeChange(option.value) }
                                    )
                                    .testTag("inbox-mode-${option.value}")
                                    .padding(vertical = 8.dp),
                                verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.spacedBy(12.dp)
                            ) {
                                RadioButton(
                                    selected = inboxMode == option.value,
                                    onClick = null
                                )
                                Column(modifier = Modifier.weight(1f)) {
                                    Text(localized(option.label) ?: option.value, style = MaterialTheme.typography.bodyLarge)
                                    Text(
                                        localized(option.desc) ?: "",
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant
                                    )
                                }
                            }
                        }

                        if (inboxModeError != null) {
                            Spacer(Modifier.height(4.dp))
                            Text(inboxModeError, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                        } else if (inboxMode == null) {
                            // Painted while the real mode is unfetched — the radios
                            // above already show none selected (`==` against a null
                            // inboxMode is false for every option), and this names
                            // why (settings.md § Privacy sub-page item 7). Mirrors
                            // the apple leg's PrivacySettingsView.swift.
                            Spacer(Modifier.height(4.dp))
                            Text(
                                stringResource(R.string.settings_privacy_page_inbox_mode_unknown),
                                color = MaterialTheme.colorScheme.error,
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                    }
                }
            }

            // Section 2: Email Filters
            item {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text(stringResource(R.string.settings_privacy_page_email_filters), style = MaterialTheme.typography.titleMedium)
                        Spacer(Modifier.height(8.dp))

                        if (emailFilters.isEmpty()) {
                            Text(
                                stringResource(R.string.settings_privacy_page_no_filters),
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                style = MaterialTheme.typography.bodyMedium
                            )
                        }
                    }
                }
            }

            if (emailFilters.isNotEmpty()) {
                items(emailFilters) { filter ->
                    val actionLabel = localized(com.fauna.ffi.emailFilterActionLabel(filter.action)).orEmpty()
                    ListItem(
                        headlineContent = { Text(filter.name, modifier = Modifier.testTag(Ids.FILTER_NAME)) },
                        // The post-succession review mark and its Keep half —
                        // only on a rule the aftermath carried across and the
                        // owner has not answered. No remove button of its own:
                        // `filter-delete` below is the Remove half (no second
                        // removal mechanism; the VM records the verdict after
                        // the delete lands).
                        supportingContent = if (filter.id in filterMarks) {
                            {
                                Column {
                                    Text(
                                        stringResource(R.string.settings_privacy_page_filter_inherited),
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        modifier = Modifier.testTag(Ids.FILTER_UNATTESTED_MARK),
                                    )
                                    TextButton(
                                        onClick = { onKeepFilterMark(filter.id) },
                                        modifier = Modifier.testTag(Ids.FILTER_REVIEW_KEEP_BUTTON),
                                    ) {
                                        Text(stringResource(R.string.settings_privacy_page_filter_keep))
                                    }
                                }
                            }
                        } else {
                            null
                        },
                        trailingContent = {
                            Row(
                                verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.spacedBy(4.dp)
                            ) {
                                Text(
                                    actionLabel,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    modifier = Modifier.testTag(Ids.FILTER_ACTION)
                                )
                                // Gated: a filter only a raw API call could have
                                // produced (multi-rule, or a richer rule/action
                                // no dialog collects) never opens a form that
                                // would silently narrow it on save.
                                if (isFilterEditable(filter)) {
                                    IconButton(
                                        onClick = { onEditFilter(filter.id) },
                                        modifier = Modifier.testTag(Ids.FILTER_EDIT)
                                    ) {
                                        Icon(Icons.Default.Edit, contentDescription = stringResource(R.string.settings_privacy_page_edit_filter))
                                    }
                                }
                                IconButton(
                                    onClick = { onDeleteFilter(filter.id) },
                                    modifier = Modifier.testTag(Ids.FILTER_DELETE)
                                ) {
                                    Icon(Icons.Default.Delete, contentDescription = stringResource(R.string.settings_privacy_page_delete_filter))
                                }
                            }
                        },
                        modifier = Modifier.testTag(Ids.FILTER_ITEM)
                    )
                }
            }

            item {
                emailFilterError?.let {
                    Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                    Spacer(Modifier.height(4.dp))
                }

                TextButton(
                    onClick = { showCreateForm = true },
                    modifier = Modifier.testTag(Ids.ADD_FILTER_BTN)
                ) {
                    Text(stringResource(R.string.settings_privacy_page_add_filter))
                }
            }

            // Section 3: Spam Preferences
            item {
                Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SPAM_PREFERENCES)) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text(stringResource(R.string.settings_privacy_page_spam_preferences), style = MaterialTheme.typography.titleMedium)
                        Spacer(Modifier.height(8.dp))

                        // Threshold band label off the shared contract (no Kotlin-side
                        // buckets): per-mille = value * 1000 (settings.md § Spam threshold
                        // slider labels). 0.0–1.0 by step 0.1 ⇒ steps = 9 intermediate stops.
                        val bandLabel = when (bandKeyFor(spamThreshold)) {
                            "aggressive" -> stringResource(R.string.status_spam_aggressive)
                            "moderate" -> stringResource(R.string.status_spam_moderate)
                            else -> stringResource(R.string.status_spam_permissive)
                        }
                        Text(
                            "${stringResource(R.string.status_spam_spam_threshold)}: ${String.format("%.2f", spamThreshold)}",
                            style = MaterialTheme.typography.bodyMedium
                        )
                        Slider(
                            value = spamThreshold,
                            onValueChange = onSpamThresholdChange,
                            valueRange = 0f..1f,
                            steps = 9,
                            modifier = Modifier.testTag(Ids.SPAM_THRESHOLD)
                        )
                        Text(
                            bandLabel,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.testTag(Ids.SPAM_THRESHOLD_BAND)
                        )

                        Spacer(Modifier.height(8.dp))

                        Text(
                            "${stringResource(R.string.status_spam_phishing_threshold)}: ${String.format("%.2f", phishingThreshold)}",
                            style = MaterialTheme.typography.bodyMedium
                        )
                        Slider(
                            value = phishingThreshold,
                            onValueChange = onPhishingThresholdChange,
                            valueRange = 0f..1f,
                            steps = 9,
                            modifier = Modifier.testTag(Ids.PHISHING_THRESHOLD)
                        )

                        Spacer(Modifier.height(16.dp))

                        Button(
                            onClick = onSaveSpamPrefs,
                            enabled = !spamPrefsLoading,
                            modifier = Modifier.testTag(Ids.SAVE_SPAM_PREFS)
                        ) {
                            Text(stringResource(R.string.common_save_preferences))
                        }

                        if (spamPrefsSaved) {
                            Spacer(Modifier.height(4.dp))
                            Text(
                                stringResource(R.string.common_saved),
                                color = Color(0xFF4CAF50),
                                style = MaterialTheme.typography.bodySmall
                            )
                        }

                        spamPrefsError?.let {
                            Spacer(Modifier.height(4.dp))
                            Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                        }
                    }
                }
            }
        }
    }

    // Filter Form Bottom Sheet — shared between create and edit (create-
    // filter / save-filter's counterpart, same fields). `key(editingFilterId)`
    // forces fresh `remember` state each time the identity changes (a
    // different filter, or back to create), so edit-1 -> edit-2 doesn't leak
    // edit-1's fields into edit-2's sheet.
    if (showFilterForm) {
        key(editingFilterId) {
            FilterFormSheet(
                creatingFilter = creatingFilter,
                editingFilterId = editingFilterId,
                prefill = editFilterPrefill,
                onDismiss = {
                    showCreateForm = false
                    onCancelEditFilter()
                },
                onCreate = onCreateFilter,
                onSave = onSaveFilter,
            )
        }
    }
}

/** One labelled text field of the filter form, tagged with its ui.yaml id. */
@Composable
private fun FilterField(value: String, onValueChange: (String) -> Unit, label: Int, tag: String) {
    Spacer(Modifier.height(8.dp))
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(stringResource(label)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().testTag(tag)
    )
}

/** One kind dropdown of the filter form (the condition's or the action's). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun FilterKindDropdown(selected: String, options: List<String>, label: Int, tag: String, onSelect: (String) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { expanded = !expanded },
        modifier = Modifier.testTag(tag)
    ) {
        OutlinedTextField(
            value = selected,
            onValueChange = {},
            readOnly = true,
            label = { Text(stringResource(label)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier.menuAnchor().fillMaxWidth()
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(option) },
                    onClick = {
                        onSelect(option)
                        expanded = false
                    }
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun FilterFormSheet(
    creatingFilter: Boolean,
    editingFilterId: Long?,
    prefill: FilterForm?,
    onDismiss: () -> Unit,
    onCreate: (FilterForm) -> Unit,
    onSave: (FilterForm) -> Unit,
) {
    // The whole form is one value (settings.md § Email filter create-dialog
    // encoding): the condition and action inputs the shared encoders read, so
    // an edit writes back every input it was saved with (the Reject reason
    // included), plus the rule's order and its `continue` flag.
    var form by remember {
        mutableStateOf(
            prefill ?: FilterForm(
                name = "",
                rule = FfiFilterRuleInputs(kind = "SenderIs", value = ""),
                action = FfiFilterActionInputs(
                    kind = "Allow", rejectReason = "", forwardAddress = "", keepLocalCopy = true,
                ),
                priority = "",
                continueOnMatch = false,
            )
        )
    }
    val rule = form.rule
    val action = form.action

    // Dropdown tags are the canonical PascalCase variant names the shared
    // encoders consume (`SUPPORTED_RULE_KINDS` / `SUPPORTED_ACTION_KINDS`).
    val ruleTypeOptions = listOf(
        "SenderIs", "SenderDomain", "SubjectContains", "BodyContains", "HeaderExists", "HeaderContains", "SpamScoreAtLeast",
    )
    val actionOptions = listOf("Allow", "Discard", "Reject", "Forward", "FileInto", "AddLabel", "AutoReply")

    // Dismiss sheet when creation completes
    var wasCreating by remember { mutableStateOf(false) }
    LaunchedEffect(creatingFilter) {
        if (wasCreating && !creatingFilter) {
            onDismiss()
        }
        wasCreating = creatingFilter
    }

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .padding(horizontal = 24.dp, vertical = 16.dp)
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
        ) {
            Text(
                if (editingFilterId != null) stringResource(R.string.common_edit)
                else stringResource(R.string.settings_privacy_page_new_filter),
                style = MaterialTheme.typography.titleMedium
            )
            Spacer(Modifier.height(8.dp))
            FilterField(form.name, { form = form.copy(name = it) }, R.string.common_name, Ids.FILTER_NAME_INPUT)
            Spacer(Modifier.height(8.dp))
            FilterKindDropdown(rule.kind, ruleTypeOptions, R.string.settings_rule_type, Ids.FILTER_RULE_TYPE) {
                form = form.copy(rule = rule.copy(kind = it))
            }
            FilterField(rule.value, { form = form.copy(rule = rule.copy(value = it)) }, R.string.settings_rule_value, Ids.FILTER_RULE_VALUE)
            // The two conditions with a second input (email-filters.md § Email
            // filter rules): a header's value beside its name, and a spam score.
            when (rule.kind) {
                "HeaderContains" -> FilterField(
                    rule.headerValue, { form = form.copy(rule = rule.copy(headerValue = it)) },
                    R.string.settings_privacy_page_header_value, Ids.FILTER_RULE_HEADER_VALUE,
                )
                "SpamScoreAtLeast" -> FilterField(
                    rule.spamScore, { form = form.copy(rule = rule.copy(spamScore = it)) },
                    R.string.settings_privacy_page_spam_score, Ids.FILTER_RULE_SPAM_SCORE,
                )
            }
            Spacer(Modifier.height(8.dp))
            FilterKindDropdown(action.kind, actionOptions, R.string.settings_privacy_page_action, Ids.FILTER_ACTION_SELECT) {
                form = form.copy(action = action.copy(kind = it))
            }
            // Each action's own inputs, present only while it is selected.
            when (action.kind) {
                "Forward" -> {
                    FilterField(
                        action.forwardAddress, { form = form.copy(action = action.copy(forwardAddress = it)) },
                        R.string.settings_privacy_page_forward_address, Ids.FILTER_FORWARD_ADDRESS,
                    )
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth()
                    ) {
                        Checkbox(
                            checked = action.keepLocalCopy,
                            onCheckedChange = { form = form.copy(action = action.copy(keepLocalCopy = it)) },
                            modifier = Modifier.testTag(Ids.FILTER_KEEP_LOCAL_COPY)
                        )
                        Text(stringResource(R.string.settings_privacy_page_keep_local_copy))
                    }
                }
                "FileInto" -> FilterField(
                    action.fileIntoMailbox, { form = form.copy(action = action.copy(fileIntoMailbox = it)) },
                    R.string.settings_privacy_page_file_into_mailbox, Ids.FILTER_FILE_INTO_MAILBOX,
                )
                "AddLabel" -> FilterField(
                    action.label, { form = form.copy(action = action.copy(label = it)) },
                    R.string.settings_privacy_page_add_label, Ids.FILTER_ADD_LABEL,
                )
                "AutoReply" -> {
                    FilterField(
                        action.autoReplySubject, { form = form.copy(action = action.copy(autoReplySubject = it)) },
                        R.string.settings_privacy_page_auto_reply_subject, Ids.FILTER_AUTO_REPLY_SUBJECT,
                    )
                    FilterField(
                        action.autoReplyBody, { form = form.copy(action = action.copy(autoReplyBody = it)) },
                        R.string.settings_privacy_page_auto_reply_body, Ids.FILTER_AUTO_REPLY_BODY,
                    )
                    FilterField(
                        action.autoReplyIntervalHours,
                        { form = form.copy(action = action.copy(autoReplyIntervalHours = it)) },
                        R.string.settings_privacy_page_auto_reply_interval_hours, Ids.FILTER_AUTO_REPLY_INTERVAL_HOURS,
                    )
                }
            }
            // The rule's place in the order and its `continue` flag: rules run
            // in order, the first match wins unless it is marked to keep going.
            FilterField(
                form.priority, { form = form.copy(priority = it) },
                R.string.settings_privacy_page_priority, Ids.FILTER_PRIORITY_INPUT,
            )
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth()
            ) {
                Checkbox(
                    checked = form.continueOnMatch,
                    onCheckedChange = { form = form.copy(continueOnMatch = it) },
                    modifier = Modifier.testTag(Ids.FILTER_CONTINUE_TOGGLE)
                )
                Text(stringResource(R.string.settings_privacy_page_continue_matching))
            }
            Spacer(Modifier.height(16.dp))

            if (editingFilterId == null) {
                Button(
                    onClick = { onCreate(form) },
                    enabled = !creatingFilter && form.name.isNotBlank(),
                    modifier = Modifier.fillMaxWidth().testTag(Ids.CREATE_FILTER)
                ) {
                    Text(stringResource(R.string.common_create))
                }
            } else {
                Button(
                    onClick = { onSave(form) },
                    enabled = !creatingFilter && form.name.isNotBlank(),
                    modifier = Modifier.fillMaxWidth().testTag(Ids.SAVE_FILTER)
                ) {
                    Text(stringResource(R.string.common_save))
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}
