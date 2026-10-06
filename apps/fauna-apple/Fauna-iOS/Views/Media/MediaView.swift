import SwiftUI
import FaunaKit

/// iOS **Media** page shell — a thin wrapper around the shared FaunaKit
/// `MediaExplorerContent` (the cross-set, Windows-Explorer-style media browser)
/// driven by the shared-Rust `MediaMachine`, the *same* renderer macOS uses
/// (`MediaSplitView`), so both Apple apps share one page.
///
/// **2026-06-28 sync/folder UI unification (§ Apple photo-backup reframe):**
/// replaces the interim per-set `FolderListView`. Folders are the substrate;
/// Media is the unified all-media explorer over every readable set
/// (`docs/goal/ui/media.md`). The photo-backup controls moved to Settings → File
/// sets; this page is the explorer alone (the content plane, `media.md` rule 4).
///
/// Reached as a top-level "More" destination (`moreDestination("media")`); keying
/// `.task` on `reloadToken` (`appState.navGeneration`, bumped on every nav patch)
/// re-`configure()`s (builds the machine once, then `refresh()`es) when the page
/// becomes visible.
///
/// **This page survives an account switch** (`account-scoping.md` § The scoping
/// taxonomy, the in-memory corollary — the "reused shell" case): `MainTabView` stays
/// mounted and `tearDownSessionForSwitch` never clears `moreSelectedView`, so this
/// `@State` view model outlives the identity change holding the outgoing account's
/// machine — its library and its decrypted thumbnails. The key therefore carries the
/// session's client too: the nil-client phase drops it (`MediaMachineVM.reset()`), and
/// the incoming client re-`configure`s, which rebuilds because its `APIClient` differs.
struct MediaView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = MediaMachineVM()
    let reloadToken: Int

    var body: some View {
        MediaExplorerContent(vm: vm, syncStates: client?.syncStates)
            .pageTitle(L.media.title)
            .task(id: SessionKey(client, reloadToken: reloadToken)) {
                if let client {
                    await vm.configure(
                        api: client.api, deviceId: client.deviceId,
                        predecessors: client.resolvedMediaPredecessors())
                } else {
                    vm.reset()
                }
            }
    }
}
