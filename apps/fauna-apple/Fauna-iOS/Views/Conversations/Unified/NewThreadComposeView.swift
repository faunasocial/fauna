import SwiftUI
import FaunaKit

/// New-thread compose screen (iOS). Its own pushed `NavigationStack` screen
/// (the spec says no modal — desktop renders it in the detail pane, mobile
/// pushes it; the list's `new-conversation-button` pushes it). The form body
/// (the `recipient-picker`, `group-conversation-hint`, subject toggle, body,
/// `markdown-toolbar`, attachments, send) is the shared
/// `FaunaKit.NewThreadComposeForm`; this wrapper just opens/closes the
/// `newThreadCompose` state on the shared `ConversationsVM` and gives the screen
/// its nav-bar title.
///
/// Two exits, per conversations.md § Persistence (mirrors android
/// `NewThreadComposeScreen`): a **plain back** (nav-bar back / swipe →
/// `onDisappear`) DEACTIVATES the composer but PRESERVES the half-written draft
/// (re-opening `+` → `startNewConversation` restores it); the explicit
/// **`new-conversation-cancel`** toolbar action DISCARDS it (`cancelNewConversation`)
/// — the one path, alongside a successful send, that drops the draft.
struct NewThreadComposeView: View {
    @Environment(ConversationsVM.self) private var vm

    /// Pops THIS screen off `ConversationsListView`'s own `path` array,
    /// no-animation (see that call site) — the counterpart of the no-animation
    /// push `presentComposerIfNeeded` performs. Passed in rather than using the
    /// environment `dismiss()`, whose pop runs as an ANIMATED transition:
    /// re-tapping "+" while that ~0.3-0.5s animation was still settling raced
    /// the transition's own completion, which then tore down the freshly
    /// re-pushed screen instead of the one it was popping — a real double-tap
    /// hazard, not just an e2e artifact.
    var onCancel: () -> Void

    var body: some View {
        ScrollView {
            NewThreadComposeForm()
                .padding(16)
        }
        .navigationTitle(L.conversations.list.newConversation)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(L.common.cancel) { discardDraft() }
                    .accessibilityIdentifier(Ids.newConversationCancel)
                    .automationActivate(Ids.newConversationCancel) { discardDraft() }
            }
        }
        .onAppear { vm.startNewConversation() }
        // Plain back/dismiss PRESERVES the draft (deactivate, not cancel); the
        // explicit Cancel above is the only in-view discard path.
        .onDisappear { vm.deactivateNewConversation() }
    }

    /// Explicit discard: clear the new-thread draft and pop the screen. Shared by
    /// the Cancel `Button` and its `.automationActivate` so the two never diverge.
    private func discardDraft() {
        vm.cancelNewConversation()
        onCancel()
    }
}
