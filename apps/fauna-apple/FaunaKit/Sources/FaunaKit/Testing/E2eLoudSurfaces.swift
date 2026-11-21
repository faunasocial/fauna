import Foundation
#if os(macOS)
import AppKit
#endif

/// The loud surfaces' e2e seams on macOS + iOS — `fauna_e2e_agent::
/// {ALERT_SWEEP_WAKE, RECONNECT_BACKOFF, CONNECTION_REPORTS_KEY,
/// PAINTED_ERRORS_KEY, PAINTED_TEXT_AREA_KEY, KNOCK_NOTICES_KEY, ROOT_SURFACE_KEY,
/// TOPLEVEL_WINDOWS_KEY}`, which own the cross-app contracts. Every one is a thin
/// call into `fauna-ffi`'s `test-helpers` exports (`e2e_seams.rs`,
/// `critical_alerts.rs`), so apple counts, parses and wakes with the same Rust
/// as tui, linux, web and windows (windows' `E2eLoudSurfaces.cs` is the sibling
/// leg). One implementation for both targets.
///
/// **A gated real plus a same-signature production twin** (convention 15), like
/// `E2eEnv`: the connection indicator's feed and the knock toast's arm are
/// production code (`FaunaClient.startConnectionStateObserver`,
/// `FaunaClient.startKnockObserver`), and the wrapper types exist only
/// in the test FFI flavor's bindings, so an ungated reference would not compile
/// in Release. Everything else here is reached only from `#if DEBUG` code and
/// has no twin.
@MainActor
public enum E2eLoudSurfaces {
    #if DEBUG

    private static let connectionReports = FfiConnectionReportsForTest()
    private static let paintedErrors = FfiPaintedErrorTallyForTest()
    private static let paintedTextArea = FfiPaintedTextAreaForTest()
    private static let knockNotices = FfiKnockNoticesForTest()
    private static let rootSurface = FfiRootSurfaceJournalForTest()
    private static var paintedFrameQueued = false
    private static var lastShown = ""
    private static var lastRoot: String?? = .none

    /// The name of the launch surface the app's root shows now — one of the
    /// `fauna_e2e_contract::ROOT_SURFACE_*` names (`wizard`, `launch_progress`,
    /// `app`) or another launch surface's ui.yaml page name — read once per
    /// painted frame for `fauna_e2e_agent::ROOT_SURFACE_KEY`'s journal
    /// (`onboarding.md` § Wizard exit handling → *The wizard becomes the app in
    /// place*). Each target installs its own reading of its root's branch
    /// (`ContentView.rootSurfaceName`) through `TestAgentBootstrap.
    /// bindInProcessAgent`; until it does, `root_surface` is not published, so
    /// a consumer refuses loudly (convention 11) rather than read a journal
    /// of nothing.
    public static var rootSurfaceName: (@MainActor () -> String)?

    /// Count one connection-state value the indicator received — EVERY value,
    /// repeats included: a repeat is a report and never a transition, which is
    /// how "further failed attempts left 'Cannot connect' standing" is told apart
    /// from "nothing happened" (`transport-connection.md` § `Unreachable`).
    public static func observeConnectionReport(_ state: FfiConnectionState) {
        connectionReports.observe(state: state)
    }

    /// A knock push reached the knock-toast arm — called BEFORE the muted-word
    /// gate, so `seen` counts every push the arm received (`fauna_e2e_agent::
    /// KNOCK_NOTICES_KEY`, `notifications` outcome 15).
    public static func observeKnockNotice() {
        knockNotices.observe()
    }

    /// A knock toast was handed to the platform: the catalog key its text
    /// resolved from (`nil` for a verbatim text), the painted text, and the
    /// shared notification id it replaces under. Called only at the firing
    /// site, after every suppression, so an entry means "the user was shown
    /// this".
    public static func recordKnockNotice(key: String?, text: String, replaceId: UInt32) {
        knockNotices.record(key: key, text: text, replaceId: replaceId)
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
        #if os(macOS)
        WindowJournal.install()
        #endif
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
    /// applies the contract's error-surface predicate itself. The same pass
    /// feeds `painted_text_area` (convention 17) the room each such element was
    /// given; the shared record keeps the starved ones.
    private static func observePaintedFrame() {
        paintedErrors.observe(frame: AutomationRegistry.shared.paintedTexts().map {
            FfiPaintedElement(id: $0.id, text: $0.text)
        })
        paintedTextArea.observe(frame: AutomationRegistry.shared.laidOutTexts().map {
            FfiLaidOutText(
                id: $0.id, text: $0.text,
                width: Double($0.room.width), height: Double($0.room.height),
                pane: $0.room.pane.map { FfiLayoutSize(width: Double($0.width), height: Double($0.height)) },
                depth: UInt32($0.room.depth))
        })
        // One log line per change of what is painted, never per frame: the
        // evidence a count that did (or did not) move can be checked against.
        let shown = paintedErrors.json()
        if shown != lastShown {
            lastShown = shown
            logMessage(level: .info, target: "fauna.e2e", message: "[painted-errors] \(shown)")
        }
        observeRootSurface()
        #if os(macOS)
        WindowJournal.record()
        #endif
    }

    /// `fauna_e2e_agent::ROOT_SURFACE_KEY` — what this frame showed at the
    /// root: the target's reading of its root branch, or `nil` when no
    /// element was visible at all — nothing a person could read, which
    /// `onboarding.md` § Wizard exit handling forbids from the wizard's exit
    /// to the app (a bare `Color.clear` registers nothing). One log line per
    /// change.
    private static func observeRootSurface() {
        guard let rootSurfaceName else { return }
        let showing: String? = AutomationRegistry.shared.paintsAnything() ? rootSurfaceName() : nil
        rootSurface.observe(showing: showing)
        if lastRoot != .some(showing) {
            lastRoot = .some(showing)
            logMessage(level: .info, target: "fauna.e2e",
                       message: "[root-surface] \(showing ?? "nothing")")
        }
    }

    /// The `connection_reports`, `painted_errors`, `painted_text_area` and `knock_notices` state
    /// values, decoded (a raw JSON string would double-encode at the wire).
    /// Published unconditionally: all three are process-wide and start at zero
    /// (both apple targets have built the knock toast, so an empty
    /// `knock_notices` is a real negative, never an unbuilt leg). The painted frame is
    /// read once more here, so a change no slot mutation announced (a frame that
    /// only moved on screen) is still seen by the next read.
    public static var stateFragment: [String: Any] {
        observePaintedFrame()
        var fragment: [String: Any] = [:]
        var published = [("connection_reports", connectionReports.json()),
                         ("painted_errors", paintedErrors.json()),
                         ("painted_text_area", paintedTextArea.json()),
                         ("knock_notices", knockNotices.json())]
        // Only once the target has installed its root reading (convention 11).
        if rootSurfaceName != nil {
            published.append(("root_surface", rootSurface.json()))
        }
        #if os(macOS)
        published.append(("toplevel_windows", WindowJournal.json()))
        #endif
        for (key, json) in published {
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
            #if FAUNA_KIDS
            // The critical-alert sweep is excised from the kids build: refused
            // loudly (convention 11), never acked.
            return "\(action): the critical-alert sweep is excised from the kids build"
            #else
            return criticalAlertSweepWakeForTest()
                ? nil
                : "\(action): no sweep loop runs for this identity, so there is nothing to wake"
            #endif
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

    #if os(macOS)
    /// `fauna_e2e_agent::TOPLEVEL_WINDOWS_KEY` on macOS — the app's visible
    /// windows, so the in-place witness can tell the window that showed the
    /// wizard from one opened after it (a close and a reopen lands the same
    /// elements in a new window, which no element read can see). Each
    /// `NSWindow` gets a serial for its whole life; recorded on every
    /// application update (once per processed event), on a window's close,
    /// and on every painted frame — the shared journal keeps changes only.
    /// A window counts when it is visible, can be a main window and is no
    /// sheet (a sheet rides its parent window); its name is its scene
    /// identifier, else its class.
    @MainActor
    private enum WindowJournal {
        private final class Tagged {
            weak var window: NSWindow?
            let id: UInt64
            init(_ window: NSWindow, _ id: UInt64) { self.window = window; self.id = id }
        }

        private static let journal = FfiToplevelWindowJournalForTest()
        private static var tagged: [Tagged] = []
        private static var nextId: UInt64 = 0
        private static var installed = false

        static func install() {
            guard !installed else { return }
            installed = true
            let center = NotificationCenter.default
            center.addObserver(forName: NSApplication.didUpdateNotification, object: nil, queue: .main) { _ in
                MainActor.assumeIsolated { record() }
            }
            center.addObserver(forName: NSWindow.willCloseNotification, object: nil, queue: .main) { _ in
                // The closing window is still visible here: record once it is gone.
                Task { @MainActor in record() }
            }
            record()
        }

        private static func serial(of window: NSWindow) -> UInt64 {
            tagged.removeAll { $0.window == nil }
            if let known = tagged.first(where: { $0.window === window }) { return known.id }
            nextId += 1
            tagged.append(Tagged(window, nextId))
            return nextId
        }

        static func record() {
            let visible = NSApplication.shared.windows
                .filter { $0.isVisible && $0.canBecomeMain && !$0.isSheet }
                .map { FfiToplevelWindow(
                    id: serial(of: $0),
                    name: $0.identifier?.rawValue ?? String(describing: type(of: $0))) }
            journal.observe(visible: visible)
        }

        static func json() -> String {
            record()
            return journal.json()
        }
    }
    #endif

    #else

    // Production twin: same signature, a no-op. The agent that would publish the
    // counter is itself compiled out of a Release build.
    public static func observeConnectionReport(_ state: FfiConnectionState) {}
    public static func observeKnockNotice() {}
    public static func recordKnockNotice(key: String?, text: String, replaceId: UInt32) {}

    #endif
}
