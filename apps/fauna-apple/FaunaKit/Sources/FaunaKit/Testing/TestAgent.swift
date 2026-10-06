import Foundation

// Compiled out of release artifacts (testing.md convention 15).
#if DEBUG

/// Test agent for E2E state protocol. Activated by FAUNA_E2E_BRIDGE env var.
/// Polls the bridge for commands and pushes app state back.
@MainActor
public class TestAgent {
    public static let shared = TestAgent()

    private var bridgeUrl: String = ""
    private var running = false
    private var lastCommandId: String = ""
    private var ready = true
    private var stateProvider: (() -> [String: Any])?
    private var commandHandler: (([String: Any]) async -> Void)?

    private init() {}

    /// Configure the agent with callbacks for state reading and command handling.
    public func configure(
        stateProvider: @escaping () -> [String: Any],
        commandHandler: @escaping ([String: Any]) async -> Void
    ) {
        self.stateProvider = stateProvider
        self.commandHandler = commandHandler
    }

    /// Start the polling loop. Call after configure().
    public func start(bridgeUrl: String) {
        self.bridgeUrl = bridgeUrl
        guard !running else { return }
        running = true
        NSLog("[TestAgent] Starting with bridge: %@", bridgeUrl)
        Task.detached { [weak self] in
            await self?.pollLoop()
        }
    }

    public func stop() {
        running = false
    }

    // MARK: - Polling

    private func pollLoop() async {
        var statePushCounter = 0
        while running {
            do {
                let command = try await fetchCommand()
                if let command {
                    ready = false
                    await processCommand(command)
                    ready = true
                    // Push state immediately after processing a command
                    pushState()
                } else {
                    // No command — push state every 5th cycle (~1 second at 200ms interval)
                    statePushCounter += 1
                    if statePushCounter >= 5 {
                        pushState()
                        statePushCounter = 0
                    }
                }
            } catch {
                NSLog("[TestAgent] Poll error: %@", String(describing: error))
                try? await Task.sleep(for: .seconds(1))
                continue
            }
            try? await Task.sleep(for: .milliseconds(200))
        }
    }

    // MARK: - HTTP

    private func fetchCommand() async throws -> [String: Any]? {
        let url = URL(string: "\(bridgeUrl)/app/commands")!
        let (data, response) = try await URLSession.shared.data(from: url)
        let httpResp = response as! HTTPURLResponse
        if httpResp.statusCode == 204 { return nil }
        guard httpResp.statusCode == 200 else { return nil }
        return try JSONSerialization.jsonObject(with: data) as? [String: Any]
    }

    @MainActor
    private func pushState() {
        guard let provider = stateProvider else { return }
        // Snapshot on MainActor (fast struct copies)
        let state = provider()
        let cmdId = lastCommandId
        let isReady = ready
        let url = bridgeUrl

        // JSON serialization + HTTP POST off-MainActor
        Task.detached {
            let payload: [String: Any] = [
                "last_command_id": cmdId,
                "ready": isReady,
                "state": state,
            ]
            guard let body = try? JSONSerialization.data(withJSONObject: payload) else { return }
            var request = URLRequest(url: URL(string: "\(url)/app/state")!)
            request.httpMethod = "POST"
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = body
            _ = try? await URLSession.shared.data(for: request)
        }
    }

    // MARK: - Command dispatch

    @MainActor
    private func processCommand(_ command: [String: Any]) async {
        let cmdId = command["id"] as? String ?? "unknown"
        let action = command["action"] as? String ?? "patch"
        lastCommandId = cmdId
        NSLog("[TestAgent] Processing command %@ (action: %@)", cmdId, action)

        switch action {
        case "patch":
            // Convention 11 names a BAD PAYLOAD alongside an unknown command:
            // `set_state` always sends a `state` dict (`http_bridge.py`), so its
            // absence is malformed, and dropping it silently made the patch's
            // effect simply not happen — indistinguishable downstream from the
            // product ignoring the state it was given.
            guard let state = command["state"] as? [String: Any] else {
                AppMessages.reportRefusedAgentCommand(
                    "patch: no `state` object in the command body")
                return
            }
            await commandHandler?(state)
        case "reset":
            await commandHandler?(["__action": "reset"])
        case "logout":
            await commandHandler?(["__action": "logout"])
        case "call_machine_method":
            // Per docs/goal/behavior/onboarding.md §"E2E bridge contract".
            // Dispatches a named OnboardingMachine
            // method with a JSON-serialized argument. Used by
            // tests/e2e-unified/drivers/machine_test_setter.py to fixture
            // wizard snapshots.
            guard let method = command["method"] as? String,
                  let jsonArg = command["json_arg"] as? String else {
                // Same bad-payload clause. A dropped machine call leaves the
                // wizard snapshot un-fixtured, and the test then fails on
                // whatever the un-fixtured wizard does next — a downstream read
                // that names neither this command nor the missing field.
                AppMessages.reportRefusedAgentCommand(
                    "call_machine_method: needs both `method` and `json_arg` strings")
                return
            }
            await commandHandler?([
                "__action": "call_machine_method",
                "method": method,
                "json_arg": jsonArg,
            ])
        default:
            // Forward any other action verbatim so the per-platform handler
            // can route it (e.g. the conversations test commands
            // conversations_inject_inbound / _create_mls_group /
            // _accept_recipient — see FaunaMacApp.handleTestCommand). The
            // handler reads command["__action"] plus the top-level fields the
            // Python action layer spread into the command body.
            var forwarded = command
            forwarded["__action"] = action
            await commandHandler?(forwarded)
        }
    }
}
#endif
