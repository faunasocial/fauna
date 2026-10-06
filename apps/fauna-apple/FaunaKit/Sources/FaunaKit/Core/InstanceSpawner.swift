#if os(macOS)
import AppKit
import Foundation

/// Spawn a NEW app instance bound to a chosen account — the running
/// instance's concurrent-instances affordance (`account-scoping.md`
/// § Concurrent instances; ui.yaml `account-open-new-instance-button`).
///
/// The chosen account travels as `FAUNA_BOUND_ACCOUNT` in the child's
/// environment (bucket-1 launch wiring — the one channel all apps use).
/// The child owns its whole binding outcome: unknown account → terminal
/// refusal; `require_confirm` account → the child's native re-auth prompt;
/// already-served account → refused by the per-account instance lock. The
/// spawner deliberately validates nothing — `bind_account` in the child is
/// the single gate.
///
/// macOS-only by construction (iOS admits one instance per app). Two launch
/// paths, chosen by how THIS process is packaged:
/// - a real `.app` bundle spawns via `NSWorkspace` with
///   `createsNewApplicationInstance` — the sanctioned way past
///   LaunchServices' per-bundle single-instance behaviour;
/// - a bare binary (dev / e2e — the e2e driver always launches bare) spawns
///   its own executable directly.
public enum InstanceSpawner {
    /// Spawn records for the e2e state protocol (`spawned_instances`): the
    /// harness reads the child's `agent_port` here and drives/observes the
    /// child over its own automation server — the parent's port must never
    /// be inherited (two servers can't share one port).
    private static let records = Locked<[[String: Any]]>([])

    /// Snapshot for the state protocol. Each record: `actor_id`, plus
    /// `agent_port` (e2e runs only) and `pid` (bare-binary spawns only).
    public static func stateRecords() -> [[String: Any]] {
        records.withLock { $0 }
    }

    public static func openNewInstance(actorIdHex: String) {
        var extraEnv: [String: String] = [
            fauna_bound_account_env: actorIdHex
        ]
        var record: [String: Any] = ["actor_id": actorIdHex]

        // Under e2e, allocate the child its OWN automation port: the agent
        // server binds the port exactly once per process, and the env is
        // otherwise inherited wholesale (same credential dir, same relocated
        // HOME — the child must share this instance's install world).
        //
        // `#if DEBUG`, because handing a child an automation port is what makes
        // that child drivable — automation surface, and convention 15 excludes it
        // from the artifact rather than gating it at runtime
        // (`e2e-automation-surface-gating.md` § The convention).
        #if DEBUG
        if E2eEnv.agentPort != nil, let port = allocateFreePort() {
            extraEnv[E2eEnv.agentPortName] = String(port)
            record["agent_port"] = Int(port)
        }
        #endif

        let bundleURL = Bundle.main.bundleURL
        if bundleURL.pathExtension == "app" {
            let config = NSWorkspace.OpenConfiguration()
            config.createsNewApplicationInstance = true
            config.environment = extraEnv
            NSWorkspace.shared.openApplication(at: bundleURL, configuration: config) { app, error in
                if let error {
                    logMessage(
                        level: .error, target: "fauna.accounts",
                        message: "[instance-spawn] NSWorkspace launch failed for \(actorIdHex): \(error)")
                    return
                }
                var completed = record
                if let pid = app?.processIdentifier { completed["pid"] = Int(pid) }
                records.withLock { $0.append(completed) }
            }
            return
        }

        // Bare binary: spawn our own executable with the merged environment.
        let exePath = Bundle.main.executablePath ?? CommandLine.arguments[0]
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: exePath)
        var childEnv = ProcessInfo.processInfo.environment
        for (k, v) in extraEnv { childEnv[k] = v }
        proc.environment = childEnv
        do {
            try proc.run()
            record["pid"] = Int(proc.processIdentifier)
            records.withLock { $0.append(record) }
        } catch {
            logMessage(
                level: .error, target: "fauna.accounts",
                message: "[instance-spawn] bare-binary launch failed for \(actorIdHex): \(error)")
        }
    }

    /// The launch-wiring env key (`fauna_client_accounts::BOUND_ACCOUNT_ENV`,
    /// mirrored — the FFI exposes the *read* (`requestedBoundAccount`), not
    /// the constant).
    private static let fauna_bound_account_env = "FAUNA_BOUND_ACCOUNT"

    /// Bind-to-port-0 free-port allocation (the same trick the harness's
    /// `find_free_port` uses). Racy in principle; the window is milliseconds
    /// and e2e-only.
    private static func allocateFreePort() -> UInt16? {
        let sock = socket(AF_INET, SOCK_STREAM, 0)
        guard sock >= 0 else { return nil }
        defer { close(sock) }
        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        addr.sin_port = 0
        let bindResult = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(sock, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bindResult == 0 else { return nil }
        var bound = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let nameResult = withUnsafeMutablePointer(to: &bound) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                getsockname(sock, $0, &len)
            }
        }
        guard nameResult == 0 else { return nil }
        return UInt16(bigEndian: bound.sin_port)
    }
}
#endif
