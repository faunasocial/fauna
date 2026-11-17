import FaunaFFISwift
import Foundation

#if canImport(FileProvider)
    import FileProvider

    /// How a refresh that changed something reaches the OS, and what the working
    /// set's change feed reports — shared FaunaKit so the appex's refresh tick
    /// and its push-driven `enumerateChanges` signal alike, and the feed is
    /// pinned headlessly (`on-demand-files-apple.md` § Apple File Provider
    /// binding).
    ///
    /// The OS re-reads an item it already lists — its keep policy included
    /// (`FileProviderKeep`) — only when a change feed reports it. A refresh
    /// whose posture read or pull flipped an item's holder-keeps bit counts as
    /// a change (`FfiFileProviderHost.refresh`) and holds the flipped items
    /// until the working set's feed takes them (`takeKeptFlips`).
    public enum FileProviderWorkingSetFeed {
        /// The containers to signal after a refresh: the root and the working
        /// set, whose change feeds report what the refresh pulled or flipped
        /// (the OS never re-lists a container it holds). Nothing when the
        /// refresh changed nothing.
        public static func containersToSignal(changed: Bool) -> [NSFileProviderItemIdentifier] {
            changed ? [.rootContainer, .workingSet] : []
        }

        /// Whether `container`'s change feed reports the set's changes: the
        /// working set (the replicated OS's feed of every item) and the root.
        public static func reportsTheSet(_ container: NSFileProviderItemIdentifier) -> Bool {
            container == .workingSet || container == .rootContainer
        }

        /// A change feed's `enumerateChanges`: the set's changes since
        /// `anchor` (`changesSince`, the shared `serve_changes_since`)
        /// reported as updates and deletions, plus the items whose kept bit a
        /// refresh flipped (`takeKeptFlips`; the working set's alone), each
        /// projected by `item`, then finish at the feed's new anchor. A feed
        /// that fails reports the kept flips alone and finishes at `anchor`,
        /// so the next ask covers the same span.
        public static func reportChanges(
            to observer: NSFileProviderChangeObserver,
            from anchor: NSFileProviderSyncAnchor,
            changesSince: (Data) async throws -> FfiFileProviderChanges,
            takeKeptFlips: () async throws -> [FfiFileProviderItem],
            item: (FfiFileProviderItem) -> NSFileProviderItem
        ) async {
            let changes = try? await changesSince(anchor.rawValue)
            var updated = changes?.updated ?? []
            let flipped = (try? await takeKeptFlips()) ?? []
            let reported = Set(updated.map { $0.rel })
            updated += flipped.filter { !reported.contains($0.rel) }
            if !updated.isEmpty {
                observer.didUpdate(updated.map(item))
            }
            if let deleted = changes?.deleted, !deleted.isEmpty {
                observer.didDeleteItems(
                    withIdentifiers: deleted.map { NSFileProviderItemIdentifier(rawValue: $0) })
            }
            let next = changes.map { NSFileProviderSyncAnchor(Data($0.anchor)) } ?? anchor
            observer.finishEnumeratingChanges(upTo: next, moreComing: false)
        }
    }
#endif
