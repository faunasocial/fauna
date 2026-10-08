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
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.AccountReauth
import com.fauna.app.core.findFragmentActivity
import com.fauna.app.ui.components.AccountSwitcherSection
import com.fauna.app.ui.components.CopyableRow
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.components.IdentityExportSection
import com.fauna.app.ui.components.RecoveryKitSection
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.localizedNested
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.shareFileBytes
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.AccountSettingsVM
import kotlinx.coroutines.launch
import social.fauna.generated.Ids
import uniffi.fauna_core.LocalizedText

@Composable
fun AccountSettingsScreen(
    navController: NavController,
    // Handed the sign-out residue — `null` when the erase left nothing behind,
    // otherwise what `identity_choice`'s `sign-out-residue` view paints about
    // data still on this device (`account-scoping.md` § Erasure follows scope →
    // *the residue surface*). The caller carries it to the onboarding surface
    // it mounts.
    onSignOut: (com.fauna.ffi.FfiSignOutResidue?) -> Unit,
    // Re-enter launch routing after a switch/append activates a different
    // identity — long-term-store.md § Multi-account evolution, Decision 1 (live
    // in-session reconnect, no relaunch). Distinct from [onSignOut] in intent
    // (no credentials are erased), though both flip the same `AppState` latch.
    onAccountSwitched: () -> Unit,
    onAddAccount: () -> Unit,
    vm: AccountSettingsVM = hiltViewModel()
) {
    val newHandle by vm.newHandle.collectAsState()
    val changingHandle by vm.changingHandle.collectAsState()
    val changeHandleError by vm.changeHandleError.collectAsState()
    val changeHandleSuccess by vm.changeHandleSuccess.collectAsState()
    val exporting by vm.exporting.collectAsState()
    val exportError by vm.exportError.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val deleteAccountSuccess by vm.deleteAccountSuccess.collectAsState()
    val accounts by vm.accounts.collectAsState()
    val activeActorId by vm.activeActorId.collectAsState()
    val switchError by vm.switchError.collectAsState()
    val quotaData by vm.quota.collectAsState()
    val featuresData by vm.features.collectAsState()
    val pendingActions by vm.pendingActions.collectAsState()
    val pendingActionsError by vm.pendingActionsError.collectAsState()

    val context = LocalContext.current
    val appMessages = LocalAppMessages.current
    val scope = rememberCoroutineScope()

    // Host activity + prompt strings for the Stage-2 re-auth gate (BiometricPrompt).
    val reauthActivity = remember(context) { context.findFragmentActivity() }
    val reauthTitle = stringResource(R.string.settings_account_page_reauth_prompt_title)
    val reauthSubtitle = stringResource(R.string.settings_account_page_reauth_reason)

    // Re-read the registry on appear so a require-confirm flag set by the admin
    // auto-default (SettingsAdminGateVM, before navigating here) is visible before
    // any switch tap — the "read the flag fresh at render" rule (long-term-store.md
    // § Per-account re-auth; linux's build-once page hit exactly this bug).
    LaunchedEffect(Unit) { vm.reloadAccounts() }

    // Hoisted here (not private to the two pass-45 sub-composables) so both the
    // native-dependent Screen and the plain-parameter Content can reach it.
    var showDeleteDialog by remember { mutableStateOf(false) }
    // Sign-out confirm — same hoisting rationale as [showDeleteDialog], settings.md § Navigation model "Account sub-page is pure
    // actions".
    var showSignOutDialog by remember { mutableStateOf(false) }

    val actorId = remember { vm.actorId() }
    val nodeUrl = remember { vm.nodeUrl() }
    val deviceId = remember { vm.deviceId() }
    // Read from the platform secure store, never from a settings snapshot
    // (architecture/apps/common.md § Credential storage).
    val secretHex = remember { vm.secretHex() }
    val handle = remember { vm.handle() }

    // Route all errors and success messages through the global MessageBanner.
    // Every error write asks the stolen ceremony's hold first: while its parked
    // persist-failure message — the only copy of the account's new key — sits
    // on this page's `error-message`, no other writer may replace it
    // (`settings.md` § Recovery kit → *The persist-failure message survives the
    // page*). Leaving the page is its one acknowledgment.
    val showPageError: (String) -> Unit = { msg -> if (vm.ceremonyHold.admits(msg)) appMessages.showError(msg) }
    LaunchedEffect(changeHandleError) {
        changeHandleError?.let { showPageError(it) }
    }
    LaunchedEffect(changeHandleSuccess) {
        if (changeHandleSuccess) appMessages.showInfo(context.getString(R.string.settings_account_page_handle_changed))
    }
    // The transient receipt for a scheduled-not-executed deletion (ruled
    // 2026-08-26 — no navigation, the user stays right here to read it).
    LaunchedEffect(deleteAccountSuccess) {
        if (deleteAccountSuccess) appMessages.showInfo(context.getString(R.string.settings_account_page_delete_requested))
    }
    LaunchedEffect(exportError) {
        exportError?.let { showPageError(it) }
    }
    LaunchedEffect(errorMessage) {
        errorMessage?.let { showPageError(it) }
    }
    LaunchedEffect(switchError) {
        switchError?.let { showPageError(it) }
    }
    LaunchedEffect(pendingActionsError) {
        pendingActionsError?.let { showPageError(it) }
    }

    AccountSettingsContent(
        accounts = accounts,
        activeActorId = activeActorId,
        actorId = actorId,
        nodeUrl = nodeUrl,
        deviceId = deviceId,
        secretHex = secretHex,
        handle = handle,
        quotaData = quotaData,
        featuresData = featuresData,
        pendingActions = pendingActions,
        newHandle = newHandle,
        changingHandle = changingHandle,
        exporting = exporting,
        showDeleteDialog = showDeleteDialog,
        showSignOutDialog = showSignOutDialog,
        onBack = { navController.popBackStack() },
        accountLabel = vm::accountLabel,
        onSwitch = { targetActorId ->
            // Stage 2 gate: a require-confirm account demands a native
            // re-auth BEFORE the switch seam (mutation-first invariant
            // holds — nothing is torn down until the confirmed switch).
            // Declining is a pure no-op. An unflagged account switches
            // straight through (long-term-store.md § Per-account re-auth).
            // Each arm is one `activationGesture`: its completion is counted
            // whatever the gate decided — the decline arm's only observable.
            if (vm.requiresConfirm(targetActorId)) {
                scope.launch {
                    AccountReauth.activationGesture {
                        val approved = AccountReauth.confirmActivation(
                            reauthActivity, reauthTitle, reauthSubtitle,
                        )
                        if (approved) {
                            vm.switchAccount(targetActorId, confirmed = true, onAccountSwitched)
                        }
                    }
                }
            } else {
                AccountReauth.activationGesture {
                    vm.switchAccount(targetActorId, confirmed = false, onAccountSwitched)
                }
            }
        },
        onRemove = vm::removeAccount,
        onRequireConfirmToggle = { toggledActorId, require -> vm.setRequireConfirm(toggledActorId, require) },
        onAddAccount = {
            vm.beginAddAccount()
            onAddAccount()
        },
        onNewHandleChange = { vm.newHandle.value = it },
        onChangeHandle = { vm.changeHandle() },
        onExportClick = {
            scope.launch {
                val bytes = vm.exportData()
                if (bytes != null) {
                    try {
                        context.shareFileBytes(
                            "fauna-export-${System.currentTimeMillis()}.zip",
                            bytes,
                            mimeType = "application/zip",
                            chooserTitle = "Export Data",
                        )
                    } catch (e: Exception) {
                        vm.exportError.value = e.message
                    }
                }
            }
        },
        onShowDeleteDialog = { showDeleteDialog = true },
        onDismissDeleteDialog = { showDeleteDialog = false },
        onConfirmDelete = {
            showDeleteDialog = false
            vm.deleteAccount()
        },
        onCancelPendingAction = { vm.cancelPendingAction(it) },
        onShowSignOutDialog = { showSignOutDialog = true },
        onDismissSignOutDialog = { showSignOutDialog = false },
        onConfirmSignOut = {
            showSignOutDialog = false
            // The credential-erasing call — [onSignOut] alone only resets
            // AppState back to onboarding, exactly like [onConfirmDelete]'s
            // post-delete relaunch hook above; it does not touch the secure
            // store. Mirrors the root Settings screen's prior wiring
            // (`accountVm.signOut { appState.isOnboarding = true }` in
            // FaunaNavHost, before this button lived here).
            vm.signOut(onSignOut)
        },
        recoveryKitSection = {
            RecoveryKitSection(
                sessionActorIdHex = actorId,
                // The ceremony's persisted landed arm: the account moved, so
                // this is an account SWITCH to the successor (never the
                // sign-out reset) — the same seam the switcher drives.
                onSucceeded = { successor -> vm.switchAccount(successor, confirmed = false, onAccountSwitched) },
            )
        },
    )
}

/**
 * The Account page's stateless render surface (`settings.md` § Layout & flow
 * item 1 / § Element IDs "Root/account family") — the whole-page split every
 * other android settings sub-page has (`apps/android.md:160`: "Screen
 * composables are stateless"). Plain parameters + callbacks only; renders
 * under Robolectric with no Hilt, no VM, no Activity, no FFI.
 *
 * [AccountSettingsScreen] stays the thin stateful wrapper: it collects the VM
 * flows and supplies the callbacks that genuinely need the Activity/NavController
 * (the account-switch BiometricPrompt re-auth gate, the data-export file/Intent
 * dance, delete's post-success relaunch) — those cannot be plain-parameter'd
 * without dragging Hilt/FragmentActivity/NavController into this function.
 *
 * `showDeleteDialog` is hoisted here (a parameter, not private `remember`d
 * state inside [ChangeHandleCard]/[DeleteAccountConfirmDialog]) so every
 * existing `OfflineGateTest` case against those two — three on the handle
 * card, two on the delete dialog — keeps driving them at its current
 * precision instead of losing reachability in the split.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AccountSettingsContent(
    accounts: List<com.fauna.ffi.FfiAccountEntry>,
    activeActorId: String?,
    actorId: String?,
    nodeUrl: String?,
    deviceId: String?,
    secretHex: String?,
    handle: String?,
    quotaData: com.fauna.ffi.FfiQuotaGetReply?,
    featuresData: List<com.fauna.ffi.FfiFeatureRow>?,
    pendingActions: List<com.fauna.ffi.FfiPendingActionSummary>?,
    newHandle: String,
    changingHandle: Boolean,
    exporting: Boolean,
    showDeleteDialog: Boolean,
    showSignOutDialog: Boolean,
    onBack: () -> Unit,
    accountLabel: (com.fauna.ffi.FfiAccountEntry) -> String,
    onSwitch: (actorId: String) -> Unit,
    onRemove: (actorId: String) -> Unit,
    onRequireConfirmToggle: (actorId: String, require: Boolean) -> Unit,
    onAddAccount: () -> Unit,
    onNewHandleChange: (String) -> Unit,
    onChangeHandle: () -> Unit,
    onExportClick: () -> Unit,
    onShowDeleteDialog: () -> Unit,
    onDismissDeleteDialog: () -> Unit,
    onConfirmDelete: () -> Unit,
    onCancelPendingAction: (Long) -> Unit,
    onShowSignOutDialog: () -> Unit,
    onDismissSignOutDialog: () -> Unit,
    onConfirmSignOut: () -> Unit,
    // Shared `fauna_core::format::short_id` display encoder (value-formatting.md
    // § Short id), injected FFI-free so the Robolectric harness stays off the
    // native path — the idiom every other Content composable uses.
    shortId: (String) -> String = { com.fauna.ffi.shortId(it) },
    // Shared `fauna_core::format::quota_fraction`/`quota_percent`
    // (value-formatting.md § Quota fraction), injected FFI-free for the same
    // reason.
    quotaFraction: (Long, Long) -> Double = { used, max -> com.fauna.ffi.quotaFraction(used, max) },
    quotaPercent: (Long, Long) -> UInt = { used, max -> com.fauna.ffi.quotaPercent(used, max) },
    // `ValueFormat.byteSize`'s own native half (`fauna_core::format::byte_size`),
    // injected FFI-free; the Context-dependent resolution half
    // (`resolveLocalized`) touches no native code and stays inline below —
    // `cellValueText` (used by [FeatureLimitQuotaCell]) is the same way and
    // needs no injection at all.
    byteSize: (kotlin.ULong) -> LocalizedText = { com.fauna.ffi.byteSize(it) },
    // Shared `fauna_protocol::pending_actions::describe_pending_action`
    // (settings.md § Pending actions) — what a scheduled action will do, as
    // one sentence; injected FFI-free for the same Robolectric reason.
    describePendingAction: (String, String?) -> String =
        { actionType, target -> com.fauna.ffi.describePendingAction(actionType, target) },
    // Shared `fauna_core::format::format_unix_local` (value-formatting.md
    // § Absolute local timestamp display) — the pending-action row's
    // execute-after text; injected FFI-free for the same reason.
    absoluteLocal: (Long) -> String = { com.fauna.app.ui.util.ValueFormat.absoluteLocal(it) },
    // The stateful Recovery kit section ([RecoveryKitSection], VM-backed) —
    // supplied by [AccountSettingsScreen]; empty under the Content harness.
    recoveryKitSection: @Composable () -> Unit = {},
) {
    val context = LocalContext.current
    val appMessages = LocalAppMessages.current
    val byteSizeText = { bytes: Long -> resolveLocalized(context, byteSize(bytes.toULong())) ?: "" }

    Scaffold(
        topBar = { AccountSettingsTopBar(onBack = onBack) }
    ) { padding ->
        // A plain scrollable Column, not LazyColumn — matches every sibling
        // settings Content's shape (AdminNestContent, ProfileTiersContent, …)
        // and is what keeps this page's own sections reliably Robolectric-
        // testable without a LazyColumn's virtualization getting in the way
        // (a fixed, small section count here; the one variable-length list,
        // accounts, is a plain Column inside AccountSwitcherSection's own
        // Card, never the page's own laziness).
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp)
        ) {
            // Accounts switcher section (long-term-store.md § Multi-account
            // evolution, Stage 1) — FIRST section, mirroring linux/apple ordering.
            AccountSwitcherSection(
                accounts = accounts,
                activeActorId = activeActorId,
                label = accountLabel,
                onSwitch = onSwitch,
                onRemove = onRemove,
                onRequireConfirmToggle = onRequireConfirmToggle,
                onAddAccount = onAddAccount,
            )

            // Identity Section
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Text(stringResource(R.string.common_identity), style = MaterialTheme.typography.titleMedium)
                    Spacer(Modifier.height(8.dp))

                    CopyableRow(
                        label = stringResource(R.string.common_actor_id),
                        displayValue = actorId?.let { shortId(it) } ?: "Unknown",
                        fullValue = actorId ?: "",
                        testTag = Ids.ACCOUNT_ACTOR_ID_COPY_BTN,
                        valueTestTag = "account-actor-id",
                        onCopied = { appMessages.showInfo(context.getString(R.string.settings_account_page_copied_clipboard)) },
                    )
                    CopyableRow(
                        label = stringResource(R.string.common_node_url),
                        displayValue = nodeUrl ?: "Unknown",
                        fullValue = nodeUrl ?: "",
                        onCopied = { appMessages.showInfo(context.getString(R.string.settings_account_page_copied_clipboard)) },
                    )
                    CopyableRow(
                        label = stringResource(R.string.devices_detail_device_id),
                        displayValue = deviceId?.let { shortId(it) } ?: "Unknown",
                        fullValue = deviceId ?: "",
                        onCopied = { appMessages.showInfo(context.getString(R.string.settings_account_page_copied_clipboard)) },
                    )
                }
            }

            // Storage Quota Section
            quotaData?.let { q ->
                Card(modifier = Modifier.fillMaxWidth().testTag(Ids.QUOTA_SECTION)) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text(stringResource(R.string.common_storage), style = MaterialTheme.typography.titleMedium)
                        Spacer(Modifier.height(12.dp))

                        // Headline bar tracks the storage resource; the rows
                        // below break out the distinct inbox / storage /
                        // device quotas the rich fauna.quota.get reply carries.
                        val storageUsed = q.storage.usedBytes
                        val storageLimit = q.storage.maxBytes
                        // Shared `fauna_core::format::quota_fraction` — the
                        // 0.0..=1.0 bar-fill fraction (guards maxBytes <= 0 → 0,
                        // clamps over-quota → 1), single-sourced with
                        // linux/web/windows (value-formatting.md § Quota fraction);
                        // replaces a hand-rolled guard/clamp that could divide-by-
                        // zero on a zero quota.
                        val progress = quotaFraction(storageUsed, storageLimit).toFloat()
                        LinearProgressIndicator(
                            progress = { progress },
                            modifier = Modifier.fillMaxWidth().height(8.dp).testTag(Ids.SETTINGS_STORAGE_BAR),
                        )
                        Spacer(Modifier.height(8.dp))

                        Row(
                            modifier = Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.SpaceBetween
                        ) {
                            Text(
                                "${byteSizeText(storageUsed)} / ${byteSizeText(storageLimit)}",
                                style = MaterialTheme.typography.bodyMedium,
                                modifier = Modifier.testTag(Ids.SETTINGS_STORAGE_TEXT)
                            )
                            Text(
                                "${quotaPercent(storageUsed, storageLimit)}%",
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant
                            )
                        }

                        Spacer(Modifier.height(8.dp))

                        Text(
                            "${stringResource(R.string.common_inbox)}: ${byteSizeText(q.inbox.usedBytes)} / ${byteSizeText(q.inbox.maxBytes)}",
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.testTag(Ids.QUOTA_INBOX)
                        )
                        Text(
                            "${stringResource(R.string.common_storage)}: ${byteSizeText(storageUsed)} / ${byteSizeText(storageLimit)}",
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.testTag(Ids.QUOTA_STORAGE)
                        )
                        Text(
                            "${stringResource(R.string.common_devices)}: ${q.devices.used} / ${q.devices.max}",
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.testTag(Ids.QUOTA_DEVICES)
                        )
                    }
                }
            }

            // Feature Limits Section — the gated-feature plane's transparency
            // read (`feature-limits-section`, `dynamic-features.md` §
            // Transparency & auditability, boundary 4: "no silent gates").
            // Placed directly after Quota as its sibling "what bounds me"
            // surface (`settings.md` § Layout & flow item 2b). This block
            // paints; it decides nothing — every judgement (which cells
            // survived the tier meet, per-cell tier attribution, available/
            // restricted/hidden) is `FfiFeaturesClient.rows()`'s output,
            // mirroring tui's `feature_limits_elements` and linux's
            // `views::status::update_features` field-for-field.
            featuresData?.let { rows ->
                // A `hidden` row means this nest build does not carry the
                // feature at all (its capability token is absent) —
                // filtered client-side, not in the shared crate, same as
                // every other app.
                val visible = rows.filter { it.affordance != "hidden" }
                Card(modifier = Modifier.fillMaxWidth().testTag(Ids.FEATURE_LIMITS_SECTION)) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text(
                            stringResource(R.string.features_section_title),
                            style = MaterialTheme.typography.titleMedium,
                        )
                        Spacer(Modifier.height(12.dp))
                        if (visible.isEmpty()) {
                            Text(
                                stringResource(R.string.features_empty),
                                modifier = Modifier.testTag(Ids.FEATURE_LIMITS_EMPTY),
                            )
                        } else {
                            visible.forEach { row -> FeatureLimitRow(row) }
                        }
                    }
                }
            }

            // Identity Export Section — the QR a second device scans to import this
            // identity (settings.md § Identity export). Placed before Change handle,
            // mirroring the shipped Apple order.
            IdentityExportSection(secretHex = secretHex, handle = handle)

            // Recovery kit — immediately after Identity export (settings.md
            // § Layout & flow item 4, § Recovery kit): a slot, so this
            // Content stays FFI- and Hilt-free under Robolectric.
            recoveryKitSection()

            // Change Handle Section
            ChangeHandleCard(
                newHandle = newHandle,
                changingHandle = changingHandle,
                onNewHandleChange = onNewHandleChange,
                onChangeHandle = onChangeHandle,
            )

            // Data Export Section
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Text(stringResource(R.string.settings_account_page_data_export), style = MaterialTheme.typography.titleMedium)
                    Spacer(Modifier.height(8.dp))

                    OutlinedButton(
                        modifier = Modifier.testTag(Ids.SETTINGS_EXPORT_DATA_BUTTON),
                        onClick = onExportClick,
                        enabled = !exporting
                    ) {
                        if (exporting) {
                            CircularProgressIndicator(
                                modifier = Modifier.size(16.dp),
                                strokeWidth = 2.dp
                            )
                            Spacer(Modifier.width(8.dp))
                        }
                        Text(stringResource(R.string.settings_account_page_export_my_data))
                    }
                }
            }

            // Sign Out Section — settings.md § Layout & flow item 13, placed
            // right after Data export and before Delete account (item 14): the
            // Account sub-page is where every other app already renders
            // `sign-out-button` (settings.md § Element IDs "Root/account family";
            // mirrors iOS's `SignOutSection` immediately ahead of its own Danger
            // Zone `GroupBox`).
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Button(
                        onClick = onShowSignOutDialog,
                        colors = ButtonDefaults.buttonColors(
                            containerColor = MaterialTheme.colorScheme.error
                        ),
                        modifier = Modifier.fillMaxWidth().testTag(Ids.SIGN_OUT_BUTTON)
                    ) {
                        Text(stringResource(R.string.settings_sign_out))
                    }
                }
            }

            // Danger Zone Section
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    Text(stringResource(R.string.common_danger_zone), style = MaterialTheme.typography.titleMedium)
                    Spacer(Modifier.height(8.dp))

                    OutlinedButton(
                        onClick = onShowDeleteDialog,
                        colors = ButtonDefaults.outlinedButtonColors(
                            contentColor = MaterialTheme.colorScheme.error
                        ),
                        modifier = Modifier.testTag(Ids.SETTINGS_DELETE_ACCOUNT_BUTTON)
                    ) {
                        Text(stringResource(R.string.settings_account_page_delete_account))
                    }
                }
            }

            // Pending actions (settings.md § Pending actions) — STANDING,
            // sitting below the two delayed verbs this page hosts (the
            // third, snapshot delete, schedules from the Backups page and
            // appears here on the next Account visit's hydrate). Always
            // rendered — a conditional render would hide the affordance
            // exactly when a mis-clicker goes looking for it. Mirrors tui's
            // pending_actions_elements / linux's build_pending_actions_group
            // / web's +page.svelte section exactly.
            PendingActionsCard(
                pendingActions = pendingActions,
                onCancelPendingAction = onCancelPendingAction,
                describePendingAction = describePendingAction,
                absoluteLocal = absoluteLocal,
            )
        }
    }

    if (showSignOutDialog) {
        SignOutConfirmDialog(
            onDismiss = onDismissSignOutDialog,
            onConfirm = onConfirmSignOut,
        )
    }

    if (showDeleteDialog) {
        DeleteAccountConfirmDialog(
            onDismiss = onDismissDeleteDialog,
            onConfirm = onConfirmDelete,
        )
    }
}

/**
 * The `change-handle` card: the `new-handle` buffer and the commit beside it.
 *
 * A public sub-composable of [AccountSettingsContent] rather than folded in as
 * a private inline block — this keeps its own three `OfflineGateTest` cases
 * drivable at their pass-45 precision (they render just this card, not the
 * whole page) alongside the ones that now render the full `Content`.
 */
@Composable
fun ChangeHandleCard(
    newHandle: String,
    changingHandle: Boolean,
    onNewHandleChange: (String) -> Unit,
    onChangeHandle: () -> Unit,
) {
    // The commit gates; the `new-handle` buffer beside it stays live so a handle
    // can be typed with no nest and sent on reconnect. The page's own predicate
    // (a non-blank handle, no change already in flight) is handed over so the
    // gate is that `enabled` argument's only author.
    val handleGate = faunaGate(
        "fauna.profile.handle.change",
        enabled = newHandle.isNotBlank() && !changingHandle,
    )
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                stringResource(R.string.settings_account_page_change_handle),
                style = MaterialTheme.typography.titleMedium,
            )
            Spacer(Modifier.height(8.dp))
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                OutlinedTextField(
                    value = newHandle,
                    onValueChange = onNewHandleChange,
                    label = { Text(stringResource(R.string.settings_account_page_new_handle)) },
                    singleLine = true,
                    modifier = Modifier.weight(1f).testTag(Ids.NEW_HANDLE)
                )
                Button(
                    onClick = onChangeHandle,
                    enabled = handleGate.enabled,
                    modifier = Modifier.testTag(Ids.CHANGE_HANDLE)
                ) {
                    Text(stringResource(R.string.common_change))
                }
            }
            DisabledControlReasonText(handleGate.reason)
        }
    }
}

/**
 * The STANDING `pending-actions-section` (`settings.md` § Pending actions,
 * ui.yaml's five `pending-action*` ids) — the Account page's cancellable-
 * window listing for the three delayed verbs (handle change, account
 * delete, snapshot delete). Mirrors tui's `pending_actions_elements` / linux's
 * `build_pending_actions_group` / web's `+page.svelte` section: **always
 * present** (never a conditional render), a title that answers honestly
 * across three states (bare title un-hydrated / empty-state line / counted
 * title), and one row per still-`pending` action with its description,
 * execute-after time, and a **one-click** cancel (no confirm — cancelling is
 * the safe direction).
 *
 * A public sub-composable of [AccountSettingsContent], same rationale as
 * [ChangeHandleCard]: independently drivable/testable at its own precision.
 *
 * **Never feed a delayed verb's echoed new value into any local cache** —
 * the change has not applied (the section's own iron-clad, `settings.md`
 * § Pending actions): [com.fauna.ffi.FfiPendingActionSummary.target] exists
 * only to be *described*, never stored as state — [AccountSettingsVM]'s
 * `changeHandle`/`deleteAccount` never do this.
 */
@Composable
fun PendingActionsCard(
    pendingActions: List<com.fauna.ffi.FfiPendingActionSummary>?,
    onCancelPendingAction: (Long) -> Unit,
    describePendingAction: (String, String?) -> String =
        { actionType, target -> com.fauna.ffi.describePendingAction(actionType, target) },
    absoluteLocal: (Long) -> String = { com.fauna.app.ui.util.ValueFormat.absoluteLocal(it) },
) {
    val title = when {
        pendingActions == null -> stringResource(R.string.settings_pending_actions_title)
        pendingActions.isEmpty() -> stringResource(R.string.settings_pending_actions_none_scheduled)
        else -> stringResource(R.string.settings_pending_actions_title_count)
            .replace("{count}", pendingActions.size.toString())
    }
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                title,
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.testTag(Ids.PENDING_ACTIONS_SECTION),
            )
            pendingActions?.forEach { action ->
                Spacer(Modifier.height(8.dp))
                ListItem(
                    headlineContent = {
                        Text(
                            describePendingAction(action.actionType, action.target),
                            modifier = Modifier.testTag(Ids.PENDING_ACTION_DESCRIPTION),
                        )
                    },
                    supportingContent = {
                        Text(
                            stringResource(R.string.settings_pending_actions_applies)
                                .replace("{time}", absoluteLocal(action.executeAfter)),
                            modifier = Modifier.testTag(Ids.PENDING_ACTION_EXECUTE_AFTER),
                        )
                    },
                    trailingContent = {
                        TextButton(
                            onClick = { onCancelPendingAction(action.id) },
                            modifier = Modifier.testTag(Ids.PENDING_ACTION_CANCEL_BUTTON),
                        ) {
                            Text(stringResource(R.string.settings_pending_actions_cancel))
                        }
                    },
                    modifier = Modifier.testTag(Ids.PENDING_ACTION_ITEM),
                )
            }
        }
    }
}

/**
 * The `sign-out-button` confirm — an AlertDialog whose confirm button carries
 * the drivable `sign-out-confirm-button` id, uniform with
 * `admin-factory-reset-confirm-button` / [DeleteAccountConfirmDialog] (android's
 * own established confirm idiom; settings.md § User actions: "per-app the
 * confirm matches that app's own factory-reset confirm idiom … drivable
 * native dialog on linux/android").
 */
@Composable
fun SignOutConfirmDialog(
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.settings_sign_out)) },
        text = { Text(stringResource(R.string.settings_sign_out_confirm)) },
        confirmButton = {
            TextButton(
                onClick = onConfirm,
                colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
                modifier = Modifier.testTag(Ids.SIGN_OUT_CONFIRM_BUTTON),
            ) {
                Text(stringResource(R.string.settings_sign_out))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(stringResource(R.string.common_cancel))
            }
        },
    )
}

/**
 * The account-deletion confirm.
 *
 * **Arming is local, the commit gates**: `settings-delete-account-button` only
 * opens this dialog and stays live with no nest, while the confirm here is the
 * account's most destructive gesture and issues `fauna.account.delete`. The
 * cancel beside it deliberately declares nothing — dismissing a dialog is pure
 * local UI, and that pairing is what proves the gate did not simply grey the
 * whole dialog.
 *
 * ⚠ This is a Compose [AlertDialog] — an ordinary composable lambda in the same
 * composition — so it reads `LocalConnectionState` exactly like the page around
 * it. ui.yaml's `settings-delete-confirm-field` comment describes android as
 * confirming "behind a native OS Yes/No alert with no drivable id": true of
 * linux, stale for android since this dialog was built in Compose. That is why
 * the gate can land here at all, and why the ui.yaml note should not be read as
 * an android absence.
 */
@Composable
fun DeleteAccountConfirmDialog(
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
) {
    val deleteGate = faunaGate("fauna.account.delete")
    AlertDialog(
        onDismissRequest = onDismiss,
        title = {
            Text(
                stringResourceFmt(
                    R.string.common_fmt_question,
                    stringResource(R.string.settings_account_page_delete_account),
                )
            )
        },
        text = {
            Text(stringResource(R.string.settings_account_page_delete_confirm_text))
        },
        confirmButton = {
            Column {
                TextButton(
                    onClick = onConfirm,
                    enabled = deleteGate.enabled,
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error
                    )
                ) {
                    Text(stringResource(R.string.common_delete))
                }
                DisabledControlReasonText(deleteGate.reason)
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.common_cancel)) }
        }
    )
}

/**
 * One `feature-limits-row` — a registry member's name, status, restriction (if
 * any), and its bound cells. Mirrors tui's `feature_limits_elements` /
 * linux's `views::status::update_features` field-for-field so the answer to
 * "why can't I do this" cannot differ across apps (priority #1).
 */
@Composable
private fun FeatureLimitRow(row: com.fauna.ffi.FfiFeatureRow) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 6.dp)
            .testTag(Ids.FEATURE_LIMITS_ROW),
    ) {
        Text(
            localized(row.name) ?: "",
            style = MaterialTheme.typography.titleSmall,
            modifier = Modifier.testTag(Ids.FEATURE_LIMITS_NAME),
        )
        Text(
            localized(row.status) ?: "",
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.testTag(Ids.FEATURE_LIMITS_STATUS),
        )
        // Only when something actually blocks — boundary 4's "no silent
        // gates" half. `localizedNested`, not `localized`: the sentence's
        // `{window}` is itself an i18n key.
        row.restriction?.let { reason ->
            Text(
                localizedNested(reason) ?: "",
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.FEATURE_LIMITS_RESTRICTION),
            )
        }
        row.cells.forEach { cell -> FeatureLimitQuotaCell(cell) }
    }
}

/**
 * One `feature-limits-quota` cell — per CELL, not per row: the meet takes the
 * MIN per (dimension, window), so two bounds on one feature can come from
 * different tiers.
 */
@Composable
private fun FeatureLimitQuotaCell(cell: com.fauna.ffi.FfiLimitCell) {
    val context = androidx.compose.ui.platform.LocalContext.current
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 12.dp)
            .testTag(Ids.FEATURE_LIMITS_QUOTA),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            localizedNested(cell.label) ?: "",
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.testTag(Ids.FEATURE_LIMITS_QUOTA_LABEL),
        )
        Text(
            com.fauna.app.ui.util.cellValueText(context, cell),
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.testTag(Ids.FEATURE_LIMITS_QUOTA_VALUE),
        )
        Text(
            localized(cell.tierLabel) ?: "",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.FEATURE_LIMITS_QUOTA_TIER),
        )
    }
}

/**
 * The Account page's top bar — its own composable so [AccountSettingsContent]
 * (and, before the whole-page split, a narrower Robolectric probe) can verify
 * the heading id without pulling in [AccountSettingsScreen]'s VM-entangled
 * body (Hilt, biometric re-auth, file export, account switching).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AccountSettingsTopBar(onBack: () -> Unit) {
    TopAppBar(
        title = {
            Text(
                stringResource(R.string.common_account),
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

