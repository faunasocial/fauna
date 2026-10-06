import Foundation
import Testing

@testable import FaunaKit

/// Headless pins for the domain-owner record (`FileProviderDomainOwners`) — the
/// cross-account guard behind the iOS drain-hold (`on-demand-files.md`
/// § Multi-account × File Provider, consequences 2+3): a dirty domain lingering across a switch
/// keeps its owner on record, so the extension refuses to serve it under the
/// incoming account and the removal gate reads the OWNER's scoped state. All
/// storage runs against an injected temp URL — never the real app-group
/// container (testing.md § conventions point 10).
@Suite struct FileProviderDomainOwnersTests {
    private func tempMapURL() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("fp-owners-\(UUID().uuidString)")
            .appendingPathComponent(FileProviderDomainOwners.filename)
    }

    @Test func recordOwnerClearRoundTrip() {
        let url = tempMapURL()
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }

        #expect(FileProviderDomainOwners.owner(domainId: "local:1", at: url) == nil,
                "absent map → no owner (served, backfilled by the next reconcile)")

        let a = String(repeating: "aa", count: 32)
        let b = String(repeating: "bb", count: 32)
        FileProviderDomainOwners.record(domainId: "local:1", actorIdHex: a, at: url)
        FileProviderDomainOwners.record(domainId: "local:2", actorIdHex: b, at: url)
        #expect(FileProviderDomainOwners.owner(domainId: "local:1", at: url) == a)
        #expect(FileProviderDomainOwners.owner(domainId: "local:2", at: url) == b)

        // The store stays an upsert (a later add supersedes a stale record);
        // whether a reconcile may write over an existing owner is the plan's
        // decision (`FileProviderReconcilePlanTests`), never the store's.
        FileProviderDomainOwners.record(domainId: "local:1", actorIdHex: b, at: url)
        #expect(FileProviderDomainOwners.owner(domainId: "local:1", at: url) == b)

        // Clearing one set leaves the others.
        FileProviderDomainOwners.clear(domainId: "local:1", at: url)
        #expect(FileProviderDomainOwners.owner(domainId: "local:1", at: url) == nil)
        #expect(FileProviderDomainOwners.owner(domainId: "local:2", at: url) == b)
    }

    @Test func ownerHexIsCaseNormalized() {
        let url = tempMapURL()
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }

        let upper = String(repeating: "AB", count: 32)
        FileProviderDomainOwners.record(domainId: "local:1", actorIdHex: upper, at: url)
        #expect(
            FileProviderDomainOwners.owner(domainId: "local:1", at: url) == upper.lowercased(),
            "stored lowercase so the extension's data_to_hex comparison can never miss on case")
    }

    @Test func corruptMapReadsAsEmpty() throws {
        let url = tempMapURL()
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("not json".utf8).write(to: url)
        #expect(FileProviderDomainOwners.owner(domainId: "local:1", at: url) == nil,
                "a corrupt map behaves like an absent owner record, never wedges the surface")
        FileProviderDomainOwners.record(
            domainId: "local:1", actorIdHex: String(repeating: "cc", count: 32), at: url)
        #expect(FileProviderDomainOwners.owner(domainId: "local:1", at: url) != nil,
                "recording over a corrupt map heals it")
    }
}
