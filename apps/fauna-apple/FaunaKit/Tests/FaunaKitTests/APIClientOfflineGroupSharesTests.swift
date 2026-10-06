import Foundation
import Testing
@testable import FaunaKit

// `loadOfflineGroupShares` (`APIClient.swift`) builds an uncached
// `FfiNestClient` per call whenever `nestClient` is nil, so a run against a
// down nest used to leave one reconnect supervisor per call redialling with
// the actor's secret forever: neither `FfiNestClient` nor `NestClient` has a
// `Drop` impl (`libs/fauna-client/src/client.rs:382-386`, the class behind
// the 2026-08-22 mac incident that accumulated ~16,300 sockets), so dropping
// the reference the method never stored anywhere did nothing to stop it.
// Fixed by disconnecting the ephemeral client once the read returns.
//
// This target dials no real socket — every FFI-touching build here is a real
// network client with no bare Swift initializer, and `ActorScopeTests` /
// `APIClientActorAdoptionTests` are deliberately synchronous for the same
// reason (see that file's header). A real repro needs a supervisor that
// spawns but never reaches `Connected` (a live-but-silent listener), which
// belongs in a heavier tier than this fast unit target; `disconnect()`
// itself stopping the supervisor is already pinned at the Rust layer
// (`client.rs`'s `connect_twice_does_not_strand_the_first_supervisor`,
// `disconnect_is_visible_to_a_subscriber_that_attaches_afterwards`). What
// this test pins STRUCTURALLY is the Swift-side half those Rust tests can't
// see: that `loadOfflineGroupShares` actually calls `disconnect()` on the
// client it built, and never on one it merely reused from the cache.

private let apiClientSourceURL = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // FaunaKitTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // FaunaKit
    .appendingPathComponent("Sources/FaunaKit/Core/APIClient.swift")
    .standardizedFileURL

@Test("loadOfflineGroupShares disconnects the ephemeral client it built, but never a cached one it reused")
func loadOfflineGroupSharesDisconnectsOnlyTheEphemeralClient() throws {
    let source = try String(contentsOf: apiClientSourceURL, encoding: .utf8)
    let start = try #require(
        source.range(of: "public func loadOfflineGroupShares(seat: FfiCeremonySeat? = nil) async -> FfiGroupShareViews {"),
        "loadOfflineGroupShares's signature moved or was renamed — update this test's anchor"
    )
    let end = try #require(
        source.range(of: "\n    #if DEBUG", range: start.upperBound..<source.endIndex),
        "loadOfflineGroupShares no longer ends where this test expects — update this test's anchor"
    )
    let body = source[start.lowerBound..<end.lowerBound]

    let cachedBranch = try #require(
        body.range(of: "if let cached = nestClient {"),
        "the cached-client branch moved — update this test's anchor"
    )
    let uncachedBranch = try #require(
        body.range(of: "} else {", range: cachedBranch.upperBound..<body.endIndex),
        "the uncached-client branch moved — update this test's anchor"
    )
    let readCall = try #require(
        body.range(of: "try? await offlineShareLoadGroupShares(", range: uncachedBranch.upperBound..<body.endIndex),
        "the group-share read call moved — update this test's anchor"
    )

    let cachedBranchBody = body[cachedBranch.upperBound..<uncachedBranch.lowerBound]
    #expect(!cachedBranchBody.contains("disconnect()"),
            "a cached client came from ensureNestConnected()'s shared cache — disconnecting it here would tear down every other in-flight use of the same actor's WS-RPC connection")

    let disconnectCall = try #require(
        body.range(of: "ephemeral.disconnect()", range: readCall.upperBound..<body.endIndex),
        "loadOfflineGroupShares no longer disconnects the client it built — a fresh FfiNestClient's reconnect supervisor is never stopped by dropping the reference (no Drop impl), so an unstored ephemeral client redials with the actor's secret for as long as the object lives"
    )
    #expect(readCall.upperBound < disconnectCall.lowerBound,
            "the ephemeral client must be disconnected only AFTER the read returns, not before — disconnecting first would fail the very read it was built for")
}
