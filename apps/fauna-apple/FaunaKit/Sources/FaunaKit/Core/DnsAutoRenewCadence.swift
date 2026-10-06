import Foundation

/// Native background auto-renew cadence for TLS certificates (macOS + iOS, shared
/// FaunaKit — priority #2). The apple analog of linux
/// `client.rs::run_auto_renew_cadence_tick` (spawned in `start_ws_rpc`): every
/// `autoRenewPollSecs()`, run the shared `DnsManagementMachine.autoRenewScan()` /
/// `.autoRenewIssue(domains:targetNestId:)` pair — the refresh-then-ask order,
/// the skip-if-empty rule, the per-domain non-fatal rule and the trailing
/// health re-read all live in `fauna_client_dns`, so every native app
/// runs the identical sequence rather than a per-app hand-rolled copy. The web
/// SPA runs no such timer (a browser has no background loop), so this is
/// native-only; the at-risk push (§ C.4) is the *wake* for a closed mobile app,
/// after which this loop (or a manual page open) renews.
///
/// The first tick fires after the *full* interval — a fresh session / e2e run is
/// never disturbed (matching linux — `autoRenewPollSecs()`'s own doc comment).
/// Best-effort: every error is swallowed (the next manual page open or the
/// at-risk push covers a missed window).
@MainActor
public final class DnsAutoRenewCadence {
    public static let shared = DnsAutoRenewCadence()

    private var loop: Task<Void, Never>?

    /// Whether a loop is currently armed. Exists so `ActorScopeTests` can pin that
    /// a teardown actually disarms this cadence — the latch in `start` below makes
    /// that load-bearing rather than cosmetic, and nothing else can observe it.
    var isRunning: Bool { loop != nil }

    /// How many times `start` has actually armed a loop — incremented only when the
    /// latch below *lets it through*. `isRunning` alone cannot witness the latch (it
    /// reads `true` whether `start` rebound or was swallowed), so this is what lets
    /// `ActorScopeTests` pin the real mechanism: a second `start` without an
    /// intervening `stop` must NOT arm, which is why a teardown that skips the drop
    /// leaves this cadence bound to the outgoing `APIClient` for good.
    private(set) var armCount = 0

    private init() {}

    /// Idempotent — starts the loop once per launched session. Call **after** the
    /// actor secret is primed (post-login, e.g. right after the conversations
    /// session activates), so the credentialed `DnsManagementMachine` can build.
    public func start(api: APIClient) {
        guard loop == nil else { return }
        armCount += 1
        loop = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: autoRenewPollSecs() * 1_000_000_000)
                if Task.isCancelled { return }
                await self?.tick(api: api)
            }
        }
    }

    public func stop() {
        loop?.cancel()
        loop = nil
    }

    /// One cadence tick: the shared `autoRenewScan()` / `autoRenewIssue(domains:
    /// targetNestId:)` pair owns the refresh-then-ask order, the skip-if-empty
    /// rule, the per-domain non-fatal rule and the trailing health re-read —
    /// this wrapper owns only what is genuinely apple's: resolving
    /// `targetNestId` (linked-nests state, deliberately outside the DNS
    /// machine, same split as linux's `run_auto_renew_cadence_tick`).
    private func tick(api: APIClient) async {
        guard let machine = try? await api.dnsManagementMachine() else { return }
        let due = await machine.autoRenewScan()
        guard !due.isEmpty else { return }
        guard let nestId = try? await api.thisNestId() else { return }
        _ = await machine.autoRenewIssue(domains: due, targetNestId: nestId)
    }
}
