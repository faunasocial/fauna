package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.TaskDelegationVM
import com.fauna.app.ui.components.TokenSelect
import com.fauna.ffi.FfiParticipantRef
import com.fauna.ffi.FfiPinOption
import com.fauna.ffi.FfiRunnerStatus
import com.fauna.ffi.FfiTaskDelegationRow
import com.fauna.ffi.hexFull
import uniffi.fauna_core.LocalizedText
import social.fauna.generated.Ids

/**
 * The "Task delegation" Settings sub-page (`ui.yaml` page `task-delegation`;
 * settings.md § Navigation model — placed after Nests). Lists each heavy
 * background task kind (`docs/goal/behavior/participants.md` § Task
 * delegation), its current runner, and an assignment picker (Automatic /
 * pinned-to-a-participant). Stateless [TaskDelegationContent] is split out for
 * the Robolectric test harness (mirrors [LinkedNestsContent]); the VM-bound
 * [TaskDelegationScreen] is the thin wrapper the NavHost mounts. Per priority
 * #2 this shell holds **no** delegation policy — it renders
 * [TaskDelegationVM]'s rows and dispatches `setAssignment`; all sequencing
 * lives in the shared `fauna_client_delegation::TaskDelegationView` over
 * UniFFI. `page-heading` is this screen's TopAppBar title, `error-message` the
 * global MessageBanner (mirrors [WebSettingsScreen] / [LinkedNestsScreen] — the
 * majority settings-sub-page convention). Reference render: linux
 * `apps/fauna-linux/src/settings/task_delegation.rs`.
 */
@Composable
fun TaskDelegationScreen(
    navController: NavController,
    vm: TaskDelegationVM = hiltViewModel(),
) {
    val rows by vm.rows.collectAsState()
    val deviceLabels by vm.deviceLabels.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    // Re-load on every visit, not just first mount — the runner column is
    // **live** advisory-lease state (a peer claims the lease; a desktop
    // unplugs and yields).
    LaunchedEffect(Unit) { vm.load() }
    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    TaskDelegationContent(
        rows = rows,
        deviceLabels = deviceLabels,
        onBack = { navController.popBackStack() },
        onSetAssignment = vm::setAssignment,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TaskDelegationContent(
    rows: List<FfiTaskDelegationRow>,
    deviceLabels: Map<String, String>,
    onBack: () -> Unit,
    onSetAssignment: (String, FfiPinOption) -> Unit,
    // The shared `fauna_core::delegation::{runner_label,option_label}` decision
    // over UniFFI (priority #2; mirrors web's `taskDelegationRunnerLabel`/
    // `taskDelegationOptionLabel` wasm twins — participants.md § Task
    // delegation) — injected so Content stays FFI-free for the Robolectric
    // harness (the `claimStatusLabel`/`providerStatusLabel` FFI-free-injection
    // pattern, `ProfileTiersTab.kt`).
    runnerLabelFn: (FfiRunnerStatus, Map<String, String>) -> LocalizedText =
        { runner, labels -> com.fauna.ffi.taskDelegationRunnerLabel(runner, labels) },
    optionLabelFn: (FfiPinOption, Map<String, String>) -> LocalizedText =
        { option, labels -> com.fauna.ffi.taskDelegationOptionLabel(option, labels) },
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.task_delegation_title),
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
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(16.dp)
                .testTag(Ids.TASK_DELEGATION),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResource(R.string.task_delegation_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            // task-delegation-list — the container of the per-kind rows.
            Column(
                modifier = Modifier.testTag(Ids.TASK_DELEGATION_LIST),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                rows.forEach { row ->
                    TaskDelegationKindRow(
                        row = row,
                        deviceLabels = deviceLabels,
                        runnerLabelFn = runnerLabelFn,
                        optionLabelFn = optionLabelFn,
                        onSetAssignment = { option -> onSetAssignment(row.taskKind, option) },
                    )
                }
            }
        }
    }
}

/**
 * One `task-delegation-kind-item` row (bare id — rows are addressed
 * positionally in `LIVE_TASK_KINDS` order): the kind's display name, the
 * current runner/status, and the assignment picker.
 */
@Composable
private fun TaskDelegationKindRow(
    row: FfiTaskDelegationRow,
    deviceLabels: Map<String, String>,
    runnerLabelFn: (FfiRunnerStatus, Map<String, String>) -> LocalizedText,
    optionLabelFn: (FfiPinOption, Map<String, String>) -> LocalizedText,
    onSetAssignment: (FfiPinOption) -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.TASK_DELEGATION_KIND_ITEM)) {
        Column(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                localized(row.name) ?: row.name.key,
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.TASK_DELEGATION_KIND_NAME),
            )
            Text(
                runnerLabel(row.runner, deviceLabels, runnerLabelFn),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.TASK_DELEGATION_KIND_RUNNER),
            )
            AssignmentPicker(
                row = row,
                deviceLabels = deviceLabels,
                optionLabelFn = optionLabelFn,
                onSelect = onSetAssignment,
            )
        }
    }
}

/**
 * The assignment picker (`task-delegation-assignment-picker`) — renders
 * `row.pinOptions` **verbatim** (participants.md § The assignment picker: the
 * shared layer already decided the legal option set — never construct,
 * filter, or extend it here). The app's one token-round-tripping select
 * ([TokenSelect]): each option's VALUE is its stable cross-app key
 * ([pinOptionKey]) and the human sees only the shared `option_label`, so the
 * bridge's `select`/`get_text` speak the same keys the other six apps' do and
 * its `options` read lists exactly the legal set the menu paints. android
 * passes `FfiHeavyTaskCapability.VIEWER_ONLY` ([TaskDelegationVM]), so
 * `pinOptions` legitimately never carries `ThisDevice` — this renderer still
 * handles it uniformly rather than special-casing android (priority #1).
 */
@Composable
private fun AssignmentPicker(
    row: FfiTaskDelegationRow,
    deviceLabels: Map<String, String>,
    optionLabelFn: (FfiPinOption, Map<String, String>) -> LocalizedText,
    onSelect: (FfiPinOption) -> Unit,
) {
    val options = row.pinOptions.map { option ->
        pinOptionKey(option) to optionLabel(option, deviceLabels, optionLabelFn)
    }
    val selected = pinOptionKey(row.assignment)
    TokenSelect(
        testTagValue = Ids.TASK_DELEGATION_ASSIGNMENT_PICKER,
        selected = selected,
        // A foreign pin is rendered but never offered: its label still shows.
        options = if (options.any { it.first == selected }) {
            options
        } else {
            options + (selected to optionLabel(row.assignment, deviceLabels, optionLabelFn))
        },
        onSelect = { key -> row.pinOptions.firstOrNull { pinOptionKey(it) == key }?.let(onSelect) },
        modifier = Modifier.fillMaxWidth(),
    )
}

/**
 * The stable cross-app picker key for one option — `"automatic"` /
 * `"this-device"` / a participant's hex (a device's id verbatim, a nest's
 * pubkey hex-encoded); the same three keys web's `pinOptionKey`, windows'
 * `TaskDelegationViewModel.OptionKey` and apple's `optionKey` derive.
 */
private fun pinOptionKey(option: FfiPinOption): String = when (option) {
    FfiPinOption.Automatic -> "automatic"
    FfiPinOption.ThisDevice -> "this-device"
    is FfiPinOption.Other -> when (val who = option.who) {
        is FfiParticipantRef.Device -> who.deviceId
        is FfiParticipantRef.Nest -> hexFull(who.actorPubkey)
    }
}

/** Label a runner status via the injected shared decision (`runnerLabelFn` —
 *  `fauna_core::delegation::runner_label` over UniFFI, participant-name
 *  resolution included). Falls back to the raw key if no matching resource
 *  exists (mirrors `row.name`'s `localized(...) ?: row.name.key`). */
@Composable
private fun runnerLabel(
    runner: FfiRunnerStatus,
    labels: Map<String, String>,
    runnerLabelFn: (FfiRunnerStatus, Map<String, String>) -> LocalizedText,
): String {
    val text = runnerLabelFn(runner, labels)
    return localized(text) ?: text.key
}

/** Label one picker option via the injected shared decision (`optionLabelFn` —
 *  `fauna_core::delegation::option_label` over UniFFI). */
@Composable
private fun optionLabel(
    option: FfiPinOption,
    labels: Map<String, String>,
    optionLabelFn: (FfiPinOption, Map<String, String>) -> LocalizedText,
): String {
    val text = optionLabelFn(option, labels)
    return localized(text) ?: text.key
}
