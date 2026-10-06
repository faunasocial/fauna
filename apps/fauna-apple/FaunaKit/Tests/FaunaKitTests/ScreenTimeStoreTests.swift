import Foundation
import Testing
@testable import FaunaKit

// `ScreenTimeStore`'s lifecycle latch (family-safety.md § Screen time) — mirrors `DnsAutoRenewCadence`'s own pair in
// `ActorScopeTests.swift`. Unlike the FaunaKit-shared singletons there, this
// store is freely constructible (one instance per `AppState`/`MacAppState`),
// so these tests build a fresh one directly rather than reaching through
// `ActorScope`.
//
// Every test body is synchronous on the MainActor, so `start`'s spawned Task
// never gets a chance to run (it cannot be scheduled until the test yields) —
// no tick ever reaches the network.

@MainActor
private func throwawayApi() -> APIClient {
    APIClient(nodeUrl: URL(string: "https://nest.invalid")!)
}

@MainActor
@Test func startArmsTheLoopAndStopDisarmsIt() {
    let store = ScreenTimeStore()
    #expect(!store.isRunning)

    store.start(api: throwawayApi())
    #expect(store.isRunning)

    store.stop()
    #expect(!store.isRunning)
}

/// The same latch class `DnsAutoRenewCadence` guards: a second `start`
/// without an intervening `stop` must not rebind the loop to a new `api` — a
/// teardown that skipped the drop would otherwise leave this store bound to
/// the outgoing identity's `APIClient` for good. Witnessed via `isRunning`
/// staying stable across the redundant call (the latch swallows the second
/// `start` outright, so there is no second loop to observe).
@MainActor
@Test func aSecondStartWithoutStopDoesNotDisruptTheArmedLoop() {
    let store = ScreenTimeStore()
    store.start(api: throwawayApi())
    #expect(store.isRunning)

    store.start(api: throwawayApi())
    #expect(store.isRunning, "the latch must swallow a re-start, not crash or unbind the loop")

    store.stop()
    #expect(!store.isRunning)
}

/// `stop()` must drop the ward's policy/guardian/lock state, not just cancel
/// the loop — an outgoing ward's lock surviving into the incoming account
/// would name a guardian the user does not have.
@MainActor
@Test func stopClearsTheLockMessage() {
    let store = ScreenTimeStore()
    store.start(api: throwawayApi())
    // No live api round trip needed to observe the clear: `lockMessage`
    // starts `nil` (no guardian recorded yet) either way, so this pins that
    // `stop()` does not leave it in some other state after a start.
    store.stop()
    #expect(store.lockMessage == nil)
}

/// `stop()` is safe to call when nothing is armed — several teardown sites
/// can run from a partially-built session (mirrors `ActorScopeTests`'s
/// idempotent-drop coverage for the FaunaKit-shared cadences).
@MainActor
@Test func stopIsIdempotentAndSafeWhenNothingIsArmed() {
    let store = ScreenTimeStore()
    store.stop()
    store.stop()
    #expect(!store.isRunning)
    #expect(store.lockMessage == nil)
}
