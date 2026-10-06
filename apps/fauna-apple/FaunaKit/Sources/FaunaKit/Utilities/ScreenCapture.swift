import SwiftUI

#if os(macOS)
import AppKit
#elseif os(iOS)
import UIKit
#endif

// Capture suppression on credential reveals — the apple leg of
// `docs/goal/architecture/security.md` § On-screen secret exposure (screen
// capture), ratified 2026-08-15.
//
// The posture in one sentence: suppression is defense-in-depth applied to
// MINTED, REVOCABLE credentials only, never to the user's own root secrets, and
// never a control the security model relies on.
//
// ⚠ RULE 1 IS THE ONE THAT WINS ON COLLISION: never attach this to
// `secret-key-display` or `recovery-kit-secret-display`. Those show
// client-only-resident key material — a user who loses it loses the account,
// with no recovery path — and users screenshot a recovery kit precisely because
// that copy is what saves them. Suppressing there trades a shoulder-surfing
// risk for an account-loss risk, and account loss is the irreversible one.
// Both of those screens live in the per-target onboarding views
// (`Fauna-macOS/Views/Onboarding/`, `Fauna-iOS/Views/Onboarding/`), not in
// FaunaKit, so the separation is structural — keep it that way.
//
// ⚠ `.privacySensitive()` IS NOT THIS. It drives redaction placeholders for
// widgets and Always-On Display; it suppresses no screenshot and no recording.
// The security-review finding that named it as apple's mechanism was wrong, and
// reaching for it ships a control that does nothing and tests green against the
// modifier's presence rather than against the platform's behavior.
//
// ⚠ NOTHING ON THE HOST COMPILES THE `#if os(iOS)` BRANCH BELOW. `swift test`,
// `just mac-debug` and `just apple-swift-build-check` all build for the macOS
// host — including the `FaunaiOS` line, which typechecks the iOS app's *sources*
// for macOS — so `os(iOS)` is false in every one of them. This file has no
// FaunaKit-internal dependencies precisely so it can be checked directly, which
// takes seconds and needs no FFI slices:
//
//     swiftc -typecheck \
//       -sdk "$(xcrun --sdk iphonesimulator --show-sdk-path)" \
//       -target arm64-apple-ios17.0-simulator \
//       apps/fauna-apple/FaunaKit/Sources/FaunaKit/Utilities/ScreenCapture.swift
//
// Run it after touching the iOS branch. The general gap it works around — no
// gate anywhere typechecks Swift for an apple *phone* target, the Swift-layer
// twin of the Rust-slice blind spot the mac gate closed 2026-08-12 — is
// tracked.

// MARK: - The platform-neutral half (unit-testable on any host)

/// Decides whether a revealed secret must be blanked, given what the platform
/// reports about capture and foreground state.
///
/// Split out from the UIKit/AppKit bindings deliberately: the iOS half of this
/// feature cannot run in `swift test` (which builds for the macOS host), so the
/// *mechanism* lives here where a host test can exercise every arm and only the
/// last inch — reading `UIScreen.isCaptured`, observing `scenePhase` — is left
/// to the platform.
public enum ScreenCaptureBlankPolicy {
    /// `true` when the revealed value must not be on screen.
    ///
    /// Two independent reasons, and both are real exposures an iOS app can
    /// actually influence:
    ///
    /// - **A live capture** (`UIScreen.isCaptured`): screen recording, AirPlay
    ///   mirroring, or a screen-shared call. This is the sustained-exposure
    ///   case, and it is the option `security.md` names.
    /// - **Leaving the foreground**: iOS snapshots the app as it resigns active
    ///   and shows that image in the app switcher (and stores it on disk). That
    ///   is the exact analogue of the recents-screen thumbnail android's
    ///   `FLAG_SECURE` removes — the concrete harm the original finding named —
    ///   so a leg that covered only live capture would leave the platform's
    ///   most durable copy of the secret untouched.
    ///
    /// What it does NOT cover, stated plainly rather than implied away: a still
    /// screenshot. iOS gives an app no way to prevent one, so this is a partial
    /// control by construction, not by omission (`security.md` § On-screen
    /// secret exposure — capture suppression is never load-bearing).
    public static func shouldBlank(isCaptured: Bool, isForeground: Bool) -> Bool {
        isCaptured || !isForeground
    }
}

#if os(macOS)

// MARK: - macOS: the real per-window suppression

/// Refcounted `NSWindow.sharingType` guard.
///
/// Suppression is a per-WINDOW property, so a leaked hold leaves the whole app
/// unscreenshottable — which users experience as a broken machine, not as
/// security. Two credential reveals can coexist on one window (the mail
/// settings page lists several rows), and a navigation transition can briefly
/// hold two screens at once, so the count is per window and the original
/// sharing type is restored only when the last holder leaves. android needed
/// refcounting for exactly this reason.
@MainActor
public final class ScreenCaptureGuard {
    public static let shared = ScreenCaptureGuard()

    private struct Hold {
        weak var window: NSWindow?
        var count: Int
        var previous: NSWindow.SharingType
    }

    private var holds: [ObjectIdentifier: Hold] = [:]

    init() {}

    /// Take a hold on `window`. The first hold flips it to `.none`.
    public func acquire(_ window: NSWindow) {
        let key = ObjectIdentifier(window)
        if var hold = holds[key] {
            hold.count += 1
            holds[key] = hold
        } else {
            holds[key] = Hold(window: window, count: 1, previous: window.sharingType)
            window.sharingType = .none
        }
    }

    /// Drop a hold on `window`. The last one restores what was there before.
    ///
    /// Restoring the REMEMBERED value rather than hard-coding `.readOnly`
    /// matters: this must not silently widen a window some other code had
    /// already narrowed.
    public func release(_ window: NSWindow) {
        let key = ObjectIdentifier(window)
        guard var hold = holds[key] else { return }
        hold.count -= 1
        if hold.count <= 0 {
            window.sharingType = hold.previous
            holds.removeValue(forKey: key)
        } else {
            holds[key] = hold
        }
    }

    /// Live hold count for `window` — test seam, and the thing a leak assertion
    /// reads.
    public func holdCount(for window: NSWindow) -> Int {
        holds[ObjectIdentifier(window)]?.count ?? 0
    }
}

/// The view that reaches the hosting `NSWindow`. SwiftUI has no window handle,
/// so an `NSViewRepresentable` in the background is the sanctioned path — the
/// same idiom the automation registry's attachment probe uses, kept separate
/// from it because that one is compiled out of release builds and this must not
/// be.
final class _ScreenCaptureProbeView: NSView {
    /// Whether the secret is revealed right now. A surface whose reveal control
    /// is one always-present view (bluesky's reveal Button becomes the secret)
    /// cannot express "only while revealed" by mounting and unmounting, so it
    /// says so here instead.
    var isActive: Bool = true {
        didSet { if isActive != oldValue { sync() } }
    }

    private weak var held: NSWindow?

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        sync()
    }

    /// Make the hold match (isActive, window). Idempotent — every entry point
    /// calls this rather than reasoning about transitions itself, which is what
    /// keeps a double-acquire (a permanently unscreenshottable app) out of
    /// reach.
    func sync() {
        let target = isActive ? window : nil
        guard target !== held else { return }
        if let held { ScreenCaptureGuard.shared.release(held) }
        held = nil
        if let target {
            ScreenCaptureGuard.shared.acquire(target)
            held = target
        }
    }

    /// The release of last resort: a view torn down without a
    /// `viewDidMoveToWindow(nil)` would otherwise strand its hold and leave the
    /// window permanently unscreenshottable. `deinit` is nonisolated and the
    /// guard is `@MainActor`, so hop rather than assume — same shape as the
    /// automation probe's own death hook.
    deinit {
        guard let held else { return }
        Task { @MainActor in ScreenCaptureGuard.shared.release(held) }
    }
}

private struct _ScreenCaptureProbe: NSViewRepresentable {
    let isActive: Bool

    func makeNSView(context: Context) -> _ScreenCaptureProbeView {
        let view = _ScreenCaptureProbeView()
        view.isActive = isActive
        return view
    }

    func updateNSView(_ view: _ScreenCaptureProbeView, context: Context) {
        view.isActive = isActive
        view.sync()
    }

    static func dismantleNSView(_ view: _ScreenCaptureProbeView, coordinator: ()) {
        view.isActive = false
    }
}

private struct SuppressScreenCapture: ViewModifier {
    let isActive: Bool

    func body(content: Content) -> some View {
        content.background(
            _ScreenCaptureProbe(isActive: isActive)
                .frame(width: 0, height: 0)
                // The probe exists to reach a window, never to be read. Hidden
                // from accessibility because every surface it attaches to is an
                // element the e2e resolves by id
                // (`connected-apps-item-secret`,
                // `nostr-bunker-connect-string`, `atproto-app-credential-reveal`)
                // — a security control that quietly perturbs the automation tree
                // would be paid for in someone else's misdiagnosed test failure.
                .accessibilityHidden(true))
    }
}

#elseif os(iOS)

// MARK: - iOS: blank while captured or backgrounded

/// iOS has **no** per-window capture opt-out — no `FLAG_SECURE`, no
/// `sharingType`, no `SetWindowDisplayAffinity`. `security.md` names two honest
/// options and requires the leg to state which it took.
///
/// **This leg takes `UIScreen.isCaptured`**, extended to the foreground
/// transition so the app-switcher snapshot is covered too (see
/// `ScreenCaptureBlankPolicy.shouldBlank` for why both arms are real).
///
/// The option NOT taken, and why: the alternative is hosting the revealed value
/// inside a secure `UITextField`'s excluded canvas layer. That covers still
/// screenshots too — the one thing this option cannot — but only by reparenting
/// content into a layer whose exclusion UIKit documents nowhere, with a failure
/// mode of *silently stopping working* on a future iOS, plus real layout and
/// text-selection risk on the three surfaces involved. Since capture
/// suppression is explicitly never load-bearing here, a documented partial
/// control beat an undocumented total one.
private struct SuppressScreenCapture: ViewModifier {
    let isActive: Bool

    @Environment(\.scenePhase) private var scenePhase
    @State private var isCaptured = UIScreen.main.isCaptured

    private var shouldBlank: Bool {
        isActive
            && ScreenCaptureBlankPolicy.shouldBlank(
                isCaptured: isCaptured, isForeground: scenePhase == .active)
    }

    func body(content: Content) -> some View {
        content
            // `opacity`, not a branch: the revealed value keeps its layout slot,
            // so the row does not resize as the app switcher takes its snapshot
            // (a resize is itself a tell, and a reflow mid-snapshot can capture
            // a half-drawn frame).
            .opacity(shouldBlank ? 0 : 1)
            .overlay {
                if shouldBlank {
                    // Hidden from accessibility for the reason the macOS probe
                    // is: these surfaces are e2e-resolved by id, and a mask that
                    // joined the automation tree would read as the secret.
                    Text(verbatim: "••••••••")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .accessibilityHidden(true)
                }
            }
            .onReceive(
                NotificationCenter.default.publisher(
                    for: UIScreen.capturedDidChangeNotification)
            ) { _ in
                isCaptured = UIScreen.main.isCaptured
            }
    }
}

#else

// MARK: - Every other platform: a declared absence

/// watchOS has no per-window capture API either, and no credential-reveal
/// surface. Rule 3: a declared absence, never a simulated control.
private struct SuppressScreenCapture: ViewModifier {
    let isActive: Bool

    func body(content: Content) -> some View { content }
}

#endif

public extension View {
    /// Suppress screen capture while this view is on screen and `isActive`.
    ///
    /// Attach it to the **revealed** branch of a minted-credential surface, so
    /// the hold's lifetime is exactly the reveal's — `security.md`'s rule 2 is
    /// "on while revealed, off on hide or navigate-away", and scoping it to the
    /// page instead is what leaves an app unscreenshottable after the user
    /// navigates away.
    ///
    /// Pass `isActive:` only where the reveal cannot be expressed by mounting:
    /// bluesky's reveal control is one Button whose own label becomes the
    /// secret, so it is always on screen and has to say when it is holding one.
    ///
    /// ⚠ Never attach it to `secret-key-display` or
    /// `recovery-kit-secret-display` (rule 1, above).
    func suppressScreenCapture(isActive: Bool = true) -> some View {
        modifier(SuppressScreenCapture(isActive: isActive))
    }
}
