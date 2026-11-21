import FaunaKit
import FileProvider

/// Enumerates a container's children by folding the per-set engine's tracked rows
/// (`host.enumerate(parentRel)` → the shared `serve_enumerate`, keyed by the File
/// Provider domain identifier = set name). The root container maps to the folder
/// root (`parentRel == ""`); reserved system containers (working set, trash) have no
/// engine rows and enumerate empty.
final class FileProviderEnumerator: NSObject, NSFileProviderEnumerator {
    private let container: NSFileProviderItemIdentifier
    private let host: FfiFileProviderHost
    private let domain: NSFileProviderDomain

    init(container: NSFileProviderItemIdentifier, host: FfiFileProviderHost, domain: NSFileProviderDomain) {
        self.container = container
        self.host = host
        self.domain = domain
        super.init()
    }

    func invalidate() {}

    func enumerateItems(
        for observer: NSFileProviderEnumerationObserver,
        startingAt _: NSFileProviderPage
    ) {
        // Working set / trash are OS-reserved containers, not engine paths.
        guard container != .workingSet, container != .trashContainer else {
            observer.didEnumerate([])
            observer.finishEnumerating(upTo: nil)
            return
        }
        let parentRel = FileProviderExtension.rel(for: container)
        let host = self.host
        Task {
            do {
                let items = try await host.enumerate(parentRel: parentRel)
                let readOnly = await FileProviderItemCapabilities.isReadOnly(host)
                observer.didEnumerate(items.map { FileProviderItem(ffi: $0, readOnly: readOnly) })
                observer.finishEnumerating(upTo: nil)
            } catch {
                observer.finishEnumeratingWithError(error)
            }
        }
    }

    func enumerateChanges(
        for observer: NSFileProviderChangeObserver,
        from anchor: NSFileProviderSyncAnchor
    ) {
        // The change feed (`on-demand-files-apple.md` § Apple File Provider
        // binding, *the change feed*): a replicated File Provider learns of a
        // change to a container it already lists ONLY from here — a signal
        // makes the OS ask what moved since the anchor it holds and never
        // re-lists the container. So the working set and the root report the
        // set's changes since `anchor` (`host.changesSince`, the shared
        // `serve_changes_since`): what a pull, a peer's landed body or a
        // posture flip added or changed, and what went. Each item names its
        // own parent, so one answer covers the whole set. Other containers
        // report nothing.
        //
        // This is also the app-side wiring's landing point (file-sync.md §
        // Remote-change nudge, the `fauna.sync.changed` push arm):
        // `signalEnumerator` is the ONLY externally-reachable hook into this
        // appex process, so the MAIN app's push observer lands here. Refresh
        // first, so the feed reports what the pull folded; a refresh that
        // changed something re-signals, and the next ask reports nothing new.
        //
        // The working set also reports the items whose kept bit a refresh
        // flipped (`FileProviderWorkingSetFeed`), so a folder flipped between
        // full and metadata-only reaches each item's keep policy.
        let host = self.host
        let domain = self.domain
        let container = self.container
        Task {
            let changed = (try? await host.refresh()) == true
            for signalled in FileProviderWorkingSetFeed.containersToSignal(changed: changed) {
                try? await NSFileProviderManager(for: domain)?.signalEnumerator(for: signalled)
            }
            guard FileProviderWorkingSetFeed.reportsTheSet(container) else {
                observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
                return
            }
            let readOnly = await FileProviderItemCapabilities.isReadOnly(host)
            let takesFlips = container == .workingSet
            await FileProviderWorkingSetFeed.reportChanges(
                to: observer, from: anchor,
                changesSince: { try await host.changesSince(anchor: $0) },
                takeKeptFlips: { takesFlips ? try await host.takeKeptFlips() : [] },
                item: { FileProviderItem(ffi: $0, readOnly: readOnly) })
        }
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        // The feed's anchor for the tree as it stands; a host that cannot say
        // hands back none, and the OS starts from a full report.
        let host = self.host
        Task {
            let anchor = try? await host.currentChangeAnchor()
            completionHandler(anchor.map { NSFileProviderSyncAnchor(Data($0)) })
        }
    }
}
