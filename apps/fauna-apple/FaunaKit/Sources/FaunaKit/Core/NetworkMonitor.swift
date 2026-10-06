import Foundation
import Network

@MainActor @Observable
public class NetworkMonitor {
    public var isExpensive: Bool = false
    public var isConstrained: Bool = false
    public var isConnected: Bool = true

    private let monitor = NWPathMonitor()
    private let queue = DispatchQueue(label: "social.fauna.network-monitor")

    public init() {
        monitor.pathUpdateHandler = { [weak self] path in
            Task { @MainActor in
                self?.isExpensive = path.isExpensive
                self?.isConstrained = path.isConstrained
                self?.isConnected = path.status == .satisfied
            }
        }
        monitor.start(queue: queue)
    }

    public func shouldSync() -> Bool {
        isConnected && !isConstrained
    }

    public func shouldSyncPhotos() -> Bool {
        isConnected && !isExpensive && !isConstrained
    }

    deinit {
        monitor.cancel()
    }
}
