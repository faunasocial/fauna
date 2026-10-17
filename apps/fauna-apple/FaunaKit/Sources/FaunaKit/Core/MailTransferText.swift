import Foundation

/// The time half of the two mail wizards' summaries: the elapsed time and time
/// left the progress step appends (`mail-export.md` § Wizard steps step 4,
/// `mailbox-migration.md` § Wizard steps step 5) and the time the Done card
/// reports (steps 5 / 6). The computation is the shared Rust
/// `fauna_client_mail_settings::transfer_timing` (priority #2); this only
/// reads the session's clock through it and renders the numbers with
/// `ValueFormat.durationSecs` and the generated templates, once for
/// `MailExportView` and `MailImportView`.
enum MailTransferText {
    /// The shared timing of one session at the device's clock. `processed`
    /// counts every message dealt with (done, skipped and failed alike).
    static func timing(
        startedAt: Int64, lastProgressAt: Int64, running: Bool,
        processed: UInt64, total: UInt64,
        nowSecs: Int64 = Int64(Date().timeIntervalSince1970)
    ) -> TransferTiming {
        transferTiming(
            startedAt: startedAt, lastProgressAt: lastProgressAt, running: running,
            processed: processed, total: total, nowSecs: nowSecs)
    }

    /// Export progress: `summary` plus elapsed and, while running, time left.
    static func exportProgress(summary: String, timing: TransferTiming) -> String {
        guard let elapsed = timing.elapsedSecs else { return summary }
        if let remaining = timing.remainingSecs {
            return L.mailExport.progressRemainingFmt(
                summary: summary, elapsed: ValueFormat.durationSecs(elapsed),
                remaining: ValueFormat.durationSecs(remaining))
        }
        return L.mailExport.progressElapsedFmt(
            summary: summary, elapsed: ValueFormat.durationSecs(elapsed))
    }

    /// Import progress: the `exportProgress` twin.
    static func importProgress(summary: String, timing: TransferTiming) -> String {
        guard let elapsed = timing.elapsedSecs else { return summary }
        if let remaining = timing.remainingSecs {
            return L.mailImport.progressRemainingFmt(
                summary: summary, elapsed: ValueFormat.durationSecs(elapsed),
                remaining: ValueFormat.durationSecs(remaining))
        }
        return L.mailImport.progressElapsedFmt(
            summary: summary, elapsed: ValueFormat.durationSecs(elapsed))
    }

    /// Export Done card: "X messages · format · Y bytes · took T", with the
    /// saved path once Download ran. A snapshot with no clock yet (not read
    /// from the nest) keeps the untimed form.
    static func exportDone(
        exported: UInt64, format: String, bytes: UInt64, savedPath: String, timing: TransferTiming
    ) -> String {
        if let elapsed = timing.elapsedSecs {
            let took = ValueFormat.durationSecs(elapsed)
            if !savedPath.isEmpty {
                return L.mailExport.savedSummaryTimedFmt(
                    exported: String(exported), format: format, bytes: String(bytes),
                    elapsed: took, path: savedPath)
            }
            return L.mailExport.doneSummaryTimedFmt(
                exported: String(exported), format: format, bytes: String(bytes), elapsed: took)
        }
        if !savedPath.isEmpty {
            return L.mailExport.savedSummaryFmt(format: format, bytes: String(bytes), path: savedPath)
        }
        return L.mailExport.doneSummaryFmt(format: format, bytes: String(bytes))
    }

    /// Import Done card: the counts and the time taken (untimed without a clock).
    static func importDone(imported: UInt64, skipped: UInt64, errored: UInt64, timing: TransferTiming) -> String {
        if let elapsed = timing.elapsedSecs {
            return L.mailImport.doneSummaryTimedFmt(
                imported: String(imported), skipped: String(skipped), errored: String(errored),
                elapsed: ValueFormat.durationSecs(elapsed))
        }
        return L.mailImport.doneSummaryFmt(
            imported: String(imported), skipped: String(skipped), errored: String(errored))
    }
}
