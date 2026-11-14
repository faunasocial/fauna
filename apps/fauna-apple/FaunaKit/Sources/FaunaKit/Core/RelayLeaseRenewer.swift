import Foundation

/// Where a [RelayLeaseRenewer]'s grants and revokes go: on iOS, every
/// on-demand domain of the active account, across the File Provider
/// extension's relay-lease door (`RelayLeaseDoors`).
public protocol RelayLeaseDoorSet: Sendable {
    /// Grant or renew the lease on every domain's seat.
    func grantAll() async
    /// End the lease on every domain's seat now.
    func revokeAll() async
}

/// The app's foreground lease on its File Provider extension's relay seat
/// (`relay-serving.md` § Relay serving → *Apple's File Provider replica*,
/// point (2)): while the app is in front it grants the lease and renews it
/// every renewal period; at the background edge it stops renewing and revokes,
/// so a backgrounded phone serves nothing. A renewal that never reaches the
/// extension — the app killed or suspended first — lapses on the Rust side
/// within the lease's length.
///
/// Not part of the share plane: relay serving is not excisable
/// (`relay-serving.md` § Relay serving → *The seat serves from the file*), so
/// this sits outside `FAUNA_EXCISE_P2P_SHARE`. The lease's length and
/// renewal period are Rust constants; the period reaches here as `period`.
public actor RelayLeaseRenewer {
    /// Waits `period` between renewals — `Task.sleep` in production, a
    /// driven clock in the test.
    public typealias Sleep = @Sendable (Duration) async throws -> Void

    private let period: Duration
    private let sleep: Sleep
    private var loop: Task<Void, Never>?
    private var doors: RelayLeaseDoorSet?

    public init(
        period: Duration,
        sleep: @escaping Sleep = { try await Task.sleep(for: $0) }
    ) {
        self.period = period
        self.sleep = sleep
    }

    /// Whether a renewal loop runs now.
    public var isRenewing: Bool { loop != nil }

    /// The foreground edge: grant now and renew every period over `doors`.
    /// Starting again replaces a running loop (the start and resume edges
    /// may both fire).
    public func start(doors: RelayLeaseDoorSet) {
        loop?.cancel()
        self.doors = doors
        let (period, sleep) = (self.period, self.sleep)
        loop = Task {
            while !Task.isCancelled {
                await doors.grantAll()
                do { try await sleep(period) } catch { return }
            }
        }
    }

    /// The background edge: stop renewing and revoke the lease, so the seat
    /// withdraws at once. A no-op with no loop running.
    public func stop() async {
        guard let loop else { return }
        loop.cancel()
        self.loop = nil
        let doors = self.doors
        self.doors = nil
        // A grant in flight lands before the revoke, never after it (a late
        // grant would hold the seat for a whole lease in the background).
        await loop.value
        await doors?.revokeAll()
    }
}
