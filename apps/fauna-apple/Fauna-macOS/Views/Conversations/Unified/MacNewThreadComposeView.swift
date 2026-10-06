import SwiftUI
import FaunaKit

/// New-thread compose, in the detail pane (no modal) — `dm-compose-form`. The
/// form body (recipient picker → group hint → `dm-compose-bar`) is the shared
/// `NewThreadComposeForm`; macOS just frames it with a title + a Cancel button
/// since there's no nav bar to host one (iOS pushes it as its own screen).
///
/// The Cancel button is the explicit-discard affordance (`new-conversation-cancel`,
/// → `cancelNewConversation`, clears the draft) per conversations.md § Persistence;
/// switching away by selecting another thread (`select_thread`) preserves the
/// half-written draft in shared Rust, so macOS needs no separate preserve path.
struct MacNewThreadComposeView: View {
    @Environment(ConversationsVM.self) private var vm

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(L.conversations.list.newConversation).font(.headline)
                Spacer()
                Button(L.common.cancel) { vm.cancelNewConversation() }
                    .buttonStyle(.borderless)
                    .accessibilityIdentifier(Ids.newConversationCancel)
                    .automationActivate(Ids.newConversationCancel) {
                        vm.cancelNewConversation()
                    }
            }
            NewThreadComposeForm()
            Spacer()
        }
        .padding(12)
    }
}
