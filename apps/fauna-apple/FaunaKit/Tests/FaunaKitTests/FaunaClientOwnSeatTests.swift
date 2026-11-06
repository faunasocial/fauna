import Foundation
import SwiftData
import Testing
@testable import FaunaKit

// A `FaunaClient` signs as the identity it was BUILT for, whatever account the
// registry's active pointer names (`account-scoping.md` § Concurrent instances →
// *The binding follows the account*).
//
// On a bound seat the active account can be one the seat does not serve.
// `start()` once authenticated from a store read of it (the retired single
// slot), and `authenticate` re-points the client (`APIClient.adoptActor`): a
// bound seat's post-succession client was
// re-pointed at the RETIRED actor and every call was refused
// `fauna.auth.superseded` — measured on macOS, where the journey
// `test_apple_a_bound_instance_survives_its_own_succession` timed out waiting
// for the successor's kit.
//
// Pinned at `authenticateOwnSeat()`, the auth step `start()` and `resume()`
// share: the rest of `start()` hosts the account runtime, which opens the real
// Keychain-backed registry and has no place in a unit test. No nest is needed
// either — `authenticate` adopts the secret synchronously BEFORE its network
// hop, so a refused connection still leaves the seat it chose readable.

private func inMemoryContext() -> ModelContext {
    let schema = Schema([PhotoBackupRecord.self])
    let config = ModelConfiguration(schema: schema, isStoredInMemoryOnly: true)
    return ModelContext(try! ModelContainer(for: schema, configurations: [config]))
}

@MainActor
@Test func startupAuthSignsAsTheClientsOwnIdentityNotTheActiveAccount() async throws {
    let own = String(repeating: "33", count: 32)
    let active = String(repeating: "44", count: 32)
    let keychain = KeychainStore(testBackend: FakeKeychainBackend())
    let registry = FfiAccountRegistry(store: KeychainSecretStore(keychain: keychain))
    try registry.setActive(
        actorId: try registry.addAccount(secretHex: active, nestUrl: nil, deviceId: nil))

    let client = FaunaClient(
        nodeUrl: URL(string: "http://127.0.0.1:1")!, secretHex: own,
        deviceId: String(repeating: "00", count: 32), modelContext: inMemoryContext(),
        keychain: keychain)
    let ownActor = try #require(client.ownActorIdHex)
    #expect(client.api.boundActorIdHex == ownActor, "construction must prime the client's own seat")

    // Refused by the port — the seat it adopted first is what is under test.
    try? await client.authenticateOwnSeat()

    #expect(client.api.boundActorIdHex == ownActor,
            "the startup auth re-pointed the seat at the active account's actor — a bound seat then signs as an account it does not serve")
}

// `start()` and `resume()` take the bearer mint through
// `mintOwnSeatBearerOrLeaveItToTheSupervisor`, which must NOT throw: a first
// mint that found no nest (an iOS launch whose optimistic shell started the
// session before the launch machine installed the reach hint) used to throw out
// of `start()` past every observer, so the connection indicator read
// `connecting` for good while the session's supervisor sat connected over the
// hint (`transport-connection.md` § Connection lifecycle, *A first mint that
// finds no nest still starts the supervisor*). The residual HTTP bearer stays
// unset, so `ensureAuthenticated` mints again on the next HTTP call.
@MainActor
@Test func aFailedStartupMintIsReportedNotThrown() async throws {
    let own = String(repeating: "35", count: 32)
    let keychain = KeychainStore(testBackend: FakeKeychainBackend())
    let client = FaunaClient(
        nodeUrl: URL(string: "http://127.0.0.1:1")!, secretHex: own,
        deviceId: String(repeating: "00", count: 32), modelContext: inMemoryContext(),
        keychain: keychain)

    // Refused by the port: no nest answers.
    let minted = await client.mintOwnSeatBearerOrLeaveItToTheSupervisor("start")

    #expect(!minted, "a refused mint must report itself as failed")
    #expect(client.api.currentToken == nil,
            "a failed mint must leave no bearer, so the next HTTP call mints again")
}
