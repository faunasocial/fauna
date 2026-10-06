import SwiftUI

/// Rich compose dialog for feed posts (ui.yaml `feed-compose-dialog`). Shared
/// macOS+iOS — presented as a `.sheet` by the host view (`compose-dialog-button`
/// on both `FeedListView` (iOS, also toolbar `compose-button`) and
/// `MacFeedDetailView` (macOS)); reads/writes the same manager-backed compose
/// fields (`vm.composeText`/`vm.composeTags`) the inline composer uses, so text
/// typed in either surface stays in sync.
public struct FeedComposeDialog: View {
    let vm: FeedVM
    let onClose: () -> Void

    public init(vm: FeedVM, onClose: @escaping () -> Void) {
        self.vm = vm
        self.onClose = onClose
    }

    public var body: some View {
        NavigationStack {
            Form {
                Section(L.common.post) {
                    TextField(L.feed.post.whatsOnYourMind, text: Binding(
                        get: { vm.composeText },
                        set: { vm.setComposeText($0) }
                    ), axis: .vertical)
                    .lineLimit(5...20)
                    TextField(L.feed.post.tagsPlaceholder, text: Binding(
                        get: { vm.composeTags },
                        set: { vm.setComposeTags($0) }
                    ))
                    if let composeError = vm.composeError {
                        Text(composeError)
                            .font(.caption)
                            .foregroundStyle(.red)
                            .accessibilityIdentifier(Ids.composeError)
                            // Register the error read with the in-process driver —
                            // a bare `.accessibilityIdentifier` never enters the
                            // AutomationRegistry, so a submit failure here would
                            // read as absent (e2e points 2/6/11).
                            .automationValue(Ids.composeError, text: { vm.composeError })
                    }
                }
            }
            .accessibilityIdentifier(Ids.feedComposeDialog)
            .automationValue(Ids.feedComposeDialog, text: { "" })
            .navigationTitle(L.feed.post.compose)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L.common.cancel, action: onClose)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(L.common.post) {
                        Task {
                            await vm.submitPost()
                            onClose()
                        }
                    }
                    .disabled(
                        vm.composeText.trimmingCharacters(in: .whitespaces).isEmpty
                        || !vm.composeReady
                    )
                }
            }
        }
        #if os(macOS)
        // macOS `.sheet` doesn't auto-size a `Form` the way iOS's full-screen
        // sheet does — an explicit frame, mirroring `MacFeedDetailView`'s
        // `PostDetailSheet`/`ReplySheet` sibling dialogs.
        .frame(width: 480, height: 420)
        #endif
    }
}
