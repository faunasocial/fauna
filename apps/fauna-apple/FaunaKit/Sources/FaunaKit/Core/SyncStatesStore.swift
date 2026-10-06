import Foundation

/// Per-file **display** sync state for the `sync-state-badge` — the client half of
/// `docs/goal/behavior/file-sync.md` § Per-file sync-status display.
///
/// The converged apple engine is that section's ratified **first consumer** of
/// `SyncState::to_display()`: the shared engine's per-set `SyncDb` is the one
/// per-file state store (Swift keeps no mirror — the SwiftData `SyncFile` model it
/// used to keep retired with the B2 cutover), and `FfiSyncEngineHost.fileStates`
/// surfaces it already collapsed from the engine's eight states to the six display
/// states. This store is a pure cache of that read.
///
/// **Pull only.** The in-app host runs no resident engine, so nothing pushes a
/// transfer event: the Media page calls ``refresh(folders:)`` with the sets it
/// shows and this store re-reads the authoritative `fileStates`, never
/// duplicating engine state across the FFI boundary.
@MainActor @Observable
public final class SyncStatesStore {
    /// `"<folder>/<path>"` → the file's display state.
    ///
    /// A file with **no** entry is one this device holds no engine row for — a set
    /// it doesn't sync locally (every set on iOS, which has no folder bindings).
    /// The Media page renders those as `.remoteOnly`: on the nest only, not present
    /// here, which is exactly what the six-state vocabulary means by it.
    public private(set) var states: [String: SyncDisplayState] = [:]

    private var host: FfiSyncEngineHost?

    /// macOS's second root: the **container** domain's scoped dir, read as the
    /// container's steward for the sets whose local presence is a File Provider
    /// domain (`SyncStateDir`'s two-domain doc — the extension hosts those sets
    /// in the container; the host above reads its own root, the user domain,
    /// where the agent-hosted sets live). `nil` on iOS (the host's root IS the
    /// container), under e2e, and when the container is unreachable.
    private var containerScopedDir: URL?

    public init() {}

    /// Take the session's engine host — and, on macOS, the container root the
    /// FP-bound sets are read from. Called once the host is built (post-auth).
    public func attach(host: FfiSyncEngineHost, containerScopedDir: URL? = nil) {
        self.host = host
        self.containerScopedDir = containerScopedDir
    }

    /// Drop the host and every cached state (sign-out / factory reset).
    public func detach() {
        host = nil
        containerScopedDir = nil
        states.removeAll()
    }

    /// This file's display state, or `nil` when the device has no engine row for it.
    public func state(folder: String, path: String) -> SyncDisplayState? {
        states["\(folder)/\(path)"]
    }

    /// Which root one set's `fsid-<ref>.db` is read from — the **two-root fold**
    /// (`on-demand-files.md` § Apple File Provider binding, *state unification*):
    /// a set whose local presence is a registered File Provider domain is hosted
    /// by the sandboxed extension, so its DB is in the container; every other set
    /// this device hosts is in the host's own root. Registered domains are the
    /// truth of where the DB lives — a bound set never has one (the binding
    /// yields the domain before its resident engine starts) and an unbound set
    /// gets it back on the next reconcile — which is what routes a set that has
    /// switched hosts to its CURRENT root rather than the stale DB the old host
    /// left behind. Keyed by the set's `FolderRef` wire string — the ref half of
    /// the registered domain identifier, for the active account's domains only
    /// (`fileProviderBoundRefs`) — so a same-named set elsewhere, or another
    /// account's same-numbered ref, never borrows another's root. Pure, so the
    /// routing is pinned headlessly (`SyncStatesStoreTests`).
    enum ReadRoot: Equatable {
        case host
        case container
    }

    static func readRoot(folderId: String, fileProviderBound: Set<String>, hasContainer: Bool)
        -> ReadRoot
    {
        fileProviderBound.contains(folderId) && hasContainer ? .container : .host
    }

    /// One set's slice replaced wholesale, so a file the engine no longer tracks
    /// (deleted → `to_display()` == nil → omitted from the reply) loses its badge
    /// instead of keeping a stale one. Pure (pinned in `SyncStatesStoreTests`).
    static func replacingSlice(
        of states: [String: SyncDisplayState], folder: String,
        with entries: [FfiSyncFileState]
    ) -> [String: SyncDisplayState] {
        var next = states.filter { !$0.key.hasPrefix("\(folder)/") }
        for entry in entries {
            next["\(folder)/\(entry.path)"] = entry.state
        }
        return next
    }

    /// Re-read `fileStates` for each set — the Media page calls this with the
    /// sets its snapshot spans (`MediaPageSnapshot.folderOptions`: the name the
    /// badge map is keyed by, plus the set's identity the READ is keyed by —
    /// the `fsid-<ref>.db` the hosting engine writes). Best-effort: a set with
    /// no state DB on this device (never hosted here) simply contributes
    /// nothing — and the read never mints one (the shared read is pure,
    /// `sync_file_states`). A set with no identity (known only from its items:
    /// a shared-with-me set the caller's list does not carry) is one this
    /// device hosts nowhere, and is skipped.
    public func refresh(folders: [MediaFolderOption]) async {
        guard let host else { return }
        let fileProviderBound = await Self.fileProviderBoundSets()
        for option in folders {
            let folder = option.name
            guard let folderId = option.folderId else { continue }
            let entries: [FfiSyncFileState]?
            switch Self.readRoot(
                folderId: folderId, fileProviderBound: fileProviderBound,
                hasContainer: containerScopedDir != nil)
            {
            case .container:
                let root = containerScopedDir!
                entries = logTry(
                    .debug, "fauna.sync", "fileStates \(folder) (container)",
                    { try syncFileStates(stateDir: root.path, folderId: folderId) })
            case .host:
                entries = await logTryAsync(
                    .debug, "fauna.sync", "fileStates \(folder)",
                    { try await host.fileStates(folderId: folderId) })
            }
            guard let entries else { continue }
            states = Self.replacingSlice(of: states, folder: folder, with: entries)
        }
    }

    /// The refs of the ACTIVE account's sets whose local presence is a
    /// registered File Provider domain: each registered identifier is the
    /// set's actor-scoped identity (`<ref-component>@<actor>`), and only the ones scoped
    /// to `actorIdHex` name a set this session reads — another account's
    /// lingering domain, or an identifier from before actor scoping, names
    /// none. Pure (pinned in `SyncStatesStoreTests`).
    static func fileProviderBoundRefs(domainIds: [String], actorIdHex: String) -> Set<String> {
        Set(
            domainIds.compactMap { domainId in
                guard let identity = FileProviderDomainIdentity.parse(domainId),
                    identity.actorIdHex == actorIdHex.lowercased()
                else { return nil }
                return identity.folderId
            })
    }

    /// The registered domains resolved to the active account's refs
    /// (`fileProviderBoundRefs`). Listing fails outside the appex-embedding
    /// bundle (`mac-app`'s bare binary, e2e), which is exactly the "no
    /// extension is serving anything" answer; so does an unpublished actor.
    /// iOS: the host's root is the container already — nothing to route.
    private static func fileProviderBoundSets() async -> Set<String> {
        #if os(macOS)
            guard let actorIdHex = FaunaClient.activeActorIdHex,
                let domainIds = try? await FileProviderDomains.list()
            else { return [] }
            return fileProviderBoundRefs(domainIds: domainIds, actorIdHex: actorIdHex)
        #else
            return []
        #endif
    }
}
