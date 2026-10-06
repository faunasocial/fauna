package com.fauna.app.ui.screen.backups

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_backups_machine.BackupOp
import uniffi.fauna_backups_machine.RowIntegrity
import uniffi.fauna_backups_machine.SnapshotRow
import uniffi.fauna_backups_machine.SnapshotState

/**
 * Pins android's adoption of the shared `fauna_backups_machine::{busy_text,
 * snapshot_state_text, snapshot_integrity_text}` faces over `com.fauna.ffi`
 * (`docs/goal/ui/backups.md` § Where logic lives).
 * Android hand-rolled these three mappings before the lift; this test is the
 * row-text safety net the lift's own definition of success calls for, since
 * only windows carried one before.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class SnapshotRowTextTest {

    private val context: Context get() = ApplicationProvider.getApplicationContext()

    private fun row(state: SnapshotState, integrity: RowIntegrity = RowIntegrity.UNKNOWN) =
        SnapshotRow(
            id = 1,
            createdAt = 1_700_000_000L,
            fileCount = 3,
            totalBytes = 1_024,
            deviceId = null,
            tags = emptyList(),
            state = state,
            integrity = integrity,
        )

    @Test
    fun everyBackupOpNamesItselfOnTheBusyLine() {
        assertEquals(context.getString(R.string.backups_busy_create), busyText(context, BackupOp.CREATE))
        assertEquals(context.getString(R.string.backups_busy_delete), busyText(context, BackupOp.DELETE))
        assertEquals(context.getString(R.string.backups_busy_prune), busyText(context, BackupOp.PRUNE))
        assertEquals(context.getString(R.string.backups_busy_refresh), busyText(context, BackupOp.REFRESH))
        assertEquals(context.getString(R.string.backups_busy_refresh), busyText(context, BackupOp.DETAIL))
    }

    @Test
    fun anActiveSnapshotRendersNoLifecycleSuffix() {
        val text = snapshotRowText(context, row(SnapshotState.Active))
        assertFalse(text.contains(context.getString(R.string.backups_snapshot_state_deletion_pending_undated)))
        assertFalse(text.contains(context.getString(R.string.backups_snapshot_state_soft_deleted_undated)))
    }

    @Test
    fun aDeletionPendingSnapshotWithNoDeadlineRendersTheUndatedSuffix() {
        val text = snapshotRowText(context, row(SnapshotState.DeletionPending(executeAfter = null)))
        assertTrue(text.contains(context.getString(R.string.backups_snapshot_state_deletion_pending_undated)))
    }

    @Test
    fun aDeletionPendingSnapshotWithADeadlineRendersTheDatedSuffix() {
        val text = snapshotRowText(context, row(SnapshotState.DeletionPending(executeAfter = 1_700_100_000L)))
        // The dated template substitutes `{when}` with the app's own
        // relative-time rendering — the shared fn owns only which key fires.
        // "cancel before" is unique to the dated template: the undated string
        // ("Deletion scheduled") is itself a PREFIX of it, so a plain
        // `!contains(undated)` check would fail on a correct dated render too.
        assertTrue(text.contains("cancel before"))
    }

    @Test
    fun aSoftDeletedSnapshotWithNoDeadlineRendersTheUndatedSuffix() {
        val text = snapshotRowText(context, row(SnapshotState.SoftDeleted(purgeAfter = null)))
        assertTrue(text.contains(context.getString(R.string.backups_snapshot_state_soft_deleted_undated)))
    }

    @Test
    fun unknownIntegrityPaintsNothing() {
        val text = snapshotRowText(context, row(SnapshotState.Active, RowIntegrity.UNKNOWN))
        assertFalse(text.contains(context.getString(R.string.backups_snapshot_integrity_ok)))
        assertFalse(text.contains(context.getString(R.string.backups_snapshot_integrity_implicated)))
    }

    @Test
    fun checkedOkIntegrityRendersItsVerdict() {
        val text = snapshotRowText(context, row(SnapshotState.Active, RowIntegrity.CHECKED_OK))
        assertTrue(text.contains(context.getString(R.string.backups_snapshot_integrity_ok)))
    }

    @Test
    fun implicatedIntegrityRendersItsVerdict() {
        val text = snapshotRowText(context, row(SnapshotState.Active, RowIntegrity.IMPLICATED))
        assertTrue(text.contains(context.getString(R.string.backups_snapshot_integrity_implicated)))
    }
}
