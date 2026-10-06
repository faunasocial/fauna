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
    /// Shares the same state-protocol callbacks as `startTestAgentIfNeeded`,
    /// so the `/app/*` set_state path is identical; only the UI-driving
    /// (`/element/*`) differs (NSAccessibility in-process vs. cross-process
    /// XCUITest).
    @MainActor
    public static func startInProcessAgentIfNeeded(
        stateProvider: @escaping () -> [String: Any],
        commandHandler: @escaping ([String: Any]) async -> Void
    ) {
        guard let portStr = E2eEnv.agentPort,
              let port = UInt16(portStr) else {
            return
        }
        InProcessAutomationServer.shared.configure(stateProvider: stateProvider, commandHandler: commandHandler)
        InProcessAutomationServer.shared.start(port: port)
        // `painted_errors`' feed reads the registry this driver makes live.
        E2eLoudSurfaces.installPaintedErrorObserver()
    }
}

#endif
