import Testing

#if os(macOS)
import AppKit
import SwiftUI
#endif

@testable import FaunaKit

// The apple leg of `docs/goal/architecture/security.md` § On-screen secret
// exposure (screen capture), ratified 2026-08-15.
//
// These assert the PLATFORM BIT, never the modifier's presence — android's
// `ScreenCaptureWindowFlagTest` is the shape, and the reason is that the wrong
// apple mechanism (`.privacySensitive()`) compiles, renders and changes nothing:
// a test that asserted "the modifier is attached" would have passed over it.

// MARK: - The iOS half, tested where it can actually run

// `swift test` builds for the macOS host, so the iOS bindings themselves
// (`UIScreen.isCaptured`, `scenePhase`) cannot execute here. That is exactly why
// the decision is a pure function: the mechanism is testable, and only the last
// inch — reading the two platform values — is not.

@Test func aLiveCaptureBlanksTheRevealedValue() {
    #expect(ScreenCaptureBlankPolicy.shouldBlank(isCaptured: true, isForeground: true))
}

/// The app-switcher snapshot is the durable copy, and it is the exact analogue
/// of the recents-screen thumbnail android's `FLAG_SECURE` removes — the
/// concrete harm the original finding named. A leg covering only live capture
/// would leave iOS's most persistent copy of the secret on disk.
@Test func leavingTheForegroundBlanksTheRevealedValue() {
    #expect(ScreenCaptureBlankPolicy.shouldBlank(isCaptured: false, isForeground: false))
}

/// The beside-control: without it, a policy that simply returned `true` would
/// satisfy both assertions above and blank the secret the user asked to see.
@Test func aForegroundAppWithNoCaptureShowsTheSecret() {
    #expect(!ScreenCaptureBlankPolicy.shouldBlank(isCaptured: false, isForeground: true))
}

// MARK: - The macOS half: the disposable reveal window, read back

#if os(macOS)

/// A REAL (non-deferred) window, ordered on screen: a deferred one has no
/// window-server counterpart, so its `sharingType` lives only in memory — the
/// earlier suite passed for months over a restore that real windows on macOS
/// 27.2 never do.
@MainActor
private func makeTestWindow() -> NSWindow {
    _ = NSApplication.shared
    let window = NSWindow(
        contentRect: NSRect(x: 200, y: 200, width: 300, height: 200),
        styleMask: [.titled], backing: .buffered, defer: false)
    window.isReleasedWhenClosed = false
    window.orderFrontRegardless()
    return window
}

/// The window server's own reading of a window's sharing state
/// (`kCGWindowSharingState`: 0 none, 1 read-only) — no capture API, no dialog.
///
/// ⚠ `.optionAll` filtered by window number, not `.optionIncludingWindow`:
/// measured on macOS 27.2 the latter returns an EMPTY list for a fresh
/// process's own window until the process has an activation policy and a run
/// loop turn, while the full list already carries it.
@MainActor
private func serverSharingState(_ window: NSWindow) -> Int? {
    let all = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []
    let mine = all.first { ($0[kCGWindowNumber as String] as? Int) == window.windowNumber }
    return mine?[kCGWindowSharingState as String] as? Int
}

/// A probe with a hosted value, sized like a slot and placed in `window`.
@MainActor
private func addProbe(to window: NSWindow, active: Bool = true) -> _ScreenCaptureProbeView {
    let probe = _ScreenCaptureProbeView(rootView: AnyView(Text(verbatim: "app-password")))
    probe.isActive = active
    probe.frame = NSRect(x: 20, y: 40, width: 120, height: 20)
    window.contentView?.addSubview(probe)
    probe.sync()
    return probe
}

/// Spin the main run loop a short turn — what lets a closed window's last
/// autoreleased reference drain, as it does in the running app.
@MainActor
private func spinRunLoop() {
    RunLoop.main.run(until: Date().addingTimeInterval(0.1))
}

@MainActor
private func excludedWindows() -> [NSWindow] {
    NSApp.windows.filter { $0.sharingType == .none }
}

/// The core of the mechanism: the reveal window is out of capture by BOTH
/// readings — AppKit's getter and the window server's — while its parent, the
/// app window, stays capturable by both. The parent's bit is never written.
@Test @MainActor func aRevealWindowIsExcludedWhileItsParentStaysCapturable() {
    let window = makeTestWindow()
    defer { window.close() }
    let probe = addProbe(to: window)
    defer { probe.teardown(); probe.removeFromSuperview() }

    guard let panel = probe.controller.panel else {
        Issue.record("a revealed credential must stand up its reveal window")
        return
    }
    #expect(panel.parent === window, "the reveal window is a child of the app window")
    #expect(panel.sharingType == .none)
    #expect(serverSharingState(panel) == 0, "the window server must read the reveal window excluded")
    #expect(window.sharingType == .readOnly, "the app window's bit must never be written")
    #expect(serverSharingState(window) == 1, "the app window must stay capturable")
    #expect(!panel.canBecomeMain, "the reveal window may take key for selection, never main")
    #expect(ScreenCaptureRevealWindows.count >= 1)
}

/// Placement: the reveal window covers the slot exactly, in screen space; and
/// a parent move needs nothing — AppKit moves a child window with its parent.
@Test @MainActor func theRevealWindowCoversTheSlotAndFollowsAParentMove() {
    let window = makeTestWindow()
    defer { window.close() }
    let probe = addProbe(to: window)
    defer { probe.teardown(); probe.removeFromSuperview() }
    guard let panel = probe.controller.panel else {
        Issue.record("no reveal window")
        return
    }
    #expect(panel.frame == window.convertToScreen(probe.convert(probe.bounds, to: nil)))

    let before = panel.frame.origin
    window.setFrameOrigin(NSPoint(x: window.frame.origin.x + 37, y: window.frame.origin.y + 11))
    #expect(panel.frame.origin.x == before.x + 37)
    #expect(panel.frame.origin.y == before.y + 11)
}

/// Rule 2's clear path, the failure users actually feel: when the reveal ends
/// the reveal window is destroyed, and after one run-loop turn no window of
/// the app reads `.none` — the strict read the e2e posture makes.
@Test @MainActor func endingTheRevealLeavesNoExcludedWindow() {
    let window = makeTestWindow()
    defer { window.close() }
    // Scoped to THIS reveal window rather than every `.none` window in the
    // process: the run-loop spin below can interleave other main-actor tests,
    // whose own reveal windows are live by design.
    weak var revealed: NSWindow?
    autoreleasepool {
        let probe = addProbe(to: window)
        revealed = probe.controller.panel
        #expect(revealed != nil)
        probe.removeFromSuperview()  // navigate-away: viewDidMoveToWindow(nil)
        #expect(probe.controller.panel == nil, "detaching the slot must tear the reveal window down")
    }
    spinRunLoop()
    #expect(revealed == nil, "the reveal window outlived its reveal — it would still read `.none`")
    #expect(!excludedWindows().contains { $0 === revealed })
    #expect(window.sharingType == .readOnly)
}

/// The bluesky shape: a slot always on screen that holds a reveal window only
/// while its credential is revealed — and a second reveal is a second window,
/// not a refcount.
@Test @MainActor func anInactiveSlotHoldsNoRevealWindowUntilItsSecretIsRevealed() {
    let window = makeTestWindow()
    defer { window.close() }
    let probe = addProbe(to: window, active: false)
    defer { probe.teardown(); probe.removeFromSuperview() }
    #expect(probe.controller.panel == nil, "an unrevealed credential stands up nothing")

    probe.isActive = true
    let first = probe.controller.panel
    #expect(first?.sharingType == NSWindow.SharingType.none)

    probe.isActive = false
    #expect(probe.controller.panel == nil, "hiding the secret destroys its reveal window")
    #expect(first?.isVisible == false)
    #expect(window.sharingType == .readOnly, "the app window was never touched")

    probe.isActive = true
    #expect(probe.controller.panel != nil)
    #expect(probe.controller.panel !== first, "one reveal, one window")
}

/// A slot scrolled out of view orders its reveal window out (the mask stays
/// under it in the app window) and gets it back when it returns — the window
/// still stands, so it still counts as a hold.
@Test @MainActor func aSlotOutOfViewOrdersItsRevealWindowOut() {
    let window = makeTestWindow()
    defer { window.close() }
    let probe = addProbe(to: window)
    defer { probe.teardown(); probe.removeFromSuperview() }
    #expect(probe.controller.panel?.isVisible == true)

    probe.frame.origin.y = -15  // partly below the content rect
    probe.sync()
    #expect(probe.controller.panel != nil)
    #expect(probe.controller.panel?.isVisible == false)

    probe.frame.origin.y = 40
    probe.sync()
    #expect(probe.controller.panel?.isVisible == true)
    #expect(probe.controller.panel?.parent === window)
}

@Test func theInViewRuleWantsTheWholeSlotInsideEveryClip() {
    let content = CGRect(x: 0, y: 0, width: 300, height: 200)
    let clip = CGRect(x: 0, y: 50, width: 300, height: 100)
    let inside = CGRect(x: 10, y: 60, width: 100, height: 20)
    #expect(ScreenCaptureRevealGeometry.isWhollyInView(slot: inside, clips: [clip, content]))
    // Half scrolled out of the clip: not in view.
    #expect(!ScreenCaptureRevealGeometry.isWhollyInView(
        slot: CGRect(x: 10, y: 140, width: 100, height: 20), clips: [clip, content]))
    // Fractional layout within half a point is still in view.
    #expect(ScreenCaptureRevealGeometry.isWhollyInView(
        slot: CGRect(x: -0.25, y: 60, width: 100, height: 20), clips: [clip, content]))
    // A collapsed slot has nothing to reveal.
    #expect(!ScreenCaptureRevealGeometry.isWhollyInView(slot: .zero, clips: [content]))
    // No enclosing scroll view: only the content rect clips.
    #expect(ScreenCaptureRevealGeometry.isWhollyInView(slot: inside, clips: [content]))
}

/// The probe reports the slot's own frame in window space — the input to the
/// screen mapping — and the clips it sits under, content rect last.
@Test @MainActor func theProbeReportsItsSlotAndClipsInWindowSpace() {
    let window = makeTestWindow()
    defer { window.close() }
    let probe = addProbe(to: window, active: false)
    defer { probe.removeFromSuperview() }
    #expect(probe.slotInWindow == NSRect(x: 20, y: 40, width: 120, height: 20))
    #expect(probe.clipsInWindow.last == window.contentView?.convert(window.contentView!.bounds, to: nil))
}

#endif
