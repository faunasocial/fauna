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
        /// The containers to signal after a refresh: the root, whose re-listing
        /// picks up pulled rows, and the working set, whose change feed reports
        /// the kept flips. Nothing when the refresh changed nothing.
        public static func containersToSignal(changed: Bool) -> [NSFileProviderItemIdentifier] {
            changed ? [.rootContainer, .workingSet] : []
        }

        /// The working set's `enumerateChanges`: report the items whose kept bit
        /// a refresh flipped as updated, projected by `item` so the OS reads
        /// their new policy, then finish at `anchor`. A take that fails reports
        /// nothing — the items keep the policy the OS last read.
        public static func reportChanges(
            to observer: NSFileProviderChangeObserver,
            upTo anchor: NSFileProviderSyncAnchor,
            takeKeptFlips: () async throws -> [FfiFileProviderItem],
            item: (FfiFileProviderItem) -> NSFileProviderItem
        ) async {
            let flipped = (try? await takeKeptFlips()) ?? []
            if !flipped.isEmpty {
                observer.didUpdate(flipped.map(item))
            }
            observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
        }
    }
#endif
