import Testing
@testable import FaunaMacOSLib

// macOS must await the sync agent's un-provision reply — the agent's receipt
// that its own account-store mount is down (`sync-agent.md` § Control plane
// split) — before the caller's erase runs. A
// spawned (fire-and-forget) un-provision is a race the erase can win; tui and
// linux already await it (`session::await_before_erase`, `sync_agent.rs`
// `teardown`), and `awaitSyncAgentUnprovision` is macOS's peer.
//
// The fake's `unprovision()` suspends until the test releases it, so the
// ordering is pinned deterministically rather than hoped for under load.

/// A `SyncAgentUnprovisioning` fake whose `unprovision()` suspends until
/// `release()` is called — mirrors `ScriptedEventsAPI`'s gate
/// (`EventsVMPublishOrderingTests.swift`), pared to the one call this seam has.
@MainActor
private final class GatedUnprovisioner: SyncAgentUnprovisioning, @unchecked Sendable {
    private(set) var callStarted = false
    private var continuation: CheckedContinuation<Void, Never>?
    private var released = false

    func unprovision() async throws {
        callStarted = true
        if released { return }
        await withCheckedContinuation { continuation = $0 }
    }

    func release() {
        released = true
        continuation?.resume()
        continuation = nil
    }
}

private actor OrderMarker {
    private(set) var order: [String] = []
    func append(_ step: String) { order.append(step) }
}

@MainActor
@Test("awaitSyncAgentUnprovision blocks the caller's erase until the un-provision reply arrives")
func awaitSyncAgentUnprovisionBlocksErase() async {
    let fake = GatedUnprovisioner()
    let marker = OrderMarker()

    let teardown = Task { @MainActor in
        await awaitSyncAgentUnprovision(fake)
        await marker.append("erase")
    }

    // Let the teardown task reach the gated `unprovision()` call before this
    // test proceeds, so the assertion below observes the real interleaving
    // rather than whichever the scheduler happens to pick.
    while !fake.callStarted { await Task.yield() }

    #expect(await marker.order.isEmpty,
            "the erase must not have run yet — the un-provision reply hasn't arrived")

    fake.release()
    await teardown.value

    #expect(await marker.order == ["erase"],
            "the erase must run only after the un-provision reply arrives")
}

@MainActor
@Test("awaitSyncAgentUnprovision returns (degrade open) when the un-provision call throws")
func awaitSyncAgentUnprovisionDegradesOpenOnFailure() async {
    final class ThrowingUnprovisioner: SyncAgentUnprovisioning, @unchecked Sendable {
        enum Failure: Error { case unreachable }
        func unprovision() async throws { throw Failure.unreachable }
    }

    // Must return (not throw/hang) so the caller's erase always proceeds —
    // an unreachable agent or a refusal degrades open, same as every other
    // teardown probe.
    await awaitSyncAgentUnprovision(ThrowingUnprovisioner())
}
