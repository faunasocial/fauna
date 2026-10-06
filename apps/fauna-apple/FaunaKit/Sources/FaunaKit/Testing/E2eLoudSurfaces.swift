import Foundation

/// The loud surfaces' four e2e seams on macOS + iOS — `fauna_e2e_agent::
/// {ALERT_SWEEP_WAKE, RECONNECT_BACKOFF, CONNECTION_REPORTS_KEY,
/// PAINTED_ERRORS_KEY}`, which own the cross-app contracts. Every one is a thin
/// call into `fauna-ffi`'s `test-helpers` exports (`e2e_seams.rs`,
/// `critical_alerts.rs`), so apple counts, parses and wakes with the same Rust
/// as tui, linux, web and windows (windows' `E2eLoudSurfaces.cs` is the sibling
/// leg). One implementation for both targets.
///
/// **A gated real plus a same-signature production twin** (convention 15), like
/// `E2eEnv`: the connection indicator's feed is production code
/// (`FaunaClient.startConnectionStateObserver`), and the wrapper types exist only
/// in the test FFI flavor's bindings, so an ungated reference would not compile
/// in Release. Everything else here is reached only from `#if DEBUG` code and
/// has no twin.
@MainActor
public enum E2eLoudSurfaces {
    #if DEBUG

    private static let connectionReports = FfiConnectionReportsForTest()
    private static let paintedErrors = FfiPaintedErrorTallyForTest()
    private static var paintedFrameQueued = false
    private static var lastShown = ""

    /// Count one connection-state value the indicator received — EVERY value,
    /// repeats included: a repeat is a report and never a transition, which is
    /// how "further failed attempts left 'Cannot connect' standing" is told apart
    /// from "nothing happened" (`transport-connection.md` § `Unreachable`).
    public static func observeConnectionReport(_ state: FfiConnectionState) {
        connectionReports.observe(state: state)
    }

    /// Start feeding `painted_errors` from the in-process registry — the frame
    /// `/registry` serves. Every slot-table change marks the frame dirty, and it
    /// is read once on the next main-actor turn, after the burst that changed
    /// it (windows reads once per layout pass, web once per animation frame).
    /// Called where the in-process agent starts, the only time the registry is
    /// live; idempotent.
    public static func installPaintedErrorObserver() {
        AutomationRegistry.shared.onChange = { schedulePaintedFrame() }
        observePaintedFrame()
    }

    private static func schedulePaintedFrame() {
        guard !paintedFrameQueued else { return }
        paintedFrameQueued = true
        Task { @MainActor in
            paintedFrameQueued = false
            observePaintedFrame()
        }
    }

    /// Feed the tally one frame: every visible element with text. The tally
    /// applies the contract's error-surface predicate itself.
    private static func observePaintedFrame() {
        paintedErrors.observe(frame: AutomationRegistry.shared.paintedTexts().map {
            FfiPaintedElement(id: $0.id, text: $0.text)
        })
        // One log line per change of what is painted, never per frame: the
        // evidence a count that did (or did not) move can be checked against.
        let shown = paintedErrors.json()
        if shown != lastShown {
            lastShown = shown
            logMessage(level: .info, target: "fauna.e2e", message: "[painted-errors] \(shown)")
        }
    }

    /// The `connection_reports` and `painted_errors` state values, decoded (a raw
    /// JSON string would double-encode at the wire). Published unconditionally:
    /// both counters are process-wide and start at zero. The painted frame is
    /// read once more here, so a change no slot mutation announced (a frame that
    /// only moved on screen) is still seen by the next read.
    public static var stateFragment: [String: Any] {
        observePaintedFrame()
        var fragment: [String: Any] = [:]
        for (key, json) in [("connection_reports", connectionReports.json()),
                            ("painted_errors", paintedErrors.json())] {
            if let data = json.data(using: .utf8),
               let value = try? JSONSerialization.jsonObject(with: data) {
                fragment[key] = value
            }
        }
        return fragment
    }

    /// `alert_sweep_wake` / `reconnect_backoff`. Returns the refusal sentence, or
    /// `nil` once the seam landed — convention 11: a seam that did not land is
    /// refused loudly, never acked.
    public static func apply(_ action: String, _ command: [String: Any], api: APIClient?) -> String? {
        switch action {
        case "alert_sweep_wake":
            // End the current identity's re-sweep WAIT so the production LOOP
            // sweeps again; the caller's barrier is `alert_sweep_passes`, never
            // this ack. Never a one-shot pass: that would pass "announced without
            // a restart" with the loop deleted.
            return criticalAlertSweepWakeForTest()
                ? nil
                : "\(action): no sweep loop runs for this identity, so there is nothing to wake"
        case "reconnect_backoff":
            // Pace this session's reconnect retries (never the `Unreachable`
            // threshold), or restore them with `{}`. The payload is the
            // command's own fields, handed to shared Rust verbatim, which
            // parses and refuses a malformed one.
            var fields = command
            for key in ["id", "action", "__action"] { fields.removeValue(forKey: key) }
            guard let data = try? JSONSerialization.data(withJSONObject: fields),
                  let payload = String(data: data, encoding: .utf8) else {
                return "\(action): the payload is not JSON-encodable"
            }
            guard let api else { return "\(action): no fauna client, so no connection to pace" }
            do {
                return try api.setReconnectBackoffForTest(payloadJson: payload)
                    ? nil
                    : "\(action): no nest connection to pace"
            } catch {
                return "\(action): \(error)"
            }
        default:
            return "\(action): not a loud-surface seam"
        }
    }

    #else

    // Production twin: same signature, a no-op. The agent that would publish the
    // counter is itself compiled out of a Release build.
    public static func observeConnectionReport(_ state: FfiConnectionState) {}

    #endif
}
