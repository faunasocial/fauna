import Testing
import Foundation
@testable import FaunaKit

/// The mail wizards' time summaries (`mail-export.md` § Wizard steps steps 4–5,
/// `mailbox-migration.md` § Wizard steps steps 5–6): the elapsed time and time
/// left come from the shared Rust `transfer_timing` (the REAL UniFFI export) and
/// render through the generated templates, matching tui / linux / web.

@Test func aRunningSessionReportsElapsedAndTimeLeft() {
    // 25 of 100 in 60 s → 75 left at the same rate is 180 s.
    let t = MailTransferText.timing(
        startedAt: 1_000, lastProgressAt: 1_030, running: true,
        processed: 25, total: 100, nowSecs: 1_060)
    #expect(t.elapsedSecs == 60)
    #expect(t.remainingSecs == 180)
    let base = L.mailExport.progressSummaryFmt(exported: "25", total: "100", skipped: "0", errored: "0")
    #expect(MailTransferText.exportProgress(summary: base, timing: t)
        == L.mailExport.progressRemainingFmt(
            summary: base, elapsed: ValueFormat.durationSecs(60), remaining: ValueFormat.durationSecs(180)))
}

@Test func aStoppedSessionFreezesAndHasNoTimeLeft() {
    let t = MailTransferText.timing(
        startedAt: 1_000, lastProgressAt: 1_090, running: false,
        processed: 100, total: 100, nowSecs: 4_600)
    #expect(t.elapsedSecs == 90)
    #expect(t.remainingSecs == nil)
    let base = L.mailImport.progressSummaryFmt(imported: "100", total: "100", skipped: "0", errored: "0")
    #expect(MailTransferText.importProgress(summary: base, timing: t)
        == L.mailImport.progressElapsedFmt(summary: base, elapsed: ValueFormat.durationSecs(90)))
}

@Test func doneCardsSayHowLongTheRunTook() {
    let t = MailTransferText.timing(
        startedAt: 1_000, lastProgressAt: 1_090, running: false,
        processed: 7, total: 7, nowSecs: 4_600)
    let took = ValueFormat.durationSecs(90)
    #expect(MailTransferText.exportDone(exported: 7, format: "mbox", bytes: 2048, savedPath: "", timing: t)
        == L.mailExport.doneSummaryTimedFmt(exported: "7", format: "mbox", bytes: "2048", elapsed: took))
    #expect(MailTransferText.exportDone(exported: 7, format: "mbox", bytes: 2048, savedPath: "/x.zst", timing: t)
        == L.mailExport.savedSummaryTimedFmt(
            exported: "7", format: "mbox", bytes: "2048", elapsed: took, path: "/x.zst"))
    #expect(MailTransferText.importDone(imported: 5, skipped: 1, errored: 1, timing: t)
        == L.mailImport.doneSummaryTimedFmt(imported: "5", skipped: "1", errored: "1", elapsed: took))
}

@Test func aSnapshotWithNoClockKeepsTheUntimedForms() {
    let none = MailTransferText.timing(
        startedAt: 0, lastProgressAt: 0, running: false, processed: 0, total: 0, nowSecs: 4_600)
    #expect(none.elapsedSecs == nil)
    #expect(MailTransferText.importDone(imported: 5, skipped: 1, errored: 1, timing: none)
        == L.mailImport.doneSummaryFmt(imported: "5", skipped: "1", errored: "1"))
    #expect(MailTransferText.exportDone(exported: 7, format: "mbox", bytes: 2048, savedPath: "", timing: none)
        == L.mailExport.doneSummaryFmt(format: "mbox", bytes: "2048"))
}
