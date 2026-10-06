package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.core.HexUtil
import com.fauna.app.ui.util.getStringFmt
import com.fauna.ffi.FfiParticipantRef
import com.fauna.ffi.FfiPinOption
import com.fauna.ffi.FfiRunnerStatus
import com.fauna.ffi.FfiTaskDelegationRow
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText

/**
 * Compose-level coverage for the stateless [TaskDelegationContent] (the "Task
 * delegation" settings page, `docs/goal/behavior/participants.md` § Task
 * delegation): one row per live task kind (name/runner/picker), the assignment
 * picker rendering `pin_options` **verbatim** (never "This device" on android —
 * `TaskDelegationVM` passes `FfiHeavyTaskCapability.VIEWER_ONLY`), and the
 * runner/option participant-name resolution (roster label, or a short-hex
 * fallback). Renders with hand-built `FfiTaskDelegationRow` fixtures — no Hilt,
 * no VM, no FFI native calls (`runnerLabelFn`/`optionLabelFn` are injected as
 * stubs mirroring `fauna_core::delegation::{runner_label,option_label}`'s exact
 * key/arg shape, the `claimStatusLabel`/`providerStatusLabel`
 * FFI-free-injection pattern — `ProfileTiersContentTest`). The cross-app
 * `test_task_delegation.py` is the standing gate once the host emulator lands.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TaskDelegationContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val ctx get() = ApplicationProvider.getApplicationContext<android.content.Context>()

    private fun testShortId(hex: String): String = "short-$hex"

    /** Mirrors `fauna_core::delegation::participant_name` — the roster label,
     *  or a short-hex fallback for an unknown device / a nest ref. */
    private fun testParticipantName(who: FfiParticipantRef, labels: Map<String, String>): String =
        when (who) {
            is FfiParticipantRef.Device ->
                labels[who.deviceId]?.takeIf { it.isNotEmpty() } ?: testShortId(who.deviceId)
            is FfiParticipantRef.Nest -> testShortId(HexUtil.bytesToHex(who.actorPubkey))
        }

    /** FFI-free stub matching `runner_label`'s exact key/arg shape, so
     *  `localized(...)` resolves it through the real generated string
     *  resources exactly as the live FFI call would. */
    private fun testRunnerLabel(runner: FfiRunnerStatus, labels: Map<String, String>): LocalizedText =
        when (runner) {
            is FfiRunnerStatus.ThisDevice ->
                LocalizedText(key = "task_delegation.runner_this_device", args = emptyMap())
            is FfiRunnerStatus.Waiting ->
                LocalizedText(key = "task_delegation.runner_waiting", args = emptyMap())
            is FfiRunnerStatus.Other -> LocalizedText(
                key = "task_delegation.runner_other_device",
                args = mapOf("device" to testParticipantName(runner.who, labels)),
            )
        }

    /** FFI-free stub matching `option_label`'s exact key/arg shape (see
     *  [testRunnerLabel]). */
    private fun testOptionLabel(option: FfiPinOption, labels: Map<String, String>): LocalizedText =
        when (option) {
            is FfiPinOption.Automatic ->
                LocalizedText(key = "task_delegation.assignment_automatic", args = emptyMap())
            is FfiPinOption.ThisDevice ->
                LocalizedText(key = "task_delegation.assignment_this_device", args = emptyMap())
            is FfiPinOption.Other -> LocalizedText(
                key = "task_delegation.assignment_other_name",
                args = mapOf("name" to testParticipantName(option.who, labels)),
            )
        }

    private fun row(
        taskKind: String = "backup-upload",
        nameKey: String = "task_delegation.kind_backup_upload",
        runner: FfiRunnerStatus = FfiRunnerStatus.Waiting,
        assignment: FfiPinOption = FfiPinOption.Automatic,
        pinOptions: List<FfiPinOption> = listOf(FfiPinOption.Automatic),
    ) = FfiTaskDelegationRow(
        taskKind = taskKind,
        name = LocalizedText(key = nameKey, args = emptyMap()),
        runner = runner,
        assignment = assignment,
        pinOptions = pinOptions,
    )

    private fun render(
        rows: List<FfiTaskDelegationRow>,
        deviceLabels: Map<String, String> = emptyMap(),
        onSetAssignment: (String, FfiPinOption) -> Unit = { _, _ -> },
    ) {
        composeTestRule.setContent {
            TaskDelegationContent(
                rows = rows,
                deviceLabels = deviceLabels,
                onBack = {},
                onSetAssignment = onSetAssignment,
                runnerLabelFn = ::testRunnerLabel,
                optionLabelFn = ::testOptionLabel,
            )
        }
    }

    @Test
    fun rendersPageChromeWithNoRows() {
        render(rows = emptyList())
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("task-delegation-list").assertExists()
        composeTestRule.onNodeWithTag("task-delegation-kind-item").assertDoesNotExist()
    }

    @Test
    fun rendersOneRowPerKindWithResolvedNameAndAutomaticPicker() {
        render(
            rows = listOf(
                row(taskKind = "backup-upload", nameKey = "task_delegation.kind_backup_upload"),
                row(taskKind = "content-rescore", nameKey = "task_delegation.kind_content_rescore"),
                row(taskKind = "index", nameKey = "task_delegation.kind_index"),
            ),
        )
        composeTestRule.onAllNodesWithTag("task-delegation-kind-item").assertCountEquals(3)
        composeTestRule.onAllNodesWithTag("task-delegation-kind-name")[0]
            .assertTextEquals(ctx.getString(R.string.task_delegation_kind_backup_upload))
        composeTestRule.onAllNodesWithTag("task-delegation-kind-name")[1]
            .assertTextEquals(ctx.getString(R.string.task_delegation_kind_content_rescore))
        composeTestRule.onAllNodesWithTag("task-delegation-kind-name")[2]
            .assertTextEquals(ctx.getString(R.string.task_delegation_kind_index))
        composeTestRule.onAllNodesWithTag("task-delegation-kind-runner")[0]
            .assertTextEquals(ctx.getString(R.string.task_delegation_runner_waiting))
        composeTestRule.onAllNodesWithTag("task-delegation-assignment-picker")[0]
            .assertTextEquals(ctx.getString(R.string.task_delegation_assignment_automatic))
    }

    @Test
    fun runnerThisDeviceAndOtherResolveToTheExpectedLabels() {
        render(
            rows = listOf(
                row(taskKind = "backup-upload", runner = FfiRunnerStatus.ThisDevice),
                row(
                    taskKind = "content-rescore",
                    runner = FfiRunnerStatus.Other(who = FfiParticipantRef.Device(deviceId = "dev1")),
                ),
            ),
            deviceLabels = mapOf("dev1" to "My Laptop"),
        )
        composeTestRule.onAllNodesWithTag("task-delegation-kind-runner")[0]
            .assertTextEquals(ctx.getString(R.string.task_delegation_runner_this_device))
        composeTestRule.onAllNodesWithTag("task-delegation-kind-runner")[1]
            .assertTextEquals(ctx.getStringFmt(R.string.task_delegation_runner_other_device, "My Laptop"))
    }

    @Test
    fun runnerOtherFallsBackToShortIdWhenDeviceLabelUnknown() {
        render(
            rows = listOf(
                row(runner = FfiRunnerStatus.Other(who = FfiParticipantRef.Device(deviceId = "unknown-dev"))),
            ),
            deviceLabels = emptyMap(),
        )
        composeTestRule.onNodeWithTag("task-delegation-kind-runner")
            .assertTextEquals(ctx.getStringFmt(R.string.task_delegation_runner_other_device, "short-unknown-dev"))
    }

    @Test
    fun pickerRendersPinOptionsVerbatimNeverThisDeviceOnAndroid() {
        // android's VM passes ViewerOnly, so `pin_options` never carries
        // ThisDevice — a pin made on another device is still rendered
        // (participants.md § The assignment picker's escapability corollary).
        render(
            rows = listOf(
                row(
                    assignment = FfiPinOption.Other(who = FfiParticipantRef.Device(deviceId = "dev1")),
                    pinOptions = listOf(
                        FfiPinOption.Automatic,
                        FfiPinOption.Other(who = FfiParticipantRef.Device(deviceId = "dev1")),
                    ),
                ),
            ),
            deviceLabels = mapOf("dev1" to "My Laptop"),
        )
        composeTestRule.onNodeWithTag("task-delegation-assignment-picker").assertTextEquals("My Laptop")
        composeTestRule.onNodeWithTag("task-delegation-assignment-picker").performClick()
        composeTestRule.onNodeWithText(ctx.getString(R.string.task_delegation_assignment_automatic)).assertExists()
        // "My Laptop" now appears twice: the closed field's own current value
        // (still in the tree while the menu is open) plus the menu item.
        composeTestRule.onAllNodesWithText("My Laptop").assertCountEquals(2)
        composeTestRule.onNodeWithText(ctx.getString(R.string.task_delegation_assignment_this_device))
            .assertDoesNotExist()
    }

    @Test
    fun selectingAnOptionFiresOnSetAssignmentWithTaskKindAndOption() {
        var got: Pair<String, FfiPinOption>? = null
        render(
            rows = listOf(
                row(
                    taskKind = "backup-upload",
                    assignment = FfiPinOption.Automatic,
                    pinOptions = listOf(
                        FfiPinOption.Automatic,
                        FfiPinOption.Other(who = FfiParticipantRef.Device(deviceId = "dev1")),
                    ),
                ),
            ),
            deviceLabels = mapOf("dev1" to "My Laptop"),
            onSetAssignment = { kind, option -> got = kind to option },
        )
        composeTestRule.onNodeWithTag("task-delegation-assignment-picker").performClick()
        composeTestRule.onNodeWithText("My Laptop").performClick()
        assertEquals("backup-upload" to FfiPinOption.Other(who = FfiParticipantRef.Device(deviceId = "dev1")), got)
    }
}
