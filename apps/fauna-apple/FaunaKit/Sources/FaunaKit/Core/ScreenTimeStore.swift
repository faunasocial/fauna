import Foundation
#if os(macOS)
import AppKit
#else
import UIKit
#endif

/// The ward's **screen-time lock** state — the apple twin of android's
/// `@Singleton ScreenTimeStore` / linux's `screen_lock.rs` / web's
/// `screenTime.svelte.ts`. Holds the [`FfiUsageHeartbeat`] handle for the
/// session and exposes the one [`lockMessage`](Self.lockMessage) the global
/// `screen-time-lock` overlay renders, so the gate and its wording can never
/// drift apart.
///
/// **Client-enforced by construction.** The nest cannot see when a child's
/// device is in use and deliberately does not gate on it, so this store IS
/// the enforcement. **Every decision is shared Rust** — this class holds no
/// policy logic at all: whether to lock, and what the lock says, are one call
/// to `screenLockMessage`, which folds the policy, the device's local clock
/// and the day's cross-device total.
///
/// `@Observable`, held as a stored property on both `AppState` (iOS) and
/// `MacAppState` (macOS) — the `ContentPolicyStore`/`FamilyStatusStore`
/// shape, not `GuardianNotifyCadence`'s `.shared` singleton, because SwiftUI
/// must observe `lockMessage` reactively and each shell already constructs
/// its own state object. Deliberately **not** class-level `@MainActor`
/// (unlike `GuardianNotifyCadence`): a `.shared` static's lazy init needs no
/// isolated caller, but `AppState`/`MacAppState` construct this as a plain
/// stored-property default value in a synchronous, non-isolated context —
/// exactly why `ContentPolicyStore` isn't class-level `@MainActor` either.
/// Every method is individually `@MainActor` instead (a bare `init()` is the
/// one thing that stays unmarked), which keeps the type's actual isolation
/// contract the same as a `@MainActor` class without touching how `init()`
/// is called.
@Observable
public final class ScreenTimeStore {
    /// The current lock verdict, resolved to display text, or `nil` for "not
    /// locked" — the single decision point the global overlay renders from.
    public private(set) var lockMessage: String?

    /// How often the tick re-evaluates the lock AND drives the heartbeat —
    /// matches `fauna_core::screen_time::MAX_ACCRUAL_STEP_SECS` (2 min): a
    /// slower tick would silently under-count (the engine credits at most one
    /// accrual step per call), and the policy's window/budget bounds are
    /// whole minutes anyway, so no finer tick could change a verdict.
    private static let tickIntervalSeconds: UInt64 = 60

    private let heartbeat = FfiUsageHeartbeat()
    private var policy: FfiScreenTimePolicy?
    private var guardianHandle: String?
    private var loop: Task<Void, Never>?
    private var api: APIClient?

    /// Test-only clock skew in seconds (`testing.md` § convention 14's fake
    /// clock), the apple twin of linux `advance_test_clock` / android
    /// `testClockSkewSecs`. Stays 0 in production; only the debug-only
    /// `screen_time_heartbeat` TestAgent command writes it via
    /// [`advanceTestClockAndTick`](Self.advanceTestClockAndTick).
    private var testClockSkewSecs: Int64 = 0

    public init() {}

    /// Idempotent — starts the one-minute tick once per launched session.
    /// Call alongside `GuardianNotifyCadence.shared.start(api:)` at the same
    /// post-login trigger — AND from the e2e session-patch path directly
    /// (mirrors `criticalAlertsHost.startSweepLoop`): that path never runs
    /// the production launch closure, so without an explicit call there this
    /// store's `api` stays `nil` for every e2e-logged-in ward.
    @MainActor
    public func start(api: APIClient) {
        guard loop == nil else { return }
        self.api = api
        loop = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: Self.tickIntervalSeconds * 1_000_000_000)
                if Task.isCancelled { return }
                await self?.tick()
            }
        }
    }

    /// Drop everything on an identity change — sign-out, account switch,
    /// factory reset. Without this a new account inherits the previous
    /// ward's lock/accrual, naming a guardian the user does not have.
    @MainActor
    public func stop() {
        loop?.cancel()
        loop = nil
        api = nil
        policy = nil
        guardianHandle = nil
        testClockSkewSecs = 0
        heartbeat.reset()
        lockMessage = nil
    }

    /// Whether a loop is currently armed — exists so a teardown test can pin
    /// that it actually disarms, mirroring `GuardianNotifyCadence.isRunning`.
    @MainActor
    var isRunning: Bool { loop != nil }

    /// The currently-recorded policy/guardian — exist so a `seed` test can pin
    /// the restore actually landed (mirrors `isRunning`). `lockMessage` alone
    /// cannot witness this for a budget-only policy: a budget lock also needs
    /// a nest-CONFIRMED usage total, which `seed` deliberately never restores
    /// (only a real `usageReport` round trip sets it), so these are the only
    /// window into `seed`'s effect that needs neither a live network call nor
    /// the real clock.
    @MainActor
    var policyForTest: FfiScreenTimePolicy? { policy }
    @MainActor
    var guardianHandleForTest: String? { guardianHandle }

    /// Restore (or clear) the window/guardian half from the persisted
    /// last-known supervision snapshot, ahead of the first `refresh` landing
    /// (`family-safety.md` § Content policy, clause 2). The day's cross-device
    /// usage total is deliberately NOT restored — it re-arrives with the
    /// first successful read (the ratified fail-open on the budget arm;
    /// android's `setWardScreenTime(policy, guardian, null)` is the reference
    /// shape). `nil` clears to unlocked, which is what makes this safe to
    /// call at every session establish, including the bare `client == nil`
    /// teardown phase — see `seedSupervisionSnapshot`.
    @MainActor
    func seed(from snapshot: FfiSupervisionSnapshot?) {
        apply(snapshot, usageTodayMinutes: nil)
    }

    /// Re-read `fauna.family.status` and record the ward's own screen-time
    /// policy, guardian, and the day's cross-device usage total. Call
    /// alongside `ContentPolicyStore.refresh(api:)` at the same post-auth +
    /// reconnect trigger, so a guardian's policy edit takes effect on the
    /// ward's next read rather than needing a restart.
    ///
    /// **Keep-on-failure, not fail-closed-to-unlocked**
    /// (`family-safety.md` § Screen time: "a failed status read never
    /// unlocks, never clears a window or budget" — the same unfetched-policy
    /// ruling `ContentPolicyStore` follows for the content floor). A `nil`
    /// api (not yet wired, or dropped) and a failed read both leave the
    /// last-known policy/guardian/lock in force; only a SUCCESSFUL read
    /// carrying no supervision fold (unsupervised) may clear the lock.
    @MainActor
    public func refresh(api: APIClient?) async {
        guard let api, let status = try? await api.familyStatus() else { return }
        land(status)
    }

    /// A SUCCESSFUL status read's landing: the window/budget policy and the
    /// lock's guardian come off the reply's supervision fold, never its raw
    /// `policy`. The shared `SupervisionSnapshot::from_status` gates both on a
    /// named guardian (the graduation gate), so a reply that still carries a
    /// policy document but names no guardian locks nothing
    /// (`family-client-enforcement.md` § Implementation status today). The
    /// day's usage total rides the reply itself — it is not a supervision
    /// input, so the fold does not carry it. `internal` so a test can land a
    /// fixed reply without a live `APIClient`.
    @MainActor
    func land(_ status: FfiFamilyStatus) {
        apply(status.supervision, usageTodayMinutes: status.usageTodayMinutes)
    }

    /// The one mapping both doors onto the lock route through — `seed` (a
    /// restore, usage total unknown) and `land` (a live read) — so a restore
    /// and a read cannot disagree on what the lock is fed.
    @MainActor
    private func apply(_ snapshot: FfiSupervisionSnapshot?, usageTodayMinutes: UInt32?) {
        policy = snapshot?.screenTime
        guardianHandle = snapshot?.supervisedBy.handle
        heartbeat.setPolicy(policy: policy)
        heartbeat.seedTotal(usageTodayMinutes: usageTodayMinutes)
        recompute()
    }

    /// The ward's own usage figure for their read-only summary — the same
    /// number the guardian sees (§ Screen time transparency rule). `nil`
    /// when no total has been heard yet.
    @MainActor
    public func usedTodayMinutes() -> UInt32? {
        heartbeat.usedTodayMinutes(nowSecs: nowSecs())
    }

    @MainActor
    private func nowSecs() -> Int64 {
        Int64(Date().timeIntervalSince1970) + testClockSkewSecs
    }

    /// The device's local clock as minutes from local midnight — the unit
    /// `FfiScreenTimePolicy` stores its window bounds in. Deliberately reads
    /// through `nowSecs()` (not a fresh OS clock read), so the test clock
    /// moves the *window* verdict too, not only the *budget* one — the
    /// property linux's own `local_minutes_from_midnight` calls out as the
    /// thing a real client-side bug once broke by bypassing it.
    @MainActor
    private func localMinutesFromMidnight() -> UInt16 {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .current
        let comps = calendar.dateComponents(
            [.hour, .minute], from: Date(timeIntervalSince1970: TimeInterval(nowSecs())))
        return UInt16((comps.hour ?? 0) * 60 + (comps.minute ?? 0))
    }

    /// Re-evaluate the lock verdict and publish it. The single decision
    /// point: both the overlay's visibility and its text come from here.
    @MainActor
    private func recompute() {
        guard let guardianHandle else {
            lockMessage = nil
            return
        }
        let text = screenLockMessage(
            policy: policy,
            nowLocalMinutes: localMinutesFromMidnight(),
            usedTodayMinutes: heartbeat.usedTodayMinutes(nowSecs: nowSecs()),
            guardianHandle: guardianHandle)
        lockMessage = text.map(renderLocalizedText)
    }

    /// Whether the app is actually being looked at right now — foregrounded
    /// AND active. `#if os` rather than a shell-supplied signal: every method
    /// on this type already needs the local clock (native-only, WASM-safe
    /// shared Rust owns none), so reading the platform's own foreground state
    /// directly here keeps both shells' launch glue unchanged.
    @MainActor
    private func isAppActive() -> Bool {
        #if os(macOS)
        return NSApplication.shared.isActive
        #else
        return UIApplication.shared.applicationState == .active
        #endif
    }

    /// Drive the heartbeat one step and, if the shared engine says a report
    /// is due, send `fauna.family.usage_report` and land the reply. `active`
    /// is whether the app is being used right now: foregrounded/active AND
    /// the lock is not showing — lock-screen time is not use, crediting it
    /// would inflate the guardian's readout with minutes the child never
    /// spent. A failure re-credits the minutes rather than forgiving them
    /// (the delta is defined against the last *successful* report).
    @MainActor
    private func heartbeatStep(active: Bool) async {
        let now = nowSecs()
        heartbeat.setActive(active: active, nowSecs: now)
        guard let minutes = heartbeat.takeDue(nowSecs: now) else { return }
        guard let api else { return }
        do {
            let reply = try await api.familyClient().usageReport(
                minutes: minutes, utcOffsetMinutes: DeviceOffset.utcOffsetMinutes())
            heartbeat.reportSucceeded(
                day: reply.day, dayTotalMinutes: reply.dayTotalMinutes, nowSecs: nowSecs())
        } catch {
            heartbeat.reportFailed()
        }
        recompute()
    }

    /// The one-minute tick (also the shared engine's own accrual-step
    /// requirement of a caller — a slower tick would silently under-count).
    @MainActor
    private func tick() async {
        guard heartbeat.isAccounting() else { return }
        await heartbeatStep(active: isAppActive() && lockMessage == nil)
    }

    /// Test-only: advance the clock by `minutes` of foreground use, in the
    /// accrual steps a real caller would tick in, then run one production
    /// heartbeat step — the apple twin of linux `advance_test_clock` +
    /// `flush_usage_report(true)` / android `advanceTestClockAndTick`.
    /// `active = true` throughout: the poke asserts what a ward actively
    /// using the app accrues. Driven only by the debug-only TestAgent's
    /// `screen_time_heartbeat` command (`testing.md` § convention 14's fake
    /// clock + run_now poke; § convention 15 keeps this whole path out of
    /// release artifacts via the shell's own `#if DEBUG` command surface).
    @MainActor
    public func advanceTestClockAndTick(minutes: Int) async {
        let step: Int64 = 120 // fauna_core::screen_time::MAX_ACCRUAL_STEP_SECS
        var remaining = Int64(max(minutes, 0)) * 60
        // Prime the engine's reference point before advancing — it accrues
        // from the GAP between calls, so with no prior call the first step
        // would credit nothing and the poke would silently deliver less use
        // than it was asked for.
        heartbeat.setActive(active: true, nowSecs: nowSecs())
        while remaining > 0 {
            let bump = min(remaining, step)
            testClockSkewSecs += bump
            remaining -= bump
            heartbeat.setActive(active: true, nowSecs: nowSecs())
        }
        await heartbeatStep(active: true)
    }
}
