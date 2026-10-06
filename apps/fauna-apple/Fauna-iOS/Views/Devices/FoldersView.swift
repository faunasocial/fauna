import SwiftUI
import FaunaKit

/// iOS **Settings → Folders** sub-page shell — the folder control plane (list +
/// wizard + per-set config + conflicts + the Apple photo-backup controls), the
/// shared FaunaKit `FoldersContent` driven by `DevicesMachine`. iOS has **no**
/// local-folder binding (no user-bindable folder tree), so the per-set binding is
/// the default empty view. Reached at `{"view":"settings","id":"folders"}`; no
/// inner `NavigationStack` (pushed into the `SettingsView` stack).
struct FoldersView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @Environment(AppState.self) private var appState: AppState?
    /// The session's one VM (app-scene-level, shared with Settings → Devices).
    @Environment(DevicesMachineVM.self) private var vm
    let reloadToken: Int

    var body: some View {
        let reconcile = appState?.fpReconcile
        FoldersContent(
            vm: vm,
            // A `folder-on-demand-toggle` flip re-converges the Files-app FP
            // domains immediately (FaunaApp wires the hook on launch).
            onDemandChanged: { Task { await reconcile?() } }
        )
        .pageTitle(L.folders.title)
            .task(id: reloadToken) {
                if let client {
                    await vm.configure(api: client.api)
                }
            }
    }
}
