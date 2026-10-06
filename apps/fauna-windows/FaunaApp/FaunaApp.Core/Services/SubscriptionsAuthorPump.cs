using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The author-side subscriptions reconciliation loop — the windows leg of the
/// encrypted-mode auto-approve run-wiring (`monetization.md` § The unifying
/// model, grant path 2: "auto_approve … no creator action"). In encrypted mode
/// the nest cannot mint, so a follow (any auto_approve-tier subscribe)
/// *enqueues*; this pump is the author's client picking it up.
/// <para><b>Scheduling only.</b> What a tick does and how long to wait between
/// ticks are shared policy (`monetization.md` § Pillar 1 → Where the logic
/// lives: "An app MUST NOT re-derive either") — the tick body is
/// <c>subscriptions_reconcile_once</c> and the cadence is
/// <c>subscriptions_author_poll_secs</c>, both reached through
/// <see cref="INestRpcClient"/> / <see cref="DefaultPollInterval"/>. This class
/// owns just the part that is genuinely platform-specific: the
/// <see cref="Task.Delay(TimeSpan, CancellationToken)"/> loop and its
/// cancellation. Twin of `apps/fauna-tui/src/subscriptions_author.rs` and
/// `apps/fauna-linux/src/subscriptions_author.rs`.</para>
/// <para>Every tick is best-effort: each half's fault comes back inside the
/// pass, is logged, and must never disrupt login (the reconnect/knock-pump
/// stance).</para>
/// </summary>
internal sealed class SubscriptionsAuthorPump
{
    /// <summary>The shared backstop cadence — 30 s plus the
    /// <c>FAUNA_SUBS_POLL_SECS</c> e2e override, read from shared Rust rather
    /// than restated here (the deviation that previously left the windows pump
    /// unable to take the e2e's fast drain cadence). Read per construction, not
    /// at type load, so the override applies to a pump built after the
    /// environment is set.</summary>
    public static TimeSpan DefaultPollInterval =>
        TimeSpan.FromSeconds(FaunaFfiMethods.SubscriptionsAuthorPollSecs());

    private readonly INestRpcClient _rpc;
    private readonly TimeSpan _pollInterval;
    private readonly Func<TimeSpan, CancellationToken, Task> _delay;

    /// <param name="rpc">The seam the shared tick dispatches through (owner
    /// secret is held inside the client).</param>
    /// <param name="pollInterval">Backstop cadence; defaults to
    /// <see cref="DefaultPollInterval"/>.</param>
    /// <param name="delay">Test seam for the poll wait; defaults to
    /// <see cref="Task.Delay(TimeSpan, CancellationToken)"/>.</param>
    public SubscriptionsAuthorPump(
        INestRpcClient rpc,
        TimeSpan? pollInterval = null,
        Func<TimeSpan, CancellationToken, Task>? delay = null)
    {
        _rpc = rpc;
        _pollInterval = pollInterval ?? DefaultPollInterval;
        _delay = delay ?? Task.Delay;
    }

    /// <summary>Run until <paramref name="ct"/> cancels. Never throws.</summary>
    public async Task RunAsync(CancellationToken ct = default)
    {
        while (!ct.IsCancellationRequested)
        {
            // BOTH halves, in the shared order, on EVERY tick. Hoisting the
            // resume half to once-per-connect looks equivalent and is not: a
            // removal staged mid-session — a transient upload failure, or a peer
            // device's staging merged in by config sync — would then heal only
            // at the next connect, leaving the removed subscriber covered by the
            // current KeyBlob until then.
            try
            {
                var pass = await _rpc.SubscriptionsReconcileOnceAsync().ConfigureAwait(false);
                if (pass.resumeError is not null)
                    ShellLog.Warn("SubscriptionsAuthorPump", $"resume_pending_removals failed: {pass.resumeError}");
                if (pass.resumed > 0)
                    ShellLog.Info("SubscriptionsAuthorPump", $"resumed {pass.resumed} staged removal(s)");
                if (pass.drainError is not null)
                    ShellLog.Warn("SubscriptionsAuthorPump", $"drain_auto_approvals failed: {pass.drainError}");
                if (pass.approved > 0)
                    ShellLog.Info("SubscriptionsAuthorPump", $"auto-approved {pass.approved} queued subscribe(s)");
            }
            catch (Exception ex)
            {
                // The shared tick reports per-half faults in the pass rather
                // than throwing; this catches the surrounding seam (no
                // connection yet, a torn-down client) so a fault can never end
                // the loop.
                ShellLog.Warn("SubscriptionsAuthorPump", $"reconcile_once failed: {ex.Message}");
            }

            try
            {
                await _delay(_pollInterval, ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                break;
            }
        }
    }
}
