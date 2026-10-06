// Compiled out of release artifacts (testing.md convention 15).
#if DEBUG && (os(macOS) || os(iOS))
import Foundation
import Darwin
#if os(macOS)
import AppKit
#endif
#if os(iOS)
import UIKit
#endif

/// In-process automation HTTP server (macOS + iOS).
///
/// Hosts the cross-app e2e `/element/*` + `/app/*` contract *inside* the app
/// process and drives the UI by looking up the `AutomationRegistry` — **no
/// XCUITest, no AutomationMode**. This is the macOS analogue of the linux in-app
/// agent (`apps/fauna-linux/src/automation/`): one HTTP server per app instance,
/// bound to a per-instance port from `FAUNA_E2E_AGENT_PORT`, so concurrent e2e
/// runs never contend on a single machine-wide resource (the whole point —
/// AutomationMode is shared + wedge-prone; this is neither).
///
/// **Why a registry and not NSAccessibility?** The Phase-0 spike
/// proved the in-process NSAccessibility tree does
/// not populate for SwiftUI without an external assistive-technology client, and
/// the AXUIElement self-query path needs TCC trust and deadlocks. So the
/// automatable surface is built explicitly by the views via the `automation*`
/// view modifiers (`AutomationRegistry`), keyed by the same test ids they carry
/// in `.accessibilityIdentifier(...)`. Debug/test-only — gated on the env var,
/// never active in a release run.
public final class InProcessAutomationServer: @unchecked Sendable {
    public static let shared = InProcessAutomationServer()

    private var listenFd: Int32 = -1
    private var running = false
    public private(set) var port: UInt16 = 0

    /// State-protocol bridges (set by the app shell, mirroring `TestAgent`):
    /// `/app/state` reads `stateProvider`; `/app/commands` forwards to
    /// `commandHandler`.
    private var stateProvider: (() -> [String: Any])?
    private var commandHandler: (([String: Any]) async -> Void)?
    /// Command-ack bookkeeping, accessed **only on the main actor** (written by
    /// the command Task, read by `/app/state`). Mirrors `TestAgent`'s
    /// `lastCommandId` + `ready`: the id becomes visible when a command starts,
    /// but `ready` stays `false` until the handler **and** the SwiftUI re-render
    /// it triggers have settled — so the driver's `last_command_id==id && ready`
    /// gate can never pass before a conditionally-shown control has registered.
    private var lastCommandId = ""
    private var ready = true

    /// How long to let SwiftUI flush the re-render a state command triggers
    /// before signalling readiness. A state injection that *reveals* a control
    /// (e.g. `handle-control-checkbox`) only registers that control on its
    /// `.onAppear`, which runs on a later main-runloop pass than the state
    /// mutation; sleeping the main actor here yields that pass. Without it the
    /// ack races the render (the known-limitation-(d) staleness across the
    /// session-reused driver). Bounded + cheap (a handful of injections/test).
    private static let renderSettleMs = 100

    private init() {}

    /// Configure the state-protocol callbacks (same shape as `TestAgent.configure`).
    public func configure(
        stateProvider: @escaping () -> [String: Any],
        commandHandler: @escaping ([String: Any]) async -> Void
    ) {
        self.stateProvider = stateProvider
        self.commandHandler = commandHandler
    }

    /// Start the server on `port` (from `FAUNA_E2E_AGENT_PORT`). Idempotent.
    public func start(port: UInt16) {
        guard !running else { return }
        running = true
        self.port = port
        let thread = Thread { [weak self] in
            self?.serve(port: port)
        }
        thread.stackSize = 1 << 20
        thread.start()
        NSLog("[InProcessAutomation] starting on 127.0.0.1:%d registryEnabled=%@",
              Int(port), AutomationRegistry.isEnabled ? "YES" : "NO")
    }

    // MARK: - Socket server (blocking accept loop on its own thread)

    private func serve(port: UInt16) {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { NSLog("[InProcessAutomation] socket() failed"); return }
        listenFd = fd
        var yes: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))

        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        addr.sin_port = port.bigEndian
        let bound = withUnsafePointer(to: &addr) { p in
            p.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0 else { NSLog("[InProcessAutomation] bind(%d) failed", Int(port)); return }
        listen(fd, 16)
        NSLog("[InProcessAutomation] listening on 127.0.0.1:%d", Int(port))

        while running {
            let client = accept(fd, nil, nil)
            if client < 0 { continue }
            handleClient(client)
        }
        close(fd)
    }

    private func handleClient(_ clientFd: Int32) {
        defer { close(clientFd) }
        var data = Data()
        var buf = [UInt8](repeating: 0, count: 65536)
        let n = read(clientFd, &buf, buf.count)
        guard n > 0 else { return }
        data.append(contentsOf: buf[0..<n])
        // Read the rest of the body if Content-Length exceeds the first chunk.
        if let headerEnd = data.range(of: Data("\r\n\r\n".utf8)) {
            let header = String(data: data[..<headerEnd.lowerBound], encoding: .utf8) ?? ""
            if let cl = header.lowercased()
                .split(separator: "\r\n")
                .first(where: { $0.hasPrefix("content-length:") })
                .flatMap({ Int($0.split(separator: ":")[1].trimmingCharacters(in: .whitespaces)) }) {
                var remaining = cl - (data.count - headerEnd.upperBound)
                while remaining > 0 {
                    let m = read(clientFd, &buf, min(buf.count, remaining))
                    if m <= 0 { break }
                    data.append(contentsOf: buf[0..<m])
                    remaining -= m
                }
            }
        }
        let raw = String(data: data, encoding: .utf8) ?? ""
        let (method, path, query, body) = parseHTTP(raw)
        let (status, payload) = route(method: method, path: path, query: query, body: body)
        sendResponse(clientFd, status: status, payload: payload)
    }

    private func parseHTTP(_ raw: String) -> (String, String, [String: String], [String: Any]) {
        let lines = raw.components(separatedBy: "\r\n")
        let first = lines.first ?? ""
        let parts = first.split(separator: " ")
        let method = parts.count > 0 ? String(parts[0]) : "GET"
        let fullPath = parts.count > 1 ? String(parts[1]) : "/"
        var path = fullPath
        var query: [String: String] = [:]
        if let q = fullPath.firstIndex(of: "?") {
            path = String(fullPath[..<q])
            let qs = String(fullPath[fullPath.index(after: q)...])
            for pair in qs.split(separator: "&") {
                let kv = pair.split(separator: "=", maxSplits: 1)
                func dec(_ s: Substring) -> String {
                    String(s).replacingOccurrences(of: "+", with: "%20").removingPercentEncoding ?? String(s)
                }
                if kv.count == 2 { query[dec(kv[0])] = dec(kv[1]) }
            }
        }
        var body: [String: Any] = [:]
        if let r = raw.range(of: "\r\n\r\n") {
            let bodyStr = String(raw[r.upperBound...])
            if let d = bodyStr.data(using: .utf8),
               let obj = try? JSONSerialization.jsonObject(with: d) as? [String: Any] {
                body = obj
            }
        }
        return (method, path, query, body)
    }

    private func sendResponse(_ fd: Int32, status: Int, payload: [String: Any]) {
        let json = (try? JSONSerialization.data(withJSONObject: payload)).flatMap {
            String(data: $0, encoding: .utf8)
        } ?? "{}"
        let resp = "HTTP/1.1 \(status) \(status == 200 ? "OK" : "Error")\r\n"
            + "Content-Type: application/json\r\n"
            + "Content-Length: \(json.utf8.count)\r\n\r\n\(json)"
        _ = resp.withCString { write(fd, $0, strlen($0)) }
    }

    // MARK: - Routing

    /// JSON-encode a picker's painted option texts for the `/element/attr`
    /// `"options"` case — a free function, not inlined, so the switch
    /// statement's per-case type-checking stays simple (a nested
    /// `flatMap`/`try?` chain inline there once blew Swift's expression
    /// type-checker: "failed to produce diagnostic for expression"). `nil` on
    /// encode failure, mirroring every other unresolved attr.
    private static func jsonStringArray(_ values: [String]) -> String? {
        guard let data = try? JSONSerialization.data(withJSONObject: values) else { return nil }
        return String(data: data, encoding: .utf8)
    }

    /// The full ancestor scope path, or `[]` if no scope. The driver sends
    /// `scope` as a JSON string in the GET query (`scope=[{"id":..,"index":N}]`)
    /// and as a parsed JSON array in a POST body — handle both. The whole chain
    /// is preserved (not just the innermost step) so the registry can filter by
    /// real subtree containment (`AutomationRegistry.resolved*`), which is what
    /// makes a multi-level scope like `post-card[1]/quoted-post` distinguish a
    /// leaf present on only some rows — the open item the old innermost-index
    /// heuristic couldn't resolve (apple-e2e-automation.md § limitation (a)).
    private static func scopeSteps(_ query: [String: String], _ body: [String: Any]) -> [AutomationScopeStep] {
        var arr = body["scope"] as? [[String: Any]]
        if arr == nil, let s = query["scope"], let d = s.data(using: .utf8) {
            arr = (try? JSONSerialization.jsonObject(with: d)) as? [[String: Any]]
        }
        guard let arr else { return [] }
        return arr.compactMap { step in
            guard let id = step["id"] as? String else { return nil }
            return AutomationScopeStep(id: id, index: step["index"] as? Int ?? 0)
        }
    }

    // MARK: - Actuation gate (isEnabled)

    /// What an actuation route should do with a resolved entry.
    ///
    /// `/element/{click,double_click,type,clear,select}` are the routes that
    /// *drive* a control. Every one of them used to invoke the registered
    /// closure without consulting `Entry.isEnabled`, so the harness could
    /// activate a control the real UI has `.disabled(...)` — see
    /// `FaunaE2E.strictEnabled` for why that matters and why the refusal is
    /// staged behind a flag.
    enum ActuationGate: Equatable {
        /// Drive it. `warn` carries the log line for the permissive case (the
        /// control *is* disabled but strict mode is off), `nil` when enabled.
        case allow(warn: String?)
        /// Refuse loudly — the (status, error message) the route must return.
        case refuse(String)
    }

    /// Decide whether an actuation route may drive `entry`.
    ///
    /// Pure and `static` so it is unit-testable without a socket, an app, or a
    /// registry (`AutomationActuationGateTests`). An entry that registered no
    /// `isEnabled` predicate is treated as enabled — the overwhelming majority
    /// of registrations, and the same default `/element/enabled` already serves.
    static func actuationGate(
        route: String, id: String, index: Int,
        isEnabled: (() -> Bool)?, strict: Bool
    ) -> ActuationGate {
        if isEnabled?() ?? true { return .allow(warn: nil) }
        let what = "\(route) id=\(id) index=\(index)"
        if strict {
            return .refuse(
                "element is disabled: \(id)[\(index)] — \(route) refused because the "
                + "control's isEnabled predicate is false, i.e. the real UI has it "
                + "disabled and no user could perform this action. Drive the control "
                + "through the real user path (wait_until_enabled, or fill whatever "
                + "precondition its predicate names) rather than around it; to "
                + "ENUMERATE violations instead of refusing them, sweep with "
                + "FAUNA_E2E_PERMISSIVE_ACTUATION=1 (pytest --permissive-actuation)")
        }
        return .allow(warn: "[InProcessAutomation] DISABLED-ACTUATION \(what)")
    }

    /// Serialises appends to `FaunaE2E.actuationLogPath`. Insurance rather than a
    /// live race today: `serve`'s accept loop answers one connection at a time, and
    /// every gate runs inside `onMainActor` — which is also why ONE long main-thread
    /// block silences every route, `/health` included, until it ends.
    private static let actuationLogQueue = DispatchQueue(
        label: "social.fauna.automation.actuation-log")

    /// Append one line to the run-scoped actuation log, when the harness named
    /// one (`FaunaE2E.actuationLogPath`). Best-effort by construction: a harness
    /// sink that cannot be opened or written must never fail the app under test,
    /// so every error here is swallowed — the `NSLog` marker is still emitted.
    static func appendActuationLog(_ line: String) {
        guard let path = FaunaE2E.actuationLogPath,
              let data = (line + "\n").data(using: .utf8) else { return }
        actuationLogQueue.sync {
            if let handle = FileHandle(forWritingAtPath: path) {
                defer { try? handle.close() }
                _ = try? handle.seekToEnd()
                try? handle.write(contentsOf: data)
            } else {
                // First writer for this path (the harness need not pre-create it).
                try? data.write(to: URL(fileURLWithPath: path))
            }
        }
    }

    /// The refusal message for a `type`/`clear` at an *entry mode* control whose
    /// input is not open (`Entry.typeRefusal`), or `nil` to proceed. Pure and
    /// `static`, like `actuationGate`, so it is unit-testable without a socket
    /// (`AutomationTypeRefusalTests`).
    static func typeRefusalMessage(
        route: String, id: String, index: Int, typeRefusal: (() -> String?)?
    ) -> String? {
        guard let reason = typeRefusal?() else { return nil }
        return "\(route) at \(id)[\(index)] refused: \(reason)"
    }

    /// Apply `typeRefusalMessage` — 409, like `gateActuation`: the element
    /// resolved, what is wrong is its state. Returns `nil` to proceed.
    private func typeRefusal(
        _ route: String, _ id: String, _ index: Int, _ entry: AutomationRegistry.Entry
    ) -> (Int, [String: Any])? {
        guard let message = Self.typeRefusalMessage(
            route: route, id: id, index: index, typeRefusal: entry.typeRefusal)
        else { return nil }
        return (409, ["error": message, "id": id, "index": index])
    }

    /// Apply `actuationGate`, emitting the permissive-mode warning as a side
    /// effect. Returns the refusal reply for the route to return, or `nil` to
    /// proceed.
    private func gateActuation(
        _ route: String, _ id: String, _ index: Int, _ entry: AutomationRegistry.Entry
    ) -> (Int, [String: Any])? {
        switch Self.actuationGate(
            route: route, id: id, index: index,
            isEnabled: entry.isEnabled, strict: FaunaE2E.strictEnabled
        ) {
        case .allow(let warn):
            // NSLog → stderr → the driver's captured `app.err`, which is how one
            // permissive run enumerates every offender (never a `.debug` log
            // that nothing reads — testing.md convention 11). The same line also
            // goes to the run-scoped log when the harness named one, because
            // `app.err` dies at the per-module cold relaunch.
            if let warn {
                NSLog("%@", warn)
                Self.appendActuationLog(warn)
            }
            return nil
        case .refuse(let message):
            // Record the refusal too, so one log tells the whole story in either
            // mode (in strict mode this line is the red's own cause).
            Self.appendActuationLog(
                "[InProcessAutomation] disabled-actuation-refused \(route) id=\(id) index=\(index)")
            // 409 Conflict, not 404: the element EXISTS and was resolved; what
            // is wrong is the state it is in. `http_bridge._post` maps 404 to
            // LookupError (absent) and every other code to a loud RuntimeError,
            // so the two stay distinguishable at the test.
            return (409, ["error": message, "id": id, "index": index])
        }
    }

    private func route(
        method: String, path: String, query: [String: String], body: [String: Any]
    ) -> (Int, [String: Any]) {
        let id = query["id"] ?? (body["id"] as? String) ?? ""
        // Scope resolution. The registry is a flat `id -> [Slot]` map, but each
        // slot now carries its ancestor scope path (the `.automationScope`
        // containers it sits under). `AutomationRegistry.resolved{Count,Entry,
        // Visible}` filter by that path (real subtree containment) when the id
        // participates, and fall back — byte-for-byte — to the prior flat
        // occurrence-index heuristic when it does not (the common case, e.g.
        // `backup-destination-status-row[0]`, serving-indicator, sync-folders).
        // `leafIndex` is the queried leaf's OWN explicit index (`index=` on the
        // wire), distinct from the innermost *scope* step's index; the legacy
        // branch inside `resolved*` reconstructs the old `scope.last.index`
        // behavior, so un-retrofitted ids are unaffected.
        let scopeSteps = Self.scopeSteps(query, body)
        let leafIndex = Int(query["index"] ?? "") ?? (body["index"] as? Int) ?? 0

        switch (method, path) {
        case ("GET", "/health"):
            return (200, ["ok": true])
        case ("GET", "/tree"):
            // The in-process analogue of the AX-tree dump: every slot of every
            // id with its live visibility inputs (geometry / votes / scope
            // path / capabilities) — see `AutomationRegistry.debugDump`.
            return (200, ["tree": onMainActor { AutomationRegistry.shared.debugDump() }])
        case ("GET", "/registry"):
            // The STRUCTURED twin of `/tree`: every visible element as a record,
            // so a caller can quantify over the whole screen instead of grepping a
            // human-readable dump. `/tree` stays exactly as it is — it answers
            // "why did this one lookup resolve absent" (hidden slots, votes,
            // geometry), which is a different question and needs the hidden slots
            // this route deliberately omits.
            return (200, ["elements": onMainActor {
                AutomationRegistry.shared.snapshot().map { row -> [String: Any] in
                    var out: [String: Any] = [
                        "id": row.id,
                        "index": row.index,
                        "enabled": row.enabled,
                        "declares_enabled": row.declaresEnabled,
                        "actuable": row.actuable,
                        "editable": row.editable,
                        "scope": row.scope,
                    ]
                    // Same "x,y,w,h" spelling `/element/attr?attr=frame` uses, so
                    // one parser serves both reads.
                    if let f = row.frame {
                        out["frame"] =
                            "\(Int(f.minX)),\(Int(f.minY)),\(Int(f.width)),\(Int(f.height))"
                    }
                    if let t = row.text { out["text"] = t }
                    return out
                }
            }])

        case ("GET", "/clipboard/text"):
            // The OS pasteboard's plain-text content, `null` when it holds none —
            // the windows / linux drivers' `get_clipboard_text` contract, so a
            // journey can carry what a copy button really put on the clipboard
            // (a kit copied here restores with nothing typed). Compiled out of
            // release artifacts with the rest of this server (convention 15). On
            // iOS a pasteboard ANOTHER app wrote raises the system paste banner;
            // the buttons under test write this app's own, which never does.
            let text: String? = onMainActor { Self.pasteboardText() }
            return (200, ["text": text.map { $0 as Any } ?? NSNull()])

        // --- element reads ---
        case ("GET", "/element/text"):
            return onMainActor {
                guard let e = AutomationRegistry.shared.resolvedEntry(id, scope: scopeSteps, leafIndex: leafIndex) else { return self.notFound(id, leafIndex) }
                return (200, ["text": e.text?() ?? e.value?() ?? ""])
            }
        case ("GET", "/element/visible"):
            // Predicate semantics: a missing element is `{visible:false}`, never 404.
            return (200, ["visible": onMainActor { AutomationRegistry.shared.resolvedVisible(id, scope: scopeSteps, leafIndex: leafIndex) }])
        case ("GET", "/element/enabled"):
            return (200, ["enabled": onMainActor {
                let e = AutomationRegistry.shared.resolvedEntry(id, scope: scopeSteps, leafIndex: leafIndex)
                return e?.isEnabled?() ?? (e != nil)
            }])
        case ("GET", "/element/count"):
            return (200, ["count": onMainActor { AutomationRegistry.shared.resolvedCount(id, scope: scopeSteps) }])
        case ("GET", "/element/attr"):
            let attr = query["attr"] ?? ""
            return onMainActor {
                guard let e = AutomationRegistry.shared.resolvedEntry(id, scope: scopeSteps, leafIndex: leafIndex) else { return self.notFound(id, leafIndex) }
                let v: String?
                switch attr {
                case "enabled": v = (e.isEnabled?() ?? true) ? "true" : "false"
                case "disabled": v = (e.isEnabled?() ?? true) ? "false" : "true"
                case "visible":
                    // The element's RENDERED text, where that differs from its source
                    // (`Entry.visibleText` — today only the compose field, whose markdown
                    // markers may be concealed). Deliberately NOT falling through to the
                    // `default` arm's source read: an element that publishes no rendered
                    // text answers null, so a reader of an unwired surface fails loudly
                    // rather than silently green on the source.
                    v = e.visibleText?()
                case "text-runs":
                    // The compose field's APPLIED styling off its live text storage
                    // (`Entry.textRuns`), linux's JSON shape. Null for every element
                    // that registers none — never the `default` arm's source read.
                    v = e.textRuns?()
                case "frame":
                    // Live window-space geometry (top-left origin, "x,y,w,h") off
                    // the registration sentinel — the harness's on-screen /
                    // non-zero-size probe. Scope-aware (`resolvedFrame`, slot-level
                    // since geometry lives on the slot, not the `Entry`) since
                    // 2026-08-24; nil = unresolved scope/index or sentinel not
                    // realized, reported as an empty value.
                    v = AutomationRegistry.shared
                        .resolvedFrame(id, scope: scopeSteps, leafIndex: leafIndex)
                        .map { r in
                            "\(Int(r.minX)),\(Int(r.minY)),\(Int(r.width)),\(Int(r.height))"
                        }
                case "options":
                    // The full list of option TEXTS this frame painted for a
                    // picker — not just the selected one — JSON-encoded so
                    // `drivers/base.py::option_texts` can assert the whole set
                    // (mirrors tui/web's own "options" attr). `nil` (→ wire
                    // `null`) for a non-picker element — the twin-rule
                    // `e.options` closure `/element/select` already reads —
                    // never falling to the `default` arm's selected-value
                    // read, which is a different, non-JSON string.
                    if let opts = e.options?() {
                        v = Self.jsonStringArray(opts)
                    } else {
                        v = nil
                    }
                case "checked":
                    // The cross-app toggle read (`drivers/base.py::get_attr`):
                    // "true"/"false". A named `checked` the entry publishes wins;
                    // otherwise an apple toggle's registered value is its "on"/"off"
                    // (the Switch convention), mapped here. Anything else answers
                    // null — never the `default` arm's raw value, which would read
                    // "on" where a test compares against "true".
                    if let named = e.attributes?()["checked"] {
                        v = named
                    } else {
                        switch e.value?() {
                        case "on": v = "true"
                        case "off": v = "false"
                        default: v = nil
                        }
                    }
                default:
                    // A named attribute the entry publishes wins (`Entry.attributes`);
                    // anything else keeps the generic value-then-text read.
                    if let named = e.attributes?(), let hit = named[attr] {
                        v = hit
                    } else {
                        v = e.value?() ?? e.text?()
                    }
                }
                return (200, ["value": v as Any])
            }

        // --- element actions ---
        case ("POST", "/element/click"):
            return onMainActor {
                guard let e = AutomationRegistry.shared.resolvedEntry(id, scope: scopeSteps, leafIndex: leafIndex) else {
                    return self.notFound(id, leafIndex)
                }
                guard let act = e.activate else { return self.notActuable("click", id, leafIndex) }
                if let refusal = self.gateActuation("click", id, leafIndex, e) { return refusal }
                act()
                return (200, ["ok": true])
            }
        case ("POST", "/element/double_click"):
            // The Outlook day-cell double-click → new-event compose. Fire the
            // distinct `doubleActivate` when registered; otherwise fall back to
            // the single `activate` so a double on a plain control isn't a 404.
            return onMainActor {
                guard let e = AutomationRegistry.shared.resolvedEntry(id, scope: scopeSteps, leafIndex: leafIndex) else {
                    return self.notFound(id, leafIndex)
                }
                guard let act = e.doubleActivate ?? e.activate else {
                    return self.notActuable("double_click", id, leafIndex)
                }
                if let refusal = self.gateActuation("double_click", id, leafIndex, e) { return refusal }
                act()
                return (200, ["ok": true])
            }
        case ("POST", "/element/type"):
            let text = body["text"] as? String ?? ""
            return onMainActor {
                guard let e = AutomationRegistry.shared.resolvedEntry(id, scope: scopeSteps, leafIndex: leafIndex), let set = e.setValue else {
                    return self.notFound(id, leafIndex)
                }
                if let refusal = self.gateActuation("type", id, leafIndex, e) { return refusal }
                if let refusal = self.typeRefusal("type", id, leafIndex, e) { return refusal }
                set(text)
                return (200, ["ok": true])
            }
        case ("POST", "/element/clear"):
            return onMainActor {
                guard let e = AutomationRegistry.shared.resolvedEntry(id, scope: scopeSteps, leafIndex: leafIndex), let set = e.setValue else {
                    return self.notFound(id, leafIndex)
                }
                if let refusal = self.gateActuation("clear", id, leafIndex, e) { return refusal }
                if let refusal = self.typeRefusal("clear", id, leafIndex, e) { return refusal }
                set("")
                return (200, ["ok": true])
            }
        case ("POST", "/element/key"):
            // One named caret key into a text field, through the real text view's own
            // caret-move action (`Entry.pressKey`) so its selection-change handlers run
            // as for a real key. `Enter` on a control with no key door is keyboard
            // activation: its `activate`, which returns at once as a keypress does
            // while whatever it starts runs on. An element without a key door, or a
            // key it does not drive, is REFUSED with a 409 — never acked (convention 11).
            let key = body["key"] as? String ?? ""
            return onMainActor {
                guard let e = AutomationRegistry.shared.resolvedEntry(id, scope: scopeSteps, leafIndex: leafIndex) else {
                    return self.notFound(id, leafIndex)
                }
                if e.pressKey == nil && key == "Enter" {
                    guard let act = e.activate else { return self.notActuable("key", id, leafIndex) }
                    if let refusal = self.gateActuation("key", id, leafIndex, e) { return refusal }
                    act()
                    return (200, ["ok": true])
                }
                guard let press = e.pressKey else {
                    return (409, ["error": "press_key \(key.debugDescription): \(id) takes no named key on apple",
                                  "id": id, "index": leafIndex])
                }
                if let refusal = self.gateActuation("key", id, leafIndex, e) { return refusal }
                if let error = press(key) {
                    return (409, ["error": error, "id": id, "index": leafIndex])
                }
                return (200, ["ok": true])
            }
        case ("POST", "/element/select"):
            // Pickers/menus: the wire `value` is the option to choose. A
            // control registers its selection writeback as `setValue` (via the
            // `automationSelect` modifier), which maps the wire string to the
            // bound selection exactly as choosing the menu item would.
            let value = body["value"] as? String ?? ""
            return onMainActor {
                guard let e = AutomationRegistry.shared.resolvedEntry(id, scope: scopeSteps, leafIndex: leafIndex), let set = e.setValue else {
                    return self.notFound(id, leafIndex)
                }
                if let refusal = self.gateActuation("select", id, leafIndex, e) { return refusal }
                // Convention 11's twin rule (e2e-conventions.md): refuse a value
                // this frame never painted rather than writing it through — a
                // call site without `options:` (not yet converted) keeps the
                // old unchecked behaviour. 409, not 404: the element WAS
                // resolved; what's wrong is the value, mirroring `gateActuation`
                // above (`http_bridge.py::select` keys off the 409, not the
                // message shape).
                if let options = e.options {
                    let painted = options()
                    if !painted.contains(value) {
                        let message = "select target \(value.debugDescription) is not offered by "
                            + "\(id) — this frame painted [\(painted.joined(separator: ", "))]"
                        return (409, ["error": message, "id": id, "index": leafIndex])
                    }
                }
                set(value)
                return (200, ["ok": true])
            }

        // --- state protocol (set_state / call_machine_method), same as TestAgent ---
        case ("GET", "/app/state"):
            // Read the ack bookkeeping + state together on the main actor, so
            // `ready` reflects the in-flight command honestly (no torn read with
            // the command Task that writes them).
            let (cmdId, isReady, state) = onMainActor {
                (self.lastCommandId, self.ready, self.stateProvider?() ?? [:])
            }
            return (200, ["last_command_id": cmdId, "ready": isReady, "state": state])
        case ("POST", "/app/commands"):
            // The id + ready flag are set inside the dispatched main-actor Task
            // (not here), so `ready` can only flip true after the handler and
            // the resulting render have settled.
            dispatchCommand(body)
            return (200, ["ok": true])

        // --- targeted scroll (real; linux serves this too) ---
        case ("POST", "/element/scroll-into-view"):
            // Centres the element in every enclosing scroll view. Reply shape is
            // linux's 1:1 (`automation/agent.rs::scroll_into_view`): `found` on a
            // scroll, an `error` string otherwise — never a bare `ok`, which the
            // driver reads as NOT-found. This route was a no-op stub returning
            // `["ok": true]` until it was implemented; the stub was safe only
            // because both apple drivers left `_supports_scroll_into_view` False,
            // and its trap was that the route LOOKED present, so flipping the
            // flag alone would have bought a silent no-op.
            return onMainActor {
                guard let scroll = AutomationRegistry.shared.resolvedScrollIntoView(
                    id, scope: scopeSteps, leafIndex: leafIndex) else {
                    return self.notFound(id, leafIndex)
                }
                switch scroll() {
                case .scrolled(let geo):
                    var reply: [String: Any] = ["found": true]
                    if let geo {
                        // Additive over linux's `{"found": true}` (the driver reads
                        // only `found`), so a caller can assert the element's REAL
                        // post-scroll exposure instead of trusting the bare flag.
                        reply["frame"] = "\(Int(geo.frame.minX)),\(Int(geo.frame.minY))," +
                            "\(Int(geo.frame.width)),\(Int(geo.frame.height))"
                        reply["viewport"] =
                            "\(Int(geo.windowBounds.width)),\(Int(geo.windowBounds.height))"
                    }
                    return (200, reply)
                case .noScrollableAncestor:
                    return (200, ["error": "no scrollable ancestor"])
                case .detached:
                    return (200, ["error": "sentinel not realized (detached or no sentinel)"])
                }
            }

        // --- window ops (macOS only — desktop-residency.md's "real close door";
        // the windows/linux drivers' own `/window/close` twin, AppKit's shape) ---
        case ("POST", "/window/close"):
            #if os(macOS)
            onMainActor {
                // Snapshot first: `performClose` removes the window from
                // `NSApp.windows` as it runs, so iterating the live array would
                // mutate under us.
                let windows = NSApp.windows.filter { $0.isVisible }
                for window in windows { window.performClose(nil) }
            }
            return (200, ["ok": true])
            #else
            return (404, ["error": "window/close is macOS-only"])
            #endif

        // The app's on-screen windows — what the hidden auto-start launch
        // (`apps/macos.md` § App Lifecycle → *Auto-start at sign-in*) promises
        // there are none of. Only windows that can become MAIN count: the
        // menu-bar status item and its popover are windows too, and a resident
        // app always has those.
        case ("GET", "/window/visible"):
            #if os(macOS)
            let identifiers: [String] = onMainActor {
                NSApp.windows
                    .filter { $0.isVisible && $0.canBecomeMain }
                    .map { $0.identifier?.rawValue ?? "" }
            }
            return (200, ["windows": identifiers])
            #else
            return (404, ["error": "window/visible is macOS-only"])
            #endif

        // --- app-lifecycle leave doors (`reserved-folders.md` § The
        // leave-flush promise). Each target gets ITS OWN door, because the
        // platforms' leave doors genuinely differ and the promise is worded
        // door-neutrally ("closing its window, switching away on your phone,
        // quitting"). Both drive the REAL delegate callback the OS itself
        // calls — not a test-only flush entry point — so what the witness
        // proves is the app's own wiring on its own door. What neither proves
        // is that the OS invokes that callback (AppKit's and UIKit's own
        // contract), exactly as linux's close-request witness does not prove
        // that GTK emits close-request. ---
        case ("POST", "/app/lifecycle/quit"):
            #if os(macOS)
            // `NSApp.terminate` is literally what the Quit menu item sends, so
            // this runs the real `applicationShouldTerminate` gate — including
            // its bounded drafts/cue flush and its `.terminateLater` reply.
            // Deliberately NOT `/window/close`: on macOS a window close is
            // never a quit (`test_sync_agent_survives_macos_window_close.py`),
            // so `performClose` would not reach the leave-flush door at all.
            //
            // Replies BEFORE terminating: the caller must not have to race a
            // process exit for its HTTP response (the `/window/close` twin
            // documents the same hazard from the driver side).
            //
            // ⚠ Scheduled on the RUN LOOP (`perform(_:with:afterDelay:)`), never
            // `DispatchQueue.main.asyncAfter`. `applicationShouldTerminate` may
            // answer `.terminateLater`, which parks AppKit in a nested run loop
            // until `reply(toApplicationShouldTerminate:)`. `DispatchQueue.main`
            // is SERIAL, so terminating from inside a main-queue work item blocks
            // that item for the whole park — and the queue behind it — so the very
            // reply that would release it can never run, and neither can the
            // gate's bounded fallback. The nested loop drains the main queue
            // happily; it just cannot start a second item while the first is still
            // on the stack. A run-loop timer puts no item on that queue at all.
            // MEASURED, not reasoned: the dispatch form hung the app indefinitely
            // and stalled the automation server ("bridge thread pegged", app
            // alive) — a real ⌘Q never hits this because AppKit delivers terminate
            // from the run loop's own event dispatch.
            onMainActor {
                NSApp.perform(#selector(NSApplication.terminate(_:)), with: nil, afterDelay: 0.05)
            }
            return (200, ["ok": true])
            #else
            return (404, ["error": "app/lifecycle/quit is macOS-only"])
            #endif

        case ("POST", "/app/lifecycle/background"):
            #if os(iOS)
            // The real `UIApplicationDelegate.applicationDidEnterBackground`,
            // invoked on the real `UIApplication.shared.delegate` — the same
            // selector UIKit sends when the user swipes home. The simulator
            // offers no way to background an app from outside the process
            // (`simctl` has no such verb), so this is the door.
            //
            // Unlike the macOS quit, the app keeps running afterwards: iOS
            // backgrounding is not an exit, which is why the flush rides a
            // `beginBackgroundTask` extension rather than a terminate reply.
            onMainActor {
                let app = UIApplication.shared
                app.delegate?.applicationDidEnterBackground?(app)
            }
            return (200, ["ok": true])
            #else
            return (404, ["error": "app/lifecycle/background is iOS-only"])
            #endif

        // --- stubs so the driver's helpers don't 404 (linux stubs these too) ---
        case ("POST", "/scroll"), ("POST", "/dismiss-dialogs"):
            return (200, ["ok": true])
        case ("POST", "/screenshot"):
            return (200, ["path": ""])

        default:
            return (404, ["error": "unknown route", "method": method, "path": path])
        }
    }

    private func notFound(_ id: String, _ index: Int) -> (Int, [String: Any]) {
        (404, ["error": "element not found", "id": id, "index": index])
    }

    /// The element is on screen but offers nothing to activate — a 409 refusal
    /// (convention 11), never `notFound`: a 404 would read as "not here yet" and
    /// send the driver scrolling and retrying for an element it already found.
    private func notActuable(_ route: String, _ id: String, _ index: Int) -> (Int, [String: Any]) {
        (409, ["error": "\(route): \(id) is not actuable", "id": id, "index": index])
    }

    /// Run `work` on the main actor (the registry + SwiftUI state are main-only).
    /// The server runs on its own thread, so `.sync` to main never self-deadlocks.
    private func onMainActor<T>(_ work: @MainActor () -> T) -> T {
        if Thread.isMainThread { return MainActor.assumeIsolated(work) }
        return DispatchQueue.main.sync { MainActor.assumeIsolated(work) }
    }

    /// The general pasteboard's plain-text content (`GET /clipboard/text`).
    @MainActor private static func pasteboardText() -> String? {
        #if os(macOS)
        return NSPasteboard.general.string(forType: .string)
        #else
        return UIPasteboard.general.string
        #endif
    }

    /// Forward an injected command to the app's handler (state protocol). The
    /// driver polls `/app/state` for the ack, so the whole flow runs on the main
    /// actor and gates `ready`: id-visible + `ready=false` at the start, the
    /// handler, a render settle, then `ready=true`. This mirrors `TestAgent`'s
    /// `ready=false`-while-processing discipline (the hosted server lost it when
    /// it hard-coded `ready:true`, which is what made conditionally-shown
    /// controls go stale across the reused driver — known limitation (d)).
    private func dispatchCommand(_ command: [String: Any]) {
        guard let handler = commandHandler else { return }
        let cmdId = command["id"] as? String ?? ""
        let action = command["action"] as? String ?? "patch"
        DispatchQueue.main.async {
            Task { @MainActor in
                self.lastCommandId = cmdId
                self.ready = false
                switch action {
                case "patch":
                    if let state = command["state"] as? [String: Any] { await handler(state) }
                case "reset": await handler(["__action": "reset"])
                case "logout": await handler(["__action": "logout"])
                default:
                    var fwd = command
                    fwd["__action"] = action
                    await handler(fwd)
                }
                // Yield the main actor so SwiftUI flushes the re-render this
                // state change triggers (registering any newly-shown control)
                // before we ack.
                try? await Task.sleep(for: .milliseconds(Self.renderSettleMs))
                self.ready = true
            }
        }
    }
}
#endif
