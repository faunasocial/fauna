import SwiftUI

/// The shared report sheet and its acknowledgement line (`report-sheet`,
/// `report-status` — `moderation.md` § User-initiated reporting → *App surface*),
/// mounted ONCE per shell over whatever page is showing: the three verbs
/// (``FeedPostActionsButton``, the conversation message ⋯, an OTHER profile)
/// only call ``ReportSheetStore/open(_:)`` and this paints what is open. Shared
/// by the macOS and iOS shells (identical logic; priority #2).
///
/// Inline and `@State`-driven, NOT a system `.sheet` — the ⋯ menus' precedent
/// (`FeedPostActions`, `DmMessageBubble`): every id attaches to a real,
/// in-process-registered element, which a system sheet's content would not.
///
/// Every decision is shared Rust's (``ReportSheetStore/view``); this view paints
/// the fold and forwards the gestures. A failed send keeps the sheet and lands on
/// `error-message`; a failed block/hide lands there BESIDE the acknowledgement.
public struct ReportHost: View {
    @Environment(ContentPolicyStore.self) private var contentPolicy: ContentPolicyStore?
    @Environment(FaunaClient.self) private var client: FaunaClient?

    public init() {}

    public var body: some View {
        if let contentPolicy {
            content(contentPolicy)
        }
    }

    @ViewBuilder
    private func content(_ policy: ContentPolicyStore) -> some View {
        let store = policy.report
        VStack(alignment: .leading, spacing: 8) {
            if let view = store.view {
                sheet(store, view, policy: policy)
            }
            if !store.status.isEmpty {
                automationText(Ids.reportStatus, store.status)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // Built hidden, shown only on error (the cross-app `error-message` contract).
            ErrorBanner(message: store.errorMessage)
        }
        .frame(maxWidth: 480, alignment: .leading)
        .padding(.horizontal, 16)
    }

    private func sheet(
        _ store: ReportSheetStore, _ view: FfiReportSheetView, policy: ContentPolicyStore
    ) -> some View {
        let form = Bindable(store).form
        let send: () -> Void = {
            Task { await store.submit(api: client?.api, contentPolicy: policy) }
        }
        let canSend = view.canSubmit && !store.sending
        return VStack(alignment: .leading, spacing: 8) {
            Text(renderLocalizedText(view.title)).font(.headline)

            Picker(renderLocalizedText(view.reasonLabel), selection: Binding(
                get: { store.form.reason ?? "" },
                set: { store.form.reason = $0.isEmpty ? nil : $0 }
            )) {
                Text(renderLocalizedText(view.reasonLabel)).tag("")
                ForEach(view.reasons, id: \.reason) { option in
                    Text(renderLocalizedText(option.label)).tag(option.reason)
                }
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier(Ids.reportReasonSelect)
            .automationSelect(
                Ids.reportReasonSelect,
                value: { store.form.reason },
                options: { view.reasons.map(\.reason) },
                set: { store.form.reason = $0 })

            TextField(renderLocalizedText(view.noteLabel), text: form.note, axis: .vertical)
                .lineLimit(2...4)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.reportNoteInput)
                .automationField(Ids.reportNoteInput, text: form.note)

            // Rendered only for a sealed subject (a message; a gated post) — the
            // shared fold says so, the app never re-derives it.
            if view.showIncludeText {
                Toggle(renderLocalizedText(view.includeTextLabel), isOn: form.includeText)
                    .accessibilityIdentifier(Ids.reportIncludeTextCheckbox)
                    .automationActivate(
                        Ids.reportIncludeTextCheckbox,
                        value: { store.form.includeText ? "on" : "off" }
                    ) { store.form.includeText.toggle() }
            }

            Toggle(renderLocalizedText(view.blockAuthorLabel), isOn: form.blockAuthor)
                .accessibilityIdentifier(Ids.reportBlockAuthorCheckbox)
                .automationActivate(
                    Ids.reportBlockAuthorCheckbox,
                    value: { store.form.blockAuthor ? "on" : "off" }
                ) { store.form.blockAuthor.toggle() }

            if !view.canSubmit, let blocked = view.blockedReason {
                Text(renderLocalizedText(blocked)).font(.caption).foregroundStyle(.secondary)
            }

            HStack(spacing: 8) {
                Button(renderLocalizedText(view.submitLabel), action: send)
                    .disabled(!canSend)
                    .accessibilityIdentifier(Ids.reportSubmitButton)
                    .automationActivate(Ids.reportSubmitButton, isEnabled: { canSend }, perform: send)
                Button(renderLocalizedText(view.cancelLabel)) { store.cancel() }
                    .accessibilityIdentifier(Ids.reportCancelButton)
                    .automationActivate(Ids.reportCancelButton) { store.cancel() }
            }
        }
        .padding(12)
        .background(Color.secondary.opacity(0.12), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.reportSheet)
        // Presence anchor so the in-process driver can confirm the sheet opened —
        // a bare `.accessibilityIdentifier` on a container is invisible to the
        // registry (`DmMessageBubble`'s menu precedent).
        .automationValue(Ids.reportSheet, text: { "" })
    }
}
