import Foundation

/// Reference-type holder for the launch `LaunchMachine` this process is currently
/// routing on, held via `@State` on `FaunaMacApp`/`FaunaApp` (mirrors
/// `MachineMethodResultBox`, `conversationsVM`, `feedVM`: a class instance mutated
/// in place, never a plain value reassigned).
///
/// **The indirection is load-bearing, not stylistic.** The trust button on the
/// blocking `launch_identity_changed` surface must call `trustNestIdentity()` on the
/// machine that PRODUCED the verdict — a machine in any other phase returns without
/// side effects by design (`fauna_launch_machine::LaunchMachine::trust_nest_identity`:
/// "Meaningful only from `IdentityChanged`", so the pin is never forgotten outside
/// that user-approved action). A machine handle that fails to reach the live view
/// therefore does not merely go missing; it silently substitutes a *different*
/// machine and turns the button into a no-op.
///
/// That is exactly what a bare `@State private var launchMachine: LaunchMachine?`
/// did on both Apple apps. `startInProcessAgentIfNeeded()`/`startTestAgentIfNeeded()`
/// wire the bridge's `commandHandler`/`stateProvider` closures from inside `init()`,
/// capturing `self` before SwiftUI has installed the live `@State` storage for the
/// instance that renders `body`; per Apple's own guidance `@State` must never be read
/// or written from `init()`, and an assignment made through that init-time `self`
/// silently no-ops (the full statement lives on `MachineMethodResultBox`). So every
/// launch the *post-auth* escalation re-enters — `performPostAuthSilentSignIn()` →
/// `tearDownSessionForSwitch()` → `runLaunch()`, which is what the `silent_sign_in`
/// test command drives — installed its new machine into a dead copy. The live view
/// kept the boot machine, still `Online`; the trust button re-dispatched *that*
/// snapshot, `completeAuthenticatedLaunch()` rebuilt the session over a pin that had
/// never been forgotten, and its own tail-end post-auth re-check escalated straight
/// back to the blocking surface — a loop, observed as
/// `test_nest_identity_pin_post_auth.py::test_post_auth_identity_change_warns_then_recovers`
/// timing out in `reached_authenticated_app` after the click, deterministically. The same lost write also defeated the
/// `reset_app_state` test command's "launch surfaces are launch-scoped, not
/// app-scoped" clear, handing a stale machine to the next test in the module.
///
/// Class *identity* is what `@State` actually shares across the struct copies here,
/// which is why mutating a property on this box works from the init-time closure
/// while reassigning a plain `@State` value does not. Shared by macOS/iOS
/// (`FaunaMacApp`/`FaunaApp`) — one holder, no per-app divergence.
public final class LaunchMachineBox {
    /// The machine for the launch currently on screen, or `nil` when no launch is in
    /// flight. Written by `runLaunchMachine()` and cleared by every teardown path.
    ///
    /// ⚠ **It is also the freshness token a launch verdict is checked against, and
    /// both apple targets do check it.** A `LaunchMachine` decides from a store read
    /// taken at `start()` and renders that decision one `await` later, so anything
    /// that becomes a NEWER session in between — another `runLaunch()`, a teardown, or
    /// the e2e session patch — leaves a verdict in flight that was already wrong when
    /// it was computed. Rendering it walks the app backwards: measured on macOS
    /// 2026-09-21 as a launch that read "no identity", lost the race to an injected
    /// admin session (`applySessionPatch` built the client, the WS connected, the
    /// account store mounted), and then landed its `wizardAt(identityChoice)` on top,
    /// so the shell it had just authenticated into became the wizard again and stayed
    /// there.
    ///
    /// So the rule, one sentence, and the reason this property is the mechanism: a
    /// launch renders its verdict only while it is still the launch this box holds.
    /// Clearing the box is how every superseding path — teardown, reset, session patch
    /// — retires an in-flight launch without having to reach its `Task`, and installing
    /// a new machine retires the previous one for free.
    ///
    /// Every clear also advances ``generation`` — even a clear of an already-empty
    /// box, which is the whole point (see ``beginLaunch()``).
    public var machine: LaunchMachine? {
        didSet { if machine == nil { generation &+= 1 } }
    }

    /// Advanced by every clear of ``machine``; what ``beginLaunch()`` tokens compare to.
    public private(set) var generation: UInt64 = 0

    /// Claim the launch slot SYNCHRONOUSLY, before the launch's first `await`, and
    /// hand back its freshness token.
    ///
    /// ⚠ **The machine identity check alone has a window, and macOS fell in it.**
    /// `FaunaMacApp.runLaunch()` must resolve the launch binding (primary or bound)
    /// before it can build its machine, so for that one `await` the launch was in
    /// flight while the box still held nothing. A session patch landing then cleared
    /// an empty box, retired nothing, and the launch went on to install its machine
    /// and run `completeAuthenticatedLaunch()` over the patched-in session — measured
    /// on a macOS builder seat as two live conversations sessions in one process, the
    /// first's MLS replica load refused as "handed over to a newer engine", two index
    /// builders resumed. A launch that claims here is retired by that clear, because a
    /// clear advances ``generation`` whether or not a machine was installed yet.
    public func beginLaunch() -> UInt64 {
        machine = nil
        return generation
    }

    /// Whether the launch that ``beginLaunch()`` returned `launch` to is still the
    /// current one — nothing has cleared the box or begun a newer launch since.
    public func isCurrent(launch: UInt64) -> Bool {
        launch == generation
    }

    public init() {}
}

extension LaunchPhase {
    /// The phase's case name alone, for diagnostics.
    ///
    /// Deliberately payload-free: `String(describing:)` on this enum would splice
    /// `IdentityChanged`'s `pinnedHex`/`seenHex` fingerprints (and every other case's
    /// associated values) into whatever consumes it. Nothing there is secret — a
    /// `nest_actor_id` is public key material — but a log line is not the place to
    /// start printing identity blobs, and a stable one-word name is what a reader
    /// (or a `grep`) actually wants.
    public var diagnosticName: String {
        switch self {
        case .boot: return "boot"
        case .hydrating: return "hydrating"
        case .silentChallenge: return "silentChallenge"
        case .refreshing: return "refreshing"
        case .online: return "online"
        case .offline: return "offline"
        case .wizardAt: return "wizardAt"
        case .identityChanged: return "identityChanged"
        }
    }
}
