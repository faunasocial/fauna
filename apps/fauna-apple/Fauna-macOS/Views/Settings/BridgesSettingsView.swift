import SwiftUI
import FaunaKit

struct BridgesSettingsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    /// The ward's durable feed-source asks ride the app-root status projection
    /// (`family-safety.md` § Feed-source approvals); `nil` where none is injected.
    @Environment(FamilyStatusStore.self) private var familyStatus: FamilyStatusStore?
    @State private var vm = BridgeManagerVM()

    var body: some View {
        Form {
            if vm.isLoading && vm.bridges.isEmpty {
                Section {
                    ProgressView(L.bridges.loadingBridges)
                }
            } else if vm.bridges.isEmpty {
                Section {
                    Text(L.bridges.noBridges)
                        .foregroundStyle(.secondary)
                }
            } else {
                ForEach(Array(vm.bridges.enumerated()), id: \.offset) { index, bridge in
                    bridgeSection(bridge, index: index)
                }
            }

            if let error = vm.errorMessage {
                Section {
                    ErrorBanner(message: error)
                }
            }
        }
        .pageTitle(L.bridges.title)
        .formStyle(.grouped)
        // The drop below is REDUNDANT on macOS and carried for uniformity, as
        // `SearchVM`'s is: `tearDownSessionForSwitch()` sets `isOnboarded = false`,
        // which unmounts `MainWindowView` wholesale, so this view dies with the window
        // and its view model with it. That unmount IS the guarantee here
        // (`account-scoping.md` § The scoping taxonomy, the in-memory corollary: an app
        // whose drop rides a shell teardown must say where the guarantee comes from) —
        // which is exactly what iOS does not have, and why the seam lives on the view
        // model rather than at either site .
        .task(id: SessionKey(client)) {
            guard let client else {
                vm.reset()
                return
            }
            vm.configure(api: client.api, familyStatus: familyStatus)
            // A guardian's answer is an async event: re-read the ward's own asks on
            // open so an approved source reads "Approved — try again" rather than
            // the last post-auth snapshot's "waiting".
            await familyStatus?.refresh(api: client.api)
            await vm.refresh()
        }
    }

    @ViewBuilder
    private func bridgeSection(_ bridge: BridgeInfo, index: Int) -> some View {
        Section(bridge.name) {
            BridgeCardContent(bridge: bridge, vm: vm, isDesktop: true, compact: true, index: index)
        }
    }
}
