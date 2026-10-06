import Foundation
import Testing
@testable import FaunaKit

// A fix from the standing security review: `adoptActor` used to drop only the WS-RPC connection —
// every other actor-bound cache on `APIClient` (the Events-page caldav
// handle, the atproto settings machine, the conversations session, and the
// HTTP bearer) survived an in-place re-point, so a re-pointed client kept
// serving the outgoing actor's calendar, ATProto credentials, MLS session,
// and HTTP bearer. `account-scoping.md` § The scoping taxonomy's in-memory
// corollary: "one canonical drop … never a list per call site."
//
// The three FFI-opaque caches (`cachedSeededCaldavClient`,
// `cachedAtprotoMachine`, `conversationsSessionCache`/`Task`) cannot be
// populated here: each is built only through `ensureNestConnected()` →
// `FfiNestClient.connect()`, a real socket dial with no bare Swift
// initializer, and `ActorScopeTests` is deliberately synchronous so no test
// in this target ever dials one. What this file witnesses AT RUNTIME is the
// HTTP-bearer slice (`token`/`tokenExpiresAt`/`currentToken`) — real Swift
// types, no network needed — plus the `actorGeneration`/`sameActorSince()`
// seam every cache's post-await write relies on. The rest of `adoptActor`'s
// drop list is pinned STRUCTURALLY, the same split web's
// `actor-generation-contract.test.ts` makes for the identical class of
// hazard (an async singleton builder writing actor-scoped state after an
// await that a reset nil-ing the handle cannot stop).
//
@MainActor
private func throwawayApi() -> APIClient {
    APIClient(nodeUrl: URL(string: "https://nest.invalid")!)
}

/// `APIClient.swift`'s own source, for the structural test below — mirrors
/// `FaunaDeepLinkTests.uiAppexPlistDeclaresEveryAction`'s `#filePath`-relative
/// navigation to a sibling source file.
private let apiClientSourceURL = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // FaunaKitTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // FaunaKit
    .appendingPathComponent("Sources/FaunaKit/Core/APIClient.swift")
    .standardizedFileURL

/// Every actor-bound cache `adoptActor` must drop on a real switch — the one
/// list both the structural drop-list test and the async-builder census
/// below read, so the two can never silently name a different set.
private let actorBoundCacheNames = [
    "cachedSeededCaldavClient", "cachedAtprotoMachine",
    "conversationsSessionCache", "conversationsSessionTask",
    "token", "tokenExpiresAt", "currentToken",
]

@MainActor
@Test("adoptActor drops the outgoing actor's HTTP bearer on a real actor switch")
func adoptActorDropsTheHttpBearerOnASwitch() {
    let api = throwawayApi()
    api.adoptActor("actor-a-secret-hex")
    api.token = "actor-a-token"
    api.currentToken = "actor-a-token"
    api.tokenExpiresAt = Date().addingTimeInterval(3600)

    api.adoptActor("actor-b-secret-hex")

    #expect(api.currentToken == nil,
            "the outgoing actor's bearer must not be sent on the incoming actor's requests")
    #expect(api.token == nil)
    #expect(api.tokenExpiresAt == nil,
            "a surviving tokenExpiresAt lets ensureAuthenticated() skip re-authenticating as the new actor and keeps sending the outgoing actor's bearer until it expires")
}

@MainActor
@Test("adoptActor is a same-actor no-op — it must not re-clear a cache two back-to-back calls for the same incoming actor just populated")
func adoptActorSameActorNoOpPreservesCaches() {
    let api = throwawayApi()
    api.adoptActor("actor-a-secret-hex")
    api.token = "actor-a-token"
    api.currentToken = "actor-a-token"
    api.tokenExpiresAt = Date().addingTimeInterval(3600)

    // A second call for the SAME actor — e.g. two call sites (`feedManager`,
    // `conversationsSession`) both re-pointing before either's
    // `ensureNestConnected()` has re-set the WS-RPC-level actor — must be a
    // no-op, never a second drop of a cache the first call already
    // (re)populated.
    api.adoptActor("actor-a-secret-hex")

    #expect(api.currentToken == "actor-a-token")
    #expect(api.token == "actor-a-token")
    #expect(api.tokenExpiresAt != nil)
}

@MainActor
@Test("sameActorSince()'s predicate goes false exactly on a real switch, never on a same-actor no-op")
func sameActorSincePredicateTracksRealSwitchesOnly() {
    let api = throwawayApi()
    api.adoptActor("actor-a-secret-hex")
    let stillActorA = api.sameActorSince()
    #expect(stillActorA())

    api.adoptActor("actor-a-secret-hex")
    #expect(stillActorA(), "a same-actor no-op must not trip a build already in flight for actor A")

    api.adoptActor("actor-b-secret-hex")
    #expect(!stillActorA(),
            "a real switch must trip the predicate — this is what stops seededCaldavClient()/atprotoSettingsMachine()/conversationsSession() from writing actor A's already-in-flight build into the cache after the switch")
}

@Test("adoptActor's drop list still names every actor-bound cache (structural — a new cache must be added HERE, not at a call site)")
func adoptActorDropListNamesEveryActorBoundCache() throws {
    let source = try String(contentsOf: apiClientSourceURL, encoding: .utf8)
    let start = try #require(
        source.range(of: "func adoptActor(_ secretHex: String) {"),
        "adoptActor's signature moved or was renamed — update this test's anchor"
    )
    let end = try #require(
        source.range(of: "\n    private func ensureNestConnected()", range: start.upperBound..<source.endIndex),
        "ensureNestConnected no longer follows adoptActor's block — update this test's anchor"
    )
    let body = source[start.lowerBound..<end.lowerBound]
    for name in actorBoundCacheNames {
        #expect(body.contains("\(name) = nil"),
                "adoptActor no longer drops \(name) — a re-pointed client would keep serving the outgoing actor's data through it (account-scoping.md's in-memory corollary)")
    }
}

// MARK: - Adoption census
//
// The drop-list test above pins that `adoptActor` NILS every actor-bound
// cache. It says nothing about the other half of the hazard
// (`account-scoping.md`'s in-memory corollary): the three caches below are
// each populated by an ASYNC builder that writes its result after an
// `await`, which a reset that only nils the handle cannot stop — a switch
// landing mid-build must not let the outgoing actor's already-in-flight
// result land in the cache anyway. A later change added a
// `sameActorSince()`/`stillThisActor()` guard to each builder for exactly
// this; nothing here calls the builders (each dials a real
// `ensureNestConnected()` socket — this file's own header explains why no
// test in this target does that), so the guard's PRESENCE and ORDERING are
// pinned structurally instead — apple's twin of web's
// `actor-generation-contract.test.ts`, which makes the identical split for
// the identical hazard on its own three (now four) singleton builders.

/// The subset of `actorBoundCacheNames` that is FFI-opaque and populated by
/// a post-await builder carrying the `sameActorSince()`/`stillThisActor()`
/// guard — never a hand-picked list of its own: each key is asserted below
/// to be a member of `actorBoundCacheNames`, so a rename in the drop list
/// (or a name dropped from it) reds here too instead of the two lists
/// silently drifting apart. `token`/`tokenExpiresAt`/`currentToken` (the
/// HTTP-bearer trio) are deliberately absent — `adoptActor`/
/// `ensureAuthenticated` write them synchronously, never through a memoized
/// post-await builder, so they carry no entry in this census; the drop-list
/// test above already pins that they are dropped.
private struct GuardedBuilder {
    let builder: String
    let startAnchor: String
    let endAnchor: String
    /// The FULL capture statement, not the bare `sameActorSince()` call — each
    /// builder's own explanatory comment mentions `` `sameActorSince()` ``
    /// and "await" in prose ahead of the real code (e.g. "the only await
    /// here is …"), and a bare-substring search would match the comment,
    /// not the statement. The full statement text is specific enough that a
    /// comment coincidentally repeating it verbatim is not a realistic risk.
    let captureAnchor: String
    let awaitAnchor: String
    let guardAnchor: String
    let writeAnchor: String
}

private let asyncCacheBuilders: [String: GuardedBuilder] = [
    "cachedSeededCaldavClient": GuardedBuilder(
        builder: "seededCaldavClient",
        startAnchor: "private func seededCaldavClient() async throws -> FfiCaldavClient {",
        endAnchor: "public func queryEventsSeeded(calendarId: String) async throws -> [EventSummary]? {",
        captureAnchor: "let stillThisActor = sameActorSince()",
        awaitAnchor: "let client = try await caldavClient()",
        guardAnchor: "guard stillThisActor() else {",
        writeAnchor: "cachedSeededCaldavClient = client"
    ),
    "conversationsSessionCache": GuardedBuilder(
        builder: "conversationsSession",
        startAnchor: "public func conversationsSession(",
        endAnchor: "/// This build's `index`-lease seat",
        captureAnchor: "let stillThisActor = sameActorSince()",
        awaitAnchor: "let session = try await task.value",
        guardAnchor: "guard stillThisActor() else {",
        writeAnchor: "conversationsSessionCache = session"
    ),
    "cachedAtprotoMachine": GuardedBuilder(
        builder: "atprotoSettingsMachine",
        startAnchor: "public func atprotoSettingsMachine(observer: AtprotoSettingsObserver) async throws -> AtprotoSettingsMachine {",
        endAnchor: "// MARK: - Backup-destination management",
        captureAnchor: "let stillThisActor = sameActorSince()",
        awaitAnchor: "nest: try await ensureNestConnected()",
        guardAnchor: "guard stillThisActor() else {",
        writeAnchor: "cachedAtprotoMachine = machine"
    ),
]

@Test(
    "an async cache builder captures sameActorSince() before its await and re-checks stillThisActor() before writing the cache — never re-ordered",
    arguments: Array(asyncCacheBuilders.keys).sorted()
)
func asyncCacheBuilderGuardsThePostAwaitWrite(cacheName: String) throws {
    #expect(actorBoundCacheNames.contains(cacheName),
            "\(cacheName) is not in actorBoundCacheNames — the census and the drop-list test have drifted apart; both must name the same cache")

    let entry = try #require(asyncCacheBuilders[cacheName])
    let source = try String(contentsOf: apiClientSourceURL, encoding: .utf8)
    let start = try #require(
        source.range(of: entry.startAnchor),
        "\(entry.builder)'s signature moved or was renamed — update this test's anchor"
    )
    let end = try #require(
        source.range(of: entry.endAnchor, range: start.upperBound..<source.endIndex),
        "\(entry.builder) no longer ends where this test expects — update this test's anchor"
    )
    let body = String(source[start.lowerBound..<end.lowerBound])

    let captureRange = try #require(
        body.range(of: entry.captureAnchor),
        """
        \(entry.builder) no longer captures the actor generation as `\(entry.captureAnchor)` — \
        its cache write happens after an await, so it needs the identity seam \
        (account-scoping.md's in-memory corollary)
        """)
    let awaitRange = try #require(
        body.range(of: entry.awaitAnchor),
        "\(entry.builder) no longer awaits at `\(entry.awaitAnchor)` — update this test's anchor")
    #expect(captureRange.lowerBound < awaitRange.lowerBound,
            """
            \(entry.builder) captures the actor generation at or after its await — capture it on \
            the synchronous path, before the first await, or the check compares the \
            generation to itself
            """)

    let guardRange = try #require(
        body.range(of: entry.guardAnchor, range: awaitRange.upperBound..<body.endIndex),
        """
        \(entry.builder) captures the actor generation but never re-checks it (`\(entry.guardAnchor)`) \
        after its await, before writing the cache
        """)
    let writeRange = try #require(
        body.range(of: entry.writeAnchor),
        "\(entry.builder) no longer writes \(cacheName) at `\(entry.writeAnchor)` — update this test's anchor"
    )
    #expect(guardRange.upperBound < writeRange.lowerBound,
            """
            \(entry.builder) writes \(cacheName) before (or without) re-checking stillThisActor() — \
            exactly the defect class row 288 fixed: the outgoing actor's already-in-flight \
            build would land in the cache after a switch
            """)
}

// MARK: - The WS-RPC client is disconnected, never merely dropped
//
// `nestClient` is the one actor-bound handle whose drop needs more than a
// nil: a cached `FfiNestClient` always has a running reconnect supervisor
// (`ensureNestConnected` caches only after `connect()` succeeds), and
// dropping the Swift reference does not stop it — the supervisor holds its
// own `Arc`s, so it keeps redialling with the outgoing actor's secret
// (`libs/fauna-client/src/client.rs`'s `connect_twice_does_not_strand_the_first_supervisor`
// pins the Rust-side twin). No test here dials a real socket (this file's
// header), so the disconnect's presence and ordering are pinned
// structurally, like the census above.

private func sourceSlice(from startAnchor: String, to endAnchor: String) throws -> String {
    let source = try String(contentsOf: apiClientSourceURL, encoding: .utf8)
    let start = try #require(source.range(of: startAnchor),
                             "`\(startAnchor)` moved or was renamed — update this test's anchor")
    let end = try #require(source.range(of: endAnchor, range: start.upperBound..<source.endIndex),
                           "`\(endAnchor)` no longer follows `\(startAnchor)` — update this test's anchor")
    return String(source[start.lowerBound..<end.lowerBound])
}

@Test("adoptActor disconnects the outgoing WS-RPC client before dropping it on a real actor switch (structural)")
func adoptActorDisconnectsTheOutgoingNestClient() throws {
    let body = try sourceSlice(from: "func adoptActor(_ secretHex: String) {",
                               to: "\n    private var actorGeneration")
    let sameActorReturn = try #require(
        body.range(of: "if let nestClientSecret, nestClientSecret == secretHex { return }"),
        "adoptActor's same-actor early return moved — a same-actor no-op must never disconnect a client still in use")
    let disconnect = try #require(
        body.range(of: "Task.detached { await outgoing.disconnect() }"),
        "adoptActor no longer disconnects the outgoing nestClient — nil-ing it alone orphans its reconnect supervisor, which redials with the outgoing actor's secret forever")
    let drop = try #require(body.range(of: "nestClient = nil"))
    #expect(sameActorReturn.upperBound < disconnect.lowerBound,
            "the disconnect must sit after the same-actor early return, or a same-actor no-op tears down the live connection")
    #expect(disconnect.upperBound < drop.lowerBound,
            "the disconnect must capture the outgoing client before nestClient is nil-ed")
}

@Test("ensureNestConnected disconnects, never caches, a client whose actor changed or whose seat a concurrent connect already took (structural)")
func ensureNestConnectedNeverStrandsAClient() throws {
    let body = try sourceSlice(from: "private func ensureNestConnected() async throws -> FfiNestClient {",
                               to: "public func subscribeReconnects()")
    let capture = try #require(body.range(of: "let stillThisActor = sameActorSince()"),
                               "ensureNestConnected no longer captures the actor generation before connecting")
    let connect = try #require(body.range(of: "try await client.connect()"))
    let guardStmt = try #require(
        body.range(of: "guard stillThisActor() else {", range: connect.upperBound..<body.endIndex),
        "ensureNestConnected no longer re-checks the actor after connect() — a switch landing mid-connect caches the outgoing actor's client")
    let staleDisconnect = try #require(
        body.range(of: "await client.disconnect()", range: guardStmt.upperBound..<body.endIndex),
        "the stale-actor branch must disconnect the client it minted, not just throw — dropping it orphans its supervisor")
    let winner = try #require(
        body.range(of: "if let winner = nestClient {", range: staleDisconnect.upperBound..<body.endIndex),
        "ensureNestConnected no longer yields to a concurrently cached client — overwriting it orphans that client's supervisor")
    let loserDisconnect = try #require(
        body.range(of: "await client.disconnect()", range: winner.upperBound..<body.endIndex))
    let write = try #require(body.range(of: "nestClient = client"))
    #expect(capture.lowerBound < connect.lowerBound,
            "capture the actor generation before the connect() await, or the check compares the generation to itself")
    #expect(loserDisconnect.upperBound < write.lowerBound,
            "both guards must run before nestClient is written")
}
