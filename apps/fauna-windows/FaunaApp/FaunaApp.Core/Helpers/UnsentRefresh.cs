using FaunaApp.Core.Logs;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// The unsent-changes surfaces' re-read edge (<c>account-offline-mutation.md</c>
/// § Implementation status today, the unsent-changes entry): the generic drain runs in
/// the account runtime's pump and tells the app nothing, so while the feed, a
/// conversation or the account page shows, the surface re-reads the device outbox
/// every <see cref="Interval"/> — the feed manager's <c>refresh_unsent_likes</c>, the
/// conversations manager's <c>refresh_unsent_sends</c>, the account page's list. tui's
/// <c>session::spawn_unsent_refresh</c>, FaunaKit's <c>unsentRefresh</c> modifier and
/// android's <c>refreshUnsentChangesWhileShown</c> are the twins; each is a local store
/// read, so no nest is needed.
///
/// <para>Started on the UI thread by the page that shows the surface and disposed when it
/// leaves: the first read runs at once, and every await resumes on the caller's context
/// (no <c>ConfigureAwait(false)</c> — a WinUI view model must be written on the UI
/// thread). A failed read is logged and the next tick reads again; the surface keeps
/// what the last good read showed.</para>
/// </summary>
internal sealed class UnsentRefresh : IDisposable
{
    /// <summary>tui's <c>UNSENT_REFRESH_INTERVAL</c>, android's <c>UNSENT_REFRESH_MS</c>.</summary>
    internal static readonly TimeSpan Interval = TimeSpan.FromSeconds(2);

    private readonly CancellationTokenSource _stop = new();

    /// <summary>The loop's task — a test awaits it after <see cref="Dispose"/>.</summary>
    internal Task Running { get; }

    private UnsentRefresh(string surface, Func<Task> read, TimeSpan interval)
    {
        Running = LoopAsync(surface, read, interval, _stop.Token);
    }

    /// <summary>Read now, then every <paramref name="interval"/> (default
    /// <see cref="Interval"/>) until disposed.</summary>
    internal static UnsentRefresh Start(string surface, Func<Task> read, TimeSpan? interval = null) =>
        new(surface, read, interval ?? Interval);

    private static async Task LoopAsync(string surface, Func<Task> read, TimeSpan interval, CancellationToken stop)
    {
        while (!stop.IsCancellationRequested)
        {
            try
            {
                await read();
            }
            catch (Exception ex)
            {
                ShellLog.Debug(nameof(UnsentRefresh), $"{surface}: unsent changes not re-read: {ex.Message}");
            }
            try
            {
                await Task.Delay(interval, stop);
            }
            catch (OperationCanceledException)
            {
                return;
            }
        }
    }

    /// <summary>Stop the loop. Cancelled, never disposed: the loop may still be reading
    /// the token when the page leaves, and a cancelled source holds nothing.</summary>
    public void Dispose() => _stop.Cancel();
}
