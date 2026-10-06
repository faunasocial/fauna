import Testing
import Foundation
@testable import FaunaKit

// Pins `OpenURL.open`'s e2e-harness suppression, mirroring windows' `UrlOpener.cs`:
// under `FaunaE2E.isActive` the OS handoff is skipped and the URL is logged
// to the shared `fauna_log` ring instead, so a test can witness the seam
// without ever spawning a real browser (which would otherwise squat idle
// connections on e2e's `fake_cloud` fixture — e2e-conventions.md point 10).
//
// ⚠ Deliberately no case exercises the NON-suppressed branch: without
// `FaunaE2E.isActive` set, `OpenURL.open` really does call
// `NSWorkspace.shared.open` / `UIApplication.shared.open`, which would open a
// real browser on whatever machine runs `swift test`.
//
// `.serialized`: `logSnapshot()`/`logClear()` read/write the same
// process-wide ring every other log-emitting test can also touch.
@Suite("OpenURL e2e-harness suppression", .serialized)
struct OpenURLTests {
    /// `logMessage` is a no-op until the process-global `tracing` subscriber is
    /// installed (normally the app's own launch-time `installLogging` call,
    /// which no test target ever runs) — so the suite installs it once, into a
    /// throwaway temp dir, exactly like `FaunaMacApp`/`FaunaApp` do at launch.
    /// Idempotent (`fauna_log::init`'s contract): safe however many suites call it.
    private static let loggingInstalled: Void = {
        installLogging(dataDir: NSTemporaryDirectory())
    }()

    @Test func suppressesTheRealOpenAndLogsTheURLUnderTheE2EHarness() {
        _ = Self.loggingInstalled
        setenv("FAUNA_E2E_BRIDGE", "1", 1)
        defer { unsetenv("FAUNA_E2E_BRIDGE") }
        #expect(ProcessInfo.processInfo.environment["FAUNA_E2E_BRIDGE"] == "1",
                "setenv must be visible through ProcessInfo for FaunaE2E.isActive to see it")

        logClear()
        let url = URL(string: "https://example.test/verify?code=abc123")!
        OpenURL.open(url)

        let suppressed = logSnapshot().first { $0.target == "fauna.ui" && $0.message.contains(url.absoluteString) }
        #expect(suppressed != nil, "no fauna.ui log entry recorded the suppressed URL")
        #expect(suppressed?.message.contains("suppressed under the e2e harness") == true)
    }
}
