import SwiftUI
import FaunaKit

/// The Bridges page — feed-side bridges the user enables to bring external-network
/// content into their unified feed/follows (Bluesky/ActivityPub + future protocols).
/// **Nostr is NOT here**: it is a deep integration with its own dedicated page (the
/// shared FaunaKit `NostrSettingsView`, reached from the `nostr-settings-link`
/// Settings row), the same treatment as mail (nostr.md § Page structure / bridges.md
/// § Scope, ratified 2026-06-13).
///
/// Rendered entirely from the metadata-driven `BridgeManagerVM` (the one shared
/// bridge VM, also driving macOS `BridgesSettingsView` — priority #2; the former
/// iOS-only `BridgesVM` + hard-coded Bluesky `Section` were retired 2026-06-28).
/// Every bridge — Bluesky included — renders through the same generic list +
/// metadata link form, so Bluesky's `handle` field surfaces as the canonical
/// `bridge-link-field-handle` testid with no per-bridge branching (bridges.md
/// § Element IDs / § Implementation status). This is the iOS half of the
/// Bluesky-ws-rpc "Commit C" fan-out.
struct BridgesView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    /// The ward's durable feed-source asks ride the app-root status projection
    /// (`family-safety.md` § Feed-source approvals); `nil` where none is injected.
    @Environment(FamilyStatusStore.self) private var familyStatus: FamilyStatusStore?
    @State private var vm = BridgeManagerVM()

    var body: some View {
        List {
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
                    Section {
                        BridgeCardContent(bridge: bridge, vm: vm, isDesktop: false, compact: false, index: index)
                    } header: {
                        Text(bridge.name)
                    }
                }
            }

            if let error = vm.errorMessage {
                Section {
                    ErrorBanner(message: error)
                }
            }
        }
        .listStyle(.insetGrouped)
        .pageTitle(L.bridges.title)
        // Keyed on the session's client, not one-shot: More → Bridges is not
        // unmounted by the switch teardown, so the outgoing account's bridges — and
        // its half-typed link-form credentials — would otherwise survive into the
        // incoming account's page (`BridgeManagerVM.reset()` names why that is the
        // sharp one). `account-scoping.md` § The scoping taxonomy, the "reused shell"
        // case.
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
}
