package com.fauna.app.ui.screen.settings

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
import com.fauna.app.ui.viewmodel.FilterPrefill
import com.fauna.app.ui.viewmodel.PrivacySettingsVM
import com.fauna.ffi.FfiEmailFilter
import com.fauna.ffi.FfiFilterActionInputs
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
    editFilterPrefill: FilterPrefill?,
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
    onCreateFilter: (name: String, ruleType: String, ruleValue: String, action: FfiFilterActionInputs) -> Unit,
    onEditFilter: (Long) -> Unit,
    onSaveFilter: (name: String, ruleType: String, ruleValue: String, action: FfiFilterActionInputs) -> Unit,
    onCancelEditFilter: () -> Unit,
    onSpamThresholdChange: (Float) -> Unit,
    onPhishingThresholdChange: (Float) -> Unit,
    onSaveSpamPrefs: () -> Unit,
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
                            color = MaterialTheme.colorScheme.onSurfaceVariant
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
                onCreate = { name, ruleType, ruleValue, action ->
                    onCreateFilter(name, ruleType, ruleValue, action)
                },
                onSave = { name, ruleType, ruleValue, action ->
                    onSaveFilter(name, ruleType, ruleValue, action)
                }
            )
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun FilterFormSheet(
    creatingFilter: Boolean,
    editingFilterId: Long?,
    prefill: FilterPrefill?,
    onDismiss: () -> Unit,
    onCreate: (name: String, ruleType: String, ruleValue: String, action: FfiFilterActionInputs) -> Unit,
    onSave: (name: String, ruleType: String, ruleValue: String, action: FfiFilterActionInputs) -> Unit,
) {
    var name by remember { mutableStateOf(prefill?.name ?: "") }
    var ruleType by remember { mutableStateOf(prefill?.ruleType ?: "SenderIs") }
    var ruleValue by remember { mutableStateOf(prefill?.ruleValue ?: "") }
    // The whole inputs struct is the form state (settings.md § Email filter
    // create-dialog encoding): the Reject reason rides along on an edit and a
    // Forward's destination + copy mode sit beside the action tag.
    var action by remember {
        mutableStateOf(
            prefill?.action ?: FfiFilterActionInputs(
                kind = "Allow", rejectReason = "", forwardAddress = "", keepLocalCopy = true,
            )
        )
    }

    var ruleTypeExpanded by remember { mutableStateOf(false) }
    var actionExpanded by remember { mutableStateOf(false) }

    // Dropdown tags are the canonical PascalCase variant names — the shared
    // encoder (encodeEmailFilterRule / encodeEmailFilterActionInputs) consumes them
    // directly, so every app emits the same wire shape (settings.md § Email
    // filter create-dialog encoding: the 5 single-string rules + 3 actions).
    val ruleTypeOptions = listOf("SenderIs", "SenderDomain", "SubjectContains", "BodyContains", "HeaderExists")
    val actionOptions = listOf("Allow", "Discard", "Reject", "Forward")

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
        ) {
            Text(
                if (editingFilterId != null) stringResource(R.string.common_edit)
                else stringResource(R.string.settings_privacy_page_new_filter),
                style = MaterialTheme.typography.titleMedium
            )
            Spacer(Modifier.height(16.dp))

            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text(stringResource(R.string.common_name)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.FILTER_NAME_INPUT)
            )
            Spacer(Modifier.height(8.dp))

            ExposedDropdownMenuBox(
                expanded = ruleTypeExpanded,
                onExpandedChange = { ruleTypeExpanded = !ruleTypeExpanded },
                modifier = Modifier.testTag(Ids.FILTER_RULE_TYPE)
            ) {
                OutlinedTextField(
                    value = ruleType,
                    onValueChange = {},
                    readOnly = true,
                    label = { Text(stringResource(R.string.settings_rule_type)) },
                    trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = ruleTypeExpanded) },
                    modifier = Modifier.menuAnchor().fillMaxWidth()
                )
                ExposedDropdownMenu(
                    expanded = ruleTypeExpanded,
                    onDismissRequest = { ruleTypeExpanded = false }
                ) {
                    ruleTypeOptions.forEach { option ->
                        DropdownMenuItem(
                            text = { Text(option) },
                            onClick = {
                                ruleType = option
                                ruleTypeExpanded = false
                            }
                        )
                    }
                }
            }
            Spacer(Modifier.height(8.dp))

            OutlinedTextField(
                value = ruleValue,
                onValueChange = { ruleValue = it },
                label = { Text(stringResource(R.string.settings_rule_value)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.FILTER_RULE_VALUE)
            )
            Spacer(Modifier.height(8.dp))

            ExposedDropdownMenuBox(
                expanded = actionExpanded,
                onExpandedChange = { actionExpanded = !actionExpanded },
                modifier = Modifier.testTag(Ids.FILTER_ACTION_SELECT)
            ) {
                OutlinedTextField(
                    value = action.kind,
                    onValueChange = {},
                    readOnly = true,
                    label = { Text(stringResource(R.string.settings_privacy_page_action)) },
                    trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = actionExpanded) },
                    modifier = Modifier.menuAnchor().fillMaxWidth()
                )
                ExposedDropdownMenu(
                    expanded = actionExpanded,
                    onDismissRequest = { actionExpanded = false }
                ) {
                    actionOptions.forEach { option ->
                        DropdownMenuItem(
                            text = { Text(option) },
                            onClick = {
                                action = action.copy(kind = option)
                                actionExpanded = false
                            }
                        )
                    }
                }
            }
            if (action.kind == "Forward") {
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = action.forwardAddress,
                    onValueChange = { action = action.copy(forwardAddress = it) },
                    label = { Text(stringResource(R.string.settings_privacy_page_forward_address)) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().testTag(Ids.FILTER_FORWARD_ADDRESS)
                )
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Checkbox(
                        checked = action.keepLocalCopy,
                        onCheckedChange = { action = action.copy(keepLocalCopy = it) },
                        modifier = Modifier.testTag(Ids.FILTER_KEEP_LOCAL_COPY)
                    )
                    Text(stringResource(R.string.settings_privacy_page_keep_local_copy))
                }
            }
            Spacer(Modifier.height(16.dp))

            if (editingFilterId == null) {
                Button(
                    onClick = { onCreate(name, ruleType, ruleValue, action) },
                    enabled = !creatingFilter && name.isNotBlank() && ruleValue.isNotBlank(),
                    modifier = Modifier.fillMaxWidth().testTag(Ids.CREATE_FILTER)
                ) {
                    Text(stringResource(R.string.common_create))
                }
            } else {
                Button(
                    onClick = { onSave(name, ruleType, ruleValue, action) },
                    enabled = !creatingFilter && name.isNotBlank() && ruleValue.isNotBlank(),
                    modifier = Modifier.fillMaxWidth().testTag(Ids.SAVE_FILTER)
                ) {
                    Text(stringResource(R.string.common_save))
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}
