// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
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

    #if DEBUG
    /// What ends the armed loop's current wait early — the e2e seam behind
    /// `fauna_e2e_agent::AUTO_RENEW_WAKE` (tui's `LoopWake` is the reference;
    /// convention 14). Minted fresh by every `start` and dropped by `stop`, so
    /// a departed session's loop, which lingers until its wait ends, holds a
    /// handle nothing fires any more. Compiled out of a Release build
    /// (convention 15), whose loop waits on its clock alone.
    private var wake: CadenceWake?
    #endif

    private init() {}

    /// Idempotent — starts the loop once per launched session. Call **after** the
    /// actor secret is primed (post-login, e.g. right after the conversations
    /// session activates), so the credentialed `DnsManagementMachine` can build.
    public func start(api: APIClient) {
        guard loop == nil else { return }
        armCount += 1
        #if DEBUG
        let wake = CadenceWake()
        self.wake = wake
        #endif
        loop = Task { [weak self] in
            while !Task.isCancelled {
                #if DEBUG
                await wake.sleep(nanoseconds: autoRenewPollSecs() * 1_000_000_000)
                #else
                try? await Task.sleep(nanoseconds: autoRenewPollSecs() * 1_000_000_000)
                #endif
                if Task.isCancelled { return }
                await self?.tick(api: api)
            }
        }
    }

    public func stop() {
        loop?.cancel()
        loop = nil
        #if DEBUG
        wake = nil
        #endif
    }

    #if DEBUG
    /// `auto_renew_wake`: end the armed loop's current wait, so its own body
    /// runs the next pass now — the caller's barrier is that pass's
    /// `auto-renew pass:` log line, never this return. Returns the refusal
    /// sentence, or `nil` once the wake landed (convention 11: a seam that did
    /// not land is refused loudly, never acked). No loop is armed before
    /// sign-in, so a pre-auth wake is refused.
    public func wakeForTest() -> String? {
        guard let wake else {
            return "auto_renew_wake (no authenticated session, so no auto-renew tick to wake)"
        }
        wake.wake()
        return nil
    }
    #endif

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
        guard let nestId = try? await api.boundNestId() else { return }
        _ = await machine.autoRenewIssue(domains: due, targetNestId: nestId)
    }
}

#if DEBUG
/// A wait one `wake()` ends early — the apple twin of tui's `LoopWake`. A wake
/// that lands while no wait is pending (the loop mid-pass) is kept and ends the
/// very next wait, so a test never loses its poke to a busy loop. Main-actor
/// confined, like the cadence that owns it; cancelling the waiting task ends
/// the wait too, so `stop` still tears the loop down promptly.
@MainActor
final class CadenceWake {
    private var pending = false
    private var waiter: (token: UInt64, resume: CheckedContinuation<Void, Never>)?
    private var nextToken: UInt64 = 0

    func wake() {
        if let waiter {
            self.waiter = nil
            waiter.resume.resume()
        } else {
            pending = true
        }
    }

    /// Sleep `nanoseconds`, or until `wake()` (or the task's cancellation)
    /// ends the wait first.
    func sleep(nanoseconds: UInt64) async {
        if pending {
            pending = false
            return
        }
        nextToken &+= 1
        let token = nextToken
        var timer: Task<Void, Never>?
        await withTaskCancellationHandler {
            await withCheckedContinuation { (resume: CheckedContinuation<Void, Never>) in
                waiter = (token, resume)
                timer = Task { @MainActor [weak self] in
                    try? await Task.sleep(nanoseconds: nanoseconds)
                    self?.end(token)
                }
            }
        } onCancel: {
            Task { @MainActor [weak self] in self?.end(token) }
        }
        timer?.cancel()
    }

    /// Whether a wake is stored for the next wait. Exists so the unit test can
    /// pin the keep-an-early-wake rule without a clock.
    var hasPendingWake: Bool { pending }

    /// Whether a wait is suspended on this wake now — the unit test's barrier
    /// for "the wake lands mid-wait", in place of a sleep.
    var isWaitingForTest: Bool { waiter != nil }

    private func end(_ token: UInt64) {
        guard let waiter, waiter.token == token else { return }
        self.waiter = nil
        waiter.resume.resume()
    }
}
#endif

#endif // !FAUNA_KIDS
