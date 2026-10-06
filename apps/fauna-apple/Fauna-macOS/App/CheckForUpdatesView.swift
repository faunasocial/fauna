import SwiftUI
import Sparkle

/// A SwiftUI view that wraps Sparkle's "Check for Updates" action as a menu item.
/// Disables itself while an update check is already in progress.
struct CheckForUpdatesView: View {
    @ObservedObject private var checkForUpdatesViewModel: CheckForUpdatesViewModel

    init(updater: SPUUpdater) {
        self.checkForUpdatesViewModel = CheckForUpdatesViewModel(updater: updater)
    }

    var body: some View {
        Button("Check for Updates...", action: checkForUpdatesViewModel.checkForUpdates)
            .disabled(!checkForUpdatesViewModel.canCheckForUpdates)
    }
}

/// Observes the Sparkle updater to determine when "Check for Updates" should be enabled.
private final class CheckForUpdatesViewModel: ObservableObject {
    @Published var canCheckForUpdates = false

    private let updater: SPUUpdater

    init(updater: SPUUpdater) {
        self.updater = updater
        updater.publisher(for: \.canCheckForUpdates)
            .assign(to: &$canCheckForUpdates)
    }

    func checkForUpdates() {
        updater.checkForUpdates()
    }
}
