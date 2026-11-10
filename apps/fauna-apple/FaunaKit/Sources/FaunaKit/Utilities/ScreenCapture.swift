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

// MARK: - macOS: a disposable excluded reveal window

// The mechanism (`security.md` § Implementation status today, the paragraph
// ratified 2026-11-08 — built here): each revealed minted credential is painted
// in its own borderless, non-activating `NSPanel`, a CHILD of the app window,
// created non-deferred with `sharingType = .none` set before it is first shown,
// and torn down the moment the reveal ends. The app window's own `sharingType`
// is NEVER written.
//
// ⚠ WHY NOT THE APP WINDOW'S BIT: on macOS 27 `NSWindow.sharingType` is ONE-WAY
// (measured 2026-11-07 on 27.2, 26B123) — once a window reads `.none`, no later
// assignment sticks. The previous shape flipped the app window and refcounted
// the flip; on that release the first reveal left the whole app uncapturable
// until relaunch, rule 2's named failure. Putting the bit on a window that is
// thrown away means it never has to come back, and nothing is refcounted: one
// reveal, one window. One mechanism on every release — a restorable-bit arm
// would be code no test on this fleet can exercise.
//
// What a capture shows: an excluded window is left out of screenshots,
// recordings and screen shares, and the capture shows what the app window
// paints beneath it. So the slot in the app window paints the value at opacity
// 0 (keeping its layout and its accessibility) under a `••••••••` mask — the
// same blank iOS uses — and a capture shows the mask exactly where the value
// is, while the user sees the value in the reveal window above it. Were the
// exclusion ever to stop working the value would show in the capture: the
// control fails OPEN and visibly, and the posture reads the reveal window's
// real bit, so the witness sees it. That is what keeps this from being rule
// 3's simulated suppression.

/// The pure geometry of a reveal: where the slot is, and whether it is in view.
/// Split out so the rules have host tests without a SwiftUI scene.
public enum ScreenCaptureRevealGeometry {
    /// Whether a slot (window coordinates) is wholly inside every clip it sits
    /// under — each enclosing clip view's visible rect and the window's content
    /// rect, all in window coordinates. A slot scrolled partly out of view is
    /// NOT in view: the reveal window is ordered out and the mask stays, rather
    /// than paint a value over whatever the app window draws next to the slot.
    /// Half a point of slack absorbs fractional layout.
    public static func isWhollyInView(slot: CGRect, clips: [CGRect]) -> Bool {
        guard !slot.isNull, !slot.isEmpty, slot.width >= 1, slot.height >= 1 else { return false }
        return clips.allSatisfy { $0.insetBy(dx: -0.5, dy: -0.5).contains(slot) }
    }
}

/// The live reveal windows — what the posture reports as `holds`. Weak: a
/// reveal window torn down and deallocated leaves by itself.
@MainActor
enum ScreenCaptureRevealWindows {
    private static let live = NSHashTable<NSWindow>.weakObjects()

    static func register(_ window: NSWindow) { live.add(window) }
    static func unregister(_ window: NSWindow) { live.remove(window) }

    /// Reveal windows currently standing — ordered out (slot scrolled away)
    /// still counts: its bit is still on a live window.
    static var count: Int { live.allObjects.count }
}

/// The reveal window itself. A panel so it never activates the app; it may
/// become KEY so text in it is selectable as the slot's text was, but never
/// MAIN, so the app window keeps its title-bar state and its menu target.
final class _ScreenCaptureRevealPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

/// Owns one reveal window for one slot: creates it, places it, orders it out
/// and in, and destroys it. Every entry point goes through `sync`, which is
/// idempotent — the shape that keeps a stranded window out of reach.
@MainActor
final class _ScreenCaptureRevealController {
    private(set) var panel: _ScreenCaptureRevealPanel?
    private var hosting: NSHostingView<AnyView>?
    private weak var parent: NSWindow?
    private var rootView: AnyView

    init(rootView: AnyView) {
        self.rootView = rootView
    }

    func update(rootView: AnyView) {
        self.rootView = rootView
        hosting?.rootView = rootView
    }

    /// Make the reveal window match the slot.
    ///
    /// - `window`: the app window the slot is in, or `nil` (not revealed, or
    ///   detached) — then the reveal window is destroyed.
    /// - `slotInWindow`: the slot's frame in `window`'s coordinates.
    /// - `inView`: whether the slot is wholly in view
    ///   (`ScreenCaptureRevealGeometry.isWhollyInView`); while it is not, the
    ///   reveal window is ordered out and the mask stays.
    func sync(window: NSWindow?, slotInWindow: CGRect, inView: Bool) {
        guard let window else {
            tearDown()
            return
        }
        if parent !== window { tearDown() }
        let panel = self.panel ?? makePanel(for: window)
        guard inView else {
            orderOut(panel)
            return
        }
        let frame = window.convertToScreen(slotInWindow)
        if panel.frame != frame { panel.setFrame(frame, display: false) }
        panel.appearance = window.effectiveAppearance
        if panel.parent !== window {
            // `addChildWindow` orders the child in with the parent (and keeps
            // it there across moves, Spaces, full screen and miniaturize).
            window.addChildWindow(panel, ordered: .above)
        }
    }

    private func makePanel(for window: NSWindow) -> _ScreenCaptureRevealPanel {
        // ⚠ `defer: false` is load-bearing: a deferred window has no
        // window-server counterpart yet, so its bit lives only in memory.
        let panel = _ScreenCaptureRevealPanel(
            contentRect: .zero,
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false)
        // Set BEFORE the window is first shown: the exclusion must already
        // hold on the first frame the window server composites.
        panel.sharingType = .none
        // ⚠ `false` is load-bearing: `true` under the live Swift reference this
        // controller holds is the ARC over-release AppKit documents (the probe
        // crashed with SIGSEGV). The window deallocates when the last reference
        // drops, on the next run-loop turn after `close`.
        panel.isReleasedWhenClosed = false
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.animationBehavior = .none
        // The e2e harness never makes the app active, and a user switching away
        // with a secret on screen keeps it where the app window keeps it — so the
        // reveal window follows its parent, not the activation state.
        panel.hidesOnDeactivate = false
        panel.becomesKeyOnlyIfNeeded = true
        panel.isFloatingPanel = false
        let hosting = NSHostingView(rootView: rootView)
        panel.contentView = hosting
        self.panel = panel
        self.hosting = hosting
        self.parent = window
        ScreenCaptureRevealWindows.register(panel)
        return panel
    }

    private func orderOut(_ panel: NSPanel) {
        if let parent = panel.parent { parent.removeChildWindow(panel) }
        if panel.isVisible { panel.orderOut(nil) }
    }

    /// Destroy the reveal window: off its parent, closed, every reference
    /// dropped. The app window takes key back if the reveal window had it.
    func tearDown() {
        guard let panel else { return }
        let wasKey = panel.isKeyWindow
        let parent = self.parent
        ScreenCaptureRevealWindows.unregister(panel)
        orderOut(panel)
        panel.contentView = nil
        panel.close()
        self.panel = nil
        self.hosting = nil
        self.parent = nil
        if wasKey, let parent, parent.isVisible { parent.makeKey() }
    }
}

/// The slot's anchor: an `NSView` behind the revealed value, sized to it by
/// SwiftUI, which reaches the hosting `NSWindow` (SwiftUI has no window handle)
/// and reports the slot's frame. The same idiom as the automation registry's
/// attachment probe, kept separate because that one is compiled out of release
/// builds and this must not be.
final class _ScreenCaptureProbeView: NSView {
    /// Whether the secret is revealed right now. A surface whose reveal control
    /// is one always-present view (bluesky's reveal Button becomes the secret)
    /// cannot express "only while revealed" by mounting and unmounting, so it
    /// says so here instead.
    var isActive: Bool = true {
        didSet { if isActive != oldValue { sync() } }
    }

    let controller: _ScreenCaptureRevealController
    private var observed: [NSObjectProtocol] = []
    private var syncScheduled = false

    init(rootView: AnyView) {
        controller = _ScreenCaptureRevealController(rootView: rootView)
        super.init(frame: .zero)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    // The probe takes no clicks and draws nothing — the slot's own view does.
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        observe()
        sync()
    }

    // Layout moves the slot inside its window: re-place on the next turn, once
    // the whole pass has settled (ancestors included).
    override func setFrameOrigin(_ newOrigin: NSPoint) {
        super.setFrameOrigin(newOrigin)
        scheduleSync()
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        scheduleSync()
    }

    /// Scroll (an enclosing clip view's bounds), resize and full screen all move
    /// the slot against the window without touching the probe's own frame. A
    /// parent MOVE needs nothing: a child window moves with its parent.
    private func observe() {
        for token in observed { NotificationCenter.default.removeObserver(token) }
        observed = []
        guard let window else { return }
        let center = NotificationCenter.default
        var view: NSView? = superview
        while let v = view {
            if let clip = v as? NSClipView {
                clip.postsBoundsChangedNotifications = true
                observed.append(center.addObserver(
                    forName: NSView.boundsDidChangeNotification, object: clip, queue: .main
                ) { [weak self] _ in MainActor.assumeIsolated { self?.sync() } })
            }
            view = v.superview
        }
        for name in [
            NSWindow.didResizeNotification, NSWindow.didEnterFullScreenNotification,
            NSWindow.didExitFullScreenNotification, NSWindow.didChangeBackingPropertiesNotification,
        ] {
            observed.append(center.addObserver(forName: name, object: window, queue: .main) {
                [weak self] _ in MainActor.assumeIsolated { self?.scheduleSync() }
            })
        }
    }

    func scheduleSync() {
        guard !syncScheduled else { return }
        syncScheduled = true
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated {
                self?.syncScheduled = false
                self?.sync()
            }
        }
    }

    /// The slot's frame in its window's coordinates.
    var slotInWindow: CGRect { convert(bounds, to: nil) }

    /// Every clip the slot sits under, in window coordinates: each enclosing
    /// clip view's visible rect, innermost outward, and the content view's.
    var clipsInWindow: [CGRect] {
        var clips: [CGRect] = []
        var view: NSView? = superview
        while let v = view {
            if let clip = v as? NSClipView { clips.append(clip.convert(clip.bounds, to: nil)) }
            view = v.superview
        }
        if let content = window?.contentView { clips.append(content.convert(content.bounds, to: nil)) }
        return clips
    }

    /// Make the reveal window match (isActive, window, geometry). Idempotent.
    func sync() {
        guard isActive, let window else {
            controller.tearDown()
            return
        }
        let slot = slotInWindow
        controller.sync(
            window: window, slotInWindow: slot,
            inView: ScreenCaptureRevealGeometry.isWhollyInView(slot: slot, clips: clipsInWindow))
    }

    func teardown() {
        isActive = false
        controller.tearDown()
    }

    /// The teardown of last resort: a view torn down without a
    /// `viewDidMoveToWindow(nil)` would otherwise strand its reveal window —
    /// a `.none` window that outlives its reveal. `deinit` is nonisolated, so
    /// hop rather than assume.
    deinit {
        for token in observed { NotificationCenter.default.removeObserver(token) }
        let controller = controller
        Task { @MainActor in controller.tearDown() }
    }
}

private struct _ScreenCaptureProbe: NSViewRepresentable {
    let isActive: Bool
    let rootView: AnyView
    /// The slot's SwiftUI-side frame — passed only so a SwiftUI layout change
    /// (a scroll SwiftUI performs without moving the probe's own frame)
    /// re-runs `updateNSView`; the placement itself reads AppKit's geometry.
    let layoutToken: CGRect

    func makeNSView(context: Context) -> _ScreenCaptureProbeView {
        let view = _ScreenCaptureProbeView(rootView: rootView)
        view.isActive = isActive
        return view
    }

    func updateNSView(_ view: _ScreenCaptureProbeView, context: Context) {
        view.controller.update(rootView: rootView)
        view.isActive = isActive
        view.sync()
        view.scheduleSync()
    }

    static func dismantleNSView(_ view: _ScreenCaptureProbeView, coordinator: ()) {
        view.teardown()
    }
}

private struct SuppressScreenCapture<Hosted: View>: ViewModifier {
    let isActive: Bool
    /// The revealed view itself (the call site's `self`), hosted again in the
    /// reveal window. Not `content`: a modifier's `content` is a placeholder
    /// that exists only inside its own body and cannot be hosted elsewhere.
    let hosted: Hosted

    @State private var layoutToken: CGRect = .zero

    private var revealRoot: AnyView {
        AnyView(
            hosted
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                // Opaque: the mask lies directly beneath this window in the app
                // window, and a clear background would paint the value over
                // the bullets. The window background, which is what an eager
                // ScrollView page paints behind the slot.
                .background(Color(nsColor: .windowBackgroundColor))
                // The reveal window's copy is never an element: the slot in the
                // app window carries the id and the accessibility value, so each
                // id registers once, where convention 1 puts it.
                .accessibilityHidden(true))
    }

    func body(content: Content) -> some View {
        content
            // Under the reveal window: the value at opacity 0 (layout and
            // accessibility kept) beneath the mask a capture shows.
            .opacity(isActive ? 0 : 1)
            .overlay {
                if isActive {
                    Text(verbatim: "••••••••")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
                        .accessibilityHidden(true)
                }
            }
            .background(
                _ScreenCaptureProbe(isActive: isActive, rootView: revealRoot, layoutToken: layoutToken)
                    // Hidden from accessibility: every surface this attaches to
                    // is resolved by id in the e2e, and a security control that
                    // perturbed the automation tree would be paid for in someone
                    // else's misdiagnosed test failure.
                    .accessibilityHidden(true))
            .onGeometryChange(for: CGRect.self, of: { $0.frame(in: .global) }) { layoutToken = $0 }
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
    /// Whether this view holds one of `ScreenCaptureBlankArming`'s counts —
    /// kept per view so every entry point is idempotent (the same shape as the
    /// macOS probe's `held`): a double-arm would read as a reveal that never
    /// ended.
    @State private var armed = false

    private func syncArming(_ on: Bool) {
        guard on != armed else { return }
        armed = on
        if on {
            ScreenCaptureBlankArming.shared.arm()
        } else {
            ScreenCaptureBlankArming.shared.disarm()
        }
    }

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
            .onAppear { syncArming(isActive) }
            .onDisappear { syncArming(false) }
            .onChange(of: isActive) { _, now in syncArming(now) }
    }
}

/// How many revealed secrets have the blanking ARMED right now — on screen,
/// `isActive`, and so blanked the moment a capture starts or the app leaves the
/// foreground. The iOS half of the e2e `capture_posture` observable
/// (`ScreenCapturePosture`): iOS has no platform bit to read back, so the
/// harness reads that the control is armed while the secret shows and disarmed
/// once it is hidden or left, and `ScreenCaptureBlankPolicy`'s host tests own
/// the decision itself.
@MainActor
public final class ScreenCaptureBlankArming {
    public static let shared = ScreenCaptureBlankArming()
    public private(set) var count = 0

    init() {}

    func arm() { count += 1 }
    func disarm() { count = max(0, count - 1) }
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

// MARK: - The e2e read-back

/// The `capture_posture` state observable — how the cross-app e2e witnesses
/// rule 2 against the running app rather than against its source
/// (`tests/e2e-unified/helpers/capture_posture.py` owns the reader).
///
/// - `mechanism`: `window-exclusion` where the platform excludes the window
///   from every capture (macOS here; windows' display affinity and android's
///   `FLAG_SECURE` are the same kind), `blank-on-capture` on iOS — the partial
///   control `security.md` names, which does NOT stop a still screenshot. On
///   macOS the excluded windows are the disposable reveal windows, never the
///   app window (`_ScreenCaptureRevealController`), on every release.
/// - `holds`: live suppression holds — on macOS the reveal windows standing
///   right now, on iOS the reveals with the blanking armed.
/// - `window_excluded` (macOS only): the REAL platform bit — whether ANY window
///   of the app reads `sharingType == .none` right now. The strict read on
///   purpose: a reveal window that lingers after its reveal is a leak the
///   witness must catch. Absent on iOS, where no such bit exists, rather than a
///   `false` a reader could mistake for a measured one.
/// - `excluded_windows` (macOS only): those windows, by class and visibility.
@MainActor
public enum ScreenCapturePosture {
    public static let stateKey = "capture_posture"

    public static func observable() -> [String: Any] {
        #if os(macOS)
        // `NSApp` is nil outside a running app (a `swift test` process that
        // builds the state observables): no app, no windows, none excluded.
        let excluded = (NSApp?.windows ?? []).filter { $0.sharingType == .none }
        return [
            "mechanism": "window-exclusion",
            "holds": ScreenCaptureRevealWindows.count,
            "window_excluded": !excluded.isEmpty,
            // Which windows, by class and visibility — what a failing witness
            // prints (convention 6), so "excluded with no hold" names its window.
            "excluded_windows": excluded.map {
                "\(type(of: $0)) visible=\($0.isVisible) main=\($0.canBecomeMain)"
            },
        ]
        #elseif os(iOS)
        return [
            "mechanism": "blank-on-capture",
            "holds": ScreenCaptureBlankArming.shared.count,
        ]
        #else
        return ["mechanism": "absent", "holds": 0]
        #endif
    }
}

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
    /// ⚠ Apply it INNERMOST — directly on the revealed view, with the call
    /// site's `.accessibilityIdentifier` / `.automationValue` /
    /// `.automationActivate` OUTSIDE it. On macOS this view is hosted a second
    /// time, in the reveal window; whatever is applied inside the modifier is
    /// hosted with it, so an id registered inside would register twice.
    ///
    /// ⚠ Never attach it to `secret-key-display` or
    /// `recovery-kit-secret-display` (rule 1, above).
    func suppressScreenCapture(isActive: Bool = true) -> some View {
        #if os(macOS)
        modifier(SuppressScreenCapture(isActive: isActive, hosted: self))
        #else
        modifier(SuppressScreenCapture(isActive: isActive))
        #endif
    }
}
