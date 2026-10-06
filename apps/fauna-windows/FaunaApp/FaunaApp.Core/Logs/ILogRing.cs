using System.Collections.Generic;
using uniffi.fauna_ffi;
using uniffi.fauna_log;

namespace FaunaApp.Core.Logs;

/// <summary>
/// The process-global client log ring (the <c>fauna_log</c> ring filled by every
/// <c>tracing</c> event the client emits, installed at app start by
/// <c>FaunaFfiMethods.InstallLogging</c>). The seam the Settings → Logs
/// <c>LogsViewModel</c> reads, so the VM is deterministically unit-testable against
/// a fake — the real global ring is process-shared and racy under parallel xUnit.
/// </summary>
internal interface ILogRing
{
    /// <summary>Entries at or above <paramref name="min"/> in severity, oldest-first
    /// (<c>fauna_log::snapshot_at_least</c>); <c>null</c> ⇒ every entry
    /// (<c>fauna_log::snapshot</c>).</summary>
    IReadOnlyList<LogEntry> Snapshot(LogLevel? min);

    /// <summary>Drop the in-memory ring (<c>fauna_log::clear</c>); leaves the
    /// on-disk rolling file untouched.</summary>
    void Clear();
}

/// <summary>Production <see cref="ILogRing"/> over the UniFFI global log fns.</summary>
internal sealed class FfiLogRing : ILogRing
{
    public IReadOnlyList<LogEntry> Snapshot(LogLevel? min)
        => min is LogLevel m ? FaunaFfiMethods.LogSnapshotAtLeast(m) : FaunaFfiMethods.LogSnapshot();

    public void Clear() => FaunaFfiMethods.LogClear();
}
