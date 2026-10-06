import SwiftUI
import FaunaKit

/// Unified conversations page (macOS). Two-pane `NavigationSplitView`:
/// the list pane (`new-conversation-button`, `conversation-sort`,
/// `conversation-search-box`, indexed `conversation-item` rows, page-level
/// `error-message`) and a detail pane that swaps between an empty hint, the
/// shared `ThreadDetailView` (an open thread), and `MacNewThreadComposeView`
/// (the in-pane new-thread compose — no modal). The rename + add-participant
/// `.sheet` overlays live inside `ThreadDetailView`.
///
/// Renders entirely off `ConversationsVM` (a thin `@Observable` observer over
/// `fauna-conversations`'s `ConversationsManager`); no client state machine,
/// no rail branches — capability gating lives in the leaf views and keys off
/// `capabilities.*`. There is no Groups page: group threads live here with
/// `flavor: .mlsGroup`. All the leaf views (`ConversationListRow`,
/// `ThreadHeader`, `DmComposeBar`, `DmMessageBubble`, `SubjectDivider`,
/// `RecipientPicker`, `ThreadDetailView`, `NewThreadComposeForm`, `RenameSheet`,
/// `AddParticipantSheet`) are shared with iOS in `FaunaKit/Views/`.
struct MacConversationsView: View {
    @Environment(ConversationsVM.self) private var vm

    var body: some View {
        NavigationSplitView {
            listPane
        } detail: {
            detailPane
                .accessibilityElement(children: .contain)
        }
        .pageTitle(L.conversations.list.title)
    }

    // ── List pane ──────────────────────────────────────────────────────────

    private var listPane: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Button {
                    vm.startNewConversation()
                } label: { Image(systemName: "square.and.pencil") }
                    .help(L.conversations.list.newConversation)
                    .accessibilityIdentifier(Ids.newConversationButton)
                    .automationActivate(Ids.newConversationButton) { vm.startNewConversation() }

                Spacer()

                Button {
                    vm.cycleSort()
                } label: { Image(systemName: "arrow.up.arrow.down") }
                    .help(L.conversations.list.sort)
                    .accessibilityIdentifier(Ids.conversationSort)
                    // Cycle-button pattern (AutomationRegistry.swift): one entry
                    // actuates AND reads the current order (mirrors iOS).
                    .automationActivate(Ids.conversationSort, value: { ConversationsUI.sortLabel(vm.snapshot.sort) }) {
                        vm.cycleSort()
                    }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)

            TextField(L.conversations.list.searchPlaceholder, text: vm.searchBinding)
                .textFieldStyle(.roundedBorder)
                .padding(.horizontal, 12)
                .padding(.bottom, 6)
                .accessibilityIdentifier(Ids.conversationSearchBox)
                .automationField(Ids.conversationSearchBox, text: vm.searchBinding)

            List(selection: selectionBinding) {
                ForEach(Array(vm.rows.enumerated()), id: \.element.id) { index, row in
                    ConversationListRow(model: row)
                        .tag(row.id)
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier(Ids.conversationItem)
                        // Indexed row actuation — fires the same select path the
                        // `List(selection:)` binding does; reads the row label.
                        .automationActivate(Ids.conversationItem, value: { row.label }) {
                            selectRow(row.id)
                        }
                        // Scoped container (rule 5): `dm-unread-indicator` is 0-or-1
                        // per row and read by `scope="conversation-item[N]"`. Without
                        // a real subtree path the flat heuristic answers a scoped
                        // count with the GLOBAL one, so every row reads unread while
                        // any is (feed-item / device-card precedent).
                        .automationScope(Ids.conversationItem, index: index)
                }
            }

            // Always-present page-level error element (Rule 2).
            ErrorBanner(message: vm.pageError)
                .padding(.horizontal, 12)
                .padding(.bottom, 6)
        }
    }

    private var selectionBinding: Binding<ThreadId?> {
        Binding(
            get: { vm.selectedThreadId },
            set: { newValue in
                if let id = newValue { selectRow(id) }
            }
        )
    }

    /// Select a thread — the same path the `List(selection:)` binding and the
    /// `conversation-item` automation activation both run. Selecting is what
    /// reads it: the shared manager decides that (conversations.md § State &
    /// data shape → When a thread is read), so there is no `markRead` here to
    /// keep in step.
    private func selectRow(_ id: ThreadId) {
        vm.selectThread(id)
    }

    // ── Detail pane ────────────────────────────────────────────────────────

    @ViewBuilder private var detailPane: some View {
        if vm.newThreadCompose != nil {
            MacNewThreadComposeView()
        } else if let detail = vm.selectedDetail {
            ThreadDetailView(detail: detail)
        } else {
            ContentUnavailableView(
                L.conversations.list.title,
                systemImage: "bubble.left.and.bubble.right",
                description: Text(L.conversations.list.selectConversation)
            )
        }
    }
}
