import AppKit
import ServiceManagement
import FaunaKit

/// Auto-start at sign-in — the macOS leg (`docs/goal/architecture/apps/macos.md`
/// § App Lifecycle → *Auto-start at sign-in*; cross-app intent in `windows.md`,
/// the linux leg in `linux.md`).
///
/// The same split linux (`autostart.rs`) and windows (`AutoStartGate` policy vs.
/// `AutoStartService` mechanics) use: the decisions are pure functions,
/// unit-tested by `AutoStartTests` without touching the machine's login items;
/// the `SMAppService` calls around them are the thin mechanism.
///
/// **Mechanism: `SMAppService.agent(plistName:)`** over the LaunchAgent plist the
/// `Fauna` target ships at `Contents/Library/LaunchAgents/` — it moves and
/// uninstalls with the bundle, carries the `--autostart` argument the hidden
/// launch needs, and its `status` reports a System Settings → Login Items
/// opt-out (`.requiresApproval`) so the toggle can tell the truth.
enum AutoStart {
    /// The argument the registered job passes back to us — "started by the login
    /// session", not by the user. OS wiring, never a user-facing knob; the same
    /// flag linux's `.desktop` entry and windows' Run key carry.
    static let flag = "--autostart"

    /// The persisted tri-state choice: absent = never chosen (register by
    /// default), `false` = an explicit opt-out, `true` = an explicit opt-in.
    static let choiceKey = "fauna.launchAtLogin"

    // MARK: - Pure decisions

    /// Whether the post-auth hook registers at all — linux's `should_register`,
    /// windows' `AutoStartGate.ShouldRegister`, the same truth table: never under
    /// e2e (a harness login must not write the dev machine's login items), never
    /// over an explicit opt-out, otherwise yes.
    static func shouldRegister(choice: Bool?, e2e: Bool) -> Bool {
        !e2e && choice != false
    }

    /// What a settled launch needs, for the hidden-launch decision.
    enum Route: Equatable {
        /// Signed in, the main shell mounted — nothing for the user to do.
        case authenticated
        /// Onboarding or any launch surface that waits on the user.
        case needsUser
    }

    /// The route a launch gate settles on; `nil` while it is still resolving.
    /// Only `.ready` with an account is authenticated — onboarding, a transient
    /// retry and every terminal surface need the user, so a hidden start never
    /// strands a dead session out of sight (fails safe to visible, as linux).
    static func route(gate: LaunchGate, isOnboarded: Bool) -> Route? {
        switch gate {
        case .launching:
            return nil
        case .ready:
            return isOnboarded ? .authenticated : .needsUser
        case .retrying, .needsUpdate, .signInRefused, .identityChanged, .accountIndexUnreadable:
            return .needsUser
        }
    }

    /// Whether a launch stays resident with no main window: only an auto-start
    /// launch, and only once it lands signed in.
    static func shouldStartHidden(autostartFlag: Bool, route: Route) -> Bool {
        autostartFlag && route == .authenticated
    }

    static func isAutostartLaunch(arguments: [String] = CommandLine.arguments) -> Bool {
        arguments.contains(flag)
    }

    /// What the Settings toggle shows: the choice (default on), except that a
    /// Login Items opt-out reads off — that switch is the user's choice too.
    static func displayedChoice(choice: Bool?, osOptedOut: Bool) -> Bool {
        !osOptedOut && (choice ?? true)
    }

    enum EnableAction: Equatable { case register, openSystemSettings }

    /// Turning the toggle on over a Login Items opt-out routes the user to the
    /// OS switch; it never silently re-registers over it.
    static func enableAction(osOptedOut: Bool) -> EnableAction {
        osOptedOut ? .openSystemSettings : .register
    }

    enum PostAuthAction: Equatable { case register, leaveAlone }

    /// The post-auth hook: register a default-on or opted-in install, leave
    /// everything else — e2e, an explicit opt-out, a Login Items opt-out — alone.
    static func postAuthAction(choice: Bool?, e2e: Bool, osOptedOut: Bool) -> PostAuthAction {
        shouldRegister(choice: choice, e2e: e2e) && !osOptedOut ? .register : .leaveAlone
    }

    // MARK: - Mechanism

    static var storedChoice: Bool? {
        UserDefaults.standard.object(forKey: choiceKey) as? Bool
    }

    private static var service: SMAppService {
        SMAppService.agent(plistName: "\(AppleIdentifiers.appLaunchAgent).plist")
    }

    /// True when the user turned Fauna off under System Settings → General →
    /// Login Items. Never asked under e2e, where nothing is registered.
    static var osOptedOut: Bool {
        guard !FaunaE2E.isActive else { return false }
        return service.status == .requiresApproval
    }

    /// The universal post-auth hook's registration step
    /// (`completeAuthenticatedLaunch`, every login and returning-user relaunch),
    /// so the first successful sign-in wires every later login.
    static func registerAtPostAuth() {
        let action = postAuthAction(
            choice: storedChoice, e2e: FaunaE2E.isActive, osOptedOut: osOptedOut)
        if action == .register { register() }
    }

    /// An explicit choice from the Settings toggle: persist the tri-state, then
    /// make the registration match it.
    static func applyUserChoice(_ on: Bool) {
        UserDefaults.standard.set(on, forKey: choiceKey)
        guard !FaunaE2E.isActive else { return }
        guard on else {
            unregister()
            return
        }
        switch enableAction(osOptedOut: osOptedOut) {
        case .register: register()
        case .openSystemSettings: SMAppService.openSystemSettingsLoginItems()
        }
    }

    private static func register() {
        let service = service
        guard service.status != .enabled else { return }
        do {
            try service.register()
        } catch {
            logMessage(level: .warn, target: "fauna.app",
                       message: "[autostart] register failed: \(error.localizedDescription)")
        }
    }

    private static func unregister() {
        let service = service
        guard service.status != .notRegistered else { return }
        do {
            try service.unregister()
        } catch {
            logMessage(level: .warn, target: "fauna.app",
                       message: "[autostart] unregister failed: \(error.localizedDescription)")
        }
    }
}

/// The hidden launch's window half: an `--autostart` launch mounts its main
/// window (the launch runs from its content — `ContentView`'s `.task` — so
/// SwiftUI's `defaultLaunchBehavior(.suppressed)` would never sign in), orders
/// it out at once, and settles when the launch does: authenticated → the window
/// closes, the same resident state as a user's window close; anything that
/// needs the user → it comes to the front.
@MainActor
enum AutoStartWindow {
    private enum State {
        case unarmed
        case holding([NSWindow])
        case settled
    }

    private static var state: State = .unarmed

    /// From the main window's first `.task`, before the launch starts. Once per
    /// process: a later re-open of the window is a plain window.
    static func armIfAutostartLaunch() {
        guard case .unarmed = state else { return }
        guard AutoStart.isAutostartLaunch() else {
            state = .settled
            return
        }
        let windows = NSApp.windows.filter { $0.isVisible && $0.canBecomeMain }
        windows.forEach { $0.orderOut(nil) }
        state = .holding(windows)
        logMessage(level: .info, target: "fauna.app",
                   message: "[autostart] login launch — holding \(windows.count) window(s) until the launch settles")
    }

    /// On every launch-gate write; acts once, on the first settled route.
    static func settle(gate: LaunchGate, isOnboarded: Bool) {
        guard case .holding(let windows) = state,
              let route = AutoStart.route(gate: gate, isOnboarded: isOnboarded) else { return }
        state = .settled
        if AutoStart.shouldStartHidden(autostartFlag: true, route: route) {
            logMessage(level: .info, target: "fauna.app",
                       message: "[autostart] signed in — resident with no window")
            windows.forEach { $0.close() }
        } else {
            logMessage(level: .info, target: "fauna.app",
                       message: "[autostart] the launch needs the user — showing the window")
            windows.forEach { $0.makeKeyAndOrderFront(nil) }
            NSApp.activate(ignoringOtherApps: true)
        }
    }
}
