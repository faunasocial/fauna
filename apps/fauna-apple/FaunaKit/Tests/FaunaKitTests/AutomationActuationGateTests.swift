import Testing
import Foundation
@testable import FaunaKit

// The `isEnabled` gate on the in-process automation server's ACTUATION routes
// (`/element/{click,double_click,type,clear,select}`).
//
// Before this gate those five routes invoked the registered closure without ever
// consulting `Entry.isEnabled`, so the apple harness could drive a control the
// real UI has `.disabled(...)` — a harness-only capability with no user
// analogue, and a silent divergence from web (Playwright's `click()` auto-waits
// for enabled and fails loudly). testing.md convention 11 one layer down: not a
// *dropped* command, but an *illegal* one silently honoured, whose downstream
// failure reads exactly like a product bug.
//
// `actuationGate` is pure and static precisely so this proof needs no socket, no
// app process and no registry — the routes' own wiring is proven end-to-end by
// `tests/e2e-unified/tests/test_apple_disabled_actuation.py`.

@Suite("Automation actuation gate (isEnabled)")
struct AutomationActuationGateTests {
    private typealias Gate = InProcessAutomationServer.ActuationGate

    private func gate(
        route: String = "click", id: String = "create-event", index: Int = 0,
        enabled: Bool?, strict: Bool
    ) -> Gate {
        InProcessAutomationServer.actuationGate(
            route: route, id: id, index: index,
            isEnabled: enabled.map { v in { v } }, strict: strict)
    }

    @Test("an enabled control is driven, with no warning, in either mode")
    func enabledIsAllowedSilently() {
        #expect(gate(enabled: true, strict: false) == .allow(warn: nil))
        #expect(gate(enabled: true, strict: true) == .allow(warn: nil))
    }

    @Test("an entry with no isEnabled predicate defaults to enabled")
    func noPredicateDefaultsEnabled() {
        // The overwhelming majority of registrations register no predicate; the
        // same default `/element/enabled` already serves. Strict mode must not
        // turn those into refusals — that would be a blast radius of every
        // control on both apps rather than the ~170 that opted in.
        #expect(gate(enabled: nil, strict: true) == .allow(warn: nil))
        #expect(gate(enabled: nil, strict: false) == .allow(warn: nil))
    }

    @Test("strict mode refuses a disabled control with a named, greppable error")
    func strictRefusesDisabled() {
        guard case .refuse(let message) = gate(enabled: false, strict: true) else {
            Issue.record("strict mode must refuse a disabled control")
            return
        }
        // The message is what a failing test's author reads, so it must name the
        // element, the reason, and the way out — never a bare "refused". The way
        // out is deliberately the REAL user path first (that is the fix in almost
        // every case), with the enumeration flag second so nobody reads the
        // refusal as "set this flag to make the red go away".
        #expect(message.contains("element is disabled"))
        #expect(message.contains("create-event[0]"))
        #expect(message.contains("wait_until_enabled"))
        #expect(message.contains("FAUNA_E2E_PERMISSIVE_ACTUATION"))
    }

    @Test("refusal is the DEFAULT — the staged rollout flipped 2026-08-05")
    func refusalIsTheDefault() {
        // The polarity pin. `strictEnabled` used to be opt-IN
        // (`FAUNA_E2E_STRICT_ENABLED`), so an ordinary run drove disabled
        // controls and every downstream "nothing happened" failure was
        // ambiguous. Both targets are now swept clean (macOS 4 violations on 3
        // elements, all fixed; iOS 1 violation on 1 element, the gate's own
        // probe), so refusal is on unless a sweep explicitly opts out.
        //
        // Asserted against the real process environment on purpose: the bug this
        // guards against is someone reintroducing an opt-IN default, which no
        // amount of testing the pure `actuationGate(strict:)` function can catch
        // — that function is polarity-agnostic by construction.
        #expect(ProcessInfo.processInfo.environment["FAUNA_E2E_PERMISSIVE_ACTUATION"] == nil,
                "this pin assumes the test process itself is not opted out")
        #expect(FaunaE2E.strictEnabled == true)
    }

    @Test("permissive mode still drives, but emits a countable marker")
    func permissiveWarnsAndAllows() {
        guard case .allow(let warn) = gate(enabled: false, strict: false),
              let warn else {
            Issue.record("permissive mode must allow a disabled control AND warn")
            return
        }
        // One ordinary suite run enumerates every offender by grepping the app's
        // captured stderr for this marker — that is what turns "unknown blast
        // radius" into a list without a single new red.
        #expect(warn.contains("DISABLED-ACTUATION"))
        #expect(warn.contains("click"))
        #expect(warn.contains("id=create-event"))
        #expect(warn.contains("index=0"))
    }

    @Test("every actuation route is gated, and names itself in both modes",
          arguments: ["click", "double_click", "type", "clear", "select"])
    func allActuationRoutesAreGated(route: String) {
        guard case .allow(let warn) = gate(route: route, enabled: false, strict: false),
              let warn else {
            Issue.record("\(route) must warn on a disabled control")
            return
        }
        #expect(warn.contains(route))
        guard case .refuse(let message) = gate(route: route, enabled: false, strict: true) else {
            Issue.record("\(route) must refuse a disabled control under strict mode")
            return
        }
        #expect(message.contains(route))
    }

    @Test("the index reported is the queried leaf index, not always 0")
    func indexIsReported() {
        guard case .refuse(let message) = gate(id: "post-card", index: 3, enabled: false, strict: true) else {
            Issue.record("expected a refusal")
            return
        }
        // A list-row refusal that always said [0] would send the reader to the
        // wrong row — the registry addresses repeated ids by occurrence index.
        #expect(message.contains("post-card[3]"))
    }

    @Test("the predicate is consulted live, not captured at registration")
    func predicateIsLive() {
        // Registrations are re-run per body pass and the predicates close over
        // view state (`!vm.isBusy`, `!form.canCreate`), so the gate must read
        // the CURRENT value each call — a snapshot would refuse a control that
        // has since enabled, which is worse than not gating at all.
        var enabled = false
        let live: () -> Bool = { enabled }
        func decide() -> Gate {
            InProcessAutomationServer.actuationGate(
                route: "click", id: "submit", index: 0, isEnabled: live, strict: true)
        }
        guard case .refuse = decide() else {
            Issue.record("expected a refusal while disabled")
            return
        }
        enabled = true
        #expect(decide() == .allow(warn: nil))
    }
}

// The run-scoped actuation log (`FAUNA_E2E_ACTUATION_LOG`).
//
// `NSLog` reaches only the current launch's `app.err`, which dies with that
// launch's temp dir — and the app cold-relaunches per test module (testing.md
// convention 10). So a whole-suite permissive sweep used to keep nothing but the
// last module's markers, which is what forced enumeration onto strict mode, where
// each offender is a red that STOPS its test at the first one and hides the rest.
// This sink is what makes one permissive pass enumerate every offender.
// `.serialized` because every case here mutates the SAME process-wide
// `FAUNA_E2E_ACTUATION_LOG`; run in parallel (the Swift Testing default) one
// case's `setenv`/`unsetenv` lands mid-flight in another and the appends scatter
// across paths. The real app never sees this: the harness sets the variable once
// at launch and never touches it again.
@Suite("Automation actuation log (run-scoped marker sink)", .serialized)
struct AutomationActuationLogTests {
    /// Run `body` with `FAUNA_E2E_ACTUATION_LOG` pointed at a fresh temp path,
    /// restoring the environment afterwards.
    private func withLogPath(_ body: (String) throws -> Void) rethrows {
        let path = FileManager.default.temporaryDirectory
            .appendingPathComponent("actuation-log-\(UUID().uuidString).txt").path
        setenv("FAUNA_E2E_ACTUATION_LOG", path, 1)
        defer {
            unsetenv("FAUNA_E2E_ACTUATION_LOG")
            try? FileManager.default.removeItem(atPath: path)
        }
        try body(path)
    }

    @Test("with no path set, appending is a silent no-op")
    func noPathIsNoOp() {
        unsetenv("FAUNA_E2E_ACTUATION_LOG")
        #expect(FaunaE2E.actuationLogPath == nil)
        // Must not throw, crash, or create anything: an ordinary run sets no
        // path and has to behave exactly as it did before the sink existed.
        InProcessAutomationServer.appendActuationLog("ignored")
    }

    @Test("the first marker creates the file; later ones append")
    func createsThenAppends() throws {
        try withLogPath { path in
            // The harness need not pre-create the file — the app may be the
            // first writer, and each cold relaunch re-opens the same path.
            #expect(!FileManager.default.fileExists(atPath: path))
            InProcessAutomationServer.appendActuationLog("first")
            InProcessAutomationServer.appendActuationLog("second")

            let lines = try String(contentsOfFile: path, encoding: .utf8)
                .split(separator: "\n", omittingEmptySubsequences: true)
            // Appending, never truncating: a second launch writing over the
            // first would silently discard whole modules' worth of offenders,
            // which is precisely the loss this sink exists to stop.
            #expect(lines == ["first", "second"])
        }
    }

    @Test("a marker appends after content another writer already put there")
    func appendsAfterForeignContent() throws {
        try withLogPath { path in
            // The harness stamps `=== TEST <nodeid>` boundaries into the same
            // file from the pytest side, interleaved with the app's markers —
            // that interleaving is what attributes each offender to its test.
            try "=== TEST tests/test_x.py::test_y\n".write(
                toFile: path, atomically: true, encoding: .utf8)
            InProcessAutomationServer.appendActuationLog("[InProcessAutomation] DISABLED-ACTUATION click id=submit index=0")

            let lines = try String(contentsOfFile: path, encoding: .utf8)
                .split(separator: "\n", omittingEmptySubsequences: true)
            #expect(lines.count == 2)
            #expect(lines.first == "=== TEST tests/test_x.py::test_y")
            #expect(lines.last?.contains("DISABLED-ACTUATION click id=submit") == true)
        }
    }
}
