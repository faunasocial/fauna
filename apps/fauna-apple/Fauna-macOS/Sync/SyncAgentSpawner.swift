import Foundation
import FaunaKit

/// The macOS `FfiAgentSpawner`: ensure the per-user `social.fauna.sync-agent`
/// LaunchAgent is installed and running (`sync-agent.md` § Packaging + lifecycle).
/// Called from the Rust convergence loop's probe step (a background tokio thread —
/// never the main actor) whenever the agent socket is absent; best-effort — a
/// failed spawn just means the next tick retries.
///
/// Two install channels converge here: the `.pkg` ships the binary at
/// `/usr/local/bin/fauna-sync-agent` with its own enabled LaunchAgent plist, while
/// a `.dmg`-only install has no installer step — so this spawner **self-installs**
/// the plist on first need (a `(plist as NSDictionary).write` plus a
/// `launchctl bootstrap` — unlike the app's own auto-start, which is a
/// bundle-shipped plist registered through `SMAppService`, `AutoStart.swift`),
/// pointing at the first agent binary it can resolve: one bundled next to the
/// app executable, else the `.pkg` path.
final class LaunchdSyncAgentSpawner: FfiAgentSpawner, @unchecked Sendable {
    static let label = AppleIdentifiers.syncAgentLaunchAgent

    private static var plistURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/LaunchAgents/\(label).plist")
    }

    /// The agent binary this machine can run: bundled beside the app executable
    /// (`Fauna.app/Contents/MacOS/fauna-sync-agent`, and equally a debug build's
    /// sibling binary), else the `.pkg` install path. `nil` when neither exists —
    /// nothing to spawn (logged; the tick retries after an install).
    private static func resolveAgentBinary() -> String? {
        var candidates: [String] = []
        if let executable = Bundle.main.executableURL {
            candidates.append(
                executable.deletingLastPathComponent()
                    .appendingPathComponent("fauna-sync-agent").path)
        }
        candidates.append("/usr/local/bin/fauna-sync-agent")
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0) }
    }

    func spawnAgent() {
        guard let binary = Self.resolveAgentBinary() else {
            logMessage(
                level: .warn, target: "fauna.sync",
                message: "[sync-agent] no agent binary found (bundle or /usr/local/bin) — cannot spawn")
            return
        }

        // Self-install / heal the plist when missing, pointing at a different
        // binary (an app moved out of quarantine, a .pkg→bundle switch), or
        // still carrying an older `.pkg`'s stdout/stderr redirect — the agent
        // writes its own size-capped log, and launchd never rotates a redirect
        // (one reached 6.6 GB). A healed plist takes effect at the job's next
        // bootstrap (login, or this spawner's next run).
        let plistURL = Self.plistURL
        let desired: [String: Any] = [
            "Label": Self.label,
            "ProgramArguments": [binary],
            "RunAtLoad": true,
            "KeepAlive": true,
        ]
        let existing = NSDictionary(contentsOf: plistURL)
        let redirects = existing?["StandardOutPath"] != nil
            || existing?["StandardErrorPath"] != nil
        if (existing?["ProgramArguments"] as? [String]) != [binary] || redirects {
            try? FileManager.default.createDirectory(
                at: plistURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            (desired as NSDictionary).write(to: plistURL, atomically: true)
        }

        // `bootstrap` errors when the job is already loaded — that's the common
        // steady-state, so ignore it and let `kickstart` start a stopped job.
        let domain = "gui/\(getuid())"
        _ = Self.launchctl(["bootstrap", domain, plistURL.path])
        let status = Self.launchctl(["kickstart", "\(domain)/\(Self.label)"])
        if status != 0 {
            logMessage(
                level: .warn, target: "fauna.sync",
                message: "[sync-agent] launchctl kickstart exited \(status)")
        }
    }

    private static func launchctl(_ arguments: [String]) -> Int32 {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        process.arguments = arguments
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
        } catch {
            logMessage(
                level: .warn, target: "fauna.sync",
                message: "[sync-agent] launchctl \(arguments.first ?? "") failed to run: \(error)")
            return -1
        }
        process.waitUntilExit()
        return process.terminationStatus
    }
}
