using System;
using System.IO;
using System.Linq;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;
using uniffi.fauna_log;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Conformance for the windows shell-log shim (<see cref="ShellLog"/>) over the
/// REAL <c>fauna_log</c> ring: a producer-side <c>ShellLog</c> call must reach the
/// process-global ring the Settings → Logs page reads (observability.md § The emit
/// API — <c>log_message</c> re-enters the same subscriber <c>installLogging</c> set
/// up). Calls the real <c>FaunaFfiMethods</c> (the native <c>fauna_ffi</c> dll loads
/// in the test host — memory <c>reference_windows_dotnet_test_loads_native_ffi</c>).
/// The ring is process-global + racy under parallel xUnit, so we assert PRESENCE of
/// a GUID-unique message, never exact ring contents.
/// </summary>
public class ShellLogTests
{
    /// <summary>Ensure a subscriber exists so <c>log_message</c> has a ring to feed.
    /// Idempotent at the Rust layer; a repeat install on a later test is swallowed.</summary>
    private static void EnsureRingInstalled()
    {
        try { FaunaFfiMethods.InstallLogging(Path.Combine(Path.GetTempPath(), "fauna-shelllog-test")); }
        catch { /* already installed this run — the global subscriber persists */ }
    }

    [Fact]
    public void Error_ReachesTheRing_WithWindowsTargetPrefix()
    {
        EnsureRingInstalled();
        var msg = "shelllog-error-" + Guid.NewGuid();

        ShellLog.Error("ShellLogTest", msg);

        var hit = FaunaFfiMethods.LogSnapshot().FirstOrDefault(e => e.message == msg);
        Assert.NotNull(hit);
        Assert.Equal(LogLevel.Error, hit!.level);
        Assert.Equal("fauna_windows::ShellLogTest", hit.target);
    }

    [Fact]
    public void Warn_ReachesTheRing()
    {
        EnsureRingInstalled();
        var msg = "shelllog-warn-" + Guid.NewGuid();

        ShellLog.Warn("ShellLogTest", msg);

        Assert.Contains(FaunaFfiMethods.LogSnapshot(), e => e.message == msg && e.level == LogLevel.Warn);
    }
}
