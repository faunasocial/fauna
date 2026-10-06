import Foundation
import Testing
import WidgetKit
@testable import FaunaExtensionKit
@testable import FaunaKit

// The home-screen widget's two halves (apps/common.md § Home-screen widget),
// pinned headlessly: what the widget renders from a seeded snapshot (the
// FFI-free `FaunaExtensionKit` half the appex links), and what the app's cadence
// publishes (the FaunaKit half). The e2e witness —
// `tests/e2e-unified/tests/test_home_screen_widget.py` — proves the app's own
// conversations-list total reaches the snapshot; these pin both ends of the file.

private func scratchStore() -> UnreadSnapshotStore {
    UnreadSnapshotStore(directory: FileManager.default.temporaryDirectory
        .appendingPathComponent("fauna-widget-\(UUID().uuidString)"))
}

@Suite struct HomeScreenWidgetTests {
    // MARK: Snapshot store — the file the app writes and the widget reads

    @Test func aSavedSnapshotReadsBackExactly() throws {
        let store = scratchStore()
        let snapshot = UnreadSnapshot(count: 7, updatedAt: Date(timeIntervalSince1970: 1_800_000_000))
        try store.save(snapshot)
        #expect(store.load() == snapshot)
    }

    @Test func noFileAndAClearedFileBothReadAsNoSnapshot() throws {
        let store = scratchStore()
        #expect(store.load() == nil)
        try store.save(UnreadSnapshot(count: 3, updatedAt: .now))
        store.clear()
        #expect(store.load() == nil)
    }

    @Test func anUnreadableFileReadsAsNoSnapshotRatherThanThrowing() throws {
        let store = scratchStore()
        try FileManager.default.createDirectory(at: store.directory, withIntermediateDirectories: true)
        try Data("not json".utf8).write(to: store.fileURL)
        #expect(store.load() == nil)
    }

    // MARK: Timeline — what the widget shows for a snapshot

    @Test func theWidgetShowsTheSnapshotsCount() {
        let now = Date(timeIntervalSince1970: 1_800_000_100)
        let entry = UnreadWidgetEntry.from(UnreadSnapshot(count: 12, updatedAt: now), now: now)
        #expect(entry == UnreadWidgetEntry(date: now, count: 12))
    }

    @Test func noSnapshotShowsZeroLikeAndroid() {
        #expect(UnreadWidgetEntry.from(nil, now: .distantPast).count == 0)
    }

    @Test func theTimelineHasOneEntryAndNeverSchedulesItsOwnReload() {
        let timeline = UnreadWidgetTimeline.timeline(for: UnreadSnapshot(count: 4, updatedAt: .now), now: .now)
        #expect(timeline.entries.map(\.count) == [4])
        #expect(timeline.policy == .never)
    }

    // MARK: Publisher — what the app writes

    @MainActor
    @Test func publishingWritesTheCountAndReloadsTheWidget() {
        let store = scratchStore()
        var reloads = 0
        let fixed = Date(timeIntervalSince1970: 1_800_000_200)
        let publisher = WidgetUnreadPublisher(store: { store }, reload: { reloads += 1 }, now: { fixed })
        publisher.publish(unread: 5)
        #expect(store.load() == UnreadSnapshot(count: 5, updatedAt: fixed))
        #expect(reloads == 1)
    }

    @MainActor
    @Test func anUnchangedCountIsNotRewrittenOrReloaded() {
        let store = scratchStore()
        var reloads = 0
        let publisher = WidgetUnreadPublisher(store: { store }, reload: { reloads += 1 }, now: Date.init)
        publisher.publish(unread: 3)
        publisher.publish(unread: 3)
        #expect(reloads == 1, "every manager tick (a compose keystroke too) must not redraw the widget")
        publisher.publish(unread: 4)
        #expect(reloads == 2)
        #expect(store.load()?.count == 4)
    }

    @MainActor
    @Test func clearingForgetsTheOutgoingAccountsCount() {
        let store = scratchStore()
        var reloads = 0
        let publisher = WidgetUnreadPublisher(store: { store }, reload: { reloads += 1 }, now: Date.init)
        publisher.publish(unread: 2)
        publisher.clear()
        #expect(store.load() == nil)
        #expect(reloads == 2)
        // The incoming account's first tick writes even if it equals the old count.
        publisher.publish(unread: 2)
        #expect(store.load()?.count == 2)
    }

    @MainActor
    @Test func withNoResolvableStoreNothingIsWrittenOrReloaded() {
        var reloads = 0
        let publisher = WidgetUnreadPublisher(store: { nil }, reload: { reloads += 1 }, now: Date.init)
        publisher.publish(unread: 1)
        #expect(reloads == 0)
    }

    @Test func theTotalIsTheListsPerThreadUnreadSummed() {
        #expect(WidgetUnreadPublisher.total(of: []) == 0)
    }
}
