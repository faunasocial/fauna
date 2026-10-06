package com.fauna.app.ui.screen.settings

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.core.AppMessages
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.ffi.FfiAccountEntry
import com.fauna.ffi.FfiFeatureRow
import com.fauna.ffi.FfiPendingActionSummary
import com.fauna.ffi.FfiQuotaDeviceUsage
import com.fauna.ffi.FfiQuotaFeatures
import com.fauna.ffi.FfiQuotaGetReply
import com.fauna.ffi.FfiUsageBytes
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText

/**
 * Compose-level coverage for the stateless [AccountSettingsContent] — the
 * whole-page split owed (`apps/android.md:160`:
 * "Screen composables are stateless"). No Hilt, no VM, no Activity, no FFI:
 * every native-touching value ([shortId], [quotaFraction], [quotaPercent],
 * [byteSize]) is injected FFI-free, the idiom every other Content composable
 * uses. (Android E2E `--client android` is host-emulator-gated like every
 * other android leg.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AccountSettingsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun account(actorId: String, handle: String?, requireConfirm: Boolean = false) =
        FfiAccountEntry(
            actorId = actorId,
            handle = handle,
            domain = null,
            tier = "free",
            requireConfirmToActivate = requireConfirm,
        )

    private fun quota(
        storageUsed: Long = 100,
        storageMax: Long = 1000,
        inboxUsed: Long = 10,
        inboxMax: Long = 100,
        devicesUsed: Long = 1,
        devicesMax: Long = 5,
    ) = FfiQuotaGetReply(
        tier = "free",
        inbox = FfiUsageBytes(usedBytes = inboxUsed, maxBytes = inboxMax),
        storage = FfiUsageBytes(usedBytes = storageUsed, maxBytes = storageMax),
        devices = FfiQuotaDeviceUsage(used = devicesUsed, max = devicesMax),
        features = FfiQuotaFeatures(versionedBackup = false, bridges = false, maxFeeds = 1),
    )

    private fun featureRow(feature: String, affordance: String = "available") = FfiFeatureRow(
        feature = feature,
        name = LocalizedText("features.$feature.name", emptyMap()),
        availability = if (affordance == "available") "allow" else "deny",
        deniedBy = null,
        cells = emptyList(),
        perOperationMax = null,
        perOperationMaxTier = null,
        unit = "bytes",
        affordance = affordance,
        restriction = null,
        status = LocalizedText("features.status.$affordance", emptyMap()),
    )

    private fun pendingAction(
        id: Long = 1L,
        actionType: String = "handle.change",
        target: String? = "bob",
        executeAfter: Long = 1_700_000_000L,
    ) = FfiPendingActionSummary(
        id = id,
        actionType = actionType,
        target = target,
        status = "pending",
        createdAt = 0L,
        executeAfter = executeAfter,
        requiresQuorum = 0L,
        approvals = emptyList(),
    )

    private fun render(
        accounts: List<FfiAccountEntry> = listOf(account("aa".repeat(32), "alice")),
        activeActorId: String? = "aa".repeat(32),
        actorId: String? = "aa".repeat(32),
        nodeUrl: String? = "https://nest.example",
        deviceId: String? = "bb".repeat(32),
        secretHex: String? = "cc".repeat(32),
        handle: String? = "alice",
        quotaData: FfiQuotaGetReply? = quota(),
        featuresData: List<FfiFeatureRow>? = listOf(featureRow("payments")),
        pendingActions: List<FfiPendingActionSummary>? = null,
        newHandle: String = "",
        changingHandle: Boolean = false,
        exporting: Boolean = false,
        showDeleteDialog: Boolean = false,
        showSignOutDialog: Boolean = false,
        onBack: () -> Unit = {},
        accountLabel: (FfiAccountEntry) -> String = { it.handle ?: it.actorId },
        onSwitch: (String) -> Unit = {},
        onRemove: (String) -> Unit = {},
        onRequireConfirmToggle: (String, Boolean) -> Unit = { _, _ -> },
        onAddAccount: () -> Unit = {},
        onNewHandleChange: (String) -> Unit = {},
        onChangeHandle: () -> Unit = {},
        onExportClick: () -> Unit = {},
        onShowDeleteDialog: () -> Unit = {},
        onDismissDeleteDialog: () -> Unit = {},
        onConfirmDelete: () -> Unit = {},
        onCancelPendingAction: (Long) -> Unit = {},
        onShowSignOutDialog: () -> Unit = {},
        onDismissSignOutDialog: () -> Unit = {},
        onConfirmSignOut: () -> Unit = {},
    ) {
        composeTestRule.setContent {
          CompositionLocalProvider(LocalAppMessages provides AppMessages()) {
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
                onBack = onBack,
                accountLabel = accountLabel,
                onSwitch = onSwitch,
                onRemove = onRemove,
                onRequireConfirmToggle = onRequireConfirmToggle,
                onAddAccount = onAddAccount,
                onNewHandleChange = onNewHandleChange,
                onChangeHandle = onChangeHandle,
                onExportClick = onExportClick,
                onShowDeleteDialog = onShowDeleteDialog,
                onDismissDeleteDialog = onDismissDeleteDialog,
                onConfirmDelete = onConfirmDelete,
                onCancelPendingAction = onCancelPendingAction,
                onShowSignOutDialog = onShowSignOutDialog,
                onDismissSignOutDialog = onDismissSignOutDialog,
                onConfirmSignOut = onConfirmSignOut,
                // FFI-free stubs — the real screen injects com.fauna.ffi.* directly.
                shortId = { it.take(8) },
                quotaFraction = { used, max -> if (max <= 0) 0.0 else (used.toDouble() / max).coerceIn(0.0, 1.0) },
                quotaPercent = { used, max -> if (max <= 0) 0u else ((used * 100 / max).coerceIn(0, 100)).toUInt() },
                byteSize = { LocalizedText("common.byte_size_raw", mapOf("n" to it.toString())) },
                describePendingAction = { actionType, target ->
                    if (actionType == "handle.change" && target != null) "Change handle to $target" else actionType
                },
                absoluteLocal = { "2026-01-01 00:00" },
            )
          }
        }
    }

    @Test
    fun pageHeadingRenders() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
    }

    @Test
    fun accountSwitcherRendersEveryAccountAndActiveIndicator() {
        render(
            accounts = listOf(
                account("aa".repeat(32), "alice"),
                account("bb".repeat(32), "bob"),
            ),
            activeActorId = "aa".repeat(32),
        )
        composeTestRule.onNodeWithTag("account-switcher-item[0]").assertExists()
        composeTestRule.onNodeWithTag("account-switcher-item[1]").assertExists()
        composeTestRule.onNodeWithTag("account-item-active-indicator[0]").assertExists()
        // The active row offers no remove button; the inactive one does.
        composeTestRule.onNodeWithTag("account-remove-button[0]").assertDoesNotExist()
        composeTestRule.onNodeWithTag("account-remove-button[1]").assertExists()
    }

    @Test
    fun switchingAccountFiresOnSwitchWithTheTargetActorId() {
        var switched: String? = null
        val bob = "bb".repeat(32)
        render(
            accounts = listOf(account("aa".repeat(32), "alice"), account(bob, "bob")),
            activeActorId = "aa".repeat(32),
            onSwitch = { switched = it },
        )
        composeTestRule.onNodeWithTag("account-switcher-item[1]").performClick()
        assertEquals(bob, switched)
    }

    @Test
    fun removingAnAccountFiresOnRemoveWithItsActorId() {
        var removed: String? = null
        val bob = "bb".repeat(32)
        render(
            accounts = listOf(account("aa".repeat(32), "alice"), account(bob, "bob")),
            activeActorId = "aa".repeat(32),
            onRemove = { removed = it },
        )
        composeTestRule.onNodeWithTag("account-remove-button[1]").performClick()
        assertEquals(bob, removed)
    }

    @Test
    fun addAccountButtonFiresOnAddAccount() {
        var fired = false
        render(onAddAccount = { fired = true })
        composeTestRule.onNodeWithTag("account-add-button").performScrollTo().performClick()
        assertEquals(true, fired)
    }

    @Test
    fun identitySectionRendersActorIdShortenedViaTheInjectedShortId() {
        // The injected `shortId` stub is `.take(8)` (see render()'s default) —
        // proves the display value routes through the injected fn, not the
        // native com.fauna.ffi.shortId this JVM host cannot load.
        // useUnmergedTree: CopyableRow's Row is `.clickable`, which merges its
        // two Text children's semantics into the row's own node by default —
        // the same class of bug the AttendeeRowContentTest fix already
        // established for this codebase's clickable-row composables.
        render(actorId = "aa".repeat(32))
        composeTestRule.onNodeWithTag("account-actor-id", useUnmergedTree = true).assertTextEquals("aaaaaaaa")
    }

    @Test
    fun identitySectionRendersUnknownForANullActorId() {
        render(actorId = null)
        composeTestRule.onNodeWithTag("account-actor-id", useUnmergedTree = true).assertTextEquals("Unknown")
    }

    @Test
    fun quotaSectionRendersWhenPresent() {
        render(quotaData = quota())
        composeTestRule.onNodeWithTag("quota-section").assertExists()
        composeTestRule.onNodeWithTag("settings-storage-bar").assertExists()
        composeTestRule.onNodeWithTag("settings-storage-text").assertExists()
        composeTestRule.onNodeWithTag("quota-inbox").assertExists()
        composeTestRule.onNodeWithTag("quota-storage").assertExists()
        composeTestRule.onNodeWithTag("quota-devices").assertExists()
    }

    @Test
    fun quotaSectionAbsentWhenNull() {
        render(quotaData = null)
        composeTestRule.onNodeWithTag("quota-section").assertDoesNotExist()
    }

    @Test
    fun featureLimitsSectionRendersRows() {
        render(featuresData = listOf(featureRow("payments"), featureRow("zaps")))
        composeTestRule.onNodeWithTag("feature-limits-section").assertExists()
        assertEquals(2, composeTestRule.onAllNodesWithTag("feature-limits-row").fetchSemanticsNodes().size)
    }

    @Test
    fun featureLimitsSectionShowsEmptyStateWhenEveryRowIsHidden() {
        render(featuresData = listOf(featureRow("payments", affordance = "hidden")))
        composeTestRule.onNodeWithTag("feature-limits-section").assertExists()
        composeTestRule.onNodeWithTag("feature-limits-empty").assertExists()
        composeTestRule.onNodeWithTag("feature-limits-row").assertDoesNotExist()
    }

    @Test
    fun featureLimitsSectionAbsentWhenNull() {
        render(featuresData = null)
        composeTestRule.onNodeWithTag("feature-limits-section").assertDoesNotExist()
    }

    @Test
    fun identityExportSectionRenders() {
        render(secretHex = "cc".repeat(32), handle = "alice")
        composeTestRule.onNodeWithTag("identity-export-section").assertExists()
    }

    @Test
    fun changeHandleCardRendersAndFiresOnChangeHandle() {
        var fired = false
        render(newHandle = "newalice", onChangeHandle = { fired = true })
        composeTestRule.onNodeWithTag("change-handle").performScrollTo().performClick()
        assertEquals(true, fired)
    }

    @Test
    fun changeHandleInputFiresOnNewHandleChange() {
        var typed: String? = null
        render(onNewHandleChange = { typed = it })
        composeTestRule.onNodeWithTag("new-handle").performScrollTo().performTextInput("bob")
        assertEquals("bob", typed)
    }

    @Test
    fun exportButtonFiresOnExportClick() {
        var fired = false
        render(exporting = false, onExportClick = { fired = true })
        composeTestRule.onNodeWithTag("settings-export-data-button").performScrollTo().performClick()
        assertEquals(true, fired)
    }

    @Test
    fun exportButtonDisabledWhileExporting() {
        render(exporting = true)
        composeTestRule.onNodeWithTag("settings-export-data-button").performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun signOutButtonFiresOnShowSignOutDialog() {
        var fired = false
        render(showSignOutDialog = false, onShowSignOutDialog = { fired = true })
        composeTestRule.onNodeWithTag("sign-out-button").performScrollTo().performClick()
        assertEquals(true, fired)
    }

    @Test
    fun signOutDialogRendersWhenShowSignOutDialogIsTrue() {
        render(showSignOutDialog = true)
        // AlertDialog's own Sign out/Cancel text buttons — the dialog
        // composable itself is proven by SignOutConfirmDialog's own render
        // here; this only proves AccountSettingsContent actually opens it.
        composeTestRule.onNodeWithText("Cancel").assertExists()
    }

    @Test
    fun signOutDialogAbsentWhenShowSignOutDialogIsFalse() {
        render(showSignOutDialog = false)
        composeTestRule.onNodeWithTag("sign-out-confirm-button").assertDoesNotExist()
    }

    @Test
    fun signOutDialogCancelFiresOnDismissSignOutDialog() {
        var fired = false
        render(showSignOutDialog = true, onDismissSignOutDialog = { fired = true })
        composeTestRule.onNodeWithText("Cancel").performClick()
        assertEquals(true, fired)
    }

    @Test
    fun signOutDialogConfirmFiresOnConfirmSignOut() {
        var fired = false
        render(showSignOutDialog = true, onConfirmSignOut = { fired = true })
        composeTestRule.onNodeWithTag("sign-out-confirm-button").performClick()
        assertEquals(true, fired)
    }

    @Test
    fun deleteButtonFiresOnShowDeleteDialog() {
        var fired = false
        render(showDeleteDialog = false, onShowDeleteDialog = { fired = true })
        composeTestRule.onNodeWithTag("settings-delete-account-button").performScrollTo().performClick()
        assertEquals(true, fired)
    }

    @Test
    fun deleteDialogRendersWhenShowDeleteDialogIsTrue() {
        render(showDeleteDialog = true)
        // AlertDialog's own Delete/Cancel text buttons — the dialog composable
        // itself is proven by DeleteAccountConfirmDialog's own OfflineGateTest
        // cases; this only proves AccountSettingsContent actually opens it.
        composeTestRule.onNodeWithText("Cancel").assertExists()
    }

    @Test
    fun deleteDialogAbsentWhenShowDeleteDialogIsFalse() {
        render(showDeleteDialog = false)
        composeTestRule.onNodeWithText("Cancel").assertDoesNotExist()
    }

    @Test
    fun deleteDialogCancelFiresOnDismissDeleteDialog() {
        var fired = false
        render(showDeleteDialog = true, onDismissDeleteDialog = { fired = true })
        composeTestRule.onNodeWithText("Cancel").performClick()
        assertEquals(true, fired)
    }

    // ── Pending actions (settings.md § Pending actions) — the section
    // answers honestly across its three states (never a settled "nothing
    // scheduled" claim before the first list read lands), mirroring tui's/
    // linux's/web's own three-state coverage. ──

    @Test
    fun pendingActionsSectionIsBareTitleWhenNotYetHydrated() {
        render(pendingActions = null)
        composeTestRule.onNodeWithTag("pending-actions-section").assertTextEquals("Pending actions")
        composeTestRule.onNodeWithTag("pending-action-item").assertDoesNotExist()
    }

    @Test
    fun pendingActionsSectionIsNoneScheduledWhenHydratedAndEmpty() {
        render(pendingActions = emptyList())
        composeTestRule.onNodeWithTag("pending-actions-section").assertTextEquals("Nothing is scheduled.")
        composeTestRule.onNodeWithTag("pending-action-item").assertDoesNotExist()
    }

    @Test
    fun pendingActionsSectionIsCountedTitleWithRowsWhenHydratedAndNonEmpty() {
        render(
            pendingActions = listOf(
                pendingAction(id = 1L, actionType = "handle.change", target = "carol"),
                pendingAction(id = 2L, actionType = "account.delete", target = null),
            ),
        )
        composeTestRule.onNodeWithTag("pending-actions-section").assertTextEquals("Pending actions (2)")
        composeTestRule.onAllNodesWithTag("pending-action-item").assertCountEquals(2)
        // useUnmergedTree: ListItem merges its descendants' semantics into its
        // own node (same reason account-actor-id / dm-subject need it above).
        composeTestRule.onAllNodesWithTag("pending-action-description", useUnmergedTree = true)[0]
            .assertTextEquals("Change handle to carol")
        composeTestRule.onAllNodesWithTag("pending-action-execute-after", useUnmergedTree = true)
            .assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("pending-action-cancel-button", useUnmergedTree = true)
            .assertCountEquals(2)
    }

    @Test
    fun pendingActionCancelButtonFiresOnCancelPendingActionWithTheRowId() {
        var cancelledId: Long? = null
        render(
            pendingActions = listOf(pendingAction(id = 42L)),
            onCancelPendingAction = { cancelledId = it },
        )
        composeTestRule.onNodeWithTag("pending-action-cancel-button", useUnmergedTree = true)
            .performScrollTo().performClick()
        assertEquals(42L, cancelledId)
    }
}
