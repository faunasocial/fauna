import SwiftUI

/// The feed's imported-posts selection notice + clear (`feed-imported-notice` /
/// `feed-imported-clear`; `feed.md` § State & data shape → *The imported-posts
/// selection*). Painted above the post list while `FeedVM.importedSelected` is
/// set; one shared view for macOS and iOS (priority #2).
public struct FeedImportedNotice: View {
    let vm: FeedVM

    public init(vm: FeedVM) {
        self.vm = vm
    }

    public var body: some View {
        if let platform = vm.importedPlatformLabel {
            HStack {
                automationText(Ids.feedImportedNotice, L.feed.importedNotice(platform: platform))
                    .font(.callout)
                Spacer()
                Button(L.feed.importedClear) { Task { await vm.clearImported() } }
                    .accessibilityIdentifier(Ids.feedImportedClear)
                    .automationActivate(Ids.feedImportedClear) { Task { await vm.clearImported() } }
            }
        }
    }
}
