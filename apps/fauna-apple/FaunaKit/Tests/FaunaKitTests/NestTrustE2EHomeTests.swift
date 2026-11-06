#if os(macOS)
import Testing
import Foundation
@testable import FaunaKit

/// The macOS e2e launch's pin-store home (transport-trust.md § Pin custody across
/// processes, rule 1): the app's writer store and the private sync agent it spawns
/// (no `--data-dir`) must resolve the SAME dir — shared Rust's
/// `install_scoped_trust_home()` inside the launch's relocated `HOME`.
@Suite(.serialized)
struct NestTrustE2EHomeTests {
    @Test func e2eTrustDirIsTheAgentsInstallScopedHome() throws {
        let home = FileManager.default.temporaryDirectory
            .appendingPathComponent("nest-trust-e2e-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }

        let saved = getenv("HOME").map { String(cString: $0) }
        setenv("HOME", home.path, 1)
        defer {
            if let saved { setenv("HOME", saved, 1) } else { unsetenv("HOME") }
        }

        let dir = try NestTrust.e2eTrustDir()
        #expect(dir == (try installScopedTrustHome()))
        #expect(dir.hasSuffix("Library/Application Support/Fauna/trust"))
    }
}
#endif
