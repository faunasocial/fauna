import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for apple's `focus_move` / `switch_pane` leg (convention 17
// layer (c); contract in `fauna_e2e_agent::{FOCUS_MOVE, SWITCH_PANE}`).
//
// These cases pin the PARSE half only — the two rulings mirrored from
// `focus_move_request`/`switch_pane_target` (a present-but-malformed field is a
// refusal, never a silent fallback; only an ABSENT `times` defaults; `times`
// above the cap is refused, never clamped) — which is exactly what `swift test`
// can grade headlessly. The AppKit half (`selectNextKeyView`/
// `makeFirstResponder`/`FocusRegionRegistry`) needs a real key window and a
// mounted `SidebarView`/detail region, so it is graded by the real e2e walk
// sweep instead (`tests/walk/test_ui_walk_sweep.py --walk-sweep --app macos`),
// matching how `BarrierTestCommandTests` splits mechanism-vs-wiring coverage.
#if DEBUG

@MainActor
@Suite("Focus-walk test-agent command (convention 17 layer (c))")
struct FocusWalkTestCommandTests {
    // MARK: - focus_move parsing

    @Test func focusMoveRefusesAMissingDirection() async {
        let reason = await FocusWalkTestCommand.apply(
            action: FocusWalkTestCommand.focusMoveAction, command: [:])
        #expect(reason?.contains("direction") == true)
    }

    @Test func focusMoveRefusesAnUnknownDirection() async {
        let reason = await FocusWalkTestCommand.apply(
            action: FocusWalkTestCommand.focusMoveAction,
            command: ["direction": "sideways"])
        #expect(reason?.contains("direction") == true)
    }

    /// A present-but-malformed `times` is a refusal — only an ABSENT `times`
    /// defaults. A `.intValue`-style silent coercion would turn `"3"` into a
    /// legal move, the exact disguise `fauna_e2e_agent::focus_move_request`'s
    /// doc comment names.
    @Test func focusMoveRefusesAStringTimesRatherThanCoercingIt() async {
        let reason = await FocusWalkTestCommand.apply(
            action: FocusWalkTestCommand.focusMoveAction,
            command: ["direction": "next", "times": "3"])
        #expect(reason?.contains("times") == true)
    }

    @Test func focusMoveRefusesTimesAboveTheCapRatherThanClamping() async {
        let reason = await FocusWalkTestCommand.apply(
            action: FocusWalkTestCommand.focusMoveAction,
            command: ["direction": "next", "times": FocusWalkTestCommand.focusMoveMaxTimes + 1])
        #expect(reason?.contains("times") == true)
        #expect(reason?.contains("\(FocusWalkTestCommand.focusMoveMaxTimes)") == true)
    }

    @Test func focusMoveAcceptsTimesExactlyAtTheCap() async {
        // Headless: no key window exists in a `swift test` process, so a
        // validly-PARSED command still refuses — but on the WINDOW step, not
        // the parse step, proving the cap itself is inclusive.
        let reason = await FocusWalkTestCommand.apply(
            action: FocusWalkTestCommand.focusMoveAction,
            command: ["direction": "next", "times": FocusWalkTestCommand.focusMoveMaxTimes])
        #expect(reason?.contains("window") == true, "expected a window-step refusal, got: \(reason ?? "nil")")
    }

    @Test func focusMoveDefaultsAnAbsentTimesToOne() async {
        let reason = await FocusWalkTestCommand.apply(
            action: FocusWalkTestCommand.focusMoveAction,
            command: ["direction": "next"])
        // Parses fine (no "times" complaint); headless refusal is the window step.
        #expect(reason?.contains("window") == true, "expected a window-step refusal, got: \(reason ?? "nil")")
    }

    // MARK: - switch_pane parsing

    @Test func switchPaneRefusesAMissingPane() async {
        let reason = await FocusWalkTestCommand.apply(
            action: FocusWalkTestCommand.switchPaneAction, command: [:])
        #expect(reason?.contains("pane") == true)
    }

    @Test func switchPaneRefusesAnUnknownPane() async {
        let reason = await FocusWalkTestCommand.apply(
            action: FocusWalkTestCommand.switchPaneAction,
            command: ["pane": "toolbar"])
        #expect(reason?.contains("pane") == true)
    }

    @Test func switchPaneRefusesAnUnmountedRegion() async {
        // No `SidebarView`/detail root is mounted in a `swift test` process, so
        // `FocusRegionRegistry` has no anchor for either name — the same
        // refusal path a real app would hit if asked to switch to a region
        // that page genuinely never mounts.
        let reason = await FocusWalkTestCommand.apply(
            action: FocusWalkTestCommand.switchPaneAction,
            command: ["pane": "sidebar"])
        #expect(reason?.contains("sidebar") == true)
    }

    // MARK: - Contract

    /// The name set this app claims to implement is spelled once; a shell's
    /// `switch` and this handler cannot disagree about it.
    @Test func handlesExactlyTheTwoCrossAppNames() {
        #expect(FocusWalkTestCommand.handles("focus_move"))
        #expect(FocusWalkTestCommand.handles("switch_pane"))
        #expect(!FocusWalkTestCommand.handles("some_other_command"))
        // Mirrors `fauna_e2e_agent::FOCUS_MOVE` / `::SWITCH_PANE` — the wire
        // names, asserted so a rename here cannot silently drop the command.
        #expect(FocusWalkTestCommand.focusMoveAction == "focus_move")
        #expect(FocusWalkTestCommand.switchPaneAction == "switch_pane")
        // Mirrors `fauna_e2e_agent::FOCUS_MOVE_MAX_TIMES`.
        #expect(FocusWalkTestCommand.focusMoveMaxTimes == 256)
    }

    @Test func applyRefusesAnActionThatIsNotOurs() async {
        let reason = await FocusWalkTestCommand.apply(action: "barrier", command: [:])
        #expect(reason?.contains("not my command") == true)
    }
}

#endif
