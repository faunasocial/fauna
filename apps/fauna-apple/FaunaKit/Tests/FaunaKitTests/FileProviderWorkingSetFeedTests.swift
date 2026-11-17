import FaunaFFISwift
import Foundation
import Testing

#if canImport(FileProvider)
    import FileProvider
#endif

@testable import FaunaKit

#if canImport(FileProvider)

    /// Headless pins for the change feed (`FileProviderWorkingSetFeed`,
    /// `on-demand-files-apple.md` § Apple File Provider binding, *the change
    /// feed*): a refresh that changed something signals the working set as well
    /// as the root; the feed reports the set's changes since the anchor (updates
    /// and deletions) plus the items whose kept bit flipped, once each, then
    /// finishes at the feed's new anchor. What changed is the host's (pinned in
    /// Rust, `provider_face::change_feed` and `kept_flips_across`); whether the
    /// OS then lists it is the live inch.
    @Suite struct FileProviderWorkingSetFeedTests {
        private final class Observer: NSObject, NSFileProviderChangeObserver {
            var events: [String] = []
            func didUpdate(_ updatedItems: [NSFileProviderItemProtocol]) {
                for item in updatedItems {
                    events.append("update \(item.itemIdentifier.rawValue)")
                }
            }
            func didDeleteItems(withIdentifiers deletedItemIdentifiers: [NSFileProviderItemIdentifier]) {
                for id in deletedItemIdentifiers {
                    events.append("delete \(id.rawValue)")
                }
            }
            func finishEnumeratingChanges(upTo anchor: NSFileProviderSyncAnchor, moreComing: Bool) {
                events.append("finish \(String(decoding: anchor.rawValue, as: UTF8.self)) \(moreComing)")
            }
            func finishEnumeratingWithError(_ error: Error) {
                events.append("error")
            }
        }

        private final class Item: NSObject, NSFileProviderItem {
            let itemIdentifier: NSFileProviderItemIdentifier
            let parentItemIdentifier: NSFileProviderItemIdentifier = .rootContainer
            let filename: String
            init(_ ffi: FfiFileProviderItem) {
                itemIdentifier = NSFileProviderItemIdentifier("\(ffi.rel) kept=\(ffi.kept)")
                filename = ffi.name
            }
        }

        private struct TakeFailed: Error {}

        private func ffi(_ rel: String, kept: Bool) -> FfiFileProviderItem {
            FfiFileProviderItem(
                rel: rel, name: rel, sizeBytes: 1, mtime: 0, isDir: false,
                contentVersion: Data(), kept: kept, shareLinkEligible: false)
        }

        private func changes(
            _ updated: [FfiFileProviderItem], deleted: [String] = [], anchor: String = "g2"
        ) -> FfiFileProviderChanges {
            FfiFileProviderChanges(updated: updated, deleted: deleted, anchor: Data(anchor.utf8))
        }

        private let anchor = NSFileProviderSyncAnchor(Data("g1".utf8))

        @Test func aChangedRefreshSignalsTheWorkingSetToo() {
            #expect(
                FileProviderWorkingSetFeed.containersToSignal(changed: true)
                    == [.rootContainer, .workingSet])
            #expect(FileProviderWorkingSetFeed.containersToSignal(changed: false).isEmpty)
        }

        @Test func theWorkingSetAndTheRootReportTheSet() {
            #expect(FileProviderWorkingSetFeed.reportsTheSet(.workingSet))
            #expect(FileProviderWorkingSetFeed.reportsTheSet(.rootContainer))
            #expect(!FileProviderWorkingSetFeed.reportsTheSet(NSFileProviderItemIdentifier("sub")))
        }

        /// The live failure this pins (`p2p-shared-set-build.md` § The live
        /// witness): a row that landed after the OS listed the root is
        /// reported as an update, and the feed finishes at the new anchor.
        @Test func aLandedRowAndAGoneRowAreReportedAndTheFeedFinishesAtTheNewAnchor() async {
            let observer = Observer()
            let landed = changes([ffi("offline.txt", kept: false)], deleted: ["gone.txt"])
            await FileProviderWorkingSetFeed.reportChanges(
                to: observer, from: anchor,
                changesSince: { asked in
                    #expect(asked == Data("g1".utf8))
                    return landed
                },
                takeKeptFlips: { [] }, item: { Item($0) })
            #expect(
                observer.events == [
                    "update offline.txt kept=false", "delete gone.txt", "finish g2 false",
                ])
        }

        @Test func theFlippedItemsAreReportedWithTheirNewWordBeforeTheFinish() async {
            let observer = Observer()
            let flipped = [ffi("doc.txt", kept: false), ffi("sub/a.txt", kept: true)]
            let none = changes([])
            await FileProviderWorkingSetFeed.reportChanges(
                to: observer, from: anchor, changesSince: { _ in none },
                takeKeptFlips: { flipped }, item: { Item($0) })
            #expect(
                observer.events == [
                    "update doc.txt kept=false", "update sub/a.txt kept=true", "finish g2 false",
                ])
        }

        @Test func aFlipTheFeedAlreadyReportsIsReportedOnce() async {
            let observer = Observer()
            let both = changes([ffi("doc.txt", kept: true)])
            let flipped = [ffi("doc.txt", kept: true)]
            await FileProviderWorkingSetFeed.reportChanges(
                to: observer, from: anchor, changesSince: { _ in both },
                takeKeptFlips: { flipped }, item: { Item($0) })
            #expect(observer.events == ["update doc.txt kept=true", "finish g2 false"])
        }

        @Test func nothingMovedReportsNoUpdate() async {
            let observer = Observer()
            let none = changes([], anchor: "g1")
            await FileProviderWorkingSetFeed.reportChanges(
                to: observer, from: anchor, changesSince: { _ in none },
                takeKeptFlips: { [] }, item: { Item($0) })
            #expect(observer.events == ["finish g1 false"])
        }

        /// A feed that fails keeps the anchor, so the next ask covers the span.
        @Test func aFailedFeedAndTakeStillFinishAtTheOldAnchor() async {
            let observer = Observer()
            await FileProviderWorkingSetFeed.reportChanges(
                to: observer, from: anchor, changesSince: { _ in throw TakeFailed() },
                takeKeptFlips: { throw TakeFailed() }, item: { Item($0) })
            #expect(observer.events == ["finish g1 false"])
        }
    }
#endif
