package com.fauna.app.ui.screen.backups

import android.content.Context
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.util.LocalConnectionState
import com.fauna.app.ui.viewmodel.RestoreProgress
import com.fauna.ffi.FfiBackupDestinationView
import com.fauna.ffi.FfiConnectionState
import com.fauna.ffi.FfiDestinationForm
import com.fauna.ffi.carveOutPossibility
import com.fauna.ffi.destinationFormPossibility
import com.fauna.ffi.snapshotDeletePossibility
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_backups_machine.BackupFolderRow
import uniffi.fauna_backups_machine.BackupsSnapshot
import uniffi.fauna_backups_machine.RowIntegrity
import uniffi.fauna_backups_machine.SnapshotRow
import uniffi.fauna_backups_machine.SnapshotState
import uniffi.fauna_core.Possibility

/**
 * The Backups page's "offer only what is possible" lift on android
 * (`docs/goal/ui/backups.md` § Errors & edge cases → *What the page offers*;
 * `docs/goal/ui/README.md` § Offer only what is possible, rules 2, 4, 6, 7).
 * Every answer here is the shared `fauna_backups_machine::possibility`
 * predicate over UniFFI, loaded from the host-built `libfauna_ffi` through JNA
 * (`just android-host-test`), so a blocked assertion can only pass when the
 * shared rule itself said so.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class BackupsPossibilityTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val context get() = ApplicationProvider.getApplicationContext<Context>()

    private fun key(answer: Possibility): String? = (answer as? Possibility.Blocked)?.reason?.key

    // ── The destination dialog ────────────────────────────────────────────

    @Test
    fun theDialogsCeremonyMapsOntoTheSharedForm() {
        assertEquals(FfiDestinationForm.ADD_NEST, destinationFormOf(editing = false, custodian = false))
        assertEquals(FfiDestinationForm.ADD_CLIENT_DEVICE, destinationFormOf(editing = false, custodian = true))
        assertEquals(FfiDestinationForm.EDIT_NEST, destinationFormOf(editing = true, custodian = false))
        assertEquals(FfiDestinationForm.EDIT_CLIENT_DEVICE, destinationFormOf(editing = true, custodian = true))
    }

    @Test
    fun theSharedFormAnswer_blankUrlBlocksANestForm_butNotAThisDeviceRename() {
        assertEquals(
            "backups.backup_destination_url_required",
            key(destinationFormPossibility(FfiDestinationForm.ADD_NEST, "", "")),
        )
        assertTrue(
            destinationFormPossibility(FfiDestinationForm.EDIT_CLIENT_DEVICE, "", "") is Possibility.Possible,
        )
    }

    private fun renderDestinations(
        destinations: List<FfiBackupDestinationView> = emptyList(),
        onEdit: (String, String, String) -> Unit = { _, _, _ -> },
    ) {
        composeTestRule.setContent {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                BackupDestinationsContent(
                    destinations = destinations,
                    working = false,
                    onAdd = { _, _ -> },
                    onEdit = onEdit,
                    onRemove = { _, _ -> },
                    kindOptions = KIND_OPTIONS,
                    isClientDevice = { it.kind == CLIENT_DEVICE },
                )
            }
        }
    }

    private fun node(tag: String) = composeTestRule.onNodeWithTag(tag).performScrollTo()

    @Test
    fun aNestFormWithABlankUrl_paintsTheConfirmDisabledWithTheUrlRequiredReason() {
        renderDestinations()
        node("backup-destination-add-button").performClick()
        node("backup-destination-add-confirm-button").assertIsNotEnabled()
        composeTestRule
            .onNodeWithText(context.getString(R.string.backups_backup_destination_url_required))
            .assertExists()
        node("backup-destination-url-input").performTextInput("https://b.example")
        node("backup-destination-add-confirm-button").assertIsEnabled()
    }

    @Test
    fun aThisDeviceRename_savesWithABlankUrl() {
        var edited: Triple<String, String, String>? = null
        val mine = FfiBackupDestinationView(
            destinationId = "id-mine",
            destinationNestUrl = "",
            displayName = "This phone",
            kind = CLIENT_DEVICE,
            custodianDeviceId = "dev-a",
            capacityCapBytes = null,
            unattested = false,
        )
        renderDestinations(destinations = listOf(mine), onEdit = { id, url, name -> edited = Triple(id, url, name) })
        node("backup-destination-edit-button").performClick()
        node("backup-destination-name-input").performTextClearance()
        node("backup-destination-name-input").performTextInput("Old phone")
        node("backup-destination-add-confirm-button").assertIsEnabled().performClick()
        assertEquals(Triple("id-mine", "", "Old phone"), edited)
    }

    // ── The snapshot rows ─────────────────────────────────────────────────

    private fun row(id: Long, state: SnapshotState) = SnapshotRow(
        id = id,
        createdAt = 1_700_000_000L,
        fileCount = 1,
        totalBytes = 1,
        deviceId = null,
        tags = emptyList(),
        state = state,
        integrity = RowIntegrity.UNKNOWN,
    )

    private fun page(rows: List<SnapshotRow>, selected: String? = "f") = BackupsSnapshot(
        folders = listOf(BackupFolderRow(name = "f", snapshotCount = 0, lastSnapshotAt = null, messageKind = null)),
        selectedFolder = selected,
        snapshots = rows,
        lastBackedUp = null,
        inProgressOp = null,
        checkResult = null,
        prunePreview = null,
        detail = null,
        error = null,
    )

    @Test
    fun aPendingRowsSoftDelete_isRefusedWithItsReason_whileAnActiveRowsIsOffered() {
        val snap = page(listOf(row(1, SnapshotState.Active), row(2, SnapshotState.DeletionPending(executeAfter = null))))
        assertTrue(softDeleteAnswer(snap, 1) is Possibility.Possible)
        assertEquals("backups.snapshot_delete_already_pending", key(softDeleteAnswer(snap, 2)))
        assertEquals(
            "backups.snapshot_delete_already_pending",
            key(snapshotDeletePossibility(snap.snapshots[1])),
        )
        // A row no longer listed is not offered at all.
        assertTrue(softDeleteAnswer(snap, 9) is Possibility.Absent)
    }

    @Test
    fun theSetActionsNeedASelectedFolder() {
        assertTrue(setActionAnswer(page(emptyList())) is Possibility.Possible)
        assertEquals(
            "backups.snapshot_select_folder_first",
            key(setActionAnswer(page(emptyList(), selected = null))),
        )
        assertEquals("common.still_loading", key(setActionAnswer(null)))
    }

    // ── The carve-out fetch (a snapshot file's download) ──────────────────

    @Test
    fun theCarveOutAnswer_blocksOnlyWhileOffline() {
        assertEquals("common.needs_nest", key(carveOutPossibility("disconnected")))
        assertTrue(carveOutPossibility("connected") is Possibility.Possible)
    }

    private fun renderDownload(state: FfiConnectionState, onDownload: () -> Unit = {}) {
        composeTestRule.setContent {
            CompositionLocalProvider(LocalConnectionState provides state) {
                SnapshotFileDownloadButton(onDownload = onDownload, onRefuse = {})
            }
        }
    }

    @Test
    fun theFileDownload_isPaintedDisabledWithItsReason_whileOffline() {
        var downloads = 0
        renderDownload(FfiConnectionState.DISCONNECTED) { downloads++ }
        composeTestRule.onNodeWithTag("snapshot-file-download-button").assertIsNotEnabled().performClick()
        composeTestRule.onNodeWithText(context.getString(R.string.common_needs_nest)).assertIsDisplayed()
        assertEquals(0, downloads)
    }

    @Test
    fun theFileDownload_isLiveWhenConnected() {
        var downloads = 0
        renderDownload(FfiConnectionState.CONNECTED) { downloads++ }
        composeTestRule.onNodeWithTag("snapshot-file-download-button").assertIsEnabled().performClick()
        assertEquals(1, downloads)
    }

    // ── Restore ───────────────────────────────────────────────────────────

    @Test
    fun theRestoreSelectionMapsOntoTheSharedInputs() {
        val kinds = listOf("mail", "calendar")
        assertEquals(RestoreAsk("7", false), restoreAskOf(7, "mail", kinds, listOf(false, true)))
        assertEquals(RestoreAsk("7", true), restoreAskOf(7, null, kinds, listOf(false, false)))
        val none = restoreAskOf(null, null, kinds, listOf(true, true))
        assertNull(none.snapshotId)
    }

    @Test
    fun withNothingToRestore_theInputsAreDisabledWithTheSharedReason() {
        composeTestRule.setContent {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                LocalRestoreContent(
                    snapshots = emptyList(),
                    hasDestinations = false,
                    progress = RestoreProgress.IDLE,
                    configAbsent = false,
                    onRestore = { _, _ -> },
                )
            }
        }
        composeTestRule.onNodeWithTag("restore-confirm-input").assertIsNotEnabled()
        composeTestRule.onAllNodesWithTag("restore-kind-checkbox")[0].assertIsNotEnabled()
        composeTestRule.onNodeWithTag("restore-confirm-button").assertIsNotEnabled()
        assertFalse(
            composeTestRule
                .onAllNodesWithText(context.getString(R.string.backups_restore_no_snapshots))
                .fetchSemanticsNodes()
                .isEmpty(),
        )
    }

    private companion object {
        const val CLIENT_DEVICE = "client-device"
        val KIND_OPTIONS = listOf(
            DestinationKindOption("nest", "Another nest", isClientDevice = false),
            DestinationKindOption(CLIENT_DEVICE, "This device", isClientDevice = true),
        )
    }
}
