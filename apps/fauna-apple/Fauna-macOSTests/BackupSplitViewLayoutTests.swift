import Testing
import SwiftUI
import AppKit
import FaunaKit
@testable import FaunaMacOSLib

// Headless layout regression test for the Backups page's three-pane `HSplitView`
// (`BackupSplitView`) — the densest layout in the macOS app.
//
// WHY THIS EXISTS. `HSplitView` does NOT compress below the sum of its panes'
// minimum widths. Given less room than that it silently OVERHANGS its container
// instead, and never reports the larger minimum back up to SwiftUI (so
// `.windowResizability(.contentSize)` does not widen the window either). When the
// default window was 1100x700 that parked `snapshot-file-download-button` past the
// right edge, where no user could click it and the automation registry reported it
// `HIDDEN(geo-parked)` — the defect behind THREE failed pane-level fix attempts
// (a per-row `Text` width constraint, a `maxWidth` cap on `snapshotContent`, and a
// `layoutPriority` pass), each of which only redistributed width WITHIN a total
// that stayed pinned at the floor. It was finally fixed by giving
// the window enough room (`.defaultSize(width: 1280, height: 800)`).
//
// That fix was guarded only by a comment on `.defaultSize`. This test enforces it:
// it measures the real `NSSplitView` pane frames through an `NSHostingView` (no
// window, no app, no e2e — ~0.2 s) and fails if the packing floor ever climbs past
// the width the default window actually affords this page. A pane-minimum bump is
// the regression this catches, and it is invisible to every other check we run.

/// Width the Backups `HSplitView` actually gets: the `.defaultSize` window width
/// (`FaunaMacApp.swift`) minus the `NavigationSplitView` sidebar, which the e2e
/// build pins visible (`ContentView.swift`: `columnVisibility = .all`). The
/// sidebar measured 324 pt in the live trace that produced the original bug
/// (1100 pt window, 46 pt overhang ⇒ 776 pt available ⇒ 324 pt sidebar).
private let defaultWindowWidth: CGFloat = 1280
private let navigationSidebarWidth: CGFloat = 324
private let availableWidthBudget = defaultWindowWidth - navigationSidebarWidth  // 956

@MainActor
private func populatedVM() throws -> BackupsMachineVM {
    // Populated on purpose: an empty folder list and a closed detail both
    // under-measure the panes, which is exactly why this overflow stayed
    // invisible to everything short of a live e2e run.
    //
    // Built as a `BackupsSnapshot` handed to the VM's probe seam rather than by
    // assigning per-field VM state: since the machine adoption the page reads
    // ONE record from shared Rust, so the probe populates that record — which
    // also means this fixture cannot drift from the shape the page really
    // renders (a field added to the record is a compile error here).
    let vm = BackupsMachineVM()
    vm.probeSnapshot = BackupsSnapshot(
        folders: [
            BackupFolderRow(name: "documents",
                             snapshotCount: 12, lastSnapshotAt: 1_754_000_000),
            BackupFolderRow(name: "photos",
                             snapshotCount: 3, lastSnapshotAt: 1_753_900_000),
        ],
        selectedFolder: "documents",
        snapshots: [
            SnapshotRow(id: 1, createdAt: 1_754_000_000, fileCount: 4,
                        totalBytes: 1_048_576, deviceId: "aabbccddeeff0011",
                        tags: ["daily"], state: .active, integrity: .unknown),
        ],
        lastBackedUp: 1_754_000_000,
        inProgressOp: nil,
        checkResult: nil,
        prunePreview: nil,
        detail: SnapshotDetail(snapshotId: 1, files: [
            SnapshotFileRow(path: "documents/quarterly-report-final.pdf",
                            sizeBytes: 524_288, fileType: "regular",
                            manifestHash: String(repeating: "ab", count: 32)),
        ]),
        error: nil)
    return vm
}

/// Lays `BackupSplitView` out at `width` and returns its `HSplitView` pane frames,
/// left to right (dividers excluded).
@MainActor
private func paneFrames(atWidth width: CGFloat) throws -> [CGRect] {
    let host = NSHostingView(rootView: BackupSplitView(vmForTest: try populatedVM()))
    host.frame = NSRect(x: 0, y: 0, width: width, height: 278)
    host.layoutSubtreeIfNeeded()

    var splits: [NSSplitView] = []
    func collect(_ view: NSView) {
        if let split = view as? NSSplitView { splits.append(split) }
        view.subviews.forEach(collect)
    }
    collect(host)

    guard let split = splits.first else { return [] }
    return split.subviews
        .filter { !String(describing: type(of: $0)).contains("Divider") }
        .map(\.frame)
        .sorted { $0.minX < $1.minX }
}

/// Narrowest width at which no pane overhangs — i.e. the sum of the panes'
/// minimum widths plus their dividers.
@MainActor
private func measuredPackingFloor() throws -> CGFloat {
    var low: CGFloat = 400, high: CGFloat = 1400
    while high - low > 1 {
        let mid = ((low + high) / 2).rounded()
        let overhangs = try paneFrames(atWidth: mid).contains { $0.maxX > mid + 0.5 }
        if overhangs { low = mid } else { high = mid }
    }
    return high
}

@Test @MainActor
func backupsSplitViewFitsTheDefaultWindow() throws {
    let floor = try measuredPackingFloor()

    #expect(floor <= availableWidthBudget, """
        The Backups HSplitView needs \(Int(floor)) pt but the default window \
        affords only \(Int(availableWidthBudget)) pt \
        (\(Int(defaultWindowWidth)) pt window − \(Int(navigationSidebarWidth)) pt sidebar).
        HSplitView does not compress below its panes' minimum widths — it overhangs \
        the window, parking the trailing pane's controls (e.g. \
        snapshot-file-download-button) where no user can click them and the \
        automation registry reports HIDDEN(geo-parked).
        Fix by lowering a pane minWidth in BackupSplitView.swift (or the nested \
        minWidth on MacSnapshotTimelineView's List) — NOT by adjusting a maxWidth \
        cap, which is never binding in this regime. Raising .defaultSize in \
        FaunaMacApp.swift also works, but only moves the floor.
        """)
}

@Test @MainActor
func backupsSplitViewPanesStayInsideTheirContainer() throws {
    let width = availableWidthBudget
    let panes = try paneFrames(atWidth: width)

    #expect(panes.count == 3, "expected 3 HSplitView panes, measured \(panes.count)")
    for (index, frame) in panes.enumerated() {
        #expect(frame.maxX <= width + 0.5, """
            Backups pane \(index) spans \(Int(frame.minX))…\(Int(frame.maxX)) pt, \
            overhanging the \(Int(width)) pt container by \(Int(frame.maxX - width)) pt. \
            Panes: \(panes.map { "\(Int($0.minX))+\(Int($0.width))" }.joined(separator: " | ")).
            """)
    }
}
