// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
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
    /// stop signal (`CriticalAlerts::teardown_epoch`).
    public func clearAll() {
        registry.clearAll()
    }

    /// The post-auth hook's one entry point: one call to the shared
    /// `startCriticalAlertSweep` (critical-alerts.md § Mechanism → *Who runs the
    /// detector* / *How often the detector runs*), which makes the loop /
    /// one-pass split itself, keyed on the identity AND the client — so a
    /// same-identity re-establish that retired the old client gets a loop on the
    /// new one, and the shell keeps no record. Runs on a task of its own, never
    /// a view-scoped one: it returns only once the loop ends or one pass is done.
    public func startSweepLoop(api: APIClient) {
        Task {
            do {
                try await api.startCriticalAlertSweepForSession()
            } catch {
                logMessage(level: .warn, target: "fauna.critical_alerts", message: "sweep error: \(error)")
            }
        }
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
#endif // !FAUNA_KIDS
