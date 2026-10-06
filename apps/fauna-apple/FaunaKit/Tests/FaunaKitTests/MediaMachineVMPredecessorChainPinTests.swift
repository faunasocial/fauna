import Foundation
import Testing

// Call-site pin: the Media machine `MediaMachineVM.configure` builds is handed the
// attested predecessor ids AND the paired predecessor chain, as tui, linux, web and
// android hand them (`writer-signed-change-records.md`, rulings (8)(b) and (8)(c)).
//
// Without the ids the listing's judge must prove the succession link by a nest
// lookup per unplaced signed actor; without the PAIRED chain a key arrives bare and
// never opens a row a predecessor signed — the inherited corpus lists but stays
// sealed. The machine itself is exercised in
// `libs/fauna-media-machine/tests/media_lifecycle.rs`; this pins only that the seat
// makes the calls, so removing either line turns it red. A source pin, like
// `APIClientActorAdoptionTests`: no Media-machine runtime fixture can observe which
// setters a Rust machine was handed.

private let faunaKitSources = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // FaunaKitTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // FaunaKit
    .appendingPathComponent("Sources/FaunaKit")
    .standardizedFileURL

/// A source file's CODE lines — comments may name the setters to explain them.
private func codeLines(_ relativePath: String) throws -> String {
    let url = faunaKitSources.appendingPathComponent(relativePath)
    let text = try String(contentsOf: url, encoding: .utf8)
    return text.split(separator: "\n", omittingEmptySubsequences: false)
        .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
        .joined(separator: "\n")
}

@Suite("MediaMachineVM hands the machine the predecessor chain")
struct MediaMachineVMPredecessorChainPinTests {
    @Test("the machine is handed the attested predecessor ids")
    func theMachineIsHandedTheAttestedPredecessorIds() throws {
        let vm = try codeLines("ViewModels/MediaMachineVM.swift")
        #expect(vm.contains("built.setPredecessorActorIds(ids: predecessors.attestedActorIds)"))
        let client = try codeLines("Core/FaunaClient.swift")
        #expect(client.contains("attestedActorIds: resolvedAttestedPredecessorActorIds()"))
    }

    @Test("the machine is handed the paired predecessor chain")
    func theMachineIsHandedThePairedPredecessorChain() throws {
        let vm = try codeLines("ViewModels/MediaMachineVM.swift")
        #expect(vm.contains(
            "built.setPredecessorChain(actorIds: predecessors.chainActorIds, keys: predecessors.chainKeys)"))
        let client = try codeLines("Core/FaunaClient.swift")
        #expect(client.contains("FaunaAccounts.registry().predecessorChain(actorId: actor)"))
    }
}
