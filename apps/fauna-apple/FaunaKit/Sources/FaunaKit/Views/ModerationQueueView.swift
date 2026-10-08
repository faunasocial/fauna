import SwiftUI

/// The shared standalone **Moderation queue** page (both apple apps, priority
/// #2) — the user's view onto why their *own* content was labeled / quarantined /
/// rejected, and their lever to correct it (moderation.md § Goal). Presentation
/// over the **union** of the server `fauna.moderation.actions` obligation rows and
/// the client's own post-decrypt local detections (moderation.md § Layout & flow):
/// one `moderation-queue` row per merged `QueueRow`, each carrying a
/// `content-label-badge` (the category, via the shared `content_label_style` map)
/// + an *optional* enforcement action (via the shared `obligation_action_label`
/// map — blank for a local detection) + a truncated content ref + confidence, with
/// a `train-correction-button`. Mirrors linux's `views/moderation.rs`.
///
/// Unified shape: a standalone page on standalone clients (macOS `moderation`
/// sidebar tab; iOS the `moderation` More entry), the **same IDs** either way
/// (moderation.md § Architectural rules 1). Spam *preferences*
/// (`spam-moderation-controls`) live on the Settings page — the queue consumes
/// those preferences but does not host them.
public struct ModerationQueueView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = ModerationQueueVM()

    public init() {}

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Section caption above the queue (mirrors linux + Windows'
            // "Enforcement Actions").
            Text(L.moderation.enforcementTitle)
                .font(.headline)
                .padding(.horizontal, 16)
                .padding(.top, 12)
                .padding(.bottom, 6)

            // Page-level error surface (training-submission failure) — built
            // hidden, shown only on error (moderation.md § Errors & edge cases).
            if let error = vm.errorMessage {
                ErrorBanner(message: error)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 6)
            }

            queue
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        // Keyed on the session's client, not one-shot: iOS reaches this page as a
        // More destination, which the switch teardown does not unmount, and the VM
        // held the outgoing account's moderation client, conversations session and
        // mail-settings machine behind `== nil` guards — so the survival was
        // permanent and a correction would have written to that account
        // (`ModerationQueueVM.reset()` spells it out). The nil-client phase drops
        // them; the incoming client rebuilds. `account-scoping.md` § The scoping
        // taxonomy, the "reused shell" case. macOS unmounts the whole window shell on a switch, so there the reset is
        // redundant — carried for uniformity, as `SearchVM`'s is.
        .task(id: SessionKey(client)) {
            guard let client else {
                vm.reset()
                return
            }
            await vm.configure(api: client.api)
        }
    }

    private var queue: some View {
        ScrollView {
            VStack(spacing: 0) {
                queueRows
                reportsSection
            }
        }
        // Container id + `.contain` so the child row ids (`content-label-badge`,
        // `train-correction-button`) stay queryable alongside it (apple container
        // a11y rule); `automationValue` registers it for the in-process driver so
        // `is_visible("moderation-queue")` holds even on an empty queue.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.moderationQueue)
        .automationValue(Ids.moderationQueue, text: { "\(vm.rows.count)" })
    }

    @ViewBuilder
    private var queueRows: some View {
        Group {
            if vm.rows.isEmpty {
                // `rows: []` is the empty state, not an error.
                VStack(spacing: 8) {
                    Image(systemName: "checkmark.shield")
                        .font(.largeTitle)
                        .foregroundStyle(.secondary)
                    Text(L.moderation.noActions)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity)
                .padding(.top, 48)
                .padding(.horizontal, 16)
            } else {
                VStack(spacing: 0) {
                    ForEach(vm.rows, id: \.contentId) { row in
                        ModerationQueueRow(row: row) {
                            await vm.correct(row: row)
                        }
                        Divider()
                    }
                }
            }
        }
    }

    /// The reporter's own ledger (`moderation-reports-section`;
    /// moderation.md § User-initiated reporting → *What the reporter is told*):
    /// every report this user filed, one flat `moderation-report-item` per row
    /// with a `moderation-report-withdraw-button` on the open ones. Read beside
    /// the queue on every Moderation entry; the empty line paints only off the
    /// `loaded` bit.
    private var reportsSection: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(renderLocalizedText(reportLedgerTitle()))
                .font(.headline)
                .padding(.top, 16)
            if vm.reportsLoaded && vm.reports.isEmpty {
                Text(renderLocalizedText(reportLedgerEmpty()))
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            ForEach(vm.reports, id: \.reportId) { row in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    let line = ModerationQueueVM.ledgerLine(row)
                    Text(line)
                        .font(.caption)
                        .accessibilityIdentifier(Ids.moderationReportItem)
                        .automationValue(Ids.moderationReportItem, text: { line })
                    Spacer()
                    if row.canWithdraw {
                        Button(L.moderation.report.withdraw) {
                            Task { await vm.withdrawReport(reportId: row.reportId) }
                        }
                        .controlSize(.small)
                        .accessibilityIdentifier(Ids.moderationReportWithdrawButton)
                        .automationActivate(Ids.moderationReportWithdrawButton) {
                            Task { await vm.withdrawReport(reportId: row.reportId) }
                        }
                    }
                }
            }
            if !vm.reportStatus.isEmpty {
                Text(vm.reportStatus).font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 16)
        .padding(.bottom, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.moderationReportsSection)
        .automationValue(Ids.moderationReportsSection, text: { "\(vm.reports.count)" })
    }
}

/// One `moderation-queue` row: category badge + an *optional* enforcement action
/// (blank for a local-only detection — never fabricate one, moderation.md § Don't
/// do these) + content ref + confidence, with a single `train-correction-button`.
/// `onCorrect` fires the same correction whether driven by a human tap or the
/// in-process `automationActivate`, so the two can't diverge.
private struct ModerationQueueRow: View {
    let row: QueueRow
    let onCorrect: () async -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    ContentLabelBadge(category: row.category)
                    // The enforcement action taken, via the shared discriminant →
                    // label map (co-located with the enum in `fauna_core::obligation`).
                    // `nil` for a local detection — blank action column.
                    if let action = row.action {
                        Text(renderLocalizedText(obligationActionLabel(action: action)))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                // Content ref (type + truncated id) — an opaque correlation handle;
                // shared-Rust `short_id` (12 chars + …) via the UniFFI face (priority #2/#4).
                Text("\(row.contentType) · \(shortId(hex: row.contentId))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text("\(ModerationQueueVM.confidencePercent(row.confidencePerMille))% \(L.moderation.confidence)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            correctButton
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 16)
    }

    /// The correction control, gated only where it actually needs a nest.
    ///
    /// A **server** row's correction must reach the nest either way — the sealed
    /// client-write path ends in `fauna.bridges.put_spam_model` and the fallback
    /// is `fauna.moderation.train`, both `OnlineOnly` — so it declares.
    ///
    /// A **local** row's does not: its primary effect is
    /// `moderationRemoveLocalDetection`, client-side state that succeeds with no
    /// nest, and the model train it additionally attempts is best-effort
    /// (`try?`) by construction. Gating it would grey a control that works, which
    /// is the over-claim rulings 1–3 exist to prevent — so the declaration rides
    /// the same discriminant `ModerationQueueVM.correct` branches on, rather than
    /// a blanket gate on the page.
    @ViewBuilder
    private var correctButton: some View {
        let button = Button(L.moderation.correct) {
            Task { await onCorrect() }
        }
        .controlSize(.small)
        .accessibilityIdentifier(Ids.trainCorrectionButton)
        .automationActivate(Ids.trainCorrectionButton) {
            Task { await onCorrect() }
        }

        if row.source == .local {
            button
        } else {
            button.faunaGate("fauna.moderation.train")
        }
    }
}
