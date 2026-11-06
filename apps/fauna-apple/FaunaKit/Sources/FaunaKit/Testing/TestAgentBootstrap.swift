import Foundation

// Compiled out of release artifacts (testing.md convention 15) — neither
// function, nor the `TestAgent`/`InProcessAutomationServer` types they
// reach, exist in a non-DEBUG build.
#if DEBUG

/// Starts `TestAgent`/`InProcessAutomationServer` from the app shell's own
/// `serializeState`/`handleTestCommand` closures. Takes the two closures
/// directly (constructed at each call site, where `self` has its real
/// per-platform type) rather than `AppState`/`MacAppState` or a protocol —
/// neither function ever touched app state itself, only `self`-capturing
/// glue, so no shared type was needed at all.
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (priority #2) —
/// was a byte-identical per-target twin (differing only in doc-comment
/// prose) until this harvest pass found it .
public enum TestAgentBootstrap {
    /// What a launch broken by `FAUNA_E2E_FAULT_AT_LAUNCH=exit` exits with: the
    /// other apps' code (`fauna_e2e_agent::FAULT_AT_LAUNCH_EXIT_CODE`, windows'
    /// `E2eFaultAtLaunchExitCode`).
    private static let faultAtLaunchExitCode: Int32 = 73

    /// The forced launch failures of convention 6's launch-timeout rider, the
    /// twin of `fauna_e2e_agent::fault_at_launch` and windows' `App.xaml.cs`: right
    /// after the log is installed, `FAUNA_E2E_FAULT_AT_LAUNCH=exit` ends the launch
    /// and `block` parks the main thread for good, before the agent starts. A
    /// Swift twin because the apple apps do not link `fauna-e2e-agent`.
    public static func faultAtLaunchIfAsked() {
        switch E2eEnv.faultAtLaunch {
        case "exit":
            exit(faultAtLaunchExitCode)
        case "block":
            while true {
                sleep(3600)
            }
        default:
            break
        }
    }

    /// The forced death of convention 6's close rider, the twin of
    /// `fauna_e2e_agent::fault_at_reset`: `FAUNA_E2E_FAULT_AT_RESET=abort` crashes
    /// the app at the head of its next test-agent `reset` / `logout` — the death
    /// that lands between a test's last step and the harness's close, which the
    /// close must read as a crash (`tests/test_app_death_at_close_forced_apple.py`).
    /// A Swift `fatalError` (a trap, `SIGTRAP` on arm64), so its message lands in
    /// the crash report's `asi` — the one witness the iOS harness can read, since
    /// the simulator's console is not captured. Logged first, to stderr directly
    /// as well, because the shared log's file writer is non-blocking.
    public static func faultAtResetIfAsked() {
        guard E2eEnv.faultAtReset == "abort" else { return }
        let line = "[fauna] FATAL FAUNA_E2E_FAULT_AT_RESET=abort: crashing this launch at its reset on purpose"
        logMessage(level: .error, target: "fauna.e2e", message: line)
        FileHandle.standardError.write(Data((line + "\n").utf8))
        fatalError(line)
    }

    /// Gated on `FAUNA_E2E_BRIDGE` — the legacy cross-process XCUITest bridge.
    @MainActor
    public static func startTestAgentIfNeeded(
        stateProvider: @escaping () -> [String: Any],
        commandHandler: @escaping ([String: Any]) async -> Void
    ) {
        guard let bridgeUrl = E2eEnv.bridgeUrl else {
            return
        }
        TestAgent.shared.configure(stateProvider: stateProvider, commandHandler: commandHandler)
        TestAgent.shared.start(bridgeUrl: bridgeUrl)
    }

    /// Gated on `FAUNA_E2E_AGENT_PORT` — the in-process driver replacement.
    /// Starts the server UNBOUND: `/health` answers at once, `/app/state`
    /// answers 503 and commands are held until `bindInProcessAgent` hands it
    /// the rendered app's state-protocol callbacks — the same shape as
    /// `startTestAgentIfNeeded`'s, so the `/app/*` set_state path is
    /// identical; only the UI-driving (`/element/*`) differs (NSAccessibility
    /// in-process vs. cross-process XCUITest).
    @MainActor
    public static func startInProcessAgentIfNeeded() {
        guard let portStr = E2eEnv.agentPort,
              let port = UInt16(portStr) else {
            return
        }
        InProcessAutomationServer.shared.start(port: port)
        // `painted_errors`' feed reads the registry this driver makes live.
        E2eLoudSurfaces.installPaintedErrorObserver()
    }

    /// Bind the running in-process server to closures built from the
    /// RENDERED app value — call it from the scene's first `.task`, before
    /// `runLaunch()` on the same main-actor turn. The server is started from
    /// `App.init()` so `/health` answers before the first render, but an
    /// `App`'s `@State` storage is installed only once SwiftUI renders `body`:
    /// closures `init()` built read whatever `appState` that init saw, which
    /// the SDK does not promise is the instance the app then renders (measured
    /// 2026-10-26: under the Xcode 27.1 SDK the iOS app's agent reported a
    /// never-signed-in session while the rendered app was signed in). So no
    /// init-time closure is ever installed: a session patch a driver sent the
    /// moment a relaunch served a state used to land on that unrendered
    /// instance, leaving the rendered app with no client and no `connection`
    /// key. Inert without `FAUNA_E2E_AGENT_PORT`.
    @MainActor
    public static func bindInProcessAgent(
        stateProvider: @escaping () -> [String: Any],
        commandHandler: @escaping ([String: Any]) async -> Void
    ) {
        guard E2eEnv.agentPort != nil else { return }
        InProcessAutomationServer.shared.configure(stateProvider: stateProvider, commandHandler: commandHandler)
    }
}

#endif
