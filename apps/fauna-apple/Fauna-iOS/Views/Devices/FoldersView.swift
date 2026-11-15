import SwiftUI
import FaunaKit

/// iOS **Settings → Folders** sub-page shell — the folder control plane (list +
/// wizard + per-set config + conflicts + the Apple photo-backup controls), the
/// shared FaunaKit `FoldersContent` driven by `DevicesMachine`. iOS has **no**
/// local-folder binding (no user-bindable folder tree), so the per-set binding is
/// the default empty view. Reached at `{"view":"settings","id":"folders"}`; no
/// inner `NavigationStack` (pushed into the More tab's stack, `MoreView.settingsPath`).
struct FoldersView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @Environment(AppState.self) private var appState: AppState?
    /// The session's one VM (app-scene-level, shared with Settings → Devices).
    @Environment(DevicesMachineVM.self) private var vm
    let reloadToken: Int
    #if !FAUNA_EXCISE_P2P_SHARE && os(iOS)
        /// A group set's review list: read off the File Provider extension's
        /// state DB, the restore crossing to the extension (`GroupSetReviewRows`).
        @State private var review = GroupSetReviewRows()
    #endif

    var body: some View {
        let reconcile = appState?.fpReconcile
        content(onDemandChanged: { Task { await reconcile?() } })
            .pageTitle(L.folders.title)
            .task(id: reloadToken) {
                if let client {
                    await vm.configure(api: client.api)
                }
            }
    }

    /// The shared page; `onDemandChanged` re-converges the Files-app FP domains
    /// on a `folder-on-demand-toggle` flip (FaunaApp wires the hook on launch).
    @ViewBuilder
    private func content(onDemandChanged: @escaping () -> Void) -> some View {
        #if FAUNA_EXCISE_P2P_SHARE || !os(iOS)
            FoldersContent(vm: vm, onDemandChanged: onDemandChanged)
        #else
            let actor = client?.ownActorIdHex
            FoldersContent(
                vm: vm,
                setReviewRows: { folderId, setLabel in
                    review.setReviewRows(folderId: folderId, setLabel: setLabel)
                },
                useRetainedVersion: { folderId, conflictId in
                    guard let actor else { return }
                    try await review.useRetainedVersion(
                        actorIdHex: actor, folderId: folderId, conflictId: conflictId)
                },
                onDemandChanged: onDemandChanged
            )
            .task(id: actor) {
                if let actor { await review.poll(actorIdHex: actor) }
            }
        #endif
    }
}
