import SwiftUI
import FaunaKit

/// macOS **Media** page shell. A thin wrapper around the shared FaunaKit
/// `MediaExplorerContent` (the cross-set, Windows-Explorer-style media browser)
/// driven by the shared-Rust `MediaMachine` — the *same* renderer iOS uses
/// (`MediaView`), so both Apple apps share one page.
///
/// **2026-06-28 sync/folder UI unification (§ Apple photo-backup reframe):**
/// replaces the former per-set three-pane file browser (`MacFolderListView` /
/// `MacFileListView` / `MacFileDetailView`). Folders are the substrate; Media is
/// the media-optimized *view* of them — the unified all-media explorer across
/// every readable folder (`docs/goal/ui/media.md`). The photo-backup *controls*
/// moved to Settings → Folders (`FoldersContent`); this page is the explorer
/// alone (the content plane, `media.md` rule 4).
///
/// The sidebar switch is wrapped in `Group { … }.id(selectedSidebar…)`
/// (`ContentView`), so navigating here recreates this view and `.task` re-loads
/// the snapshot — the "reload on becoming visible" contract `test_media.py` relies
/// on (mirrors how `FeedSplitView` / `EventSplitView` refresh on appear).
struct MediaSplitView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = MediaMachineVM()

    var body: some View {
        MediaExplorerContent(vm: vm, syncStates: client?.syncStates)
            .pageTitle(L.media.title)
            .task {
                if let client {
                    await vm.configure(
                        api: client.api, deviceId: client.deviceId,
                        predecessors: client.resolvedMediaPredecessors())
                }
            }
    }
}
