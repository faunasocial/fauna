import SwiftUI

/// New-thread composition — `dm-compose-form`: the `recipient-picker`, a
/// `group-conversation-hint` once there are ≥2 chips, then the same
/// `dm-compose-bar` grammar (running in its all-enabled mode — `capabilities:
/// nil` — since the rail isn't locked until the first chip resolves). Reads the
/// shared `ConversationsVM.newThreadCompose` state; renders nothing until
/// `startNewConversation()` has set it. Shared by the macOS + iOS conversations
/// views — macOS embeds it in the detail pane (no modal, per the spec), iOS in a
/// pushed screen; the form body is identical.
public struct NewThreadComposeForm: View {
    public init() {}

    @Environment(ConversationsVM.self) private var vm

    /// Manager send failure, falling back to a client-glue error the snapshot
    /// cannot carry — same precedence as `ThreadDetailView.sendError`.
    private func composeError(_ compose: ComposeState) -> String {
        composeSendOrClientError(compose.sendState, clientError: vm.clientErrorMessage)
    }

    public var body: some View {
        if let compose = vm.newThreadCompose {
            VStack(alignment: .leading, spacing: 8) {
                if let picker = compose.recipientPicker {
                    RecipientPicker(
                        state: picker,
                        // Live re-read off the @Observable vm (manager snapshot), so the
                        // in-place `recipient-resolve-status` registry read reflects the
                        // manager's synchronous Idle→Resolving flip (and the probe's later
                        // terminal state) instead of the snapshot captured at `.onAppear`
                        // (which stayed 'idle' in-process).
                        liveResolveState: { vm.newThreadCompose?.recipientPicker?.resolveState ?? .idle },
                        // Typing owes a probe; Enter resolves first, then commits what the
                        // probe confirmed (conversations.md § Errors & edge cases → *The
                        // picker tells the truth*) — the same order android/web/linux/tui drive.
                        onInputChange: { text in
                            vm.setNewThreadRecipientInput(text)
                            Task { await vm.resolveRecipient() }
                        },
                        onAcceptCurrent: {
                            Task {
                                await vm.resolveRecipient()
                                _ = vm.acceptCurrentRecipientChip()
                            }
                        },
                        onAcceptSuggestion: { vm.acceptNewThreadChip($0) }
                    )
                }

                if (compose.recipientPicker?.chips.count ?? 0) >= 2 {
                    automationText(Ids.groupConversationHint, L.conversations.unified.groupConversationHint)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                // Same `error-message` surface the thread detail pane carries:
                // a failed attach read has no manager-snapshot home, and a
                // new-thread send failure (`sendState = .failed`) previously had
                // nowhere to render at all. The two forms are mutually exclusive
                // in the detail pane, so this never duplicates the id.
                if !composeError(compose).isEmpty {
                    ErrorBanner(message: composeError(compose))
                }

                Divider()

                DmComposeBar(
                    compose: compose,
                    capabilities: nil,
                    replyPreviewText: nil,
                    onBodyChange: { vm.setNewThreadBody($0) },
                    onSubjectChange: { vm.setNewThreadSubject($0.isEmpty ? nil : $0) },
                    onToggleTopic: { vm.setNewThreadSubject(compose.subjectDraft == nil ? "" : nil) },
                    onCancelReply: {},
                    onAttachFile: { try vm.attachNewThreadFile(at: $0) },
                    onAttachError: { vm.clientErrorMessage = $0 },
                    onRemoveAttachment: { vm.removeNewThreadAttachment(at: $0) },
                    onSend: { Task { _ = await vm.sendNewThread() } }
                )
            }
        }
    }
}
