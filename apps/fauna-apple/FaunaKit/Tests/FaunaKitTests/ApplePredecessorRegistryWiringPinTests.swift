import FaunaFFISwift
import Foundation
import Testing

@testable import FaunaKit

// Call-site pins: the sync-agent provisioner and the sync engine host are handed
// the account REGISTRY, and the app-dead File Provider host the PAIRED predecessor
// chain the app provisions beside `backup_key` (`writer-signed-change-records.md`,
// rulings (8)(b) source (ii) and (8)(c); `sync-agent-credentials.md` § Credential
// model). Without them a row a retired identity signed verifies by the statement
// walk but opens under no retired root. What Rust does with the registry or the
// chain is pinned in `libs/fauna-ffi` (`accounts_registry.rs`,
// `sync_engine_host.rs`); these pin only that the Swift seats make the calls —
// source pins, like `MediaMachineVMPredecessorChainPinTests`: no FaunaKit runtime
// fixture can observe which optional arguments a Rust face was handed.

private let faunaKitSources = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // FaunaKitTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // FaunaKit
    .appendingPathComponent("Sources/FaunaKit")
    .standardizedFileURL

/// A source file's CODE lines — comments may name the arguments to explain them.
private func codeLines(_ relativePath: String) throws -> String {
    let url = faunaKitSources.appendingPathComponent(relativePath)
    let text = try String(contentsOf: url, encoding: .utf8)
    return text.split(separator: "\n", omittingEmptySubsequences: false)
        .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
        .joined(separator: "\n")
}

/// The body of the one function whose signature line starts with `signature`,
/// up to its closing brace at the signature's own indentation — so a pin on
/// `APIClient.swift` names THIS method, not another one that passes the registry.
private func functionBody(_ source: String, _ signature: String) -> String? {
    guard let start = source.range(of: signature) else { return nil }
    let lineStart = source[..<start.lowerBound].lastIndex(of: "\n").map(source.index(after:))
        ?? source.startIndex
    let indent = String(source[lineStart..<start.lowerBound])
    guard let end = source.range(of: "\n\(indent)}\n", range: start.upperBound..<source.endIndex)
    else { return nil }
    return String(source[start.lowerBound..<end.upperBound])
}

@Suite("Apple seats hand Rust the registry and the paired predecessor chain")
struct ApplePredecessorRegistryWiringPinTests {
    @Test("the sync-agent provisioner is handed the registry beside the two lists")
    func theProvisionerIsHandedTheRegistry() throws {
        let api = try codeLines("Core/APIClient.swift")
        let body = try #require(functionBody(api, "public func syncAgentProvisioner("))
        #expect(body.contains("accounts: FaunaAccounts.registry()"))
        // The FFI face still takes both lists non-optionally; they stay.
        #expect(body.contains("predecessorBackupKeys: predecessorBackupKeys"))
        #expect(body.contains("predecessorActorIds: predecessorActorIds"))
    }

    @Test("the sync engine host is handed the registry")
    func theEngineHostIsHandedTheRegistry() throws {
        let api = try codeLines("Core/APIClient.swift")
        let body = try #require(functionBody(api, "public func syncEngineHost("))
        #expect(body.contains("accounts: FaunaAccounts.registry()"))
    }

    @Test("the File Provider provisioning writes the registry's paired chain")
    func theProvisioningWritesThePairedChain() throws {
        let coordinator = try codeLines("FileProvider/FileProviderDomainCoordinator.swift")
        let body = try #require(
            functionBody(coordinator, "private static func provision(_ ctx: FileProviderProvisioningContext)"))
        #expect(body.contains("FaunaAccounts.registry().predecessorChain(actorId: data_to_hex(actorId))"))
    }

    @Test("the app-dead host is handed the provisioned chain")
    func theAppDeadHostIsHandedTheChain() throws {
        let host = try codeLines("FileProvider/FileProviderHost.swift")
        let body = try #require(functionBody(host, "public func makeFileProviderHost(domainId: String)"))
        #expect(body.contains("predecessorChain: creds.predecessorChain"))
    }
}

/// The chain's two Keychain items round-trip through the store's pure codec, and
/// anything but a well-paired pair loads as no chain (the host then walks).
@Suite struct FileProviderPredecessorChainStoreTests {
    private let ids = [Data(repeating: 0x11, count: 32), Data(repeating: 0x22, count: 32)]
    private let keys = [Data(repeating: 0xA1, count: 32), Data(repeating: 0xB2, count: 32)]

    @Test func aPairedChainRoundTrips() {
        let chain = FfiPredecessorChain(actorIds: ids, keys: keys)
        let items = FileProviderCredentialStore.predecessorChainItems(chain)
        #expect(
            FileProviderCredentialStore.predecessorChain(actorIds: items.actorIds, keys: items.keys)
                == chain)
    }

    @Test func anEmptyChainRoundTripsAsEmpty() {
        let empty = FfiPredecessorChain(actorIds: [], keys: [])
        let items = FileProviderCredentialStore.predecessorChainItems(empty)
        #expect(
            FileProviderCredentialStore.predecessorChain(actorIds: items.actorIds, keys: items.keys)
                == empty)
    }

    /// A store written before the chain existed, or a crash between the two
    /// writes, is no chain — never ids paired with some other write's keys.
    @Test func aMissingOrMisshapenPairIsNoChain() {
        let items = FileProviderCredentialStore.predecessorChainItems(
            FfiPredecessorChain(actorIds: ids, keys: keys))
        #expect(FileProviderCredentialStore.predecessorChain(actorIds: nil, keys: nil) == nil)
        #expect(FileProviderCredentialStore.predecessorChain(actorIds: items.actorIds, keys: nil) == nil)
        #expect(FileProviderCredentialStore.predecessorChain(actorIds: nil, keys: items.keys) == nil)
        // One identity short of its key.
        #expect(
            FileProviderCredentialStore.predecessorChain(
                actorIds: items.actorIds.prefix(32), keys: items.keys) == nil)
        // Not a whole number of 32-byte entries.
        #expect(
            FileProviderCredentialStore.predecessorChain(
                actorIds: items.actorIds + Data([0x00]), keys: items.keys + Data([0x00])) == nil)
    }

    @Test func theCredentialsCarryTheChainAndDefaultToNone() {
        let chain = FfiPredecessorChain(actorIds: ids, keys: keys)
        let with = FileProviderCredentials(
            nestURL: "https://n", actorId: Data(count: 32), deviceId: Data(count: 32),
            deviceLabel: "d", backupKey: Data(count: 32), predecessorChain: chain)
        #expect(with.predecessorChain == chain)
        let without = FileProviderCredentials(
            nestURL: "https://n", actorId: Data(count: 32), deviceId: Data(count: 32),
            deviceLabel: "d", backupKey: Data(count: 32))
        #expect(without.predecessorChain == nil)
    }
}
