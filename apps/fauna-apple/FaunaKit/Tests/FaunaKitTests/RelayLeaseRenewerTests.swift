import Foundation
import Testing

@testable import FaunaKit

/// The app's foreground lease loop (`relay-serving.md` § Relay serving →
/// *Apple's File Provider replica*, point (2)): it grants at the foreground
/// edge, renews every period, and at the background edge stops and revokes.
@Suite struct RelayLeaseRenewerTests {
    /// The doors as the renewer used them.
    actor Doors: RelayLeaseDoorSet {
        private(set) var grants = 0
        private(set) var revokes = 0
        /// Grants and revokes in the order they were sent.
        private(set) var calls: [String] = []

        func grantAll() async {
            grants += 1
            calls.append("grant")
        }

        func revokeAll() async {
            revokes += 1
            calls.append("revoke")
        }
    }

    /// A clock the test drives: each `sleep` waits until `tick()` releases
    /// it, so a renewal happens only when the test says a period passed.
    actor Clock {
        private var waiters: [CheckedContinuation<Void, Error>] = []

        var sleeping: Int { waiters.count }

        func sleep() async throws {
            try await withCheckedThrowingContinuation { waiters.append($0) }
            try Task.checkCancellation()
        }

        /// One period passes: every sleeper wakes.
        func tick() {
            let woken = waiters
            waiters = []
            for waiter in woken { waiter.resume() }
        }

        /// A cancelled loop's sleeper wakes with cancellation.
        func cancelAll() {
            let woken = waiters
            waiters = []
            for waiter in woken { waiter.resume(throwing: CancellationError()) }
        }
    }

    /// Wait (bounded) until `condition` holds — the loop runs on its own task.
    private func eventually(_ condition: () async -> Bool) async -> Bool {
        for _ in 0..<2000 {
            if await condition() { return true }
            await Task.yield()
        }
        return await condition()
    }

    @Test func the_loop_grants_at_the_foreground_edge_and_renews_each_period() async {
        let (doors, clock) = (Doors(), Clock())
        let renewer = RelayLeaseRenewer(period: .seconds(10)) { _ in try await clock.sleep() }
        await renewer.start(doors: doors)
        #expect(await eventually { await doors.grants == 1 })
        #expect(await eventually { await clock.sleeping == 1 })
        await clock.tick()
        #expect(await eventually { await doors.grants == 2 })
        #expect(await eventually { await clock.sleeping == 1 })
        await clock.tick()
        #expect(await eventually { await doors.grants == 3 })
        #expect(await renewer.isRenewing)
        #expect(await doors.revokes == 0)
    }

    @Test func the_background_edge_stops_the_loop_and_revokes() async {
        let (doors, clock) = (Doors(), Clock())
        let renewer = RelayLeaseRenewer(period: .seconds(10)) { _ in try await clock.sleep() }
        await renewer.start(doors: doors)
        #expect(await eventually { await clock.sleeping == 1 })
        // The loop's sleep is cancelled by the stop; a driven clock wakes it.
        let stopping = Task { await renewer.stop() }
        #expect(await eventually { await clock.sleeping == 1 })
        await clock.cancelAll()
        await stopping.value
        #expect(await doors.revokes == 1)
        #expect(await doors.calls.last == "revoke")
        #expect(!(await renewer.isRenewing))
        // No renewal after the revoke, however long the app stays out of front.
        await clock.tick()
        for _ in 0..<50 { await Task.yield() }
        #expect(await doors.grants == 1)
    }

    @Test func a_stop_with_no_loop_running_revokes_nothing() async {
        let doors = Doors()
        let renewer = RelayLeaseRenewer(period: .seconds(10)) { _ in }
        await renewer.stop()
        #expect(await doors.revokes == 0)
    }

    @Test func the_production_sleep_is_cancelled_by_a_stop() async {
        let doors = Doors()
        let renewer = RelayLeaseRenewer(period: .seconds(3600))
        await renewer.start(doors: doors)
        #expect(await eventually { await doors.grants == 1 })
        await renewer.stop()
        #expect(await doors.revokes == 1)
    }
}
