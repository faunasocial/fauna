#if DEBUG && !FAUNA_KIDS
import Foundation
import Testing
@testable import FaunaKit

// The apple twin of tui's `loop_wake.rs` tests: the `auto_renew_wake` poke
// (`fauna_e2e_agent::AUTO_RENEW_WAKE`) ends `DnsAutoRenewCadence`'s wait, a wake
// that lands before the wait begins is kept for it, and a pre-auth poke (no loop
// armed) is refused loudly (convention 11). No assertion reads a clock
// (convention 14): every wait below is six hours, so it can only end by a wake
// or a cancellation, and the time limit is the hang guard, never the assertion.

private let sixHours: UInt64 = 6 * 60 * 60 * 1_000_000_000

@MainActor
@Test(.timeLimit(.minutes(1)))
func aWakeLandingBeforeTheWaitIsKeptForIt() async {
    let wake = CadenceWake()
    wake.wake()
    #expect(wake.hasPendingWake, "a wake with no wait pending was dropped, not kept")
    await wake.sleep(nanoseconds: sixHours)
    #expect(!wake.hasPendingWake, "the stored wake was not consumed by the wait it ended")
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func aWakeEndsAWaitAlreadyInProgress() async {
    let wake = CadenceWake()
    let waiter = Task { @MainActor in await wake.sleep(nanoseconds: sixHours) }
    // Let the waiter reach its suspension before the wake lands, so this pins
    // the mid-wait path rather than the stored-wake one above.
    while !wake.isWaitingForTest { await Task.yield() }
    wake.wake()
    await waiter.value
    #expect(!wake.hasPendingWake, "a mid-wait wake was stored instead of ending the wait")
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func cancellingTheWaitingTaskEndsTheWait() async {
    let wake = CadenceWake()
    let waiter = Task { @MainActor in await wake.sleep(nanoseconds: sixHours) }
    while !wake.isWaitingForTest { await Task.yield() }
    waiter.cancel()
    await waiter.value
}

/// Synchronous on the MainActor, like `ActorScopeTests`: the armed loop's task
/// cannot run before the body ends, and `stop` cancels it first, so no tick
/// ever reaches the network.
@MainActor
@Test func theCadenceRefusesAWakeWithNoLoopAndAcceptsOneWhileArmed() {
    DnsAutoRenewCadence.shared.stop()
    let refusal = DnsAutoRenewCadence.shared.wakeForTest()
    #expect(refusal?.contains("auto_renew_wake") == true,
            "a pre-auth wake was acked instead of refused: \(String(describing: refusal))")
    DnsAutoRenewCadence.shared.start(api: APIClient(nodeUrl: URL(string: "https://nest.invalid")!))
    #expect(DnsAutoRenewCadence.shared.wakeForTest() == nil, "a wake of the armed loop was refused")
    DnsAutoRenewCadence.shared.stop()
    #expect(DnsAutoRenewCadence.shared.wakeForTest() != nil,
            "the departed loop's wake still answers after stop")
}
#endif
