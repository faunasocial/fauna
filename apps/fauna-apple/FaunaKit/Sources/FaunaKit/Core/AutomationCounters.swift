import Foundation

/// Apple's leg of convention 14's **counter observables** — the two monotonic
/// values the shared negative-assert helper reads
/// (`docs/goal/architecture/e2e-conventions.md` § convention 14; the contracts
/// live in ONE home, `fauna_e2e_agent::SESSION_GENERATION_KEY` and
/// `fauna_e2e_agent::ACTIVATION_GESTURES_KEY`).
///
/// **Why these are NOT under `Testing/` and NOT `#if DEBUG`,** unlike
/// `BarrierTestCommand` and its siblings: convention 15 compiles out the
/// automation *surface* — agents, command handlers, in-process servers — and
/// the surface here is each shell's `serializeState()` / `handleTestCommand`,
/// which is already gated. What is left is two `Int`s bumped by **production
/// teardown code** (`tearDownSessionForSwitch`, `StatusVM`-driven sign-out, the
/// account-switch gate). A `#if DEBUG` counter would force every one of those
/// call sites behind its own `#if`, which is how a teardown arm eventually gets
/// added without one — the exact silent-undercount this file exists to prevent.
/// tui and linux made the same call: tui's counter is a plain field on the
/// production `App`, linux's a plain `static` in a module compiled in release.
///
/// Both counters are `@MainActor`-isolated because every writer already is (the
/// teardown paths and the switch gate all run on the main actor), so no locking
/// is needed and none is implied.
@MainActor
public enum SessionGeneration {
    /// How many authenticated-session teardowns this app **process** has
    /// initiated — the value published at `state.session_generation`.
    ///
    /// Deliberately NOT cleared by the `reset` / `logout` test commands, unlike
    /// `BarrierTestCommand`'s per-test probe slots: those are scratch, this
    /// counts teardowns, and a `reset` **is** one — zeroing it there would erase
    /// the very event the counter exists to report. Tests read a delta across
    /// their own gesture, so an accumulating value costs them nothing.
    public private(set) static var count: Int = 0

    /// Count one **initiated** authenticated-session teardown.
    ///
    /// ⚠ **Call this synchronously at the point the app COMMITS to tearing the
    /// session down — never from inside whatever the teardown defers.** On
    /// apple the commit point is the top of each shell's teardown function
    /// (`tearDownSessionForSwitch`, `resetToFactory`, `logoutKeepData`,
    /// `factoryResetReonboard`): the callers ahead of them can still bail (the
    /// `switchInFlight` guard; a `setActive` that throws leaves the live session
    /// untouched), and everything after them is synchronous. The FP sign-out and
    /// `runLaunch()` that follow in a `Task` are the teardown's *effect*, not its
    /// decision — a counter bumped there would be invisible to a `barrier` that
    /// had already returned, and every negative assert reading it would silently
    /// revert to the race it replaced (`fauna_e2e_agent::SESSION_GENERATION_KEY`
    /// documents the same trap on linux, where the deferral is a 100 ms glib
    /// timeout).
    ///
    /// Bump inside the teardown FUNCTIONS rather than at their call sites, so a
    /// teardown arm added later counts itself — tui's property, and the reason
    /// its counter sits in the one seam every arm shares.
    public static func recordTeardown() {
        count &+= 1
    }
}

/// Completed **account-activation gestures** — apple's leg of the counter the
/// three native-prompt apps need and tui/linux/web do not
/// (`fauna_e2e_agent::ACTIVATION_GESTURES_KEY` owns the contract; windows landed
/// the first leg, and the name, on 2026-08-13).
///
/// Their re-auth prompt is an in-app element, so `assert_no_relaunch`'s required
/// `settled` argument is just "the prompt closed". Apple's is the **OS** sheet
/// (`AccountReauth`, `LAContext`) — nothing renders, so nothing closes, and on a
/// decline the gesture's entire visible effect is a log line. This counter is the
/// honest completion observable in its place.
///
/// ⚠ **Every arm counts, including the ones that decide nothing** — the tap on an
/// already-active row, the unflagged straight-through, and the decline. The key's
/// contract is "one tap, counted when the handler returns, *whatever* it
/// decided", and that is what lets a caller wait on it without knowing which arm
/// its tap hit; a test that had to know would be reading the app's internals to
/// test the app.
@MainActor
public enum ActivationGestures {
    /// Published at `state.activation_gestures`.
    public private(set) static var count: Int = 0

    /// Record that one activation gesture's handler has finished.
    ///
    /// ⚠ The **last** statement of the handler, after any verdict has been
    /// dispatched — see the key's doc comment for why counting first would let
    /// the barrier behind it anchor to the click's dispatch instead.
    public static func recordCompleted() {
        count &+= 1
    }
}

/// Handled **LaunchServices reopen events** — macOS's observable for the
/// plain-launch raise layer (`account-scoping.md` § Concurrent instances:
/// LaunchServices deduplicates a second launch of a running bundle id and hands
/// the running instance a reopen instead of starting a process), and the twin
/// of linux's `raises_served` state key: platform-specific observables for
/// platform-specific raise mechanisms, so like `raises_served` it has no shared
/// `fauna_e2e_agent` key constant.
///
/// macOS-only by construction — iOS has no reopen concept (one instance per
/// app, structurally), so only the macOS shell bumps or serializes it
/// (`state.reopens_handled`). It lives here beside the other counters for the
/// same reason they are not `#if DEBUG`: the writer is production AppKit
/// delegate code (`AppDelegate.applicationShouldHandleReopen`), and the gated
/// surface is `serializeState()` itself.
///
/// ⚠ **Every reopen counts, whatever the visible-windows flag said** — the
/// counter answers "did the raise arrive and get handled", not "which arm ran"
/// (`ActivationGestures`' rule, same reason: a test that had to know the arm
/// would be reading the app's internals to test the app).
@MainActor
public enum ReopensHandled {
    /// Published at `state.reopens_handled` (macOS only).
    public private(set) static var count: Int = 0

    /// Count one handled reopen event.
    public static func recordHandled() {
        count &+= 1
    }
}
