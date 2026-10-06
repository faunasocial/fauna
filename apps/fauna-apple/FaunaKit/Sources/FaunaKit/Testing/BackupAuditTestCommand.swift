import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// Shared handler for the cross-app `backup_audit_run_now` TestAgent command
/// (`ui/backups.md` § Audit-alert surface —
/// `tests/e2e-unified/tests/test_backups.py`'s two audit tests).
///
/// Runs one **real** audit pass — real connection to each configured
/// destination, real `fauna.backup.custody.list`, this client's own persisted
/// observation high-water — with only the clock shifted by `now_offset_secs`.
/// That shift is what a staleness proof needs and cannot fake any other way:
/// freshness floors a destination's high-water at its `added_at`, so a
/// destination enrolled seconds ago is *correctly* never stale in real time
/// (testing.md convention 14 — poke the clock, never sleep out a 48h+ window).
///
/// Lives in FaunaKit so the macOS + iOS shells share ONE implementation,
/// mirroring `DelegationClockTestCommand`. Drives the same shared Rust seam
/// linux/tui reach directly — `fauna_client_backup::audit_clock
/// ::set_clock_offset_secs`, reached here through the `test-helpers` UniFFI
/// free function `setBackupAuditClockOffsetSecs`.
///
/// ⚠ The offset is **process-wide and nothing auto-resets it** — the test
/// resets it to `0` after use, because a stale offset would silently lapse
/// the next audit pass the process runs.
public enum BackupAuditTestCommand {
    /// Apply the command. Returns `nil` on success, or a human-readable reason
    /// the caller must surface as a **loud** TestAgent failure — never a silent
    /// no-op (testing.md convention 11: honour the command or refuse audibly).
    @MainActor
    public static func apply(_ command: [String: Any]) async -> String? {
        let offset = (command["now_offset_secs"] as? NSNumber)?.int64Value
            ?? (command["now_offset_secs"] as? Int).map(Int64.init)
            ?? 0
        setBackupAuditClockOffsetSecs(offsetSecs: offset)

        guard let vm = BackupDestinationsVM.liveInstanceForTest else {
            return "backup_audit_run_now: the Backups page is not open "
                + "(no live BackupDestinationsVM), so the audit cannot re-run"
        }
        await vm.runAudit()
        return nil
    }
}

#endif
