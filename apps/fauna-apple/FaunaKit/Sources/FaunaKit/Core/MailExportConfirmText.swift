// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import Foundation

/// The mail-export wizard's Confirm step text (`docs/goal/behavior/mail-export.md`
/// § Wizard steps step 3): the message total, estimated bytes and estimated time
/// all come from the ONE shared computation `exportConfirmEstimate`; nothing is
/// summed here. Mirrors tui's `confirm_step_elements` and android's `ConfirmStep`.
struct MailExportConfirmText: Equatable {
    /// The `mail-export-confirm-summary` line.
    let summary: String
    /// Whether the "not estimated" note is shown under the summary (selected
    /// messages that carry zero bytes — a nest that reports no sizes).
    let showsEstimateUnavailable: Bool

    /// `mailboxes` carries the selection AS RENDERED (the local echo already
    /// applied), so the step never disagrees with the checkboxes.
    static func make(format: String, mailboxes: [MailboxOption]) -> MailExportConfirmText {
        let estimate = exportConfirmEstimate(mailboxes: mailboxes)
        let counts = L.mailExport.confirmSummaryCountFmt(
            format: format,
            mailboxes: String(mailboxes.filter { $0.selected }.count),
            messages: String(estimate.messages))
        let summary = estimate.sealedBytes == 0
            ? counts
            : L.mailExport.confirmSummaryEstimatedFmt(
                summary: counts,
                bytes: ValueFormat.byteSize(estimate.sealedBytes),
                duration: ValueFormat.durationSecs(estimate.secs))
        return MailExportConfirmText(
            summary: summary,
            showsEstimateUnavailable: estimate.messages > 0 && estimate.sealedBytes == 0)
    }
}
#endif
