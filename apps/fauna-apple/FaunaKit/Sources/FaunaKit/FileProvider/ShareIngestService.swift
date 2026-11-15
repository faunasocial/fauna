import FaunaFFISwift
import Foundation

// EXCISED BY `FAUNA_EXCISE_P2P_SHARE` (the reason is written once, at the top of
// `SharePlaneModel.swift`): the store-safe FFI exports no share door.
#if !FAUNA_EXCISE_P2P_SHARE && canImport(FileProvider)
    import FileProvider

    /// The share plane's ingest door, crossed from the iOS app to its File
    /// Provider extension (`p2p-shared-set-build.md` § *Phone peers — design*,
    /// decision 3). The extension is each set's one writer, so a page of peer
    /// rows the app's plane accepted is handed to it over the platform's
    /// app→provider service channel — `NSFileProviderServicing`, measured on
    /// the simulator 2026-09-27 — with the bodies waiting in a spool in the
    /// app-group container, which both processes reach. The extension answers
    /// with its host's `shareIngest` (the same host operation android calls in
    /// process) and signals its enumerator.
    @objc public protocol FaunaShareIngestService {
        /// One page: `rows` are canonical dag-cbor peer rows, `spoolDir` the
        /// pre-fetched bodies, `provenActorHex` the peer the channel proved.
        /// Replies with the encoded outcome ([ShareIngestReply]), or an error
        /// message.
        func shareIngest(
            provenActorHex: String, rows: [Data], spoolDir: String,
            reply: @escaping (Data?, String?) -> Void)

        /// The channel's second door: a group set's review row's *use the
        /// other version* (`group-set.md` § Build design — the group set,
        /// decision 9) on the host's engine — the set's one writer is the
        /// extension, so the restore runs there, never in an app-side engine
        /// (decision 11). Replies `nil` once restored, or the refusal's
        /// message, the row kept.
        func restoreRetainedVersion(conflictId: Int64, reply: @escaping (String?) -> Void)
    }

    public enum ShareIngestService {
        /// The service source's name — what the app looks up among the
        /// extension's services for a domain's root.
        public static let serviceName = NSFileProviderServiceName("social.fauna.share-ingest")

        /// The interface both ends configure the connection with.
        public static func interface() -> NSXPCInterface {
            NSXPCInterface(with: FaunaShareIngestService.self)
        }
    }

    /// One ingest page's outcome as it crosses the service channel — the
    /// FFI's `FfiShareIngestOutcome`, field for field, in a form the XPC
    /// reply can carry. Pure, so the round trip is pinned headlessly.
    public struct ShareIngestReply: Codable, Equatable, Sendable {
        public var refused: UInt32
        public var overlaid: UInt32
        public var materialized: UInt32
        public var alreadyCurrent: UInt32
        public var cursor: Int64
        public var storageLimited: Bool

        public init(_ outcome: FfiShareIngestOutcome) {
            refused = outcome.refused
            overlaid = outcome.overlaid
            materialized = outcome.materialized
            alreadyCurrent = outcome.alreadyCurrent
            cursor = outcome.cursor
            storageLimited = outcome.storageLimited
        }

        public var outcome: FfiShareIngestOutcome {
            FfiShareIngestOutcome(
                refused: refused, overlaid: overlaid, materialized: materialized,
                alreadyCurrent: alreadyCurrent, cursor: cursor, storageLimited: storageLimited)
        }

        /// Whether the page changed anything the OS lists — the extension
        /// signals its enumerator only then.
        public var changedTheTree: Bool { overlaid > 0 || materialized > 0 }

        public func encoded() throws -> Data { try JSONEncoder().encode(self) }

        public static func decode(_ data: Data) throws -> ShareIngestReply {
            try JSONDecoder().decode(ShareIngestReply.self, from: data)
        }
    }
#endif

#if !FAUNA_EXCISE_P2P_SHARE && os(iOS)
    import FileProvider

    /// The app's view of its File Provider extension's replicas — the two
    /// foreign traits `foreignReplicaAccess` runs the share plane over
    /// (`libs/fauna-ffi/src/share_plane.rs`). `on_demand_replica_access`
    /// cannot serve iOS: it reads this process's host registry, and every
    /// host is the extension's.
    ///
    /// Both traits are called on a blocking thread Rust spawns for them,
    /// never the main thread, so each waits on the OS with a semaphore and a
    /// bound (the `EngineRoster` precedent).
    public final class FileProviderReplicaHosts: FfiForeignReplicaHosts {
        private let actorIdHex: String

        public init(actorIdHex: String) {
            self.actorIdHex = actorIdHex
        }

        public func hosts() -> [FfiForeignReplicaHost] {
            Self.registeredDomains()
                .compactMap { domain -> FfiForeignReplicaHost? in
                    guard let identity = FileProviderDomainIdentity.parse(domain.identifier.rawValue),
                        identity.actorIdHex == actorIdHex
                    else { return nil }
                    return FileProviderReplicaHost(domain: domain, identity: identity)
                }
        }

        /// This account's replica of the set `folderId`, when a domain for it
        /// is registered — the door a group set's review row restores through.
        func host(folderId: String) -> FileProviderReplicaHost? {
            for domain in Self.registeredDomains() {
                guard let identity = FileProviderDomainIdentity.parse(domain.identifier.rawValue),
                    identity.actorIdHex == actorIdHex, identity.folderId == folderId
                else { continue }
                return FileProviderReplicaHost(domain: domain, identity: identity)
            }
            return nil
        }

        /// The registered domains, or none when the OS does not answer in time.
        private static func registeredDomains() -> [NSFileProviderDomain] {
            let done = DispatchSemaphore(value: 0)
            var found: [NSFileProviderDomain] = []
            NSFileProviderManager.getDomainsWithCompletionHandler { domains, _ in
                found = domains
                done.signal()
            }
            return done.wait(timeout: .now() + 10) == .success ? found : []
        }
    }

    /// One domain's replica, hosted by the extension: the state DB and staging
    /// root are in the app-group container (the same derivation the
    /// extension's host takes — `makeFileProviderHost`), and the door is the
    /// extension's service.
    final class FileProviderReplicaHost: FfiForeignReplicaHost {
        private let domain: NSFileProviderDomain
        private let identity: FileProviderDomainIdentity

        /// How long one ingest page may take across the channel: a cold
        /// extension launch measured under 2 s, and a page lands bodies.
        private static let ingestBound: DispatchTimeInterval = .seconds(120)

        init(domain: NSFileProviderDomain, identity: FileProviderDomainIdentity) {
            self.domain = domain
            self.identity = identity
        }

        func replica() -> FfiForeignReplica? {
            guard let stateDir = SyncStateDir.resolve(actorIdHex: identity.actorIdHex, in: .container),
                let stagingRoot = try? fileProviderRootDir(for: identity)
            else { return nil }
            return FfiForeignReplica(
                folder: domain.displayName, folderId: identity.folderId,
                stateDir: stateDir.path, stagingRoot: stagingRoot.path)
        }

        func shareIngest(provenActorHex: String, rows: [Data], spoolDir: String) throws
            -> FfiShareIngestOutcome
        {
            let connection = try openConnection()
            defer { connection.invalidate() }
            let done = DispatchSemaphore(value: 0)
            var result: Result<FfiShareIngestOutcome, Error> = .failure(
                FfiError.General(msg: "the extension's share door did not answer"))
            let proxy =
                connection.remoteObjectProxyWithErrorHandler { error in
                    result = .failure(FfiError.General(msg: "the extension's share door failed: \(error)"))
                    done.signal()
                } as? FaunaShareIngestService
            guard let proxy else {
                throw FfiError.General(msg: "the extension's share door has the wrong interface")
            }
            proxy.shareIngest(provenActorHex: provenActorHex, rows: rows, spoolDir: spoolDir) {
                data, message in
                if let data, let reply = try? ShareIngestReply.decode(data) {
                    result = .success(reply.outcome)
                } else {
                    result = .failure(FfiError.General(msg: message ?? "an unreadable ingest reply"))
                }
                done.signal()
            }
            guard done.wait(timeout: .now() + Self.ingestBound) == .success else {
                throw FfiError.General(msg: "the extension's share door timed out")
            }
            return try result.get()
        }

        /// The review row's restore across the channel (the door's second
        /// method). Blocks on the extension's answer, bounded like a page.
        func restoreRetainedVersion(conflictId: Int64) throws {
            let connection = try openConnection()
            defer { connection.invalidate() }
            let done = DispatchSemaphore(value: 0)
            var failure: String? = "the extension's restore door did not answer"
            let proxy =
                connection.remoteObjectProxyWithErrorHandler { error in
                    failure = "the extension's restore door failed: \(error)"
                    done.signal()
                } as? FaunaShareIngestService
            guard let proxy else {
                throw FfiError.General(msg: "the extension's door has the wrong interface")
            }
            proxy.restoreRetainedVersion(conflictId: conflictId) { message in
                failure = message
                done.signal()
            }
            guard done.wait(timeout: .now() + Self.ingestBound) == .success else {
                throw FfiError.General(msg: "the extension's restore door timed out")
            }
            if let failure { throw FfiError.General(msg: failure) }
        }

        /// The service connection to the extension for this domain — opening it
        /// launches a stopped extension.
        private func openConnection() throws -> NSXPCConnection {
            guard let manager = NSFileProviderManager(for: domain) else {
                throw FfiError.General(msg: "no File Provider manager for the domain")
            }
            let root = try wait("the domain's root URL") { finish in
                manager.getUserVisibleURL(for: .rootContainer) { url, error in
                    finish(url.map(Result.success) ?? .failure(error ?? CocoaError(.fileNoSuchFile)))
                }
            }
            let service = try wait("the extension's services") { finish in
                FileManager.default.getFileProviderServicesForItem(at: root) { services, error in
                    if let service = services?[ShareIngestService.serviceName] {
                        finish(.success(service))
                    } else {
                        finish(.failure(error ?? CocoaError(.featureUnsupported)))
                    }
                }
            }
            let connection = try wait("the service connection") { finish in
                service.getFileProviderConnection { connection, error in
                    finish(connection.map(Result.success) ?? .failure(error ?? CocoaError(.featureUnsupported)))
                }
            }
            connection.remoteObjectInterface = ShareIngestService.interface()
            connection.resume()
            return connection
        }

        private func wait<T>(
            _ what: String, _ start: (@escaping (Result<T, Error>) -> Void) -> Void
        ) throws -> T {
            let done = DispatchSemaphore(value: 0)
            var result: Result<T, Error>?
            start { outcome in
                result = outcome
                done.signal()
            }
            guard done.wait(timeout: .now() + 15) == .success, let result else {
                throw FfiError.General(msg: "the share door timed out waiting for \(what)")
            }
            return try result.get()
        }
    }
#endif
