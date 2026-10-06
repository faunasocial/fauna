import FaunaKit
import FileProvider
import Foundation
import os

/// The Fauna `NSFileProviderReplicatedExtension` (M2 read path).
///
/// One extension instance per File Provider domain; the domain identifier is the
/// set's actor-scoped identity (`FileProviderDomainIdentity`,
/// `<ref-component>@<actor>` — `local%3A1@…`), from which this instance recovers
/// the account it serves under and the set's bare ref for the Rust host. Each instance holds one `folder`-scoped
/// `FfiFileProviderHost` — the app-dead hydration host that runs the shared
/// `fauna-sync-engine` behind the OS callbacks (`file-sync.md` § On-Demand Files →
/// Apple File Provider binding, *the extension hosts the engines*). The OS owns the
/// on-disk tree under `~/Library/CloudStorage/` and drives enumerate / `item` /
/// `fetchContents` / eviction; fauna supplies the engine face.
///
/// Write path (M3 slice 1): `createItem`/`modifyItem`/`deleteItem` materialize the
/// OS-provided bytes into the engine's staging root (`fileProviderRootDir` — the
/// `watch_dir` `upload_file` reads) and drive `host.ingest`/`rename`/`delete`,
/// acking the OS **only on `UploadOutcome::recorded`** (`upload_ack`). A
/// not-recorded / failed upload returns an error, which leaves the change pending
/// in the OS's replicated queue for retry — that OS-side pending set IS the
/// offline queue (nothing to persist ourselves).
///
/// Ignore gate: an ignored rel (dotfile component, built-in default ignore,
/// `.faunaignore` pattern — `file-sync.md` § Built-in default ignores) never
/// ingests. The engine-side write cores answer `excluded`, and `createItem`
/// additionally asks `host.isIgnored` up front (before staging bytes; folders
/// excluded whole so the OS never fans out their children). Both map to
/// `NSFileProviderError.excludedFromSync`: the OS keeps the local file, stops
/// syncing it, and re-evaluates via a fresh `createItem` when it changes.
///
/// Conflict arm (M3): a content-only `modifyItem` ingests carrying the OS's
/// `baseVersion` (`ingestWithBase`). When the row's head has moved since the OS
/// read it — a concurrent writer advanced the nest head while the OS edited an
/// older version — the staged write routes through the engine's shared
/// auto-resolve (retention-first, non-destructive; `file-sync.md` § Conflicts)
/// rather than clobbering the newer head; when the base still matches it is a
/// plain fast-forward ingest. A merge / incoming winner comes back with the
/// ack's `contentChanged`, surfaced as the completion's `shouldFetchContent`,
/// so the OS re-fetches the winning content it does not hold. Rename/move stays
/// the single-writer delete+ingest pair (a rename's content is new — no shared
/// base to reconcile).
///
/// Fail-closed: if the app has not provisioned the shared-Keychain capability
/// (signed out, or no domain yet), `host` is `nil` and every callback reports
/// `.notAuthenticated` rather than serving — the extension never fabricates content.
final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    let domain: NSFileProviderDomain
    private let host: FfiFileProviderHost?
    /// The engine's staging root for this set (bytes land here before `ingest`).
    private let rootDir: URL?
    /// The in-session live-refresh tick (see `startRefreshTick`), cancelled on
    /// `invalidate()`.
    private var refreshTask: Task<Void, Never>?

    private static let log = Logger(subsystem: "social.fauna.fileprovider", category: "extension")

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        // The extension is its own process (never shares the app's Rust statics), so
        // without this it has no TOFU trust for a self-signed/LAN nest and every
        // connect attempt fails TLS validation — silently, since the engine retries
        // rather than surfacing the error, which reads as a stuck "Loading" in Files.
        // Read-only over the app-group store (security.md § Transport trust, pin
        // custody): the extension consumes the pins the app minted at onboarding —
        // a per-process store here was always empty, and a writable one would let
        // a background process TOFU-mint with no user in the loop.
        NestTrust.installPinStoreReadOnly()
        // Also never installed here, so every `tracing::info!`/`error!` in the
        // engine worker (including the initial `populate_placeholders_from_nest`
        // success/failure line) went nowhere — the extension ran with zero
        // diagnostic visibility. Mirrors FaunaApp/FaunaMacApp's launch call.
        //
        // It must land in the shared **app-group** container, not the extension's
        // own `.applicationSupportDirectory`. Measured on iOS: an appex's
        // `.applicationSupportDirectory` does not resolve to any readable per-appex
        // container (no `PluginKitPlugin` container carries a `social.fauna`
        // identifier, and `Library/Application Support/` is absent from the ones
        // that exist), so `installLogging` silently wrote nowhere reachable and the
        // one line that diagnoses a non-rendering domain stayed invisible. The app
        // group is a directory the app, this extension, and a developer inspecting
        // a simulator can all reach, and `FileProvider/` keeps it clear of the
        // sync-state tenants (`<group>/sync`, `<group>/FileProvider/state`).
        //
        // Note this is a *file* the host reads, not the app's Settings → Logs page:
        // that page renders `logSnapshot()`, an in-process ring, so it can only ever
        // show the app's own lines — never another process's.
        let appGroupLogDir = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: FileProviderCredentialStore.accessGroup
        )?.appendingPathComponent("FileProvider", isDirectory: true).path
        let logDir = appGroupLogDir
            ?? (try? FileManager.default.url(
                for: .applicationSupportDirectory, in: .userDomainMask,
                appropriateFor: nil, create: true))?.path
            ?? NSTemporaryDirectory()
        installLogging(dataDir: logDir)
        Self.log.info(
            "file-provider extension logging to \(logDir, privacy: .public) for domain \(domain.identifier.rawValue, privacy: .public)"
        )
        self.rootDir = FileProviderDomainIdentity.parse(domain.identifier.rawValue)
            .flatMap { try? fileProviderRootDir(for: $0) }
        do {
            self.host = try makeFileProviderHost(domainId: domain.identifier.rawValue)
        } catch FileProviderHostError.notProvisioned {
            // Normal signed-out / no-domain-yet state — fail closed quietly.
            self.host = nil
        } catch FileProviderHostError.unscopedDomainIdentifier {
            // A domain identifier that does not parse as actor-scoped (a bare ref or a set
            // name): never served; the app's next reconcile removes it.
            Self.log.info(
                "domain \(domain.identifier.rawValue, privacy: .public) scopes no set to an account — failing closed until the app removes it"
            )
            self.host = nil
        } catch FileProviderHostError.foreignDomainOwner {
            // A dirty domain lingering across an account switch (the iOS
            // drain-hold): another account owns it, so serving it under the
            // provisioned credentials would cross accounts. Expected runtime
            // state — fail closed; the drain resumes when the owner signs in.
            Self.log.info(
                "domain \(domain.identifier.rawValue, privacy: .public) is owned by another account — failing closed until its owner returns"
            )
            self.host = nil
        } catch {
            // `.noAppGroupContainer` (missing entitlement) is a *packaging* bug, not a
            // runtime state; surface it instead of letting `try?` swallow it into an
            // indistinguishable `.notAuthenticated`.
            Self.log.error(
                "FileProvider host build failed for domain \(domain.identifier.rawValue, privacy: .public): \(String(describing: error), privacy: .public)"
            )
            self.host = nil
        }
        super.init()
        startRefreshTick()
    }

    func invalidate() {
        refreshTask?.cancel()
        refreshTask = nil
    }

    /// Drive an in-session re-pull tick so a remote change appears without waiting
    /// for `fileproviderd` to reconstruct the extension. The FP host runs no
    /// watcher/loop (control inversion), so on the shared reconcile cadence
    /// (`defaultRescanIntervalSecs()` — the one constant every seat ticks at
    /// since folders re-model phase 5, crossed from Rust rather than copied
    /// here) we `host.refresh()` and, iff it folded something new, tell the OS to
    /// re-enumerate via `signalEnumerator(for: .rootContainer)` (`file-sync.md`
    /// § Apple File Provider binding — *a pulled remote change signals the
    /// enumerator*). A `nil` host or an indeterminate binding starts no tick.
    ///
    /// The same tick runs **eviction observation**: the OS evicts materialized
    /// items behind the provider's back ("Remove Download", storage pressure)
    /// and never calls the extension about it, so we diff the host's hydrated
    /// set against `enumeratorForMaterializedItems` and demote what the OS
    /// dropped (`host.evict` → `mark_placeholder`), keeping the dehydration
    /// bookkeeping honest. The diff is the pure, headlessly-tested
    /// `FileProviderEviction.evictionCandidates`; only the OS query is live.
    private func startRefreshTick() {
        guard host != nil else { return }
        refreshTask = Task { [weak self] in
            guard let self, let host = self.host else { return }
            let secs = defaultRescanIntervalSecs()
            guard secs > 0 else { return }
            let interval = secs * 1_000_000_000
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: interval)
                if Task.isCancelled { break }
                if (try? await host.refresh()) == true {
                    try? await NSFileProviderManager(for: self.domain)?
                        .signalEnumerator(for: .rootContainer)
                }
                await self.observeEvictions(host: host)
            }
        }
    }

    /// One eviction-observation pass (see `startRefreshTick`). A failed or
    /// partial OS enumeration skips the whole pass — evicting from a partial
    /// materialized set would demote items the OS still holds.
    private func observeEvictions(host: FfiFileProviderHost) async {
        guard let materialized = await materializedRels() else { return }
        guard let hydrated = try? await host.hydratedRels() else { return }
        for rel in FileProviderEviction.evictionCandidates(
            hydratedRels: hydrated, materializedRels: materialized)
        {
            do {
                try await host.evict(rel: rel)
                Self.log.info(
                    "evicted \(rel, privacy: .public) — OS dropped its materialized copy")
            } catch {
                Self.log.error(
                    "evict \(rel, privacy: .public) failed: \(String(describing: error), privacy: .public)"
                )
            }
        }
    }

    /// The OS's materialized-item set for this domain, or `nil` when it cannot
    /// be fully enumerated (no manager, or the enumeration errored mid-way).
    private func materializedRels() async -> Set<String>? {
        guard let manager = NSFileProviderManager(for: domain) else { return nil }
        let enumerator = manager.enumeratorForMaterializedItems()
        return await withCheckedContinuation { continuation in
            let observer = MaterializedRelsObserver(enumerator: enumerator) {
                continuation.resume(returning: $0)
            }
            enumerator.enumerateItems(
                for: observer,
                startingAt: NSFileProviderPage(
                    NSFileProviderPage.initialPageSortedByName as Data))
        }
    }

    /// The folder-relative `rel` path an item identifier maps to (`""` = the file
    /// set root). Non-root identifiers already carry the `rel` as their raw value.
    static func rel(for identifier: NSFileProviderItemIdentifier) -> String {
        identifier == .rootContainer ? "" : identifier.rawValue
    }

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request _: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        guard let host else {
            if identifier == .rootContainer {
                completionHandler(
                    FileProviderItem.rootContainer(named: domain.displayName, readOnly: true), nil)
            } else {
                completionHandler(nil, NSFileProviderError(.notAuthenticated))
            }
            progress.completedUnitCount = 1
            return progress
        }
        let rel = Self.rel(for: identifier)
        let displayName = domain.displayName
        Task {
            let readOnly = await FileProviderItemCapabilities.isReadOnly(host)
            if identifier == .rootContainer {
                completionHandler(
                    FileProviderItem.rootContainer(named: displayName, readOnly: readOnly), nil)
                progress.completedUnitCount = 1
                return
            }
            do {
                if let ffi = try await host.item(rel: rel) {
                    completionHandler(FileProviderItem(ffi: ffi, readOnly: readOnly), nil)
                } else {
                    completionHandler(nil, NSFileProviderError(.noSuchItem))
                }
            } catch {
                completionHandler(nil, error)
            }
            progress.completedUnitCount = 1
        }
        return progress
    }

    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version _: NSFileProviderItemVersion?,
        request _: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        guard let host else {
            completionHandler(nil, nil, NSFileProviderError(.notAuthenticated))
            progress.completedUnitCount = 1
            return progress
        }
        let rel = Self.rel(for: itemIdentifier)
        Task {
            do {
                // Bounded-memory hydrate (`fetch_to_path`): Rust writes the
                // decrypted plaintext straight to the temp URL, so no file ever
                // crosses UniFFI as one buffer (the iOS appex memory cap; also
                // just correct on macOS for multi-GB files). `mark_hydrated` +
                // the verified whole-file hash as the new contentVersion happen
                // engine-side.
                let tmp = FileManager.default.temporaryDirectory
                    .appendingPathComponent(UUID().uuidString)
                let contentVersion = try await host.fetchToPath(rel: rel, destPath: tmp.path)
                let meta = try await host.item(rel: rel)
                let readOnly = await FileProviderItemCapabilities.isReadOnly(host)
                let item: NSFileProviderItem
                if let meta {
                    item = FileProviderItem(
                        ffi: meta, overrideContentVersion: contentVersion, readOnly: readOnly)
                } else {
                    let size =
                        (try? FileManager.default.attributesOfItem(atPath: tmp.path))?[.size]
                        as? Int64 ?? 0
                    item = FileProviderItem(
                        identifier: itemIdentifier,
                        parent: FileProviderItem.parentIdentifier(forRel: rel),
                        name: (rel as NSString).lastPathComponent,
                        isFolder: false,
                        size: size,
                        contentVersion: contentVersion,
                        readOnly: readOnly
                    )
                }
                completionHandler(tmp, item, nil)
            } catch {
                completionHandler(nil, nil, error)
            }
            progress.completedUnitCount = 1
        }
        return progress
    }

    // MARK: - Write path (M3 slice 1)

    /// Copy the OS-provided contents into the engine staging root at `rel`
    /// (creating intermediate dirs, replacing any stale copy) so `ingest` — which
    /// reads `watch_dir` — seals + uploads exactly the bytes the OS handed us.
    private func materialize(contents: URL, atRel rel: String) throws {
        guard let rootDir else { throw NSFileProviderError(.notAuthenticated) }
        let dst = rootDir.appendingPathComponent(rel)
        try FileManager.default.createDirectory(
            at: dst.deletingLastPathComponent(), withIntermediateDirectories: true)
        if FileManager.default.fileExists(atPath: dst.path) {
            try FileManager.default.removeItem(at: dst)
        }
        try FileManager.default.copyItem(at: contents, to: dst)
    }

    /// Bytes for a rel that must be re-ingested at a new path (rename): prefer the
    /// staged copy, else hydrate from the nest (a rename of a never-hydrated
    /// placeholder still needs bytes — `serve_rename` re-ingests under the new rel).
    private func stagedOrFetchedBytes(rel: String, host: FfiFileProviderHost) async throws -> Data {
        if let rootDir {
            let staged = rootDir.appendingPathComponent(rel)
            if let data = try? Data(contentsOf: staged) { return data }
        }
        return try await host.fetch(rel: rel).bytes
    }

    /// The recorded-ack gate: a not-recorded outcome (nest unreachable, record
    /// refused) surfaces as `.serverUnreachable`, which keeps the change pending
    /// OS-side (the offline queue) — the OS must never be told a change landed
    /// that the nest has not recorded. Returns the ack so callers can read
    /// `contentChanged` (the `shouldFetchContent` signal — a conflicted modify
    /// whose winner is content the OS does not hold).
    @discardableResult
    private func requireAcked(_ ack: FfiFileProviderAck) throws -> FfiFileProviderAck {
        // An ignored rel (dotfile, Office lock/`.tmp` litter, `.faunaignore`
        // pattern) was deliberately not ingested: `.excludedFromSync` is the
        // OS's purpose-built answer — the file stays on the user's disk, the
        // system stops syncing it and re-evaluates via a fresh `createItem`
        // when it changes (file-sync.md § Built-in default ignores).
        guard !ack.excluded else { throw NSFileProviderError(.excludedFromSync) }
        guard ack.acked else { throw NSFileProviderError(.serverUnreachable) }
        return ack
    }

    /// The refreshed item handed back to the OS after a successful write (only
    /// a writable set gets one — `refuseIfReadOnly` ran first).
    private func refreshedItem(rel: String, host: FfiFileProviderHost) async throws
        -> NSFileProviderItem
    {
        if let meta = try await host.item(rel: rel) {
            return FileProviderItem(ffi: meta, readOnly: false)
        }
        throw NSFileProviderError(.noSuchItem)
    }

    /// The read-only gate every write callback passes first, before any byte
    /// is staged: a set the account holds as a reader advertises no write
    /// capability, and a write the OS sends anyway is refused here with the
    /// platform's read-only error (`on-demand-files.md` § Shared sets on a
    /// capability host, decision 3). The host refuses it too.
    private func refuseIfReadOnly(_ host: FfiFileProviderHost) async throws {
        if await FileProviderItemCapabilities.isReadOnly(host) {
            throw FileProviderItemCapabilities.readOnlyRefusal()
        }
    }

    func createItem(
        basedOn itemTemplate: NSFileProviderItem,
        fields _: NSFileProviderItemFields,
        contents: URL?,
        options _: NSFileProviderCreateItemOptions = [],
        request _: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        guard let host else {
            completionHandler(nil, [], false, NSFileProviderError(.notAuthenticated))
            progress.completedUnitCount = 1
            return progress
        }
        let parentRel = Self.rel(for: itemTemplate.parentItemIdentifier)
        let rel = parentRel.isEmpty
            ? itemTemplate.filename : "\(parentRel)/\(itemTemplate.filename)"
        let parentIdentifier = itemTemplate.parentItemIdentifier
        let filename = itemTemplate.filename
        let isFolder = itemTemplate.contentType == .folder

        Task {
            do {
                try await refuseIfReadOnly(host)
                // Ignored rels (dotfiles, Office lock/`.tmp` litter,
                // `.faunaignore` patterns) never enter the set — checked before
                // any byte is staged. For a folder the answer excludes the
                // whole subtree in one call: the OS then never sends
                // `createItem` for its children (`.git/`, `~$dir/`).
                if try await host.isIgnored(rel: rel) {
                    throw NSFileProviderError(.excludedFromSync)
                }
                if isFolder {
                    // The engine has no directory records — a directory exists
                    // once a child rel does (synthesized on enumerate). Ack the
                    // create locally so the OS proceeds to create children
                    // under it; an empty folder that never gains a child is not
                    // durable (documented v1 shape, same as the row model).
                    completionHandler(
                        FileProviderItem(
                            identifier: NSFileProviderItemIdentifier(rel),
                            parent: parentIdentifier,
                            name: filename,
                            isFolder: true,
                            readOnly: false
                        ), [], false, nil)
                    progress.completedUnitCount = 1
                    return
                }
                if let contents {
                    try materialize(contents: contents, atRel: rel)
                } else if let rootDir {
                    // A contentless file create (e.g. `touch`): stage an empty file.
                    let dst = rootDir.appendingPathComponent(rel)
                    try FileManager.default.createDirectory(
                        at: dst.deletingLastPathComponent(), withIntermediateDirectories: true)
                    FileManager.default.createFile(atPath: dst.path, contents: Data())
                }
                try requireAcked(try await host.ingest(rel: rel))
                completionHandler(try await refreshedItem(rel: rel, host: host), [], false, nil)
            } catch {
                completionHandler(nil, [], false, error)
            }
            progress.completedUnitCount = 1
        }
        return progress
    }

    func modifyItem(
        _ item: NSFileProviderItem,
        baseVersion: NSFileProviderItemVersion,
        changedFields: NSFileProviderItemFields,
        contents: URL?,
        options _: NSFileProviderModifyItemOptions = [],
        request _: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        guard let host else {
            completionHandler(nil, [], false, NSFileProviderError(.notAuthenticated))
            progress.completedUnitCount = 1
            return progress
        }

        let oldRel = Self.rel(for: item.itemIdentifier)
        let oldParent = FileProviderPathMapping.parentRel(forRel: oldRel) ?? ""
        let newName =
            changedFields.contains(.filename)
            ? item.filename : (oldRel as NSString).lastPathComponent
        let newParent =
            changedFields.contains(.parentItemIdentifier)
            ? Self.rel(for: item.parentItemIdentifier) : oldParent
        let newRel = newParent.isEmpty ? newName : "\(newParent)/\(newName)"

        Task {
            do {
                try await refuseIfReadOnly(host)
                if item.contentType == .folder {
                    // Directory rename/move: no dir records — sweep every child row
                    // under the old prefix through the delete+ingest rename pair.
                    guard newRel != oldRel else {
                        completionHandler(item, [], false, nil)
                        progress.completedUnitCount = 1
                        return
                    }
                    if try await host.isIgnored(rel: newRel) {
                        // Renaming a directory to an ignored name takes the
                        // whole subtree out of the set: tombstone every child
                        // row (other devices drop them), then hand the OS
                        // `.excludedFromSync` so the local tree stays put and
                        // stops syncing.
                        try await deleteSubtree(rel: oldRel, host: host)
                        throw NSFileProviderError(.excludedFromSync)
                    }
                    try await renameSubtree(from: oldRel, to: newRel, host: host)
                    completionHandler(
                        FileProviderItem(
                            identifier: NSFileProviderItemIdentifier(newRel),
                            parent: newParent.isEmpty
                                ? .rootContainer : NSFileProviderItemIdentifier(newParent),
                            name: newName,
                            isFolder: true,
                            readOnly: false
                        ), [], false, nil)
                    progress.completedUnitCount = 1
                    return
                }

                if newRel == oldRel {
                    // Content-only modify: stage the new bytes, then ingest carrying
                    // the OS's `baseVersion`. If the row's head has moved since the OS
                    // read it (a concurrent writer), `ingestWithBase` routes through
                    // the engine's shared auto-resolve instead of clobbering it; when
                    // the base still matches it is a plain fast-forward ingest. Acks
                    // on recorded either way (`file-sync.md` § Conflicts).
                    //
                    // `shouldFetchContent` = the ack's `contentChanged`: a resolve
                    // whose winner (merge / incoming) is content the OS does NOT
                    // hold must make the OS re-fetch it — returning `false` there
                    // associates the OS's loser bytes with the winning version, so
                    // the user keeps seeing the loser and the next edit carries the
                    // winning version as its base and fast-forwards over the
                    // resolved head.
                    var shouldFetchContent = false
                    if changedFields.contains(.contents), let contents {
                        try materialize(contents: contents, atRel: oldRel)
                        let ack = try requireAcked(
                            try await host.ingestWithBase(
                                rel: oldRel, baseContentVersion: baseVersion.contentVersion))
                        shouldFetchContent = ack.contentChanged
                    }
                    completionHandler(
                        try await refreshedItem(rel: oldRel, host: host), [],
                        shouldFetchContent, nil)
                } else {
                    // Rename / move (possibly with new content): stage the bytes at
                    // the NEW rel — `serve_rename` tombstones the old path and
                    // ingests the new one from staging.
                    if changedFields.contains(.contents), let contents {
                        try materialize(contents: contents, atRel: newRel)
                    } else {
                        let bytes = try await stagedOrFetchedBytes(rel: oldRel, host: host)
                        guard let rootDir else { throw NSFileProviderError(.notAuthenticated) }
                        let dst = rootDir.appendingPathComponent(newRel)
                        try FileManager.default.createDirectory(
                            at: dst.deletingLastPathComponent(), withIntermediateDirectories: true)
                        try bytes.write(to: dst)
                    }
                    try requireAcked(try await host.rename(fromRel: oldRel, toRel: newRel))
                    completionHandler(try await refreshedItem(rel: newRel, host: host), [], false, nil)
                }
            } catch {
                completionHandler(nil, [], false, error)
            }
            progress.completedUnitCount = 1
        }
        return progress
    }

    /// Rename every file row under `from`'s prefix to the same subpath under `to`
    /// (depth-first walk over the synthesized tree). Each child rides the shared
    /// delete+ingest rename pair with the recorded-ack gate.
    private func renameSubtree(from: String, to: String, host: FfiFileProviderHost) async throws {
        let children = try await host.enumerate(parentRel: from)
        for child in children {
            let childName = (child.rel as NSString).lastPathComponent
            let childTo = "\(to)/\(childName)"
            if child.isDir {
                try await renameSubtree(from: child.rel, to: childTo, host: host)
            } else {
                let bytes = try await stagedOrFetchedBytes(rel: child.rel, host: host)
                guard let rootDir else { throw NSFileProviderError(.notAuthenticated) }
                let dst = rootDir.appendingPathComponent(childTo)
                try FileManager.default.createDirectory(
                    at: dst.deletingLastPathComponent(), withIntermediateDirectories: true)
                try bytes.write(to: dst)
                let ack = try await host.rename(fromRel: child.rel, toRel: childTo)
                // An ignored child (a `.faunaignore` pattern added after it
                // synced): the rename tombstoned its old row and
                // excluded the new name — done, not an error for the
                // enclosing directory rename.
                if !ack.excluded { try requireAcked(ack) }
            }
        }
    }

    /// Tombstone every file row under `rel`'s prefix (recursive delete of a
    /// synthesized directory). Each child delete rides the recorded-ack gate —
    /// an unrecorded child delete throws, the OS keeps the whole delete pending,
    /// and the retry resolves already-recorded children vacuously.
    private func deleteSubtree(rel: String, host: FfiFileProviderHost) async throws {
        let children = try await host.enumerate(parentRel: rel)
        for child in children {
            if child.isDir {
                try await deleteSubtree(rel: child.rel, host: host)
            } else {
                try requireAcked(try await host.delete(rel: child.rel))
            }
        }
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion _: NSFileProviderItemVersion,
        options _: NSFileProviderDeleteItemOptions = [],
        request _: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        guard let host else {
            completionHandler(NSFileProviderError(.notAuthenticated))
            progress.completedUnitCount = 1
            return progress
        }
        let rel = Self.rel(for: identifier)
        Task {
            do {
                try await refuseIfReadOnly(host)
                if let meta = try await host.item(rel: rel), meta.isDir {
                    try await deleteSubtree(rel: rel, host: host)
                } else {
                    // Recorded-ack gate, same as create/modify/rename: a delete
                    // the nest has not recorded must NOT be acked — throwing
                    // `.serverUnreachable` keeps it pending OS-side for retry
                    // instead of the file silently living on everywhere else.
                    try requireAcked(try await host.delete(rel: rel))
                }
                // Drop the staged copy so a later create at the same rel can't
                // resurrect stale bytes.
                if let rootDir {
                    try? FileManager.default.removeItem(at: rootDir.appendingPathComponent(rel))
                }
                completionHandler(nil)
            } catch {
                completionHandler(error)
            }
            progress.completedUnitCount = 1
        }
        return progress
    }

    func enumerator(
        for containerItemIdentifier: NSFileProviderItemIdentifier,
        request _: NSFileProviderRequest
    ) throws -> NSFileProviderEnumerator {
        guard let host else { throw NSFileProviderError(.notAuthenticated) }
        return FileProviderEnumerator(container: containerItemIdentifier, host: host, domain: domain)
    }
}

/// Drains the OS's materialized-item enumerator
/// (`NSFileProviderManager.enumeratorForMaterializedItems`) into the full rel
/// set for the eviction-observation diff, following pagination. Completes with
/// `nil` on an enumeration error — the caller must then skip the eviction pass,
/// because diffing against a *partial* set would demote items the OS still
/// holds. Holds a self-retain until it completes (the enumerator does not keep
/// its observer alive across pages).
private final class MaterializedRelsObserver: NSObject, NSFileProviderEnumerationObserver {
    private let enumerator: NSFileProviderEnumerator
    private let onDone: (Set<String>?) -> Void
    private var rels: Set<String> = []
    private var selfRetain: MaterializedRelsObserver?

    init(enumerator: NSFileProviderEnumerator, onDone: @escaping (Set<String>?) -> Void) {
        self.enumerator = enumerator
        self.onDone = onDone
        super.init()
        selfRetain = self
    }

    func didEnumerate(_ updatedItems: [NSFileProviderItemProtocol]) {
        for item in updatedItems {
            rels.insert(FileProviderExtension.rel(for: item.itemIdentifier))
        }
    }

    func finishEnumerating(upTo nextPage: NSFileProviderPage?) {
        if let nextPage {
            enumerator.enumerateItems(for: self, startingAt: nextPage)
        } else {
            onDone(rels)
            selfRetain = nil
        }
    }

    func finishEnumeratingWithError(_ error: Error) {
        onDone(nil)
        selfRetain = nil
    }
}
