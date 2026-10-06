import Testing

#if os(macOS)
import AppKit
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

// MARK: - The macOS half: the real window property, read back

#if os(macOS)

@MainActor
private func makeTestWindow() -> NSWindow {
    _ = NSApplication.shared
    return NSWindow(
        contentRect: NSRect(x: 0, y: 0, width: 10, height: 10),
        styleMask: [.titled], backing: .buffered, defer: true)
}

@Test @MainActor func aHeldWindowRefusesCapture() {
    let window = makeTestWindow()
    #expect(window.sharingType != NSWindow.SharingType.none)

    ScreenCaptureGuard.shared.acquire(window)
    #expect(
        window.sharingType == NSWindow.SharingType.none,
        "a revealed minted credential must put its hosting window out of capture")

    ScreenCaptureGuard.shared.release(window)
    #expect(
        window.sharingType != NSWindow.SharingType.none,
        "hiding the secret must give the window back — rule 2 is on-while-revealed")
}

/// Two credential rows can be revealed at once on one window, and a navigation
/// transition can briefly hold two screens. Without refcounting, the first hide
/// hands capture back while a second secret is still on screen — the silent
/// half of the bug, and the reason android needed a refcount too.
@Test @MainActor func theHoldIsRefcountedPerWindow() {
    let window = makeTestWindow()

    ScreenCaptureGuard.shared.acquire(window)
    ScreenCaptureGuard.shared.acquire(window)
    #expect(ScreenCaptureGuard.shared.holdCount(for: window) == 2)

    ScreenCaptureGuard.shared.release(window)
    #expect(
        window.sharingType == NSWindow.SharingType.none,
        "one of two revealed secrets was hidden — the other is still on screen")

    ScreenCaptureGuard.shared.release(window)
    #expect(
        window.sharingType != NSWindow.SharingType.none,
        "the last holder left, so the window must stop refusing capture")
    #expect(ScreenCaptureGuard.shared.holdCount(for: window) == 0)
}

/// Restore what was there, not a hard-coded `.readOnly`: this guard must never
/// silently WIDEN a window some other code had already narrowed.
@Test @MainActor func theLastReleaseRestoresThePreviousSharingType() {
    let window = makeTestWindow()
    window.sharingType = NSWindow.SharingType.none

    ScreenCaptureGuard.shared.acquire(window)
    ScreenCaptureGuard.shared.release(window)

    #expect(
        window.sharingType == NSWindow.SharingType.none,
        "a window that was already out of capture must stay out of it")
}

/// The wiring, not just the guard: a probe added to a real window's view tree
/// must take the hold, and removing it must give it back.
///
/// Without this the suite would pin a guard nobody is proven to call — the
/// modifier reaches its `NSWindow` through `viewDidMoveToWindow`, and that
/// attachment is the one link between "a credential is revealed" and "this
/// window refuses capture". `swift test` cannot host a SwiftUI scene, so the
/// representable's own body stays untested; the probe view under it does not
/// have to.
@Test @MainActor func aProbeAddedToAWindowTakesAndReturnsTheHold() {
    let window = makeTestWindow()
    let probe = _ScreenCaptureProbeView()

    window.contentView?.addSubview(probe)
    #expect(
        window.sharingType == NSWindow.SharingType.none,
        "attaching the probe must reach the hosting window — that attachment IS the feature")
    #expect(ScreenCaptureGuard.shared.holdCount(for: window) == 1)

    probe.removeFromSuperview()
    #expect(
        window.sharingType != NSWindow.SharingType.none,
        "navigating away from a revealed secret must give capture back")
    #expect(ScreenCaptureGuard.shared.holdCount(for: window) == 0)
}

/// The bluesky shape: a probe that is always on screen and holds only while its
/// credential is revealed, because that surface's reveal control is one Button
/// whose own label becomes the secret.
@Test @MainActor func anInactiveProbeHoldsNothingUntilItsSecretIsRevealed() {
    let window = makeTestWindow()
    let probe = _ScreenCaptureProbeView()
    probe.isActive = false

    window.contentView?.addSubview(probe)
    #expect(
        ScreenCaptureGuard.shared.holdCount(for: window) == 0,
        "an unrevealed credential must not put the window out of capture")

    probe.isActive = true
    #expect(window.sharingType == NSWindow.SharingType.none)

    probe.isActive = false
    #expect(
        window.sharingType != NSWindow.SharingType.none,
        "hiding the secret releases, even though the view never left the tree")

    probe.removeFromSuperview()
}

/// An unbalanced release must not underflow into a negative count, or the next
/// acquire would leave the window suppressed forever — the leak the whole
/// refcount exists to prevent, arriving from the other direction.
@Test @MainActor func releasingAnUnheldWindowIsANoOp() {
    let window = makeTestWindow()
    let before = window.sharingType

    ScreenCaptureGuard.shared.release(window)

    #expect(window.sharingType == before)
    #expect(ScreenCaptureGuard.shared.holdCount(for: window) == 0)
}

#endif
