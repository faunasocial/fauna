import Testing
@testable import FaunaKit

// Guards for the shared Media explorer (`MediaExplorerContent` + `MediaMachineVM`).
//
// The explorer's browse/sort/filter logic lives in shared Rust
// (`fauna-media-machine`, unit-tested there) and the rendering/IDs are proven by
// the cross-app e2e (`tests/e2e-unified/tests/test_media.py`). These two
// in-process guards cover the thin Swift seam that neither reaches:
//   1. the all-media filter sentinel must equal the cross-app wire value, and
//   2. the VM's gestures must be safe to call before `configure` (no machine yet).

/// The `media-folder-filter` all-media sentinel must stay byte-identical to the
/// value linux/web/windows + the e2e driver use (`MEDIA_FILTER_ALL` in
/// `tests/e2e-unified/actions/media.py`; `FILTER_ALL_VALUE` in shared Rust). A
/// drift here silently breaks the all-media default view + the cross-app
/// `select("media-folder-filter", "__all__")` contract (priority #1).
@Test func allMediaFilterSentinelMatchesCrossClientValue() {
    #expect(MediaExplorerContent.filterAllValue == "__all__")
}

/// A freshly-constructed `MediaMachineVM` (before `configure` builds the machine)
/// has no snapshot/error and its gestures are safe no-ops — a navigation that
/// renders the explorer before the WS-RPC connect lands must not crash on an
/// early toggle/sort/filter/upload (`machine == nil` guards everything).
@Test @MainActor func vmGesturesAreSafeBeforeConfigure() async {
    let vm = MediaMachineVM()
    #expect(vm.snapshot == nil)
    #expect(vm.errorMessage == nil)

    // View-state setters: no machine → no-op, no crash.
    vm.setSort("size")
    vm.setViewGrid(true)
    vm.setFilter("photos")

    // Upload gestures bail on the empty path and on the missing machine/api/key
    // BEFORE touching the filesystem, so no glue error is surfaced.
    await vm.uploadFromPath("")
    await vm.uploadFromPath("/nonexistent/path/to/file")
    await vm.deleteItem(folder: "photos", path: "x.jpg")

    // The per-item thumbnail fetch is a query: with no machine/api it degrades to
    // `nil` (the card keeps its placeholder) without crashing or touching the page
    // banner (`media.md` § Thumbnails — one unreadable thumbnail never blanks the page).
    let thumb = await vm.fetchThumbnail(hash: "deadbeef")
    #expect(thumb == nil)

    #expect(vm.snapshot == nil)
    #expect(vm.errorMessage == nil)
}
