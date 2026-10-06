import SwiftUI

/// Process-wide owner of the cross-page critical-alerts registry
/// (`docs/goal/behavior/critical-alerts.md`) — apple's twin of android's
/// `CriticalAlertsHost` / windows' `CriticalAlertsHost.Instance`. Held at the
/// App root (`FaunaMacApp`/`FaunaApp`, like `feedVM`/`conversationsVM`) and
/// injected via `.environment(...)`, so the banner and any TestAgent command
/// reach the SAME instance. `criticalAlertsRegistry()` hands back the same
/// process-wide `Arc` every call — one process = one registry, so subscribing
/// once here covers every feeder that posts to it (today: `AtprotoSettingsMachine`'s
/// S4-C custody check), with no wiring on this class's part.
@MainActor @Observable
public final class CriticalAlertsHost {
    private let registry = criticalAlertsRegistry()
    private let observerBox = CriticalAlertsObserverBox()

    public private(set) var active: [CriticalAlertRow] = []

    /// Which identity the live sweep loop covers — the loop/one-pass split's
    /// record (see `startSweepLoop`). Internal for `CriticalAlertsLoopClaimTests`;
    /// ignored by observation, since no view renders it.
    @ObservationIgnored var loopClaim = CriticalAlertsLoopClaim()

    public init() {
        observerBox.target = self
        registry.subscribe(observer: observerBox)
        active = registry.active()
    }

    fileprivate func onRegistryChanged() {
        active = registry.active()
    }

    /// Identity-teardown boundary (sign-out, account switch, factory reset) —
    /// critical-alerts.md § Mechanism → *Lifetime*. Also the sweep loop's own
    /// stop signal (`CriticalAlerts::teardown_epoch`), which is why the loop
    /// record drops here too: the two can never disagree.
    public func clearAll() {
        registry.clearAll()
        loopClaim.forget()
    }

    /// The post-auth hook's one entry point, and the split every app makes
    /// (critical-alerts.md § Implementation status today, the 2026-09-24 windows
    /// entry): a first or identity-changing sign-in starts the session-start +
    /// periodic re-sweep LOOP (§ Mechanism → *How often*); a same-identity
    /// re-establish runs ONE pass, because that identity's loop is still running.
    ///
    /// The loop cannot be restarted instead: a Swift `Task.cancel()` never
    /// reaches a UniFFI future (`uniffiRustCallAsync` polls to completion), so
    /// cancelling the previous loop's task — this method's old shape — left that
    /// loop running and stacked a second per re-auth. Its connection outlives an
    /// `APIClient` rebuild too (the Rust loop holds its own `Arc`), so the first
    /// loop keeps covering the identity. Each loop runs on a task of its own,
    /// never a view-scoped one: it returns only once the identity tears down.
    public func startSweepLoop(api: APIClient) {
        guard let identity = api.sweepIdentityKey else {
            logMessage(level: .warn, target: "fauna.critical_alerts",
                       message: "sweep not started: the API client holds no secret")
            return
        }
        guard let token = loopClaim.claim(identity) else {
            Task {
                do {
                    try await api.runCriticalAlertSweepOnce()
                } catch {
                    logMessage(level: .warn, target: "fauna.critical_alerts", message: "sweep pass error: \(error)")
                }
            }
            return
        }
        Task { [weak self] in
            do {
                try await api.startCriticalAlertSweepLoop()
            } catch {
                logMessage(level: .warn, target: "fauna.critical_alerts", message: "sweep loop error: \(error)")
            }
            // Ended (a failed connect, or the teardown): it no longer covers
            // its identity, so the next sign-in must start one.
            self?.loopClaim.release(token)
        }
    }
}

/// Which identity — the actor AT a nest — the live sweep loop covers. The
/// windows twin is `CriticalAlertsSweep.ClaimLoop`/`ReleaseLoop`/`ForgetLoop`.
struct CriticalAlertsLoopClaim {
    private var key: String?
    private var token: UUID?

    /// A token when `identity` has no live loop (the caller starts one, owning
    /// the token); `nil` when it has one (the caller runs a single pass).
    mutating func claim(_ identity: String) -> UUID? {
        if key == identity { return nil }
        let fresh = UUID()
        key = identity
        token = fresh
        return fresh
    }

    /// The loop `token` owns has ended. Only if it is still the recorded loop:
    /// a departed identity's loop ending late must not clear its successor's.
    mutating func release(_ ended: UUID) {
        guard token == ended else { return }
        key = nil
        token = nil
    }

    /// The identity is gone: the next sign-in starts a fresh loop.
    mutating func forget() {
        key = nil
        token = nil
    }
}

/// Trampoline conforming to UniFFI's `CriticalAlertsObserver` — mirrors
/// `AtprotoSettingsObserverBox` (`AtprotoSettingsVM.swift`).
private final class CriticalAlertsObserverBox: CriticalAlertsObserver, @unchecked Sendable {
    weak var target: CriticalAlertsHost?
    func onChanged() {
        notifyOnMainActor(target) { $0.onRegistryChanged() }
    }
}

/// Permanent cross-page critical-alert banner (`docs/goal/behavior/critical-alerts.md`
/// § Severity bar + § Mechanism). Present iff ≥1 alert is active, on every
/// authenticated page — an alert disappears only when its condition is
/// re-checked and found resolved, never from a user gesture, so unlike
/// `MessageBanner`'s rows these carry no dismiss control. Destructive-styled
/// and deliberately plain — visual polish is a per-app follow-on, not part of
/// the rendering contract. Mirrors android's `CriticalAlertsBanner` /
/// `CriticalAlertsBannerContent` split.
public struct CriticalAlertsBanner: View {
    let host: CriticalAlertsHost?

    public init(host: CriticalAlertsHost?) {
        self.host = host
    }

    public var body: some View {
        if let host, !host.active.isEmpty {
            VStack(spacing: 0) {
                ForEach(Array(host.active.enumerated()), id: \.element.key) { _, row in
                    let text = row.lines.map(renderLocalizedText).joined(separator: " ")
                    automationText(Ids.criticalAlert, text)
                        .font(.callout)
                        .foregroundStyle(.white)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 16)
                        .padding(.vertical, 10)
                        .background(Color(red: 0.69, green: 0, blue: 0.125))
                }
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.criticalAlerts)
            .automationValue(Ids.criticalAlerts, text: { "\(host.active.count)" })
        }
    }
}
