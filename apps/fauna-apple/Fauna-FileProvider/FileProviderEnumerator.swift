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
        // M2 read path: no incremental delta tracking (always reports zero changes
        // on this channel) — materialization instead goes through a fresh
        // `enumerateItems` the OS issues off a re-signal, exactly like the
        // extension's own periodic tick (`FileProviderExtension.startRefreshTick`).
        //
        // This is also the app-side wiring's landing point (file-sync.md § Remote-
        // change nudge, the `fauna.sync.changed` push arm): `signalEnumerator` is
        // the ONLY externally-reachable hook into this appex process, so
        // `FaunaClient.swift`'s push observer calling
        // `FileProviderCoordinator.signalChanged(folder:folderHash:)` from the MAIN app lands
        // here. Refresh first, then — mirroring the tick's own refresh-then-signal
        // pairing — re-signal iff something new landed, so the OS re-lists
        // reflecting the now-updated engine rows. Self-terminating: a second
        // refresh() with nothing new returns `false` and no further signal fires.
        let host = self.host
        let domain = self.domain
        Task {
            if (try? await host.refresh()) == true {
                try? await NSFileProviderManager(for: domain)?.signalEnumerator(for: .rootContainer)
            }
            observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
        }
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        completionHandler(NSFileProviderSyncAnchor(Data("v1".utf8)))
    }
}
