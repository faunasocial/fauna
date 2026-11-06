import FaunaFFISwift
import Foundation
import Testing

#if canImport(FileProvider)
    import FileProvider
#endif

@testable import FaunaKit

#if canImport(FileProvider)

    /// Headless pins for the working set's change feed
    /// (`FileProviderWorkingSetFeed`, `on-demand-files-apple.md` § Apple File
    /// Provider binding): a refresh that changed something signals the working
    /// set as well as the root, and the feed reports exactly the items whose kept
    /// bit flipped, as they stand now, before it finishes. Which items flipped is
    /// the host's (pinned in Rust, `kept_flips_across`); whether the OS then
    /// honours the new keep policy is the live inch.
    @Suite struct FileProviderWorkingSetFeedTests {
        private final class Observer: NSObject, NSFileProviderChangeObserver {
            var events: [String] = []
            func didUpdate(_ updatedItems: [NSFileProviderItemProtocol]) {
                for item in updatedItems {
                    events.append("update \(item.itemIdentifier.rawValue)")
                }
            }
            func didDeleteItems(withIdentifiers deletedItemIdentifiers: [NSFileProviderItemIdentifier]) {
                events.append("delete")
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

        private let anchor = NSFileProviderSyncAnchor(Data("v1".utf8))

        @Test func aChangedRefreshSignalsTheWorkingSetToo() {
            #expect(
                FileProviderWorkingSetFeed.containersToSignal(changed: true)
                    == [.rootContainer, .workingSet])
            #expect(FileProviderWorkingSetFeed.containersToSignal(changed: false).isEmpty)
        }

        @Test func theFlippedItemsAreReportedWithTheirNewWordBeforeTheFinish() async {
            let observer = Observer()
            let flipped = [ffi("doc.txt", kept: false), ffi("sub/a.txt", kept: true)]
            await FileProviderWorkingSetFeed.reportChanges(
                to: observer, upTo: anchor, takeKeptFlips: { flipped }, item: { Item($0) })
            #expect(
                observer.events == [
                    "update doc.txt kept=false", "update sub/a.txt kept=true", "finish v1 false",
                ])
        }

        @Test func noFlipReportsNoUpdate() async {
            let observer = Observer()
            await FileProviderWorkingSetFeed.reportChanges(
                to: observer, upTo: anchor, takeKeptFlips: { [] }, item: { Item($0) })
            #expect(observer.events == ["finish v1 false"])
        }

        @Test func aFailedTakeStillFinishes() async {
            let observer = Observer()
            await FileProviderWorkingSetFeed.reportChanges(
                to: observer, upTo: anchor, takeKeptFlips: { throw TakeFailed() }, item: { Item($0) })
            #expect(observer.events == ["finish v1 false"])
        }
    }
#endif
