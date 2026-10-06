package com.fauna.app.ui.screen.backups

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.ui.viewmodel.RestoreProgress
import com.fauna.ffi.FfiSnapshotSummary
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [LocalRestoreContent] (the backups-page
 * local-restore action card, backups.md § Restore from backup destination): the
 * friction bar (`restore-confirm-button` enables only when the typed text matches
 * the selected snapshot id), the per-kind checkboxes, and the disabled
 * `restore-source-select` at zero destinations. Renders with seeded state — no
 * Hilt, no VM, no FFI native calls.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class RestoreActionContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun snap(id: Long = 42, kind: String? = "mail") = FfiSnapshotSummary(
        id = id,
        createdAt = 100,
        messageKind = kind,
        fileCount = 1,
        totalBytes = 10,
        deviceId = null,
    )

    private fun render(
        snapshots: List<FfiSnapshotSummary> = listOf(snap()),
        hasDestinations: Boolean = false,
        progress: RestoreProgress = RestoreProgress.IDLE,
        configAbsent: Boolean = false,
        onRestore: (Long, String) -> Unit = { _, _ -> },
    ) {
        composeTestRule.setContent {
            LocalRestoreContent(
                snapshots = snapshots,
                hasDestinations = hasDestinations,
                progress = progress,
                configAbsent = configAbsent,
                onRestore = onRestore,
            )
        }
    }

    @Test
    fun sourceSelect_disabledAtZeroDestinations() {
        render(hasDestinations = false)
        composeTestRule.onNodeWithTag("restore-source-select").assertIsNotEnabled()
    }

    @Test
    fun sourceSelect_enabledWhenDestinationsExist() {
        render(hasDestinations = true)
        composeTestRule.onNodeWithTag("restore-source-select").assertIsEnabled()
    }

    @Test
    fun kindsCheckboxes_renderBothCheckedByDefault_andToggle() {
        render()
        composeTestRule.onNodeWithTag("restore-kinds-checkboxes").assertIsDisplayed()
        val checks = composeTestRule.onAllNodesWithTag("restore-kind-checkbox")
        checks.assertCountEquals(2)
        checks[0].assertIsOn()
        checks[1].assertIsOn()
        checks[0].performClick()
        checks[0].assertIsOff()
    }

    @Test
    fun confirmButton_disabledUntilTypedIdMatchesSelectedSnapshot() {
        var restored: Pair<Long, String>? = null
        render(snapshots = listOf(snap(id = 42)), onRestore = { id, c -> restored = id to c })

        // Friction bar closed: nothing typed → disabled.
        composeTestRule.onNodeWithTag("restore-confirm-button").assertIsNotEnabled()

        // Wrong id → still disabled.
        composeTestRule.onNodeWithTag("restore-confirm-input").performTextInput("41")
        composeTestRule.onNodeWithTag("restore-confirm-button").assertIsNotEnabled()

        // Correct id → enabled; click dispatches the selected snapshot's id.
        composeTestRule.onNodeWithTag("restore-confirm-input").performTextClearance()
        composeTestRule.onNodeWithTag("restore-confirm-input").performTextInput("42")
        composeTestRule.onNodeWithTag("restore-confirm-button").assertIsEnabled()
        composeTestRule.onNodeWithTag("restore-confirm-button").performClick()
        assert(restored == 42L to "42") { "expected restore(42, \"42\"), got $restored" }
    }

    @Test
    fun emptySnapshots_pickerDisabled_andConfirmStaysDisabled() {
        render(snapshots = emptyList())
        composeTestRule.onNodeWithTag("restore-snapshot-select").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("restore-confirm-input").performTextInput("42")
        composeTestRule.onNodeWithTag("restore-confirm-button").assertIsNotEnabled()
    }

    @Test
    fun progress_rendersStepText() {
        render(progress = RestoreProgress.RUNNING)
        composeTestRule.onNodeWithTag("restore-progress").assertIsDisplayed()
    }

    /** `restore-warning` renders only after a restore whose reply said
     *  `config_present == false`, carrying the shared string; absent otherwise. */
    @Test
    fun restoreWarning_rendersOnlyWhenConfigurationAbsent() {
        render(progress = RestoreProgress.DONE, configAbsent = false)
        composeTestRule.onNodeWithTag("restore-warning").assertDoesNotExist()
    }

    @Test
    fun restoreWarning_carriesTheSharedStringWhenConfigurationAbsent() {
        render(progress = RestoreProgress.DONE, configAbsent = true)
        // assertExists, not assertIsDisplayed: the card does not scroll itself,
        // so the line sits below this unscrolled host's fold.
        composeTestRule.onNodeWithTag("restore-warning").assertExists()
        composeTestRule.onNodeWithTag("restore-warning").assertTextEquals(
            androidx.test.core.app.ApplicationProvider
                .getApplicationContext<android.content.Context>()
                .getString(com.fauna.app.R.string.backups_restore_warning_config_absent),
        )
    }
}
