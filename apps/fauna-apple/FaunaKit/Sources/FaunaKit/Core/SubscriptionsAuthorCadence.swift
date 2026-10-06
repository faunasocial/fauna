import Foundation

/// Author-side subscription reconciliation loop (macOS + iOS, shared FaunaKit —
/// priority #2). The apple analog of linux `subscriptions_author.rs::start`, spawned
/// at login alongside the conversations receive loop.
///
/// **Scheduling only.** What a tick does and how long to wait between ticks are
/// shared policy (`monetization.md` § Pillar 1 → *Where the logic lives*: "An app
/// MUST NOT re-derive either") — the tick body is `subscriptions_reconcile_once`
/// (resume any crash-staged subscriber removal, then auto-approve every pending
/// `auto_approve` **subscribe** request, minting the covering `KeyBlob` per row; this
/// is what makes an encrypted-mode **follow** frictionless, since the nest cannot mint
/// and a follow otherwise *enqueues* — `monetization.md` § The unifying model, grant
/// path 2) and the cadence is `subscriptions_author_poll_secs`, both reached through
/// `APIClient.reconcileSubscriptionsOnce()`. This class owns just the part that is
/// genuinely platform-specific: the `Task` loop and its cancellation.
///
/// There is no subscribe-request push kind today, so the poll is the delivery floor: a
/// follow arriving while the author is online lands within one interval; follows that
/// accumulated while the author was offline are drained by the connect-time first pass.
///
/// All mint / seal / rotation crypto stays in shared Rust
/// (`fauna-client-subscriptions`); this is transport glue only. Best-effort — every
/// error is logged and swallowed, and the next tick retries.
@MainActor
public final class SubscriptionsAuthorCadence {
    public static let shared = SubscriptionsAuthorCadence()

    /// The shared backstop cadence — 30 s plus the `FAUNA_SUBS_POLL_SECS` e2e
    /// override, read from shared Rust rather than restated here.
    private static var pollSeconds: UInt64 { subscriptionsAuthorPollSecs() }

    private var loop: Task<Void, Never>?

    /// Whether a loop is currently armed — see the twin on `DnsAutoRenewCadence`.
    /// Lets `ActorScopeTests` pin that a teardown actually disarms this cadence.
    var isRunning: Bool { loop != nil }

    private init() {}

    /// Start the reconcile loop for the just-authenticated actor. Call **after** the
    /// actor secret is primed (post-login, next to the conversations session
    /// activation), so the owner's custody + `ManageSubscribers` self-delegation can be
    /// derived. A re-login supersedes the prior loop (it would hold a stale nest
    /// client) — mirroring linux's login-generation guard.
    public func start(api: APIClient) {
        loop?.cancel()
        loop = Task { [weak self] in
            while !Task.isCancelled {
                // First pass runs immediately on connect: it drains whatever queued
                // while the author was offline.
                await self?.tick(api: api)
                if Task.isCancelled { return }
                try? await Task.sleep(nanoseconds: Self.pollSeconds * 1_000_000_000)
            }
        }
    }

    public func stop() {
        loop?.cancel()
        loop = nil
    }

    /// One tick — both halves, in the shared order, every time. Hoisting the resume
    /// half to once-per-connect looks equivalent and is not: a removal staged
    /// mid-session (a transient upload failure, or a peer device's staging merged in
    /// by config sync) would then heal only at the next connect, leaving the removed
    /// subscriber covered by the current `KeyBlob` until then.
    private func tick(api: APIClient) async {
        do {
            let pass = try await api.reconcileSubscriptionsOnce()
            if let resumeError = pass.resumeError {
                logMessage(level: .warn, target: "fauna.subscriptions",
                           message: "resume_pending_removals failed: \(resumeError)")
            }
            if pass.resumed > 0 {
                logMessage(level: .info, target: "fauna.subscriptions",
                           message: "resumed \(pass.resumed) staged removal(s)")
            }
            if let drainError = pass.drainError {
                logMessage(level: .warn, target: "fauna.subscriptions",
                           message: "drain_auto_approvals failed: \(drainError)")
            }
            if pass.approved > 0 {
                logMessage(level: .info, target: "fauna.subscriptions",
                           message: "auto-approved \(pass.approved) pending follow(s)")
            }
        } catch {
            // The shared tick reports per-half faults in the pass rather than
            // throwing; this catches the surrounding seam (no secret yet, a
            // torn-down client) so a fault can never end the loop.
            logMessage(level: .warn, target: "fauna.subscriptions",
                       message: "reconcile_once failed: \(error)")
        }
    }
}
