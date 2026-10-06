namespace FaunaApp.Core.Logs;

/// <summary>
/// The single writer behind <c>FAUNA_E2E_AGENT_LOG</c>: one timestamped,
/// thread-tagged trace file shared by the e2e agent's poll loop
/// (<c>FaunaApp.Testing.TestAgent</c>) and the shell's own log
/// (<see cref="ShellLog"/>). Null — and a total no-op — unless the variable is
/// set, so this costs nothing outside a deliberate debugging session.
///
/// <para><b>Why this exists as a type rather than an <c>AppendAllText</c> at each
/// call site.</b> The two producers write from different threads at once (the
/// agent polls on a thread-pool thread while the UI thread runs post-actions), and
/// <c>File.AppendAllText</c> takes an exclusive handle: concurrent callers throw
/// <c>IOException</c> ("used by another process"). A diagnostic writer must never
/// crash its app, so those throws get swallowed — which silently DROPS trace lines
/// exactly when the app is busiest, i.e. exactly when the bug being chased happens.
/// A trace that thins out under load is worse than no trace: it reads as "the app
/// stopped doing anything there", which is a false finding. Serializing the writes
/// behind one lock is what makes an absent line mean "it did not happen".</para>
///
/// <para>Both producers must share THIS lock, not one each — two locks over one
/// file reintroduce the same cross-thread collision.</para>
/// </summary>
internal static class E2eTrace
{
    // Read through Core.Services.E2eEnv so the variable is compiled out of release
    // builds (convention 15): the path is taken verbatim, so an ungated read is an
    // append-anywhere primitive at app privilege in a shipped MSI.
    private static readonly string? _path = Services.E2eEnv.AgentLog;

    private static readonly object _gate = new();

    /// <summary>True when tracing is on; lets a caller skip building a message.</summary>
    internal static bool Enabled => _path is not null;

    internal static void Write(string message)
    {
        if (_path is null) return;
        var line = $"[{DateTime.Now:HH:mm:ss.fff}] [T{Environment.CurrentManagedThreadId}] {message}\n";
        lock (_gate)
        {
            try { System.IO.File.AppendAllText(_path, line); }
            catch { /* diagnostics must never break the app */ }
        }
    }
}
