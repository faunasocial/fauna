import Foundation
import Testing

// Call-site pin: the Devices machine `APIClient.devicesMachine` builds is handed the
// registry's PAIRED predecessor chain, as windows' `BuildDevicesMachineAsync` hands it
// (`succession-aftermath.md` § Re-key scope, the `BackupKey` corpus row).
//
// A succession re-points an owned set to the successor and re-seals nothing, so a
// set's name still rests under the predecessor's owner root; without the chain the
// machine's label custody holds the session's own key only and the successor's
// Folders page drops every set it inherited. The machine itself is exercised in
// `fauna-devices-machine`'s
// `a_successors_inherited_set_lists_once_the_predecessor_chain_is_wired`; this pins
// only that the seat makes the call, so removing it turns this red. A source pin,
// like `MediaMachineVMPredecessorChainPinTests`.

private let faunaKitSources = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // FaunaKitTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // FaunaKit
    .appendingPathComponent("Sources/FaunaKit")
    .standardizedFileURL

/// A source file's CODE lines — comments may name the setter to explain it.
private func codeLines(_ relativePath: String) throws -> String {
    let url = faunaKitSources.appendingPathComponent(relativePath)
    let text = try String(contentsOf: url, encoding: .utf8)
    return text.split(separator: "\n", omittingEmptySubsequences: false)
        .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
        .joined(separator: "\n")
}

@Suite("APIClient hands the Devices machine the predecessor chain")
struct DevicesMachinePredecessorChainPinTests {
    @Test("the Devices machine is handed the paired predecessor chain")
    func theDevicesMachineIsHandedThePairedPredecessorChain() throws {
        let client = try codeLines("Core/APIClient.swift")
        #expect(client.contains("FaunaAccounts.registry().predecessorChain(actorId: actorId)"))
        #expect(client.contains(
            "machine.setPredecessorChain(actorIds: chain.actorIds, keys: chain.keys)"))
    }
}
