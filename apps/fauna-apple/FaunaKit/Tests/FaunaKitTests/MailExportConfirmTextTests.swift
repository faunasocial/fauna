import Foundation
import Testing
@testable import FaunaKit

#if !FAUNA_KIDS
// The Confirm step's figures come from the shared `exportConfirmEstimate`
// (mail-export.md § Wizard steps step 3); the app sums nothing itself.

@Test func confirmTextFoldsEstimatedBytesAndTimeIntoTheSummary() {
    let boxes = [
        MailboxOption(name: "INBOX", selected: true, messageCount: 10, sealedBytes: 4_096),
        MailboxOption(name: "Sent", selected: true, messageCount: 5, sealedBytes: 2_048),
        MailboxOption(name: "Archive", selected: false, messageCount: 99, sealedBytes: 999_999),
    ]
    let estimate = exportConfirmEstimate(mailboxes: boxes)
    #expect(estimate.messages == 15)
    #expect(estimate.sealedBytes == 6_144)

    let text = MailExportConfirmText.make(format: "mbox", mailboxes: boxes)
    let counts = L.mailExport.confirmSummaryCountFmt(format: "mbox", mailboxes: "2", messages: "15")
    #expect(text.summary == L.mailExport.confirmSummaryEstimatedFmt(
        summary: counts,
        bytes: ValueFormat.byteSize(UInt64(6_144)),
        duration: ValueFormat.durationSecs(estimate.secs)))
    #expect(!text.showsEstimateUnavailable)
}

@Test func confirmTextKeepsBareCountsAndNoteWhenMessagesCarryNoBytes() {
    let boxes = [MailboxOption(name: "INBOX", selected: true, messageCount: 7, sealedBytes: 0)]
    let text = MailExportConfirmText.make(format: "mbox", mailboxes: boxes)
    #expect(text.summary == L.mailExport.confirmSummaryCountFmt(
        format: "mbox", mailboxes: "1", messages: "7"))
    #expect(text.showsEstimateUnavailable)
}

@Test func confirmTextWithNothingSelectedShowsNoNote() {
    let boxes = [MailboxOption(name: "INBOX", selected: false, messageCount: 7, sealedBytes: 100)]
    let text = MailExportConfirmText.make(format: "mbox", mailboxes: boxes)
    #expect(!text.showsEstimateUnavailable)
}
#endif
