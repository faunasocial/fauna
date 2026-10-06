import SwiftUI

/// The user-facing **mail-spam** page (`docs/goal/behavior/mail-spam.md`), shared
/// by macOS + iOS (one FaunaKit view, thin per-target call sites).
/// A dumb renderer of `MailSpamSnapshot` + dispatcher of
/// `MailSpamAction` over the shared `MailSpamMachine` (via `MailSpamVM`); no
/// business logic here. Element IDs match `tests/e2e-unified/ui.yaml` `mail-spam`
/// / `mail-spam-training-history-list` exactly. Reference implementation: linux
/// `apps/fauna-linux/src/settings/mail_spam.rs`.
///
/// **UI precedes backend:** the per-user Bayesian feedback loop is unbuilt today,
/// so the controls render but every action surfaces the seam's `unimplemented`
/// rejection via `error-message` and the training-history list stays empty (the
/// nest returns no rows). The reset is destructive ("cannot be undone") so it
/// goes through a confirmation dialog; Undo is reversible-by-retrain (no confirm).
///
/// On macOS this is the `mail-spam` sub-page of the Settings sidebar-swap shell
/// (`SettingsShellView`); on iOS it is a Settings sub-page (NavigationLink).
public struct MailSpamView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = MailSpamVM()

    /// Two-click inline reset confirm (no modal): `nil` = unarmed,
    /// `Ids.mailSpamResetModelButton` while armed (`tapArmedDelete`'s
    /// single-control convention — arm and confirm are the same button, so it
    /// takes its OWN id as the sentinel, mirroring `MailListsView`/
    /// `MailAliasesView`'s per-row `armedDeleteId`).
    @State private var resetArmedId: String?
    @State private var thresholdOverrideDraft = ""

    public init() {}

    public var body: some View {
        // Eager `ScrollView { VStack }`, NOT a lazy `Form` (rule 6 —
        // apple-e2e-automation.md § Registration rules). An iOS `Form` is a lazy
        // `List` whose off-screen sections are never realized, so they never
        // register a slot at all. Measured: with
        // `reportSharePublishedSection` below the fold, the `/tree` dump — which
        // lists hidden slots too — reported `<no published-list slots
        // registered>` while `mail-spam-reset-model-button` (top of the same
        // form) read `visible=True, count=1`, so
        // `report-share-published-list-item-hash` counted 0 for the whole retry
        // window of `test_mail_spam_report_share_toggle_and_published_list[ios]`.
        // Never-registered, not hidden — which is also what rules out the VM: a
        // hydrate that returned an empty list would still have registered the
        // section's `publishedEmpty` marker. macOS is exempt from rule 6 for
        // `Form` (AppKit realizes it eagerly), which is exactly why the identical
        // shared code passed there. The sibling pane rendering the same published
        // list — `PersonalizationView.signalSharePublishedSection` — was already
        // eager; this page was the last one still on `Form`. Cost: rows lose the
        // grouped `Form` styling (the accepted rule-6 production-UI tradeoff,
        // as in `MailAliasesView` / `PrivacySettingsView`).
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                controlsSection
                reportSharePublishedSection
                historySection
                // Absent from the tree when nil — a registered-but-empty
                // element would read as present.
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageTitle(L.mailSpam.title)
        .task {
            guard let client else { return }
            await vm.configure(api: client.api)
        }
    }

    // MARK: - Controls (reset + contribute toggle)

    private var controlsSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L.mailSpam.description)
                .font(.caption)
                .foregroundStyle(.secondary)

            Button(
                resetArmedId == Ids.mailSpamResetModelButton ? L.mailSpam.resetConfirm : L.mailSpam.resetButton,
                role: .destructive
            ) {
                tapResetModel()
            }
            .accessibilityIdentifier(Ids.mailSpamResetModelButton)
            // `text:` reports the armed/unarmed label (without it `/element/text`
            // falls back to `""` regardless of the rendered title —
            // `AutomationRegistry.paintedTexts()`'s `entry.text?() ?? entry.value?()
            // ?? ""`), which is what outcome 10's `reset_button_text()` reads.
            .automationActivate(
                Ids.mailSpamResetModelButton,
                text: { resetArmedId == Ids.mailSpamResetModelButton ? L.mailSpam.resetConfirm : L.mailSpam.resetButton }
            ) {
                tapResetModel()
            }
            // Arm and confirm are the SAME control (two-click inline, no modal —
            // the mail-aliases/mail-lists delete idiom, and mail-spam.md outcome
            // 10's own spec: "the first press only arms ... the second press
            // resets"), promoted off a system `.confirmationDialog` that carried
            // no automation id at all. "Arming is
            // local" does NOT split this one, exactly as the delete buttons'
            // comments already note: arming a confirm that cannot fire is worse
            // than not arming it.
            .faunaGate("fauna.bridges.reset_spam_model")
            Text(L.mailSpam.resetSubtitle)
                .font(.caption)
                .foregroundStyle(.secondary)

            Toggle(L.mailSpam.contributeBaselineLabel, isOn: Binding(
                get: { vm.snapshot?.contributeBaseline ?? false },
                set: { on in Task { await vm.dispatch(.setContributeBaseline(contribute: on)) } }
            ))
            .accessibilityIdentifier(Ids.mailSpamContributeBaselineToggle)
            .automationActivate(
                Ids.mailSpamContributeBaselineToggle,
                value: { (vm.snapshot?.contributeBaseline ?? false) ? "on" : "off" }
            ) {
                let next = !(vm.snapshot?.contributeBaseline ?? false)
                Task { await vm.dispatch(.setContributeBaseline(contribute: next)) }
            }
            // Dispatch-on-change: the setter IS the commit, so it declares
            // (rule 1's third arm — there is no Save to carry it).
            .faunaGate("fauna.bridges.set_baseline_contribution")
            Text(L.mailSpam.contributeBaselineSubtitle)
                .font(.caption)
                .foregroundStyle(.secondary)

            // Report sharing (report-sharing.md § Client wire + transparency
            // surface) — a distinct k-anonymity mechanism from the baseline
            // toggle above, over FfiModerationClient directly (no
            // MailSpamMachine involvement). Non-optimistic: setReportShare
            // re-reads status from the nest, so `vm.reportShare` only ever
            // reflects a nest-confirmed value — that doubles as the round-trip
            // proof the e2e action layer reads via the toggle's `state` attr.
            Toggle(L.mailSpam.shareReportsLabel, isOn: Binding(
                get: { vm.reportShare },
                set: { on in Task { await vm.setReportShare(on) } }
            ))
            .accessibilityIdentifier(Ids.mailSpamShareReportsToggle)
            .automationActivate(
                Ids.mailSpamShareReportsToggle,
                value: { vm.reportShare ? "on" : "off" }
            ) {
                Task { await vm.setReportShare(!vm.reportShare) }
            }
            // NO gate, deliberately: this is `fauna.moderation.report_share.set`
            // — OfflineSafe by the shared table, unlike the baseline toggle two
            // rows up. The pairing is this page's own live-beside-dead witness.
            Text(L.mailSpam.shareReportsSubtitle)
                .font(.caption)
                .foregroundStyle(.secondary)

            // Per-account spam-folder threshold override (mail-spam.md) — a direct `MailAccountClient` RPC pair,
            // bypassing MailSpamMachine like report-sharing above. Type-then-
            // commit (no live keyboard debounce on a native field): a real
            // keystroke only mutates the local draft, `.onSubmit` (Return)
            // fires the actual commit. `automationField` gets its OWN Binding
            // whose `set` commits immediately — `/element/type` delivers the
            // whole final string in one call with no separate "press Return"
            // primitive, so for THIS shape (no save button, unlike
            // `BridgeNumberSettingRow`) the type call itself is automation's
            // only stand-in for submit. `.task(id:)` re-syncs the draft from
            // the nest-confirmed value only, never `.onAppear`.
            LabeledContent(L.mailSpam.thresholdOverrideLabel) {
                TextField("", text: $thresholdOverrideDraft)
                    #if os(iOS)
                    .keyboardType(.numberPad)
                    #endif
                    .multilineTextAlignment(.trailing)
                    .onSubmit {
                        Task { await vm.commitThresholdOverride(text: thresholdOverrideDraft) }
                    }
            }
            .accessibilityIdentifier(Ids.mailSpamThresholdOverrideInput)
            .automationField(Ids.mailSpamThresholdOverrideInput, text: Binding(
                get: { thresholdOverrideDraft },
                set: { newValue in
                    thresholdOverrideDraft = newValue
                    Task { await vm.commitThresholdOverride(text: newValue) }
                }
            ), pressKey: { key in
                // `/element/type` above already committed — the cross-app action
                // layer still presses Enter afterward (every other app's real
                // "type then Return commits" idiom, linux's `connect_activate`
                // shape), so this is a deliberate no-op success, not a missing
                // key door: refusing it would 409 a call every other app accepts.
                key == "Enter" ? nil : "\(Ids.mailSpamThresholdOverrideInput) takes no other named key"
            })
            .task(id: vm.thresholdOverrideText) {
                thresholdOverrideDraft = vm.thresholdOverrideText
            }
            Text(L.mailSpam.thresholdOverrideSubtitle)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// Reset-model action — shared by the button and its `.automationActivate`
    /// so the two never diverge (`tapDelete`'s pattern). `tapArmedDelete` takes
    /// the button's own id as the arm sentinel since there is only one control
    /// (promoted off a system `.confirmationDialog` that had no automation id
    /// at all).
    private func tapResetModel() {
        tapArmedDelete(Ids.mailSpamResetModelButton, armed: $resetArmedId) {
            await vm.dispatch(.resetModel)
        }
    }

    // MARK: - Report-share published list ("what this nest publishes")

    private var reportSharePublishedSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.mailSpam.publishedTitle)
                .font(.subheadline.weight(.semibold))
            Text(L.mailSpam.publishedDescription)
                .font(.caption)
                .foregroundStyle(.secondary)
            let published = vm.reportSharePublished
            if published.isEmpty {
                automationText(Ids.reportSharePublishedList, L.mailSpam.publishedEmpty)
                    .foregroundStyle(.secondary)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(Array(published.enumerated()), id: \.offset) { _, entry in
                        reportSharePublishedRow(entry)
                    }
                }
                .accessibilityIdentifier(Ids.reportSharePublishedList)
                .automationValue(Ids.reportSharePublishedList, text: { "" })
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private func reportSharePublishedRow(_ entry: FfiReportShareEntry) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            // Display title combines count + label ("N reporters", mirrors
            // linux's ActionRow title) — but the -count test id carries the
            // RAW number only (linux value_marker uses entry.count.to_string(),
            // not the composed title; the cross-app action layer's
            // published_reporter_count() asserts the bare digit string).
            Text("\(entry.count) \(L.mailSpam.publishedReporters)")
                .font(.headline)
            automationText(Ids.reportSharePublishedListItemHash, entry.contentHash)
                .font(.caption.monospaced())
                .textSelection(.enabled)
            HStack(spacing: 4) {
                automationText(Ids.reportSharePublishedListItemFactor, entry.factor)
                Text("·")
                automationText(Ids.reportSharePublishedListItemCount, "\(entry.count)")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.reportSharePublishedListItem)
        // Per-row presence entry so the flat in-process registry can `count`
        // published rows (mirrors admin-dns-domain / admin-dns-record).
        .automationValue(Ids.reportSharePublishedListItem, text: { entry.contentHash })
    }

    // MARK: - Training-history list

    private var historySection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L.mailSpam.historyTitle)
                .font(.subheadline.weight(.semibold))
            let events = vm.snapshot?.events ?? []
            if events.isEmpty {
                automationText(Ids.mailSpamTrainingHistoryList, L.mailSpam.empty)
                    .foregroundStyle(.secondary)
            } else {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(Array(events.enumerated()), id: \.element.historyIdHex) { _, event in
                        historyRow(event)
                    }
                }
                .accessibilityIdentifier(Ids.mailSpamTrainingHistoryList)
                .automationValue(Ids.mailSpamTrainingHistoryList, text: { "" })
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private func historyRow(_ event: SpamTrainingView) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            automationText(Ids.mailSpamTrainingHistoryListItemMessage, event.message)
                .font(.headline)
                .textSelection(.enabled)
            HStack(spacing: 8) {
                automationText(
                    Ids.mailSpamTrainingHistoryListItemLabel,
                    renderLocalizedText(trainingLabelBadge(label: event.label))
                )
                Text("·")
                automationText(
                    Ids.mailSpamTrainingHistoryListItemSource,
                    renderLocalizedText(trainingSourceBadge(source: event.source))
                )
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            automationText(
                Ids.mailSpamTrainingHistoryListItemCreatedAt,
                ValueFormat.absoluteDate(epochMs: event.createdAtMs, withTime: true)
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            // Undo is reversible-by-retrain (applies the inverse n-gram delta),
            // so no confirm — single-click dispatch.
            Button(L.mailSpam.undo) {
                Task { await vm.dispatch(.undoTraining(historyIdHex: event.historyIdHex)) }
            }
            .controlSize(.small)
            .accessibilityIdentifier(Ids.mailSpamTrainingHistoryListItemUndoButton)
            .automationActivate(Ids.mailSpamTrainingHistoryListItemUndoButton) {
                Task { await vm.dispatch(.undoTraining(historyIdHex: event.historyIdHex)) }
            }
            .faunaGate("fauna.bridges.put_spam_model")
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.mailSpamTrainingHistoryListItem)
        .automationValue(Ids.mailSpamTrainingHistoryListItem, text: { event.message })
    }
}
