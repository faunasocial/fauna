import Foundation
import SwiftUI
#if os(macOS)
import AppKit
#endif

// Compiled out of release artifacts (`e2e-conventions.md` convention 15), like
// `BarrierTestCommand` and the app shells' whole `handleTestCommand` surface
// that calls into it.
#if DEBUG

/// Apple's leg of the cross-app `focus_move` / `switch_pane` TestAgent commands
/// — convention 17 layer (c)'s walk vocabulary
/// (`docs/goal/architecture/e2e-conventions.md` § convention 17; the contract
/// and the payload grammar live in ONE home, `fauna_e2e_agent::{FOCUS_MOVE,
/// SWITCH_PANE}`).
///
/// **Contract, identical on every app:** move/cross the keyboard focus through
/// the app's REAL key door — the same handler a human's Tab/Shift-Tab keystroke
/// reaches, never a private seam that sets a focus index directly. That
/// qualifier is the whole value of the command (see the Rust doc comment for
/// why); it is why this file drives AppKit's own key-view loop on macOS rather
/// than a hand-rolled ordering, and why iOS — which has no public API to
/// trigger that loop's equivalent — refuses instead of faking one (see
/// `docs/goal/architecture/apps/apple-e2e-automation.md` § Declared platform
/// absences).
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (and so
/// `swift-test` covers it), mirroring `BarrierTestCommand`. Both shells route
/// `focus_move` and `switch_pane` here from `handleTestCommand`.
@MainActor
public enum FocusWalkTestCommand {
    // MARK: - The cross-app names, mirrored (not imported)
    //
    // apple cannot link `fauna-e2e-agent` (it is a Rust crate the two Rust apps
    // use directly) — exactly the situation `BarrierTestCommand` documents, and
    // the same fix: re-spell the wire vocabulary here against its one
    // documented home. Any drift is caught by a cross-app self-test the same
    // way `test_agent_barrier.py` catches `BarrierTestCommand`'s.

    /// Mirrors `fauna_e2e_agent::FOCUS_MOVE`.
    public static let focusMoveAction = "focus_move"
    /// Mirrors `fauna_e2e_agent::SWITCH_PANE`.
    public static let switchPaneAction = "switch_pane"
    /// Mirrors `fauna_e2e_agent::FOCUS_MOVE_MAX_TIMES`.
    public static let focusMoveMaxTimes = 256

    /// Whether this action is one of ours — so a shell's `switch` and this file
    /// cannot disagree about which names apple claims to implement.
    public static func handles(_ action: String) -> Bool {
        action == focusMoveAction || action == switchPaneAction
    }

    /// Apply `focus_move` / `switch_pane`. Returns `nil` on success, or a
    /// human-readable reason the caller must surface as a **loud** TestAgent
    /// failure — never a silent no-op (convention 11).
    public static func apply(action: String, command: [String: Any]) async -> String? {
        #if os(macOS)
        switch action {
        case focusMoveAction:
            return applyFocusMove(command)
        case switchPaneAction:
            return applySwitchPane(command)
        default:
            return "FocusWalkTestCommand: not my command: \(action)"
        }
        #else
        guard handles(action) else {
            return "FocusWalkTestCommand: not my command: \(action)"
        }
        // ⚠ DECLARED ABSENCE, not a dropped command — see the doc pointer above.
        // iOS/UIKit exposes no public in-process API to trigger the focus
        // engine's Tab-equivalent traversal the way AppKit's
        // `selectNextKeyView`/`makeFirstResponder` do (the mechanism responds
        // only to real hardware key/press events UIKit does not let an app
        // synthesize into its own responder chain), and this app's shell has no
        // sidebar/content two-region split for `switch_pane` to cross (it is a
        // bottom `TabView`, not a `NavigationSplitView`). Refusing loudly, by
        // name, is convention 17's own prescribed outcome for a genuine
        // structural absence.
        return "\(action): not honoured on iOS — declared platform absence, " +
            "apple-e2e-automation.md § Declared platform absences"
        #endif
    }

    #if os(macOS)

    /// `{"direction": "next"|"prev", "times": <n>}`, mirroring
    /// `fauna_e2e_agent::focus_move_request`'s rulings: a present-but-malformed
    /// field is a refusal, never a silent fallback; only an ABSENT `times`
    /// defaults (to 1); `times` above the cap is refused, never clamped.
    private static func applyFocusMove(_ command: [String: Any]) -> String? {
        guard let direction = command["direction"] as? String,
              direction == "next" || direction == "prev" else {
            return "\(focusMoveAction): `direction` must be \"next\" or \"prev\", got " +
                describe(command["direction"])
        }
        let times: Int
        if let raw = command["times"] {
            // JSON numbers arrive as `NSNumber` over the wire; take the widest
            // read, matching `BarrierTestCommand`'s `count` parse.
            guard let n = (raw as? NSNumber)?.intValue ?? (raw as? Int), n >= 0 else {
                return "\(focusMoveAction): `times` must be a non-negative integer, got " +
                    describe(raw)
            }
            times = n
        } else {
            times = 1
        }
        guard times <= focusMoveMaxTimes else {
            return "\(focusMoveAction): `times` is \(times), above the \(focusMoveMaxTimes) " +
                "cap — the step loop runs on the thread that serves this agent, so a count " +
                "that large stalls every later command rather than just this one"
        }
        guard let window = ensureKeyWindow() else {
            return "\(focusMoveAction): no window to move focus in"
        }
        // The exact call AppKit's own Tab/Shift-Tab key binding makes
        // (`NSResponder.insertTab(_:)`/`insertBacktab(_:)` → `selectNextKeyView`/
        // `selectPreviousKeyView`) — SwiftUI's macOS focus system runs on this
        // same key-view loop, so this is the real door, not a shortcut around it.
        for _ in 0..<times {
            if direction == "next" {
                window.selectNextKeyView(nil)
            } else {
                window.selectPreviousKeyView(nil)
            }
        }
        return nil
    }

    /// `{"pane": "page"|"sidebar"}`, mirroring `fauna_e2e_agent::switch_pane_target`.
    ///
    /// **Mechanism:** each region (`SidebarView`, the detail column) plants one
    /// `.focusRegionAnchor(name)` marker at the top of its view tree
    /// (`FocusRegionRegistry`, below). Jumping there walks
    /// `NSView.nextValidKeyView` from that marker — the SAME key-view-loop
    /// traversal `selectNextKeyView` uses, just entering it at a specific point
    /// rather than walking the whole ring — then hands first responder to
    /// whatever it finds via `NSWindow.makeFirstResponder`, the identical call
    /// AppKit makes when a click (or a real region-crossing shortcut) changes
    /// first responder. Not a private seam: no view is chosen by identity or
    /// index, only by where the real traversal lands.
    private static func applySwitchPane(_ command: [String: Any]) -> String? {
        guard let pane = command["pane"] as? String,
              pane == "page" || pane == "sidebar" else {
            return "\(switchPaneAction): `pane` must be \"page\" or \"sidebar\", got " +
                describe(command["pane"])
        }
        guard let anchor = FocusRegionRegistry.shared.view(for: pane) else {
            return "\(switchPaneAction): no `\(pane)` region is mounted on the page " +
                "currently on screen"
        }
        guard anchor.window != nil else {
            return "\(switchPaneAction): the `\(pane)` region anchor is not attached to a window"
        }
        guard let window = ensureKeyWindow() else {
            return "\(switchPaneAction): no window to switch panes in"
        }
        // No focusable descendant is a legitimate, silent no-op — not a
        // refusal. It means this region's current CONTENT has nothing to
        // focus (measured: the bridges page renders only a `ProgressView`/
        // static `Text` when the e2e fixture's actor has no bridges
        // configured), not that the region or the command is unimplemented.
        // A real Tab-equivalent keystroke into an empty pane does nothing
        // either — no error dialog, no refusal — so treating it as a
        // violation here would make `switch_pane` measure page CONTENT
        // instead of the mechanism it exists to prove.
        guard let target = anchor.nextValidKeyView else {
            return nil
        }
        guard window.makeFirstResponder(target) else {
            return "\(switchPaneAction): the `\(pane)` region's key view refused first responder"
        }
        return nil
    }

    /// Bring the app's window key + frontmost, and return it.
    ///
    /// **Why this is needed at all, and why it belongs here rather than in
    /// production launch code.** A real human pressing Tab can only do so in a
    /// window that is ALREADY key — that precondition is implicit, not
    /// something a person "does". But this app is launched in-process as a
    /// **bare binary** with no Dock/Finder activation
    /// (`apple-e2e-automation.md` § Architecture point 5), so `NSApp.keyWindow`
    /// is `nil` at launch — measured empirically (`test_ui_walk_sweep.py`'s
    /// first real macOS run, 2026-08-17): every other `automation*` route works
    /// with no key window because it calls SwiftUI bindings directly and never
    /// touches the AppKit responder chain, so this is the FIRST command class
    /// to expose the gap. Establishing the precondition here — never
    /// production code, and gated the same `#if DEBUG` as the rest of this
    /// file — makes the harness match reality rather than the mechanism fake
    /// one.
    private static func ensureKeyWindow() -> NSWindow? {
        if let window = NSApp.keyWindow { return window }
        NSApp.activate(ignoringOtherApps: true)
        guard let window = NSApp.mainWindow
            ?? NSApp.windows.first(where: { $0.isVisible && $0.canBecomeKey })
        else {
            return nil
        }
        window.makeKeyAndOrderFront(nil)
        return window
    }

    #endif

    /// Render a payload field for a refusal message — `absent` rather than
    /// `null` when the key is missing, mirroring
    /// `fauna_e2e_agent::describe` so a failing walk names the exact mistake.
    private static func describe(_ v: Any?) -> String {
        guard let v else { return "absent" }
        if v is NSNull { return "null" }
        return "\(v)"
    }
}

#if os(macOS)

/// Tracks the CURRENT sidebar/detail region's leading marker view, so
/// `FocusWalkTestCommand.applySwitchPane` can jump the real AppKit key-view
/// loop into either region via `NSView.nextValidKeyView` rather than handpick
/// a target.
///
/// Deliberately independent of `AutomationRegistry`: it needs only a raw view
/// reference (for `nextValidKeyView` + `makeFirstResponder`), never the
/// activate/value/geometry machinery that registry exists for, and touching
/// that heavily-relied-on file for an unrelated need is its own risk.
///
/// One entry per region name, replaced (not accumulated) on every render pass
/// — `NavigationSplitView`'s detail column is `.id()`-keyed and torn down on
/// every sidebar-selection change (`ContentView.swift`), so the anchor must
/// track whichever instance is CURRENTLY mounted, exactly like
/// `AutomationRegistry.refresh` re-registers closures every body pass rather
/// than trusting the first one forever.
@MainActor
final class FocusRegionRegistry {
    static let shared = FocusRegionRegistry()
    private struct WeakBox { weak var view: NSView? }
    private var anchors: [String: WeakBox] = [:]

    private init() {}

    func register(_ name: String, view: NSView) {
        anchors[name] = WeakBox(view: view)
    }

    func view(for name: String) -> NSView? {
        anchors[name]?.view
    }
}

/// Zero-size, non-interactive marker `NSViewRepresentable` that registers its
/// backing view into `FocusRegionRegistry` under `name` on every SwiftUI
/// update — the region-anchor analogue of `AutomationRegistry`'s
/// `_AttachmentSentinel`/`refresh` freshness discipline, but needs no
/// visibility voting: a stale anchor is simply superseded by the next render's
/// `updateNSView`, and `switch_pane` reads it live at command time.
private struct _FocusRegionAnchor: NSViewRepresentable {
    let name: String

    func makeNSView(context: Context) -> NSView {
        let view = NSView(frame: .zero)
        FocusRegionRegistry.shared.register(name, view: view)
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        FocusRegionRegistry.shared.register(name, view: view)
    }
}

#endif

#endif

// MARK: - The region-anchor modifier
//
// This extension sits OUTSIDE the file's `#if DEBUG` on purpose, with the body
// gated internally — the same shape every `automation*` modifier in
// `AutomationRegistry.swift` uses (its `public extension View` is unconditional;
// each body carries its own `#if DEBUG` / `#else self`).
//
// The reason is load-bearing: the app shells call `.focusRegionAnchor(...)`
// UNCONDITIONALLY (`Fauna-macOS/Views/MainWindow/ContentView.swift`), so a
// DEBUG-only *declaration* fails every release build with "value of type
// 'ZStack<some View>' has no member 'focusRegionAnchor'". That is not
// hypothetical: it left `just mac-app` red on `origin/main` from 2026-08-17
// to 2026-08-23, invisible the whole time because `swift-test`
// builds DEBUG and no merge gate builds the macOS release app.
//
// Convention 15 (`e2e-conventions.md` — the automation surface is compiled out
// of release artifacts) is satisfied by the BODY compiling away, not by the
// signature disappearing: in release this is `self`, so there is no registry
// call, no marker view, and nothing for a `strings` grep to find.
#if os(macOS)
public extension View {
    /// Mark this view's subtree as the `switch_pane` region named `name`
    /// (`"sidebar"` or `"page"`) — apply once, at the top of the region, e.g.
    /// `SidebarView(...)` and the detail column's root `Group` in
    /// `ContentView.swift`. Compiled to `self` in release; in a test-capable
    /// build `AutomationRegistry.isEnabled` gates it off at runtime, matching
    /// every other `automation*`-family modifier.
    func focusRegionAnchor(_ name: String) -> some View {
        #if DEBUG
        if AutomationRegistry.isEnabled {
            return AnyView(background(_FocusRegionAnchor(name: name).frame(width: 0, height: 0)))
        }
        return AnyView(self)
        #else
        return self
        #endif
    }
}
#endif
