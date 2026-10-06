package com.fauna.app.ui.screen.backups

import com.fauna.ffi.FfiDestinationAuditRow
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.fauna_core.BackupAuditAlertReason

/**
 * [auditAlertReasons] — the owner-side `backup-audit-alert` reasons of one audit
 * row (`docs/goal/ui/backups.md` § Audit-alert surface). The list is projected
 * by shared Rust at pass time (`DestinationAuditRecord::alert_reasons`), window
 * logic included, so these pin only that the shell hands every entry on, in
 * order, and invents none. The reasons stay opaque: any variant will do.
 */
class AuditAlertReasonsTest {

    private fun row(reasons: List<BackupAuditAlertReason>) = FfiDestinationAuditRow(
        destinationId = "id-a",
        lastPassedAt = null,
        alertReason = null,
        alertReasons = reasons,
    )

    @Test
    fun noAuditRowYet_yieldsNothing() {
        assertEquals(emptyList<BackupAuditAlertReason>(), auditAlertReasons(null))
    }

    @Test
    fun anEmptyList_yieldsNothing() {
        assertEquals(emptyList<BackupAuditAlertReason>(), auditAlertReasons(row(emptyList())))
    }

    @Test
    fun aSingleReason_yieldsOne() {
        val reasons = listOf(BackupAuditAlertReason.SelfReported)
        assertEquals(reasons, auditAlertReasons(row(reasons)))
    }

    @Test
    fun aStandingVerdictAndARecoveryWindow_yieldTwoInOrder() {
        val reasons = listOf(
            BackupAuditAlertReason.Overdue(sinceSecs = 9L),
            BackupAuditAlertReason.SelfReported,
        )
        assertEquals(reasons, auditAlertReasons(row(reasons)))
    }
}
