import Foundation

/// The connection word the offline gate reads, as a feed a non-view consumer
/// can follow — today the feed manager's offline outbox
/// (`account-offline-mutation.md` § The offline-mutation contract): the first
/// word handed to `FfiFeedManager.setConnectionState` wires the device's
/// outbox, so a like tapped with no nest is kept in the account store and sent
/// when the nest is back, and a control the gate keeps live is exactly a control
/// whose gesture queues.
///
/// `FaunaClient.startConnectionStateObserver` publishes every transport state
/// it receives, spelled by the shared `connectionStateWord` — never a Swift
/// mapping — the same word `FaunaOfflineGate` reads. The twin of android's
/// `FeedManagerHost` collecting `ApiClient.connectionState`; one implementation
/// for macOS and iOS.
@MainActor
public final class ConnectionWordFeed {
    public static let shared = ConnectionWordFeed()

    /// The latest word — `connecting` until the first transport state arrives,
    /// the value `FaunaClient.connectionState` starts at.
    public private(set) var word: String = connectionStateWord(state: .connecting)

    private var followers: [UUID: (String) -> Void] = [:]

    private init() {}

    /// A transport state the session client received.
    public func publish(_ state: FfiConnectionState) {
        let next = connectionStateWord(state: state)
        word = next
        for follower in followers.values { follower(next) }
    }

    /// Call `onWord` with the current word now and with every later one, until
    /// the returned token is cancelled or released.
    public func follow(_ onWord: @escaping (String) -> Void) -> Following {
        let id = UUID()
        followers[id] = onWord
        onWord(word)
        return Following { [weak self] in self?.followers.removeValue(forKey: id) }
    }

    /// A follow's handle: cancelling it (or releasing it) stops the calls.
    public final class Following {
        private var stop: (@MainActor () -> Void)?

        init(_ stop: @escaping @MainActor () -> Void) { self.stop = stop }

        @MainActor public func cancel() {
            stop?()
            stop = nil
        }

        deinit {
            guard let stop else { return }
            Task { @MainActor in stop() }
        }
    }
}
