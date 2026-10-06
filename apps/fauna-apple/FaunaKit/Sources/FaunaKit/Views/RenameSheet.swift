import SwiftUI

/// Rename-thread sheet — `thread-rename-field` + `thread-rename-confirm`.
/// Opened from `thread-rename-button` (visible only on MLS groups, where
/// `capabilities.supportsRename`). Rename is pure client UI state (unlike
/// add-participant, which the manager owns via `snapshot.addParticipant`);
/// confirm commits via `ConversationsVM.renameThread`. Shared by the macOS +
/// iOS conversations views — macOS hosts it as a `.sheet` on the conversations
/// page, iOS on the thread-detail screen; the *content* is identical.
public struct RenameSheet: View {
    public let threadId: ThreadId
    public let current: String

    public init(threadId: ThreadId, current: String) {
        self.threadId = threadId
        self.current = current
    }

    @Environment(ConversationsVM.self) private var vm
    @Environment(\.dismiss) private var dismiss
    @State private var text: String = ""

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L.conversations.unified.threadRename).font(.headline)
            TextField(L.conversations.unified.threadRenamePlaceholder, text: $text)
                .textFieldStyle(.roundedBorder)
                .frame(minWidth: 280)
                .accessibilityIdentifier(Ids.threadRenameField)
                // The sheet content registers in-process, but a bare
                // `.accessibilityIdentifier` is invisible to the in-process driver
                // (it only sees `automation*` modifiers) — without this the field
                // 404s under the in-process e2e (the old XCUITest path read the
                // raw a11y tree, so this regressed silently at the migration).
                .automationField(Ids.threadRenameField, text: $text)
                .onSubmit { confirm() }
            HStack {
                Spacer()
                Button(L.common.cancel) { dismiss() }
                Button(L.common.save) { confirm() }
                    .buttonStyle(.borderedProminent)
                    .disabled(text.trimmingCharacters(in: .whitespaces).isEmpty)
                    .accessibilityIdentifier(Ids.threadRenameConfirm)
                    .automationActivate(
                        Ids.threadRenameConfirm,
                        isEnabled: { !text.trimmingCharacters(in: .whitespaces).isEmpty }
                    ) { confirm() }
            }
        }
        .padding(20)
        .onAppear { text = current }
    }

    private func confirm() {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        Task { await vm.renameThread(threadId, trimmed) }
        dismiss()
    }
}
