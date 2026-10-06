import Testing
import Foundation
@testable import FaunaKit

/// FaunaKit recipient resolution runs entirely through the shared UniFFI seam
/// (`classifyRecipient` + the anonymous `resolveNest`/`resolveHandle` discovery
/// kinds) — no Swift `64-hex` regex, `@`-split, or HTTP to the deleted
/// `/api/v1/{resolve-node,actor/by-handle}` twins remains (priority #2/#4).
/// Mirrors android `ResolveService` (`core/ResolveService.kt`).
///
/// The `actor_id` and `invalid` paths short-circuit in `classify_recipient`
/// before any network call, so they unit-test offline here; the cross-nest
/// `handle` path (`resolveNest` → `resolveHandle`, both anonymous WS-RPC) is
/// covered by tier_3 e2e against a live nest.

@Test func resolveRecipientClassifiesActorIdOffline() async throws {
    let api = APIClient(nodeUrl: URL(string: "https://nest.example")!)
    // Uppercase 64-hex → shared `classify_recipient` lowercases it; the
    // actor_id branch returns the local nest URL without any network call.
    let r = try await api.resolveRecipient(String(repeating: "A", count: 64))
    #expect(r.actorId == String(repeating: "a", count: 64))
    #expect(r.nodeUrl == "https://nest.example")
    #expect(r.handle == nil)
    #expect(r.domain == nil)
}

@Test func resolveRecipientRejectsGarbageOffline() async {
    let api = APIClient(nodeUrl: URL(string: "https://nest.example")!)
    // Not 64-hex and no `@` → `classify_recipient` returns "invalid" → throw,
    // matching android (which rejects rather than treating garbage as an id).
    await #expect(throws: (any Error).self) {
        _ = try await api.resolveRecipient("justtext")
    }
}
