import FaunaFFISwift
import Foundation

// NOT excised by `FAUNA_EXCISE_P2P_SHARE`: relay serving is not an excisable
// feature (`relay-serving.md` § Relay serving → *The seat serves from the
// file*), so its lease crosses a door of its own, beside the share plane's.
#if os(iOS)
    import FileProvider

    /// The relay lease's door, crossed from the iOS app to its File Provider
    /// extension (`relay-serving.md` § Relay serving → *Apple's File Provider
    /// replica*, point (2)): the extension's seat serves the relay only while
    /// the app, in front, holds the lease granted here. Crossed over the same
    /// app→provider service channel as the share plane's ingest door
    /// (`ShareIngestService`), under a service name of its own.
    @objc public protocol FaunaRelayLeaseService {
        /// Grant or renew the lease on this domain's seat (the host's
        /// `grantRelayLease`). Replies once granted.
        func grantRelayLease(reply: @escaping () -> Void)

        /// End the lease now (the host's `revokeRelayLease`): the seat stops
        /// and withdraws. Replies once revoked.
        func revokeRelayLease(reply: @escaping () -> Void)
    }

    public enum RelayLeaseService {
        /// The service source's name — what the app looks up among the
        /// extension's services for a domain's root.
        public static let serviceName = NSFileProviderServiceName("social.fauna.relay-lease")

        /// The interface both ends configure the connection with.
        public static func interface() -> NSXPCInterface {
            NSXPCInterface(with: FaunaRelayLeaseService.self)
        }
    }

    /// The extension's side: the door over one domain's host.
    public final class RelayLeaseDoor: NSObject, FaunaRelayLeaseService {
        private let host: FfiFileProviderHost

        public init(host: FfiFileProviderHost) {
            self.host = host
        }

        public func grantRelayLease(reply: @escaping () -> Void) {
            host.grantRelayLease()
            reply()
        }

        public func revokeRelayLease(reply: @escaping () -> Void) {
            host.revokeRelayLease()
            reply()
        }
    }

    /// The extension's service source for the lease door: one anonymous
    /// listener per extension instance, whose connections each export the
    /// door over this domain's host.
    public final class RelayLeaseServiceSource: NSObject, NSFileProviderServiceSource,
        NSXPCListenerDelegate
    {
        private let door: RelayLeaseDoor
        private lazy var listener: NSXPCListener = {
            let listener = NSXPCListener.anonymous()
            listener.delegate = self
            listener.resume()
            return listener
        }()

        public init(host: FfiFileProviderHost) {
            door = RelayLeaseDoor(host: host)
        }

        public var serviceName: NSFileProviderServiceName { RelayLeaseService.serviceName }

        public func makeListenerEndpoint() throws -> NSXPCListenerEndpoint {
            listener.endpoint
        }

        public func listener(_: NSXPCListener, shouldAcceptNewConnection connection: NSXPCConnection)
            -> Bool
        {
            connection.exportedInterface = RelayLeaseService.interface()
            connection.exportedObject = door
            connection.resume()
            return true
        }
    }

    /// The app's side: every registered domain of one account, the lease
    /// granted or revoked across each domain's door. Opening a domain's
    /// connection launches a stopped extension, whose host then connects and,
    /// under the lease, announces.
    public struct RelayLeaseDoors: RelayLeaseDoorSet {
        private let actorIdHex: String

        /// How long one domain's grant or revoke may take: a cold extension
        /// launch measured under 2 s (`FileProviderReplicaHost`). A door that
        /// does not answer in time is passed over — its lease lapses.
        private static let bound: Duration = .seconds(15)

        public init(actorIdHex: String) {
            self.actorIdHex = actorIdHex
        }

        public func grantAll() async {
            await each { proxy, done in proxy.grantRelayLease(reply: done) }
        }

        public func revokeAll() async {
            await each { proxy, done in proxy.revokeRelayLease(reply: done) }
        }

        /// Cross every domain's door at once, each bounded.
        private func each(
            _ call: @escaping @Sendable (FaunaRelayLeaseService, @escaping () -> Void) -> Void
        ) async {
            let domains = (try? await NSFileProviderManager.domains()) ?? []
            let mine = domains.filter {
                FileProviderDomainIdentity.parse($0.identifier.rawValue)?.actorIdHex == actorIdHex
            }
            await withTaskGroup(of: Void.self) { group in
                for domain in mine {
                    group.addTask { await Self.cross(domain, call) }
                }
            }
        }

        private static func cross(
            _ domain: NSFileProviderDomain,
            _ call: @escaping @Sendable (FaunaRelayLeaseService, @escaping () -> Void) -> Void
        ) async {
            guard let connection = try? await open(domain) else { return }
            defer { connection.invalidate() }
            // A door that does not answer in time: the invalidation fires the
            // proxy's error handler, which ends the wait.
            let timer = Task {
                try? await Task.sleep(for: bound)
                connection.invalidate()
            }
            defer { timer.cancel() }
            await withCheckedContinuation { (finished: CheckedContinuation<Void, Never>) in
                let once = Once(finished)
                let proxy =
                    connection.remoteObjectProxyWithErrorHandler { _ in once.fire() }
                    as? FaunaRelayLeaseService
                guard let proxy else { return once.fire() }
                call(proxy) { once.fire() }
            }
        }

        /// The lease service's connection for `domain`.
        private static func open(_ domain: NSFileProviderDomain) async throws -> NSXPCConnection {
            guard let manager = NSFileProviderManager(for: domain) else {
                throw CocoaError(.featureUnsupported)
            }
            let root = try await manager.getUserVisibleURL(for: .rootContainer)
            let services = try await FileManager.default.fileProviderServicesForItem(at: root)
            guard let service = services[RelayLeaseService.serviceName] else {
                throw CocoaError(.featureUnsupported)
            }
            let connection = try await service.fileProviderConnection()
            connection.remoteObjectInterface = RelayLeaseService.interface()
            connection.resume()
            return connection
        }
    }

    /// Resumes a continuation once, whichever of the reply and the error
    /// handler comes first.
    private final class Once: @unchecked Sendable {
        private let lock = NSLock()
        private var continuation: CheckedContinuation<Void, Never>?

        init(_ continuation: CheckedContinuation<Void, Never>) {
            self.continuation = continuation
        }

        func fire() {
            lock.lock()
            let taken = continuation
            continuation = nil
            lock.unlock()
            taken?.resume()
        }
    }
#endif
